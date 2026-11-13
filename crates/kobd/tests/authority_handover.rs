//! A chain authority that stays reachable while its chain requests fail (`rpc::multi`, `MultiNode::authority_failing`;
//! `indexer::follower`, *A failing chain authority*). Mock nodes on one shared chain: a primary whose VSPC and chain requests
//! time out while its small calls answer hands the authority over to the other node, and the store catches up to a clean
//! replay; a node whose chain does not continue the cursor, or one that lied, is never promoted; a single node is retried with
//! the backoff and nothing is promoted; the old primary that recovers is a secondary again and takes the authority back only
//! when the new one fails the same way.

mod common;

use common::*;
use kob_protocol::state::*;
use kobd::config::StartMode;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kobd::indexer::ingest::Ingest;
use kobd::indexer::status::{FollowerState, HealthState};
use kobd::rpc::multi::{MultiNode, NodeSpec};
use kobd::rpc::types::*;
use kobd::rpc::{ChainSource, RpcError};
use kobd::testkit::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A node whose chain requests (VSPC, chain hashes, the acceptance data) can time out while every small call answers (the
/// incident of 2026-11-13: health and sink fine, chain requests reset or timed out), or that alters an output of the
/// windows it serves (a liar).
struct Flaky {
    node: Arc<MockNode>,
    chain_down: AtomicBool,
    alters: AtomicBool,
    chain_calls: AtomicUsize,
    /// Serves its VSPC answers from this node instead (another view of the chain), while every other call answers from
    /// `node`.
    vspc_from: Mutex<Option<Arc<MockNode>>>,
}

impl Flaky {
    fn new(node: &Arc<MockNode>) -> Arc<Flaky> {
        Arc::new(Flaky {
            node: node.clone(),
            chain_down: AtomicBool::new(false),
            alters: AtomicBool::new(false),
            chain_calls: AtomicUsize::new(0),
            vspc_from: Mutex::new(None),
        })
    }

    fn chain_gate(&self) -> Result<(), RpcError> {
        self.chain_calls.fetch_add(1, Ordering::Relaxed);
        if self.chain_down.load(Ordering::Relaxed) {
            return Err(RpcError::Timeout(Duration::from_secs(180)));
        }
        Ok(())
    }
}

impl ChainSource for Flaky {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.chain_gate()?;
        let full = req.data_verbosity_level == Verbosity::Full;
        let from = self.vspc_from.lock().unwrap().clone().unwrap_or_else(|| self.node.clone());
        let mut r = from.vspc_v2(req).await?;
        if full && self.alters.load(Ordering::Relaxed) {
            let tx = r.chain_block_accepted_transactions.iter_mut().flat_map(|b| b.accepted_transactions.iter_mut());
            if let Some(t) = tx.into_iter().find(|t| !t.outputs.is_empty()) {
                t.outputs[0].value += 1;
            }
        }
        Ok(r)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.node.dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.node.server_info().await
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.node.utxos_by_addresses(a).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.node.block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.node.sink_blue_score().await
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.node.block_blue_score(hash).await
    }
    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.node.block_daa_score(hash).await
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.chain_gate()?;
        ChainSource::chain_hashes(&*self.node, start).await
    }
    async fn chain_with_ids(&self, start: Hash32, m: Option<u64>) -> Result<ChainIds, RpcError> {
        self.chain_gate()?;
        self.node.chain_with_ids(start, m).await
    }
}

/// The judge, the resolver state and the promotion count are process-wide: these tests run one at a time.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static M: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let g = M.lock().await;
    kobd::rpc::resolver::set_network("testnet-10");
    g
}

fn spec(url: &str, fetch: bool) -> NodeSpec {
    NodeSpec { url: url.into(), fetch, submit: true, connections: 1, cross_check: false, may_lead: true }
}

/// A node that only takes submissions (`[[nodes]] fetch = false`): it may not lead unless the configuration says so.
fn submit_only(url: &str) -> NodeSpec {
    NodeSpec { may_lead: false, ..spec(url, false) }
}

struct Run {
    source: Arc<MultiNode<Flaky>>,
    follower: Follower<MultiNode<Flaky>>,
    ingest: Arc<Mutex<Ingest>>,
    health: Arc<HealthState>,
}

/// A follower over named nodes (the first the primary, also its verifier) that hands the authority over after two failed
/// chain requests in a row (no wait for the stall: `authority_stall` zero), on a fresh database. The health watch runs
/// only when the test asks (`check_health`).
fn run(anchor: Hash32, nodes: Vec<(NodeSpec, Arc<Flaky>)>, fetch_parallel: usize, backoff: Duration) -> Run {
    let verifier = nodes[0].1.clone();
    let source = Arc::new(MultiNode::new(nodes, verifier).with_watch(Duration::from_secs(3_600), 6));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(anchor),
        poll_interval: Duration::from_millis(1),
        backoff,
        max_backoff: backoff * 8,
        min_confirmations: None,
        max_walkback_blocks: 1_000,
        status_refresh: Duration::from_millis(0),
        caught_up_daa: 100,
        batch_initial_blue: 8,
        batch_target: Duration::from_secs(30),
        fetch_parallel,
        prefetch_max_bytes: 256 << 20,
        prefetch_min_lag_blue: 0,
        prefetch_initial_blocks: 6,
        authority_failures: 2,
        authority_stall: Duration::ZERO,
    };
    let follower = Follower::new(source.clone(), ingest.clone(), cfg, health.clone(), events);
    Run { source, follower, ingest, health }
}

impl Run {
    /// Steps until `done`, or panics after `max` steps; never a gap.
    async fn until(&self, max: usize, what: &str, done: impl Fn(&Run) -> bool) {
        for _ in 0..max {
            if done(self) {
                return;
            }
            let o = self.follower.step().await;
            assert!(!matches!(o, StepOutcome::Gap(_) | StepOutcome::Halted(_)), "{o:?}");
        }
        panic!("{what}: not reached in {max} steps; health {:?}", self.health.snapshot());
    }

    /// `n` steps; never a gap. The outcomes.
    async fn steps(&self, n: usize) -> Vec<StepOutcome> {
        let mut out = vec![];
        for _ in 0..n {
            let o = self.follower.step().await;
            assert!(!matches!(o, StepOutcome::Gap(_) | StepOutcome::Halted(_)), "{o:?}");
            out.push(o);
        }
        out
    }

    fn cursor(&self) -> Option<Hash32> {
        self.health.snapshot().cursor_hash
    }

    fn db(&self) -> String {
        snapshot(self.ingest.lock().unwrap().conn())
    }
}

/// Blocks of noise with an ask every tenth.
fn grow(c: &Ctx, blocks: u64, salt: u64) {
    for i in 0..blocks {
        let noise: Vec<Tx> = (0..4).map(|j| noise_tx(salt + i * 1_000 + j, j % 2 == 0)).collect();
        if i % 10 == 0 {
            let a = AskState { amount_left: 2 * WHOLE, ..ask(MAKER_A, P250 + (salt + i) as i64) };
            let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 2 * WHOLE);
            c.w.include_with(&c.hs.node, &[&create], noise);
        } else {
            c.hs.node.push_block(noise);
        }
    }
}

/// The incident of 2026-11-13: the primary's health and sink answer, its chain requests time out. After two of them in a row
/// (and the stall, zero here) the other node is the chain authority, the store catches up to the tip on it and equals a clean
/// replay. The swap is in the health (roles, `promoted`, the old primary cut off with a node alarm, the authority counter)
/// and on the status line. Sequential steps and the parallel fetch alike.
#[tokio::test]
async fn a_primary_whose_chain_requests_time_out_hands_the_authority_to_the_other_node() {
    let _g = serial().await;
    for (k, fetch_parallel) in [1usize, 2].into_iter().enumerate() {
        let c = Ctx::new();
        grow(&c, 40, 0);
        let node = c.hs.node.clone();
        let (ua, ub) = (format!("ws://10.1.{k}.1:18210"), format!("ws://10.1.{k}.2:18210"));
        let (a, b) = (Flaky::new(&node), Flaky::new(&node));
        let r = run(
            node.anchor(),
            vec![(spec(&ua, true), a.clone()), (spec(&ub, true), b.clone())],
            fetch_parallel,
            Duration::from_millis(1),
        );
        r.until(2_000, "first sync", |r| r.cursor() == Some(node.tip())).await;
        assert_eq!(r.source.board().authority(), 0);
        let replaced_before = r.health.snapshot().authority_replaced_total;

        a.chain_down.store(true, Ordering::Relaxed);
        grow(&c, 40, 100_000);
        r.until(2_000, "caught up on the other node", |r| {
            r.cursor() == Some(node.tip()) && r.health.snapshot().state == FollowerState::Following
        })
        .await;
        assert_eq!(r.source.board().authority(), 1, "the other node is the chain authority");
        assert_eq!(r.db(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
        let h = r.health.snapshot();
        assert_eq!(h.state, FollowerState::Following);
        let (n0, n1) = (&h.nodes[0], &h.nodes[1]);
        assert_eq!((n1.role.as_str(), n1.promoted, n1.state.as_str()), ("primary", 1, "ok"), "{n1:?}");
        assert_eq!((n0.role.as_str(), n0.state.as_str()), ("secondary", "cut"), "{n0:?}");
        assert!(n0.cut_reason.as_deref().is_some_and(|w| w.contains("chain requests keep failing")), "{n0:?}");
        assert!(h.node_alarms.iter().any(|w| w.starts_with(&ua) && w.contains("chain requests keep failing")), "{:?}", h.node_alarms);
        assert!(h.authority_replaced_total > replaced_before, "counted in kob_indexer_authority_replaced_total");
        let line = h.status_line();
        assert!(line.contains(&format!("{ub} (authority, ok, took over 1x)")), "{line}");
        assert!(line.contains("NODE ALARM") && line.contains(&ua), "{line}");
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(json["nodes"][1]["role"], "primary");
        assert!(json["node_alarms"][0].as_str().unwrap().contains("chain requests keep failing"));
    }
}

/// A candidate whose selected chain does not run through the cursor (it reorganised below it: it would contradict what the
/// store holds), and one that lied about a window (dropped), are never promoted: the primary stays the authority with an
/// alarm, the cursor stays and nothing of the other chain is applied.
#[tokio::test]
async fn a_candidate_that_contradicts_the_store_or_lied_is_never_promoted() {
    let _g = serial().await;
    // the same 40 chain blocks on both, then the candidate reorganises 25 of them away
    let (na, nb) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    na.push_empty(40);
    nb.push_empty(40);
    let (a, b) = (Flaky::new(&na), Flaky::new(&nb));
    let (ua, ub) = ("ws://10.2.0.1:18210", "ws://10.2.0.2:18210");
    let r = run(na.anchor(), vec![(spec(ua, true), a.clone()), (spec(ub, true), b.clone())], 1, Duration::from_millis(1));
    r.until(500, "first sync", |r| r.cursor() == Some(na.tip())).await;
    nb.reorg(25, vec![vec![]; 30]);
    a.chain_down.store(true, Ordering::Relaxed);
    let out = r.steps(40).await;
    assert!(out.iter().all(|o| matches!(o, StepOutcome::Retry(_))), "{out:?}");
    assert_eq!(r.source.board().authority(), 0, "not promoted");
    let h = r.health.snapshot();
    assert_eq!(h.cursor_hash, Some(na.tip()), "the cursor stays");
    assert_eq!((h.reorgs_total, h.reverted_blocks_total), (0, 0), "nothing of the other chain applied");
    assert_eq!(h.state, FollowerState::NodeUnavailable);
    assert_eq!(h.nodes[1].promoted, 0);
    assert!(kobd::rpc::judge::alarm_of(ua).is_some_and(|w| w.contains("chain requests keep failing")), "the primary's alarm");

    // a liar: dropped at its first window, never promoted
    let c = Ctx::new();
    grow(&c, 60, 0);
    let node = c.hs.node.clone();
    let (a, liar) = (Flaky::new(&node), Flaky::new(&node));
    liar.alters.store(true, Ordering::Relaxed);
    let (ua, ul) = ("ws://10.2.1.1:18210", "ws://10.2.1.2:18210");
    // the primary serves windows only when no other node can: the liar is offered the first one
    let r = run(node.anchor(), vec![(spec(ua, false), a.clone()), (spec(ul, true), liar.clone())], 2, Duration::from_millis(1));
    r.until(2_000, "first sync", |r| r.cursor() == Some(node.tip())).await;
    assert_eq!(r.health.snapshot().nodes[1].state, "dropped");
    a.chain_down.store(true, Ordering::Relaxed);
    liar.alters.store(false, Ordering::Relaxed);
    grow(&c, 10, 100_000);
    r.steps(40).await;
    assert_eq!(r.source.board().authority(), 0, "a node that lied is never promoted");
    let h = r.health.snapshot();
    assert_eq!((h.nodes[1].promoted, h.nodes[1].state.as_str()), (0, "dropped"));
    assert_eq!(h.cursor_hash, Some(node.chain()[60].hash), "nothing applied without an authority that answers");
}

/// A single node: nothing to promote. Its chain requests time out, the follower retries with its backoff (doubling, so a
/// handful of requests in the time thousands would fit without it), the node stays the authority and shows a node alarm.
#[tokio::test]
async fn a_single_node_is_retried_with_the_backoff_and_nothing_is_promoted() {
    let _g = serial().await;
    let node = MockNode::new("testnet-10");
    node.push_empty(20);
    let a = Flaky::new(&node);
    let ua = "ws://10.3.0.1:18210";
    let r = run(node.anchor(), vec![(spec(ua, true), a.clone())], 1, Duration::from_millis(20));
    r.until(200, "first sync", |r| r.cursor() == Some(node.tip())).await;
    let promoted = kobd::rpc::multi::promotions();
    a.chain_down.store(true, Ordering::Relaxed);
    node.push_empty(5);
    let before = a.chain_calls.load(Ordering::Relaxed);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let _ = tokio::time::timeout(Duration::from_millis(700), r.follower.run(rx)).await;
    drop(stop);
    let asked = a.chain_calls.load(Ordering::Relaxed) - before;
    // 20, 40, 80, 160, 160 ms ...: about six requests in 0.7 s
    assert!((3..=12).contains(&asked), "{asked} chain requests: the backoff applies");
    let h = r.health.snapshot();
    assert!(h.consecutive_failures >= 3, "{h:?}");
    assert_eq!(h.state, FollowerState::NodeUnavailable);
    assert_eq!(h.cursor_hash, Some(node.chain()[20].hash));
    assert_eq!(r.source.board().authority(), 0);
    assert_eq!(kobd::rpc::multi::promotions(), promoted, "nothing promoted");
    assert!(h.node_alarms.iter().any(|w| w.starts_with(ua) && w.contains("chain requests keep failing")), "{:?}", h.node_alarms);
}

/// The old primary recovers: it stays a secondary, cut off until its alarm ends (its chain requests answer again for the
/// alarm's clearing time), then serves windows as any secondary, and the authority does not move back. Only when the new
/// authority fails the same way is the old primary, healthy by then, promoted again.
#[tokio::test]
async fn the_recovered_old_primary_does_not_take_the_authority_back() {
    let _g = serial().await;
    let c = Ctx::new();
    grow(&c, 30, 0);
    let node = c.hs.node.clone();
    let (a, b) = (Flaky::new(&node), Flaky::new(&node));
    let (ua, ub) = ("ws://10.4.0.1:18210", "ws://10.4.0.2:18210");
    let r = run(node.anchor(), vec![(spec(ua, true), a.clone()), (spec(ub, true), b.clone())], 2, Duration::from_millis(1));
    r.until(2_000, "first sync", |r| r.cursor() == Some(node.tip())).await;
    a.chain_down.store(true, Ordering::Relaxed);
    grow(&c, 20, 100_000);
    r.until(2_000, "caught up on the other node", |r| r.cursor() == Some(node.tip())).await;
    assert_eq!(r.source.board().authority(), 1);

    // it recovers: its chain answers again, but its alarm ends only after a while of them (a minute in production)
    a.chain_down.store(false, Ordering::Relaxed);
    for i in 0..5u64 {
        grow(&c, 10, 200_000 + i * 10_000);
        r.source.check_health().await;
        r.until(2_000, "following", |r| r.cursor() == Some(node.tip())).await;
    }
    assert_eq!(r.source.board().authority(), 1, "the authority stays");
    assert_eq!(r.health.snapshot().nodes[0].state, "cut", "still within the alarm's clearing time");

    // its alarm ends: a secondary again, serving windows; the authority still does not move back
    kobd::rpc::judge::set_alarm_clear_after(Duration::ZERO);
    r.source.check_health().await;
    r.source.check_health().await;
    assert_eq!(kobd::rpc::judge::alarm_of(ua), None, "the alarm ended");
    let windows = r.health.snapshot().nodes[0].windows_ok;
    for i in 0..5u64 {
        grow(&c, 20, 400_000 + i * 10_000);
        r.until(2_000, "following", |r| r.cursor() == Some(node.tip())).await;
    }
    let h = r.health.snapshot();
    assert_eq!(r.source.board().authority(), 1, "no flapping back");
    assert_eq!((h.nodes[0].role.as_str(), h.nodes[0].state.as_str()), ("secondary", "ok"), "{:?}", h.nodes[0]);
    assert!(h.nodes[0].windows_ok > windows, "it serves windows again: {:?}", h.nodes[0]);
    assert_eq!((h.nodes[0].promoted, h.nodes[1].promoted), (0, 1));

    // the new authority fails the same way: the old primary, healthy now, takes over
    b.chain_down.store(true, Ordering::Relaxed);
    grow(&c, 20, 600_000);
    r.until(2_000, "caught up on the old primary", |r| r.cursor() == Some(node.tip())).await;
    assert_eq!(r.source.board().authority(), 0);
    let h = r.health.snapshot();
    assert_eq!((h.nodes[0].promoted, h.nodes[1].promoted), (1, 1));
    assert_eq!(r.db(), fresh_snapshot(&node.chain()), "the same rows as a clean replay across both handovers");
    kobd::rpc::judge::set_alarm_clear_after(kobd::rpc::judge::ALARM_CLEAR_AFTER);
}

/// Only a node the operator lets lead is promoted (`NodeSpec::may_lead`). A submit-only node (`[[nodes]] fetch = false`,
/// so `may_lead` is false unless set) is never even checked as a candidate, however healthy: the primary stays the
/// authority with its alarm.
#[tokio::test]
async fn a_submit_only_node_is_never_promoted() {
    let _g = serial().await;
    let node = MockNode::new("testnet-10");
    node.push_empty(20);
    let (a, b) = (Flaky::new(&node), Flaky::new(&node));
    let (ua, ub) = ("ws://10.5.0.1:18210", "ws://10.5.0.2:18210");
    let r = run(node.anchor(), vec![(spec(ua, true), a.clone()), (submit_only(ub), b.clone())], 1, Duration::from_millis(1));
    r.until(500, "first sync", |r| r.cursor() == Some(node.tip())).await;
    a.chain_down.store(true, Ordering::Relaxed);
    node.push_empty(5);
    let out = r.steps(40).await;
    assert!(out.iter().all(|o| matches!(o, StepOutcome::Retry(_))), "{out:?}");
    assert_eq!(r.source.board().authority(), 0, "not promoted");
    assert_eq!(r.health.snapshot().nodes[1].promoted, 0);
    assert_eq!(b.chain_calls.load(Ordering::Relaxed), 0, "never checked as a candidate");
    assert!(kobd::rpc::judge::alarm_of(ua).is_some_and(|w| w.contains("chain requests keep failing")), "the primary's alarm");
}

/// The new authority's answers stay checked until the cursor moves (`MultiNode::settle`). Its chain hashes from the cursor
/// continue it, but its VSPC answer from the same cursor removes 130 chain blocks: refused, nothing reverted. Before, the
/// chain hashes ended the check and the VSPC answer was applied as a reorg.
#[tokio::test]
async fn every_answer_from_the_handover_cursor_is_checked_not_only_the_first() {
    let _g = serial().await;
    // the same 200 chain blocks on both views (the mock's blocks are deterministic)
    let (na, nb) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    na.push_empty(200);
    nb.push_empty(200);
    let (a, b) = (Flaky::new(&na), Flaky::new(&na));
    let (ua, ub) = ("ws://10.6.0.1:18210", "ws://10.6.0.2:18210");
    let r = run(na.anchor(), vec![(spec(ua, true), a.clone()), (spec(ub, true), b.clone())], 2, Duration::from_millis(1));
    r.until(2_000, "first sync", |r| r.cursor() == Some(na.tip())).await;
    let cursor = na.tip();
    // both grow alike, then the candidate's VSPC view reorganises the last 150 chain blocks away
    na.push_empty(20);
    nb.push_empty(20);
    nb.reorg(150, vec![vec![]; 160]);
    *b.vspc_from.lock().unwrap() = Some(nb.clone());
    a.chain_down.store(true, Ordering::Relaxed);
    let out = r.steps(60).await;
    assert_eq!(r.source.board().authority(), 1, "promoted: its chain hashes from the cursor continue it");
    let h = r.health.snapshot();
    assert_eq!((h.reorgs_total, h.reverted_blocks_total), (0, 0), "nothing reverted: {out:?}");
    assert_eq!(h.cursor_hash, Some(cursor));
    let refused = |o: &StepOutcome| matches!(o, StepOutcome::Retry(m) if m.contains("at most 100 right after it took over"));
    assert!(out.iter().any(refused), "{out:?}");
}
