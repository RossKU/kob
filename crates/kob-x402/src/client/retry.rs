//! Safe retry of one x402 payment (`docs/spec/x402-retry.md`): the same rules as the TS SDK's `KobX402Client`
//! (`packages/kob-x402/src/retry.ts`), for a Rust payer that brings its own transport (HTTP to a merchant, or straight
//! to a facilitator's `/settle`).
//!
//! A failed attempt is followed by one of three steps ([`Next`]):
//!
//! * [`Next::Resend`]: the outcome is unknown (no answer, a timeout, a 5xx, `settlement_pending`, `node_unavailable`) or the
//!   same signed payment may still go through later (`rate_limited`, `intent_not_executable`). The SAME signed payment is
//!   sent again: same payment id, same transaction. The facilitator answers an identical retry from its ledger (the cached
//!   settlement, `extensions.kob.replayed = true`), so a payment that went through while its answer was lost is found, never
//!   paid again.
//! * [`Next::Rebuild`]: the facilitator reported the attempt dead and released its inputs (`order_conflict`: an order was
//!   taken by someone else; the node refused the transaction; the authorization expired). A NEW attempt is built from the
//!   chain state of the moment (a fresh quote for a swap), held again to every limit of the payer and preflighted, under a
//!   fresh payment id.
//! * [`Next::Stop`]: retrying cannot help (a policy refusal, an invalid offer, a payment id conflict, an unknown diagnostic).
//!
//! **No double payment.** Every attempt of one payment spends the same *anchor*: one payer-owned input of the first attempt.
//! Two transactions that spend one outpoint exclude each other on chain, so at most one attempt of a payment can ever be
//! accepted, whatever a merchant does with the attempts it holds, and the facilitator's ledger refuses a second attempt
//! while the first one's anchor is reserved. A rebuild happens only while the anchor is still unspent (an anchor that is
//! gone may have been spent by an earlier attempt that went through: the driver stops, the caller reconciles), and a rebuilt
//! attempt that does not spend the anchor is never sent.
//!
//! Bounds ([`RetryPolicy`]): at most `max_attempts` signed attempts (default 3), at most `max_resends` re-sends of each
//! (default 4), exponential backoff with jitter between steps, and an overall deadline: the shorter of `budget_ms` and the
//! offer's `maxTimeoutSeconds`.

use std::collections::BTreeSet;

use crate::chain::Outpoint;
use crate::error::X402Error;
use crate::wire::SettlementResponse;

/// What follows a failed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Send the same signed payment again (its outcome is unknown, or it may still succeed as it is).
    Resend,
    /// The attempt is dead: build a new one from the current chain state (anchored, limits re-checked).
    Rebuild,
    /// Retrying cannot succeed.
    Stop,
}

impl Next {
    pub fn as_str(self) -> &'static str {
        match self {
            Next::Resend => "resend",
            Next::Rebuild => "rebuild",
            Next::Stop => "stop",
        }
    }
}

/// Diagnostics after which the same signed payment is sent again.
pub const RESEND_DIAGNOSTICS: &[&str] =
    &["settlement_pending", "node_unavailable", "rate_limited", "intent_not_executable", "invoice_pending"];

/// Diagnostics of an attempt the facilitator failed (its inputs released) that a new attempt may get past.
pub const REBUILD_DIAGNOSTICS: &[&str] = &[
    "order_conflict",
    "order_not_spendable",
    "invalid_kaspa_exact_utxo",
    "invalid_kaspa_exact_transaction",
    "invalid_kaspa_exact_fee",
    "invalid_kaspa_exact_mass",
    "expired_authorization",
    "expired",
];

/// The step after a facilitator / merchant failure with `diagnostic` (`extensions.kaspa.diagnostic`) and its `retryable`
/// flag. `internal` follows the flag (the operator's pause is retryable: resend); anything not listed stops.
pub fn classify(diagnostic: &str, retryable: bool) -> Next {
    if RESEND_DIAGNOSTICS.contains(&diagnostic) {
        Next::Resend
    } else if REBUILD_DIAGNOSTICS.contains(&diagnostic) {
        Next::Rebuild
    } else if diagnostic == "internal" && retryable {
        Next::Resend
    } else {
        Next::Stop
    }
}

/// The step after an HTTP answer to a paid request (a merchant's paywall or a facilitator) that is not a verified success.
/// `status` 0 means no answer at all (connection error, timeout). No answer, a 5xx and a 2xx without a settlement leave the
/// outcome unknown: resend. A `402` (corrective challenge) and a `409` follow the diagnostic; any other 4xx stops.
pub fn classify_status(status: u16, diagnostic: Option<&str>, retryable: bool) -> Next {
    match status {
        0 => Next::Resend,
        200..=299 => Next::Resend,
        402 | 409 => diagnostic.map_or(Next::Stop, |d| classify(d, retryable)),
        429 => Next::Resend,
        500..=599 => match diagnostic {
            Some(d) if classify(d, retryable) == Next::Stop && d != "internal" => Next::Stop,
            _ => Next::Resend,
        },
        _ => Next::Stop,
    }
}

/// Bounds of the retry of one payment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Signed attempts at most, the first included (1 = never rebuild).
    pub max_attempts: u32,
    /// Re-sends of one signed attempt at most (0 = never re-send).
    pub max_resends: u32,
    /// First backoff step (ms); it doubles per step up to `max_delay_ms`.
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    /// Overall time budget (ms) from the first send; further capped by the offer's `maxTimeoutSeconds`.
    pub budget_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy { max_attempts: 3, max_resends: 4, base_delay_ms: 500, max_delay_ms: 8_000, budget_ms: 120_000 }
    }
}

impl RetryPolicy {
    /// One attempt, no re-send: the behaviour without retry.
    pub fn none() -> Self {
        RetryPolicy { max_attempts: 1, max_resends: 0, ..Self::default() }
    }

    /// Backoff before step `n` (1-based), "equal jitter": half of the exponential step fixed, half scaled by `unit` (a
    /// uniform draw in `[0, 1)`), so parallel payers spread out and no step is shorter than half its base.
    pub fn delay_ms(&self, n: u32, unit: f64) -> u64 {
        let exp = self.base_delay_ms.saturating_mul(1u64 << n.saturating_sub(1).min(20)).min(self.max_delay_ms);
        let unit = if unit.is_finite() { unit.clamp(0.0, 1.0) } else { 0.5 };
        exp / 2 + ((exp - exp / 2) as f64 * unit) as u64
    }

    /// The deadline (unix ms) of a payment that starts at `start_ms` for an offer of `max_timeout_seconds`.
    pub fn deadline_ms(&self, start_ms: u64, max_timeout_seconds: u64) -> u64 {
        start_ms.saturating_add(self.budget_ms.min(max_timeout_seconds.saturating_mul(1000)))
    }
}

/// The anchor of a payment: the first input of the first attempt that the payer owns (`payer_owned`: the payer's own KAS
/// and token UTXOs it built from; order inputs are not the payer's). `None` when it spends none (nothing ties a rebuilt
/// attempt to it: such a payment is only ever re-sent, never rebuilt).
pub fn choose_anchor(consumed: &[Outpoint], payer_owned: &BTreeSet<Outpoint>) -> Option<Outpoint> {
    consumed.iter().find(|o| payer_owned.contains(o)).copied()
}

/// One signed attempt as the driver sees it.
#[derive(Clone, Debug)]
pub struct Built<P> {
    /// The transport's signed payment (a `PaymentPayload`, an HTTP request, ...).
    pub payment: P,
    pub payment_id: String,
    pub transaction_id: String,
    /// Every outpoint the transaction spends.
    pub consumed: Vec<Outpoint>,
    /// Which of them are the payer's own (KAS and token UTXOs; not the orders it fills).
    pub payer_inputs: Vec<Outpoint>,
}

/// The answer to one send.
#[derive(Clone, Debug)]
pub enum Sent {
    /// A verified success for exactly this attempt (the transport checks the settlement against what it signed).
    Settled(SettlementResponse),
    /// A failure with its diagnostic; `next` overrides the classification (an HTTP transport passes
    /// [`classify_status`]); `None` classifies `diagnostic` with [`classify`].
    Failed { diagnostic: String, retryable: bool, message: String, next: Option<Next> },
    /// No usable answer: the outcome is unknown.
    Unknown(String),
}

/// What the driver needs from the payer and its transport.
pub trait RetryTransport {
    type Payment: Clone;
    /// Builds and signs attempt `n` (1-based) from the chain state of the moment: a fresh quote, the payer's limits and the
    /// preflight run again. For `n > 1`, `anchor` names the outpoint the attempt must spend (the driver checks it). An error
    /// is final (a limit, a refused approval, no funds): it is never retried.
    fn build(&mut self, n: u32, anchor: Option<&Outpoint>) -> Result<Built<Self::Payment>, X402Error>;
    /// Sends one signed attempt (the first time, or again: the identical payment).
    fn send(&mut self, attempt: &Built<Self::Payment>) -> Sent;
    /// Which of `outpoints` are unspent now on the node (in the UTXO set and not spent by a mempool transaction).
    fn unspent(&mut self, outpoints: &[Outpoint]) -> Result<BTreeSet<Outpoint>, X402Error>;
    fn now_ms(&mut self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
    /// A uniform draw in `[0, 1)` for the jitter (default 0.5: no jitter).
    fn jitter(&mut self) -> f64 {
        0.5
    }
}

/// Why the driver gave up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiveUp {
    /// The last failure cannot be retried (see [`classify`]).
    Stopped,
    /// `max_attempts` signed attempts failed, or the last one was re-sent `max_resends` times.
    Exhausted,
    /// The overall deadline would pass before the next step.
    Deadline,
    /// The anchor is no longer unspent: an earlier attempt may have been accepted. Nothing was rebuilt; reconcile the attempts.
    AnchorSpent,
    /// The rebuilt attempt does not spend the anchor (the payer's coins changed): it was not sent.
    Unanchored,
    /// The first attempt spends no payer-owned input: it can be re-sent, never rebuilt.
    NoAnchor,
    /// Building an attempt failed (a limit, a refused approval, no funds, the preflight).
    Build,
}

/// The successful end of a retried payment.
#[derive(Clone, Debug)]
pub struct Receipt<P> {
    /// The attempt that paid.
    pub attempt: Built<P>,
    pub settlement: SettlementResponse,
    /// Signed attempts (1 = the first one paid).
    pub attempts: u32,
    /// Re-sends over all attempts.
    pub resends: u32,
    /// Payment ids of the earlier attempts: dead now (the paying attempt spent their anchor).
    pub superseded: Vec<String>,
    pub anchor: Option<Outpoint>,
}

/// The driver gave up. `outcome_unknown` is set when the last attempt's outcome was never learnt (it may still be
/// settled: reconcile it with an identical re-send, or revoke it).
#[derive(Clone, Debug)]
pub struct RetryError<P> {
    pub why: GiveUp,
    /// The last failure (diagnostic and message), or the build error.
    pub last: X402Error,
    pub attempts: u32,
    pub resends: u32,
    pub outcome_unknown: bool,
    /// Every attempt that was signed, the last one last (keep them: they are signed payments a merchant may hold).
    pub signed: Vec<Built<P>>,
    pub anchor: Option<Outpoint>,
}

impl<P> std::fmt::Display for RetryError<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "x402 payment gave up ({:?}) after {} attempt(s) and {} re-send(s): {}",
            self.why, self.attempts, self.resends, self.last
        )
    }
}

fn state(diag: crate::error::Diag, msg: impl Into<String>) -> X402Error {
    X402Error::state(diag, msg)
}

/// The remote failure as an error of this crate (an unknown diagnostic becomes `internal`, its spelling kept in `details`).
fn last_of(diagnostic: &str, retryable: bool, message: &str) -> X402Error {
    let d = crate::error::Diag::parse(diagnostic);
    let mut e = X402Error::state(d.unwrap_or(crate::error::Diag::Internal), message.to_string());
    e.retryable = retryable;
    if d.is_none() {
        e.details = Some(serde_json::json!({ "diagnostic": diagnostic }));
    }
    e
}

/// Pays with retry: builds the first attempt, sends it, and resends / rebuilds per [`classify`] within `policy`. See the
/// module docs for the guarantees.
pub fn pay_with_retry<T: RetryTransport>(
    policy: &RetryPolicy,
    max_timeout_seconds: u64,
    t: &mut T,
) -> Result<Receipt<T::Payment>, RetryError<T::Payment>> {
    use crate::error::Diag;
    let start = t.now_ms();
    let deadline = policy.deadline_ms(start, max_timeout_seconds);
    let mut signed: Vec<Built<T::Payment>> = Vec::new();
    let mut resends_total = 0u32;
    let give_up = |why: GiveUp, last: X402Error, signed: Vec<Built<T::Payment>>, resends: u32, unknown: bool, anchor| {
        Err(RetryError { why, last, attempts: signed.len() as u32, resends, outcome_unknown: unknown, signed, anchor })
    };

    let first = match t.build(1, None) {
        Ok(b) => b,
        Err(e) => return give_up(GiveUp::Build, e, signed, 0, false, None),
    };
    let owned: BTreeSet<Outpoint> = first.payer_inputs.iter().copied().collect();
    let anchor = choose_anchor(&first.consumed, &owned);
    signed.push(first);
    let mut resends = 0u32;
    loop {
        let cur = signed.last().expect("one attempt at least").clone();
        let (next, last, unknown) = match t.send(&cur) {
            Sent::Settled(s) => {
                let superseded = signed[..signed.len() - 1].iter().map(|b| b.payment_id.clone()).collect();
                return Ok(Receipt {
                    attempts: signed.len() as u32,
                    resends: resends_total,
                    attempt: cur,
                    settlement: s,
                    superseded,
                    anchor,
                });
            }
            Sent::Failed { diagnostic, retryable, message, next } => {
                let n = next.unwrap_or_else(|| classify(&diagnostic, retryable));
                (n, last_of(&diagnostic, retryable, &message), false)
            }
            Sent::Unknown(m) => (Next::Resend, state(Diag::NodeUnavailable, m).retryable(), true),
        };
        match next {
            Next::Stop => return give_up(GiveUp::Stopped, last, signed, resends_total, unknown, anchor),
            Next::Resend => {
                if resends >= policy.max_resends {
                    return give_up(GiveUp::Exhausted, last, signed, resends_total, unknown, anchor);
                }
                resends += 1;
                let wait = policy.delay_ms(resends, t.jitter());
                if t.now_ms().saturating_add(wait) >= deadline {
                    return give_up(GiveUp::Deadline, last, signed, resends_total, unknown, anchor);
                }
                t.sleep_ms(wait);
                resends_total += 1;
            }
            Next::Rebuild => {
                if signed.len() as u32 >= policy.max_attempts {
                    return give_up(GiveUp::Exhausted, last, signed, resends_total, false, anchor);
                }
                let Some(a) = anchor else { return give_up(GiveUp::NoAnchor, last, signed, resends_total, false, anchor) };
                let wait = policy.delay_ms(signed.len() as u32, t.jitter());
                if t.now_ms().saturating_add(wait) >= deadline {
                    return give_up(GiveUp::Deadline, last, signed, resends_total, false, anchor);
                }
                t.sleep_ms(wait);
                // the earlier attempts are dead only while the anchor is unspent: one that is gone may have paid
                match t.unspent(&[a]) {
                    Ok(u) if u.contains(&a) => {}
                    Ok(_) => {
                        let e = state(
                            Diag::Replay,
                            format!("the anchor {a} of this payment is spent: an earlier attempt may have been accepted; nothing was rebuilt"),
                        );
                        return give_up(GiveUp::AnchorSpent, e, signed, resends_total, true, anchor);
                    }
                    Err(e) => return give_up(GiveUp::AnchorSpent, e, signed, resends_total, true, anchor),
                }
                let n = signed.len() as u32 + 1;
                let b = match t.build(n, Some(&a)) {
                    Ok(b) => b,
                    Err(e) => return give_up(GiveUp::Build, e, signed, resends_total, false, anchor),
                };
                if !b.consumed.contains(&a) {
                    let e = state(
                        Diag::Replay,
                        format!("the rebuilt attempt does not spend the anchor {a} of this payment; it was not sent"),
                    );
                    return give_up(GiveUp::Unanchored, e, signed, resends_total, false, anchor);
                }
                signed.push(b);
                resends = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn classification_table() {
        // every listed spelling is a diagnostic of this crate
        for d in RESEND_DIAGNOSTICS.iter().chain(REBUILD_DIAGNOSTICS) {
            assert!(crate::error::Diag::parse(d).is_some(), "{d}");
        }
        for d in RESEND_DIAGNOSTICS {
            assert_eq!(classify(d, false), Next::Resend, "{d}");
        }
        for d in REBUILD_DIAGNOSTICS {
            assert_eq!(classify(d, false), Next::Rebuild, "{d}");
        }
        for d in [
            "kaspa_payment_identifier_conflict",
            "underpayment",
            "invalid_kaspa_x402_accepted",
            "replay",
            "unknown_thing",
            "intent_expired",
        ] {
            assert_eq!(classify(d, true), Next::Stop, "{d}");
        }
        assert_eq!(classify("internal", true), Next::Resend);
        assert_eq!(classify("internal", false), Next::Stop);
        assert_eq!(classify_status(0, None, false), Next::Resend);
        assert_eq!(classify_status(503, Some("node_unavailable"), true), Next::Resend);
        assert_eq!(classify_status(502, None, false), Next::Resend);
        assert_eq!(classify_status(502, Some("unauthorized"), false), Next::Stop);
        assert_eq!(classify_status(409, Some("settlement_pending"), true), Next::Resend);
        assert_eq!(classify_status(409, Some("kaspa_payment_identifier_conflict"), false), Next::Stop);
        assert_eq!(classify_status(402, Some("order_conflict"), true), Next::Rebuild);
        assert_eq!(classify_status(402, None, false), Next::Stop);
        assert_eq!(classify_status(413, None, false), Next::Stop);
        assert_eq!(classify_status(200, None, false), Next::Resend);
    }

    #[test]
    fn backoff_grows_with_equal_jitter_and_caps() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_ms(1, 0.0), 250);
        assert_eq!(p.delay_ms(1, 0.999_999), 499);
        assert_eq!(p.delay_ms(2, 0.5), 750);
        assert_eq!(p.delay_ms(10, 1.0), 8_000);
        assert_eq!(p.delay_ms(40, 0.0), 4_000);
        assert_eq!(p.deadline_ms(1_000, 60), 61_000);
        assert_eq!(p.deadline_ms(1_000, 600), 121_000);
    }

    fn op(n: u8) -> Outpoint {
        Outpoint::new([n; 32], 0)
    }

    /// A scripted transport: each build takes the next payer inputs, each send the next answer.
    struct Script {
        builds: VecDeque<Result<Vec<Outpoint>, X402Error>>,
        answers: VecDeque<Sent>,
        unspent: BTreeSet<Outpoint>,
        now: u64,
        sent: Vec<String>,
        built: u32,
    }

    impl RetryTransport for Script {
        type Payment = ();
        fn build(&mut self, n: u32, anchor: Option<&Outpoint>) -> Result<Built<()>, X402Error> {
            assert_eq!(anchor.is_some(), n > 1);
            self.built += 1;
            let ins = self.builds.pop_front().expect("scripted build")?;
            Ok(Built {
                payment: (),
                payment_id: format!("attempt-{n}"),
                transaction_id: format!("tx-{n}"),
                consumed: ins.clone(),
                payer_inputs: ins,
            })
        }
        fn send(&mut self, a: &Built<()>) -> Sent {
            self.sent.push(a.payment_id.clone());
            self.answers.pop_front().expect("scripted answer")
        }
        fn unspent(&mut self, of: &[Outpoint]) -> Result<BTreeSet<Outpoint>, X402Error> {
            Ok(of.iter().filter(|o| self.unspent.contains(o)).copied().collect())
        }
        fn now_ms(&mut self) -> u64 {
            self.now
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.now += ms;
        }
    }

    fn ok() -> Sent {
        Sent::Settled(SettlementResponse {
            success: true,
            error_reason: None,
            transaction: "tx".into(),
            network: None,
            payer: None,
            amount: None,
            extensions: None,
        })
    }

    fn failed(d: &str, retryable: bool) -> Sent {
        Sent::Failed { diagnostic: d.into(), retryable, message: d.into(), next: None }
    }

    fn script(builds: Vec<Vec<Outpoint>>, answers: Vec<Sent>) -> Script {
        Script {
            builds: builds.into_iter().map(Ok).collect(),
            answers: answers.into(),
            unspent: [op(1), op(2), op(3)].into_iter().collect(),
            now: 0,
            sent: vec![],
            built: 0,
        }
    }

    #[test]
    fn a_lost_order_is_rebuilt_once_and_anchored() {
        let mut s = script(vec![vec![op(1), op(2)], vec![op(1), op(3)]], vec![failed("order_conflict", true), ok()]);
        let r = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap();
        assert_eq!((r.attempts, r.resends), (2, 0));
        assert_eq!(r.anchor, Some(op(1)));
        assert_eq!(r.superseded, vec!["attempt-1".to_string()]);
        assert_eq!(s.sent, vec!["attempt-1", "attempt-2"]);
    }

    #[test]
    fn an_unknown_outcome_is_resent_never_rebuilt() {
        let mut s = script(vec![vec![op(1)]], vec![Sent::Unknown("timeout".into()), failed("settlement_pending", true), ok()]);
        let r = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap();
        assert_eq!((r.attempts, r.resends, s.built), (1, 2, 1));
        assert_eq!(s.sent, vec!["attempt-1"; 3]);
    }

    #[test]
    fn a_spent_anchor_stops_before_rebuilding() {
        let mut s = script(vec![vec![op(1)]], vec![failed("invalid_kaspa_exact_utxo", false)]);
        s.unspent.remove(&op(1));
        let e = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap_err();
        assert_eq!((e.why, e.attempts, s.built), (GiveUp::AnchorSpent, 1, 1));
        assert!(e.outcome_unknown);
    }

    #[test]
    fn an_unanchored_rebuild_is_not_sent() {
        let mut s = script(vec![vec![op(1)], vec![op(3)]], vec![failed("order_conflict", true)]);
        let e = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap_err();
        assert_eq!(e.why, GiveUp::Unanchored);
        assert_eq!(s.sent.len(), 1);
    }

    #[test]
    fn gives_up_after_max_attempts_and_on_stop_and_on_build_errors() {
        let mut s = script(vec![vec![op(1)], vec![op(1)], vec![op(1)]], vec![failed("invalid_kaspa_exact_transaction", false); 3]);
        let e = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap_err();
        assert_eq!((e.why, e.attempts, e.signed.len()), (GiveUp::Exhausted, 3, 3));
        assert_eq!(s.sent.len(), 3);

        let mut s = script(vec![vec![op(1)]], vec![failed("kaspa_payment_identifier_conflict", false)]);
        let e = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap_err();
        assert_eq!((e.why, s.sent.len()), (GiveUp::Stopped, 1));

        // a worse re-quote above the payer's limit: the build refuses, nothing more is sent
        let mut s = script(vec![vec![op(1)]], vec![failed("order_conflict", true)]);
        s.builds.push_back(Err(X402Error::requirements(crate::error::Diag::Overpayment, "above maxPay")));
        let e = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap_err();
        assert_eq!((e.why, s.sent.len()), (GiveUp::Build, 1));
    }

    #[test]
    fn resends_and_the_deadline_are_bounded() {
        let mut s = script(vec![vec![op(1)]], vec![Sent::Unknown("down".into()); 10]);
        let e = pay_with_retry(&RetryPolicy::default(), 600, &mut s).unwrap_err();
        assert_eq!((e.why, s.sent.len()), (GiveUp::Exhausted, 5));
        assert!(e.outcome_unknown);
        // a 2 s offer: the backoff steps run into the deadline first
        let mut s = script(vec![vec![op(1)]], vec![Sent::Unknown("down".into()); 10]);
        let e = pay_with_retry(&RetryPolicy::default(), 2, &mut s).unwrap_err();
        assert_eq!(e.why, GiveUp::Deadline);
        assert!(s.now < 2_000);
        // no retry at all
        let mut s = script(vec![vec![op(1)]], vec![failed("order_conflict", true)]);
        let e = pay_with_retry(&RetryPolicy::none(), 60, &mut s).unwrap_err();
        assert_eq!((e.why, s.sent.len()), (GiveUp::Exhausted, 1));
    }
}
