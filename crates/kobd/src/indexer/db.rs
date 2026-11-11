//! SQLite plumbing: opening the database, the single writer, and the read pool the API uses.
//!
//! Why SQLite (WAL) rather than RocksDB or a hand-rolled file: the indexer needs one atomic commit
//! of (cursor + order delta + undo stamps + raw-log position), ad-hoc queries for the read API
//! (books by token and price, orders by maker, recent fills) and operational transparency (an
//! operator can open the file with the `sqlite3` shell, back it up online with `VACUUM INTO`, and
//! diff two instances). It is one small C dependency, has readers that never block the writer in
//! WAL mode, and the data volume (KOB orders only, not the chain) is tiny.

use super::packed::{sql_genesis, sql_order_ids, sql_order_ids_more};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// 3 (protocol v2.6): the `receipts` table is gone (the trade receipt is retired; `kobd db migrate` does not carry it over). 5 (protocol v3): quantities
/// in token base units and prices per whole token (`orders.scale`, `min_fill`, `tip`, `budget_rate`, `initial_amount`, ...). A
/// database of schema 4 or older cannot be migrated ([`refuse_pre_v3`]): `kobd db replay` rebuilds it from the record log.
/// 6 (2026-11): the state spans of `order_utxos` and `token_holdings` are stored packed ([`super::packed`]): a build of schema 5
/// misreads them, so it refuses a schema 6 store. 7: `orders.genesis_state` and `order_events.detail` are stored deflated against frozen
/// dictionaries, and `holding_programs` replaces the `(token, program)` index of the holdings: a build of schema 6 misreads the former.
/// 8: the 32-byte ids the big tables repeated are integers (`orders.id`, `ids`; [`super::ids`]): the tables have other columns, so no
/// build reads a store of another schema than its own, and this one refuses a store of schema 8 or older until `kobd db migrate` has rebuilt
/// it ([`refuse_old_layout`]). 9: an order names its maker, template hashes, extension commitment and quote token by id too, the page indexes
/// of the orders and holdings end at the block ([`super::ids`], schema.sql). 10: a fill event keeps its fee and the script that funded
/// it (`order_events.fee`, `executor_id`, `scripts`), and the activity totals are kept by triggers (`activity`, `actors`;
/// [`super::activity`]): `kobd db migrate` computes the fees of the fills a store holds (their executors are unknown).
pub const SCHEMA_VERSION: i64 = 10;
const SCHEMA_SQL: &str = include_str!("schema.sql");

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

pub type DbResult<T> = Result<T, DbError>;

/// What a write-path failure says about the storage itself (audit 2026-11-11 M-5): a full disk, an I/O error, a corrupt or
/// foreign database file, a read-only file. `None`: something else (a bug, a constraint), not the disk's fault.
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
        // the write path re-reads the rows of the orders it touches: 64 MiB of page cache (the default is 2 MiB)
        conn.pragma_update(None, "cache_size", -65_536)?;
        // The WAL file keeps its largest size: a checkpoint resets it only while no read holds an older snapshot, and reads
        // that overlap without a gap (the history-wide reads of earlier builds) grew it to 785 MB on the TN10 soak. Once a
        // reset happens it is cut back to this (docs/ops/executor.md, B 5.5).
        conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024)?;
    }
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    // `kob_pack`, `kob_ustate`, `kob_hpack`, `kob_hstate`: how queries read and write the packed state spans
    super::packed::register(conn)?;
    // the row counters' triggers count an INSERT OR REPLACE of `order_state` once (schema.sql, `counters`): a REPLACE must
    // not fire the delete trigger as well
    conn.pragma_update(None, "recursive_triggers", "OFF")?;
    // Every statement the write path and the reads run is prepared once and kept: about 60 distinct ones per KOB transaction
    // cycle. With rusqlite's default of 16 the cache evicted them in turn and nearly every execution re-prepared its
    // statement (the parse and plan cost more than the statement itself: about 40 % of the per-transaction time on a
    // KOB-heavy chain, docs/ops/executor.md *Processing capacity*).
    conn.set_prepared_statement_cache_capacity(256);
    Ok(())
}

/// Open (creating if needed) the writer connection and apply the schema.
pub fn open_writer(path: &Path, network: &str) -> DbResult<Connection> {
    open_writer_with(path, network, false)
}

/// [`open_writer`]; with `lean` the store gives freed pages back to the file (`auto_vacuum = INCREMENTAL`, [`super::lean`]):
/// set before the schema on a new store, and an older store is rewritten once.
pub fn open_writer_with(path: &Path, network: &str, lean: bool) -> DbResult<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    pragmas(&conn, true)?;
    // before anything is created or altered: an older database is refused as it is
    refuse_pre_v3(&conn, path)?;
    refuse_old_layout(&conn, path)?;
    if lean {
        // the WAL of a lean store is kept small too (the default limit is 64 MiB)
        conn.pragma_update(None, "journal_size_limit", 8 * 1024 * 1024)?;
        if super::lean::ensure_incremental(&conn)? {
            // the rewrite went through the WAL: cut it back
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            tracing::info!(file = %path.display(), "lean storage: the store was rewritten once so that it can give freed pages back");
        }
    }
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute_batch(crate::logo_reports::SCHEMA)?;
    if lean {
        // the indexes only the pruning reads (a few seconds once, on a store of a million orders)
        super::lean::ensure_indexes(&conn)?;
    }
    migrate(&conn)?;
    init_meta(&conn, network)?;
    super::candles::configure(&conn, lean)?;
    Ok(conn)
}

/// A store of schema 8 or older has the tables of the earlier layout (covenant ids and token ids in every row): opening it with this build
/// would build the new indexes on columns that are not there. Refused untouched, with the way out (`kobd db migrate`); checked before the schema is applied.
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

/// A database written by a build before protocol v3 (schema 4 or older) has the column `orders.lot_units` of the older
/// templates' layouts. It cannot be migrated: every order in it is of a template this build does not pin (never listed)
/// and every stored quantity counts in the old layouts. The record log rebuilds the database (`kobd db replay`: reveals of
/// templates this build does not pin are dropped, frames it cannot decode skipped). Checked BEFORE the schema is applied,
/// so the refused file is left exactly as it was.
pub fn refuse_pre_v3(conn: &Connection, path: &Path) -> DbResult<()> {
    let old: bool = conn.prepare("SELECT 1 FROM pragma_table_info('orders') WHERE name = 'lot_units'")?.exists([])?;
    if old {
        return Err(DbError::PreV3(path.display().to_string()));
    }
    Ok(())
}

/// What a store of this build is brought up to date with at every open: the row counters and the program counts (counted once, kept by
/// triggers), and the idempotent backfills below, which repair rows an earlier build wrote (a no-op on rows this build wrote; `index
/// migrate` carries such rows over, and they are repaired by the first open of the new store). Schema 8 and older are refused before this
/// runs ([`refuse_old_layout`]); the layout of the tables is what `schema.sql` says.
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
    // KCC-23 proofs an earlier build stored once per covenant id: shared per transaction, in short transactions of 200 rows
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
    let unknown = backfill_unknown_pair_extensions(conn)?;
    if unknown > 0 {
        tracing::info!(unknown, "token events: an unknown pair-token commitment an earlier build recorded as zero is now none");
    }
    Ok(())
}

/// The row counters (`counters`, schema.sql) of a database that has not had them: counted from the tables once, in one
/// transaction, after which the triggers keep them. A no-op (false) once done (`meta.counters`); a fresh database counts its
/// empty tables.
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

/// The activity totals (`activity`, `actors`, schema.sql) of a database that has not had them: counted from the events and orders once,
/// in one transaction, after which the triggers keep them. A no-op (false) once done (`meta.activity`); a fresh database counts its empty
/// tables. The minute buckets are counted from the events (one pass), the hour buckets from the minutes.
pub fn backfill_activity(conn: &Connection) -> DbResult<bool> {
    if meta_get(conn, "activity")?.is_some() {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DELETE FROM activity; DELETE FROM actors;          INSERT INTO activity (iv, t, fills, fees, orders)            SELECT 60000, ts - ts % 60000, SUM(kind = 'fill'), SUM(CASE WHEN kind = 'fill' THEN COALESCE(fee, 0) ELSE 0 END), SUM(kind = 'create')            FROM order_events WHERE kind IN ('fill', 'create') GROUP BY ts - ts % 60000;          INSERT INTO activity (iv, t, fills, fees, orders)            SELECT 3600000, t - t % 3600000, SUM(fills), SUM(fees), SUM(orders) FROM activity WHERE iv = 60000 GROUP BY t - t % 3600000;          INSERT INTO actors (kind, actor_id, last_block)            SELECT 1, maker_id, MAX(genesis_block) FROM orders WHERE maker_id IS NOT NULL GROUP BY maker_id;          INSERT INTO actors (kind, actor_id, last_block)            SELECT 2, executor_id, MAX(block_seq) FROM order_events WHERE kind = 'fill' AND executor_id IS NOT NULL GROUP BY executor_id;",
    )?;
    meta_set(&tx, "activity", "1")?;
    tx.commit()?;
    Ok(true)
}

/// The (token, program) counts of `holding_programs` (schema.sql) of a database that has not had them: counted from the holdings once, in
/// one transaction, after which the triggers keep them. A no-op (`(0, 0)`) once done (`meta.holding_programs`); a fresh database counts
/// its empty table. Returns (rows of the table, holdings counted).
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

/// The token identities the order `(contract, token, program, commitment, genesis state)` placed, as the processor records
/// them now ([`super::processor::placement_identities`]).
type Identity = (Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);
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

/// An earlier build recorded the token identity of a pair order's token whose commitment its state does not carry (a
/// family code other than KCC-20) with the all-zero commitment, so the token list could not tell it from the real zero
/// commitment of the standard fixed-supply class and reported none for every KCC-20 token of that class. Re-derive each
/// zero `seen` row from the orders: a row no order of its transaction names the zero commitment for becomes the identity
/// without a commitment (dropped when that one is already recorded), and the first order that does name the zero commitment
/// gets the zero row the processor skipped (the identity looked seen). Idempotent and a no-op on a database written by this
/// build; `kobd db replay` records the same identities. Returns the rows changed.
pub fn backfill_unknown_pair_extensions(conn: &Connection) -> DbResult<usize> {
    let zero = [0u8; 32];
    let rows: Vec<ZeroRow> = conn
        .prepare("SELECT id, token_cov_id, tpl_hash, txid FROM token_events WHERE kind = 'seen' AND ext_commit = ?1 ORDER BY id")?
        .query_map([&zero[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(0);
    }
    // (genesis txid, genesis block, genesis daa, identities) of the orders that name one of these tokens (of one transaction:
    // `txid`)
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
        // the orders of the row's own transaction say whether it stands: only their states are decoded (this runs at every
        // start-up; decoding every order of the token for every row took 12 s on a store of 300,000 orders)
        if names(&tok, &tpl, Some(&txid))?.iter().any(|(t, _, _, named)| *named && t.as_deref() == Some(&txid[..])) {
            continue;
        }
        let orders = names(&tok, &tpl, None)?;
        changed += 1;
        conn.execute("UPDATE token_events SET ext_commit = NULL WHERE id = ?1", [id])?;
        // the identity without a commitment once: its first sighting stays
        conn.execute(
            "DELETE FROM token_events WHERE kind = 'seen' AND token_cov_id = ?1 AND tpl_hash IS ?2 AND ext_commit IS NULL \
             AND id > (SELECT MIN(id) FROM token_events WHERE kind = 'seen' AND token_cov_id = ?1 AND tpl_hash IS ?2 AND ext_commit IS NULL)",
            rusqlite::params![tok, tpl],
        )?;
        // the zero commitment's first real sighting, which the processor skipped (the identity looked seen)
        if let Some((Some(t), block, daa, _)) = orders.iter().find(|o| o.3 && o.0.is_some()) {
            conn.execute(
                "INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) VALUES (?1,?2,?3,'seen',?4,?5,?6)",
                rusqlite::params![tok, tpl, &zero[..], t, block, daa],
            )?;
        }
    }
    Ok(changed)
}

/// An earlier build listed every if-done entry in its KAS book at its limit, unarmed stop entries too (which nothing fills
/// before their trigger): re-derive the live entries' state, which now keeps an unarmed stop entry out of the books
/// (`processor::refresh_order_state`). Idempotent; returns the entries taken out.
pub fn backfill_stop_entry_books(conn: &Connection) -> DbResult<usize> {
    let ids: Vec<Vec<u8>> = conn
        .prepare(
            "SELECT o.covenant_id FROM order_state s CROSS JOIN orders o ON o.id = s.order_id              WHERE o.in_book = 1 AND o.contract IN ('KobIfdAsk','KobIfdBid','KobIfdAskKron','KobIfdBidKron')                AND s.status IN ('open','partial')",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = 0;
    for id in ids {
        let Ok(cov) = <[u8; 32]>::try_from(id.as_slice()) else { continue };
        super::processor::refresh_order_state(conn, &crate::hex::Hash32(cov))?;
        let in_book: i64 = conn.query_row("SELECT in_book FROM orders WHERE covenant_id = ?1", [&cov[..]], |r| r.get(0))?;
        out += usize::from(in_book == 0);
    }
    Ok(out)
}

/// If-done exits booked by a build before the fix were stored without an extension commitment (`orders.ext_commit` NULL), so
/// their views could not rebuild the custody token state (cancel, cancel-replace and refund failed) and the matcher rebuilt it
/// with a zero commitment. An exit's commitment is its entry's (the entry covenant writes it into the exit's custody; a
/// `KobCondBid` exit also carries it in its state), which the database already holds: copy it. Idempotent and a no-op on a
/// database written by this build; `kobd db replay` gives the same rows. Also drops the `seen` token registry events those exits
/// recorded for the bogus (token, program, no commitment) identity, which a replay does not record (the entry saw the identity
/// first), unless some order of that token still has no commitment. Returns (exits fixed, events dropped).
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

/// In-memory database for tests.
/// Open an existing database read-only (no schema change, no lock: works next to the writer, e.g. `kobd db export-orders`
/// while `run` is running). Refuses a database of another network or of another schema version (a writer of this build
/// migrates it first).
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
                // `migrate` has brought the tables up to date
                meta_set(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
            }
        }
    }
    Ok(())
}

pub fn meta_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT v FROM meta WHERE k = ?1", [key], |r| r.get(0)).optional()
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", [key, value])?;
    Ok(())
}

/// Longest one pooled read may run before SQLite is told to interrupt it.
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A pool of read-only connections. WAL lets them read while the follower commits.
///
/// Two lanes: [`ReadPool::with`] for every read, and [`ReadPool::with_light`], a connection of its own for the reads that
/// cost a few index lookups (the health counters, one order), so they never queue behind the heavy ones (on the TN10 soak
/// copy the health counters, 0.06 ms to run, waited about 1 s behind four candle reads and 3 to 4 s behind sixteen).
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

/// What a caller that went away (its future dropped: the API guard's timeout, a client disconnect) leaves its read: the
/// read is not started, or the statement it is running is interrupted (`sqlite3_interrupt`), so it gives its connection back.
#[derive(Default)]
struct Cancel {
    gone: std::sync::atomic::AtomicBool,
    /// The connection the read runs on, while it does (cleared before the connection goes back to the pool, so a late
    /// interrupt never reaches the next read).
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
    /// Open `size` read-only connections to an existing database file, and the light lane's one.
    pub fn open(path: &Path, size: usize) -> DbResult<Self> {
        Ok(ReadPool { inner: Arc::new(PoolInner { heavy: open_lane(path, size.max(1))?, light: open_lane(path, 1)? }) })
    }

    /// Run a read on a pooled connection on the blocking thread pool.
    pub async fn with<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        self.run(false, f).await
    }

    /// [`ReadPool::with`] on the light lane: only for reads bounded by a few index lookups, whatever the request asks.
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
        // The permit travels INTO the blocking task and is released only when the task ends: a caller that
        // is cancelled (request timeout, client disconnect) must not hand its permit to the next request while the read
        // still holds its pooled connection, or the pool would run dry and the bound on concurrent reads would not hold.
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
            // the caller went away while the read waited for the blocking pool: it is not run
            let r = if cancel.gone.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(Err(DbError::Invalid("read cancelled: the caller went away".into())))
            } else {
                // A read that outlives `READ_TIMEOUT` is interrupted: a hostile request should not hold a pooled connection
                // for minutes.
                let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
                let handle = conn.get_interrupt_handle();
                let watchdog = std::thread::Builder::new().name("kob-read-watchdog".into()).spawn(move || {
                    if done_rx.recv_timeout(READ_TIMEOUT) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                        handle.interrupt();
                    }
                });
                // the connection goes back even if the read panics
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

    /// Audit 2026-11-11 M-5: a full disk, an I/O error and a corrupt file are told as what they are (not as "node unavailable").
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
        // not the disk's fault
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

    /// `holding_programs` is the (token, program) set of the holdings with a count: the triggers keep it through inserts (an ignored one
    /// counts nothing), deletes, a change of the key columns, and a database without it gets it counted once.
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
        // a database that never had the table: counted at the next open, once
        let before = programs(&c);
        c.execute_batch("DELETE FROM holding_programs; DELETE FROM meta WHERE k = 'holding_programs';").unwrap();
        assert_eq!(backfill_holding_programs(&c).unwrap(), (2, 3));
        assert_eq!(programs(&c), before);
        assert_eq!(backfill_holding_programs(&c).unwrap(), (0, 0));
        // the reads: the skip-scan of a token's programs
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

    /// A database of a build before protocol v3 (its `orders` table has the `lot_units` column of the older layouts) is
    /// refused by the writer and the reader before anything touches it, with the way out (`kobd db replay`).
    #[test]
    fn a_pre_v3_database_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        {
            // the shape a schema-4 build left behind (only what the check and the reader look at)
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
        // nothing was created or altered: no other table, the old version
        let c = Connection::open(&p).unwrap();
        let tables: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 2);
        assert_eq!(meta_get(&c, "schema_version").unwrap().as_deref(), Some("4"));
        // a database of this build opens
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
            // what a v2.4 build left behind
            c.execute_batch("CREATE TABLE receipts (txid BLOB NOT NULL, idx INTEGER NOT NULL, PRIMARY KEY (txid, idx))").unwrap();
            c.execute_batch("CREATE INDEX receipts_created ON receipts (txid); INSERT INTO receipts VALUES (x'00', 0)").unwrap();
            meta_set(&c, "schema_version", "2").unwrap();
        }
        let has_receipts =
            |c: &Connection| c.prepare("SELECT 1 FROM sqlite_master WHERE name LIKE 'receipts%'").unwrap().exists([]).unwrap();
        // since schema 9 a store of an older schema is refused untouched (`kobd db migrate` rebuilds it): nothing is dropped from it
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
