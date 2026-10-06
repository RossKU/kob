//! Read-only integration test against a live testnet-10 node (v2.1.0 with `--utxoindex`, JSON wRPC).
//!
//! It checks what mocks cannot: that the wire schema the indexer decodes matches the node, that a
//! real `High` batch (with covenant transactions from other projects) is processed without
//! errors, and that the follower keeps up with new chain blocks.
//!
//! Environment:
//! * `KOB_TN10_WRPC`               endpoint, default `ws://127.0.0.1:18210` (a testnet-10 node of your own)
//! * `KOB_SKIP_NETWORK_TESTS=1`    skip (offline runs)
//! * `KOB_REQUIRE_NETWORK_TESTS=1` fail instead of skip when the node is unreachable
//! * `KOB_TN10_FOLLOW_BLOCKS`      new chain blocks to wait for after catching up (default 3)

use kob_executor::config::StartMode;
use kob_executor::hex::Hash32;
use kob_executor::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kob_executor::indexer::ingest::Ingest;
use kob_executor::indexer::status::{FollowerState, HealthState};
use kob_executor::rpc::types::{Verbosity, VspcRequest};
use kob_executor::rpc::{ChainSource, WrpcClient, WrpcConfig};
use kob_executor::testkit::processor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_ENDPOINT: &str = "ws://127.0.0.1:18210";

/// A block `n` selected-parent steps behind `from` (the node reports it in verboseData).
async fn ancestor(client: &WrpcClient, from: Hash32, n: usize) -> Hash32 {
    let mut cur = from;
    for _ in 0..n {
        let raw = client
            .call_raw("getBlock", serde_json::json!({ "hash": cur.to_hex(), "includeTransactions": false }))
            .await
            .expect("getBlock");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        cur = Hash32::parse(v["block"]["verboseData"]["selectedParentHash"].as_str().expect("selectedParentHash")).unwrap();
    }
    cur
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_testnet_10_and_decodes_real_vspc_v2_batches() {
    if std::env::var("KOB_SKIP_NETWORK_TESTS").is_ok() {
        eprintln!("skipped: KOB_SKIP_NETWORK_TESTS is set");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("kob_executor=info"))
        .with_test_writer()
        .try_init();
    let url = std::env::var("KOB_TN10_WRPC").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let mut wcfg = WrpcConfig::new(url.clone());
    wcfg.connect_timeout = Duration::from_secs(8);
    let client = Arc::new(WrpcClient::new(wcfg));
    let info = match client.server_info().await {
        Ok(i) => i,
        Err(e) if std::env::var("KOB_REQUIRE_NETWORK_TESTS").is_err() => {
            eprintln!("skipped: {url} unreachable ({e}); set KOB_REQUIRE_NETWORK_TESTS=1 to fail instead");
            return;
        }
        Err(e) => panic!("node {url} unreachable: {e}"),
    };
    eprintln!(
        "node {url}: version {} network {} synced {} utxoindex {}",
        info.server_version, info.network_id, info.is_synced, info.has_utxo_index
    );
    assert_eq!(info.network_id, "testnet-10");
    assert!(info.is_synced, "the test node must be synced");

    // 1. schema check on a real High batch: parse it directly and count what the indexer will see
    let dag = client.dag_info().await.unwrap();
    assert_eq!(dag.network, "testnet-10");
    // A catch-up from the pruning point would take hundreds of ~30 MB batches (a call returns at most
    // ~2,480 chain blocks); start a few hundred chain blocks behind the sink instead.
    let start = ancestor(&client, dag.sink, 300).await;
    let raw = client.vspc_v2(VspcRequest::new(start, Verbosity::High, None)).await.expect("VSPC v2 from a recent block");
    let batch = raw.into_batch().expect("every accepted transaction decodes");
    assert!(!batch.added.is_empty());
    let (mut txs, mut cov_outputs, mut with_utxo, mut inputs) = (0usize, 0usize, 0usize, 0usize);
    for b in &batch.added {
        for t in &b.txs {
            txs += 1;
            cov_outputs += t.outputs.iter().filter(|o| o.covenant.is_some()).count();
            for i in &t.inputs {
                inputs += 1;
                if i.verbose_data.as_ref().and_then(|v| v.utxo_entry.as_ref()).is_some() {
                    with_utxo += 1;
                }
            }
        }
    }
    eprintln!(
        "first batch: {} chain blocks, {txs} txs, {cov_outputs} covenant outputs, {with_utxo}/{inputs} inputs carry the spent utxo",
        batch.added.len()
    );
    assert_eq!(with_utxo, inputs, "High verbosity must carry the spent UTXO of every input");
    // The node reports `blockDaaScore: null` on every spent utxo entry of a VSPC v2 response: the indexer must
    // not depend on it (it keeps the DAA of the block that created each tracked UTXO).
    let mut cov_inputs = 0usize;
    for b in &batch.added {
        for t in &b.txs {
            for i in &t.inputs {
                if i.verbose_data.as_ref().and_then(|v| v.utxo_entry.as_ref()).is_some_and(|u| u.covenant_id.is_some()) {
                    cov_inputs += 1;
                }
            }
        }
    }
    eprintln!("{cov_inputs} covenant inputs decoded");
    // header fields the indexer relies on are populated and increasing
    let daas: Vec<u64> = batch.added.iter().map(|b| b.header.daa_score).collect();
    assert!(daas.windows(2).all(|w| w[0] <= w[1]), "DAA scores must not decrease along the selected chain");
    assert!(daas[0] > 0);
    drop(batch);

    // 2. the follower end to end: start from the same block, catch up, then follow new blocks
    // the production write path: file database, record log, default reorg window and checkpoints
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("index.sqlite3");
    let log_dir = dir.path().join("records");
    let (log, _) = kob_executor::indexer::recordlog::RecordLog::open(&log_dir, 1 << 26, 0).unwrap();
    let conn = kob_executor::indexer::db::open_writer(&db_path, "testnet-10").unwrap();
    let ingest = Arc::new(Mutex::new(
        Ingest::new(conn, processor(), Some(log)).with_config(kob_executor::indexer::ingest::IngestConfig::default()),
    ));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _rx) = tokio::sync::broadcast::channel(16);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(start),
        poll_interval: Duration::from_secs(1),
        backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(5),
        min_confirmations: None,
        max_walkback_blocks: 1000,
        status_refresh: Duration::from_secs(1),
        caught_up_daa: 1_000,
        batch_initial_blue: 600,
        batch_target: Duration::from_secs(30),
        fetch_parallel: 1,
        prefetch_max_bytes: 256 << 20,
        prefetch_min_lag_blue: 1_200,
        prefetch_initial_blocks: 64,
    };
    let follower = Follower::new(client.clone(), ingest.clone(), cfg, health.clone(), events);
    let want: u64 = std::env::var("KOB_TN10_FOLLOW_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let started = Instant::now();
    let mut catch_up_deadline = started + Duration::from_secs(900);
    let mut applied_total = 0u64;
    // catch up
    loop {
        match follower.step().await {
            StepOutcome::Applied { added, .. } => applied_total += added as u64,
            StepOutcome::Idle => break,
            StepOutcome::Retry(r) => {
                eprintln!("retry: {r}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
        assert!(Instant::now() < catch_up_deadline, "catch-up too slow");
    }
    eprintln!("caught up: {applied_total} chain blocks in {:?}", started.elapsed());
    assert!(applied_total > 0);
    let after_catch_up = health.snapshot().blocks_applied_total;
    // follow: wait for `want` further chain blocks
    catch_up_deadline = Instant::now() + Duration::from_secs(300);
    while health.snapshot().blocks_applied_total < after_catch_up + want {
        match follower.step().await {
            StepOutcome::Applied { .. } | StepOutcome::Idle => {}
            StepOutcome::Retry(r) => eprintln!("retry: {r}"),
            other => panic!("unexpected outcome {other:?}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(Instant::now() < catch_up_deadline, "no new chain blocks within 300 s");
    }
    let snap = health.snapshot();
    eprintln!("health: {snap:?}");
    assert!(matches!(snap.state, FollowerState::Following | FollowerState::CatchingUp));
    assert_eq!(snap.last_error, None);
    assert!(snap.cursor_hash.is_some() && snap.cursor_daa > 0);
    let lag = snap.lag_daa().expect("node status was refreshed");
    assert!(lag < 100_000, "lag {lag} DAA is implausible for a synced node");
    // the cursor is a block the node knows
    assert!(client.block_exists(snap.cursor_hash.unwrap()).await.unwrap());
    // TN10 carries little KOB1 traffic (other projects' covenants are ignored), and today's traffic comes from
    // test deployments: whatever there is must index cleanly and account for every relevant transaction
    let orders = ingest.lock().unwrap().order_count().unwrap();
    let rejects: i64 = ingest.lock().unwrap().conn().query_row("SELECT COUNT(*) FROM rejects", [], |r| r.get(0)).unwrap();
    let relevant = health.snapshot().relevant_txs_total;
    eprintln!("KOB traffic on the followed range: {orders} order(s), {relevant} relevant transaction(s), {rejects} reject(s)");
    assert!(rejects as u64 <= relevant, "a reject is a relevant transaction");
    assert!(orders <= relevant, "an order comes from a relevant transaction");
    // storage policy: nothing raw is kept. The record log holds only the checkpoint frame(s) and the block rows
    // are the reorg window; report the sizes (docs/ops/executor.md quotes this run)
    let blocks: i64 = ingest.lock().unwrap().conn().query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0)).unwrap();
    let applied = health.snapshot().blocks_applied_total;
    let log_bytes = kob_executor::indexer::recordlog::log_bytes(&log_dir).unwrap();
    let frames = kob_executor::indexer::recordlog::record_count(&log_dir).unwrap();
    ingest.lock().unwrap().conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    let db_bytes = std::fs::metadata(&db_path).unwrap().len();
    eprintln!(
        "storage after {applied} chain blocks: record log {log_bytes} B in {frames} frame(s), {blocks} block rows, database file {db_bytes} B"
    );
    // no traffic: only a checkpoint frame; with traffic, about 1 KB (up to a few) per relevant transaction
    assert!(log_bytes < 1_000 + 4_000 * relevant, "the record log holds {log_bytes} B for {relevant} relevant transaction(s)");
    assert_eq!(blocks as u64, applied - health.snapshot().reverted_blocks_total, "one row per block currently on the chain");
}
