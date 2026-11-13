//! Per-generation cache of the hot read responses of the API.
//!
//! Every web client of a market polls the same few reads of the same token, and between two
//! commits of the indexer their answers are the same. A response body is kept under the
//! parameters of the request for one data generation of the store
//! ([`crate::indexer::DataGeneration`]). Reads that depend on the clock carry it in their key,
//! so a cached answer is byte for byte the one the request would compute.
//!
//! Concurrent requests of one key compute it once and the others wait for it. A read that
//! fails, or whose client goes away, is not kept, and the next request computes it again.
//!
//! # Bounds
//!
//! A new generation drops the entries of the previous one. Within a generation at most
//! [`MAX_ENTRIES`] responses and [`MAX_BYTES`] bytes are kept. A new key past either bound
//! evicts the least recently used entries. Many distinct keys therefore push out the rare
//! keys and never the hot ones.

use axum::body::Bytes;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// Most responses one generation keeps.
pub const MAX_ENTRIES: usize = 1_024;

/// Most response bytes one generation keeps (32 MiB).
pub const MAX_BYTES: usize = 32 << 20;

/// The response cache. It is shared by all request handlers.
#[derive(Default)]
pub struct ResponseCache {
    inner: Mutex<Inner>,
}

/// One kept response: the cell its requests share, when it was last asked for, and its size.
///
/// The size is 0 until the response is computed.
struct Entry {
    cell: Arc<OnceCell<Bytes>>,
    used: u64,
    bytes: usize,
}

#[derive(Default)]
struct Inner {
    generation: u64,
    cells: HashMap<String, Entry>,
    bytes: usize,
    /// Counter of the lookups. It is the recency that [`Entry::used`] records.
    tick: u64,
}

impl Inner {
    /// Evicts the least recently used entries until a new one fits both bounds.
    fn make_room(&mut self) {
        while !self.cells.is_empty() && (self.cells.len() >= MAX_ENTRIES || self.bytes >= MAX_BYTES) {
            let Some(oldest) = self.cells.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            if let Some(e) = self.cells.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(e.bytes);
            }
        }
    }
}

impl ResponseCache {
    /// Returns the body of `key` read at `generation`.
    ///
    /// The body is the kept one, else the result of `compute`, which is kept when it succeeds.
    /// A request that saw an older generation than the cache holds is computed and not kept.
    pub async fn get_or_compute<E, F, Fut>(&self, generation: u64, key: String, compute: F) -> Result<Bytes, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Bytes, E>>,
    {
        let cell = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if generation > g.generation {
                g.generation = generation;
                g.cells.clear();
                g.bytes = 0;
            }
            if generation == g.generation {
                g.tick += 1;
                let tick = g.tick;
                if !g.cells.contains_key(&key) {
                    g.make_room();
                    g.cells.insert(key.clone(), Entry { cell: Arc::default(), used: tick, bytes: 0 });
                }
                let e = g.cells.get_mut(&key).expect("present");
                e.used = tick;
                Some(e.cell.clone())
            } else {
                None
            }
        };
        let Some(cell) = cell else { return compute().await };
        let mut computed = false;
        let body = cell
            .get_or_try_init(|| {
                computed = true;
                compute()
            })
            .await?
            .clone();
        if computed {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let g = &mut *g;
            // Count the bytes only while the entry is still the one computed, that is, not evicted
            // and not dropped with its generation.
            if let Some(e) = g.cells.get_mut(&key).filter(|e| Arc::ptr_eq(&e.cell, &cell)) {
                e.bytes = body.len();
                g.bytes = g.bytes.saturating_add(body.len());
            }
        }
        Ok(body)
    }

    /// Entries kept now.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).cells.values().filter(|c| c.cell.initialized()).count()
    }

    /// Bytes kept now.
    #[cfg(test)]
    fn bytes(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn body(s: &str) -> Result<Bytes, ()> {
        Ok(Bytes::from(s.to_string()))
    }

    #[tokio::test]
    async fn one_generation_computes_a_key_once_and_a_new_one_drops_it() {
        let c = ResponseCache::default();
        let runs = AtomicUsize::new(0);
        let get = |generation: u64, key: &str, v: &'static str| {
            let runs = &runs;
            let c = &c;
            let key = key.to_string();
            async move {
                c.get_or_compute(generation, key, || async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    body(v)
                })
                .await
                .unwrap()
            }
        };
        assert_eq!(get(1, "a", "x").await, "x");
        assert_eq!(get(1, "a", "y").await, "x", "kept for the generation");
        assert_eq!(get(1, "b", "z").await, "z");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(get(2, "a", "y").await, "y", "a new generation computes again");
        assert_eq!(c.len(), 1, "and dropped the previous generation's entries");
        // a request that saw an older generation is answered but not kept
        assert_eq!(get(1, "a", "old").await, "old");
        assert_eq!(get(2, "a", "w").await, "y");
    }

    #[tokio::test]
    async fn concurrent_requests_of_a_key_share_one_computation_and_failures_are_not_kept() {
        let c = Arc::new(ResponseCache::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (c, runs) = (c.clone(), runs.clone());
                tokio::spawn(async move {
                    c.get_or_compute(1, "k".into(), || async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        body("v")
                    })
                    .await
                    .unwrap()
                })
            })
            .collect();
        for t in tasks {
            assert_eq!(t.await.unwrap(), "v");
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "one read for eight requests");
        let failed: Result<Bytes, &str> = c.get_or_compute(1, "e".into(), || async { Err("busy") }).await;
        assert!(failed.is_err());
        assert_eq!(c.get_or_compute(1, "e".into(), || async { body("ok") }).await.unwrap(), "ok", "an error is not kept");
    }

    #[tokio::test]
    async fn a_generation_is_bounded() {
        let c = ResponseCache::default();
        for i in 0..MAX_ENTRIES + 10 {
            c.get_or_compute(7, format!("k{i}"), || async { body("v") }).await.unwrap();
        }
        assert_eq!(c.len(), MAX_ENTRIES);
        let big = Bytes::from(vec![0u8; MAX_BYTES]);
        c.get_or_compute(7, "big".into(), || async { Ok::<_, ()>(big.clone()) }).await.unwrap();
        assert!(c.bytes() >= MAX_BYTES);
        c.get_or_compute(7, "next".into(), || async { body("v") }).await.unwrap();
        assert!(c.bytes() < MAX_BYTES, "the byte bound evicted down to fit: {}", c.bytes());
        assert!(c.len() <= MAX_ENTRIES);
    }

    /// Many distinct keys evict the least recently used entries one by one, never the whole
    /// cache. A key that keeps being asked for stays and is computed once.
    #[tokio::test]
    async fn distinct_keys_evict_the_least_recently_used_and_keep_the_hot_ones() {
        let c = ResponseCache::default();
        let hot = AtomicUsize::new(0);
        let get_hot = || async {
            c.get_or_compute(3, "hot".into(), || async {
                hot.fetch_add(1, Ordering::SeqCst);
                body("h")
            })
            .await
            .unwrap()
        };
        for i in 0..3 * MAX_ENTRIES {
            c.get_or_compute(3, format!("k{i}"), || async { body("v") }).await.unwrap();
            assert_eq!(get_hot().await, "h");
        }
        assert_eq!(hot.load(Ordering::SeqCst), 1, "the hot key was never evicted");
        assert_eq!(c.len(), MAX_ENTRIES);
        // The newest of the cold keys are still kept and the oldest are gone.
        let runs = AtomicUsize::new(0);
        for k in [format!("k{}", 3 * MAX_ENTRIES - 1), "k0".to_string()] {
            c.get_or_compute(3, k, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                body("v")
            })
            .await
            .unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "only the evicted k0 was computed again");
    }
}
