//! The activity totals (`GET /v1/stats`): what the Activity page's summary shows beside the candles' volume and trades, read from the
//! tables the indexer already keeps, every query on an index and every scan capped.
//!
//! * **orders** placed in the range: the orders whose placement block is at or after the first block with an event at `since`
//!   (`orders_genesis_page`, a covering range count; all time: the `orders` counter). Block time is monotone to within seconds, which
//!   the day-sized windows do not see.
//! * **traders**: the distinct makers of those orders (`orders_maker_genesis`: the next maker is one seek, whether it placed one in
//!   the range another). A maker is the key of an order, not a person.
//! * **fees paid**: what the fills of the range released to the filler, as the orders' tip terms say: `amount x tip / scale` per
//!   fill event (`order_events_fill_ts` per token). An order amended in place counts at its current tip. `null` when the range holds
//!   more fills than [`FILL_SCAN_MAX`].
//! * **locked**: the KAS in the live order UTXOs now (`order_utxos_spent`: the unspent ones), whatever the range.
//!
//! Executors are not counted: the record log keeps no key of whoever submitted a fill, so the figure does not exist for this store.

use super::db::DbResult;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// Most fill events one request reads for the fees; a longer range answers `null`.
pub const FILL_SCAN_MAX: usize = 200_000;
/// Most makers one request walks for the traders; a larger set answers `null`.
pub const MAKER_SCAN_MAX: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityView {
    /// The start of the range used (unix ms, whole minutes); `null`: all time.
    pub since: Option<i64>,
    /// Newest chain block time the store recorded an event for (unix ms).
    pub ts: Option<i64>,
    /// Orders placed in the range.
    pub orders: i64,
    /// Distinct makers of those orders (`null` beyond [`MAKER_SCAN_MAX`] makers).
    pub traders: Option<i64>,
    /// Always `null`: the store does not record who submitted a fill.
    pub executors: Option<i64>,
    /// Sompi the fills of the range released to their fillers (a string; `null` beyond [`FILL_SCAN_MAX`] fills).
    pub fees_paid: Option<String>,
    /// Sompi in the live order UTXOs now (a string).
    pub kas_locked: String,
}

/// The totals of the range from `since` (unix ms) to the newest block, or of all time.
pub fn activity(conn: &Connection, since: Option<i64>) -> DbResult<ActivityView> {
    let ts = conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get::<_, Option<i64>>(0))?;
    // the first block of the range: none means nothing happened since
    let first_seq = match since {
        None => Some(0),
        Some(s) => conn
            .query_row("SELECT block_seq FROM order_events WHERE ts >= ?1 AND block_seq > 0 ORDER BY ts LIMIT 1", [s], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?,
    };
    let (orders, traders) = match first_seq {
        None => (0, Some(0)),
        Some(0) => (
            conn.query_row("SELECT COALESCE((SELECT n FROM counters WHERE name = 'orders'), 0)", [], |r| r.get(0))?,
            makers_from(conn, 0)?,
        ),
        Some(seq) => {
            (conn.query_row("SELECT COUNT(*) FROM orders WHERE genesis_block >= ?1", [seq], |r| r.get(0))?, makers_from(conn, seq)?)
        }
    };
    let fees = fees_since(conn, since.unwrap_or(0))?;
    let locked: i64 = conn.query_row("SELECT COALESCE(SUM(value), 0) FROM order_utxos WHERE spent_block IS NULL", [], |r| r.get(0))?;
    Ok(ActivityView {
        since,
        ts,
        orders,
        traders,
        executors: None,
        fees_paid: fees.map(|f| f.to_string()),
        kas_locked: locked.to_string(),
    })
}

/// The distinct makers with an order placed at block `seq` or later: one seek for the next maker, one for its newest order.
fn makers_from(conn: &Connection, seq: i64) -> DbResult<Option<i64>> {
    let mut next = conn.prepare_cached("SELECT MIN(maker_id) FROM orders WHERE maker_id > ?1")?;
    let mut placed = conn.prepare_cached("SELECT 1 FROM orders WHERE maker_id = ?1 AND genesis_block >= ?2 LIMIT 1")?;
    let (mut maker, mut n, mut walked) = (i64::MIN, 0i64, 0usize);
    while let Some(m) = next.query_row([maker], |r| r.get::<_, Option<i64>>(0))? {
        walked += 1;
        if walked > MAKER_SCAN_MAX {
            return Ok(None);
        }
        if placed.query_row(params![m, seq], |_| Ok(())).optional()?.is_some() {
            n += 1;
        }
        maker = m;
    }
    Ok(Some(n))
}

/// The fill events at or after `since`, token by token (the partial index `order_events_fill_ts`), each with its order's tip and scale.
fn fees_since(conn: &Connection, since: i64) -> DbResult<Option<i128>> {
    let mut next = conn.prepare_cached("SELECT MIN(token_id) FROM order_events WHERE kind = 'fill' AND token_id > ?1")?;
    let mut fills = conn.prepare_cached(&format!("{FILLS_SQL} LIMIT ?3"))?;
    let (mut token, mut total, mut left) = (i64::MIN, 0i128, FILL_SCAN_MAX);
    while let Some(t) = next.query_row([token], |r| r.get::<_, Option<i64>>(0))? {
        token = t;
        let rows = fills.query_map(params![t, since, left as i64 + 1], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?))
        })?;
        for row in rows {
            if left == 0 {
                return Ok(None);
            }
            left -= 1;
            if let (Some(amount), Some(tip), Some(scale)) = row? {
                if scale > 0 && tip > 0 {
                    total += i128::from(amount) * i128::from(tip) / i128::from(scale);
                }
            }
        }
    }
    Ok(Some(total))
}

/// One token's fill events from a time: amount, the order's tip and scale.
const FILLS_SQL: &str = "SELECT e.amount, o.tip, o.scale FROM order_events e JOIN orders o ON o.id = e.order_id \
     WHERE e.kind = 'fill' AND e.token_id = ?1 AND e.ts >= ?2";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every read of the totals is on an index, so the cost follows the range and not the size of the store.
    #[test]
    fn the_reads_use_their_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let c = super::super::db::open_writer(&dir.path().join("t.sqlite3"), "testnet-10").unwrap();
        let plan = |sql: &str| -> String {
            let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let rows: Vec<String> = st.query_map([], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect();
            rows.join(" | ")
        };
        let p = plan("SELECT block_seq FROM order_events WHERE ts >= 5 AND block_seq > 0 ORDER BY ts LIMIT 1");
        assert!(p.contains("order_events_ts"), "{p}");
        let p = plan("SELECT COUNT(*) FROM orders WHERE genesis_block >= 5");
        assert!(p.contains("orders_genesis_page"), "{p}");
        let p = plan("SELECT MIN(maker_id) FROM orders WHERE maker_id > 5");
        assert!(p.contains("orders_maker_genesis"), "{p}");
        let p = plan("SELECT 1 FROM orders WHERE maker_id = 5 AND genesis_block >= 5 LIMIT 1");
        assert!(p.contains("orders_maker_genesis"), "{p}");
        let p = plan("SELECT MIN(token_id) FROM order_events WHERE kind = 'fill' AND token_id > 5");
        assert!(p.contains("order_events_fills") || p.contains("order_events_fill_ts"), "{p}");
        let p = plan(&format!("{FILLS_SQL} LIMIT 9").replace("?1", "5").replace("?2", "5"));
        assert!(p.contains("order_events_fill_ts"), "{p}");
        let p = plan("SELECT COALESCE(SUM(value), 0) FROM order_utxos WHERE spent_block IS NULL");
        assert!(p.contains("order_utxos_spent"), "{p}");
    }
}
