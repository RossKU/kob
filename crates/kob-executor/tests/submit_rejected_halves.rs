//! Audit 2026-10-30 E2 regression: a node-level `Rejected` names no order, so the batch is searched by halves across ticks
//! instead of backing off every order of it.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use common::*;
use kob_executor::matcher::book::MemoryBook;
use kob_executor::matcher::node::*;
use kob_executor::matcher::run::{Roles, RunConfig, Runner, ViewSource};
use kob_executor::matcher::wallet::p2pk_spk_string;
use serde_json::Value;

#[derive(Default)]
struct State {
    daa: u64,
    replies: VecDeque<String>,
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
            has_utxo_index: true,
            virtual_daa_score: s.daa,
        })
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(DagInfo { network: "testnet-10".into(), sink: "s0".into(), virtual_daa_score: s.daa })
    }
    async fn submit(&self, _tx: Value) -> Result<[u8; 32], RpcError> {
        match self.0.lock().unwrap().replies.pop_front() {
            Some(m) => Err(RpcError::Node(m)),
            None => Ok([0; 32]),
        }
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        Ok(ChainUpdate::default())
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        let funding = AddressUtxo {
            transaction_id: [0xf1; 32],
            index: 0,
            amount: 100 * KAS,
            script_public_key: p2pk_spk_string(&pk(MATCHER)),
            block_daa_score: 500,
            covenant_id: None,
        };
        if a.len() == 1 && a[0].starts_with("kaspatest:q") {
            return Ok(vec![funding]);
        }
        Ok(vec![])
    }
}

#[tokio::test]
async fn a_generic_node_rejection_does_not_back_off_every_order_of_the_batch() {
    let daa = NOW + 5;
    let orders = vec![
        l_bid(1, bid(1, P260, T3), 10 * WHOLE),
        l_ask(6, ask(6, P250, 5 * WHOLE, T3)),
        l_ask(100, ask(2, P245, 2 * WHOLE, T3)),
    ];
    let node = Node::default();
    node.0.lock().unwrap().daa = daa;
    // a refusal that names no outpoint and is no double spend / orphan / full mempool: what a node-only policy check says
    node.0.lock().unwrap().replies.push_back(format!("Rejected transaction {}: transaction is not standard: some policy", "ee".repeat(32)));
    let cfg = RunConfig { roles: Roles { matcher: true, keeper: false }, verify_live_min_lag: u64::MAX, ..RunConfig::default() };
    let book = MemoryBook { daa_score: daa, orders, wallet_tokens: vec![], keeper_only: vec![] };
    let mut r = Runner::new(node.clone(), ViewSource(book), Box::new(signer()), cfg);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.rejected.len(), 1);
    let off = r.tracker.backed_off(daa);
    let all: BTreeSet<[u8; 32]> = [cid(1), cid(6), cid(100)].into_iter().collect();
    assert!(!off.is_empty(), "the refused batch is searched: part of it sits out");
    assert!(off.len() < all.len() && off.is_subset(&all), "not every order of the refused batch backs off: {off:?}");
}
