//! R6C1-M1: a chain authority that answers late but lands each commit within the tolerance. The lag is the sawtooth of
//! a step: low right after a commit, high while the authority holds its next answer back. Judged only after each commit,
//! it never showed. The rule now judges the lag every second, inside the step (`indexer::follower`, *Progress*).
//! Production constants (a tolerance of 90 s, a period of 180 s) on paused time; a named pair of mock nodes on one chain
//! mined on the virtual clock, the other node honest.
//!
//! These tests use only what the follower offered before the round-6 rule, so that they run against that code too.

use kobd::config::StartMode;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::follower::{Follower, FollowerConfig, AUTHORITY_LAG_TOLERANCE, AUTHORITY_STALL};
use kobd::indexer::ingest::Ingest;
use kobd::indexer::status::HealthState;
use kobd::rpc::multi::{MultiNode, NodeSpec};
use kobd::rpc::types::*;
use kobd::rpc::{ChainSource, Origin, RpcError};
use kobd::testkit::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// The shared chain at the virtual time, a block a second.
struct Miner {
    node: Arc<MockNode>,
    clock: Clock,
    next: AtomicU64,
}

impl Miner {
    fn new(clock: &Clock) -> Arc<Miner> {
        let node = MockNode::new("testnet-10");
        node.set_added_cap(50);
        let from = clock.now() - 5_000;
        node.set_block_clock(from, 1_000);
        let m = Arc::new(Miner { node, clock: clock.clone(), next: AtomicU64::new(from) });
        m.mine();
        m
    }
    fn mine(&self) {
        let now = self.clock.now();
        let next = self.next.load(Ordering::Relaxed);
        if now >= next {
            let n = (now - next) / 1_000 + 1;
            self.node.push_empty(n as usize);
            self.next.store(next + n * 1_000, Ordering::Relaxed);
        }
    }
}

/// A node of the chain; the authority of these tests answers its sink and its VSPC requests late.
struct Node {
    miner: Arc<Miner>,
    /// `getSinkBlueScore` takes this long (ms) and names a sink just past the cursor it was last asked from.
    sink_delay: AtomicU64,
    /// `getVirtualChainFromBlockV2` takes this long (ms) and serves the chain as it stood when asked.
    vspc_delay: AtomicU64,
    last_start: AtomicU64,
}

impl Node {
    fn new(miner: &Arc<Miner>) -> Arc<Node> {
        Arc::new(Node { miner: miner.clone(), sink_delay: AtomicU64::new(0), vspc_delay: AtomicU64::new(0), last_start: AtomicU64::new(0) })
    }
    fn chain(&self) -> &Arc<MockNode> {
        &self.miner.node
    }
}

impl ChainSource for Node {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.miner.mine();
        if let Ok(Some(d)) = self.chain().block_daa_score(req.start_hash).await {
            self.last_start.store(d, Ordering::Relaxed);
        }
        let asked = self.miner.clock.now();
        let mut r = self.chain().vspc_v2(req).await?;
        let delay = self.vspc_delay.load(Ordering::Relaxed);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let n = r.chain_block_accepted_transactions.iter().take_while(|b| b.chain_block_header.timestamp <= asked).count();
            r.added_chain_block_hashes.truncate(n);
            r.chain_block_accepted_transactions.truncate(n);
        }
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
        let delay = self.sink_delay.load(Ordering::Relaxed);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            return Ok(self.last_start.load(Ordering::Relaxed) + 1);
        }
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

async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static M: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let g = M.lock().await;
    kobd::rpc::resolver::set_network("testnet-10");
    g
}

fn spec(url: &str) -> NodeSpec {
    NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check: true, may_lead: true }
}

/// Runs the follower for an hour of virtual time over a named pair whose first node answers late: whether it was handed
/// over, and the largest `lag_seconds` seen before that.
async fn an_hour(k: u32, sink_delay: u64, vspc_delay: u64) -> (bool, u64) {
    let clock = Clock::new();
    let miner = Miner::new(&clock);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    a.sink_delay.store(sink_delay, Ordering::Relaxed);
    a.vspc_delay.store(vspc_delay, Ordering::Relaxed);
    let nodes = vec![(spec(&format!("ws://10.60.{k}.1:18210")), a.clone()), (spec(&format!("ws://10.60.{k}.2:18210")), b.clone())];
    let source = Arc::new(MultiNode::new(nodes, a.clone()).with_watch(Duration::from_secs(3 * 3_600), 6));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(miner.node.chain()[1].hash),
        poll_interval: Duration::from_millis(500),
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
    };
    let mut f = Follower::new(source.clone(), ingest, cfg, health.clone(), events);
    f.set_clock_at(clock.now());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let (mut handed, mut max_lag) = (false, 0u64);
    let driver = async {
        for _ in 0..3_600 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if source.board().authority() != 0 {
                handed = true;
                break;
            }
            let h = health.snapshot();
            if let Some(t) = h.cursor_time_unix_ms {
                max_lag = max_lag.max(clock.now().saturating_sub(t) / 1_000);
            }
        }
        let _ = stop.send(true);
    };
    tokio::join!(f.run(rx), driver);
    (handed, max_lag)
}

/// The finding's case: the sink answered after 170 s, naming a sink just past the cursor (so the request is unbounded),
/// then the VSPC answer after 85 s with the chain as it stood when asked. Each commit lands about 85 s behind the
/// local clock, within the tolerance, while the lag swings up to about 340 s in between. Judged every second, the lag
/// is above the tolerance for most of each step: the period fails, the step is given up and the other node leads,
/// within the hour, the lag never far above one period past the tolerance.
#[tokio::test(start_paused = true)]
async fn an_authority_that_answers_late_but_lands_each_commit_within_the_tolerance_is_handed_over() {
    let _g = serial().await;
    let (handed, max_lag) = an_hour(0, 170_000, 85_000).await;
    assert!(handed, "never handed over in an hour (largest lag {max_lag} s)");
    assert!(max_lag <= 90 + 180 + 30, "the lag reached {max_lag} s");
}

/// The same with the VSPC delay alone (85 s): the lag swings between about 87 s and about 175 s, above the tolerance
/// for half of each step. The period never sees the lag fall by half a period: handed over within the hour.
#[tokio::test(start_paused = true)]
async fn an_authority_whose_vspc_answers_come_late_is_handed_over() {
    let _g = serial().await;
    let (handed, max_lag) = an_hour(1, 0, 85_000).await;
    assert!(handed, "never handed over in an hour (largest lag {max_lag} s)");
    assert!(max_lag <= 90 + 180 + 30, "the lag reached {max_lag} s");
}
