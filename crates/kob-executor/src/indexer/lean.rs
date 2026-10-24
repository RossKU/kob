//! Lean storage (`lean = true`): an executor needs the live book, not the history. The store keeps
//!
//! * every open or partly filled order with all its rows (events, UTXOs, custody, strays, flags): the matcher, the keepers,
//!   the maintenance jobs and the read API's live book read exactly these;
//! * the reorg window: nothing stamped with a chain block of the window is ever deleted, so a revert is exact;
//! * `lean_retention_days` (30 by default) of closed orders (filled, cancelled, refunded, killed), with their fills and
//!   events, so the API still answers for what happened lately;
//!
//! and drops the rest in small periodic batches ([`prune`], run by the follower between steps, each batch one short write
//! transaction): closed orders past the retention with everything that belongs to them, spent UTXO rows (orders' and tokens')
//! past the retention, the diagnostics of blocks past the window. A size cap (`lean_max_db_mb`, 500 by default) applies only
//! while the store is larger than it: then the OLDEST closed orders go first, with their fills and events, retention or not,
//! until the store is under the cap (the live book and the reorg window are never touched, so a store whose live book alone
//! exceeds the cap stays as it is). The record log is off, and the file shrinks: the store is
//! created with `auto_vacuum = INCREMENTAL` (an older store is converted once, at the first lean start), and each batch gives
//! the freed pages back ([`release_pages`]).
//!
//! What no read of the executor needs is what goes: candles, 24 h statistics and trade lists are derived from the fill events
//! of the retention window; the history is an archive indexer's job (`lean = false`).
//!
//! What stays whatever the retention: the rows an open order's state is derived from (a partly filled order's fill events sum
//! to its filled amount, `processor::FILLED_AMOUNT_SQL`), an if-done entry that a live exit names as its parent, token
//! events and KCC-23 declarations (tiny), the counters.

use super::db::DbResult;
use rusqlite::{params, Connection};

/// The statuses of an order that is over (`order_state.status`); `open` and `partial` are the live set.
const CLOSED: &str = "('filled','cancelled','refunded','killed','closed')";

/// How much one batch deletes at most (closed orders; rows of the other tables). A batch is one write transaction of a few
/// tens of milliseconds, so the follower and the matcher never wait long on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeanConfig {
    /// Closed orders and spent rows older than this many DAA behind the node stay (never less than the reorg window).
    pub retention_daa: u64,
    /// The store's size cap in bytes (pages in use): above it the oldest closed orders are deleted, retention or not. 0: none.
    pub max_db_bytes: u64,
    /// Closed orders deleted per batch.
    pub orders_per_batch: usize,
    /// Rows deleted per batch from each table that is pruned by age.
    pub rows_per_batch: usize,
    /// Freed pages handed back to the file per batch.
    pub pages_per_batch: usize,
}

impl LeanConfig {
    pub fn new(retention_daa: u64, max_db_bytes: u64) -> Self {
        LeanConfig { retention_daa, max_db_bytes, orders_per_batch: 500, rows_per_batch: 5_000, pages_per_batch: 2_000 }
    }
}

/// What one batch did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LeanReport {
    /// Closed orders deleted (with their events, UTXOs, custody and strays).
    pub orders: usize,
    /// Rows deleted in all, orders' rows included.
    pub rows: usize,
    /// Pages handed back to the file.
    pub pages: usize,
    /// A limit was reached: another batch has more to delete.
    pub more: bool,
    /// The store was above its size cap when the batch began (the oldest closed orders went first).
    pub over_cap: bool,
}

/// The cut-offs of a batch: `final_seq` (rows stamped with a chain block below it are out of the reorg window, so final) and
/// `before_daa` (older than the retention at the node's position, and older than the reorg window at the store's cursor).
/// `None` when nothing can be final yet.
pub fn cutoffs(
    conn: &Connection,
    cursor_daa: u64,
    node_daa: Option<u64>,
    reorg_window_daa: u64,
    retention_daa: u64,
) -> DbResult<Option<(i64, i64)>> {
    let min_seq: Option<i64> = conn.query_row("SELECT MIN(seq) FROM blocks", [], |r| r.get(0))?;
    let Some(final_seq) = min_seq else { return Ok(None) };
    let node_daa = node_daa.unwrap_or(cursor_daa).max(cursor_daa);
    let before = node_daa.saturating_sub(retention_daa.max(reorg_window_daa)).min(cursor_daa.saturating_sub(reorg_window_daa));
    Ok(Some((final_seq, before as i64)))
}

/// Bytes of the pages in use (the file less its free pages: what a size cap counts).
pub fn used_bytes(conn: &Connection) -> DbResult<u64> {
    let (pages, free, size): (i64, i64, i64) = conn.query_row(
        "SELECT (SELECT page_count FROM pragma_page_count), (SELECT freelist_count FROM pragma_freelist_count),          (SELECT page_size FROM pragma_page_size)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok((pages - free).max(0) as u64 * size.max(0) as u64)
}

/// One bounded batch of the lean pruning; see the module documentation for what is deleted and what never is.
pub fn prune(
    conn: &mut Connection,
    cfg: &LeanConfig,
    cursor_daa: u64,
    node_daa: Option<u64>,
    reorg_window_daa: u64,
) -> DbResult<LeanReport> {
    let mut rep = LeanReport::default();
    let Some((final_seq, before)) = cutoffs(conn, cursor_daa, node_daa, reorg_window_daa, cfg.retention_daa)? else {
        return Ok(rep);
    };
    // above the size cap the oldest closed orders go first, whatever their age (only what is final, as always)
    let over_cap = cfg.max_db_bytes > 0 && used_bytes(conn)? > cfg.max_db_bytes;
    rep.over_cap = over_cap;
    let order_before = if over_cap { cursor_daa.saturating_sub(reorg_window_daa) as i64 } else { before };
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS lean_batch (cov BLOB PRIMARY KEY) WITHOUT ROWID; DELETE FROM lean_batch;")?;

    // closed orders past the retention whose exits (if-done children) are not live
    let picked = tx.execute(
        &format!(
            "INSERT INTO lean_batch SELECT s.covenant_id FROM order_state s \
             WHERE s.status IN {CLOSED} AND s.last_daa < ?1 AND s.last_block < ?2 \
             AND NOT EXISTS (SELECT 1 FROM orders c JOIN order_state cs ON cs.covenant_id = c.covenant_id \
                             WHERE c.parent = s.covenant_id AND cs.status IN ('open','partial')) \
             {order} LIMIT ?3",
            order = if over_cap { "ORDER BY s.last_daa" } else { "" }
        ),
        params![order_before, final_seq, cfg.orders_per_batch as i64],
    )?;
    if picked > 0 {
        for sql in [
            "DELETE FROM order_events WHERE covenant_id IN (SELECT cov FROM lean_batch)",
            "DELETE FROM order_amends WHERE covenant_id IN (SELECT cov FROM lean_batch)",
            "DELETE FROM pair_fills WHERE covenant_id IN (SELECT cov FROM lean_batch)",
            "DELETE FROM order_utxos WHERE covenant_id IN (SELECT cov FROM lean_batch)",
            "DELETE FROM token_holdings WHERE owner IN (SELECT cov FROM lean_batch) AND role IN ('custody','stray')",
            "DELETE FROM token_utxos WHERE owner IN (SELECT cov FROM lean_batch)",
            "DELETE FROM order_flags WHERE covenant_id IN (SELECT cov FROM lean_batch)",
            "DELETE FROM order_state WHERE covenant_id IN (SELECT cov FROM lean_batch)",
        ] {
            rep.rows += tx.execute(sql, [])?;
        }
        rep.orders = tx.execute("DELETE FROM orders WHERE covenant_id IN (SELECT cov FROM lean_batch)", [])?;
        rep.rows += rep.orders;
    }
    rep.more = picked >= cfg.orders_per_batch;
    // under the cap again once this batch's pages are free: the next batch (if any) is an ordinary one

    // spent rows past the retention, final (spent in a block out of the window; created before the cut-off, so spent after the
    // retention started is impossible to mistake): a live order's earlier UTXOs, the token UTXOs and holdings that moved
    let limit = cfg.rows_per_batch as i64;
    for sql in [
        "DELETE FROM order_utxos WHERE rowid IN (SELECT rowid FROM order_utxos WHERE spent_block IS NOT NULL AND spent_block < ?1 \
         AND created_daa < ?2 LIMIT ?3)",
        "DELETE FROM token_utxos WHERE rowid IN (SELECT rowid FROM token_utxos WHERE spent_block IS NOT NULL AND spent_block < ?1 \
         AND created_daa < ?2 LIMIT ?3)",
        "DELETE FROM token_holdings WHERE rowid IN (SELECT rowid FROM token_holdings WHERE spent_block IS NOT NULL AND spent_block < ?1 \
         AND created_daa < ?2 LIMIT ?3)",
    ] {
        let n = tx.execute(sql, params![final_seq, before, limit])?;
        rep.rows += n;
        rep.more |= n as i64 >= limit;
    }
    // pair-order fills past the retention (volume statistics of the window only) and the diagnostics of blocks out of the window
    let n = tx.execute(
        "DELETE FROM pair_fills WHERE id IN (SELECT id FROM pair_fills WHERE block_seq < ?1 AND daa < ?2 LIMIT ?3)",
        params![final_seq, before, limit],
    )?;
    rep.rows += n;
    rep.more |= n as i64 >= limit;
    let n = tx.execute(
        "DELETE FROM rejects WHERE id IN (SELECT id FROM rejects WHERE block_seq < ?1 LIMIT ?2)",
        params![final_seq, limit],
    )?;
    rep.rows += n;
    rep.more |= n as i64 >= limit;
    tx.execute_batch("DELETE FROM lean_batch")?;
    tx.commit()?;
    rep.pages = release_pages(conn, cfg.pages_per_batch)?;
    Ok(rep)
}

/// Hands up to `max` freed pages back to the file (`incremental_vacuum`; a no-op on a store created without it). Returns the
/// pages released.
pub fn release_pages(conn: &Connection, max: usize) -> DbResult<usize> {
    let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    if free <= 0 || auto_vacuum_mode(conn)? != 2 {
        return Ok(0);
    }
    let n = (free as usize).min(max);
    // the pragma returns one row per freed page: read them all
    let mut st = conn.prepare(&format!("PRAGMA incremental_vacuum({n})"))?;
    let mut rows = st.query([])?;
    while rows.next()?.is_some() {}
    Ok(n)
}

/// `PRAGMA auto_vacuum`: 0 none, 1 full, 2 incremental.
pub fn auto_vacuum_mode(conn: &Connection) -> DbResult<i64> {
    Ok(conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?)
}

/// Makes the store incremental-vacuum capable: a store created by a build without lean storage is rewritten once
/// (`VACUUM`; the file is copied, so it needs the free space of its own size and takes a minute per few hundred MB). A new
/// store is empty, so the rewrite is instant (setting the mode alone does not take once the journal mode has written the
/// file's header). Returns whether a store with tables was rewritten.
pub fn ensure_incremental(conn: &Connection) -> DbResult<bool> {
    if auto_vacuum_mode(conn)? == 2 {
        return Ok(false);
    }
    conn.pragma_update(None, "auto_vacuum", 2)?;
    let tables: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))?;
    conn.execute_batch("VACUUM")?;
    Ok(tables > 0)
}
