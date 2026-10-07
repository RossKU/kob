//! Intent-based swap-and-pay settlement (`kob-intent-v1`): the facilitator is the intent's keeper.
//!
//! [`Facilitator::settle_intent`]:
//!
//! 1. an identical retry resumes from the ledger (the creation id is the key);
//! 2. the creation is verified ([`kob_x402::intent::verify_intent`]) and **dry-run**: an execution must
//!    build and validate against the current book, or nothing is consumed or broadcast
//!    (`intent_not_executable`, retryable);
//! 3. the creation's inputs and the intent outpoint are consumed in the ledger, the creation is broadcast;
//! 4. [`Facilitator::drive_intent`] runs until the execution is final or the settle wait ends: once the
//!    intent UTXO is accepted it plans an execution against the book of the moment, records it in the
//!    ledger BEFORE submitting it, and submits it; when an order it named is taken by someone else (a
//!    `Conflict` on submit, or the order spent while the execution is not accepted) the execution is dead,
//!    the order is remembered as lost and the next step plans again, without the payer. The periodic
//!    reconcile keeps driving intents whose `/settle` returned `settlement_pending`.
//!
//! Settlement is the execution's merchant output at the required finality. The intent ends without a
//! payment when its deadline (the authorization's expiry, capped by an invoice's) passes with no execution
//! in flight (`intent_expired`), when it is spent outside this facilitator (the payer's cancel or another
//! keeper: `intent_spent`), or after `maxAttempts` executions. An expired intent is then expired on chain by
//! the facilitator ([`Facilitator::drive_expiry`], from the router intent's own deadline on, in the reconcile
//! loop): the router returns the payer's KAS and tokens, and the intent can no longer be executed by anyone.
//! The payer can also cancel it at any time.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_x402::chain::{ChainError, Outpoint, OutputStatus, SubmitError, Tracked, Txid};
use kob_x402::error::{Diag, Reason, Result, X402Error};
use kob_x402::intent::{build_execution, build_expiry, BookView, IntentFacts, KeeperParams};
use kob_x402::safe_tx::{spk_from_hex, spk_to_hex};
use kob_x402::verify::VerifyCtx;
use kob_x402::wire::{hex, parse_hash32, FacilitatorRequest, Finality, OutpointJson, SettlementResponse, BINDING_INTENT};
use serde_json::{json, Value};

use super::invoice::InvoiceCtx;
use super::{kind_str, pending_error, short, unavailable, Facilitator, Metrics};
use crate::x402::ledger::{Claim, Entry, ExecRecord, ExecState, ExpiryRecord, IntentRecord, NewEntry, OrderRef, State, Watched};

/// The book a facilitator executes intents against, and the keeper's parameters.
#[derive(Clone)]
pub struct IntentRuntime {
    /// A consistent snapshot of the plain resting orders (inside `kob-executor run`: the indexer's book).
    /// `Err` while the book is not usable (the indexer catching up).
    pub book: Arc<dyn Fn() -> std::result::Result<BookView, String> + Send + Sync>,
    pub keeper: KeeperParams,
    /// Executions tried per intent before it is given up.
    pub max_attempts: usize,
    /// The executions' lock time is the virtual DAA score minus this margin.
    pub lock_margin_daa: u64,
}

/// One step of [`Facilitator::drive_intent`].
#[derive(Clone, Debug, PartialEq)]
pub enum Drive {
    /// An execution reached the required finality (the entry is `accepted`).
    Final(Box<SettlementResponse>),
    /// Something is in flight (the creation, an execution, confirmations) or the book cannot execute the
    /// intent right now; drive again later.
    Waiting(&'static str),
    /// The intent ended without a payment (the entry is `failed`).
    Dead(String),
}

/// True when the request is an intent payment (`route.binding` of the payload or the offer).
pub fn is_intent_request(req: &FacilitatorRequest) -> bool {
    req.payment_payload.payload.route.as_ref().is_some_and(|r| r.binding == BINDING_INTENT)
        || req.payment_requirements.is_intent()
        || req.payment_payload.accepted.is_intent()
}

fn outpoint_json(o: &Outpoint) -> OutpointJson {
    o.to_json()
}

fn op_of(j: &OutpointJson) -> Option<Outpoint> {
    Some(Outpoint::new(parse_hash32(&j.txid)?, j.index))
}

/// How long the facilitator retries the expiry of an intent on every reconcile, counted from when it gave the intent up
/// (never before the deadline: the node accepts the expiry once its past median time, which trails the wall clock by
/// minutes, reached the deadline). A facilitator that was down past the deadline starts its window when it comes back.
pub const EXPIRY_WINDOW_MS: u64 = 3_600_000;

/// Past [`EXPIRY_WINDOW_MS`] the expiry is still retried while the intent stands (it holds the payer's funds and stays
/// executable by anyone), at most once per this interval.
pub const EXPIRY_RETRY_MS: u64 = 600_000;

/// How long an expiry may wait in the mempool before it is replaced at the high rate (C5 X-8): the normal bucket aims at
/// inclusion within the minute.
pub const EXPIRY_BUMP_MS: u64 = 60_000;

fn intent_unsupported() -> X402Error {
    X402Error::new(Reason::UnsupportedScheme, Diag::RouteUnsupported, "intent-based swap-and-pay is not enabled on this facilitator")
}

impl Facilitator {
    fn ctx(&self) -> VerifyCtx<'_> {
        VerifyCtx { chain: &*self.chain, clock: &*self.clock, policy: &self.policy }
    }

    fn lock_time(&self, rt: &IntentRuntime) -> std::result::Result<u64, ChainError> {
        Ok(self.chain.virtual_daa_score()?.saturating_sub(rt.lock_margin_daa))
    }

    /// The keeper's own rate (the floor of its executions and expiries) and its parameters at another rate.
    fn keeper_rate(rt: &IntentRuntime) -> u64 {
        rt.keeper.fee.fee_rate.unwrap_or(kob_protocol::tx::MIN_FEE_RATE)
    }

    fn keeper_at(rt: &IntentRuntime, rate: u64) -> KeeperParams {
        let mut kp = rt.keeper.clone();
        kp.fee.fee_rate = Some(rate);
        kp
    }

    /// An execution at the high rate of the process (`crate::fee`: the merchant waits for it), held to the total cap; at the
    /// keeper's own rate when the intent cannot pay the higher fee (its funds bound what an execution may cost).
    fn priced_execution(
        &self,
        rt: &IntentRuntime,
        f: &IntentFacts,
        book: &BookView,
        exclude: &BTreeSet<Outpoint>,
        lock_time: u64,
    ) -> Result<Option<kob_x402::intent::Execution>> {
        let rates = crate::fee::read_board(&self.fees);
        let base = Self::keeper_rate(rt);
        let mut rate = rates.rate(crate::fee::Urgency::High, base);
        while rate > base {
            let Some(ex) = build_execution(f, book, exclude, lock_time, &Self::keeper_at(rt, rate))? else { break };
            let fee = tx_fee(&ex.tx, &ex.entries);
            match rates.capped(rate, fee, base) {
                Some(r) => rate = r,
                None => return Ok(Some(ex)),
            }
        }
        build_execution(f, book, exclude, lock_time, &rt.keeper)
    }

    /// The expiry at the rate of `urgency` (the first one: normal; a replacement: high), held to the total cap and to what the
    /// router lets an expiry pay (`EXPIRE_MAX_FEE`: the highest rate whose fee fits); at the keeper's own rate when the intent
    /// cannot pay a higher fee. Returns the transaction and the rate it pays.
    fn priced_expiry(
        &self,
        rt: &IntentRuntime,
        facts: &IntentFacts,
        urgency: crate::fee::Urgency,
    ) -> Result<(kaspa_consensus_core::tx::Transaction, u64)> {
        let rates = crate::fee::read_board(&self.fees);
        let base = Self::keeper_rate(rt);
        let (floor_tx, floor_entries) = build_expiry(facts, &facts.intent, facts.lock.as_ref(), &rt.keeper.fee)?;
        // the fee is linear in the rate: the highest rate whose fee stays within EXPIRE_MAX_FEE
        let floor_fee = tx_fee(&floor_tx, &floor_entries).max(1);
        let fits = (base as u128 * kob_protocol::router::EXPIRE_MAX_FEE as u128 / floor_fee as u128) as u64;
        let mut rate = rates.rate(urgency, base).min(fits.max(base));
        while rate > base {
            let Ok((tx, entries)) = build_expiry(facts, &facts.intent, facts.lock.as_ref(), &Self::keeper_at(rt, rate).fee) else {
                rate = rate.saturating_mul(9) / 10; // rounding at the cap: a little lower
                continue;
            };
            match rates.capped(rate, tx_fee(&tx, &entries), base) {
                Some(r) => rate = r,
                None => return Ok((tx, rate)),
            }
        }
        Ok((floor_tx, base))
    }

    /// C5 X-8: an expiry that waits in the mempool for `EXPIRY_BUMP_MS` is replaced (replace-by-fee) at the high rate of the
    /// process when that pays more than it does: the deadline has passed, and until the expiry is accepted anyone can still
    /// execute the intent. The replacement keeps every rule of the expiry (the same outputs, a higher fee within
    /// `EXPIRE_MAX_FEE`); one that the node refuses leaves the waiting expiry as it is.
    fn bump_expiry(
        &self,
        rt: &IntentRuntime,
        txid: &str,
        facts: &IntentFacts,
        x: &ExpiryRecord,
        waiting: Txid,
        now: u64,
    ) -> Result<()> {
        if x.last_try_ms.is_some_and(|t| now < t.saturating_add(EXPIRY_BUMP_MS)) {
            return Ok(());
        }
        let paid = x.rate.unwrap_or_else(|| Self::keeper_rate(rt));
        let touch = |rate: Option<(Txid, u64)>| {
            self.edit_intent(txid, |en| {
                if let Some(r) = en.intent.as_mut().and_then(|r| r.expiry.as_mut()) {
                    r.last_try_ms = Some(now);
                    if let Some((id, rate)) = rate {
                        r.txid = Some(hex(&id));
                        r.rate = Some(rate);
                    }
                }
            })
        };
        let Ok((tx, rate)) = self.priced_expiry(rt, facts, crate::fee::Urgency::High) else { return touch(None).map(|_| ()) };
        let id = tx.id().as_bytes();
        if rate <= paid || id == waiting {
            return touch(None).map(|_| ());
        }
        self.chain.track(&id);
        match self.chain.replace(&tx) {
            Ok(_) | Err(SubmitError::AlreadyKnown) => {
                self.chain.untrack(&waiting);
                eprintln!(
                    "x402: intent {txid}: expiry {} waited in the mempool at {paid}; replaced by {} at {rate} sompi/gram",
                    hex(&waiting),
                    hex(&id)
                );
                touch(Some((id, rate))).map(|_| ())
            }
            Err(_) => {
                self.chain.untrack(&id);
                touch(None).map(|_| ())
            }
        }
    }
}

/// The fee a built transaction pays: its inputs' values less its outputs'.
fn tx_fee(tx: &kaspa_consensus_core::tx::Transaction, entries: &[kaspa_consensus_core::tx::UtxoEntry]) -> u64 {
    let inp: u64 = entries.iter().map(|e| e.amount).sum();
    let out: u64 = tx.outputs.iter().map(|o| o.value).sum();
    inp.saturating_sub(out)
}

impl Facilitator {
    /// `POST /settle` (or an invoice payment) of an intent creation. See the module docs.
    pub(super) fn settle_intent(
        &self,
        merchant: &str,
        req: &FacilitatorRequest,
        inv: Option<&InvoiceCtx>,
    ) -> Result<SettlementResponse> {
        let rt = self.intents.as_ref().ok_or_else(intent_unsupported)?;
        let rh = self.request_hash(req)?;
        let reqs_hash = hex(&kob_x402::canonical::requirements_hash(&req.payment_requirements)?);
        let accepted_hash = hex(&kob_x402::canonical::requirements_hash(&req.payment_payload.accepted)?);
        let wait = self.observe_wait(req);

        // (1) the ledger first: an identical retry resumes
        let parsed_id = kob_x402::common::parse_tx(&self.ctx(), &req.payment_payload).ok().map(|p| p.tx.id().as_bytes());
        let mut guard = parsed_id.map(|id| self.locks.lock(id));
        if let Some(id) = parsed_id {
            if let Some(e) = self.ledger.get(&hex(&id)) {
                if e.request_hash != rh || e.requirements_hash != reqs_hash || e.merchant != merchant || accepted_hash != reqs_hash {
                    return Err(X402Error::state(Diag::Replay, "this intent was already consumed by a different request"));
                }
                match e.state {
                    State::Accepted => {
                        self.check_known_id(&e, req)?;
                        Metrics::inc(&self.metrics.settle_resumed);
                        return self.cached(&e);
                    }
                    State::Broadcast => {
                        self.check_known_id(&e, req)?;
                        Metrics::inc(&self.metrics.settle_resumed);
                        return self.drive_and_observe(&e.txid, wait);
                    }
                    // the creation may never have reached the node (a crash before the broadcast, an unknown
                    // outcome): resume only when the chain knows it, else the full path below resubmits it
                    State::Pending | State::Ambiguous => match self.creation_seen(&e) {
                        Ok(true) => {
                            self.check_known_id(&e, req)?;
                            Metrics::inc(&self.metrics.settle_resumed);
                            self.ledger.transition(&e.txid, State::Broadcast, None, self.clock.now_ms())?;
                            return self.drive_and_observe(&e.txid, wait);
                        }
                        Ok(false) => {}
                        Err(ce) => return Err(unavailable(format!("cannot reconcile the earlier attempt: {ce}"))),
                    },
                    State::Failed => {}
                }
            }
        }

        // (2) verification and the dry run
        let (v, mut facts) = kob_x402::intent::verify_intent(&self.ctx(), &req.payment_requirements, &req.payment_payload, &rh)?;
        self.check_spendable_now(&v)?;
        if parsed_id != Some(v.txid) {
            drop(guard.take());
            guard = Some(self.locks.lock(v.txid));
        }
        self.check_not_expired(&v)?;
        if let Some(i) = inv {
            facts.deadline_ms = facts.deadline_ms.min(i.expires_ms);
        }
        let book = (rt.book)().map_err(|e| unavailable(format!("the book is not available: {e}")))?;
        let lock_time = self.lock_time(rt).map_err(|e| unavailable(e.to_string()))?;
        if build_execution(&facts, &book, &BTreeSet::new(), lock_time, &rt.keeper)?.is_none() {
            return Err(X402Error::state(
                Diag::IntentNotExecutable,
                "the book cannot execute this intent now (no orders fit its shape and limits); nothing was broadcast",
            )
            .retryable());
        }

        // (3) consume and broadcast the creation
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
            order_inputs: vec![],
            watched: Watched {
                txid: hex(&v.merchant_output.outpoint.txid),
                index: v.merchant_output.outpoint.index,
                spk: spk_to_hex(&v.merchant_output.script_public_key),
                amount: v.merchant_output.amount,
            },
            extension: v.response_extension.clone(),
            now_ms: self.clock.now_ms(),
            intent: Some(IntentRecord { facts, executions: vec![], lost: vec![], seen_daa: None, expiry: None }),
            invoice: inv.map(|i| i.id.clone()),
        })?;
        let entry = match claim {
            Claim::New(e) => e,
            Claim::Existing(e) => {
                Metrics::inc(&self.metrics.settle_resumed);
                if e.txid != hex(&v.txid) {
                    return self.respond_other(&e);
                }
                if e.state == State::Accepted {
                    return self.cached(&e);
                }
                e
            }
        };
        if entry.state != State::Broadcast {
            if let Err(e) = self.check_not_expired(&v) {
                self.fail(&entry.txid, "authorization expired before broadcast");
                return Err(e);
            }
            // the pause / kill switch, read again right before the creation is broadcast
            if self.killed() {
                self.fail(&entry.txid, "the facilitator was disabled before the broadcast");
                return Err(X402Error::new(
                    Reason::UnexpectedSettleError,
                    Diag::Internal,
                    "the facilitator is disabled by its operator",
                )
                .retryable());
            }
            self.chain.track(&v.txid);
            match self.chain.submit(&v.tx) {
                Ok(_) | Err(SubmitError::AlreadyKnown) => {
                    Metrics::inc(&self.metrics.broadcasts);
                    self.ledger.transition(&entry.txid, State::Broadcast, None, self.clock.now_ms())?;
                }
                Err(SubmitError::Conflict(m)) => {
                    self.fail(&entry.txid, &format!("creation conflict: {m}"));
                    return Err(X402Error::state(
                        Diag::InvalidKaspaExactUtxo,
                        format!("an input of the intent creation was spent by another transaction: {}", short(&m)),
                    ));
                }
                Err(SubmitError::Rejected(m)) => {
                    self.fail(&entry.txid, &format!("creation rejected: {m}"));
                    return Err(X402Error::state(
                        Diag::InvalidKaspaExactTransaction,
                        format!("the node rejected the intent creation: {}", short(&m)),
                    ));
                }
                Err(SubmitError::Unavailable(m)) => {
                    let _ = self.ledger.transition(
                        &entry.txid,
                        State::Ambiguous,
                        Some(format!("creation submit outcome unknown: {m}")),
                        self.clock.now_ms(),
                    );
                    return Err(unavailable(
                        "the node could not be reached; the outcome of the broadcast is unknown, retry the identical request",
                    ));
                }
            }
        }
        // (4) execute and observe (the per-creation lock stays held)
        let r = self.drive_and_observe(&entry.txid, wait);
        drop(guard);
        r
    }

    /// True if the chain knows the creation of an intent entry (its intent output accepted or spent, or it is in the
    /// mempool).
    fn creation_seen(&self, e: &Entry) -> std::result::Result<bool, ChainError> {
        let Some(rec) = e.intent.as_ref() else { return Ok(false) };
        if rec.seen_daa.is_some() || !rec.executions.is_empty() {
            return Ok(true);
        }
        let op = rec.facts.outpoint();
        self.chain.track(&op.txid);
        if matches!(self.chain.tracked(&op.txid), Tracked::Accepted { .. }) || self.chain.in_mempool(&op.txid)? {
            return Ok(true);
        }
        let spk = spk_from_hex(&rec.facts.intent_spk).map_err(|_| ChainError("ledger: bad intent script".into()))?;
        Ok(matches!(self.chain.output_status(&op, &spk)?, OutputStatus::Accepted { .. } | OutputStatus::Mempool))
    }

    /// Drives an intent until it is final, dead, or `wait` elapsed (`settlement_pending`, retryable).
    fn drive_and_observe(&self, txid: &str, wait: Duration) -> Result<SettlementResponse> {
        let deadline = Instant::now() + wait;
        loop {
            match self.drive_intent(txid)? {
                Drive::Final(r) => return Ok(*r),
                Drive::Dead(why) => return Err(self.dead_error(txid, &why)),
                Drive::Waiting(_) => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(pending_error(
                    "the intent is being executed but has not reached the required finality yet; retry the identical request",
                ));
            }
            std::thread::sleep(self.config.poll_interval.min(left));
        }
    }

    fn dead_error(&self, txid: &str, why: &str) -> X402Error {
        let diag = match self.ledger.get(txid).and_then(|e| e.reason) {
            Some(r) if r.starts_with("intent_expired") => Diag::IntentExpired,
            Some(r) if r.starts_with("intent_spent") => Diag::IntentSpent,
            Some(r) if r.starts_with("intent_not_executable") => Diag::IntentNotExecutable,
            _ => Diag::InvalidKaspaExactUtxo,
        };
        X402Error::state(diag, why.to_string())
    }

    fn intent_entry(&self, txid: &str) -> Result<(Entry, IntentRecord)> {
        let e = self
            .ledger
            .get(txid)
            .ok_or_else(|| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "unknown intent entry"))?;
        let rec = e
            .intent
            .clone()
            .ok_or_else(|| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "the entry has no intent"))?;
        Ok((e, rec))
    }

    fn edit_intent(&self, txid: &str, f: impl FnOnce(&mut Entry)) -> Result<Entry> {
        Ok(self.ledger.update(txid, self.clock.now_ms(), f)?)
    }

    fn give_up(&self, e: &Entry, reason: String) -> Drive {
        Metrics::inc(&self.metrics.intent_failed);
        self.fail(&e.txid, &reason);
        Drive::Dead(reason)
    }

    /// Gives an intent that may be (or later be) on chain up AND schedules its expiry: the reconcile loop expires it from its
    /// deadline on ([`Self::drive_expiry`]), whatever made the payment fail, so the payer's KAS and tokens come back without
    /// the payer.
    fn give_up_and_expire(&self, e: &Entry, facts: &IntentFacts, reason: String) -> Result<Drive> {
        let until_ms = self.clock.now_ms().max(facts.state.deadline().max(0) as u64).saturating_add(EXPIRY_WINDOW_MS);
        self.edit_intent(&e.txid, |en| {
            if let Some(r) = en.intent.as_mut() {
                if r.expiry.is_none() {
                    r.expiry = Some(ExpiryRecord { until_ms, txid: None, last_try_ms: None, rate: None, outcome: None });
                }
            }
        })?;
        let dead = self.give_up(e, reason);
        let _ = self.drive_expiry(&e.txid);
        Ok(dead)
    }

    /// True while the creation of an intent whose UTXO is missing may still appear: in the mempool (never mined yet, or
    /// re-queued by a reorg that dropped the block that had it).
    fn creation_pending(&self, op: &Outpoint, spk: &ScriptPublicKey) -> std::result::Result<bool, ChainError> {
        Ok(self.chain.in_mempool(&op.txid)? || matches!(self.chain.output_status(op, spk)?, OutputStatus::Mempool))
    }

    fn exec_out(rec: &IntentRecord, x: &ExecRecord) -> Option<(Outpoint, ScriptPublicKey)> {
        Some((Outpoint::new(parse_hash32(&x.txid)?, x.merchant_index), spk_from_hex(&rec.facts.merchant_spk).ok()?))
    }

    /// The success response of an intent: `transaction` is the execution that paid the merchant.
    fn intent_success(&self, e: &Entry, rec: &IntentRecord, x: &ExecRecord, accepted_daa: u64) -> SettlementResponse {
        let mut kaspa = e.extension.clone();
        kaspa.insert("finality".into(), json!(e.finality));
        kaspa.insert("paymentOutputIndex".into(), json!(x.merchant_index));
        SettlementResponse {
            success: true,
            error_reason: None,
            transaction: x.txid.clone(),
            network: Some(e.network.clone()),
            payer: e.payer.clone(),
            amount: Some(e.amount.clone()),
            extensions: Some(json!({
                "kaspa": Value::Object(kaspa),
                "kob": {
                    "acceptedDaaScore": accepted_daa.to_string(),
                    "confirmationsDaa": self.depth_of(&e.finality).to_string(),
                    "intent": {
                        "creation": e.txid,
                        "outpoint": outpoint_json(&rec.facts.outpoint()),
                        "executions": rec.executions.len(),
                    },
                },
            })),
        }
    }

    fn finish(&self, e: &Entry, rec: &IntentRecord, k: usize, accepted_daa: u64) -> Result<Drive> {
        let x = &rec.executions[k];
        let resp = self.intent_success(e, rec, x, accepted_daa);
        let value =
            serde_json::to_value(&resp).map_err(|x| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, x.to_string()))?;
        let txid = x.txid.clone();
        let mi = x.merchant_index;
        self.edit_intent(&e.txid, |en| {
            if let Some(r) = en.intent.as_mut() {
                r.executions[k].state = ExecState::Accepted;
            }
            en.watched.txid = txid;
            en.watched.index = mi;
            en.watched.spk = rec.facts.merchant_spk.clone();
            en.watched.amount = rec.facts.merchant_value;
        })?;
        self.ledger.set_accepted(&e.txid, accepted_daa, value, self.clock.now_ms())?;
        Ok(Drive::Final(Box::new(resp)))
    }

    /// One step of the expiry of an intent that ended unexecuted (its entry is `failed` with a pending
    /// [`ExpiryRecord`]): from the intent's deadline on, build and submit the expiry (no signature: the router returns
    /// the intent's KAS, less the fee, and the locked tokens to the payer). A node accepts it once its past median time
    /// reached the deadline, so a rejection before that is retried on the next reconcile. Returns `true` while the
    /// expiry is still pending.
    pub fn drive_expiry(&self, txid: &str) -> Result<bool> {
        let rt = self.intents.as_ref().ok_or_else(intent_unsupported)?;
        let (_, rec) = self.intent_entry(txid)?;
        let Some(x) = rec.expiry.clone() else { return Ok(false) };
        if x.outcome.is_some() {
            return Ok(false);
        }
        if self.killed() {
            // paused: nothing is broadcast; the expiry stays pending
            return Ok(true);
        }
        let node = |x: ChainError| unavailable(x.to_string());
        let settle = |outcome: &str| -> Result<bool> {
            self.edit_intent(txid, |en| {
                if let Some(r) = en.intent.as_mut().and_then(|r| r.expiry.as_mut()) {
                    r.outcome = Some(outcome.to_string());
                }
            })?;
            Ok(false)
        };
        let facts = &rec.facts;
        let intent_op = facts.outpoint();
        let intent_spk = spk_from_hex(&facts.intent_spk)
            .map_err(|_| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "bad intent script"))?;
        let found = self.chain.utxos(&[(intent_op, intent_spk.clone())]).map_err(node)?.into_iter().next().flatten();
        let ours = x.txid.as_deref().and_then(parse_hash32);
        if found.is_none() {
            if let Some(id) = ours {
                // our expiry is accepted: the tracker says so, or its payer output (output 0) is in the UTXO set
                let payer_out = (Outpoint::new(id, 0), kob_protocol::script::p2pk_spk(&facts.state.payer()));
                let accepted = matches!(self.chain.tracked(&id), Tracked::Accepted { .. })
                    || matches!(self.chain.output_status(&payer_out.0, &payer_out.1).map_err(node)?, OutputStatus::Accepted { .. });
                if accepted {
                    self.chain.untrack(&id);
                    return settle("expired");
                }
                if self.chain.in_mempool(&id).map_err(node)? {
                    return Ok(true);
                }
                self.chain.untrack(&id);
            }
            // The creation is not (or no longer) in the UTXO set. Re-queued by a reorg, or never mined yet: it may still
            // appear, and then it must be expired like any other.
            if self.creation_pending(&intent_op, &intent_spk).map_err(node)? {
                return Ok(true);
            }
            if rec.seen_daa.is_none() {
                return if self.clock.now_ms() > x.until_ms { settle("never_created") } else { Ok(true) };
            }
            return settle("spent");
        }
        let now = self.clock.now_ms();
        if (now as i128) < facts.state.deadline() as i128 {
            return Ok(true);
        }
        if let Some(id) = ours {
            if self.chain.in_mempool(&id).map_err(node)? {
                self.bump_expiry(rt, txid, facts, &x, id, now)?;
                return Ok(true);
            }
        }
        // Past the window the expiry is still retried while the intent stands (it holds the payer's funds and stays
        // executable), throttled.
        if now > x.until_ms && x.last_try_ms.is_some_and(|t| now < t.saturating_add(EXPIRY_RETRY_MS)) {
            return Ok(true);
        }
        let (tx, rate) = match self.priced_expiry(rt, facts, crate::fee::Urgency::Normal) {
            Ok(v) => v,
            // deterministic in the intent's facts: it will never build (the payer's cancel remains)
            Err(e) => return settle(&format!("unbuildable: {}", short(&e.to_string()))),
        };
        let id = tx.id().as_bytes();
        self.chain.track(&id);
        let sub = Some(hex(&id));
        self.edit_intent(txid, |en| {
            if let Some(r) = en.intent.as_mut().and_then(|r| r.expiry.as_mut()) {
                r.last_try_ms = Some(now);
            }
        })?;
        match self.chain.submit(&tx) {
            Ok(_) | Err(SubmitError::AlreadyKnown) => {
                self.edit_intent(txid, |en| {
                    if let Some(r) = en.intent.as_mut().and_then(|r| r.expiry.as_mut()) {
                        r.txid = sub;
                        r.rate = Some(rate);
                    }
                })?;
                Ok(true)
            }
            // the node's past median time has not reached the deadline yet (or the intent is being spent): next round
            Err(SubmitError::Rejected(_)) | Err(SubmitError::Unavailable(_)) => {
                self.chain.untrack(&id);
                Ok(true)
            }
            Err(SubmitError::Conflict(_)) => {
                self.chain.untrack(&id);
                settle("spent")
            }
        }
    }

    /// One non-blocking step of an intent payment (see the module docs). Persists every decision.
    pub fn drive_intent(&self, txid: &str) -> Result<Drive> {
        let rt = self.intents.as_ref().ok_or_else(intent_unsupported)?;
        let (e, rec) = self.intent_entry(txid)?;
        match e.state {
            State::Accepted => {
                let k = rec.executions.iter().position(|x| x.state == ExecState::Accepted);
                return match (k, e.response.as_ref()) {
                    (Some(_), Some(r)) => Ok(Drive::Final(Box::new(
                        serde_json::from_value(r.clone())
                            .map_err(|x| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, x.to_string()))?,
                    ))),
                    _ => Ok(Drive::Waiting("accepted")),
                };
            }
            State::Failed => return Ok(Drive::Dead(e.reason.clone().unwrap_or_else(|| "failed".into()))),
            _ => {}
        }
        if self.killed() {
            // paused: nothing is broadcast (no execution, no expiry); the intent waits
            return Ok(Drive::Waiting("paused"));
        }
        let required = Finality::parse(&e.finality).unwrap_or(Finality::Accepted).max(Finality::Accepted);
        let facts: &IntentFacts = &rec.facts;
        let now = self.clock.now_ms();
        let node = |x: ChainError| unavailable(x.to_string());

        // a. every execution of ours: final? (a dead one may have won a race), live ones in flight?
        for (k, x) in rec.executions.iter().enumerate() {
            let Some(w) = Self::exec_out(&rec, x) else { continue };
            if let Some(daa) = self.check_final(&w, required).map_err(node)? {
                return self.finish(&e, &rec, k, daa);
            }
        }
        if let Some((k, x)) = rec.executions.iter().enumerate().rev().find(|(_, x)| x.state == ExecState::Live) {
            let w = Self::exec_out(&rec, x)
                .ok_or_else(|| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "bad execution record"))?;
            if self.accepted_daa(&w.0, &w.1).map_err(node)?.is_some() {
                return Ok(Drive::Waiting("confirmations"));
            }
            let orders: Vec<(Outpoint, ScriptPublicKey)> = x
                .orders
                .iter()
                .filter_map(|o| Some((Outpoint::new(parse_hash32(&o.txid)?, o.index), spk_from_hex(&o.spk).ok()?)))
                .collect();
            let spent = self.spent_orders(&orders).ok_or_else(|| unavailable("cannot look the orders up"))?;
            let in_pool = self.chain.in_mempool(&w.0.txid).map_err(node)?;
            if spent.is_empty() && in_pool {
                return Ok(Drive::Waiting("execution in the mempool"));
            }
            // re-check: our own acceptance spends the orders too
            if self.accepted_daa(&w.0, &w.1).map_err(node)?.is_some() {
                return Ok(Drive::Waiting("confirmations"));
            }
            Metrics::inc(&self.metrics.intent_conflicts);
            let reason = if spent.is_empty() {
                "not in the mempool and not accepted".to_string()
            } else {
                "an order was filled by another transaction".to_string()
            };
            let lost: Vec<OutpointJson> = spent.iter().map(outpoint_json).collect();
            self.edit_intent(txid, |en| {
                if let Some(r) = en.intent.as_mut() {
                    r.executions[k].state = ExecState::Dead;
                    r.executions[k].reason = Some(reason);
                    for l in lost {
                        if !r.lost.contains(&l) {
                            r.lost.push(l);
                        }
                    }
                }
                en.order_inputs.clear();
            })?;
            return self.drive_intent(txid);
        }

        // b. the intent itself
        let intent_op = facts.outpoint();
        let intent_spk = spk_from_hex(&facts.intent_spk)
            .map_err(|_| X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "bad intent script"))?;
        let found = self.chain.utxos(&[(intent_op, intent_spk.clone())]).map_err(node)?.into_iter().next().flatten();
        let Some(intent_utxo) = found else {
            let created = rec.seen_daa.is_some() || matches!(self.chain.tracked(&intent_op.txid), Tracked::Accepted { .. });
            // Not in the UTXO set but in the mempool: never mined yet, or seen once and re-queued by a reorg that dropped
            // its block. Either way it may still (again) hold the payer's funds: wait.
            if self.creation_pending(&intent_op, &intent_spk).map_err(node)? {
                return Ok(Drive::Waiting(if created { "creation re-queued (reorg)" } else { "creation in the mempool" }));
            }
            if !created {
                // dropped from the mempool (or never relayed): the creation is in the payload of the entry? We hold only its
                // id, so a creation that vanished is resolved by the payer's identical retry (it carries the bytes) or by its
                // inputs being spent; until the deadline we wait. Past it the expiry stays scheduled: a creation that still
                // turns up is expired.
                if now >= facts.deadline_ms {
                    return self.give_up_and_expire(
                        &e,
                        facts,
                        "intent_expired: the intent creation never reached the chain before the deadline".into(),
                    );
                }
                return Ok(Drive::Waiting("creation not seen yet"));
            }
            return Ok(self.give_up(
                &e,
                "intent_spent: the intent was spent outside this facilitator (the payer's cancel or another keeper's execution)"
                    .into(),
            ));
        };
        if rec.seen_daa.is_none() {
            let daa = intent_utxo.block_daa_score;
            self.edit_intent(txid, |en| {
                if let Some(r) = en.intent.as_mut() {
                    r.seen_daa = Some(daa);
                }
            })?;
        }
        if now >= facts.deadline_ms {
            // The payment is over. The intent itself stays executable by anyone until it is spent (a covenant cannot
            // prove an upper time bound), so the facilitator expires it from its deadline on: that returns the payer's
            // KAS and tokens and ends it (`drive_expiry`, driven by the reconcile loop).
            return self.give_up_and_expire(
                &e,
                facts,
                "intent_expired: no execution was accepted before the deadline; the facilitator expires the intent (its KAS and \
                 tokens return to the payer), the payer can also cancel it"
                    .into(),
            );
        }
        if rec.executions.len() >= rt.max_attempts {
            // the intent stays on chain: expired from its deadline on like an unexecuted one
            return self.give_up_and_expire(
                &e,
                facts,
                format!(
                    "intent_not_executable: {} executions failed; the facilitator expires the intent from its deadline on, the \
                     payer can also cancel it",
                    rec.executions.len()
                ),
            );
        }
        // c. plan and submit an execution against the book of the moment
        let mut f = facts.clone();
        f.intent.block_daa_score = intent_utxo.block_daa_score;
        if let Some(l) = f.lock.as_mut() {
            l.utxo.block_daa_score = intent_utxo.block_daa_score;
        }
        let book = match (rt.book)() {
            Ok(b) => b,
            Err(_) => return Ok(Drive::Waiting("book not available")),
        };
        let exclude: BTreeSet<Outpoint> = rec.lost.iter().filter_map(op_of).collect();
        let lock_time = self.lock_time(rt).map_err(node)?;
        let Some(ex) = self.priced_execution(rt, &f, &book, &exclude, lock_time)? else {
            return Ok(Drive::Waiting("the book cannot execute the intent now"));
        };
        let ex_txid = ex.tx.id().as_bytes();
        let order_refs: Vec<OrderRef> = ex
            .orders
            .iter()
            .map(|o| {
                let i = ex.tx.inputs.iter().position(|inp| kob_x402::common::outpoint_of(inp) == *o).unwrap_or(0);
                OrderRef { txid: hex(&o.txid), index: o.index, spk: spk_to_hex(&ex.entries[i].script_public_key) }
            })
            .collect();
        let rec_x = ExecRecord {
            txid: hex(&ex_txid),
            merchant_index: ex.merchant_output.index,
            orders: order_refs.clone(),
            state: ExecState::Live,
            reason: None,
            at_ms: now,
        };
        let mspk = spk_to_hex(&ex.merchant_spk);
        let mval = facts.merchant_value;
        let k = rec.executions.len();
        self.edit_intent(txid, |en| {
            if let Some(r) = en.intent.as_mut() {
                r.executions.push(rec_x);
            }
            en.order_inputs = order_refs;
            en.watched = Watched { txid: hex(&ex_txid), index: ex.merchant_output.index, spk: mspk, amount: mval };
        })?;
        self.chain.track(&ex_txid);
        Metrics::inc(&self.metrics.intent_executions);
        let dead = |reason: String, lost: Vec<OutpointJson>| -> Result<Entry> {
            self.chain.untrack(&ex_txid);
            self.edit_intent(txid, |en| {
                if let Some(r) = en.intent.as_mut() {
                    r.executions[k].state = ExecState::Dead;
                    r.executions[k].reason = Some(reason);
                    for l in lost {
                        if !r.lost.contains(&l) {
                            r.lost.push(l);
                        }
                    }
                }
                en.order_inputs.clear();
            })
        };
        match self.chain.submit(&ex.tx) {
            Ok(_) | Err(SubmitError::AlreadyKnown) => Ok(Drive::Waiting("execution submitted")),
            Err(SubmitError::Conflict(m)) => {
                Metrics::inc(&self.metrics.intent_conflicts);
                let refs: Vec<(Outpoint, ScriptPublicKey)> = ex
                    .orders
                    .iter()
                    .map(|o| {
                        let i = ex.tx.inputs.iter().position(|inp| kob_x402::common::outpoint_of(inp) == *o).unwrap_or(0);
                        (*o, ex.entries[i].script_public_key.clone())
                    })
                    .collect();
                let spent = self.spent_orders(&refs).unwrap_or_default();
                // an order in the mempool of another transaction is not spent in the UTXO set yet: name all of them lost
                let lost: Vec<OutpointJson> = if spent.is_empty() {
                    ex.orders.iter().map(outpoint_json).collect()
                } else {
                    spent.iter().map(outpoint_json).collect()
                };
                dead(format!("conflict: {}", short(&m)), lost)?;
                Ok(Drive::Waiting("execution conflicted; re-planning"))
            }
            Err(SubmitError::Rejected(m)) => {
                eprintln!("x402 intent {txid}: execution {} rejected: {m}", hex(&ex_txid));
                dead(format!("rejected: {}", short(&m)), vec![])?;
                Ok(Drive::Waiting("execution rejected; re-planning"))
            }
            Err(SubmitError::Unavailable(_)) => Ok(Drive::Waiting("execution submit outcome unknown")),
        }
    }
}
