//! The submission loop against a scripted node: submission, benign double spends (back-off, no
//! replacement), acceptance through VSPC v2, finality, reorg rollback and resend, chained steps
//! only after their parent, the kill switch, dry runs and the keeper role.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use common::*;
use kob_executor::matcher::book::MemoryBook;
use kob_executor::matcher::node::*;
use kob_executor::matcher::run::{Roles, RunConfig, Runner, ViewSource};
use kob_executor::matcher::wallet::p2pk_spk_string;
use kob_protocol::state::*;
use serde_json::Value;

#[derive(Default)]
struct State {
    daa: u64,
    submits: Vec<Value>,
    submit_replies: VecDeque<Result<(), String>>,
    chain: VecDeque<ChainUpdate>,
    funding: Vec<AddressUtxo>,
    network: String,
    /// The `getFeeEstimate` answer (None: the node answers none).
    fee: Option<Value>,
    fee_calls: usize,
}

#[derive(Clone, Default)]
struct MockNode(Arc<Mutex<State>>);

impl MockNode {
    fn new(daa: u64) -> Self {
        let n = MockNode::default();
        {
            let mut s = n.0.lock().unwrap();
            s.daa = daa;
            s.network = "testnet-10".into();
            s.funding = vec![AddressUtxo {
                transaction_id: [0xf1; 32],
                index: 0,
                amount: 100 * KAS,
                script_public_key: p2pk_spk_string(&pk(MATCHER)),
                block_daa_score: 500,
                covenant_id: None,
            }];
        }
        n
    }
    fn submits(&self) -> usize {
        self.0.lock().unwrap().submits.len()
    }
    fn txid_of(v: &Value) -> [u8; 32] {
        // The RPC JSON has no id: recompute it from the transaction.
        let _ = v;
        [0; 32]
    }
}

impl NodeApi for MockNode {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(ServerInfo {
            server_version: "2.1.0".into(),
            network_id: s.network.clone(),
            is_synced: true,
            has_utxo_index: true,
            virtual_daa_score: s.daa,
        })
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(DagInfo { network: s.network.clone(), sink: "s0".into(), virtual_daa_score: s.daa })
    }
    async fn submit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let mut s = self.0.lock().unwrap();
        s.submits.push(tx.clone());
        match s.submit_replies.pop_front() {
            Some(Err(m)) if m.starts_with("transport:") => Err(RpcError::Transport(m)),
            Some(Err(m)) => Err(RpcError::Node(m)),
            _ => Ok(MockNode::txid_of(&tx)),
        }
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        Ok(self.0.lock().unwrap().chain.pop_front().unwrap_or_default())
    }
    async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        Ok(self.0.lock().unwrap().funding.clone())
    }
    async fn fee_estimate(&self) -> Result<kob_executor::fee::FeeEstimate, RpcError> {
        let mut s = self.0.lock().unwrap();
        s.fee_calls += 1;
        match &s.fee {
            Some(v) => kob_executor::fee::FeeEstimate::parse(v).map_err(RpcError::Decode),
            None => Err(RpcError::Transport("no answer".into())),
        }
    }
}

fn crossing_book() -> MemoryBook {
    book(vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))])
}

fn runner(node: MockNode, b: MemoryBook, roles: Roles) -> Runner<MockNode, ViewSource<MemoryBook>> {
    let cfg = RunConfig { roles, ..RunConfig::default() };
    Runner::new(node, ViewSource(b), Box::new(signer()), cfg)
}

fn accept(node: &MockNode, hash: &str, daa: u64, txids: Vec<[u8; 32]>) {
    node.0
        .lock()
        .unwrap()
        .chain
        .push_back(ChainUpdate { removed: vec![], added: vec![ChainBlock { hash: hash.into(), daa_score: daa, accepted: txids }] });
}

const M: Roles = Roles { matcher: true, keeper: false };

#[tokio::test]
async fn submit_accept_finalize() {
    let node = MockNode::new(NOW + 5);
    let mut r = runner(node.clone(), crossing_book(), M);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.built.len(), 1);
    assert_eq!(rep.submitted.len(), 1);
    let txid = rep.submitted[0];
    assert_eq!(node.submits(), 1);
    // The book still shows the spent orders (indexer lag): they are busy, nothing is resent.
    let rep = r.step().await.unwrap();
    assert!(rep.built.is_empty(), "no order is planned twice while its transaction is pending");
    // Accepted, then final after 100 DAA.
    accept(&node, "b1", NOW + 6, vec![txid]);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.accepted, 1);
    node.0.lock().unwrap().daa = NOW + 106;
    let rep = r.step().await.unwrap();
    assert_eq!(rep.finalized, 1);
    assert_eq!(r.tracker.finalized_count, 1);
    assert!(r.tracker.finalized_profit > 0);
}

#[tokio::test]
async fn double_spend_backs_off_and_never_replaces() {
    let node = MockNode::new(NOW + 5);
    node.0.lock().unwrap().submit_replies.push_back(Err("output ab:0 already spent by transaction cd in the mempool".into()));
    let mut r = runner(node.clone(), crossing_book(), M);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert_eq!(r.tracker.conflicts, 1);
    // Backed off: the same orders are not rebuilt within the back-off window.
    let rep = r.step().await.unwrap();
    assert!(rep.built.is_empty());
    assert_eq!(node.submits(), 1, "no blind replacement");
    // After the back-off (20 DAA) the orders are tried again.
    node.0.lock().unwrap().daa = NOW + 30;
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1);
}

#[tokio::test]
async fn reorg_rolls_back_and_resends() {
    let node = MockNode::new(NOW + 5);
    let mut r = runner(node.clone(), crossing_book(), M);
    let txid = r.step().await.unwrap().submitted[0];
    accept(&node, "b1", NOW + 6, vec![txid]);
    r.step().await.unwrap();
    node.0.lock().unwrap().chain.push_back(ChainUpdate { removed: vec!["b1".into()], added: vec![] });
    let rep = r.step().await.unwrap();
    assert_eq!(rep.rolled_back, 1);
    assert_eq!(r.tracker.pending(), 1);
    assert_eq!(node.submits(), 2, "rolled-back transaction resent (idempotent)");
    // Never marked done before acceptance + depth.
    node.0.lock().unwrap().daa = NOW + 500;
    r.step().await.unwrap();
    assert_eq!(r.tracker.finalized_count, 0);
}

/// A submit that fails on the wire may have reached the node. It is tracked as pending (its inputs stay reserved, no
/// conflicting transaction is planned) and resent idempotently until the node answers.
#[tokio::test]
async fn unknown_submit_outcome_is_tracked_and_resent() {
    let node = MockNode::new(NOW + 5);
    node.0.lock().unwrap().submit_replies.push_back(Err("transport: connection reset".into()));
    let mut r = runner(node.clone(), crossing_book(), M);
    let rep = r.step().await.unwrap();
    assert_eq!((rep.built.len(), rep.unknown.len(), rep.submitted.len(), rep.rejected.len()), (1, 1, 0, 0));
    assert_eq!(r.tracker.unknown, 1);
    let txid = rep.unknown[0];
    assert!(r.tracker.txs[&txid].unconfirmed, "tracked as pending, doubt recorded");
    assert_eq!(r.tracker.backoff.len(), 0, "no back-off: the transaction may be in the mempool");
    // Next tick: resent (the node now answers), and nothing conflicting is planned over the same orders.
    let rep = r.step().await.unwrap();
    assert!(rep.built.is_empty(), "the orders are busy while the doubtful transaction is pending");
    assert_eq!(node.submits(), 2, "resent once");
    assert!(!r.tracker.txs[&txid].unconfirmed, "the node's answer settled it");
    // Settled: no further resend.
    r.step().await.unwrap();
    assert_eq!(node.submits(), 2);
    // Accepted, then final as usual.
    accept(&node, "b1", NOW + 6, vec![txid]);
    assert_eq!(r.step().await.unwrap().accepted, 1);
    node.0.lock().unwrap().daa = NOW + 106;
    assert_eq!(r.step().await.unwrap().finalized, 1);
}

#[tokio::test]
async fn unknown_submit_outcome_then_refusal_drops_it_and_backs_off() {
    let node = MockNode::new(NOW + 5);
    {
        let mut s = node.0.lock().unwrap();
        s.submit_replies.push_back(Err("transport: timeout".into()));
        s.submit_replies.push_back(Err("output ab:0 already spent by transaction cd in the mempool".into()));
    }
    let mut r = runner(node.clone(), crossing_book(), M);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.unknown.len(), 1);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1, "the resend found the inputs taken elsewhere");
    assert!(r.tracker.txs.is_empty());
    assert!(!r.tracker.backoff.is_empty(), "the orders back off");
    assert!(rep.built.is_empty());
}

#[tokio::test]
async fn unknown_submit_outcome_survives_repeated_transport_errors_and_times_out() {
    let node = MockNode::new(NOW + 5);
    {
        let mut s = node.0.lock().unwrap();
        for _ in 0..3 {
            s.submit_replies.push_back(Err("transport: down".into()));
        }
    }
    let mut r = runner(node.clone(), crossing_book(), M);
    r.step().await.unwrap();
    r.step().await.unwrap();
    r.step().await.unwrap();
    assert_eq!(node.submits(), 3);
    assert_eq!(r.tracker.txs.len(), 1);
    assert!(r.tracker.txs.values().all(|t| t.unconfirmed));
    // never accepted: dropped at the pending timeout, the orders back off
    node.0.lock().unwrap().daa = NOW + 5 + 700;
    let rep = r.step().await.unwrap();
    assert!(r.tracker.txs.is_empty(), "given up at the pending timeout");
    assert!(rep.built.is_empty(), "the orders back off before they are planned again");
    assert_eq!(node.submits(), 3, "no resend of a transaction that was dropped");
}

#[tokio::test]
async fn no_resend_while_paused() {
    let node = MockNode::new(NOW + 5);
    node.0.lock().unwrap().submit_replies.push_back(Err("transport: reset".into()));
    let dir = std::env::temp_dir().join(format!("kob-pause-unknown-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pause = dir.join("pause");
    let mut r = runner(node.clone(), crossing_book(), M);
    r.cfg.pause_file = Some(pause.clone());
    r.step().await.unwrap();
    assert_eq!(node.submits(), 1);
    std::fs::write(&pause, "").unwrap();
    let rep = r.step().await.unwrap();
    assert!(rep.paused);
    assert_eq!(node.submits(), 1, "the kill switch stops resends too");
    std::fs::remove_file(&pause).unwrap();
    r.step().await.unwrap();
    assert_eq!(node.submits(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn kill_switch_and_dry_run() {
    let node = MockNode::new(NOW + 5);
    let dir = std::env::temp_dir().join(format!("kob-pause-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pause = dir.join("pause");
    std::fs::write(&pause, "").unwrap();
    let mut r = runner(node.clone(), crossing_book(), M);
    r.cfg.pause_file = Some(pause.clone());
    let rep = r.step().await.unwrap();
    assert!(rep.paused && rep.built.is_empty());
    std::fs::remove_file(&pause).unwrap();
    r.cfg.dry_run = true;
    let rep = r.step().await.unwrap();
    assert_eq!(rep.built.len(), 1);
    assert_eq!(node.submits(), 0, "dry run submits nothing");
    // Metrics file.
    let m = dir.join("m.prom");
    r.cfg.metrics_file = Some(m.clone());
    r.step().await.unwrap();
    let text = std::fs::read_to_string(&m).unwrap();
    assert!(text.contains("kob_matcher_funding_sompi 10000000000"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn chained_steps_wait_for_their_parent() {
    let node = MockNode::new(NOW + 5);
    // The first step is rejected: its chained child must not be sent.
    node.0.lock().unwrap().submit_replies.push_back(Err("transaction x is invalid".into()));
    let bd = bid(1, P260, kob_protocol::artifacts::TemplateId::Kcc20Ref8x8);
    let mut orders = vec![listed(cid(1), AnyState::KobBid(bd.clone()), (bd.used(12 * WHOLE).unwrap() + 2 * DC) as u64, 1_000)];
    orders.extend((0..12).map(|i| l_ask(100 + i, ask(2, P250, WHOLE, kob_protocol::artifacts::TemplateId::Kcc20Ref8x8))));
    let mut r = runner(node.clone(), book(orders), M);
    let rep = r.step().await.unwrap();
    assert!(rep.built.len() >= 2);
    assert_eq!(node.submits(), 1, "the child of a rejected parent is never submitted");
    assert_eq!(rep.rejected.len(), 1);
    assert_eq!(rep.conflicts.len(), rep.built.len() - 1, "children are held back as missing inputs");
}

#[tokio::test]
async fn keeper_role_kills_expired_iocs() {
    let node = MockNode::new(5_700);
    let mut ioc = ask(1, P250, 5 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    ioc.expiry_daa = NO_EXPIRY;
    let o = listed(cid(1), AnyState::KobAsk(ioc), CARRIER, 5_000);
    let mut b = book(vec![o]);
    b.daa_score = 5_700;
    let mut r = runner(node.clone(), b, Roles { matcher: false, keeper: true });
    let rep = r.step().await.unwrap();
    assert_eq!(rep.built.len(), 1);
    assert_eq!(rep.built[0].0, "kill");
    assert_eq!(rep.submitted.len(), 1);
}

#[tokio::test]
async fn wrong_network_is_refused() {
    let node = MockNode::new(NOW);
    node.0.lock().unwrap().network = "mainnet".into();
    let mut r = runner(node, crossing_book(), M);
    assert!(r.step().await.is_err());
}

// ---------------------------------------------------------------------------------------------
// fee policy: the runner reads the node's estimate and prices the tick with it

fn estimate(priority: f64, normal: f64, low: f64) -> Value {
    serde_json::json!({"estimate": {
        "priorityBucket": {"feerate": priority, "estimatedSeconds": 0.5},
        "normalBuckets": [{"feerate": normal, "estimatedSeconds": 40.0}],
        "lowBuckets": [{"feerate": low, "estimatedSeconds": 900.0}]
    }})
}

#[tokio::test]
async fn the_tick_is_priced_from_the_node_estimate_and_falls_back_to_the_floor() {
    let node = MockNode::new(NOW + 5);
    // the soak's congestion of 10-02
    node.0.lock().unwrap().fee = Some(estimate(250.4, 194.0, 115.0));
    let cfg = RunConfig {
        roles: M,
        fee_policy: kob_executor::fee::FeePolicy { refresh_ms: 0, max_age_ms: 0, ..Default::default() },
        ..RunConfig::default()
    };
    let mut r = Runner::new(node.clone(), ViewSource(crossing_book()), Box::new(signer()), cfg);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1);
    assert_eq!((r.fee_rates.high, r.fee_rates.normal, r.fee_rates.low, r.fee_rates.estimated), (251, 194, 115, true));
    // a resting batch: the normal rate, and its profit is the one at that rate
    let t = r.tracker.txs.values().next().unwrap();
    assert_eq!(t.fee_rate, 194);
    assert!(t.profit > 0);
    let m = kob_executor::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_fee_rate_normal 194") && m.contains("kob_fee_estimated 1"), "{m}");
    // the node stops answering: the last estimate is too old (max age 0) and every rate is the floor
    node.0.lock().unwrap().fee = None;
    r.step().await.unwrap();
    assert_eq!((r.fee_rates.high, r.fee_rates.normal, r.fee_rates.low, r.fee_rates.estimated), (100, 100, 100, false));
    assert_eq!(r.fee_state.failures, 1);
    // a malformed answer is no estimate either
    node.0.lock().unwrap().fee = Some(serde_json::json!({"estimate": {"priorityBucket": {"feerate": -1.0}}}));
    r.step().await.unwrap();
    assert!(!r.fee_rates.estimated);
    assert_eq!(r.fee_state.failures, 2);
}

#[tokio::test]
async fn an_ioc_fill_pays_the_priority_rate_and_a_static_policy_never_asks() {
    let node = MockNode::new(NOW + 5);
    node.0.lock().unwrap().fee = Some(estimate(250.0, 194.0, 115.0));
    let mut ib = bid(1, P260, T3);
    ib.tif = TIF_IOC;
    let b = book(vec![fresh(l_bid(1, ib, 5 * WHOLE)), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let mut r = Runner::new(node.clone(), ViewSource(b.clone()), Box::new(signer()), RunConfig { roles: M, ..RunConfig::default() });
    r.step().await.unwrap();
    assert_eq!(r.tracker.txs.values().next().unwrap().fee_rate, 250);
    // `--no-fee-estimate`: the floor, and the node is never asked
    let node = MockNode::new(NOW + 5);
    node.0.lock().unwrap().fee = Some(estimate(250.0, 194.0, 115.0));
    let cfg = RunConfig { roles: M, fee_policy: kob_executor::fee::FeePolicy::fixed(100), ..RunConfig::default() };
    let mut r = Runner::new(node.clone(), ViewSource(b), Box::new(signer()), cfg);
    r.step().await.unwrap();
    assert_eq!(r.tracker.txs.values().next().unwrap().fee_rate, 100);
    assert_eq!(node.0.lock().unwrap().fee_calls, 0);
}
