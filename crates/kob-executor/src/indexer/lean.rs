//! Lean storage (`lean = true`): an executor needs the live book, not the history. The store keeps
//!
//! * every open or partly filled order with all its rows (events, UTXOs, custody, strays, flags): the matcher, the keepers,
//!   the maintenance jobs and the read API's live book read exactly these;
//! * the reorg window: nothing stamped with a chain block of the window is ever deleted, so a revert is exact;
//! * `lean_retention_days` (30 by default) of closed orders (filled, cancelled, refunded, killed), with their fills and
//!   events, so the API still answers for what happened lately;
//!
//! and drops the rest in small periodic batches ([`prune`], run by the follower between its steps and only while the store follows
//! the node: a store far behind it catches up first; each batch is one write transaction of small steps that stops after its time
//! budget, tens of milliseconds whatever the size of the store): closed orders past the retention with everything that belongs to them, spent UTXO rows (orders' and tokens')
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
use std::time::{Duration, Instant};

/// How much one batch deletes at most, and how long it may take. A batch is one write transaction made of small steps (a few dozen
/// closed orders with all their rows, a few hundred rows of each table pruned by age) that stops when its time budget is spent, so the
/// follower and the matcher never wait long on it, however large the store is and whatever there is to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeanConfig {
    /// Closed orders and spent rows older than this many DAA behind the node stay (never less than the reorg window).
    pub retention_daa: u64,
    /// The store's size cap in bytes (pages in use): above it the oldest closed orders are deleted, retention or not. 0: none.
    pub max_db_bytes: u64,
    /// Closed orders deleted per batch at most.
    pub orders_per_batch: usize,
    /// Rows deleted per batch at most from each table that is pruned by age.
    pub rows_per_batch: usize,
    /// Freed pages handed back to the file per batch at most.
    pub pages_per_batch: usize,
    /// The time a batch may take: the steps run while it has not elapsed (at least one step runs, so a batch always makes progress;
    /// the last step may overrun it by its own duration).
    pub budget: Duration,
    /// Closed orders per step.
    pub step_orders: usize,
    /// Rows per table per step.
    pub step_rows: usize,
    /// Freed pages handed back per step.
    pub step_pages: usize,
}

impl LeanConfig {
    pub fn new(retention_daa: u64, max_db_bytes: u64) -> Self {
        LeanConfig {
            retention_daa,
            max_db_bytes,
            orders_per_batch: 500,
            rows_per_batch: 5_000,
            pages_per_batch: 2_000,
            budget: Duration::from_millis(12),
            step_orders: 10,
            step_rows: 50,
            step_pages: 64,
        }
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

/// The indexes only the pruning reads, created when a store becomes lean (a store that keeps its history does not pay for them):
/// * the oldest closed orders by age (`order_state_closed_daa`): without it every batch above the size cap sorted all the closed
///   orders, 2 s on a store of a million;
/// * the spent rows and the pair fills by the DAA they were created at (`*_spent_daa`, `pair_fills_daa`): the age cut-off is on that
///   column and `spent_block` says nothing about it, so a batch that found nothing old enough to delete read every spent row of the
///   store (3 s on the soak store, in every batch);
/// * the two tables the deletion of an order reaches by its id with no index of their own.
pub fn ensure_indexes(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS order_state_closed_daa ON order_state (last_daa) WHERE status NOT IN ('open','partial'); \
         CREATE INDEX IF NOT EXISTS order_amends_order ON order_amends (order_id); \
         CREATE INDEX IF NOT EXISTS pair_fills_order ON pair_fills (covenant_id); \
         CREATE INDEX IF NOT EXISTS pair_fills_daa ON pair_fills (daa); \
         CREATE INDEX IF NOT EXISTS order_utxos_spent_daa ON order_utxos (created_daa) WHERE spent_block IS NOT NULL; \
         CREATE INDEX IF NOT EXISTS token_utxos_spent_daa ON token_utxos (created_daa) WHERE spent_block IS NOT NULL; \
         CREATE INDEX IF NOT EXISTS token_holdings_spent_daa ON token_holdings (created_daa) WHERE spent_block IS NOT NULL;",
    )?;
    Ok(())
}

/// One bounded batch of the lean pruning; see the module documentation for what is deleted and what never is. It runs small steps in
/// one write transaction until [`LeanConfig::budget`] has elapsed or the per-batch limits are reached.
pub fn prune(
    conn: &mut Connection,
    cfg: &LeanConfig,
    cursor_daa: u64,
    node_daa: Option<u64>,
    reorg_window_daa: u64,
) -> DbResult<LeanReport> {
    let started = Instant::now();
    let mut rep = LeanReport::default();
    let Some((final_seq, before)) = cutoffs(conn, cursor_daa, node_daa, reorg_window_daa, cfg.retention_daa)? else {
        return Ok(rep);
    };
    // above the size cap the oldest closed orders go first, whatever their age (only what is final, as always)
    let over_cap = cfg.max_db_bytes > 0 && used_bytes(conn)? > cfg.max_db_bytes;
    rep.over_cap = over_cap;
    let order_before = if over_cap { cursor_daa.saturating_sub(reorg_window_daa) as i64 } else { before };
    // (a no-op once the store has them; `open_writer_with` creates them at the first lean start)
    ensure_indexes(conn)?;
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS lean_batch (cov BLOB PRIMARY KEY, id INTEGER NOT NULL) WITHOUT ROWID; DELETE FROM lean_batch;")?;

    // closed orders past the retention whose exits (if-done children) are not live; the partial index `order_state_closed_daa` lists
    // them oldest first, so the oldest are the first rows read whatever the size of the store
    let pick = "INSERT INTO lean_batch SELECT p.covenant_id, p.id FROM order_state s INDEXED BY order_state_closed_daa CROSS JOIN orders p ON p.id = s.order_id \
                WHERE s.status NOT IN ('open','partial') AND s.last_daa < ?1 AND s.last_block < ?2 \
                AND NOT EXISTS (SELECT 1 FROM orders c JOIN order_state cs ON cs.order_id = c.id \
                                WHERE c.parent = p.covenant_id AND cs.status IN ('open','partial')) \
                ORDER BY s.last_daa LIMIT ?3";
    let aged: [(&str, &str); 3] = [
        (
            "order_utxos",
            "DELETE FROM order_utxos WHERE rowid IN (SELECT rowid FROM order_utxos INDEXED BY order_utxos_spent_daa WHERE spent_block IS NOT NULL AND spent_block < ?1 \
             AND created_daa < ?2 LIMIT ?3)",
        ),
        (
            "token_utxos",
            "DELETE FROM token_utxos WHERE rowid IN (SELECT rowid FROM token_utxos INDEXED BY token_utxos_spent_daa WHERE spent_block IS NOT NULL AND spent_block < ?1 \
             AND created_daa < ?2 LIMIT ?3)",
        ),
        (
            "token_holdings",
            "DELETE FROM token_holdings WHERE rowid IN (SELECT rowid FROM token_holdings INDEXED BY token_holdings_spent_daa WHERE spent_block IS NOT NULL AND spent_block < ?1 \
             AND created_daa < ?2 LIMIT ?3)",
        ),
    ];
    // rows left to delete from each table by age in this batch
    let mut left_rows = [cfg.rows_per_batch; 3];
    let (mut pair_left, mut rejects_left) = (cfg.rows_per_batch, cfg.rows_per_batch);
    let mut orders_left = cfg.orders_per_batch;
    loop {
        let mut progressed = false;
        // a step of closed orders
        let want = cfg.step_orders.min(orders_left);
        let mut more_orders = false;
        if want > 0 {
            let picked = tx.execute(pick, params![order_before, final_seq, want as i64])?;
            if picked > 0 {
                for sql in [
                    "DELETE FROM order_events WHERE order_id IN (SELECT id FROM lean_batch)",
                    "DELETE FROM order_amends WHERE order_id IN (SELECT id FROM lean_batch)",
                    "DELETE FROM pair_fills WHERE covenant_id IN (SELECT cov FROM lean_batch)",
                    "DELETE FROM order_utxos WHERE order_id IN (SELECT id FROM lean_batch)",
                    "DELETE FROM token_holdings WHERE owner_id IN (SELECT i.id FROM lean_batch b JOIN ids i ON i.h = b.cov) AND role IN ('custody','stray')",
                    "DELETE FROM token_utxos WHERE order_id IN (SELECT id FROM lean_batch)",
                    "DELETE FROM order_flags WHERE covenant_id IN (SELECT cov FROM lean_batch)",
                    "DELETE FROM order_state WHERE order_id IN (SELECT id FROM lean_batch)",
                ] {
                    rep.rows += tx.execute(sql, [])?;
                }
                let n = tx.execute("DELETE FROM orders WHERE covenant_id IN (SELECT cov FROM lean_batch)", [])?;
                rep.orders += n;
                rep.rows += n;
                tx.execute_batch("DELETE FROM lean_batch")?;
                orders_left = orders_left.saturating_sub(picked);
                progressed = true;
            }
            more_orders = picked >= want;
        }
        // a step of the rows pruned by age: spent rows past the retention, final (spent in a block out of the window; created before
        // the cut-off, so spent after the retention started is impossible to mistake): a live order's earlier UTXOs, the token UTXOs
        // and holdings that moved
        let mut more_rows = false;
        for (k, (_, sql)) in aged.iter().enumerate() {
            let want = cfg.step_rows.min(left_rows[k]);
            if want == 0 {
                continue;
            }
            let n = tx.execute(sql, params![final_seq, before, want as i64])?;
            rep.rows += n;
            left_rows[k] -= n;
            progressed |= n > 0;
            more_rows |= n >= want;
        }
        // pair-order fills past the retention (volume statistics of the window only) and the diagnostics of blocks out of the window
        let want = cfg.step_rows.min(pair_left);
        if want > 0 {
            let n = tx.execute(
                "DELETE FROM pair_fills WHERE id IN (SELECT id FROM pair_fills INDEXED BY pair_fills_daa WHERE block_seq < ?1 AND daa < ?2 LIMIT ?3)",
                params![final_seq, before, want as i64],
            )?;
            rep.rows += n;
            pair_left -= n;
            progressed |= n > 0;
            more_rows |= n >= want;
        }
        let want = cfg.step_rows.min(rejects_left);
        if want > 0 {
            let n = tx.execute(
                "DELETE FROM rejects WHERE id IN (SELECT id FROM rejects WHERE block_seq < ?1 LIMIT ?2)",
                params![final_seq, want as i64],
            )?;
            rep.rows += n;
            rejects_left -= n;
            progressed |= n > 0;
            more_rows |= n >= want;
        }
        // another batch has more when a step was full, or a limit of this batch was reached (its time ends it before the end of the work)
        let limit_hit = orders_left == 0 || left_rows.contains(&0) || pair_left == 0 || rejects_left == 0;
        rep.more = more_orders || more_rows || limit_hit;
        if !progressed || !rep.more || started.elapsed() >= cfg.budget {
            break;
        }
    }
    tx.commit()?;
    // the freed pages go back to the file in steps, while the budget lasts (and at least one step)
    let mut handed = 0;
    while handed < cfg.pages_per_batch {
        let n = release_pages(conn, cfg.step_pages.min(cfg.pages_per_batch - handed))?;
        if n == 0 {
            break;
        }
        handed += n;
        if started.elapsed() >= cfg.budget {
            rep.more |= handed >= cfg.pages_per_batch;
            break;
        }
    }
    rep.pages = handed;
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
