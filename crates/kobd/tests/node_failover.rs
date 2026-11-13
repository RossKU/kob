//! The Indexer's node failover (`rpc::failover`): the configured node first, the fallback (in production the public pair)
//! when it stops answering, and back once it is healthy. Mock nodes: one chain served by both tiers (the database must equal
//! a clean replay across both switches), a fallback whose chain diverges deep below the cursor (refused, nothing applied, the
//! cursor stays), one that diverges by a shallow reorg (applied as any reorg), and one that does not know the cursor (never
//! switched to). And the pair's health watch (`rpc::multi`, `rpc::judge`): both nodes of a pair are judged the same way, a
//! named node that is behind or on another network raises an alarm and a cross-checking one is cut off until it recovers; a
//! public node (a resolver entry) that is behind or on another network is replaced, the authority as well as the
//! cross-checking one.

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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A node that can go away (every call fails in transport, as a closed port does), or over-report its DAA (`boost`, with its
/// real sink or, `fake_sink`, one no other node ever learns).
struct Tier {
    node: Arc<MockNode>,
    down: AtomicBool,
    calls: AtomicUsize,
    boost: AtomicU64,
    fake_sink: AtomicBool,
    /// Added to its sink blue score (`getSinkBlueScore`).
    sink_boost: AtomicU64,
}

impl Tier {
    fn new(node: &Arc<MockNode>) -> Arc<Tier> {
        Arc::new(Tier {
            node: node.clone(),
            down: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            boost: AtomicU64::new(0),
            fake_sink: AtomicBool::new(false),
            sink_boost: AtomicU64::new(0),
        })
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
        let mut d = self.node.dag_info().await?;
        d.virtual_daa_score += self.boost.load(Ordering::Relaxed);
        if self.fake_sink.load(Ordering::Relaxed) {
            d.sink = Hash32([0xee; 32]);
        }
        Ok(d)
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.gate()?;
        let mut i = self.node.server_info().await?;
        i.virtual_daa_score += self.boost.load(Ordering::Relaxed);
        Ok(i)
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
        Ok(self.node.sink_blue_score().await? + self.sink_boost.load(Ordering::Relaxed))
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
        authority_failures: kobd::indexer::follower::AUTHORITY_FAILURES,
        authority_stall: kobd::indexer::follower::AUTHORITY_STALL,
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

/// The resolver state and the judge are process-wide: the pair tests run one at a time.
async fn pair_serial() -> tokio::sync::MutexGuard<'static, ()> {
    static M: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    M.lock().await
}

fn pair<N: ChainSource>(a: (&str, &Arc<N>), b: (&str, &Arc<N>)) -> kobd::rpc::multi::MultiNode<N> {
    use kobd::rpc::multi::{MultiNode, NodeSpec};
    let spec =
        |url: &str, cross_check| NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check, may_lead: true };
    MultiNode::new(vec![(spec(a.0, false), a.1.clone()), (spec(b.0, true), b.1.clone())], a.1.clone()).with_watch(Duration::ZERO, 2)
}

/// A pair of named nodes: the cross-checking one falls behind, an honest lag (its sink an ancestor of the other node's: an alarm
/// once it stays behind and has placed the other node's sink in its own DAG, that far ahead, the
/// node cut off from the pair and shown in the health), then catches up (the alarm ends, it is back). One on another network:
/// an alarm at once. The authority is judged the same way, and stays the authority. The checks run on the test's task only (a
/// follower request would start one on a task of its own too).
#[tokio::test]
async fn a_named_node_that_is_behind_or_on_another_network_raises_an_alarm_and_is_cut_off_until_it_recovers() {
    let _g = pair_serial().await;
    kobd::rpc::resolver::set_network("testnet-10");
    // a lag alarm ends at the first fine check here (after a minute of them in production)
    kobd::rpc::judge::set_alarm_clear_after(Duration::ZERO);
    let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    a.push_empty(800);
    b.push_empty(100);
    assert!(a.tip_daa() > b.tip_daa() + kobd::rpc::multi::LAG_DAA);
    let (ua, ub) = ("ws://10.0.0.1:18210", "ws://10.0.0.2:18210");
    let m = pair((ua, &a), (ub, &b));
    m.check_health().await;
    assert!(kobd::rpc::judge::alarm_of(ub).is_none(), "behind once is no finding yet");
    m.check_health().await;
    assert!(kobd::rpc::judge::alarm_of(ub).is_none(), "behind by the answers only: the node has not confirmed the lead yet");
    // it learns the other node's sink of those checks (still far behind): the lead is confirmed
    b.push_empty(700);
    a.push_empty(800);
    m.check_health().await;
    let why = kobd::rpc::judge::alarm_of(ub).expect("an alarm once it stays behind");
    assert!(why.contains("DAA behind the other node"), "{why}");
    let st = m.board().snapshot();
    assert_eq!((st[1].state.as_str(), st[1].cut_reason.as_deref()), ("cut", Some(why.as_str())), "{st:?}");
    assert_eq!(st[0].state, "ok");
    let mut h = kobd::indexer::status::HealthSnapshot::new("testnet-10");
    (h.nodes, h.node_alarms) = (st, kobd::rpc::judge::alarms());
    assert!(h.status_line().contains("| NODE ALARM: ws://10.0.0.2:18210: it stays"), "{}", h.status_line());
    assert!(h.metrics_text().contains("kob_indexer_node_alarms 1"));
    assert!(serde_json::to_value(&h).unwrap()["nodes"][1]["cut_reason"].as_str().unwrap().contains("behind"));
    // it catches up: the alarm ends and the node is back in the pair
    b.push_empty(800);
    m.check_health().await;
    assert_eq!(kobd::rpc::judge::alarm_of(ub), None);
    assert_eq!(m.board().snapshot()[1].state, "ok");

    // a cross-checking node on another network: an alarm at the first check; the authority behind: an alarm, still the authority
    let c = MockNode::new("testnet-11");
    c.push_empty(900);
    let (uc, ud) = ("ws://10.0.0.3:18210", "ws://10.0.0.4:18210");
    let d = MockNode::new("testnet-10");
    d.push_empty(10);
    let m = pair((ud, &d), (uc, &c));
    m.check_health().await;
    assert!(kobd::rpc::judge::alarm_of(uc).unwrap().contains("it is on testnet-11, not testnet-10"));
    assert_eq!(m.board().snapshot()[1].state, "cut");
    // the authority is compared with the other answers only (the node on another network gave none): judged on its own here
    let e = MockNode::new("testnet-10");
    e.push_empty(900);
    let m = pair((ud, &d), ("ws://10.0.0.5:18210", &e));
    m.check_health().await;
    m.check_health().await;
    d.push_empty(890);
    e.push_empty(900);
    m.check_health().await;
    assert!(kobd::rpc::judge::alarm_of(ud).unwrap().contains("behind"), "the authority is watched the same way");
    assert_eq!(m.board().snapshot()[0].role, "primary");
    kobd::rpc::judge::set_alarm_clear_after(kobd::rpc::judge::ALARM_CLEAR_AFTER);
}

/// A local Kaspa resolver that names `wss://n<k>.example/...` nodes in turn (no node is ever connected to: the pair's nodes
/// are mock nodes; the resolver entries only decide which public node each entry stands for).
fn local_resolver() -> String {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut k = 0u32;
        while let Ok((mut c, _)) = l.accept() {
            let mut b = [0u8; 1024];
            let _ = c.read(&mut b);
            k += 1;
            let body = format!(r#"{{"uid":"u","url":"wss://n{k}.example/kaspa/testnet-10/wrpc/borsh"}}"#);
            let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A public pair (resolver entries): the cross-checking node on another network is given up at once for another public node,
/// and the health says why; the authority that stays behind is given up too, the cross-checking node is the authority at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_public_node_that_is_behind_or_on_another_network_is_replaced_whichever_role_it_has() {
    use kobd::rpc::resolver;
    let _g = pair_serial().await;
    std::env::set_var("KOB_RESOLVERS", local_resolver());
    // the other pair tests resolved too: this one is not held back by their resolutions (`resolver::CHURN_WINDOW`)
    resolver::forget_churn();
    resolver::set_network("testnet-10");
    let first = resolver::endpoint("resolver").unwrap();
    let second = resolver::endpoint("resolver:2").unwrap();
    assert_ne!(resolver::host(&first), resolver::host(&second));
    let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-11"));
    a.push_empty(20);
    b.push_empty(20);
    let m = pair(("resolver", &a), ("resolver:2", &b));
    // One health check on the test's task, no follower request: a request also starts the watch on a task of its own, and a
    // mock answers the same whichever host its entry stands for, so a second check that ran after the replacement was
    // resolved judged the mock's answer as the new host's and replaced that one too (CI, 2026-11-11). A real client always
    // asks the entry's current node, and a report about a node the entry no longer uses changes nothing (`rpc::judge::act`).
    m.check_health().await;
    let st = m.board().snapshot();
    assert_eq!(st[1].replaced, 1, "{st:?}");
    let why = st[1].cut_reason.clone().unwrap();
    assert!(why.starts_with(&resolver::host(&second)) && why.contains("it is on testnet-11"), "{why}");
    assert_eq!(resolver::current("resolver"), Some(first.clone()), "the authority keeps its role");
    // the replacement is resolved in the background, on another host
    let t0 = std::time::Instant::now();
    let third = loop {
        if let Some(n) = resolver::current("resolver:2") {
            break n;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "no replacement resolved");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(![resolver::host(&first), resolver::host(&second)].contains(&resolver::host(&third)));

    // the authority stays behind the (new) cross-checking node: given up, the cross-checking node is the authority at once
    let c = MockNode::new("testnet-10");
    c.push_empty(900);
    let m = pair(("resolver", &a), ("resolver:2", &c));
    // two checks in a row (the watch's bound here), on the test's task only (as above: no check the test does not see)
    m.check_health().await;
    m.check_health().await;
    assert_eq!(resolver::current("resolver"), Some(first.clone()), "behind by the answers only: not confirmed yet");
    // the authority learns the other node's sink of those checks, still far behind: the lead is confirmed
    a.push_empty(880);
    c.push_empty(900);
    m.check_health().await;
    assert_eq!(resolver::current("resolver"), Some(third.clone()), "the healthy node is the authority now");
    let st = m.board().snapshot();
    assert!(st[0].cut_reason.as_deref().unwrap().contains("DAA behind the other node"), "{st:?}");
    assert!(!kobd::rpc::judge::alarms().iter().any(|a| a.contains(".example")), "public nodes are replaced, they raise no alarm");
}

/// The resolver entries for a pair test: a local resolver, the network, no rate limit left over from another test, and the two
/// nodes the entries stand for now.
fn public_pair() -> (String, String) {
    use kobd::rpc::resolver;
    std::env::set_var("KOB_RESOLVERS", local_resolver());
    resolver::set_network("testnet-10");
    resolver::forget_churn();
    (resolver::endpoint("resolver").unwrap(), resolver::endpoint("resolver:2").unwrap())
}

/// A node that over-reports its DAA with a sink no node ever learns (a lie, or a bug) never puts its honest partner behind,
/// whichever role either has: the lead is refuted after `LAG_CONFIRM_CHECKS` checks, nothing is replaced, nothing is resolved,
/// and the honest node is never suspect (it is behind by the other node's claim only).
#[tokio::test(flavor = "multi_thread")]
async fn a_node_that_over_reports_its_daa_never_evicts_its_honest_partner() {
    use kobd::rpc::resolver;
    let _g = pair_serial().await;
    let (first, second) = public_pair();
    let replaced = (resolver::replacements("resolver"), resolver::replacements("resolver:2"));
    let (honest, lying) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    honest.push_empty(50);
    lying.push_empty(50);
    let (h, l) = (Tier::new(&honest), Tier::new(&lying));
    l.boost.store(5_000, Ordering::Relaxed);
    l.fake_sink.store(true, Ordering::Relaxed);
    for (authority, cross, honest_node) in [(&h, &l, &first), (&l, &h, &second)] {
        let m = pair(("resolver", authority), ("resolver:2", cross));
        for _ in 0..kobd::rpc::multi::LAG_CONFIRM_CHECKS + 5 {
            m.check_health().await;
            assert!(!kobd::rpc::judge::suspect(honest_node), "behind by the other node's claim only: not suspect");
        }
        assert_eq!((resolver::current("resolver"), resolver::current("resolver:2")), (Some(first.clone()), Some(second.clone())));
        assert_eq!((resolver::replacements("resolver"), resolver::replacements("resolver:2")), replaced);
    }
}

/// Audit round 3 R3-M-2: an authority that over-reports its DAA over its real sink is held to account. The DAG denies the
/// lead: its sink, a block the honest node places in its own DAG too, is far below the virtual DAA score it reports, as both of
/// them report that block. Two checks in a row are a contradiction, it is given up, and the honest node is the authority at
/// once (never suspect meanwhile).
#[tokio::test(flavor = "multi_thread")]
async fn an_authority_whose_lead_the_dag_denies_is_replaced() {
    use kobd::rpc::resolver;
    let _g = pair_serial().await;
    let (_first, second) = public_pair();
    let (honest, lying) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    honest.push_empty(50);
    lying.push_empty(50);
    let (h, l) = (Tier::new(&honest), Tier::new(&lying));
    l.boost.store(5_000, Ordering::Relaxed);
    let m = pair(("resolver", &l), ("resolver:2", &h));
    let base = resolver::replacements("resolver");
    m.check_health().await;
    assert_eq!(resolver::replacements("resolver"), base, "one check is not enough");
    assert!(!kobd::rpc::judge::suspect(&second));
    m.check_health().await;
    assert_eq!(resolver::current("resolver"), Some(second.clone()), "the honest node is the authority now");
    let why = resolver::last_reason("resolver").unwrap();
    assert!(why.contains("a lead it does not have"), "{why}");
}

/// A local monotonic clock the test moves: `Instant` at the start plus the seconds in `offset` (no waiting).
fn test_clock() -> (Arc<AtomicU64>, impl Fn() -> std::time::Instant + Send + Sync + 'static) {
    let (t0, offset) = (std::time::Instant::now(), Arc::new(AtomicU64::new(0)));
    let o = offset.clone();
    (offset, move || t0 + Duration::from_secs(o.load(Ordering::Relaxed)))
}

/// Audit round 3 R3-M-2 (the founder: "nodes must be compared by the tip structure", "block time is a lie"): an authority whose
/// sink stands still while it still reports synced never confirms a lead (it cannot learn the other node's blocks). Its sink
/// has not changed for two minutes of the local monotonic clock while the other node's advances: it stopped advancing, and it
/// is given up like a node that lags. No block timestamp is read. Both standing still (the network stalls) is nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_frozen_authority_whose_sink_stands_still_is_given_up() {
    use kobd::rpc::resolver;
    let _g = pair_serial().await;
    let (_first, second) = public_pair();
    let (frozen, current) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    frozen.push_empty(100);
    current.push_empty(400);
    let (offset, clock) = test_clock();
    let m = pair(("resolver", &frozen), ("resolver:2", &current)).with_clock(clock);
    let base = resolver::replacements("resolver");
    m.check_health().await;
    // a minute later: the current node advanced, the frozen one did not, not for long enough yet
    offset.store(60, Ordering::Relaxed);
    current.push_empty(10);
    m.check_health().await;
    assert_eq!(resolver::replacements("resolver"), base, "a minute is not enough");
    offset.store(125, Ordering::Relaxed);
    current.push_empty(10);
    m.check_health().await;
    assert_eq!(resolver::current("resolver"), Some(second.clone()), "the advancing node is the authority now");
    let why = resolver::last_reason("resolver").unwrap();
    assert!(why.contains("stopped advancing") && why.contains("has not changed"), "{why}");
    // both standing still (the network stalls, or the test's chain does): nothing
    let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    a.push_empty(500);
    b.push_empty(500);
    let (offset, clock) = test_clock();
    let before = resolver::replacements("resolver") + resolver::replacements("resolver:2");
    let m = pair(("resolver", &a), ("resolver:2", &b)).with_clock(clock);
    for s in [0, 200, 400] {
        offset.store(s, Ordering::Relaxed);
        m.check_health().await;
    }
    assert_eq!(resolver::replacements("resolver") + resolver::replacements("resolver:2"), before);
}

/// Audit round 4 R4-L-3: a named cross-checking node whose sink stands still keeps its "stopped advancing" alarm for as long as
/// it stays frozen, through the lag streak's refutation (its claims are never learned) that used to end it, and the alarm ends
/// once its sink moves again (the alarm ends at the first fine check here, after a minute of them in production).
#[tokio::test]
async fn a_named_frozen_node_keeps_its_alarm_while_it_stays_frozen() {
    let _g = pair_serial().await;
    kobd::rpc::resolver::set_network("testnet-10");
    kobd::rpc::judge::set_alarm_clear_after(Duration::ZERO);
    let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    a.push_empty(400);
    b.push_empty(400);
    let (ua, ub) = ("ws://10.0.1.1:18210", "ws://10.0.1.2:18210");
    let (offset, clock) = test_clock();
    let m = pair((ua, &a), (ub, &b)).with_clock(clock);
    let mut pushed = 400usize;
    m.check_health().await;
    let mut t = 0u64;
    let mut raised_at = None;
    for _ in 0..(kobd::rpc::multi::LAG_CONFIRM_CHECKS as usize + 40) {
        t += 10;
        offset.store(t, Ordering::Relaxed);
        a.push_empty(10);
        pushed += 10;
        m.check_health().await;
        match (kobd::rpc::judge::alarm_of(ub), raised_at) {
            (Some(why), None) => {
                assert!(why.contains("stopped advancing"), "{why}");
                raised_at = Some(t);
            }
            (Some(why), Some(_)) => assert!(why.contains("stopped advancing"), "at {t} s: {why}"),
            (None, Some(r)) => panic!("the alarm raised at {r} s ended at {t} s while the node is still frozen"),
            (None, None) => assert!(t < 130, "no alarm by {t} s"),
        }
        if raised_at.is_some() {
            assert_eq!(m.board().snapshot()[1].state, "cut", "at {t} s");
        }
    }
    assert!(raised_at.is_some_and(|r| r <= 130));
    // it catches up (the same chain as the other node): its sink moved, the alarm ends and it is back in the pair
    b.push_empty(pushed - 400);
    t += 10;
    offset.store(t, Ordering::Relaxed);
    m.check_health().await;
    assert_eq!(kobd::rpc::judge::alarm_of(ub), None);
    assert_eq!(m.board().snapshot()[1].state, "ok");
    kobd::rpc::judge::set_alarm_clear_after(kobd::rpc::judge::ALARM_CLEAR_AFTER);
}

/// Audit round 4 R4-L-4: a node seen for the first time (here one that answers again after a while) has no vote on whether the
/// chain advances until its sink has been seen to change: during a network stall (neither sink moves) the node that stood
/// still longer is not judged frozen next to it. Once the newcomer's sink moves and the other's does not, it is.
#[tokio::test]
async fn a_node_seen_for_the_first_time_does_not_count_as_advancing() {
    let _g = pair_serial().await;
    kobd::rpc::resolver::set_network("testnet-10");
    let (a, b) = (MockNode::new("testnet-10"), MockNode::new("testnet-10"));
    a.push_empty(300);
    b.push_empty(300);
    let (ta, tb) = (Tier::new(&a), Tier::new(&b));
    tb.down.store(true, Ordering::Relaxed);
    let (ua, ub) = ("ws://10.0.2.1:18210", "ws://10.0.2.2:18210");
    let (offset, clock) = test_clock();
    let m = pair((ua, &ta), (ub, &tb)).with_clock(clock);
    // the stall: neither chain moves; the second node answers again at 200 s
    for s in [0u64, 100, 200, 250, 300, 350, 400] {
        offset.store(s, Ordering::Relaxed);
        if s == 200 {
            tb.down.store(false, Ordering::Relaxed);
        }
        m.check_health().await;
        assert_eq!(kobd::rpc::judge::alarm_of(ua), None, "at {s} s: a stall is nothing");
        assert_eq!(kobd::rpc::judge::alarm_of(ub), None, "at {s} s");
    }
    // the newcomer advances, the first node does not: two minutes later it is frozen
    for s in [410u64, 470, 530] {
        offset.store(s, Ordering::Relaxed);
        b.push_empty(5);
        m.check_health().await;
    }
    assert!(kobd::rpc::judge::alarm_of(ua).is_some_and(|w| w.contains("stopped advancing")), "{:?}", kobd::rpc::judge::alarm_of(ua));
    kobd::rpc::judge::set_alarm_clear_after(Duration::ZERO);
    a.push_empty(15);
    offset.store(540, Ordering::Relaxed);
    m.check_health().await;
    kobd::rpc::judge::set_alarm_clear_after(kobd::rpc::judge::ALARM_CLEAR_AFTER);
    assert_eq!(kobd::rpc::judge::alarm_of(ua), None, "it moved again");
}

/// A node of the fallback pair that over-reports its sink blue score cannot keep the indexer off its primary. The probe
/// compares the primary's sink with the lowest one the pair vouches for (`ChainSource::vouched_sink_blue_score`), not
/// with the pair's authority alone, whose claim would make the primary look behind forever.
#[tokio::test]
async fn a_fallback_node_over_reporting_its_sink_does_not_keep_the_primary_away() {
    use kobd::rpc::multi::{MultiNode, NodeSpec};
    let _g = pair_serial().await;
    let c = Ctx::new();
    grow(&c, 40, 0);
    let node = c.hs.node.clone();
    let (own, fa, fb) = (Tier::new(&node), Tier::new(&node), Tier::new(&node));
    let spec =
        |url: &str, cross_check| NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check, may_lead: true };
    let primary = Arc::new(MultiNode::new(vec![(spec("ws://10.8.0.1:18210", false), own.clone())], own.clone()));
    let fallback = Arc::new(MultiNode::new(
        vec![(spec("ws://10.8.0.2:18210", false), fa.clone()), (spec("ws://10.8.0.3:18210", true), fb.clone())],
        fa.clone(),
    ));
    let source = Arc::new(Failover::new(primary, fallback, "ws://10.8.0.1:18210", "resolver", "testnet-10", policy()));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(node.anchor()),
        poll_interval: Duration::from_millis(1),
        backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        min_confirmations: None,
        max_walkback_blocks: 1_000,
        status_refresh: Duration::from_millis(0),
        caught_up_daa: 100,
        batch_initial_blue: 8,
        batch_target: Duration::from_secs(30),
        fetch_parallel: 1,
        prefetch_max_bytes: 256 << 20,
        prefetch_min_lag_blue: 0,
        prefetch_initial_blocks: 6,
        authority_failures: kobd::indexer::follower::AUTHORITY_FAILURES,
        authority_stall: kobd::indexer::follower::AUTHORITY_STALL,
    };
    let follower = Follower::new(source.clone(), ingest.clone(), cfg, health.clone(), events);
    step_until(&follower, "first sync", || health.snapshot().cursor_hash == Some(node.tip())).await;
    own.down.store(true, Ordering::Relaxed);
    grow(&c, 10, 100_000);
    step_until(&follower, "on the fallback", || source.active() == 1 && health.snapshot().cursor_hash == Some(node.tip())).await;
    // the pair's authority claims a sink far ahead of everyone's; the configured node comes back
    fa.sink_boost.store(10_000, Ordering::Relaxed);
    own.down.store(false, Ordering::Relaxed);
    step_until(&follower, "back on the configured node", || source.active() == 0).await;
    let src = source.status();
    assert!(src.last_switch_reason.as_deref().unwrap().contains("healthy again"), "{src:?}");
}

/// Steps `follower` (the background checks run between the steps) until `done`, or panics after 2,000 steps.
async fn step_until<S: ChainSource>(follower: &Follower<S>, what: &str, done: impl Fn() -> bool) {
    for _ in 0..2_000 {
        if done() {
            return;
        }
        let o = follower.step().await;
        assert!(!matches!(o, StepOutcome::Gap(_) | StepOutcome::Halted(_)), "{o:?}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("{what}: not reached in 2000 steps");
}
