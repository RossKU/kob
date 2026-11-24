//! The fee ceiling of every sending command.
//!
//! A plan whose fee is above [`FACTOR`] times what the node's fee estimate asks for is refused
//! with the issue code [`CODE`], unless `--allow-high-fee` is given. The reference rate is the
//! higher of the node's priority and normal buckets (sompi per gram), at least the relay floor.
//! It is charged on the mass the fee itself is charged on (the compute and transient mass, or with
//! `--priority-fee` the storage-inclusive one), so the relay fee, the priority fee and a normal `--fee-rate` pass; a fee that is a mistake (a wrong
//! `--fee-rate`, `--fee`, or a change folded into the fee) does not. A node that gives no estimate
//! leaves the relay floor as the reference.

use kob_protocol::fee_estimate::FeeEstimate;
use kob_protocol::tx::{BuiltTx, MIN_FEE_RATE};
use serde_json::Value;

/// How many times the reference fee a plan may pay without `--allow-high-fee`.
pub const FACTOR: u64 = 4;
/// The most any transaction may pay in fees, whatever a node's estimate says (R9FC-2): 0.5 KAS, the web's
/// `FEE_ABSOLUTE_MAX`. Only `--max-fee` changes it; `--allow-high-fee` lifts the relative ceiling, not this one.
pub const FEE_ABSOLUTE_MAX: u64 = 50_000_000;
/// The issue code of a plan above the absolute ceiling.
pub const CODE_ABSOLUTE: &str = "FEE_ABOVE_MAXIMUM";

thread_local! {
    static MAX_FEE: std::cell::Cell<u64> = const { std::cell::Cell::new(FEE_ABSOLUTE_MAX) };
}

/// Sets the absolute ceiling of this run (`--max-fee`).
pub fn set_max_fee(sompi: u64) {
    MAX_FEE.with(|m| m.set(sompi));
}

/// The absolute ceiling of this run.
pub fn max_fee() -> u64 {
    MAX_FEE.with(|m| m.get())
}

/// Reads `--max-fee KAS` (a KAS amount, above zero) and sets it.
pub fn max_fee_arg(v: Option<&str>) -> Result<(), String> {
    if let Some(v) = v {
        set_max_fee(crate::trade::units::parse_kas(v, "--max-fee", false)?);
    }
    Ok(())
}
/// The issue code of a plan refused by the ceiling.
pub const CODE: &str = "FEE_ABOVE_CEILING";

/// The ceiling of one command: the reference rate, and whether `--allow-high-fee` lifts it.
#[derive(Debug, Clone, Copy)]
pub struct Ceiling {
    rate: u64,
    estimated: bool,
    allow: bool,
}

impl Ceiling {
    /// From a `getFeeEstimate` answer that was read already (`None`: the node gave none).
    pub fn from_answer(answer: Option<&Value>, allow: bool) -> Ceiling {
        let est = answer.and_then(|a| FeeEstimate::parse(a).ok());
        let rate = est.map(|e| e.priority.max(e.normal).ceil() as u64).unwrap_or(0).max(MIN_FEE_RATE);
        Ceiling { rate, estimated: est.is_some(), allow }
    }

    /// Reads the estimate of `node`. Nothing is asked of the node with `--allow-high-fee`.
    pub fn read(node: &str, allow: bool) -> Ceiling {
        if allow {
            return Ceiling::offline(true);
        }
        Ceiling::from_answer(crate::congestion::answer(node).as_ref(), false)
    }

    /// The ceiling at the relay floor, with no node asked (a dry run that stays offline).
    pub fn offline(allow: bool) -> Ceiling {
        Ceiling { rate: MIN_FEE_RATE, estimated: false, allow }
    }

    /// The most a transaction of `mass` may pay.
    pub fn limit(&self, mass: u64) -> u64 {
        FACTOR.saturating_mul(self.rate).saturating_mul(mass)
    }

    /// The finding when `fee` for `mass` is above the ceiling (none with `--allow-high-fee`).
    pub fn violation(&self, fee: u64, mass: u64) -> Option<String> {
        if fee > max_fee() {
            return Some(format!(
                "the fee {fee} sompi ({} KAS) is above the most any transaction may pay, {} KAS ({CODE_ABSOLUTE}; no node estimate raises it): pass --max-fee to allow more",
                crate::common::kas(fee),
                crate::common::kas(max_fee()),
            ));
        }
        let limit = self.limit(mass);
        if self.allow || fee <= limit {
            return None;
        }
        Some(format!(
            "the fee {fee} sompi ({} KAS) is above the ceiling {limit} sompi ({} KAS): {FACTOR} x {} sompi per gram ({}) on {mass} grams; pass --allow-high-fee to pay it",
            crate::common::kas(fee),
            crate::common::kas(limit),
            self.rate,
            if self.estimated { "the node's fee estimate" } else { "the relay floor, the node gave no estimate" },
        ))
    }

    /// [`Ceiling::violation`] of a built transaction.
    pub fn violation_of(&self, built: &BuiltTx) -> Option<String> {
        let m = &built.fee.mass;
        // The mass the fee is charged on: the relay fee on compute and transient mass, the priority fee on the storage-inclusive one.
        let mass = match built.fee.fee_mode {
            kob_protocol::tx::FeeMode::Relay => m.fee_mass,
            kob_protocol::tx::FeeMode::Priority => m.priority_mass.max(m.fee_mass),
        };
        self.violation(built.fee.fee, mass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer(priority: f64, normal: f64) -> Value {
        json!({"estimate": {
            "priorityBucket": {"feerate": priority, "estimatedSeconds": 0.5},
            "normalBuckets": [{"feerate": normal, "estimatedSeconds": 20.0}],
            "lowBuckets": [{"feerate": 100.0, "estimatedSeconds": 3000.0}]
        }})
    }

    #[test]
    fn the_ceiling_is_four_times_the_estimate_at_least_the_floor() {
        let c = Ceiling::from_answer(None, false);
        assert_eq!(c.limit(1_000), 400_000);
        assert!(c.violation(400_000, 1_000).is_none());
        let e = c.violation(400_001, 1_000).unwrap();
        assert!(e.contains("--allow-high-fee") && e.contains("relay floor"), "{e}");
        // The higher of the priority and the normal bucket, rounded up.
        let c = Ceiling::from_answer(Some(&answer(250.2, 140.0)), false);
        assert_eq!(c.limit(1_000), 4 * 251 * 1_000);
        // An estimate below the floor does not lower the ceiling.
        assert_eq!(Ceiling::from_answer(Some(&answer(50.0, 40.0)), false).limit(10), 4_000);
        // Nonsense counts as no estimate.
        assert_eq!(Ceiling::from_answer(Some(&json!({"estimate": {}})), false).limit(10), 4_000);
    }

    #[test]
    fn allow_high_fee_lifts_it() {
        let c = Ceiling::from_answer(None, true);
        assert!(c.violation(FEE_ABSOLUTE_MAX, 1).is_none());
        // And asks no node.
        assert!(Ceiling::read("ws://127.0.0.1:1", true).violation(FEE_ABSOLUTE_MAX, 1).is_none());
        // It never lifts the absolute ceiling (R9FC-2): only --max-fee moves that.
        let e = c.violation(FEE_ABSOLUTE_MAX + 1, 1).unwrap();
        assert!(e.contains("--max-fee") && e.contains(CODE_ABSOLUTE), "{e}");
        set_max_fee(2 * FEE_ABSOLUTE_MAX);
        assert!(c.violation(FEE_ABSOLUTE_MAX + 1, 1).is_none());
        set_max_fee(FEE_ABSOLUTE_MAX);
    }
}
