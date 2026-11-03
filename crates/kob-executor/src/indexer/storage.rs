//! `kob-executor index migrate`: brings an existing data directory to the compact storage layout of schema 6 and gives the space
//! back. A store written by an earlier build is read by this build as it is (every change below is one a reader tolerates), so
//! the migration can run at a time of the operator's choosing; it is idempotent and can be stopped and run again.
//!
//! The steps, each reported with its time:
//!
//! 1. **Indexes.** The fills-only indexes `order_events_fills` and `order_events_fill_ts` replace the ones over every event, and
//!    `token_holdings_owner` (which the owner's page index serves) and `token_holdings_token_program` (which the `holding_programs`
//!    table serves) are dropped (`schema.sql`).
//! 2. **State spans.** The state of every `order_utxos` and `token_holdings` row is rewritten in the packed form
//!    ([`super::packed`]), in batches of one short transaction each (`PACK_BATCH` rows). Each batch is read back through the SQL
//!    functions the queries use and compared with the span it replaced before it commits; a mismatch aborts the migration and
//!    leaves every row either as it was or packed and verified.
//! 3. **Compaction.** The pages freed (and the free space inside the pages the updates left) are returned by `VACUUM INTO` a new
//!    file, which replaces `index.sqlite3`; the old file stays beside it as `index.sqlite3.before-migrate`, the rollback. The
//!    new file needs room for the compacted store (about the packed size), and the old file stays until the operator deletes it.
//! 4. **Record log.** Every sealed segment is gzip-compressed ([`super::recordlog::compress_sealed`]); the newest is left whole.
//!
//! The data directory's writer lock ([`super::lock`]) is held throughout: no `run` or `index` can write it meanwhile.

use super::db::{self, DbError, DbResult};
use super::packed;
use super::recordlog;
use super::IndexerError;
use crate::config::IndexerConfig;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Rows packed per transaction.
const PACK_BATCH: usize = 20_000;
/// The suffix of the file the compacted store replaced.
pub const BEFORE_MIGRATE: &str = "before-migrate";

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Compact the store (`VACUUM INTO`, then swap the files). Needs free disk for the new file.
    pub compact: bool,
    /// Compress the sealed record-log segments.
    pub compress_log: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { compact: true, compress_log: true }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub indexes_swapped: Vec<String>,
    pub indexes_dropped: Vec<String>,
    pub utxos_packed: u64,
    pub utxos_total: u64,
    pub holdings_packed: u64,
    pub holdings_total: u64,
    pub db_bytes_before: u64,
    pub db_bytes_after: u64,
    pub log_bytes_before: u64,
    pub log_bytes_after: u64,
    pub log_segments_compressed: usize,
    /// Where the store was before the compaction (the rollback), when it was compacted.
    pub kept_old: Option<PathBuf>,
    pub steps: Vec<(&'static str, Duration)>,
}

fn file_bytes(p: &Path) -> u64 {
    ["", "-wal"].iter().map(|s| std::fs::metadata(format!("{}{s}", p.display())).map(|m| m.len()).unwrap_or(0)).sum()
}

/// The `CREATE INDEX IF NOT EXISTS <name> ...;` statement of `schema.sql`.
fn schema_statement(name: &str) -> Option<String> {
    let sql = include_str!("schema.sql");
    let at = sql.find(&format!("CREATE INDEX IF NOT EXISTS {name} "))?;
    let end = sql[at..].find(';')?;
    Some(sql[at..at + end + 1].to_string())
}

fn index_sql(conn: &Connection, name: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1", [name], |r| r.get(0)).map(Some).or_else(|e| {
        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
            Ok(None)
        } else {
            Err(e)
        }
    })
}

/// Step 1.
fn swap_indexes(conn: &Connection, rep: &mut Report) -> DbResult<()> {
    // the owner's page index serves what `token_holdings_owner` did; `holding_programs` (kept by triggers, counted at start-up) serves what
    // `token_holdings_token_program` did
    for name in ["token_holdings_owner", "token_holdings_token_program"] {
        if index_sql(conn, name)?.is_some() {
            conn.execute_batch(&format!("DROP INDEX {name}"))?;
            rep.indexes_dropped.push(name.to_string());
        }
    }
    for name in ["order_events_fills", "order_events_fill_ts"] {
        let wanted = schema_statement(name).ok_or_else(|| DbError::Invalid(format!("schema.sql has no index {name}")))?;
        // the current definition has the fills-only condition (a store built by this build) or lacks it (an earlier build's)
        if index_sql(conn, name)?.is_some_and(|have| have.contains("WHERE kind = 'fill'")) {
            continue;
        }
        conn.execute_batch(&format!("DROP INDEX IF EXISTS {name}"))?;
        conn.execute_batch(&wanted)?;
        rep.indexes_swapped.push(name.to_string());
    }
    Ok(())
}

fn checkpoint(conn: &Connection) {
    // best effort: a reader holding the log back only delays the truncation
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

/// One table's state column: `select` yields `(rowid, stored state, base)` for rows after a rowid (`?1`, at most `?2`), `set`
/// writes a state by rowid, `read_back` yields the state the queries see for a rowid.
struct Column {
    table: &'static str,
    select: &'static str,
    set: &'static str,
    read_back: &'static str,
}

const UTXO_STATE: Column = Column {
    table: "order_utxos",
    select: "SELECT u.rowid, u.state, o.genesis_state FROM order_utxos u LEFT JOIN orders o ON o.covenant_id = u.covenant_id \
             WHERE u.rowid > ?1 AND u.state IS NOT NULL ORDER BY u.rowid LIMIT ?2",
    set: "UPDATE order_utxos SET state = ?1 WHERE rowid = ?2",
    read_back: "SELECT kob_ustate(u.state, (SELECT o.genesis_state FROM orders o WHERE o.covenant_id = u.covenant_id)) \
                FROM order_utxos u WHERE u.rowid = ?1",
};

/// A holding's base is its columns': the select returns it as the third value, built from them.
const HOLDING_STATE: Column = Column {
    table: "token_holdings",
    select: "SELECT rowid, state, family, owner, owner_kind, amount FROM token_holdings WHERE rowid > ?1 AND state IS NOT NULL \
             ORDER BY rowid LIMIT ?2",
    set: "UPDATE token_holdings SET state = ?1 WHERE rowid = ?2",
    read_back: "SELECT kob_hstate(h.state, h.family, h.owner, h.owner_kind, h.amount) FROM token_holdings h WHERE h.rowid = ?1",
};

/// Step 2 for one table: returns (rows packed, rows seen).
fn pack_column(conn: &Connection, col: &Column, holdings: bool, mut progress: impl FnMut(u64)) -> DbResult<(u64, u64)> {
    let (mut packed_rows, mut seen, mut last) = (0u64, 0u64, 0i64);
    loop {
        let tx = conn.unchecked_transaction()?;
        // (rowid, stored span, its base)
        let rows: Vec<(i64, Vec<u8>, Option<Vec<u8>>)> = {
            let mut st = tx.prepare_cached(col.select)?;
            let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, Vec<u8>, Option<Vec<u8>>)> {
                let base = if holdings {
                    packed::holding_base(r.get(2)?, &r.get::<_, Vec<u8>>(3)?, r.get(4)?, r.get(5)?)
                } else {
                    r.get::<_, Option<Vec<u8>>>(2)?
                };
                Ok((r.get(0)?, r.get(1)?, base))
            };
            let rows = st.query_map(params![last, PACK_BATCH as i64], map)?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(&(next, _, _)) = rows.last() else { break };
        let mut changed: Vec<(i64, Vec<u8>)> = Vec::new();
        {
            let mut set = tx.prepare_cached(col.set)?;
            for (rowid, stored, base) in &rows {
                let span = packed::unpack(stored, base.as_deref())
                    .map_err(|e| DbError::Invalid(format!("{} row {rowid}: {e}", col.table)))?
                    .into_owned();
                let want = packed::pack(&span, base.as_deref());
                if want != *stored {
                    set.execute(params![&want, rowid])?;
                    changed.push((*rowid, span));
                }
            }
        }
        // what the queries now read for each rewritten row is the span it held
        {
            let mut read = tx.prepare_cached(col.read_back)?;
            for (rowid, span) in &changed {
                let got: Vec<u8> = read.query_row([rowid], |r| r.get(0))?;
                if got != *span {
                    return Err(DbError::Invalid(format!(
                        "{} row {rowid}: the packed state does not read back as the span it replaced; nothing from this batch was kept",
                        col.table
                    )));
                }
            }
        }
        tx.commit()?;
        packed_rows += changed.len() as u64;
        seen += rows.len() as u64;
        last = next;
        progress(seen);
        if seen % (PACK_BATCH as u64 * 25) == 0 {
            checkpoint(conn);
        }
    }
    checkpoint(conn);
    Ok((packed_rows, seen))
}

/// Step 3: the store, compacted into a new file that replaces the old one (kept beside it).
fn compact(cfg: &IndexerConfig, conn: Connection) -> DbResult<PathBuf> {
    let path = cfg.db_path();
    checkpoint(&conn);
    let mut new = path.as_os_str().to_owned();
    new.push(".compacting");
    let new = PathBuf::from(new);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    conn.execute("VACUUM INTO ?1", [new.to_string_lossy().as_ref()])?;
    drop(conn);
    // the copy opens as a store of this build (schema, counters, state) before it replaces anything
    {
        let check = db::open_writer_with(&new, &cfg.network, cfg.lean)?;
        let ok: String = check.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if ok != "ok" {
            return Err(DbError::Invalid(format!("the compacted copy fails its check ({ok}); the store was not replaced")));
        }
        checkpoint(&check);
    }
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    let mut old = path.as_os_str().to_owned();
    old.push(format!(".{BEFORE_MIGRATE}"));
    let old = PathBuf::from(old);
    if old.exists() {
        return Err(DbError::Invalid(format!("{} exists already: delete it (or move it away) first", old.display())));
    }
    // the old file's -wal and -shm belong to it; the new file starts clean
    std::fs::rename(&path, &old)?;
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    if let Err(e) = std::fs::rename(&new, &path) {
        // put the store back
        let _ = std::fs::rename(&old, &path);
        return Err(e.into());
    }
    Ok(old)
}

fn note(steps: &mut Vec<(&'static str, Duration)>, say: &mut impl FnMut(&str), name: &'static str, t: Instant, what: String) {
    steps.push((name, t.elapsed()));
    say(&format!("{name}: {what} ({:.1} s)", t.elapsed().as_secs_f64()));
}

/// Runs the migration of the data directory of `cfg` (takes its writer lock). `say` reports each step as it finishes.
pub fn migrate(cfg: &IndexerConfig, opts: Options, mut say: impl FnMut(&str)) -> Result<Report, IndexerError> {
    let _writer = super::lock::WriterLock::acquire(cfg)?;
    let path = cfg.db_path();
    if !path.exists() {
        return Err(IndexerError::Config(format!("{} does not exist: there is nothing to migrate", path.display())));
    }
    let mut rep = Report { db_bytes_before: file_bytes(&path), ..Default::default() };
    if cfg.records.enabled {
        rep.log_bytes_before = recordlog::log_bytes(&cfg.records_dir()).unwrap_or(0);
    }

    let conn = db::open_writer_with(&path, &cfg.network, cfg.lean)?;

    let t = Instant::now();
    swap_indexes(&conn, &mut rep)?;
    note(&mut rep.steps, &mut say, "indexes", t, format!("rebuilt {:?}, dropped {:?}", rep.indexes_swapped, rep.indexes_dropped));

    let t = Instant::now();
    let (p, n) = pack_column(&conn, &UTXO_STATE, false, |seen| {
        if seen % 500_000 == 0 {
            say(&format!("order_utxos: {seen} rows seen"));
        }
    })?;
    (rep.utxos_packed, rep.utxos_total) = (p, n);
    note(&mut rep.steps, &mut say, "order_utxos", t, format!("{p} of {n} states packed"));

    let t = Instant::now();
    let (p, n) = pack_column(&conn, &HOLDING_STATE, true, |seen| {
        if seen % 500_000 == 0 {
            say(&format!("token_holdings: {seen} rows seen"));
        }
    })?;
    (rep.holdings_packed, rep.holdings_total) = (p, n);
    note(&mut rep.steps, &mut say, "token_holdings", t, format!("{p} of {n} states packed"));

    if opts.compact {
        let t = Instant::now();
        let old = compact(cfg, conn)?;
        note(&mut rep.steps, &mut say, "compact", t, format!("the store was rewritten; the previous file is {}", old.display()));
        rep.kept_old = Some(old);
    } else {
        checkpoint(&conn);
        drop(conn);
    }
    rep.db_bytes_after = file_bytes(&path);

    if opts.compress_log && cfg.records.enabled {
        let t = Instant::now();
        let r = recordlog::compress_sealed(&cfg.records_dir())?;
        rep.log_segments_compressed = r.segments;
        rep.log_bytes_after = recordlog::log_bytes(&cfg.records_dir()).unwrap_or(0);
        note(&mut rep.steps, &mut say, "record log", t, format!("{} segment(s): {} -> {} bytes", r.segments, r.before, r.after));
    } else {
        rep.log_bytes_after = rep.log_bytes_before;
    }
    Ok(rep)
}
