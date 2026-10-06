//! Fee policy (`docs/ops/executor.md`, fee policy; `docs/spec/matcher.md` §7): the rate every transaction of the executor pays,
//! from the node's fee estimate.
//!
//! The node (rusty-kaspa v2.1.0 `getFeeEstimate`) answers three kinds of buckets in sompi per gram: a *priority* bucket
//! (sub-second inclusion), *normal* buckets (the first: sub-minute) and *low* buckets (the first: sub-hour). Each job picks a
//! bucket by its [`Urgency`]:
//!
//! | urgency | bucket | jobs |
//! |---|---|---|
//! | [`Urgency::High`] | priority | matcher batches that fill an IOC / FOK / market / streaming order or a triggered stop; keeper kills; x402 intent executions |
//! | [`Urgency::Normal`] | first normal | every other matcher batch; keeper refunds; x402 intent expiries |
//! | [`Urgency::Low`] | first low | maintenance (merges, sales of the operator's tokens); keeper closes and sweeps |
//!
//! The rate is `ceil(bucket)` clamped to `[floor, max_rate]` (the floor is the relay minimum 100 or the operator's
//! `--fee-rate`, which wins over a lower `max_rate`). A transaction whose fee would exceed `max_tx_fee` is rebuilt at the rate
//! that fits ([`FeeRates::capped`]), never below the floor. Without an estimate (the node does not answer it, it is malformed,
//! or the last good one is older than `max_age`) every bucket is the floor: the policy never stops a job, it only prices it.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How soon a transaction should be accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Urgency {
    /// Housekeeping: within the hour is fine.
    Low,
    /// Within the minute.
    Normal,
    /// As soon as possible (a deadline: a kill, an IOC fill).
    High,
}

impl Urgency {
    pub fn name(self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::High => "high",
        }
    }
}

/// The node's fee estimate, sompi per gram: the priority bucket, the first normal and the first low bucket.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeeEstimate {
    pub priority: f64,
    pub normal: f64,
    pub low: f64,
}

impl FeeEstimate {
    /// Parses a `getFeeEstimate` response (`{"estimate": {"priorityBucket": {"feerate", "estimatedSeconds"}, "normalBuckets":
    /// [...], "lowBuckets": [...]}}`, or the inner `estimate` object). An empty normal or low list takes the next faster
    /// bucket. Every rate must be finite and positive.
    pub fn parse(v: &serde_json::Value) -> Result<FeeEstimate, String> {
        let e = v.get("estimate").unwrap_or(v);
        let rate = |b: &serde_json::Value| b.get("feerate").and_then(serde_json::Value::as_f64);
        let first = |k: &str| e.get(k).and_then(serde_json::Value::as_array).and_then(|a| a.first()).and_then(rate);
        let priority = e.get("priorityBucket").and_then(rate).ok_or("getFeeEstimate: no priorityBucket.feerate")?;
        let normal = first("normalBuckets").unwrap_or(priority);
        let low = first("lowBuckets").unwrap_or(normal);
        let est = FeeEstimate { priority, normal, low };
        est.check()?;
        Ok(est)
    }

    fn check(&self) -> Result<(), String> {
        for (k, r) in [("priority", self.priority), ("normal", self.normal), ("low", self.low)] {
            if !r.is_finite() || r <= 0.0 {
                return Err(format!("getFeeEstimate: {k} feerate {r} is not a positive number"));
            }
        }
        Ok(())
    }

    /// The bucket of an urgency.
    pub fn bucket(&self, u: Urgency) -> f64 {
        match u {
            Urgency::High => self.priority,
            Urgency::Normal => self.normal,
            Urgency::Low => self.low,
        }
    }
}

/// Fee policy settings (`kob-executor run --fee-*`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FeePolicy {
    /// Read the node's fee estimate. Off: every transaction pays the floor.
    pub dynamic: bool,
    /// The lowest rate, sompi per gram (at least the relay minimum, 100).
    pub floor: u64,
    /// The highest rate the estimate may raise a transaction to, sompi per gram (below the floor: the floor).
    pub max_rate: u64,
    /// The most one transaction pays in fees, sompi (0: no total cap). Never pushes a transaction below the floor.
    pub max_tx_fee: u64,
    /// How often the estimate is read, milliseconds.
    pub refresh_ms: u64,
    /// The last good estimate is used this long while the node does not answer a new one, milliseconds; then the floor.
    pub max_age_ms: u64,
}

impl Default for FeePolicy {
    fn default() -> Self {
        FeePolicy {
            dynamic: true,
            floor: kob_protocol::tx::MIN_FEE_RATE,
            max_rate: 1_000,
            max_tx_fee: 100_000_000,
            refresh_ms: 10_000,
            max_age_ms: 60_000,
        }
    }
}

impl FeePolicy {
    /// The policy that always pays `rate` (no estimate).
    pub fn fixed(rate: u64) -> Self {
        FeePolicy { dynamic: false, floor: rate, ..FeePolicy::default() }
    }

    /// The rates of a tick from an estimate (`None`: unavailable).
    pub fn rates(&self, est: Option<&FeeEstimate>) -> FeeRates {
        let floor = self.floor.max(kob_protocol::tx::MIN_FEE_RATE);
        let ceiling = self.max_rate.max(floor);
        let est = est.filter(|_| self.dynamic);
        let pick = |u: Urgency| match est {
            // `as u64` saturates (and maps NaN to 0, which the floor lifts)
            Some(e) => (e.bucket(u).ceil() as u64).clamp(floor, ceiling),
            None => floor,
        };
        FeeRates {
            floor,
            low: pick(Urgency::Low),
            normal: pick(Urgency::Normal),
            high: pick(Urgency::High),
            max_tx_fee: self.max_tx_fee,
            estimated: est.is_some(),
        }
    }
}

/// Command-line options of the fee policy (`kob-executor run`, `match`, `keep`); the floor is the command's `--fee-rate`.
#[derive(clap::Args, Debug, Clone)]
pub struct FeeArgs {
    /// Do not read the node's fee estimate: every transaction pays `--fee-rate`.
    #[arg(long)]
    pub no_fee_estimate: bool,
    /// The highest rate the fee estimate may raise a transaction to, sompi per gram.
    #[arg(long, default_value_t = 1_000)]
    pub fee_max_rate: u64,
    /// The most one transaction pays in fees, KAS (0: no total cap). Never pushes a transaction below `--fee-rate`.
    #[arg(long, default_value_t = 1.0)]
    pub fee_max_tx_kas: f64,
    /// How often the fee estimate is read, milliseconds.
    #[arg(long, default_value_t = 10_000)]
    pub fee_refresh_ms: u64,
    /// The last good fee estimate is used this long while the node answers none, milliseconds; then `--fee-rate`.
    #[arg(long, default_value_t = 60_000)]
    pub fee_max_age_ms: u64,
}

impl FeeArgs {
    /// The policy with `floor` (the command's `--fee-rate`).
    pub fn policy(&self, floor: u64) -> Result<FeePolicy, String> {
        if floor < kob_protocol::tx::MIN_FEE_RATE {
            return Err(format!("--fee-rate {floor} is below the relay minimum {}", kob_protocol::tx::MIN_FEE_RATE));
        }
        if !self.fee_max_tx_kas.is_finite() || self.fee_max_tx_kas < 0.0 || self.fee_max_tx_kas > 1e9 {
            return Err(format!("--fee-max-tx-kas {} is not an amount of KAS", self.fee_max_tx_kas));
        }
        Ok(FeePolicy {
            dynamic: !self.no_fee_estimate,
            floor,
            max_rate: self.fee_max_rate,
            max_tx_fee: (self.fee_max_tx_kas * 100_000_000.0).round() as u64,
            refresh_ms: self.fee_refresh_ms,
            max_age_ms: self.fee_max_age_ms,
        })
    }
}

/// The rates of one tick (sompi per gram), by urgency. The default is the floor everywhere and no total cap: a configuration
/// that never sets them behaves as before the policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FeeRates {
    pub floor: u64,
    pub low: u64,
    pub normal: u64,
    pub high: u64,
    /// The most one transaction pays, sompi (0: no cap).
    pub max_tx_fee: u64,
    /// The rates come from the node's estimate (false: the floor).
    pub estimated: bool,
}

impl Default for FeeRates {
    fn default() -> Self {
        FeeRates::flat(kob_protocol::tx::MIN_FEE_RATE)
    }
}

impl FeeRates {
    /// Every urgency at `rate`, no total cap.
    pub fn flat(rate: u64) -> Self {
        FeeRates { floor: rate, low: rate, normal: rate, high: rate, max_tx_fee: 0, estimated: false }
    }

    /// The rate of an urgency, at least `base` (a component's own configured rate).
    pub fn rate(&self, u: Urgency, base: u64) -> u64 {
        let r = match u {
            Urgency::Low => self.low,
            Urgency::Normal => self.normal,
            Urgency::High => self.high,
        };
        r.max(base).max(self.floor)
    }

    /// The lowest rate a job may fall back to (its own configured rate or the policy floor).
    pub fn floor_of(&self, base: u64) -> u64 {
        self.floor.max(base)
    }

    /// The total cap: a transaction built at `rate` that paid `fee` above `max_tx_fee` is rebuilt at the returned rate (the
    /// largest that fits, `rate × max / fee` rounded down, at least `floor`). `None`: no cap, the fee fits, or the rate cannot
    /// go lower.
    pub fn capped(&self, rate: u64, fee: u64, floor: u64) -> Option<u64> {
        if self.max_tx_fee == 0 || fee <= self.max_tx_fee || rate <= floor || fee == 0 {
            return None;
        }
        let r = ((rate as u128 * self.max_tx_fee as u128) / fee as u128) as u64;
        Some(r.max(floor)).filter(|r| *r < rate)
    }

    /// The rate of a job priced from its build at `floor` (which paid `fee_at_floor` and left `profit_at_floor`, when its
    /// profit counts): `want` (its urgency's rate) lowered to the total cap and to the highest rate that leaves `min_profit`
    /// (the fee is linear in the rate: `fee(r) = r × fee_at_floor / floor`). At least `floor`.
    pub fn priced(&self, want: u64, floor: u64, fee_at_floor: u64, profit_at_floor: Option<i64>, min_profit: i64) -> u64 {
        if want <= floor || floor == 0 || fee_at_floor == 0 {
            return floor;
        }
        let mass = fee_at_floor.div_ceil(floor) as u128;
        let mut r = want as u128;
        if self.max_tx_fee > 0 {
            r = r.min(self.max_tx_fee as u128 / mass);
        }
        if let Some(p) = profit_at_floor {
            let room = p as i128 - min_profit as i128;
            if room <= 0 {
                return floor;
            }
            r = r.min(floor as u128 + room as u128 / mass);
        }
        (r.min(u64::MAX as u128) as u64).max(floor)
    }

    /// The highest rate at which a job built at `rate` for `fee` still leaves `min_profit` of a `profit` (profit is linear in
    /// the fee: `profit(r) = profit + fee − r × fee / rate`). `None`: not even the floor leaves it, or `rate` already does.
    pub fn affordable(rate: u64, fee: u64, profit: i64, min_profit: i64, floor: u64) -> Option<u64> {
        if profit >= min_profit || rate == 0 || fee == 0 {
            return None;
        }
        let room = profit as i128 + fee as i128 - min_profit as i128;
        if room <= 0 {
            return None;
        }
        let r = (room * rate as i128 / fee as i128).min(rate as i128 - 1) as u64;
        (r >= floor).then_some(r)
    }
}

/// The current rates shared by the parts of one process: the runner publishes the rates of each step that plans, the x402
/// facilitator of `kob-executor run` prices its intent executions and expiries with them. Starts at the floor.
pub type FeeBoard = std::sync::Arc<std::sync::Mutex<FeeRates>>;

/// The rates on a board (a poisoned lock still holds the last rates).
pub fn read_board(b: &FeeBoard) -> FeeRates {
    *b.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets the fee rate of a builder request (any action; the fee mode is kept).
pub fn set_rate(a: &mut kob_protocol::build::Action, rate: u64) {
    use kob_protocol::build::Action;
    let fee = match a {
        Action::CreateOrder(r) => &mut r.fee,
        Action::CancelOrder(r) => &mut r.fee,
        Action::AmendOrder(r) => &mut r.fee,
        Action::CancelPosition(r) => &mut r.fee,
        Action::RefundOrder(r) => &mut r.fee,
        Action::SendTokens(r) => &mut r.fee,
        Action::Batch(r) => &mut r.fee,
        Action::SwapRoute(r) => &mut r.fee,
        Action::SweepOrder(r) => &mut r.fee,
    };
    fee.fee_rate = Some(rate);
}

/// The estimate a process keeps between ticks: refreshed every `refresh_ms`, the last good one used up to `max_age_ms`.
#[derive(Debug, Default)]
pub struct FeeState {
    /// The last good estimate and when it was read.
    pub last: Option<(Instant, FeeEstimate)>,
    /// When the node was last asked.
    pub asked: Option<Instant>,
    /// The last read failed (logged once per outage).
    pub failing: bool,
    /// Reads that failed (metrics).
    pub failures: u64,
}

impl FeeState {
    /// Whether the node should be asked again at `now`.
    pub fn due(&self, p: &FeePolicy, now: Instant) -> bool {
        p.dynamic && self.asked.is_none_or(|a| now.saturating_duration_since(a) >= Duration::from_millis(p.refresh_ms))
    }

    /// Records the answer of a read at `now`.
    pub fn record(&mut self, now: Instant, r: Result<FeeEstimate, String>) {
        self.asked = Some(now);
        match r {
            Ok(e) => {
                if self.failing {
                    tracing::info!(priority = e.priority, normal = e.normal, low = e.low, "fee estimate available again");
                }
                self.failing = false;
                self.last = Some((now, e));
            }
            Err(why) => {
                self.failures += 1;
                if !self.failing {
                    tracing::warn!(%why, "no fee estimate from the node; the last one is used while it is fresh, then the floor");
                }
                self.failing = true;
            }
        }
    }

    /// The estimate usable at `now` (the last good one within `max_age_ms`).
    pub fn current(&self, p: &FeePolicy, now: Instant) -> Option<&FeeEstimate> {
        self.last.as_ref().filter(|(t, _)| now.saturating_duration_since(*t) <= Duration::from_millis(p.max_age_ms)).map(|(_, e)| e)
    }

    /// The rates at `now`.
    pub fn rates(&self, p: &FeePolicy, now: Instant) -> FeeRates {
        p.rates(self.current(p, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn est(p: f64, n: f64, l: f64) -> FeeEstimate {
        FeeEstimate { priority: p, normal: n, low: l }
    }

    #[test]
    fn the_node_answer_parses() {
        let v = json!({"estimate": {
            "priorityBucket": {"feerate": 211.4, "estimatedSeconds": 0.9},
            "normalBuckets": [{"feerate": 194.0, "estimatedSeconds": 30.0}, {"feerate": 140.2, "estimatedSeconds": 50.0}],
            "lowBuckets": [{"feerate": 115.0, "estimatedSeconds": 3000.0}]
        }});
        assert_eq!(FeeEstimate::parse(&v).unwrap(), est(211.4, 194.0, 115.0));
        // the inner object alone, empty lists take the next faster bucket
        let v = json!({"priorityBucket": {"feerate": 1.0}, "normalBuckets": [], "lowBuckets": []});
        assert_eq!(FeeEstimate::parse(&v).unwrap(), est(1.0, 1.0, 1.0));
        for bad in [
            json!({}),
            json!({"estimate": {"priorityBucket": {}}}),
            json!({"priorityBucket": {"feerate": 0.0}}),
            json!({"priorityBucket": {"feerate": -3.0}}),
            json!({"priorityBucket": {"feerate": 200.0}, "lowBuckets": [{"feerate": "x"}], "normalBuckets": [{"feerate": -1}]}),
        ] {
            assert!(FeeEstimate::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn buckets_by_urgency_rounded_up_and_clamped() {
        let p = FeePolicy { max_rate: 300, ..FeePolicy::default() };
        // the soak's congestion: normal 140..194, low 115, priority above
        let r = p.rates(Some(&est(250.2, 194.0, 115.01)));
        assert_eq!((r.high, r.normal, r.low, r.floor, r.estimated), (251, 194, 116, 100, true));
        assert_eq!(r.rate(Urgency::High, 100), 251);
        assert_eq!(r.rate(Urgency::Normal, 100), 194);
        assert_eq!(r.rate(Urgency::Low, 100), 116);
        // a component's own higher rate wins
        assert_eq!(r.rate(Urgency::Low, 150), 150);
        // an idle network estimates the relay minimum or less: the floor
        let r = p.rates(Some(&est(1.0, 1.0, 1.0)));
        assert_eq!((r.high, r.normal, r.low), (100, 100, 100));
        // a spike is clamped to max_rate; an absurd one too
        let r = p.rates(Some(&est(5_000.0, f64::MAX, 301.0)));
        assert_eq!((r.high, r.normal, r.low), (300, 300, 300));
        // max_rate below the floor: the floor
        let p2 = FeePolicy { floor: 200, max_rate: 150, ..FeePolicy::default() };
        let r = p2.rates(Some(&est(500.0, 180.0, 120.0)));
        assert_eq!((r.high, r.normal, r.low, r.floor), (200, 200, 200, 200));
        // a configured floor below the relay minimum is lifted to it
        assert_eq!(FeePolicy { floor: 10, ..FeePolicy::default() }.rates(None).floor, 100);
    }

    #[test]
    fn without_an_estimate_or_when_static_every_bucket_is_the_floor() {
        let p = FeePolicy { floor: 120, ..FeePolicy::default() };
        let r = p.rates(None);
        assert_eq!((r.high, r.normal, r.low, r.estimated), (120, 120, 120, false));
        let fixed = FeePolicy::fixed(130);
        let r = fixed.rates(Some(&est(900.0, 900.0, 900.0)));
        assert_eq!((r.high, r.normal, r.low, r.estimated), (130, 130, 130, false));
        assert_eq!(FeeRates::default(), FeeRates::flat(100));
    }

    #[test]
    fn total_cap_lowers_the_rate_never_below_the_floor() {
        let r = FeeRates { max_tx_fee: 1_000_000, ..FeeRates::flat(100) };
        // 500 sompi/g for 2,000,000: 250 fits exactly
        assert_eq!(r.capped(500, 2_000_000, 100), Some(250));
        // under the cap: unchanged
        assert_eq!(r.capped(500, 1_000_000, 100), None);
        // the floor is the limit
        assert_eq!(r.capped(500, 50_000_000, 100), Some(100));
        assert_eq!(r.capped(100, 50_000_000, 100), None);
        // no cap
        assert_eq!(FeeRates::flat(100).capped(500, u64::MAX, 100), None);
    }

    #[test]
    fn affordable_rate_keeps_the_minimum_profit() {
        // built at 300 for 30,000 (mass 100), profit -5,000: room 25,000 -> 250
        assert_eq!(FeeRates::affordable(300, 30_000, -5_000, 0, 100), Some(250));
        // profitable already
        assert_eq!(FeeRates::affordable(300, 30_000, 10, 0, 100), None);
        // not even at the floor (100 -> fee 10,000, profit -25,000)
        assert_eq!(FeeRates::affordable(300, 30_000, -25_000, 0, 100), None);
        // min_profit counts
        assert_eq!(FeeRates::affordable(300, 30_000, -5_000, 5_000, 100), Some(200));
    }

    #[test]
    fn a_job_is_priced_from_its_floor_build() {
        let r = FeeRates { max_tx_fee: 0, ..FeeRates::flat(100) };
        // mass 1,000 (fee 100,000 at 100): a tip leaving 250,000 at the floor pays up to 350
        assert_eq!(r.priced(300, 100, 100_000, Some(250_000), 0), 300);
        assert_eq!(r.priced(1_000_000, 100, 100_000, Some(250_000), 0), 350);
        assert_eq!(r.priced(1_000_000, 100, 100_000, Some(250_000), 50_000), 300);
        // nothing left at the floor: the floor
        assert_eq!(r.priced(1_000, 100, 100_000, Some(-5), 0), 100);
        // no profit to keep (a merge): the cap only
        let capped = FeeRates { max_tx_fee: 200_000, ..FeeRates::flat(100) };
        assert_eq!(capped.priced(1_000, 100, 100_000, None, 0), 200);
        assert_eq!(capped.priced(1_000, 100, 300_000, None, 0), 100, "the cap never goes below the floor");
        assert_eq!(r.priced(100, 100, 100_000, None, 0), 100);
    }

    #[test]
    fn state_refreshes_and_ages_out() {
        let p = FeePolicy { refresh_ms: 10_000, max_age_ms: 60_000, ..FeePolicy::default() };
        let t0 = Instant::now();
        let mut s = FeeState::default();
        assert!(s.due(&p, t0));
        assert_eq!(s.rates(&p, t0), p.rates(None));
        s.record(t0, Ok(est(300.0, 200.0, 120.0)));
        assert!(!s.due(&p, t0 + Duration::from_secs(5)));
        assert!(s.due(&p, t0 + Duration::from_secs(10)));
        assert_eq!(s.rates(&p, t0 + Duration::from_secs(5)).normal, 200);
        // a failed read keeps the last good estimate while it is fresh
        s.record(t0 + Duration::from_secs(10), Err("down".into()));
        assert!(s.failing);
        assert_eq!(s.rates(&p, t0 + Duration::from_secs(60)).normal, 200);
        // then the floor
        assert_eq!(s.rates(&p, t0 + Duration::from_secs(61)).normal, 100);
        assert_eq!(s.failures, 1);
        // a static policy never asks
        assert!(!FeeState::default().due(&FeePolicy::fixed(100), t0));
    }
}
