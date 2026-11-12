//! `kobd db migrate`: brings a data directory written by an earlier build (schema 5 to 11) to the layout of schema 12 in ONE offline
//! run, and gives the space back. `kobd db vacuum` ([`vacuum`]): gives the free pages of a store of this build back, offline.
//!
//! The store is not altered in place. A new file is built beside it and replaces it only when it has been verified; the old file
//! stays as `index.sqlite3.before-migrate` (the rollback) until the operator deletes it. The steps, each reported with its time:
//!
//! 1. **Copy.** The tables are created from `schema.sql` (without its indexes and triggers), then filled from the old store in bulk
//!    (`INSERT ... SELECT`, no per-row Rust). The old store is read through views that give every layout the shape of schema 7 (32-byte
//!    ids in every row; a schema 8 store has them joined back): `orders` get their integer id and name their maker, templates, extension
//!    commitment, token and quote token by id, the other ids (tokens, holding owners) get their `ids` rows ([`super::ids`]; the values the
//!    orders name come first, the most used first, so that they take one byte), every row that named an order, a token or an owner by its
//!    32 bytes names it by the integer, the packed state columns are written in the form [`super::packed`] stores (a value an earlier build
//!    wrote whole is packed; a packed one is packed again from its decoded form), and the event ids, the sequence numbers and every other
//!    table are kept as they were. A fill event gets its fee (schema 10): `amount x tip / scale` at the tip the order had at the fill (the
//!    tip the next in-place amend of the order replaced, `detail.previous.tip` of its event, else the order's tip), as the processor
//!    writes it; its executor stays unknown (the record log of the past has no payer sections).
//! 2. **Indexes.** `schema.sql` is applied again: the indexes and triggers are built over the loaded rows (faster than maintaining
//!    them row by row), then the counters and the activity totals the triggers keep are counted once.
//! 3. **Verification.** Per table, the rows of the old store (states decoded, ids as their 32 bytes) and the rows of the new one
//!    (joined back) are hashed in the same order and compared with their counts: a row that does not come back identical, a row
//!    that has no order (an orphan the integer layout cannot hold), or a count that differs aborts the migration with the old
//!    store untouched. The new file must also pass `PRAGMA quick_check` and open as a store of this build.
//! 4. **Swap.** The new file replaces `index.sqlite3`.
//! 5. **Record log.** Every sealed segment is gzip-compressed ([`super::recordlog::compress_sealed`]); the newest is left whole.
//!
//! The data directory's writer lock ([`super::lock`]) is held throughout: no `run` or `index` can write it meanwhile. Free disk
//! needed: the new store (about 3/4 of the old one) plus the temporary files of the index builds, while the old store stays. A run
//! that stops half way leaves only `index.sqlite3.migrating`, deleted by the next run; the old store is not touched before the
//! swap. A store of schema 9 to 11 takes the short way ([`upgrade_in_place`]: copied as it is and given the new columns, tables and
//! triggers). A store that is of schema 12 already is left as it is (the record log step still runs).
//!
//! Schema 11 counts the executors' transactions the store already holds as of unknown income and network fee (`db::backfill_executor_txs`):
//! the store keeps no record of who owns their outputs. `kobd db replay` rebuilds their income from the record log (their network fees
//! stay unknown: the log of the past has no input values).

use super::db::{self, DbError, DbResult};
use super::recordlog;
use super::IndexerError;
use crate::config::IndexerConfig;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The suffix of the file the new store replaced.
pub const BEFORE_MIGRATE: &str = "before-migrate";
/// The suffix of the file the new store is built in.
const MIGRATING: &str = "migrating";

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Compress the sealed record-log segments.
    pub compress_log: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { compress_log: true }
    }
}

/// One table: rows of the old store, rows of the new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableReport {
    pub table: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The schema version the store was at (8: nothing was rebuilt).
    pub from_schema: i64,
    pub tables: Vec<TableReport>,
    /// Rows of `ids` (token covenant ids and holding owners).
    pub ids: u64,
    pub db_bytes_before: u64,
    pub db_bytes_after: u64,
    pub log_bytes_before: u64,
    pub log_bytes_after: u64,
    pub log_segments_compressed: usize,
    /// Where the previous store was kept, when the store was rebuilt.
    pub kept_old: Option<PathBuf>,
    pub steps: Vec<(&'static str, Duration)>,
}

impl Report {
    /// Rows the store holds in `table`.
    pub fn rows(&self, table: &str) -> u64 {
        self.tables.iter().find(|t| t.table == table).map_or(0, |t| t.rows)
    }
}

fn file_bytes(p: &Path) -> u64 {
    ["", "-wal"].iter().map(|s| std::fs::metadata(format!("{}{s}", p.display())).map(|m| m.len()).unwrap_or(0)).sum()
}

fn checkpoint(conn: &Connection) {
    // best effort: a reader holding the log back only delays the truncation
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{suffix}"));
    PathBuf::from(s)
}

fn bad(msg: String) -> DbError {
    DbError::Invalid(msg)
}

/// The columns of `schema.table`, in table order.
fn columns(conn: &Connection, schema: &str, table: &str) -> DbResult<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM pragma_table_info(?1, ?2) ORDER BY cid")?;
    let v = st.query_map(params![table, schema], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

fn table_exists(conn: &Connection, schema: &str, table: &str) -> DbResult<bool> {
    Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"), [table], |r| {
        r.get::<_, i64>(0)
    })? > 0)
}

fn count(conn: &Connection, schema: &str, table: &str) -> DbResult<u64> {
    Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.{table}"), [], |r| r.get::<_, i64>(0))? as u64)
}

/// A file path as the path of a `file:` URI (read-only attach).
fn uri_of(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:");
    if !p.starts_with('/') {
        out.push('/');
    }
    for ch in p.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            c => out.push(c),
        }
    }
    out.push_str("?mode=ro");
    out
}

// ------------------------------------------------------------------------------------------------ copy

/// How a column of the new layout is read from the old store: its expression, or `None` to take the column of that name as it is
/// (NULL when the old store has none: a column an earlier build did not have).
type Map<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A table whose rows are loaded from the source view `src_<table>` with some columns computed: every column of the new table (but `skip`)
/// from `map` or, where it returns `None`, from the source column of the same name with alias `alias`. Source columns that are neither in
/// the new table nor in `renamed` (the source names the map consumes) would be lost: an error. `ord` (the order of the source rows) is the
/// view's own.
#[allow(clippy::too_many_arguments)]
fn copy_mapped(
    conn: &Connection,
    table: &str,
    skip: &[&str],
    renamed: &[&str],
    alias: &str,
    from: &str,
    order: &str,
    map: Map<'_>,
) -> DbResult<u64> {
    if !source_has(conn, table)? {
        return Ok(0);
    }
    let new_cols = columns(conn, "main", table)?;
    let old_cols = columns(conn, "temp", &format!("src_{table}"))?;
    for c in &old_cols {
        if c != "ord" && !new_cols.contains(c) && !renamed.contains(&c.as_str()) {
            return Err(bad(format!("{table}: the old store has a column `{c}` that this build does not know: nothing was migrated")));
        }
    }
    let target: Vec<&String> = new_cols.iter().filter(|c| !skip.contains(&c.as_str())).collect();
    let select: Vec<String> = target
        .iter()
        .map(|c| map(c).unwrap_or_else(|| if old_cols.contains(c) { format!("{alias}.{c}") } else { "NULL".to_string() }))
        .collect();
    let sql = format!(
        "INSERT INTO main.{table} ({}) SELECT {} FROM {from} ORDER BY {order}",
        target.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", "),
        select.join(", ")
    );
    conn.execute(&sql, [])?;
    count(conn, "main", table)
}

/// The tables whose columns hold the ids of schema 9 (the source views exist for these).
const MAPPED: [&str; 7] = ["orders", "order_utxos", "order_events", "order_state", "order_amends", "token_utxos", "token_holdings"];

/// Whether the old store has the table (and so its source view).
fn source_has(conn: &Connection, table: &str) -> DbResult<bool> {
    table_exists(conn, "old", table)
}

/// The source views `temp.src_<table>`: every table of [`MAPPED`] in the shape of schema 7 (32-byte ids in the rows) plus a leading `ord`
/// (the order the rows are loaded in: the old rowid). A store of schema 5, 6 or 7 is that shape already; one of schema 8 has its orders,
/// tokens and owners named by integer, which are joined back (a row that names an order that is not there comes back with a NULL covenant
/// id and fails the verification).
fn create_source_views(conn: &Connection, from_schema: i64) -> DbResult<()> {
    for table in MAPPED {
        if !table_exists(conn, "old", table)? {
            continue;
        }
        let cols = columns(conn, "old", table)?;
        let sql = if from_schema >= 8 && table != "orders" {
            let mut joins = String::new();
            let mut list = vec!["x.rowid AS ord".to_string()];
            for c in &cols {
                list.push(match (table, c.as_str()) {
                    (_, "order_id") if table == "token_utxos" => {
                        joins.push_str(" LEFT JOIN old.orders ro ON ro.id = x.order_id");
                        "ro.covenant_id AS owner".to_string()
                    }
                    (_, "order_id") => {
                        joins.push_str(" LEFT JOIN old.orders ro ON ro.id = x.order_id");
                        "ro.covenant_id AS covenant_id".to_string()
                    }
                    (_, "token_id") => {
                        joins.push_str(" LEFT JOIN old.ids rt ON rt.id = x.token_id");
                        "rt.h AS token_cov_id".to_string()
                    }
                    (_, "owner_id") => {
                        joins.push_str(" LEFT JOIN old.ids rw ON rw.id = x.owner_id");
                        "rw.h AS owner".to_string()
                    }
                    (_, c) => format!("x.{c}"),
                });
            }
            format!("CREATE TEMP VIEW src_{table} AS SELECT {} FROM old.{table} x{joins}", list.join(", "))
        } else if from_schema >= 8 {
            // the orders of schema 8 keep their blobs: only the integer key is the view's `ord`
            let list: Vec<String> = cols.iter().filter(|c| *c != "id").map(|c| format!("x.{c}")).collect();
            format!("CREATE TEMP VIEW src_{table} AS SELECT x.id AS ord, {} FROM old.{table} x", list.join(", "))
        } else {
            // columns an earlier build did not have read as NULL
            let mut extra = String::new();
            for (t, c) in [("orders", "quote_cov_id"), ("order_amends", "min_fill")] {
                if table == t && !cols.iter().any(|x| x == c) {
                    extra.push_str(&format!(", NULL AS {c}"));
                }
            }
            format!("CREATE TEMP VIEW src_{table} AS SELECT rowid AS ord, *{extra} FROM old.{table}")
        };
        conn.execute_batch(&sql)?;
    }
    Ok(())
}

/// A table loaded as it is: the old columns by name (`ORDER BY` its natural order).
fn copy_plain(conn: &Connection, table: &str) -> DbResult<u64> {
    if !table_exists(conn, "old", table)? {
        return Ok(0);
    }
    let new_cols = columns(conn, "main", table)?;
    let old_cols = columns(conn, "old", table)?;
    for c in &old_cols {
        if !new_cols.contains(c) {
            return Err(bad(format!("{table}: the old store has a column `{c}` that this build does not know: nothing was migrated")));
        }
    }
    let list = old_cols.join(", ");
    let without_rowid: bool = conn.query_row(
        "SELECT sql LIKE '%WITHOUT ROWID%' FROM main.sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    // a WITHOUT ROWID table in the order of its primary key, any other in the order of its rows
    let order = if without_rowid {
        let mut st = conn.prepare("SELECT name FROM pragma_table_info(?1, 'main') WHERE pk > 0 ORDER BY pk")?;
        let keys = st.query_map([table], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        keys.join(", ")
    } else {
        "rowid".to_string()
    };
    conn.execute(&format!("INSERT INTO main.{table} ({list}) SELECT {list} FROM old.{table} ORDER BY {order}"), [])?;
    count(conn, "main", table)
}

/// The bulk load of step 1. Returns the rows per table.
fn load(conn: &Connection, say: &mut impl FnMut(&str)) -> DbResult<Vec<TableReport>> {
    let mut out: Vec<TableReport> = Vec::new();
    let mut done = |name: &str, rows: u64, t: Instant, say: &mut dyn FnMut(&str)| {
        say(&format!("  {name}: {rows} rows ({:.1} s)", t.elapsed().as_secs_f64()));
        out.push(TableReport { table: name.to_string(), rows });
    };
    // meta: every key but the version and the counts the backfills write afresh
    {
        let t = Instant::now();
        conn.execute(
            "INSERT INTO main.meta (k, v) SELECT k, v FROM old.meta WHERE k NOT IN ('schema_version', 'counters', 'holding_programs', 'activity', 'executor_txs', 'orders_all')",
            [],
        )?;
        done("meta", count(conn, "main", "meta")?, t, say);
    }
    {
        let t = Instant::now();
        let n = copy_plain(conn, "blocks")?;
        done("blocks", n, t, say);
    }

    // the ids the orders name come first, the most used first: they are a few dozen values that a row and an index entry then hold in one or two
    // bytes (the makers, tokens, programs, templates, the extension commitment and the quote tokens)
    let t = Instant::now();
    conn.execute_batch(
        "CREATE TEMP TABLE dict (h BLOB PRIMARY KEY, c INTEGER NOT NULL) WITHOUT ROWID; \
         INSERT INTO dict SELECT h, SUM(c) FROM (\
           SELECT template_hash AS h, COUNT(*) AS c FROM src_orders GROUP BY 1 \
           UNION ALL SELECT maker, COUNT(*) FROM src_orders WHERE maker IS NOT NULL GROUP BY 1 \
           UNION ALL SELECT token_cov_id, COUNT(*) FROM src_orders WHERE token_cov_id IS NOT NULL GROUP BY 1 \
           UNION ALL SELECT token_tpl_hash, COUNT(*) FROM src_orders WHERE token_tpl_hash IS NOT NULL GROUP BY 1 \
           UNION ALL SELECT ext_commit, COUNT(*) FROM src_orders WHERE ext_commit IS NOT NULL GROUP BY 1 \
           UNION ALL SELECT quote_cov_id, COUNT(*) FROM src_orders WHERE quote_cov_id IS NOT NULL GROUP BY 1) GROUP BY h; \
         INSERT INTO main.ids (h) SELECT h FROM dict ORDER BY c DESC, h; \
         DROP TABLE dict;",
    )?;
    // then the other tokens and the holding owners (an owner may be a key, a script hash or a covenant id)
    for sql in [
        "INSERT OR IGNORE INTO main.ids (h) SELECT token_cov_id FROM src_token_holdings GROUP BY token_cov_id",
        "INSERT OR IGNORE INTO main.ids (h) SELECT token_cov_id FROM src_token_utxos GROUP BY token_cov_id",
        "INSERT OR IGNORE INTO main.ids (h) SELECT token_cov_id FROM src_order_events WHERE token_cov_id IS NOT NULL GROUP BY token_cov_id",
        "INSERT OR IGNORE INTO main.ids (h) SELECT owner FROM src_token_holdings GROUP BY owner",
    ] {
        conn.execute(sql, [])?;
    }
    done("ids", count(conn, "main", "ids")?, t, say);

    // the orders: ids in the order of the old rows
    let t = Instant::now();
    let idq = |col: &str| format!("(SELECT id FROM main.ids WHERE h = {col})");
    let n = copy_mapped(
        conn,
        "orders",
        &["id"],
        &["template_hash", "maker", "token_cov_id", "token_tpl_hash", "ext_commit", "quote_cov_id"],
        "o",
        "src_orders o",
        "o.ord",
        &|c| {
            match c {
            "template_id" => Some(idq("o.template_hash")),
            "maker_id" => Some(idq("o.maker")),
            "token_id" => Some(idq("o.token_cov_id")),
            "token_tpl_id" => Some(idq("o.token_tpl_hash")),
            "ext_id" => Some(idq("o.ext_commit")),
            "quote_id" => Some(idq("o.quote_cov_id")),
            "genesis_state" => Some(
                "kob_gpack(kob_gstate(o.genesis_state, o.maker, o.token_cov_id, o.token_tpl_hash, o.quote_cov_id), o.contract, o.maker, o.token_cov_id, o.token_tpl_hash, o.quote_cov_id)"
                    .to_string(),
            ),
            _ => None,
        }
        },
    )?;
    done("orders", n, t, say);

    let genesis = "kob_gstate(so.genesis_state, so.maker, so.token_cov_id, so.token_tpl_hash, so.quote_cov_id)";
    let t = Instant::now();
    let n = copy_mapped(
        conn,
        "order_utxos",
        &[],
        &["covenant_id"],
        "u",
        "src_order_utxos u JOIN main.orders n ON n.covenant_id = u.covenant_id JOIN src_orders so ON so.covenant_id = u.covenant_id",
        "u.ord",
        &|c| match c {
            "order_id" => Some("n.id".to_string()),
            "state" => {
                Some(format!("CASE WHEN u.state IS NULL THEN NULL ELSE kob_pack(kob_ustate(u.state, {genesis}), {genesis}) END"))
            }
            _ => None,
        },
    )?;
    done("order_utxos", n, t, say);

    // the tip each in-place amend replaced (`detail.previous.tip`: an amend needs the order's verified state, so every one has it), by the
    // new order id and the event id: the tip a fill was paid at is the one the next amend after it replaced, or the order's own
    let t = Instant::now();
    conn.execute_batch(
        "CREATE TEMP TABLE amend_tips (order_id INTEGER NOT NULL, id INTEGER NOT NULL, tip INTEGER, PRIMARY KEY (order_id, id)) WITHOUT ROWID;          INSERT INTO temp.amend_tips (order_id, id, tip)            SELECT n.id, e.id, json_extract(kob_dtext(e.detail), '$.previous.tip') FROM src_order_events e JOIN main.orders n ON n.covenant_id = e.covenant_id            WHERE e.kind = 'amend' ORDER BY 1, 2;",
    )?;
    say(&format!("  the tips amends replaced: {} rows ({:.1} s)", count(conn, "temp", "amend_tips")?, t.elapsed().as_secs_f64()));
    let t = Instant::now();
    let n = copy_mapped(
        conn,
        "order_events",
        &[],
        &["covenant_id", "token_cov_id"],
        "e",
        "src_order_events e JOIN main.orders n ON n.covenant_id = e.covenant_id LEFT JOIN main.ids t ON t.h = e.token_cov_id",
        "e.id",
        &|c| {
            match c {
            "order_id" => Some("n.id".to_string()),
            "token_id" => Some("t.id".to_string()),
            "detail" => Some("CASE WHEN e.detail IS NULL THEN NULL ELSE kob_dpack(kob_dtext(e.detail), e.kind) END".to_string()),
            // an amend without its previous tip (none should be) makes the fee unknown, not the order's current tip
            "fee" => Some(
                "CASE WHEN e.kind = 'fill' THEN kob_fill_fee(e.amount, COALESCE((SELECT COALESCE(a.tip, -1) FROM temp.amend_tips a                  WHERE a.order_id = n.id AND a.id > e.id ORDER BY a.id LIMIT 1), n.tip), n.scale) END"
                    .to_string(),
            ),
            _ => None,
        }
        },
    )?;
    done("order_events", n, t, say);
    conn.execute_batch("DROP TABLE temp.amend_tips")?;

    let t = Instant::now();
    let n = copy_mapped(
        conn,
        "order_state",
        &[],
        &["covenant_id"],
        "s",
        "src_order_state s JOIN main.orders n ON n.covenant_id = s.covenant_id",
        "n.id",
        &|c| (c == "order_id").then(|| "n.id".to_string()),
    )?;
    done("order_state", n, t, say);

    let t = Instant::now();
    let n = copy_mapped(
        conn,
        "order_amends",
        &[],
        &["covenant_id"],
        "a",
        "src_order_amends a JOIN main.orders n ON n.covenant_id = a.covenant_id",
        "a.id",
        &|c| (c == "order_id").then(|| "n.id".to_string()),
    )?;
    done("order_amends", n, t, say);

    let t = Instant::now();
    let n = copy_mapped(
        conn,
        "token_utxos",
        &[],
        &["token_cov_id", "owner"],
        "u",
        "src_token_utxos u JOIN main.ids t ON t.h = u.token_cov_id JOIN main.orders n ON n.covenant_id = u.owner",
        "u.ord",
        &|c| match c {
            "token_id" => Some("t.id".to_string()),
            "order_id" => Some("n.id".to_string()),
            _ => None,
        },
    )?;
    done("token_utxos", n, t, say);

    let t = Instant::now();
    let n = copy_mapped(
        conn,
        "token_holdings",
        &[],
        &["token_cov_id", "owner"],
        "h",
        "src_token_holdings h JOIN main.ids t ON t.h = h.token_cov_id JOIN main.ids w ON w.h = h.owner",
        "h.ord",
        &|c| match c {
            "token_id" => Some("t.id".to_string()),
            "owner_id" => Some("w.id".to_string()),
            "state" => Some(
                "kob_hpack(kob_hstate(h.state, h.family, h.owner, h.owner_kind, h.amount), h.family, h.owner, h.owner_kind, h.amount)"
                    .to_string(),
            ),
            _ => None,
        },
    )?;
    done("token_holdings", n, t, say);

    for table in [
        "scripts",
        "token_events",
        "rejects",
        "candles",
        "candle_meta",
        "candle_dirty",
        "order_flags",
        "pair_fills",
        "kcc23_candidates",
        "kcc23_txs",
        "kcc23_declarations",
        "logo_blocklist",
        "logo_reports",
    ] {
        let t = Instant::now();
        let n = copy_plain(conn, table)?;
        done(table, n, t, say);
    }

    // the sequences: an id of an autoincrement table is never reused, so the next one is above every id the old store ever gave
    for table in ["blocks", "order_events", "order_amends", "rejects", "token_events", "pair_fills", "logo_reports"] {
        let old: Option<i64> = conn
            .query_row("SELECT seq FROM old.sqlite_sequence WHERE name = ?1", [table], |r| r.get(0))
            .map(Some)
            .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
        let cur: Option<i64> =
            conn.query_row(&format!("SELECT MAX({}) FROM main.{table}", if table == "blocks" { "seq" } else { "id" }), [], |r| {
                r.get(0)
            })?;
        let seq = old.unwrap_or(0).max(cur.unwrap_or(0));
        conn.execute("DELETE FROM main.sqlite_sequence WHERE name = ?1", [table])?;
        if seq > 0 {
            conn.execute("INSERT INTO main.sqlite_sequence (name, seq) VALUES (?1, ?2)", params![table, seq])?;
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ verification

/// A hash of the rows of `sql` (every column of every row) and their count.
fn digest(conn: &Connection, sql: &str) -> DbResult<([u8; 32], u64)> {
    use rusqlite::types::ValueRef;
    let mut st = conn.prepare(sql)?;
    let n = st.column_count();
    let mut rows = st.query([])?;
    let mut h = blake3::Hasher::new();
    let mut rows_n = 0u64;
    while let Some(r) = rows.next()? {
        for i in 0..n {
            match r.get_ref(i)? {
                ValueRef::Null => {
                    h.update(&[0]);
                }
                ValueRef::Integer(v) => {
                    h.update(&[1]);
                    h.update(&v.to_le_bytes());
                }
                ValueRef::Real(v) => {
                    h.update(&[2]);
                    h.update(&v.to_le_bytes());
                }
                ValueRef::Text(b) => {
                    h.update(&[3]);
                    h.update(&(b.len() as u64).to_le_bytes());
                    h.update(b);
                }
                ValueRef::Blob(b) => {
                    h.update(&[4]);
                    h.update(&(b.len() as u64).to_le_bytes());
                    h.update(b);
                }
            }
        }
        rows_n += 1;
    }
    Ok((*h.finalize().as_bytes(), rows_n))
}

/// Per table: (name, SQL on the old store (the source views, 32-byte ids), SQL on the new one (ids joined back)).
fn checks() -> Vec<(&'static str, String, String)> {
    let gs = |o: &str, ids: [&str; 4]| format!("kob_gstate({o}.genesis_state, {}, {}, {}, {})", ids[0], ids[1], ids[2], ids[3]);
    let old_g = gs("o", ["o.maker", "o.token_cov_id", "o.token_tpl_hash", "o.quote_cov_id"]);
    let new_g = gs("o", ["o_mk.h", "o_tk.h", "o_tp.h", "o_qt.h"]);
    let new_joins = format!(
        "{}{}",
        super::packed::ORDER_IDS_O,
        " LEFT JOIN ids o_tm ON o_tm.id = o.template_id LEFT JOIN ids o_ex ON o_ex.id = o.ext_id "
    );
    let orders_tail = |g: &str| {
        format!(
            "o.scale, o.min_fill, o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, o.reserve, o.initial_amount, o.deadline, {g}, o.genesis_txid, o.genesis_out, o.genesis_block, o.genesis_daa, o.parent, o.listed, o.unlisted_reason, o.origin"
        )
    };
    vec![
        (
            "orders",
            format!(
                "SELECT o.covenant_id, o.contract, o.template_hash, o.family, o.side, o.maker, o.token_cov_id, o.token_tpl_hash, o.ext_commit, {}, o.quote_cov_id FROM src_orders o ORDER BY o.covenant_id",
                orders_tail(&old_g)
            ),
            format!(
                "SELECT o.covenant_id, o.contract, o_tm.h, o.family, o.side, o_mk.h, o_tk.h, o_tp.h, o_ex.h, {}, o_qt.h FROM main.orders o{new_joins} ORDER BY o.covenant_id",
                orders_tail(&new_g)
            ),
        ),
        (
            "order_utxos",
            format!(
                "SELECT u.txid, u.idx, u.covenant_id, u.value, u.spk, kob_ustate(u.state, {old_g}), u.created_block, u.created_daa, u.spent_block, u.spent_txid, u.spent_entry, u.spent_amount \
                 FROM src_order_utxos u JOIN src_orders o ON o.covenant_id = u.covenant_id ORDER BY u.txid, u.idx"
            ),
            format!(
                "SELECT u.txid, u.idx, o.covenant_id, u.value, u.spk, kob_ustate(u.state, {new_g}), u.created_block, u.created_daa, u.spent_block, u.spent_txid, u.spent_entry, u.spent_amount \
                 FROM main.order_utxos u JOIN main.orders o ON o.id = u.order_id{} ORDER BY u.txid, u.idx",
                super::packed::ORDER_IDS_O
            ),
        ),
        (
            "order_events",
            "SELECT e.id, e.covenant_id, e.block_seq, e.daa, e.ts, e.txid, e.tx_pos, e.kind, e.token_cov_id, e.side, e.amount, e.price, e.payout, e.closes, kob_dtext(e.detail) \
             FROM src_order_events e ORDER BY e.id"
                .to_string(),
            "SELECT e.id, o.covenant_id, e.block_seq, e.daa, e.ts, e.txid, e.tx_pos, e.kind, t.h, e.side, e.amount, e.price, e.payout, e.closes, kob_dtext(e.detail) \
             FROM main.order_events e JOIN main.orders o ON o.id = e.order_id LEFT JOIN main.ids t ON t.id = e.token_id ORDER BY e.id"
                .to_string(),
        ),
        (
            "order_state",
            "SELECT s.covenant_id, s.status, s.filled_amount, s.remaining_amount, s.amount_exact, s.cur_txid, s.cur_idx, s.cur_value, s.state_known, s.last_block, s.last_daa \
             FROM src_order_state s ORDER BY s.covenant_id"
                .to_string(),
            "SELECT o.covenant_id, s.status, s.filled_amount, s.remaining_amount, s.amount_exact, s.cur_txid, s.cur_idx, s.cur_value, s.state_known, s.last_block, s.last_daa \
             FROM main.order_state s JOIN main.orders o ON o.id = s.order_id ORDER BY o.covenant_id"
                .to_string(),
        ),
        (
            "order_amends",
            "SELECT a.id, a.covenant_id, a.block_seq, a.price, a.tip, a.tif, a.expiry_daa, a.active_from, a.deadline, a.listed, a.unlisted_reason, a.budget_rate, a.reserve, a.min_fill \
             FROM src_order_amends a ORDER BY a.id"
                .to_string(),
            "SELECT a.id, o.covenant_id, a.block_seq, a.price, a.tip, a.tif, a.expiry_daa, a.active_from, a.deadline, a.listed, a.unlisted_reason, a.budget_rate, a.reserve, a.min_fill \
             FROM main.order_amends a JOIN main.orders o ON o.id = a.order_id ORDER BY a.id"
                .to_string(),
        ),
        (
            "token_utxos",
            "SELECT u.txid, u.idx, u.token_cov_id, u.owner, u.amount, u.value, u.role, u.created_block, u.created_daa, u.spent_block, u.spent_txid FROM src_token_utxos u ORDER BY u.txid, u.idx"
                .to_string(),
            "SELECT u.txid, u.idx, t.h, o.covenant_id, u.amount, u.value, u.role, u.created_block, u.created_daa, u.spent_block, u.spent_txid \
             FROM main.token_utxos u JOIN main.ids t ON t.id = u.token_id JOIN main.orders o ON o.id = u.order_id ORDER BY u.txid, u.idx"
                .to_string(),
        ),
        (
            "token_holdings",
            "SELECT h.txid, h.idx, h.token_cov_id, h.program, h.family, h.owner, h.owner_kind, h.amount, h.value, kob_hstate(h.state, h.family, h.owner, h.owner_kind, h.amount), h.role, h.created_block, h.created_daa, h.spent_block, h.spent_txid \
             FROM src_token_holdings h ORDER BY h.txid, h.idx"
                .to_string(),
            "SELECT h.txid, h.idx, t.h, h.program, h.family, w.h, h.owner_kind, h.amount, h.value, kob_hstate(h.state, h.family, w.h, h.owner_kind, h.amount), h.role, h.created_block, h.created_daa, h.spent_block, h.spent_txid \
             FROM main.token_holdings h JOIN main.ids t ON t.id = h.token_id JOIN main.ids w ON w.id = h.owner_id ORDER BY h.txid, h.idx"
                .to_string(),
        ),
    ]
}

/// The activity totals just counted against the rows: the minutes and the hours hold every fill and `create` event once, and the fees
/// the events carry. A difference aborts the migration (the old store is untouched).
fn check_activity(conn: &Connection) -> DbResult<()> {
    let sums = |iv: i64| -> DbResult<(i64, i64, i64)> {
        Ok(conn.query_row(
            "SELECT COALESCE(SUM(fills), 0), COALESCE(SUM(fees), 0), COALESCE(SUM(orders), 0) FROM main.activity WHERE iv = ?1",
            [iv],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    };
    let events: (i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(kind = 'fill'), 0), COALESCE(SUM(CASE WHEN kind = 'fill' THEN COALESCE(fee, 0) ELSE 0 END), 0), COALESCE(SUM(kind = 'create'), 0)          FROM main.order_events WHERE kind IN ('fill', 'create')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let (m, h) = (sums(super::activity::MINUTE)?, sums(super::activity::HOUR)?);
    if m != events || h != events {
        return Err(bad(format!(
            "activity: the buckets ({m:?} by minute, {h:?} by hour) do not add up to the events ({events:?}). Nothing was migrated (the old store is untouched)"
        )));
    }
    // (schema 11) the executors' transactions, their income and network fees
    let txs = |iv: i64| -> DbResult<(i64, i64, i64)> {
        Ok(conn.query_row(
            "SELECT COALESCE(SUM(txs), 0), COALESCE(SUM(income_n), 0), COALESCE(SUM(miner_fee_n), 0) FROM main.activity WHERE iv = ?1",
            [iv],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    };
    let rows: (i64, i64, i64) =
        conn.query_row("SELECT COUNT(*), COUNT(income), COUNT(miner_fee) FROM main.executor_txs", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let (m, h) = (txs(super::activity::MINUTE)?, txs(super::activity::HOUR)?);
    if m != rows || h != rows {
        return Err(bad(format!(
            "activity: the executors' transactions in the buckets ({m:?} by minute, {h:?} by hour) are not the rows ({rows:?}). Nothing was migrated (the old store is untouched)"
        )));
    }
    Ok(())
}

/// Step 3 (part): old and new rows, table by table, compared. Tables copied as they were are compared by `SELECT *`.
fn verify(conn: &Connection, say: &mut impl FnMut(&str)) -> DbResult<()> {
    for (table, old_sql, new_sql) in checks() {
        let t = Instant::now();
        if !source_has(conn, table)? {
            continue;
        }
        let raw = count(conn, "temp", &format!("src_{table}"))?;
        let (a, na) = digest(conn, &old_sql)?;
        let (b, nb) = digest(conn, &new_sql)?;
        if na != raw {
            return Err(bad(format!(
                "{table}: {} of {raw} rows of the old store refer to an order or token that is not there; the integer layout cannot hold them. Nothing was migrated (the old store is untouched)",
                raw - na
            )));
        }
        if nb != raw || a != b {
            return Err(bad(format!(
                "{table}: the new store does not read back as the old one ({nb} rows against {raw}{}). Nothing was migrated (the old store is untouched)",
                if a != b { ", contents differ" } else { "" }
            )));
        }
        say(&format!("  {table}: {raw} rows identical ({:.1} s)", t.elapsed().as_secs_f64()));
    }
    for table in [
        "blocks",
        "scripts",
        "token_events",
        "rejects",
        "candles",
        "candle_meta",
        "candle_dirty",
        "order_flags",
        "pair_fills",
        "kcc23_candidates",
        "kcc23_txs",
        "kcc23_declarations",
        "logo_blocklist",
        "logo_reports",
    ] {
        if !table_exists(conn, "old", table)? {
            continue;
        }
        let t = Instant::now();
        let cols = columns(conn, "old", table)?.join(", ");
        let order = {
            let mut st = conn.prepare("SELECT name FROM pragma_table_info(?1, 'main') WHERE pk > 0 ORDER BY pk")?;
            let keys = st.query_map([table], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            keys.join(", ")
        };
        let (a, na) = digest(conn, &format!("SELECT {cols} FROM old.{table} ORDER BY {order}"))?;
        let (b, nb) = digest(conn, &format!("SELECT {cols} FROM main.{table} ORDER BY {order}"))?;
        if na != nb || a != b {
            return Err(bad(format!(
                "{table}: the new store does not read back as the old one ({nb} rows against {na}). Nothing was migrated (the old store is untouched)"
            )));
        }
        say(&format!("  {table}: {na} rows identical ({:.1} s)", t.elapsed().as_secs_f64()));
    }
    let (a, _) = digest(
        conn,
        "SELECT k, v FROM old.meta WHERE k NOT IN ('schema_version', 'counters', 'holding_programs', 'activity', 'executor_txs', 'orders_all') ORDER BY k",
    )?;
    let (b, _) = digest(
        conn,
        "SELECT k, v FROM main.meta WHERE k NOT IN ('schema_version', 'counters', 'holding_programs', 'activity', 'executor_txs', 'orders_all') ORDER BY k",
    )?;
    if a != b {
        return Err(bad(
            "meta: the new store does not read back as the old one. Nothing was migrated (the old store is untouched)".into()
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------ build and swap

/// Steps 1 to 3: the new store in `new`, loaded, indexed and verified against `old`.
fn build(cfg: &IndexerConfig, old: &Path, new: &Path, from_schema: i64, rep: &mut Report, say: &mut impl FnMut(&str)) -> DbResult<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    let conn = Connection::open(new)?;
    // a crash leaves a file nobody reads: no journal, no sync
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", -262_144)?;
    // the sorts of the index builds go to disk (the system temporary directory, or SQLITE_TMPDIR)
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    conn.pragma_update(None, "recursive_triggers", "OFF")?;
    super::packed::register(&conn)?;
    super::activity::register(&conn)?;
    conn.execute("ATTACH DATABASE ?1 AS old", [uri_of(old)])?;
    let page_size: i64 = conn.query_row("PRAGMA old.page_size", [], |r| r.get(0))?;
    conn.pragma_update(None, "page_size", page_size)?;
    if cfg.lean {
        conn.pragma_update(None, "auto_vacuum", 2)?;
    }

    // the tables from the schema, then without the indexes and triggers: they are built over the loaded rows
    conn.execute_batch(include_str!("schema.sql"))?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    let drops: Vec<String> = {
        let mut st = conn.prepare(
            "SELECT 'DROP ' || type || ' ' || name FROM main.sqlite_master WHERE (type = 'trigger') OR (type = 'index' AND sql IS NOT NULL)",
        )?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        v
    };
    for d in drops {
        conn.execute_batch(&d)?;
    }

    // a table of the old store that this build does not have would be lost: refuse before anything is copied (receipts: retired in schema 3)
    {
        let mut st = conn.prepare(
            "SELECT name FROM old.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'receipts' \
             AND name NOT IN (SELECT name FROM main.sqlite_master WHERE type = 'table') ORDER BY name",
        )?;
        let unknown = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        if !unknown.is_empty() {
            return Err(bad(format!(
                "the old store has tables this build does not know ({}): nothing was migrated (the old store is untouched)",
                unknown.join(", ")
            )));
        }
    }

    create_source_views(&conn, from_schema)?;

    let t = Instant::now();
    say("copy:");
    conn.execute_batch("BEGIN")?;
    let tables = load(&conn, say)?;
    conn.execute_batch("COMMIT")?;
    rep.ids = tables.iter().find(|t| t.table == "ids").map_or(0, |t| t.rows);
    rep.tables = tables;
    rep.steps.push(("copy", t.elapsed()));

    let t = Instant::now();
    conn.execute_batch(include_str!("schema.sql"))?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    // the counts the triggers keep, counted once (as a store without them gets them at start-up)
    db::backfill_counters(&conn)?;
    db::backfill_holding_programs(&conn)?;
    db::backfill_activity(&conn)?;
    db::backfill_executor_txs(&conn)?;
    db::backfill_all_time(&conn)?;
    check_activity(&conn)?;
    // the counters the old store kept against the ones counted from the rows
    if table_exists(&conn, "old", "counters")? {
        let (a, _) = digest(&conn, "SELECT name, n FROM old.counters WHERE n != 0 ORDER BY name")?;
        let (b, _) = digest(&conn, "SELECT name, n FROM main.counters WHERE n != 0 ORDER BY name")?;
        say(if a == b {
            "  counters: the same as the old store kept"
        } else {
            "  counters: counted afresh (the old store's differ: its triggers had drifted, or it predates them)"
        });
    }
    conn.execute("INSERT OR REPLACE INTO main.meta (k, v) VALUES ('schema_version', ?1)", [db::SCHEMA_VERSION.to_string()])?;
    rep.steps.push(("indexes", t.elapsed()));
    say(&format!("indexes: built ({:.1} s)", t.elapsed().as_secs_f64()));

    let t = Instant::now();
    say("verify:");
    verify(&conn, say)?;
    rep.steps.push(("verify", t.elapsed()));
    conn.execute_batch("DETACH DATABASE old")?;
    drop(conn);
    opens_and_checks(cfg, new, say)
}

/// The file opens as a store of this build and passes `PRAGMA quick_check` before it replaces anything.
fn opens_and_checks(cfg: &IndexerConfig, new: &Path, say: &mut impl FnMut(&str)) -> DbResult<()> {
    let t = Instant::now();
    let check = db::open_writer_with(new, &cfg.network, cfg.lean)?;
    let ok: String = check.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if ok != "ok" {
        return Err(bad(format!("the new store fails its check ({ok}); the store was not replaced")));
    }
    checkpoint(&check);
    drop(check);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    say(&format!("check: the new store opens and passes quick_check ({:.1} s)", t.elapsed().as_secs_f64()));
    Ok(())
}

/// Schema 9 to 12 to 13 (steps 1 to 3 for a store whose tables keep their layout): the store is copied as it is (`VACUUM INTO`, which
/// also gives the free pages back), its triggers are dropped (`schema.sql` creates this build's: schema 12 changed the one that keeps the
/// programs of the holdings), and
/// * from 9, the fill events get their two columns and their fees (`amount x tip / scale` at the tip the order had at the fill, as in the
///   copy of an older store);
/// * from 10, the activity buckets get the columns of the executors' transactions and the actors their `kept` flag (schema 11);
/// * from 9 to 11, the orders get the KAS of their genesis output (`orders.genesis_value`, schema 12), read from the genesis UTXO rows;
/// * (schema 13) `recorded_outputs` is created empty by `schema.sql` (the outputs of the past are in the record log: a replay fills it);
///
/// then `schema.sql` adds the new tables, indexes and triggers, the activity totals are counted once (from 9), the executors' transactions
/// the store holds are counted as of unknown income and network fee (`db::backfill_executor_txs`: the store has no record of their
/// outputs' owners; `kobd db replay` rebuilds them from the record log), the all-time order counter is set, the totals are checked against
/// the rows, and every table of the old store holds as many rows in the new one (the keys of `meta` and `counters` all there; the
/// activity buckets, which may gain the minutes of executors' transactions without a fill, are checked against the rows instead). No row
/// is rewritten but the fills (from 9) and the orders' new column; nothing of the old store is touched.
fn upgrade_in_place(
    cfg: &IndexerConfig,
    from: i64,
    old: &Path,
    new: &Path,
    rep: &mut Report,
    say: &mut impl FnMut(&str),
) -> DbResult<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    let t = Instant::now();
    {
        let src = Connection::open_with_flags(old, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI)?;
        src.busy_timeout(Duration::from_secs(30))?;
        src.execute("VACUUM INTO ?1", [new.to_string_lossy().as_ref()])?;
    }
    note(&mut rep.steps, say, "copy", t, format!("{} bytes", file_bytes(new)));

    let conn = Connection::open(new)?;
    // a crash leaves a file nobody reads: no journal, no sync
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", -262_144)?;
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    conn.pragma_update(None, "recursive_triggers", "OFF")?;
    super::packed::register(&conn)?;
    super::activity::register(&conn)?;
    conn.execute("ATTACH DATABASE ?1 AS old", [uri_of(old)])?;

    // (no transaction: without a journal the file is thrown away on any error)
    let triggers: Vec<String> = {
        let mut st = conn.prepare("SELECT name FROM main.sqlite_master WHERE type = 'trigger' ORDER BY name")?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        v
    };
    for name in &triggers {
        conn.execute_batch(&format!("DROP TRIGGER main.\"{name}\""))?;
    }
    if from == 9 {
        let t = Instant::now();
        conn.execute_batch(
            "ALTER TABLE main.order_events ADD COLUMN fee INTEGER; \
             ALTER TABLE main.order_events ADD COLUMN executor_id INTEGER; \
             CREATE TEMP TABLE amend_tips (order_id INTEGER NOT NULL, id INTEGER NOT NULL, tip INTEGER, PRIMARY KEY (order_id, id)) WITHOUT ROWID; \
             INSERT INTO temp.amend_tips (order_id, id, tip) \
               SELECT order_id, id, json_extract(kob_dtext(detail), '$.previous.tip') FROM main.order_events WHERE kind = 'amend' ORDER BY 1, 2;",
        )?;
        let amends = count(&conn, "temp", "amend_tips")?;
        // an amend without its previous tip (none should be) makes the fee unknown, not the order's current tip
        let fills = conn.execute(
            "UPDATE main.order_events SET fee = (SELECT kob_fill_fee(order_events.amount, COALESCE((SELECT COALESCE(a.tip, -1) FROM temp.amend_tips a \
               WHERE a.order_id = order_events.order_id AND a.id > order_events.id ORDER BY a.id LIMIT 1), o.tip), o.scale) \
               FROM main.orders o WHERE o.id = order_events.order_id) \
             WHERE kind = 'fill'",
            [],
        )?;
        conn.execute_batch("DROP TABLE temp.amend_tips")?;
        note(&mut rep.steps, say, "fees", t, format!("{fills} fills, {amends} amends"));
    } else if from == 10 {
        conn.execute_batch(
            "ALTER TABLE main.activity ADD COLUMN txs INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.activity ADD COLUMN income INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.activity ADD COLUMN income_n INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.activity ADD COLUMN miner_fees INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.activity ADD COLUMN miner_fee_n INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.activity ADD COLUMN net INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.activity ADD COLUMN net_n INTEGER NOT NULL DEFAULT 0; \
             ALTER TABLE main.actors ADD COLUMN kept INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    // (schema 12) the KAS of each order's genesis output
    if !columns(&conn, "main", "orders")?.iter().any(|c| c == "genesis_value") {
        conn.execute_batch("ALTER TABLE main.orders ADD COLUMN genesis_value INTEGER")?;
    }

    let t = Instant::now();
    conn.execute_batch(include_str!("schema.sql"))?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    if from == 9 {
        db::backfill_activity(&conn)?;
    }
    let unknown = db::backfill_executor_txs(&conn)?;
    db::backfill_all_time(&conn)?;
    let genesis_values = db::backfill_genesis_values(&conn)?;
    say(&format!("  orders: the KAS of {genesis_values} genesis outputs kept on the order (schema 12)"));
    check_activity(&conn)?;
    conn.execute("INSERT OR REPLACE INTO main.meta (k, v) VALUES ('schema_version', ?1)", [db::SCHEMA_VERSION.to_string()])?;
    note(
        &mut rep.steps,
        say,
        "activity",
        t,
        format!("the new tables and the totals; {unknown} executors' transactions of unknown income and network fee"),
    );

    let t = Instant::now();
    let tables: Vec<String> = {
        let mut st =
            conn.prepare("SELECT name FROM old.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        v
    };
    for table in tables {
        let table = table.as_str();
        let a = count(&conn, "old", table)?;
        let b = match table {
            // the version is the one row of meta that changes, the backfills add keys and the all-time counter: every old key is there
            "meta" => {
                conn.query_row("SELECT COUNT(*) FROM old.meta o WHERE EXISTS (SELECT 1 FROM main.meta n WHERE n.k = o.k)", [], |r| {
                    r.get::<_, i64>(0)
                })? as u64
            }
            "counters" => conn.query_row(
                "SELECT COUNT(*) FROM old.counters o WHERE EXISTS (SELECT 1 FROM main.counters n WHERE n.name = o.name)",
                [],
                |r| r.get::<_, i64>(0),
            )? as u64,
            // checked against the rows above (`check_activity`)
            "activity" => a,
            _ => count(&conn, "main", table)?,
        };
        if a != b {
            return Err(bad(format!(
                "{table}: {b} rows in the new store against {a}. Nothing was migrated (the old store is untouched)"
            )));
        }
        rep.tables.push(TableReport { table: table.to_string(), rows: a });
    }
    rep.ids = rep.rows("ids");
    note(&mut rep.steps, say, "verify", t, "every table holds the rows of the old store".to_string());
    conn.execute_batch("DETACH DATABASE old")?;
    drop(conn);
    opens_and_checks(cfg, new, say)
}

/// Step 4: `new` takes the place of `path`; the old file is kept beside it.
fn swap(path: &Path, new: &Path) -> DbResult<PathBuf> {
    swap_as(path, new, BEFORE_MIGRATE)
}

/// `new` takes the place of `path`; the old file (with its `-wal` and `-shm`) is kept beside it with `suffix`.
fn swap_as(path: &Path, new: &Path, suffix: &str) -> DbResult<PathBuf> {
    let old = suffixed(path, suffix);
    if old.exists() {
        return Err(bad(format!("{} exists already: delete it (or move it away) first", old.display())));
    }
    // the old file's -wal and -shm belong to it; the new file starts clean
    std::fs::rename(path, &old)?;
    for suffix in ["-wal", "-shm"] {
        let from = format!("{}{suffix}", path.display());
        if Path::new(&from).exists() {
            std::fs::rename(&from, format!("{}{suffix}", old.display()))?;
        }
    }
    if let Err(e) = std::fs::rename(new, path) {
        // put the store back
        let _ = std::fs::rename(&old, path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::rename(format!("{}{suffix}", old.display()), format!("{}{suffix}", path.display()));
        }
        return Err(e.into());
    }
    Ok(old)
}

/// The schema version a store file says it is (0: none).
fn stored_version(path: &Path) -> DbResult<i64> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    db::refuse_pre_v3(&conn, path)?;
    if !table_exists(&conn, "main", "meta")? {
        return Ok(0);
    }
    Ok(db::meta_get(&conn, "schema_version")?.and_then(|v| v.parse().ok()).unwrap_or(0))
}

fn note(steps: &mut Vec<(&'static str, Duration)>, say: &mut impl FnMut(&str), name: &'static str, t: Instant, what: String) {
    steps.push((name, t.elapsed()));
    say(&format!("{name}: {what} ({:.1} s)", t.elapsed().as_secs_f64()));
}

/// The lowest schema `kobd db migrate` brings forward (an older store is rebuilt with `kobd db replay`).
pub const MIGRATE_FROM_SCHEMA: i64 = 5;

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

    rep.from_schema = stored_version(&path)?;
    if rep.from_schema > db::SCHEMA_VERSION {
        return Err(DbError::SchemaTooNew { found: rep.from_schema, supported: db::SCHEMA_VERSION }.into());
    }
    if rep.from_schema == db::SCHEMA_VERSION {
        say(&format!("the store is of schema {} already: nothing to rebuild", db::SCHEMA_VERSION));
    } else if rep.from_schema < MIGRATE_FROM_SCHEMA {
        return Err(IndexerError::Config(format!(
            "{}: schema {}: open it once with the previous build (`run` or `index`) so that it is brought to schema {MIGRATE_FROM_SCHEMA} or later, or rebuild it with `kobd db replay`",
            path.display(),
            rep.from_schema
        )));
    } else {
        say(&format!("schema {} -> {}: building the new store beside the old one", rep.from_schema, db::SCHEMA_VERSION));
        let new = suffixed(&path, MIGRATING);
        let t = Instant::now();
        let built = if rep.from_schema >= 9 {
            upgrade_in_place(cfg, rep.from_schema, &path, &new, &mut rep, &mut say)
        } else {
            build(cfg, &path, &new, rep.from_schema, &mut rep, &mut say)
        };
        if let Err(e) = built {
            // nothing of the half-built file is worth keeping; the old store was not touched
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
            }
            return Err(e.into());
        }
        let t_swap = Instant::now();
        let old = swap(&path, &new)?;
        note(&mut rep.steps, &mut say, "swap", t_swap, format!("the previous store is {}", old.display()));
        say(&format!(
            "store: {} bytes -> {} bytes ({:.1} s in all)",
            rep.db_bytes_before,
            file_bytes(&path),
            t.elapsed().as_secs_f64()
        ));
        rep.kept_old = Some(old);
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

/// What [`vacuum`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VacuumReport {
    /// Bytes of the store (file and write-ahead log) before and after.
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Free pages the store held (what the rewrite gave back), and the page size.
    pub free_pages: u64,
    pub page_size: u64,
}

/// (pages, free pages, page size) of a store file, read-only.
fn page_stats(path: &Path) -> DbResult<(i64, i64, i64)> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI)?;
    c.busy_timeout(Duration::from_secs(30))?;
    Ok(c.query_row(
        "SELECT (SELECT page_count FROM pragma_page_count), (SELECT freelist_count FROM pragma_freelist_count), \
         (SELECT page_size FROM pragma_page_size)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

/// The suffix of the copy `kobd db vacuum` writes, and of the old file while it is swapped out.
const VACUUMING: &str = "vacuuming";
const BEFORE_VACUUM: &str = "before-vacuum";

/// `kobd db vacuum`: the store of the data directory of `cfg`, rewritten without its free pages (offline: it takes the writer lock). The
/// pruning (lean or archive) frees pages that SQLite uses again for new rows but never gives back to the file system by itself (a lean
/// store gives them back as it goes, `lean::release_pages`); a store that just had most of its history pruned (the first archive pruning
/// of a store that kept everything) is mostly free pages. The copy (`VACUUM INTO index.sqlite3.vacuuming`, the pages in use only) opens as
/// a store of this build and passes `PRAGMA quick_check` before it takes the place of the store, whose file is then deleted. The store is
/// not touched before the swap: a run that fails or is stopped leaves only the partial copy, deleted by the next run. Free disk needed: the
/// pages in use (at most the store's size), checked first.
pub fn vacuum(cfg: &IndexerConfig, mut say: impl FnMut(&str)) -> Result<VacuumReport, IndexerError> {
    let _writer = super::lock::WriterLock::acquire(cfg)?;
    let path = cfg.db_path();
    if !path.exists() {
        return Err(IndexerError::Config(format!("{} does not exist: there is nothing to vacuum", path.display())));
    }
    let version = stored_version(&path)?;
    if version != db::SCHEMA_VERSION {
        return Err(IndexerError::Config(format!(
            "{}: schema {version}, this build's is {}: run `kobd db migrate` (it rewrites the store without its free pages as well)",
            path.display(),
            db::SCHEMA_VERSION
        )));
    }
    let mut rep = VacuumReport { bytes_before: file_bytes(&path), ..Default::default() };
    let (pages, free, size) = page_stats(&path)?;
    (rep.free_pages, rep.page_size) = (free.max(0) as u64, size.max(0) as u64);
    let used = (pages - free).max(0) as u64 * rep.page_size;
    say(&format!(
        "{}: {} bytes, {} free pages of {} bytes ({} bytes in use)",
        path.display(),
        rep.bytes_before,
        rep.free_pages,
        rep.page_size,
        used
    ));
    let dir = path.parent().unwrap_or(Path::new("."));
    if let Some(avail) = crate::disk::free_bytes(dir) {
        // the copy, and a margin for its write-ahead log and the temporary files of the check
        let need = used + used / 10 + (64 << 20);
        if avail < need {
            return Err(IndexerError::Config(format!(
                "{} has {avail} bytes free; the copy needs about {need}: free space first (nothing was changed)",
                dir.display()
            )));
        }
    }
    let new = suffixed(&path, VACUUMING);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    let t = Instant::now();
    let copied = (|| -> DbResult<()> {
        {
            let src =
                Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI)?;
            src.busy_timeout(Duration::from_secs(30))?;
            src.execute("VACUUM INTO ?1", [new.to_string_lossy().as_ref()])?;
        }
        say(&format!("copy: {} bytes ({:.1} s)", file_bytes(&new), t.elapsed().as_secs_f64()));
        opens_and_checks(cfg, &new, &mut say)
    })();
    if let Err(e) = copied {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
        }
        return Err(e.into());
    }
    let old = swap_as(&path, &new, BEFORE_VACUUM)?;
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", old.display()));
    }
    rep.bytes_after = file_bytes(&path);
    say(&format!("store: {} bytes -> {} bytes ({:.1} s in all)", rep.bytes_before, rep.bytes_after, t.elapsed().as_secs_f64()));
    Ok(rep)
}
