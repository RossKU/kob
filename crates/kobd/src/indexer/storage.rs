//! Offline store migration for `kobd db migrate`, and the offline vacuum of `kobd db vacuum`.
//!
//! `kobd db migrate` brings a data directory written by an earlier build (schema 5 to 13) to the
//! layout of schema 14 in one offline run, and gives the space back. `kobd db vacuum` ([`vacuum`])
//! gives the free pages of a store of this build back.
//!
//! The store is not altered in place. A new file is built beside it and replaces it only when it
//! has been verified. The old file stays as `index.sqlite3.before-migrate`, the rollback, until the
//! operator deletes it.
//!
//! # Steps
//!
//! Each step is reported with its time.
//!
//! 1. **Copy.** The tables are created from `schema.sql`, without its indexes and triggers. They
//!    are then filled from the old store in bulk (`INSERT ... SELECT`, no per-row Rust). The old
//!    store is read through views that give every layout the shape of schema 7, with 32-byte ids in
//!    every row. A schema 8 store has them joined back. The load does the following:
//!    - `orders` get their integer id and name their maker, templates, extension commitment, token
//!      and quote token by id.
//!    - The other ids (tokens, holding owners) get their `ids` rows ([`super::ids`]). The values
//!      the orders name come first, the most used first, so that they take one byte.
//!    - Every row that named an order, a token or an owner by its 32 bytes names it by the
//!      integer.
//!    - The packed state columns are written in the form [`super::packed`] stores. A value an
//!      earlier build wrote whole is packed. A packed one is packed again from its decoded form.
//!    - The event ids, the sequence numbers and every other table are kept as they were.
//!    - A fill event gets its fee, `amount x tip / scale`, as the processor writes it. The tip is
//!      the one the order had at the fill: the tip that the next in-place amend of the order
//!      replaced (`detail.previous.tip` of its event), else the order's tip. Its executor stays
//!      unknown, because the record log of the past has no payer sections.
//! 2. **Indexes.** `schema.sql` is applied again. The indexes and triggers are built over the
//!    loaded rows, which is faster than maintaining them row by row. Then the counters and the
//!    activity totals that the triggers keep are counted once.
//! 3. **Verification.** Per table, the rows of the old store (states decoded, ids as their 32
//!    bytes) and the rows of the new one (joined back) are hashed in the same order and compared
//!    with their counts. The migration aborts, with the old store untouched, on a row that does not
//!    come back identical, a row that has no order (an orphan the integer layout cannot hold), or a
//!    count that differs. The new file must also pass `PRAGMA quick_check` and open as a store of
//!    this build.
//! 4. **Swap.** The new file replaces `index.sqlite3`.
//! 5. **Record log.** Every sealed segment is zstd-compressed, the gzips of older builds included
//!    ([`super::recordlog::compact`]). The newest is left whole.
//!
//! # Locking and disk
//!
//! The data directory's writer lock ([`super::lock`]) is held throughout, so no `run` or `index`
//! can write it meanwhile. The free disk needed is the new store (about 3/4 of the old one) plus
//! the temporary files of the index builds, while the old store stays. A run that stops half way
//! leaves only `index.sqlite3.migrating`, which the next run deletes. The old store is not touched
//! before the swap.
//!
//! A store of schema 9 to 12 takes the short way ([`upgrade_in_place`]). It is copied as it is and
//! given the new columns, tables and triggers.
//!
//! # Schema 14
//!
//! The steps above build the layout of schema 13 (`schema13.sql`), in `index.sqlite3.migrating13`.
//! [`compact`] then builds the store of schema 14 from it, and the file of schema 13 is deleted. A
//! store of schema 13 is compacted directly, with no file between. Schema 14 stores the 32-byte
//! transaction ids of the token holdings once each (`txs`), their program names once each
//! (`programs`) and their roles as codes, and names a pair fill's order and tokens by integer: the
//! holdings with their indexes take a quarter less, the pair fills less than half. Every other table is copied
//! as it is, and every table is read back and compared with the old store (step 3) before the swap.
//! The free disk needed is the new store, plus the file of schema 13 for an older store.
//!
//! A store that is of schema 14 already is left as it is, and the record log step still runs.
//!
//! The executors' transactions that the store already holds are counted as of unknown income and
//! network fee (`db::backfill_executor_txs`), because the store keeps no record of who owns their
//! outputs. `kobd db replay` rebuilds their income from the record log. Their network fees stay
//! unknown, because the log of the past has no input values.

use super::db::{self, DbError, DbResult};
use super::recordlog;
use super::IndexerError;
use crate::config::IndexerConfig;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The suffix of the file that the new store replaced.
pub const BEFORE_MIGRATE: &str = "before-migrate";
/// The suffix of the file that the new store is built in.
const MIGRATING: &str = "migrating";
/// The suffix of the file that a store older than schema 13 is brought to the layout of schema 13 in,
/// before [`compact`] builds the new store from it.
const MIGRATING13: &str = "migrating13";
/// The layout of schema 13, which the steps from an older store build ([`build`], [`upgrade_in_place`])
/// before schema 14 compacts it ([`compact`]).
const SCHEMA13: &str = include_str!("schema13.sql");
/// The schema version of [`SCHEMA13`].
const SCHEMA13_VERSION: i64 = 13;

/// Options of [`migrate`].
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

/// Row count of one table of the new store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableReport {
    /// Name of the table.
    pub table: String,
    /// Rows the new store holds in the table.
    pub rows: u64,
}

/// What [`migrate`] did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The schema version the store was at. It equals this build's when nothing was rebuilt.
    pub from_schema: i64,
    /// Rows per table that the bulk load copied.
    pub tables: Vec<TableReport>,
    /// Rows of `ids` (token covenant ids and holding owners).
    pub ids: u64,
    /// Size of the store (file and write-ahead log) before the migration, in bytes.
    pub db_bytes_before: u64,
    /// Size of the store (file and write-ahead log) after the migration, in bytes.
    pub db_bytes_after: u64,
    /// Size of the record log before the migration, in bytes.
    pub log_bytes_before: u64,
    /// Size of the record log after the migration, in bytes.
    pub log_bytes_after: u64,
    /// Sealed record-log segments that were compressed.
    pub log_segments_compressed: usize,
    /// Where the previous store was kept, when the store was rebuilt.
    pub kept_old: Option<PathBuf>,
    /// Each step with its duration.
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
    // Best effort. A reader holding the log back only delays the truncation.
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

// -------------------------------------------------------------------------------------------- copy

/// How a column of the new layout is read from the old store.
///
/// It is an expression, or `None` to take the column of that name as it is. The column reads as
/// NULL when the old store has none, as for a column an earlier build did not have.
type Map<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Loads a table from the source view `src_<table>` with some columns computed.
///
/// Every column of the new table but `skip` comes from `map` or, where it returns `None`, from the
/// source column of the same name with alias `alias`. A source column that is neither in the new
/// table nor in `renamed` (the source names the map consumes) would be lost, so it is an error.
/// `ord`, the order of the source rows, is the view's own.
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

/// The tables whose columns hold the ids of schema 9. The source views exist for these.
const MAPPED: [&str; 7] = ["orders", "order_utxos", "order_events", "order_state", "order_amends", "token_utxos", "token_holdings"];

/// Whether the old store has the table, and so its source view.
fn source_has(conn: &Connection, table: &str) -> DbResult<bool> {
    table_exists(conn, "old", table)
}

/// The source views `temp.src_<table>`.
///
/// Each table of [`MAPPED`] is in the shape of schema 7 (32-byte ids in the rows) plus a leading
/// `ord`, the order the rows are loaded in (the old rowid). A store of schema 5, 6 or 7 is in that
/// shape already. One of schema 8 has its orders, tokens and owners named by integer, which are
/// joined back. A row that names an order that is not there comes back with a NULL covenant id and
/// fails the verification.
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
            // The orders of schema 8 keep their blobs. Only the integer key is the view's `ord`.
            let list: Vec<String> = cols.iter().filter(|c| *c != "id").map(|c| format!("x.{c}")).collect();
            format!("CREATE TEMP VIEW src_{table} AS SELECT x.id AS ord, {} FROM old.{table} x", list.join(", "))
        } else {
            // Columns an earlier build did not have read as NULL.
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

/// A table loaded as it is: the old columns by name, in the table's natural order.
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
    // A WITHOUT ROWID table goes in the order of its primary key, any other in the order of its
    // rows.
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
    // meta: every key but the version and the counts that the backfills write afresh.
    {
        let t = Instant::now();
        conn.execute(
            "INSERT INTO main.meta (k, v) SELECT k, v FROM old.meta WHERE k NOT IN ('schema_version', 'counters', 'holding_programs', 'activity', 'executor_txs', 'orders_all', 'book_live', 'kcc23_auto_live')",
            [],
        )?;
        done("meta", count(conn, "main", "meta")?, t, say);
    }
    {
        let t = Instant::now();
        let n = copy_plain(conn, "blocks")?;
        done("blocks", n, t, say);
    }

    // The ids the orders name come first, the most used first. They are a few dozen values (the
    // makers, tokens, programs, templates, the extension commitment and the quote tokens), so a row
    // and an index entry then hold them in one or two bytes.
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
    // Then the other tokens and the holding owners. An owner may be a key, a script hash or a
    // covenant id.
    for sql in [
        "INSERT OR IGNORE INTO main.ids (h) SELECT token_cov_id FROM src_token_holdings GROUP BY token_cov_id",
        "INSERT OR IGNORE INTO main.ids (h) SELECT token_cov_id FROM src_token_utxos GROUP BY token_cov_id",
        "INSERT OR IGNORE INTO main.ids (h) SELECT token_cov_id FROM src_order_events WHERE token_cov_id IS NOT NULL GROUP BY token_cov_id",
        "INSERT OR IGNORE INTO main.ids (h) SELECT owner FROM src_token_holdings GROUP BY owner",
    ] {
        conn.execute(sql, [])?;
    }
    done("ids", count(conn, "main", "ids")?, t, say);

    // The orders: ids in the order of the old rows.
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

    // The tip each in-place amend replaced (`detail.previous.tip`), by the new order id and the
    // event id. An amend needs the order's verified state, so every one has it. The tip a fill was
    // paid at is the one the next amend after it replaced, or the order's own.
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
            // An amend without its previous tip, which should not happen, makes the fee unknown
            // rather than the order's current tip.
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
        "book_quotes",
        "order_flags",
        "pair_fills",
        "kcc23_candidates",
        "kcc23_txs",
        "kcc23_declarations",
        "logo_blocklist",
        "logo_reports",
        "logo_votes",
        "logo_vote_totals",
    ] {
        let t = Instant::now();
        let n = copy_plain(conn, table)?;
        done(table, n, t, say);
    }

    // The sequences. An id of an autoincrement table is never reused, so the next one is above
    // every id the old store ever gave.
    for table in ["blocks", "order_events", "order_amends", "rejects", "token_events", "pair_fills", "logo_reports", "logo_votes"] {
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

// ------------------------------------------------------------------------------------ verification

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

/// Per table: its name, the SQL on the old store and the SQL on the new one.
///
/// The old SQL reads the source views with 32-byte ids. The new SQL joins the ids back.
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

/// Checks the activity totals just counted against the rows.
///
/// The minutes and the hours hold every fill and `create` event once, and the fees the events
/// carry, each bucket's saturating at `i64::MAX` (`db::backfill_activity`). A difference aborts the
/// migration and leaves the old store untouched.
///
/// The fees are added up in Rust, in 128 bits: the buckets of a store, and the fees of a minute,
/// may sum past 64 bits, where the SQLite `SUM` fails.
pub(crate) fn check_activity(conn: &Connection) -> DbResult<()> {
    let total = |sql: &str, iv: i64| -> DbResult<(i64, i128, i64)> {
        let mut st = conn.prepare(sql)?;
        let mut rows = st.query([iv])?;
        let mut s = (0i64, 0i128, 0i64);
        while let Some(r) = rows.next()? {
            s.0 += r.get::<_, i64>(0)?;
            s.1 += i128::from(r.get::<_, i64>(1)?);
            s.2 += r.get::<_, i64>(2)?;
        }
        Ok(s)
    };
    let buckets = "SELECT fills, fees, orders FROM main.activity WHERE iv = ?1";
    // Per bucket, as the backfill sums them.
    let events = "SELECT SUM(kind = 'fill'), MIN(SUM(fees >> 32) * 4294967296 + SUM(fees & 4294967295), 9223372036854775807), SUM(kind = 'create')                   FROM (SELECT ts, kind, CASE WHEN kind = 'fill' THEN COALESCE(fee, 0) ELSE 0 END AS fees FROM main.order_events WHERE kind IN ('fill', 'create'))                   GROUP BY ts - ts % ?1";
    for (iv, name) in [(super::activity::MINUTE, "minute"), (super::activity::HOUR, "hour")] {
        let (b, e) = (total(buckets, iv)?, total(events, iv)?);
        if b != e {
            return Err(bad(format!(
                "activity: the buckets by {name} ({b:?}) do not add up to the events ({e:?}). Nothing was migrated (the old store is untouched)"
            )));
        }
    }
    // The executors' transactions, their income and network fees.
    let txs = |iv: i64| -> DbResult<(i64, i64, i64)> {
        Ok(conn.query_row(
            "SELECT COALESCE(SUM(txs), 0), COALESCE(SUM(income_n), 0), COALESCE(SUM(miner_fee_n), 0) FROM main.activity WHERE iv = ?1",
            [iv],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    };
    let rows: (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COUNT(CASE WHEN own_trade = 0 THEN income END), COUNT(miner_fee) FROM main.executor_txs",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let (m, h) = (txs(super::activity::MINUTE)?, txs(super::activity::HOUR)?);
    if m != rows || h != rows {
        return Err(bad(format!(
            "activity: the executors' transactions in the buckets ({m:?} by minute, {h:?} by hour) are not the rows ({rows:?}). Nothing was migrated (the old store is untouched)"
        )));
    }
    Ok(())
}

/// Step 3 (part): compares the old and new rows, table by table.
///
/// Tables copied as they were are compared by `SELECT *`.
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
        "book_quotes",
        "order_flags",
        "pair_fills",
        "kcc23_candidates",
        "kcc23_txs",
        "kcc23_declarations",
        "logo_blocklist",
        "logo_reports",
        "logo_votes",
        "logo_vote_totals",
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
        "SELECT k, v FROM old.meta WHERE k NOT IN ('schema_version', 'counters', 'holding_programs', 'activity', 'executor_txs', 'orders_all', 'book_live', 'kcc23_auto_live') ORDER BY k",
    )?;
    let (b, _) = digest(
        conn,
        "SELECT k, v FROM main.meta WHERE k NOT IN ('schema_version', 'counters', 'holding_programs', 'activity', 'executor_txs', 'orders_all', 'book_live', 'kcc23_auto_live') ORDER BY k",
    )?;
    if a != b {
        return Err(bad(
            "meta: the new store does not read back as the old one. Nothing was migrated (the old store is untouched)".into()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------- build and swap

/// Steps 1 to 3: builds the new store in `new`, loaded, indexed and verified against `old`.
fn build(cfg: &IndexerConfig, old: &Path, new: &Path, from_schema: i64, rep: &mut Report, say: &mut impl FnMut(&str)) -> DbResult<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    let conn = Connection::open(new)?;
    // A crash leaves a file nobody reads, so there is no journal and no sync.
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", -262_144)?;
    // The sorts of the index builds go to disk (the system temporary directory, or SQLITE_TMPDIR).
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

    // The tables come from the schema, without the indexes and triggers. They are built over the
    // loaded rows.
    conn.execute_batch(SCHEMA13)?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    conn.execute_batch(crate::logo_votes::SCHEMA)?;
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

    // A table of the old store that this build does not have would be lost, so refuse before
    // anything is copied. The retired `receipts` table is ignored.
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
    conn.execute_batch(SCHEMA13)?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    conn.execute_batch(crate::logo_votes::SCHEMA)?;
    // The counts the triggers keep, counted once, as a store without them gets them at start-up.
    db::backfill_counters(&conn)?;
    count_programs13(&conn)?;
    db::backfill_activity(&conn)?;
    db::backfill_executor_txs(&conn)?;
    db::backfill_all_time(&conn)?;
    check_activity(&conn)?;
    // The counters the old store kept, against the ones counted from the rows.
    if table_exists(&conn, "old", "counters")? {
        let (a, _) = digest(&conn, "SELECT name, n FROM old.counters WHERE n != 0 ORDER BY name")?;
        let (b, _) = digest(&conn, "SELECT name, n FROM main.counters WHERE n != 0 ORDER BY name")?;
        say(if a == b {
            "  counters: the same as the old store kept"
        } else {
            "  counters: counted afresh (the old store's differ: its triggers had drifted, or it predates them)"
        });
    }
    conn.execute("INSERT OR REPLACE INTO main.meta (k, v) VALUES ('schema_version', ?1)", [SCHEMA13_VERSION.to_string()])?;
    rep.steps.push(("indexes", t.elapsed()));
    say(&format!("indexes: built ({:.1} s)", t.elapsed().as_secs_f64()));

    let t = Instant::now();
    say("verify:");
    verify(&conn, say)?;
    rep.steps.push(("verify", t.elapsed()));
    conn.execute_batch("DETACH DATABASE old")?;
    drop(conn);
    quick_checked(new, say)
}

/// Checks that the file opens as a store of this build and passes `PRAGMA quick_check`.
///
/// This runs before the file replaces anything.
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

/// Checks that a store of the layout of schema 13, built on the way to schema 14, passes
/// `PRAGMA quick_check`. It is not a store of this build, so it is not opened as one: [`compact`]
/// checks the store it builds from it.
fn quick_checked(path: &Path, say: &mut impl FnMut(&str)) -> DbResult<()> {
    let t = Instant::now();
    let c = Connection::open(path)?;
    let ok: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if ok != "ok" {
        return Err(bad(format!(
            "the store of schema {SCHEMA13_VERSION} built on the way fails its check ({ok}); the store was not replaced"
        )));
    }
    drop(c);
    say(&format!("check: the store of schema {SCHEMA13_VERSION} passes quick_check ({:.1} s)", t.elapsed().as_secs_f64()));
    Ok(())
}

/// Counts the programs of the holdings (`holding_programs`) of a store of the layout of schema 13,
/// as `db::backfill_holding_programs` does for this build's.
fn count_programs13(conn: &Connection) -> DbResult<()> {
    if db::meta_get(conn, "holding_programs")?.is_some() {
        return Ok(());
    }
    conn.execute_batch(
        "DELETE FROM holding_programs; \
         INSERT INTO holding_programs (token_id, program, n) SELECT token_id, program, COUNT(*) FROM token_holdings GROUP BY token_id, program;",
    )?;
    db::meta_set(conn, "holding_programs", "1")?;
    Ok(())
}

// --------------------------------------------------------------------------------------- schema 14

/// The tables whose layout schema 14 changed. [`compact`] rebuilds them and copies every other
/// table as it is.
const COMPACTED: [&str; 2] = ["token_holdings", "pair_fills"];

/// The columns of the two tables in the layout of schema 13, which [`compact`] reads.
const HOLDINGS13: &str = "txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa, spent_block, spent_txid";
const PAIR_FILLS13: &str =
    "id, block_seq, daa, ts, txid, tx_pos, covenant_id, contract, base_cov_id, quote_cov_id, side, amount_a, amount_b, price, \
                            a_scale, tip_kas, counterparty, price_source";

/// Steps 1 to 3 from a store of the layout of schema 13: builds the store of schema 14 in `new`.
///
/// Schema 14 changes two tables and adds two (`schema.sql`):
///
/// - `token_holdings` names its transaction ids by `txs.id` and its program by `programs.id`, and
///   its role by a code. `txs` holds each transaction id that a holding names once, with the
///   holdings that name it counted (`n`). The ids are given in the order of the holdings, and each
///   holding keeps its rowid.
/// - `pair_fills` names its order by `orders.id` and its tokens by `ids.id`.
///
/// Every other table is copied as it is, its rows and their ids, and the sequences of the
/// autoincrement tables. The indexes and triggers are built over the loaded rows. The counters and
/// totals the triggers keep are copied, not counted again: a store that pruned keeps the totals of
/// what it deleted. Then every table is read back (the two changed ones joined back to the
/// columns of schema 13) and compared with the old store, row by row. A holding or pair fill that
/// names a value the integer layout cannot hold (an unknown role, an order or token not stored), a
/// row that does not read back identical, or a `txs` count that is not the holdings that name it
/// aborts the migration, with the old store untouched.
fn compact(cfg: &IndexerConfig, old: &Path, new: &Path, rep: &mut Report, say: &mut impl FnMut(&str)) -> DbResult<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", new.display()));
    }
    let conn = Connection::open(new)?;
    // A crash leaves a file nobody reads, so there is no journal and no sync.
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", -262_144)?;
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
    conn.execute_batch(include_str!("schema.sql"))?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    conn.execute_batch(crate::logo_votes::SCHEMA)?;
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
    let tables: Vec<String> = {
        let mut st =
            conn.prepare("SELECT name FROM old.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        v
    };
    for table in &tables {
        if !table_exists(&conn, "main", table)? {
            return Err(bad(format!(
                "the old store has a table this build does not know ({table}): nothing was migrated (the old store is untouched)"
            )));
        }
    }
    for (table, cols) in [("token_holdings", HOLDINGS13), ("pair_fills", PAIR_FILLS13)] {
        if table_exists(&conn, "old", table)? && columns(&conn, "old", table)?.join(", ") != cols {
            return Err(bad(format!("{table}: the old store's columns are not those of schema {SCHEMA13_VERSION}: nothing was migrated (the old store is untouched)")));
        }
    }

    let t = Instant::now();
    say("copy:");
    rep.tables.clear();
    conn.execute_batch("BEGIN")?;
    for table in tables.iter().filter(|t| !COMPACTED.contains(&t.as_str())) {
        let t = Instant::now();
        let n = copy_plain(&conn, table)?;
        say(&format!("  {table}: {n} rows ({:.1} s)", t.elapsed().as_secs_f64()));
        rep.tables.push(TableReport { table: table.clone(), rows: n });
    }

    if table_exists(&conn, "old", "token_holdings")? {
        let t = Instant::now();
        let unknown: i64 = conn.query_row(
            "SELECT COUNT(*) FROM old.token_holdings WHERE role NOT IN ('owned', 'custody', 'stray', 'bought')",
            [],
            |r| r.get(0),
        )?;
        if unknown > 0 {
            return Err(bad(format!(
                "token_holdings: {unknown} rows of the old store have a role this build does not know: nothing was migrated (the old store is untouched)"
            )));
        }
        // The common programs first: their ids take no byte.
        conn.execute_batch(
            "INSERT INTO main.programs (name) SELECT program FROM old.token_holdings GROUP BY program ORDER BY COUNT(*) DESC, program; \
             CREATE TEMP TABLE tx_first (h BLOB PRIMARY KEY, r INTEGER NOT NULL, n INTEGER NOT NULL) WITHOUT ROWID; \
             INSERT INTO temp.tx_first (h, r, n) SELECT h, MIN(r), COUNT(*) FROM ( \
               SELECT txid AS h, rowid AS r FROM old.token_holdings \
               UNION ALL SELECT spent_txid, rowid FROM old.token_holdings WHERE spent_txid IS NOT NULL) GROUP BY h; \
             INSERT INTO main.txs (h, n) SELECT h, n FROM temp.tx_first ORDER BY r, h; \
             DROP TABLE temp.tx_first; \
             CREATE INDEX main.txs_h ON txs (substr(h, 1, 8));",
        )?;
        let n = conn.execute(
            concat!(
                "INSERT INTO main.token_holdings (rowid, tx_id, idx, token_id, program_id, family, owner_id, owner_kind, amount, value, state, role_code, \
                 created_block, created_daa, spent_block, spent_tx_id) \
                 SELECT x.rowid, ",
                super::ids::tx_of!("x.txid"),
                ", x.idx, x.token_id, (SELECT p.id FROM main.programs p WHERE p.name = x.program), x.family, x.owner_id, x.owner_kind, x.amount, \
                 x.value, x.state, CASE x.role WHEN 'owned' THEN 0 WHEN 'custody' THEN 1 WHEN 'stray' THEN 2 WHEN 'bought' THEN 3 END, \
                 x.created_block, x.created_daa, x.spent_block, ",
                super::ids::tx_of!("x.spent_txid"),
                " FROM old.token_holdings x ORDER BY x.rowid"
            ),
            [],
        )? as u64;
        let txs = count(&conn, "main", "txs")?;
        say(&format!("  token_holdings: {n} rows, {txs} transaction ids ({:.1} s)", t.elapsed().as_secs_f64()));
        rep.tables.push(TableReport { table: "token_holdings".into(), rows: n });
        rep.tables.push(TableReport { table: "txs".into(), rows: txs });
    }
    if table_exists(&conn, "old", "pair_fills")? {
        let t = Instant::now();
        let n = conn.execute(
            "INSERT INTO main.pair_fills (id, block_seq, daa, ts, txid, tx_pos, order_id, contract, base_id, quote_id, side, amount_a, amount_b, \
             price, a_scale, tip_kas, counterparty, price_source) \
             SELECT p.id, p.block_seq, p.daa, p.ts, p.txid, p.tx_pos, o.id, p.contract, b.id, q.id, p.side, p.amount_a, p.amount_b, p.price, \
             p.a_scale, p.tip_kas, p.counterparty, p.price_source \
             FROM old.pair_fills p JOIN main.orders o ON o.covenant_id = p.covenant_id JOIN main.ids b ON b.h = p.base_cov_id \
             JOIN main.ids q ON q.h = p.quote_cov_id ORDER BY p.id",
            [],
        )? as u64;
        let raw = count(&conn, "old", "pair_fills")?;
        if n != raw {
            return Err(bad(format!(
                "pair_fills: {} of {raw} rows of the old store name an order or token that is not there; the integer layout cannot hold them. Nothing was migrated (the old store is untouched)",
                raw - n
            )));
        }
        say(&format!("  pair_fills: {n} rows ({:.1} s)", t.elapsed().as_secs_f64()));
        rep.tables.push(TableReport { table: "pair_fills".into(), rows: n });
    }
    conn.execute_batch("COMMIT")?;
    rep.ids = rep.rows("ids");
    rep.steps.push(("copy", t.elapsed()));

    let t = Instant::now();
    conn.execute_batch(include_str!("schema.sql"))?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    conn.execute_batch(crate::logo_votes::SCHEMA)?;
    // The index only the archive pruning reads, when the old store had it (it is built at the first
    // pruning otherwise). A lean store's are built when it opens.
    let spent_daa: bool =
        conn.prepare("SELECT 1 FROM old.sqlite_master WHERE type = 'index' AND name = 'token_holdings_spent_daa'")?.exists([])?;
    if spent_daa {
        super::archive::ensure_indexes(&conn)?;
    }
    conn.execute("INSERT OR REPLACE INTO main.meta (k, v) VALUES ('schema_version', ?1)", [db::SCHEMA_VERSION.to_string()])?;
    // The sequences, as the old store had them, once every row is in: an id of an autoincrement table
    // is never reused.
    conn.execute_batch(
        "DELETE FROM main.sqlite_sequence; INSERT INTO main.sqlite_sequence (name, seq) SELECT name, seq FROM old.sqlite_sequence;",
    )?;
    rep.steps.push(("indexes", t.elapsed()));
    say(&format!("indexes: built ({:.1} s)", t.elapsed().as_secs_f64()));

    let t = Instant::now();
    say("verify:");
    verify_compact(&conn, &tables, say)?;
    rep.steps.push(("verify", t.elapsed()));
    // The live books and the holding auto locks are derived (R4C2-2, R4C2-6): the copy was made without their triggers, so
    // they are filled again when the check below opens the store (`db::backfill_book_live`, `db::backfill_auto_live`).
    conn.execute_batch(
        "DELETE FROM main.book_live; DELETE FROM main.pair_live; DELETE FROM main.kcc23_auto_live; \
         DELETE FROM main.meta WHERE k IN ('book_live', 'kcc23_auto_live');",
    )?;
    conn.execute_batch("DETACH DATABASE old")?;
    drop(conn);
    opens_and_checks(cfg, new, say)
}

/// Step 3 of [`compact`]: every table of the old store reads back from the new one.
fn verify_compact(conn: &Connection, tables: &[String], say: &mut impl FnMut(&str)) -> DbResult<()> {
    let same = |table: &str, old_sql: &str, new_sql: &str, say: &mut dyn FnMut(&str)| -> DbResult<()> {
        let t = Instant::now();
        let (a, na) = digest(conn, old_sql)?;
        let (b, nb) = digest(conn, new_sql)?;
        if na != nb || a != b {
            return Err(bad(format!(
                "{table}: the new store does not read back as the old one ({nb} rows against {na}). Nothing was migrated (the old store is untouched)"
            )));
        }
        say(&format!("  {table}: {na} rows identical ({:.1} s)", t.elapsed().as_secs_f64()));
        Ok(())
    };
    for table in tables {
        let table = table.as_str();
        match table {
            "token_holdings" => same(
                table,
                &format!("SELECT rowid, {HOLDINGS13} FROM old.token_holdings ORDER BY rowid"),
                &format!("SELECT hid, {HOLDINGS13} FROM main.token_holdings_named ORDER BY hid"),
                say,
            )?,
            "pair_fills" => same(
                table,
                &format!("SELECT {PAIR_FILLS13} FROM old.pair_fills ORDER BY id"),
                "SELECT p.id, p.block_seq, p.daa, p.ts, p.txid, p.tx_pos, o.covenant_id, p.contract, b.h, q.h, p.side, p.amount_a, p.amount_b, \
                 p.price, p.a_scale, p.tip_kas, p.counterparty, p.price_source FROM main.pair_fills p LEFT JOIN main.orders o ON o.id = p.order_id \
                 LEFT JOIN main.ids b ON b.id = p.base_id LEFT JOIN main.ids q ON q.id = p.quote_id ORDER BY p.id",
                say,
            )?,
            // The version is the one row that changes.
            "meta" => same(
                table,
                "SELECT k, v FROM old.meta WHERE k != 'schema_version' ORDER BY k",
                "SELECT k, v FROM main.meta WHERE k != 'schema_version' ORDER BY k",
                say,
            )?,
            _ => {
                let cols = columns(conn, "old", table)?.join(", ");
                let without_rowid: bool = conn.query_row(
                    "SELECT sql LIKE '%WITHOUT ROWID%' FROM main.sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )?;
                let order = if without_rowid {
                    let mut st = conn.prepare("SELECT name FROM pragma_table_info(?1, 'main') WHERE pk > 0 ORDER BY pk")?;
                    let keys = st.query_map([table], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    keys.join(", ")
                } else {
                    "rowid".to_string()
                };
                // The rows in the order they were loaded in. A rowid that is not a column of its
                // own is renumbered, as a vacuum does.
                same(
                    table,
                    &format!("SELECT {cols} FROM old.{table} ORDER BY {order}"),
                    &format!("SELECT {cols} FROM main.{table} ORDER BY {order}"),
                    say,
                )?
            }
        }
    }
    same(
        "sqlite_sequence",
        "SELECT name, seq FROM old.sqlite_sequence ORDER BY name",
        "SELECT name, seq FROM main.sqlite_sequence ORDER BY name",
        say,
    )?;
    // Each transaction id once, and counted by the holdings that name it.
    let t = Instant::now();
    let q = |sql: &str| -> DbResult<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    let twice = q("SELECT COUNT(*) FROM (SELECT h FROM main.txs GROUP BY h HAVING COUNT(*) > 1)")?;
    let miscounted = q(
        "SELECT COUNT(*) FROM main.txs x LEFT JOIN (SELECT id, COUNT(*) AS c FROM (SELECT tx_id AS id FROM main.token_holdings \
         UNION ALL SELECT spent_tx_id FROM main.token_holdings WHERE spent_tx_id IS NOT NULL) GROUP BY id) r ON r.id = x.id WHERE r.c IS NOT x.n",
    )?;
    if twice > 0 || miscounted > 0 {
        return Err(bad(format!(
            "txs: {twice} transaction ids stored twice, {miscounted} counted wrong. Nothing was migrated (the old store is untouched)"
        )));
    }
    say(&format!("  txs: each transaction id once, counted by its holdings ({:.1} s)", t.elapsed().as_secs_f64()));
    Ok(())
}

/// Migrates a store of schema 9 to 12 to the current schema, keeping its table layout.
///
/// These are steps 1 to 3 for a store whose tables keep their layout. The store is copied as it is
/// (`VACUUM INTO`, which also gives the free pages back). Its triggers are dropped, because
/// `schema.sql` creates this build's triggers (schema 12 changed the one that keeps the programs
/// of the holdings). Then:
///
/// - From schema 9, the fill events get their two columns and their fees: `amount x tip / scale`
///   at the tip the order had at the fill, as in the copy of an older store.
/// - From schema 10, the activity buckets get the columns of the executors' transactions, and the
///   actors get their `kept` flag (schema 11).
/// - From schema 9 to 11, the orders get the KAS of their genesis output (`orders.genesis_value`,
///   schema 12), read from the genesis UTXO rows.
/// - `schema.sql` creates `recorded_outputs` empty (schema 13). The outputs of the past are in
///   the record log, and a replay fills the table.
/// - The executors' transactions and the buckets get their own-trade flag and count
///   (`executor_txs.own_trade`, `activity.own_n`). The transactions already counted as of unknown
///   income become unknown, and a replay tells the own trades.
///
/// Then `schema.sql` adds the new tables, indexes and triggers, and the activity totals are
/// counted once (from 9). The executors' transactions the store holds are counted as of unknown
/// income and network fee (`db::backfill_executor_txs`), because the store has no record of their
/// outputs' owners. `kobd db replay` rebuilds them from the record log. The all-time order counter
/// is set and the totals are checked against the rows.
///
/// Every table of the old store must hold as many rows in the new one, and the keys of `meta` and
/// `counters` must all be there. The activity buckets may gain the minutes of executors'
/// transactions without a fill, so they are checked against the rows instead. No row is rewritten
/// but the fills (from 9), the orders' new column and the incomes of the executors' transactions
/// (reset to unknown). Nothing of the old store is touched.
fn upgrade_in_place(from: i64, old: &Path, new: &Path, rep: &mut Report, say: &mut impl FnMut(&str)) -> DbResult<()> {
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
    // A crash leaves a file nobody reads, so there is no journal and no sync.
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", -262_144)?;
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    conn.pragma_update(None, "recursive_triggers", "OFF")?;
    super::packed::register(&conn)?;
    super::activity::register(&conn)?;
    conn.execute("ATTACH DATABASE ?1 AS old", [uri_of(old)])?;

    // No transaction: without a journal the file is thrown away on any error.
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
        // An amend without its previous tip, which should not happen, makes the fee unknown
        // rather than the order's current tip.
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
    // Schema 12: the KAS of each order's genesis output.
    // Schema 13: the own trades of the executors' transactions, and their count in the buckets. A
    // store of schema 11 or 12 has both tables.
    if table_exists(&conn, "main", "executor_txs")? && !columns(&conn, "main", "executor_txs")?.iter().any(|c| c == "own_trade") {
        conn.execute_batch("ALTER TABLE main.executor_txs ADD COLUMN own_trade INTEGER NOT NULL DEFAULT 0")?;
        // The store cannot tell which of the incomes a store of schema 11 or 12 counted were own
        // trades, so they become unknown and a replay tells them. The buckets count no income. The
        // triggers are dropped, so the rows and the buckets change together.
        conn.execute_batch(
            "UPDATE main.executor_txs SET income = NULL WHERE income IS NOT NULL; \
             UPDATE main.activity SET income = 0, income_n = 0, net = 0, net_n = 0;",
        )?;
    }
    if table_exists(&conn, "main", "activity")? && !columns(&conn, "main", "activity")?.iter().any(|c| c == "own_n") {
        conn.execute_batch("ALTER TABLE main.activity ADD COLUMN own_n INTEGER NOT NULL DEFAULT 0")?;
    }
    if !columns(&conn, "main", "orders")?.iter().any(|c| c == "genesis_value") {
        conn.execute_batch("ALTER TABLE main.orders ADD COLUMN genesis_value INTEGER")?;
    }

    let t = Instant::now();
    conn.execute_batch(SCHEMA13)?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    conn.execute_batch(crate::logo_votes::SCHEMA)?;
    if from == 9 {
        db::backfill_activity(&conn)?;
    }
    let unknown = db::backfill_executor_txs(&conn)?;
    db::backfill_all_time(&conn)?;
    let genesis_values = db::backfill_genesis_values(&conn)?;
    say(&format!("  orders: the KAS of {genesis_values} genesis outputs kept on the order (schema 12)"));
    check_activity(&conn)?;
    conn.execute("INSERT OR REPLACE INTO main.meta (k, v) VALUES ('schema_version', ?1)", [SCHEMA13_VERSION.to_string()])?;
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
            // The version is the one row of meta that changes. The backfills add keys and the
            // all-time counter. Every old key is there.
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
            // Checked against the rows above (`check_activity`).
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
    quick_checked(new, say)
}

/// Step 4: `new` takes the place of `path`, and the old file is kept beside it.
fn swap(path: &Path, new: &Path) -> DbResult<PathBuf> {
    swap_as(path, new, BEFORE_MIGRATE)
}

/// `new` takes the place of `path`. The old file, with its `-wal` and `-shm`, is kept as `suffix`.
///
/// Crash-safe (R3C2-3): the old store's write-ahead log is checkpointed into it first and must be
/// empty, so no log of the old store can sit beside a missing or new file. The new file is synced
/// before it takes the place, and the directory after each rename (`fsync` on Unix; on Windows
/// `sync_all` is `FlushFileBuffers` and a directory cannot be synced, NTFS journals the renames). A
/// start that finds the store missing puts the old file back ([`recover_interrupted_swap`]).
fn swap_as(path: &Path, new: &Path, suffix: &str) -> DbResult<PathBuf> {
    let old = suffixed(path, suffix);
    // Refused before the first rename, the new file goes: left beside a kept copy, a later start
    // without the store would take it for a swap that stopped and restore that copy (R5C2-I2).
    let discard = |e: DbError| {
        for s in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{s}", new.display()));
        }
        e
    };
    if old.exists() {
        return Err(discard(bad(format!(
            "{} exists already: delete it (or move it away) first; the new copy {} was removed",
            old.display(),
            new.display()
        ))));
    }
    checkpoint_out(path).map_err(discard)?;
    std::fs::OpenOptions::new().write(true).open(new).and_then(|f| f.sync_all()).map_err(|e| discard(e.into()))?;
    // The old file's -wal and -shm belong to it. The new file starts clean.
    std::fs::rename(path, &old).map_err(|e| discard(e.into()))?;
    for suffix in ["-wal", "-shm"] {
        let from = format!("{}{suffix}", path.display());
        if Path::new(&from).exists() {
            std::fs::rename(&from, format!("{}{suffix}", old.display()))?;
        }
    }
    recordlog::sync_dir(path)?;
    if let Err(e) = std::fs::rename(new, path) {
        // Put the store back.
        let _ = std::fs::rename(&old, path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::rename(format!("{}{suffix}", old.display()), format!("{}{suffix}", path.display()));
        }
        return Err(e.into());
    }
    recordlog::sync_dir(path)?;
    Ok(old)
}

/// Moves every frame of the store's write-ahead log into the store and empties the log.
///
/// It fails while another connection holds the log (a writer still runs).
fn checkpoint_out(path: &Path) -> DbResult<()> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    let (busy, log, done): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    drop(conn);
    let wal = format!("{}-wal", path.display());
    let left = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    if busy != 0 || log != done || left > 0 {
        return Err(bad(format!(
            "{}: its write-ahead log cannot be emptied ({left} bytes left): stop every writer of the data directory first",
            path.display()
        )));
    }
    Ok(())
}

/// Puts the old store back when `path` is missing beside a copy a `kobd db migrate` or `vacuum`
/// swap set aside (`.before-migrate`, `.before-vacuum`): the swap stopped between its renames
/// (R3C2-3). Without this, a lean store would start fresh and a store with a record log would be
/// refused. The old file comes back with its `-wal` and `-shm`; the half-swapped new file stays
/// where it was. Returns whether it did.
///
/// Only a swap that stopped is undone: its new file (`.migrating`, `.vacuuming`) is still beside the
/// copy, which proves that the last rename did not happen, and names the command. A `.before-migrate`
/// is kept on purpose for days, so a store that is merely missing (an operator's fresh start) is
/// left missing, with a warning that names the copies (R4C2-4).
pub fn recover_interrupted_swap(path: &Path) -> DbResult<bool> {
    if path.exists() {
        return Ok(false);
    }
    let stopped: Vec<&str> = [(BEFORE_MIGRATE, MIGRATING), (BEFORE_VACUUM, VACUUMING)]
        .into_iter()
        .filter(|(before, new)| suffixed(path, before).exists() && suffixed(path, new).exists())
        .map(|(before, _)| before)
        .collect();
    if stopped.len() > 1 {
        return Err(bad(format!(
            "{} is missing and both a stopped migrate and a stopped vacuum left their files beside it: put the right copy back by hand",
            path.display()
        )));
    }
    if stopped.is_empty() {
        let copies: Vec<String> = [BEFORE_MIGRATE, BEFORE_VACUUM]
            .into_iter()
            .map(|s| suffixed(path, s))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .collect();
        if !copies.is_empty() {
            tracing::warn!(store = %path.display(), copies = %copies.join(", "), "the store is missing and no swap of `kobd db migrate` or `vacuum` was in progress: the copies beside it are not put back (move one to the store's name to use it)");
        }
        return Ok(false);
    }
    let old = suffixed(path, stopped[0]);
    // The log first: a stop between the two leaves the same state, which this repeats.
    for s in ["-wal", "-shm"] {
        let from = format!("{}{s}", old.display());
        if Path::new(&from).exists() {
            std::fs::rename(&from, format!("{}{s}", path.display()))?;
        }
    }
    std::fs::rename(&old, path)?;
    recordlog::sync_dir(path)?;
    tracing::warn!(store = %path.display(), from = %old.display(), "the store was missing after an interrupted swap of `kobd db migrate` or `vacuum`: the old store is back in its place; run the command again");
    Ok(true)
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

/// The lowest schema that `kobd db migrate` brings forward.
///
/// An older store is rebuilt with `kobd db replay`.
pub const MIGRATE_FROM_SCHEMA: i64 = 5;

/// Migrates the store of the data directory of `cfg` and compresses its sealed record log.
///
/// It takes the writer lock. `say` reports each step as it finishes.
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
        let built = if rep.from_schema == SCHEMA13_VERSION {
            compact(cfg, &path, &new, &mut rep, &mut say)
        } else {
            // An older store is brought to the layout of schema 13 in a file of its own first, which
            // is compacted in turn and then deleted.
            let mid = suffixed(&path, MIGRATING13);
            let r = if rep.from_schema >= 9 {
                upgrade_in_place(rep.from_schema, &path, &mid, &mut rep, &mut say)
            } else {
                build(cfg, &path, &mid, rep.from_schema, &mut rep, &mut say)
            }
            .and_then(|()| compact(cfg, &mid, &new, &mut rep, &mut say));
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", mid.display()));
            }
            r
        };
        if let Err(e) = built {
            // Nothing of the half-built file is worth keeping. The old store was not touched.
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
        let r = recordlog::compact(&cfg.records_dir(), recordlog::Compact::All)?;
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
    /// Size of the store (file and write-ahead log) before the rewrite, in bytes.
    pub bytes_before: u64,
    /// Size of the store (file and write-ahead log) after the rewrite, in bytes.
    pub bytes_after: u64,
    /// Free pages the store held, which the rewrite gave back.
    pub free_pages: u64,
    /// Page size in bytes.
    pub page_size: u64,
}

/// Reads (pages, free pages, page size) of a store file, read-only.
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

/// Rewrites the store of the data directory of `cfg` without its free pages, offline.
///
/// It takes the writer lock. Pruning (lean or archive) frees pages that SQLite uses again for new
/// rows but never gives back to the file system by itself. A lean store gives them back as it
/// goes (`lean::release_pages`). A store that just had most of its history pruned, such as the
/// first archive pruning of a store that kept everything, is mostly free pages.
///
/// The copy (`VACUUM INTO index.sqlite3.vacuuming`, the pages in use only) opens as a store of
/// this build and passes `PRAGMA quick_check` before it takes the place of the store. The old file
/// is then deleted. The store is not touched before the swap, so a run that fails or is stopped
/// leaves only the partial copy, which the next run deletes. The free disk needed is the pages in
/// use (at most the store's size), checked first.
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
        // The copy, and a margin for its write-ahead log and the temporary files of the check.
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

#[cfg(test)]
mod swap_tests {
    use super::*;

    fn store(p: &Path, rows: i64) {
        let c = db::open_writer(p, "testnet-10").unwrap();
        for i in 0..rows {
            db::meta_set(&c, &format!("zz{i}"), "v").unwrap();
        }
    }

    fn keys(p: &Path) -> i64 {
        Connection::open(p).unwrap().query_row("SELECT COUNT(*) FROM meta WHERE k LIKE 'zz%'", [], |r| r.get(0)).unwrap()
    }

    /// R3C2-3: the swap checkpoints the old store's log into it and syncs; a start that finds the store missing between the
    /// renames puts the old one back, with its rows, instead of starting fresh.
    #[test]
    fn an_interrupted_swap_is_undone_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let (path, new) = (dir.path().join("index.sqlite3"), dir.path().join("index.sqlite3.vacuuming"));
        store(&path, 3);
        store(&new, 5);
        let old = swap_as(&path, &new, BEFORE_VACUUM).unwrap();
        assert_eq!((keys(&path), keys(&old)), (5, 3));
        assert!(!Path::new(&format!("{}-wal", old.display())).exists(), "no log beside the old store");
        assert!(!recover_interrupted_swap(&path).unwrap(), "nothing to undo");

        // stopped after the first rename: the store is missing, its old copy is set aside
        std::fs::remove_file(&path).unwrap();
        // a stop between the renames leaves the new file beside the copy
        store(&new, 5);
        assert!(recover_interrupted_swap(&path).unwrap());
        assert_eq!(keys(&path), 3, "the old rows are back");
        assert!(!old.exists());
    }

    /// R4C2-4: a store that is merely missing, beside an old `.before-migrate` kept on purpose (and no swap's new file), is
    /// not replaced by that copy. A migrate whose copy is kept followed by a vacuum that stopped puts back the vacuum's copy.
    #[test]
    fn a_missing_store_with_only_an_old_before_migrate_is_not_restored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite3");
        let (before_migrate, before_vacuum) = (suffixed(&path, BEFORE_MIGRATE), suffixed(&path, BEFORE_VACUUM));
        store(&before_migrate, 1);
        assert!(!recover_interrupted_swap(&path).unwrap(), "an operator's fresh start");
        assert!(!path.exists() && before_migrate.exists());

        // a vacuum stopped between its renames, with the migrate's copy still kept
        store(&before_vacuum, 7);
        store(&suffixed(&path, VACUUMING), 9);
        assert!(recover_interrupted_swap(&path).unwrap());
        assert_eq!(keys(&path), 7, "the store from before the vacuum, not the older one from before the migrate");
        assert!(before_migrate.exists());
    }

    /// R5C2-I2: a migrate whose swap is refused (a `.before-migrate` is kept already) removes its new file, so a later
    /// fresh start (the store moved away) does not take the pair for a swap that stopped and restore the old copy.
    #[test]
    fn a_refused_swap_leaves_no_new_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite3");
        let (kept, new) = (suffixed(&path, BEFORE_MIGRATE), suffixed(&path, MIGRATING));
        store(&path, 3);
        store(&kept, 1);
        store(&new, 5);
        assert!(swap_as(&path, &new, BEFORE_MIGRATE).unwrap_err().to_string().contains("was removed"));
        assert!(!new.exists() && keys(&path) == 3 && kept.exists());
        std::fs::remove_file(&path).unwrap();
        assert!(!recover_interrupted_swap(&path).unwrap(), "an operator's fresh start");
    }
}
