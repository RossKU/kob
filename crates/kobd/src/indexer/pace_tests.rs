//! The progress rule (module documentation of `indexer::follower`, *Progress*) on a pair of public nodes (resolver
//! entries): a network that stops making blocks fails every authority the same way. At test scale (a tolerance of
//! 400 ms, a period of 1.2 s, on the real clock, with room for a loaded runner: R6F-2), the pair keeps an authority all along, the resolutions stay
//! bounded by about one per period, and once blocks come again the cursor is fresh within a
//! period.

use super::*;
use crate::rpc::multi::{CrossCheckPolicy, MultiNode, NodeSpec};
use crate::rpc::resolver;
use crate::rpc::types::{AddressUtxo, ChainHashes, ChainIds, DagInfo, ServerInfo};
use crate::testkit::MockNode;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

fn real_ms() -> u64 {
    now_ms()
}

/// The chain at the real time, a block every `per` ms, unless halted.
struct Miner {
    node: Arc<MockNode>,
    next: AtomicU64,
    per: u64,
    halted: AtomicBool,
}

impl Miner {
    fn new(per: u64) -> Arc<Miner> {
        let node = MockNode::new("testnet-10");
        let from = real_ms() - 1_000;
        node.set_block_clock(from, per);
        let m = Arc::new(Miner { node, next: AtomicU64::new(from), per, halted: AtomicBool::new(false) });
        m.mine();
        m
    }

    fn mine(&self) {
        if self.halted.load(Ordering::Relaxed) {
            return;
        }
        let now = real_ms();
        let next = self.next.load(Ordering::Relaxed);
        if now >= next {
            let n = (now - next) / self.per + 1;
            self.node.push_empty(n as usize);
            self.next.store(next + n * self.per, Ordering::Relaxed);
        }
    }

    fn resume(&self) {
        let now = real_ms();
        self.node.set_block_clock(now, self.per);
        self.next.store(now, Ordering::Relaxed);
        self.halted.store(false, Ordering::Relaxed);
        self.mine();
    }
}

/// The client of a resolver entry: like a node client, it asks the entry for its node at every call, which resolves a
/// replacement for an entry that gave its node up.
struct Entry {
    entry: &'static str,
    miner: Arc<Miner>,
}

impl Entry {
    fn node(&self) -> Result<&MockNode, RpcError> {
        self.miner.mine();
        resolver::endpoint(self.entry).map_err(RpcError::Transport)?;
        Ok(&self.miner.node)
    }
}

impl ChainSource for Entry {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.node()?.vspc_v2(req).await
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.node()?.dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.node()?.server_info().await
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.node()?.utxos_by_addresses(a).await
    }
    async fn block_exists(&self, h: Hash32) -> Result<bool, RpcError> {
        self.node()?.block_exists(h).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.node()?.sink_blue_score().await
    }
    async fn block_blue_score(&self, h: Hash32) -> Result<Option<u64>, RpcError> {
        self.node()?.block_blue_score(h).await
    }
    async fn block_daa_score(&self, h: Hash32) -> Result<Option<u64>, RpcError> {
        self.node()?.block_daa_score(h).await
    }
    async fn chain_hashes(&self, s: Hash32) -> Result<ChainHashes, RpcError> {
        ChainSource::chain_hashes(self.node()?, s).await
    }
    async fn chain_with_ids(&self, s: Hash32, m: Option<u64>) -> Result<ChainIds, RpcError> {
        self.node()?.chain_with_ids(s, m).await
    }
}

static RESOLUTIONS: AtomicUsize = AtomicUsize::new(0);

/// Twelve public nodes; the first one not avoided, else the first.
fn resolve(_network: &str, avoid: &[String]) -> Result<resolver::Resolved, String> {
    RESOLUTIONS.fetch_add(1, Ordering::Relaxed);
    let nodes: Vec<String> = (0..12).map(|i| format!("wss://n{i}.example/x/borsh")).collect();
    let pick = nodes.iter().find(|n| !avoid.contains(&resolver::host(n))).unwrap_or(&nodes[0]);
    Ok(resolver::Resolved { url: pick.clone(), resolver: "fake".into() })
}

/// A network that stops making blocks under a pair of public nodes: every period the authority is given up and another
/// takes the role. The follower never stops and always has an authority, the resolutions
/// stay at about one per handover, and once blocks come again the cursor is fresh within a period.
#[tokio::test]
// The guard only serialises the tests that share the process-wide resolver state (a current-thread runtime).
#[allow(clippy::await_holding_lock)]
async fn a_halted_network_under_a_public_pair_costs_only_handovers() {
    let _g = resolver::tests::serial();
    resolver::tests::reset(resolve);
    let miner = Miner::new(10);
    let spec =
        |url: &str, cross_check| NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check, may_lead: true };
    let (a, b) =
        (Arc::new(Entry { entry: "resolver", miner: miner.clone() }), Arc::new(Entry { entry: "resolver:2", miner: miner.clone() }));
    let policy =
        CrossCheckPolicy { single_after: Duration::from_millis(200), rotate_after: Duration::from_millis(100), deep_daa: 600 };
    let source = Arc::new(
        MultiNode::new(vec![(spec("resolver", false), a.clone()), (spec("resolver:2", true), b.clone())], a.clone())
            .with_policy(policy)
            .with_watch(Duration::from_secs(3_600), 6),
    );
    let ingest =
        Arc::new(Mutex::new(Ingest::new(crate::indexer::db::open_memory("testnet-10").unwrap(), crate::testkit::processor(), None)));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = broadcast::channel(8);
    let mut cfg = FollowerConfig::from_indexer(&crate::config::IndexerConfig { network: "testnet-10".into(), ..Default::default() });
    cfg.start = StartMode::Hash(miner.node.chain()[1].hash);
    cfg.fetch_parallel = 1;
    cfg.chain_proof = None;
    cfg.poll_interval = Duration::from_millis(5);
    cfg.backoff = Duration::from_millis(5);
    cfg.max_backoff = Duration::from_millis(20);
    cfg.authority_lag_tolerance = Duration::from_millis(400);
    cfg.authority_stall = Duration::from_millis(1_200);
    let f = Follower::new(source.clone(), ingest, cfg, health.clone(), events);
    let step_for = |d: Duration| {
        let f = &f;
        async move {
            let t0 = std::time::Instant::now();
            let mut out = vec![];
            while t0.elapsed() < d {
                out.push(f.step().await);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            out
        }
    };
    let age = || health.snapshot().cursor_time_unix_ms.map(|t| real_ms().saturating_sub(t));
    step_for(Duration::from_millis(1_500)).await;
    assert!(age().is_some_and(|a| a <= 400), "at the tip: {:?}", age());
    let (replaced0, resolutions0) = (resolver::authority_replaced(), RESOLUTIONS.load(Ordering::Relaxed));

    // 12 s of halt: about 8 periods
    miner.halted.store(true, Ordering::Relaxed);
    let out = step_for(Duration::from_secs(12)).await;
    assert!(out.iter().all(|o| !matches!(o, StepOutcome::Gap(_) | StepOutcome::Halted(_))), "never stopped");
    let replaced = resolver::authority_replaced() - replaced0;
    let resolutions = RESOLUTIONS.load(Ordering::Relaxed) - resolutions0;
    assert!((4..=14).contains(&replaced), "an authority given up about every period: {replaced}");
    assert!(resolutions <= replaced as usize + 2, "about one resolution per handover: {resolutions} for {replaced}");
    assert!(resolver::current("resolver").is_some(), "the pair has an authority");

    // blocks again: fresh within a period
    miner.resume();
    let t0 = std::time::Instant::now();
    while age().is_none_or(|a| a > 400) && t0.elapsed() < Duration::from_secs(10) {
        f.step().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let took = t0.elapsed();
    assert!(age().is_some_and(|a| a <= 400), "fresh again: {:?}", age());
    assert!(took <= Duration::from_millis(1_200 + 400 + 3_000), "fresh {took:?} after blocks came again");
}
