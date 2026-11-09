//! The activity totals (`GET /v1/stats`): what the Activity page's summary shows beside the candles' volume and trades, read from the
//! small aggregates the store keeps by triggers in the transaction of every write (a batch, a revert, a replay, the lean pruning;
//! schema.sql, `activity` and `actors`), so every figure costs the same whatever the size of the history.
//!
//! * **orders** placed in the range: the `create` events at or after `since`, summed from the per-minute and per-hour buckets of
//!   `activity` (the minutes up to the first whole hour, the hours after it); all time: the `orders` counter.
//! * **fills** and **fees paid** likewise: the fill events, and what they released to the filler, `amount x tip / scale` at the
//!   tip the order had at the fill (`order_events.fee`, written with the event: a later amend does not change it).
//! * **traders**: the makers (order keys, not persons) with an order placed in the range: `actors` keeps the newest placement
//!   block of each, so the range is one index range (`actors_last`) from the first block with an event at `since`.
//! * **executors**: the scripts that funded the fill transactions of the range (`order_events.executor_id`, from the record log's
//!   payer section: an Executor's funding key), likewise. A fill recorded without one (indexed from a record-log frame written
//!   before payers were recorded, 2026-11-09, or a transaction no plain input funded) is not counted.
//! * **locked**: the KAS in the live order UTXOs now (`order_utxos_spent`: the unspent ones), whatever the range.
//!
//! The buckets are of block time, the newest blocks' time stamps monotone to within seconds, which the day-sized windows do not see.

use super::db::DbResult;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// The bucket sizes of `activity` (ms): minutes, and hours.
pub const MINUTE: i64 = 60_000;
pub const HOUR: i64 = 3_600_000;

/// The most a fee may be: the KAS supply in sompi (29 billion KAS). A fee the terms put above it is not a fee any transaction
/// can pay (and the sums stay in 64 bits): unknown.
pub const MAX_FEE: i64 = 29_000_000_000 * 100_000_000;

/// What a fill of `amount` base units released to its filler at `tip` sompi per whole token of `scale` base units: `floor(amount x
/// tip / scale)` (the covenants' rounding). `None` when a term is missing or out of range.
pub fn fill_fee(amount: Option<i64>, tip: Option<i64>, scale: Option<i64>) -> Option<i64> {
    let (amount, tip, scale) = (amount?, tip?, scale?);
    if amount < 0 || tip < 0 || scale <= 0 {
        return None;
    }
    let fee = i128::from(amount) * i128::from(tip) / i128::from(scale);
    i64::try_from(fee).ok().filter(|f| *f <= MAX_FEE)
}

/// The id of a script public key in `scripts` (added on first sight; never deleted, like `ids`).
pub fn intern_script(conn: &Connection, spk: &[u8]) -> DbResult<i64> {
    if let Some(id) =
        conn.prepare_cached("SELECT id FROM scripts WHERE spk = ?1")?.query_row([spk], |r| r.get::<_, i64>(0)).optional()?
    {
        return Ok(id);
    }
    conn.prepare_cached("INSERT INTO scripts (spk) VALUES (?1)")?.execute([spk])?;
    Ok(conn.last_insert_rowid())
}

/// `kob_fill_fee(amount, tip, scale)`: [`fill_fee`] in SQL (the migration computes the fees of the fills a store already holds).
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function("kob_fill_fee", 3, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        Ok(fill_fee(ctx.get(0)?, ctx.get(1)?, ctx.get(2)?))
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityView {
    /// The start of the range used (unix ms, whole minutes); `null`: all time.
    pub since: Option<i64>,
    /// Newest chain block time the store recorded an event for (unix ms).
    pub ts: Option<i64>,
    /// Orders placed in the range.
    pub orders: i64,
    /// Fill events in the range.
    pub fills: i64,
    /// Distinct makers of the orders placed in the range.
    pub traders: i64,
    /// Distinct scripts that funded the fill transactions of the range (fills recorded without one are not counted).
    pub executors: i64,
    /// Sompi the fills of the range released to their fillers (a string).
    pub fees_paid: String,
    /// Sompi in the live order UTXOs now (a string).
    pub kas_locked: String,
}

/// The totals of the range from `since` (unix ms) to the newest block, or of all time.
pub fn activity(conn: &Connection, since: Option<i64>) -> DbResult<ActivityView> {
    let ts = conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get::<_, Option<i64>>(0))?;
    let from = since.unwrap_or(0).max(0);
    let (fills, fees, mut orders) = buckets_from(conn, from)?;
    // the first block of the range (`traders`, `executors`): none means nothing happened since
    let first_seq = match since {
        None => Some(0),
        Some(s) => conn
            .query_row("SELECT block_seq FROM order_events WHERE ts >= ?1 AND block_seq > 0 ORDER BY ts LIMIT 1", [s], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?,
    };
    let (traders, executors) = match first_seq {
        None => (0, 0),
        Some(seq) => (actors_from(conn, 1, seq)?, actors_from(conn, 2, seq)?),
    };
    if since.is_none() {
        // every order, imported ones too (an order a tool wrote without its `create` event)
        orders = conn.query_row("SELECT COALESCE((SELECT n FROM counters WHERE name = 'orders'), 0)", [], |r| r.get(0))?;
    }
    let locked: i64 = conn.query_row("SELECT COALESCE(SUM(value), 0) FROM order_utxos WHERE spent_block IS NULL", [], |r| r.get(0))?;
    Ok(ActivityView { since, ts, orders, fills, traders, executors, fees_paid: fees.to_string(), kas_locked: locked.to_string() })
}

/// (fills, fees, orders) of the buckets from `from` on: the minutes up to the first whole hour at or after it, then the hours.
fn buckets_from(conn: &Connection, from: i64) -> DbResult<(i64, i128, i64)> {
    let hour = from.saturating_add(HOUR - 1) / HOUR * HOUR;
    let mut st = conn.prepare_cached(
        "SELECT fills, fees, orders FROM activity WHERE iv = ?1 AND t >= ?2 AND t < ?3 \
         UNION ALL SELECT fills, fees, orders FROM activity WHERE iv = ?4 AND t >= ?3",
    )?;
    let mut rows = st.query(params![MINUTE, from, hour, HOUR])?;
    let (mut fills, mut fees, mut orders) = (0i64, 0i128, 0i64);
    while let Some(r) = rows.next()? {
        fills += r.get::<_, i64>(0)?;
        // a sum that left 64 bits (it cannot with real fees, [`MAX_FEE`]) reads as a float rather than failing the read
        fees += match r.get_ref(1)? {
            rusqlite::types::ValueRef::Integer(v) => i128::from(v),
            rusqlite::types::ValueRef::Real(v) => v as i128,
            _ => 0,
        };
        orders += r.get::<_, i64>(2)?;
    }
    Ok((fills, fees, orders))
}

/// The actors of `kind` (1 makers, 2 executors) active at block `seq` or later.
fn actors_from(conn: &Connection, kind: i64, seq: i64) -> DbResult<i64> {
    Ok(conn
        .prepare_cached("SELECT COUNT(*) FROM actors WHERE kind = ?1 AND last_block >= ?2")?
        .query_row(params![kind, seq], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every read of the totals is on an index, so the cost follows the buckets and not the size of the store.
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
        let p = plan("SELECT fills, fees, orders FROM activity WHERE iv = 60000 AND t >= 5 AND t < 9");
        assert!(p.contains("PRIMARY KEY") && p.contains("t>?"), "{p}");
        let p = plan("SELECT COUNT(*) FROM actors WHERE kind = 1 AND last_block >= 5");
        assert!(p.contains("actors_last"), "{p}");
        let p = plan("SELECT COALESCE(SUM(value), 0) FROM order_utxos WHERE spent_block IS NULL");
        assert!(p.contains("order_utxos_spent"), "{p}");
        // the triggers' recounts on a revert: one seek each
        let p = plan("SELECT MAX(block_seq) FROM order_events WHERE kind = 'fill' AND executor_id = 5");
        assert!(p.contains("order_events_executor"), "{p}");
        let p = plan("SELECT MAX(genesis_block) FROM orders WHERE maker_id = 5");
        assert!(p.contains("orders_maker_genesis"), "{p}");
    }

    #[test]
    fn a_fee_is_the_covenants_floor_and_nothing_out_of_range() {
        assert_eq!(fill_fee(Some(150), Some(1_000), Some(100)), Some(1_500));
        assert_eq!(fill_fee(Some(7), Some(10), Some(3)), Some(23));
        assert_eq!(fill_fee(Some(7), Some(0), Some(3)), Some(0));
        assert_eq!(fill_fee(None, Some(10), Some(3)), None);
        assert_eq!(fill_fee(Some(7), None, Some(3)), None);
        assert_eq!(fill_fee(Some(7), Some(10), Some(0)), None);
        assert_eq!(fill_fee(Some(-7), Some(10), Some(1)), None);
        assert_eq!(fill_fee(Some(i64::MAX), Some(i64::MAX), Some(1)), None);
        assert_eq!(fill_fee(Some(MAX_FEE), Some(1), Some(1)), Some(MAX_FEE));
        assert_eq!(fill_fee(Some(MAX_FEE), Some(2), Some(1)), None);
    }
}
