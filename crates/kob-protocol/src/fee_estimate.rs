//! The node's fee estimate (rusty-kaspa v2.1.0 `getFeeEstimate`) and the congestion rule drawn from it.
//!
//! The node answers three kinds of buckets in sompi per gram: a *priority* bucket (sub-second inclusion), *normal* buckets (the
//! first: sub-minute) and *low* buckets (the first: sub-hour). The executor prices its transactions from them
//! (`kob-executor`, `fee`); the command line and the web app use the same reading to tell a person that blocks are contended
//! ([`FeeEstimate::contended`]).

use serde::{Deserialize, Serialize};

use crate::tx::MIN_FEE_RATE;

/// How far above the relay floor the first normal bucket must be before blocks count as contended, percent.
pub const CONTENDED_MARGIN_PERCENT: u64 = 10;

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

    /// Whether blocks are contended: the rate a normal-urgency transaction would pay (`ceil` of the first normal bucket, as the
    /// executor rounds it) is at least [`CONTENDED_MARGIN_PERCENT`] above `floor` (the relay minimum, [`MIN_FEE_RATE`], or the
    /// rate the caller pays if it is higher). At the relay floor a transaction then waits for a block with room, so a person
    /// may want to pay more. Conservative on purpose: an idle node reports the floor in every bucket, and a normal bucket within
    /// the margin of it is no reason to speak. The priority bucket alone (sub-second inclusion costs a little more even on a quiet
    /// network) and the low bucket (always the slowest) are not read.
    pub fn contended(&self, floor: u64) -> bool {
        let floor = floor.max(MIN_FEE_RATE);
        // `as u64` saturates and maps NaN to 0
        let normal = self.normal.ceil() as u64;
        normal.saturating_mul(100) >= floor.saturating_mul(100 + CONTENDED_MARGIN_PERCENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn est(normal: f64) -> FeeEstimate {
        FeeEstimate { priority: normal + 5.0, normal, low: 100.0 }
    }

    #[test]
    fn contended_at_ten_percent_above_the_floor() {
        // the TN10 flood of 2026-10-02: normal buckets 140 to 194, low 115, while the floor is 100
        assert!(est(194.0).contended(100));
        assert!(est(140.0).contended(100));
        // the margin: 110 is contended, 109.2 rounds up to 110 (the executor pays ceil), 109 is not
        assert!(est(110.0).contended(100));
        assert!(est(109.2).contended(100));
        assert!(!est(109.0).contended(100));
        // an idle node: every bucket at the floor
        assert!(!est(100.0).contended(100));
        assert!(!FeeEstimate { priority: 100.0, normal: 100.0, low: 100.0 }.contended(100));
        // a node that reports below the floor
        assert!(!est(1.0).contended(100));
    }

    #[test]
    fn only_the_normal_bucket_counts() {
        // a dear priority bucket on a quiet network, a dear low bucket: not contended
        assert!(!FeeEstimate { priority: 900.0, normal: 100.0, low: 100.0 }.contended(100));
        assert!(!FeeEstimate { priority: 100.0, normal: 100.0, low: 900.0 }.contended(100));
    }

    #[test]
    fn the_floor_never_falls_below_the_relay_minimum() {
        // a caller that names a lower floor than the relay minimum is measured against the minimum
        assert!(!est(105.0).contended(1));
        assert!(est(110.0).contended(1));
        // a caller that pays more than the minimum is measured against its own rate
        assert!(!est(150.0).contended(150));
        assert!(!est(164.0).contended(150));
        assert!(est(165.0).contended(150));
    }

    #[test]
    fn degenerate_numbers_are_not_contended() {
        assert!(!est(f64::NAN).contended(100));
        assert!(est(f64::INFINITY).contended(100));
        assert!(!est(0.0).contended(100));
        assert!(!est(-5.0).contended(100));
    }

    #[test]
    fn the_node_answer_parses() {
        let v = json!({"estimate": {
            "priorityBucket": {"feerate": 211.4, "estimatedSeconds": 0.9},
            "normalBuckets": [{"feerate": 194.0, "estimatedSeconds": 30.0}, {"feerate": 140.2, "estimatedSeconds": 50.0}],
            "lowBuckets": [{"feerate": 115.0, "estimatedSeconds": 3000.0}]
        }});
        let e = FeeEstimate::parse(&v).unwrap();
        assert_eq!(e, FeeEstimate { priority: 211.4, normal: 194.0, low: 115.0 });
        assert!(e.contended(100));
        let idle = json!({"priorityBucket": {"feerate": 100.0}, "normalBuckets": [], "lowBuckets": []});
        assert!(!FeeEstimate::parse(&idle).unwrap().contended(100));
    }
}
