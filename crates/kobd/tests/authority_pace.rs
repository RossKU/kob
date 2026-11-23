//! The rule that judges the chain authority's progress (`indexer::follower`, *Progress*) with its production constants (a
//! tolerance of 90 s, a period of 180 s) on paused time: honest setups it must never blame (a catch-up of two days, the
//! tip with ordinary jitter for an hour, a pause of the follower itself), the drips it must and must not hand over, and a
//! network that stops making blocks for 20 minutes under an own node with the failover to the public pair. The follower
//! runs as in production (`Follower::run`); the chain is mined on the virtual clock as the test runs.

use kobd::config::StartMode;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::follower::{Follower, FollowerConfig, AUTHORITY_LAG_TOLERANCE, AUTHORITY_STALL};
use kobd::indexer::ingest::Ingest;
use kobd::indexer::status::{FollowerState, HealthState, PaceState};
use kobd::rpc::failover::{Failover, FailoverPolicy};
use kobd::rpc::multi::{MultiNode, NodeSpec};
use kobd::rpc::types::*;
use kobd::rpc::{ChainSource, Origin, RpcError};
use kobd::testkit::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The virtual clock (Unix ms): the follower's local clock reads the same.
#[derive(Clone)]
struct Clock {
    t0: tokio::time::Instant,
    base: u64,
}

impl Clock {
    fn new() -> Clock {
        Clock { t0: tokio::time::Instant::now(), base: 1_800_000_000_000 }
    }
    fn now(&self) -> u64 {
        self.base + self.t0.elapsed().as_millis() as u64
    }
}

/// Keeps the shared chain at the virtual time, a block every `per` ms, unless the network is halted.
struct Miner {
    node: Arc<MockNode>,
    clock: Clock,
    next: AtomicU64,
    per: u64,
    halted: AtomicBool,
}

impl Miner {
    /// A chain whose blocks run from `age` ms ago to now, one every `per` ms.
    fn new(clock: &Clock, age: u64, per: u64) -> Arc<Miner> {
        let node = MockNode::new("testnet-10");
        let from = clock.now() - age;
        node.set_block_clock(from, per);
        // small answers: a long chain behind the cursor is not rebuilt at every poll
        node.set_added_cap(50);
        let m = Arc::new(Miner { node, clock: clock.clone(), next: AtomicU64::new(from), per, halted: AtomicBool::new(false) });
        m.mine();
        m
    }

    fn mine(&self) {
        if self.halted.load(Ordering::Relaxed) {
            return;
        }
        let now = self.clock.now();
        let next = self.next.load(Ordering::Relaxed);
        if now >= next {
            let n = (now - next) / self.per + 1;
            self.node.push_empty(n as usize);
            self.next.store(next + n * self.per, Ordering::Relaxed);
        }
    }

    /// The network stops making blocks.
    fn halt(&self) {
        self.halted.store(true, Ordering::Relaxed);
    }

    /// Blocks come again, stamped from now (none for the time of the halt).
    fn resume(&self) {
        let now = self.clock.now();
        self.node.set_block_clock(now, self.per);
        self.next.store(now, Ordering::Relaxed);
        self.halted.store(false, Ordering::Relaxed);
        self.mine();
    }

    fn first(&self) -> Hash32 {
        self.node.chain()[1].hash
    }
}

/// A node of the shared chain: honest, but it may serve the chain at a pace, or a set time behind.
struct Node {
    miner: Arc<Miner>,
    /// Serves only the chain blocks stamped at least this long ago (ms).
    hold_ms: Mutex<Option<u64>>,
    /// Serves the chain at this many times real time from a block's time at an instant: (times, the instant, the
    /// block's time); a link or a node of that speed.
    speed: Mutex<Option<(u64, u64, u64)>>,
    /// Holds the chain back by an amount that wanders between 2 and 30 s from answer to answer (ordinary jitter at the
    /// tip: a slow answer, a block that took long).
    jitter: AtomicU64,
}

impl Node {
    fn new(miner: &Arc<Miner>) -> Arc<Node> {
        Arc::new(Node { miner: miner.clone(), hold_ms: Mutex::new(None), speed: Mutex::new(None), jitter: AtomicU64::new(0) })
    }

    fn chain(&self) -> &Arc<MockNode> {
        &self.miner.node
    }

    /// Serve the chain at `x` times real time from the block stamped `from`.
    fn at_speed(&self, x: u64, from: u64) {
        *self.speed.lock().unwrap() = Some((x, self.miner.clock.now(), from));
    }
}

impl ChainSource for Node {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.miner.mine();
        let mut r = self.chain().vspc_v2(req).await?;
        let now = self.miner.clock.now();
        let mut until = u64::MAX;
        if let Some(hold) = *self.hold_ms.lock().unwrap() {
            until = until.min(now.saturating_sub(hold));
        }
        let j = self.jitter.load(Ordering::Relaxed);
        if j > 0 {
            // 2 to 30 s and back every 10 minutes, with a wobble from answer to answer
            let k = self.jitter.fetch_add(7_919, Ordering::Relaxed) % 2_001;
            let phase = (now / 1_000) % 600;
            let tri = (if phase < 300 { phase } else { 600 - phase }) * 26_000 / 300;
            until = until.min(now.saturating_sub(2_000 + tri + k));
        }
        if let Some((x, at, from)) = *self.speed.lock().unwrap() {
            until = until.min(from + x * now.saturating_sub(at));
        }
        let n = r.chain_block_accepted_transactions.iter().take_while(|b| b.chain_block_header.timestamp <= until).count();
        r.added_chain_block_hashes.truncate(n);
        r.chain_block_accepted_transactions.truncate(n);
        Ok(r)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.miner.mine();
        self.chain().dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.chain().server_info().await
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.chain().utxos_by_addresses(a).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.chain().block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.miner.mine();
        self.chain().sink_blue_score().await
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.chain().block_blue_score(hash).await
    }
    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.chain().block_daa_score(hash).await
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.miner.mine();
        ChainSource::chain_hashes(&**self.chain(), start).await
    }
    async fn chain_with_ids(&self, start: Hash32, m: Option<u64>) -> Result<ChainIds, RpcError> {
        self.miner.mine();
        self.chain().chain_with_ids(start, m).await
    }
    async fn block(&self, hash: Hash32, _txs: bool) -> Result<Option<(kaspa_consensus_core::block::Block, Origin)>, RpcError> {
        let b = self.chain().chain().into_iter().find(|b| b.hash == hash);
        Ok(b.map(|b| (kaspa_consensus_core::block::Block::from_header(b.header.clone()), Origin::PRIMARY)))
    }
}

/// The judge and the promotion count are process-wide: these tests run one at a time.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static M: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let g = M.lock().await;
    kobd::rpc::resolver::set_network("testnet-10");
    g
}

fn spec(url: &str) -> NodeSpec {
    NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check: true, may_lead: true }
}

fn config(start: Hash32, poll: Duration) -> FollowerConfig {
    FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(start),
        poll_interval: poll,
        backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        min_confirmations: None,
        max_walkback_blocks: 1_000,
        status_refresh: Duration::from_millis(0),
        caught_up_daa: 100,
        batch_initial_blue: 600,
        batch_target: Duration::from_secs(30),
        fetch_parallel: 1,
        prefetch_max_bytes: 256 << 20,
        prefetch_min_lag_blue: 1 << 30,
        prefetch_initial_blocks: 6,
        authority_failures: kobd::indexer::follower::AUTHORITY_FAILURES,
        authority_stall: AUTHORITY_STALL,
        authority_lag_tolerance: AUTHORITY_LAG_TOLERANCE,
        chain_proof: None,
    }
}

fn follower<S: ChainSource>(source: &Arc<S>, cfg: FollowerConfig, clock: &Clock) -> (Follower<S>, Arc<HealthState>) {
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let mut f = Follower::new(source.clone(), ingest, cfg, health.clone(), events);
    f.set_clock_at(clock.now());
    (f, health)
}

/// A named pair, the first node the chain authority, each cross-checking the other.
fn pair(a: &Arc<Node>, b: &Arc<Node>, k: u32) -> Arc<MultiNode<Node>> {
    let nodes = vec![(spec(&format!("ws://10.50.{k}.1:18210")), a.clone()), (spec(&format!("ws://10.50.{k}.2:18210")), b.clone())];
    Arc::new(MultiNode::new(nodes, a.clone()).with_watch(Duration::from_secs(3 * 3_600), 6))
}

/// Runs `f` as in production while `drive` runs; `drive` is called every second of virtual time with the seconds
/// elapsed, and ends the run by returning false.
async fn drive<S: ChainSource>(f: &Follower<S>, mut each_second: impl FnMut(u64) -> bool) {
    let (stop, rx) = tokio::sync::watch::channel(false);
    let driver = async {
        let mut s = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            s += 1;
            if !each_second(s) {
                break;
            }
        }
        let _ = stop.send(true);
    };
    tokio::join!(f.run(rx), driver);
}

/// The cursor block's lag behind the virtual clock, in seconds.
fn lag_secs(h: &HealthState, clock: &Clock) -> Option<i64> {
    h.snapshot().cursor_time_unix_ms.map(|t| (clock.now() as i64 - t as i64) / 1_000)
}

/// An honest catch-up from two days behind at five times real time (a slow link): above the tolerance for 12 hours, it
/// gains 720 s on real time in every period of 180 s, so it is never blamed and the health is `ok` throughout
/// (`authority_pace` `gaining`, then `keeping_up`).
#[tokio::test(start_paused = true)]
async fn an_honest_catch_up_of_two_days_at_five_times_real_time_is_never_blamed() {
    let _g = serial().await;
    let clock = Clock::new();
    let miner = Miner::new(&clock, 2 * 86_400_000, 60_000);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    a.at_speed(5, clock.now() - 2 * 86_400_000);
    let source = pair(&a, &b, 0);
    let (f, health) = follower(&source, config(miner.first(), Duration::from_secs(20)), &clock);
    let mut failing = 0;
    drive(&f, |s| {
        if health.snapshot().authority_failing() {
            failing += 1;
        }
        let lag = lag_secs(&health, &clock);
        !(s > 600 && lag.is_some_and(|l| l <= 60)) && s < 14 * 3_600
    })
    .await;
    let h = health.snapshot();
    assert_eq!(source.board().authority(), 0, "never handed over: {h:?}");
    assert_eq!(failing, 0, "never failing");
    assert!(lag_secs(&health, &clock).is_some_and(|l| l <= 90), "caught up: {:?}", lag_secs(&health, &clock));
    assert_eq!(h.authority_pace.map(|p| p.state), Some(PaceState::KeepingUp));
}

/// An honest node at the tip whose answers hold the chain back by 2 to 30 s, wandering, for an hour: never blamed, the
/// health keeping up all the time.
#[tokio::test(start_paused = true)]
async fn an_honest_node_at_the_tip_with_ordinary_jitter_is_never_blamed_in_an_hour() {
    let _g = serial().await;
    let clock = Clock::new();
    let miner = Miner::new(&clock, 5_000, 1_000);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    a.jitter.store(1, Ordering::Relaxed);
    let source = pair(&a, &b, 1);
    let (f, health) = follower(&source, config(miner.first(), Duration::from_secs(1)), &clock);
    let (mut lags, mut not_keeping) = (vec![], 0);
    drive(&f, |s| {
        if s > 60 {
            if let Some(l) = lag_secs(&health, &clock) {
                lags.push(l);
            }
            if health.snapshot().authority_pace.map(|p| p.state) != Some(PaceState::KeepingUp) {
                not_keeping += 1;
            }
        }
        s < 3_600
    })
    .await;
    assert_eq!(source.board().authority(), 0, "never handed over");
    assert_eq!(not_keeping, 0, "keeping up all the time");
    let (min, max) = (lags.iter().min().copied().unwrap(), lags.iter().max().copied().unwrap());
    assert!(min >= 1 && (20..=32).contains(&max), "the lag wandered between {min} and {max} s");
}

/// R5C1-L1: the follower itself stops for 15 minutes (no step: a stopped process, a host asleep), then its honest
/// authority catches up at five times real time. Before, the time of the pause counted against the authority, which was
/// deposed at once. Now a gap longer than `PAUSE_CAP` starts the period again from the lag at that moment: never blamed.
#[tokio::test(start_paused = true)]
async fn a_pause_of_the_follower_itself_is_not_the_authoritys_failure() {
    let _g = serial().await;
    let clock = Clock::new();
    let miner = Miner::new(&clock, 5_000, 1_000);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    let source = pair(&a, &b, 2);
    let (f, health) = follower(&source, config(miner.first(), Duration::from_secs(1)), &clock);
    drive(&f, |s| s < 120).await;
    assert!(lag_secs(&health, &clock).is_some_and(|l| l <= 5));
    // the pause: no step for 15 minutes, while the chain goes on
    tokio::time::sleep(Duration::from_secs(15 * 60)).await;
    miner.mine();
    let cursor = health.snapshot().cursor_time_unix_ms.unwrap();
    a.at_speed(5, cursor);
    let mut failing = 0;
    drive(&f, |s| {
        failing += health.snapshot().authority_failing() as u32;
        s < 600
    })
    .await;
    assert_eq!(source.board().authority(), 0, "not handed over");
    assert_eq!(failing, 0, "never failing");
    assert!(lag_secs(&health, &clock).is_some_and(|l| l <= 90), "caught up: {:?}", lag_secs(&health, &clock));
}

/// A drip that holds the cursor block 100 s behind (above the 90 s tolerance) is handed over within one period of the
/// lag passing the tolerance; one that holds it 60 s behind is within the tolerance and is never handed over.
#[tokio::test(start_paused = true)]
async fn a_drip_at_100_s_is_handed_over_and_one_at_60_s_is_not() {
    let _g = serial().await;
    for (k, (hold, handed)) in [(100_000u64, true), (60_000, false)].into_iter().enumerate() {
        let clock = Clock::new();
        let miner = Miner::new(&clock, 5_000, 1_000);
        let (a, b) = (Node::new(&miner), Node::new(&miner));
        *a.hold_ms.lock().unwrap() = Some(hold);
        let source = pair(&a, &b, 3 + k as u32);
        let (f, health) = follower(&source, config(miner.first(), Duration::from_secs(1)), &clock);
        let mut at = None;
        drive(&f, |s| {
            if at.is_none() && source.board().authority() != 0 {
                at = Some(s);
            }
            s < 1_800 && at.is_none()
        })
        .await;
        if handed {
            // the lag passes 90 s about 85 s in (the cursor starts 5 s old), then one period of 180 s
            let at = at.expect("handed over");
            assert!((180..=300).contains(&at), "handed over after {at} s");
            assert!(health.snapshot().authority_replaced_total >= 1);
        } else {
            assert_eq!(at, None, "a lag of 60 s is within the tolerance");
            assert_eq!(health.snapshot().authority_pace.map(|p| p.state), Some(PaceState::KeepingUp));
        }
    }
}

/// A lone authority that holds the index 40 minutes behind has nothing to hand over to: the rule drives the health. One
/// period after the start it is `failing`: `/v1/health` is not ok and not caught up (the executor plans nothing), the
/// progress stops so it turns `stale` 10 minutes later, and `lag_seconds` shows the lag all along.
#[tokio::test(start_paused = true)]
async fn a_lone_authority_that_holds_the_index_back_turns_the_health_failing() {
    let _g = serial().await;
    let clock = Clock::new();
    let miner = Miner::new(&clock, 40 * 60_000, 1_000);
    let a = Node::new(&miner);
    *a.hold_ms.lock().unwrap() = Some(40 * 60_000);
    let source = Arc::new(MultiNode::new(vec![(spec("ws://10.51.0.1:18210"), a.clone())], a.clone()));
    let (f, health) = follower(&source, config(miner.first(), Duration::from_secs(1)), &clock);
    let (mut first_failing, mut running) = (None, true);
    drive(&f, |s| {
        let h = health.snapshot();
        if first_failing.is_none() && h.authority_failing() {
            first_failing = Some(s);
        }
        running &= s < 10 || matches!(h.state, FollowerState::Following | FollowerState::CatchingUp);
        s < 400
    })
    .await;
    let at = first_failing.expect("failing");
    assert!((180..=200).contains(&at), "failing after {at} s");
    let h = health.snapshot();
    assert!(h.authority_failing() && !h.caught_up() && !h.within_lag_tolerance());
    assert!(h.status_line().contains("CHAIN AUTHORITY NOT KEEPING UP"), "{}", h.status_line());
    assert!(h.metrics_text().contains("kob_indexer_authority_failing 1"));
    assert!(running, "it goes on applying what the node serves");
}

/// The Kaspa network stops making blocks for 20 minutes under an own node with `[failover] fallback`. Every authority
/// fails the same way, so the follower moves between the own node and the fallback every few minutes: it never stops,
/// always has a node, and once blocks come again it is on a fresh cursor within one period and on its own node within
/// the probes' time (3 probes 30 s apart).
#[tokio::test(start_paused = true)]
async fn a_halted_network_with_an_own_node_and_a_fallback_costs_only_switches() {
    let _g = serial().await;
    let clock = Clock::new();
    let miner = Miner::new(&clock, 5_000, 1_000);
    let (own, fallback) = (Node::new(&miner), Node::new(&miner));
    let policy = FailoverPolicy {
        after_failures: 5,
        after: Duration::from_secs(60),
        probe_every: Duration::from_secs(30),
        return_after_probes: 3,
        max_reorg_blocks: 100,
        max_behind_blue: 600,
        check_timeout: Duration::from_secs(20),
    };
    let source = Arc::new(Failover::new(own.clone(), fallback.clone(), "ws://10.52.0.1:18210", "resolver", "testnet-10", policy));
    let (f, health) = follower(&source, config(miner.first(), Duration::from_secs(1)), &clock);
    // a while on the own node first (a deposal more than 30 minutes into a stint is no strike), then 20 minutes of halt
    let (halt, resume) = (2_400u64, 2_400 + 1_200);
    let (mut fresh_at, mut own_at, mut stopped) = (None, None, false);
    drive(&f, |s| {
        if s == halt {
            miner.halt();
        }
        if s == resume {
            miner.resume();
        }
        let h = health.snapshot();
        stopped |= matches!(h.state, FollowerState::Stopped | FollowerState::Gap);
        if s > resume {
            if fresh_at.is_none() && lag_secs(&health, &clock).is_some_and(|l| l <= 90) {
                fresh_at = Some(s - resume);
            }
            if own_at.is_none() && fresh_at.is_some() && source.active() == 0 {
                own_at = Some(s - resume);
            }
        }
        s < resume + 900 && own_at.is_none()
    })
    .await;
    let st = source.status();
    assert!(!stopped, "never stopped");
    assert!(st.switches >= 4, "switched back and forth during the halt: {st:?}");
    let fresh = fresh_at.expect("a fresh cursor after the halt");
    assert!(fresh <= 180, "fresh {fresh} s after blocks came again");
    let own = own_at.expect("back on the own node");
    assert!(own <= 180 + 3 * 30 + 30, "on the own node {own} s after blocks came again: {st:?}");
}
