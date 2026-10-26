//! Stopping: a graceful stop (Ctrl+C, SIGTERM; on Windows Ctrl+Break and the console's close) ends the process within a few
//! seconds even when the node does not answer, and leaves the database flushed.
//!
//! The in-process tests give the follower and the runner a node that never answers; the process test starts the real binary
//! against a TCP port that accepts and says nothing, and stops it the way an operator does.

#[path = "matcher_common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_executor::config::{IndexerConfig, StartMode};
use kob_executor::hex::Hash32;
use kob_executor::indexer::Indexer;
use kob_executor::matcher::book::MemoryBook;
use kob_executor::matcher::node::*;
use kob_executor::matcher::run::{Roles, RunConfig, Runner, ViewSource};
use kob_executor::rpc::types::{
    AddressUtxo as ChainUtxo, ChainHashes, ChainIds, DagInfo as ChainDag, RawVspcResponse, ServerInfo as ChainInfo, VspcRequest,
};
use kob_executor::rpc::{ChainSource, RpcError as ChainError};
use kob_executor::testkit::MockNode as ChainMock;

/// A node whose every call hangs: the connection is open and nothing comes back (the request timeouts are minutes).
struct Silent;

impl NodeApi for Silent {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        std::future::pending().await
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        std::future::pending().await
    }
    async fn submit(&self, _tx: serde_json::Value) -> Result<[u8; 32], RpcError> {
        std::future::pending().await
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        std::future::pending().await
    }
    async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        std::future::pending().await
    }
}

/// The runner is asked to stop while its step waits for a node that does not answer: it returns after the grace period, not
/// after the node's request timeout (it used to look at the stop only between steps, and not at all during the first).
#[tokio::test]
async fn the_runner_stops_while_a_step_waits_for_the_node() {
    let cfg = RunConfig { roles: Roles { matcher: true, keeper: false }, ..RunConfig::default() };
    let mut runner = Runner::new(Silent, ViewSource(MemoryBook::default()), Box::new(common::signer()), cfg);
    let t0 = Instant::now();
    let stop = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let res = tokio::time::timeout(Duration::from_secs(10), runner.run(stop)).await;
    assert!(matches!(res, Ok(Ok(()))), "the runner did not stop: {res:?}");
    assert!(t0.elapsed() < kob_executor::matcher::run::STEP_GRACE + Duration::from_secs(3), "stopped after {:?}", t0.elapsed());
}

/// The mock chain node, except that the windows never arrive.
struct Stalled {
    inner: Arc<ChainMock>,
    windows_asked: AtomicUsize,
}

impl ChainSource for Stalled {
    async fn vspc_v2(&self, _req: VspcRequest) -> Result<RawVspcResponse, ChainError> {
        self.windows_asked.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
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

fn index_config(dir: &std::path::Path, node: &ChainMock) -> IndexerConfig {
    let tokens = dir.join("tokens.json");
    std::fs::write(&tokens, "[]").unwrap();
    let mut cfg = IndexerConfig {
        network: "testnet-10".into(),
        data_dir: dir.join("data"),
        tokens_path: Some(tokens),
        start: StartMode::Hash(node.anchor()),
        poll_interval_ms: 5,
        backoff_ms: 5,
        max_backoff_ms: 20,
        ..IndexerConfig::default()
    };
    cfg.api.enabled = false;
    cfg
}

async fn until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The follower is waiting for a window the node does not send: the stop does not wait for it (the request timeout is
/// minutes), and the database is flushed after it.
#[tokio::test]
async fn the_follower_stops_while_a_window_is_on_its_way() {
    let dir = tempfile::tempdir().unwrap();
    let node = ChainMock::new("testnet-10");
    node.push_empty(10);
    let cfg = index_config(dir.path(), &node);
    let idx = Indexer::open(cfg.clone()).unwrap();
    let source = Arc::new(Stalled { inner: node, windows_asked: AtomicUsize::new(0) });
    let running = idx.spawn(source.clone()).unwrap();
    until("the follower asks for a window", 20, || source.windows_asked.load(Ordering::SeqCst) > 0).await;
    let t0 = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), running.stop()).await.expect("the follower did not stop");
    assert!(t0.elapsed() < Duration::from_secs(2), "stopped after {:?}", t0.elapsed());
    idx.flush().await;
}

/// A clean stop writes the write-ahead log into the database file and cuts it back, even while a reader still has the file
/// open (the read API's connections are closed by then, but a reader of an operator's tool is not).
#[tokio::test]
async fn a_flush_leaves_the_file_alone_without_its_log() {
    let dir = tempfile::tempdir().unwrap();
    let node = ChainMock::new("testnet-10");
    node.push_empty(30);
    let cfg = index_config(dir.path(), &node);
    let idx = Indexer::open(cfg.clone()).unwrap();
    let running = idx.spawn(node.clone()).unwrap();
    until("the store is at the tip", 20, || idx.health.snapshot().cursor_hash == Some(node.tip())).await;
    running.stop().await;
    let reader = kob_executor::indexer::db::open_reader(&cfg.db_path(), "testnet-10").unwrap();
    let wal = {
        let mut p = cfg.db_path().into_os_string();
        p.push("-wal");
        std::path::PathBuf::from(p)
    };
    assert!(wal.metadata().map(|m| m.len()).unwrap_or(0) > 0, "the batches are in the log until it is flushed");
    idx.flush().await;
    assert_eq!(wal.metadata().map(|m| m.len()).unwrap_or(0), 0, "the log is empty after the flush");
    drop(reader);
}

// ---- the real binary -------------------------------------------------------------------------------------------------

/// Asks the process to stop the way the platform does. `false`: it cannot be done here (a Windows test run without a console).
#[cfg(unix)]
fn ask_to_stop(child: &std::process::Child) -> bool {
    std::process::Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn ask_to_stop(child: &std::process::Child) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
    }
    const CTRL_BREAK_EVENT: u32 = 1;
    // the child leads a process group of its own (`creation_flags` below), so only it gets the event
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) != 0 }
}

/// `kob-executor run` against a node that accepts the connection and never answers: a stop asked for while the first step
/// waits for it (the signal used to kill the process on the spot there, and later ones were swallowed until the node's
/// request timeout) ends the process cleanly within a few seconds.
#[test]
fn the_binary_stops_within_seconds_when_the_node_is_silent() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    // a node that takes connections and says nothing
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = held.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            keep.lock().unwrap().push(s);
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("operator.key");
    // a throwaway key, never funded (readable by its owner only: the executor refuses a key file others can read)
    kob_executor::matcher::wallet::HotKey::create_file(&key).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kob-executor"));
    cmd.args(["run", "--no-fast-start", "--no-api", "--network", "testnet-10", "--rpc-url"])
        .arg(format!("ws://127.0.0.1:{port}"))
        .arg("--data-dir")
        .arg(dir.path().join("data"))
        .arg("--key-file")
        .arg(&key)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = cmd.spawn().unwrap();

    // the log, line by line, from a thread: the test waits for the line that comes after the signal listeners are installed
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let t0 = Instant::now();
    let mut log = String::new();
    while !log.contains("starting the executor") {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(l) => {
                log.push_str(&l);
                log.push('\n');
            }
            Err(_) => {
                let _ = child.kill();
                panic!("the executor did not start in {:?}: {log}", t0.elapsed());
            }
        }
    }
    // the first step is now waiting for the node
    std::thread::sleep(Duration::from_millis(1_500));
    if !ask_to_stop(&child) {
        eprintln!("skipped: this test run cannot send the stop signal (no console)");
        let _ = child.kill();
        let _ = child.wait();
        return;
    }
    let asked = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if asked.elapsed() > Duration::from_secs(8) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the executor was still running {:?} after it was asked to stop", asked.elapsed());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = reader.join();
    while let Ok(l) = rx.try_recv() {
        log.push_str(&l);
        log.push('\n');
    }
    assert!(status.success(), "the stop is a clean exit, not a kill: {status:?}\n{log}");
    assert!(log.contains("shutting down"), "{log}");
    assert!(log.contains("database flushed"), "{log}");
    assert!(asked.elapsed() < Duration::from_secs(6), "stopped after {:?}", asked.elapsed());
}
