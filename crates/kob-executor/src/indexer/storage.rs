//! `kob-executor index migrate`: brings an existing data directory to the compact storage layout and gives the space back. A store
//! written by an earlier build is read by this build as it is (every change below is one a reader tolerates), so the migration can
//! run at a time of the operator's choosing; it is idempotent and can be stopped and run again.
//!
//! The steps, each reported with its time:
//!
//! 1. **Indexes.** The fills-only indexes `order_events_fills` and `order_events_fill_ts` replace the ones over every event, and
//!    `token_holdings_owner` (which the owner's page index serves) and `token_holdings_token_program` (which the `holding_programs`
//!    table serves) are dropped (`schema.sql`).
//! 2. **Packed columns.** `order_utxos.state` and `token_holdings.state` (differences from a base), `orders.genesis_state` and
//!    `order_events.detail` (deflated against a frozen dictionary) are rewritten in the form [`super::packed`] stores, in batches of
//!    one short transaction each (`PACK_BATCH` rows). Each batch is read back through the SQL functions the queries use and its
//!    values hashed before and after: a value that does not come back the same aborts the migration and leaves every row either as
//!    it was or packed and verified.
//! 3. **Compaction.** The pages freed (and the free space inside the pages the updates left) are returned by `VACUUM INTO` a new
//!    file, which replaces `index.sqlite3`; the old file stays beside it as `index.sqlite3.before-migrate`, the rollback. The
//!    new file needs room for the compacted store (about the packed size), and the old file stays until the operator deletes it.
//! 4. **Record log.** Every sealed segment is gzip-compressed ([`super::recordlog::compress_sealed`]); the newest is left whole.
//!
//! The data directory's writer lock ([`super::lock`]) is held throughout: no `run` or `index` can write it meanwhile.

use super::db::{self, DbError, DbResult};
use super::packed::GENESIS_G;
use super::recordlog;
use super::IndexerError;
use crate::config::IndexerConfig;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Rows rewritten per transaction.
const PACK_BATCH: i64 = 20_000;
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

/// One packed column: rows rewritten out of rows seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnReport {
    pub column: &'static str,
    pub changed: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub indexes_swapped: Vec<String>,
    pub indexes_dropped: Vec<String>,
    pub columns: Vec<ColumnReport>,
    pub db_bytes_before: u64,
    pub db_bytes_after: u64,
    pub log_bytes_before: u64,
    pub log_bytes_after: u64,
    pub log_segments_compressed: usize,
    /// Where the store was before the compaction (the rollback), when it was compacted.
    pub kept_old: Option<PathBuf>,
    pub steps: Vec<(&'static str, Duration)>,
}

impl Report {
    /// Rows rewritten in `column` (`table.column`).
    pub fn changed(&self, column: &str) -> u64 {
        self.columns.iter().find(|c| c.column == column).map_or(0, |c| c.changed)
    }
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

/// A packed column: what a query reads (`decoded`) and what is stored (`packed`), as SQL over the row of `table`.
struct Column {
    /// `table.column`
    name: &'static str,
    table: &'static str,
    column: &'static str,
    decoded: String,
    packed: String,
}

fn columns() -> Vec<Column> {
    // an order UTXO's state against the genesis state of its order; both forms of the genesis state give the same base
    let genesis = format!("(SELECT {GENESIS_G} FROM orders g WHERE g.covenant_id = order_utxos.covenant_id)");
    let utxo = format!("kob_ustate(state, {genesis})");
    let holding = "kob_hstate(state, family, owner, owner_kind, amount)".to_string();
    let gstate = "kob_gstate(genesis_state, maker, token_cov_id, token_tpl_hash, quote_cov_id)".to_string();
    vec![
        Column {
            name: "order_utxos.state",
            table: "order_utxos",
            column: "state",
            packed: format!("kob_pack({utxo}, {genesis})"),
            decoded: utxo,
        },
        Column {
            name: "token_holdings.state",
            table: "token_holdings",
            column: "state",
            packed: format!("kob_hpack({holding}, family, owner, owner_kind, amount)"),
            decoded: holding,
        },
        Column {
            name: "orders.genesis_state",
            table: "orders",
            column: "genesis_state",
            packed: format!("kob_gpack({gstate}, contract, maker, token_cov_id, token_tpl_hash, quote_cov_id)"),
            decoded: gstate,
        },
        Column {
            name: "order_events.detail",
            table: "order_events",
            column: "detail",
            packed: "kob_dpack(kob_dtext(detail), kind)".to_string(),
            decoded: "kob_dtext(detail)".to_string(),
        },
    ]
}

/// A hash of what the queries read in the rows `(lo, hi]` of the column, in rowid order.
fn digest(conn: &Connection, col: &Column, lo: i64, hi: i64) -> DbResult<([u8; 32], u64)> {
    let mut st = conn.prepare_cached(&format!(
        "SELECT rowid, {} FROM {} WHERE rowid > ?1 AND rowid <= ?2 AND {} IS NOT NULL ORDER BY rowid",
        col.decoded, col.table, col.column
    ))?;
    let mut rows = st.query(params![lo, hi])?;
    let mut h = blake3::Hasher::new();
    let mut n = 0;
    while let Some(r) = rows.next()? {
        h.update(&r.get::<_, i64>(0)?.to_le_bytes());
        match r.get_ref(1)? {
            rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b) => {
                h.update(&(b.len() as u64).to_le_bytes());
                h.update(b);
            }
            _ => {
                h.update(&[0xff]);
            }
        }
        n += 1;
    }
    Ok((*h.finalize().as_bytes(), n))
}

/// Step 2 for one column: returns (rows rewritten, rows seen).
fn pack_column(conn: &Connection, col: &Column, mut progress: impl FnMut(u64)) -> DbResult<(u64, u64)> {
    // only the rows whose stored form is not the one this build writes
    let update = format!(
        "UPDATE {t} SET {c} = {packed} WHERE rowid > ?1 AND rowid <= ?2 AND {c} IS NOT NULL AND {c} IS NOT {packed}",
        t = col.table,
        c = col.column,
        packed = col.packed
    );
    let next_batch = format!("SELECT MAX(rowid) FROM (SELECT rowid FROM {} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2)", col.table);
    let (mut changed, mut seen, mut last) = (0u64, 0u64, 0i64);
    loop {
        let Some(hi) = conn.query_row(&next_batch, params![last, PACK_BATCH], |r| r.get::<_, Option<i64>>(0))? else { break };
        let tx = conn.unchecked_transaction()?;
        let (before, n) = digest(&tx, col, last, hi)?;
        let rewritten = tx.execute(&update, params![last, hi])?;
        if rewritten > 0 {
            let (after, _) = digest(&tx, col, last, hi)?;
            if after != before {
                // the transaction is dropped, not committed: this batch stays as it was
                return Err(DbError::Invalid(format!(
                    "{}: rows {last}..{hi} do not read back as they did before they were rewritten; nothing from this batch was kept",
                    col.name
                )));
            }
        }
        tx.commit()?;
        changed += rewritten as u64;
        seen += n;
        last = hi;
        progress(seen);
        if (seen / PACK_BATCH as u64).is_multiple_of(25) {
            checkpoint(conn);
        }
    }
    checkpoint(conn);
    Ok((changed, seen))
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

    for col in columns() {
        let t = Instant::now();
        let name = col.name;
        let (changed, total) = pack_column(&conn, &col, |seen| {
            if seen % 500_000 < PACK_BATCH as u64 {
                say(&format!("{name}: {seen} rows seen"));
            }
        })?;
        rep.columns.push(ColumnReport { column: name, changed, total });
        note(&mut rep.steps, &mut say, name, t, format!("{changed} of {total} rewritten"));
    }

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
