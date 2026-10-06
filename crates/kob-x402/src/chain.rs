//! The trusted chain view the verifiers and the facilitator work against.
//!
//! Nothing a payer supplies (UTXO hints, ids, finality claims) is authoritative; every fact comes
//! through [`ChainView`]. The facilitator implements it over node RPC (`kob-executor`), tests over
//! [`crate::testkit::MockChain`].

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kaspa_consensus_core::Hash;

/// 32-byte transaction id in display order (`TransactionId::as_bytes`).
pub type Txid = [u8; 32];

/// An outpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Outpoint {
    pub txid: Txid,
    pub index: u32,
}

impl Outpoint {
    pub fn new(txid: Txid, index: u32) -> Self {
        Outpoint { txid, index }
    }
    pub fn to_json(&self) -> crate::wire::OutpointJson {
        crate::wire::OutpointJson { txid: crate::wire::hex(&self.txid), index: self.index }
    }
}

impl std::fmt::Display for Outpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", crate::wire::hex(&self.txid), self.index)
    }
}

/// An unspent output as the trusted chain reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainUtxo {
    pub amount: u64,
    pub script_public_key: ScriptPublicKey,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
    pub covenant_id: Option<[u8; 32]>,
}

impl ChainUtxo {
    pub fn to_entry(&self) -> UtxoEntry {
        UtxoEntry::new(
            self.amount,
            self.script_public_key.clone(),
            self.block_daa_score,
            self.is_coinbase,
            self.covenant_id.map(Hash::from_bytes),
        )
    }
}

/// Where a transaction output stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStatus {
    /// Neither in the accepted state nor in the mempool (as far as the view can tell).
    Unknown,
    /// The creating transaction is in the node's mempool.
    Mempool,
    /// The output is in the virtual UTXO set: the transaction is in accepted chain state.
    Accepted { block_daa_score: u64 },
}

/// A lookup failure (node unreachable, malformed reply). Verification fails closed on it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("chain view: {0}")]
pub struct ChainError(pub String);

/// Why a submission did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// The node already has this transaction (mempool or accepted): benign for idempotent resends.
    AlreadyKnown,
    /// An input is already spent (also in the mempool by another transaction) or missing.
    Conflict(String),
    /// Any other consensus / policy rejection (mass, fee, script).
    Rejected(String),
    /// The node could not be reached; the outcome is unknown.
    Unavailable(String),
}

/// What an acceptance tracker (an indexer following the selected chain) knows of a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracked {
    /// This view has no tracker, or the transaction is not tracked: use the UTXO observation.
    Untracked,
    /// Tracked, and no chain block of the current selected chain accepted it (or its accepting block
    /// was reorged away). Tracking starts when [`ChainView::track`] is called, so a transaction accepted
    /// before that reads as `NotAccepted`: callers use this only together with the UTXO observation.
    NotAccepted,
    /// A chain block of the current selected chain with this DAA score accepted it.
    Accepted { block_daa_score: u64 },
}

/// Trusted chain facts and transaction submission.
pub trait ChainView: Send + Sync {
    /// Looks up unspent outputs. `wanted` pairs each outpoint with the script it is claimed to be
    /// locked by (the lookup key on address-indexed nodes). `None` = not in the UTXO set (spent,
    /// unknown, or the claimed script is wrong).
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError>;
    /// Every unspent output locked by `spk` (wallet enumeration, acceptance polling).
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError>;
    /// DAA score of the virtual tip.
    fn virtual_daa_score(&self) -> Result<u64, ChainError>;
    /// Status of the output `out` locked by `spk`.
    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError>;
    /// True if `txid` is in the mempool.
    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError>;
    /// Submits a signed transaction (node RPC; never REST, which drops compute budgets).
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError>;
    /// Replaces the mempool transactions that spend an input of `tx` by `tx` (replace-by-fee, the node's
    /// `submitTransactionReplacement`: `tx` must pay a higher fee rate than what it replaces). A view that cannot replace
    /// refuses it (the default).
    fn replace(&self, _tx: &Transaction) -> Result<Txid, SubmitError> {
        Err(SubmitError::Rejected("this chain view cannot replace a mempool transaction".into()))
    }
    /// Starts following the acceptance of `txid` (called before it is submitted, so no chain block is
    /// missed). Views without an acceptance tracker ignore it.
    fn track(&self, _txid: &Txid) {}
    /// Stops following `txid`.
    fn untrack(&self, _txid: &Txid) {}
    /// Where the acceptance tracker places `txid` (see [`Tracked`]); [`Tracked::Untracked`] without one.
    fn tracked(&self, _txid: &Txid) -> Tracked {
        Tracked::Untracked
    }
}

impl<T: ChainView + ?Sized> ChainView for std::sync::Arc<T> {
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        (**self).utxos(wanted)
    }
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        (**self).utxos_of(spk)
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        (**self).virtual_daa_score()
    }
    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        (**self).output_status(out, spk)
    }
    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError> {
        (**self).in_mempool(txid)
    }
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        (**self).submit(tx)
    }
    fn replace(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        (**self).replace(tx)
    }
    fn track(&self, txid: &Txid) {
        (**self).track(txid)
    }
    fn untrack(&self, txid: &Txid) {
        (**self).untrack(txid)
    }
    fn tracked(&self, txid: &Txid) -> Tracked {
        (**self).tracked(txid)
    }
}

/// Wall clock in unix milliseconds.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// The system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
}

/// A settable clock for tests.
pub struct FixedClock(pub std::sync::atomic::AtomicU64);

impl FixedClock {
    pub fn new(ms: u64) -> Self {
        FixedClock(std::sync::atomic::AtomicU64::new(ms))
    }
    pub fn set(&self, ms: u64) {
        self.0.store(ms, std::sync::atomic::Ordering::SeqCst)
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
