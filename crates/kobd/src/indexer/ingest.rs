//! The write path of the indexer: one atomic commit per VSPC batch.
//!
//! A commit is one SQLite transaction. It holds:
//! - The reverts for the `removed` chain blocks, tip-first.
//! - The applied `added` blocks, in the order the node returned them.
//! - The new cursor.
//! - The pruning of chain-block rows older than the reorg window, and of the KCC-23 candidates
//!   older than the declaration window.
//! - The position of the record log.
//!
//! The log frame, which holds the extracted KOB records of the batch, is appended and fsynced just
//! before the commit. If the commit fails, the frame is cut off again. The cursor never advances on
//! any error.

use super::db::{meta_get, meta_set, DbError, DbResult};
use super::processor::{revert_block, Delta, OpReport, Processor};
use super::recordlog::{LogBatch, LogBlock, LogCursor, LogOp, RecordLog, RecordLogError};
use super::status::IndexEvent;
use crate::hex::Hash32;
use crate::rpc::types::{AddedBlock, ChainBlockHeader, VspcBatch};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

/// Why a batch was not committed.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Log(#[from] RecordLogError),
    #[error("batch was requested from {requested} but the cursor is {cursor}")]
    Stale { requested: Hash32, cursor: Hash32 },
    #[error("the node's removed list does not match the stored chain: {0}")]
    InconsistentRemoved(String),
    #[error("the node reported removed chain blocks but no added blocks; refusing to move the cursor")]
    EmptyAfterReorg,
    #[error("the database has no cursor yet")]
    NoCursor,
}

impl From<rusqlite::Error> for IngestError {
    fn from(e: rusqlite::Error) -> Self {
        IngestError::Db(e.into())
    }
}

/// The chain block the store has followed up to, with its DAA score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Hash of the chain block.
    pub hash: Hash32,
    /// DAA score of the chain block.
    pub daa: u64,
}

/// What one commit did.
#[derive(Debug)]
pub struct Applied {
    /// The event sent to the subscribers.
    pub event: IndexEvent,
    /// Chain blocks the node reported as removed.
    pub removed_hashes: usize,
    /// Removed chain blocks whose rows were reverted.
    pub reverted_rows_blocks: usize,
    /// Removed chain blocks this store does not hold.
    ///
    /// They are older than its reorg window or before its start, so nothing of theirs can be
    /// reverted. Consensus finality bounds a reorg to the window (`IngestConfig::reorg_window_daa`,
    /// 12 h by default). The count is reported, as a warning, rather than guessed at.
    pub unknown_removed: usize,
    /// Chain blocks applied.
    pub added: usize,
    /// Transactions of the batch that touched KOB state.
    pub relevant_txs: u64,
    /// Orders created by the batch.
    pub new_orders: u64,
    /// A placement of the batch announced a release newer than this build, so the notice may have
    /// changed (`super::release`).
    pub release_noticed: bool,
    /// Transactions of the batch recorded as rejects.
    pub rejects: u64,
    /// Log frame number, when a frame was written.
    pub log_n: Option<u64>,
    /// Size of the log frame in bytes.
    pub log_bytes: u64,
    /// The cursor after the commit.
    pub cursor: Cursor,
    /// What the operator operations of the commit did.
    pub ops: OpReport,
}

/// Tunables of the write path.
#[derive(Debug, Clone, Copy)]
pub struct IngestConfig {
    /// Chain blocks older than this many DAA behind the cursor are deleted. This is the reorg and
    /// finality window.
    pub reorg_window_daa: u64,
    /// A batch with nothing to record is still logged when the cursor moved this far since the
    /// last logged cursor, so a rebuild from the log resumes close to the tip.
    pub checkpoint_daa: u64,
    /// KCC-23 candidate preparations older than this many DAA behind the cursor are pruned, see
    /// `super::kcc23::prune`.
    pub declaration_window_daa: u64,
    /// Threads for the pure per-batch work that precedes the single database pass.
    ///
    /// The work is `record::precompute`, and 1 does it inline.
    pub verify_threads: usize,
    /// Lean storage ([`super::lean`]), which `Ingest::lean_prune` applies in batches.
    ///
    /// `None` means the store keeps its history.
    pub lean: Option<super::lean::LeanConfig>,
    /// Archive pruning ([`super::archive`]) of a store that keeps its history.
    ///
    /// `None` means nothing is pruned. A lean store prunes by `lean`.
    pub archive: Option<super::archive::ArchiveConfig>,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            reorg_window_daa: 12 * 3600 * 10,
            checkpoint_daa: 3600 * 10,
            declaration_window_daa: 72 * 3600 * 10,
            verify_threads: verify_threads(0),
            lean: None,
            archive: None,
        }
    }
}

/// The verification threads for a configured value.
///
/// 0 is the available parallelism, at most 4. The database pass stays one thread, and four keep
/// the pure work well ahead of it on a KOB-heavy chain.
pub fn verify_threads(configured: usize) -> usize {
    match configured {
        0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4),
        n => n,
    }
}

/// The chain block that accepted a watched transaction and its DAA score.
pub type Acceptance = (Hash32, u64);

/// Who follows a watched transaction: each consumer keeps and drops its own watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watcher {
    /// The matcher and the keepers (`matcher::run`).
    Matcher,
    /// The x402 facilitator.
    Facilitator,
}

impl Watcher {
    fn bit(self) -> u8 {
        match self {
            Watcher::Matcher => 1,
            Watcher::Facilitator => 2,
        }
    }
}

/// The transaction of the selected chain that spent a watched outpoint ([`Ingest::watch_spend`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spender {
    /// Id of the spending transaction.
    pub txid: Hash32,
    /// The chain block that accepted it.
    pub block: Hash32,
    /// DAA score of that chain block.
    pub daa: u64,
    /// Its outputs.
    pub outputs: Vec<SpenderOutput>,
}

/// An output of a [`Spender`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpenderOutput {
    /// Script public key version.
    pub version: u16,
    /// Script public key script.
    pub script: Vec<u8>,
    /// Value in sompi.
    pub value: u64,
    /// The covenant id the output is bound to.
    pub covenant_id: Option<Hash32>,
}

/// Most outpoints whose spend is followed at once.
const MAX_SPEND_WATCHES: usize = 4_096;

/// A watched transaction: where the selected chain accepts it now, and the consumers following it.
#[derive(Clone, Copy, Debug)]
struct Watch {
    acc: Option<Acceptance>,
    owners: u8,
}

/// The single writer of the indexer database and the record log.
pub struct Ingest {
    pub(crate) conn: Connection,
    pub(crate) proc: Processor,
    log: Option<RecordLog>,
    cfg: IngestConfig,
    /// Transactions that consumers, such as the matcher, asked to have followed, and where the
    /// selected chain accepted them now. In memory only. It is updated after each commit, reorgs
    /// included.
    watch: HashMap<Hash32, Watch>,
    /// Outpoints whose spender consumers asked to have followed, and the transaction of the
    /// selected chain that spent each now. The outpoints are intent UTXOs of the x402 facilitator.
    /// In memory only, like `watch`, and bounded by [`MAX_SPEND_WATCHES`].
    spend_watch: HashMap<(Hash32, u32), Option<Spender>>,
    /// Bumped after every commit (`super::DataGeneration`).
    generation: super::DataGeneration,
}

fn get_cursor(conn: &Connection) -> DbResult<Option<Cursor>> {
    let Some(h) = meta_get(conn, "cursor_hash")? else { return Ok(None) };
    let daa = meta_get(conn, "cursor_daa")?.and_then(|d| d.parse().ok()).unwrap_or(0);
    Ok(Hash32::parse(&h).ok().map(|hash| Cursor { hash, daa }))
}

fn set_cursor(conn: &Connection, c: &Cursor) -> DbResult<()> {
    meta_set(conn, "cursor_hash", &c.hash.to_hex())?;
    meta_set(conn, "cursor_daa", &c.daa.to_string())?;
    Ok(())
}

/// The stored `history_from_daa` marker, see [`Ingest::history_from_daa`].
pub fn history_from_daa(conn: &Connection) -> DbResult<Option<u64>> {
    Ok(meta_get(conn, "history_from_daa")?.and_then(|d| d.parse().ok()))
}

/// The position of the record log that the database has committed, as frames `0..n`.
pub fn records_next_n(conn: &Connection) -> DbResult<u64> {
    Ok(meta_get(conn, "records_next_n")?.and_then(|d| d.parse().ok()).unwrap_or(0))
}

fn logged_daa(conn: &Connection) -> DbResult<u64> {
    Ok(meta_get(conn, "log_cursor_daa")?.and_then(|d| d.parse().ok()).unwrap_or(0))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// `(seq, hash, daa)` of a stored chain block.
pub type StoredBlock = (i64, Hash32, u64);

/// The chain blocks of a batch, live from the node or already extracted from the log.
enum Blocks<'a> {
    Live(&'a [AddedBlock]),
    Logged(&'a [LogBlock]),
}

impl Blocks<'_> {
    fn len(&self) -> usize {
        match self {
            Blocks::Live(b) => b.len(),
            Blocks::Logged(b) => b.len(),
        }
    }

    fn last(&self) -> Option<Cursor> {
        match self {
            Blocks::Live(b) => b.last().map(|b| Cursor { hash: b.header.hash, daa: b.header.daa_score }),
            Blocks::Logged(b) => b.last().map(|b| Cursor { hash: b.hash, daa: b.daa }),
        }
    }
}

impl Ingest {
    /// Wraps a connection, a processor and an optional record log, with the default config.
    pub fn new(conn: Connection, proc: Processor, log: Option<RecordLog>) -> Self {
        Ingest {
            conn,
            proc,
            log,
            cfg: IngestConfig::default(),
            watch: HashMap::new(),
            spend_watch: HashMap::new(),
            generation: Default::default(),
        }
    }

    /// Builds the candle aggregates of every token that has fills and no aggregates.
    ///
    /// See [`super::candles::backfill`].
    pub fn backfill_candles(&self) -> DbResult<super::candles::Backfill> {
        super::candles::backfill(&self.conn, &self.proc.tokens)
    }

    /// Sets the write-path tunables.
    pub fn with_config(mut self, cfg: IngestConfig) -> Self {
        self.cfg = cfg;
        self
    }

    /// The database connection of the writer.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// The data generation of the store, bumped after every commit ([`super::DataGeneration`]).
    pub fn generation(&self) -> &super::DataGeneration {
        &self.generation
    }

    /// Whether this is a lean store, which prunes with `lean_prune`.
    pub fn is_lean(&self) -> bool {
        self.cfg.lean.is_some()
    }

    /// Whether the store prunes, lean or archive.
    ///
    /// The follower then runs [`Ingest::prune_batch`] between its steps.
    pub fn prunes(&self) -> bool {
        self.cfg.lean.is_some() || self.cfg.archive.is_some()
    }

    /// Runs one bounded pruning batch of the store's kind, lean or else archive.
    ///
    /// It returns `None` when the store does not prune.
    pub fn prune_batch(&mut self, node_daa: Option<u64>) -> Result<Option<super::lean::LeanReport>, IngestError> {
        if self.cfg.lean.is_some() {
            return self.lean_prune(node_daa);
        }
        self.archive_prune(node_daa)
    }

    /// Runs one bounded batch of the archive pruning ([`super::archive::prune`]).
    ///
    /// `node_daa` is the position of the node, and the cursor's when unknown. It returns `None`
    /// when the store has no archive pruning. A batch that deleted something bumps the data
    /// generation.
    pub fn archive_prune(&mut self, node_daa: Option<u64>) -> Result<Option<super::lean::LeanReport>, IngestError> {
        let Some(a) = self.cfg.archive else { return Ok(None) };
        let Some(cursor) = get_cursor(&self.conn)? else { return Ok(None) };
        let rep = super::archive::prune(&mut self.conn, &a, cursor.daa, node_daa, self.cfg.reorg_window_daa)?;
        if rep.rows > 0 {
            self.generation.bump();
        }
        Ok(Some(rep))
    }

    /// Runs the archive pruning at the cursor until a batch finds nothing more.
    ///
    /// `kobd db replay` uses it so that the rebuilt store holds what a pruned one does. It returns
    /// the rows deleted.
    pub fn archive_prune_all(&mut self) -> Result<usize, IngestError> {
        let mut rows = 0;
        while let Some(rep) = self.archive_prune(None)? {
            rows += rep.rows;
            if !rep.more {
                break;
            }
        }
        Ok(rows)
    }

    /// Changes the size cap in bytes of a lean store, for tests. 0 means no cap.
    pub fn set_lean_cap(&mut self, bytes: u64) {
        if let Some(l) = self.cfg.lean.as_mut() {
            l.max_db_bytes = bytes;
        }
    }

    /// Runs one bounded batch of the lean pruning ([`super::lean::prune`]).
    ///
    /// `node_daa` is the position of the node, and the cursor's when unknown. It returns `None`
    /// when the store is not lean. A batch that deleted something bumps the data generation.
    pub fn lean_prune(&mut self, node_daa: Option<u64>) -> Result<Option<super::lean::LeanReport>, IngestError> {
        let Some(lean) = self.cfg.lean else { return Ok(None) };
        let Some(cursor) = get_cursor(&self.conn)? else { return Ok(None) };
        let rep = super::lean::prune(&mut self.conn, &lean, cursor.daa, node_daa, self.cfg.reorg_window_daa)?;
        if rep.rows > 0 {
            self.generation.bump();
        }
        Ok(Some(rep))
    }

    /// Follows the acceptance of `txid` for the matcher from now on.
    ///
    /// Call it before submitting the transaction, so that no block is missed.
    pub fn watch_tx(&mut self, txid: Hash32) {
        self.watch_tx_for(Watcher::Matcher, txid)
    }

    /// Follows the acceptance of `txid` for `who` from now on.
    pub fn watch_tx_for(&mut self, who: Watcher, txid: Hash32) {
        self.watch.entry(txid).or_insert(Watch { acc: None, owners: 0 }).owners |= who.bit();
    }

    /// Follows which transaction of the selected chain spends `out` from now on.
    ///
    /// A spend accepted before the call is not seen. It returns `false` when the table is full and
    /// `out` is not followed.
    pub fn watch_spend(&mut self, out: (Hash32, u32)) -> bool {
        if self.spend_watch.contains_key(&out) {
            return true;
        }
        if self.spend_watch.len() < MAX_SPEND_WATCHES {
            self.spend_watch.insert(out, None);
            return true;
        }
        false
    }

    /// Stops following the spend of `out`.
    pub fn unwatch_spend(&mut self, out: &(Hash32, u32)) {
        self.spend_watch.remove(out);
    }

    /// The spender of a watched outpoint.
    ///
    /// It returns `None` when the outpoint is not watched. It returns `Some(None)` when it is
    /// watched and not spent, or its accepting block was reorged away.
    pub fn spender_of(&self, out: &(Hash32, u32)) -> Option<Option<Spender>> {
        self.spend_watch.get(out).cloned()
    }

    /// Stops following `txid` for `who`. Other consumers keep their watch.
    pub fn unwatch_tx_for(&mut self, who: Watcher, txid: &Hash32) {
        if let Some(w) = self.watch.get_mut(txid) {
            w.owners &= !who.bit();
            if w.owners == 0 {
                self.watch.remove(txid);
            }
        }
    }

    /// Where the selected chain accepted a watched transaction.
    ///
    /// It returns `None` when nobody watches it. It returns `Some(None)` when it is watched and
    /// not accepted, or its block was reorged away.
    pub fn acceptance_of(&self, txid: &Hash32) -> Option<Option<Acceptance>> {
        self.watch.get(txid).map(|w| w.acc)
    }

    /// Keeps the matcher's watches to those in `keep` and reports where each is accepted.
    ///
    /// The matcher stops following every transaction not in `keep`. For each kept one the result
    /// holds where the selected chain accepted it now. `None` means it is not accepted, or its
    /// block was reorged away.
    pub fn watched_acceptance(&mut self, keep: &[Hash32]) -> Vec<(Hash32, Option<Acceptance>)> {
        let keep_set: std::collections::HashSet<&Hash32> = keep.iter().collect();
        let bit = Watcher::Matcher.bit();
        self.watch.retain(|k, w| {
            if !keep_set.contains(k) {
                w.owners &= !bit;
            }
            w.owners != 0
        });
        keep.iter().map(|t| (*t, self.watch.get(t).filter(|w| w.owners & bit != 0).and_then(|w| w.acc))).collect()
    }

    /// Updates the watches after a commit.
    ///
    /// Transactions of removed chain blocks lose their acceptance, and transactions of the added
    /// blocks gain it. A transaction re-accepted by the new chain does both.
    fn track_acceptance(&mut self, removed: &[Hash32], blocks: &Blocks<'_>) {
        if self.watch.is_empty() && self.spend_watch.is_empty() {
            return;
        }
        for w in self.watch.values_mut() {
            if w.acc.is_some_and(|(h, _)| removed.contains(&h)) {
                w.acc = None;
            }
        }
        for s in self.spend_watch.values_mut() {
            if s.as_ref().is_some_and(|s| removed.contains(&s.block)) {
                *s = None;
            }
        }
        if let Blocks::Live(added) = blocks {
            for b in *added {
                for t in &b.txs {
                    if let Some(w) = self.watch.get_mut(&t.verbose_data.transaction_id) {
                        w.acc = Some((b.header.hash, b.header.daa_score));
                    }
                    if self.spend_watch.is_empty() {
                        continue;
                    }
                    for i in &t.inputs {
                        let key = (i.previous_outpoint.transaction_id, i.previous_outpoint.index);
                        if let Some(slot) = self.spend_watch.get_mut(&key) {
                            let outputs = t
                                .outputs
                                .iter()
                                .filter_map(|o| {
                                    let spk = &o.script_public_key.0;
                                    Some(SpenderOutput {
                                        version: u16::from_be_bytes([*spk.first()?, *spk.get(1)?]),
                                        script: spk[2..].to_vec(),
                                        value: o.value,
                                        covenant_id: o.covenant.as_ref().map(|c| c.covenant_id),
                                    })
                                })
                                .collect();
                            *slot = Some(Spender {
                                txid: t.verbose_data.transaction_id,
                                block: b.header.hash,
                                daa: b.header.daa_score,
                                outputs,
                            });
                        }
                    }
                }
            }
        }
    }

    /// The processor that applies the records.
    pub fn processor(&self) -> &Processor {
        &self.proc
    }

    /// The stored cursor, or `None` for a database that has none yet.
    pub fn cursor(&self) -> DbResult<Option<Cursor>> {
        get_cursor(&self.conn)
    }

    /// Sets the cursor of a fresh database, for the start mode, without touching any rows.
    pub fn init_cursor(&self, c: &Cursor) -> DbResult<()> {
        set_cursor(&self.conn, c)
    }

    /// Where the history of this store begins, when it does not begin at the pruning point.
    ///
    /// `None` means it read everything the node retained. See `HealthSnapshot::history_from_daa`.
    pub fn history_from_daa(&self) -> DbResult<Option<u64>> {
        history_from_daa(&self.conn)
    }

    /// Records that orders created before `daa` may be missing from this store.
    ///
    /// That happens after a start at the sink or at a block, and after a rebase past unread chain.
    /// The marker only ever moves forward. It returns the value now stored.
    pub fn mark_history_from(&self, daa: u64) -> DbResult<u64> {
        let now = history_from_daa(&self.conn)?.map_or(daa, |d| d.max(daa));
        meta_set(&self.conn, "history_from_daa", &now.to_string())?;
        Ok(now)
    }

    /// The number of the next record-log frame, or `None` without a log.
    pub fn records_next(&self) -> Option<u64> {
        self.log.as_ref().map(|r| r.next_n())
    }

    /// The number of orders in the database.
    pub fn order_count(&self) -> DbResult<u64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// The newer release seen on chain often enough to tell the operator, see `super::release`.
    pub fn newer_release_seen(&self) -> DbResult<Option<super::release::NewerRelease>> {
        super::release::newer_seen(&self.conn)
    }

    /// The newest stored chain blocks, newest first.
    pub fn stored_blocks_desc(&self, limit: usize) -> DbResult<Vec<StoredBlock>> {
        let mut st = self.conn.prepare("SELECT seq, hash, daa FROM blocks ORDER BY seq DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| {
            let h: Vec<u8> = r.get(1)?;
            Ok((r.get::<_, i64>(0)?, Hash32::from_slice(&h).unwrap_or_default(), r.get::<_, i64>(2)? as u64))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn seq_of(conn: &Connection, h: &Hash32) -> DbResult<Option<i64>> {
        Ok(conn.prepare_cached("SELECT seq FROM blocks WHERE hash = ?1")?.query_row([&h.0[..]], |r| r.get(0)).optional()?)
    }

    fn max_seq(conn: &Connection) -> DbResult<i64> {
        Ok(conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM blocks", [], |r| r.get(0))?)
    }

    /// Applies one VSPC response requested from `start`, which must be the cursor.
    pub fn apply_batch(&mut self, start: Hash32, batch: &VspcBatch) -> Result<Applied, IngestError> {
        let cursor = get_cursor(&self.conn)?.ok_or(IngestError::NoCursor)?;
        if start != cursor.hash {
            return Err(IngestError::Stale { requested: start, cursor: cursor.hash });
        }
        if batch.removed.is_empty() && batch.added.is_empty() {
            // At the sink there is nothing to commit, so an idle poll costs no fsync.
            return Ok(Applied {
                event: IndexEvent::default(),
                removed_hashes: 0,
                reverted_rows_blocks: 0,
                unknown_removed: 0,
                added: 0,
                relevant_txs: 0,
                new_orders: 0,
                release_noticed: false,
                rejects: 0,
                log_n: None,
                log_bytes: 0,
                cursor,
                ops: OpReport::default(),
            });
        }
        if let Some(first) = batch.removed.first() {
            if *first != cursor.hash {
                return Err(IngestError::InconsistentRemoved(format!("first removed {first} is not the cursor {}", cursor.hash)));
            }
            if batch.added.is_empty() {
                return Err(IngestError::EmptyAfterReorg);
            }
        }
        self.commit_batch(Some(start), &batch.removed, Blocks::Live(&batch.added), None, true, &[], false, None)
    }

    /// Commits a batch. Live batches, replays and rewinds share it.
    #[allow(clippy::too_many_arguments)]
    fn commit_batch(
        &mut self,
        start: Option<Hash32>,
        removed: &[Hash32],
        blocks: Blocks<'_>,
        forced_cursor: Option<Cursor>,
        strict: bool,
        ops: &[LogOp],
        always_log: bool,
        replayed: Option<u64>,
    ) -> Result<Applied, IngestError> {
        let old_cursor = get_cursor(&self.conn)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut delta = Delta::default();
        let mut reverted = 0usize;
        let mut unknown_removed = 0usize;
        let mut reverted_relevant = false;
        for h in removed {
            let Some(seq) = Self::seq_of(&tx, h)? else {
                unknown_removed += 1;
                continue;
            };
            {
                if seq != Self::max_seq(&tx)? {
                    if strict {
                        return Err(IngestError::InconsistentRemoved(format!("block {h} is stored but not the newest stored block")));
                    }
                    continue;
                }
                reverted_relevant |= revert_block(&tx, seq, &mut delta)?;
                reverted += 1;
            }
        }
        let mut log_blocks = Vec::new();
        match &blocks {
            Blocks::Live(added) => {
                // The pure work of the whole batch comes first, on several threads. The database
                // pass below stays single-threaded.
                let txs: Vec<&[crate::rpc::types::Tx]> = added.iter().map(|b| b.txs.as_slice()).collect();
                let pre = super::record::precompute(&txs, self.cfg.verify_threads);
                for b in *added {
                    let recs = self.proc.extract_and_apply_block_pre(&tx, &b.header, &b.txs, Some(&pre), &mut delta)?;
                    if !recs.is_empty() {
                        log_blocks.push(LogBlock {
                            hash: b.header.hash,
                            daa: b.header.daa_score,
                            blue: b.header.blue_score,
                            ts: b.header.timestamp,
                            txs: recs,
                        });
                    }
                }
            }
            Blocks::Logged(logged) => {
                for b in *logged {
                    let header = ChainBlockHeader {
                        hash: b.hash,
                        daa_score: b.daa,
                        blue_score: b.blue,
                        timestamp: b.ts,
                        ..ChainBlockHeader::default()
                    };
                    self.proc.apply_records(&tx, &header, &b.txs, &mut delta)?;
                }
            }
        }
        let cursor = match (forced_cursor, blocks.last()) {
            (Some(c), _) => c,
            (None, Some(c)) => c,
            (None, None) => old_cursor.ok_or(IngestError::NoCursor)?,
        };
        let mut op_report = OpReport::default();
        for op in ops {
            self.proc.apply_op(&tx, op, cursor.daa, &mut delta, &mut op_report)?;
        }
        // The candle aggregates follow the fills this transaction wrote or reverted.
        super::candles::refresh(&tx, &self.proc.tokens)?;
        set_cursor(&tx, &cursor)?;
        // Chain blocks older than the reorg window are final and no longer kept.
        tx.execute("DELETE FROM blocks WHERE daa < ?1", [cursor.daa.saturating_sub(self.cfg.reorg_window_daa) as i64])?;
        // Delete the recorded outputs that no later input can need: spent and final, or unspent
        // past their retention.
        super::outputs::prune(&tx, cursor.daa, self.cfg.reorg_window_daa)?;
        // Delete the KCC-23 candidate preparations that no genesis spent within the declaration
        // window. A logged genesis carries the preparations it binds, so a replay never depends on
        // this table for them. A replay prunes at the logged cursors only.
        super::kcc23::prune(&tx, cursor.daa.saturating_sub(self.cfg.declaration_window_daa))?;

        let checkpoint_due = blocks.len() > 0 && cursor.daa >= logged_daa(&tx)?.saturating_add(self.cfg.checkpoint_daa);
        let loggable =
            self.log.is_some() && (always_log || reverted_relevant || !log_blocks.is_empty() || !ops.is_empty() || checkpoint_due);
        let mut log_n = None;
        let mut log_len = 0;
        if loggable {
            let log = self.log.as_mut().expect("checked");
            let rec = LogBatch {
                start: start.or(old_cursor.map(|c| c.hash)).unwrap_or_default(),
                // A reorg that touched no KOB row is not recorded, because replay has no rows to
                // revert either.
                removed: if reverted_relevant { removed.to_vec() } else { vec![] },
                cursor: LogCursor { hash: cursor.hash, daa: cursor.daa },
                blocks: log_blocks,
                ops: ops.to_vec(),
            };
            let (n, len) = log.append(&rec, now_ms())?;
            meta_set(&tx, "records_next_n", &(n + 1).to_string())?;
            meta_set(&tx, "log_cursor_daa", &cursor.daa.to_string())?;
            log_n = Some(n);
            log_len = len;
        } else if let Some(n) = replayed {
            // A replayed frame moves the position in the same transaction that applies it. A replay
            // that stops part way, by an interruption, a full disk or a failing frame, resumes
            // after the last frame it committed.
            meta_set(&tx, "records_next_n", &(n + 1).to_string())?;
            meta_set(&tx, "log_cursor_daa", &cursor.daa.to_string())?;
        }
        if let Err(e) = tx.commit() {
            if let (Some(n), Some(log)) = (log_n, self.log.as_mut()) {
                if let Err(t) = log.truncate_to(n) {
                    // The log keeps a frame the database never committed, so no further append may
                    // follow it.
                    tracing::error!(commit = %e, "database commit failed after the record-log append");
                    return Err(IngestError::Log(t));
                }
            }
            return Err(IngestError::Db(e.into()));
        }
        self.generation.bump();
        self.track_acceptance(removed, &blocks);
        if unknown_removed > 0 {
            // The reorg is deeper than the stored chain. The rows of those blocks stay, and so do
            // operator imports stamped block 0.
            tracing::warn!(
                unknown_removed,
                window_daa = self.cfg.reorg_window_daa,
                "the node removed chain blocks this store no longer holds: their rows are not reverted"
            );
        }
        let event = IndexEvent {
            cursor_hash: Some(cursor.hash),
            cursor_daa: cursor.daa,
            reverted_blocks: reverted as u64,
            added_blocks: blocks.len() as u64,
            orders: delta.orders.iter().copied().collect(),
            tokens: delta.tokens.iter().copied().collect(),
            fills: delta.fills.clone(),
        };
        Ok(Applied {
            event,
            removed_hashes: removed.len(),
            reverted_rows_blocks: reverted,
            unknown_removed,
            added: blocks.len(),
            relevant_txs: delta.relevant_txs,
            new_orders: delta.new_orders,
            release_noticed: delta.release_noticed,
            rejects: delta.rejects,
            log_n,
            log_bytes: log_len,
            cursor,
            ops: op_report,
        })
    }

    /// Reverts every stored block newer than `target_seq` and moves the cursor to `target`.
    ///
    /// It is used when the node no longer knows the cursor and an older stored block is still known
    /// to it.
    pub fn rewind_to(&mut self, target_seq: i64, target: Cursor) -> Result<Applied, IngestError> {
        let mut hashes = Vec::new();
        for (seq, hash, _) in self.stored_blocks_desc(usize::MAX >> 1)? {
            if seq > target_seq {
                hashes.push(hash);
            }
        }
        self.commit_batch(None, &hashes, Blocks::Live(&[]), Some(target), true, &[], false, None)
    }

    /// Applies operator operations in one logged commit.
    ///
    /// The operations are a recovery import or a gap reconciliation. With `new_cursor` the cursor
    /// moves too, as in a rebase after a gap. Otherwise it stays.
    pub fn apply_ops(&mut self, ops: Vec<LogOp>, new_cursor: Option<Cursor>) -> Result<Applied, IngestError> {
        let cur = get_cursor(&self.conn)?;
        let forced = new_cursor.or(cur).ok_or(IngestError::NoCursor)?;
        self.commit_batch(None, &[], Blocks::Live(&[]), Some(forced), true, &ops, true, None)
    }

    /// Rebuilds this empty database from record-log frames.
    ///
    /// It returns the number of frames, the relevant transactions and the orders.
    pub fn replay(&mut self, records: Vec<(u64, LogBatch)>) -> Result<(u64, u64, u64), IngestError> {
        let frames = records.last().map_or(0, |(n, _)| n + 1);
        self.replay_frames(records, frames)
    }

    /// [`Ingest::replay`] of a log of `frames` frames, some of which this build may have skipped.
    ///
    /// The skipped frames are `recordlog::Replay::skipped`. The database ends at the end of the log
    /// either way, so the follower appends after the last frame.
    ///
    /// Each frame is applied in one transaction together with the position after it
    /// (`records_next_n`). A replay that stops part way can therefore be run again, and it
    /// continues from the first frame not committed.
    pub fn replay_frames(&mut self, records: Vec<(u64, LogBatch)>, frames: u64) -> Result<(u64, u64, u64), IngestError> {
        let mut run = ReplayRun::default();
        for (n, rec) in records {
            self.replay_frame(&mut run, n, rec)?;
        }
        self.replay_finish(run, frames)
    }

    /// Applies record-log frame `n` of a replay, in one transaction with the position after it.
    ///
    /// It is the step of [`Ingest::replay_frames`] for frames streamed from the log one at a time,
    /// see `recordlog::for_each_from`.
    pub fn replay_frame(&mut self, run: &mut ReplayRun, n: u64, rec: LogBatch) -> Result<(), IngestError> {
        let cursor = Cursor { hash: rec.cursor.hash, daa: rec.cursor.daa };
        run.last_daa = Some(cursor.daa);
        let applied = self.commit_batch(
            Some(rec.start),
            &rec.removed,
            Blocks::Logged(&rec.blocks),
            Some(cursor),
            false,
            &rec.ops,
            false,
            Some(n),
        )?;
        run.relevant += applied.relevant_txs;
        Ok(())
    }

    /// Ends a replay of a log of `frames` frames.
    ///
    /// The database ends at the end of the log, which includes the frames this build skipped after
    /// the last applied one. It returns the number of frames, the relevant transactions and the
    /// orders.
    pub fn replay_finish(&mut self, run: ReplayRun, frames: u64) -> Result<(u64, u64, u64), IngestError> {
        meta_set(&self.conn, "records_next_n", &frames.to_string())?;
        if let Some(daa) = run.last_daa {
            meta_set(&self.conn, "log_cursor_daa", &daa.to_string())?;
        }
        Ok((frames, run.relevant, self.order_count()?))
    }
}

/// A replay in progress, and what it applied so far ([`Ingest::replay_frame`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReplayRun {
    relevant: u64,
    last_daa: Option<u64>,
}

impl ReplayRun {
    /// No frame applied yet.
    pub fn is_empty(&self) -> bool {
        self.last_daa.is_none()
    }
}
