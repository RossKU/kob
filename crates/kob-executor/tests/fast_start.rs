//! Fast start (`kob_executor::faststart`): the first start of `run` trades on a store that follows the chain from the node's
//! sink while the normal store indexes the history, then hands over to the normal one.
//!
//! The mock node serves both followers; the normal store's node client is gated (it applies nothing until the test opens
//! the gate), so "the matcher plans on the tip store" and "the matcher plans on the full store" are told apart by which orders
//! they can see: the book created before the start is in the full store only.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kob_executor::config::{IndexerConfig, StartMode};
use kob_executor::faststart::{self, Plan};
use kob_executor::hex::Hash32;
use kob_executor::matcher::node::{AddressUtxo, DagInfo, NodeApi, RpcError, ServerInfo};
use kob_executor::matcher::run::{Roles, RunConfig};
use kob_executor::matcher::wallet::{p2pk_spk_string, LocalKeys};
use kob_executor::rpc::types::{
    AddressUtxo as ChainUtxo, ChainHashes, ChainIds, DagInfo as ChainDag, RawVspcResponse, ServerInfo as ChainInfo, VspcRequest,
};
use kob_executor::rpc::{ChainSource, RpcError as ChainError};
use kob_executor::testkit::*;
use kob_protocol::state::*;
use serde_json::Value;

/// The operator's node: the mock chain, funding coins and a log of what was submitted (nothing is ever accepted: a submitted
/// transaction stays pending).
#[derive(Clone)]
struct ExecNode {
    chain: Arc<MockNode>,
    submits: Arc<Mutex<Vec<Value>>>,
}

impl ExecNode {
    fn new(chain: Arc<MockNode>) -> Self {
        ExecNode { chain, submits: Arc::default() }
    }

    fn submitted(&self) -> usize {
        self.submits.lock().unwrap().len()
    }

    fn distinct(&self) -> usize {
        let all = self.submits.lock().unwrap();
        let mut seen: Vec<String> = all.iter().map(|v| v.to_string()).collect();
        seen.sort();
        seen.dedup();
        seen.len()
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
        // a few coins: the first fill spends one, the second one is not chained onto it
        Ok((1..=3u64)
            .map(|n| AddressUtxo {
                transaction_id: h("operator-coin", n).0,
                index: 0,
                amount: 100 * KAS,
                script_public_key: p2pk_spk_string(&pk(MATCHER)),
                block_daa_score: 10,
                covenant_id: None,
            })
            .collect())
    }
}

/// The mock node as the normal store's client: a window is not served until the gate opens.
struct Gated {
    inner: Arc<MockNode>,
    open: tokio::sync::watch::Receiver<bool>,
}

impl ChainSource for Gated {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, ChainError> {
        // a closed gate fails the request after a moment (a node that does not answer), so a follower can be stopped
        let mut open = self.open.clone();
        let opened = tokio::time::timeout(Duration::from_millis(200), open.wait_for(|o| *o)).await.is_ok();
        if !opened {
            return Err(ChainError::Transport("gated".into()));
        }
        self.inner.vspc_v2(req).await
    }
    async fn dag_info(&self) -> Result<ChainDag, ChainError> {
        self.inner.dag_info().await
    }
    async fn server_info(&self) -> Result<ChainInfo, ChainError> {
        self.inner.server_info().await
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<ChainUtxo>, ChainError> {
        self.inner.utxos_by_addresses(a).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, ChainError> {
        self.inner.block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, ChainError> {
        self.inner.sink_blue_score().await
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, ChainError> {
        self.inner.block_blue_score(hash).await
    }
    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, ChainError> {
        self.inner.block_daa_score(hash).await
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, ChainError> {
        ChainSource::chain_hashes(&*self.inner, start).await
    }
    async fn chain_with_ids(&self, start: Hash32, m: Option<u64>) -> Result<ChainIds, ChainError> {
        self.inner.chain_with_ids(start, m).await
    }
}

fn signer() -> LocalKeys {
    LocalKeys { operator: pk(MATCHER), keys: keys() }
}

/// A resting ask and a crossing bid, in two blocks, aged by 20 empty ones.
fn crossing_pair(w: &World, node: &MockNode) {
    let create_a = w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    let b = bid(MAKER_B, P260);
    let create_b = w.create_tx(AnyState::KobBid(b.clone()), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    w.include(node, &[&create_a]);
    w.include(node, &[&create_b]);
    node.push_empty(20);
}

fn free_addr() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}

fn config(dir: &Path, node: &MockNode, api: Option<SocketAddr>) -> IndexerConfig {
    let tokens = dir.join("tokens.json");
    std::fs::write(
        &tokens,
        format!(r#"[{{"ticker":"TST","family":"kcc20","covenant_id":"{}"}}]"#, kob_executor::hex::encode(&TOKEN_COV)),
    )
    .unwrap();
    let mut cfg = IndexerConfig {
        network: "testnet-10".into(),
        data_dir: dir.join("data"),
        tokens_path: Some(tokens),
        poll_interval_ms: 5,
        backoff_ms: 20,
        max_backoff_ms: 50,
        fast_start: true,
        rules: kob_executor::tokens::ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..Default::default() },
        ..IndexerConfig::default()
    };
    // the mock node's pruning point is its first block: the normal store starts there, the tip store at the sink
    let _ = node;
    cfg.start = StartMode::PruningPoint;
    match api {
        Some(a) => cfg.api.listen = a,
        None => cfg.api.enabled = false,
    }
    cfg.api.rate_limit.per_ip_rps = 0.0;
    cfg
}

fn run_cfg() -> RunConfig {
    RunConfig {
        roles: Roles { matcher: true, keeper: true },
        tick: Duration::from_millis(50),
        max_book_lag: Some(300),
        ..RunConfig::default()
    }
}

struct Job {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    open: tokio::sync::watch::Sender<bool>,
    exec: ExecNode,
}

impl Job {
    /// `faststart::run_with` on the mock node; the normal store's client is gated unless `open`.
    fn start(cfg: &IndexerConfig, node: &Arc<MockNode>, open: bool) -> Job {
        let plan = faststart::plan(cfg);
        assert_ne!(plan, Plan::Normal, "this start is a fast start");
        let (gate, rx) = tokio::sync::watch::channel(open);
        let full = Arc::new(Gated { inner: node.clone(), open: rx });
        let tip = Arc::new(Gated { inner: node.clone(), open: tokio::sync::watch::channel(true).1 });
        let exec = ExecNode::new(node.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn({
            let (cfg, exec) = (cfg.clone(), exec.clone());
            async move {
                faststart::run_with(cfg, plan, full, tip, exec, Box::new(signer()), run_cfg(), async move {
                    let _ = stopped.await;
                })
                .await
            }
        });
        Job { stop, task, open: gate, exec }
    }

    async fn finish(self) {
        self.stop.send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

/// The tip store has set its cursor (its follower runs): what it sees from here on is "since the start".
fn tip_follows(cfg: &IndexerConfig) -> bool {
    let db = faststart::tip_config(cfg).db_path();
    db.exists()
        && kob_executor::indexer::db::open_reader(&db, "testnet-10")
            .ok()
            .and_then(|c| kob_executor::indexer::db::meta_get(&c, "cursor_hash").ok().flatten())
            .is_some()
}

async fn until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn get_json(addr: SocketAddr, path: &str) -> Option<Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.ok()?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.ok()?;
    let text = String::from_utf8_lossy(&buf).to_string();
    serde_json::from_str(text.split("\r\n\r\n").nth(1)?).ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_start_trades_on_the_tip_store_and_hands_over_to_the_full_one() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let w = World::new();
    // history: a crossing book the tip store will never see (it starts at the sink)
    crossing_pair(&w, &node);
    let cfg = config(dir.path(), &node, None);
    assert_eq!(faststart::plan(&cfg), Plan::Fresh);

    let job = Job::start(&cfg, &node, false);
    let tip_dir = faststart::tip_dir(&cfg);
    until("the tip store follows the chain", 30, || tip_follows(&cfg)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(job.exec.submitted(), 0, "the history is not on the tip store, and the full store applies nothing yet");

    // a crossing book created after the start: the tip store sees it, the matcher plans on it with the full store still empty
    crossing_pair(&w, &node);
    until("a fill of the new book", 30, || job.exec.submitted() >= 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(job.exec.submitted(), 1, "one fill, planned on the tip store");
    assert!(tip_dir.exists(), "still on the tip store");

    // the full store applies the history and takes over: the tip store is deleted and the history's book is traded
    job.open.send(true).unwrap();
    until("the tip store is deleted", 30, || !tip_dir.exists()).await;
    until("a fill of the old book, planned on the full store", 30, || job.exec.submitted() >= 2).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(job.exec.submitted(), 2, "the pending fill is not planned again on the other store");
    assert_eq!(job.exec.distinct(), 2, "no transaction was submitted twice across the switch");
    job.finish().await;
    assert!(!tip_dir.exists() && cfg.db_path().exists());

    // a later start finds the full store only: no fast start
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_read_api_serves_the_tip_store_with_the_backfill_and_follows_the_switch() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let w = World::new();
    crossing_pair(&w, &node);
    let addr = free_addr();
    let cfg = config(dir.path(), &node, Some(addr));
    let job = Job::start(&cfg, &node, false);

    let mut health = None;
    let t0 = Instant::now();
    while health.is_none() {
        assert!(t0.elapsed() < Duration::from_secs(30), "the read API never answered");
        health = get_json(addr, "/v1/health").await.filter(|h| h.get("backfill").is_some());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let h = health.unwrap();
    assert_eq!(h["book_complete"], false, "{h}");
    assert!(h["backfill"]["tip_since_unix"].as_u64().unwrap() > 0, "{h}");
    assert_eq!(h["state"], "following", "the tip store is current at once: {h}");
    let orders = get_json(addr, "/v1/orders?status=open").await.unwrap();
    assert_eq!(orders["items"].as_array().map_or(0, Vec::len), 0, "the history is not on the tip store: {orders}");

    job.open.send(true).unwrap();
    let tip_dir = faststart::tip_dir(&cfg);
    until("the tip store is deleted", 30, || !tip_dir.exists()).await;
    // the API restarts over the full store
    let mut after = None;
    let t0 = Instant::now();
    while after.is_none() {
        assert!(t0.elapsed() < Duration::from_secs(30), "the read API did not come back");
        after = get_json(addr, "/v1/health").await.filter(|h| h["book_complete"] == true);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let h = after.unwrap();
    assert!(h.get("backfill").is_none(), "{h}");
    let orders = get_json(addr, "/v1/orders?status=open").await.unwrap();
    assert!(orders["items"].as_array().map_or(0, Vec::len) >= 2, "the full store lists the history: {orders}");
    job.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_fast_start_resumes_on_its_tip_store() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let w = World::new();
    crossing_pair(&w, &node);
    let cfg = config(dir.path(), &node, None);

    let job = Job::start(&cfg, &node, false);
    let tip_dir = faststart::tip_dir(&cfg);
    until("the tip store follows the chain", 30, || tip_follows(&cfg)).await;
    crossing_pair(&w, &node);
    until("a fill of the new book", 30, || job.exec.submitted() >= 1).await;
    job.finish().await;
    assert!(tip_dir.exists(), "stopped before the switch: the tip store stays");

    // the next start resumes (the full store exists, and so does the tip store)
    assert_eq!(faststart::plan(&cfg), Plan::Resume);
    let job = Job::start(&cfg, &node, true);
    until("the switch", 30, || !tip_dir.exists()).await;
    job.finish().await;
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
}

#[test]
fn only_an_enabled_first_start_at_the_pruning_point_is_a_fast_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = IndexerConfig { data_dir: dir.path().join("d"), ..IndexerConfig::default() };
    // off by default: today's behaviour
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
    cfg.fast_start = true;
    assert_eq!(faststart::plan(&cfg), Plan::Fresh);
    // a start that is not the pruning point has no history to wait for
    cfg.start = StartMode::Sink;
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
    cfg.start = StartMode::PruningPoint;
    // a data directory with a store: used directly
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    std::fs::write(cfg.db_path(), b"").unwrap();
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
    // a leftover tip store: resumed
    std::fs::create_dir_all(faststart::tip_dir(&cfg)).unwrap();
    assert_eq!(faststart::plan(&cfg), Plan::Resume);
    cfg.fast_start = false;
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
}

/// The node moves its pruning point past the full store's cursor before the store has applied anything (a live finding on
/// testnet-10: a start at the pruning point is hit by the node pruning its oldest data): the full store restarts from the new
/// pruning point instead of halting at a gap for good, and the fast start still hands over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_store_whose_start_the_node_pruned_restarts_from_the_new_pruning_point() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let w = World::new();
    node.push_empty(6);
    crossing_pair(&w, &node);
    let cfg = config(dir.path(), &node, None);

    let job = Job::start(&cfg, &node, false);
    let tip_dir = faststart::tip_dir(&cfg);
    // the full store has its cursor at the pruning point (the first block); then the node prunes up to the fourth block
    until("the full store has its start", 30, || {
        cfg.db_path().exists()
            && kob_executor::indexer::db::open_reader(&cfg.db_path(), "testnet-10")
                .ok()
                .and_then(|c| kob_executor::indexer::db::meta_get(&c, "cursor_hash").ok().flatten())
                .is_some()
    })
    .await;
    node.set_retention_floor(3);
    job.open.send(true).unwrap();
    until("the tip store is deleted", 30, || !tip_dir.exists()).await;
    until("a fill of the book, planned on the full store", 30, || job.exec.submitted() >= 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(job.exec.submitted(), 1, "one fill");
    job.finish().await;
    assert_eq!(faststart::plan(&cfg), Plan::Normal);
}
