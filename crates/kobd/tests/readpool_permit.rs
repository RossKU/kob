//! Regression: a cancelled read keeps its permit until its blocking task ends.
//!
//! If `ReadPool::with` released the semaphore permit when the caller's future was dropped (the API
//! guard's timeout, a client disconnect) while the blocking task still held its pooled connection,
//! the next request would find the pool empty and panic (HTTP 500). So the permit travels into the
//! blocking task, and the read itself is interrupted (or never started), so the permit comes back
//! as soon as the statement stops. Short reads have a lane of their own.

use kobd::indexer::db::{meta_get, open_writer, ReadPool};
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

/// A read whose caller goes away (the API guard's timeout, a client disconnect) is interrupted, so its connection is free
/// for the next read at once instead of after the statement (or the 20 s watchdog).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_whose_caller_went_away_is_interrupted_and_frees_its_connection() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.sqlite3");
    drop(open_writer(&p, "testnet-10").unwrap());
    let pool = ReadPool::open(&p, 1).unwrap();
    let endless = {
        let pool = pool.clone();
        tokio::spawn(async move {
            pool.with(|c| {
                // minutes of work in one statement
                let n: i64 = c.query_row(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000000000) SELECT COUNT(*) FROM n",
                    [],
                    |r| r.get(0),
                )?;
                Ok(n)
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    endless.abort();
    let started = std::time::Instant::now();
    let r = tokio::time::timeout(Duration::from_secs(5), pool.with(|c| Ok(meta_get(c, "network")?))).await;
    assert_eq!(r.expect("the interrupted read gave its connection back").unwrap().as_deref(), Some("testnet-10"));
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
}

/// The light lane ([`ReadPool::with_light`]) has a connection of its own: a short read never waits for the heavy ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_light_read_does_not_queue_behind_heavy_ones() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.sqlite3");
    drop(open_writer(&p, "testnet-10").unwrap());
    let pool = ReadPool::open(&p, 1).unwrap();
    let heavy: Vec<_> = (0..3)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move {
                pool.with(|c| {
                    std::thread::sleep(Duration::from_millis(500));
                    Ok(meta_get(c, "network")?)
                })
                .await
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let r = tokio::time::timeout(Duration::from_millis(300), pool.with_light(|c| Ok(meta_get(c, "network")?))).await;
    assert_eq!(r.expect("served while the heavy lane is busy").unwrap().as_deref(), Some("testnet-10"));
    for h in heavy {
        assert!(h.await.unwrap().is_ok());
    }
}

/// R3C2-1: a client's reads wait on its own permits (`ClientLanes`), not in the lane's queue, so a client that sends many
/// reads at once holds one place in it, and another client's read is served next, not after all of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_client_holds_one_place_in_the_light_lane_queue() {
    use kobd::indexer::db::{ClientLanes, CLIENT_LANES};
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.sqlite3");
    drop(open_writer(&p, "testnet-10").unwrap());
    let pool = ReadPool::open(&p, 1).unwrap();
    let done: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let (a, b) = (Arc::new(ClientLanes::default()), Arc::new(ClientLanes::default()));
    let read = |who: Arc<ClientLanes>, name: &'static str| {
        let (pool, done) = (pool.clone(), done.clone());
        tokio::spawn(CLIENT_LANES.scope(who, async move {
            pool.with_light(move |c| {
                std::thread::sleep(Duration::from_millis(150));
                Ok(meta_get(c, "network")?)
            })
            .await
            .unwrap();
            done.lock().unwrap().push(name);
        }))
    };
    // client a sends four reads at once, then client b one
    let mut tasks = vec![];
    for name in ["a1", "a2", "a3", "a4"] {
        tasks.push(read(a.clone(), name));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tasks.push(read(b.clone(), "b1"));
    for t in tasks {
        t.await.unwrap();
    }
    let order = done.lock().unwrap().clone();
    let at = |n: &str| order.iter().position(|x| *x == n).unwrap();
    assert!(at("b1") <= 2, "b waits behind at most one read of a besides the running one: {order:?}");
}
