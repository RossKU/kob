//! Maintained candle aggregates behind `GET /v1/candles/{token}` and the pair charts.
//!
//! The candles are kept in the table `candles` for the four intervals the API serves ([`IVS`]). A
//! bucket is a row, written in the transaction that writes the fills it holds, so a read of 500
//! candles is 500 rows however long the history is. Folding every fill of the window on each
//! request would cost time that grows with the whole chart (measurements: `docs/ops/executor.md`,
//! section 5.3).
//!
//! # Maintenance
//!
//! A fill, and the revert of a block that had fills, marks its token's 1-minute bucket in
//! `candle_dirty` ([`mark`], [`mark_block`]). Before the transaction commits, [`refresh`]
//! recomputes every marked bucket from the fills, with the very fold of the reference path
//! ([`super::market::scan_candles`]). It rolls the bucket up into the 5 m, 1 h and 1 d buckets that
//! contain it: the open of the first, the close of the last, the highest high, the lowest low and
//! the sums. A bucket with no fill left has no row. Reorgs, replays and the operator's imports go
//! through the same two calls, so the aggregates are exactly what the fills say after each of them.
//!
//! # Price basis
//!
//! A trade's price is computed at the token's price basis: the standard scale of its decimals, else
//! the scale of its first order. The integer rounding of the price depends on the basis, so a
//! candle cannot be converted to another basis. `candle_meta` names the basis a token's rows were
//! built at. A read at another basis, for example once the token's decimals become known, gets no
//! aggregates and is answered from the fills until the next fill of the token rebuilds its rows at
//! the new basis ([`refresh`]).
//!
//! # Backfill
//!
//! [`backfill`] builds the rows of every token that has none, as on the first start on an existing
//! store. It works one token per transaction, so an interrupted run resumes. A token is read from
//! its aggregates only once its rows are complete.
//!
//! # Rollback
//!
//! A build that does not maintain the aggregates may open an upgraded store and write or revert
//! fills without touching them. Every transaction that maintains the aggregates records the newest
//! event id it saw in `meta.candles_event`. A start that finds another id discards the aggregates
//! and rebuilds them ([`backfill`]), so a rollback and a second upgrade never serve stale charts.
//!
//! # Price rule
//!
//! A change of how a trade is priced (`PRICE_RULE`) makes [`backfill`] rebuild every token's rows
//! once. Each token is rebuilt in one transaction that replaces its old rows, so readers never wait
//! and never see a gap.
//!
//! # Lean stores
//!
//! Lean stores prune the fills they chart, so they keep no aggregates (`meta.candles` is not `on`,
//! see [`enabled`]).
//!
//! The aggregates answer exactly what the fills do. The tests compare them with the reference over
//! random histories, reverts and unaligned windows, and over a million-fill history.

use super::db::{meta_get, meta_set, DbResult};
use super::market::{add_trade, for_each_trade, scan_candles, Candle};
use super::reads;
use crate::hex::Hash32;
use crate::tokens::TokenAllowlist;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::VecDeque;

/// The intervals (ms) the aggregates keep.
///
/// Each is a multiple of the one before: the 1 m base and what rolls up from it.
pub const IVS: [i64; 4] = [60_000, 300_000, 3_600_000, 86_400_000];

const BASE: i64 = IVS[0];

/// The version of the trade price rule the stored rows were built with (`meta.candles_rule`).
///
/// [`backfill`] rebuilds the rows built with another one.
const PRICE_RULE: &str = "2";

/// Whether this store maintains aggregates.
///
/// That is `meta.candles = on`, which is set when the store is opened as an archive and never on a
/// lean store.
pub fn enabled(conn: &Connection) -> bool {
    meta_get(conn, "candles").ok().flatten().as_deref() == Some("on")
}

/// Turns the aggregates on for an archive store, or off for a lean store, and empties them.
///
/// A lean store prunes its fills, so the aggregates would outlive them.
pub fn configure(conn: &Connection, lean: bool) -> DbResult<()> {
    if lean {
        if meta_get(conn, "candles")?.as_deref() != Some("off") {
            conn.execute_batch("DELETE FROM candles; DELETE FROM candle_meta; DELETE FROM candle_dirty;")?;
            meta_set(conn, "candles", "off")?;
        }
    } else if meta_get(conn, "candles")?.as_deref() != Some("on") {
        // A store that had no aggregates: whatever rows it gets are built by the current rule.
        meta_set(conn, "candles", "on")?;
        meta_set(conn, "candles_rule", PRICE_RULE)?;
    }
    Ok(())
}

fn bucket(ts: i64, iv: i64) -> i64 {
    ts.div_euclid(iv).saturating_mul(iv)
}

/// Marks the 1-minute bucket of a fill of `token` at `ts` (ms) for [`refresh`].
pub fn mark(conn: &Connection, token: &[u8], ts: i64) -> DbResult<()> {
    if enabled(conn) {
        conn.prepare_cached("INSERT OR IGNORE INTO candle_dirty (token_cov_id, t) VALUES (?1, ?2)")?
            .execute(params![token, bucket(ts, BASE)])?;
    }
    Ok(())
}

/// Marks the buckets of the fills that chain block `seq` holds.
///
/// Call it before the events are deleted.
pub fn mark_block(conn: &Connection, seq: i64) -> DbResult<()> {
    if enabled(conn) {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO candle_dirty (token_cov_id, t) \
             SELECT DISTINCT tk.h, e.ts - ((e.ts % 60000) + 60000) % 60000 FROM order_events e JOIN ids tk ON tk.id = e.token_id \
             WHERE e.block_seq = ?1 AND e.kind = 'fill'",
        )?
        .execute([seq])?;
    }
    Ok(())
}

/// The price basis of a token as the API resolves it.
///
/// It uses [`reads::token_decimals`] and [`super::market::price_basis`].
pub fn token_basis(conn: &Connection, tokens: &TokenAllowlist, token: &Hash32) -> DbResult<i128> {
    let decimals = reads::token_decimals(conn, tokens, token)?;
    super::market::price_basis(conn, token, decimals)
}

/// The newest order-event id. The id is an autoincrement, so a revert followed by new events never
/// repeats one.
fn newest_event(conn: &Connection) -> DbResult<i64> {
    Ok(conn.prepare_cached("SELECT COALESCE(MAX(id), 0) FROM order_events")?.query_row([], |r| r.get(0))?)
}

fn record_position(conn: &Connection) -> DbResult<()> {
    let n = newest_event(conn)?.to_string();
    if meta_get(conn, "candles_event")?.as_deref() != Some(n.as_str()) {
        meta_set(conn, "candles_event", &n)?;
    }
    Ok(())
}

fn meta_basis(conn: &Connection, token: &[u8]) -> DbResult<Option<i128>> {
    Ok(conn
        .prepare_cached("SELECT basis FROM candle_meta WHERE token_cov_id = ?1")?
        .query_row([token], |r| r.get::<_, i64>(0))
        .optional()?
        .map(i128::from))
}

/// Recomputes every marked bucket and returns the number recomputed.
///
/// Call it inside the transaction that wrote the fills, before it commits. A token with no rows
/// yet, or with rows at another basis, is rebuilt whole.
pub fn refresh(conn: &Connection, tokens: &TokenAllowlist) -> DbResult<usize> {
    if !enabled(conn) {
        return Ok(0);
    }
    let mut done = 0;
    loop {
        let token: Option<Vec<u8>> =
            conn.prepare_cached("SELECT token_cov_id FROM candle_dirty LIMIT 1")?.query_row([], |r| r.get(0)).optional()?;
        let Some(token) = token else {
            // The aggregates are current with every event up to this id.
            record_position(conn)?;
            return Ok(done);
        };
        let minutes: Vec<i64> = {
            let mut st = conn.prepare_cached("SELECT t FROM candle_dirty WHERE token_cov_id = ?1")?;
            let v = st.query_map([&token], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            v
        };
        conn.prepare_cached("DELETE FROM candle_dirty WHERE token_cov_id = ?1")?.execute([&token])?;
        let Ok(id) = <[u8; 32]>::try_from(token.as_slice()) else { continue };
        let id = Hash32(id);
        let basis = token_basis(conn, tokens, &id)?;
        if meta_basis(conn, &token)? == Some(basis) {
            for m in &minutes {
                refresh_minute(conn, &id, basis, *m)?;
            }
            done += minutes.len();
        } else {
            done += rebuild(conn, &id, basis)?;
        }
    }
}

/// Recomputes the 1-minute bucket starting at `m` from the fills, and the buckets that contain it
/// from the ones below.
fn refresh_minute(conn: &Connection, token: &Hash32, basis: i128, m: i64) -> DbResult<()> {
    let base = scan_candles(conn, token, basis, BASE, m, m.saturating_add(BASE), 1)?.into_iter().next();
    put(conn, &token.0, BASE, m, base.as_ref())?;
    for pair in IVS.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        let b = bucket(m, hi);
        let rows = rows_between(conn, &token.0, lo, b, b.saturating_add(hi), usize::MAX, false)?;
        put(conn, &token.0, hi, b, rollup(&rows, b).as_ref())?;
    }
    Ok(())
}

/// The candle that the sub-buckets `rows` make up, or `None` for no rows.
///
/// The rows are ascending and all inside bucket `t`.
fn rollup(rows: &[Candle], t: i64) -> Option<Candle> {
    let (first, last) = (rows.first()?, rows.last()?);
    Some(Candle {
        t,
        o: first.o,
        h: rows.iter().map(|c| c.h).max()?,
        l: rows.iter().map(|c| c.l).min()?,
        c: last.c,
        volume: rows.iter().fold(0i128, |s, c| s.saturating_add(c.volume)),
        quote_volume: rows.iter().fold(0i128, |s, c| s.saturating_add(c.quote_volume)),
        trades: rows.iter().map(|c| c.trades).sum(),
    })
}

/// Writes the row of bucket `t` of interval `iv`, or deletes it for `None`.
fn put(conn: &Connection, token: &[u8], iv: i64, t: i64, c: Option<&Candle>) -> DbResult<()> {
    match c {
        Some(c) => {
            conn.prepare_cached(
                "INSERT OR REPLACE INTO candles (token_cov_id, iv, t, o, h, l, c, volume, quote_volume, trades) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                token,
                iv,
                t,
                c.o.to_string(),
                c.h.to_string(),
                c.l.to_string(),
                c.c.to_string(),
                c.volume.to_string(),
                c.quote_volume.to_string(),
                c.trades as i64
            ])?;
        }
        None => {
            conn.prepare_cached("DELETE FROM candles WHERE token_cov_id = ?1 AND iv = ?2 AND t = ?3")?
                .execute(params![token, iv, t])?;
        }
    }
    Ok(())
}

/// The rows of interval `iv` with `from <= t < to`, ascending.
///
/// With `newest`, only the newest `limit` of them are returned, still ascending.
fn rows_between(conn: &Connection, token: &[u8], iv: i64, from: i64, to: i64, limit: usize, newest: bool) -> DbResult<Vec<Candle>> {
    let sql = format!(
        "SELECT t, o, h, l, c, volume, quote_volume, trades FROM candles WHERE token_cov_id = ?1 AND iv = ?2 AND t >= ?3 AND t < ?4 \
         ORDER BY t {} LIMIT ?5",
        if newest { "DESC" } else { "ASC" }
    );
    let mut st = conn.prepare_cached(&sql)?;
    let num = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<i128> {
        r.get::<_, String>(i)?
            .parse::<i128>()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))
    };
    let mut v = st
        .query_map(params![token, iv, from, to, limit.min(i64::MAX as usize) as i64], |r| {
            Ok(Candle {
                t: r.get(0)?,
                o: num(r, 1)?,
                h: num(r, 2)?,
                l: num(r, 3)?,
                c: num(r, 4)?,
                volume: num(r, 5)?,
                quote_volume: num(r, 6)?,
                trades: r.get::<_, i64>(7)? as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if newest {
        v.reverse();
    }
    Ok(v)
}

/// Rebuilds every row of `token` at `basis` from its fills and returns the rows written.
///
/// It makes one pass over the fills, folding each trade into the four intervals exactly as the
/// reference path folds it, and records the basis.
pub fn rebuild(conn: &Connection, token: &Hash32, basis: i128) -> DbResult<usize> {
    conn.prepare_cached("DELETE FROM candles WHERE token_cov_id = ?1")?.execute([&token.0[..]])?;
    let mut open: [VecDeque<Candle>; 4] = Default::default();
    let mut written = 0usize;
    for_each_trade(conn, token, basis, i64::MIN, i64::MAX, |t| {
        for (k, iv) in IVS.iter().enumerate() {
            add_trade(&mut open[k], &t, *iv);
            // A trade is newer than every one before it, so the bucket before the newest is
            // complete.
            while open[k].len() > 1 {
                let done = open[k].pop_front().expect("two buckets");
                put(conn, &token.0, *iv, done.t, Some(&done))?;
                written += 1;
            }
        }
        Ok(())
    })?;
    for (k, iv) in IVS.iter().enumerate() {
        while let Some(done) = open[k].pop_front() {
            put(conn, &token.0, *iv, done.t, Some(&done))?;
            written += 1;
        }
    }
    conn.prepare_cached("INSERT OR REPLACE INTO candle_meta (token_cov_id, basis) VALUES (?1, ?2)")?
        .execute(params![&token.0[..], i64::try_from(basis).unwrap_or(i64::MAX)])?;
    Ok(written)
}

/// What a [`backfill`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Backfill {
    /// Tokens whose rows were rebuilt.
    pub tokens: usize,
    /// Rows written.
    pub rows: usize,
}

/// Builds the rows of every token that has fills and none, or none at its current basis.
///
/// It uses one transaction per token. It does nothing on a store that is up to date, on a lean
/// store and on a store with no fills.
pub fn backfill(conn: &Connection, tokens: &TokenAllowlist) -> DbResult<Backfill> {
    let mut report = Backfill::default();
    if !enabled(conn) {
        return Ok(report);
    }
    // The trade price rule changed. Rule 2 takes the age of an amended order from the DAA of its
    // latest amend, not of its placement. Every token's rows are rebuilt from the fills. Nothing is
    // deleted first: a token's rows are replaced in one transaction ([`rebuild`]), so a reader sees
    // the old rows until the new ones commit, and never an empty or half-built chart.
    let stale_rule = meta_get(conn, "candles_rule")?.as_deref() != Some(PRICE_RULE);
    // Fills written or reverted by a build that did not maintain the aggregates cannot be trusted,
    // so all of them are rebuilt.
    let now = newest_event(conn)?.to_string();
    if meta_get(conn, "candles_event")?.is_some_and(|m| m != now) {
        conn.execute_batch("DELETE FROM candles; DELETE FROM candle_meta; DELETE FROM candle_dirty;")?;
        tracing::warn!(
            "the fills changed since the candle aggregates were last maintained (a rollback to an earlier build?): rebuilding them"
        );
    }
    // The distinct tokens that have fills: one index seek each (`order_events_fills`).
    let mut next = conn.prepare_cached("SELECT MIN(token_id) FROM order_events WHERE kind = 'fill' AND token_id > ?1")?;
    let mut hash = conn.prepare_cached("SELECT h FROM ids WHERE id = ?1")?;
    let mut after: i64 = 0;
    while let Some(token_id) = next.query_row([after], |r| r.get::<_, Option<i64>>(0))? {
        after = token_id;
        let token: Vec<u8> = hash.query_row([token_id], |r| r.get(0))?;
        let Ok(id) = <[u8; 32]>::try_from(token.as_slice()) else { continue };
        let id = Hash32(id);
        let basis = token_basis(conn, tokens, &id)?;
        if !stale_rule && meta_basis(conn, &token)? == Some(basis) {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        report.rows += rebuild(&tx, &id, basis)?;
        // A rebuild covers everything marked so far.
        tx.execute("DELETE FROM candle_dirty WHERE token_cov_id = ?1", [&token])?;
        tx.commit()?;
        report.tokens += 1;
    }
    meta_set(conn, "candles_event", &now)?;
    // Every token is rebuilt by the rule, so an interrupted run starts over with the rule unmarked.
    if stale_rule {
        meta_set(conn, "candles_rule", PRICE_RULE)?;
    }
    Ok(report)
}

/// `Some(a)` for the first multiple of `iv` at or after `x`, and `None` beyond `i64`.
fn ceil_to(x: i64, iv: i64) -> Option<i64> {
    let q = (x as i128 + iv as i128 - 1).div_euclid(iv as i128) * iv as i128;
    i64::try_from(q).ok()
}

/// The candles of `token` over `[from, to)` at interval `iv`, read from the aggregates.
///
/// It returns the newest `keep` of the buckets whose trades are in the window. The result is `None`
/// when the aggregates cannot answer: they are off, the interval is not kept, the token has no rows
/// at this basis, or the window does not contain a whole bucket. The caller then reads the fills. A
/// window that starts or ends inside a bucket has that partial bucket read from the fills, at most
/// one bucket on each side.
pub fn read(
    conn: &Connection,
    token: &Hash32,
    basis: i128,
    iv: i64,
    from: i64,
    to: i64,
    keep: usize,
) -> DbResult<Option<Vec<Candle>>> {
    if !IVS.contains(&iv) || !enabled(conn) || meta_basis(conn, &token.0)? != Some(basis) {
        return Ok(None);
    }
    if to <= from || keep == 0 {
        return Ok(Some(vec![]));
    }
    // The whole buckets inside the window: [a, b).
    let (Some(a), b) = (ceil_to(from, iv), bucket(to, iv)) else { return Ok(None) };
    if a >= b {
        return Ok(None);
    }
    let mut out: Vec<Candle> = Vec::new();
    if from < a {
        out.extend(scan_candles(conn, token, basis, iv, from, a, keep)?);
    }
    out.extend(rows_between(conn, &token.0, iv, a, b, keep, true)?);
    if b < to {
        out.extend(scan_candles(conn, token, basis, iv, b, to, keep)?);
    }
    if out.len() > keep {
        out.drain(..out.len() - keep);
    }
    Ok(Some(out))
}

#[cfg(test)]
#[path = "candles_tests.rs"]
mod tests;
