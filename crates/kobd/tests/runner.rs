//! The submission loop against a scripted node: submission, benign double spends (back-off, no
//! replacement), acceptance through VSPC v2, finality, reorg rollback and resend, chained steps
//! only after their parent, the kill switch, dry runs and the keeper role.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use common::*;
use kob_protocol::state::*;
use kobd::matcher::book::MemoryBook;
use kobd::matcher::node::*;
use kobd::matcher::run::{FileSource, Roles, RunConfig, Runner, ViewSource};
use kobd::matcher::wallet::p2pk_spk_string;
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
    /// The chain's clock (the sink's timestamp, unix ms); None: the node does not tell.
    tip_ms: Option<u64>,
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
    async fn fee_estimate(&self) -> Result<kobd::fee::FeeEstimate, RpcError> {
        let mut s = self.0.lock().unwrap();
        s.fee_calls += 1;
        match &s.fee {
            Some(v) => kobd::fee::FeeEstimate::parse(v).map_err(RpcError::Decode),
            None => Err(RpcError::Transport("no answer".into())),
        }
    }
    async fn tip_timestamp_ms(&self) -> Result<u64, RpcError> {
        self.0.lock().unwrap().tip_ms.ok_or_else(|| RpcError::Node("no tip timestamp".into()))
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
async fn unknown_submit_outcome_survives_repeated_transport_errors_until_the_node_answers() {
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
    // never accepted: not given up at the pending timeout while nobody knows (audit 2026-11-11 M-2): the node may hold it, and
    // forgetting it would free its inputs for a batch the node refuses; resent until the node answers
    node.0.lock().unwrap().submit_replies.push_back(Err("transport: down".into()));
    node.0.lock().unwrap().daa = NOW + 5 + 700;
    let rep = r.step().await.unwrap();
    assert_eq!(r.tracker.txs.len(), 1, "still in doubt: kept");
    assert!(rep.built.is_empty(), "its orders stay busy");
    assert_eq!(node.submits(), 4);
    // the node answers: its inputs went elsewhere, dropped and the orders back off
    node.0.lock().unwrap().submit_replies.push_back(Err("transaction is an orphan where orphan is disallowed".into()));
    let rep = r.step().await.unwrap();
    assert!(r.tracker.txs.is_empty(), "the node's refusal drops it");
    assert!(rep.built.is_empty(), "the orders back off before they are planned again");
    assert_eq!(r.tracker.dropped_total, 1);
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
    assert!(text.contains("kob_matcher_funding_total_sompi 10000000000"));
    assert!(text.contains("kob_operator_low_funds 0"));
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

/// A keeper fed by a `--book-file` serves the orders the indexer proves but does not list (the snapshot's `keeperOnly`, an
/// out-of-range token, a carrier below the listing minimum): the expired IOC is killed as the in-process keeper kills it,
/// and the matcher never plans it. The file is written by hand, so the test reads the format, not a struct.
#[tokio::test]
async fn a_book_file_keeper_kills_a_keeper_only_order() {
    let node = MockNode::new(5_700);
    let mut ioc = ask(1, P250, 5 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    ioc.expiry_daa = NO_EXPIRY;
    let o = listed(cid(1), AnyState::KobAsk(ioc), CARRIER, 5_000);
    let mut b = book(vec![]);
    b.daa_score = 5_700;
    let mut json = serde_json::to_value(&b).unwrap();
    json["keeperOnly"] = serde_json::json!([serde_json::to_value(&o).unwrap()]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.json");
    std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
    let read = kobd::matcher::run::read_snapshot(&path).unwrap();
    assert!(read.orders.is_empty(), "a keeper-only order is never on the book");
    for (roles, built) in [(M, 0), (Roles { matcher: false, keeper: true }, 1)] {
        let cfg = RunConfig { roles, ..RunConfig::default() };
        let mut r = Runner::new(node.clone(), FileSource::new(&path), Box::new(signer()), cfg);
        let rep = r.step().await.unwrap();
        assert_eq!(rep.built.len(), built, "{roles:?}: {:?}", rep.built);
        if built > 0 {
            assert_eq!(rep.built[0].0, "kill");
        }
    }
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
        fee_policy: kobd::fee::FeePolicy { refresh_ms: 0, max_age_ms: 0, ..Default::default() },
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
    let m = kobd::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_fee_rate_normal 194") && m.contains("kob_fee_estimated 1"), "{m}");
    // the compute-budget safety net is exported (and never used: the table covers every shape)
    assert!(m.contains("kob_matcher_budget_slack_retries_total 0"), "{m}");
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
    let cfg = RunConfig { roles: M, fee_policy: kobd::fee::FeePolicy::fixed(100), ..RunConfig::default() };
    let mut r = Runner::new(node.clone(), ViewSource(b), Box::new(signer()), cfg);
    r.step().await.unwrap();
    assert_eq!(r.tracker.txs.values().next().unwrap().fee_rate, 100);
    assert_eq!(node.0.lock().unwrap().fee_calls, 0);
}

/// The weighted selection (`docs/spec/matcher.md` §3.6) in the loop: alone the matcher plans as the reference; a lost race
/// lowers its share, the crossing it then leaves for others is planned again at the latest once it has been crossable for
/// `max_defer_daa`, and the metrics show the selection.
#[tokio::test]
async fn a_lost_race_lowers_the_share_and_the_bound_still_plans_the_crossing() {
    use kobd::matcher::select::SelectConfig;
    let node = MockNode::new(NOW + 5);
    node.0.lock().unwrap().submit_replies.push_back(Err("output ab:0 already spent by transaction cd in the mempool".into()));
    let cfg = RunConfig {
        roles: M,
        select: SelectConfig { enabled: true, seed: Some(9), ..SelectConfig::default() },
        ..RunConfig::default()
    };
    let mut r = Runner::new(node.clone(), ViewSource(crossing_book()), Box::new(signer()), cfg);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.built.len(), 1, "share 1: the reference planner");
    assert_eq!(rep.conflicts.len(), 1);
    assert!(r.selector.share_match < 1.0, "a lost race lowers the share: {}", r.selector.share_match);
    let m = kobd::matcher::run::metrics_text(&r, &rep);
    for line in ["kob_select_enabled 1", "kob_select_lost_steps_match_total 1", "kob_select_share_keep 1"] {
        assert!(m.contains(line), "{line}: {m}");
    }
    // after the back-off the crossing is a work unit the sample may leave; once crossable for 30 DAA it is mandatory
    let mut built_at = None;
    for k in 0..6u64 {
        let daa = NOW + 30 + 10 * k;
        node.0.lock().unwrap().daa = daa;
        let rep = r.step().await.unwrap();
        if !rep.built.is_empty() {
            built_at = Some(daa);
            break;
        }
    }
    let at = built_at.expect("planned again");
    assert!(at <= NOW + 60, "within the starvation bound of its first sampled tick: {at}");
    assert!(r.selector.stats.sampled_ticks >= 1);
    let m = kobd::matcher::run::metrics_text(&r, &r.last.clone());
    assert!(m.contains("kob_select_units_total"), "{m}");
}

/// Audit 2026-11-11 M-2: a mempool double spend that names the operator's own funding coin (a transaction this process does not
/// track holds it) backs off no order: the coin is set aside and the next step plans the same crossing over the other coin.
#[tokio::test]
async fn a_funding_coin_a_mempool_transaction_holds_is_set_aside_not_the_orders() {
    let node = MockNode::new(NOW + 5);
    let (big, small) = ([0xf1; 32], [0xf2; 32]);
    {
        let mut s = node.0.lock().unwrap();
        s.funding[0].amount = 300 * KAS;
        let mut other = s.funding[0].clone();
        (other.transaction_id, other.amount) = (small, 100 * KAS);
        s.funding.push(other);
        s.submit_replies.push_back(Err(format!(
            "output ({}, 0) already spent by transaction {} in the mempool",
            kob_protocol::json::to_hex(&big),
            "cd".repeat(32)
        )));
    }
    let mut r = runner(node.clone(), crossing_book(), M);
    let rep = r.step().await.unwrap();
    assert_eq!((rep.built.len(), rep.conflicts.len()), (1, 1));
    assert!(r.tracker.backoff.is_empty(), "no order backs off for the operator's own coin");
    assert!(r.tracker.held.contains_key(&(big, 0)));
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1, "planned again at once over the other coin");
    let t = r.tracker.txs.values().next().unwrap();
    assert!(t.spends.contains(&(small, 0)) && !t.spends.contains(&(big, 0)), "{:?}", t.spends);
    let m = kobd::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_matcher_held_coins 1") && m.contains("kob_matcher_held_coins_total 1"), "{m}");
    // the node no longer lists the coin (its spender was mined): released
    node.0.lock().unwrap().funding.retain(|u| u.transaction_id != big);
    r.step().await.unwrap();
    assert!(r.tracker.held.is_empty());
}

/// Audit 2026-11-11 M-14: the local clock is compared with the chain's; beyond the bound the day orders near their deadline are
/// not filled (whichever clock is right, a fill could fall after the maker's deadline), and the alarm is in the metrics.
#[tokio::test]
async fn a_day_order_near_its_deadline_is_left_alone_while_the_clock_is_off() {
    let now_ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let node = MockNode::new(NOW + 5);
    // the local clock 45 s ahead of the chain's: beyond the bound (30 s), within twice it
    node.0.lock().unwrap().tip_ms = Some(now_ms() - 45_000);
    let mut b = crossing_book();
    b.orders[0].deadline = Some(kobd::matcher::book::utc_now() + 60);
    let mut r = runner(node.clone(), b.clone(), M);
    // a dry run plans every step anew (nothing is submitted), so each step shows whether the day order is planned
    r.cfg.dry_run = true;
    // a block's timestamp is its miner's: a skew this close to the bound waits for three samples
    for _ in 0..kobd::matcher::run::CLOCK_SAMPLES_MIN - 1 {
        r.clock_checked = None;
        let rep = r.step().await.unwrap();
        assert_eq!(rep.built.len(), 1, "planned: no judgment on fewer samples");
        assert!(!r.clock_alarm());
    }
    r.clock_checked = None;
    let rep = r.step().await.unwrap();
    assert!(rep.built.is_empty(), "the day order is left alone");
    assert!(r.clock_alarm() && r.clock_skew.is_some_and(|s| (44..=46).contains(&s)), "{:?}", r.clock_skew);
    // audit round 4 R4-L-5: two minutes ahead, far beyond the bound, is judged at the first sample after the start
    node.0.lock().unwrap().tip_ms = Some(now_ms() - 120_000);
    let mut r = runner(node.clone(), b, M);
    r.cfg.dry_run = true;
    let rep = r.step().await.unwrap();
    assert!(rep.built.is_empty(), "the day order is left alone from the first step");
    assert!(r.clock_alarm() && r.clock_skew.is_some_and(|s| (119..=121).contains(&s)), "{:?}", r.clock_skew);
    let m = kobd::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_clock_skew_alarm 1"), "{m}");
    assert!(r.executor_health(&rep).clock_alarm);
    let f = r.status_facts(&rep);
    let line = kobd::matcher::run::status_line(&f);
    // the samples span no time yet: the clock is ahead, or every node is behind; the line names both
    assert!(line.contains("the local clock is 1") && line.contains("every node is behind"), "{line}");
    // the clock agrees again: once the median of the last samples does, planned again
    node.0.lock().unwrap().tip_ms = Some(now_ms());
    let mut rep = r.step().await.unwrap();
    for _ in 0..3 {
        r.clock_checked = None;
        rep = r.step().await.unwrap();
    }
    assert_eq!(rep.built.len(), 1);
    assert!(!r.clock_alarm());
}

/// The founder: "block time is a lie". One node's timestamp never decides: a sample takes the node tip nearest the local
/// clock (a node an hour off, or one that stopped advancing, is outvoted by one that agrees), and the median of the samples
/// is judged, so one odd block (a single lying sample) neither raises the alarm nor holds day orders back.
#[test]
fn a_lying_block_timestamp_neither_raises_the_clock_alarm_nor_holds_day_orders() {
    use kobd::matcher::run::{chain_skew_secs, median};
    let local = 1_790_000_000_000u64;
    assert_eq!(chain_skew_secs(local, &[local + 3_600_000, local - 2_000]), Some(2), "the liar an hour ahead is outvoted");
    assert_eq!(chain_skew_secs(local, &[local - 900_000, local + 1_000]), Some(-1), "a node 15 min behind is outvoted");
    assert_eq!(chain_skew_secs(local, &[local - 120_000, local - 121_000]), Some(120), "every node shows it: it counts");
    assert_eq!(chain_skew_secs(local, &[]), None);
    let v = |s: &[i64]| s.iter().copied().collect::<std::collections::VecDeque<i64>>();
    assert_eq!(median(&v(&[0, 3_600, 0, 1, 0])), Some(0), "one odd sample does not move the median");
    assert_eq!(median(&v(&[120, 0, 120])), Some(120));
    assert_eq!(median(&v(&[])), None);
}

/// Audit round 4 R4-L-5: the alarm does not blame the local clock when every node is behind it and the chain stands still (a
/// stall or a partition): a sample grows with the local time then, while a clock that is merely off keeps it level.
#[test]
fn the_clock_alarm_names_the_nodes_when_the_chain_stands_still() {
    use kobd::matcher::run::{clock_cause, ClockCause};
    let p = |s: &[(u64, i64)]| s.iter().copied().collect::<std::collections::VecDeque<(u64, i64)>>();
    let t = 1_790_000_000_000u64;
    // every sample 120 s, over two minutes: the chain advances with the local time, the clock is ahead
    let level = p(&[(t, 120), (t + 60_000, 120), (t + 120_000, 121)]);
    assert_eq!(clock_cause(120, &level), ClockCause::ClockAhead);
    // the sample grows with the local time: the chain stands still, the nodes are behind
    let still = p(&[(t, 40), (t + 60_000, 100), (t + 120_000, 160)]);
    assert_eq!(clock_cause(100, &still), ClockCause::NodesBehind);
    // too short a span to tell (the start's samples a few seconds apart)
    assert_eq!(clock_cause(120, &p(&[(t, 120), (t + 5_000, 125)])), ClockCause::AheadOrNodesBehind);
    assert_eq!(clock_cause(120, &p(&[])), ClockCause::AheadOrNodesBehind);
    // every node ahead of the local clock: the local clock is behind
    assert_eq!(clock_cause(-90, &still), ClockCause::ClockBehind);
    assert!(ClockCause::NodesBehind.describe(100).contains("the nodes are behind: check them"));
    assert!(!ClockCause::NodesBehind.describe(100).contains("NTP"), "the local clock is not blamed");
    assert!(ClockCause::ClockAhead.describe(120).contains("set it: NTP"));
}

/// Audit round 2 R2-M-2: a batch carries the deadline of the day order it fills, and a resend is checked against it: once the
/// deadline is within the clock margin, the batch is not sent again (counted in the metrics), its inputs stay reserved.
#[tokio::test]
async fn a_batch_past_a_day_orders_deadline_is_not_sent_again() {
    let node = MockNode::new(NOW + 5);
    let mut b = crossing_book();
    let deadline = kobd::matcher::book::utc_now() + 3_600;
    b.orders[0].deadline = Some(deadline);
    let mut r = runner(node.clone(), b, M);
    let rep = r.step().await.unwrap();
    assert_eq!((rep.submitted.len(), node.submits()), (1, 1));
    let t = r.tracker.txs.values().next().unwrap().clone();
    assert_eq!(t.deadline, Some(deadline), "the batch knows the deadline of the day order it fills");
    // no block accepts it; the deadline comes within the margin before its recheck
    r.tracker.txs.get_mut(&t.txid).unwrap().deadline = Some(kobd::matcher::book::utc_now() + 10);
    node.0.lock().unwrap().daa = NOW + 5 + 700;
    let rep = r.step().await.unwrap();
    assert_eq!(node.submits(), 1, "not sent again");
    assert!(rep.built.is_empty(), "its orders stay reserved");
    assert!(r.tracker.txs[&t.txid].retired.is_some());
    let m = kobd::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_matcher_resend_dropped_total 1"), "{m}");
}

/// Audit round 2 R2-L-7: with `report_funds` off (a read API other hosts can reach) the balance is left out of what the book
/// source serves (`/v1/health`, `/v1/metrics`), and the rest stays.
#[test]
fn the_funding_series_are_left_out_for_a_public_listener() {
    let m = "# HELP kob_matcher_funding_sompi x\n# TYPE kob_matcher_funding_sompi gauge\nkob_matcher_funding_sompi 5\n\
             # TYPE kob_matcher_funding_total_sompi gauge\nkob_matcher_funding_total_sompi 6\n\
             # TYPE kob_matcher_pending_txs gauge\nkob_matcher_pending_txs 2\nkob_operator_low_funds 0\n";
    let out = kobd::matcher::run::without_funds(m);
    assert_eq!(out, "# TYPE kob_matcher_pending_txs gauge\nkob_matcher_pending_txs 2\nkob_operator_low_funds 0\n");
}

/// A book source whose indexer halted (`BookSource::fatal`).
struct Halted(MemoryBook, bool);

impl kobd::matcher::run::BookSource for Halted {
    fn snapshot(&mut self, _operator: &[u8; 32]) -> anyhow::Result<MemoryBook> {
        Ok(self.0.clone())
    }
    fn fatal(&mut self) -> Option<(bool, String)> {
        Some((self.1, "the indexer is stopped: disk full".into()))
    }
}

/// Audit 2026-11-11 M-4: a halted indexer (or a dead read API) ends the runner with an error once it lasts (so a supervisor
/// restarts the process), at once for a dead API.
#[tokio::test]
async fn a_halted_book_source_ends_the_run_with_an_error() {
    for (now, wait) in [(true, std::time::Duration::from_secs(3600)), (false, std::time::Duration::from_millis(300))] {
        let node = MockNode::new(NOW + 5);
        let cfg = RunConfig { roles: M, tick: std::time::Duration::from_millis(100), halt_exit_after: wait, ..RunConfig::default() };
        let mut r = Runner::new(node, Halted(crossing_book(), now), Box::new(signer()), cfg);
        let t0 = std::time::Instant::now();
        let e =
            tokio::time::timeout(std::time::Duration::from_secs(10), r.run(std::future::pending())).await.expect("ended").unwrap_err();
        assert!(e.to_string().contains("disk full") && e.to_string().contains("supervisor"), "{e}");
        assert!(now || t0.elapsed() >= wait);
    }
}
