//! The permanent record log: append-only, compact, hash-chained.
//!
//! What it holds is what the node cannot give back after pruning and what the indexer cannot
//! re-derive: the extracted KOB records (`record::TxRecord`: KOB1 genesis payloads, every
//! transaction that touched a KOB covenant, token custody / stray outputs) grouped by chain
//! block, plus the reorg and operator operations that changed the derived state. It never holds
//! a raw node response. The database (`index.sqlite3`) is a derived view of it: any schema change or
//! lost database is repaired with `kob-executor index replay`, which re-applies the records and then
//! re-syncs the tail from the node.
//!
//! Frame layout, one per committed batch:
//!
//! ```text
//! u32 LE body length | body | 32-byte chain hash = blake3(previous chain hash || body)
//! ```
//!
//! The body is versioned ([`FORMAT`]). Format 2 (since 2026-10-01) is self-describing: `0x00 0x02`, the write time, a
//! template table per frame (record-log code -> template hash of every order template and token program the frame refers
//! to) and length-prefixed states, so a frame decodes whatever the templates are when it is read; a reveal of a template
//! this build does not have (an older artifact of a kind, or an unknown kind) is dropped and counted, never misread.
//! Format 1 (the frames written before, no marker) stored a revealed state at its template's length of the time; it is read
//! with today's layouts plus the retired ones of [`super::layouts`]. A frame neither format can decode is kept and skipped
//! by [`read_all`] with a counted warning. Both formats can follow each other in one segment (an older build appends
//! format 1 to a log this build wrote: `open` checks the chain only and never decodes a body).
//!
//! The chain detects truncation in the middle and edits. Ordering guarantee with the database: a frame
//! is appended and fsynced BEFORE the database transaction commits, and the database stores
//! `records_next_n`. After a crash the log can hold one frame the database never committed;
//! [`RecordLog::open`] cuts it off. The reverse (database ahead of the log) means the log was lost and is
//! reported as an error.

use super::record::{decode_v1, DecodeCtx, DecodeStats, TemplateTable, TxRecord};
use crate::hex::Hash32;
use crate::wire::{Reader, WireError, Writer};
use kob_protocol::artifacts::TemplateId;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCursor {
    pub hash: Hash32,
    pub daa: u64,
}

/// A chain block that carried KOB-relevant transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogBlock {
    pub hash: Hash32,
    pub daa: u64,
    pub blue: u64,
    pub ts: u64,
    pub txs: Vec<TxRecord>,
}

/// The custody token UTXO of an imported ask-side order (verified against the node when the maker
/// supplied the extension commitment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCustody {
    pub txid: Hash32,
    pub index: u32,
    pub value: u64,
    pub amount: i64,
    /// KCC-20 extension commitment of the custody token.
    pub ext: Hash32,
}

/// Operator operations that are not chain data but change the derived state, logged so a replay
/// reproduces them (recovery imports, gap reconciliation).
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
        /// The entry of an if-done exit (from the export; `None` for every other order and in operations written before
        /// 2026-10-02, op code 1).
        parent: Option<Hash32>,
        /// A sell-first `KobIfdPair`'s second custody (its B prefund; op code 6). `None` for every other order.
        prefund: Option<ImportCustody>,
    },
    /// A tracked order output the node no longer has (spent during a gap): close its lineage.
    CloseGap { txid: Hash32, index: u32, covenant_id: Hash32 },
    /// A tracked order continued during a gap under the same script: adopt the node's current output.
    Adopt {
        covenant_id: Hash32,
        old_txid: Hash32,
        old_index: u32,
        txid: Hash32,
        index: u32,
        value: u64,
        spk: Vec<u8>,
        /// The DAA score the node reports for the adopted output (its refund / kill time counts from it). `None` in operations
        /// written before 2026-10-02 (op code 3): the cursor's DAA stands in, as it did then.
        daa: Option<u64>,
        /// The order's custody, found by the reconciliation at the custody script it derives (token-holding kinds).
        custody: Option<ImportCustody>,
        /// Token UTXOs owned by the order (custody or strays) the node still holds: they stay live, every other one is closed.
        keep: Vec<(Hash32, u32)>,
    },
}

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
    pub ops: Vec<LogOp>,
}

fn code_of(t: TemplateId) -> u8 {
    crate::model::wire_code(t)
}

/// Record-log frame format this build writes. Format 1 (no marker; frames written before 2026-10-01) stores a revealed
/// order state at its template's length of the time; format 2 starts the body with the marker `0x00 0x02`, carries a
/// template table (wire code -> template hash) and length-prefixes every state, so a frame decodes without assuming
/// today's templates. A format-1 body never starts with `0x00` (it starts with the write time in ms, a varint > 0).
pub const FORMAT: u8 = 2;

impl LogOp {
    fn encode(&self, w: &mut Writer) {
        match self {
            LogOp::Import { template, state, covenant_id, txid, index, value, spk, daa, custody, parent, prefund } => {
                // op 5 = op 1 followed by the parent (op 1 is still read); op 6 = op 5 followed by the second custody (a pair
                // entry's B prefund), written only when there is one
                w.u8(if prefund.is_some() { 6 } else { 5 });
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
                if prefund.is_some() {
                    put_custody(w, prefund.as_ref());
                }
            }
            LogOp::CloseGap { txid, index, covenant_id } => {
                w.u8(2);
                w.hash(txid);
                w.var(*index as u64);
                w.hash(covenant_id);
            }
            LogOp::Adopt { covenant_id, old_txid, old_index, txid, index, value, spk, daa, custody, keep } => {
                // op 4 = op 3 followed by the output's DAA, the custody and the kept token UTXOs (op 3 is still read)
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
        }
    }

    /// `None`: an import of an order layout this build cannot interpret (read past and counted in `ctx`).
    fn decode(r: &mut Reader<'_>, ctx: &mut DecodeCtx<'_>) -> Result<Option<LogOp>, WireError> {
        Ok(Some(match r.u8()? {
            op @ (1 | 5 | 6) => {
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
                let prefund = if op == 6 { get_custody(r)? } else { None };
                let Some(template) = ctx.import_template(code, state.len())? else { return Ok(None) };
                LogOp::Import { template, state, covenant_id, txid, index, value, spk, daa, custody, parent, prefund }
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
                    let n = r.var()?;
                    if n > 65_536 {
                        return Err(WireError("adopt keep count"));
                    }
                    let mut keep = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        keep.push((r.hash()?, r.var()? as u32));
                    }
                    (daa, custody, keep)
                } else {
                    (None, None, Vec::new())
                };
                LogOp::Adopt { covenant_id, old_txid, old_index, txid, index, value, spk, daa, custody, keep }
            }
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
    /// Record-log format of the frame (1 or 2).
    pub format: u8,
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

    /// Encodes the batch as a format-2 frame body.
    pub fn encode(&self, now_ms: u64) -> Vec<u8> {
        self.encode_as(now_ms, FORMAT)
    }

    /// Encodes the batch as a format-1 frame body (what builds before 2026-10-01 wrote; `now_ms` must be positive). Tests
    /// and tools reproduce old logs with it; this build never writes it.
    #[doc(hidden)]
    pub fn encode_v1(&self, now_ms: u64) -> Vec<u8> {
        assert!(now_ms > 0, "a format-1 body starts with a positive write time");
        self.encode_as(now_ms, 1)
    }

    fn encode_as(&self, now_ms: u64, format: u8) -> Vec<u8> {
        let mut w = Writer::new();
        if format >= 2 {
            w.u8(0);
            w.u8(format);
        }
        w.var(now_ms);
        if format >= 2 {
            let (orders, programs) = self.tables();
            write_table(&mut w, &orders);
            write_table(&mut w, &programs);
        }
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
                if format >= 2 {
                    t.encode(&mut w);
                } else {
                    t.encode_v1(&mut w);
                }
            }
        }
        w.var(self.ops.len() as u64);
        for o in &self.ops {
            o.encode(&mut w);
        }
        w.buf
    }

    /// Decodes a frame body of either format.
    pub fn decode(body: &[u8]) -> Result<DecodedBatch, WireError> {
        match body {
            [0, FORMAT, rest @ ..] => {
                let mut r = Reader::new(rest);
                let written_ms = r.var()?;
                let orders = read_table(&mut r)?;
                let programs = read_table(&mut r)?;
                let mut ctx = DecodeCtx::v2(&orders, &programs);
                let batch = LogBatch::decode_rest(&mut r, &mut ctx)?;
                if !r.done() {
                    return Err(WireError("trailing bytes"));
                }
                Ok(DecodedBatch { batch, written_ms, format: FORMAT, stats: ctx.stats })
            }
            [0, ..] => Err(WireError("record-log frame format newer than this build")),
            _ => {
                let ((written_ms, batch), stats) = decode_v1(body, |r, ctx| Ok((r.var()?, LogBatch::decode_rest(r, ctx)?)))?;
                Ok(DecodedBatch { batch, written_ms, format: 1, stats })
            }
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

#[derive(Debug, thiserror::Error)]
pub enum RecordLogError {
    #[error("record log io: {0}")]
    Io(#[from] std::io::Error),
    #[error("record log frame {n} is corrupt: {reason}")]
    Corrupt { n: u64, reason: String },
    #[error("record log ends at frame {log_next} but the database expects {db_next}: the log was lost or replaced")]
    BehindDatabase { log_next: u64, db_next: u64 },
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

fn list_segments(dir: &Path) -> std::io::Result<Vec<(u64, PathBuf)>> {
    let mut v = Vec::new();
    if !dir.exists() {
        return Ok(v);
    }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if let Some(rest) = name.strip_prefix("seg-").and_then(|r| r.strip_suffix(".kobrec")) {
            if let Ok(n) = rest.parse::<u64>() {
                v.push((n, p));
            }
        }
    }
    v.sort();
    Ok(v)
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

fn read_frame(r: &mut BufReader<File>) -> std::io::Result<FrameRead> {
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

/// Where a scan stopped.
struct ScanEnd {
    /// Number of frames accepted.
    frames: u64,
    last_chain: [u8; 32],
    /// `(segment index, byte offset)` of the first byte that is not part of an accepted frame, when
    /// the scan stopped early (torn tail or `stop` returned true).
    cut: Option<(usize, u64)>,
    torn: bool,
}

/// Walk every frame in order, verifying the chain. `visit(n, body)` returns `true` to stop before
/// frame `n` (the offset of that frame is reported in `cut`).
fn scan(
    segs: &[(u64, PathBuf)],
    mut visit: impl FnMut(u64, &[u8]) -> Result<bool, RecordLogError>,
) -> Result<ScanEnd, RecordLogError> {
    let mut n = 0u64;
    let mut prev = ZERO;
    for (si, (_, path)) in segs.iter().enumerate() {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut rdr = BufReader::new(file);
        let mut offset = 0u64;
        loop {
            match read_frame(&mut rdr)? {
                FrameRead::Eof => break,
                FrameRead::Torn => {
                    if si != segs.len() - 1 {
                        return Err(RecordLogError::Corrupt { n, reason: "incomplete frame before the end of the log".into() });
                    }
                    return Ok(ScanEnd { frames: n, last_chain: prev, cut: Some((si, offset)), torn: true });
                }
                FrameRead::Frame { body, chain, total } => {
                    if chain_hash(&prev, &body) != chain {
                        let last = si == segs.len() - 1 && offset + total == file_len;
                        if !last {
                            return Err(RecordLogError::Corrupt { n, reason: "chain hash mismatch".into() });
                        }
                        return Ok(ScanEnd { frames: n, last_chain: prev, cut: Some((si, offset)), torn: true });
                    }
                    if visit(n, &body)? {
                        return Ok(ScanEnd { frames: n, last_chain: prev, cut: Some((si, offset)), torn: false });
                    }
                    prev = chain;
                    n += 1;
                    offset += total;
                }
            }
        }
    }
    Ok(ScanEnd { frames: n, last_chain: prev, cut: None, torn: false })
}

/// What [`RecordLog::open`] found and repaired.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OpenReport {
    /// Frames past the database position that were cut off (crash between log and commit).
    pub dropped_uncommitted: u64,
    /// A torn (partial) last frame that was removed.
    pub torn_tail_removed: bool,
}

pub struct RecordLog {
    dir: PathBuf,
    segment_bytes: u64,
    file: File,
    seg_len: u64,
    next_n: u64,
    last_chain: [u8; 32],
}

impl RecordLog {
    /// Open the log, reconcile it with the database position `db_next_n`, and position for append.
    /// `db_next_n` is 0 for a fresh database.
    pub fn open(dir: &Path, segment_bytes: u64, db_next_n: u64) -> Result<(RecordLog, OpenReport), RecordLogError> {
        std::fs::create_dir_all(dir)?;
        let mut report = OpenReport::default();
        let mut segs = list_segments(dir)?;
        let end = scan(&segs, |n, _| Ok(n >= db_next_n))?;
        let mut total_frames = end.frames;
        if let Some((si, offset)) = end.cut {
            if end.torn {
                report.torn_tail_removed = true;
            } else {
                // frames at or after db_next_n: count them, then cut
                let all = scan(&segs, |_, _| Ok(false))?;
                report.dropped_uncommitted = all.frames.saturating_sub(end.frames);
            }
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
        // A torn tail can sit after uncommitted frames: after the cut above the log ends at frame `end.frames`.
        let (path, seg_len) = match segs.last() {
            Some((_, p)) => (p.clone(), std::fs::metadata(p)?.len()),
            None => (dir.join(segment_name(total_frames)), 0),
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok((
            RecordLog { dir: dir.to_path_buf(), segment_bytes, file, seg_len, next_n: total_frames, last_chain: end.last_chain },
            report,
        ))
    }

    pub fn next_n(&self) -> u64 {
        self.next_n
    }

    /// Append one batch and fsync. Returns the frame number and the frame's size in bytes.
    pub fn append(&mut self, batch: &LogBatch, now_ms: u64) -> Result<(u64, u64), RecordLogError> {
        self.append_body(&batch.encode(now_ms))
    }

    /// Append one frame body as it is (tests and tools write frames of other formats with it). Returns the frame number
    /// and size.
    #[doc(hidden)]
    pub fn append_body(&mut self, body: &[u8]) -> Result<(u64, u64), RecordLogError> {
        if self.seg_len >= self.segment_bytes && self.seg_len > 0 {
            self.file.sync_all()?;
            let path = self.dir.join(segment_name(self.next_n));
            self.file = OpenOptions::new().create(true).append(true).open(path)?;
            self.seg_len = 0;
        }
        let n = self.next_n;
        let chain = chain_hash(&self.last_chain, body);
        let mut frame = Vec::with_capacity(body.len() + 36);
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body);
        frame.extend_from_slice(&chain);
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.seg_len += frame.len() as u64;
        self.last_chain = chain;
        self.next_n += 1;
        Ok((n, frame.len() as u64))
    }

    /// Discard frames `>= n` (a commit failed after the append). Only the newest frames can be cut.
    pub fn truncate_to(&mut self, n: u64) -> Result<(), RecordLogError> {
        if n >= self.next_n {
            return Ok(());
        }
        let segs = list_segments(&self.dir)?;
        let end = scan(&segs, |k, _| Ok(k >= n))?;
        if let Some((si, offset)) = end.cut {
            let f = OpenOptions::new().write(true).open(&segs[si].1)?;
            f.set_len(offset)?;
            f.sync_all()?;
            for (_, p) in segs.iter().skip(si + 1) {
                std::fs::remove_file(p)?;
            }
            self.file = OpenOptions::new().append(true).open(&segs[si].1)?;
            self.seg_len = offset;
        }
        self.next_n = end.frames;
        self.last_chain = end.last_chain;
        Ok(())
    }
}

/// Number of complete frames in a log directory, without decoding them. Used at startup to refuse
/// an empty database next to a non-empty log (that is a job for `replay`, never for `open`).
pub fn record_count(dir: &Path) -> Result<u64, RecordLogError> {
    let segs = list_segments(dir)?;
    Ok(scan(&segs, |_, _| Ok(false))?.frames)
}

/// Total size of the log segments in bytes.
pub fn log_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut t = 0;
    for (_, p) in list_segments(dir)? {
        t += std::fs::metadata(p)?.len();
    }
    Ok(t)
}

/// Every frame of a log directory in order, verified.
pub struct Replay {
    /// The frames this build decoded, with their numbers.
    pub records: Vec<(u64, LogBatch)>,
    pub torn: bool,
    /// Every complete frame of the log (decoded or skipped): the database position after a replay.
    pub frames: u64,
    /// Frames this build could not decode at all (a newer format, or a layout no table here describes), with the
    /// reason. They stay in the log; a replay skips them.
    pub skipped: Vec<(u64, String)>,
    /// Frames per format (index 1 and 2).
    pub formats: [u64; 3],
    /// What the decoded frames had to drop (reveals / holdings / imports of retired or unknown layouts).
    pub dropped: DecodeStats,
}

/// Every frame of a log directory in order, chain verified. A frame that is authentic (its chain hash verifies) but that
/// this build cannot decode is skipped and reported in [`Replay::skipped`] instead of failing the whole read; a broken
/// chain is still an error.
pub fn read_all(dir: &Path) -> Result<Replay, RecordLogError> {
    let segs = list_segments(dir)?;
    let mut records = Vec::new();
    let mut skipped = Vec::new();
    let mut formats = [0u64; 3];
    let mut dropped = DecodeStats::default();
    let end = scan(&segs, |n, body| {
        match LogBatch::decode(body) {
            Ok(d) => {
                formats[d.format.min(2) as usize] += 1;
                dropped.add(&d.stats);
                records.push((n, d.batch));
            }
            Err(e) => skipped.push((n, e.to_string())),
        }
        Ok(false)
    })?;
    Ok(Replay { records, torn: end.torn, frames: end.frames, skipped, formats, dropped })
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
            // a sell-first pair entry's import carries its second custody (op 6); any other import is op 5
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
            },
        ];
        let d = LogBatch::decode(&b.encode(5)).unwrap();
        assert_eq!(d.batch, b);

        // operations written before 2026-10-02: op 3 (adopt without DAA, custody and kept token UTXOs)
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
        assert_eq!((d.batch, d.written_ms, d.format), (b, 1234, FORMAT));
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

    #[test]
    fn uncommitted_tail_is_cut_and_log_behind_db_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        for i in 0..4 {
            log.append(&batch(i), 0).unwrap();
        }
        drop(log);
        // database only committed 3 frames: the 4th is cut off
        let (mut log, rep) = RecordLog::open(d.path(), 1 << 20, 3).unwrap();
        assert_eq!(rep.dropped_uncommitted, 1);
        assert_eq!(log.next_n(), 3);
        // and the chain continues correctly
        log.append(&batch(9), 0).unwrap();
        drop(log);
        assert_eq!(read_all(d.path()).unwrap().records.len(), 4);
        // database claims more than the log has
        assert!(matches!(RecordLog::open(d.path(), 1 << 20, 7), Err(RecordLogError::BehindDatabase { log_next: 4, db_next: 7 })));
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
        // flip a byte inside the second frame's body
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

    // ---- template evolution (record-log formats 1 and 2) ----

    use crate::indexer::record::{RecIn, RecOut, Reveal};
    use kob_protocol::artifacts::template;

    /// `settle` of the KobCross of protocol v2.6 before the pair-market auction (333-byte state) and of the v2.6 one with the
    /// auction (351, retired by v3). Today's code 0x08 is `KobPair` (payload v4): its `settle` is [`new_settle`].
    const OLD_SETTLE: [u8; 4] = [0x1e, 0x6c, 0xf5, 0x28];
    const V26_SETTLE: [u8; 4] = [0xbd, 0x44, 0x24, 0x3d];

    /// The dispatch tag of today's `KobPair.settle` (the kind under the record-log code the cross limits had).
    fn new_settle() -> [u8; 4] {
        let t = template(TemplateId::KobPair);
        *t.contract().entries.get("settle").expect("KobPair.settle").dispatch_tag.as_bytes()
    }

    /// Today's `KobPair` state length.
    fn new_len() -> usize {
        template(TemplateId::KobPair).state_len
    }
    const CANCEL: [u8; 4] = [0xa0, 0x89, 0x31, 0x09];
    const OLD_CROSS_HASH: &str = "70b1dcd3e5f2c3821f031d5346680d0772fb37851760aea8fa841e80424a92c7";
    const V26_CROSS_HASH: &str = "692cfdd07dae71749db2d0ad8249eb70c7476e9ef67a4f8f4c09c409e991b752";

    /// A format-2 body whose single order template (at the head of its table) is replaced by a retired layout's hash: what a
    /// build that pinned that template wrote.
    fn with_table_hash(mut body: Vec<u8>, hash: &str) -> Vec<u8> {
        // body: 0x00 0x02 | time (6-byte varint) | order table count | code | hash
        let at = 2 + 6 + 1;
        assert_eq!(body[at], 0x08, "the code of the retired cross limits, today KobPair's");
        let l = crate::indexer::layouts::RETIRED_LAYOUTS.iter().find(|l| l.hash == hash).unwrap();
        body[at + 1..at + 33].copy_from_slice(&l.hash_bytes());
        body
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
    fn the_cross_layouts_this_test_uses_are_the_real_ones() {
        assert_eq!(new_len(), 414);
        assert_eq!(crate::model::wire_code(TemplateId::KobPair), 0x08);
        assert_eq!(crate::model::entry_name(TemplateId::KobPair, new_settle()), Some("settle"));
        assert_eq!(crate::model::entry_name(TemplateId::KobPair, CANCEL), Some("cancel"));
        assert_eq!(crate::model::entry_name(TemplateId::KobPair, OLD_SETTLE), None);
        assert_eq!(crate::model::entry_name(TemplateId::KobPair, V26_SETTLE), None);
        let old = crate::indexer::layouts::retired(0x08).find(|l| l.hash == OLD_CROSS_HASH).unwrap();
        assert!(old.state_len == 333 && old.has_tag(&OLD_SETTLE) && old.has_tag(&CANCEL));
        let v26 = crate::indexer::layouts::retired(0x08).find(|l| l.hash == V26_CROSS_HASH).unwrap();
        assert!(v26.state_len == 351 && v26.has_tag(&V26_SETTLE) && v26.has_tag(&CANCEL));
    }

    #[test]
    fn a_log_mixing_old_and_new_cross_layouts_reads_end_to_end() {
        // what the soak's exec-a log holds since the 958d013 swap: format-1 frames of the old KobCross, then (old build
        // replaced) format-1 frames of the v2.6 one, then format-2 frames of the v2.6 one (a build of 2026-10-01 .. 10-05);
        // this build (protocol v3, pair orders) appends format 2 of today's KobPair (the same code 0x08) after them: every reveal
        // of a retired layout is dropped and counted, either format, today's decodes exactly
        let nb = vec![2, 0, 0, 0, 0, 0, 0, 0];
        let frames = [
            (batch_of(1, vec![cross_tx(1, 333, OLD_SETTLE, vec![nb.clone(), vec![5], vec![]])]), 1),
            (batch_of(2, vec![cross_tx(2, 333, CANCEL, vec![vec![]])]), 1),
            (batch_of(3, vec![cross_tx(3, 351, V26_SETTLE, vec![nb.clone(), vec![5], vec![], vec![1], vec![7]])]), 1),
            (batch_of(4, vec![cross_tx(4, 351, V26_SETTLE, vec![nb.clone(), vec![6], vec![], vec![1], vec![7]])]), 26),
            (
                batch_of(
                    5,
                    vec![cross_tx(5, new_len(), new_settle(), vec![nb.clone(), vec![6], vec![], vec![1], vec![7], vec![0x81]])],
                ),
                2,
            ),
            (batch(6), 2),
        ];
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        for (b, fmt) in &frames {
            let body = match *fmt {
                1 => b.encode_v1(1_790_000_000_000),
                26 => with_table_hash(b.encode(1_790_000_000_000), V26_CROSS_HASH),
                _ => b.encode(1_790_000_000_000),
            };
            log.append_body(&body).unwrap();
        }
        drop(log);
        let first = read_all(d.path()).unwrap();
        assert_eq!(first.frames, 6);
        assert!(first.skipped.is_empty(), "{:?}", first.skipped);
        assert_eq!(first.records.len(), 6);
        assert_eq!(first.formats, [0, 3, 3]);
        assert_eq!(first.dropped.retired_reveals, 4, "the four retired-layout reveals are dropped and counted");
        assert_eq!(first.dropped.unknown_reveals, 0);
        let recs: Vec<&LogBatch> = first.records.iter().map(|(_, b)| b).collect();
        // retired layouts: the reveal is dropped, everything else of the record (inputs, outputs, cursor) is kept
        for b in &recs[..4] {
            assert!(reveal_of(b).is_none());
            assert_eq!(b.blocks[0].txs[0].inputs.len(), 2);
            assert_eq!(b.blocks[0].txs[0].inputs[0].cov, Some(Hash32([7; 32])));
            assert_eq!(b.blocks[0].txs[0].outputs[0].value, 1_000);
        }
        assert_eq!(recs[0].cursor.daa, 101);
        // today's layout: decoded exactly
        assert_eq!(recs[4], &frames[4].0);
        assert_eq!(reveal_of(recs[4]).unwrap().state.len(), new_len());
        assert_eq!(*recs[5], batch(6));
        // the log stays appendable and a reopen (chain check only) accepts the mixed formats
        let (log, rep) = RecordLog::open(d.path(), 1 << 20, 6).unwrap();
        assert_eq!((log.next_n(), rep), (6, OpenReport::default()));
    }

    #[test]
    fn an_ambiguous_old_reveal_resolves_by_decoding_the_whole_frame() {
        // an old cancel whose argument happens to hold the v2.6 cross limit's settle tag exactly where that layout would read
        // its tag (offset 351): both retired layouts fit locally; the frame decodes with the old one (tried first)
        let mut arg = vec![0xff; 30];
        // state(333) + tag(4) + arg count(1) + arg length(1) = 339: the arg starts there, offset 351 is arg[12]
        arg[12..16].copy_from_slice(&V26_SETTLE);
        let b = batch_of(1, vec![cross_tx(1, 333, CANCEL, vec![arg])]);
        let body = b.encode_v1(1_790_000_000_000);
        let d = LogBatch::decode(&body).unwrap();
        assert_eq!(d.format, 1);
        assert_eq!(d.stats.ambiguous_frames, 1);
        assert_eq!(d.stats.retired_reveals, 1);
        assert!(reveal_of(&d.batch).is_none());
        assert_eq!(d.batch.blocks[0].txs[0].inputs[1].index, 3);
        assert_eq!(d.batch.cursor, b.cursor);

        // the reverse: a v2.6 settle (351) whose state happens to hold the old cancel tag at offset 333, followed by a byte
        // no argument count can start with: the old layout (tried first) fits locally but cannot decode the frame; the search
        // backtracks to the v2.6 layout
        let mut tx = cross_tx(2, 351, V26_SETTLE, vec![vec![2, 0, 0, 0, 0, 0, 0, 0], vec![5], vec![], vec![1], vec![7]]);
        let st = &mut tx.inputs[0].reveal.as_mut().unwrap().state;
        st[333..337].copy_from_slice(&CANCEL);
        st[337] = 0xff;
        let b = batch_of(2, vec![tx]);
        let d = LogBatch::decode(&b.encode_v1(1_790_000_000_000)).unwrap();
        assert_eq!((d.format, d.stats.ambiguous_frames, d.stats.retired_reveals), (1, 1, 1));
        assert!(reveal_of(&d.batch).is_none());
        assert_eq!(d.batch.blocks[0].txs[0].inputs[1].index, 3);
        assert_eq!(d.batch.cursor, b.cursor);
    }

    #[test]
    fn frames_this_build_cannot_decode_are_kept_and_skipped() {
        let d = tempfile::tempdir().unwrap();
        let (mut log, _) = RecordLog::open(d.path(), 1 << 20, 0).unwrap();
        let t = 1_790_000_000_000u64;
        log.append_body(&batch(0).encode(t)).unwrap();
        // a format-2 frame under code 0x08 whose template hash this build does not know (a future artifact)
        let mut unknown = batch_of(1, vec![cross_tx(1, 400, [1, 2, 3, 4], vec![])]).encode(t);
        let mut retired = batch_of(2, vec![cross_tx(2, 333, OLD_SETTLE, vec![])]).encode(t);
        // body: 0x00 0x02 | time (6-byte varint) | order table count | code | hash
        let at = 2 + 6 + 1;
        assert_eq!(unknown[at], 0x08, "the code 0x08");
        unknown[at + 1..at + 33].copy_from_slice(&[0xee; 32]);
        let old = crate::indexer::layouts::retired(0x08).find(|l| l.hash == OLD_CROSS_HASH).unwrap();
        retired[at + 1..at + 33].copy_from_slice(&old.hash_bytes());
        log.append_body(&unknown).unwrap();
        log.append_body(&retired).unwrap();
        // a newer frame format, and a format-1 frame of a kind code no build ever had
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
        assert_eq!(r.records.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![0, 1, 2, 5]);
        assert_eq!((r.dropped.unknown_reveals, r.dropped.retired_reveals), (1, 1));
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
        // a batch without reveals pays two bytes for the empty tables
        let plain = batch(1);
        assert_eq!(plain.encode(5).len(), plain.encode_v1(5).len() + 4);
    }
}
