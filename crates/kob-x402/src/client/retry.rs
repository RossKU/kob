//! Safe retry of one x402 payment, for a Rust payer that brings its own transport.
//!
//! The transport can be HTTP to a merchant or a direct call to a facilitator's `/settle`. The rules
//! are the ones in `docs/spec/x402-retry.md`. They match the TS SDK's `retry.ts`.
//!
//! # Steps
//!
//! A failed attempt is followed by one of three steps ([`Next`]). The diagnostic table is in
//! section 1 of the spec.
//!
//! - [`Next::Resend`] sends the SAME signed payment again, with the same payment id and the same
//!   transaction. It applies when the outcome is unknown or the payment may still go through. An
//!   identical retry is answered from the facilitator's ledger, so a payment whose answer was lost
//!   is found and never paid twice.
//! - [`Next::Rebuild`] builds a NEW attempt from the chain state of the moment, under a fresh
//!   payment id. It applies when the facilitator reported the attempt dead and released its inputs.
//!   The new attempt is held again to every limit of the payer and preflighted.
//! - [`Next::Stop`] ends the retry because retrying cannot help.
//!
//! # No double payment
//!
//! Every attempt of one payment spends the same anchor, which is one payer-owned input of the first
//! attempt. Two transactions that spend one outpoint exclude each other on chain, so at most one
//! attempt of a payment can ever be accepted. A rebuild happens only while the anchor is unspent,
//! and a rebuilt attempt that does not spend the anchor is never sent. Spec section 2 gives the
//! full argument.
//!
//! # Bounds
//!
//! [`RetryPolicy`] bounds the signed attempts (`max_attempts`, default 3), the re-sends of each
//! attempt (`max_resends`, default 4) and the backoff between steps. The overall deadline is the
//! shorter of `budget_ms` and the offer's `maxTimeoutSeconds`. Spec section 4 lists the defaults.
//!
//! # Intent payments
//!
//! [`pay_intent_with_retry`] and [`ResendTransport`] cover intent payments. The payer signs one
//! creation and the facilitator re-plans the execution, so the creation is only re-sent, within
//! the intent's own deadline. A failure that would need a new signature ends the retry with
//! [`GiveUp::NoRebuild`]. Spec section 5 has the details.

use std::collections::BTreeSet;

use crate::chain::Outpoint;
use crate::error::X402Error;
use crate::wire::SettlementResponse;

/// What follows a failed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Send the same signed payment again.
    ///
    /// Its outcome is unknown, or it may still succeed as it is.
    Resend,
    /// The attempt is dead. Build a new one from the current chain state.
    ///
    /// The new attempt spends the anchor and every limit is checked again.
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

/// Diagnostics of an attempt the facilitator failed and released that a new attempt may get past.
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

/// The step after a facilitator or merchant failure with `diagnostic` and its `retryable` flag.
///
/// The diagnostic is `extensions.kaspa.diagnostic`. `internal` follows the flag, because the
/// operator's pause is retryable and gets a resend. Anything not listed stops.
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

/// The step after an HTTP answer to a paid request that is not a verified success.
///
/// The answer comes from a merchant's paywall or a facilitator. `status` 0 means no answer at all,
/// such as a connection error or a timeout. No answer, a 5xx and a 2xx without a settlement leave
/// the outcome unknown, so they resend. A `402` (corrective challenge) and a `409` follow the
/// diagnostic. Any other 4xx stops.
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
    /// Overall time budget (ms) from the first send, capped by the offer's `maxTimeoutSeconds`.
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

    /// Backoff before step `n` (1-based), with "equal jitter".
    ///
    /// Half of the exponential step is fixed and half is scaled by `unit`, a uniform draw in
    /// `[0, 1)`. Parallel payers spread out, and no step is shorter than half its base.
    pub fn delay_ms(&self, n: u32, unit: f64) -> u64 {
        let exp = self.base_delay_ms.saturating_mul(1u64 << n.saturating_sub(1).min(20)).min(self.max_delay_ms);
        let unit = if unit.is_finite() { unit.clamp(0.0, 1.0) } else { 0.5 };
        exp / 2 + ((exp - exp / 2) as f64 * unit) as u64
    }

    /// The deadline (unix ms) of a payment that starts at `start_ms` for an offer of
    /// `max_timeout_seconds`.
    pub fn deadline_ms(&self, start_ms: u64, max_timeout_seconds: u64) -> u64 {
        start_ms.saturating_add(self.budget_ms.min(max_timeout_seconds.saturating_mul(1000)))
    }
}

/// The anchor of a payment: the first input of the first attempt that the payer owns.
///
/// `payer_owned` holds the payer's own KAS and token UTXOs it built from. Order inputs are not the
/// payer's. The result is `None` when the attempt spends no such input. Nothing ties a rebuilt
/// attempt to the first one then, so such a payment is only re-sent, never rebuilt.
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
    /// A verified success for exactly this attempt (the transport checks the settlement against
    /// what it signed).
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
    /// Builds and signs attempt `n` (1-based) from the chain state of the moment.
    ///
    /// A swap gets a fresh quote, and the payer's limits and the preflight run again. For `n > 1`,
    /// `anchor` names the outpoint the attempt must spend, and the driver checks it. An error is
    /// final (a limit, a refused approval, no funds) and is never retried.
    fn build(&mut self, n: u32, anchor: Option<&Outpoint>) -> Result<Built<Self::Payment>, X402Error>;
    /// Sends one signed attempt (the first time, or again: the identical payment).
    fn send(&mut self, attempt: &Built<Self::Payment>) -> Sent;
    /// Which of `outpoints` are unspent now on the node (in the UTXO set and not spent by a mempool
    /// transaction).
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
    /// The anchor has been spent, so an earlier attempt may have been accepted.
    ///
    /// Nothing is rebuilt. Reconcile the attempts.
    AnchorSpent,
    /// The rebuilt attempt does not spend the anchor (the payer's coins changed): it was not sent.
    Unanchored,
    /// The first attempt spends no payer-owned input: it can be re-sent, never rebuilt.
    NoAnchor,
    /// An intent creation ([`pay_intent_with_retry`]) failed in a way only a new signature could
    /// get past.
    ///
    /// It is never re-signed, because a second creation would be a second intent. Cancel the intent
    /// or let it expire.
    NoRebuild,
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
    /// Payment ids of the earlier attempts. They are dead because the paying attempt spent their
    /// anchor.
    pub superseded: Vec<String>,
    pub anchor: Option<Outpoint>,
}

/// The driver gave up.
///
/// `outcome_unknown` is set when the last attempt's outcome was never learnt. That attempt may
/// still be settled. Reconcile it with an identical re-send, or revoke it.
#[derive(Clone, Debug)]
pub struct RetryError<P> {
    pub why: GiveUp,
    /// The last failure (diagnostic and message), or the build error.
    pub last: X402Error,
    pub attempts: u32,
    pub resends: u32,
    pub outcome_unknown: bool,
    /// Every attempt that was signed, the last one last.
    ///
    /// Keep them, because they are signed payments a merchant may hold.
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

/// The remote failure as an error of this crate.
///
/// An unknown diagnostic becomes `internal`, and its spelling is kept in `details`.
fn last_of(diagnostic: &str, retryable: bool, message: &str) -> X402Error {
    let d = crate::error::Diag::parse(diagnostic);
    let mut e = X402Error::state(d.unwrap_or(crate::error::Diag::Internal), message.to_string());
    e.retryable = retryable;
    if d.is_none() {
        e.details = Some(serde_json::json!({ "diagnostic": diagnostic }));
    }
    e
}

/// Pays with retry: builds the first attempt, sends it, and resends or rebuilds per [`classify`].
///
/// The steps stay within `policy`. See the module docs for the guarantees.
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
                // The earlier attempts are dead only while the anchor is unspent. A gone anchor may
                // have paid.
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

// --------------------------------------------------------------------------------- intent payments

/// What [`pay_intent_with_retry`] needs: the transport of ONE signed intent creation.
///
/// The creation is never rebuilt, so nothing is built here and no anchor is read.
pub trait ResendTransport {
    type Payment: Clone;
    /// Sends the signed creation (the first time, or again: the identical payment).
    fn send(&mut self, attempt: &Built<Self::Payment>) -> Sent;
    fn now_ms(&mut self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
    /// A uniform draw in `[0, 1)` for the jitter (default 0.5: no jitter).
    fn jitter(&mut self) -> f64 {
        0.5
    }
}

/// Pays an intent (`kob-intent-v1`) with retry. See `docs/spec/x402-retry.md` section 5.
///
/// The payer signed ONE creation, `creation`, for example from
/// [`crate::client::intent::IntentPayment::attempt`]. Re-planning against the book is the
/// facilitator's job.
///
/// The creation is re-sent while its outcome is unknown or it may still execute ([`Next::Resend`]).
/// That covers no answer, `settlement_pending`, `node_unavailable`, `rate_limited` and
/// `intent_not_executable`. It is re-sent at most `max_resends` times, with backoff, and only
/// before the earliest of three limits. They are the policy's budget, the offer's
/// `maxTimeoutSeconds` and the intent's own deadline (`deadline_ms`, unix ms, its authorization's
/// expiry).
///
/// It is NEVER rebuilt. A failure that would need a new signature ([`Next::Rebuild`]) ends the
/// retry with [`GiveUp::NoRebuild`], and a refusal ends it with [`GiveUp::Stopped`].
///
/// When it gives up, the creation may still have been broadcast. Cancel the intent
/// ([`crate::client::intent::cancel_intent`]) once the facilitator no longer executes it, or let
/// the facilitator (or anyone, at its expiry) end it at its deadline.
pub fn pay_intent_with_retry<T: ResendTransport>(
    policy: &RetryPolicy,
    max_timeout_seconds: u64,
    deadline_ms: u64,
    creation: Built<T::Payment>,
    t: &mut T,
) -> Result<Receipt<T::Payment>, RetryError<T::Payment>> {
    use crate::error::Diag;
    let start = t.now_ms();
    let deadline = policy.deadline_ms(start, max_timeout_seconds).min(deadline_ms);
    let signed = vec![creation];
    let mut resends = 0u32;
    let give_up = |why: GiveUp, last: X402Error, signed: Vec<Built<T::Payment>>, resends: u32, unknown: bool| {
        Err(RetryError { why, last, attempts: 1, resends, outcome_unknown: unknown, signed, anchor: None })
    };
    loop {
        let cur = &signed[0];
        let (next, last, unknown) = match t.send(cur) {
            Sent::Settled(s) => {
                return Ok(Receipt {
                    attempt: signed.into_iter().next().expect("the creation"),
                    settlement: s,
                    attempts: 1,
                    resends,
                    superseded: vec![],
                    anchor: None,
                })
            }
            Sent::Failed { diagnostic, retryable, message, next } => {
                let n = next.unwrap_or_else(|| classify(&diagnostic, retryable));
                (n, last_of(&diagnostic, retryable, &message), false)
            }
            Sent::Unknown(m) => (Next::Resend, state(Diag::NodeUnavailable, m).retryable(), true),
        };
        match next {
            Next::Stop => return give_up(GiveUp::Stopped, last, signed, resends, unknown),
            Next::Rebuild => return give_up(GiveUp::NoRebuild, last, signed, resends, false),
            Next::Resend => {
                if resends >= policy.max_resends {
                    return give_up(GiveUp::Exhausted, last, signed, resends, unknown);
                }
                let wait = policy.delay_ms(resends + 1, t.jitter());
                if t.now_ms().saturating_add(wait) >= deadline {
                    return give_up(GiveUp::Deadline, last, signed, resends, unknown);
                }
                t.sleep_ms(wait);
                resends += 1;
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
        // Every listed spelling is a diagnostic of this crate.
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

        // A worse re-quote above the payer's limit: the build refuses and nothing more is sent.
        let mut s = script(vec![vec![op(1)]], vec![failed("order_conflict", true)]);
        s.builds.push_back(Err(X402Error::requirements(crate::error::Diag::Overpayment, "above maxPay")));
        let e = pay_with_retry(&RetryPolicy::default(), 60, &mut s).unwrap_err();
        assert_eq!((e.why, s.sent.len()), (GiveUp::Build, 1));
    }

    /// A scripted transport of one intent creation.
    struct Resends {
        answers: VecDeque<Sent>,
        now: u64,
        sent: u32,
    }

    impl ResendTransport for Resends {
        type Payment = ();
        fn send(&mut self, a: &Built<()>) -> Sent {
            assert_eq!(a.payment_id, "creation");
            self.sent += 1;
            self.answers.pop_front().expect("scripted answer")
        }
        fn now_ms(&mut self) -> u64 {
            self.now
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.now += ms;
        }
    }

    fn creation() -> Built<()> {
        Built {
            payment: (),
            payment_id: "creation".into(),
            transaction_id: "c0".into(),
            consumed: vec![op(1)],
            payer_inputs: vec![op(1)],
        }
    }

    fn resends(answers: Vec<Sent>) -> Resends {
        Resends { answers: answers.into(), now: 0, sent: 0 }
    }

    #[test]
    fn an_intent_creation_is_re_sent_while_it_may_execute_never_rebuilt() {
        // The keeper cannot execute yet, the answer is lost and the execution is pending. Then it
        // settles. One creation throughout.
        let mut t = resends(vec![
            failed("intent_not_executable", true),
            Sent::Unknown("timeout".into()),
            failed("settlement_pending", true),
            ok(),
        ]);
        let r = pay_intent_with_retry(&RetryPolicy::default(), 600, u64::MAX, creation(), &mut t).unwrap();
        assert_eq!((r.attempts, r.resends, t.sent), (1, 3, 4));
        assert_eq!(r.attempt.payment_id, "creation");
        assert!(r.superseded.is_empty() && r.anchor.is_none());

        // A failure that would need a new signature ends it. Nothing is rebuilt.
        for d in ["order_conflict", "invalid_kaspa_exact_utxo", "expired_authorization"] {
            let mut t = resends(vec![failed(d, true)]);
            let e = pay_intent_with_retry(&RetryPolicy::default(), 600, u64::MAX, creation(), &mut t).unwrap_err();
            assert_eq!((e.why, e.attempts, e.signed.len(), t.sent), (GiveUp::NoRebuild, 1, 1, 1), "{d}");
            assert!(!e.outcome_unknown);
        }
        // A refusal stops.
        let mut t = resends(vec![failed("intent_spent", false)]);
        let e = pay_intent_with_retry(&RetryPolicy::default(), 600, u64::MAX, creation(), &mut t).unwrap_err();
        assert_eq!((e.why, t.sent), (GiveUp::Stopped, 1));
    }

    #[test]
    fn an_intent_retry_is_bounded_by_its_resends_and_its_deadline() {
        let mut t = resends(vec![failed("settlement_pending", true); 10]);
        let e = pay_intent_with_retry(&RetryPolicy::default(), 600, u64::MAX, creation(), &mut t).unwrap_err();
        assert_eq!((e.why, e.resends, t.sent), (GiveUp::Exhausted, 4, 5));
        // The intent's own deadline (its authorization's expiry) comes before the offer's and the
        // budget.
        let mut t = resends(vec![Sent::Unknown("down".into()); 10]);
        let e = pay_intent_with_retry(&RetryPolicy::default(), 600, 1_500, creation(), &mut t).unwrap_err();
        assert_eq!(e.why, GiveUp::Deadline);
        assert!(e.outcome_unknown);
        assert!(t.now < 1_500, "nothing is sent after the intent's deadline: {}", t.now);
        // no retry at all
        let mut t = resends(vec![failed("settlement_pending", true)]);
        let e = pay_intent_with_retry(&RetryPolicy::none(), 600, u64::MAX, creation(), &mut t).unwrap_err();
        assert_eq!((e.why, t.sent), (GiveUp::Exhausted, 1));
    }

    #[test]
    fn resends_and_the_deadline_are_bounded() {
        let mut s = script(vec![vec![op(1)]], vec![Sent::Unknown("down".into()); 10]);
        let e = pay_with_retry(&RetryPolicy::default(), 600, &mut s).unwrap_err();
        assert_eq!((e.why, s.sent.len()), (GiveUp::Exhausted, 5));
        assert!(e.outcome_unknown);
        // A 2 s offer: the backoff steps run into the deadline first.
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
