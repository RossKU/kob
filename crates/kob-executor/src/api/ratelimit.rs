//! Token-bucket rate limiting: one bucket per client plus one global bucket.
//!
//! Memory is bounded: idle client buckets expire after `bucket_ttl_secs`, and when the table hits
//! `max_tracked_clients` the oldest buckets are evicted (an address-spray flood can only churn
//! entries, never grow the table). The clock is injectable for tests.

use crate::api::client_ip::rate_key;
use crate::config::RateLimitConfig;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait Clock: Send + Sync + 'static {
    /// Monotonic nanoseconds.
    fn now_ns(&self) -> u64;
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock { start: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

/// A manually advanced clock.
#[derive(Default)]
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn advance(&self, d: Duration) {
        self.0.fetch_add(d.as_nanos() as u64, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ns(&self) -> u64 {
        // Start away from zero so "last seen" arithmetic never underflows.
        self.0.load(Ordering::SeqCst) + 1_000_000_000
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_ns: u64,
}

impl Bucket {
    fn full(burst: f64, now: u64) -> Self {
        Bucket { tokens: burst, last_ns: now }
    }

    /// Take `cost` tokens (at most the burst, or nothing could ever pass); on failure return the seconds until they are available.
    fn take(&mut self, now: u64, rate: f64, burst: f64, cost: f64) -> Result<(), f64> {
        let cost = cost.clamp(1.0, burst.max(1.0));
        let elapsed = now.saturating_sub(self.last_ns) as f64 / 1e9;
        self.tokens = (self.tokens + elapsed * rate).min(burst);
        self.last_ns = now;
        if self.tokens >= cost {
            self.tokens -= cost;
            Ok(())
        } else {
            Err(((cost - self.tokens) / rate).max(0.0))
        }
    }
}

struct Clients {
    map: HashMap<IpAddr, Bucket>,
    last_sweep_ns: u64,
}

pub struct RateLimiter {
    cfg: RateLimitConfig,
    clock: Arc<dyn Clock>,
    clients: Mutex<Clients>,
    global: Mutex<Bucket>,
}

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Self {
        Self::with_clock(cfg, Arc::new(SystemClock::new()))
    }

    pub fn with_clock(cfg: RateLimitConfig, clock: Arc<dyn Clock>) -> Self {
        let now = clock.now_ns();
        let global = Bucket::full(cfg.global_burst.max(1) as f64, now);
        RateLimiter {
            cfg,
            clock,
            clients: Mutex::new(Clients { map: HashMap::new(), last_sweep_ns: now }),
            global: Mutex::new(global),
        }
    }

    /// Admit one request from `ip`, or say how long to wait.
    pub fn check(&self, ip: IpAddr) -> Result<(), Duration> {
        self.check_cost(ip, 1)
    }

    /// Admit a request that costs `cost` tokens (a candles or stats query is not the price of a health check).
    /// The client bucket is charged first; the global bucket is charged only for requests that passed it, so a client that
    /// is already limited cannot drain everybody else's share.
    pub fn check_cost(&self, ip: IpAddr, cost: u32) -> Result<(), Duration> {
        let now = self.clock.now_ns();
        let cost = cost.max(1) as f64;
        if self.cfg.per_ip_rps > 0.0 {
            let key = rate_key(ip, self.cfg.ipv6_prefix_bits);
            let burst = self.cfg.per_ip_burst.max(1) as f64;
            let mut c = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            self.maintain(&mut c, now, &key);
            let b = c.map.entry(key).or_insert_with(|| Bucket::full(burst, now));
            b.take(now, self.cfg.per_ip_rps, burst, cost).map_err(secs)?;
        }
        if self.cfg.global_rps > 0.0 {
            let burst = self.cfg.global_burst.max(1) as f64;
            let mut g = self.global.lock().unwrap_or_else(|e| e.into_inner());
            g.take(now, self.cfg.global_rps, burst, cost).map_err(secs)?;
        }
        Ok(())
    }

    fn maintain(&self, c: &mut Clients, now: u64, key: &IpAddr) {
        let ttl = self.cfg.bucket_ttl_secs.max(1) * 1_000_000_000;
        if now.saturating_sub(c.last_sweep_ns) >= ttl / 4 {
            c.map.retain(|_, b| now.saturating_sub(b.last_ns) < ttl);
            c.last_sweep_ns = now;
        }
        let cap = self.cfg.max_tracked_clients.max(1);
        if c.map.len() >= cap && !c.map.contains_key(key) {
            c.map.retain(|_, b| now.saturating_sub(b.last_ns) < ttl);
            if c.map.len() >= cap {
                // Evict the oldest tenth (at least one) in one pass so a spray attack pays O(n) rarely.
                let evict = (cap / 10).max(1);
                let mut ages: Vec<(u64, IpAddr)> = c.map.iter().map(|(k, b)| (b.last_ns, *k)).collect();
                ages.sort_unstable_by_key(|(t, _)| *t);
                for (_, k) in ages.into_iter().take(evict) {
                    c.map.remove(&k);
                }
            }
        }
    }

    /// Number of tracked client buckets.
    pub fn tracked_clients(&self) -> usize {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).map.len()
    }
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s.max(0.0))
}

/// `Retry-After` header value: whole seconds, at least 1.
pub fn retry_after_secs(d: Duration) -> u64 {
    d.as_secs_f64().ceil().max(1.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(per_ip_rps: f64, per_ip_burst: u32, global_rps: f64, global_burst: u32) -> RateLimitConfig {
        RateLimitConfig {
            per_ip_rps,
            per_ip_burst,
            global_rps,
            global_burst,
            bucket_ttl_secs: 60,
            max_tracked_clients: 100,
            ..Default::default()
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn limiter(c: RateLimitConfig) -> (RateLimiter, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::default());
        (RateLimiter::with_clock(c, clock.clone()), clock)
    }

    #[test]
    fn burst_then_refill() {
        let (l, clock) = limiter(cfg(2.0, 3, 0.0, 1));
        let a = ip("1.1.1.1");
        for _ in 0..3 {
            assert!(l.check(a).is_ok());
        }
        let wait = l.check(a).unwrap_err();
        assert!(wait > Duration::ZERO && wait <= Duration::from_millis(500), "{wait:?}");
        assert_eq!(retry_after_secs(wait), 1);
        clock.advance(Duration::from_millis(500));
        assert!(l.check(a).is_ok());
        assert!(l.check(a).is_err());
        // tokens never exceed the burst however long we wait
        clock.advance(Duration::from_secs(3600));
        for _ in 0..3 {
            assert!(l.check(a).is_ok());
        }
        assert!(l.check(a).is_err());
    }

    #[test]
    fn clients_are_independent_and_v6_shares_a_64() {
        let (l, _c) = limiter(cfg(1.0, 1, 0.0, 1));
        assert!(l.check(ip("1.1.1.1")).is_ok());
        assert!(l.check(ip("1.1.1.1")).is_err());
        assert!(l.check(ip("2.2.2.2")).is_ok());
        assert!(l.check(ip("2001:db8:0:1::1")).is_ok());
        assert!(l.check(ip("2001:db8:0:1::ffff")).is_err());
        assert!(l.check(ip("2001:db8:0:2::1")).is_ok());
    }

    #[test]
    fn global_bucket_caps_all_clients() {
        let (l, clock) = limiter(cfg(100.0, 100, 1.0, 2));
        assert!(l.check(ip("1.1.1.1")).is_ok());
        assert!(l.check(ip("2.2.2.2")).is_ok());
        assert!(l.check(ip("3.3.3.3")).is_err());
        clock.advance(Duration::from_secs(1));
        assert!(l.check(ip("4.4.4.4")).is_ok());
    }

    #[test]
    fn heavy_requests_cost_more_and_never_exceed_the_burst() {
        let (l, _c) = limiter(cfg(1.0, 10, 0.0, 1));
        let a = ip("1.1.1.1");
        assert!(l.check_cost(a, 5).is_ok());
        assert!(l.check_cost(a, 5).is_ok());
        assert!(l.check_cost(a, 5).is_err(), "ten tokens are gone");
        // a cost above the burst is clamped to it: it can pass, alone, once the bucket is full
        let (l, _c) = limiter(cfg(1.0, 3, 0.0, 1));
        assert!(l.check_cost(a, 99).is_ok());
        assert!(l.check_cost(a, 1).is_err());
    }

    #[test]
    fn a_limited_client_does_not_drain_the_global_bucket() {
        let (l, _c) = limiter(cfg(1.0, 1, 1.0, 5));
        let a = ip("1.1.1.1");
        assert!(l.check(a).is_ok());
        for _ in 0..50 {
            assert!(l.check(a).is_err());
        }
        // four global tokens are left for everybody else
        for i in 2..6 {
            assert!(l.check(ip(&format!("2.2.2.{i}"))).is_ok(), "{i}");
        }
    }

    #[test]
    fn disabled_when_rates_are_zero() {
        let (l, _c) = limiter(cfg(0.0, 1, 0.0, 1));
        for _ in 0..1000 {
            assert!(l.check(ip("1.1.1.1")).is_ok());
        }
        assert_eq!(l.tracked_clients(), 0);
    }

    #[test]
    fn table_is_bounded_and_ttl_expires() {
        let mut c = cfg(1.0, 1, 0.0, 1);
        c.max_tracked_clients = 50;
        let (l, clock) = limiter(c);
        for i in 0..500u32 {
            clock.advance(Duration::from_millis(1));
            let a = IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + i));
            let _ = l.check(a);
            assert!(l.tracked_clients() <= 50);
        }
        clock.advance(Duration::from_secs(1000));
        let _ = l.check(ip("9.9.9.9"));
        assert_eq!(l.tracked_clients(), 1);
    }
}
