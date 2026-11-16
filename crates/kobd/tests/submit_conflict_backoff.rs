//! A mempool double spend of one input of a matcher batch (an order's maker cancels it just before the matcher submits: the
//! book is read from accepted blocks only) makes the node refuse the whole transaction. Only the order whose input the
//! node names backs off; the other orders of the batch did nothing wrong and are planned again at once. A refusal that
//! names no order's input still backs off every order of the transaction.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use common::*;
use kob_protocol::tx::TokenUtxo;
use kobd::matcher::book::{ListedOrder, MemoryBook};
use kobd::matcher::node::*;
use kobd::matcher::run::{Roles, RunConfig, Runner, ViewSource};
use kobd::matcher::wallet::p2pk_spk_string;
use serde_json::Value;

#[derive(Default)]
struct State {
    daa: u64,
    submits: usize,
    replies: VecDeque<String>,
    /// What an address query returns (`None`: one funding coin); `no_index`: the node has no UTXO index.
    live: Option<Vec<AddressUtxo>>,
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
        s.submits += 1;
        match s.replies.pop_front() {
            Some(m) => Err(RpcError::Node(m)),
            None => Ok([0; 32]),
        }
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        Ok(ChainUpdate::default())
    }
    async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        if let Some(live) = &self.0.lock().unwrap().live {
            return Ok(live.clone());
        }
        Ok(vec![AddressUtxo {
            transaction_id: [0xf1; 32],
            index: 0,
            amount: 100 * KAS,
            script_public_key: p2pk_spk_string(&pk(MATCHER)),
            block_daa_score: 500,
            covenant_id: None,
        }])
    }
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One book: a bid (maker 1, 10 whole) crossing two asks (makers 2 and 5, 5 whole each), one batch.
fn setup() -> (Runner<Node, ViewSource<MemoryBook>>, Node, ListedOrder) {
    let first = l_bid(1, bid(1, P260, T3), 10 * WHOLE);
    let b = MemoryBook {
        daa_score: NOW + 5,
        orders: vec![first.clone(), l_ask(2, ask(2, P250, 5 * WHOLE, T3)), l_ask(5, ask(5, P250, 5 * WHOLE, T3))],
        wallet_tokens: vec![],
        keeper_only: vec![],
    };
    let node = Node::default();
    node.0.lock().unwrap().daa = NOW + 5;
    let cfg = RunConfig { roles: Roles { matcher: true, keeper: false }, ..RunConfig::default() };
    (Runner::new(node.clone(), ViewSource(b), Box::new(signer()), cfg), node, first)
}

#[tokio::test]
async fn a_mempool_conflict_backs_off_only_the_order_whose_input_it_names() {
    let (mut r, node, first) = setup();
    // the node names the bid's own order UTXO, the way rusty-kaspa prints an outpoint
    let op = &first.order.utxo;
    node.0.lock().unwrap().replies.push_back(format!(
        "Rejected transaction {0}: output ({1}, {2}) already spent by transaction {0} in the mempool",
        "ee".repeat(32),
        hex(&op.transaction_id),
        op.index
    ));
    let rep = r.step().await.unwrap();
    assert_eq!((rep.conflicts.len(), rep.submitted.len()), (1, 0), "{:?}", rep.skipped);
    let off = r.tracker.backed_off(NOW + 5);
    assert_eq!(off, [cid(1)].into_iter().collect::<BTreeSet<_>>(), "only the order whose input was spent elsewhere");
    assert!(!r.tracker.backoff.contains_key(&cid(2)) && !r.tracker.backoff.contains_key(&cid(5)));

    // the other orders are planned again next tick (here nothing crosses them any more: no batch, no back-off)
    let rep = r.step().await.unwrap();
    assert!(rep.conflicts.is_empty());
    assert_eq!(r.tracker.backed_off(NOW + 5), [cid(1)].into_iter().collect::<BTreeSet<_>>());
}

#[tokio::test]
async fn a_conflict_that_names_no_order_backs_off_the_whole_batch() {
    let (mut r, node, _) = setup();
    // an outpoint no order of the batch owns (a funding input, say)
    node.0.lock().unwrap().replies.push_back(format!(
        "output ({}, 0) already spent by transaction {} in the mempool",
        "99".repeat(32),
        "ee".repeat(32)
    ));
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    let all: BTreeSet<[u8; 32]> = [cid(1), cid(2), cid(5)].into_iter().collect();
    assert_eq!(r.tracker.backed_off(NOW + 5), all);
}

/// A mode-2 bid (maker 1) whose fill tops up its maker's receive UTXO, crossing one ask (maker 2).
fn topup_setup() -> (Runner<Node, ViewSource<MemoryBook>>, Node, TokenUtxo, Vec<ListedOrder>) {
    let r0 = receive(1, 4 * WHOLE, 500);
    let orders = vec![m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), vec![r0.clone()]), l_ask(2, ask(2, P250, 2 * WHOLE, T3))];
    let b = MemoryBook { daa_score: NOW + 5, orders: orders.clone(), wallet_tokens: vec![], keeper_only: vec![] };
    let node = Node::default();
    node.0.lock().unwrap().daa = NOW + 5;
    let cfg = RunConfig { roles: Roles { matcher: true, keeper: false }, ..RunConfig::default() };
    let mut r = Runner::new(node.clone(), ViewSource(b), Box::new(signer()), cfg);
    r.capture = Some(vec![]);
    (r, node, r0, orders)
}

fn spends(tx: &kob_protocol::tx::SignedTx, op: &kob_protocol::tx::Utxo) -> bool {
    tx.tx.inputs.iter().any(|i| i.transaction_id == op.transaction_id && i.index == op.index)
}

/// The order and custody UTXOs of `orders` as the node's address answer (they are all still there).
fn live(orders: &[ListedOrder]) -> Vec<AddressUtxo> {
    let u = |x: &kob_protocol::tx::Utxo| AddressUtxo {
        transaction_id: x.transaction_id,
        index: x.index,
        amount: x.amount,
        script_public_key: String::new(),
        block_daa_score: 500,
        covenant_id: x.covenant_id,
    };
    orders.iter().flat_map(|o| std::iter::once(u(&o.order.utxo)).chain(o.own_tokens().map(|c| u(&c.utxo)))).collect()
}

/// A lost race over a top-up's receive UTXO (a double spend naming it) backs off no order: the receive UTXO goes busy
/// and the next tick fills the same orders at once with a new receive UTXO (form 1).
#[tokio::test]
async fn a_conflict_on_a_receive_utxo_rebuilds_with_form_1() {
    let (mut r, node, r0, _) = topup_setup();
    node.0.lock().unwrap().replies.push_back(format!(
        "output ({}, {}) already spent by transaction {} in the mempool",
        hex(&r0.utxo.transaction_id),
        r0.utxo.index,
        "ee".repeat(32)
    ));
    let rep = r.step().await.unwrap();
    assert_eq!((rep.conflicts.len(), rep.submitted.len()), (1, 0), "{:?}", rep.skipped);
    assert!(spends(&r.capture.as_ref().unwrap()[0], &r0.utxo), "the first build tops up");
    assert!(r.tracker.backed_off(NOW + 5).is_empty(), "no order backs off");
    assert!(r.tracker.busy_receives(NOW + 5).contains(&(r0.utxo.transaction_id, r0.utxo.index)));
    let rep = r.step().await.unwrap();
    assert_eq!((rep.conflicts.len(), rep.submitted.len()), (0, 1));
    let sent = r.capture.as_ref().unwrap().last().unwrap();
    assert!(!spends(sent, &r0.utxo), "the rebuild delivers a new receive UTXO");
}

/// A missing input the node places on the receive UTXO alone backs off no order; one it cannot place makes the receive
/// UTXO busy and backs off nobody before the rebuild without a top-up, whose own refusal then backs off as usual.
#[tokio::test]
async fn a_missing_receive_utxo_backs_off_no_order() {
    let (mut r, node, r0, orders) = topup_setup();
    {
        let mut s = node.0.lock().unwrap();
        s.replies.push_back("transaction is an orphan: missing outpoint".into());
        s.live = Some(live(&orders));
    }
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert!(r.tracker.backed_off(NOW + 5).is_empty());
    assert!(r.tracker.busy_receives(NOW + 5).contains(&(r0.utxo.transaction_id, r0.utxo.index)));

    let (mut r, node, r0, _) = topup_setup();
    {
        let mut s = node.0.lock().unwrap();
        s.no_index = true;
        s.replies.push_back("transaction is an orphan: missing outpoint".into());
        s.replies.push_back("transaction is an orphan: missing outpoint".into());
    }
    r.step().await.unwrap();
    assert!(r.tracker.backed_off(NOW + 5).is_empty(), "the node cannot tell: rebuild first");
    assert!(r.tracker.busy_receives(NOW + 5).contains(&(r0.utxo.transaction_id, r0.utxo.index)));
    r.step().await.unwrap();
    assert!(!spends(r.capture.as_ref().unwrap().last().unwrap(), &r0.utxo));
    assert_eq!(r.tracker.backed_off(NOW + 5), [cid(1), cid(2)].into_iter().collect::<BTreeSet<_>>(), "then as usual");
}
