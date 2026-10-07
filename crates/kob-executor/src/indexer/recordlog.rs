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
//! this build does not pin is dropped and counted, never misread. A frame this build cannot decode (format 1, written
//! before 2026-10-01 without a marker, or a newer format) is kept and skipped by [`read_all`] with a counted warning
//! (`open` checks the chain only and never decodes a body).
//!
//! The chain detects truncation in the middle and edits. Ordering guarantee with the database: a frame
//! is appended and fsynced BEFORE the database transaction commits, and the database stores
//! `records_next_n`. After a crash the log can hold one frame the database never committed;
//! [`RecordLog::open`] cuts it off. The reverse (database ahead of the log) means the log was lost and is
//! reported as an error. A failed append (full disk, I/O error) is rolled back to the last complete frame before the
//! error is returned, so nothing is ever appended after a partial frame; an incomplete frame with complete frames after
//! it is corruption (refused), only an incomplete last frame is a torn tail (removed).

use super::record::{DecodeCtx, DecodeStats, TemplateTable, TxRecord};
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

/// Record-log frame format this build writes and reads: the body starts with the marker `0x00 0x02`, carries a template
/// table (wire code -> template hash) and length-prefixes every state, so a frame decodes without assuming today's
/// templates. Format 1 (frames written before 2026-10-01, no marker: the body starts with the write time in ms, a varint >
/// 0) is not read.
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
        let mut w = Writer::new();
        w.u8(0);
        w.u8(FORMAT);
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

    /// Decodes a frame body (format 2).
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

#[derive(Debug, thiserror::Error)]
pub enum RecordLogError {
    #[error("record log io: {0}")]
    Io(#[from] std::io::Error),
    #[error("record log frame {n} is corrupt: {reason}")]
    Corrupt { n: u64, reason: String },
    #[error("record log ends at frame {log_next} but the database expects {db_next}: the log was lost or replaced")]
    BehindDatabase { log_next: u64, db_next: u64 },
    /// An append or a truncation failed and the segment could not be put back to its last complete frame: nothing more is
    /// written until a restart, whose `open` repairs the log (a torn last frame) or refuses it.
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

/// Whether the bytes of `path` after `offset` (an incomplete frame) hold a complete frame that continues the chain from
/// `prev`. A crash during an append leaves nothing after the incomplete frame; a complete, chained frame after it means
/// the incomplete one sits in the middle of the log, and cutting there would drop good frames.
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
                    if complete_frame_follows(path, offset, &prev)? {
                        return Err(RecordLogError::Corrupt {
                            n,
                            reason: "incomplete frame followed by complete frames (a write that failed part way, then \
                                     later appends); the log is left as it is"
                                .into(),
                        });
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

    pub fn next_n(&self) -> u64 {
        self.next_n
    }

    /// Append one batch and fsync. Returns the frame number and the frame's size in bytes.
    pub fn append(&mut self, batch: &LogBatch, now_ms: u64) -> Result<(u64, u64), RecordLogError> {
        self.append_body(&batch.encode(now_ms))
    }

    /// Append one frame body as it is (tests and tools write frames of other formats with it). Returns the frame number
    /// and size.
    ///
    /// A failed write or fsync is rolled back: the segment is cut back to its length before the append, so the next
    /// append starts right after the last complete frame. When even that fails, the log refuses every later write
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

    /// Cut the current segment back to its last complete frame after a failed append. Returns the error to report: the
    /// append's own when the cut succeeded; [`RecordLogError::Stopped`] when it did not (the log then refuses every later
    /// write).
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

    /// Discard frames `>= n` (a commit failed after the append). Only the newest frames can be cut. When the cut fails,
    /// the log refuses every later write ([`RecordLogError::Stopped`]); the next start's `open` cuts the uncommitted frame.
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
            let f = OpenOptions::new().write(true).open(&segs[si].1)?;
            f.set_len(offset)?;
            f.sync_all()?;
            for (_, p) in segs.iter().skip(si + 1) {
                std::fs::remove_file(p)?;
            }
            self.file = OpenOptions::new().append(true).open(&segs[si].1)?;
            self.seg_path = segs[si].1.clone();
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
    /// What the decoded frames had to drop (reveals / holdings / imports of unknown layouts).
    pub dropped: DecodeStats,
}

/// Every frame of a log directory in order, chain verified. A frame that is authentic (its chain hash verifies) but that
/// this build cannot decode is skipped and reported in [`Replay::skipped`] instead of failing the whole read; a broken
/// chain is still an error.
pub fn read_all(dir: &Path) -> Result<Replay, RecordLogError> {
    let segs = list_segments(dir)?;
    let mut records = Vec::new();
    let mut skipped = Vec::new();
    let mut dropped = DecodeStats::default();
    let end = scan(&segs, |n, body| {
        match LogBatch::decode(body) {
            Ok(d) => {
                dropped.add(&d.stats);
                records.push((n, d.batch));
            }
            Err(e) => skipped.push((n, e.to_string())),
        }
        Ok(false)
    })?;
    Ok(Replay { records, torn: end.torn, frames: end.frames, skipped, dropped })
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

    /// A partial frame (a write that failed part way) with complete, chained frames after it is not a torn tail: `open` and
    /// `read_all` refuse the log and leave every byte of it in place, whether the partial frame's length runs past the end
    /// of the file or into the frames after it.
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
            // the partial frame: its length, then only part of its body
            bytes.extend_from_slice(&declared.to_le_bytes());
            bytes.extend_from_slice(&[7u8; 3000]);
            // two complete frames chained from the last complete one
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

    // ---- template evolution ----

    use crate::indexer::record::{RecIn, RecOut, Reveal};
    use kob_protocol::artifacts::template;

    /// The dispatch tag of today's `KobPair.settle` (the kind under the record-log code the cross limits had).
    fn new_settle() -> [u8; 4] {
        let t = template(TemplateId::KobPair);
        *t.contract().entries.get("settle").expect("KobPair.settle").dispatch_tag.as_bytes()
    }

    /// Today's `KobPair` state length.
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
        // format-2 frames under code 0x08 whose template hash this build does not pin (an older or a future artifact, today's
        // state length or another)
        let mut unknown = batch_of(1, vec![cross_tx(1, 400, [1, 2, 3, 4], vec![])]).encode(t);
        let mut older = batch_of(2, vec![cross_tx(2, new_len(), new_settle(), vec![])]).encode(t);
        // body: 0x00 0x02 | time (6-byte varint) | order table count | code | hash
        let at = 2 + 6 + 1;
        assert_eq!(unknown[at], 0x08, "the code 0x08");
        unknown[at + 1..at + 33].copy_from_slice(&[0xee; 32]);
        older[at + 1..at + 33].copy_from_slice(&[0xdd; 32]);
        log.append_body(&unknown).unwrap();
        log.append_body(&older).unwrap();
        // a newer frame format, and a format-1 frame (no marker)
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
        // a batch without reveals pays two bytes for the empty tables after the marker and the write time
        assert_eq!(batch(1).encode(5)[..5], [0, FORMAT, 5, 0, 0]);
    }
}
