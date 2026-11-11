//! The in-flight journal: the transactions this executor submitted and still tracks, kept on disk so that a restart (a
//! crash, `kill -9`, a stop that cut a step) does not forget what the node's mempool holds (audit 2026-11-11 M-3).
//!
//! Without it the tracker ([`super::submit::Tracker`]) lives in memory only: after a restart the inputs of the previous run's
//! mempool transactions look free, the next batch takes the same funding coin and the node refuses batch after batch as a
//! mempool double spend, and a stuck kill or refund can no longer be replaced at a higher fee.
//!
//! The format is the smallest that survives a kill at any instant: one JSON object per line, appended,
//!
//! * `{"op":"add","tx":{...},"job":{...}}` BEFORE a transaction is submitted (write-ahead: a kill between the write and the
//!   submit leaves an entry for a transaction the node may never have seen, which the restart settles like any other), and
//!   again when it becomes a keeper job that may be replaced (`job`); a later `add` of the same id replaces the earlier one;
//! * `{"op":"remove","txid":"..."}` once the tracker no longer follows it (final, dropped, refused, replaced).
//!
//! Lines are written without `fsync`: a killed process loses nothing the operating system has accepted (only a power loss
//! can, and then the node's mempool is usually gone too). A torn last line is skipped. At the start the file is read, the
//! entries still added are rewritten to a fresh file (compaction, also done when it grows past [`COMPACT_LINES`]), and each
//! comes back to the tracker as a submission without a verdict (`unconfirmed`): the first step resends it, idempotently, and
//! the node's answer settles it the way the tracker settles any doubtful submit ("already in the mempool": tracked; already in
//! the chain: done; refused: dropped and its orders back off). A dry run and the kill switch send nothing, so neither
//! resends (the entries simply stay reserved).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::book::{CovId, Outpoint};
use super::run::InFlightJob;
use super::submit::Tracked;

/// The journal's file name in the executor's data directory.
pub const FILE_NAME: &str = "executor-inflight.jsonl";

/// Lines appended since the last compaction past which the file is rewritten with only the live entries.
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
    pub txid: String,
    pub spends: Vec<(String, u32)>,
    pub orders: Vec<String>,
    #[serde(default)]
    pub owners: Vec<((String, u32), String)>,
    #[serde(default)]
    pub addresses: Vec<((String, u32), String)>,
    #[serde(default)]
    pub parent: Option<String>,
    pub submitted_daa: u64,
    pub rpc_tx: serde_json::Value,
    pub kind: String,
    pub profit: i64,
    pub fee: u64,
    pub fee_rate: u64,
}

impl Entry {
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
        }
    }

    /// The tracked transaction, as a submission without a verdict (the restart resends it and the node settles it).
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
        })
    }
}

/// One line of the journal (one is written per submission: the size of the variants does not matter).
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
    /// The ids the file holds as added, and whether that entry carries a keeper job.
    live: BTreeMap<[u8; 32], bool>,
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

impl Journal {
    /// Opens (creates) the journal at `path`: returns what it held, rewritten to a compact file that is then appended to.
    pub fn open(path: &Path) -> std::io::Result<(Journal, Restored)> {
        let (entries, skipped) = read_lines(path)?;
        let mut restored = Restored { skipped, ..Restored::default() };
        let mut live = BTreeMap::new();
        let mut body = String::new();
        for (tx, job) in entries.into_values() {
            match tx.tracked() {
                Ok(t) => {
                    live.insert(t.txid, job.is_some());
                    body.push_str(&serde_json::to_string(&Line::Add { tx, job: job.clone() }).map_err(std::io::Error::other)?);
                    body.push('\n');
                    restored.txs.push((t, job));
                }
                Err(_) => restored.skipped += 1,
            }
        }
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        let file = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
        Ok((Journal { path: path.to_path_buf(), file, live, appended: 0, failing: false }, restored))
    }

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
        self.live.insert(t.txid, job.is_some());
    }

    /// Brings the file in line with what the tracker follows (`txs`) and the keeper jobs that may be replaced (`jobs`): an entry
    /// for every tracked transaction (with its job), a removal for every one it no longer tracks. Compacts the file when it
    /// grew past [`COMPACT_LINES`].
    pub fn sync(&mut self, txs: &BTreeMap<[u8; 32], Tracked>, jobs: &BTreeMap<[u8; 32], InFlightJob>) {
        let gone: Vec<[u8; 32]> = self.live.keys().filter(|k| !txs.contains_key(*k)).copied().collect();
        for id in gone {
            self.write(&Line::Remove { txid: hex(&id) });
            self.live.remove(&id);
        }
        for (id, t) in txs {
            let job = jobs.get(id);
            if self.live.get(id) != Some(&job.is_some()) {
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
        }
    }

    /// Everything the tracker needs survives the file: what was added and not removed comes back as a submission without a
    /// verdict (unaccepted), in order of id; a torn last line (a kill mid-write) is skipped; the restart compacts the file.
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
        // b refused (no longer tracked); the keeper jobs' round trip is `tests/keeper_liveness.rs`
        j.sync(&txs, &BTreeMap::new());
        assert_eq!(j.live().copied().collect::<Vec<_>>(), vec![[1; 32], [3; 32]]);
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

    /// A long run keeps the file small: it is rewritten with the live entries once enough lines were appended.
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
        // 1024 lines were appended: at most COMPACT_LINES since the last compaction, plus the live entry it wrote
        let lines = std::fs::read_to_string(&p).unwrap().lines().count();
        assert!(lines <= COMPACT_LINES + 1, "{lines} lines");
        drop(j);
        let (_, r) = Journal::open(&p).unwrap();
        assert_eq!(r.txs.len(), 1);
        assert_eq!(r.txs[0].0.txid, keep.txid);
    }
}
