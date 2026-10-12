//! Keeper liveness under fee pressure (`docs/ops/executor.md`, fee policy): a kill or refund whose refund tip pays it at the
//! floor but not at its urgency's rate is topped up from the operator's own funding (bounded by `subsidy_max`, the total cap
//! and the highest rate; the maker's outputs never change), and a kill or refund of this process that stays unaccepted is
//! rebuilt at a higher rate and replaced with `submitTransactionReplacement`, a bounded number of times, never a
//! transaction this process did not submit.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use common::*;
use kob_executor::fee::{FeePolicy, FeeRates};
use kob_executor::keepers::{tick, Job, JobKind, KeeperConfig, KeeperInput, KeeperReport};
use kob_executor::matcher::book::{Clock, ListedOrder, MemoryBook};
use kob_executor::matcher::node::*;
use kob_executor::matcher::run::{Roles, RunConfig, Runner, ViewSource};
use kob_executor::matcher::wallet::p2pk_spk_string;
use kob_protocol::state::*;
use kob_protocol::tx::TxOutputJson;
use serde_json::Value;

const KILL_DAA: u64 = 5_600;

/// An IOC ask listed at DAA 5,000 (killable from 5,600) whose refund tip is `tip`.
fn ioc(n: u32, tip: i64) -> ListedOrder {
    let mut s = ask(1, P250, 5 * WHOLE, T3);
    s.tif = TIF_IOC;
    s.expiry_daa = NO_EXPIRY;
    s.refund_tip = tip;
    listed(cid(n), AnyState::KobAsk(s), CARRIER, 5_000)
}

fn keep(orders: Vec<ListedOrder>, cfg: &KeeperConfig) -> KeeperReport {
    let inp = KeeperInput {
        orders,
        clock: Clock { daa: KILL_DAA, utc: UTC },
        funding: vec![funding(20 * KAS)],
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
        known_unprofitable: BTreeSet::new(),
    };
    let r = tick(&inp, cfg, &signer());
    for j in &r.jobs {
        assert!(j.validation.is_some(), "{:?} not engine-validated", j.kind);
    }
    r
}

fn high(rate: u64, subsidy_max: u64, max_tx_fee: u64) -> KeeperConfig {
    KeeperConfig {
        fees: FeeRates { floor: 100, low: 100, normal: 100, high: rate, max_tx_fee, estimated: true },
        subsidy_max,
        ..KeeperConfig::default()
    }
}

/// Every output of a job but the operator's change: what the order's covenant pays out.
fn covenant_outputs(j: &Job) -> Vec<TxOutputJson> {
    let change = j.built.fee.change_output.map(|i| i as usize);
    j.built.tx.outputs.iter().enumerate().filter(|(i, _)| Some(*i) != change).map(|(_, o)| o.clone()).collect()
}

/// The fee of the kill of `ioc(1, ..)` at the floor (the tip does not matter to the fee).
fn floor_fee() -> u64 {
    let r = keep(vec![ioc(1, 10 * KAS as i64)], &high(100, 0, 0));
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    r.jobs[0].built.fee.fee
}

#[test]
fn a_kill_its_tip_cannot_pay_at_the_high_rate_is_topped_up_by_the_operator() {
    let f0 = floor_fee();
    // a tip that pays the floor twice over: about 200 sompi per gram, far below the priority rate 2,000
    let tip = 2 * f0 as i64;
    let plain = keep(vec![ioc(1, tip)], &high(2_000, 0, 0));
    assert_eq!(plain.jobs.len(), 1, "{:?}", plain.skipped);
    let p = &plain.jobs[0];
    assert_eq!(p.kind, JobKind::Kill);
    assert!(
        p.built.fee.fee_rate > 100 && p.built.fee.fee_rate < 2_000,
        "the subsidy off: the rate its tip pays ({})",
        p.built.fee.fee_rate
    );
    assert!(p.profit >= 0 && p.subsidy == 0);
    // the subsidy on: the priority rate, the operator pays the difference out of its funding
    let cap = 10 * KAS;
    let sub = keep(vec![ioc(1, tip)], &high(2_000, cap, 0));
    assert_eq!(sub.jobs.len(), 1, "{:?}", sub.skipped);
    let s = &sub.jobs[0];
    assert_eq!(s.built.fee.fee_rate, 2_000);
    assert!(s.profit < 0, "the operator pays: profit {}", s.profit);
    assert_eq!(s.subsidy, s.profit.unsigned_abs());
    assert!(s.subsidy <= cap);
    // fee = tip + subsidy: nothing else pays it
    assert_eq!(s.built.fee.fee as i64, tip + s.subsidy as i64);
    // the maker's outputs are exactly the covenant's (value - refundTip), whatever the fee
    assert_eq!(covenant_outputs(s), covenant_outputs(p), "the maker's outputs do not depend on the subsidy");
    let kas_out: u64 = covenant_outputs(s).iter().map(|o| o.value).sum();
    assert_eq!(kas_out, CARRIER - tip as u64 + CARRIER, "the order's value less its tip, plus the custody carrier");
    // the top-up comes from the operator's P2PK funding input only
    let kob_protocol::build::Action::RefundOrder(req) = &s.action else { panic!() };
    assert_eq!(req.funding.len(), 1);
    assert!(s
        .built
        .tx
        .inputs
        .iter()
        .any(|i| (i.transaction_id, i.index) == (req.funding[0].utxo.transaction_id, req.funding[0].utxo.index)));
}

#[test]
fn the_subsidy_is_bounded_by_its_cap_and_the_total_fee_cap() {
    let f0 = floor_fee();
    let tip = 2 * f0 as i64;
    // a cap smaller than the priority rate needs: the highest rate the cap reaches, never more than the cap
    let cap = f0 / 2;
    let r = keep(vec![ioc(1, tip)], &high(2_000, cap, 0));
    let j = &r.jobs[0];
    let plain = keep(vec![ioc(1, tip)], &high(2_000, 0, 0)).jobs[0].built.fee.fee_rate;
    assert!(j.built.fee.fee_rate > plain && j.built.fee.fee_rate < 2_000, "rate {} (plain {plain})", j.built.fee.fee_rate);
    assert!(j.subsidy > 0 && j.subsidy <= cap, "subsidy {} within {cap}", j.subsidy);
    // `--fee-max-tx-kas`: the total fee stays within it whatever the subsidy allows
    let max_tx_fee = 3 * f0;
    let r = keep(vec![ioc(1, tip)], &high(2_000, 10 * KAS, max_tx_fee));
    let j = &r.jobs[0];
    assert!(j.built.fee.fee <= max_tx_fee, "fee {} within the cap {max_tx_fee}", j.built.fee.fee);
    assert!(j.built.fee.fee_rate < 2_000 && j.subsidy > 0);
    // a tip that does not pay even the floor is never subsidised: the order's end is not the operator's to pay
    let r = keep(vec![ioc(2, f0 as i64 / 2)], &high(2_000, 10 * KAS, 0));
    assert!(r.jobs.is_empty(), "{:?}", r.jobs.iter().map(|j| (j.kind, j.profit)).collect::<Vec<_>>());
    // closes and sweeps are never subsidised (their urgency is low; here the low rate is the floor anyway), refunds are
    let mut gtd = ask(3, P250, 5 * WHOLE, T3);
    gtd.refund_tip = tip;
    gtd.expiry_daa = 5_500;
    let refund = listed(cid(3), AnyState::KobAsk(gtd), CARRIER, 5_000);
    let cfg = KeeperConfig {
        fees: FeeRates { floor: 100, low: 100, normal: 2_000, high: 2_000, max_tx_fee: 0, estimated: true },
        subsidy_max: 10 * KAS,
        ..KeeperConfig::default()
    };
    let r = keep(vec![refund], &cfg);
    assert_eq!((r.jobs[0].kind, r.jobs[0].built.fee.fee_rate), (JobKind::Refund, 2_000));
    assert!(r.jobs[0].subsidy > 0);
}

// ---------------------------------------------------------------------------------------------
// replace-by-fee of this process's own stuck kills and refunds

#[derive(Default)]
struct State {
    daa: u64,
    submits: Vec<Value>,
    submit_replies: VecDeque<Result<(), String>>,
    replacements: Vec<Value>,
    replace_replies: VecDeque<Result<(), String>>,
    funding: Vec<AddressUtxo>,
    fee: Option<Value>,
}

#[derive(Clone, Default)]
struct MockNode(Arc<Mutex<State>>);

impl MockNode {
    fn new(daa: u64, priority: f64) -> Self {
        let n = MockNode::default();
        {
            let mut s = n.0.lock().unwrap();
            s.daa = daa;
            s.funding = vec![AddressUtxo {
                transaction_id: [0xf1; 32],
                index: 0,
                amount: 100 * KAS,
                script_public_key: p2pk_spk_string(&pk(MATCHER)),
                block_daa_score: 500,
                covenant_id: None,
            }];
            s.fee = Some(serde_json::json!({"estimate": {
                "priorityBucket": {"feerate": priority, "estimatedSeconds": 0.5},
                "normalBuckets": [{"feerate": 100.0, "estimatedSeconds": 40.0}],
                "lowBuckets": [{"feerate": 100.0, "estimatedSeconds": 900.0}]
            }}));
        }
        n
    }
    fn set_daa(&self, daa: u64) {
        self.0.lock().unwrap().daa = daa;
    }
    fn replacements(&self) -> usize {
        self.0.lock().unwrap().replacements.len()
    }
}

impl NodeApi for MockNode {
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
    async fn submit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let mut s = self.0.lock().unwrap();
        s.submits.push(tx);
        match s.submit_replies.pop_front() {
            Some(Err(m)) => Err(RpcError::Node(m)),
            _ => Ok([0; 32]),
        }
    }
    async fn submit_replacement(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let mut s = self.0.lock().unwrap();
        s.replacements.push(tx);
        match s.replace_replies.pop_front() {
            Some(Err(m)) if m.starts_with("transport:") => Err(RpcError::Transport(m)),
            Some(Err(m)) => Err(RpcError::Node(m)),
            _ => Ok([0; 32]),
        }
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        Ok(ChainUpdate::default())
    }
    async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        Ok(self.0.lock().unwrap().funding.clone())
    }
    async fn fee_estimate(&self) -> Result<kob_executor::fee::FeeEstimate, RpcError> {
        let s = self.0.lock().unwrap();
        kob_executor::fee::FeeEstimate::parse(s.fee.as_ref().expect("estimate")).map_err(RpcError::Decode)
    }
}

const K: Roles = Roles { matcher: false, keeper: true };

fn runner(node: MockNode, orders: Vec<ListedOrder>, roles: Roles, max_replacements: u32) -> Runner<MockNode, ViewSource<MemoryBook>> {
    let mut b = book(orders);
    b.daa_score = node.0.lock().unwrap().daa;
    let mut cfg = RunConfig {
        roles,
        fee_policy: FeePolicy { refresh_ms: 0, max_age_ms: u64::MAX / 4, max_rate: 1_000, ..FeePolicy::default() },
        max_book_lag: None,
        ..RunConfig::default()
    };
    cfg.keeper.subsidy_max = KAS;
    cfg.keeper.replace_after_daa = 100;
    cfg.keeper.max_replacements = max_replacements;
    Runner::new(node, ViewSource(b), Box::new(signer()), cfg)
}

/// The outpoints a transaction spends.
type Spends = BTreeSet<([u8; 32], u32)>;

/// The transaction the runner follows for the one keeper job: (txid, fee rate, spends).
fn tracked(r: &Runner<MockNode, ViewSource<MemoryBook>>) -> ([u8; 32], u64, Spends) {
    assert_eq!(r.tracker.txs.len(), 1, "one tracked transaction");
    let t = r.tracker.txs.values().next().unwrap();
    (t.txid, t.fee_rate, t.spends.clone())
}

#[tokio::test]
async fn a_stuck_kill_is_replaced_at_a_higher_rate_at_most_k_times() {
    let node = MockNode::new(KILL_DAA, 200.0);
    let order = ioc(1, 10 * KAS as i64);
    let order_op = (order.order.utxo.transaction_id, order.order.utxo.index);
    let mut r = runner(node.clone(), vec![order], K, 2);
    let rep = r.step().await.unwrap();
    assert_eq!((rep.built.len(), rep.submitted.len()), (1, 1));
    let (t0, rate0, spends0) = tracked(&r);
    assert_eq!(rate0, 200, "a kill at the priority rate");
    // not yet stuck: 99 DAA later nothing is replaced
    node.set_daa(KILL_DAA + 99);
    let rep = r.step().await.unwrap();
    assert!(rep.replaced.is_empty() && rep.built.is_empty());
    assert_eq!(node.replacements(), 0);
    // stuck for 100 DAA: rebuilt at a higher rate and replaced (the same order input and funding)
    node.set_daa(KILL_DAA + 100);
    let rep = r.step().await.unwrap();
    assert_eq!(node.replacements(), 1);
    let (t1, rate1, spends1) = tracked(&r);
    assert_eq!(rep.replaced, vec![(t0, t1)]);
    assert!(rate1 > rate0, "{rate1} > {rate0}");
    assert_eq!(rate1, 250, "a quarter above the stuck rate (the estimate did not move)");
    assert!(spends1.contains(&order_op) && spends0.contains(&order_op), "the same order input");
    assert_eq!(spends1, spends0, "the same inputs (the funding included)");
    assert!(rep.built.is_empty() && rep.submitted.is_empty(), "nothing else is built over the order");
    // the estimate rises: the next replacement takes it
    node.0.lock().unwrap().fee = Some(serde_json::json!({"estimate": {"priorityBucket": {"feerate": 700.0}}}));
    node.set_daa(KILL_DAA + 200);
    r.step().await.unwrap();
    let (_, rate2, _) = tracked(&r);
    assert_eq!(rate2, 700);
    // K = 2: no third replacement, the job waits for the pending timeout as before
    node.set_daa(KILL_DAA + 300);
    r.step().await.unwrap();
    assert_eq!(node.replacements(), 2);
    assert_eq!(r.keeper_replaced, 2);
    let m = kob_executor::matcher::run::metrics_text(&r, &r.last.clone());
    assert!(m.contains("kob_keeper_replaced_total 2"), "{m}");
    // dropped at the pending timeout (600 DAA after the first submission), the order backs off as before
    node.set_daa(KILL_DAA + 600);
    r.step().await.unwrap();
    assert!(r.tracker.txs.is_empty());
    assert!(r.keeper_inflight.is_empty());
}

#[tokio::test]
async fn a_refused_replacement_falls_back_to_the_pending_timeout() {
    let node = MockNode::new(KILL_DAA, 200.0);
    node.0.lock().unwrap().replace_replies.push_back(Err("transaction aa is missing outpoint (bb, 0)".into()));
    let mut r = runner(node.clone(), vec![ioc(1, 10 * KAS as i64)], K, 3);
    r.step().await.unwrap();
    let (t0, rate0, _) = tracked(&r);
    node.set_daa(KILL_DAA + 100);
    let rep = r.step().await.unwrap();
    assert_eq!(node.replacements(), 1);
    assert!(rep.replaced.is_empty());
    assert_eq!(tracked(&r).0, t0, "the original stays tracked");
    assert_eq!(tracked(&r).1, rate0);
    assert_eq!(r.keeper_replace_refused, 1);
    // never tried again; the pending timeout drops it and the order backs off
    node.set_daa(KILL_DAA + 300);
    r.step().await.unwrap();
    assert_eq!(node.replacements(), 1, "a refused job is not replaced again");
    node.set_daa(KILL_DAA + 600);
    let rep = r.step().await.unwrap();
    assert!(r.tracker.txs.is_empty() && rep.built.is_empty(), "dropped, the order backs off");
    assert!(r.tracker.backed_off(KILL_DAA + 600).contains(&cid(1)));
}

#[tokio::test]
async fn a_replacement_without_a_verdict_follows_both() {
    let node = MockNode::new(KILL_DAA, 200.0);
    node.0.lock().unwrap().replace_replies.push_back(Err("transport: reset".into()));
    let mut r = runner(node.clone(), vec![ioc(1, 10 * KAS as i64)], K, 3);
    r.step().await.unwrap();
    node.set_daa(KILL_DAA + 100);
    r.step().await.unwrap();
    assert_eq!(r.tracker.txs.len(), 2, "the original and the doubtful replacement");
    assert_eq!(r.tracker.txs.values().filter(|t| t.unconfirmed).count(), 1);
    node.set_daa(KILL_DAA + 300);
    r.step().await.unwrap();
    assert_eq!(node.replacements(), 1, "no further replacement");
}

#[tokio::test]
async fn only_transactions_this_process_submitted_are_ever_replaced() {
    // another party's kill holds the order in the mempool: ours is refused as a double spend and never tracked, so never
    // replaced (replacing would evict someone else's transaction)
    let node = MockNode::new(KILL_DAA, 200.0);
    let theirs = format!("output ({}, 0) already spent by transaction {} in the mempool", "ab".repeat(32), "cd".repeat(32));
    for _ in 0..8 {
        node.0.lock().unwrap().submit_replies.push_back(Err(theirs.clone()));
    }
    let mut r = runner(node.clone(), vec![ioc(1, 10 * KAS as i64)], K, 3);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.conflicts.len(), 1);
    assert!(r.keeper_inflight.is_empty());
    for d in [100, 200, 300, 400] {
        node.set_daa(KILL_DAA + d);
        let rep = r.step().await.unwrap();
        assert!(rep.submitted.is_empty() && r.keeper_inflight.is_empty());
    }
    assert!(node.0.lock().unwrap().submits.len() >= 3, "our kill is retried after its backoff, by plain submission");
    assert_eq!(node.replacements(), 0, "nobody else's transaction is replaced");
    // a matcher batch is never replaced either (only keeper kills and refunds are)
    let node = MockNode::new(NOW + 5, 200.0);
    let orders = vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))];
    let mut r = runner(node.clone(), orders, Roles { matcher: true, keeper: true }, 3);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1);
    assert_eq!(r.tracker.txs.values().next().unwrap().kind, "match");
    node.set_daa(NOW + 300);
    r.step().await.unwrap();
    assert_eq!(node.replacements(), 0);
    // replacement off (K = 0): a stuck kill waits for the pending timeout
    let node = MockNode::new(KILL_DAA, 200.0);
    let mut r = runner(node.clone(), vec![ioc(1, 10 * KAS as i64)], K, 0);
    r.step().await.unwrap();
    node.set_daa(KILL_DAA + 300);
    r.step().await.unwrap();
    assert_eq!(node.replacements(), 0);
}

#[tokio::test]
async fn a_subsidised_kill_is_counted_in_the_metrics() {
    // the tip pays the floor but not the priority rate 1,000: the operator tops it up
    let f0 = floor_fee();
    let node = MockNode::new(KILL_DAA, 1_000.0);
    let mut r = runner(node.clone(), vec![ioc(1, 2 * f0 as i64)], K, 3);
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 1);
    assert_eq!(tracked(&r).1, 1_000);
    assert_eq!(r.keeper_subsidised, 1);
    assert!(r.keeper_subsidy_sompi > 0 && r.keeper_subsidy_sompi <= KAS);
    let m = kob_executor::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_keeper_subsidised_total 1"), "{m}");
}

// ---------------------------------------------------------------------------------------------
// the rolling subsidy budget (`--keeper-subsidy-budget-kas` per `--keeper-subsidy-window-daa`, `--keeper-subsidy-max-jobs`):
// without it, a flood of small IOC / GTD orders whose tips pay only the floor, expiring under fee pressure, drains the
// operator's funding `subsidy_max` at a time

/// An IOC ask listed at `daa` (killable from `daa + 600`) whose refund tip is `tip`.
fn ioc_at(n: u32, tip: i64, daa: u64) -> ListedOrder {
    let o = ioc(n, tip);
    listed(cid(n), o.order.state, CARRIER, daa)
}

/// A keeper runner (no replacement unless asked) with the given budget and window, over `orders`.
fn budgeted(
    node: MockNode,
    orders: Vec<ListedOrder>,
    max_replacements: u32,
    budget: u64,
    window: u64,
) -> Runner<MockNode, ViewSource<MemoryBook>> {
    let mut r = runner(node, orders, K, max_replacements);
    r.cfg.keeper.subsidy_budget = budget;
    r.cfg.keeper.subsidy_window_daa = window;
    r
}

/// (fee rate, operator subsidy) of every tracked keeper transaction submitted at `daa` over one of `ids`, by order.
fn jobs_at(r: &Runner<MockNode, ViewSource<MemoryBook>>, daa: u64, ids: &[u32]) -> Vec<(u64, u64)> {
    let want: BTreeSet<[u8; 32]> = ids.iter().map(|n| cid(*n)).collect();
    let mut v: Vec<([u8; 32], u64, u64)> = r
        .tracker
        .txs
        .values()
        .filter(|t| t.submitted_daa == daa && t.orders.iter().any(|o| want.contains(o)))
        .map(|t| (*t.orders.iter().next().unwrap(), t.fee_rate, if t.profit < 0 { t.profit.unsigned_abs() } else { 0 }))
        .collect();
    v.sort();
    v.into_iter().map(|(_, rate, s)| (rate, s)).collect()
}

/// Subsidy of the kill of `ioc(1, tip)` at the priority rate `rate` (one funding input and a change output, as in the runner).
fn full_subsidy(tip: i64, rate: u64) -> u64 {
    let r = keep(vec![ioc(1, tip)], &high(rate, KAS, 0));
    assert_eq!(r.jobs[0].built.fee.fee_rate, rate);
    r.jobs[0].subsidy
}

/// The rate the tip alone pays (the subsidy off).
fn own_rate(tip: i64, rate: u64) -> u64 {
    keep(vec![ioc(1, tip)], &high(rate, 0, 0)).jobs[0].built.fee.fee_rate
}

fn count(jobs: &[(u64, u64)], want: (u64, u64)) -> usize {
    jobs.iter().filter(|j| **j == want).count()
}

#[tokio::test]
async fn the_subsidy_budget_bounds_the_operator_across_ticks_and_rolls_with_the_window() {
    let tip = 2 * floor_fee() as i64;
    let s1 = full_subsidy(tip, 1_000);
    let plain = own_rate(tip, 1_000);
    assert!(s1 > 0 && plain < 1_000);
    const W: u64 = 1_000;
    // a budget of exactly two full subsidies per 1,000 DAA; five floor-tip kills due at once
    let node = MockNode::new(KILL_DAA, 1_000.0);
    // funding for every tick (the node keeps reporting the UTXOs; the runner leaves out those its pending jobs spend), so
    // only the budget can deny a subsidy
    {
        let mut s = node.0.lock().unwrap();
        let f = s.funding[0].clone();
        s.funding = (0..12u8).map(|i| AddressUtxo { transaction_id: [0xf0 ^ i; 32], ..f.clone() }).collect();
    }
    let mut orders: Vec<ListedOrder> = (1..=5).map(|n| ioc_at(n, tip, 5_000)).collect();
    orders.extend((11..=13).map(|n| ioc_at(n, tip, 5_100))); // due at 5,700 (inside the window)
    orders.extend((21..=23).map(|n| ioc_at(n, tip, 6_000))); // due at 6,600 (the first charges left the window)
    let mut r = budgeted(node.clone(), orders, 0, 2 * s1, W);
    r.source.0.daa_score = KILL_DAA;
    let rep = r.step().await.unwrap();
    assert_eq!(rep.submitted.len(), 5, "{:?}", rep.skipped);
    let first = jobs_at(&r, KILL_DAA, &[1, 2, 3, 4, 5]);
    assert_eq!(first.len(), 5);
    assert_eq!(count(&first, (1_000, s1)), 2, "two kills subsidised to the priority rate: {first:?}");
    assert_eq!(count(&first, (plain, 0)), 3, "the rest at the rate their tip pays: {first:?}");
    assert_eq!(r.keeper_subsidy.spent(&r.cfg.keeper, KILL_DAA), 2 * s1);
    assert_eq!(r.keeper_subsidy.left(&r.cfg.keeper, KILL_DAA), 0);
    assert_eq!((r.keeper_subsidised, r.keeper_subsidy_sompi), (2, 2 * s1));
    let m = kob_executor::matcher::run::metrics_text(&r, &rep);
    assert!(m.contains("kob_keeper_subsidy_budget_left_sompi 0"), "{m}");
    // the next tick inside the window: the budget is still spent, the kills go at their own rate (subsidy_max = 0 behaviour)
    node.set_daa(KILL_DAA + 100);
    r.source.0.daa_score = KILL_DAA + 100;
    r.step().await.unwrap();
    let second = jobs_at(&r, KILL_DAA + 100, &[11, 12, 13]);
    assert_eq!(second, vec![(plain, 0); 3], "budget spent: no subsidy inside the window");
    assert_eq!(r.keeper_subsidy_sompi, 2 * s1);
    // a full window after the first charges: subsidy resumes, again within the budget
    node.set_daa(KILL_DAA + W);
    r.source.0.daa_score = KILL_DAA + W;
    r.step().await.unwrap();
    let third = jobs_at(&r, KILL_DAA + W, &[21, 22, 23]);
    assert_eq!(count(&third, (1_000, s1)), 2, "subsidy resumes after the window: {third:?}");
    assert_eq!(count(&third, (plain, 0)), 1, "{third:?}");
    assert_eq!(r.keeper_subsidy.spent(&r.cfg.keeper, KILL_DAA + W), 2 * s1, "the old charges left the window");
    assert_eq!(r.keeper_subsidy_sompi, 4 * s1);
}

#[tokio::test]
async fn a_zero_budget_or_job_cap_means_no_subsidy_and_the_cap_bounds_one_step() {
    let tip = 2 * floor_fee() as i64;
    let plain = own_rate(tip, 1_000);
    let s1 = full_subsidy(tip, 1_000);
    // budget 0: exactly the subsidy_max = 0 behaviour
    let node = MockNode::new(KILL_DAA, 1_000.0);
    let mut r = budgeted(node, (1..=3).map(|n| ioc_at(n, tip, 5_000)).collect(), 0, 0, 36_000);
    r.step().await.unwrap();
    assert_eq!(jobs_at(&r, KILL_DAA, &[1, 2, 3]), vec![(plain, 0); 3]);
    assert_eq!((r.keeper_subsidised, r.keeper_subsidy_sompi), (0, 0));
    // job cap 0: the same
    let node = MockNode::new(KILL_DAA, 1_000.0);
    let mut r = budgeted(node, (1..=3).map(|n| ioc_at(n, tip, 5_000)).collect(), 0, KAS, 36_000);
    r.cfg.keeper.subsidy_max_jobs = 0;
    r.step().await.unwrap();
    assert_eq!(jobs_at(&r, KILL_DAA, &[1, 2, 3]), vec![(plain, 0); 3]);
    // job cap 1: one subsidised kill per step, whatever the budget left
    let node = MockNode::new(KILL_DAA, 1_000.0);
    let mut r = budgeted(node, (1..=3).map(|n| ioc_at(n, tip, 5_000)).collect(), 0, KAS, 36_000);
    r.cfg.keeper.subsidy_max_jobs = 1;
    r.step().await.unwrap();
    let mut jobs = jobs_at(&r, KILL_DAA, &[1, 2, 3]);
    jobs.sort();
    assert_eq!(jobs, vec![(plain, 0), (plain, 0), (1_000, s1)]);
    assert_eq!(r.keeper_subsidy_sompi, s1);
}

#[tokio::test]
async fn a_replacement_is_charged_its_added_subsidy_and_refused_past_the_budget() {
    let tip = 2 * floor_fee() as i64;
    let s400 = full_subsidy(tip, 400);
    let s1000 = full_subsidy(tip, 1_000);
    assert!(s1000 > s400);
    for (budget, replaced) in [(s1000, true), (s1000 - 1, false)] {
        let node = MockNode::new(KILL_DAA, 400.0);
        let mut r = budgeted(node.clone(), vec![ioc(1, tip)], 3, budget, 36_000);
        r.step().await.unwrap();
        assert_eq!(jobs_at(&r, KILL_DAA, &[1]), vec![(400, s400)]);
        assert_eq!(r.keeper_subsidy.spent(&r.cfg.keeper, KILL_DAA), s400);
        // the estimate rises; the stuck kill wants the priority rate, which needs s1000 of subsidy in all
        node.0.lock().unwrap().fee = Some(serde_json::json!({"estimate": {"priorityBucket": {"feerate": 1000.0}}}));
        node.set_daa(KILL_DAA + 100);
        let rep = r.step().await.unwrap();
        let spent = r.keeper_subsidy.spent(&r.cfg.keeper, KILL_DAA + 100);
        if replaced {
            assert_eq!(rep.replaced.len(), 1, "within the budget: replaced");
            assert_eq!(tracked(&r).1, 1_000);
            assert_eq!(spent, s1000, "the original's subsidy plus the replacement's increase");
            assert_eq!(r.keeper_subsidy_sompi, s1000);
            assert_eq!(r.keeper_subsidy.left(&r.cfg.keeper, KILL_DAA + 100), 0);
        } else {
            assert!(rep.replaced.is_empty(), "the increase does not fit the budget: not replaced");
            assert_eq!(node.replacements(), 0);
            assert_eq!((tracked(&r).1, spent), (400, s400), "the original stays, nothing more charged");
        }
    }
}
