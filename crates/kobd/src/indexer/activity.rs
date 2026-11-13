//! The activity totals behind `GET /v1/stats`, which the Activity page shows beside the candles'
//! volume and trades.
//!
//! They are read from the small aggregates that the store keeps with triggers, in the transaction
//! of every write: a batch, a revert, a replay and the lean pruning (`schema.sql`, `activity` and
//! `actors`). Every figure therefore costs the same whatever the size of the history.
//!
//! # Figures
//!
//! - **orders**: the orders placed in the range. These are the `create` events at or after `since`,
//!   summed from the per-minute and per-hour buckets of `activity`: the minutes up to the first
//!   whole hour, then the hours. For all time it is the `orders` counter.
//! - **fills** and **tips paid**, likewise: the fill events, and the priority tips they released to
//!   the filler, `amount x tip / scale` at the tip the order had at the fill. The tip is stored
//!   with the event (`order_events.fee`), so a later amend does not change it. `fees_paid` is the
//!   same figure under its first name. It is deprecated, because it is not all the executors earn.
//! - **executor income**: what the executors' transactions of the range left to their senders
//!   before the network fee. The transactions are fills, refunds, kills, expiries, arms, trails and
//!   merges. The income is the crossing spread, auction margins, and priority and keeper tips
//!   (`executor_txs.income`, [`super::income`]). It is summed over the transactions whose income is
//!   known, with how many were counted and how many were not. A transaction is not counted for an
//!   unproven order state, for a maker's cancel in the same transaction, or in a store migrated
//!   with its history.
//! - **miner fees**, likewise: the network fees of those transactions (`executor_txs.miner_fee`,
//!   what their inputs held, as the node reported them, minus their outputs). They are known for
//!   the transactions indexed from records with input values. A transaction in which the sender
//!   trades for itself, an **own trade** ([`super::income`]), is not counted in the income but its
//!   network fee is. Own trades are counted apart (`executor_income_own_trade_txs`).
//! - **executor net**: income minus network fee over the counted transactions where both are known.
//! - **traders**: the makers (order keys, not persons) with an order placed in the range. `actors`
//!   keeps the newest placement block of each, so the range is one index range (`actors_last`) from
//!   the first block with an event at `since`.
//! - **executors**: the scripts that funded the fill transactions of the range, likewise. They come
//!   from `order_events.executor_id`, which is set from the payer section of the record log, an
//!   Executor's funding key. A fill recorded without one is not counted. That is a fill indexed
//!   from a record-log frame written before payers were recorded, or a transaction that no plain
//!   input funded.
//! - **locked**: the KAS in the live order UTXOs now (`order_utxos_spent`: the unspent ones),
//!   whatever the range.
//!
//! The buckets are of block time. The time stamps of the newest blocks are monotone to within
//! seconds, which the day-sized windows do not see. The buckets hold everything the store ever
//! indexed: the lean pruning does not take back what it deletes. The totals of all time of a lean
//! store are therefore not only those of the history it still holds, and neither is its all-time
//! order count (`counters.orders_all`).

use super::db::DbResult;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// The bucket sizes of `activity` in ms: minutes, and hours.
pub const MINUTE: i64 = 60_000;
pub const HOUR: i64 = 3_600_000;

/// The most a fee may be: the KAS supply in sompi (29 billion KAS).
///
/// A fee the terms put above it is not a fee any transaction can pay, and the sums stay in 64 bits,
/// so it is unknown.
pub const MAX_FEE: i64 = 29_000_000_000 * 100_000_000;

/// What a fill of `amount` base units released to its filler at `tip` sompi per whole token.
///
/// A whole token is `scale` base units. The result is `floor(amount x tip / scale)`, the covenants'
/// rounding. It is `None` when a term is missing or out of range.
pub fn fill_fee(amount: Option<i64>, tip: Option<i64>, scale: Option<i64>) -> Option<i64> {
    let (amount, tip, scale) = (amount?, tip?, scale?);
    if amount < 0 || tip < 0 || scale <= 0 {
        return None;
    }
    let fee = i128::from(amount) * i128::from(tip) / i128::from(scale);
    i64::try_from(fee).ok().filter(|f| *f <= MAX_FEE)
}

/// The id of a script public key in `scripts`, added on first sight and never deleted, like `ids`.
pub fn intern_script(conn: &Connection, spk: &[u8]) -> DbResult<i64> {
    if let Some(id) =
        conn.prepare_cached("SELECT id FROM scripts WHERE spk = ?1")?.query_row([spk], |r| r.get::<_, i64>(0)).optional()?
    {
        return Ok(id);
    }
    conn.prepare_cached("INSERT INTO scripts (spk) VALUES (?1)")?.execute([spk])?;
    Ok(conn.last_insert_rowid())
}

/// `kob_fill_fee(amount, tip, scale)`: [`fill_fee`] in SQL.
///
/// The migration uses it to compute the fees of the fills a store already holds.
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function("kob_fill_fee", 3, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        Ok(fill_fee(ctx.get(0)?, ctx.get(1)?, ctx.get(2)?))
    })
}

/// The activity totals of a range, as `GET /v1/stats` serves them.
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
    /// Distinct scripts that funded the fill transactions of the range.
    ///
    /// Fills recorded without a funding script are not counted.
    pub executors: i64,
    /// Deprecated alias of `tips_paid`, as a string in sompi.
    ///
    /// It is not all the executors earn: see `executor_income`.
    pub fees_paid: String,
    /// The priority tips the fills of the range released to their fillers (sompi, a string).
    ///
    /// It is `floor(amount x tip / scale)` per fill.
    pub tips_paid: String,
    /// What the executors' transactions of the range left to their senders before the network fee.
    ///
    /// It is in sompi, a string, and may be negative when a sender pays more than it takes. It is
    /// the crossing spread, auction margins and the priority and keeper tips. It is the sum over
    /// `executor_income_txs` transactions, and `executor_income_unknown_txs` more are not counted.
    pub executor_income: String,
    /// Transactions counted in `executor_income`.
    pub executor_income_txs: i64,
    /// Transactions of the range whose income is not known.
    pub executor_income_unknown_txs: i64,
    /// Executors' transactions of the range in which the sender traded for itself.
    ///
    /// Its own tokens went in, or tokens came out that no order owns and no covenant pins, as in an
    /// inventory buy or sale or a taker. Their income is not counted in `executor_income`, because
    /// it is what the sender paid or got for tokens, not what traders paid it. Their network fees
    /// are counted.
    pub executor_income_own_trade_txs: i64,
    /// The network fees of those transactions (sompi, a string), over `miner_fee_txs` of them.
    ///
    /// `miner_fee_unknown_txs` more are not counted: they were indexed from records without the
    /// inputs' values.
    pub miner_fees: String,
    /// Transactions counted in `miner_fees`.
    pub miner_fee_txs: i64,
    /// Transactions of the range whose network fee is not known.
    pub miner_fee_unknown_txs: i64,
    /// Income minus network fee over the `executor_net_txs` transactions where both are known.
    ///
    /// It is in sompi, a string, and may be negative.
    pub executor_net: String,
    /// Transactions counted in `executor_net`.
    pub executor_net_txs: i64,
    /// Sompi in the live order UTXOs now (a string).
    pub kas_locked: String,
}

/// The totals of the range from `since` (unix ms) to the newest block, or of all time.
pub fn activity(conn: &Connection, since: Option<i64>) -> DbResult<ActivityView> {
    let ts = conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get::<_, Option<i64>>(0))?;
    let from = since.unwrap_or(0).max(0);
    let b = buckets_from(conn, from)?;
    let mut orders = b.orders;
    // The first block of the range (`traders`, `executors`). `None` means nothing happened since.
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
        // Every order ever stored: imported ones too, such as an order a tool wrote without its
        // `create` event, and pruned ones.
        orders = conn.query_row("SELECT COALESCE((SELECT n FROM counters WHERE name = 'orders_all'), 0)", [], |r| r.get(0))?;
    }
    let locked: i64 = conn.query_row("SELECT COALESCE(SUM(value), 0) FROM order_utxos WHERE spent_block IS NULL", [], |r| r.get(0))?;
    Ok(ActivityView {
        since,
        ts,
        orders,
        fills: b.fills,
        traders,
        executors,
        fees_paid: b.tips.to_string(),
        tips_paid: b.tips.to_string(),
        executor_income: b.income.to_string(),
        executor_income_txs: b.income_n,
        executor_income_unknown_txs: b.txs - b.income_n - b.own_n,
        executor_income_own_trade_txs: b.own_n,
        miner_fees: b.miner_fees.to_string(),
        miner_fee_txs: b.miner_fee_n,
        miner_fee_unknown_txs: b.txs - b.miner_fee_n,
        executor_net: b.net.to_string(),
        executor_net_txs: b.net_n,
        kas_locked: locked.to_string(),
    })
}

/// The sums of the buckets of a range (`activity`, `schema.sql`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Sums {
    fills: i64,
    tips: i128,
    orders: i64,
    txs: i64,
    income: i128,
    income_n: i64,
    miner_fees: i128,
    miner_fee_n: i64,
    net: i128,
    net_n: i64,
    own_n: i64,
}

/// The sums of the buckets from `from` on.
///
/// It takes the minutes up to the first whole hour at or after `from`, then the hours.
fn buckets_from(conn: &Connection, from: i64) -> DbResult<Sums> {
    let hour = from.saturating_add(HOUR - 1) / HOUR * HOUR;
    let cols = "fills, fees, orders, txs, income, income_n, miner_fees, miner_fee_n, net, net_n, own_n";
    let mut st = conn.prepare_cached(&format!(
        "SELECT {cols} FROM activity WHERE iv = ?1 AND t >= ?2 AND t < ?3 UNION ALL SELECT {cols} FROM activity WHERE iv = ?4 AND t >= ?3"
    ))?;
    let mut rows = st.query(params![MINUTE, from, hour, HOUR])?;
    // A sompi sum that left 64 bits reads as a float rather than failing the read. It cannot happen
    // with real amounts ([`MAX_FEE`]).
    let sompi = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<i128> {
        Ok(match r.get_ref(i)? {
            rusqlite::types::ValueRef::Integer(v) => i128::from(v),
            rusqlite::types::ValueRef::Real(v) => v as i128,
            _ => 0,
        })
    };
    let mut s = Sums::default();
    while let Some(r) = rows.next()? {
        s.fills += r.get::<_, i64>(0)?;
        s.tips += sompi(r, 1)?;
        s.orders += r.get::<_, i64>(2)?;
        s.txs += r.get::<_, i64>(3)?;
        s.income += sompi(r, 4)?;
        s.income_n += r.get::<_, i64>(5)?;
        s.miner_fees += sompi(r, 6)?;
        s.miner_fee_n += r.get::<_, i64>(7)?;
        s.net += sompi(r, 8)?;
        s.net_n += r.get::<_, i64>(9)?;
        s.own_n += r.get::<_, i64>(10)?;
    }
    Ok(s)
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

    /// Every read of the totals is on an index, so the cost follows the buckets, not the store
    /// size.
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
        // The triggers' recounts on a revert: one seek each.
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
