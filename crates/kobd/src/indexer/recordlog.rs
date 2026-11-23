//! The permanent record log: append-only, compact, hash-chained.
//!
//! The log holds what the node cannot give back after pruning and what the indexer cannot
//! re-derive: the extracted KOB records (`record::TxRecord`: KOB1 genesis payloads, every
//! transaction that touched a KOB covenant, token custody and stray outputs) grouped by chain
//! block, plus the reorg and operator operations that changed the derived state. It never holds a
//! raw node response. The database (`index.sqlite3`) is a derived view of it. Any schema change or
//! lost database is repaired with `kobd db replay`, which re-applies the records and then re-syncs
//! the tail from the node.
//!
//! # Frame layout
//!
//! There is one frame per committed batch:
//!
//! ```text
//! u32 LE body length | body | 32-byte chain hash = blake3(previous chain hash || body)
//! ```
//!
//! # Formats
//!
//! The body is versioned ([`FORMAT`]).
//!
//! - Format 2 is self-describing. It starts with `0x00 0x02` and the write time, and it carries a
//!   template table per frame: the record-log code of every order template and token program the
//!   frame refers to, with its template hash. States are length-prefixed. A frame therefore
//!   decodes whatever the templates are when it is read. A reveal of a template this build does
//!   not pin is dropped and counted, never misread.
//! - Format 3 ([`FORMAT_KCC23`]) is format 2 with KCC-23 declaration sections in its records. It is
//!   written only for the frames that hold one.
//! - Format 4 ([`FORMAT_PAYER`]) is format 3 with payer sections (who funded a transaction that
//!   spends a tracked order), likewise.
//! - Format 5 ([`FORMAT_IN_VALUE`]) is format 4 with input-value sections (what such a
//!   transaction's inputs held), likewise.
//!
//! A frame this build cannot decode (format 1, which has no marker, or a newer format) is kept and
//! skipped by [`read_all`] with a counted warning. `open` checks the chain only and never decodes a
//! body.
//!
//! # Segments
//!
//! A sealed segment (every one but the newest) is kept as `seg-<n>.kobrec.zst`, the zstd of the
//! segment file. The writer compresses the segment it has just rotated away from
//! ([`compress_sealed`]), after reading the copy back and comparing it with the segment. Builds
//! before 2026-11-19 kept them as gzip (`.kobrec.gz`), which is still read. `kobd db compact-log`
//! ([`compact`]) recompresses those (next to a running writer too) and `kobd db migrate` does it
//! offline. The frames, their chain hashes and everything that reads the log are the same. Only
//! the bytes on disk are about a quarter of the size (gzip: a third).
//!
//! # Integrity and crashes
//!
//! The chain detects truncation in the middle and edits. A frame is appended and fsynced BEFORE the
//! database transaction commits, and the database stores `records_next_n`. After a crash the log
//! can hold one frame that the database never committed. A database restored from a backup one
//! frame older looks the same.
//!
//! [`RecordLog::open`] never cuts a complete frame whose chain verifies. It refuses
//! ([`RecordLogError::AheadOfDatabase`]) and keeps it. `Indexer::open` applies one such frame
//! itself when it continues the database: its `start` is the database's cursor hash, or the
//! frame before it ends at the cursor the database recorded with the last frame it applied
//! (`log_cursor_hash`; a batch without KOB rows moves the cursor but is not logged). The commit it
//! records is what the database lacks. It refuses one of another history
//! ([`RecordLogError::Lineage`]). More frames past the database position mean that the database is
//! older than the log, for example a restored backup. They are kept for `kobd db replay
//! --onto-database`, which checks the same lineage. The reverse, a database ahead of the log,
//! means that the log was lost and is reported as an error.
//!
//! A failed append (full disk, I/O error) is rolled back to the last complete frame before the
//! error is returned, so nothing is ever appended after a partial frame. An incomplete frame with
//! complete frames after it is corruption and is refused. Only a bad last frame past the database
//! position is a torn tail, and it is removed: an incomplete one, or a complete one whose chain
//! hash does not verify. Either is an append the database never committed. A bad last frame that
//! the database committed is refused before anything is cut, with the log left as it is.

use super::record::{DecodeCtx, DecodeStats, TemplateTable, TxRecord};
use crate::hex::Hash32;
use crate::wire::{Reader, WireError, Writer};
use kob_protocol::artifacts::TemplateId;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// A position in the chain: a block hash and its DAA score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCursor {
    /// Hash of the chain block.
    pub hash: Hash32,
    /// DAA score of the chain block.
    pub daa: u64,
}

/// A chain block that carried KOB-relevant transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogBlock {
    /// Hash of the chain block.
    pub hash: Hash32,
    /// DAA score of the chain block.
    pub daa: u64,
    /// Blue score of the chain block.
    pub blue: u64,
    /// Timestamp of the chain block.
    pub ts: u64,
    /// The relevant transactions of the block, as records.
    pub txs: Vec<TxRecord>,
}

/// The custody token UTXO of an imported ask-side order.
///
/// It is verified against the node when the maker supplied the extension commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCustody {
    /// Transaction id of the custody output.
    pub txid: Hash32,
    /// Index of the custody output.
    pub index: u32,
    /// KAS value of the custody output, in sompi.
    pub value: u64,
    /// Token amount in base units.
    pub amount: i64,
    /// KCC-20 extension commitment of the custody token.
    pub ext: Hash32,
}

/// Operator operations that are not chain data but change the derived state.
///
/// They are logged so a replay reproduces them (recovery imports, gap reconciliation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOp {
    /// An order recovered from a maker export or archive, verified against the node's UTXO set.
    Import {
        template: TemplateId,
        state: Vec<u8>,
        covenant_id: Hash32,
        txid: Hash32,
        index: u32,
        value: u64,
        spk: Vec<u8>,
        daa: u64,
        custody: Option<ImportCustody>,
        /// The entry of an if-done exit, from the export.
        ///
        /// `None` for every other order, and for an operation of op code 1.
        parent: Option<Hash32>,
        /// A sell-first `KobIfdPair`'s second custody, its B prefund (op code 6).
        ///
        /// `None` for every other order.
        prefund: Option<ImportCustody>,
        /// A mode-1 order's bought-custody, node-verified (op code 7).
        ///
        /// `None` for every other order, and for an operation of op code 1, 5 or 6.
        bought: Option<ImportCustody>,
    },
    /// A tracked order output that the node no longer has, spent during a gap: close its lineage.
    CloseGap { txid: Hash32, index: u32, covenant_id: Hash32 },
    /// A tracked order continued during a gap under the same script: adopt the node's current
    /// output.
    Adopt {
        covenant_id: Hash32,
        old_txid: Hash32,
        old_index: u32,
        txid: Hash32,
        index: u32,
        value: u64,
        spk: Vec<u8>,
        /// The DAA score the node reports for the adopted output.
        ///
        /// The order's refund and kill time count from it. `None` in an operation of op code 3,
        /// where the cursor's DAA stands in.
        daa: Option<u64>,
        /// The order's custody for the token-holding kinds, found by the reconciliation at the
        /// custody script it derives.
        custody: Option<ImportCustody>,
        /// Token UTXOs owned by the order (custody or strays) that the node still holds. They stay
        /// live, and every other one is closed.
        keep: Vec<(Hash32, u32)>,
    },
    /// The store's history begins at this DAA score: a start other than at the pruning point, or a
    /// rebase past unread chain (the `meta` key `history_from_daa`, which only moves forward). A
    /// replay keeps it so (R3C2-4). Op code 8: a build before it skips the frame.
    HistoryFrom { daa: u64 },
}

/// One committed batch, which is one frame of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogBatch {
    /// The `start_hash` the request was made with.
    pub start: Hash32,
    /// Tip-first, as the node returned it.
    pub removed: Vec<Hash32>,
    /// The cursor after applying this batch.
    pub cursor: LogCursor,
    /// Only the chain blocks that carried relevant transactions.
    pub blocks: Vec<LogBlock>,
    /// Operator operations of the batch.
    pub ops: Vec<LogOp>,
}

fn code_of(t: TemplateId) -> u8 {
    crate::model::wire_code(t)
}

/// The base record-log frame format.
///
/// The body starts with the marker `0x00 0x02`, carries a template table (wire code -> template
/// hash) and length-prefixes every state, so a frame decodes without assuming the current
/// templates. Format 1, which has no marker (the body starts with the write time in ms, a varint
/// greater than 0), is not read.
pub const FORMAT: u8 = 2;

/// Format 3: format 2 whose records may carry a KCC-23 declaration section.
///
/// The section is `record::TxRecord::kcc23`, flagged by bit 30 of a record's position. Only a
/// frame that holds such a section is written as format 3. Every other frame stays format 2, byte
/// for byte, so an older build skips only the frames with declarations, with a counted "newer
/// format" warning, instead of misreading them.
pub const FORMAT_KCC23: u8 = 3;

/// Format 4: format 3 whose records may carry a payer section.
///
/// The section is `record::TxRecord::payer`: the script that funded a transaction spending a
/// tracked order, flagged by bit 29 of a record's position. Only a frame that holds such a section
/// is written as format 4, and KCC-23 sections may be in it too. Every other frame stays format 2
/// or 3, so an older build skips only those frames, with a counted "newer format" warning, instead
/// of misreading them. A record of an earlier frame has no payer: the submitter of its transaction
/// is unknown.
pub const FORMAT_PAYER: u8 = 4;

/// Format 5: format 4 whose records may carry an input-value section.
///
/// The section is `record::TxRecord::in_value`: what the inputs of a transaction spending a tracked
/// order held, as the node reported them, so that its network fee is known. It is flagged by bit
/// 28 of a record's position. Only a frame that holds such a section is written as format 5, and
/// payer and KCC-23 sections may be in it too. Every other frame stays format 2, 3 or 4, so an
/// older build skips only those frames, with a counted "newer format" warning, instead of
/// misreading them. A record of an earlier frame has no input value: the network fee of its
/// transaction is unknown.
pub const FORMAT_IN_VALUE: u8 = 5;

impl LogOp {
    fn encode(&self, w: &mut Writer) {
        match self {
            LogOp::Import { template, state, covenant_id, txid, index, value, spk, daa, custody, parent, prefund, bought } => {
                // Op 5 is op 1 followed by the parent (op 1 is still read). Op 6 is op 5 followed
                // by the second custody (a pair entry's B prefund), written only when there is one.
                // Op 7 is op 6 followed by the bought-custody, written only when there is one.
                w.u8(if bought.is_some() {
                    7
                } else if prefund.is_some() {
                    6
                } else {
                    5
                });
                w.u8(code_of(*template));
                w.bytes(state);
                w.hash(covenant_id);
                w.hash(txid);
                w.var(*index as u64);
                w.var(*value);
                w.bytes(spk);
                w.var(*daa);
                put_custody(w, custody.as_ref());
                put_opt_hash(w, parent.as_ref());
                if prefund.is_some() || bought.is_some() {
                    put_custody(w, prefund.as_ref());
                }
                if bought.is_some() {
                    put_custody(w, bought.as_ref());
                }
            }
            LogOp::CloseGap { txid, index, covenant_id } => {
                w.u8(2);
                w.hash(txid);
                w.var(*index as u64);
                w.hash(covenant_id);
            }
            LogOp::Adopt { covenant_id, old_txid, old_index, txid, index, value, spk, daa, custody, keep } => {
                // Op 4 is op 3 followed by the output's DAA, the custody and the kept token UTXOs
                // (op 3 is still read).
                w.u8(4);
                w.hash(covenant_id);
                w.hash(old_txid);
                w.var(*old_index as u64);
                w.hash(txid);
                w.var(*index as u64);
                w.var(*value);
                w.bytes(spk);
                match daa {
                    Some(d) => {
                        w.u8(1);
                        w.var(*d);
                    }
                    None => w.u8(0),
                }
                put_custody(w, custody.as_ref());
                w.var(keep.len() as u64);
                for (t, i) in keep {
                    w.hash(t);
                    w.var(*i as u64);
                }
            }
            LogOp::HistoryFrom { daa } => {
                w.u8(8);
                w.var(*daa);
            }
        }
    }

    /// Decodes an operation.
    ///
    /// It is `None` for an import of an order layout this build cannot interpret, which is read
    /// past and counted in `ctx`.
    fn decode(r: &mut Reader<'_>, ctx: &mut DecodeCtx<'_>) -> Result<Option<LogOp>, WireError> {
        Ok(Some(match r.u8()? {
            op @ (1 | 5 | 6 | 7) => {
                let code = r.u8()?;
                let state = r.bytes()?.to_vec();
                let covenant_id = r.hash()?;
                let txid = r.hash()?;
                let index = r.var()? as u32;
                let value = r.var()?;
                let spk = r.bytes()?.to_vec();
                let daa = r.var()?;
                let custody = get_custody(r)?;
                let parent = if op >= 5 { get_opt_hash(r)? } else { None };
                let prefund = if op >= 6 { get_custody(r)? } else { None };
                let bought = if op == 7 { get_custody(r)? } else { None };
                let Some(template) = ctx.import_template(code, state.len())? else { return Ok(None) };
                LogOp::Import { template, state, covenant_id, txid, index, value, spk, daa, custody, parent, prefund, bought }
            }
            2 => LogOp::CloseGap { txid: r.hash()?, index: r.var()? as u32, covenant_id: r.hash()? },
            op @ (3 | 4) => {
                let covenant_id = r.hash()?;
                let old_txid = r.hash()?;
                let old_index = r.var()? as u32;
                let txid = r.hash()?;
                let index = r.var()? as u32;
                let value = r.var()?;
                let spk = r.bytes()?.to_vec();
                let (daa, custody, keep) = if op == 4 {
                    let daa = match r.u8()? {
                        0 => None,
                        1 => Some(r.var()?),
                        _ => return Err(WireError("adopt daa tag")),
                    };
                    let custody = get_custody(r)?;
                    // Bounded by the bytes left (a hash and an index each), not by a fixed cap,
                    // so every list that encode writes into a frame decodes.
                    let n = r.count(33)?;
                    let mut keep = Vec::with_capacity(n);
                    for _ in 0..n {
                        keep.push((r.hash()?, r.var()? as u32));
                    }
                    (daa, custody, keep)
                } else {
                    (None, None, Vec::new())
                };
                LogOp::Adopt { covenant_id, old_txid, old_index, txid, index, value, spk, daa, custody, keep }
            }
            8 => LogOp::HistoryFrom { daa: r.var()? },
            _ => return Err(WireError("unknown operation")),
        }))
    }
}

fn put_custody(w: &mut Writer, c: Option<&ImportCustody>) {
    match c {
        Some(c) => {
            w.u8(1);
            w.hash(&c.txid);
            w.var(c.index as u64);
            w.var(c.value);
            w.svar(c.amount);
            w.hash(&c.ext);
        }
        None => w.u8(0),
    }
}

fn get_custody(r: &mut Reader<'_>) -> Result<Option<ImportCustody>, WireError> {
    Ok(match r.u8()? {
        0 => None,
        1 => Some(ImportCustody { txid: r.hash()?, index: r.var()? as u32, value: r.var()?, amount: r.svar()?, ext: r.hash()? }),
        _ => return Err(WireError("custody tag")),
    })
}

fn put_opt_hash(w: &mut Writer, h: Option<&Hash32>) {
    match h {
        Some(h) => {
            w.u8(1);
            w.hash(h);
        }
        None => w.u8(0),
    }
}

fn get_opt_hash(r: &mut Reader<'_>) -> Result<Option<Hash32>, WireError> {
    Ok(match r.u8()? {
        0 => None,
        1 => Some(r.hash()?),
        _ => return Err(WireError("hash tag")),
    })
}

/// A decoded frame body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBatch {
    pub batch: LogBatch,
    /// Wall-clock time (ms) the frame was written at.
    pub written_ms: u64,
    /// What this build could not interpret and dropped (the frame stays in the log).
    pub stats: DecodeStats,
}

fn write_table(w: &mut Writer, t: &TemplateTable) {
    w.var(t.len() as u64);
    for (code, hash) in t {
        w.u8(*code);
        w.raw(hash);
    }
}

fn read_table(r: &mut Reader<'_>) -> Result<TemplateTable, WireError> {
    let n = r.count(33)?;
    let mut t = TemplateTable::new();
    for _ in 0..n {
        let code = r.u8()?;
        let hash: [u8; 32] = r.raw(32)?.try_into().expect("32");
        if t.insert(code, hash).is_some() {
            return Err(WireError("duplicate template code"));
        }
    }
    Ok(t)
}

impl LogBatch {
    /// The templates the batch refers to: order reveals and imports, holding programs.
    fn tables(&self) -> (TemplateTable, TemplateTable) {
        let (mut orders, mut programs) = (TemplateTable::new(), TemplateTable::new());
        for t in self.blocks.iter().flat_map(|b| &b.txs) {
            t.templates(&mut orders, &mut programs);
        }
        for o in &self.ops {
            if let LogOp::Import { template, .. } = o {
                orders.insert(code_of(*template), kob_protocol::artifacts::template(*template).hash);
            }
        }
        (orders, programs)
    }

    /// Encodes the batch as a frame body.
    ///
    /// The body is format 2, or format 3 when a record carries a KCC-23 declaration section, 4 when
    /// one carries a payer section, and 5 when one carries an input-value section.
    pub fn encode(&self, now_ms: u64) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0);
        let txs = || self.blocks.iter().flat_map(|b| &b.txs);
        w.u8(if txs().any(|t| t.in_value.is_some()) {
            FORMAT_IN_VALUE
        } else if txs().any(|t| t.payer.is_some()) {
            FORMAT_PAYER
        } else if txs().any(|t| t.kcc23.is_some()) {
            FORMAT_KCC23
        } else {
            FORMAT
        });
        w.var(now_ms);
        let (orders, programs) = self.tables();
        write_table(&mut w, &orders);
        write_table(&mut w, &programs);
        w.hash(&self.start);
        w.hash(&self.cursor.hash);
        w.var(self.cursor.daa);
        w.var(self.removed.len() as u64);
        for h in &self.removed {
            w.hash(h);
        }
        w.var(self.blocks.len() as u64);
        for b in &self.blocks {
            w.hash(&b.hash);
            w.var(b.daa);
            w.var(b.blue);
            w.var(b.ts);
            w.var(b.txs.len() as u64);
            for t in &b.txs {
                t.encode(&mut w);
            }
        }
        w.var(self.ops.len() as u64);
        for o in &self.ops {
            o.encode(&mut w);
        }
        w.buf
    }

    /// Decodes a frame body (format 2, 3, 4 or 5).
    pub fn decode(body: &[u8]) -> Result<DecodedBatch, WireError> {
        match body {
            [0, FORMAT | FORMAT_KCC23 | FORMAT_PAYER | FORMAT_IN_VALUE, rest @ ..] => {
                let mut r = Reader::new(rest);
                let written_ms = r.var()?;
                let orders = read_table(&mut r)?;
                let programs = read_table(&mut r)?;
                let mut ctx = DecodeCtx::v2(&orders, &programs);
                let batch = LogBatch::decode_rest(&mut r, &mut ctx)?;
                if !r.done() {
                    return Err(WireError("trailing bytes"));
                }
                Ok(DecodedBatch { batch, written_ms, stats: ctx.stats })
            }
            [0, ..] => Err(WireError("record-log frame format newer than this build")),
            _ => Err(WireError("record-log frame format 1 (written before 2026-10-01) is not read by this build")),
        }
    }

    /// Everything after the write time (and, in format 2, the tables).
    fn decode_rest(r: &mut Reader<'_>, ctx: &mut DecodeCtx<'_>) -> Result<LogBatch, WireError> {
        let start = r.hash()?;
        let cursor = LogCursor { hash: r.hash()?, daa: r.var()? };
        let n = r.count(32)?;
        let mut removed = Vec::with_capacity(n);
        for _ in 0..n {
            removed.push(r.hash()?);
        }
        let n = r.count(36)?;
        let mut blocks = Vec::with_capacity(n);
        for _ in 0..n {
            let hash = r.hash()?;
            let daa = r.var()?;
            let blue = r.var()?;
            let ts = r.var()?;
            let nt = r.count(40)?;
            let mut txs = Vec::with_capacity(nt);
            for _ in 0..nt {
                txs.push(TxRecord::decode_in(r, ctx)?);
            }
            blocks.push(LogBlock { hash, daa, blue, ts, txs });
        }
        let n = r.count(2)?;
        let mut ops = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(op) = LogOp::decode(r, ctx)? {
                ops.push(op);
            }
        }
        Ok(LogBatch { start, removed, cursor, blocks, ops })
    }
}

/// Errors of the record log.
#[derive(Debug, thiserror::Error)]
pub enum RecordLogError {
    /// A read or write of a segment failed.
    #[error("record log io: {0}")]
    Io(#[from] std::io::Error),
    /// A frame cannot be read.
    #[error("record log frame {n} is corrupt: {reason}")]
    Corrupt { n: u64, reason: String },
    /// The log ends before the database's position: it was lost or replaced.
    #[error("record log ends at frame {log_next} but the database expects {db_next}: the log was lost or replaced")]
    BehindDatabase { log_next: u64, db_next: u64 },
    /// The log holds frames that the database has not committed, so the database is older.
    #[error(
        "record log holds frames {db_next}..{log_next} that the database has not committed; an unclean shutdown leaves at \
         most one, which the indexer applies itself when it continues the database's cursor, so the database is older than \
         the log (restored from a backup?). The log is left as it is: apply the missing frames with `kobd db \
         replay --onto-database`, or move the database aside and rebuild it with `kobd db replay` \
         (docs/ops/executor.md, Part B, 8)"
    )]
    AheadOfDatabase { log_next: u64, db_next: u64 },
    /// The first frame past the database position does not continue the database's cursor.
    ///
    /// The log and the database are of different histories.
    #[error(
        "record log frame {n} starts at block {frame_start}, but the database's cursor is {db_cursor}: the log and the \
         database do not belong together (another instance's log, or a database of another history). Nothing was changed; \
         rebuild the database from this log with `kobd db replay` into a fresh data directory"
    )]
    Lineage { n: u64, frame_start: Hash32, db_cursor: String },
    /// An append or a truncation failed, and the segment could not be put back to its last
    /// complete frame.
    ///
    /// Nothing more is written until a restart, whose `open` repairs the log (a torn last frame) or
    /// refuses it.
    #[error("record log is not written to any more until a restart: {0}")]
    Stopped(String),
}

const ZERO: [u8; 32] = [0u8; 32];
/// A frame larger than this is treated as garbage (the biggest batch is a few MB).
const MAX_FRAME: u32 = 1 << 30;

fn chain_hash(prev: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(prev);
    h.update(body);
    *h.finalize().as_bytes()
}

fn segment_name(first_n: u64) -> String {
    format!("seg-{first_n:012}.kobrec")
}

/// The form a segment file is kept in.
///
/// `seg-<n>.kobrec` is the plain segment. `seg-<n>.kobrec.zst` (zstd, written now) and
/// `seg-<n>.kobrec.gz` (gzip, written by builds before 2026-11-19, still read) hold its bytes,
/// frame for frame. The chain hashes cover the frame bodies, so the log reads and verifies the
/// same. Only a sealed segment is ever compressed, never the newest one, which the writer appends
/// to. The order is the trust order: when a crash left a segment in two forms, the older form (the
/// one a compression started from) is the one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Form {
    Plain,
    Gz,
    Zst,
}

impl Form {
    const ALL: [Form; 3] = [Form::Plain, Form::Gz, Form::Zst];

    fn suffix(self) -> &'static str {
        match self {
            Form::Plain => ".kobrec",
            Form::Gz => ".kobrec.gz",
            Form::Zst => ".kobrec.zst",
        }
    }

    fn of(p: &Path) -> Form {
        match p.extension().and_then(|e| e.to_str()) {
            Some("gz") => Form::Gz,
            Some("zst") => Form::Zst,
            _ => Form::Plain,
        }
    }
}

fn is_compressed(p: &Path) -> bool {
    Form::of(p) != Form::Plain
}

/// The path of the same segment in form `f`.
fn with_form(p: &Path, f: Form) -> PathBuf {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let stem = name.strip_suffix(Form::of(p).suffix()).unwrap_or(name);
    p.with_file_name(format!("{stem}{}", f.suffix()))
}

fn list_segments(dir: &Path) -> std::io::Result<Vec<(u64, PathBuf)>> {
    let mut v: Vec<(u64, Form, PathBuf)> = Vec::new();
    if !dir.exists() {
        return Ok(vec![]);
    }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(rest) = name.strip_prefix("seg-") else { continue };
        let Some((digits, form)) = Form::ALL.iter().find_map(|f| rest.strip_suffix(f.suffix()).map(|d| (d, *f))) else { continue };
        let Ok(n) = digits.parse::<u64>() else { continue };
        // Two forms of one segment exist when a compression stopped before it removed the one it
        // started from. That one is trusted.
        match v.iter().position(|(m, _, _)| *m == n) {
            Some(i) if form < v[i].1 => v[i] = (n, form, p),
            Some(_) => {}
            None => v.push((n, form, p)),
        }
    }
    v.sort();
    Ok(v.into_iter().map(|(n, _, p)| (n, p)).collect())
}

/// One segment file for reading, decompressed on the fly.
fn open_form(path: &Path) -> std::io::Result<Box<dyn Read>> {
    open_as(path, Form::of(path))
}

/// A file holding a segment in form `form` (a `.tmp` copy too), for reading.
fn open_as(path: &Path, form: Form) -> std::io::Result<Box<dyn Read>> {
    let f = File::open(path)?;
    Ok(match form {
        Form::Plain => Box::new(f),
        Form::Gz => Box::new(flate2::read::GzDecoder::new(BufReader::with_capacity(1 << 20, f))),
        Form::Zst => Box::new(zstd::stream::read::Decoder::with_buffer(BufReader::with_capacity(1 << 20, f))?),
    })
}

/// A segment for reading.
///
/// A compaction next to this reader (`kobd db compact-log`) may have replaced the file listed by
/// another form of the same segment. That one is read instead.
fn open_segment(path: &Path) -> std::io::Result<Box<dyn Read>> {
    match open_form(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            for f in Form::ALL {
                let other = with_form(path, f);
                if other != path && other.exists() {
                    return open_form(&other);
                }
            }
            Err(e)
        }
        r => r,
    }
}

fn read_fully(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        let n = r.read(&mut buf[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    Ok(got)
}

enum FrameRead {
    Eof,
    /// Incomplete frame at the end of the file.
    Torn,
    Frame {
        body: Vec<u8>,
        chain: [u8; 32],
        total: u64,
    },
}

fn read_frame(r: &mut impl Read) -> std::io::Result<FrameRead> {
    let mut len4 = [0u8; 4];
    let got = read_fully(r, &mut len4)?;
    if got == 0 {
        return Ok(FrameRead::Eof);
    }
    if got < 4 {
        return Ok(FrameRead::Torn);
    }
    let len = u32::from_le_bytes(len4);
    if len == 0 || len > MAX_FRAME {
        return Ok(FrameRead::Torn);
    }
    let mut body = vec![0u8; len as usize];
    if read_fully(r, &mut body)? < body.len() {
        return Ok(FrameRead::Torn);
    }
    let mut chain = [0u8; 32];
    if read_fully(r, &mut chain)? < 32 {
        return Ok(FrameRead::Torn);
    }
    Ok(FrameRead::Frame { body, chain, total: 4 + len as u64 + 32 })
}

/// Whether a complete frame, chained from `prev`, follows the incomplete frame at `offset`.
///
/// `path` is the segment to read. A crash during an append leaves nothing after the incomplete
/// frame. A complete, chained frame after it means the incomplete one sits in the middle of the
/// log, and cutting there would drop good frames.
fn complete_frame_follows(path: &Path, offset: u64, prev: &[u8; 32]) -> std::io::Result<bool> {
    let mut f = File::open(path)?;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(offset))?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail)?;
    for k in 1..tail.len() {
        let rest = &tail[k..];
        if rest.len() < 4 + 1 + 32 {
            break;
        }
        let len = u32::from_le_bytes(rest[..4].try_into().expect("4")) as usize;
        if len == 0 || len > rest.len() - 36 {
            continue;
        }
        if chain_hash(prev, &rest[4..4 + len]) == rest[4 + len..4 + len + 32] {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Where a scan stopped.
struct ScanEnd {
    /// Number of frames accepted.
    frames: u64,
    last_chain: [u8; 32],
    /// `(segment index, byte offset)` of the first byte that is not part of an accepted frame, when
    /// the scan stopped early (torn tail or `stop` returned true).
    cut: Option<(usize, u64)>,
    torn: bool,
    /// The torn tail is a complete last frame whose chain hash does not verify, not an
    /// incomplete one.
    mismatch: bool,
}

/// Walks every frame in order, verifying the chain.
///
/// `visit(n, body)` returns `true` to stop before frame `n`, and the offset of that frame is
/// reported in `cut`.
fn scan(
    segs: &[(u64, PathBuf)],
    mut visit: impl FnMut(u64, &[u8]) -> Result<bool, RecordLogError>,
) -> Result<ScanEnd, RecordLogError> {
    let mut n = 0u64;
    let mut prev = ZERO;
    for (si, (_, path)) in segs.iter().enumerate() {
        // The newest segment is never compressed (the writer appends to it), so the torn-tail
        // checks below see plain bytes.
        let mut rdr = BufReader::with_capacity(1 << 20, open_segment(path)?);
        let mut offset = 0u64;
        loop {
            match read_frame(&mut rdr)? {
                FrameRead::Eof => break,
                FrameRead::Torn => {
                    if si != segs.len() - 1 {
                        return Err(RecordLogError::Corrupt { n, reason: "incomplete frame before the end of the log".into() });
                    }
                    if complete_frame_follows(path, offset, &prev)? {
                        return Err(RecordLogError::Corrupt {
                            n,
                            reason: "incomplete frame followed by complete frames (a write that failed part way, then \
                                     later appends); the log is left as it is"
                                .into(),
                        });
                    }
                    return Ok(ScanEnd { frames: n, last_chain: prev, cut: Some((si, offset)), torn: true, mismatch: false });
                }
                FrameRead::Frame { body, chain, total } => {
                    if chain_hash(&prev, &body) != chain {
                        let last = si == segs.len() - 1 && offset + total == std::fs::metadata(path)?.len();
                        if !last {
                            return Err(RecordLogError::Corrupt { n, reason: "chain hash mismatch".into() });
                        }
                        return Ok(ScanEnd { frames: n, last_chain: prev, cut: Some((si, offset)), torn: true, mismatch: true });
                    }
                    if visit(n, &body)? {
                        return Ok(ScanEnd { frames: n, last_chain: prev, cut: Some((si, offset)), torn: false, mismatch: false });
                    }
                    prev = chain;
                    n += 1;
                    offset += total;
                }
            }
        }
    }
    Ok(ScanEnd { frames: n, last_chain: prev, cut: None, torn: false, mismatch: false })
}

/// What [`RecordLog::open`] found and repaired.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OpenReport {
    /// A torn (partial) last frame that was removed.
    pub torn_tail_removed: bool,
}

/// An open record log: the segment being appended to and the position in the chain.
pub struct RecordLog {
    dir: PathBuf,
    segment_bytes: u64,
    file: File,
    /// The segment `file` appends to.
    seg_path: PathBuf,
    /// Bytes of complete frames in the current segment.
    seg_len: u64,
    next_n: u64,
    last_chain: [u8; 32],
    /// Set when a failed append or truncation could not be undone: every later write is refused.
    stopped: Option<String>,
}

impl RecordLog {
    /// Opens the log, reconciles it with the database position `db_next_n` and positions it for
    /// append.
    ///
    /// `db_next_n` is 0 for a fresh database.
    pub fn open(dir: &Path, segment_bytes: u64, db_next_n: u64) -> Result<(RecordLog, OpenReport), RecordLogError> {
        std::fs::create_dir_all(dir)?;
        let mut report = OpenReport::default();
        let mut segs = list_segments(dir)?;
        let end = scan(&segs, |n, _| Ok(n >= db_next_n))?;
        let mut total_frames = end.frames;
        if let Some((si, offset)) = end.cut {
            if end.torn && end.frames < db_next_n {
                // The bad tail is a frame the database committed. Cutting it would destroy a
                // committed frame and still leave the log short, so the start is refused first,
                // with the log as it is.
                if end.mismatch {
                    return Err(RecordLogError::Corrupt {
                        n: end.frames,
                        reason: format!(
                            "the last frame is complete but its chain hash does not verify, and the database committed it \
                             (it expects {db_next_n} frames); the log is left as it is"
                        ),
                    });
                }
                return Err(RecordLogError::BehindDatabase { log_next: end.frames, db_next: db_next_n });
            }
            if end.torn {
                // An incomplete last frame, or a complete one whose chain hash does not verify,
                // past the database position: an append that the database never committed.
                report.torn_tail_removed = true;
            } else {
                // A complete frame at or after db_next_n is never cut. It may be the one append an
                // unclean shutdown left uncommitted, or a committed frame of a newer database (a
                // backup one frame older looks the same). Either way it is a faithful record of
                // the chain. The caller applies it after a lineage check (`Indexer::open`) or
                // refuses.
                let all = scan(&segs, |_, _| Ok(false))?;
                return Err(RecordLogError::AheadOfDatabase { log_next: all.frames, db_next: db_next_n });
            }
            segs[si].1 = make_cuttable(&segs[si].1)?;
            let f = OpenOptions::new().write(true).open(&segs[si].1)?;
            f.set_len(offset)?;
            f.sync_all()?;
            for (_, p) in segs.drain(si + 1..) {
                std::fs::remove_file(p)?;
            }
            total_frames = end.frames;
        }
        if total_frames < db_next_n {
            return Err(RecordLogError::BehindDatabase { log_next: total_frames, db_next: db_next_n });
        }
        // A torn tail can sit after uncommitted frames. After the cut above, the log ends at frame
        // `end.frames`.
        // The segment appended to is a whole one. Compression leaves the newest alone, and should
        // it be a gzip anyway, a new one starts.
        let (path, seg_len) = match segs.last() {
            Some((_, p)) if !is_compressed(p) => (p.clone(), std::fs::metadata(p)?.len()),
            _ => (dir.join(segment_name(total_frames)), 0),
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok((
            RecordLog {
                dir: dir.to_path_buf(),
                segment_bytes,
                file,
                seg_path: path,
                seg_len,
                next_n: total_frames,
                last_chain: end.last_chain,
                stopped: None,
            },
            report,
        ))
    }

    /// The number of the next frame to be appended.
    pub fn next_n(&self) -> u64 {
        self.next_n
    }

    /// Appends one batch and fsyncs. Returns the frame number and the frame's size in bytes.
    pub fn append(&mut self, batch: &LogBatch, now_ms: u64) -> Result<(u64, u64), RecordLogError> {
        self.append_body(&batch.encode(now_ms))
    }

    /// Appends one frame body as it is. Tests and tools write frames of other formats with it.
    ///
    /// Returns the frame number and size. A failed write or fsync is rolled back: the segment is
    /// cut back to its length before the append, so the next append starts right after the last
    /// complete frame. When even that fails, the log refuses every later write
    /// ([`RecordLogError::Stopped`]).
    #[doc(hidden)]
    pub fn append_body(&mut self, body: &[u8]) -> Result<(u64, u64), RecordLogError> {
        if let Some(why) = &self.stopped {
            return Err(RecordLogError::Stopped(why.clone()));
        }
        if self.seg_len >= self.segment_bytes && self.seg_len > 0 {
            self.file.sync_all()?;
            let path = self.dir.join(segment_name(self.next_n));
            self.file = OpenOptions::new().create(true).append(true).open(&path)?;
            self.seg_path = path;
            self.seg_len = 0;
            // The segment just sealed (and any sealed one an earlier run left whole) is kept
            // compressed. A failure only costs space.
            match compress_sealed(&self.dir) {
                Ok(r) if r.segments > 0 => {
                    tracing::info!(segments = r.segments, before = r.before, after = r.after, "record log: sealed segments compressed")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("record log: compressing the sealed segments failed (they stay as they are): {e}"),
            }
        }
        let n = self.next_n;
        let chain = chain_hash(&self.last_chain, body);
        let mut frame = Vec::with_capacity(body.len() + 36);
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body);
        frame.extend_from_slice(&chain);
        if let Err(e) = self.file.write_all(&frame).and_then(|()| self.file.sync_data()) {
            return Err(self.roll_back(e));
        }
        self.seg_len += frame.len() as u64;
        self.last_chain = chain;
        self.next_n += 1;
        Ok((n, frame.len() as u64))
    }

    /// Cuts the current segment back to its last complete frame after a failed append.
    ///
    /// Returns the error to report: the append's own when the cut succeeded, and
    /// [`RecordLogError::Stopped`] when it did not. The log then refuses every later write.
    fn roll_back(&mut self, e: std::io::Error) -> RecordLogError {
        let undo = OpenOptions::new().write(true).open(&self.seg_path).and_then(|f| {
            f.set_len(self.seg_len)?;
            f.sync_all()
        });
        match undo {
            Ok(()) => RecordLogError::Io(e),
            Err(u) => {
                let why = format!(
                    "appending frame {} to {} failed ({e}) and cutting the partial frame off failed too ({u})",
                    self.next_n,
                    self.seg_path.display()
                );
                self.stopped = Some(why.clone());
                RecordLogError::Stopped(why)
            }
        }
    }

    /// Whether a failed write could not be undone (every later write is refused until a restart).
    pub fn is_stopped(&self) -> bool {
        self.stopped.is_some()
    }

    /// Discards frames `>= n`, after a commit failed following the append.
    ///
    /// Only the newest frames can be cut. When the cut fails, the log refuses every later write
    /// ([`RecordLogError::Stopped`]), and the next start's `open` cuts the uncommitted frame.
    pub fn truncate_to(&mut self, n: u64) -> Result<(), RecordLogError> {
        if let Some(why) = &self.stopped {
            return Err(RecordLogError::Stopped(why.clone()));
        }
        if n >= self.next_n {
            return Ok(());
        }
        self.truncate_inner(n).map_err(|e| {
            let why = format!("discarding record-log frames from {n} on failed: {e}");
            self.stopped = Some(why.clone());
            RecordLogError::Stopped(why)
        })
    }

    fn truncate_inner(&mut self, n: u64) -> Result<(), RecordLogError> {
        let segs = list_segments(&self.dir)?;
        let end = scan(&segs, |k, _| Ok(k >= n))?;
        if let Some((si, offset)) = end.cut {
            let cut = make_cuttable(&segs[si].1)?;
            let f = OpenOptions::new().write(true).open(&cut)?;
            f.set_len(offset)?;
            f.sync_all()?;
            for (_, p) in segs.iter().skip(si + 1) {
                std::fs::remove_file(p)?;
            }
            self.file = OpenOptions::new().append(true).open(&cut)?;
            self.seg_path = cut;
            self.seg_len = offset;
        }
        self.next_n = end.frames;
        self.last_chain = end.last_chain;
        Ok(())
    }
}

/// Returns the segment to cut a log at, expanding a compressed one to a whole segment first.
///
/// This is the cut of the newest frames of a log whose last rotation was just compressed
/// (`truncate_to`). The whole file's path is returned.
fn make_cuttable(p: &Path) -> std::io::Result<PathBuf> {
    if !is_compressed(p) {
        return Ok(p.to_path_buf());
    }
    let plain = with_form(p, Form::Plain);
    let tmp = tmp_path(&plain);
    {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        std::io::copy(&mut open_form(p)?, &mut out)?;
        out.flush()?;
        out.get_ref().sync_all()?;
    }
    std::fs::rename(&tmp, &plain)?;
    sync_dir(&plain)?;
    remove_other_forms(&plain)?;
    Ok(plain)
}

/// The zstd level of a sealed segment.
///
/// Measured on copies of three 256 MiB segments of the TN10 soak log: 4.3x (gzip 6, before: 3.3x,
/// so a quarter smaller), 55 to 65 MB/s, so a rotation holds the writer about 5 s, as gzip did.
/// Level 19 gains 5 % more at 2 MB/s, and a long window nothing at this level.
const ZSTD_LEVEL: i32 = 9;

/// What [`compress_sealed`] and [`compact`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompressReport {
    /// Number of segments compressed.
    pub segments: usize,
    /// Bytes of the segments before compression.
    pub before: u64,
    /// Bytes of the segments after compression.
    pub after: u64,
}

fn digest_of(mut r: impl Read) -> std::io::Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(*h.finalize().as_bytes());
        }
        h.update(&buf[..n]);
    }
}

fn tmp_path(p: &Path) -> PathBuf {
    let mut t = p.as_os_str().to_owned();
    t.push(".tmp");
    PathBuf::from(t)
}

/// Makes a rename or a removal in the directory of `p` durable (Unix; NTFS journals it).
pub(crate) fn sync_dir(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(d) = p.parent() {
        File::open(d)?.sync_all()?;
    }
    let _ = p;
    Ok(())
}

/// Removes every form of the segment at `keep` but that one.
fn remove_other_forms(keep: &Path) -> std::io::Result<()> {
    for f in Form::ALL {
        let p = with_form(keep, f);
        if p != keep {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    sync_dir(keep)
}

/// A step of [`compress_one`] after which a test stops, as a crash would.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Half the compressed copy is written.
    Writing,
    /// The copy is written, synced and read back, not yet renamed.
    Written,
    /// The copy is renamed into place, and the file it was made from is still there.
    Renamed,
}

#[cfg(test)]
thread_local! {
    static CRASH_AFTER: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

/// Stops [`compress_one`] after step `s` in a test that asks for it.
#[cfg(test)]
fn crash_after(s: Step) -> std::io::Result<()> {
    if CRASH_AFTER.get() == Some(s) {
        return Err(std::io::Error::other(format!("simulated crash after {s:?}")));
    }
    Ok(())
}

/// Compresses one sealed segment (plain, or a gzip of an older build) to zstd and returns the
/// compressed size.
///
/// The copy is written beside it as `.zst.tmp`, synced, read back and compared with it (blake3 of
/// the bytes both hold). Only then is it renamed into place, after which the file it was made
/// from is removed. A crash leaves the segment in one form or two, and two forms read as the one
/// it started from, so no step loses a frame.
fn compress_one(src: &Path) -> std::io::Result<u64> {
    let dst = with_form(src, Form::Zst);
    let tmp = tmp_path(&dst);
    {
        let mut enc = zstd::stream::write::Encoder::new(BufWriter::with_capacity(1 << 20, File::create(&tmp)?), ZSTD_LEVEL)?;
        enc.include_checksum(true)?;
        #[cfg(test)]
        if CRASH_AFTER.get() == Some(Step::Writing) {
            let mut all = Vec::new();
            open_form(src)?.read_to_end(&mut all)?;
            enc.write_all(&all[..all.len() / 2])?;
            enc.flush()?;
            crash_after(Step::Writing)?;
        }
        std::io::copy(&mut open_form(src)?, &mut enc)?;
        let mut out = enc.finish()?;
        out.flush()?;
        out.get_ref().sync_all()?;
    }
    if digest_of(open_form(src)?)? != digest_of(open_as(&tmp, Form::Zst)?)? {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::other(format!("the compressed copy of {} does not read back the same", src.display())));
    }
    #[cfg(test)]
    crash_after(Step::Written)?;
    std::fs::rename(&tmp, &dst)?;
    sync_dir(&dst)?;
    #[cfg(test)]
    crash_after(Step::Renamed)?;
    let size = std::fs::metadata(&dst)?.len();
    remove_other_forms(&dst)?;
    Ok(size)
}

/// Which sealed segments [`compact`] compresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compact {
    /// The plain ones: the writer, after each rotation.
    Plain,
    /// The gzips of older builds, recompressed. Safe next to a running writer, which never
    /// changes a compressed segment.
    Gzip,
    /// Both, and the `.tmp` files a crash left behind. Offline only: the caller holds the writer
    /// lock (`kobd db compact-log`, `kobd db migrate`).
    All,
}

/// The lock that keeps two compactions of one log apart.
const COMPACT_LOCK: &str = "kob-compact.lock";

/// Compresses the sealed segments of a log directory (every one but the newest, which the writer
/// appends to). Idempotent.
///
/// [`Compact::Gzip`] and [`Compact::All`] hold an exclusive lock on `<dir>/kob-compact.lock`, so
/// two of them never work on one segment: a second one is refused.
pub fn compact(dir: &Path, which: Compact) -> Result<CompressReport, RecordLogError> {
    let _lock = match which {
        Compact::Plain => None,
        Compact::Gzip | Compact::All => {
            let path = dir.join(COMPACT_LOCK);
            let f = OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
            match f.try_lock() {
                Ok(()) => Some(f),
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(RecordLogError::Io(std::io::Error::other(format!(
                        "another compaction of {} is running ({} is locked)",
                        dir.display(),
                        path.display()
                    ))))
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    };
    if which == Compact::All {
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name.starts_with("seg-") && name.ends_with(".tmp") {
                std::fs::remove_file(&p)?;
            }
        }
    }
    let segs = list_segments(dir)?;
    let mut rep = CompressReport::default();
    for (_, p) in segs.iter().take(segs.len().saturating_sub(1)) {
        let todo = match Form::of(p) {
            Form::Plain => which != Compact::Gzip,
            Form::Gz => which != Compact::Plain,
            Form::Zst => false,
        };
        if !todo {
            continue;
        }
        let before = std::fs::metadata(p)?.len();
        let after = compress_one(p)?;
        rep.segments += 1;
        rep.before += before;
        rep.after += after;
    }
    Ok(rep)
}

/// Compresses every sealed segment of the log directory that is still whole ([`Compact::Plain`]).
///
/// The writer runs it after each rotation, holding the writer lock.
pub fn compress_sealed(dir: &Path) -> Result<CompressReport, RecordLogError> {
    compact(dir, Compact::Plain)
}

/// Number of complete frames in a log directory, without decoding them.
///
/// It is used at startup to refuse an empty database next to a non-empty log, which is a job for
/// `replay`, never for `open`.
pub fn record_count(dir: &Path) -> Result<u64, RecordLogError> {
    let segs = list_segments(dir)?;
    Ok(scan(&segs, |_, _| Ok(false))?.frames)
}

/// Total size of the log segments in bytes.
pub fn log_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut t = 0;
    for (_, p) in list_segments(dir)? {
        t += match std::fs::metadata(&p) {
            Ok(m) => m.len(),
            // compressed meanwhile by a compaction next to this reader
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::metadata(with_form(&p, Form::Zst)).map_or(0, |m| m.len()),
            Err(e) => return Err(e),
        };
    }
    Ok(t)
}

/// Where a chain-verified log ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogEnd {
    /// Every complete frame of the log (decoded or not): the database position after a replay.
    pub frames: u64,
    /// A torn (partial) last frame follows them; it is ignored.
    pub torn: bool,
}

/// Verifies the chain of the whole log without decoding a frame, with one frame in memory.
///
/// A replay calls it before it applies anything, so a broken log is refused with nothing applied.
pub fn verify(dir: &Path) -> Result<LogEnd, RecordLogError> {
    let end = scan(&list_segments(dir)?, |_, _| Ok(false))?;
    Ok(LogEnd { frames: end.frames, torn: end.torn })
}

/// What [`for_each_from`] read besides the frames it handed over.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Streamed {
    /// Complete frames read (`upto` when given).
    pub frames: u64,
    /// Whether a torn (partial) last frame follows the frames.
    pub torn: bool,
    /// Frames from `from` on this build could not decode, with the reason (they stay in the log).
    pub skipped: Vec<(u64, String)>,
    /// What the decoded frames had to drop.
    pub dropped: DecodeStats,
}

/// Decodes the frames from number `from` on, one at a time, and hands each decoded one to `apply`.
///
/// The frames are handed over in order as they are read, so memory holds one frame whatever the
/// size of the log. The frames before `from` are verified but not decoded. An undecodable frame is
/// counted in [`Streamed::skipped`] and not handed over.
///
/// The chain is verified as the frames are read, so a broken frame after some were handed over is
/// an error only then. A replay [`verify`]s the log first and passes its `frames` as `upto`, the
/// frame to stop before. A log that ends before it is an error. The first error of `apply` stops
/// the read and is returned.
pub fn for_each_from<E: From<RecordLogError>>(
    dir: &Path,
    from: u64,
    upto: Option<u64>,
    mut apply: impl FnMut(u64, LogBatch) -> Result<(), E>,
) -> Result<Streamed, E> {
    let segs = list_segments(dir).map_err(RecordLogError::from)?;
    let mut skipped = Vec::new();
    let mut dropped = DecodeStats::default();
    let mut failed = None;
    let end = scan(&segs, |n, body| {
        if upto.is_some_and(|u| n >= u) {
            return Ok(true);
        }
        if n >= from {
            match LogBatch::decode(body) {
                Ok(d) => {
                    dropped.add(&d.stats);
                    if let Err(e) = apply(n, d.batch) {
                        failed = Some(e);
                        return Ok(true);
                    }
                }
                Err(e) => skipped.push((n, e.to_string())),
            }
        }
        Ok(false)
    })?;
    if let Some(e) = failed {
        return Err(e);
    }
    if let Some(u) = upto {
        if end.frames < u {
            return Err(RecordLogError::Corrupt {
                n: end.frames,
                reason: format!("the log changed while it was read: {u} frames were verified, {} were read", end.frames),
            }
            .into());
        }
    }
    Ok(Streamed { frames: end.frames, torn: end.torn, skipped, dropped })
}

/// Every frame of a log directory in order, verified.
pub struct Replay {
    /// The frames this build decoded, with their numbers.
    pub records: Vec<(u64, LogBatch)>,
    /// Whether a torn (partial) last frame follows the frames.
    pub torn: bool,
    /// Every complete frame of the log (decoded or skipped): the database position after a replay.
    pub frames: u64,
    /// Frames this build could not decode at all, with the reason.
    ///
    /// The cause is a newer format or a layout no table here describes. They stay in the log, and
    /// a replay skips them.
    pub skipped: Vec<(u64, String)>,
    /// What the decoded frames had to drop (reveals / holdings / imports of unknown layouts).
    pub dropped: DecodeStats,
}

/// The frames from number `from` on, chain verified over the whole log.
///
/// A frame is decoded, or listed in `skipped` when it is undecodable. The frames before `from` are
/// verified but not decoded. It holds every decoded frame in memory (tests and small logs). A
/// replay streams them with [`for_each_from`].
pub fn read_from(dir: &Path, from: u64) -> Result<Replay, RecordLogError> {
    let mut records = Vec::new();
    let s = for_each_from(dir, from, None, |n, b| {
        records.push((n, b));
        Ok::<_, RecordLogError>(())
    })?;
    Ok(Replay { records, torn: s.torn, frames: s.frames, skipped: s.skipped, dropped: s.dropped })
}

/// Reads every frame of a log directory in order, chain verified.
///
/// A frame that is authentic (its chain hash verifies) but that this build cannot decode is
/// skipped and reported in [`Replay::skipped`] instead of failing the whole read. A broken chain
/// is still an error.
pub fn read_all(dir: &Path) -> Result<Replay, RecordLogError> {
    read_from(dir, 0)
}

/// Paths of the log segments, oldest first (tests and operators inspect or copy them).
pub fn segment_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    Ok(list_segments(dir)?.into_iter().map(|(_, p)| p).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(i: u8) -> LogBatch {
        LogBatch {
            start: Hash32([i; 32]),
            removed: vec![Hash32([9; 32])],
            cursor: LogCursor { hash: Hash32([i + 1; 32]), daa: i as u64 },
            blocks: vec![LogBlock {
                hash: Hash32([i + 1; 32]),
                daa: i as u64,
                blue: 1,
                ts: 2,
                txs: vec![TxRecord {
                    txid: Hash32([i; 32]),
                    pos: i as u32,
                    payload: vec![i; 5],
                    inputs: vec![],
                    outputs: vec![],
                    tok_outs: vec![],
                    holds: vec![],
                    kcc23: None,
                    payer: None,
                    in_value: None,
                }],
            }],
            ops: vec![LogOp::CloseGap { txid: Hash32([1; 32]), index: 2, covenant_id: Hash32([3; 32]) }],
        }
    }

    fn custody_of(i: u8) -> ImportCustody {
        ImportCustody { txid: Hash32([i; 32]), index: 1, value: 2, amount: -3, ext: Hash32([i + 1; 32]) }
    }

    #[test]
    fn operator_ops_roundtrip_and_older_ops_still_read() {
        let mut b = batch(3);
        b.ops = vec![
            LogOp::Adopt {
                covenant_id: Hash32([1; 32]),
                old_txid: Hash32([2; 32]),
                old_index: 3,
                txid: Hash32([4; 32]),
                index: 5,
                value: 6,
                spk: vec![7; 9],
                daa: Some(1_000_000),
                custody: Some(custody_of(8)),
                keep: vec![(Hash32([9; 32]), 0), (Hash32([10; 32]), 7)],
            },
            LogOp::Adopt {
                covenant_id: Hash32([1; 32]),
                old_txid: Hash32([2; 32]),
                old_index: 3,
                txid: Hash32([4; 32]),
                index: 5,
                value: 6,
                spk: vec![],
                daa: None,
                custody: None,
                keep: vec![],
            },
            // A sell-first pair entry's import carries its second custody (op 6). Any other import
            // is op 5.
            LogOp::Import {
                template: TemplateId::KobIfdPair,
                state: vec![0x11; template(TemplateId::KobIfdPair).state_len],
                covenant_id: Hash32([1; 32]),
                txid: Hash32([2; 32]),
                index: 3,
                value: 4,
                spk: vec![5; 3],
                daa: 6,
                custody: Some(custody_of(7)),
                parent: None,
                prefund: Some(custody_of(9)),
                bought: None,
            },
            LogOp::Import {
                template: TemplateId::KobPair,
                state: vec![0x12; template(TemplateId::KobPair).state_len],
                covenant_id: Hash32([1; 32]),
                txid: Hash32([2; 32]),
                index: 3,
                value: 4,
                spk: vec![5; 3],
                daa: 6,
                custody: Some(custody_of(7)),
                parent: Some(Hash32([8; 32])),
                prefund: None,
                bought: None,
            },
            // A mode-1 bid's import carries its bought-custody (op 7).
            LogOp::Import {
                template: TemplateId::KobBid,
                state: vec![0x13; template(TemplateId::KobBid).state_len],
                covenant_id: Hash32([1; 32]),
                txid: Hash32([2; 32]),
                index: 3,
                value: 4,
                spk: vec![5; 3],
                daa: 6,
                custody: None,
                parent: None,
                prefund: None,
                bought: Some(custody_of(10)),
            },
        ];
        let d = LogBatch::decode(&b.encode(5)).unwrap();
        assert_eq!(d.batch, b);
        // An import without a bought-custody is still written as op 5 or 6, byte for byte.
        for (op, code) in b.ops[2..].iter().zip([6u8, 5, 7]) {
            let mut w = Writer::new();
            op.encode(&mut w);
            assert_eq!(w.buf[0], code);
        }

        // ... in the op 5 layout an older build wrote, which reads back with no bought-custody.
        let mut old = Writer::new();
        old.u8(5);
        old.u8(code_of(TemplateId::KobPair));
        old.bytes(&vec![0x12; template(TemplateId::KobPair).state_len]);
        old.hash(&Hash32([1; 32]));
        old.hash(&Hash32([2; 32]));
        old.var(3);
        old.var(4);
        old.bytes(&[5; 3]);
        old.var(6);
        put_custody(&mut old, Some(&custody_of(7)));
        put_opt_hash(&mut old, Some(&Hash32([8; 32])));
        let mut w = Writer::new();
        b.ops[3].encode(&mut w);
        assert_eq!(w.buf, old.buf);
        assert!(matches!(&d.batch.ops[3], LogOp::Import { bought: None, prefund: None, .. }));

        // Older operations: op 3 (adopt without DAA, custody and kept token UTXOs).
        let mut empty = batch(3);
        empty.ops = vec![];
        let mut body = empty.encode(5);
        assert_eq!(body.pop(), Some(0), "the op count closes the body");
        let mut w = Writer::new();
        w.var(1);
        w.u8(3);
        w.hash(&Hash32([1; 32]));
        w.hash(&Hash32([2; 32]));
        w.var(3);
        w.hash(&Hash32([4; 32]));
        w.var(5);
        w.var(6);
        w.bytes(&[7; 4]);
        body.extend(w.buf);
        let d = LogBatch::decode(&body).unwrap();
        assert_eq!(
            d.batch.ops,
            vec![LogOp::Adopt {
                covenant_id: Hash32([1; 32]),
                old_txid: Hash32([2; 32]),
                old_index: 3,
                txid: Hash32([4; 32]),
                index: 5,
                value: 6,
                spk: vec![7; 4],
                daa: None,
                custody: None,
                keep: vec![],
            }]
        );
    }

    #[test]
    fn batch_roundtrip() {
        let b = batch(3);
        let body = b.encode(1234);
        let d = LogBatch::decode(&body).unwrap();
        assert_eq!((d.batch, d.written_ms), (b, 1234));
        assert_eq!(d.stats, DecodeStats::default());
        assert!(LogBatch::decode(&body[..body.len() - 1]).is_err());
    }

    #[test]
    fn append_reopen_and_read_back() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, rep) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        assert_eq!(rep, OpenReport::default());
        for i in 0..5 {
            assert_eq!(log.append(&batch(i), 1000 + i as u64).unwrap().0, i as u64);
        }
        drop(log);
        let (log, rep) = RecordLog::open(d.path(), 1 << 20, 5).unwrap();
        assert_eq!(rep, OpenReport::default());
        assert_eq!(log.next_n(), 5);
        let r = read_all(d.path()).unwrap();
        assert_eq!(r.records.len(), 5);
        assert!(!r.torn);
        assert_eq!(r.records[3].1, batch(3));
        assert_eq!(record_count(d.path()).unwrap(), 5);
    }

    /// The stream hands the frames from `from` over one at a time across segments.
    ///
    /// It stops before `upto`, refuses a log that ends before `upto`, and stops at the first error
    /// of the caller.
    #[test]
    fn frames_stream_across_segments_and_stop_where_asked() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1, 0).unwrap();
        for i in 0..6 {
            log.append(&batch(i), 1000).unwrap();
        }
        drop(log);
        assert_eq!(segment_paths(d.path()).unwrap().len(), 6);
        assert_eq!(verify(d.path()).unwrap(), LogEnd { frames: 6, torn: false });
        let mut got = Vec::new();
        let s = for_each_from(d.path(), 2, Some(5), |n, b| {
            got.push((n, b));
            Ok::<_, RecordLogError>(())
        })
        .unwrap();
        assert_eq!(got, (2..5).map(|i| (i as u64, batch(i))).collect::<Vec<_>>());
        assert_eq!((s.frames, s.torn), (5, false));
        let short = for_each_from(d.path(), 0, Some(7), |_, _| Ok::<_, RecordLogError>(()));
        assert!(matches!(short, Err(RecordLogError::Corrupt { n: 6, .. })), "{short:?}");
        let mut seen = 0;
        let stopped = for_each_from(d.path(), 0, None, |n, _| {
            seen += 1;
            if n == 3 {
                Err(RecordLogError::Stopped("caller".into()))
            } else {
                Ok(())
            }
        });
        assert!(matches!(stopped, Err(RecordLogError::Stopped(_))));
        assert_eq!(seen, 4);
    }

    #[test]
    fn a_complete_frame_past_the_database_is_kept_and_log_behind_db_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        for i in 0..4 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        // The database only committed 3 frames: the 4th is kept (the indexer applies it or
        // refuses), never cut.
        assert!(matches!(RecordLog::open(d.path(), 1 << 20, 3), Err(RecordLogError::AheadOfDatabase { log_next: 4, db_next: 3 })));
        assert_eq!(record_count(d.path()).unwrap(), 4);
        let tail = read_from(d.path(), 3).unwrap();
        assert_eq!((tail.frames, tail.records.len()), (4, 1));
        assert_eq!(tail.records[0], (3, batch(3)));
        // At the log's end the chain continues correctly.
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 4).unwrap();
        log.append(&batch(9), 0).unwrap();
        drop(log);
        assert_eq!(read_all(d.path()).unwrap().records.len(), 5);
        // The database claims more than the log has.
        assert!(matches!(RecordLog::open(d.path(), 1 << 20, 7), Err(RecordLogError::BehindDatabase { log_next: 5, db_next: 7 })));
    }

    /// A database older than the log, such as a restored backup, is not a crash.
    ///
    /// `open` refuses and keeps every frame.
    #[test]
    fn frames_of_more_than_one_uncommitted_write_are_kept_and_refused() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        for i in 0..50u8 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        for db in [0, 10, 48, 49] {
            let r = RecordLog::open(d.path(), 1 << 20, db);
            assert!(matches!(r, Err(RecordLogError::AheadOfDatabase { log_next: 50, db_next }) if db_next == db), "{db}");
            assert_eq!(record_count(d.path()).unwrap(), 50);
        }
        let msg = RecordLogError::AheadOfDatabase { log_next: 50, db_next: 10 }.to_string();
        assert!(msg.contains("replay --onto-database"), "{msg}");
    }

    #[test]
    fn torn_tail_is_repaired() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        log.append(&batch(1), 0).unwrap();
        log.append(&batch(2), 0).unwrap();
        drop(log);
        let seg = segment_paths(d.path()).unwrap().pop().unwrap();
        let mut f = OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(&[200, 0, 0, 0, 1, 2, 3]).unwrap();
        drop(f);
        assert!(read_all(d.path()).unwrap().torn);
        let (log, rep) = RecordLog::open(d.path(), 1 << 20, 2).unwrap();
        assert!(rep.torn_tail_removed);
        assert_eq!(log.next_n(), 2);
        assert!(!read_all(d.path()).unwrap().torn);
    }

    /// A bad last frame that the database committed is never cut: `open` refuses with every byte
    /// kept, whether the frame is complete with a chain hash that does not verify or incomplete.
    /// Past the database position the same frame is an append the database never committed, and
    /// is removed.
    #[test]
    fn a_committed_last_frame_with_a_bad_hash_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        for i in 0..3u8 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        let seg = segment_paths(d.path()).unwrap().pop().unwrap();
        let good = std::fs::read(&seg).unwrap();
        let mut bad = good.clone();
        *bad.last_mut().unwrap() ^= 0xff;
        std::fs::write(&seg, &bad).unwrap();
        let r = RecordLog::open(d.path(), 1 << 20, 3);
        assert!(matches!(r, Err(RecordLogError::Corrupt { n: 2, .. })), "{:?}", r.err());
        assert_eq!(std::fs::read(&seg).unwrap(), bad, "the log is left as it is");
        let short = &good[..good.len() - 5];
        std::fs::write(&seg, short).unwrap();
        let r = RecordLog::open(d.path(), 1 << 20, 3);
        assert!(matches!(r, Err(RecordLogError::BehindDatabase { log_next: 2, db_next: 3 })), "{:?}", r.err());
        assert_eq!(std::fs::read(&seg).unwrap(), short, "the log is left as it is");
        // One frame behind, the bad frame was never committed: it is removed.
        std::fs::write(&seg, &bad).unwrap();
        let (log, rep) = RecordLog::open(d.path(), 1 << 20, 2).unwrap();
        assert!(rep.torn_tail_removed);
        assert_eq!((log.next_n(), record_count(d.path()).unwrap()), (2, 2));
    }

    /// An Adopt keep list of any length that fits a frame decodes as it was encoded, past the
    /// 65,536 entries an earlier build refused, and a count larger than the bytes left is refused.
    #[test]
    fn an_adopt_keep_list_of_any_length_round_trips() {
        let adopt = |keep: Vec<(Hash32, u32)>| LogOp::Adopt {
            covenant_id: Hash32([1; 32]),
            old_txid: Hash32([2; 32]),
            old_index: 3,
            txid: Hash32([4; 32]),
            index: 5,
            value: 6,
            spk: vec![],
            daa: Some(7),
            custody: None,
            keep,
        };
        let mut b = batch(1);
        b.ops = vec![adopt((0..70_000u32).map(|i| (Hash32([(i % 251) as u8; 32]), i)).collect())];
        assert_eq!(LogBatch::decode(&b.encode(0)).unwrap().batch, b);
        // The keep count is the last byte of a frame whose last operation adopts nothing.
        b.ops = vec![adopt(vec![])];
        let mut body = b.encode(0);
        assert_eq!(body.pop(), Some(0));
        body.push(100);
        assert!(LogBatch::decode(&body).is_err());
    }

    /// A partial frame (a write that failed part way) with complete, chained frames after it is not
    /// a torn tail.
    ///
    /// `open` and `read_all` refuse the log and leave every byte of it in place, whether the
    /// partial frame's length runs past the end of the file or into the frames after it.
    #[test]
    fn an_incomplete_frame_followed_by_complete_frames_is_refused_and_kept() {
        for declared in [3_010u32, 12_000, 1 << 20] {
            let d = tempfile::tempdir().unwrap();
            let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
            log.append(&batch(0), 0).unwrap();
            let prev = log.last_chain;
            drop(log);
            let seg = segment_paths(d.path()).unwrap().pop().unwrap();
            let mut bytes = std::fs::read(&seg).unwrap();
            // The partial frame: its length, then only part of its body.
            bytes.extend_from_slice(&declared.to_le_bytes());
            bytes.extend_from_slice(&[7u8; 3000]);
            // Two complete frames chained from the last complete one.
            let mut chain = prev;
            for i in 1..3u8 {
                let body = batch(i).encode(0);
                chain = chain_hash(&chain, &body);
                bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
                bytes.extend_from_slice(&body);
                bytes.extend_from_slice(&chain);
            }
            std::fs::write(&seg, &bytes).unwrap();
            let r = RecordLog::open(d.path(), 1 << 20, 3);
            assert!(matches!(r, Err(RecordLogError::Corrupt { n: 1, .. })), "{declared}: {:?}", r.err());
            assert!(matches!(read_all(d.path()), Err(RecordLogError::Corrupt { n: 1, .. })));
            assert_eq!(std::fs::read(&seg).unwrap(), bytes, "{declared}: the log is left as it was");
        }
    }

    #[test]
    fn tampering_breaks_the_chain() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        for i in 0..3 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        let seg = segment_paths(d.path()).unwrap().pop().unwrap();
        let mut bytes = std::fs::read(&seg).unwrap();
        // Flip a byte inside the second frame's body.
        let first = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize + 36;
        bytes[first + 10] ^= 0xff;
        std::fs::write(&seg, bytes).unwrap();
        assert!(matches!(read_all(d.path()), Err(RecordLogError::Corrupt { .. })));
    }

    #[test]
    fn rotation_and_truncate_across_segments() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 200, 0).unwrap();
        for i in 0..6 {
            log.append(&batch(i), 0).unwrap();
        }
        assert!(segment_paths(d.path()).unwrap().len() > 1);
        assert_eq!(read_all(d.path()).unwrap().records.len(), 6);
        log.truncate_to(4).unwrap();
        assert_eq!(log.next_n(), 4);
        assert_eq!(read_all(d.path()).unwrap().records.len(), 4);
        log.append(&batch(7), 0).unwrap();
        let r = read_all(d.path()).unwrap();
        assert_eq!(r.records.len(), 5);
        drop(log);
        let (log, _) = RecordLog::open(d.path(), 200, 5).unwrap();
        assert_eq!(log.next_n(), 5);
    }

    #[test]
    fn sealed_segments_are_compressed_and_the_log_reads_and_extends_as_before() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 300, 0).unwrap();
        for i in 0..40 {
            log.append(&batch(i), 0).unwrap();
        }
        let paths = segment_paths(d.path()).unwrap();
        let gz = paths.iter().filter(|p| is_compressed(p)).count();
        assert!(gz >= 3, "{paths:?}");
        assert!(!is_compressed(paths.last().unwrap()), "the newest segment stays whole");
        assert_eq!(read_all(d.path()).unwrap().records.len(), 40);
        assert_eq!(record_count(d.path()).unwrap(), 40);
        drop(log);
        // Reopened and extended: the chain continues over the compressed segments.
        let (mut log, _) = RecordLog::open(d.path(), 300, 40).unwrap();
        log.append(&batch(41), 0).unwrap();
        assert_eq!(log.next_n(), 41);
        let r = read_all(d.path()).unwrap();
        assert_eq!(r.records.len(), 41);
        assert_eq!(r.records[40].1, batch(41));
        assert_eq!(compress_sealed(d.path()).unwrap().segments, 0, "idempotent");
        // The newest frame can still be cut after a rotation.
        log.truncate_to(40).unwrap();
        assert_eq!(read_all(d.path()).unwrap().records.len(), 40);
    }

    #[test]
    fn a_log_whole_before_compression_and_one_that_stopped_part_way_both_read_and_compress() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 300, 0).unwrap();
        for i in 0..30 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        let want = read_all(d.path()).unwrap().records;
        let gz: Vec<PathBuf> = segment_paths(d.path()).unwrap().into_iter().filter(|p| is_compressed(p)).collect();
        assert!(gz.len() >= 2);
        // An old log: every sealed segment whole.
        for p in &gz {
            rewrite_as(p, Form::Plain);
        }
        assert_eq!(read_all(d.path()).unwrap().records, want);
        // A compression of an older build that wrote the gzip and stopped before it removed the
        // segment: both files exist.
        let plain = segment_paths(d.path()).unwrap()[0].clone();
        let mut both = plain.as_os_str().to_owned();
        both.push(".gz");
        std::fs::write(&both, b"left over").unwrap();
        assert_eq!(read_all(d.path()).unwrap().records, want, "the whole segment is the one read");
        let r = compress_sealed(d.path()).unwrap();
        assert_eq!(r.segments, gz.len());
        assert!(r.after < r.before);
        assert!(!plain.exists());
        assert_eq!(read_all(d.path()).unwrap().records, want);
        assert_eq!(compress_sealed(d.path()).unwrap().segments, 0);
    }

    #[test]
    fn a_damaged_compressed_segment_is_an_error_not_a_shorter_log() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 300, 0).unwrap();
        for i in 0..30 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        let gz = segment_paths(d.path()).unwrap().into_iter().find(|p| is_compressed(p)).unwrap();
        let mut bytes = std::fs::read(&gz).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&gz, bytes).unwrap();
        assert!(read_all(d.path()).is_err());
        assert!(record_count(d.path()).is_err());
    }

    /// Rewrites the segment at `p` in form `to` (as an older build or a test needs it) and returns its path.
    fn rewrite_as(p: &Path, to: Form) -> PathBuf {
        let mut bytes = Vec::new();
        open_form(p).unwrap().read_to_end(&mut bytes).unwrap();
        let dst = with_form(p, to);
        match to {
            Form::Plain => std::fs::write(&dst, &bytes).unwrap(),
            Form::Gz => {
                let mut e = flate2::write::GzEncoder::new(File::create(&dst).unwrap(), flate2::Compression::new(6));
                e.write_all(&bytes).unwrap();
                e.finish().unwrap();
            }
            Form::Zst => std::fs::write(&dst, zstd::encode_all(&bytes[..], 3).unwrap()).unwrap(),
        }
        if dst != p {
            std::fs::remove_file(p).unwrap();
        }
        dst
    }

    fn forms(dir: &Path) -> Vec<Form> {
        segment_paths(dir).unwrap().iter().map(|p| Form::of(p)).collect()
    }

    /// A log of `n` frames in small segments; every sealed one is compressed.
    fn small_log(n: u8) -> (tempfile::TempDir, Vec<(u64, LogBatch)>) {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 300, 0).unwrap();
        for i in 0..n {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        let want = read_all(d.path()).unwrap().records;
        (d, want)
    }

    #[test]
    fn plain_gzip_and_zstd_segments_read_as_one_log_and_compact_to_zstd() {
        let (d, want) = small_log(30);
        let segs = segment_paths(d.path()).unwrap();
        assert!(segs.len() >= 5);
        let f = forms(d.path());
        assert!(f[..f.len() - 1].iter().all(|f| *f == Form::Zst), "a rotation writes zstd: {f:?}");
        assert_eq!(*f.last().unwrap(), Form::Plain);
        rewrite_as(&segs[0], Form::Gz);
        rewrite_as(&segs[1], Form::Plain);
        rewrite_as(&segs[2], Form::Gz);
        assert_eq!(read_all(d.path()).unwrap().records, want, "a mixed log reads the same");
        // the gzips only (next to a running writer)
        let r = compact(d.path(), Compact::Gzip).unwrap();
        assert_eq!(r.segments, 2);
        assert_eq!(forms(d.path())[..3], [Form::Zst, Form::Plain, Form::Zst]);
        assert_eq!(read_all(d.path()).unwrap().records, want);
        // the plain ones (the writer's pass), then everything is zstd and idempotent
        assert_eq!(compress_sealed(d.path()).unwrap().segments, 1);
        let f = forms(d.path());
        assert!(f[..f.len() - 1].iter().all(|f| *f == Form::Zst));
        assert_eq!(compact(d.path(), Compact::All).unwrap().segments, 0);
        assert_eq!(read_all(d.path()).unwrap().records, want);
        // the round trip is exact: every frame's bytes come back
        let (plain, _) = small_log(30);
        for (a, b) in segment_paths(d.path()).unwrap().iter().zip(segment_paths(plain.path()).unwrap()) {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            open_form(a).unwrap().read_to_end(&mut x).unwrap();
            open_form(&b).unwrap().read_to_end(&mut y).unwrap();
            assert_eq!(x, y);
        }
    }

    #[test]
    fn a_crash_at_each_step_of_a_compression_loses_no_frame() {
        for step in [Step::Writing, Step::Written, Step::Renamed] {
            for from in [Form::Plain, Form::Gz] {
                let (d, want) = small_log(30);
                let first = rewrite_as(&segment_paths(d.path()).unwrap()[0], from);
                CRASH_AFTER.set(Some(step));
                let err = compact(d.path(), Compact::All).expect_err("the simulated crash");
                CRASH_AFTER.set(None);
                assert!(err.to_string().contains("simulated crash"), "{err}");
                // the segment is there in the form it had, the copy beside it or in place too
                assert!(first.exists(), "{step:?} from {from:?}");
                assert_eq!(segment_paths(d.path()).unwrap()[0], first, "the form it started from is the one read");
                let tmp = tmp_path(&with_form(&first, Form::Zst));
                assert_eq!(tmp.exists(), step != Step::Renamed);
                assert_eq!(with_form(&first, Form::Zst).exists(), step == Step::Renamed);
                assert_eq!(read_all(d.path()).unwrap().records, want, "{step:?} from {from:?}");
                assert_eq!(verify(d.path()).unwrap().frames, 30);
                // the writer opens it and goes on
                let (mut log, _) = RecordLog::open(d.path(), 300, 30).unwrap();
                log.append(&batch(30), 0).unwrap();
                drop(log);
                // the next run finishes the job and leaves one form
                compact(d.path(), Compact::All).unwrap();
                assert!(!first.exists() && !tmp.exists());
                let f = forms(d.path());
                assert!(f[..f.len() - 1].iter().all(|f| *f == Form::Zst), "{f:?}");
                let r = read_all(d.path()).unwrap().records;
                assert_eq!(r[..30], want[..]);
                assert_eq!(r.len(), 31);
            }
        }
    }

    #[test]
    fn a_tampered_zstd_segment_is_refused() {
        let (d, want) = small_log(30);
        let seg = segment_paths(d.path()).unwrap()[1].clone();
        assert_eq!(Form::of(&seg), Form::Zst);
        let good = std::fs::read(&seg).unwrap();
        // a damaged byte: the zstd checksum or the chain refuses it
        let mut bad = good.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xff;
        std::fs::write(&seg, &bad).unwrap();
        assert!(read_all(d.path()).is_err());
        assert!(verify(d.path()).is_err());
        // a well-formed zstd of edited frames: the chain refuses it
        let mut frames = Vec::new();
        zstd::stream::read::Decoder::new(&good[..]).unwrap().read_to_end(&mut frames).unwrap();
        frames[10] ^= 0x01;
        std::fs::write(&seg, zstd::encode_all(&frames[..], 3).unwrap()).unwrap();
        match verify(d.path()) {
            Err(RecordLogError::Corrupt { reason, .. }) => assert!(reason.contains("chain hash mismatch"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(RecordLog::open(d.path(), 300, 30).is_err(), "the writer refuses it too");
        // a truncated one
        std::fs::write(&seg, &good[..good.len() - 8]).unwrap();
        assert!(read_all(d.path()).is_err());
        std::fs::write(&seg, &good).unwrap();
        assert_eq!(read_all(d.path()).unwrap().records, want);
    }

    #[test]
    fn a_reader_finds_a_segment_that_a_compaction_replaced_meanwhile() {
        let (d, want) = small_log(30);
        rewrite_as(&segment_paths(d.path()).unwrap()[0], Form::Gz);
        let listed = list_segments(d.path()).unwrap();
        assert_eq!(Form::of(&listed[0].1), Form::Gz);
        compact(d.path(), Compact::Gzip).unwrap();
        assert!(!listed[0].1.exists());
        // the reader listed the gzip; it reads the zstd that replaced it
        let mut n = 0;
        scan(&listed, |_, _| {
            n += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!(n, want.len());
        assert!(log_bytes(d.path()).unwrap() > 0);
    }

    #[test]
    fn two_compactions_of_one_log_never_run_at_once() {
        let (d, _) = small_log(10);
        let held = OpenOptions::new().create(true).truncate(false).write(true).open(d.path().join(COMPACT_LOCK)).unwrap();
        held.try_lock().unwrap();
        let err = compact(d.path(), Compact::Gzip).expect_err("locked");
        assert!(err.to_string().contains("another compaction"), "{err}");
        assert!(compact(d.path(), Compact::All).is_err());
        drop(held);
        compact(d.path(), Compact::All).unwrap();
    }

    /// Measurement on a COPY of real segments (`KOB_RECORDS_COPY=<a directory of copied sealed segments, never a live log>`,
    /// `cargo test --release -p kobd --lib -- --ignored --nocapture measure_the_compaction`): every one is compressed to zstd (an empty
    /// newest segment is added, since the newest is never compressed), with the ratio and the speed.
    #[test]
    #[ignore]
    fn measure_the_compaction_on_a_copy() {
        let Some(dir) = std::env::var_os("KOB_RECORDS_COPY") else { return };
        let dir = PathBuf::from(dir);
        let mut plain = 0u64;
        for (_, p) in list_segments(&dir).unwrap() {
            plain += plain_len(&p);
        }
        File::create(dir.join(segment_name(u64::MAX / 2))).unwrap();
        let before = log_bytes(&dir).unwrap();
        let t = std::time::Instant::now();
        let r = compact(&dir, Compact::All).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let after = log_bytes(&dir).unwrap();
        let t = std::time::Instant::now();
        for (_, p) in list_segments(&dir).unwrap() {
            digest_of(open_form(&p).unwrap()).unwrap();
        }
        let read = t.elapsed().as_secs_f64();
        println!(
            "{} segment(s): plain {plain} bytes, on disk {before} -> {after} bytes; {:.2}x of plain, {:.3} of before; \
             compaction {secs:.1} s ({:.0} MB/s of plain, read back and verified included), read {:.0} MB/s",
            r.segments,
            plain as f64 / after as f64,
            after as f64 / before as f64,
            plain as f64 / 1e6 / secs,
            plain as f64 / 1e6 / read
        );
    }

    fn plain_len(p: &Path) -> u64 {
        std::io::copy(&mut open_form(p).unwrap(), &mut std::io::sink()).unwrap()
    }

    // ---- template evolution ----

    use crate::indexer::record::{RecIn, RecOut, Reveal};
    use kob_protocol::artifacts::template;

    /// The dispatch tag of the current `KobPair.settle`, the kind under the record-log code the
    /// cross limits had.
    fn new_settle() -> [u8; 4] {
        let t = template(TemplateId::KobPair);
        *t.contract().entries.get("settle").expect("KobPair.settle").dispatch_tag.as_bytes()
    }

    /// The current `KobPair` state length.
    fn new_len() -> usize {
        template(TemplateId::KobPair).state_len
    }
    fn cross_tx(i: u8, state_len: usize, tag: [u8; 4], args: Vec<Vec<u8>>) -> TxRecord {
        TxRecord {
            txid: Hash32([i; 32]),
            pos: i as u32,
            payload: vec![],
            inputs: vec![
                RecIn {
                    txid: Hash32([i.wrapping_add(1); 32]),
                    index: 0,
                    cov: Some(Hash32([7; 32])),
                    daa: 50,
                    reveal: Some(Reveal { template: TemplateId::KobPair, state: vec![0x11; state_len], tag, args }),
                },
                RecIn { txid: Hash32([9; 32]), index: 3, cov: None, daa: 0, reveal: None },
            ],
            outputs: vec![RecOut { value: 1_000, spk: vec![], cov: None }],
            tok_outs: vec![],
            holds: vec![],
            kcc23: None,
            payer: None,
            in_value: None,
        }
    }

    fn batch_of(i: u8, txs: Vec<TxRecord>) -> LogBatch {
        LogBatch {
            start: Hash32([i; 32]),
            removed: vec![],
            cursor: LogCursor { hash: Hash32([i + 1; 32]), daa: 100 + i as u64 },
            blocks: vec![LogBlock { hash: Hash32([i + 1; 32]), daa: 100 + i as u64, blue: 1, ts: 2, txs }],
            ops: vec![],
        }
    }

    fn reveal_of(b: &LogBatch) -> Option<&Reveal> {
        b.blocks[0].txs[0].inputs[0].reveal.as_ref()
    }

    #[test]
    fn frames_this_build_cannot_decode_are_kept_and_skipped() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        let t = 1_790_000_000_000u64;
        log.append_body(&batch(0).encode(t)).unwrap();
        // Format-2 frames under code 0x08 whose template hash this build does not pin (an older
        // or a future artifact, the current state length or another).
        let mut unknown = batch_of(1, vec![cross_tx(1, 400, [1, 2, 3, 4], vec![])]).encode(t);
        let mut older = batch_of(2, vec![cross_tx(2, new_len(), new_settle(), vec![])]).encode(t);
        // Body: 0x00 0x02 | time (6-byte varint) | order table count | code | hash.
        let at = 2 + 6 + 1;
        assert_eq!(unknown[at], 0x08, "the code 0x08");
        unknown[at + 1..at + 33].copy_from_slice(&[0xee; 32]);
        older[at + 1..at + 33].copy_from_slice(&[0xdd; 32]);
        log.append_body(&unknown).unwrap();
        log.append_body(&older).unwrap();
        // A newer frame format, and a format-1 frame (no marker).
        let mut future = batch(3).encode(t);
        future[1] = 9;
        log.append_body(&future).unwrap();
        let mut w = Writer::new();
        w.var(t);
        w.hash(&Hash32([4; 32]));
        w.hash(&Hash32([5; 32]));
        w.var(104);
        w.var(0);
        w.var(1);
        w.hash(&Hash32([5; 32]));
        w.var(104);
        w.var(1);
        w.var(2);
        w.var(1);
        w.hash(&Hash32([6; 32]));
        w.var(0);
        w.bytes(&[]);
        w.var(1);
        w.u8(3);
        w.hash(&Hash32([7; 32]));
        w.var(0);
        w.hash(&Hash32([8; 32]));
        w.var(1);
        w.u8(0x7e);
        w.raw(&[0; 64]);
        log.append_body(&w.buf).unwrap();
        log.append_body(&batch(6).encode(t)).unwrap();
        drop(log);
        let r = read_all(d.path()).unwrap();
        assert_eq!(r.frames, 6);
        assert_eq!(r.skipped.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![3, 4]);
        assert!(r.skipped[0].1.contains("newer"), "{}", r.skipped[0].1);
        assert!(r.skipped[1].1.contains("format 1"), "{}", r.skipped[1].1);
        assert_eq!(r.records.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![0, 1, 2, 5]);
        assert_eq!(r.dropped.unknown_reveals, 2);
        assert!(reveal_of(&r.records[1].1).is_none() && reveal_of(&r.records[2].1).is_none());
        assert_eq!(r.records[1].1.blocks[0].txs[0].inputs.len(), 2);
        assert_eq!(r.records[3].1, batch(6));
    }

    #[test]
    fn a_frame_lists_exactly_the_templates_it_refers_to() {
        let b = batch_of(1, vec![cross_tx(1, new_len(), new_settle(), vec![])]);
        let (orders, programs) = b.tables();
        assert_eq!(orders.into_iter().collect::<Vec<_>>(), vec![(0x08, template(TemplateId::KobPair).hash)]);
        assert!(programs.is_empty());
        // A batch without reveals pays two bytes for the empty tables after the marker and the
        // write time.
        assert_eq!(batch(1).encode(5)[..5], [0, FORMAT, 5, 0, 0]);
    }
}
