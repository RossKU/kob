//! Catch-up throughput against a live testnet-10 node, read only: the follower starts several hours behind the sink and
//! catches up for a fixed time with 1, 2, 4 and 8 windows fetched at once (`fetch_parallel`), each run from the same
//! start into its own temporary database (the production write path: file database and record log). Prints the
//! sustained MB/s and transactions/s of what was applied (`docs/ops/executor.md`, Part B 3, *Parallel fetch*).
//!
//! Ignored by default (it moves hundreds of MB):
//! `cargo test --release -p kob-executor --test tn10_capacity -- --ignored --nocapture`
//!
//! Environment:
//! * `KOB_TN10_WRPC`        endpoint, default `ws://127.0.0.1:18210`
//! * `KOB_CAP_HOURS_BACK`   how far behind the sink to start (default 4)
//! * `KOB_CAP_START`        a chain block hash to start from instead (printed by a previous run)
//! * `KOB_CAP_SECS`         seconds per run (default 120)
//! * `KOB_CAP_N`            the parallelism values to run, comma separated (default `1,2,4,8`)

use kob_executor::config::StartMode;
use kob_executor::hex::Hash32;
use kob_executor::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kob_executor::indexer::ingest::{Ingest, IngestConfig};
use kob_executor::indexer::status::HealthState;
use kob_executor::rpc::{ChainSource, WrpcClient, WrpcConfig};
use kob_executor::testkit::processor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn env_or<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn client(url: &str, vspc_connections: usize) -> Arc<WrpcClient> {
    let mut w = WrpcConfig::new(url.to_string());
    w.connect_timeout = Duration::from_secs(10);
    w.vspc_connections = vspc_connections;
    Arc::new(WrpcClient::new(w))
}

/// The first chain block at least `target_blue` blue score, walking the selected chain from the pruning point as hashes.
async fn chain_block_at(c: &WrpcClient, target_blue: u64) -> Hash32 {
    let mut cur = c.dag_info().await.expect("getBlockDagInfo").pruning_point_hash;
    loop {
        let hashes = c.chain_hashes(cur).await.expect("getVirtualChainFromBlock").added_chain_block_hashes;
        let last = *hashes.last().expect("the chain continues");
        let last_blue = c.block_blue_score(last).await.unwrap().expect("known");
        if last_blue < target_blue {
            cur = last;
            continue;
        }
        let (mut lo, mut hi) = (0usize, hashes.len() - 1);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if c.block_blue_score(hashes[mid]).await.unwrap().expect("known") >= target_blue {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        return hashes[lo];
    }
}

struct Run {
    n: usize,
    secs: f64,
    bytes: u64,
    txs: u64,
    blocks: u64,
    chain_secs: f64,
    timeouts: u64,
    peak: u64,
    window_blocks: u64,
}

/// DAA score of a block the node knows.
async fn daa_of(c: &WrpcClient, h: Hash32) -> u64 {
    let raw = c.call_raw("getBlock", serde_json::json!({ "hash": h.to_hex(), "includeTransactions": false })).await.expect("getBlock");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v["block"]["header"]["daaScore"].as_u64().expect("daaScore")
}

async fn run(url: &str, start: Hash32, start_daa: u64, n: usize, secs: u64) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = kob_executor::indexer::recordlog::RecordLog::open(&dir.path().join("records"), 1 << 26, 0).unwrap();
    let conn = kob_executor::indexer::db::open_writer(&dir.path().join("index.sqlite3"), "testnet-10").unwrap();
    let ingest = Arc::new(Mutex::new(Ingest::new(conn, processor(), Some(log)).with_config(IngestConfig::default())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _rx) = tokio::sync::broadcast::channel(16);
    let mut cfg = FollowerConfig::from_indexer(&kob_executor::config::IndexerConfig {
        fetch_parallel: n,
        start: StartMode::Hash(start),
        ..Default::default()
    });
    cfg.status_refresh = Duration::from_secs(1);
    let source = client(url, if n > 1 { n } else { 0 });
    let follower = Follower::new(source, ingest, cfg, health.clone(), events);
    // the cursor starts at `start`: the first step initialises it
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        match follower.step().await {
            StepOutcome::Gap(r) => panic!("gap: {r}"),
            StepOutcome::Idle => break,
            StepOutcome::Retry(r) => {
                eprintln!("  N={n}: retry: {r}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            _ => {}
        }
    }
    let took = t0.elapsed().as_secs_f64();
    let h = health.snapshot();
    Run {
        n,
        secs: took,
        bytes: h.wire_bytes_total,
        txs: h.txs_fetched_total,
        blocks: h.blocks_applied_total,
        chain_secs: h.cursor_daa.saturating_sub(start_daa) as f64 / 10.0,
        timeouts: h.vspc_timeouts_total,
        peak: h.prefetch_bytes_peak,
        window_blocks: h.prefetch_window_blocks,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live TN10 node, hundreds of MB: run with --ignored --nocapture"]
async fn catch_up_throughput_by_parallel_windows() {
    if std::env::var("KOB_SKIP_NETWORK_TESTS").is_ok() {
        eprintln!("skipped: KOB_SKIP_NETWORK_TESTS is set");
        return;
    }
    let url = std::env::var("KOB_TN10_WRPC").unwrap_or_else(|_| "ws://127.0.0.1:18210".to_string());
    let probe = client(&url, 0);
    let info = probe.server_info().await.expect("node reachable");
    assert_eq!(info.network_id, "testnet-10");
    let start = match std::env::var("KOB_CAP_START") {
        Ok(h) => Hash32::parse(&h).expect("KOB_CAP_START is a hash"),
        Err(_) => {
            let hours: u64 = env_or("KOB_CAP_HOURS_BACK", 4);
            let sink_blue = probe.sink_blue_score().await.unwrap();
            let t = Instant::now();
            let h = chain_block_at(&probe, sink_blue - hours * 3600 * 10).await;
            eprintln!("start {h} ({hours} h behind the sink, found in {:.0?}); reuse with KOB_CAP_START={h}", t.elapsed());
            h
        }
    };
    let secs: u64 = env_or("KOB_CAP_SECS", 120);
    let ns: Vec<usize> =
        std::env::var("KOB_CAP_N").unwrap_or_else(|_| "1,2,4,8".into()).split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let start_daa = daa_of(&probe, start).await;
    let mut runs = vec![];
    for n in ns {
        let r = run(&url, start, start_daa, n, secs).await;
        eprintln!(
            "N={n}: {:.1} MB/s, {:.0} tx/s, {:.1} chain blocks/s ({} txs, {:.1} MB in {:.0} s; {:.1}x real time; timeouts {}, peak {:.1} MB ahead, window {} blocks)",
            r.bytes as f64 / 1e6 / r.secs,
            r.txs as f64 / r.secs,
            r.blocks as f64 / r.secs,
            r.txs,
            r.bytes as f64 / 1e6,
            r.secs,
            r.chain_secs / r.secs,
            r.timeouts,
            r.peak as f64 / 1e6,
            r.window_blocks
        );
        runs.push(r);
    }
    println!("\n| N | MB/s | tx/s | chain blocks/s | x real time | bytes/tx | timeouts | peak ahead (MB) |");
    println!("|---|---|---|---|---|---|---|---|");
    for r in &runs {
        println!(
            "| {} | {:.2} | {:.0} | {:.1} | {:.1} | {:.0} | {} | {:.1} |",
            r.n,
            r.bytes as f64 / 1e6 / r.secs,
            r.txs as f64 / r.secs,
            r.blocks as f64 / r.secs,
            r.chain_secs / r.secs,
            r.bytes as f64 / r.txs.max(1) as f64,
            r.timeouts,
            r.peak as f64 / 1e6
        );
    }
    assert!(runs.iter().all(|r| r.blocks > 0), "every run made progress");
}
