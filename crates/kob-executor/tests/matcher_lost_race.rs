//! A batch that loses its race over one order input backs off that order alone; the other orders of the batch are planned
//! again at once:
//!
//! * the node refuses the batch because an input is already spent in a block the book has not shown yet (an orphan, no
//!   outpoint named): the runner asks the node which order inputs are still unspent and backs off only the order whose
//!   input is gone;
//! * the batch reached the mempool, but another transaction spending one of its inputs was accepted instead: once the book
//!   no longer lists that input and the node confirms it spent, the batch is dropped before `pending_timeout` and only the
//!   owner of that input backs off.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use common::*;
use kob_executor::matcher::book::{outpoint, ListedOrder, MemoryBook, Outpoint};
use kob_executor::matcher::node::*;
use kob_executor::matcher::run::{Roles, RunConfig, Runner, ViewSource};
use kob_executor::matcher::wallet::p2pk_spk_string;
use serde_json::Value;

#[derive(Default)]
struct State {
    daa: u64,
    replies: VecDeque<String>,
    /// The order and custody outpoints the node still holds (with their covenant ids).
    live: Vec<(Outpoint, [u8; 32])>,
    /// The node cannot answer a UTXO query of order addresses.
    blind: bool,
    /// The node runs without its UTXO index (an address query proves nothing).
    no_index: bool,
}

#[derive(Clone, Default)]
struct Node(Arc<Mutex<State>>);

impl NodeApi for Node {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(ServerInfo {
            server_version: "2.1.0".into(),
            network_id: "testnet-10".into(),
            is_synced: true,
            has_utxo_index: !s.no_index,
            virtual_daa_score: s.daa,
        })
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(DagInfo { network: "testnet-10".into(), sink: "s0".into(), virtual_daa_score: s.daa })
    }
    async fn submit(&self, _tx: Value) -> Result<[u8; 32], RpcError> {
        let mut s = self.0.lock().unwrap();
        match s.replies.pop_front() {
            Some(m) => Err(RpcError::Node(m)),
            None => Ok([0; 32]),
        }
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        // the batches of these tests are never accepted
        Ok(ChainUpdate::default())
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        let s = self.0.lock().unwrap();
        let funding = AddressUtxo {
            transaction_id: [0xf1; 32],
            index: 0,
            amount: 100 * KAS,
            script_public_key: p2pk_spk_string(&pk(MATCHER)),
            block_daa_score: 500,
            covenant_id: None,
        };
        // the operator's own address: its funding
        if a.len() == 1 && a[0].starts_with("kaspatest:q") {
            return Ok(vec![funding]);
        }
        if s.blind {
            return Err(RpcError::Node("utxo index unavailable".into()));
        }
        // the order addresses: every order outpoint the node still holds
        Ok(s.live
            .iter()
            .map(|(op, cov)| AddressUtxo {
                transaction_id: op.0,
                index: op.1,
                amount: 2 * KAS,
                script_public_key: "0000aa".into(),
                block_daa_score: 500,
                covenant_id: Some(*cov),
            })
            .collect())
    }
}

const VICTIM: u32 = 1; // a bid (maker 1), 10 whole at 2.60
const OTHER: u32 = 6; // an ask (maker 6), 5 whole at 2.50
const SPENT: u32 = 100; // the cheapest ask (maker 2), 2 whole at 2.45: its input is spent elsewhere

fn orders() -> Vec<ListedOrder> {
    vec![
        l_bid(VICTIM, bid(1, P260, T3), 10 * WHOLE),
        l_ask(OTHER, ask(6, P250, 5 * WHOLE, T3)),
        l_ask(SPENT, ask(2, P245, 2 * WHOLE, T3)),
    ]
}

fn outpoints(o: &ListedOrder) -> Vec<(Outpoint, [u8; 32])> {
    let mut v = vec![(outpoint(&o.order.utxo), o.id())];
    for c in o.custody.iter().chain(o.custody_b.iter()) {
        v.push((outpoint(&c.utxo), o.id()));
    }
    v
}

/// The runner over the book `orders()`, with a node that holds every listed outpoint but those of `SPENT`.
fn runner(daa: u64) -> (Runner<Node, ViewSource<MemoryBook>>, Node) {
    let listed = orders();
    let node = Node::default();
    {
        let mut s = node.0.lock().unwrap();
        s.daa = daa;
        s.live = listed.iter().filter(|o| o.id() != cid(SPENT)).flat_map(outpoints).collect();
    }
    let cfg = RunConfig { roles: Roles { matcher: true, keeper: false }, ..RunConfig::default() };
    let book = MemoryBook { daa_score: daa, orders: listed, wallet_tokens: vec![] };
    (Runner::new(node.clone(), ViewSource(book), Box::new(signer()), cfg), node)
}

fn orphan() -> String {
    // rusty-kaspa's RejectDisallowedOrphan as the RPC wraps it: one input is already spent in the virtual UTXO set
    format!("Rejected transaction {0}: transaction {0} is an orphan where orphan is disallowed", "ee".repeat(32))
}

fn at(r: &mut Runner<Node, ViewSource<MemoryBook>>, node: &Node, daa: u64) {
    r.source.0.daa_score = daa;
    node.0.lock().unwrap().daa = daa;
}

#[tokio::test]
async fn a_missing_input_backs_off_only_the_order_whose_input_is_gone() {
    let daa = NOW + 5;
    let (mut r, node) = runner(daa);
    node.0.lock().unwrap().replies.push_back(orphan());
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1, "the batch lost its race: {:?}", rep.skipped);
    let off = r.tracker.backed_off(daa);
    assert_eq!(off, [cid(SPENT)].into_iter().collect::<BTreeSet<_>>(), "only the order whose input is gone backs off");
    // the next tick (the book still lists the spent order): the other orders cross and are submitted
    at(&mut r, &node, daa + 1);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1, "the other orders are planned again at once: {:?}", rep.skipped);
    let t = r.tracker.txs.values().next().unwrap();
    assert!(t.orders.contains(&cid(VICTIM)) && t.orders.contains(&cid(OTHER)) && !t.orders.contains(&cid(SPENT)));
}

#[tokio::test]
async fn a_missing_input_the_node_cannot_place_backs_off_every_order() {
    let daa = NOW + 5;
    let (mut r, node) = runner(daa);
    {
        let mut s = node.0.lock().unwrap();
        s.replies.push_back(orphan());
        s.blind = true;
    }
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    let off = r.tracker.backed_off(daa);
    assert!(off.contains(&cid(VICTIM)) && off.contains(&cid(OTHER)) && off.contains(&cid(SPENT)), "{off:?}");
}

#[tokio::test]
async fn a_batch_whose_input_another_transaction_spent_is_dropped_before_the_timeout() {
    let daa0 = NOW + 5;
    let (mut r, node) = runner(daa0);
    // the node holds every input when the batch is submitted
    node.0.lock().unwrap().live = r.source.0.orders.iter().flat_map(outpoints).collect();
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1, "the batch is in the mempool: {:?}", rep.skipped);
    // another transaction spends the cheapest ask: the book and the node no longer list it
    let rest: Vec<ListedOrder> = r.source.0.orders.iter().filter(|o| o.id() != cid(SPENT)).cloned().collect();
    node.0.lock().unwrap().live = rest.iter().flat_map(outpoints).collect();
    r.source.0.orders = rest;
    let mut planned_at = None;
    for d in [daa0 + 10, daa0 + 20, daa0 + 30] {
        at(&mut r, &node, d);
        let rep = r.step().await.unwrap();
        if !rep.built.is_empty() {
            planned_at = Some(d);
            break;
        }
    }
    assert_eq!(planned_at, Some(daa0 + 20), "the lost batch is dropped on the second sighting and its orders planned again");
    assert!(!r.tracker.backed_off(daa0 + 20).contains(&cid(VICTIM)));
    assert!(r.tracker.txs.values().all(|t| !t.orders.contains(&cid(SPENT))), "the lost batch is no longer tracked");
}

#[tokio::test]
async fn without_the_utxo_index_a_missing_input_is_not_taken_as_proof() {
    let daa = NOW + 5;
    let (mut r, node) = runner(daa);
    {
        let mut s = node.0.lock().unwrap();
        s.replies.push_back(orphan());
        s.no_index = true;
        // an index-less node answers the address query with nothing at all: that is no evidence of a spend
        s.live = vec![];
    }
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    let off = r.tracker.backed_off(daa);
    assert!(off.contains(&cid(VICTIM)) && off.contains(&cid(OTHER)) && off.contains(&cid(SPENT)), "as without an answer: {off:?}");
}

#[tokio::test]
async fn without_the_utxo_index_a_pending_batch_is_left_to_its_timeout() {
    let daa0 = NOW + 5;
    let (mut r, node) = runner(daa0);
    node.0.lock().unwrap().live = r.source.0.orders.iter().flat_map(outpoints).collect();
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1, "the batch is in the mempool: {:?}", rep.skipped);
    let rest: Vec<ListedOrder> = r.source.0.orders.iter().filter(|o| o.id() != cid(SPENT)).cloned().collect();
    {
        let mut s = node.0.lock().unwrap();
        s.live = vec![];
        s.no_index = true;
    }
    r.source.0.orders = rest;
    for d in [daa0 + 10, daa0 + 20, daa0 + 30] {
        at(&mut r, &node, d);
        r.step().await.unwrap();
    }
    assert_eq!(r.tracker.txs.len(), 1, "not dropped as lost: the node cannot tell");
    assert!(r.tracker.backed_off(daa0 + 30).is_empty(), "nothing backs off on a guess");
}
