//! Invoices (`kob-invoice-v1`): a merchant registers an invoice (its x402 requirements, a reference and an
//! expiry), anyone fetches it at `GET /invoices/<id>` (the URL a QR code carries), a payer pays it once at
//! `POST /invoices/<id>/pay`, and anyone reads its invoice-level status.
//!
//! Registration is merchant-authenticated (the facilitator's API keys) and checked: the invoice is
//! structurally valid, every `accepts` entry is one this facilitator settles (profile, allowlisted tokens,
//! enabled routes) and is within the merchant's allowed `payTo` / assets. The id is the invoice's content
//! hash, so the URL is self-verifying.
//!
//! **One invoice is paid once.** Under a per-invoice lock: a payment whose transaction is already an
//! attempt of the invoice resumes (an identical retry); otherwise, when an attempt is accepted the payment
//! is a **duplicate**, when one is in flight it is refused as pending (retryable), and at or after
//! `expiresAt` it is **late**. A refused payment is verified first (a forged one is just rejected) and kept
//! as evidence with its merchant output (or intent outpoint); the periodic reconcile reports it if it
//! reaches the chain anyway (the payer broadcast it), so the merchant can refund it. A payment is settled
//! with the invoice id as its request hash and the invoice's merchant; an intent executes no later than
//! `expiresAt`.
//!
//! The store is an append-only JSONL log (one record per change, the last line of an id wins; a torn last
//! line is dropped, a corrupt line elsewhere refuses to start), like the replay ledger.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kob_x402::chain::{Outpoint, OutputStatus};
use kob_x402::common::iso_from_ms;
use kob_x402::error::{Diag, Reason, Result, X402Error};
use kob_x402::invoice::{ExtraPayment, Invoice, InvoiceAttempt, InvoicePayment, InvoiceState, InvoiceStatus};
use kob_x402::safe_tx::{spk_from_hex, spk_to_hex};
use kob_x402::wire::{
    hex, FacilitatorRequest, PaymentPayload, PaymentRequirements, Profile, SettlementResponse, BINDING_INTENT, BINDING_SWAP,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Facilitator, Metrics};
use crate::x402::ledger::State;

/// The invoice an invoice payment settles for.
pub struct InvoiceCtx {
    pub id: String,
    pub expires_ms: u64,
}

/// A payment refused for an invoice (duplicate or late), or released after an ambiguous outcome (C5 X-9), kept as evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtraRecord {
    /// `duplicate`, `late`, or `released` (an attempt whose outcome stayed unknown: failed, still watched).
    pub kind: String,
    /// The payment transaction (an intent: its creation).
    pub txid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// The output that would pay the merchant (direct payments) or the intent output (intents), with its script.
    pub watch_txid: String,
    pub watch_index: u32,
    pub watch_spk: String,
    /// True when the watched output is an intent (the merchant is paid only by an execution).
    #[serde(default)]
    pub intent: bool,
    /// `refused`, `accepted` (a direct payment reached the chain), `intent-on-chain` (the payer broadcast the creation).
    pub observed: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_daa: Option<u64>,
    pub at_ms: u64,
    /// The outpoints a refused payment spends (`txid:index`, sorted): a refused payment spending exactly the same ones (a
    /// fee variant of the same funding; at most one of them can ever reach the chain) is not kept again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spends: Vec<String>,
}

/// One registered invoice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceRecord {
    pub id: String,
    pub merchant: String,
    pub invoice: Invoice,
    pub expires_ms: u64,
    pub created_ms: u64,
    #[serde(default)]
    pub extra: Vec<ExtraRecord>,
}

struct StoreInner {
    file: Option<File>,
    records: HashMap<String, InvoiceRecord>,
}

/// The durable invoice store.
pub struct InvoiceStore {
    inner: Mutex<StoreInner>,
    _lock: Option<File>,
    #[allow(dead_code)]
    path: Option<PathBuf>,
}

fn io(e: impl std::fmt::Display) -> X402Error {
    X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, format!("invoice store: {e}")).retryable()
}

impl InvoiceStore {
    /// A volatile store (tests, `:memory:`).
    pub fn in_memory() -> InvoiceStore {
        InvoiceStore { inner: Mutex::new(StoreInner { file: None, records: HashMap::new() }), _lock: None, path: None }
    }

    /// Opens (creating if needed) the log at `path` and replays it.
    pub fn open(path: impl AsRef<Path>) -> std::result::Result<InvoiceStore, String> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(format!("{}.lock", path.display()))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|_| format!("{} is locked by another process", path.display()))?;
        let mut raw = Vec::new();
        if path.exists() {
            File::open(&path).and_then(|mut f| f.read_to_end(&mut raw)).map_err(|e| e.to_string())?;
        }
        let mut records = HashMap::new();
        let mut good = 0usize;
        let mut pos = 0usize;
        let mut line_no = 0usize;
        while pos < raw.len() {
            line_no += 1;
            let end = raw[pos..].iter().position(|b| *b == b'\n').map(|i| pos + i).unwrap_or(raw.len());
            let line = &raw[pos..end];
            let last = end + 1 >= raw.len();
            if line.iter().all(u8::is_ascii_whitespace) {
                good = (end + 1).min(raw.len());
            } else {
                #[derive(Deserialize)]
                struct Rec {
                    invoice: InvoiceRecord,
                }
                match serde_json::from_slice::<Rec>(line) {
                    Ok(r) => {
                        records.insert(r.invoice.id.clone(), r.invoice);
                        good = (end + 1).min(raw.len());
                    }
                    Err(e) if last => {
                        eprintln!("invoices: discarding a torn last record at line {line_no}: {e}");
                        break;
                    }
                    Err(e) => return Err(format!("{} corrupt at line {line_no}: {e}", path.display())),
                }
            }
            pos = end + 1;
        }
        let mut file =
            OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&path).map_err(|e| e.to_string())?;
        if good as u64 != raw.len() as u64 {
            file.set_len(good as u64).map_err(|e| e.to_string())?;
        }
        file.seek(std::io::SeekFrom::End(0)).map_err(|e| e.to_string())?;
        if good > 0 && raw.get(good - 1) != Some(&b'\n') {
            file.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(InvoiceStore { inner: Mutex::new(StoreInner { file: Some(file), records }), _lock: Some(lock), path: Some(path) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StoreInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(g: &mut StoreInner, r: InvoiceRecord) -> Result<InvoiceRecord> {
        if let Some(f) = g.file.as_mut() {
            let mut line = serde_json::to_string(&json!({ "invoice": r })).map_err(io)?;
            line.push('\n');
            f.write_all(line.as_bytes()).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        g.records.insert(r.id.clone(), r.clone());
        Ok(r)
    }

    /// Inserts a new record; an existing id returns the stored record (registration is idempotent).
    pub fn insert(&self, r: InvoiceRecord) -> Result<(InvoiceRecord, bool)> {
        let mut g = self.lock();
        if let Some(e) = g.records.get(&r.id) {
            return Ok((e.clone(), false));
        }
        Ok((Self::persist(&mut g, r)?, true))
    }

    pub fn get(&self, id: &str) -> Option<InvoiceRecord> {
        self.lock().records.get(id).cloned()
    }

    /// Persists an edit of a record.
    pub fn update(&self, id: &str, f: impl FnOnce(&mut InvoiceRecord)) -> Result<InvoiceRecord> {
        let mut g = self.lock();
        let mut r = g.records.get(id).cloned().ok_or_else(|| X402Error::state(Diag::InvoiceUnknown, "unknown invoice"))?;
        f(&mut r);
        Self::persist(&mut g, r)
    }

    /// Unexpired invoices of a merchant.
    pub fn open_count(&self, merchant: &str, now_ms: u64) -> usize {
        self.lock().records.values().filter(|r| r.merchant == merchant && r.expires_ms > now_ms).count()
    }

    /// Records created after `since_ms`.
    pub fn recent(&self, since_ms: u64) -> Vec<InvoiceRecord> {
        self.lock().records.values().filter(|r| r.created_ms >= since_ms).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.lock().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Invoices served by a facilitator.
#[derive(Clone)]
pub struct InvoiceRuntime {
    pub store: Arc<InvoiceStore>,
    /// Longest `expiresAt - now` accepted at registration.
    pub max_lifetime_ms: u64,
    /// Base URL invoices are served at (`https://pay.example.com`), for the `url` of responses.
    pub public_url: Option<String>,
    /// Most unexpired invoices per merchant.
    pub max_open_per_merchant: usize,
    /// Most refused payments (duplicate or late) kept as evidence per invoice; further ones are rejected without being
    /// recorded. Anyone may submit them, so this bounds the record, its rewrites and the reconcile watch list.
    pub max_extra_payments: usize,
}

/// How long refused payments are watched for (they are evidence for refunds).
const WATCH_EXTRA_MS: u64 = 7 * 24 * 3_600_000;

fn invoices_off() -> X402Error {
    X402Error::new(Reason::UnsupportedScheme, Diag::InvoiceUnknown, "invoices are not enabled on this facilitator")
}

impl Facilitator {
    fn inv_rt(&self) -> Result<&InvoiceRuntime> {
        self.invoices.as_ref().ok_or_else(invoices_off)
    }

    /// Checks that this facilitator can settle a requirement (profile, routes, tokens), without a payment.
    fn offerable(&self, r: &PaymentRequirements) -> Result<()> {
        let net = r.network()?;
        if net != self.policy.network {
            return Err(X402Error::requirements(
                Diag::InvalidKaspaX402Binding,
                format!("this facilitator serves {}", self.policy.network),
            ));
        }
        let addr = kaspa_addresses::Address::try_from(r.pay_to.as_str())
            .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo is not a Kaspa address: {e}")))?;
        if addr.prefix != net.prefix() {
            return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "payTo prefix does not match the network"));
        }
        let spk = kaspa_txscript::pay_to_address_script(&addr);
        if r.extra_str("payToScriptPublicKey").map(str::to_ascii_lowercase) != Some(spk_to_hex(&spk)) {
            return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "extra.payToScriptPublicKey does not match payTo"));
        }
        r.finality()?;
        match r.profile()? {
            Profile::StandardNative => {
                if r.asset != kob_x402::wire::ASSET_KAS {
                    return Err(X402Error::requirements(Diag::InvalidKaspaX402Asset, "a standard-native requirement pays KAS"));
                }
            }
            Profile::Kcc20 => {
                let id = kob_x402::wire::parse_hash32(&r.asset)
                    .ok_or_else(|| X402Error::requirements(Diag::InvalidKaspaX402Asset, "asset must be a covenant id"))?;
                let t = self
                    .policy
                    .tokens
                    .find(&id)
                    .ok_or_else(|| X402Error::requirements(Diag::TokenNotAllowlisted, "the asset is not an accepted token"))?;
                if !t.is_merchant_capable() {
                    return Err(X402Error::requirements(Diag::TokenNotAllowlisted, "a KRON token cannot be received"));
                }
            }
            Profile::Additive => return Err(X402Error::requirements(Diag::UnsupportedKaspaExactProfile, "additive is not supported")),
        }
        match r.route_binding() {
            None if r.has_route() => Err(X402Error::requirements(Diag::RouteUnsupported, "extra.route without a binding")),
            None => Ok(()),
            Some(BINDING_SWAP) if self.policy.swap_enabled => kob_x402::swap::parse_route_offer(r).map(|_| ()),
            Some(BINDING_INTENT) if self.policy.swap_enabled && self.intents.is_some() => {
                kob_x402::intent::parse_intent_offer(r).map(|_| ())
            }
            Some(b) => Err(X402Error::requirements(Diag::RouteUnsupported, format!("route binding {b} is not served here"))),
        }
    }

    /// `POST /invoices` (merchant-authenticated; `allowed` checks the merchant's `payTo` / asset allowlist).
    /// Returns `{ id, url, invoice, created }`.
    pub fn register_invoice(&self, merchant: &str, inv: Invoice, allowed: &dyn Fn(&PaymentRequirements) -> bool) -> Result<Value> {
        let rt = self.inv_rt()?;
        let now = self.clock.now_ms();
        let expires_ms = inv.validate(self.policy.network, now, rt.max_lifetime_ms)?;
        for (i, r) in inv.accepts.iter().enumerate() {
            self.offerable(r).map_err(|e| X402Error::requirements(Diag::InvalidInvoice, format!("accepts[{i}]: {}", e.message)))?;
            if !allowed(r) {
                return Err(X402Error::new(
                    Reason::InvalidPaymentRequirements,
                    Diag::Unauthorized,
                    format!("accepts[{i}]: this API key may not request that payTo or asset"),
                ));
            }
        }
        if rt.store.open_count(merchant, now) >= rt.max_open_per_merchant {
            return Err(X402Error::state(Diag::RateLimited, "too many open invoices for this merchant").retryable());
        }
        let id = inv.id_hex()?;
        let (rec, created) = rt.store.insert(InvoiceRecord {
            id: id.clone(),
            merchant: merchant.to_string(),
            invoice: inv,
            expires_ms,
            created_ms: now,
            extra: vec![],
        })?;
        if rec.merchant != merchant {
            // the same content registered by another merchant: the id is taken
            return Err(X402Error::state(Diag::Replay, "this invoice is registered by another merchant"));
        }
        if created {
            Metrics::inc(&self.metrics.invoices_registered);
        }
        Ok(json!({ "id": id, "url": self.invoice_url(&id), "invoice": rec.invoice, "created": created }))
    }

    /// The URL an invoice is served at (relative when no public URL is configured).
    pub fn invoice_url(&self, id: &str) -> String {
        match self.invoices.as_ref().and_then(|r| r.public_url.as_deref()) {
            Some(base) => kob_x402::invoice::invoice_url(base, id),
            None => format!("/invoices/{id}"),
        }
    }

    /// `GET /invoices/<id>`: the invoice (its canonical content hashes to the id).
    pub fn get_invoice(&self, id: &str) -> Result<Invoice> {
        let rt = self.inv_rt()?;
        rt.store
            .get(&id.to_ascii_lowercase())
            .map(|r| r.invoice)
            .ok_or_else(|| X402Error::state(Diag::InvoiceUnknown, "unknown invoice"))
    }

    /// `GET /invoices/<id>/status`.
    pub fn invoice_status(&self, id: &str) -> Result<InvoiceStatus> {
        let rt = self.inv_rt()?;
        let id = id.to_ascii_lowercase();
        let rec = rt.store.get(&id).ok_or_else(|| X402Error::state(Diag::InvoiceUnknown, "unknown invoice"))?;
        let entries = self.ledger.entries_of_invoice(&id);
        let now = self.clock.now_ms();
        let attempts: Vec<InvoiceAttempt> = entries
            .iter()
            .map(|e| InvoiceAttempt { transaction: e.txid.clone(), state: e.state.as_str().into(), reason: e.reason.clone() })
            .collect();
        let mut extra: Vec<ExtraPayment> = rec
            .extra
            .iter()
            .map(|x| ExtraPayment {
                kind: x.kind.clone(),
                transaction: x.txid.clone(),
                payer: x.payer.clone(),
                observed: x.observed.clone(),
                accepted_daa_score: x.accepted_daa.map(|d| d.to_string()),
                at: iso_from_ms(x.at_ms),
            })
            .collect();
        let accepted: Vec<&crate::x402::ledger::Entry> = entries.iter().filter(|e| e.state == State::Accepted).collect();
        // more than one settled attempt (only possible through a reorg race): the later ones are duplicates
        for e in accepted.iter().skip(1) {
            extra.push(ExtraPayment {
                kind: "duplicate".into(),
                transaction: e
                    .response
                    .as_ref()
                    .and_then(|r| r.get("transaction"))
                    .and_then(Value::as_str)
                    .unwrap_or(&e.txid)
                    .to_string(),
                payer: e.payer.clone(),
                observed: "accepted".into(),
                accepted_daa_score: e.accepted_daa.map(|d| d.to_string()),
                at: iso_from_ms(e.updated_ms),
            });
        }
        let payment = accepted.first().map(|e| {
            let response = e.response.clone();
            let transaction =
                response.as_ref().and_then(|r| r.get("transaction")).and_then(Value::as_str).unwrap_or(&e.txid).to_string();
            let accepted_index = rec
                .invoice
                .accepts
                .iter()
                .position(|r| kob_x402::canonical::requirements_hash(r).map(|h| hex(&h) == e.requirements_hash).unwrap_or(false));
            InvoicePayment {
                transaction,
                accepted_daa_score: e.accepted_daa.map(|d| d.to_string()),
                payer: e.payer.clone(),
                accepted_index: accepted_index.unwrap_or(0),
                response,
            }
        });
        let live = entries.iter().any(|e| matches!(e.state, State::Pending | State::Broadcast | State::Ambiguous));
        let status = if payment.is_some() {
            InvoiceState::Paid
        } else if live {
            InvoiceState::Pending
        } else if now >= rec.expires_ms {
            InvoiceState::Expired
        } else if !entries.is_empty() {
            InvoiceState::Failed
        } else {
            InvoiceState::Unpaid
        };
        Ok(InvoiceStatus {
            id: rec.id.clone(),
            reference: rec.invoice.reference.clone(),
            status,
            expires_at: rec.invoice.expires_at.clone(),
            payment,
            attempts,
            extra_payments: extra,
        })
    }

    /// Verifies a refused payment (a forged one is just rejected) and keeps it as evidence.
    fn refuse(&self, rec: &InvoiceRecord, req: &FacilitatorRequest, kind: &str, err: X402Error) -> Result<SettlementResponse> {
        let rt = self.inv_rt()?;
        let ctx = kob_x402::verify::VerifyCtx { chain: &*self.chain, clock: &*self.clock, policy: &self.policy };
        let rh = req.request_hash.clone().unwrap_or_default();
        // A late payment's authorization may itself have expired: verify with the clock at the invoice's expiry
        // would be a lie; only a currently valid payment is evidence worth keeping, others are rejected as they are.
        let verified = kob_x402::verify::verify_payment(&ctx, &req.payment_requirements, &req.payment_payload, &rh);
        Metrics::inc(&self.metrics.invoice_refused);
        if let Ok(v) = verified {
            // Kept once per funding (spent outputs) and at most `max_extra_payments` per invoice: anyone can submit refused payments
            // (one funded output gives endless fee variants, each a new txid), and every kept one rewrites the record and
            // joins the reconcile watch list. The caller holds the invoice lock: re-read the record under it.
            let cur = rt.store.get(&rec.id).unwrap_or_else(|| rec.clone());
            let txid = hex(&v.txid);
            let mut spends: Vec<String> = v.consumed.iter().map(|o| o.to_string()).collect();
            spends.sort();
            let refused = cur.extra.iter().filter(|e| e.kind != "released").count();
            // a fee variant of a kept payment spends the same outputs (only one of them can reach the chain)
            let known = cur.extra.iter().any(|e| e.txid == txid || (!spends.is_empty() && e.spends == spends));
            if known || refused >= rt.max_extra_payments {
                if !known {
                    Metrics::inc(&self.metrics.invoice_evidence_dropped);
                }
                return Err(err);
            }
            let is_intent =
                matches!(v.kind, kob_x402::verify::PaymentKind::IntentToKas | kob_x402::verify::PaymentKind::IntentToToken);
            let x = ExtraRecord {
                kind: kind.into(),
                txid,
                payer: v.payer_address.clone(),
                watch_txid: hex(&v.merchant_output.outpoint.txid),
                watch_index: v.merchant_output.outpoint.index,
                watch_spk: spk_to_hex(&v.merchant_output.script_public_key),
                intent: is_intent,
                observed: "refused".into(),
                accepted_daa: None,
                at_ms: self.clock.now_ms(),
                spends,
            };
            rt.store.update(&rec.id, |r| {
                if !r.extra.iter().any(|e| e.txid == x.txid) {
                    r.extra.push(x);
                }
            })?;
        }
        Err(err)
    }

    /// `POST /invoices/<id>/pay`: settles `payload` for the invoice (see the module docs).
    pub fn pay_invoice(&self, id: &str, payload: PaymentPayload) -> SettlementResponse {
        match self.pay_invoice_inner(id, payload) {
            Ok(r) => r,
            Err(e) => SettlementResponse::failure(self.policy.network, None, &e),
        }
    }

    fn pay_invoice_inner(&self, id: &str, payload: PaymentPayload) -> Result<SettlementResponse> {
        if self.killed() {
            return Err(X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, "the facilitator is disabled by its operator")
                .retryable());
        }
        let rt = self.inv_rt()?;
        let id = id.to_ascii_lowercase();
        let rec = rt.store.get(&id).ok_or_else(|| X402Error::state(Diag::InvoiceUnknown, "unknown invoice"))?;
        let idx = rec.invoice.find(&payload.accepted)?.ok_or_else(|| {
            X402Error::requirements(Diag::InvalidKaspaX402Accepted, "the payment does not accept one of the invoice's requirements")
        })?;
        let req = FacilitatorRequest {
            x402_version: kob_x402::wire::X402_VERSION,
            payment_requirements: rec.invoice.accepts[idx].clone(),
            payment_payload: payload,
            request_hash: Some(id.clone()),
            resource: None,
        };
        // Never wait for the lock: a payment of this invoice being decided (or observed, for up to the settle wait) answers
        // every other request at once, so requests for one invoice cannot pile up behind it.
        let Some(_g) = self.invoice_locks.try_lock(&id) else {
            return Err(
                X402Error::state(Diag::InvoicePending, "a payment of this invoice is being settled; check its status").retryable()
            );
        };
        let this_tx = kob_x402::common::parse_tx(
            &kob_x402::verify::VerifyCtx { chain: &*self.chain, clock: &*self.clock, policy: &self.policy },
            &req.payment_payload,
        )
        .ok()
        .map(|p| hex(&p.tx.id().as_bytes()));
        let entries = self.ledger.entries_of_invoice(&id);
        let mine = this_tx.as_ref().is_some_and(|t| entries.iter().any(|e| &e.txid == t && e.state != State::Failed));
        if !mine {
            if let Some(paid) = entries.iter().find(|e| e.state == State::Accepted) {
                let err = X402Error::state(Diag::InvoicePaid, "the invoice is already paid; this payment was not broadcast")
                    .with_details(json!({ "paidBy": paid.response.as_ref().and_then(|r| r.get("transaction")).cloned() }));
                return self.refuse(&rec, &req, "duplicate", err);
            }
            if entries.iter().any(|e| matches!(e.state, State::Pending | State::Broadcast | State::Ambiguous)) {
                return Err(
                    X402Error::state(Diag::InvoicePending, "a payment of this invoice is being settled; check its status").retryable()
                );
            }
            if self.clock.now_ms() >= rec.expires_ms {
                let err = X402Error::state(Diag::InvoiceExpired, "the invoice has expired; this payment was not broadcast");
                return self.refuse(&rec, &req, "late", err);
            }
        }
        let ctx = InvoiceCtx { id: id.clone(), expires_ms: rec.expires_ms };
        Ok(self.settle_for(&rec.merchant, &req, Some(&ctx)))
    }

    /// C5 X-9: fails an `ambiguous` direct payment the node does not know (its outpoints are released) and, for an invoice
    /// payment, keeps watching its merchant output as a `released` extra payment, so its late acceptance is still reported.
    /// True when the entry was failed.
    pub(super) fn release_ambiguous(&self, e: &crate::x402::ledger::Entry, now: u64) -> bool {
        let reason = "ambiguous: unknown to the node after the grace period (not in the mempool, merchant output not on chain); outpoints released";
        if self.ledger.transition(&e.txid, State::Failed, Some(reason.into()), now).is_err() {
            return false;
        }
        if let (Some(inv), Some(rt)) = (e.invoice.as_deref(), self.invoices.as_ref()) {
            let x = ExtraRecord {
                kind: "released".into(),
                txid: e.txid.clone(),
                payer: e.payer.clone(),
                watch_txid: e.watched.txid.clone(),
                watch_index: e.watched.index,
                watch_spk: e.watched.spk.clone(),
                intent: false,
                observed: "refused".into(),
                accepted_daa: None,
                at_ms: now,
                spends: vec![],
            };
            if let Err(err) = rt.store.update(inv, |r| {
                if !r.extra.iter().any(|y| y.txid == x.txid) {
                    r.extra.push(x);
                }
            }) {
                eprintln!("x402: invoice {inv}: cannot record the released payment {}: {err}", e.txid);
            }
        }
        true
    }

    /// Reconcile step for invoices: refused payments that reached the chain anyway are reported.
    pub(super) fn reconcile_invoices(&self) -> usize {
        let Some(rt) = self.invoices.as_ref() else { return 0 };
        let now = self.clock.now_ms();
        let mut seen = 0;
        for rec in rt.store.recent(now.saturating_sub(WATCH_EXTRA_MS)) {
            for x in rec.extra.iter().filter(|x| x.observed == "refused" || (x.intent && x.observed == "intent-on-chain")) {
                let (Some(txid), Ok(spk)) = (kob_x402::wire::parse_hash32(&x.watch_txid), spk_from_hex(&x.watch_spk)) else {
                    continue;
                };
                let out = Outpoint::new(txid, x.watch_index);
                let st: OutputStatus = match self.chain.output_status(&out, &spk) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let observed = match (st, x.intent) {
                    (OutputStatus::Accepted { .. }, false) => "accepted",
                    (OutputStatus::Accepted { .. }, true) if x.observed == "refused" => "intent-on-chain",
                    _ => continue,
                };
                let daa = match st {
                    OutputStatus::Accepted { block_daa_score } => Some(block_daa_score),
                    _ => None,
                };
                let txid_s = x.txid.clone();
                if rt
                    .store
                    .update(&rec.id, |r| {
                        if let Some(e) = r.extra.iter_mut().find(|e| e.txid == txid_s) {
                            e.observed = observed.into();
                            e.accepted_daa = daa;
                        }
                    })
                    .is_ok()
                {
                    eprintln!("x402: invoice {}: a refused {} payment {} is on chain ({observed}); refund it", rec.id, x.kind, x.txid);
                    seen += 1;
                }
            }
        }
        seen
    }
}
