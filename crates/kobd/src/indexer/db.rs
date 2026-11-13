//! SQLite access for the indexer: opening the database, the single writer and the API's read pool.
//!
//! The store is SQLite in WAL mode, not RocksDB or a hand-rolled file. The indexer needs one atomic
//! commit of the cursor, the order delta, the undo stamps and the raw-log position. It needs ad-hoc
//! queries for the read API (books by token and price, orders by maker, recent fills). It also
//! needs operational transparency: an operator can open the file with the `sqlite3` shell, back it
//! up online with `VACUUM INTO` and diff two instances. SQLite is one small C dependency, its
//! readers never block the writer in WAL mode, and the data volume (KOB orders only, not the
//! chain) is small.

use super::packed::{sql_genesis, sql_order_ids, sql_order_ids_more};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// The schema version of the store layout.
///
/// A build reads only a store of its own schema. A store of an older schema is refused until
/// `kobd db migrate` has rebuilt it ([`refuse_old_layout`]). A store written before protocol v3
/// (schema 4 or older) cannot be migrated ([`refuse_pre_v3`]), and `kobd db replay` rebuilds it
/// from the record log. What each version changed is in the "Schema" paragraph of
/// docs/ops/executor.md.
pub const SCHEMA_VERSION: i64 = 13;
const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Errors of the indexer's database layer.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("database schema version {found} is newer than this binary supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("database belongs to network `{found}`, configured `{expected}`")]
    NetworkMismatch { found: String, expected: String },
    #[error(
        "database {0} was written by a build before protocol v3 (schema 4 or older: its orders are of templates this build does not \
         pin and its quantities are not base units) and cannot be migrated: move it aside (or delete it) and run `kobd db replay`, \
         which rebuilds it from the record log"
    )]
    PreV3(String),
    #[error(
        "database {0} is of schema {1}: this build reads schema {SCHEMA_VERSION} only. Stop every writer of the data directory and run `kobd db migrate` on it (offline; it builds the new store beside the old one and keeps the old file as `index.sqlite3.before-migrate`)"
    )]
    OldLayout(String, i64),
    #[error("{0}")]
    Invalid(String),
}

/// Result of a database operation.
pub type DbResult<T> = Result<T, DbError>;

/// Names what a failed write says about the storage, or `None` when the disk is not at fault.
///
/// The cases are a full disk, an I/O error, a corrupt or foreign database file and a read-only
/// file. Anything else, such as a bug or a constraint, gives `None`.
pub fn storage_problem(e: &super::ingest::IngestError) -> Option<&'static str> {
    use rusqlite::ErrorCode;
    let super::ingest::IngestError::Db(db) = e else { return None };
    match db {
        DbError::Io(_) => Some("an I/O error on the data directory: check the disk"),
        DbError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => match f.code {
            ErrorCode::DiskFull => Some(
                "the disk of the data directory is full: free space (the indexer goes on by itself once it can write; kob_data_dir_free_bytes shows the space)",
            ),
            ErrorCode::SystemIoFailure => Some("an I/O error writing the database: check the disk"),
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => Some(
                "the database file is corrupt: stop the executor and move the data directory's index.sqlite3 aside; a lean store (the Executor's) is rebuilt by its fast start, a full one by `kobd db replay` from the record log",
            ),
            ErrorCode::ReadOnly | ErrorCode::CannotOpen | ErrorCode::PermissionDenied => {
                Some("the database cannot be written (a read-only or inaccessible file or directory): check its permissions")
            }
            _ => None,
        },
        _ => None,
    }
}

fn pragmas(conn: &Connection, writer: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    if writer {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // FULL: a committed batch survives power loss, which the raw-log ordering relies on.
        conn.pragma_update(None, "synchronous", "FULL")?;
        // The write path re-reads the rows of the orders it touches: 64 MiB of page cache (the
        // default is 2 MiB).
        conn.pragma_update(None, "cache_size", -65_536)?;
        // The WAL file keeps its largest size, because a checkpoint resets it only while no read
        // holds an older snapshot. Once a reset happens, the file is cut back to this limit
        // (docs/ops/executor.md, B 5.5).
        conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024)?;
    }
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    // `kob_pack`, `kob_ustate`, `kob_hpack` and `kob_hstate`: how queries read and write the packed
    // state spans.
    super::packed::register(conn)?;
    // The row counters' triggers count an INSERT OR REPLACE of `order_state` once (schema.sql,
    // `counters`), so a REPLACE must not fire the delete trigger as well.
    conn.pragma_update(None, "recursive_triggers", "OFF")?;
    // Every statement of the write path and the reads is prepared once and kept: about 60 distinct
    // ones per KOB transaction cycle. The default capacity of 16 evicts them in turn, so nearly
    // every execution re-prepares its statement. Parsing and planning cost more than the statement
    // itself, about 40 % of the per-transaction time on a KOB-heavy chain (docs/ops/executor.md,
    // "Processing capacity").
    conn.set_prepared_statement_cache_capacity(256);
    Ok(())
}

/// Opens the writer connection, creating the file if needed, and applies the schema.
pub fn open_writer(path: &Path, network: &str) -> DbResult<Connection> {
    open_writer_with(path, network, false)
}

/// Like [`open_writer`], and with `lean` the store gives freed pages back to the file.
///
/// A lean store uses `auto_vacuum = INCREMENTAL` ([`super::lean`]). It is set before the schema on
/// a new store, and an older store is rewritten once.
pub fn open_writer_with(path: &Path, network: &str, lean: bool) -> DbResult<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    pragmas(&conn, true)?;
    // Before anything is created or altered, an older database is refused as it is.
    refuse_pre_v3(&conn, path)?;
    refuse_old_layout(&conn, path)?;
    refuse_newer_or_foreign(&conn, network)?;
    if lean {
        // A lean store keeps its WAL smaller still: 8 MiB instead of the default 64 MiB.
        conn.pragma_update(None, "journal_size_limit", 8 * 1024 * 1024)?;
        if super::lean::ensure_incremental(&conn)? {
            // The rewrite went through the WAL, so cut it back.
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            tracing::info!(file = %path.display(), "lean storage: the store was rewritten once so that it can give freed pages back");
        }
    }
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    if lean {
        // The indexes only the pruning reads (a few seconds once, on a store of a million orders).
        super::lean::ensure_indexes(&conn)?;
    }
    migrate(&conn)?;
    init_meta(&conn, network)?;
    super::candles::configure(&conn, lean)?;
    Ok(conn)
}

/// Refuses a store of an older schema than this build's, untouched.
///
/// The tables have an earlier layout, so opening the store with this build would build the new
/// indexes on columns that are not there. The error names the way out (`kobd db migrate`). The
/// check runs before the schema is applied.
pub fn refuse_old_layout(conn: &Connection, path: &Path) -> DbResult<()> {
    let has_meta: bool = conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'")?.exists([])?;
    if !has_meta {
        return Ok(());
    }
    if let Some(v) = meta_get(conn, "schema_version")?.and_then(|v| v.parse::<i64>().ok()) {
        if v < SCHEMA_VERSION {
            return Err(DbError::OldLayout(path.display().to_string(), v));
        }
    }
    Ok(())
}

/// Refuses a store of a newer schema than this build's, or of another network, untouched.
///
/// The check runs before the schema, the migrations and the lean rewrite, so an older build never
/// creates its tables and indexes in a newer store or rewrites its rows, and no build writes into a
/// store of another network. [`init_meta`] checks the same after a fresh store is created.
fn refuse_newer_or_foreign(conn: &Connection, network: &str) -> DbResult<()> {
    let has_meta: bool = conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'")?.exists([])?;
    if !has_meta {
        return Ok(());
    }
    let Some(v) = meta_get(conn, "schema_version")? else { return Ok(()) };
    let found: i64 = v.parse().unwrap_or(0);
    if found > SCHEMA_VERSION {
        return Err(DbError::SchemaTooNew { found, supported: SCHEMA_VERSION });
    }
    let net = meta_get(conn, "network")?.unwrap_or_default();
    if net != network {
        return Err(DbError::NetworkMismatch { found: net, expected: network.to_string() });
    }
    Ok(())
}

/// Refuses a database written before protocol v3 (schema 4 or older), untouched.
///
/// Such a database has the column `orders.lot_units` of the older templates' layouts. It cannot be
/// migrated: every order in it is of a template this build does not pin (never listed), and every
/// stored quantity counts in the old layouts. The record log rebuilds the database. `kobd db
/// replay` drops the reveals of templates this build does not pin and skips the frames it cannot
/// decode. The check runs before the schema is applied, so the refused file is left exactly as it
/// was.
pub fn refuse_pre_v3(conn: &Connection, path: &Path) -> DbResult<()> {
    let old: bool = conn.prepare("SELECT 1 FROM pragma_table_info('orders') WHERE name = 'lot_units'")?.exists([])?;
    if old {
        return Err(DbError::PreV3(path.display().to_string()));
    }
    Ok(())
}

/// Brings a store of this build up to date at every open.
///
/// It counts the row counters and the program counts once, and the triggers keep them from then
/// on. It then runs the idempotent backfills below. They repair rows an earlier build wrote and do
/// nothing on rows this build wrote. `kobd db migrate` carries such rows over, and the first open
/// of the new store repairs them. A store of an older schema is refused before this runs
/// ([`refuse_old_layout`]). The layout of the tables is what `schema.sql` says.
fn migrate(conn: &Connection) -> DbResult<()> {
    if backfill_counters(conn)? {
        tracing::info!("counted the orders, statuses, fills and rejects once for the health counters (kept by triggers from now on)");
    }
    let (programs, holdings) = backfill_holding_programs(conn)?;
    if holdings > 0 {
        tracing::info!(programs, holdings, "counted the programs of the token holdings once (kept by triggers from now on)");
    }
    let (exits, events) = backfill_exit_extensions(conn)?;
    if exits > 0 || events > 0 {
        tracing::info!(exits, events, "backfilled the extension commitment of if-done exits booked by an earlier build");
    }
    let stops = backfill_stop_entry_books(conn)?;
    if stops > 0 {
        tracing::info!(stops, "took the unarmed stop entries an earlier build listed as resting liquidity out of the books");
    }
    // KCC-23 proofs that an earlier build stored once per covenant id are shared per transaction,
    // in short transactions of 200 rows.
    let mut compacted = 0;
    loop {
        let n = super::kcc23::compact_legacy(conn, 200)?;
        compacted += n;
        if n == 0 {
            break;
        }
    }
    if compacted > 0 {
        tracing::info!(compacted, "KCC-23 declarations: the proofs are stored once per transaction instead of once per covenant id");
    }
    if backfill_activity(conn)? {
        tracing::info!("counted the activity totals once (kept by triggers from now on)");
    }
    let unknown_txs = backfill_executor_txs(conn)?;
    if unknown_txs > 0 {
        tracing::info!(
            unknown_txs,
            "counted the executors' transactions indexed before their income was recorded (income and network fee unknown)"
        );
    }
    backfill_all_time(conn)?;
    backfill_genesis_values(conn)?;
    let unknown = backfill_unknown_pair_extensions(conn)?;
    if unknown > 0 {
        tracing::info!(unknown, "token events: an unknown pair-token commitment an earlier build recorded as zero is now none");
    }
    Ok(())
}

/// Counts the row counters (`counters`, schema.sql) once, for a database that has not had them.
///
/// The counting is one transaction, after which the triggers keep the counters. It does nothing
/// (returns false) once done (`meta.counters`). A fresh database counts its empty tables.
pub fn backfill_counters(conn: &Connection) -> DbResult<bool> {
    if meta_get(conn, "counters")?.is_some() {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DELETE FROM counters; \
         INSERT INTO counters (name, n) SELECT 'orders', COUNT(*) FROM orders; \
         INSERT INTO counters (name, n) SELECT 'orders_listed', COUNT(*) FROM orders WHERE listed = 1; \
         INSERT INTO counters (name, n) SELECT 'rejects', COUNT(*) FROM rejects; \
         INSERT INTO counters (name, n) SELECT 'fills', COUNT(*) FROM order_events WHERE kind = 'fill'; \
         INSERT INTO counters (name, n) SELECT 'status:' || status, COUNT(*) FROM order_state GROUP BY status;",
    )?;
    meta_set(&tx, "counters", "1")?;
    tx.commit()?;
    Ok(true)
}

/// Counts the activity totals (`activity`, `actors`, schema.sql) once, for a database without them.
///
/// They are counted from the events and orders in one transaction, after which the triggers keep
/// them. It does nothing (returns false) once done (`meta.activity`). A fresh database counts its
/// empty tables. The minute buckets are counted from the events in one pass, and the hour buckets
/// from the minutes.
///
/// The fees of a bucket saturate at `i64::MAX` rather than fail, as `SUM` does past 64 bits. They are
/// summed as their high and low 32 bits, which cannot overflow, and an integer product or sum past
/// 64 bits turns into a real that `MIN` takes back to `i64::MAX`. A store that an earlier build
/// indexed with the tips that hostile terms name (`activity::fill_tip`) could not open otherwise.
pub fn backfill_activity(conn: &Connection) -> DbResult<bool> {
    if meta_get(conn, "activity")?.is_some() {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DELETE FROM activity; DELETE FROM actors; \
         INSERT INTO activity (iv, t, fills, fees, orders, txs, income, income_n, miner_fees, miner_fee_n, net, net_n, own_n) \
           SELECT 60000, t, SUM(fills), MIN(SUM(fees >> 32) * 4294967296 + SUM(fees & 4294967295), 9223372036854775807), SUM(orders), SUM(txs), SUM(income), SUM(income_n), SUM(miner_fees), SUM(miner_fee_n), \
                  SUM(net), SUM(net_n), SUM(own_n) FROM ( \
             SELECT ts - ts % 60000 AS t, kind = 'fill' AS fills, CASE WHEN kind = 'fill' THEN COALESCE(fee, 0) ELSE 0 END AS fees, \
                    kind = 'create' AS orders, 0 AS txs, 0 AS income, 0 AS income_n, 0 AS miner_fees, 0 AS miner_fee_n, 0 AS net, 0 AS net_n, \
                    0 AS own_n \
               FROM order_events WHERE kind IN ('fill', 'create') \
             UNION ALL SELECT ts - ts % 60000, 0, 0, 0, 1, COALESCE(ci, 0), ci IS NOT NULL, COALESCE(miner_fee, 0), \
                    miner_fee IS NOT NULL, COALESCE(ci - miner_fee, 0), ci IS NOT NULL AND miner_fee IS NOT NULL, own_trade \
               FROM (SELECT ts, miner_fee, own_trade, CASE WHEN own_trade = 0 THEN income END AS ci FROM executor_txs)) \
           GROUP BY t; \
         INSERT INTO activity (iv, t, fills, fees, orders, txs, income, income_n, miner_fees, miner_fee_n, net, net_n, own_n) \
           SELECT 3600000, t - t % 3600000, SUM(fills), MIN(SUM(fees >> 32) * 4294967296 + SUM(fees & 4294967295), 9223372036854775807), SUM(orders), SUM(txs), SUM(income), SUM(income_n), SUM(miner_fees), \
                  SUM(miner_fee_n), SUM(net), SUM(net_n), SUM(own_n) FROM activity WHERE iv = 60000 GROUP BY t - t % 3600000; \
         INSERT INTO actors (kind, actor_id, last_block) \
           SELECT 1, maker_id, MAX(genesis_block) FROM orders WHERE maker_id IS NOT NULL GROUP BY maker_id; \
         INSERT INTO actors (kind, actor_id, last_block) \
           SELECT 2, executor_id, MAX(block_seq) FROM order_events WHERE kind = 'fill' AND executor_id IS NOT NULL GROUP BY executor_id;",
    )?;
    meta_set(&tx, "activity", "1")?;
    tx.commit()?;
    Ok(true)
}

/// The event kinds of an executor's transaction.
///
/// They are all the kinds but the maker's cancel, amend and sweep and the placement.
/// [`backfill_executor_txs`] counts a transaction with one of them.
pub const EXECUTOR_EVENT_KINDS: &str = "('fill', 'kill', 'refund', 'arm', 'trail', 'update', 'rearm', 'unknown')";

/// Counts the executors' transactions of a store that indexed them before recording their income.
///
/// It adds one row to `executor_txs` per transaction with an event of an executor (a kind of
/// [`EXECUTOR_EVENT_KINDS`]), with neither its income nor its network fee. The totals then count
/// them as unknown instead of leaving them out unseen. It runs once, in one transaction
/// (`meta.executor_txs`). A fresh database has none. Returns the transactions counted.
pub fn backfill_executor_txs(conn: &Connection) -> DbResult<usize> {
    if meta_get(conn, "executor_txs")?.is_some() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        &format!(
            "INSERT INTO executor_txs (block_seq, daa, ts, txid, income, miner_fee) \
             SELECT block_seq, MIN(daa), MIN(ts), txid, NULL, NULL FROM order_events \
             WHERE kind IN {EXECUTOR_EVENT_KINDS} AND block_seq > 0 GROUP BY block_seq, txid ORDER BY MIN(id)"
        ),
        [],
    )?;
    meta_set(&tx, "executor_txs", "1")?;
    tx.commit()?;
    Ok(n)
}

/// Counts the all-time orders counter (`counters.orders_all`) for a store that did not have it.
///
/// The lean pruning does not take this counter back. It holds the orders the store has. It runs
/// once (`meta.orders_all`).
pub fn backfill_all_time(conn: &Connection) -> DbResult<bool> {
    if meta_get(conn, "orders_all")?.is_some() {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO counters (name, n) SELECT 'orders_all', COUNT(*) FROM orders WHERE true ON CONFLICT (name) DO UPDATE SET n = excluded.n",
        [],
    )?;
    meta_set(&tx, "orders_all", "1")?;
    tx.commit()?;
    Ok(true)
}

/// Fills `orders.genesis_value` for a store that did not keep the KAS of the genesis outputs.
///
/// The value is read from the genesis UTXO rows, which no build before schema 12 deleted. An order
/// whose row is not there keeps NULL, and the archive pruning keeps its genesis row whatever
/// happens. It runs once, in one transaction (`meta.genesis_values`). A fresh database has no
/// orders. Returns the orders filled.
pub fn backfill_genesis_values(conn: &Connection) -> DbResult<usize> {
    if meta_get(conn, "genesis_values")?.is_some() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE orders SET genesis_value = (SELECT u.value FROM order_utxos u WHERE u.txid = orders.genesis_txid AND u.idx = orders.genesis_out) \
         WHERE genesis_value IS NULL AND genesis_txid IS NOT NULL",
        [],
    )?;
    meta_set(&tx, "genesis_values", "1")?;
    tx.commit()?;
    Ok(n)
}

/// Counts the (token, program) pairs of `holding_programs` once, for a database without them.
///
/// They are counted from the holdings in one transaction, after which the triggers keep them
/// (schema.sql). It does nothing (`(0, 0)`) once done (`meta.holding_programs`). A fresh database
/// counts its empty table. Returns (rows of the table, holdings counted).
pub fn backfill_holding_programs(conn: &Connection) -> DbResult<(usize, usize)> {
    if meta_get(conn, "holding_programs")?.is_some() {
        return Ok((0, 0));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("DELETE FROM holding_programs;")?;
    let programs = tx.execute(
        "INSERT INTO holding_programs (token_id, program, n) SELECT token_id, program, COUNT(*) FROM token_holdings GROUP BY token_id, program",
        [],
    )?;
    let holdings: i64 = tx.query_row("SELECT COALESCE(SUM(n), 0) FROM holding_programs", [], |r| r.get(0))?;
    meta_set(&tx, "holding_programs", "1")?;
    tx.commit()?;
    Ok((programs, holdings as usize))
}

/// A token identity: (token, program, extension commitment).
type Identity = (Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);
/// The token identities that the order `(contract, token, program, commitment, genesis state)`
/// placed.
///
/// They are the identities as the processor records them
/// ([`super::processor::placement_identities`]).
fn identities_of_order(
    contract: &str,
    token: Option<Vec<u8>>,
    tpl: Option<Vec<u8>>,
    ext: Option<Vec<u8>>,
    genesis_state: Vec<u8>,
) -> Vec<Identity> {
    let h = |b: &Option<Vec<u8>>| b.as_deref().and_then(|b| <[u8; 32]>::try_from(b).ok());
    let plain = || token.clone().map(|t| (t, tpl.clone(), ext.clone())).into_iter().collect();
    let (Some(state), Some(tok)) = (super::reads::tip_state(contract, Some(genesis_state)), h(&token)) else { return plain() };
    super::processor::placement_identities(&state, crate::hex::Hash32(tok), h(&tpl).map(crate::hex::Hash32), h(&ext))
        .into_iter()
        .map(|(t, p, e)| (t.0.to_vec(), p.map(|p| p.0.to_vec()), e.map(|e| e.to_vec())))
        .collect()
}

/// A `seen` row with the zero commitment: (id, token, program, txid).
type ZeroRow = (i64, Vec<u8>, Option<Vec<u8>>, Vec<u8>);
/// An order of a token: (genesis txid, genesis block, genesis daa, it names the row's identity).
type Naming = (Option<Vec<u8>>, i64, i64, bool);

/// Re-derives the `seen` token events with the all-zero commitment from the orders.
///
/// An earlier build recorded the token identity of a pair order's token with the all-zero
/// commitment when the order's state does not carry one (a family code other than KCC-20). The
/// token list then could not tell it from the real zero commitment of the standard fixed-supply
/// class, and reported none for every KCC-20 token of that class.
///
/// - A row that no order of its transaction names with the zero commitment becomes the identity
///   without a commitment. It is dropped when that identity is already recorded.
/// - The first order that does name the zero commitment gets the zero row that the processor
///   skipped, because the identity looked seen.
///
/// It is idempotent, and a no-op on a database written by this build. `kobd db replay` records the
/// same identities. Returns the rows changed.
pub fn backfill_unknown_pair_extensions(conn: &Connection) -> DbResult<usize> {
    let zero = [0u8; 32];
    let rows: Vec<ZeroRow> = conn
        .prepare("SELECT id, token_cov_id, tpl_hash, txid FROM token_events WHERE kind = 'seen' AND ext_commit = ?1 ORDER BY id")?
        .query_map([&zero[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(0);
    }
    // The orders that name one of these tokens, each as (genesis txid, genesis block, genesis daa,
    // whether it names the row's identity). With `txid`, only the orders of that transaction.
    let names = |tok: &[u8], tpl: &Option<Vec<u8>>, txid: Option<&[u8]>| -> DbResult<Vec<Naming>> {
        let mut st = conn.prepare_cached(concat!(
            "SELECT o.contract, o_tk.h, o_tp.h, o_ex.h, ",
            sql_genesis!("o"),
            ", o.genesis_txid, o.genesis_block, o.genesis_daa FROM orders o",
            sql_order_ids!("o"),
            sql_order_ids_more!("o"),
            "WHERE (o.token_id = (SELECT id FROM ids WHERE h = ?1) OR o.quote_id = (SELECT id FROM ids WHERE h = ?1)) \
                 AND (?2 IS NULL OR o.genesis_txid = ?2) ORDER BY o.genesis_block, o.genesis_daa, o.rowid"
        ))?;
        let out = st
            .query_map(rusqlite::params![tok, txid], |r| {
                let ids = identities_of_order(&r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
                let named = ids.iter().any(|(t, p, e)| t == tok && p == tpl && e.as_deref() == Some(&zero[..]));
                Ok((r.get(5)?, r.get(6)?, r.get(7)?, named))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(out)
    };
    let mut changed = 0;
    for (id, tok, tpl, txid) in rows {
        // The orders of the row's own transaction say whether it stands, so only their states are
        // decoded. This runs at every start-up, and decoding every order of the token for every
        // row took 12 s on a store of 300,000 orders.
        if names(&tok, &tpl, Some(&txid))?.iter().any(|(t, _, _, named)| *named && t.as_deref() == Some(&txid[..])) {
            continue;
        }
        let orders = names(&tok, &tpl, None)?;
        changed += 1;
        conn.execute("UPDATE token_events SET ext_commit = NULL WHERE id = ?1", [id])?;
        // The identity without a commitment once: its first sighting stays.
        conn.execute(
            "DELETE FROM token_events WHERE kind = 'seen' AND token_cov_id = ?1 AND tpl_hash IS ?2 AND ext_commit IS NULL \
             AND id > (SELECT MIN(id) FROM token_events WHERE kind = 'seen' AND token_cov_id = ?1 AND tpl_hash IS ?2 AND ext_commit IS NULL)",
            rusqlite::params![tok, tpl],
        )?;
        // The zero commitment's first real sighting, which the processor skipped (the identity
        // looked seen).
        if let Some((Some(t), block, daa, _)) = orders.iter().find(|o| o.3 && o.0.is_some()) {
            conn.execute(
                "INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) VALUES (?1,?2,?3,'seen',?4,?5,?6)",
                rusqlite::params![tok, tpl, &zero[..], t, block, daa],
            )?;
        }
    }
    Ok(changed)
}

/// Takes the unarmed stop entries out of the KAS books.
///
/// An earlier build listed every if-done entry in its KAS book at its limit, unarmed stop entries
/// too, which nothing fills before their trigger. This re-derives the live entries' state, which
/// keeps an unarmed stop entry out of the books (`processor::refresh_order_state`). It is
/// idempotent. Returns the entries taken out.
///
/// It runs at every open, over every live if-done entry in a book. So it is one transaction, not
/// one commit (and sync) per entry, and each refresh keeps the stored filled amount instead of
/// summing the entry's fills. With 10^5 entries it took minutes per restart.
pub fn backfill_stop_entry_books(conn: &Connection) -> DbResult<usize> {
    let tx = conn.unchecked_transaction()?;
    let ids: Vec<Vec<u8>> = tx
        .prepare(
            "SELECT o.covenant_id FROM order_state s CROSS JOIN orders o ON o.id = s.order_id              WHERE o.in_book = 1 AND o.contract IN ('KobIfdAsk','KobIfdBid','KobIfdAskKron','KobIfdBidKron')                AND s.status IN ('open','partial')",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = 0;
    for id in ids {
        let Ok(cov) = <[u8; 32]>::try_from(id.as_slice()) else { continue };
        super::processor::refresh_order_state_adding(&tx, &crate::hex::Hash32(cov), 0)?;
        let in_book: i64 = tx.query_row("SELECT in_book FROM orders WHERE covenant_id = ?1", [&cov[..]], |r| r.get(0))?;
        out += usize::from(in_book == 0);
    }
    tx.commit()?;
    Ok(out)
}

/// Copies the extension commitment of each if-done exit from its entry, where the exit has none.
///
/// Exits booked by an earlier build were stored without an extension commitment (`orders.ext_id`
/// NULL). Their views could not rebuild the custody token state, so cancel, cancel-replace and
/// refund failed, and the matcher rebuilt it with a zero commitment. An exit's commitment is its
/// entry's: the entry covenant writes it into the exit's custody, and a `KobCondBid` exit also
/// carries it in its state. The database already holds it, so it is copied.
///
/// This also drops the `seen` token registry events that those exits recorded for the bogus
/// (token, program, no commitment) identity. A replay does not record them, because the entry saw
/// the identity first. They stay when some order of that token still has no commitment.
///
/// It is idempotent, and a no-op on a database written by this build. `kobd db replay` gives the
/// same rows. Returns (exits fixed, events dropped).
pub fn backfill_exit_extensions(conn: &Connection) -> DbResult<(usize, usize)> {
    let exits = conn.execute(
        "UPDATE orders SET ext_id = (SELECT p.ext_id FROM orders p WHERE p.covenant_id = orders.parent) \
         WHERE ext_id IS NULL AND parent IS NOT NULL \
           AND EXISTS (SELECT 1 FROM orders p WHERE p.covenant_id = orders.parent AND p.ext_id IS NOT NULL)",
        [],
    )?;
    let events = conn.execute(
        "DELETE FROM token_events WHERE kind = 'seen' AND ext_commit IS NULL \
           AND EXISTS (SELECT 1 FROM orders x JOIN ids xt ON xt.id = x.token_id LEFT JOIN ids xp ON xp.id = x.token_tpl_id \
                       WHERE x.genesis_txid = token_events.txid AND x.parent IS NOT NULL \
                       AND xt.h = token_events.token_cov_id AND xp.h IS token_events.tpl_hash \
                       AND x.ext_id IS NOT NULL) \
           AND NOT EXISTS (SELECT 1 FROM orders o JOIN ids ot ON ot.id = o.token_id LEFT JOIN ids op ON op.id = o.token_tpl_id \
                           WHERE ot.h = token_events.token_cov_id \
                           AND op.h IS token_events.tpl_hash AND o.ext_id IS NULL)",
        [],
    )?;
    Ok((exits, events))
}

/// Opens an existing database read-only, with no schema change and no lock.
///
/// It works next to the writer, for example `kobd db export-orders` while `run` is running. It
/// refuses a database of another network or of another schema version. A writer of this build
/// migrates it first.
pub fn open_reader(path: &Path, network: &str) -> DbResult<Connection> {
    if !path.exists() {
        return Err(DbError::Invalid(format!("{} does not exist", path.display())));
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    pragmas(&conn, false)?;
    refuse_pre_v3(&conn, path)?;
    refuse_old_layout(&conn, path)?;
    let found: i64 = meta_get(&conn, "schema_version")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    if found != SCHEMA_VERSION {
        return Err(DbError::Invalid(format!(
            "database schema version {found}, this binary reads {SCHEMA_VERSION}: open it once with a writer of this build (`run` or `index`)"
        )));
    }
    let net = meta_get(&conn, "network")?.unwrap_or_default();
    if net != network {
        return Err(DbError::NetworkMismatch { found: net, expected: network.to_string() });
    }
    Ok(conn)
}

/// Opens an in-memory database with the schema applied, for tests.
pub fn open_memory(network: &str) -> DbResult<Connection> {
    let conn = Connection::open_in_memory()?;
    super::packed::register(&conn)?;
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    migrate(&conn)?;
    init_meta(&conn, network)?;
    super::candles::configure(&conn, false)?;
    Ok(conn)
}

fn init_meta(conn: &Connection, network: &str) -> DbResult<()> {
    match meta_get(conn, "schema_version")? {
        None => {
            meta_set(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
            meta_set(conn, "network", network)?;
        }
        Some(v) => {
            let found: i64 = v.parse().unwrap_or(0);
            if found > SCHEMA_VERSION {
                return Err(DbError::SchemaTooNew { found, supported: SCHEMA_VERSION });
            }
            let net = meta_get(conn, "network")?.unwrap_or_default();
            if net != network {
                return Err(DbError::NetworkMismatch { found: net, expected: network.to_string() });
            }
            if found < SCHEMA_VERSION {
                // `migrate` has brought the tables up to date.
                meta_set(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
            }
        }
    }
    Ok(())
}

/// Reads the `meta` value of `key`.
pub fn meta_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT v FROM meta WHERE k = ?1", [key], |r| r.get(0)).optional()
}

/// Sets the `meta` value of `key`, replacing an existing one.
pub fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", [key, value])?;
    Ok(())
}

/// Longest time one pooled read may run before SQLite is told to interrupt it.
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A pool of read-only connections. WAL lets them read while the follower commits.
///
/// There are two lanes. [`ReadPool::with`] serves every read. [`ReadPool::with_light`] has a
/// connection of its own for the reads that cost a few index lookups (the health counters, one
/// order), so they never queue behind the heavy ones. The measurements are in docs/ops/executor.md,
/// "Read pool".
#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<PoolInner>,
}

/// One lane: its connections and the permits bounding the reads in flight on them.
struct Lane {
    conns: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

struct PoolInner {
    heavy: Lane,
    light: Lane,
}

/// Tells a read that its caller went away.
///
/// The caller's future is dropped, for example by the API guard's timeout or a client disconnect.
/// The read is then not started, or the statement it is running is interrupted
/// (`sqlite3_interrupt`), so it gives its connection back.
#[derive(Default)]
struct Cancel {
    gone: std::sync::atomic::AtomicBool,
    /// The connection the read runs on, while it does.
    ///
    /// It is cleared before the connection goes back to the pool, so a late interrupt never
    /// reaches the next read.
    running: Mutex<Option<rusqlite::InterruptHandle>>,
}

/// Cancels its read when dropped before [`CancelOnDrop::done`].
struct CancelOnDrop(Arc<Cancel>, bool);

impl CancelOnDrop {
    fn done(mut self) {
        self.1 = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.1 {
            self.0.gone.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(h) = self.0.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                h.interrupt();
            }
        }
    }
}

fn open_lane(path: &Path, size: usize) -> DbResult<Lane> {
    let mut conns = Vec::with_capacity(size);
    for _ in 0..size {
        let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        pragmas(&c, false)?;
        conns.push(c);
    }
    Ok(Lane { conns: Mutex::new(conns), permits: Arc::new(Semaphore::new(size)) })
}

impl ReadPool {
    /// Opens `size` read-only connections to an existing database file, and one for the light lane.
    pub fn open(path: &Path, size: usize) -> DbResult<Self> {
        Ok(ReadPool { inner: Arc::new(PoolInner { heavy: open_lane(path, size.max(1))?, light: open_lane(path, 1)? }) })
    }

    /// Runs a read on a pooled connection on the blocking thread pool.
    pub async fn with<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        self.run(false, f).await
    }

    /// Like [`ReadPool::with`], on the light lane.
    ///
    /// Use it only for reads bounded by a few index lookups, whatever the request asks.
    pub async fn with_light<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        self.run(true, f).await
    }

    async fn run<T, F>(&self, light: bool, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        let permits = if light { &self.inner.light.permits } else { &self.inner.heavy.permits };
        // The permit travels into the blocking task and is released only when the task ends. A
        // caller that is cancelled (request timeout, client disconnect) must not hand its permit to
        // the next request while the read still holds its pooled connection. Otherwise the pool
        // would run dry and the bound on concurrent reads would not hold.
        let permit = permits.clone().acquire_owned().await.map_err(|_| DbError::Invalid("read pool closed".into()))?;
        let inner = self.inner.clone();
        let cancel = Arc::new(Cancel::default());
        let guard = CancelOnDrop(cancel.clone(), true);
        let r = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let lane = if light { &inner.light } else { &inner.heavy };
            let conn = lane.conns.lock().unwrap_or_else(|e| e.into_inner()).pop();
            let Some(conn) = conn else { return Err(DbError::Invalid("read pool exhausted".into())) };
            *cancel.running.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.get_interrupt_handle());
            // The caller went away while the read waited for the blocking pool, so it is not run.
            let r = if cancel.gone.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(Err(DbError::Invalid("read cancelled: the caller went away".into())))
            } else {
                // A read that outlives `READ_TIMEOUT` is interrupted, so that a hostile request
                // cannot hold a pooled connection for minutes.
                let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
                let handle = conn.get_interrupt_handle();
                let watchdog = std::thread::Builder::new().name("kob-read-watchdog".into()).spawn(move || {
                    if done_rx.recv_timeout(READ_TIMEOUT) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                        handle.interrupt();
                    }
                });
                // The connection goes back even if the read panics.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&conn)));
                drop(done_tx);
                if let Ok(w) = watchdog {
                    let _ = w.join();
                }
                r
            };
            *cancel.running.lock().unwrap_or_else(|e| e.into_inner()) = None;
            lane.conns.lock().unwrap_or_else(|e| e.into_inner()).push(conn);
            match r {
                Ok(r) => r,
                Err(_) => Err(DbError::Invalid("read task panicked".into())),
            }
        })
        .await
        .map_err(|e| DbError::Invalid(format!("read task failed: {e}")))?;
        guard.done();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full disk, an I/O error and a corrupt file are told as what they are, not as "node
    /// unavailable".
    #[test]
    fn storage_failures_are_named() {
        use super::super::ingest::IngestError;
        let sql = |code: i32| IngestError::Db(DbError::Sqlite(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)));
        assert!(storage_problem(&sql(rusqlite::ffi::SQLITE_FULL)).unwrap().contains("disk of the data directory is full"));
        assert!(storage_problem(&sql(rusqlite::ffi::SQLITE_IOERR)).unwrap().contains("I/O error"));
        for c in [rusqlite::ffi::SQLITE_CORRUPT, rusqlite::ffi::SQLITE_NOTADB] {
            let m = storage_problem(&sql(c)).unwrap();
            assert!(m.contains("corrupt") && m.contains("fast start"), "{m}");
        }
        assert!(storage_problem(&sql(rusqlite::ffi::SQLITE_READONLY)).unwrap().contains("permissions"));
        assert!(storage_problem(&IngestError::Db(DbError::Io(std::io::Error::other("x")))).is_some());
        // Not the disk's fault.
        assert_eq!(storage_problem(&sql(rusqlite::ffi::SQLITE_CONSTRAINT)), None);
        assert_eq!(storage_problem(&IngestError::NoCursor), None);
    }

    fn programs(c: &Connection) -> Vec<(u8, String, i64)> {
        let mut st = c
            .prepare("SELECT t.h, p.program, p.n FROM holding_programs p JOIN ids t ON t.id = p.token_id ORDER BY t.h, p.program")
            .unwrap();
        st.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?[0], r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
    }

    fn holding(c: &Connection, txid: u8, token: u8, program: &str) {
        super::super::ids::intern(c, &[token; 32]).unwrap();
        super::super::ids::intern(c, &[9u8; 32]).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa)              VALUES (?1, 0, (SELECT id FROM ids WHERE h = ?2), ?3, 1, (SELECT id FROM ids WHERE h = ?4), 0, 5, 1, x'08', 'owned', 1, 1)",
            rusqlite::params![vec![txid; 32], vec![token; 32], program, vec![9u8; 32]],
        )
        .unwrap();
    }

    /// `holding_programs` is the (token, program) set of the holdings with a count.
    ///
    /// The triggers keep it through inserts (an ignored one counts nothing), deletes and a change
    /// of the key columns. A database without it gets it counted once.
    #[test]
    fn the_programs_of_the_holdings_are_counted_by_triggers_and_backfilled_once() {
        let c = open_memory("testnet-10").unwrap();
        holding(&c, 1, 1, "KCC20Ref_8x8");
        holding(&c, 2, 1, "KCC20Ref_8x8");
        holding(&c, 2, 1, "KCC20Ref_8x8"); // ignored: the same outpoint
        holding(&c, 3, 1, "KronToken2433");
        holding(&c, 4, 2, "KCC20Ref_8x8");
        assert_eq!(programs(&c), [(1, "KCC20Ref_8x8".into(), 2), (1, "KronToken2433".into(), 1), (2, "KCC20Ref_8x8".into(), 1)]);
        c.execute("DELETE FROM token_holdings WHERE txid = ?1", [vec![3u8; 32]]).unwrap();
        c.execute("UPDATE token_holdings SET spent_block = 2 WHERE txid = ?1", [vec![1u8; 32]]).unwrap();
        assert_eq!(
            programs(&c),
            [(1, "KCC20Ref_8x8".into(), 2), (2, "KCC20Ref_8x8".into(), 1)],
            "a spend changes nothing, the last of a program takes its row"
        );
        c.execute(
            "UPDATE token_holdings SET token_id = (SELECT id FROM ids WHERE h = ?1) WHERE txid = ?2",
            rusqlite::params![vec![2u8; 32], vec![1u8; 32]],
        )
        .unwrap();
        assert_eq!(programs(&c), [(1, "KCC20Ref_8x8".into(), 1), (2, "KCC20Ref_8x8".into(), 2)]);
        // A database that never had the table is counted at the next open, once.
        let before = programs(&c);
        c.execute_batch("DELETE FROM holding_programs; DELETE FROM meta WHERE k = 'holding_programs';").unwrap();
        assert_eq!(backfill_holding_programs(&c).unwrap(), (2, 3));
        assert_eq!(programs(&c), before);
        assert_eq!(backfill_holding_programs(&c).unwrap(), (0, 0));
        // The reads: the skip-scan of a token's programs.
        assert_eq!(crate::indexer::reads::proven_programs(&c, &[1u8; 32]).unwrap().len(), 1);
    }

    #[test]
    fn meta_roundtrip_and_network_guard() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        {
            let c = open_writer(&p, "testnet-10").unwrap();
            meta_set(&c, "a", "b").unwrap();
            assert_eq!(meta_get(&c, "a").unwrap().as_deref(), Some("b"));
        }
        assert!(matches!(open_writer(&p, "mainnet"), Err(DbError::NetworkMismatch { .. })));
        let c = open_writer(&p, "testnet-10").unwrap();
        assert_eq!(meta_get(&c, "a").unwrap().as_deref(), Some("b"));
    }

    /// A store of a newer schema, or of another network, is refused before this build's schema and
    /// migrations run, so nothing is created or rewritten in it.
    #[test]
    fn a_newer_or_foreign_store_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        {
            let c = open_writer(&p, "testnet-10").unwrap();
            // What a newer build's store lacks of this one's: an index this build's schema creates.
            c.execute_batch("DROP INDEX orders_genesis_page").unwrap();
        }
        let index = |p: &Path| {
            Connection::open(p)
                .unwrap()
                .prepare("SELECT 1 FROM sqlite_master WHERE name = 'orders_genesis_page'")
                .unwrap()
                .exists([])
                .unwrap()
        };
        assert!(matches!(open_writer(&p, "mainnet"), Err(DbError::NetworkMismatch { .. })));
        assert!(matches!(open_writer_with(&p, "mainnet", true), Err(DbError::NetworkMismatch { .. })));
        assert!(!index(&p), "nothing was created in a store of another network");
        meta_set(&Connection::open(&p).unwrap(), "schema_version", &(SCHEMA_VERSION + 1).to_string()).unwrap();
        assert!(matches!(open_writer(&p, "testnet-10"), Err(DbError::SchemaTooNew { .. })));
        assert!(matches!(open_writer_with(&p, "testnet-10", true), Err(DbError::SchemaTooNew { .. })));
        assert!(!index(&p), "nothing was created in a newer store");
        let c = Connection::open(&p).unwrap();
        assert_eq!(meta_get(&c, "schema_version").unwrap(), Some((SCHEMA_VERSION + 1).to_string()));
        // The lean rewrite did not run either.
        assert_eq!(c.query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    /// A database written before protocol v3 is refused by the writer and the reader.
    ///
    /// Its `orders` table has the `lot_units` column of the older layouts. The refusal comes before
    /// anything touches the file, and names the way out (`kobd db replay`).
    #[test]
    fn a_pre_v3_database_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        {
            // The shape a schema-4 build left behind (only what the check and the reader look at).
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL) WITHOUT ROWID; \
                 INSERT INTO meta VALUES ('schema_version', '4'), ('network', 'testnet-10'); \
                 CREATE TABLE orders (covenant_id BLOB PRIMARY KEY, unit INTEGER, lot_units INTEGER);",
            )
            .unwrap();
        }
        for e in [open_writer(&p, "testnet-10").unwrap_err(), open_reader(&p, "testnet-10").unwrap_err()] {
            assert!(matches!(e, DbError::PreV3(_)), "{e}");
            let m = e.to_string();
            assert!(m.contains("kobd db replay") && m.contains("move it aside"), "{m}");
        }
        // Nothing was created or altered: no other table, the old version.
        let c = Connection::open(&p).unwrap();
        let tables: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 2);
        assert_eq!(meta_get(&c, "schema_version").unwrap().as_deref(), Some("4"));
        // A database of this build opens.
        let fresh = dir.path().join("y.sqlite3");
        drop(open_writer(&fresh, "testnet-10").unwrap());
        assert!(open_reader(&fresh, "testnet-10").is_ok());
    }

    #[test]
    fn a_schema_2_database_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        {
            let c = open_writer(&p, "testnet-10").unwrap();
            // What a schema-2 build left behind.
            c.execute_batch("CREATE TABLE receipts (txid BLOB NOT NULL, idx INTEGER NOT NULL, PRIMARY KEY (txid, idx))").unwrap();
            c.execute_batch("CREATE INDEX receipts_created ON receipts (txid); INSERT INTO receipts VALUES (x'00', 0)").unwrap();
            meta_set(&c, "schema_version", "2").unwrap();
        }
        let has_receipts =
            |c: &Connection| c.prepare("SELECT 1 FROM sqlite_master WHERE name LIKE 'receipts%'").unwrap().exists([]).unwrap();
        // A store of an older schema is refused untouched (`kobd db migrate` rebuilds it), so
        // nothing is dropped from it.
        assert!(matches!(open_writer(&p, "testnet-10"), Err(DbError::OldLayout(_, 2))));
        assert!(has_receipts(&Connection::open(&p).unwrap()), "the refused store is left as it was");
    }

    #[tokio::test]
    async fn read_pool_reads_committed_data() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        let w = open_writer(&p, "testnet-10").unwrap();
        meta_set(&w, "k", "v1").unwrap();
        let pool = ReadPool::open(&p, 2).unwrap();
        let v = pool.with(|c| Ok(meta_get(c, "k")?)).await.unwrap();
        assert_eq!(v.as_deref(), Some("v1"));
        meta_set(&w, "k", "v2").unwrap();
        let v = pool.with(|c| Ok(meta_get(c, "k")?)).await.unwrap();
        assert_eq!(v.as_deref(), Some("v2"));
    }
}
