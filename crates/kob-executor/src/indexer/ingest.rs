//! The write path: one atomic commit per VSPC batch.
//!
//! A commit contains, in one SQLite transaction: the reverts for `removed` chain blocks (tip-first),
//! the applied `added` blocks (in the order the node returned them), the new cursor, the pruning of
//! chain-block rows older than the reorg window, and the record-log position. The log frame (the
//! extracted KOB records of the batch) is appended and fsynced just before the commit; if the commit
//! fails the frame is cut off again. The cursor never advances on any error.

use super::db::{meta_get, meta_set, DbError, DbResult};
use super::processor::{revert_block, Delta, OpReport, Processor};
use super::recordlog::{LogBatch, LogBlock, LogCursor, LogOp, RecordLog, RecordLogError};
use super::status::IndexEvent;
use crate::hex::Hash32;
use crate::rpc::types::{AddedBlock, ChainBlockHeader, VspcBatch};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub hash: Hash32,
    pub daa: u64,
}

#[derive(Debug)]
pub struct Applied {
    pub event: IndexEvent,
    pub removed_hashes: usize,
    pub reverted_rows_blocks: usize,
    pub added: usize,
    pub relevant_txs: u64,
    pub new_orders: u64,
    pub rejects: u64,
    /// Log frame number and size, when a frame was written.
    pub log_n: Option<u64>,
    pub log_bytes: u64,
    pub cursor: Cursor,
    pub ops: OpReport,
}

/// Tunables of the write path.
#[derive(Debug, Clone, Copy)]
pub struct IngestConfig {
    /// Chain blocks older than this many DAA behind the cursor are deleted (the reorg / finality window).
    pub reorg_window_daa: u64,
    /// A batch with nothing to record is still logged when the cursor moved this far since the last
    /// logged cursor, so a rebuild from the log resumes close to the tip.
    pub checkpoint_daa: u64,
    /// Threads of the pure per-batch work done before the single database pass (`record::precompute`): 1 does it inline.
    pub verify_threads: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig { reorg_window_daa: 12 * 3600 * 10, checkpoint_daa: 3600 * 10, verify_threads: verify_threads(0) }
    }
}

/// The verification threads for a configured value: 0 is the available parallelism, at most 4 (the database pass stays one
/// thread; four keep the pure work well ahead of it on a KOB-heavy chain).
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

/// A watched transaction: where the selected chain accepted it now, and the consumers following it.
#[derive(Clone, Copy, Debug)]
struct Watch {
    acc: Option<Acceptance>,
    owners: u8,
}

pub struct Ingest {
    pub(crate) conn: Connection,
    pub(crate) proc: Processor,
    log: Option<RecordLog>,
    cfg: IngestConfig,
    /// Transactions consumers (the matcher) asked to have followed, and where the selected chain
    /// accepted them now. In memory only: updated after each commit, reorgs included.
    watch: HashMap<Hash32, Watch>,
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

/// Position of the record log the database has committed (frames `0..n`).
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
    pub fn new(conn: Connection, proc: Processor, log: Option<RecordLog>) -> Self {
        Ingest { conn, proc, log, cfg: IngestConfig::default(), watch: HashMap::new() }
    }

    pub fn with_config(mut self, cfg: IngestConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Follow the acceptance of `txid` for the matcher from now on (call before submitting it, so no
    /// block is missed).
    pub fn watch_tx(&mut self, txid: Hash32) {
        self.watch_tx_for(Watcher::Matcher, txid)
    }

    /// Follow the acceptance of `txid` for `who` from now on.
    pub fn watch_tx_for(&mut self, who: Watcher, txid: Hash32) {
        self.watch.entry(txid).or_insert(Watch { acc: None, owners: 0 }).owners |= who.bit();
    }

    /// Stop following `txid` for `who` (other consumers keep their watch).
    pub fn unwatch_tx_for(&mut self, who: Watcher, txid: &Hash32) {
        if let Some(w) = self.watch.get_mut(txid) {
            w.owners &= !who.bit();
            if w.owners == 0 {
                self.watch.remove(txid);
            }
        }
    }

    /// Where the selected chain accepted a watched transaction: `None` when nobody watches it,
    /// `Some(None)` when it is watched and not accepted (or its block was reorged away).
    pub fn acceptance_of(&self, txid: &Hash32) -> Option<Option<Acceptance>> {
        self.watch.get(txid).map(|w| w.acc)
    }

    /// The matcher's view: stop following every transaction of the matcher not in `keep`; returns
    /// where the selected chain accepted each kept one now (`None`: not accepted, or its block was
    /// reorged away).
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

    /// After a commit: transactions of removed chain blocks lose their acceptance, transactions of
    /// the added blocks gain it (a transaction re-accepted by the new chain does both).
    fn track_acceptance(&mut self, removed: &[Hash32], blocks: &Blocks<'_>) {
        if self.watch.is_empty() {
            return;
        }
        for w in self.watch.values_mut() {
            if w.acc.is_some_and(|(h, _)| removed.contains(&h)) {
                w.acc = None;
            }
        }
        if let Blocks::Live(added) = blocks {
            for b in *added {
                for t in &b.txs {
                    if let Some(w) = self.watch.get_mut(&t.verbose_data.transaction_id) {
                        w.acc = Some((b.header.hash, b.header.daa_score));
                    }
                }
            }
        }
    }

    pub fn processor(&self) -> &Processor {
        &self.proc
    }

    pub fn cursor(&self) -> DbResult<Option<Cursor>> {
        get_cursor(&self.conn)
    }

    /// Set the cursor of a fresh database (start mode) without touching any rows.
    pub fn init_cursor(&self, c: &Cursor) -> DbResult<()> {
        set_cursor(&self.conn, c)
    }

    pub fn records_next(&self) -> Option<u64> {
        self.log.as_ref().map(|r| r.next_n())
    }

    pub fn order_count(&self) -> DbResult<u64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get::<_, i64>(0))? as u64)
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

    /// Apply one VSPC response requested from `start` (which must be the cursor).
    pub fn apply_batch(&mut self, start: Hash32, batch: &VspcBatch) -> Result<Applied, IngestError> {
        let cursor = get_cursor(&self.conn)?.ok_or(IngestError::NoCursor)?;
        if start != cursor.hash {
            return Err(IngestError::Stale { requested: start, cursor: cursor.hash });
        }
        if batch.removed.is_empty() && batch.added.is_empty() {
            // at the sink: nothing to commit (no fsync per idle poll)
            return Ok(Applied {
                event: IndexEvent::default(),
                removed_hashes: 0,
                reverted_rows_blocks: 0,
                added: 0,
                relevant_txs: 0,
                new_orders: 0,
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
        self.commit_batch(Some(start), &batch.removed, Blocks::Live(&batch.added), None, true, &[], false)
    }

    /// Shared by live batches, replays and rewinds.
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
    ) -> Result<Applied, IngestError> {
        let old_cursor = get_cursor(&self.conn)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut delta = Delta::default();
        let mut reverted = 0usize;
        let mut reverted_relevant = false;
        for h in removed {
            if let Some(seq) = Self::seq_of(&tx, h)? {
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
                // the pure work of the whole batch first, on several threads; the database pass below stays single-threaded
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
        set_cursor(&tx, &cursor)?;
        // the reorg window: chain blocks older than it are final and no longer kept
        tx.execute("DELETE FROM blocks WHERE daa < ?1", [cursor.daa.saturating_sub(self.cfg.reorg_window_daa) as i64])?;

        let checkpoint_due = blocks.len() > 0 && cursor.daa >= logged_daa(&tx)?.saturating_add(self.cfg.checkpoint_daa);
        let loggable =
            self.log.is_some() && (always_log || reverted_relevant || !log_blocks.is_empty() || !ops.is_empty() || checkpoint_due);
        let mut log_n = None;
        let mut log_len = 0;
        if loggable {
            let log = self.log.as_mut().expect("checked");
            let rec = LogBatch {
                start: start.or(old_cursor.map(|c| c.hash)).unwrap_or_default(),
                // a reorg that touched no KOB row is not recorded: replay has no rows to revert either
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
        }
        if let Err(e) = tx.commit() {
            if let (Some(n), Some(log)) = (log_n, self.log.as_mut()) {
                let _ = log.truncate_to(n);
            }
            return Err(IngestError::Db(e.into()));
        }
        self.track_acceptance(removed, &blocks);
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
            added: blocks.len(),
            relevant_txs: delta.relevant_txs,
            new_orders: delta.new_orders,
            rejects: delta.rejects,
            log_n,
            log_bytes: log_len,
            cursor,
            ops: op_report,
        })
    }

    /// Revert every stored block newer than `target_seq` and move the cursor to `target`. Used when the
    /// node no longer knows the cursor and an older stored block is still known to it.
    pub fn rewind_to(&mut self, target_seq: i64, target: Cursor) -> Result<Applied, IngestError> {
        let mut hashes = Vec::new();
        for (seq, hash, _) in self.stored_blocks_desc(usize::MAX >> 1)? {
            if seq > target_seq {
                hashes.push(hash);
            }
        }
        self.commit_batch(None, &hashes, Blocks::Live(&[]), Some(target), true, &[], false)
    }

    /// Apply operator operations (recovery import, gap reconciliation) in one logged commit. With
    /// `new_cursor` the cursor moves too (rebase after a gap); otherwise it stays.
    pub fn apply_ops(&mut self, ops: Vec<LogOp>, new_cursor: Option<Cursor>) -> Result<Applied, IngestError> {
        let cur = get_cursor(&self.conn)?;
        let forced = new_cursor.or(cur).ok_or(IngestError::NoCursor)?;
        self.commit_batch(None, &[], Blocks::Live(&[]), Some(forced), true, &ops, true)
    }

    /// Rebuild this (empty) database from record-log frames. Returns (frames, relevant txs, orders).
    pub fn replay(&mut self, records: Vec<(u64, LogBatch)>) -> Result<(u64, u64, u64), IngestError> {
        let frames = records.last().map_or(0, |(n, _)| n + 1);
        self.replay_frames(records, frames)
    }

    /// [`Ingest::replay`] of a log of `frames` frames, some of which this build skipped (`recordlog::Replay::skipped`): the
    /// database ends at the log's end either way, so the follower appends after the last frame.
    pub fn replay_frames(&mut self, records: Vec<(u64, LogBatch)>, frames: u64) -> Result<(u64, u64, u64), IngestError> {
        let mut relevant = 0u64;
        let count = frames;
        let mut last_daa = 0;
        for (_, rec) in records {
            let cursor = Cursor { hash: rec.cursor.hash, daa: rec.cursor.daa };
            last_daa = cursor.daa;
            let applied =
                self.commit_batch(Some(rec.start), &rec.removed, Blocks::Logged(&rec.blocks), Some(cursor), false, &rec.ops, false)?;
            relevant += applied.relevant_txs;
        }
        meta_set(&self.conn, "records_next_n", &count.to_string())?;
        meta_set(&self.conn, "log_cursor_daa", &last_daa.to_string())?;
        Ok((count, relevant, self.order_count()?))
    }
}
