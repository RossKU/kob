//! The Indexer's node failover (`rpc::failover`): the configured node first, the fallback (in production the public pair)
//! when it stops answering, and back once it is healthy. Mock nodes: one chain served by both tiers (the database must equal
//! a clean replay across both switches), a fallback whose chain diverges deep below the cursor (refused, nothing applied, the
//! cursor stays), one that diverges by a shallow reorg (applied as any reorg), and one that does not know the cursor (never
//! switched to).

mod common;

use common::*;
use kob_protocol::state::*;
use kobd::config::StartMode;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kobd::indexer::ingest::Ingest;
use kobd::indexer::status::{FollowerState, HealthState};
use kobd::rpc::failover::{Failover, FailoverPolicy};
use kobd::rpc::types::*;
use kobd::rpc::{ChainSource, RpcError};
use kobd::testkit::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A node that can go away (every call fails in transport, as a closed port does).
struct Tier {
    node: Arc<MockNode>,
    down: AtomicBool,
    calls: AtomicUsize,
}

impl Tier {
    fn new(node: &Arc<MockNode>) -> Arc<Tier> {
        Arc::new(Tier { node: node.clone(), down: AtomicBool::new(false), calls: AtomicUsize::new(0) })
    }

    fn gate(&self) -> Result<(), RpcError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.down.load(Ordering::Relaxed) {
            return Err(RpcError::Transport("connection refused".into()));
        }
        Ok(())
    }
}

impl ChainSource for Tier {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.gate()?;
        self.node.vspc_v2(req).await
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.gate()?;
        self.node.dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.gate()?;
        self.node.server_info().await
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.gate()?;
        self.node.utxos_by_addresses(a).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.gate()?;
        self.node.block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.gate()?;
        self.node.sink_blue_score().await
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.gate()?;
        self.node.block_blue_score(hash).await
    }
    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.gate()?;
        self.node.block_daa_score(hash).await
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.gate()?;
        ChainSource::chain_hashes(&*self.node, start).await
    }
    async fn chain_with_ids(&self, start: Hash32, m: Option<u64>) -> Result<ChainIds, RpcError> {
        self.gate()?;
        self.node.chain_with_ids(start, m).await
    }
}

fn policy() -> FailoverPolicy {
    FailoverPolicy {
        after_failures: 3,
        after: Duration::ZERO,
        probe_every: Duration::from_millis(10),
        return_after_probes: 2,
        max_reorg_blocks: 10,
        max_behind_blue: 600,
        check_timeout: Duration::from_secs(2),
    }
}

struct Run {
    source: Arc<Failover<Tier>>,
    follower: Follower<Failover<Tier>>,
    ingest: Arc<Mutex<Ingest>>,
    health: Arc<HealthState>,
}

fn run(anchor: Hash32, primary: &Arc<Tier>, fallback: &Arc<Tier>, fetch_parallel: usize) -> Run {
    let source = Arc::new(Failover::new(primary.clone(), fallback.clone(), "ws://10.0.0.5:18210", "resolver", "testnet-10", policy()));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(anchor),
        poll_interval: Duration::from_millis(1),
        backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
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
    };
    let follower = Follower::new(source.clone(), ingest.clone(), cfg, health.clone(), events);
    Run { source, follower, ingest, health }
}

impl Run {
    /// Steps (the background checks run between them) until `done`, or panics after `max` steps.
    async fn until(&self, max: usize, what: &str, done: impl Fn(&Run) -> bool) {
        for _ in 0..max {
            if done(self) {
                return;
            }
            let o = self.follower.step().await;
            assert!(!matches!(o, StepOutcome::Gap(_) | StepOutcome::Halted(_)), "{o:?}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("{what}: not reached in {max} steps; health {:?}", self.health.snapshot());
    }

    fn cursor(&self) -> Option<Hash32> {
        self.health.snapshot().cursor_hash
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

/// The configured node goes away: after three failed calls the fallback (the same chain) is the chain source and the store
/// follows it to the tip; the configured node comes back: after two healthy probes it is the chain source again. Every
/// switch is in the health; the database equals a clean replay of the chain.
#[tokio::test]
async fn the_fallback_takes_over_when_the_configured_node_dies_and_hands_back_when_it_returns() {
    for fetch_parallel in [1, 3] {
        let c = Ctx::new();
        grow(&c, 40, 0);
        let node = c.hs.node.clone();
        let (primary, fallback) = (Tier::new(&node), Tier::new(&node));
        let r = run(node.anchor(), &primary, &fallback, fetch_parallel);
        r.until(2_000, "first sync", |r| r.cursor() == Some(node.tip())).await;
        assert_eq!(r.source.active(), 0);
        assert_eq!(fallback.calls.load(Ordering::Relaxed), 0, "the fallback is not used while the configured node answers");

        primary.down.store(true, Ordering::Relaxed);
        grow(&c, 40, 100_000);
        r.until(2_000, "following on the fallback", |r| r.cursor() == Some(node.tip())).await;
        let h = r.health.snapshot();
        let src = h.node_source.clone().expect("the failover reports itself");
        assert_eq!((src.active.as_str(), src.switches, src.refused), ("fallback", 1, 0), "{src:?}");
        assert!(src.last_switch_reason.as_deref().unwrap().contains("stopped answering"), "{src:?}");
        assert_eq!(src.primary, "ws://10.0.0.5:18210");
        assert!(h.status_line().contains("node source: fallback (1 switches)"), "{}", h.status_line());
        assert!(h.metrics_text().contains("kob_indexer_node_failover_active 1"));

        primary.down.store(false, Ordering::Relaxed);
        grow(&c, 20, 200_000);
        r.until(2_000, "back on the configured node", |r| r.source.active() == 0 && r.cursor() == Some(node.tip())).await;
        // its first answer from the cursor settles it
        grow(&c, 2, 300_000);
        r.until(500, "settled on the configured node", |r| !r.source.status().settling && r.cursor() == Some(node.tip())).await;
        let src = r.source.status();
        assert_eq!((src.active.as_str(), src.switches, src.refused, src.settling), ("primary", 2, 0, false), "{src:?}");
        assert!(src.last_switch_reason.as_deref().unwrap().contains("healthy again"), "{src:?}");
        let h = r.health.snapshot();
        assert_eq!(h.state, FollowerState::Following);
        assert_eq!(snapshot(r.ingest.lock().unwrap().conn()), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
    }
}

/// The fallback's chain diverges from the configured node's below the cursor. Deeper than `max_reorg_blocks`: its answer is
/// refused, nothing is applied and the cursor stays where the configured node left it (the indexer waits, visibly). Within
/// it: applied as an ordinary reorg and followed.
#[tokio::test]
async fn a_fallback_whose_chain_diverges_deep_is_refused_and_a_shallow_reorg_is_followed() {
    for (depth, refused) in [(25usize, true), (3, false)] {
        // two nodes with the same 40 chain blocks (the mock's blocks are deterministic), then the second reorganises
        let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
        a.push_empty(40);
        b.push_empty(40);
        assert_eq!(a.tip(), b.tip());
        let (primary, fallback) = (Tier::new(&a), Tier::new(&b));
        let r = run(a.anchor(), &primary, &fallback, 1);
        r.until(500, "first sync", |r| r.cursor() == Some(a.tip())).await;
        b.reorg(depth, vec![vec![]; depth + 5]);
        primary.down.store(true, Ordering::Relaxed);
        if refused {
            r.until(500, "the refusal", |r| r.source.status().refused >= 2).await;
            let src = r.source.status();
            assert_eq!((src.active.as_str(), src.settling), ("fallback", true), "{src:?}");
            let why = src.last_refusal.unwrap();
            assert!(why.contains(&format!("removes {depth} chain blocks")), "{why}");
            let h = r.health.snapshot();
            assert_eq!(h.cursor_hash, Some(a.tip()), "nothing of the divergent chain applied");
            assert_eq!((h.reorgs_total, h.reverted_blocks_total), (0, 0));
            assert_eq!(h.state, FollowerState::NodeUnavailable);
            assert!(h.last_error.unwrap().contains("node failover"));
        } else {
            r.until(500, "following the fallback", |r| r.cursor() == Some(b.tip())).await;
            let src = r.source.status();
            assert_eq!((src.active.as_str(), src.refused, src.settling), ("fallback", 0, false), "{src:?}");
            assert!(r.health.snapshot().reorgs_total >= 1, "applied as a reorg");
        }
    }
}

/// A fallback that does not know the cursor (another chain, or far behind) is not switched to: the indexer stays on the
/// configured node and waits, as it would without a failover.
#[tokio::test]
async fn a_fallback_that_does_not_know_the_cursor_is_never_switched_to() {
    let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    a.push_empty(30);
    b.push_empty(20);
    let (primary, fallback) = (Tier::new(&a), Tier::new(&b));
    let r = run(a.anchor(), &primary, &fallback, 1);
    r.until(500, "first sync", |r| r.cursor() == Some(a.tip())).await;
    primary.down.store(true, Ordering::Relaxed);
    for _ in 0..60 {
        let _ = r.follower.step().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let src = r.source.status();
    assert_eq!((src.active.as_str(), src.switches), ("primary", 0), "{src:?}");
    assert!(fallback.calls.load(Ordering::Relaxed) > 0, "the fallback was checked");
    assert_eq!(r.cursor(), Some(a.tip()));
}
