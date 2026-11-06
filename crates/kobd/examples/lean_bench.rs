//! Times the lean pruning batches on a copy of a store: `lean_bench <index.sqlite3> [cap_mb] [max_batches]`.
//!
//! Opens the copy as a lean store (the first open rewrites a store that keeps its history: a minute per few GB), then runs the
//! batches the follower would run, at the node position of the store's cursor, until the store is under the cap. Prints the
//! time of each batch group and the totals: with a follower that is caught up a batch is followed by a pause of twice its time, at least 0.1 s.

use kobd::indexer::db::open_writer_with;
use kobd::indexer::lean::{self, LeanConfig};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(args.get(1).expect("index.sqlite3"));
    let cap_mb: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(500);
    let max_batches: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let t = Instant::now();
    let mut conn = open_writer_with(&path, "testnet-10", true).expect("open");
    println!("open as a lean store: {:.1} s", t.elapsed().as_secs_f64());
    let cursor_daa: i64 = conn.query_row("SELECT MAX(daa) FROM blocks", [], |r| r.get(0)).expect("blocks");
    let cfg = LeanConfig::new(30 * 86_400 * 10, cap_mb * 1_000_000);
    println!("used {:.0} MB, cap {cap_mb} MB, cursor daa {cursor_daa}", lean::used_bytes(&conn).unwrap() as f64 / 1e6);
    let mut times: Vec<f64> = Vec::new();
    let (mut orders, mut rows, mut pages) = (0usize, 0usize, 0usize);
    let start = Instant::now();
    for n in 0..max_batches {
        let t = Instant::now();
        let r = lean::prune(&mut conn, &cfg, cursor_daa as u64, Some(cursor_daa as u64), 432_000).expect("prune");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        times.push(ms);
        orders += r.orders;
        rows += r.rows;
        pages += r.pages;
        if n < 5 || n % 500 == 0 {
            println!(
                "batch {n}: {ms:.0} ms, {} orders, {} rows, {} pages, more {}, used {:.0} MB",
                r.orders,
                r.rows,
                r.pages,
                r.more,
                lean::used_bytes(&conn).unwrap() as f64 / 1e6
            );
        }
        if !r.more {
            break;
        }
    }
    let total: f64 = times.iter().sum::<f64>() / 1000.0;
    let mut sorted = times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| sorted[((sorted.len() as f64 - 1.0) * p) as usize];
    println!(
        "{} batches in {:.1} s of pruning ({:.1} s wall): {orders} orders, {rows} rows, {pages} pages; batch ms: median {:.0}, p90 {:.0}, p99 {:.0}, max {:.0}; used {:.0} MB",
        times.len(),
        total,
        start.elapsed().as_secs_f64(),
        q(0.5),
        q(0.9),
        q(0.99),
        sorted.last().copied().unwrap_or(0.0),
        lean::used_bytes(&conn).unwrap() as f64 / 1e6
    );
    println!(
        "with the follower's pause of max(0.1 s, 2 x the batch) after each batch: {:.0} s",
        times.iter().map(|ms| ms / 1000.0 + (2.0 * ms / 1000.0).max(0.1)).sum::<f64>()
    );
}
