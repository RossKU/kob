//! The congestion note of a signing command: when the node's fee estimate shows contended blocks (the rule of
//! `kob_protocol::fee_estimate::FeeEstimate::contended`, the executor's reading of `getFeeEstimate`) and the person set no
//! `--fee-rate` and no `--priority-fee`, one line on stderr says that KOB transactions are ranked by their storage mass, which the normal fee does not cover, so they may wait, and that
//! `--priority-fee` (the web app's "Pay the priority fee") pays for it. Nothing else changes: the fee stays the relay minimum unless the person asks for more. One estimate is read per command, right before the
//! plan is shown; a node that does not answer it, or answers nonsense, gives no note.

use std::time::Duration;

use kob_protocol::fee_estimate::FeeEstimate;
use kob_protocol::tx::MIN_FEE_RATE;
use serde_json::{json, Value};

/// How long the estimate may take: it must never hold up a command.
const ESTIMATE_TIMEOUT: Duration = Duration::from_secs(4);

/// The note for a `getFeeEstimate` answer, or none when blocks are not contended or the answer is unusable.
pub fn note_for(answer: &Value) -> Option<String> {
    let est = FeeEstimate::parse(answer).ok()?;
    est.contended(MIN_FEE_RATE).then(|| {
        format!(
            "note: blocks are contended (the node estimates {} sompi per gram for the next minute). KOB transactions are ranked by their storage mass, which the normal fee does not cover, so they may wait. --priority-fee also pays for the storage mass (a higher fee, the web app's \"Pay the priority fee\"); --fee-rate N alone raises the fee, but it is still charged on the compute and transient mass only.",
            est.normal.ceil()
        )
    })
}

/// The note of `node` for a command that set `fee_rate` / `priority_fee` (or not): none when a rate or the priority fee was chosen or the node gives no estimate.
pub fn note(node: &str, fee_rate: Option<u64>, priority_fee: bool) -> Option<String> {
    if fee_rate.is_some() || priority_fee {
        return None;
    }
    crate::wrpc::call(node, "getFeeEstimate", json!({}), ESTIMATE_TIMEOUT).ok().and_then(|a| note_for(&a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(normal: f64) -> Value {
        json!({"estimate": {
            "priorityBucket": {"feerate": normal + 20.0, "estimatedSeconds": 0.5},
            "normalBuckets": [{"feerate": normal, "estimatedSeconds": 20.0}],
            "lowBuckets": [{"feerate": 100.0, "estimatedSeconds": 3000.0}]
        }})
    }

    #[test]
    fn a_contended_estimate_gives_one_note_naming_the_flag() {
        let n = note_for(&answer(140.2)).unwrap();
        assert!(n.starts_with("note: blocks are contended"), "{n}");
        assert!(n.contains("--fee-rate N"), "{n}");
        assert!(n.contains("--priority-fee"), "{n}");
        assert!(!n.contains("cannot do anything"), "{n}");
        assert!(n.contains("ranked by their storage mass"), "{n}");
        assert!(!n.contains("relay minimum"), "{n}");
        assert!(n.contains("141 sompi per gram"), "{n}");
        assert!(!n.contains('\n'));
    }

    #[test]
    fn the_threshold_is_the_executors() {
        assert!(note_for(&answer(110.0)).is_some());
        assert!(note_for(&answer(109.0)).is_none());
        assert!(note_for(&answer(100.0)).is_none());
    }

    #[test]
    fn an_unusable_answer_gives_no_note() {
        assert!(note_for(&json!({})).is_none());
        assert!(note_for(&json!({"estimate": {"priorityBucket": {"feerate": 0.0}}})).is_none());
        assert!(note_for(&Value::Null).is_none());
    }

    #[test]
    fn a_chosen_rate_or_a_node_without_the_method_gives_no_note() {
        // no connection is made when a rate was chosen
        assert!(note("ws://127.0.0.1:1", Some(150), false).is_none());
        // ... nor when the priority fee was chosen
        assert!(note("ws://127.0.0.1:1", None, true).is_none());
        // a node that errors on the method (the test mock answers "unknown method")
        let (url, state) = crate::wrpc::mock::start(vec![]);
        assert!(note(&url, None, false).is_none());
        assert!(state.lock().unwrap().requests.iter().any(|(m, _)| m == "getFeeEstimate"));
    }

    #[test]
    fn a_node_that_estimates_contended_blocks_gives_the_note_unless_a_rate_was_chosen() {
        let (url, state) = crate::wrpc::mock::start(vec![]);
        state.lock().unwrap().fee_estimate = Some(answer(194.0));
        let n = note(&url, None, false).unwrap();
        assert!(n.contains("--fee-rate N") && n.contains("--priority-fee"), "{n}");
        assert!(note(&url, Some(150), false).is_none());
        assert!(note(&url, None, true).is_none());
        state.lock().unwrap().fee_estimate = Some(answer(100.0));
        assert!(note(&url, None, false).is_none(), "a quiet network: no note");
    }
}
