//! The durable replay ledger: an append-only JSONL write-ahead log with an fsync per record and an
//! in-memory index, replayed at startup.
//!
//! One entry per transaction id moves through
//!
//! ```text
//! pending -> broadcast -> accepted
//!    |           |            |
//!    |           +--> ambiguous <---+   (unknown outcome, reorg: consumed evidence is kept)
//!    +--> failed (definitive rejection before or without effect: outpoints are released)
//! ```
//!
//! Every state change appends the complete entry as one line (`{"entry":{...}}`); on replay the last
//! line of a transaction id wins. A record is acknowledged only once its whole line, newline included, is
//! written and fsynced; a write or fsync that fails is cut back off the file ([`append_record`]) and the
//! operation fails, so the next record never continues a partial one. On open, a last line without its newline
//! that does not parse is such an unacknowledged write (a crash mid-append) and is discarded; any other line that
//! does not parse, the last one included, refuses to start (fail closed, docs/ops/executor.md "Ledger recovery").
//!
//! [`Ledger::claim`] is the atomic consume step of the settlement lifecycle:
//!
//! * an outpoint reserved by a *different* non-failed transaction id -> replay conflict;
//! * the same transaction id and request hash -> the existing entry (idempotent; cached response);
//! * the same transaction id and another request hash -> conflict;
//! * a `payment-identifier` is bound to (request hash, profile, transaction id): same id + other request hash ->
//!   `kaspa_payment_identifier_conflict`; same id + same request + another transaction ->
//!   `kaspa_payment_identifier_conflict` while the bound transaction is not failed (the cached outcome of one
//!   transaction is never the answer for another); after it failed the new transaction is settled on its own;
//! * `failed` releases the outpoints; `broadcast`, `accepted` and `ambiguous` never do.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kob_x402::chain::Outpoint;
use kob_x402::error::{Diag, Reason, X402Error};
use kob_x402::wire::{hex, parse_hash32, OutpointJson};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Where a settlement stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Evidence consumed, the transaction may or may not have reached the node.
    Pending,
    /// The node took the transaction (or already knew it); finality is being observed.
    Broadcast,
    /// The required finality was observed; `response` holds the cached settlement response.
    Accepted,
    /// A definitive rejection: the reserved outpoints are released.
    Failed,
    /// Unknown outcome or a reorg: the evidence stays consumed until reconciliation resolves it.
    Ambiguous,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Broadcast => "broadcast",
            State::Accepted => "accepted",
            State::Failed => "failed",
            State::Ambiguous => "ambiguous",
        }
    }
}

/// The merchant output a settlement is observed by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watched {
    pub txid: String,
    pub index: u32,
    /// Version-prefixed script public key hex (`kob_x402::safe_tx::spk_to_hex`).
    pub spk: String,
    pub amount: u64,
}

/// A KOB order outpoint a swap-and-pay transaction spends, with the script it is locked by (needed to
/// re-check that it is still unspent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRef {
    pub txid: String,
    pub index: u32,
    /// Version-prefixed script public key hex.
    pub spk: String,
}

/// Where one execution of an intent stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecState {
    /// Submitted (or about to be): it may still be accepted.
    Live,
    /// It can never be accepted (an input spent elsewhere, rejected, or evicted and replaced).
    Dead,
    /// Its merchant output reached the required finality.
    Accepted,
}

/// One execution of an intent by this facilitator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecRecord {
    pub txid: String,
    /// Index of the merchant's output in the execution.
    pub merchant_index: u32,
    /// The orders it spends.
    #[serde(default)]
    pub orders: Vec<OrderRef>,
    pub state: ExecState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at_ms: u64,
}

/// The intent of an intent payment: what was verified, and the executions so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentRecord {
    pub facts: kob_x402::intent::IntentFacts,
    #[serde(default)]
    pub executions: Vec<ExecRecord>,
    /// Order outpoints lost to conflicting fills (later plans skip them).
    #[serde(default)]
    pub lost: Vec<OutpointJson>,
    /// DAA score of the block that accepted the creation, once the intent UTXO was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_daa: Option<u64>,
    /// The expiry of an intent that ended unexecuted (`intent_expired`): the facilitator expires it from its
    /// deadline on, which returns the payer's KAS and tokens; the entry itself is already `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<ExpiryRecord>,
}

/// The facilitator's expiry of an unexecuted intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpiryRecord {
    /// Tried on every reconcile until this time (unix ms; counted from when the facilitator gave the intent up, never
    /// before the deadline); after it the expiry is still retried while the intent stands, throttled to one try per
    /// `EXPIRY_RETRY_MS` (`facilitator/intent.rs`).
    pub until_ms: u64,
    /// The last expiry transaction submitted (hex txid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txid: Option<String>,
    /// When the last expiry was built and submitted (unix ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_try_ms: Option<u64>,
    /// The fee rate (sompi per gram) the submitted expiry pays: a replacement must pay more (C5 X-8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<u64>,
    /// How it ended: `expired` (our expiry accepted), `spent` (the intent was spent by another transaction),
    /// `never_created` (the creation never reached the chain within the window) or `unbuildable: <why>` (no expiry can be
    /// built, the payer's cancel remains). `abandoned` is written by older builds only. `None` while it is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl IntentRecord {
    /// The execution that may still be accepted, if any.
    pub fn live(&self) -> Option<&ExecRecord> {
        self.executions.iter().rev().find(|x| x.state == ExecState::Live)
    }
}

/// One settlement record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub txid: String,
    pub request_hash: String,
    pub requirements_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_id: Option<String>,
    pub profile: String,
    pub kind: String,
    pub merchant: String,
    pub network: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// Advertised amount (decimal string).
    pub amount: String,
    /// Finality the settlement must reach (`accepted` | `confirmed`).
    pub finality: String,
    pub consumed: Vec<OutpointJson>,
    #[serde(default)]
    pub order_inputs: Vec<OrderRef>,
    pub watched: Watched,
    /// `extensions.kaspa` of the eventual success response.
    #[serde(default)]
    pub extension: Map<String, Value>,
    pub state: State,
    /// Why the entry failed / became ambiguous (operator diagnostics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_daa: Option<u64>,
    /// The cached settlement response (set with `accepted`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// Intent payments (`kob-intent-v1`): the intent and its executions. `txid` is the creation's; `watched`
    /// follows the latest execution's merchant output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<IntentRecord>,
    /// Invoice payments: the invoice id (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice: Option<String>,
}

impl Entry {
    /// The consumed outpoints as chain outpoints.
    pub fn consumed_outpoints(&self) -> Vec<Outpoint> {
        self.consumed.iter().filter_map(to_outpoint).collect()
    }
    /// The order outpoints with their scripts, ready for a chain lookup.
    pub fn order_refs(&self) -> Vec<(Outpoint, kaspa_consensus_core::tx::ScriptPublicKey)> {
        self.order_inputs
            .iter()
            .filter_map(|o| Some((Outpoint::new(parse_hash32(&o.txid)?, o.index), kob_x402::safe_tx::spk_from_hex(&o.spk).ok()?)))
            .collect()
    }

    /// True for an intent payment (`kob-intent-v1`): it carries an intent record or its kind names an intent. Such an entry
    /// is paid only by an execution of its intent, never by the creation itself, so it is never finalized as a direct
    /// payment.
    pub fn is_intent(&self) -> bool {
        self.intent.is_some() || self.kind.starts_with("intent")
    }
}

fn to_outpoint(o: &OutpointJson) -> Option<Outpoint> {
    Some(Outpoint::new(parse_hash32(&o.txid)?, o.index))
}

/// Ledger I/O and integrity failures.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger io: {0}")]
    Io(String),
    #[error(
        "ledger corrupt at line {line}: {msg}; it is not an unfinished last write, so the facilitator does not start over it \
         (docs/ops/executor.md, \"Ledger recovery\")"
    )]
    Corrupt { line: usize, msg: String },
    /// A complete record in a format this build does not read (an intent payment recorded for an earlier router template).
    #[error(
        "ledger line {line} holds an intent payment in a format this build does not read ({msg}); stop, archive this ledger \
         (and its invoice store) and start with a new ledger path (docs/ops/executor.md, \"Ledger format\")"
    )]
    Unsupported { line: usize, msg: String },
    #[error("unknown transaction {0}")]
    Unknown(String),
    #[error("illegal ledger transition {from} -> {to}")]
    Transition { from: &'static str, to: &'static str },
}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e.to_string())
    }
}

impl From<LedgerError> for X402Error {
    fn from(e: LedgerError) -> Self {
        // The ledger is the source of truth: when it cannot be written the payment must not proceed.
        X402Error::new(Reason::UnexpectedSettleError, Diag::Internal, e.to_string()).retryable()
    }
}

/// The result of [`Ledger::claim`].
#[derive(Clone, Debug)]
pub enum Claim {
    /// A new entry (or a failed one restarted), state `pending`.
    New(Entry),
    /// An existing non-failed entry for the same request: the caller resumes or returns its outcome.
    Existing(Entry),
}

struct IdBinding {
    request_hash: String,
    profile: String,
    merchant: String,
    txid: String,
}

struct Inner {
    file: Option<File>,
    /// Set when a failed append could not be cut back off the file: nothing more is appended until a restart
    /// (which discards the unfinished last line).
    broken: Option<String>,
    entries: HashMap<String, Entry>,
    by_outpoint: HashMap<Outpoint, String>,
    ids: HashMap<String, IdBinding>,
}

/// The replay ledger.
pub struct Ledger {
    inner: Mutex<Inner>,
    path: Option<PathBuf>,
    /// Exclusive advisory lock (sidecar `<path>.lock`): two facilitators must never append to one ledger.
    _lock: Option<File>,
}

impl Inner {
    fn index(&mut self, e: &Entry) {
        // consumed outpoints follow the state: failed releases, everything else reserves
        for o in e.consumed_outpoints() {
            if e.state == State::Failed {
                if self.by_outpoint.get(&o).is_some_and(|t| t == &e.txid) {
                    self.by_outpoint.remove(&o);
                }
            } else {
                self.by_outpoint.insert(o, e.txid.clone());
            }
        }
        if let Some(id) = &e.payment_id {
            let bind = self.ids.get(id);
            // a failed entry never displaces the binding of a live one
            if e.state != State::Failed || bind.is_none() || bind.is_some_and(|b| b.txid == e.txid) {
                self.ids.insert(
                    id.clone(),
                    IdBinding {
                        request_hash: e.request_hash.clone(),
                        profile: e.profile.clone(),
                        merchant: e.merchant.clone(),
                        txid: e.txid.clone(),
                    },
                );
            }
        }
        self.entries.insert(e.txid.clone(), e.clone());
    }

    fn append(&mut self, e: &Entry) -> Result<(), LedgerError> {
        if let Some(why) = &self.broken {
            return Err(LedgerError::Io(format!("the ledger is not writable until a restart: {why}")));
        }
        if let Some(f) = self.file.as_mut() {
            let mut line = serde_json::to_string(&serde_json::json!({ "entry": e })).map_err(|x| LedgerError::Io(x.to_string()))?;
            line.push('\n');
            if let Err(fail) = append_record(f, line.as_bytes()) {
                if !fail.undone {
                    self.broken = Some(fail.error.to_string());
                }
                return Err(LedgerError::Io(fail.error.to_string()));
            }
        }
        Ok(())
    }

    fn persist_and_index(&mut self, e: Entry) -> Result<Entry, LedgerError> {
        self.append(&e)?;
        self.index(&e);
        Ok(e)
    }
}

/// A failed [`append_record`]: the error, and whether the partial write was cut back off the file.
pub(crate) struct AppendFailure {
    pub error: std::io::Error,
    pub undone: bool,
}

/// Appends one complete record (a line with its newline) to an append-only log and fsyncs it. On any write or fsync
/// error the file is cut back to its length before the write (`set_len`), so a later record never continues a partial
/// one, and the error is returned: the record is not acknowledged. `undone` is false when even the cut failed (the
/// caller then stops appending; a restart discards the unfinished last line).
pub(crate) fn append_record(f: &mut File, line: &[u8]) -> Result<(), AppendFailure> {
    use std::io::{Seek, SeekFrom};
    let start = f.seek(SeekFrom::End(0)).map_err(|error| AppendFailure { error, undone: true })?;
    let Err(error) = f.write_all(line).and_then(|()| f.sync_all()) else { return Ok(()) };
    let undone = f.set_len(start).and_then(|()| f.seek(SeekFrom::Start(start))).and_then(|_| f.sync_all()).is_ok();
    Err(AppendFailure { error, undone })
}

/// The fields a claim supplies (everything is derived from a verified payment).
#[derive(Clone, Debug)]
pub struct NewEntry {
    pub txid: [u8; 32],
    pub request_hash: [u8; 32],
    pub requirements_hash: [u8; 32],
    pub payment_id: Option<String>,
    pub profile: String,
    pub kind: String,
    pub merchant: String,
    pub network: String,
    pub payer: Option<String>,
    pub amount: String,
    pub finality: String,
    pub consumed: Vec<Outpoint>,
    pub order_inputs: Vec<(Outpoint, String)>,
    pub watched: Watched,
    pub extension: Map<String, Value>,
    pub now_ms: u64,
    pub intent: Option<IntentRecord>,
    pub invoice: Option<String>,
}

impl Ledger {
    /// A volatile ledger (tests, `--ledger :memory:`).
    pub fn in_memory() -> Ledger {
        Ledger {
            inner: Mutex::new(Inner {
                file: None,
                broken: None,
                entries: HashMap::new(),
                by_outpoint: HashMap::new(),
                ids: HashMap::new(),
            }),
            path: None,
            _lock: None,
        }
    }

    /// Opens (creating if needed) the log at `path`, replays it and compacts it when it carries much
    /// superseded history.
    pub fn open(path: impl AsRef<Path>) -> Result<Ledger, LedgerError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(format!("{}.lock", path.display()))?;
        lock.try_lock().map_err(|_| LedgerError::Io(format!("{} is locked by another process", path.display())))?;
        let mut raw = Vec::new();
        if path.exists() {
            File::open(&path)?.read_to_end(&mut raw)?;
        }
        let mut inner = Inner { file: None, broken: None, entries: HashMap::new(), by_outpoint: HashMap::new(), ids: HashMap::new() };
        let mut good_len = 0usize; // bytes of complete, valid records
        let mut records = 0usize;
        let mut pos = 0usize;
        let mut line_no = 0usize;
        while pos < raw.len() {
            line_no += 1;
            let (end, terminated) = match raw[pos..].iter().position(|b| *b == b'\n') {
                Some(i) => (pos + i, true),
                None => (raw.len(), false),
            };
            let line = &raw[pos..end];
            let is_last = end + usize::from(terminated) >= raw.len();
            match parse_line(line) {
                Ok(Some(e)) => {
                    inner.index(&e);
                    records += 1;
                    good_len = end + usize::from(terminated);
                    if !terminated {
                        // valid record whose newline was torn off: complete it below
                        good_len = end;
                    }
                }
                Ok(None) => good_len = end + usize::from(terminated), // blank line
                // a complete record of another format is never a torn tail: refuse wherever it is
                Err(LineError::Unsupported(msg)) => return Err(LedgerError::Unsupported { line: line_no, msg }),
                Err(LineError::Bad(msg)) if is_last && !terminated => {
                    // an unfinished last write (crash mid-append; torn or zero-filled): never acknowledged, so drop it.
                    // A newline-terminated line was written whole: one that does not parse is refused like any other.
                    eprintln!("ledger: discarding an unfinished last record at line {line_no}: {msg}");
                    break;
                }
                Err(LineError::Bad(msg)) => return Err(LedgerError::Corrupt { line: line_no, msg }),
            }
            pos = end + 1;
        }
        let mut file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&path)?;
        if (good_len as u64) != raw.len() as u64 {
            file.set_len(good_len as u64)?;
        }
        // position at the end; make sure the last record is newline-terminated
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(0))?;
        if good_len > 0 && raw.get(good_len - 1) != Some(&b'\n') {
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        let live = inner.entries.len();
        inner.file = Some(file);
        let ledger = Ledger { inner: Mutex::new(inner), path: Some(path), _lock: Some(lock) };
        if records > 2 * live + 1_000 {
            ledger.compact()?;
        }
        Ok(ledger)
    }

    /// Rewrites the log with one line per entry (atomic rename).
    pub fn compact(&self) -> Result<(), LedgerError> {
        let Some(path) = &self.path else { return Ok(()) };
        let mut g = self.lock();
        let tmp = path.with_extension("compact.tmp");
        {
            let mut f = File::create(&tmp)?;
            let mut entries: Vec<&Entry> = g.entries.values().collect();
            entries.sort_by(|a, b| (a.created_ms, &a.txid).cmp(&(b.created_ms, &b.txid)));
            for e in entries {
                let mut line =
                    serde_json::to_string(&serde_json::json!({ "entry": e })).map_err(|x| LedgerError::Io(x.to_string()))?;
                line.push('\n');
                f.write_all(line.as_bytes())?;
            }
            f.sync_all()?;
        }
        g.file = None; // release the handle before replacing the file (Windows)
        std::fs::rename(&tmp, path)?;
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(0))?;
        g.file = Some(file);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Atomically consumes the evidence of a verified payment (see the module docs for the rules).
    pub fn claim(&self, n: NewEntry) -> Result<Claim, X402Error> {
        let txid = hex(&n.txid);
        let request_hash = hex(&n.request_hash);
        let mut g = self.lock();

        // (b)/(c): the same transaction id
        if let Some(e) = g.entries.get(&txid) {
            if e.request_hash != request_hash || e.merchant != n.merchant {
                return Err(X402Error::state(Diag::Replay, "this transaction was already consumed by a different request"));
            }
            if e.state != State::Failed {
                return Ok(Claim::Existing(e.clone()));
            }
        }
        // (d): the payment identifier
        if let Some(id) = &n.payment_id {
            if let Some(b) = g.ids.get(id) {
                if b.request_hash != request_hash || b.profile != n.profile || b.merchant != n.merchant {
                    return Err(X402Error::state(
                        Diag::KaspaPaymentIdentifierConflict,
                        "the payment identifier is bound to a different request, profile or merchant",
                    ));
                }
                // the identifier names one payment: another transaction under it is never answered with that
                // payment's outcome (it is refused while the bound one stands, and settles on its own once it failed)
                if b.txid != txid && g.entries.get(&b.txid).is_some_and(|o| o.state != State::Failed) {
                    return Err(X402Error::state(
                        Diag::KaspaPaymentIdentifierConflict,
                        "the payment identifier is bound to another transaction",
                    ));
                }
            }
        }
        // (a): outpoints reserved by another live transaction
        for o in &n.consumed {
            if let Some(t) = g.by_outpoint.get(o).filter(|t| **t != txid) {
                return Err(X402Error::state(Diag::Replay, format!("outpoint {o} is already consumed by transaction {t}"))
                    .with_details(serde_json::json!({ "outpoint": o.to_json() })));
            }
        }
        let created_ms = g.entries.get(&txid).map(|e| e.created_ms).unwrap_or(n.now_ms);
        let e = Entry {
            txid,
            request_hash,
            requirements_hash: hex(&n.requirements_hash),
            payment_id: n.payment_id,
            profile: n.profile,
            kind: n.kind,
            merchant: n.merchant,
            network: n.network,
            payer: n.payer,
            amount: n.amount,
            finality: n.finality,
            consumed: n.consumed.iter().map(Outpoint::to_json).collect(),
            order_inputs: n
                .order_inputs
                .iter()
                .map(|(o, spk)| OrderRef { txid: hex(&o.txid), index: o.index, spk: spk.clone() })
                .collect(),
            watched: n.watched,
            extension: n.extension,
            state: State::Pending,
            reason: None,
            created_ms,
            updated_ms: n.now_ms,
            accepted_daa: None,
            response: None,
            intent: n.intent,
            invoice: n.invoice,
        };
        let e = g.persist_and_index(e).map_err(X402Error::from)?;
        Ok(Claim::New(e))
    }

    /// The entry of a transaction id.
    pub fn get(&self, txid: &str) -> Option<Entry> {
        self.lock().entries.get(txid).cloned()
    }

    /// The live (non-failed) entry a payment identifier is bound to, with its bound request hash.
    pub fn get_by_payment_id(&self, id: &str) -> Option<Entry> {
        let g = self.lock();
        let b = g.ids.get(id)?;
        g.entries.get(&b.txid).cloned()
    }

    /// The (request hash, profile) a payment identifier is bound to.
    pub fn payment_id_binding(&self, id: &str) -> Option<(String, String)> {
        self.lock().ids.get(id).map(|b| (b.request_hash.clone(), b.profile.clone()))
    }

    /// The transaction id a payment identifier is bound to while that transaction is not failed.
    pub fn payment_id_live_txid(&self, id: &str) -> Option<String> {
        let g = self.lock();
        let b = g.ids.get(id)?;
        g.entries.get(&b.txid).filter(|e| e.state != State::Failed).map(|e| e.txid.clone())
    }

    /// Entries currently in one of `states`.
    pub fn entries_in(&self, states: &[State]) -> Vec<Entry> {
        let g = self.lock();
        let mut v: Vec<Entry> = g.entries.values().filter(|e| states.contains(&e.state)).cloned().collect();
        v.sort_by(|a, b| (a.created_ms, &a.txid).cmp(&(b.created_ms, &b.txid)));
        v
    }

    /// KOB order outpoints that swap-and-pay settlements in flight spend (`pending`, `broadcast`, `ambiguous`): the payer
    /// signed those routes over the book already, so the matcher of the same process plans its batches around them.
    pub fn reserved_order_outpoints(&self) -> std::collections::BTreeSet<([u8; 32], u32)> {
        self.entries_in(&[State::Pending, State::Broadcast, State::Ambiguous])
            .iter()
            .flat_map(|e| e.order_inputs.iter().filter_map(|o| Some((parse_hash32(&o.txid)?, o.index))))
            .collect()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if `o` is currently reserved by a non-failed entry.
    pub fn is_consumed(&self, o: &Outpoint) -> bool {
        self.lock().by_outpoint.contains_key(o)
    }

    /// The transaction id (hex) that currently reserves `o`.
    pub fn consumer_of(&self, o: &Outpoint) -> Option<String> {
        self.lock().by_outpoint.get(o).cloned()
    }

    /// Moves an entry to `to`. `failed` releases its outpoints; only the transitions of the module
    /// diagram are legal (`failed` is only left through a new [`Ledger::claim`]).
    pub fn transition(&self, txid: &str, to: State, reason: Option<String>, now_ms: u64) -> Result<Entry, LedgerError> {
        self.transition_with(txid, to, reason, now_ms, |_| {})
    }

    /// [`Ledger::transition`] with an edit of the entry that is persisted in the same record (nothing is
    /// changed in memory unless the transition is legal and the record is durable).
    pub fn transition_with(
        &self,
        txid: &str,
        to: State,
        reason: Option<String>,
        now_ms: u64,
        edit: impl FnOnce(&mut Entry),
    ) -> Result<Entry, LedgerError> {
        let mut g = self.lock();
        let cur = g.entries.get(txid).ok_or_else(|| LedgerError::Unknown(txid.to_string()))?.clone();
        if cur.state == to {
            return Ok(cur);
        }
        use State::*;
        let legal = matches!(
            (cur.state, to),
            (Pending, Broadcast | Failed | Ambiguous)
                | (Broadcast, Accepted | Failed | Ambiguous)
                | (Ambiguous, Broadcast | Accepted | Failed)
                | (Accepted, Ambiguous)
        );
        if !legal {
            return Err(LedgerError::Transition { from: cur.state.as_str(), to: to.as_str() });
        }
        let mut e = cur;
        edit(&mut e);
        e.state = to;
        e.updated_ms = now_ms;
        if reason.is_some() || to == Failed || to == Ambiguous {
            e.reason = reason;
        }
        g.persist_and_index(e)
    }

    /// Persists an edit of an entry without a state change (an intent's executions, its watched output).
    pub fn update(&self, txid: &str, now_ms: u64, edit: impl FnOnce(&mut Entry)) -> Result<Entry, LedgerError> {
        let mut g = self.lock();
        let mut e = g.entries.get(txid).ok_or_else(|| LedgerError::Unknown(txid.to_string()))?.clone();
        edit(&mut e);
        e.updated_ms = now_ms;
        g.persist_and_index(e)
    }

    /// Entries of an invoice, oldest first.
    pub fn entries_of_invoice(&self, invoice: &str) -> Vec<Entry> {
        let g = self.lock();
        let mut v: Vec<Entry> = g.entries.values().filter(|e| e.invoice.as_deref() == Some(invoice)).cloned().collect();
        v.sort_by(|a, b| (a.created_ms, &a.txid).cmp(&(b.created_ms, &b.txid)));
        v
    }

    /// Records the observed acceptance and the cached response (`-> accepted`).
    pub fn set_accepted(&self, txid: &str, accepted_daa: u64, response: Value, now_ms: u64) -> Result<Entry, LedgerError> {
        self.transition_with(txid, State::Accepted, None, now_ms, |e| {
            e.accepted_daa = Some(accepted_daa);
            e.response = Some(response);
        })
    }
}

/// Why a ledger line does not replay.
enum LineError {
    /// Not a record (torn, zero-filled or corrupt).
    Bad(String),
    /// A complete record of an intent payment in a format this build does not read.
    Unsupported(String),
}

fn parse_line(line: &[u8]) -> Result<Option<Entry>, LineError> {
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Rec {
        entry: Entry,
    }
    match serde_json::from_slice::<Rec>(line) {
        // an intent payment is only ever acted on through its intent record: one without it is not replayed as anything else
        Ok(r) if r.entry.kind.starts_with("intent") && r.entry.intent.is_none() => {
            Err(LineError::Unsupported(format!("entry {} of kind {} has no intent record", r.entry.txid, r.entry.kind)))
        }
        Ok(r) => Ok(Some(r.entry)),
        Err(e) => {
            // a complete record whose intent record does not decode: written for an earlier router template
            let intent_fails = serde_json::from_slice::<Value>(line).ok().is_some_and(|v| {
                v.get("entry")
                    .and_then(|x| x.get("intent"))
                    .is_some_and(|i| !i.is_null() && serde_json::from_value::<IntentRecord>(i.clone()).is_err())
            });
            if intent_fails {
                Err(LineError::Unsupported(format!("its intent record does not decode: {e}")))
            } else {
                Err(LineError::Bad(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(n: u8, i: u32) -> Outpoint {
        Outpoint::new([n; 32], i)
    }

    fn new_entry(tx: u8, rh: u8, ids: Option<&str>, consumed: Vec<Outpoint>) -> NewEntry {
        NewEntry {
            txid: [tx; 32],
            request_hash: [rh; 32],
            requirements_hash: [9; 32],
            payment_id: ids.map(str::to_string),
            profile: "standard-native".into(),
            kind: "native".into(),
            merchant: "m1".into(),
            network: "kaspa:testnet-10".into(),
            payer: Some("kaspatest:payer".into()),
            amount: "100".into(),
            finality: "accepted".into(),
            consumed,
            order_inputs: vec![],
            watched: Watched { txid: hex(&[tx; 32]), index: 0, spk: "0000".into(), amount: 100 },
            extension: Map::new(),
            now_ms: 1_000,
            intent: None,
            invoice: None,
        }
    }

    const ID1: &str = "payment-id-0000001";
    const ID2: &str = "payment-id-0000002";

    #[test]
    fn claim_new_then_idempotent() {
        let l = Ledger::in_memory();
        let c = l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0), op(0xa1, 1)])).unwrap();
        assert!(matches!(c, Claim::New(ref e) if e.state == State::Pending));
        // same txid + same request -> existing
        let c = l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0), op(0xa1, 1)])).unwrap();
        assert!(matches!(c, Claim::Existing(_)));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn swap_settlements_in_flight_reserve_their_orders_for_the_matcher() {
        let l = Ledger::in_memory();
        let mut e = new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)]);
        e.order_inputs = vec![(op(0xb0, 2), "0000".into()), (op(0xb1, 0), "0000".into())];
        l.claim(e).unwrap();
        let mut done = new_entry(2, 8, Some(ID2), vec![op(0xa1, 0)]);
        done.order_inputs = vec![(op(0xb2, 1), "0000".into())];
        l.claim(done).unwrap();
        l.transition(&hex(&[2; 32]), State::Failed, None, 2_000).unwrap();
        // the pending swap's two orders; the failed one released its order
        let r = l.reserved_order_outpoints();
        assert_eq!(r, [([0xb0; 32], 2), ([0xb1; 32], 0)].into_iter().collect());
        l.transition(&hex(&[1; 32]), State::Broadcast, None, 3_000).unwrap();
        assert_eq!(l.reserved_order_outpoints().len(), 2, "broadcast still reserves");
    }

    #[test]
    fn same_txid_other_request_conflicts() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap();
        let e = l.claim(new_entry(1, 8, Some(ID2), vec![op(0xa0, 0)])).unwrap_err();
        assert_eq!(e.diag, Diag::Replay);
        assert_eq!(e.reason, Reason::InvalidTransactionState);
    }

    #[test]
    fn outpoint_reserved_by_other_txid_conflicts() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0), op(0xa1, 0)])).unwrap();
        // a different transaction spending one of the same outpoints (even for a different request)
        let e = l.claim(new_entry(2, 8, Some(ID2), vec![op(0xa1, 0), op(0xa2, 0)])).unwrap_err();
        assert_eq!(e.diag, Diag::Replay);
        // and nothing of the rejected claim leaked into the index
        assert!(!l.is_consumed(&op(0xa2, 0)));
        assert!(l.get(&hex(&[2; 32])).is_none());
        assert!(l.payment_id_binding(ID2).is_none());
    }

    #[test]
    fn payment_identifier_binding() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap();
        // same id, other request hash -> identifier conflict
        let e = l.claim(new_entry(2, 8, Some(ID1), vec![op(0xb0, 0)])).unwrap_err();
        assert_eq!(e.diag, Diag::KaspaPaymentIdentifierConflict);
        // same id, other profile -> identifier conflict
        let mut other = new_entry(3, 7, Some(ID1), vec![op(0xb1, 0)]);
        other.profile = "kcc20".into();
        assert_eq!(l.claim(other).unwrap_err().diag, Diag::KaspaPaymentIdentifierConflict);
        // same id, same request, different transaction -> identifier conflict (never the first one's outcome)
        let e = l.claim(new_entry(4, 7, Some(ID1), vec![op(0xb2, 0)])).unwrap_err();
        assert_eq!(e.diag, Diag::KaspaPaymentIdentifierConflict);
        assert!(!l.is_consumed(&op(0xb2, 0)));
        assert!(l.get(&hex(&[4; 32])).is_none());
        // ... also once the first one is accepted
        l.transition(&hex(&[1; 32]), State::Broadcast, None, 2).unwrap();
        l.set_accepted(&hex(&[1; 32]), 5, serde_json::json!({"success": true}), 3).unwrap();
        let e = l.claim(new_entry(4, 7, Some(ID1), vec![op(0xb2, 0)])).unwrap_err();
        assert_eq!(e.diag, Diag::KaspaPaymentIdentifierConflict);
        // the same transaction under the same id is the cached outcome
        assert!(
            matches!(l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap(), Claim::Existing(ref e) if e.txid == hex(&[1; 32]))
        );
    }

    #[test]
    fn ids_and_transactions_are_scoped_to_their_merchant() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap();
        let mut other = new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)]);
        other.merchant = "m2".into();
        assert_eq!(l.claim(other).unwrap_err().diag, Diag::Replay);
        let mut other = new_entry(2, 7, Some(ID1), vec![op(0xb0, 0)]);
        other.merchant = "m2".into();
        assert_eq!(l.claim(other).unwrap_err().diag, Diag::KaspaPaymentIdentifierConflict);
    }

    #[test]
    fn failed_releases_outpoints_and_allows_a_new_attempt() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap();
        l.transition(&hex(&[1; 32]), State::Failed, Some("rejected".into()), 2_000).unwrap();
        assert!(!l.is_consumed(&op(0xa0, 0)));
        // another transaction may now spend the released outpoint
        assert!(matches!(l.claim(new_entry(2, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap(), Claim::New(_)));
        assert!(l.is_consumed(&op(0xa0, 0)));
        // the failed transaction can be retried only after the other one is gone: the outpoint is taken
        l.transition(&hex(&[2; 32]), State::Failed, None, 3_000).unwrap();
        assert!(
            matches!(l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap(), Claim::New(ref e) if e.state == State::Pending)
        );
    }

    #[test]
    fn broadcast_and_ambiguous_never_release() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, None, vec![op(0xa0, 0)])).unwrap();
        l.transition(&hex(&[1; 32]), State::Broadcast, None, 2).unwrap();
        assert!(l.is_consumed(&op(0xa0, 0)));
        l.transition(&hex(&[1; 32]), State::Ambiguous, Some("node vanished".into()), 3).unwrap();
        assert!(l.is_consumed(&op(0xa0, 0)));
        assert!(l.claim(new_entry(2, 7, None, vec![op(0xa0, 0)])).is_err());
        // an ambiguous entry that was reconciled to accepted keeps them too
        l.set_accepted(&hex(&[1; 32]), 5, serde_json::json!({"success": true}), 4).unwrap();
        assert!(l.is_consumed(&op(0xa0, 0)));
        assert_eq!(l.get(&hex(&[1; 32])).unwrap().response.unwrap()["success"], true);
    }

    #[test]
    fn illegal_transitions_are_refused() {
        let l = Ledger::in_memory();
        l.claim(new_entry(1, 7, None, vec![op(0xa0, 0)])).unwrap();
        let t = hex(&[1; 32]);
        assert!(l.transition(&t, State::Accepted, None, 1).is_err(), "pending -> accepted needs a broadcast first");
        l.transition(&t, State::Failed, None, 1).unwrap();
        assert!(l.transition(&t, State::Broadcast, None, 2).is_err(), "failed is terminal except through claim");
        assert!(l.transition("nope", State::Failed, None, 2).is_err());
    }

    #[test]
    fn restart_replays_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        {
            let l = Ledger::open(&p).unwrap();
            l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap();
            l.transition(&hex(&[1; 32]), State::Broadcast, None, 2).unwrap();
            l.claim(new_entry(2, 8, Some(ID2), vec![op(0xb0, 0)])).unwrap();
            l.transition(&hex(&[2; 32]), State::Failed, Some("x".into()), 3).unwrap();
        }
        let l = Ledger::open(&p).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l.get(&hex(&[1; 32])).unwrap().state, State::Broadcast);
        assert_eq!(l.get(&hex(&[2; 32])).unwrap().state, State::Failed);
        assert!(l.is_consumed(&op(0xa0, 0)));
        assert!(!l.is_consumed(&op(0xb0, 0)));
        // bindings survive the restart
        assert_eq!(l.claim(new_entry(9, 9, Some(ID1), vec![op(0xc0, 0)])).unwrap_err().diag, Diag::KaspaPaymentIdentifierConflict);
        assert_eq!(l.payment_id_binding(ID2).unwrap().0, hex(&[8; 32]));
    }

    #[test]
    fn torn_last_line_is_discarded_and_the_log_stays_appendable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        {
            let l = Ledger::open(&p).unwrap();
            l.claim(new_entry(1, 7, None, vec![op(0xa0, 0)])).unwrap();
            l.claim(new_entry(2, 7, None, vec![op(0xa1, 0)])).unwrap();
        }
        // tear the last record in the middle
        let raw = std::fs::read(&p).unwrap();
        let cut = raw.len() - 40;
        std::fs::write(&p, &raw[..cut]).unwrap();
        let l = Ledger::open(&p).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l.get(&hex(&[1; 32])).is_some());
        assert!(!l.is_consumed(&op(0xa1, 0)));
        l.claim(new_entry(3, 7, None, vec![op(0xa2, 0)])).unwrap();
        drop(l);
        let l = Ledger::open(&p).unwrap();
        assert_eq!(l.len(), 2, "the append after the truncation is a clean record");
    }

    #[test]
    fn valid_last_record_missing_only_its_newline_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        {
            let l = Ledger::open(&p).unwrap();
            l.claim(new_entry(1, 7, None, vec![op(0xa0, 0)])).unwrap();
        }
        let mut raw = std::fs::read(&p).unwrap();
        assert_eq!(raw.pop(), Some(b'\n'));
        std::fs::write(&p, &raw).unwrap();
        let l = Ledger::open(&p).unwrap();
        assert_eq!(l.len(), 1);
        l.claim(new_entry(2, 7, None, vec![op(0xa1, 0)])).unwrap();
        drop(l);
        assert_eq!(Ledger::open(&p).unwrap().len(), 2);
    }

    #[test]
    fn corrupt_middle_line_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        {
            let l = Ledger::open(&p).unwrap();
            l.claim(new_entry(1, 7, None, vec![op(0xa0, 0)])).unwrap();
            l.claim(new_entry(2, 7, None, vec![op(0xa1, 0)])).unwrap();
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.insert(1, "{garbage");
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        assert!(matches!(Ledger::open(&p), Err(LedgerError::Corrupt { line: 2, .. })));
    }

    /// An intent payment recorded for an earlier router template (the token-intent lock pin of 2026-10-06 added
    /// `lockAmount` / `lockExtension` to the intent state), a record of an unknown intent format, or an intent entry without
    /// its record: the ledger refuses to start and names the line and what to do, wherever the line is (the last line too: a
    /// complete record is never discarded as a torn tail). Such an entry is never replayed as a direct payment.
    #[test]
    fn an_intent_record_of_another_format_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        {
            let l = Ledger::open(&p).unwrap();
            l.claim(new_entry(1, 7, None, vec![op(0xa0, 0)])).unwrap();
            l.claim(new_entry(2, 7, None, vec![op(0xa1, 0)])).unwrap();
        }
        let raw = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<Value> = raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let earlier =
            serde_json::json!({"facts": {"intent": {"kind": "TokenToKas", "maxSell": "200000000"}}, "executions": [], "lost": []});
        let unknown = serde_json::json!({"version": 99, "anything": [1, 2, 3]});
        for (what, intent, kind) in [
            ("an earlier router template", Some(earlier), "intent-to-kas"),
            ("an unknown intent format", Some(unknown), "intent-to-token"),
            ("an unknown intent format, other kind", Some(serde_json::json!(7)), "native"),
            ("an intent kind without its record", None, "intent-to-kas"),
        ] {
            for at in [0usize, 1] {
                let mut text = String::new();
                for (k, l) in lines.iter().enumerate() {
                    let mut l = l.clone();
                    if k == at {
                        l["entry"]["kind"] = kind.into();
                        l["entry"]["state"] = "broadcast".into();
                        match &intent {
                            Some(i) => l["entry"]["intent"] = i.clone(),
                            None => {
                                l["entry"].as_object_mut().unwrap().remove("intent");
                            }
                        }
                    }
                    text.push_str(&format!("{l}\n"));
                }
                std::fs::write(&p, &text).unwrap();
                let e = Ledger::open(&p).err().unwrap_or_else(|| panic!("{what} at line {} opened", at + 1));
                assert!(matches!(e, LedgerError::Unsupported { line, .. } if line == at + 1), "{what}: {e}");
                assert!(e.to_string().contains("archive"), "{e}");
                assert_eq!(std::fs::read_to_string(&p).unwrap(), text, "{what}: the ledger is left as it is");
            }
        }
        // a really corrupt line is still reported as corruption, and a torn tail is still dropped
        std::fs::write(&p, format!("{}\n{}\n", serde_json::json!({"entry": {"txid": 5}}), lines[1])).unwrap();
        assert!(matches!(Ledger::open(&p), Err(LedgerError::Corrupt { line: 1, .. })));
        std::fs::write(&p, format!("{}\n{{\"entry\":{{\"intent\":{{\"fa", lines[0])).unwrap();
        assert_eq!(Ledger::open(&p).unwrap().len(), 1);
    }

    #[test]
    fn a_second_process_cannot_open_the_same_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let first = Ledger::open(&p).unwrap();
        let e = Ledger::open(&p).err().expect("locked");
        assert!(e.to_string().contains("locked"), "{e}");
        drop(first);
        assert!(Ledger::open(&p).is_ok());
    }

    #[test]
    fn compaction_keeps_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let l = Ledger::open(&p).unwrap();
        l.claim(new_entry(1, 7, Some(ID1), vec![op(0xa0, 0)])).unwrap();
        for _ in 0..5 {
            l.transition(&hex(&[1; 32]), State::Broadcast, None, 2).ok();
            l.transition(&hex(&[1; 32]), State::Ambiguous, None, 3).ok();
        }
        let before = std::fs::read_to_string(&p).unwrap().lines().count();
        l.compact().unwrap();
        let after = std::fs::read_to_string(&p).unwrap().lines().count();
        assert!(after < before);
        l.claim(new_entry(2, 7, None, vec![op(0xa5, 0)])).unwrap();
        drop(l);
        let l = Ledger::open(&p).unwrap();
        assert_eq!(l.len(), 2);
        assert!(l.is_consumed(&op(0xa0, 0)));
    }
}
