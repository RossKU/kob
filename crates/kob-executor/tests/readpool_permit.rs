//! Regression: a cancelled read keeps its permit until its blocking task ends.
//!
//! `ReadPool::with` used to release the semaphore permit when the caller's future was dropped (the API guard's timeout, a client
//! disconnect) while the blocking task still held its pooled connection: the next request found the pool empty and panicked
//! (HTTP 500). The permit now travels into the blocking task.

use kob_executor::indexer::db::{meta_get, open_writer, ReadPool};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_read_holds_its_permit_so_the_next_read_waits_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.sqlite3");
    drop(open_writer(&p, "testnet-10").unwrap());
    let pool = ReadPool::open(&p, 1).unwrap();

    let slow = {
        let pool = pool.clone();
        tokio::spawn(async move {
            let _ = pool
                .with(|c| {
                    std::thread::sleep(Duration::from_millis(600));
                    Ok(meta_get(c, "network")?)
                })
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    slow.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // the concurrency bound still holds: the next read queues for the permit (no panic, no error) ...
    let waiting = tokio::time::timeout(Duration::from_millis(150), pool.with(|c| Ok(meta_get(c, "network")?))).await;
    assert!(waiting.is_err(), "the second read must still be waiting for the permit of the running one");

    // ... and is served as soon as the slow task ends
    let r = tokio::time::timeout(Duration::from_secs(3), pool.with(|c| Ok(meta_get(c, "network")?))).await;
    assert_eq!(r.expect("served after the slow read ended").unwrap().as_deref(), Some("testnet-10"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_read_returns_its_connection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.sqlite3");
    drop(open_writer(&p, "testnet-10").unwrap());
    let pool = ReadPool::open(&p, 1).unwrap();
    let r: Result<(), _> = pool.with(|_c| panic!("boom")).await;
    assert!(r.is_err());
    assert!(pool.with(|c| Ok(meta_get(c, "network")?)).await.is_ok(), "the pool is not drained by a panic");
}
