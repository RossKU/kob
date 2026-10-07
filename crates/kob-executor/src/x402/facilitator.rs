//! The facilitator core: `supported`, `verify`, `settle` and `reconcile` over a trusted
//! [`ChainView`], the replay [`Ledger`] and a [`PaymentVerifier`].
//!
//! Settlement lifecycle (`kaspa-exact-v2.md`, "Settlement lifecycle"), as implemented by
//! [`Facilitator::settle`]:
//!
//! 1. `requestHash` (mandatory, from the request body) and the transaction id are read; under a
//!    per-transaction lock the ledger is consulted first: an identical retry of a `broadcast` /
//!    `ambiguous` (seen on chain) / `accepted` settlement resumes **without** re-verifying inputs that
//!    are spent by now and without rebuilding anything;
//! 2. otherwise the payment is verified against trusted chain facts (`verify_payment`);
//! 3. expiry is re-evaluated after that awaited work and before anything is created;
//! 4. the replay evidence is consumed durably (`Ledger::claim`, state `pending`);
//! 5. the exact verified transaction is submitted; the entry becomes `broadcast` when the node took it
//!    or already knew it, `failed` (evidence released) on a definitive `Conflict` / `Rejected`, and
//!    `ambiguous` (evidence kept) when the node could not be reached;
//! 6. finality is observed by polling the merchant output: `accepted` = the output is in the virtual
//!    UTXO set, `confirmed` = additionally `virtual DAA - block DAA >= confirmations_daa`. The mempool
//!    never counts. On success the entry becomes `accepted` and the response is cached; on timeout the
//!    caller gets `invalid_transaction_state` / `settlement_pending` (retryable) and the entry stays
//!    `broadcast`.
//!
//! Intent-based swap-and-pay (`kob-intent-v1`, [`intent`]): the payment transaction is the payer's intent
//! creation; the facilitator broadcasts it and then executes the intent itself against the book, as a
//! keeper, re-planning after a conflicting fill without the payer, until an execution is accepted or the
//! authorization's deadline passes. Invoices ([`invoice`]): registered by a merchant, paid once.
//!
//! Swap-and-pay: when the transaction spends KOB orders, a `Conflict` on submit, or a timeout in which
//! an order outpoint is spent while our transaction is not accepted, is `order_conflict` (retryable):
//! the entry is marked `failed` (the payer inputs are released) and the payer must re-quote and
//! re-sign; the facilitator can never rebuild a signed transaction.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_x402::canonical::requirements_hash;
use kob_x402::chain::{ChainError, ChainView, Clock, Outpoint, OutputStatus, SubmitError, Tracked, Txid};
use kob_x402::common;
use kob_x402::error::{Diag, Reason, Result, X402Error};
use kob_x402::policy::Policy;
use kob_x402::safe_tx::{spk_from_hex, spk_to_hex};
use kob_x402::verify::{PaymentKind, Verified, VerifyCtx};
use kob_x402::wire::{
    failure_extension, hex, parse_hash32, FacilitatorRequest, Finality, PaymentPayload, PaymentRequirements, Profile,
    SettlementResponse, VerifyResponse, X402_VERSION,
};
use serde_json::{json, Map, Value};

use super::ledger::{Claim, Entry, Ledger, NewEntry, State, Watched};

mod intent;
mod invoice;

pub use intent::{IntentRuntime, EXPIRY_BUMP_MS, EXPIRY_RETRY_MS};
pub use invoice::{InvoiceRecord, InvoiceRuntime, InvoiceStore};

/// Verification seam: production uses [`ProtocolVerifier`]; tests inject fakes.
pub trait PaymentVerifier: Send + Sync {
    fn verify(&self, ctx: &VerifyCtx, offered: &PaymentRequirements, payload: &PaymentPayload, request_hash: &str)
        -> Result<Verified>;
}

/// `kob_x402::verify::verify_payment`.
pub struct ProtocolVerifier;

impl PaymentVerifier for ProtocolVerifier {
    fn verify(
        &self,
        ctx: &VerifyCtx,
        offered: &PaymentRequirements,
        payload: &PaymentPayload,
        request_hash: &str,
    ) -> Result<Verified> {
        kob_x402::verify::verify_payment(ctx, offered, payload, request_hash)
    }
}

/// Operational settings of the core.
#[derive(Clone, Debug)]
pub struct FacilitatorConfig {
    /// Longest a `/settle` call observes finality (further capped by the offer's `maxTimeoutSeconds`).
    pub settle_wait: Duration,
    /// Interval between finality polls.
    pub poll_interval: Duration,
    /// Accepted entries younger than this many DAA are re-checked for reorgs by [`Facilitator::reconcile`].
    pub reorg_watch_daa: u64,
    /// While this file exists the facilitator is disabled.
    pub kill_switch_file: Option<PathBuf>,
    /// The process's pause file (`kob-executor run --pause-file`): disables the facilitator like `kill_switch_file`.
    pub pause_file: Option<PathBuf>,
    /// A `pending` entry the node has never seen (the process died before the broadcast) is failed and its outpoints released
    /// once it is this old. Until then only the identical retry resolves it. Safe: a success is only ever reported
    /// for the transaction whose own finality was observed.
    pub pending_grace: Duration,
    /// An `ambiguous` direct payment (the node was unreachable at its broadcast, or its accepted output vanished in a reorg)
    /// that the node still does not know (not in the mempool, its merchant output not on chain) this long after it became
    /// ambiguous is failed and its outpoints released (C5 X-9): otherwise its evidence, and an invoice it pays, stay
    /// `pending` forever. An invoice keeps watching it as a `released` extra payment: if it reaches the chain after all, the
    /// status reports it `accepted` (the merchant refunds it, as a late payment).
    pub ambiguous_grace: Duration,
}

impl Default for FacilitatorConfig {
    fn default() -> Self {
        FacilitatorConfig {
            settle_wait: Duration::from_secs(30),
            poll_interval: Duration::from_millis(200),
            reorg_watch_daa: 36_000,
            kill_switch_file: None,
            pause_file: None,
            pending_grace: Duration::from_secs(15 * 60),
            ambiguous_grace: Duration::from_secs(60 * 60),
        }
    }
}

macro_rules! metrics {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {
        /// Monotonic counters exposed on `GET /metrics`.
        #[derive(Default)]
        pub struct Metrics { $($(#[$m])* pub $name: AtomicU64,)* }
        impl Metrics {
            /// Prometheus-style text.
            pub fn render(&self) -> String {
                let mut s = String::new();
                $(s.push_str(&format!("kob_x402_{} {}\n", stringify!($name), self.$name.load(Ordering::Relaxed)));)*
                s
            }
        }
    };
}

metrics! {
    verify_requests,
    verify_invalid,
    settle_requests,
    settle_success,
    settle_failed,
    /// Settlements that timed out waiting for finality (retryable).
    settle_pending,
    /// Idempotent retries answered from the ledger.
    settle_resumed,
    replay_rejected,
    broadcasts,
    node_unavailable,
    order_conflicts,
    expired_rejected,
    reorgs_detected,
    reconcile_runs,
    http_unauthorized,
    http_forbidden,
    http_rate_limited,
    http_too_large,
    http_body_timeouts,
    http_bad_requests,
    http_kill_switch,
    http_busy,
    /// Intent executions submitted.
    intent_executions,
    /// Intent executions that lost an order to another fill (re-planned without the payer).
    intent_conflicts,
    /// Intents that ended without an execution (expired, spent elsewhere, not executable).
    intent_failed,
    invoices_registered,
    /// Invoice payments refused because the invoice was paid, being paid or expired (kept as evidence).
    invoice_refused,
    /// Refused invoice payments NOT kept as evidence: the invoice already holds `maxExtraPaymentsPerInvoice` of them.
    invoice_evidence_dropped,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }
}

/// Per-transaction mutual exclusion (a retry waits for the attempt in flight).
#[derive(Default)]
struct TxLocks {
    held: Mutex<HashSet<Txid>>,
    cv: Condvar,
}

struct TxGuard<'a> {
    locks: &'a TxLocks,
    txid: Txid,
}

impl TxLocks {
    fn lock(&self, txid: Txid) -> TxGuard<'_> {
        let mut g = self.held.lock().unwrap_or_else(|p| p.into_inner());
        while g.contains(&txid) {
            g = self.cv.wait(g).unwrap_or_else(|p| p.into_inner());
        }
        g.insert(txid);
        TxGuard { locks: self, txid }
    }
    fn try_lock(&self, txid: Txid) -> Option<TxGuard<'_>> {
        let mut g = self.held.lock().unwrap_or_else(|p| p.into_inner());
        if g.contains(&txid) {
            return None;
        }
        g.insert(txid);
        Some(TxGuard { locks: self, txid })
    }
}

impl Drop for TxGuard<'_> {
    fn drop(&mut self) {
        self.locks.held.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.txid);
        self.locks.cv.notify_all();
    }
}

/// Per-key mutual exclusion for invoices (one payment of an invoice is decided at a time).
#[derive(Default)]
struct KeyLocks {
    held: Mutex<HashSet<String>>,
}

struct KeyGuard<'a> {
    locks: &'a KeyLocks,
    key: String,
}

impl KeyLocks {
    /// The lock of `key`, or `None` while another caller holds it (nobody waits on it: a payment of an invoice is decided
    /// while the others are told it is pending).
    fn try_lock(&self, key: &str) -> Option<KeyGuard<'_>> {
        let mut g = self.held.lock().unwrap_or_else(|p| p.into_inner());
        if g.contains(key) {
            return None;
        }
        g.insert(key.to_string());
        Some(KeyGuard { locks: self, key: key.to_string() })
    }
}

impl Drop for KeyGuard<'_> {
    fn drop(&mut self) {
        self.locks.held.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
    }
}

/// What one pass of [`Facilitator::reconcile`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub checked: usize,
    /// Broadcast / ambiguous entries that reached their finality and became `accepted`.
    pub finalized: usize,
    /// Pending / ambiguous entries found in the mempool or on chain (now `broadcast`).
    pub seen: usize,
    /// Accepted entries whose merchant output vanished (now `ambiguous`).
    pub reorged: usize,
    /// Pending entries the chain has never seen (crash before broadcast): kept, resolved by an identical retry.
    pub unseen_pending: usize,
    /// Ambiguous entries the node did not know for `ambiguous_grace`: failed, outpoints released (C5 X-9).
    pub released_ambiguous: usize,
    /// Pending entries the chain never saw within `pending_grace`: failed, outpoints released.
    pub expired_pending: usize,
    /// Intent payments driven (executions planned, observed or re-planned).
    pub intents_driven: usize,
    /// Invoice payments refused earlier that were found on chain anyway (refund candidates).
    pub extra_payments_seen: usize,
    /// Expiries of unexecuted intents driven (built, submitted or observed).
    pub expiries_driven: usize,
    pub errors: usize,
}

enum Observed {
    Final { accepted_daa: u64 },
    OrderConflict(Vec<Outpoint>),
    Timeout { node_down: bool },
}

/// The facilitator.
pub struct Facilitator {
    pub policy: Policy,
    pub chain: Arc<dyn ChainView>,
    pub clock: Arc<dyn Clock>,
    pub ledger: Arc<Ledger>,
    pub config: FacilitatorConfig,
    pub metrics: Arc<Metrics>,
    verifier: Arc<dyn PaymentVerifier>,
    kill: AtomicBool,
    /// Milliseconds overriding `config.settle_wait` (0 = no override).
    settle_wait_ms: AtomicU64,
    locks: TxLocks,
    /// Intent mode (`kob-intent-v1`): the book and the keeper; `None` = not served.
    pub intents: Option<IntentRuntime>,
    /// Invoices; `None` = not served.
    pub invoices: Option<InvoiceRuntime>,
    invoice_locks: KeyLocks,
    /// The fee rates of the process (`crate::fee`): intent executions go at the high rate, expiries at the normal rate, both
    /// held to the total cap. Inside `kob-executor run` the runner publishes them; elsewhere they stay the floor.
    pub fees: crate::fee::FeeBoard,
}

fn kind_str(k: PaymentKind) -> &'static str {
    match k {
        PaymentKind::Native => "native",
        PaymentKind::Kcc20 => "kcc20",
        PaymentKind::SwapToKas => "swap-to-kas",
        PaymentKind::SwapToToken => "swap-to-token",
        PaymentKind::IntentToKas => "intent-to-kas",
        PaymentKind::IntentToToken => "intent-to-token",
    }
}

/// DAA scores before a coinbase output may be spent (consensus `coinbase_maturity`, Kaspa mainnet and testnets).
const COINBASE_MATURITY_DAA: u64 = 100;

fn unavailable(msg: impl Into<String>) -> X402Error {
    X402Error::new(Reason::UnexpectedSettleError, Diag::NodeUnavailable, msg).retryable()
}

fn pending_error(msg: &str) -> X402Error {
    X402Error::state(Diag::SettlementPending, msg).retryable()
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

impl Facilitator {
    /// A facilitator over `chain` with the protocol verifier.
    pub fn new(
        policy: Policy,
        chain: Arc<dyn ChainView>,
        clock: Arc<dyn Clock>,
        ledger: Arc<Ledger>,
        config: FacilitatorConfig,
    ) -> Facilitator {
        Facilitator {
            policy,
            chain,
            clock,
            ledger,
            config,
            metrics: Arc::new(Metrics::default()),
            verifier: Arc::new(ProtocolVerifier),
            kill: AtomicBool::new(false),
            settle_wait_ms: AtomicU64::new(0),
            locks: TxLocks::default(),
            intents: None,
            invoices: None,
            invoice_locks: KeyLocks::default(),
            fees: crate::fee::FeeBoard::default(),
        }
    }

    /// Serves intent-based swap-and-pay with this book and keeper.
    pub fn with_intents(mut self, rt: IntentRuntime) -> Facilitator {
        self.intents = Some(rt);
        self
    }

    /// Serves invoices from this store.
    pub fn with_invoices(mut self, rt: InvoiceRuntime) -> Facilitator {
        self.invoices = Some(rt);
        self
    }

    /// Replaces the verifier (tests).
    pub fn with_verifier(mut self, v: Arc<dyn PaymentVerifier>) -> Facilitator {
        self.verifier = v;
        self
    }

    /// Kill switch: while set (or while the kill-switch or pause file exists) verify and settle are refused and nothing is
    /// broadcast (intent executions and expiries wait).
    pub fn set_kill(&self, on: bool) {
        self.kill.store(on, Ordering::SeqCst);
    }

    /// Overrides how long `settle` observes finality (operators can tune it live; tests use it).
    pub fn set_settle_wait(&self, d: Duration) {
        self.settle_wait_ms.store(d.as_millis().max(1) as u64, Ordering::SeqCst);
    }

    pub fn killed(&self) -> bool {
        self.kill.load(Ordering::SeqCst)
            || crate::pause::is_set_opt(self.config.kill_switch_file.as_deref())
            || crate::pause::is_set_opt(self.config.pause_file.as_deref())
    }

    // ------------------------------------------------------------------------------- supported

    /// `GET /supported` (x402 v2 supported response with the binding's `extra`). Empty while killed.
    pub fn supported(&self) -> Value {
        if self.killed() {
            return json!({ "kinds": [], "extensions": [], "signers": {} });
        }
        let mut profiles = vec![Profile::StandardNative.as_str()];
        let has_tokens = self.policy.tokens.iter().next().is_some();
        // the kcc20 profile takes KCC-20 merchant assets only (a KRON token is a swap pay asset)
        if self.policy.tokens.iter().any(|t| t.is_merchant_capable()) {
            profiles.push(Profile::Kcc20.as_str());
        }
        let mut extra = Map::new();
        extra.insert("asset".into(), json!(kob_x402::wire::ASSET_KAS));
        extra.insert("binding".into(), json!(kob_x402::wire::BINDING_EXACT));
        extra.insert("defaultProfile".into(), json!(Profile::StandardNative.as_str()));
        extra.insert("profiles".into(), json!(profiles));
        extra.insert("modes".into(), json!(["verify", "settle"]));
        // KOB extras (proposals to the binding): swap-and-pay routes and the accepted token list
        if self.policy.swap_enabled && has_tokens {
            let mut b = vec![kob_x402::wire::BINDING_SWAP];
            if self.intents.is_some() && kob_protocol::router::check_router().is_ok() {
                b.push(kob_x402::wire::BINDING_INTENT);
                extra.insert("router".into(), json!(kob_protocol::router::ROUTER_ARTIFACT_ID));
            }
            extra.insert("routeBindings".into(), json!(b));
        }
        if self.invoices.is_some() {
            extra.insert("invoices".into(), json!(kob_x402::invoice::INVOICE_VERSION));
        }
        if has_tokens {
            let tokens: Vec<Value> = self
                .policy
                .tokens
                .iter()
                .map(|t| {
                    json!({
                        "asset": hex(&t.covenant_id),
                        "family": t.family.as_str(),
                        "templateHash": hex(&t.template_hash()),
                        "extensionCommitment": hex(&t.extension_commitment),
                        "custody": t.custody.as_str(),
                        "ticker": t.ticker,
                        "decimals": t.decimals,
                    })
                })
                .collect();
            extra.insert("tokens".into(), Value::Array(tokens));
        }
        json!({
            "kinds": [{
                "x402Version": X402_VERSION,
                "scheme": kob_x402::wire::SCHEME_EXACT,
                "network": self.policy.network.as_str(),
                "extra": Value::Object(extra),
            }],
            "extensions": [],
            "signers": {},
        })
    }

    // ---------------------------------------------------------------------------------- verify

    fn request_hash(&self, req: &FacilitatorRequest) -> Result<String> {
        if req.x402_version != X402_VERSION {
            return Err(X402Error::new(Reason::InvalidX402Version, Diag::InvalidKaspaX402Payload, "unsupported x402 version"));
        }
        match req.request_hash.as_deref() {
            Some(h) if parse_hash32(h).is_some() => Ok(h.to_ascii_lowercase()),
            Some(_) => Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "requestHash is not 32-byte hex")),
            None => Err(X402Error::payload(
                Diag::InvalidKaspaX402Payload,
                "requestHash is mandatory and is never inferred from the payload",
            )),
        }
    }

    fn run_verifier(&self, req: &FacilitatorRequest, rh: &str) -> Result<Verified> {
        let ctx = VerifyCtx { chain: &*self.chain, clock: &*self.clock, policy: &self.policy };
        let v = self.verifier.verify(&ctx, &req.payment_requirements, &req.payment_payload, rh)?;
        self.check_spendable_now(&v)?;
        Ok(v)
    }

    /// The node's own spend rules the script engine does not run: an input that is a coinbase output younger than
    /// the maturity is refused by consensus and the mempool, so `/verify` must not call the payment valid. (The standard dust
    /// bound on the change output is a mempool policy the facilitator does not model; `/verify` is a check, never a delivery
    /// guarantee: only `/settle`'s observed finality is.)
    fn check_spendable_now(&self, v: &Verified) -> Result<()> {
        if !v.entries.iter().any(|e| e.is_coinbase) {
            return Ok(());
        }
        let daa = self.chain.virtual_daa_score().map_err(|e| unavailable(e.to_string()))?;
        for (i, e) in v.entries.iter().enumerate() {
            if e.is_coinbase && daa < e.block_daa_score.saturating_add(COINBASE_MATURITY_DAA) {
                return Err(X402Error::state(
                    Diag::InvalidKaspaExactUtxo,
                    format!("input {i} is a coinbase output that has not reached its maturity ({COINBASE_MATURITY_DAA} DAA)"),
                ));
            }
        }
        Ok(())
    }

    /// Read-only replay checks (`/verify` never writes the ledger).
    fn replay_preflight(&self, v: &Verified) -> Result<()> {
        if let Some(id) = &v.payment_identifier {
            if let Some((rh, profile)) = self.ledger.payment_id_binding(id) {
                if rh != hex(&v.request_hash) || profile != v.profile.as_str() {
                    return Err(X402Error::state(
                        Diag::KaspaPaymentIdentifierConflict,
                        "the payment identifier is bound to a different request or profile",
                    ));
                }
            }
            if self.ledger.payment_id_live_txid(id).is_some_and(|t| t != hex(&v.txid)) {
                return Err(X402Error::state(
                    Diag::KaspaPaymentIdentifierConflict,
                    "the payment identifier is bound to another transaction",
                ));
            }
        }
        let txid = hex(&v.txid);
        for o in &v.consumed {
            if let Some(t) = self.ledger.consumer_of(o).filter(|t| *t != txid) {
                return Err(X402Error::state(Diag::Replay, format!("outpoint {o} is already consumed by transaction {t}")));
            }
        }
        Ok(())
    }

    /// `POST /verify`: full verification, no ledger writes, no broadcast.
    pub fn verify(&self, req: &FacilitatorRequest) -> VerifyResponse {
        Metrics::inc(&self.metrics.verify_requests);
        let r: Result<Verified> = (|| {
            if self.killed() {
                return Err(X402Error::new(
                    Reason::UnexpectedSettleError,
                    Diag::Internal,
                    "the facilitator is disabled by its operator",
                )
                .retryable());
            }
            let rh = self.request_hash(req)?;
            let v = self.run_verifier(req, &rh)?;
            self.replay_preflight(&v)?;
            Ok(v)
        })();
        match r {
            Ok(v) => VerifyResponse { is_valid: true, invalid_reason: None, payer: v.payer_address, extensions: None },
            Err(e) => {
                Metrics::inc(&self.metrics.verify_invalid);
                VerifyResponse {
                    is_valid: false,
                    invalid_reason: Some(e.reason.as_str().to_string()),
                    payer: None,
                    extensions: Some(failure_extension(&e)),
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------- settle

    /// `POST /settle` for the (already authenticated) `merchant`.
    pub fn settle(&self, merchant: &str, req: &FacilitatorRequest) -> SettlementResponse {
        self.settle_for(merchant, req, None)
    }

    /// [`Facilitator::settle`], for an invoice payment when `inv` is set.
    fn settle_for(&self, merchant: &str, req: &FacilitatorRequest, inv: Option<&invoice::InvoiceCtx>) -> SettlementResponse {
        Metrics::inc(&self.metrics.settle_requests);
        match self.settle_inner(merchant, req, inv) {
            Ok(r) => {
                Metrics::inc(&self.metrics.settle_success);
                r
            }
            Err(e) => {
                match e.diag {
                    Diag::SettlementPending => Metrics::inc(&self.metrics.settle_pending),
                    Diag::Replay | Diag::KaspaPaymentIdentifierConflict => Metrics::inc(&self.metrics.replay_rejected),
                    Diag::OrderConflict => Metrics::inc(&self.metrics.order_conflicts),
                    Diag::NodeUnavailable => Metrics::inc(&self.metrics.node_unavailable),
                    Diag::ExpiredAuthorization | Diag::Expired => Metrics::inc(&self.metrics.expired_rejected),
                    _ => {}
                }
                Metrics::inc(&self.metrics.settle_failed);
                SettlementResponse::failure(self.policy.network, None, &e)
            }
        }
    }

    fn check_not_expired(&self, v: &Verified) -> Result<()> {
        if self.clock.now_ms() >= v.authorization_expires_at_ms {
            return Err(X402Error::state(
                Diag::ExpiredAuthorization,
                "the payment authorization expired before settlement could start",
            ));
        }
        Ok(())
    }

    fn observe_wait(&self, req: &FacilitatorRequest) -> Duration {
        let base = match self.settle_wait_ms.load(Ordering::SeqCst) {
            0 => self.config.settle_wait,
            ms => Duration::from_millis(ms),
        };
        base.min(Duration::from_secs(req.payment_requirements.max_timeout_seconds))
    }

    fn settle_inner(&self, merchant: &str, req: &FacilitatorRequest, inv: Option<&invoice::InvoiceCtx>) -> Result<SettlementResponse> {
        if self.killed() {
            return Err(X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "the facilitator is disabled by its operator")
                .retryable());
        }
        if intent::is_intent_request(req) {
            return self.settle_intent(merchant, req, inv);
        }
        let rh = self.request_hash(req)?;
        let reqs_hash = hex(&requirements_hash(&req.payment_requirements)?);
        let accepted_hash = hex(&requirements_hash(&req.payment_payload.accepted)?);
        let wait = self.observe_wait(req);

        // (1) identity and the ledger first: an identical retry resumes without re-verifying
        let parsed_id = {
            let ctx = VerifyCtx { chain: &*self.chain, clock: &*self.clock, policy: &self.policy };
            common::parse_tx(&ctx, &req.payment_payload).ok().map(|p| p.tx.id().as_bytes())
        };
        let mut guard = parsed_id.map(|id| self.locks.lock(id));
        if let Some(id) = parsed_id {
            if let Some(e) = self.ledger.get(&hex(&id)) {
                if e.request_hash != rh || e.requirements_hash != reqs_hash || e.merchant != merchant || accepted_hash != reqs_hash {
                    return Err(X402Error::state(Diag::Replay, "this transaction was already consumed by a different request"));
                }
                if e.is_intent() {
                    // recorded as an intent payment: only its intent's execution pays the merchant, never this transaction
                    return Err(X402Error::state(Diag::Replay, "this transaction is recorded as an intent payment, not a direct one"));
                }
                match e.state {
                    State::Accepted => {
                        Metrics::inc(&self.metrics.settle_resumed);
                        return self.cached(&e);
                    }
                    // The transaction may not have reached the node (crash before broadcast, unknown submit
                    // outcome) or may have left it (mempool eviction): reconcile with the chain first.
                    State::Pending | State::Broadcast | State::Ambiguous => match self.chain_seen(&e) {
                        Ok(true) => {
                            Metrics::inc(&self.metrics.settle_resumed);
                            let e = self.ledger.transition(&e.txid, State::Broadcast, None, self.clock.now_ms())?;
                            return self.observe_and_finish(&e, wait);
                        }
                        Ok(false) => {} // unknown to the chain: the full path below re-verifies and resubmits the same transaction
                        Err(ce) => return Err(unavailable(format!("cannot reconcile the earlier attempt: {ce}"))),
                    },
                    State::Failed => {}
                }
            }
        }

        // (2) full verification
        let v = self.run_verifier(req, &rh)?;
        if parsed_id != Some(v.txid) {
            drop(guard.take());
            guard = Some(self.locks.lock(v.txid));
        }
        // (3) expiry, re-evaluated after the awaited verification and before creating anything
        self.check_not_expired(&v)?;

        // (4) consume the evidence
        let mut orders = Vec::with_capacity(v.order_inputs.len());
        for o in &v.order_inputs {
            let idx = v.tx.inputs.iter().position(|i| common::outpoint_of(i) == *o);
            let spk = idx.and_then(|i| v.entries.get(i)).map(|e| spk_to_hex(&e.script_public_key));
            orders.push((
                *o,
                spk.ok_or_else(|| {
                    X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "an order input has no resolved entry")
                })?,
            ));
        }
        let claim = self.ledger.claim(NewEntry {
            txid: v.txid,
            request_hash: v.request_hash,
            requirements_hash: v.requirements_hash,
            payment_id: v.payment_identifier.clone(),
            profile: v.profile.as_str().to_string(),
            kind: kind_str(v.kind).to_string(),
            merchant: merchant.to_string(),
            network: self.policy.network.as_str().to_string(),
            payer: v.payer_address.clone(),
            amount: req.payment_requirements.amount.clone(),
            finality: v.finality.as_str().to_string(),
            consumed: v.consumed.clone(),
            order_inputs: orders,
            watched: Watched {
                txid: hex(&v.merchant_output.outpoint.txid),
                index: v.merchant_output.outpoint.index,
                spk: spk_to_hex(&v.merchant_output.script_public_key),
                amount: v.merchant_output.amount,
            },
            extension: v.response_extension.clone(),
            now_ms: self.clock.now_ms(),
            intent: None,
            invoice: inv.map(|i| i.id.clone()),
        })?;
        let (entry, is_new) = match claim {
            Claim::New(e) => (e, true),
            Claim::Existing(e) => {
                Metrics::inc(&self.metrics.settle_resumed);
                if e.txid != hex(&v.txid) {
                    // the same payment identifier settled (or is settling) through another transaction
                    return self.respond_other(&e);
                }
                match e.state {
                    State::Accepted => return self.cached(&e),
                    _ => (e, false),
                }
            }
        };

        // (5) broadcast the exact verified transaction
        if let Err(e) = self.check_not_expired(&v) {
            if is_new {
                self.fail(&entry.txid, "authorization expired before broadcast");
            }
            return Err(e);
        }
        // the acceptance tracker (the indexer, in `kob-executor run`) follows it from before the broadcast
        self.chain.track(&v.txid);
        let entry = match self.chain.submit(&v.tx) {
            Ok(_) | Err(SubmitError::AlreadyKnown) => {
                Metrics::inc(&self.metrics.broadcasts);
                self.ledger.transition(&entry.txid, State::Broadcast, None, self.clock.now_ms())?
            }
            Err(SubmitError::Conflict(m)) => {
                self.fail(&entry.txid, &format!("conflict: {m}"));
                return Err(if v.order_inputs.is_empty() {
                    X402Error::state(Diag::InvalidKaspaExactUtxo, format!("an input was spent by another transaction: {}", short(&m)))
                } else {
                    self.order_conflict(&entry, self.spent_orders(&entry.order_refs()).unwrap_or_default())
                });
            }
            Err(SubmitError::Rejected(m)) => {
                self.fail(&entry.txid, &format!("rejected: {m}"));
                return Err(X402Error::state(
                    Diag::InvalidKaspaExactTransaction,
                    format!("the node rejected the transaction: {}", short(&m)),
                ));
            }
            Err(SubmitError::Unavailable(m)) => {
                let _ = self.ledger.transition(
                    &entry.txid,
                    State::Ambiguous,
                    Some(format!("submit outcome unknown: {m}")),
                    self.clock.now_ms(),
                );
                eprintln!("x402: submit of {} has an unknown outcome ({m}); evidence stays consumed", entry.txid);
                return Err(unavailable(
                    "the node could not be reached; the outcome of the broadcast is unknown, retry the identical request",
                ));
            }
        };
        // (6) observe finality (the per-transaction lock stays held: an identical retry waits, then reads the outcome)
        let result = self.observe_and_finish(&entry, wait);
        drop(guard);
        result
    }

    fn fail(&self, txid: &str, reason: &str) {
        if let Some(id) = parse_hash32(txid) {
            self.chain.untrack(&id);
        }
        if let Err(e) = self.ledger.transition(txid, State::Failed, Some(reason.to_string()), self.clock.now_ms()) {
            eprintln!("x402: cannot mark {txid} failed: {e}");
        }
    }

    fn order_conflict(&self, e: &Entry, spent: Vec<Outpoint>) -> X402Error {
        let list: Vec<Outpoint> = if spent.is_empty() { e.order_refs().into_iter().map(|(o, _)| o).collect() } else { spent };
        X402Error::state(
            Diag::OrderConflict,
            "an order this payment takes was filled or cancelled; request a fresh quote and sign a new payment",
        )
        .retryable()
        .with_details(json!({ "orders": list.iter().map(Outpoint::to_json).collect::<Vec<_>>() }))
    }

    /// Answer for a payment identifier whose bound entry belongs to another transaction: the outcome of one
    /// transaction is never the answer for another (the ledger's claim refuses this case already).
    fn respond_other(&self, _e: &Entry) -> Result<SettlementResponse> {
        Err(X402Error::state(Diag::KaspaPaymentIdentifierConflict, "the payment identifier is bound to another transaction"))
    }

    fn cached(&self, e: &Entry) -> Result<SettlementResponse> {
        e.response.as_ref().and_then(|r| serde_json::from_value::<SettlementResponse>(r.clone()).ok()).ok_or_else(|| {
            X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "the ledger holds no response for an accepted settlement")
        })
    }

    /// True if the chain knows the entry's transaction (merchant output accepted, or in the mempool).
    fn chain_seen(&self, e: &Entry) -> std::result::Result<bool, ChainError> {
        let (out, spk) = watch_of(e)?;
        self.chain.track(&out.txid);
        if matches!(self.chain.tracked(&out.txid), Tracked::Accepted { .. }) {
            return Ok(true);
        }
        Ok(match self.chain.output_status(&out, &spk)? {
            OutputStatus::Accepted { .. } | OutputStatus::Mempool => true,
            OutputStatus::Unknown => self.chain.in_mempool(&out.txid)?,
        })
    }

    /// Order outpoints (of `refs`) that are not in the UTXO set any more; `None` if the lookup failed.
    fn spent_orders(&self, refs: &[(Outpoint, ScriptPublicKey)]) -> Option<Vec<Outpoint>> {
        if refs.is_empty() {
            return Some(vec![]);
        }
        let found = self.chain.utxos(refs).ok()?;
        Some(refs.iter().zip(found).filter(|(_, u)| u.is_none()).map(|((o, _), _)| *o).collect())
    }

    /// One finality check of the watched output: `Some(block DAA)` when the required finality holds.
    fn check_final(&self, w: &(Outpoint, ScriptPublicKey), required: Finality) -> std::result::Result<Option<u64>, ChainError> {
        match self.accepted_daa(&w.0, &w.1)? {
            Some(block_daa_score) => {
                if required >= Finality::Confirmed {
                    let vdaa = self.chain.virtual_daa_score()?;
                    Ok((vdaa.saturating_sub(block_daa_score) >= self.policy.confirmations_daa).then_some(block_daa_score))
                } else {
                    Ok(Some(block_daa_score))
                }
            }
            // the mempool is never sufficient
            None => Ok(None),
        }
    }

    /// DAA score of the chain block that accepted the transaction of `out`: from the acceptance tracker
    /// when it saw the acceptance (it survives the merchant spending the output), else from the output
    /// being in the virtual UTXO set. `None`: not accepted as far as the chain view can tell.
    fn accepted_daa(&self, out: &Outpoint, spk: &ScriptPublicKey) -> std::result::Result<Option<u64>, ChainError> {
        if let Tracked::Accepted { block_daa_score } = self.chain.tracked(&out.txid) {
            return Ok(Some(block_daa_score));
        }
        Ok(match self.chain.output_status(out, spk)? {
            OutputStatus::Accepted { block_daa_score } => Some(block_daa_score),
            OutputStatus::Mempool | OutputStatus::Unknown => None,
        })
    }

    fn observe(&self, e: &Entry, wait: Duration) -> Observed {
        let Ok(watch) = watch_of(e) else { return Observed::Timeout { node_down: true } };
        let required = Finality::parse(&e.finality).unwrap_or(Finality::Accepted).max(Finality::Accepted);
        let orders = e.order_refs();
        let deadline = Instant::now() + wait;
        let mut polls = 0u32;
        let mut ok_polls = 0u32;
        loop {
            match self.check_final(&watch, required) {
                Ok(Some(daa)) => return Observed::Final { accepted_daa: daa },
                Ok(None) => ok_polls += 1,
                Err(_) => {}
            }
            let last = Instant::now() >= deadline;
            if !orders.is_empty() && (last || polls % 5 == 4) {
                if let Some(spent) = self.spent_orders(&orders).filter(|s| !s.is_empty()) {
                    // an order is gone: only a still-unaccepted transaction is dead (re-check to avoid the race
                    // in which our own acceptance consumed the order)
                    return match self.check_final(&watch, required) {
                        Ok(Some(daa)) => Observed::Final { accepted_daa: daa },
                        _ => Observed::OrderConflict(spent),
                    };
                }
            }
            if last {
                return Observed::Timeout { node_down: ok_polls == 0 };
            }
            let left = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(self.config.poll_interval.min(left));
            polls += 1;
        }
    }

    fn success_response(&self, e: &Entry, accepted_daa: u64) -> SettlementResponse {
        let mut kaspa = e.extension.clone();
        kaspa.insert("finality".into(), json!(e.finality));
        SettlementResponse {
            success: true,
            error_reason: None,
            transaction: e.txid.clone(),
            network: Some(e.network.clone()),
            payer: e.payer.clone(),
            amount: Some(e.amount.clone()),
            extensions: Some(json!({ "kaspa": Value::Object(kaspa), "kob": { "acceptedDaaScore": accepted_daa.to_string() } })),
        }
    }

    fn observe_and_finish(&self, e: &Entry, wait: Duration) -> Result<SettlementResponse> {
        if e.is_intent() {
            return Err(X402Error::new(
                Reason::UnexpectedSettleError,
                Diag::Internal,
                "an intent payment is not finalized as a direct payment",
            ));
        }
        match self.observe(e, wait) {
            Observed::Final { accepted_daa } => {
                let resp = self.success_response(e, accepted_daa);
                let value = serde_json::to_value(&resp)
                    .map_err(|x| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, x.to_string()))?;
                self.ledger.set_accepted(&e.txid, accepted_daa, value, self.clock.now_ms())?;
                Ok(resp)
            }
            Observed::OrderConflict(spent) => {
                self.fail(&e.txid, "order conflict: an order outpoint was spent by another transaction");
                Err(self.order_conflict(e, spent))
            }
            Observed::Timeout { node_down } => {
                if node_down {
                    Err(unavailable("the node could not be queried while waiting for finality; the transaction stays broadcast, retry the identical request"))
                } else {
                    Err(pending_error(
                        "the transaction was broadcast but has not reached the required finality yet; retry the identical request",
                    ))
                }
            }
        }
    }

    // ------------------------------------------------------------------------------- reconcile

    /// Re-checks the ledger against the chain: finalizes broadcast entries that reached their finality,
    /// promotes seen pending / ambiguous entries, and marks recently accepted entries `ambiguous` when
    /// their merchant output vanished and the transaction is not in the mempool (a reorg or a competing
    /// spend; the evidence stays consumed and the event is logged loudly).
    pub fn reconcile(&self) -> ReconcileReport {
        Metrics::inc(&self.metrics.reconcile_runs);
        let mut rep = ReconcileReport::default();
        let Ok(vdaa) = self.chain.virtual_daa_score() else {
            rep.errors += 1;
            return rep;
        };
        let entries = self.ledger.entries_in(&[State::Pending, State::Broadcast, State::Ambiguous, State::Accepted]);
        let mut by_spk: HashMap<String, std::result::Result<Vec<(Outpoint, kob_x402::chain::ChainUtxo)>, ChainError>> = HashMap::new();
        for e in entries {
            let Ok(txid) = parse_hash32(&e.txid).ok_or(()) else { continue };
            let Some(_g) = self.locks.try_lock(txid) else { continue }; // a settle is working on it
            if e.is_intent() && e.state != State::Accepted {
                // an intent payment: plan, execute or observe one step (its watched output is not final by itself). It never
                // takes the direct path below, whose finality would be the creation's own output.
                rep.checked += 1;
                rep.intents_driven += 1;
                match self.drive_intent(&e.txid) {
                    Ok(intent::Drive::Final(_)) => rep.finalized += 1,
                    Ok(_) => {}
                    Err(_) => rep.errors += 1,
                }
                continue;
            }
            let Ok((out, spk)) = watch_of(&e) else {
                rep.errors += 1;
                continue;
            };
            if e.state == State::Accepted {
                let young = e.accepted_daa.is_some_and(|d| vdaa.saturating_sub(d) < self.config.reorg_watch_daa);
                if !young {
                    self.chain.untrack(&txid);
                    continue;
                }
            }
            rep.checked += 1;
            let on_chain = match self.chain.tracked(&txid) {
                Tracked::Accepted { block_daa_score } => Some(block_daa_score),
                Tracked::Untracked | Tracked::NotAccepted => {
                    let utxos = by_spk.entry(e.watched.spk.clone()).or_insert_with(|| self.chain.utxos_of(&spk));
                    let Ok(utxos) = utxos else {
                        rep.errors += 1;
                        continue;
                    };
                    utxos.iter().find(|(o, _)| *o == out).map(|(_, u)| u.block_daa_score)
                }
            };
            let now = self.clock.now_ms();
            match (e.state, on_chain) {
                (State::Accepted, Some(_)) => {}
                (State::Accepted, None) => match self.chain.in_mempool(&txid) {
                    Ok(true) => {}
                    Ok(false) => {
                        Metrics::inc(&self.metrics.reorgs_detected);
                        rep.reorged += 1;
                        eprintln!(
                            "x402: REORG SUSPECTED: merchant output {out} of accepted settlement {} vanished and the transaction is not in the mempool; marking ambiguous, evidence stays consumed",
                            e.txid
                        );
                        let _ = self.ledger.transition(
                            &e.txid,
                            State::Ambiguous,
                            Some("merchant output vanished after acceptance (reorg or competing spend)".into()),
                            now,
                        );
                    }
                    Err(_) => rep.errors += 1,
                },
                (state, Some(daa)) => {
                    let mut cur = e.clone();
                    if state != State::Broadcast {
                        match self.ledger.transition(&e.txid, State::Broadcast, None, now) {
                            Ok(x) => {
                                rep.seen += 1;
                                cur = x;
                            }
                            Err(_) => {
                                rep.errors += 1;
                                continue;
                            }
                        }
                    }
                    let required = Finality::parse(&cur.finality).unwrap_or(Finality::Accepted);
                    if required < Finality::Confirmed || vdaa.saturating_sub(daa) >= self.policy.confirmations_daa {
                        let resp = self.success_response(&cur, daa);
                        if let Ok(v) = serde_json::to_value(&resp) {
                            if self.ledger.set_accepted(&cur.txid, daa, v, now).is_ok() {
                                rep.finalized += 1;
                            }
                        }
                    }
                }
                (state @ (State::Pending | State::Ambiguous), None) => match self.chain.in_mempool(&txid) {
                    Ok(true) => {
                        if self.ledger.transition(&e.txid, State::Broadcast, None, now).is_ok() {
                            rep.seen += 1;
                        }
                    }
                    Ok(false) => {
                        if state == State::Ambiguous {
                            let age = now.saturating_sub(e.updated_ms);
                            if age >= self.config.ambiguous_grace.as_millis() as u64 {
                                eprintln!(
                                    "x402: settlement {} was ambiguous and unknown to the node for {} s (not in the mempool, merchant output not on chain); failing it and releasing its outpoints",
                                    e.txid,
                                    age / 1000
                                );
                                if self.release_ambiguous(&e, now) {
                                    rep.released_ambiguous += 1;
                                }
                            }
                        }
                        if state == State::Pending {
                            let age = now.saturating_sub(e.created_ms);
                            if age >= self.config.pending_grace.as_millis() as u64 {
                                // nothing can ever resolve it; free the payer's outpoints instead of holding them forever
                                eprintln!(
                                    "x402: settlement {} was pending and unknown to the node for {} s (crash before broadcast); failing it and releasing its outpoints",
                                    e.txid,
                                    age / 1000
                                );
                                if self
                                    .ledger
                                    .transition(&e.txid, State::Failed, Some("never reached the node; expired".into()), now)
                                    .is_ok()
                                {
                                    rep.expired_pending += 1;
                                }
                            } else {
                                rep.unseen_pending += 1;
                                eprintln!(
                                    "x402: settlement {} is pending and unknown to the node (crash before broadcast?); evidence stays consumed until the identical request is retried",
                                    e.txid
                                );
                            }
                        }
                    }
                    Err(_) => rep.errors += 1,
                },
                (State::Broadcast, None) => {} // still waiting; an identical retry observes it
                _ => {}
            }
        }
        // intents that ended unexecuted: expire them from their deadline on (returns the payer's funds)
        if self.intents.is_some() {
            for e in self.ledger.entries_in(&[State::Failed]) {
                if e.intent.as_ref().and_then(|r| r.expiry.as_ref()).is_none_or(|x| x.outcome.is_some()) {
                    continue;
                }
                let Some(id) = parse_hash32(&e.txid) else { continue };
                let Some(_g) = self.locks.try_lock(id) else { continue };
                rep.expiries_driven += 1;
                if self.drive_expiry(&e.txid).is_err() {
                    rep.errors += 1;
                }
            }
        }
        rep.extra_payments_seen += self.reconcile_invoices();
        rep
    }

    /// Counters plus a gauge per ledger state.
    pub fn metrics_text(&self) -> String {
        let mut s = self.metrics.render();
        for st in [State::Pending, State::Broadcast, State::Accepted, State::Failed, State::Ambiguous] {
            s.push_str(&format!("kob_x402_ledger_entries{{state=\"{}\"}} {}\n", st.as_str(), self.ledger.entries_in(&[st]).len()));
        }
        s
    }
}

fn watch_of(e: &Entry) -> std::result::Result<(Outpoint, ScriptPublicKey), ChainError> {
    let txid = parse_hash32(&e.watched.txid).ok_or_else(|| ChainError("ledger: bad watched txid".into()))?;
    let spk = spk_from_hex(&e.watched.spk).map_err(|_| ChainError("ledger: bad watched script".into()))?;
    Ok((Outpoint::new(txid, e.watched.index), spk))
}
