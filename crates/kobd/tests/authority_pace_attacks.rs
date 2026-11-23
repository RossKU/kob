//! Attacks of a chain authority on the rule that judges its progress (`indexer::follower`, *Progress*), at test scale:
//! a tolerance of 100 ms and a period of 300 ms instead of 90 s and 180 s, on the real clock. Each attack holds the index
//! back while it leads; each must cost it the role within about one period, and a cross-checking node's claims must cost
//! an honest authority nothing. Mock nodes on one chain whose blocks are stamped on the real clock and mined up to it
//! as the test runs.
//!
//! These tests use only what the follower offered before the round-5 rule, so that they run against that code too.

use kobd::config::StartMode;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kobd::indexer::ingest::Ingest;
use kobd::indexer::status::HealthState;
use kobd::rpc::multi::{MultiNode, NodeSpec};
use kobd::rpc::types::*;
use kobd::rpc::{ChainSource, Origin, RpcError};
use kobd::testkit::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Keeps the shared chain at the real time: a block every `per` ms.
struct Miner {
    node: Arc<MockNode>,
    next: AtomicU64,
    per: u64,
}

impl Miner {
    /// A chain whose blocks run from `age` ms ago to now, one every `per` ms.
    fn new(age: u64, per: u64) -> Arc<Miner> {
        let node = MockNode::new("testnet-10");
        let from = now_ms() - age;
        node.set_block_clock(from, per);
        let m = Arc::new(Miner { node, next: AtomicU64::new(from), per });
        m.mine();
        m
    }

    fn mine(&self) {
        let now = now_ms();
        let next = self.next.load(Ordering::Relaxed);
        if now >= next {
            let n = (now - next) / self.per + 1;
            self.node.push_empty(n as usize);
            self.next.store(next + n * self.per, Ordering::Relaxed);
        }
    }

    /// The first stamped block: `age` ms old at the start.
    fn first(&self) -> Hash32 {
        self.node.chain()[1].hash
    }
}

/// A node of the shared chain, honest unless told otherwise.
struct Node {
    miner: Arc<Miner>,
    /// Answers every VSPC request with nothing new (R5C1-M1).
    empty: AtomicBool,
    /// Claims its sink and its virtual DAA score just past the cursor it was last asked from (R5C1-M1: "nothing more
    /// follows").
    hides: AtomicBool,
    last_start: AtomicU64,
    /// Its `getBlock` takes this long, in ms (R5C1-M1: past the lead check's 5 s).
    block_delay_ms: AtomicU64,
    /// Serves only the chain blocks stamped at least this long ago, in ms: a lag held level (R5C1-M2, a drip).
    hold_ms: Mutex<Option<u64>>,
    /// Stamps every header it serves with the time now (R5C1-L2).
    stamps_now: AtomicBool,
    /// Claims a sink and a virtual DAA score this far ahead, and this many made-up chain blocks past its real chain.
    claims_ahead: AtomicU64,
}

impl Node {
    fn new(miner: &Arc<Miner>) -> Arc<Node> {
        Arc::new(Node {
            miner: miner.clone(),
            empty: AtomicBool::new(false),
            hides: AtomicBool::new(false),
            last_start: AtomicU64::new(0),
            block_delay_ms: AtomicU64::new(0),
            hold_ms: Mutex::new(None),
            stamps_now: AtomicBool::new(false),
            claims_ahead: AtomicU64::new(0),
        })
    }

    fn chain(&self) -> &Arc<MockNode> {
        &self.miner.node
    }

    fn claimed(&self, real: u64) -> u64 {
        if self.hides.load(Ordering::Relaxed) {
            return self.last_start.load(Ordering::Relaxed) + 1;
        }
        real + self.claims_ahead.load(Ordering::Relaxed)
    }
}

impl ChainSource for Node {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.miner.mine();
        if let Ok(Some(d)) = self.chain().block_daa_score(req.start_hash).await {
            self.last_start.store(d, Ordering::Relaxed);
        }
        let mut r = self.chain().vspc_v2(req).await?;
        if self.empty.load(Ordering::Relaxed) {
            r.added_chain_block_hashes.clear();
            r.chain_block_accepted_transactions.clear();
            r.removed_chain_block_hashes.clear();
        }
        if let Some(hold) = *self.hold_ms.lock().unwrap() {
            let until = now_ms().saturating_sub(hold);
            let n = r.chain_block_accepted_transactions.iter().take_while(|b| b.chain_block_header.timestamp <= until).count();
            r.added_chain_block_hashes.truncate(n);
            r.chain_block_accepted_transactions.truncate(n);
        }
        if self.stamps_now.load(Ordering::Relaxed) {
            for b in &mut r.chain_block_accepted_transactions {
                b.chain_block_header.timestamp = now_ms();
            }
        }
        Ok(r)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.miner.mine();
        let mut d = self.chain().dag_info().await?;
        d.virtual_daa_score = self.claimed(d.virtual_daa_score);
        Ok(d)
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
        let real = self.chain().sink_blue_score().await?;
        Ok(self.claimed(real))
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
        let mut ids = self.chain().chain_with_ids(start, m).await?;
        let extra = self.claims_ahead.load(Ordering::Relaxed).min(150);
        ids.added_chain_block_hashes.extend((0..extra).map(|i| Hash32([0xa0, (i % 251) as u8, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7])));
        Ok(ids)
    }
    async fn block(&self, hash: Hash32, _txs: bool) -> Result<Option<(kaspa_consensus_core::block::Block, Origin)>, RpcError> {
        let delay = self.block_delay_ms.load(Ordering::Relaxed);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        let Some(b) = self.chain().chain().into_iter().find(|b| b.hash == hash) else { return Ok(None) };
        let mut header = b.header.clone();
        if self.stamps_now.load(Ordering::Relaxed) {
            header.timestamp = now_ms();
        }
        Ok(Some((kaspa_consensus_core::block::Block::from_header(header), Origin::PRIMARY)))
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

/// The rule at test scale: a tolerance of 100 ms and a period of 300 ms.
const TOLERANCE: Duration = Duration::from_millis(100);
const PERIOD: Duration = Duration::from_millis(300);

struct Run {
    source: Arc<MultiNode<Node>>,
    follower: Follower<MultiNode<Node>>,
    health: Arc<HealthState>,
}

/// A follower from `start` over named nodes (the first the chain authority, each cross-checking the other).
fn run(start: Hash32, nodes: Vec<(NodeSpec, Arc<Node>)>) -> Run {
    run_with(start, nodes, TOLERANCE, PERIOD)
}

/// [`run`] with another tolerance and period.
fn run_with(start: Hash32, nodes: Vec<(NodeSpec, Arc<Node>)>, tolerance: Duration, period: Duration) -> Run {
    let verifier = nodes[0].1.clone();
    let source = Arc::new(MultiNode::new(nodes, verifier).with_watch(Duration::from_secs(3_600), 6));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(start),
        poll_interval: Duration::from_millis(5),
        backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(40),
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
        authority_failures: 2,
        authority_stall: period,
        authority_lag_tolerance: tolerance,
        chain_proof: None,
    };
    let follower = Follower::new(source.clone(), ingest, cfg, health.clone(), events);
    Run { source, follower, health }
}

impl Run {
    /// Steps, 5 ms apart, until another node leads or `limit` passed; how long it took.
    async fn until_handed_over(&self, limit: Duration) -> Option<Duration> {
        let t0 = std::time::Instant::now();
        while t0.elapsed() < limit {
            if self.source.board().authority() != 0 {
                return Some(t0.elapsed());
            }
            let o = self.follower.step().await;
            assert!(!matches!(o, StepOutcome::Gap(_) | StepOutcome::Halted(_)), "{o:?}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        None
    }

    /// The cursor block's age by the local clock, in ms.
    fn age_ms(&self) -> Option<u64> {
        self.health.snapshot().cursor_time_unix_ms.map(|t| now_ms().saturating_sub(t))
    }
}

/// R5C1-M1: from an old cursor (11 minutes) the authority answers every request with nothing new, understates its sink
/// and virtual DAA score (nothing more follows) and lets every `getBlock` take 6 s (past the lead check's timeout), while
/// the honest cross-checking node reports the chain past the cursor: 200 chain blocks and more (the lead check runs), or
/// fewer than 100 (it does not). Before, an idle answer from an old cursor blamed no node unless the authority itself
/// claimed more or the lead check showed a block, so the index stood still for good. Now it is handed over within one
/// period plus the tolerance after its first answer.
#[tokio::test]
async fn an_authority_that_answers_nothing_new_from_an_old_cursor_is_handed_over() {
    let _g = serial().await;
    // (ms of chain a block: 660 chain blocks past the cursor, or 44)
    for (k, per) in [1_000u64, 15_000].into_iter().enumerate() {
        let miner = Miner::new(11 * 60_000, per);
        let (a, b) = (Node::new(&miner), Node::new(&miner));
        a.empty.store(true, Ordering::Relaxed);
        a.hides.store(true, Ordering::Relaxed);
        a.block_delay_ms.store(6_000, Ordering::Relaxed);
        let (ua, ub) = (format!("ws://10.40.{k}.1:18210"), format!("ws://10.40.{k}.2:18210"));
        let r = run(miner.first(), vec![(spec(&ua), a.clone()), (spec(&ub), b.clone())]);
        let took = r.until_handed_over(Duration::from_secs(20)).await;
        let h = r.health.snapshot();
        assert!(took.is_some(), "case {k}: never handed over; health {h:?}");
        // the first answer waited 6 s for the cursor block's header; then one period and the tolerance
        assert!(took.unwrap() < Duration::from_secs(6) + PERIOD + TOLERANCE + Duration::from_secs(2), "case {k}: {took:?}");
        assert!(h.authority_replaced_total >= 1, "case {k}: counted");
    }
}

/// R5C1-M2: the store starts (a restart, a handover) with a cursor 40 minutes old, and the authority serves the chain at
/// the rate of real time from there, so the lag stays level at 40 minutes. Before, the rule only forbade the lag to grow
/// above the least it reached since this authority leads, which a level lag never does: no handover at any lag. Now the
/// lag must fall by half a period within each period, so it is handed over after one.
#[tokio::test]
async fn an_authority_that_holds_an_old_lag_level_is_handed_over_after_one_period() {
    let _g = serial().await;
    let miner = Miner::new(40 * 60_000, 1_000);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    *a.hold_ms.lock().unwrap() = Some(40 * 60_000);
    let r = run(miner.first(), vec![(spec("ws://10.41.0.1:18210"), a.clone()), (spec("ws://10.41.0.2:18210"), b.clone())]);
    let took = r.until_handed_over(Duration::from_secs(10)).await;
    let h = r.health.snapshot();
    assert!(took.is_some(), "never handed over; health {h:?}");
    assert!(took.unwrap() < PERIOD * 4, "{took:?}");
}

/// A drip at the tip: the authority holds the cursor block about 11 % above the tolerance behind the real time (100 s
/// against 90 s in production). Before, a cursor less than 10 minutes old was fresh and every answer progress, so a
/// node could hold the index up to 10 minutes behind for good. Now the lag must come within the tolerance or fall within
/// a period, so it is handed over.
#[tokio::test]
async fn a_drip_that_holds_the_lag_above_the_tolerance_is_handed_over() {
    let _g = serial().await;
    let miner = Miner::new(2_000, 10);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    *a.hold_ms.lock().unwrap() = Some(TOLERANCE.as_millis() as u64 * 10 / 9 + 1);
    let r = run(miner.first(), vec![(spec("ws://10.42.0.1:18210"), a.clone()), (spec("ws://10.42.0.2:18210"), b.clone())]);
    let took = r.until_handed_over(Duration::from_secs(8)).await;
    assert!(took.is_some(), "never handed over; health {:?}", r.health.snapshot());
}

/// R5C1-L2: with chain proofs off (`chain_proof: None` here), the authority holds the chain 40 minutes behind and stamps
/// every header it serves with the time now. Before, the follower took the cursor's time from the last header as the
/// node sent it: every cursor fresh, every answer progress. Now a header counts only when it hashes to its block, so the
/// time is unknown, which is not keeping up: handed over after one period.
#[tokio::test]
async fn an_authority_that_stamps_its_headers_now_with_chain_proofs_off_is_handed_over() {
    let _g = serial().await;
    let miner = Miner::new(40 * 60_000, 1_000);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    *a.hold_ms.lock().unwrap() = Some(40 * 60_000);
    a.stamps_now.store(true, Ordering::Relaxed);
    let r = run(miner.first(), vec![(spec("ws://10.43.0.1:18210"), a.clone()), (spec("ws://10.43.0.2:18210"), b.clone())]);
    let took = r.until_handed_over(Duration::from_secs(10)).await;
    assert!(took.is_some(), "never handed over; health {:?}", r.health.snapshot());
}

/// R4C1-M2 stays closed: a hostile cross-checking node that claims a sink and a virtual DAA score far ahead and reports
/// made-up chain blocks past the real chain never costs an honest authority at the tip its role, over many periods. Its
/// claims do not enter the rule at all.
#[tokio::test]
async fn a_cross_checking_node_that_claims_more_chain_never_deposes_an_honest_authority() {
    let _g = serial().await;
    let miner = Miner::new(2_000, 10);
    let (a, b) = (Node::new(&miner), Node::new(&miner));
    b.claims_ahead.store(1_000_000, Ordering::Relaxed);
    // a wider tolerance here: a step of a debug build on a busy host must not look like a lag
    let (tolerance, period) = (Duration::from_millis(400), Duration::from_millis(800));
    let nodes = vec![(spec("ws://10.44.0.1:18210"), a.clone()), (spec("ws://10.44.0.2:18210"), b.clone())];
    let r = run_with(miner.first(), nodes, tolerance, period);
    let took = r.until_handed_over(period * 8).await;
    assert!(took.is_none(), "an honest authority handed over after {took:?}; health {:?}", r.health.snapshot());
    assert!(r.age_ms().is_some_and(|a| a <= tolerance.as_millis() as u64), "it keeps up: {:?}", r.age_ms());
}
