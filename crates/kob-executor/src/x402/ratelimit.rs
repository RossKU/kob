//! Token-bucket rate limiting keyed by caller (client IP, merchant id).
//!
//! Time is passed in (`now_ms`) so tests are deterministic. Memory is bounded: buckets that are full
//! again (idle callers) are swept when the table is at capacity, and a table that is still full after the
//! sweep refuses new keys (fail closed) instead of growing.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;

/// A bucket configuration: `burst` requests at once, refilled at `per_second`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub burst: u32,
    pub per_second: f64,
}

impl Rate {
    pub fn new(burst: u32, per_second: f64) -> Rate {
        Rate { burst, per_second }
    }
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_ms: u64,
}

/// A keyed token-bucket limiter.
pub struct Limiter<K: Hash + Eq + Clone> {
    default: Rate,
    max_keys: usize,
    buckets: Mutex<HashMap<K, (Bucket, Rate)>>,
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    pub fn new(default: Rate, max_keys: usize) -> Self {
        Limiter { default, max_keys: max_keys.max(1), buckets: Mutex::new(HashMap::new()) }
    }

    /// Takes one token for `key` at `now_ms` using the limiter's default rate.
    pub fn check(&self, key: &K, now_ms: u64) -> Result<(), u64> {
        self.check_with(key, self.default, now_ms)
    }

    /// Takes one token for `key` using `rate` (a per-key override, for example one merchant's limit).
    /// `Err(retry_after_seconds)` when the bucket is empty (at least 1).
    pub fn check_with(&self, key: &K, rate: Rate, now_ms: u64) -> Result<(), u64> {
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if !map.contains_key(key) && map.len() >= self.max_keys {
            map.retain(|_, (b, r)| refilled(b, r, now_ms) < r.burst as f64);
            if map.len() >= self.max_keys {
                return Err(1);
            }
        }
        let (b, r) = map.entry(key.clone()).or_insert((Bucket { tokens: rate.burst as f64, last_ms: now_ms }, rate));
        *r = rate;
        b.tokens = refilled(b, r, now_ms).min(r.burst as f64);
        b.last_ms = now_ms.max(b.last_ms);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else if r.per_second <= 0.0 {
            Err(3600)
        } else {
            Err((((1.0 - b.tokens) / r.per_second).ceil() as u64).max(1))
        }
    }

    /// Number of tracked keys.
    pub fn len(&self) -> usize {
        self.buckets.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn refilled(b: &Bucket, r: &Rate, now_ms: u64) -> f64 {
    b.tokens + now_ms.saturating_sub(b.last_ms) as f64 / 1000.0 * r.per_second
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_refill() {
        let l = Limiter::new(Rate::new(3, 1.0), 10);
        for _ in 0..3 {
            assert!(l.check(&"a", 0).is_ok());
        }
        assert_eq!(l.check(&"a", 0), Err(1));
        assert!(l.check(&"a", 999).is_err());
        assert!(l.check(&"a", 1_000).is_ok());
        assert!(l.check(&"a", 1_000).is_err());
        // a long pause refills at most `burst`
        for _ in 0..3 {
            assert!(l.check(&"a", 3_600_000).is_ok());
        }
        assert!(l.check(&"a", 3_600_000).is_err());
    }

    #[test]
    fn keys_are_independent_and_retry_after_scales() {
        let l = Limiter::new(Rate::new(1, 0.5), 10);
        assert!(l.check(&1u32, 0).is_ok());
        assert!(l.check(&2u32, 0).is_ok());
        assert_eq!(l.check(&1u32, 0), Err(2));
    }

    #[test]
    fn per_key_override() {
        let l = Limiter::new(Rate::new(100, 100.0), 10);
        let tight = Rate::new(1, 0.1);
        assert!(l.check_with(&"m", tight, 0).is_ok());
        assert_eq!(l.check_with(&"m", tight, 0), Err(10));
    }

    #[test]
    fn table_is_bounded_and_fails_closed() {
        let l = Limiter::new(Rate::new(1, 0.001), 2);
        assert!(l.check(&"a", 0).is_ok());
        assert!(l.check(&"b", 0).is_ok());
        // both buckets are empty, so nothing can be evicted: a third key is refused
        assert!(l.check(&"c", 0).is_err());
        assert_eq!(l.len(), 2);
        // after a long time the idle buckets are full again and get swept
        assert!(l.check(&"c", 10_000_000).is_ok());
        assert!(l.len() <= 2);
    }
}
