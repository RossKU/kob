//! The facilitator's chain view inside `kob-executor run`: node RPC for UTXO facts and submission,
//! and the indexer's acceptance tracking (the same selected-chain follower the matcher uses) for
//! finality.
//!
//! With the tracker, a settlement is `accepted` when a chain block of the current selected chain
//! accepted the transaction, which stays true when the merchant spends the output right away (the
//! UTXO observation alone would lose it) and turns false when that block is reorged away. The UTXO
//! observation remains the fallback for transactions the tracker did not follow from before their
//! broadcast (a restart) and while the indexer catches up.

use std::sync::{Arc, Mutex, MutexGuard};

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, Outpoint, OutputStatus, SubmitError, Tracked, Txid};

use crate::hex::Hash32;
use crate::indexer::ingest::{Ingest, Watcher};

/// [`ChainView`] over a node view (`inner`) and the indexer's acceptance tracking.
pub struct IndexedChain<C: ChainView> {
    inner: C,
    ingest: Arc<Mutex<Ingest>>,
}

impl<C: ChainView> IndexedChain<C> {
    pub fn new(inner: C, ingest: Arc<Mutex<Ingest>>) -> Self {
        IndexedChain { inner, ingest }
    }

    /// The node view.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    fn lock(&self) -> MutexGuard<'_, Ingest> {
        // a poisoned lock leaves SQLite and the watch map consistent (see `executor::IndexerSource`)
        self.ingest.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<C: ChainView> ChainView for IndexedChain<C> {
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        self.inner.utxos(wanted)
    }

    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        self.inner.utxos_of(spk)
    }

    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        self.inner.virtual_daa_score()
    }

    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        if let Tracked::Accepted { block_daa_score } = self.tracked(&out.txid) {
            return Ok(OutputStatus::Accepted { block_daa_score });
        }
        self.inner.output_status(out, spk)
    }

    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError> {
        self.inner.in_mempool(txid)
    }

    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        self.track(&tx.id().as_bytes());
        self.inner.submit(tx)
    }

    fn replace(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        self.track(&tx.id().as_bytes());
        self.inner.replace(tx)
    }

    fn track(&self, txid: &Txid) {
        self.lock().watch_tx_for(Watcher::Facilitator, Hash32(*txid));
    }

    fn untrack(&self, txid: &Txid) {
        self.lock().unwatch_tx_for(Watcher::Facilitator, &Hash32(*txid));
    }

    fn tracked(&self, txid: &Txid) -> Tracked {
        match self.lock().acceptance_of(&Hash32(*txid)) {
            None => Tracked::Untracked,
            Some(None) => Tracked::NotAccepted,
            Some(Some((_, daa))) => Tracked::Accepted { block_daa_score: daa },
        }
    }
}
