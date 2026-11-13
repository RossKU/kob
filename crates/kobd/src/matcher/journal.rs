//! The in-flight journal: the transactions this executor submitted and still tracks, kept on disk.
//!
//! A restart (a crash, `kill -9`, a stop that cut a step) must not forget what the node's mempool
//! holds. Without the journal the tracker ([`super::submit::Tracker`]) lives in memory only. After
//! a restart the inputs of the previous run's mempool transactions look free, the next batch takes
//! the same funding coin and the node refuses batch after batch as a mempool double spend. A stuck
//! kill or refund can no longer be replaced at a higher fee.
//!
//! # Format
//!
//! The format is the smallest that survives a kill at any instant: one JSON object per line,
//! appended.
//!
//! - `{"op":"add","tx":{...},"job":{...}}` is written BEFORE a transaction is submitted
//!   (write-ahead). A kill between the write and the submit leaves an entry for a transaction the
//!   node may never have seen, which the restart settles like any other. It is written again when
//!   the entry gains a keeper job that may be replaced (`job`) or is retired. A later `add` of the
//!   same id replaces the earlier one.
//! - `{"op":"remove","txid":"..."}` is written once the tracker no longer follows the transaction
//!   (final, dropped, refused, replaced).
//!
//! Lines are written without `fsync`. A killed process loses nothing the operating system has
//! accepted. Only a power loss can, and then the node's mempool is usually gone too. A torn last
//! line is skipped.
//!
//! # Restart
//!
//! At the start the file is read and the entries still added are rewritten to a fresh file
//! (compaction, also done when the file grows past [`COMPACT_LINES`]). Each entry comes back to the
//! tracker as a submission without a verdict (`unconfirmed`). The first step resends it,
//! idempotently, and the node's answer settles it the way the tracker settles any doubtful submit:
//!
//! - "already in the mempool": tracked;
//! - already in the chain: done;
//! - refused: dropped, and its orders back off.
//!
//! A dry run and the kill switch send nothing, so neither resends. The entries simply stay
//! reserved.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::book::{CovId, Outpoint};
use super::run::InFlightJob;
use super::submit::Tracked;

/// The journal's file name in the executor's data directory.
pub const FILE_NAME: &str = "executor-inflight.jsonl";

/// Lines appended since the last compaction past which the file is rewritten with only the live
/// entries.
///
/// The file is compacted when the lines appended exceed this or four times the live entries,
/// whichever is larger.
pub const COMPACT_LINES: usize = 512;

fn hex(b: &[u8; 32]) -> String {
    kob_protocol::json::to_hex(b)
}

fn unhex(s: &str) -> Result<[u8; 32], String> {
    kob_protocol::json::hex32(s)
}

fn op_text(op: &Outpoint) -> (String, u32) {
    (hex(&op.0), op.1)
}

fn op_of((t, i): &(String, u32)) -> Result<Outpoint, String> {
    Ok((unhex(t)?, *i))
}

/// A tracked transaction as the journal writes it (hex ids; maps as lists: JSON keys are strings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Transaction id, hex.
    pub txid: String,
    /// Outpoints it spends: (transaction id hex, index).
    pub spends: Vec<(String, u32)>,
    /// Order covenant ids it spends, hex.
    pub orders: Vec<String>,
    /// `Tracked::owners` as a list.
    #[serde(default)]
    pub owners: Vec<((String, u32), String)>,
    /// `Tracked::addresses` as a list.
    #[serde(default)]
    pub addresses: Vec<((String, u32), String)>,
    /// `Tracked::parent`, hex.
    #[serde(default)]
    pub parent: Option<String>,
    /// DAA score at submission.
    pub submitted_daa: u64,
    /// `RpcTransaction` JSON, for the resend.
    pub rpc_tx: serde_json::Value,
    /// What the transaction is (`Tracked::kind`).
    pub kind: String,
    /// Operator profit promised by the build (sompi).
    pub profit: i64,
    /// Network fee paid (sompi).
    pub fee: u64,
    /// Fee rate (sompi per gram).
    pub fee_rate: u64,
    /// `Tracked::deadline`, `Tracked::resend_until` and `Tracked::retired`. A line that lacks them
    /// has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resend_until: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<u64>,
}

impl Entry {
    /// The entry that records `t`.
    pub fn of(t: &Tracked) -> Entry {
        Entry {
            txid: hex(&t.txid),
            spends: t.spends.iter().map(op_text).collect(),
            orders: t.orders.iter().map(hex).collect(),
            owners: t.owners.iter().map(|(op, id)| (op_text(op), hex(id))).collect(),
            addresses: t.addresses.iter().map(|(op, a)| (op_text(op), a.clone())).collect(),
            parent: t.parent.as_ref().map(hex),
            submitted_daa: t.submitted_daa,
            rpc_tx: t.rpc_tx.clone(),
            kind: t.kind.clone(),
            profit: t.profit,
            fee: t.fee,
            fee_rate: t.fee_rate,
            deadline: t.deadline,
            resend_until: t.resend_until,
            retired: t.retired,
        }
    }

    /// The tracked transaction, as a submission without a verdict: the restart resends it and the
    /// node settles it.
    pub fn tracked(&self) -> Result<Tracked, String> {
        Ok(Tracked {
            txid: unhex(&self.txid)?,
            spends: self.spends.iter().map(op_of).collect::<Result<BTreeSet<_>, _>>()?,
            orders: self.orders.iter().map(|o| unhex(o)).collect::<Result<BTreeSet<CovId>, _>>()?,
            owners: self.owners.iter().map(|(op, id)| Ok((op_of(op)?, unhex(id)?))).collect::<Result<BTreeMap<_, _>, String>>()?,
            addresses: self.addresses.iter().map(|(op, a)| Ok((op_of(op)?, a.clone()))).collect::<Result<BTreeMap<_, _>, String>>()?,
            parent: self.parent.as_deref().map(unhex).transpose()?,
            submitted_daa: self.submitted_daa,
            rpc_tx: self.rpc_tx.clone(),
            accepted: None,
            kind: self.kind.clone(),
            profit: self.profit,
            fee: self.fee,
            fee_rate: self.fee_rate,
            unconfirmed: true,
            deadline: self.deadline,
            resend_until: self.resend_until,
            retired: self.retired,
        })
    }
}

/// One line of the journal. One is written per submission, so the size of the variants does not
/// matter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Line {
    Add {
        tx: Entry,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job: Option<InFlightJob>,
    },
    Remove {
        txid: String,
    },
}

/// What the journal held at the start.
#[derive(Debug, Default)]
pub struct Restored {
    /// The transactions (as submissions without a verdict) and the keeper job of each that has one.
    pub txs: Vec<(Tracked, Option<InFlightJob>)>,
    /// Lines that could not be read (a torn last line after a kill; anything else is reported).
    pub skipped: usize,
}

/// The open journal ([`Journal::open`]).
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: std::fs::File,
    /// The ids the file holds as added, and whether that entry carries a keeper job and is retired
    /// (`Tracked::retired`).
    live: BTreeMap<[u8; 32], (bool, bool)>,
    appended: usize,
    /// The last write failed (logged once until a write succeeds).
    failing: bool,
}

/// The entries still added, by transaction id, with their keeper jobs.
type Live = BTreeMap<String, (Entry, Option<InFlightJob>)>;

fn read_lines(path: &Path) -> std::io::Result<(Live, usize)> {
    let text = match std::fs::read(path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((BTreeMap::new(), 0)),
        Err(e) => return Err(e),
    };
    let mut live = BTreeMap::new();
    let mut skipped = 0usize;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match serde_json::from_str::<Line>(line) {
            Ok(Line::Add { tx, job }) => {
                live.insert(tx.txid.clone(), (tx, job));
            }
            Ok(Line::Remove { txid }) => {
                live.remove(&txid);
            }
            Err(_) => skipped += 1,
        }
    }
    Ok((live, skipped))
}

/// What the journal at `path` holds, read only (nothing is written): the entries still added, as
/// submissions without a verdict.
///
/// An absent file holds nothing. A file that cannot be read is an error.
pub fn read(path: &Path) -> std::io::Result<Restored> {
    Ok(restore(read_lines(path)?).0)
}

/// The entries read, as the tracker takes them, and the compact file body that holds them.
fn restore((entries, skipped): (Live, usize)) -> (Restored, BTreeMap<[u8; 32], (bool, bool)>, String) {
    let mut restored = Restored { skipped, ..Restored::default() };
    let mut live = BTreeMap::new();
    let mut body = String::new();
    for (tx, job) in entries.into_values() {
        match tx.tracked() {
            Ok(t) => {
                let line = serde_json::to_string(&Line::Add { tx, job: job.clone() }).expect("an entry serialises");
                live.insert(t.txid, (job.is_some(), t.retired.is_some()));
                body.push_str(&line);
                body.push('\n');
                restored.txs.push((t, job));
            }
            Err(_) => restored.skipped += 1,
        }
    }
    (restored, live, body)
}

impl Journal {
    /// Opens (creates) the journal at `path`. Returns what it held, rewritten to a compact file
    /// that is then appended to.
    ///
    /// A rewrite that fails (a full disk, or on Windows a scanner holding the file) keeps the file
    /// as it is and appends to it. The next compaction tries again. Only a file that cannot be read
    /// or appended to is an error.
    pub fn open(path: &Path) -> std::io::Result<(Journal, Restored)> {
        let (restored, live, body) = restore(read_lines(path)?);
        let tmp = path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!(file = %path.display(), error = %e, "cannot compact the in-flight journal at the start: appending to it as it is");
        }
        let file = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
        Ok((Journal { path: path.to_path_buf(), file, live, appended: 0, failing: false }, restored))
    }

    /// The journal file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, line: &Line) {
        let r = serde_json::to_string(line)
            .map_err(std::io::Error::other)
            .and_then(|mut s| {
                s.push('\n');
                self.file.write_all(s.as_bytes())
            })
            .and_then(|_| self.file.flush());
        match r {
            Ok(()) => {
                self.appended += 1;
                if self.failing {
                    tracing::info!(file = %self.path.display(), "the in-flight journal is written again");
                }
                self.failing = false;
            }
            Err(e) => {
                if !self.failing {
                    tracing::error!(
                        file = %self.path.display(),
                        error = %e,
                        "cannot write the in-flight journal: a restart now may forget transactions the mempool holds (free space or fix the disk)"
                    );
                }
                self.failing = true;
            }
        }
    }

    /// Records `t` (and its keeper job) as in flight: called before it is submitted.
    pub fn add(&mut self, t: &Tracked, job: Option<&InFlightJob>) {
        self.write(&Line::Add { tx: Entry::of(t), job: job.cloned() });
        self.live.insert(t.txid, (job.is_some(), t.retired.is_some()));
    }

    /// Brings the file in line with what the tracker follows (`txs`) and the keeper jobs that may
    /// be replaced (`jobs`).
    ///
    /// It writes an entry for every tracked transaction (with its job) that is new or whose job or
    /// retirement changed, and a removal for every one the tracker no longer tracks. It compacts
    /// the file when the lines appended pass [`COMPACT_LINES`] (or four times the live entries, if
    /// that is more).
    pub fn sync(&mut self, txs: &BTreeMap<[u8; 32], Tracked>, jobs: &BTreeMap<[u8; 32], InFlightJob>) {
        let gone: Vec<[u8; 32]> = self.live.keys().filter(|k| !txs.contains_key(*k)).copied().collect();
        for id in gone {
            self.write(&Line::Remove { txid: hex(&id) });
            self.live.remove(&id);
        }
        for (id, t) in txs {
            let job = jobs.get(id);
            if self.live.get(id) != Some(&(job.is_some(), t.retired.is_some())) {
                self.add(t, job);
            }
        }
        if self.appended > COMPACT_LINES.max(4 * self.live.len()) {
            self.compact(txs, jobs);
        }
    }

    fn compact(&mut self, txs: &BTreeMap<[u8; 32], Tracked>, jobs: &BTreeMap<[u8; 32], InFlightJob>) {
        let mut body = String::new();
        for (id, t) in txs.iter().filter(|(id, _)| self.live.contains_key(*id)) {
            if let Ok(s) = serde_json::to_string(&Line::Add { tx: Entry::of(t), job: jobs.get(id).cloned() }) {
                body.push_str(&s);
                body.push('\n');
            }
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        let r = std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .and_then(|_| std::fs::OpenOptions::new().append(true).create(true).open(&self.path));
        match r {
            Ok(f) => {
                self.file = f;
                self.appended = 0;
            }
            Err(e) => {
                tracing::warn!(file = %self.path.display(), error = %e, "cannot compact the in-flight journal (it keeps growing)")
            }
        }
    }

    /// The ids the journal holds as in flight.
    pub fn live(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.live.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(n: u8) -> Tracked {
        Tracked {
            txid: [n; 32],
            spends: [([n; 32], 0), ([0xf0; 32], u32::from(n))].into_iter().collect(),
            orders: [[n; 32]].into_iter().collect(),
            owners: [(([n; 32], 0), [n; 32])].into_iter().collect(),
            addresses: [(([n; 32], 0), format!("kaspatest:q{n}"))].into_iter().collect(),
            parent: (n > 1).then_some([n - 1; 32]),
            submitted_daa: 1_000 + u64::from(n),
            rpc_tx: serde_json::json!({"n": n}),
            accepted: Some(("b".into(), 9)),
            kind: "match".into(),
            profit: i64::from(n),
            fee: 2_000,
            fee_rate: 100,
            unconfirmed: false,
            // A day order's fill: what bounds its resend survives too.
            deadline: (n == 3).then_some(1_790_000_000),
            resend_until: (n == 3).then_some(5_000),
            retired: None,
        }
    }

    /// Everything the tracker needs survives the file. What was added and not removed comes back as
    /// a submission without a verdict (unaccepted), in order of id. A torn last line (a kill
    /// mid-write) is skipped, and the restart compacts the file.
    #[test]
    fn what_is_in_flight_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let (mut j, r) = Journal::open(&p).unwrap();
        assert!(r.txs.is_empty() && r.skipped == 0);
        let (a, b, c) = (tracked(1), tracked(2), tracked(3));
        j.add(&a, None);
        j.add(&b, None);
        j.add(&c, None);
        let mut txs: BTreeMap<[u8; 32], Tracked> = [a.clone(), c.clone()].into_iter().map(|t| (t.txid, t)).collect();
        // b is refused and not tracked any more. The keeper jobs' round trip is
        // `tests/keeper_liveness.rs`.
        j.sync(&txs, &BTreeMap::new());
        assert_eq!(j.live().copied().collect::<Vec<_>>(), vec![[1; 32], [3; 32]]);
        // a is retired (not sent again): the journal says so, and a restart does not send it again
        // either.
        txs.get_mut(&[1; 32]).unwrap().retired = Some(4_000);
        j.sync(&txs, &BTreeMap::new());
        let a = txs[&[1; 32]].clone();
        drop(j);
        // a kill mid-write
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"op\":\"add\",\"tx\":{\"txid\":").unwrap();
        let (j, r) = Journal::open(&p).unwrap();
        assert_eq!(r.skipped, 1);
        let back: Vec<Tracked> = r.txs.iter().map(|(t, _)| t.clone()).collect();
        let want = |t: &Tracked| Tracked { accepted: None, unconfirmed: true, ..t.clone() };
        assert_eq!(back, vec![want(&a), want(&c)]);
        assert!(r.txs.iter().all(|(_, job)| job.is_none()));
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), 2, "compacted: one line per live entry");
        drop(j);
        // everything final: an empty journal
        let (mut j, _) = Journal::open(&p).unwrap();
        txs.clear();
        j.sync(&txs, &BTreeMap::new());
        drop(j);
        let (_, r) = Journal::open(&p).unwrap();
        assert!(r.txs.is_empty());
    }

    /// A journal that cannot be rewritten at the start (here its temporary file's name is taken by
    /// a directory, as a full disk would fail the write) is kept as it is and appended to. What it
    /// held is never lost and this run is journaled too. Read only, it gives the same entries. An
    /// absent file holds nothing.
    #[test]
    fn a_journal_that_cannot_be_compacted_is_appended_to() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let (mut j, _) = Journal::open(&p).unwrap();
        j.add(&tracked(1), None);
        drop(j);
        std::fs::create_dir(p.with_extension("jsonl.tmp")).unwrap();
        let (mut j, r) = Journal::open(&p).unwrap();
        assert_eq!(r.txs.len(), 1);
        j.add(&tracked(2), None);
        drop(j);
        assert_eq!(read(&p).unwrap().txs.iter().map(|(t, _)| t.txid).collect::<Vec<_>>(), vec![[1; 32], [2; 32]]);
        assert!(read(&dir.path().join("absent.jsonl")).unwrap().txs.is_empty());
    }

    /// A long run keeps the file small: it is rewritten with the live entries once enough lines
    /// were appended.
    #[test]
    fn the_file_is_compacted_as_it_grows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let (mut j, _) = Journal::open(&p).unwrap();
        let keep = tracked(1);
        let mut txs: BTreeMap<[u8; 32], Tracked> = [(keep.txid, keep.clone())].into_iter().collect();
        j.sync(&txs, &BTreeMap::new());
        for n in 0..(COMPACT_LINES as u32) {
            let mut t = tracked(2);
            t.txid[0] = (n % 250) as u8 + 2;
            t.txid[1] = (n / 250) as u8;
            txs.insert(t.txid, t.clone());
            j.sync(&txs, &BTreeMap::new());
            txs.remove(&t.txid);
            j.sync(&txs, &BTreeMap::new());
        }
        // 1024 lines were appended: at most COMPACT_LINES since the last compaction, plus the live
        // entry it wrote.
        let lines = std::fs::read_to_string(&p).unwrap().lines().count();
        assert!(lines <= COMPACT_LINES + 1, "{lines} lines");
        drop(j);
        let (_, r) = Journal::open(&p).unwrap();
        assert_eq!(r.txs.len(), 1);
        assert_eq!(r.txs[0].0.txid, keep.txid);
    }
}
