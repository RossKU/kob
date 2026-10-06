//! The executor end to end, in both token families (KCC-20 and KRON): synthetic chain data built with
//! the kob-protocol builders goes through the indexer (follower, processor, SQLite), and the matcher and
//! keepers plan against the indexed book (`IndexerSource`), every transaction validated in the
//! rusty-kaspa v2.1.0 script engine. Acceptance, finality and reorg rollback come from the indexer's own
//! chain follower: the test node refuses `getVirtualChainFromBlockV2`, so a second follower in the runner
//! would fail these tests.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kob_executor::executor::IndexerSource;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book::{snapshot, StoreBook};
use kob_executor::matcher::book::{ListedOrder, OrderBookView};
use kob_executor::matcher::node::{AddressUtxo, DagInfo, NodeApi, RpcError, ServerInfo};
use kob_executor::matcher::run::{Roles, RunConfig, Runner, StepReport};
use kob_executor::matcher::wallet::{p2pk_spk_string, LocalKeys};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::registry::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use serde_json::Value;

const FAMS: [Family; 2] = [Family::Kcc20, Family::Kron];

/// The operator's node: the mock chain of the indexer harness plus a funding coin and a submission log.
#[derive(Clone)]
struct ExecNode {
    chain: Arc<MockNode>,
    funding: Arc<Mutex<Vec<AddressUtxo>>>,
    submits: Arc<Mutex<Vec<Value>>>,
}

impl ExecNode {
    fn new(chain: Arc<MockNode>) -> Self {
        let coin = AddressUtxo {
            transaction_id: h("operator-coin", 1).0,
            index: 0,
            amount: 100 * KAS,
            script_public_key: p2pk_spk_string(&pk(MATCHER)),
            block_daa_score: 10,
            covenant_id: None,
        };
        ExecNode { chain, funding: Arc::new(Mutex::new(vec![coin])), submits: Arc::default() }
    }

    fn submitted(&self) -> usize {
        self.submits.lock().unwrap().len()
    }
}

impl NodeApi for ExecNode {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        Ok(ServerInfo {
            server_version: "2.1.0".into(),
            network_id: "testnet-10".into(),
            is_synced: true,
            has_utxo_index: true,
            virtual_daa_score: self.chain.tip_daa(),
        })
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        Ok(DagInfo { network: "testnet-10".into(), sink: self.chain.tip().to_hex(), virtual_daa_score: self.chain.tip_daa() })
    }

    async fn submit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        self.submits.lock().unwrap().push(tx);
        Ok([0; 32])
    }

    async fn chain_from(&self, _start_hash: &str) -> Result<kob_executor::matcher::node::ChainUpdate, RpcError> {
        panic!("the executor must follow acceptance through the indexer, not through a second VSPC follower");
    }

    async fn utxos_by_addresses(&self, _addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        Ok(self.funding.lock().unwrap().clone())
    }
}

fn signer() -> LocalKeys {
    LocalKeys { operator: pk(MATCHER), keys: keys() }
}

fn runner(c: &Ctx, roles: Roles) -> (Runner<ExecNode, IndexerSource>, ExecNode) {
    let node = ExecNode::new(c.hs.node.clone());
    let cfg = RunConfig { roles, max_book_lag: Some(300), ..RunConfig::default() };
    let mut r = Runner::new(node.clone(), IndexerSource::new(c.hs.ingest.clone()), Box::new(signer()), cfg);
    r.capture = Some(vec![]);
    (r, node)
}

fn both() -> Roles {
    Roles { matcher: true, keeper: true }
}

/// The transactions the runner built and submitted since the last call, each engine-validated again.
fn drain(r: &mut Runner<ExecNode, IndexerSource>) -> Vec<SignedTx> {
    let txs = std::mem::take(r.capture.as_mut().unwrap());
    for t in &txs {
        kob_protocol::verify::validate_signed(t).unwrap_or_else(|e| panic!("engine rejected a transaction the executor built: {e}"));
    }
    txs
}

/// A KCC-20 fixture as the same order of `fam`.
fn fx(fam: Family, s: AnyState) -> AnyState {
    if fam == Family::Kron {
        kron(s)
    } else {
        s
    }
}

/// The token covenant id of a family's fixtures.
fn token_of(fam: Family) -> [u8; 32] {
    if fam == Family::Kron {
        TOKEN_COV_KRON
    } else {
        TOKEN_COV
    }
}

/// A resting ask and a crossing bid, both indexed, aged past every freshness rule.
async fn crossing_book(c: &Ctx, fam: Family) -> (SignedTx, SignedTx) {
    let create_a = c.w.create_tx(fx(fam, AnyState::KobAsk(ask(MAKER_A, P250))), CARRIER, MAKER_A, 10 * WHOLE);
    let b = bid(MAKER_B, P260);
    let create_b = c.w.create_tx(fx(fam, AnyState::KobBid(b.clone())), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create_a]).await;
    c.push(&[&create_b]).await;
    c.hs.node.push_empty(20);
    c.hs.sync().await;
    (create_a, create_b)
}

fn listed(c: &Ctx, cov: &Hash32) -> Option<ListedOrder> {
    let g = c.hs.ingest.lock().unwrap();
    StoreBook::new(g.conn(), Some(pk(MATCHER))).orders().into_iter().find(|o| o.id() == cov.0)
}

fn step_ok(rep: anyhow::Result<StepReport>) -> StepReport {
    let r = rep.unwrap_or_else(|e| panic!("step failed: {e:#}"));
    assert_eq!(kob_executor::matcher::lower::budget_slack_retries(), 0, "a compute budget of the table was one unit short");
    r
}

// ---------------------------------------------------------------------------------------------
// the book

#[tokio::test]
async fn the_store_lists_every_kind_exactly_as_the_chain_built_it() {
    for fam in FAMS {
        let c = Ctx::new();
        let a = ask(MAKER_A, P250);
        let create_a = c.w.create_tx(fx(fam, AnyState::KobAsk(a.clone())), CARRIER, MAKER_A, 10 * WHOLE);
        let b = bid(MAKER_B, P260);
        let mut req = c.w.create(fx(fam, AnyState::KobBid(b.clone())), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
        req.deadline = Some(1_790_694_000); // a day order: the placement record carries the wall-clock deadline
        let create_b = c.w.sign(&Action::CreateOrder(req));
        let co = cond_ask(MAKER_A);
        let create_c = c.w.create_tx(fx(fam, AnyState::KobCondAsk(co.clone())), CARRIER, MAKER_A, 10 * WHOLE);
        let ib = ifd_bid(MAKER_B, 4);
        let create_ib = c.w.create_tx(fx(fam, AnyState::KobIfdBid(ib.clone())), ib.escrow().unwrap() as u64, MAKER_B, 0);
        let ia = ifd_ask(MAKER_A);
        let create_ia =
            c.w.create_tx(fx(fam, AnyState::KobIfdAsk(ia.clone())), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
        for t in [&create_a, &create_b, &create_c, &create_ib, &create_ia] {
            c.push(&[t]).await;
        }

        let want = |create: &SignedTx, state: AnyState, custody: Option<i64>, deadline: Option<u64>| {
            let cov = c.w.cov(create, 0);
            let utxo = c.w.order(create, 0, fx(fam, state));
            ListedOrder {
                family: fam,
                custody: custody.map(|n| c.w.token_at(create, 1, TokenState::custody(fam, n, cov.0, EXT))),
                custody_b: None,
                deadline,
                seen_daa: utxo.utxo.block_daa_score,
                foreign: vec![],
                strays: vec![],
                order: utxo,
            }
        };
        let expected = [
            want(&create_a, AnyState::KobAsk(a), Some(10 * WHOLE), None),
            want(&create_b, AnyState::KobBid(b), None, Some(1_790_694_000)),
            want(&create_c, AnyState::KobCondAsk(co), Some(10 * WHOLE), None),
            want(&create_ib, AnyState::KobIfdBid(ib), None, None),
            want(&create_ia, AnyState::KobIfdAsk(ia), Some(10 * WHOLE), None),
        ];
        let snap = {
            let g = c.hs.ingest.lock().unwrap();
            snapshot(g.conn(), Some(pk(MATCHER))).unwrap()
        };
        assert_eq!(snap.orders.len(), expected.len(), "{fam:?}");
        for e in &expected {
            let got = snap.orders.iter().find(|o| o.id() == e.id()).unwrap_or_else(|| panic!("{fam:?}: order is not listed"));
            assert_eq!(got, e, "{fam:?}: the listed order is the chain's order");
            assert!(got.custody_ok(), "{fam:?}: exact custody");
            assert_eq!(got.order.state.family(), fam);
        }
        assert_eq!(snap.daa_score, c.daa(), "the book is as far as the cursor");
    }
}

#[tokio::test]
async fn strays_are_reported_and_never_counted_or_spent() {
    for fam in FAMS {
        let c = Ctx::new();
        let create = c.w.create_tx(fx(fam, AnyState::KobAsk(ask(MAKER_A, P250))), CARRIER, MAKER_A, 10 * WHOLE);
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let stray = c.w.stray_for(fam, cov, 10 * WHOLE, TAKER);
        c.push(&[&stray]).await;
        let o = listed(&c, &cov).expect("listed");
        assert!(o.custody_ok(), "the exact custody is still the one custody");
        assert_eq!(o.strays.len(), 1);
        assert_eq!(o.strays[0].utxo.transaction_id, stray.tx.id);
        assert_eq!(o.strays[0].state.amount(), 10 * WHOLE);
        assert_ne!(outpoint_of(&o.strays[0]), outpoint_of(o.custody.as_ref().unwrap()));

        // a bid fills the ask: the tx never touches the stray
        let b = bid(MAKER_B, P260);
        let create_b = c.w.create_tx(fx(fam, AnyState::KobBid(b.clone())), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
        c.push(&[&create_b]).await;
        c.hs.node.push_empty(20);
        c.hs.sync().await;
        let (mut r, _node) = runner(&c, both());
        let rep = r.step().await.unwrap();
        assert!(!rep.built.is_empty(), "{fam:?} {rep:?}");
        for t in drain(&mut r) {
            assert!(!t.tx.inputs.iter().any(|i| i.transaction_id == stray.tx.id), "a stray is never spent");
        }
    }
}

fn outpoint_of(t: &TokenUtxo) -> ([u8; 32], u32) {
    (t.utxo.transaction_id, t.utxo.index)
}

// ---------------------------------------------------------------------------------------------
// triggers (a stop arms from a plain resting fill of the same transaction)

/// A sell stop at 2.00 (10 whole tokens), a resting ask at 1.90 (10 whole tokens, exposed long enough) and a bid at `bid_price`
/// funded for `bid_n` whole tokens, all indexed. Returns the stop's covenant id and the resting ask's.
async fn stop_book(c: &Ctx, fam: Family, bid_price: i64, bid_n: i64) -> (Hash32, Hash32) {
    let create = c.w.create_tx(fx(fam, AnyState::KobCondAsk(cond_ask(MAKER_A))), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let ev = resting_for(c, fam, SIDE_ASK, 190_000_000).await;
    let b = bid(TAKER, bid_price);
    let create_b = c.w.create_tx(fx(fam, AnyState::KobBid(b.clone())), b.escrow(bid_n * WHOLE, 3).unwrap() as u64, TAKER, 0);
    c.push(&[&create_b]).await;
    c.hs.node.push_empty(800); // the resting ask is exposed for more than minRestDaa (600)
    c.hs.sync().await;
    (c.w.cov(&create, 0), ev.cov(c))
}

#[tokio::test]
async fn the_matcher_arms_a_stop_from_the_indexed_book_inside_its_batch() {
    for fam in FAMS {
        let c = Ctx::new();
        // the 1.95 bid buys exactly the resting ask's 10 whole tokens: nothing is left for the stop, which the batch arms
        let (cov, ev) = stop_book(&c, fam, 195_000_000, 10).await;
        let (mut r, _node) = runner(&c, Roles { matcher: true, keeper: false });
        let rep = step_ok(r.step().await);
        assert!(rep.built.iter().any(|(k, _, _)| k == "match"), "{fam:?}: {rep:?}");
        let txs = drain(&mut r);
        let batch = txs
            .iter()
            .find(|t| t.tx.outputs.iter().any(|o| o.covenant.as_ref().is_some_and(|x| x.covenant_id == cov.0)))
            .expect("the batch carries the stop's update");
        c.push(&[batch]).await;
        match c.hs.tip_state(&cov).map(|s| s.into_family(Family::Kcc20)) {
            Some(AnyState::KobCondAsk(s)) => assert_eq!(s.armed, 1, "{fam:?}: armed on chain"),
            other => panic!("{other:?}"),
        }
        // the indexer records the arm and the evidence that armed it
        let d = c.hs.event_detail(&cov, "arm");
        assert_eq!(d["evidence"]["order"], serde_json::json!(ev.to_hex()), "{fam:?}: {d}");
    }
}

#[tokio::test]
async fn a_stop_fills_next_to_its_evidence_from_the_indexed_book() {
    for fam in FAMS {
        let c = Ctx::new();
        // a 2.05 bid funded for 20 whole tokens: the resting ask's 10, then the stop's 10 at its 2.00 trigger
        let (cov, ev) = stop_book(&c, fam, 205_000_000, 20).await;
        let (mut r, _node) = runner(&c, Roles { matcher: true, keeper: false });
        let rep = step_ok(r.step().await);
        assert!(rep.built.iter().any(|(k, _, _)| k == "match"), "{fam:?}: {rep:?}");
        let txs = drain(&mut r);
        let batch = txs.iter().find(|t| t.tx.inputs.iter().any(|i| i.utxo.covenant_id == Some(cov.0))).expect("the stop's fill");
        c.push(&[batch]).await;
        assert!(listed(&c, &cov).is_none(), "{fam:?}: the stop sold everything");
        let d = c.hs.event_detail(&cov, "fill");
        assert_eq!(d["evidence"]["order"], serde_json::json!(ev.to_hex()), "{fam:?}: {d}");
    }
}

// ---------------------------------------------------------------------------------------------
// the loop: plan, submit, accepted, final, reorg

#[tokio::test]
async fn the_matcher_plans_from_the_indexed_book_and_acceptance_comes_from_the_indexer() {
    for fam in FAMS {
        let c = Ctx::new();
        let (create_a, create_b) = crossing_book(&c, fam).await;
        let (cov_a, cov_b) = (c.w.cov(&create_a, 0), c.w.cov(&create_b, 0));
        let (mut r, node) = runner(&c, both());

        let rep = step_ok(r.step().await);
        assert!(rep.synced && !rep.book_stale, "{rep:?}");
        assert_eq!(rep.book_daa, c.daa());
        assert_eq!(rep.built.len(), 1, "{fam:?}: one crossing, one batch: {rep:?}");
        assert_eq!(rep.built[0].0, "match");
        assert!(rep.built[0].2 > 0, "the operator earns the spread");
        assert_eq!((rep.submitted.len(), node.submitted()), (1, 1));
        let txs = drain(&mut r);
        assert_eq!(txs.len(), 1);
        let fill = &txs[0];
        assert_eq!(fill.tx.id, rep.submitted[0]);
        // it spends exactly the indexed orders' UTXOs and their custody, and trades the family's token
        let spent: Vec<([u8; 32], u32)> = fill.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
        for op in [(create_a.tx.id, 0u32), (create_a.tx.id, 1), (create_b.tx.id, 0)] {
            assert!(spent.contains(&op), "the fill spends {op:?}");
        }
        assert!(fill.tx.inputs.iter().any(|i| i.utxo.covenant_id == Some(token_of(fam))));
        assert_eq!(r.tracker.pending(), 1);

        // pending: not planned twice while the first is in flight
        let rep = step_ok(r.step().await);
        assert!(rep.built.is_empty(), "the orders' outpoints are reserved by the pending transaction: {rep:?}");

        // a chain block accepts it: the indexer applies it, the runner learns it from the indexer
        c.push(&[fill]).await;
        let rep = step_ok(r.step().await);
        assert_eq!(rep.accepted, 1, "{rep:?}");
        assert!(r.tracker.txs[&fill.tx.id].accepted.is_some());
        assert_eq!(r.tracker.pending(), 0);
        assert!(rep.built.is_empty(), "nothing left to match");
        assert_eq!(c.hs.status(&cov_a), "filled");
        // the book followed: the ask is gone from it, the bid rests with what it did not spend
        assert!(listed(&c, &cov_a).is_none());
        let b = listed(&c, &cov_b);
        assert!(b.is_none() || b.is_some_and(|b| b.order.utxo.transaction_id == fill.tx.id));
        assert_eq!(r.tracker.finalized_count, 0, "accepted is not done");

        // done only after acceptance plus depth
        c.hs.node.push_empty(100);
        c.hs.sync().await;
        let rep = step_ok(r.step().await);
        assert_eq!(rep.finalized, 1, "{rep:?}");
        assert!(r.tracker.txs.is_empty());
        assert_eq!((r.tracker.finalized_count, r.tracker.finalized_profit > 0), (1, true));
    }
}

#[tokio::test]
async fn one_tick_plans_both_families_in_one_batch_and_the_indexer_follows_it() {
    let c = Ctx::new();
    // two books of the same shape in two token families: the global batch fills both in one transaction
    let mut created = vec![];
    for fam in FAMS {
        let a = fx(fam, AnyState::KobAsk(ask(MAKER_A, P250)));
        let create_a = c.w.create_tx(a, CARRIER, MAKER_A, 10 * WHOLE);
        let b = bid(MAKER_B, P260);
        let create_b = c.w.create_tx(fx(fam, AnyState::KobBid(b.clone())), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
        c.push(&[&create_a]).await;
        c.push(&[&create_b]).await;
        created.push((fam, create_a, create_b));
    }
    c.hs.node.push_empty(20);
    c.hs.sync().await;
    let (mut r, node) = runner(&c, both());
    let rep = step_ok(r.step().await);
    assert_eq!(rep.built.len(), 1, "one transaction for both books: {rep:?}");
    assert_eq!((rep.submitted.len(), node.submitted()), (1, 1));
    let txs = drain(&mut r);
    let t = &txs[0];
    for (fam, create_a, _) in &created {
        assert!(t.tx.inputs.iter().any(|i| i.transaction_id == create_a.tx.id), "the family's ask is in the batch");
        assert!(t.tx.inputs.iter().any(|i| i.utxo.covenant_id == Some(token_of(*fam))));
    }
    let refs: Vec<&SignedTx> = txs.iter().collect();
    c.push(&refs).await;
    let rep = step_ok(r.step().await);
    assert_eq!(rep.accepted, 1, "{rep:?}");
    for (_, create_a, _) in &created {
        assert_eq!(c.hs.status(&c.w.cov(create_a, 0)), "filled");
    }
}

#[tokio::test]
async fn a_reorg_moves_an_accepted_fill_back_to_pending_and_the_book_follows() {
    for fam in FAMS {
        let c = Ctx::new();
        let (create_a, _create_b) = crossing_book(&c, fam).await;
        let cov_a = c.w.cov(&create_a, 0);
        let (mut r, node) = runner(&c, both());
        step_ok(r.step().await);
        let fill = drain(&mut r).remove(0);
        let accepted_in = c.push(&[&fill]).await;
        let rep = step_ok(r.step().await);
        assert_eq!(rep.accepted, 1);
        assert_eq!(c.hs.status(&cov_a), "filled");
        assert_eq!(node.submitted(), 1);
        assert!(listed(&c, &cov_a).is_none());

        // the accepting chain block is replaced by an empty one: the fill is not accepted any more
        let removed = c.hs.node.reorg(1, vec![vec![], vec![]]);
        assert_eq!(removed.len(), 2);
        c.hs.sync().await;
        assert_ne!(c.hs.node.tip(), accepted_in);
        assert_eq!(c.hs.status(&cov_a), "open", "the indexer reverted the fill");
        let back = listed(&c, &cov_a).expect("the ask is live again");
        assert_eq!(back.order.utxo.transaction_id, create_a.tx.id);

        let rep = step_ok(r.step().await);
        assert_eq!(rep.rolled_back, 1, "{rep:?}");
        assert_eq!(r.tracker.pending(), 1, "back to pending, not lost");
        assert_eq!(node.submitted(), 2, "idempotent resend of the rolled-back transaction");
        assert!(rep.built.is_empty(), "the fill's inputs are still reserved: no second plan for the same orders");

        // the new chain includes it again
        c.push(&[&fill]).await;
        let rep = step_ok(r.step().await);
        assert_eq!(rep.accepted, 1, "{rep:?}");
        assert_eq!(c.hs.status(&cov_a), "filled");
        assert!(r.tracker.txs[&fill.tx.id].accepted.is_some());
        c.hs.node.push_empty(100);
        c.hs.sync().await;
        let rep = step_ok(r.step().await);
        assert_eq!(rep.finalized, 1);
    }
}

#[tokio::test]
async fn a_lagging_indexer_stops_planning_until_it_catches_up() {
    let c = Ctx::new();
    crossing_book(&c, Family::Kcc20).await;
    // the node runs far ahead of the indexer (the mock chain grows, the follower is not stepped)
    c.hs.node.push_empty(400);
    let (mut r, node) = runner(&c, both());
    let rep = step_ok(r.step().await);
    assert!(rep.book_stale, "{rep:?}");
    assert!(rep.built.is_empty() && node.submitted() == 0);
    c.hs.sync().await;
    let rep = step_ok(r.step().await);
    assert!(!rep.book_stale && rep.built.len() == 1, "{rep:?}");
}

/// After a restart the store is where the last run left it: until the follower reports it caught up (`following`), the
/// matcher, the keepers and the maintenance jobs plan nothing (the TN10 soak's restarts submitted maintenance sells over a
/// stale store and lost them to `MissingInput`), and no funding is reported (not `0`: unknown).
#[tokio::test]
async fn nothing_is_planned_until_the_indexer_has_caught_up() {
    use kob_executor::indexer::status::{FollowerState, HealthState};
    let c = Ctx::new();
    crossing_book(&c, Family::Kcc20).await;
    let dir = tempfile::tempdir().unwrap();
    let prom = dir.path().join("m.prom");
    // a restarted process: a fresh health report (`starting`), the store as the last run left it
    let health = Arc::new(HealthState::new("testnet-10"));
    let node = ExecNode::new(c.hs.node.clone());
    let cfg = RunConfig { roles: both(), max_book_lag: Some(300), metrics_file: Some(prom.clone()), ..RunConfig::default() };
    let source = IndexerSource::new(c.hs.ingest.clone()).with_health(health.clone());
    let mut r = Runner::new(node.clone(), source, Box::new(signer()), cfg);
    r.capture = Some(vec![]);
    let rep = step_ok(r.step().await);
    assert!(rep.not_ready.as_deref().is_some_and(|w| w.contains("starting")), "{rep:?}");
    assert!(rep.built.is_empty() && node.submitted() == 0 && rep.funding.is_none(), "{rep:?}");
    let m = std::fs::read_to_string(&prom).unwrap();
    assert!(m.contains("kob_matcher_waiting_for_indexer 1") && m.contains("kob_operator_low_funds 0"), "{m}");
    // the first batch leaves it 500 DAA behind (`catching_up`): still nothing, even within the book-lag bound of 300
    health.update(|h| {
        h.state = FollowerState::CatchingUp;
        h.node_daa = Some(c.daa() + 200);
        h.cursor_hash = Some(c.hs.node.tip());
        h.cursor_daa = c.daa() - 300;
    });
    let rep = step_ok(r.step().await);
    assert!(rep.not_ready.as_deref().is_some_and(|w| w.contains("catching_up") && w.contains("500 DAA")), "{rep:?}");
    assert!(rep.built.is_empty() && node.submitted() == 0);
    // caught up: the crossing is matched
    health.update(|h| {
        h.state = FollowerState::Following;
        h.node_daa = Some(c.daa());
        h.cursor_daa = c.daa();
    });
    let rep = step_ok(r.step().await);
    assert!(rep.not_ready.is_none() && rep.built.len() == 1 && rep.funding.is_some(), "{rep:?}");
    assert_eq!(node.submitted(), 1);
    let m = std::fs::read_to_string(&prom).unwrap();
    assert!(m.contains("kob_matcher_waiting_for_indexer 0"), "{m}");
    // the follower's own report decides in the process: the harness follower is caught up after its sync
    assert!(c.hs.health.snapshot().caught_up());
    let (mut r2, _) = runner(&c, Roles { matcher: false, keeper: true });
    r2.source = IndexerSource::new(c.hs.ingest.clone()).with_health(c.hs.health.clone());
    assert!(step_ok(r2.step().await).not_ready.is_none());
}

/// The lag tolerance (`max_lag_secs`, 30 s = 300 DAA by default): the runner plans while the follower is `catching_up` within it,
/// stops beyond it, and a plan against a view that is a few seconds old only loses the race (`MissingInput`), never breaks the order's
/// terms (the covenants enforce them): the runner counts the conflict and goes on.
#[tokio::test]
async fn the_runner_plans_within_the_lag_tolerance_and_stops_beyond_it() {
    use kob_executor::indexer::status::{FollowerState, HealthState};
    let c = Ctx::new();
    crossing_book(&c, Family::Kcc20).await;
    let health = Arc::new(HealthState::new("testnet-10"));
    health.update(|h| h.lag_tolerance_daa = 300);
    let node = ExecNode::new(c.hs.node.clone());
    let cfg = RunConfig { roles: both(), max_book_lag: Some(300), ..RunConfig::default() };
    let source = IndexerSource::new(c.hs.ingest.clone()).with_health(health.clone());
    let mut r = Runner::new(node.clone(), source, Box::new(signer()), cfg);
    r.capture = Some(vec![]);
    // `starting`: node position unknown, nothing
    let rep = step_ok(r.step().await);
    assert!(rep.not_ready.is_some() && rep.built.is_empty() && node.submitted() == 0, "{rep:?}");
    // 301 DAA behind (`catching_up`): beyond the tolerance, nothing
    health.update(|h| {
        h.state = FollowerState::CatchingUp;
        h.node_daa = Some(c.daa());
        h.cursor_hash = Some(c.hs.node.tip());
        h.cursor_daa = c.daa() - 301;
    });
    let rep = step_ok(r.step().await);
    assert!(rep.not_ready.as_deref().is_some_and(|w| w.contains("catching_up") && w.contains("301 DAA")), "{rep:?}");
    assert!(rep.built.is_empty() && node.submitted() == 0);
    // 300 DAA behind: still `catching_up`, not caught up, but within the tolerance: the crossing is matched
    health.update(|h| h.cursor_daa = c.daa() - 300);
    assert!(!health.snapshot().caught_up() && health.snapshot().within_lag_tolerance());
    let rep = step_ok(r.step().await);
    assert!(rep.not_ready.is_none() && rep.built.len() == 1, "{rep:?}");
    assert_eq!(node.submitted(), 1);
    // the strict gate (tolerance 0) refuses the same state
    health.update(|h| h.lag_tolerance_daa = 0);
    let (mut strict, _) = runner(&c, both());
    strict.source = IndexerSource::new(c.hs.ingest.clone()).with_health(health.clone());
    let rep = step_ok(strict.step().await);
    assert!(rep.not_ready.as_deref().is_some_and(|w| w.contains("catching_up")), "{rep:?}");
}

#[tokio::test]
async fn a_keeper_refunds_an_expired_order_from_the_indexed_book() {
    for fam in FAMS {
        let c = Ctx::new();
        let now = c.daa();
        let a = AskState { expiry_daa: (now + 50) as i64, ..ask(MAKER_A, P250) };
        let create = c.w.create_tx(fx(fam, AnyState::KobAsk(a)), CARRIER, MAKER_A, 10 * WHOLE);
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let (mut r, _node) = runner(&c, both());
        let rep = step_ok(r.step().await);
        assert!(rep.built.is_empty(), "not expired yet");
        c.hs.node.push_empty(60);
        c.hs.sync().await;
        let rep = step_ok(r.step().await);
        assert_eq!(rep.built.iter().filter(|(k, _, _)| k == "refund").count(), 1, "{fam:?} {rep:?}");
        let refund = drain(&mut r).remove(0);
        c.push(&[&refund]).await;
        assert_eq!(c.hs.status(&cov), "refunded");
        let rep = step_ok(r.step().await);
        assert_eq!(rep.accepted, 1);
        assert!(listed(&c, &cov).is_none());
    }
}

// ---------------------------------------------------------------------------------------------
// acceptance tracking in the store

#[tokio::test]
async fn the_store_follows_watched_transactions_across_reorgs() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
    let id = Hash32(create.tx.id);
    let other = Hash32([9; 32]);
    let seen = |keep: &[Hash32]| c.hs.ingest.lock().unwrap().watched_acceptance(keep);
    c.hs.ingest.lock().unwrap().watch_tx(id);
    assert_eq!(seen(&[id]), vec![(id, None)]);
    let b1 = c.push(&[&create]).await;
    let daa = c.daa();
    assert_eq!(seen(&[id]), vec![(id, Some((b1, daa)))]);
    // a reorg that drops the block un-accepts it; the replacement block accepts it again
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(seen(&[id]), vec![(id, None)]);
    let b2 = c.w.include(&c.hs.node, &[&create]);
    c.hs.sync().await;
    assert_eq!(seen(&[id]), vec![(id, Some((b2, c.daa())))]);
    // unwatching forgets; unknown ids are simply not accepted
    assert_eq!(seen(&[other]), vec![(other, None)]);
    assert_eq!(seen(&[id]), vec![(id, None)], "no longer watched: acceptance is not tracked retroactively");
}

// ---------------------------------------------------------------------------------------------
// the whole process

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_indexes_matches_and_shuts_down_cleanly() {
    use kob_executor::config::{IndexerConfig, StartMode};
    use kob_executor::executor::run_with;
    use kob_executor::indexer::Indexer;
    use kob_executor::tokens::ListingRules;

    for fam in FAMS {
        let dir = tempfile::tempdir().unwrap();
        let node = MockNode::new("testnet-10");
        let w = World::new();
        let create_a = w.create_tx(fx(fam, AnyState::KobAsk(ask(MAKER_A, P250))), CARRIER, MAKER_A, 10 * WHOLE);
        let b = bid(MAKER_B, P260);
        let create_b = w.create_tx(fx(fam, AnyState::KobBid(b.clone())), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
        w.include(&node, &[&create_a]);
        w.include(&node, &[&create_b]);
        node.push_empty(20);

        let tokens = dir.path().join("tokens.json");
        std::fs::write(
            &tokens,
            format!(
                r#"[{{"ticker":"TST","family":"{}","covenant_id":"{}"}}]"#,
                fam.as_str(),
                kob_executor::hex::encode(&token_of(fam))
            ),
        )
        .unwrap();
        let mut cfg = IndexerConfig {
            data_dir: dir.path().join("data"),
            tokens_path: Some(tokens),
            start: StartMode::Hash(node.anchor()),
            poll_interval_ms: 5,
            rules: ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..ListingRules::default() },
            ..IndexerConfig::default()
        };
        cfg.api.enabled = false;
        let indexer = Indexer::open(cfg).unwrap();
        let exec = ExecNode::new(node.clone());
        let metrics = dir.path().join("kob.prom");
        let run_cfg = RunConfig {
            roles: both(),
            tick: std::time::Duration::from_millis(100),
            metrics_file: Some(metrics.clone()),
            max_book_lag: Some(300),
            ..RunConfig::default()
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let job = tokio::spawn({
            let (node, exec) = (node.clone(), exec.clone());
            async move {
                run_with(indexer, node, exec, Box::new(signer()), run_cfg, async move {
                    let _ = stopped.await;
                })
                .await
            }
        });
        let started = std::time::Instant::now();
        while exec.submitted() == 0 {
            assert!(started.elapsed() < std::time::Duration::from_secs(30), "{fam:?}: the executor never submitted the crossing");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await; // more ticks: the pending fill is not planned again
        assert_eq!(exec.submitted(), 1, "the pending transaction reserves the orders");
        stop.send(()).unwrap();
        job.await.unwrap().unwrap();
        let prom = std::fs::read_to_string(&metrics).unwrap();
        assert!(prom.contains("kob_matcher_submitted_total 1"), "{prom}");
        assert!(prom.contains("kob_matcher_rejected_total 0") && prom.contains("kob_matcher_book_stale 0"), "{prom}");
    }
}
