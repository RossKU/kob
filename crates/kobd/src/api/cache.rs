//! A per-batch cache of the hot read responses (`/v1/stats`, `/v1/books`, `/v1/depth`, `/v1/tokens`, `/v1/candles`).
//!
//! Every web client of a market polls the same few reads of the same token; between two commits of the indexer their answers
//! are the same. A response body is kept under the request's parameters for one **data generation** of the store
//! ([`crate::indexer::DataGeneration`]: bumped after every committed batch, revert, operator import and frozen-flag change),
//! and the reads that depend on the clock carry it in their key (the node's DAA score, the wall-clock second), so a cached
//! answer is byte for byte the one the request would have computed. Concurrent requests of one key compute it once (the
//! others wait for it); a read that fails, or whose client goes away, is not kept and the next request computes it again.
//!
//! Memory is bounded: a new generation drops the previous one's entries, and a generation that reaches [`MAX_ENTRIES`] or
//! [`MAX_BYTES`] (many distinct parameters, or a store that has not committed for a long time while the clock moved) starts
//! over empty.

use axum::body::Bytes;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// Most responses one generation keeps.
pub const MAX_ENTRIES: usize = 1_024;

/// Most response bytes one generation keeps (32 MiB).
pub const MAX_BYTES: usize = 32 << 20;

#[derive(Default)]
pub struct ResponseCache {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    generation: u64,
    cells: HashMap<String, Arc<OnceCell<Bytes>>>,
    bytes: usize,
}

impl ResponseCache {
    /// The body of `key` read at `generation`: the kept one, else `compute`'s (kept when it succeeds). A request that saw an
    /// older generation than the cache holds is computed and not kept.
    pub async fn get_or_compute<E, F, Fut>(&self, generation: u64, key: String, compute: F) -> Result<Bytes, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Bytes, E>>,
    {
        let cell = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if generation > g.generation || g.cells.len() >= MAX_ENTRIES || g.bytes >= MAX_BYTES {
                g.generation = g.generation.max(generation);
                g.cells.clear();
                g.bytes = 0;
            }
            (generation == g.generation).then(|| g.cells.entry(key).or_default().clone())
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
            if g.generation == generation {
                g.bytes = g.bytes.saturating_add(body.len());
            }
        }
        Ok(body)
    }

    /// Entries kept now.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).cells.values().filter(|c| c.initialized()).count()
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
        assert!(c.len() <= MAX_ENTRIES, "{}", c.len());
        let big = Bytes::from(vec![0u8; MAX_BYTES]);
        c.get_or_compute(7, "big".into(), || async { Ok::<_, ()>(big.clone()) }).await.unwrap();
        c.get_or_compute(7, "next".into(), || async { body("v") }).await.unwrap();
        assert_eq!(c.len(), 1, "the byte bound started the generation over");
    }
}
