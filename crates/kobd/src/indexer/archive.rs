//! Archive pruning: a store that keeps its history (`lean = false`) with `archive_pruning` on (the default) deletes the rows that no
//! read of the API and no later write needs, in small periodic batches ([`prune`], run by the follower between its steps like the lean
//! pruning, and only while the store follows the node). Everything the API answers stays the same, bar one documented retention:
//!
//! * **spent order UTXOs** (`order_utxos`) and **spent token UTXOs of orders** (`token_utxos`) of orders that are no longer live, once
//!   final (spent in a chain block out of the reorg window): every read and every write of the follower reads an order's UTXOs only while
//!   they are unspent (its tip, its custody, the outpoints a block spends). The rows of a live order (open or partly filled) all stay; an
//!   order's rows go when the spend that closed it is final. A re-listing reads the genesis output's KAS from the order
//!   (`orders.genesis_value`, schema 12); an order without it keeps its genesis row. Rows a gap closed (`spent_block = 0`) stay: a
//!   recovery import can revive the order at them (`processor::revive`);
//! * **in-place amends** (`order_amends`) of final blocks: the rows are what a revert of their block restores, and nothing else reads them;
//! * **spent token holdings** (`token_holdings`) created more than `spent_holdings_retention_days` (30 by default) before the node's
//!   position, once final (`GET /v1/token-utxos?spent=true` lists the spent holdings of that window), except the custody of a live order
//!   ([`super::lean::live_order_holding`]). The proof that a token has a program stays (`holding_programs`, schema 12).
//!
//! Orders, events, fills, candles, pair fills and the activity totals are never touched, so the trades, candles, statistics, books,
//! orders and their events read the same. A batch runs with the `meta` key `pruning`, as the lean pruning does (no delete trigger
//! takes back what it counted), and a revert inside the window is exact: nothing stamped with a block of the window is deleted.
//!
//! The UTXO rows are visited once, by the block that spent them: the `meta` key `archive_seq` is the first chain block (`blocks.seq`) not
//! yet visited. A batch takes the orders that had a UTXO row spent in the next stretch of final blocks, and deletes all the final spent rows
//! of those that are no longer live (an order that was live when its earlier rows were visited is taken again by the spend that closed
//! it). `kobd db replay` applies the same pruning once its frames are in, and the follower goes on from there, so a rebuilt store holds
//! the rows a pruned one does (`tests/archive_prune.rs`; the decisions depend only on the store's rows and its window).
//!
//! The file does not shrink by itself (a store that keeps its history is not created with incremental vacuum): the freed pages are used
//! again by new rows, so it stops growing for a while; `kobd db vacuum` gives them back once (docs/ops/executor.md).

use super::db::{meta_get, meta_set, DbResult};
use super::lean::{cutoffs, live_order_holding, LeanReport};
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{Duration, Instant};

/// The `meta` key of the first chain block whose spent UTXO rows a batch has not visited yet.
pub const MARK_KEY: &str = "archive_seq";

/// How much one batch deletes at most, and how long it may take (one write transaction of small steps that stops when its time budget
/// is spent, as the lean pruning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveConfig {
    /// Spent holdings created this many DAA before the node's position or more go once final (never less than the reorg window).
    pub holdings_retention_daa: u64,
    /// Rows deleted per batch at most, all tables (a step that began finishes: the orders of a stretch of blocks go whole).
    pub rows_per_batch: usize,
    /// The time a batch may take (at least one step runs).
    pub budget: Duration,
    /// Spent UTXO rows of a stretch of blocks, amends and holdings per step.
    pub step_rows: usize,
}

impl ArchiveConfig {
    pub fn new(holdings_retention_daa: u64) -> Self {
        ArchiveConfig { holdings_retention_daa, rows_per_batch: 20_000, budget: Duration::from_millis(12), step_rows: 200 }
    }
}

/// The one index only this pruning reads: the spent holdings by the DAA they were created at (the lean pruning's own, the same
/// definition). Built when the indexer opens a store that prunes (a few seconds on a store of millions of holdings).
pub fn ensure_indexes(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS token_holdings_spent_daa ON token_holdings (created_daa) WHERE spent_block IS NOT NULL;",
    )?;
    Ok(())
}

/// One bounded batch; see the module documentation for what is deleted.
pub fn prune(
    conn: &mut Connection,
    cfg: &ArchiveConfig,
    cursor_daa: u64,
    node_daa: Option<u64>,
    reorg_window_daa: u64,
) -> DbResult<LeanReport> {
    let started = Instant::now();
    let mut rep = LeanReport::default();
    let Some((final_seq, before)) = cutoffs(conn, cursor_daa, node_daa, reorg_window_daa, cfg.holdings_retention_daa)? else {
        return Ok(rep);
    };
    ensure_indexes(conn)?;
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS archive_batch (id INTEGER PRIMARY KEY); DELETE FROM archive_batch;")?;
    tx.execute("INSERT INTO meta (k, v) VALUES ('pruning', '1') ON CONFLICT (k) DO UPDATE SET v = excluded.v", [])?;
    let start_mark: i64 = meta_get(&tx, MARK_KEY)?.and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let mut mark = start_mark;
    let mut left = cfg.rows_per_batch;
    let (mut amends_more, mut holdings_more) = (true, true);
    loop {
        let mut progressed = false;
        // 1. the orders a UTXO row of which was spent in the next stretch of final blocks: their final spent rows, unless they are live
        if mark < final_seq && left > 0 {
            let lo: Option<i64> = tx.query_row(
                "SELECT MIN(b) FROM (SELECT MIN(spent_block) AS b FROM order_utxos WHERE spent_block >= ?1 AND spent_block < ?2 \
                 UNION ALL SELECT MIN(spent_block) FROM token_utxos WHERE spent_block >= ?1 AND spent_block < ?2)",
                params![mark, final_seq],
                |r| r.get(0),
            )?;
            match lo {
                None => mark = final_seq,
                Some(lo) => {
                    // the stretch ends where either table has more than a step of rows past its start (a block with more stays whole)
                    let mut hi = final_seq;
                    for t in ["order_utxos", "token_utxos"] {
                        let at: Option<i64> = tx
                            .query_row(
                                &format!(
                                    "SELECT spent_block FROM {t} INDEXED BY {t}_spent WHERE spent_block >= ?1 AND spent_block < ?2 \
                                     ORDER BY spent_block LIMIT 1 OFFSET ?3"
                                ),
                                params![lo, final_seq, cfg.step_rows as i64],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if let Some(at) = at {
                            hi = hi.min(at.max(lo + 1));
                        }
                    }
                    for t in ["order_utxos", "token_utxos"] {
                        tx.execute(
                            &format!(
                                "INSERT OR IGNORE INTO archive_batch (id) SELECT order_id FROM {t} INDEXED BY {t}_spent \
                                 WHERE spent_block >= ?1 AND spent_block < ?2"
                            ),
                            params![lo, hi],
                        )?;
                    }
                    // a live order keeps its rows, and so does one whose closing a revert could still undo (its last block is in the
                    // window: the spend of that block, final later, takes it again)
                    tx.execute(
                        "DELETE FROM archive_batch WHERE EXISTS (SELECT 1 FROM order_state s WHERE s.order_id = archive_batch.id \
                         AND (s.status IN ('open','partial') OR s.last_block >= ?1))",
                        [final_seq],
                    )?;
                    let mut n = tx.execute(
                        "DELETE FROM order_utxos INDEXED BY order_utxos_cov WHERE order_id IN (SELECT id FROM archive_batch) AND spent_block > 0 AND spent_block < ?1 \
                         AND NOT EXISTS (SELECT 1 FROM orders o WHERE o.id = order_utxos.order_id AND o.genesis_value IS NULL \
                                         AND o.quote_id IS NULL AND o.genesis_txid = order_utxos.txid AND o.genesis_out = order_utxos.idx)",
                        [final_seq],
                    )?;
                    n += tx.execute(
                        "DELETE FROM token_utxos INDEXED BY token_utxos_owner WHERE order_id IN (SELECT id FROM archive_batch) AND spent_block > 0 AND spent_block < ?1",
                        [final_seq],
                    )?;
                    tx.execute_batch("DELETE FROM archive_batch")?;
                    rep.rows += n;
                    left = left.saturating_sub(n);
                    mark = hi;
                }
            }
            progressed = true;
        }
        // 2. the amends of final blocks (what a revert of their block would restore)
        let want = cfg.step_rows.min(left);
        if want > 0 && amends_more {
            let n = tx.execute(
                "DELETE FROM order_amends WHERE id IN (SELECT id FROM order_amends INDEXED BY order_amends_block WHERE block_seq < ?1 LIMIT ?2)",
                params![final_seq, want as i64],
            )?;
            rep.rows += n;
            left = left.saturating_sub(n);
            progressed |= n > 0;
            amends_more = n >= want;
        }
        // 3. the spent holdings past the retention, final, but a live order's custody
        let want = cfg.step_rows.min(left);
        if want > 0 && holdings_more {
            let n = tx.execute(
                concat!(
                    "DELETE FROM token_holdings WHERE rowid IN (SELECT h.rowid FROM token_holdings h INDEXED BY token_holdings_spent_daa \
                     WHERE h.spent_block IS NOT NULL AND h.spent_block < ?1 AND h.created_daa < ?2 AND NOT ",
                    live_order_holding!("h"),
                    " LIMIT ?3)"
                ),
                params![final_seq, before, want as i64],
            )?;
            rep.rows += n;
            left = left.saturating_sub(n);
            progressed |= n > 0;
            holdings_more = n >= want;
        }
        rep.more = mark < final_seq || amends_more || holdings_more;
        if !progressed || !rep.more || left == 0 || started.elapsed() >= cfg.budget {
            break;
        }
    }
    if mark != start_mark {
        meta_set(&tx, MARK_KEY, &mark.to_string())?;
    }
    tx.execute("DELETE FROM meta WHERE k = 'pruning'", [])?;
    tx.commit()?;
    Ok(rep)
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
