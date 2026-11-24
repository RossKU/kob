//! A chain authority that stays reachable while its chain requests fail (`rpc::multi`,
//! `MultiNode::authority_failing`; `indexer::follower`, *A failing chain authority*). Mock nodes on
//! one shared chain: a primary whose VSPC and chain requests time out while its small calls answer
//! hands the authority over to the other node, and the store catches up to a clean replay; a node
//! whose chain does not continue the cursor, or one that lied, is never promoted; a single node is
//! retried with the backoff and nothing is promoted; the old primary that recovers is a secondary
//! again and takes the authority back only when the new one fails the same way.

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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A node whose chain requests (VSPC, chain hashes, the acceptance data) can time out while every
/// small call answers (health and sink fine, chain requests reset or timed out), or that alters an
/// output of the windows it serves (a liar).
struct Flaky {
    node: Arc<MockNode>,
    chain_down: AtomicBool,
    alters: AtomicBool,
    chain_calls: AtomicUsize,
    /// Reports no UTXO index.
    no_utxo_index: AtomicBool,
    /// Serves its VSPC answers from this node instead (another view of the chain), while every other call answers from
    /// `node`.
    vspc_from: Mutex<Option<Arc<MockNode>>>,
    /// Drips the chain (R4C1-M1): every VSPC answer is at most this many chain blocks, whatever the window asked
    /// (`minConfirmationCount` ignored), after this delay.
    drip: Mutex<Option<(usize, Duration)>>,
    /// Claims a sink and a virtual DAA score this far past its real ones.
    lead: AtomicU64,
    /// Claims its sink just past the cursor it was last asked from, so a request is never bounded by a window and the
    /// follower reads `following`.
    hides: AtomicBool,
    /// The DAA score of the start of its last VSPC request.
    last_start: AtomicU64,
}

impl Flaky {
    fn new(node: &Arc<MockNode>) -> Arc<Flaky> {
        Arc::new(Flaky {
            node: node.clone(),
            chain_down: AtomicBool::new(false),
            alters: AtomicBool::new(false),
            chain_calls: AtomicUsize::new(0),
            no_utxo_index: AtomicBool::new(false),
            vspc_from: Mutex::new(None),
            drip: Mutex::new(None),
            lead: AtomicU64::new(0),
            hides: AtomicBool::new(false),
            last_start: AtomicU64::new(0),
        })
    }

    /// The sink it claims: past the real one by `lead`, or just past the last cursor while it `hides`.
    async fn claimed(&self, real: u64) -> u64 {
        if self.hides.load(Ordering::Relaxed) {
            return self.last_start.load(Ordering::Relaxed) + 1;
        }
        real + self.lead.load(Ordering::Relaxed)
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
    fn simulated_clock_ms(&self) -> Option<u64> {
        self.node.simulated_clock_ms()
    }
    async fn vspc_v2(&self, mut req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.chain_gate()?;
        let full = req.data_verbosity_level == Verbosity::Full;
        if let Ok(Some(d)) = self.node.block_daa_score(req.start_hash).await {
            self.last_start.store(d, Ordering::Relaxed);
        }
        let drip = *self.drip.lock().unwrap();
        if let Some((_, delay)) = drip {
            req.min_confirmation_count = None;
            tokio::time::sleep(delay).await;
        }
        let from = self.vspc_from.lock().unwrap().clone().unwrap_or_else(|| self.node.clone());
        let mut r = from.vspc_v2(req).await?;
        if let Some((n, _)) = drip {
            r.added_chain_block_hashes.truncate(n);
            r.chain_block_accepted_transactions.truncate(n);
        }
        if full && self.alters.load(Ordering::Relaxed) {
            let tx = r.chain_block_accepted_transactions.iter_mut().flat_map(|b| b.accepted_transactions.iter_mut());
            if let Some(t) = tx.into_iter().find(|t| !t.outputs.is_empty()) {
                t.outputs[0].value += 1;
            }
        }
        Ok(r)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        let mut d = self.node.dag_info().await?;
        d.virtual_daa_score = self.claimed(d.virtual_daa_score).await;
        Ok(d)
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        let mut info = self.node.server_info().await?;
        info.has_utxo_index &= !self.no_utxo_index.load(Ordering::Relaxed);
        Ok(info)
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.node.utxos_by_addresses(a).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.node.block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        let real = self.node.sink_blue_score().await?;
        Ok(self.claimed(real).await)
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
    run_with(anchor, nodes, fetch_parallel, backoff, |_| {})
}

/// [`run`] with the follower's configuration adjusted.
fn run_with(
    anchor: Hash32,
    nodes: Vec<(NodeSpec, Arc<Flaky>)>,
    fetch_parallel: usize,
    backoff: Duration,
    adjust: impl FnOnce(&mut FollowerConfig),
) -> Run {
    let verifier = nodes[0].1.clone();
    let source = Arc::new(MultiNode::new(nodes, verifier).with_watch(Duration::from_secs(3_600), 6));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let mut cfg = FollowerConfig {
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
        authority_lag_tolerance: kobd::indexer::follower::AUTHORITY_LAG_TOLERANCE,
        chain_proof: None,
    };
    adjust(&mut cfg);
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

/// The primary's health and sink answer, its chain requests time out. After two of them in a row
/// (and the stall, zero here) the other node is the chain authority, the store catches up to the
/// tip on it and equals a clean replay. The swap is in the health (roles, `promoted`, the old
/// primary cut off with a node alarm, the authority counter) and on the status line. Sequential
/// steps and the parallel fetch alike.
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

/// The new authority's answers stay checked until the cursor moves (`MultiNode::settle`). Its chain
/// hashes from the cursor continue it, but its VSPC answer from the same cursor removes 130 chain
/// blocks: refused, nothing reverted. The chain hashes alone do not end the check, so the VSPC
/// answer is never applied as a reorg.
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

/// When the role reads its funds from the chain authority's node (the Executor, `judge::require_utxo_index`), a candidate
/// without a UTXO index is not promoted. The judge's alarm for it would come only with the next health check.
#[tokio::test]
async fn a_candidate_without_a_utxo_index_is_not_promoted_when_the_role_needs_one() {
    let _g = serial().await;
    // process-wide for this test binary: every other node here has a UTXO index
    kobd::rpc::judge::require_utxo_index();
    let node = MockNode::new("testnet-10");
    node.push_empty(20);
    let (a, b) = (Flaky::new(&node), Flaky::new(&node));
    b.no_utxo_index.store(true, Ordering::Relaxed);
    let (ua, ub) = ("ws://10.7.0.1:18210", "ws://10.7.0.2:18210");
    let r = run(node.anchor(), vec![(spec(ua, true), a.clone()), (spec(ub, true), b.clone())], 1, Duration::from_millis(1));
    r.until(500, "first sync", |r| r.cursor() == Some(node.tip())).await;
    a.chain_down.store(true, Ordering::Relaxed);
    node.push_empty(5);
    r.steps(40).await;
    assert_eq!(r.source.board().authority(), 0, "not promoted");
    assert_eq!(r.health.snapshot().nodes[1].promoted, 0);
    assert_eq!(r.health.snapshot().cursor_hash, Some(node.chain()[20].hash));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// The pace rule at test scale (`indexer::follower`, *Progress*): a chain 100 ms behind real time is too slow, and
/// another node takes over 300 ms after the last progress.
fn paced(c: &mut FollowerConfig) {
    c.authority_lag_tolerance = Duration::from_millis(100);
    c.authority_stall = Duration::from_millis(300);
}

/// R4C1-M1: a chain authority that gives the chain slower than real time from an old cursor is handed over, whatever it
/// says of its own position: one chain block a poll with its sink claimed just past the cursor (an unbounded request;
/// `catching_up` all the same, R8C1-L4), exactly 100 a poll the same way, and one a poll with a sink and a virtual DAA claimed far ahead
/// (`catching_up`, bounded windows). Each answer applies, but none is progress, so after the stall (3 minutes in
/// production, 300 ms here) the other node leads and catches up to the tip. `/v1/health` reports the lag by the cursor
/// block's age, whatever the authority claims.
#[tokio::test]
async fn an_authority_that_gives_the_chain_slower_than_real_time_is_handed_over() {
    let _g = serial().await;
    // (chain blocks an answer, ms of chain a block, ms an answer takes, claims far ahead)
    for (k, (blocks, per, delay, far)) in
        [(1usize, 10u64, 40u64, false), (100, 1, 200, false), (1, 10, 40, true)].into_iter().enumerate()
    {
        let node = MockNode::new("testnet-10");
        // two hours ago: the cursor is never fresh
        node.set_block_clock(now_ms() - 2 * 3_600_000, per);
        node.push_empty(3_000);
        let (a, b) = (Flaky::new(&node), Flaky::new(&node));
        *a.drip.lock().unwrap() = Some((blocks, Duration::from_millis(delay)));
        if far {
            a.lead.store(1_000_000, Ordering::Relaxed);
        } else {
            a.hides.store(true, Ordering::Relaxed);
        }
        let (ua, ub) = (format!("ws://10.8.{k}.1:18210"), format!("ws://10.8.{k}.2:18210"));
        let nodes = vec![(spec(&ua, true), a.clone()), (spec(&ub, true), b.clone())];
        let r = run_with(node.anchor(), nodes, 1, Duration::from_millis(1), paced);
        let t0 = std::time::Instant::now();
        let states = Mutex::new(vec![]);
        r.until(200, "handed over", |r| {
            states.lock().unwrap().push(r.health.snapshot().state);
            r.source.board().authority() == 1
        })
        .await;
        let took = t0.elapsed();
        let states = states.into_inner().unwrap();
        // Whatever it claims of its position, the follower is `catching_up`: the cursor block's verified age decides
        // (R8C1-L4). Before, a sink claimed just past the cursor kept it `following`.
        assert!(states.contains(&FollowerState::CatchingUp) && !states.contains(&FollowerState::Following), "case {k}: {states:?}");
        assert!(took < Duration::from_secs(10), "case {k}: {took:?}");
        let h = r.health.snapshot();
        assert!(h.blocks_applied_total >= blocks as u64, "case {k}: its answers applied");
        let lag = h.lag_seconds(now_ms()).unwrap();
        assert!(lag >= 2 * 3_600 - 60, "case {k}: lag_seconds {lag}: at least the cursor block's age");
        r.until(200, "caught up on the other node", |r| r.cursor() == Some(node.tip())).await;
        assert_eq!(r.source.board().authority(), 1, "case {k}");
    }
}

/// R4C1-M1 with no other node: the dripping authority stays, its answers apply, but one period after its first answer it
/// is `failing` (`/v1/health` not ok), its answers are no progress, so the health turns `stale` ten minutes after the
/// last progress, and `lag_seconds` shows the cursor block's age meanwhile.
#[tokio::test]
async fn a_lone_authority_that_drips_the_chain_turns_the_health_failing_then_stale() {
    let _g = serial().await;
    let node = MockNode::new("testnet-10");
    node.set_block_clock(now_ms() - 2 * 3_600_000, 10);
    node.push_empty(2_000);
    let a = Flaky::new(&node);
    *a.drip.lock().unwrap() = Some((1, Duration::from_millis(40)));
    a.lead.store(1_000_000, Ordering::Relaxed);
    let r = run_with(node.anchor(), vec![(spec("ws://10.9.0.1:18210", true), a.clone())], 1, Duration::from_millis(1), paced);
    r.until(100, "failing", |r| r.health.snapshot().authority_failing()).await;
    let at = r.health.snapshot().last_progress_unix_ms;
    let out = r.steps(10).await;
    assert!(out.iter().all(|o| matches!(o, StepOutcome::Applied { .. })), "{out:?}");
    let h = r.health.snapshot();
    assert_eq!(h.state, FollowerState::CatchingUp);
    assert!(h.authority_failing() && !h.caught_up());
    assert_eq!(h.last_progress_unix_ms, at, "no progress");
    let stale_after = kobd::indexer::status::STALE_AFTER_MS;
    assert!(!h.stale(at + stale_after) && h.stale(at + stale_after + 1));
    let lag = h.lag_seconds(now_ms()).unwrap();
    assert!(lag >= 2 * 3_600 - 60, "the node claims 1,000,000 DAA ahead, the cursor is 2 h old: {lag}");
    assert_eq!(r.source.board().authority(), 0);
}

/// The honest cases of the pace rule, none of which ever counts against the authority (at test scale, a tolerance of
/// 500 ms and a period of 1 s):
/// - a restart 15 minutes behind on an honest node: it catches up faster than real time, is never failing, and the
///   health shows the real lag until it is fresh;
/// - a deep reorg, from a fresh cursor and from an old one;
/// - an answer that pauses (here 300 ms) in a catch-up from an old cursor;
/// - at the tip, a node that claims a sink far ahead in every other answer (R4C1-I3): a cursor within the tolerance is
///   progress, and the state follows the cursor's verified age, so it stays `following` (R9C1-L2; before, the claim
///   flipped it to `catching_up`).
#[tokio::test]
async fn honest_nodes_are_never_failing_by_the_pace_rule() {
    let _g = serial().await;
    let tolerant = |c: &mut FollowerConfig| {
        c.authority_lag_tolerance = Duration::from_millis(500);
        c.authority_stall = Duration::from_secs(1);
    };
    // a restart 15 minutes behind: 9,000 chain blocks of 100 ms, the last one now
    let node = MockNode::new("testnet-10");
    node.set_block_clock(now_ms() - 900_000, 100);
    node.push_empty(9_000);
    let (a, b) = (Flaky::new(&node), Flaky::new(&node));
    let nodes = vec![(spec("ws://10.10.0.1:18210", true), a.clone()), (spec("ws://10.10.0.2:18210", true), b.clone())];
    let r = run_with(node.anchor(), nodes, 2, Duration::from_millis(1), tolerant);
    r.steps(2).await;
    let lag = r.health.snapshot().lag_seconds(now_ms()).unwrap();
    assert!(lag >= 800, "the real lag shows: {lag}");
    r.until(500, "caught up", |r| r.cursor() == Some(node.tip())).await;
    let h = r.health.snapshot();
    assert_eq!(r.source.board().authority(), 0, "never handed over");
    assert!(!h.stale(now_ms()) && now_ms() - h.last_progress_unix_ms < 5_000, "{h:?}");
    assert!(h.lag_seconds(now_ms()).unwrap() < 60);

    // a deep reorg from the fresh cursor
    node.reorg(500, vec![vec![]; 520]);
    r.until(200, "the reorg applied", |r| r.cursor() == Some(node.tip())).await;
    assert!(r.health.snapshot().reorgs_total >= 1);
    assert_eq!(r.source.board().authority(), 0);

    // an old cursor: two hours ago, followed at its own pace
    let node = MockNode::new("testnet-10");
    node.set_block_clock(now_ms() - 2 * 3_600_000, 100);
    node.push_empty(300);
    let (a, b) = (Flaky::new(&node), Flaky::new(&node));
    let nodes = vec![(spec("ws://10.10.1.1:18210", true), a.clone()), (spec("ws://10.10.1.2:18210", true), b.clone())];
    let r = run_with(node.anchor(), nodes, 1, Duration::from_millis(1), tolerant);
    r.until(200, "first sync", |r| r.cursor() == Some(node.tip())).await;
    // a deep reorg there
    node.reorg(200, vec![vec![]; 210]);
    r.until(200, "the reorg applied", |r| r.cursor() == Some(node.tip())).await;
    // a pause of 300 ms in a catch-up (one chain block after it), then fast again
    node.push_empty(20_000);
    *a.drip.lock().unwrap() = Some((1, Duration::from_millis(300)));
    let before = r.health.snapshot().last_progress_unix_ms;
    let out = r.steps(1).await;
    assert!(matches!(out[..], [StepOutcome::Applied { added: 1, .. }]), "{out:?}");
    assert!(r.health.snapshot().last_progress_unix_ms > before, "within the period: progress");
    *a.drip.lock().unwrap() = None;
    r.until(400, "caught up after the pause", |r| r.cursor() == Some(node.tip())).await;
    assert_eq!(r.source.board().authority(), 0, "a pause is no failure");
    assert!(!r.health.snapshot().authority_failing());

    // at the tip with a fresh cursor: the state flickers, every answer is progress
    let node = MockNode::new("testnet-10");
    node.set_block_clock(now_ms(), 1);
    node.push_empty(10);
    let (a, b) = (Flaky::new(&node), Flaky::new(&node));
    let nodes = vec![(spec("ws://10.10.2.1:18210", true), a.clone()), (spec("ws://10.10.2.2:18210", true), b.clone())];
    let r = run_with(node.anchor(), nodes, 1, Duration::from_millis(1), tolerant);
    r.until(200, "first sync", |r| r.cursor() == Some(node.tip())).await;
    let mut states = vec![];
    for i in 0..10u64 {
        // claims far ahead and answers what there is, or not: the verified age decides, so `following` both times
        a.lead.store(if i % 2 == 0 { 1_000_000 } else { 0 }, Ordering::Relaxed);
        *a.drip.lock().unwrap() = (i % 2 == 0).then_some((2_480, Duration::ZERO));
        tokio::time::sleep(Duration::from_millis(20)).await;
        node.push_empty(1);
        let before = r.health.snapshot().last_progress_unix_ms;
        r.steps(1).await;
        let h = r.health.snapshot();
        states.push(h.state);
        assert!(h.last_progress_unix_ms > before, "a fresh cursor is progress in {:?}", h.state);
    }
    assert!(states.iter().all(|s| *s == FollowerState::Following), "the sink claim does not decide the state: {states:?}");
    assert_eq!(r.source.board().authority(), 0);
}
