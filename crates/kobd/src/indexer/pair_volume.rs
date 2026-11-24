//! Maintained pair volume behind `GET /v1/pairs/{base}/{quote}/candles` (R5C2-6).
//!
//! The volume of a pair per candle bucket of each interval the route serves ([`super::candles::IVS`])
//! is a row of `pair_volume`, so a chart of `limit` candles reads at most `limit` rows per pair and
//! no fills, however many fills the window holds. Reading the fills cost up to 226 ms at a row cap
//! of 200,000, and the cap cut the volume of a busy pair on a long window without saying so.
//!
//! # Maintenance
//!
//! The triggers on `pair_fills` (schema.sql) mark the 1-minute bucket of every fill written,
//! changed or deleted in `pair_volume_dirty`: a block's fills, a revert, a replay, an operator's
//! import and the pruning of a lean store alike, also when an earlier build that keeps no volume
//! writes the store (its triggers are the store's). Before a commit of the writer, and at open,
//! [`refresh`] recomputes each marked minute from its fills with the fold the route always used (a
//! transaction counts once, at the larger of its two sides, [`super::pairs::tx_volumes`]) and rolls
//! it up into the 5 m, 1 h and 1 d buckets that hold it, as sums. A bucket with no fill has no row.
//! So after each of them the rows are exactly what the fills say.
//!
//! # First open
//!
//! [`backfill`] marks every minute of the existing fills once (`meta.pair_volume`) and refreshes
//! them, in one transaction.

use super::db::{meta_get, meta_set, DbResult};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeSet;

use super::candles::IVS;

/// One bucket's volume: (A base units, B base units, transactions), summed in 128 bits.
type Vol = (i128, i128, u64);

fn put(conn: &Connection, ids: (i64, i64), iv: i64, t: i64, v: Vol) -> DbResult<()> {
    if v.2 == 0 {
        conn.prepare_cached("DELETE FROM pair_volume WHERE base_id = ?1 AND quote_id = ?2 AND iv = ?3 AND t = ?4")?
            .execute(params![ids.0, ids.1, iv, t])?;
    } else {
        conn.prepare_cached(
            "INSERT OR REPLACE INTO pair_volume (base_id, quote_id, iv, t, vol_a, vol_b, txs) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![ids.0, ids.1, iv, t, v.0.to_string(), v.1.to_string(), v.2 as i64])?;
    }
    Ok(())
}

/// The volume of the fills of the pair in `[t, t + iv)`, from the fills.
fn from_fills(conn: &Connection, ids: (i64, i64), t: i64, iv: i64) -> DbResult<Vol> {
    let mut st = conn.prepare_cached(
        "SELECT txid, ts, side, amount_a, amount_b FROM pair_fills INDEXED BY pair_fills_pair_ts \
         WHERE base_id = ?1 AND quote_id = ?2 AND ts >= ?3 AND ts < ?4",
    )?;
    let rows = st
        .query_map(params![ids.0, ids.1, t, t.saturating_add(iv)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut v: Vol = (0, 0, 0);
    for (_, a, b) in super::pairs::tx_volumes(rows) {
        v = (v.0.saturating_add(a), v.1.saturating_add(b), v.2 + 1);
    }
    Ok(v)
}

/// The sum of the rows of interval `child` inside `[t, t + iv)`.
fn from_rows(conn: &Connection, ids: (i64, i64), child: i64, t: i64, iv: i64) -> DbResult<Vol> {
    let mut st = conn.prepare_cached(
        "SELECT vol_a, vol_b, txs FROM pair_volume WHERE base_id = ?1 AND quote_id = ?2 AND iv = ?3 AND t >= ?4 AND t < ?5",
    )?;
    let rows = st
        .query_map(params![ids.0, ids.1, child, t, t.saturating_add(iv)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut v: Vol = (0, 0, 0);
    for (a, b, n) in rows {
        v = (v.0.saturating_add(a.parse().unwrap_or(0)), v.1.saturating_add(b.parse().unwrap_or(0)), v.2 + n.max(0) as u64);
    }
    Ok(v)
}

/// Recomputes the marked minutes and the buckets that hold them, and clears the marks. Returns the
/// minutes recomputed. The writer runs it in every commit, and the open in its transaction.
pub fn refresh(conn: &Connection) -> DbResult<usize> {
    let dirty: Vec<(i64, i64, i64)> = conn
        .prepare_cached("SELECT base_id, quote_id, t FROM pair_volume_dirty")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if dirty.is_empty() {
        return Ok(0);
    }
    let mut level: BTreeSet<(i64, i64, i64)> = BTreeSet::new();
    for &(b, q, m) in &dirty {
        put(conn, (b, q), IVS[0], m, from_fills(conn, (b, q), m, IVS[0])?)?;
        level.insert((b, q, m));
    }
    for w in IVS.windows(2) {
        let (child, iv) = (w[0], w[1]);
        let parents: BTreeSet<(i64, i64, i64)> = level.iter().map(|&(b, q, t)| (b, q, t.div_euclid(iv) * iv)).collect();
        for &(b, q, t) in &parents {
            put(conn, (b, q), iv, t, from_rows(conn, (b, q), child, t, iv)?)?;
        }
        level = parents;
    }
    conn.execute("DELETE FROM pair_volume_dirty", [])?;
    Ok(dirty.len())
}

/// Builds the rows from every existing fill, once (`meta.pair_volume`), for a store whose fills an
/// earlier build wrote without them. Returns the minutes built, `None` when done before.
pub fn backfill(conn: &Connection) -> DbResult<Option<usize>> {
    if meta_get(conn, "pair_volume")?.is_some() {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DELETE FROM pair_volume; \
         INSERT OR IGNORE INTO pair_volume_dirty (base_id, quote_id, t) SELECT base_id, quote_id, ts - (ts % 60000) FROM pair_fills;",
    )?;
    let n = refresh(&tx)?;
    meta_set(&tx, "pair_volume", "1")?;
    tx.commit()?;
    Ok(Some(n))
}

/// The volume of `buckets` (starts at interval `iv`) of the pair of the ids `ids`: one row lookup
/// each, `None` for a bucket without fills.
pub fn read(conn: &Connection, ids: (i64, i64), iv: i64, buckets: &BTreeSet<i64>) -> DbResult<std::collections::BTreeMap<i64, Vol>> {
    let mut st =
        conn.prepare_cached("SELECT vol_a, vol_b, txs FROM pair_volume WHERE base_id = ?1 AND quote_id = ?2 AND iv = ?3 AND t = ?4")?;
    let mut out = std::collections::BTreeMap::new();
    for &t in buckets {
        let row: Option<(String, String, i64)> =
            st.query_row(params![ids.0, ids.1, iv, t], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        if let Some((a, b, n)) = row {
            out.insert(t, (a.parse().unwrap_or(0), b.parse().unwrap_or(0), n.max(0) as u64));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::db::open_memory;

    fn fills(conn: &Connection, from: i64, to: i64, t0: i64) {
        // Pairs of fills share a transaction (a netting fill counts once), across 3 days of minutes.
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, order_id, contract, base_id, quote_id, side, amount_a, amount_b, price, \
             a_scale, tip_kas, counterparty) SELECT 1 + i / 2, 1, ?3 + (i / 2) * 37000, CAST(printf('p%031d', i / 2) AS BLOB), i % 2, 1, 'KobPair', 7, 8, \
             1 + i % 2, 10 + i % 7, 20 + i % 5, 1000, 1000, 0, 'netting' FROM n",
            params![from, to, t0],
        )
        .unwrap();
    }

    /// Every bucket of every interval holds what the fills of that bucket say, after the fills are written, after some are
    /// reverted, and after a refresh of nothing; a bucket left without fills has no row.
    #[test]
    fn the_aggregates_are_the_fold_of_the_fills() {
        let conn = open_memory("testnet-10").unwrap();
        let t0 = 1_700_000_000_000i64;
        fills(&conn, 0, 14_000, t0);
        refresh(&conn).unwrap();
        let check = |conn: &Connection| {
            for iv in IVS {
                let ts: BTreeSet<i64> = conn
                    .prepare("SELECT DISTINCT ts FROM pair_fills")
                    .unwrap()
                    .query_map([], |r| r.get::<_, i64>(0))
                    .unwrap()
                    .map(|t| t.unwrap().div_euclid(iv) * iv)
                    .collect();
                let got = read(conn, (7, 8), iv, &ts).unwrap();
                for t in &ts {
                    assert_eq!(got.get(t).copied(), Some(from_fills(conn, (7, 8), *t, iv).unwrap()), "iv {iv} t {t}");
                }
                let rows: i64 = conn.query_row("SELECT COUNT(*) FROM pair_volume WHERE iv = ?1", [iv], |r| r.get(0)).unwrap();
                assert_eq!(rows as usize, ts.len(), "iv {iv}: a row per bucket with fills, no other");
            }
        };
        check(&conn);
        // a revert takes the newest blocks' fills away
        conn.execute("DELETE FROM pair_fills WHERE block_seq > 4000", []).unwrap();
        refresh(&conn).unwrap();
        check(&conn);
        assert_eq!(refresh(&conn).unwrap(), 0);
        // the first open of a store whose fills an earlier build wrote builds the same rows
        let before: Vec<(i64, i64, String, String, i64)> = conn
            .prepare("SELECT iv, t, vol_a, vol_b, txs FROM pair_volume ORDER BY iv, t")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        conn.execute_batch("DELETE FROM pair_volume; DELETE FROM meta WHERE k = 'pair_volume';").unwrap();
        assert!(backfill(&conn).unwrap().is_some());
        let after: Vec<(i64, i64, String, String, i64)> = conn
            .prepare("SELECT iv, t, vol_a, vol_b, txs FROM pair_volume ORDER BY iv, t")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(before, after);
        assert!(backfill(&conn).unwrap().is_none());
    }

    /// The first-open fill, measured (`cargo test -p kobd --lib pair_volume_first_open -- --ignored --nocapture`; 100,000 pair
    /// fills over about 25 days of minutes).
    #[test]
    #[ignore]
    fn pair_volume_first_open_time() {
        let conn = open_memory("testnet-10").unwrap();
        fills(&conn, 0, 100_000, 1_700_000_000_000);
        conn.execute_batch("DELETE FROM pair_volume; DELETE FROM pair_volume_dirty; DELETE FROM meta WHERE k = 'pair_volume';")
            .unwrap();
        let t = std::time::Instant::now();
        let n = backfill(&conn).unwrap().unwrap();
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM pair_volume", [], |r| r.get(0)).unwrap();
        eprintln!("pair volume of 100,000 pair fills: {:?}, {n} minutes, {rows} rows", t.elapsed());
    }
}
