//! SQLite plumbing: opening the database, the single writer, and the read pool the API uses.
//!
//! Why SQLite (WAL) rather than RocksDB or a hand-rolled file: the indexer needs one atomic commit
//! of (cursor + order delta + undo stamps + raw-log position), ad-hoc queries for the read API
//! (books by token and price, orders by maker, recent fills) and operational transparency (an
//! operator can open the file with the `sqlite3` shell, back it up online with `VACUUM INTO`, and
//! diff two instances). It is one small C dependency, has readers that never block the writer in
//! WAL mode, and the data volume (KOB orders only, not the chain) is tiny.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// 3 (protocol v2.6): the `receipts` table is gone (the trade receipt is retired; `migrate` drops it). 5 (protocol v3): quantities
/// in token base units and prices per whole token (`orders.scale`, `min_fill`, `tip`, `budget_rate`, `initial_amount`, ...). A
/// database of schema 4 or older cannot be migrated ([`refuse_pre_v3`]): `index replay` rebuilds it from the record log.
pub const SCHEMA_VERSION: i64 = 5;
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
         pin and its quantities are not base units) and cannot be migrated: move it aside (or delete it) and run `kob-executor index replay`, \
         which rebuilds it from the record log"
    )]
    PreV3(String),
    #[error("{0}")]
    Invalid(String),
}

pub type DbResult<T> = Result<T, DbError>;

fn pragmas(conn: &Connection, writer: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    if writer {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // FULL: a committed batch survives power loss, which the raw-log ordering relies on.
        conn.pragma_update(None, "synchronous", "FULL")?;
        // the write path re-reads the rows of the orders it touches: 64 MiB of page cache (the default is 2 MiB)
        conn.pragma_update(None, "cache_size", -65_536)?;
    }
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    // Every statement the write path and the reads run is prepared once and kept: about 60 distinct ones per KOB transaction
    // cycle. With rusqlite's default of 16 the cache evicted them in turn and nearly every execution re-prepared its
    // statement (the parse and plan cost more than the statement itself: about 40 % of the per-transaction time on a
    // KOB-heavy chain, docs/ops/executor.md *Processing capacity*).
    conn.set_prepared_statement_cache_capacity(256);
    Ok(())
}

/// Open (creating if needed) the writer connection and apply the schema.
pub fn open_writer(path: &Path, network: &str) -> DbResult<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    pragmas(&conn, true)?;
    // before anything is created or altered: an older database is refused as it is
    refuse_pre_v3(&conn, path)?;
    conn.execute_batch(SCHEMA_SQL)?;
    migrate(&conn)?;
    init_meta(&conn, network)?;
    Ok(conn)
}

/// A database written by a build before protocol v3 (schema 4 or older) has the column `orders.lot_units` of the older
/// templates' layouts. It cannot be migrated: every order in it is of a template this build does not pin (never listed)
/// and every stored quantity counts in the old layouts. The record log rebuilds the database (`index replay`: reveals of
/// templates this build does not pin are dropped, frames it cannot decode skipped). Checked BEFORE the schema is applied,
/// so the refused file is left exactly as it was.
pub fn refuse_pre_v3(conn: &Connection, path: &Path) -> DbResult<()> {
    let old: bool = conn.prepare("SELECT 1 FROM pragma_table_info('orders') WHERE name = 'lot_units'")?.exists([])?;
    if old {
        return Err(DbError::PreV3(path.display().to_string()));
    }
    Ok(())
}

/// Migrations of a database created by an earlier build of schema 5: the retired `receipts` table and its indexes dropped
/// (schema 3, protocol v2.6; a no-op since), the cross limits' token B column, and the idempotent backfills below. (Schema 4
/// and older are refused before this runs: [`refuse_pre_v3`].)
fn migrate(conn: &Connection) -> DbResult<()> {
    conn.execute_batch("DROP INDEX IF EXISTS receipts_created; DROP INDEX IF EXISTS receipts_spent; DROP TABLE IF EXISTS receipts;")?;
    let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('orders') WHERE name = 'quote_cov_id'")?.exists([])?;
    if !has {
        // token B of a cross limit; a database written before cross limits existed has none
        conn.execute_batch("ALTER TABLE orders ADD COLUMN quote_cov_id BLOB")?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS orders_quote ON orders (quote_cov_id)")?;
    let (exits, events) = backfill_exit_extensions(conn)?;
    if exits > 0 || events > 0 {
        tracing::info!(exits, events, "backfilled the extension commitment of if-done exits booked by an earlier build");
    }
    let stops = backfill_stop_entry_books(conn)?;
    if stops > 0 {
        tracing::info!(stops, "took the unarmed stop entries an earlier build listed as resting liquidity out of the books");
    }
    Ok(())
}

/// An earlier build listed every if-done entry in its KAS book at its limit, unarmed stop entries too (which nothing fills
/// before their trigger): re-derive the live entries' state, which now keeps an unarmed stop entry out of the books
/// (`processor::refresh_order_state`). Idempotent; returns the entries taken out.
pub fn backfill_stop_entry_books(conn: &Connection) -> DbResult<usize> {
    let ids: Vec<Vec<u8>> = conn
        .prepare(
            "SELECT o.covenant_id FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id              WHERE o.in_book = 1 AND o.contract IN ('KobIfdAsk','KobIfdBid','KobIfdAskKron','KobIfdBidKron')                AND s.status IN ('open','partial')",
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
/// database written by this build; `index replay` gives the same rows. Also drops the `seen` token registry events those exits
/// recorded for the bogus (token, program, no commitment) identity, which a replay does not record (the entry saw the identity
/// first), unless some order of that token still has no commitment. Returns (exits fixed, events dropped).
pub fn backfill_exit_extensions(conn: &Connection) -> DbResult<(usize, usize)> {
    let exits = conn.execute(
        "UPDATE orders SET ext_commit = (SELECT p.ext_commit FROM orders p WHERE p.covenant_id = orders.parent) \
         WHERE ext_commit IS NULL AND parent IS NOT NULL \
           AND EXISTS (SELECT 1 FROM orders p WHERE p.covenant_id = orders.parent AND p.ext_commit IS NOT NULL)",
        [],
    )?;
    let events = conn.execute(
        "DELETE FROM token_events WHERE kind = 'seen' AND ext_commit IS NULL \
           AND EXISTS (SELECT 1 FROM orders x WHERE x.genesis_txid = token_events.txid AND x.parent IS NOT NULL \
                       AND x.token_cov_id = token_events.token_cov_id AND x.token_tpl_hash IS token_events.tpl_hash \
                       AND x.ext_commit IS NOT NULL) \
           AND NOT EXISTS (SELECT 1 FROM orders o WHERE o.token_cov_id = token_events.token_cov_id \
                           AND o.token_tpl_hash IS token_events.tpl_hash AND o.ext_commit IS NULL)",
        [],
    )?;
    Ok((exits, events))
}

/// In-memory database for tests.
/// Open an existing database read-only (no schema change, no lock: works next to the writer, e.g. `index export-orders`
/// while `run` is running). Refuses a database of another network or of another schema version (a writer of this build
/// migrates it first).
pub fn open_reader(path: &Path, network: &str) -> DbResult<Connection> {
    if !path.exists() {
        return Err(DbError::Invalid(format!("{} does not exist", path.display())));
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    pragmas(&conn, false)?;
    refuse_pre_v3(&conn, path)?;
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
    conn.execute_batch(SCHEMA_SQL)?;
    migrate(&conn)?;
    init_meta(&conn, network)?;
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
#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    conns: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

impl ReadPool {
    /// Open `size` read-only connections to an existing database file.
    pub fn open(path: &Path, size: usize) -> DbResult<Self> {
        let size = size.max(1);
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            pragmas(&c, false)?;
            conns.push(c);
        }
        Ok(ReadPool { inner: Arc::new(PoolInner { conns: Mutex::new(conns), permits: Arc::new(Semaphore::new(size)) }) })
    }

    /// Run a read on a pooled connection on the blocking thread pool.
    pub async fn with<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        // The permit travels INTO the blocking task and is released only when the task ends: a caller that
        // is cancelled (request timeout, client disconnect) must not hand its permit to the next request while the read
        // still holds its pooled connection, or the pool would run dry and the bound on concurrent reads would not hold.
        let permit = self.inner.permits.clone().acquire_owned().await.map_err(|_| DbError::Invalid("read pool closed".into()))?;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = inner.conns.lock().unwrap_or_else(|e| e.into_inner()).pop();
            let Some(conn) = conn else { return Err(DbError::Invalid("read pool exhausted".into())) };
            // A read that outlives `READ_TIMEOUT` is interrupted: the API guard drops the caller's future at its own
            // timeout but cannot stop a running statement, and a hostile request should not hold a pooled connection for minutes.
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
            inner.conns.lock().unwrap_or_else(|e| e.into_inner()).push(conn);
            match r {
                Ok(r) => r,
                Err(_) => Err(DbError::Invalid("read task panicked".into())),
            }
        })
        .await
        .map_err(|e| DbError::Invalid(format!("read task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// refused by the writer and the reader before anything touches it, with the way out (`index replay`).
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
            assert!(m.contains("index replay") && m.contains("move it aside"), "{m}");
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
    fn a_schema_2_database_loses_its_receipts_table() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sqlite3");
        {
            let c = open_writer(&p, "testnet-10").unwrap();
            // what a v2.4 build left behind
            c.execute_batch("CREATE TABLE receipts (txid BLOB NOT NULL, idx INTEGER NOT NULL, PRIMARY KEY (txid, idx))").unwrap();
            c.execute_batch("CREATE INDEX receipts_created ON receipts (txid); INSERT INTO receipts VALUES (x'00', 0)").unwrap();
            meta_set(&c, "schema_version", "2").unwrap();
        }
        let c = open_writer(&p, "testnet-10").unwrap();
        let has: bool = c.prepare("SELECT 1 FROM sqlite_master WHERE name LIKE 'receipts%'").unwrap().exists([]).unwrap();
        assert!(!has, "the receipts table and its indexes are dropped");
        assert_eq!(meta_get(&c, "schema_version").unwrap(), Some(SCHEMA_VERSION.to_string()));
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
