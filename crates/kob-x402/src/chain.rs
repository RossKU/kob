//! The trusted chain view that the verifiers and the facilitator work against.
//!
//! Nothing a payer supplies is authoritative. That covers UTXO hints, ids and finality claims.
//! Every fact comes through [`ChainView`]. The facilitator implements it over node RPC in `kobd`,
//! and tests implement it with [`crate::testkit::MockChain`].

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
    /// Neither in the accepted state nor in the mempool, as far as the view can tell.
    Unknown,
    /// The creating transaction is in the node's mempool.
    Mempool,
    /// The output is in the virtual UTXO set, so its transaction is in the accepted chain state.
    Accepted { block_daa_score: u64 },
}

/// A lookup failure, such as an unreachable node or a malformed reply.
///
/// Verification fails closed on it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("chain view: {0}")]
pub struct ChainError(pub String);

/// Why a submission did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// The node already has this transaction, in the mempool or accepted.
    ///
    /// Idempotent resends hit this.
    AlreadyKnown,
    /// An input is missing or already spent, including a spend by another mempool transaction.
    Conflict(String),
    /// Any other consensus or policy rejection, such as mass, fee or script.
    Rejected(String),
    /// The node could not be reached, so the outcome is unknown.
    Unavailable(String),
}

/// What an acceptance tracker knows of a transaction.
///
/// A tracker is an indexer that follows the selected chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracked {
    /// This view has no tracker, or the transaction is not tracked. Use the UTXO observation.
    Untracked,
    /// Tracked, and no block of the current selected chain accepted it.
    ///
    /// This also covers a transaction whose accepting block was reorged away. Tracking starts at
    /// [`ChainView::track`], so a transaction accepted before that call reads as `NotAccepted`.
    /// Callers therefore use this value only together with the UTXO observation.
    NotAccepted,
    /// A chain block of the current selected chain with this DAA score accepted it.
    Accepted { block_daa_score: u64 },
}

/// What a tracker saw of the spend of an outpoint watched by [`ChainView::watch_spend`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpentBy {
    /// This view has no tracker, or the outpoint is not watched, so nothing is known.
    ///
    /// A restart forgets all watches.
    Untracked,
    /// Watched, and no transaction of the current selected chain has spent it yet.
    Unspent,
    /// A transaction of the current selected chain spent it, accepted at this DAA score.
    ///
    /// `input_index` is the input of that transaction that spent the outpoint, and `sig_script`
    /// that input's signature script. `outputs` are all the outputs of the transaction, in order.
    Spent { txid: Txid, block_daa_score: u64, input_index: u32, sig_script: Vec<u8>, outputs: Vec<SpentOutput> },
}

/// An output of a transaction that spent a watched outpoint.
///
/// It records the script, the KAS value and the covenant id the output is bound to. A token output
/// carries its token's id, and a plain output carries none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpentOutput {
    pub script_public_key: ScriptPublicKey,
    pub value: u64,
    pub covenant_id: Option<[u8; 32]>,
}

/// Trusted chain facts and transaction submission.
pub trait ChainView: Send + Sync {
    /// Looks up unspent outputs.
    ///
    /// `wanted` pairs each outpoint with the script it is claimed to be locked by. Address-indexed
    /// nodes use that script as the lookup key. An entry is `None` when the outpoint is not in the
    /// UTXO set, because it is spent, unknown or claimed under the wrong script.
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError>;
    /// Every unspent output locked by `spk`. Used for wallet enumeration and acceptance polling.
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError>;
    /// DAA score of the virtual tip.
    fn virtual_daa_score(&self) -> Result<u64, ChainError>;
    /// Status of the output `out` locked by `spk`.
    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError>;
    /// True if `txid` is in the mempool.
    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError>;
    /// Submits a signed transaction over node RPC.
    ///
    /// REST is never used because it drops compute budgets.
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError>;
    /// Replaces the mempool transactions that spend an input of `tx` with `tx`.
    ///
    /// This is the node's `submitTransactionReplacement` (replace-by-fee). So `tx` must pay a
    /// higher fee rate than what it replaces. The default refuses, for views that cannot replace.
    fn replace(&self, _tx: &Transaction) -> Result<Txid, SubmitError> {
        Err(SubmitError::Rejected("this chain view cannot replace a mempool transaction".into()))
    }
    /// Starts following the acceptance of `txid`.
    ///
    /// Callers invoke it before submitting, so no chain block is missed. Views without an
    /// acceptance tracker ignore it.
    fn track(&self, _txid: &Txid) {}
    /// Stops following `txid`.
    fn untrack(&self, _txid: &Txid) {}
    /// Where the acceptance tracker places `txid`, see [`Tracked`].
    ///
    /// Without a tracker it is [`Tracked::Untracked`].
    fn tracked(&self, _txid: &Txid) -> Tracked {
        Tracked::Untracked
    }
    /// Starts following which transaction of the selected chain spends `out`.
    ///
    /// `out` is an intent UTXO, which another keeper may execute. Views without a tracker ignore
    /// the call. `false` means the watch table is full and `out` is not followed. The caller must
    /// then not start a payment whose settlement depends on the watch.
    fn watch_spend(&self, _out: &Outpoint) -> bool {
        true
    }
    /// Stops following the spend of `out`.
    fn unwatch_spend(&self, _out: &Outpoint) {}
    /// What the tracker saw of the spend of `out`, see [`SpentBy`].
    ///
    /// Without a tracker it is [`SpentBy::Untracked`].
    fn spent_by(&self, _out: &Outpoint) -> SpentBy {
        SpentBy::Untracked
    }
    /// DAA score the tracker has followed the chain up to.
    ///
    /// It is `None` without a tracker, or when the tracker does not know. Compared with
    /// [`ChainView::virtual_daa_score`], it says whether a spend not yet reported may still come.
    fn follower_daa(&self) -> Option<u64> {
        None
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
    fn watch_spend(&self, out: &Outpoint) -> bool {
        (**self).watch_spend(out)
    }
    fn unwatch_spend(&self, out: &Outpoint) {
        (**self).unwatch_spend(out)
    }
    fn spent_by(&self, out: &Outpoint) -> SpentBy {
        (**self).spent_by(out)
    }
    fn follower_daa(&self) -> Option<u64> {
        (**self).follower_daa()
    }
}

/// Wall clock in Unix milliseconds.
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
