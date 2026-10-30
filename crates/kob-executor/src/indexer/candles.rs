//! Maintained candle aggregates: `GET /v1/candles/{token}` and the pair charts without reading the history.
//!
//! The candles of a token were folded from every fill of the window on each request (the cost grew with the fills of the whole chart:
//! 3 s for `1h` and 2.3 s for `1d` on the soak's 982,000 fills of one token, a 504 on the slower executor). They are now kept in the
//! table `candles` for the four intervals the API serves ([`IVS`]): a bucket is a row, written in the transaction that writes the fills
//! it holds, so a read of 500 candles is 500 rows however long the history is.
//!
//! **Maintenance.** A fill (and the revert of a block that had fills) marks its token's 1-minute bucket in `candle_dirty`
//! ([`mark`], [`mark_block`]); before the transaction commits, [`refresh`] recomputes every marked bucket from the fills (the very
//! fold of the reference path, [`super::market::scan_candles`]) and rolls it up into the 5 m, 1 h and 1 d buckets that contain it
//! (open of the first, close of the last, highest high, lowest low, the sums). A bucket with no fill left has no row. Reorgs, replays
//! and the operator's imports go through the same two calls, so the aggregates are exactly what the fills say after each of them.
//!
//! **Price basis.** A trade's price is computed at the token's price basis (its decimals' standard scale, else the scale of its first
//! order), and the integer rounding of it depends on the basis, so a candle cannot be converted to another basis. `candle_meta` names
//! the basis a token's rows were built at; a read at another basis (the token's decimals became known) gets none, and is answered from
//! the fills as before until the next fill of the token rebuilds its rows at the new basis ([`refresh`]).
//!
//! **Backfill.** [`backfill`] builds the rows of every token that has none (the first start of this build on an existing store), one
//! token per transaction, so an interrupted run resumes; a token is read from its aggregates only once its rows are complete.
//!
//! **Rollback.** An earlier build opens an upgraded store and writes fills (or reverts them) without touching the aggregates. Every
//! transaction that maintains them records the newest event id it saw (`meta.candles_event`); a start that finds another id has
//! the aggregates discarded and rebuilt ([`backfill`]), so a rollback and a second upgrade never serve stale charts.
//!
//! **Lean stores** prune the fills they chart, so they keep no aggregates (`meta.candles` is not `on`; [`enabled`]).
//!
//! The aggregates answer exactly what the fills do: `tests` compare them with the reference over random histories, reverts and
//! unaligned windows, and over a million-fill history.

use super::db::{meta_get, meta_set, DbResult};
use super::market::{add_trade, for_each_trade, scan_candles, Candle};
use super::reads;
use crate::hex::Hash32;
use crate::tokens::TokenAllowlist;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::VecDeque;

/// The intervals (ms) the aggregates keep, each a multiple of the one before: the 1 m base and what rolls up from it.
pub const IVS: [i64; 4] = [60_000, 300_000, 3_600_000, 86_400_000];

const BASE: i64 = IVS[0];

/// Whether this store maintains aggregates (`meta.candles = on`: set when the store is opened as an archive, never on a lean store).
pub fn enabled(conn: &Connection) -> bool {
    meta_get(conn, "candles").ok().flatten().as_deref() == Some("on")
}

/// Turns the aggregates on (an archive store) or off and empties them (a lean store: its fills are pruned, the aggregates would
/// outlive them).
pub fn configure(conn: &Connection, lean: bool) -> DbResult<()> {
    if lean {
        if meta_get(conn, "candles")?.as_deref() != Some("off") {
            conn.execute_batch("DELETE FROM candles; DELETE FROM candle_meta; DELETE FROM candle_dirty;")?;
            meta_set(conn, "candles", "off")?;
        }
    } else if meta_get(conn, "candles")?.as_deref() != Some("on") {
        meta_set(conn, "candles", "on")?;
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

/// Marks the buckets of the fills chain block `seq` holds (call before its events are deleted).
pub fn mark_block(conn: &Connection, seq: i64) -> DbResult<()> {
    if enabled(conn) {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO candle_dirty (token_cov_id, t) \
             SELECT DISTINCT token_cov_id, ts - ((ts % 60000) + 60000) % 60000 FROM order_events \
             WHERE block_seq = ?1 AND kind = 'fill' AND token_cov_id IS NOT NULL",
        )?
        .execute([seq])?;
    }
    Ok(())
}

/// The price basis of a token as the API resolves it ([`reads::token_decimals`], [`super::market::price_basis`]).
pub fn token_basis(conn: &Connection, tokens: &TokenAllowlist, token: &Hash32) -> DbResult<i128> {
    let decimals = reads::token_decimals(conn, tokens, token)?;
    super::market::price_basis(conn, token, decimals)
}

/// The newest order-event id (autoincrement: a revert followed by new events never repeats an id).
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

/// Recomputes every marked bucket (call inside the transaction that wrote the fills, before it commits). A token with no rows yet, or
/// rows at another basis, is rebuilt whole. Returns the buckets recomputed.
pub fn refresh(conn: &Connection, tokens: &TokenAllowlist) -> DbResult<usize> {
    if !enabled(conn) {
        return Ok(0);
    }
    let mut done = 0;
    loop {
        let token: Option<Vec<u8>> =
            conn.prepare_cached("SELECT token_cov_id FROM candle_dirty LIMIT 1")?.query_row([], |r| r.get(0)).optional()?;
        let Some(token) = token else {
            // the aggregates are current with every event up to this id
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

/// Recomputes the 1-minute bucket starting at `m` from the fills, and the buckets that contain it from the ones below.
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

/// The candle the sub-buckets `rows` (ascending, all inside bucket `t`) make up; `None` for no rows.
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

/// Writes (or, for `None`, deletes) the row of bucket `t` of interval `iv`.
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

/// The rows of interval `iv` with `from <= t < to`, ascending; with `newest` the newest `limit` of them (still ascending).
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

/// Rebuilds every row of `token` at `basis` from its fills, in one pass over them (each trade is folded into the four intervals
/// exactly as the reference path folds it), and records the basis. Returns the rows written.
pub fn rebuild(conn: &Connection, token: &Hash32, basis: i128) -> DbResult<usize> {
    conn.prepare_cached("DELETE FROM candles WHERE token_cov_id = ?1")?.execute([&token.0[..]])?;
    let mut open: [VecDeque<Candle>; 4] = Default::default();
    let mut written = 0usize;
    for_each_trade(conn, token, basis, i64::MIN, i64::MAX, |t| {
        for (k, iv) in IVS.iter().enumerate() {
            add_trade(&mut open[k], &t, *iv);
            // a trade is newer than every one before it: the bucket before the newest is complete
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
    pub tokens: usize,
    pub rows: usize,
}

/// Builds the rows of every token that has fills and none (or none at its current basis). One transaction per token. A no-op on a
/// store that is up to date, a lean store, and a store with no fills.
pub fn backfill(conn: &Connection, tokens: &TokenAllowlist) -> DbResult<Backfill> {
    let mut report = Backfill::default();
    if !enabled(conn) {
        return Ok(report);
    }
    // fills written or reverted by a build that did not maintain the aggregates: they are not to be trusted, all of them are rebuilt
    let now = newest_event(conn)?.to_string();
    if meta_get(conn, "candles_event")?.is_some_and(|m| m != now) {
        conn.execute_batch("DELETE FROM candles; DELETE FROM candle_meta; DELETE FROM candle_dirty;")?;
        tracing::warn!(
            "the fills changed since the candle aggregates were last maintained (a rollback to an earlier build?): rebuilding them"
        );
    }
    // the distinct tokens that have fills: one index seek each (`order_events_fills`)
    let mut next = conn.prepare_cached("SELECT MIN(token_cov_id) FROM order_events WHERE kind = 'fill' AND token_cov_id > ?1")?;
    let mut after: Vec<u8> = Vec::new();
    while let Some(token) = next.query_row([&after], |r| r.get::<_, Option<Vec<u8>>>(0))? {
        after.clone_from(&token);
        let Ok(id) = <[u8; 32]>::try_from(token.as_slice()) else { continue };
        let id = Hash32(id);
        let basis = token_basis(conn, tokens, &id)?;
        if meta_basis(conn, &token)? == Some(basis) {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        report.rows += rebuild(&tx, &id, basis)?;
        // a rebuild covers everything marked so far
        tx.execute("DELETE FROM candle_dirty WHERE token_cov_id = ?1", [&token])?;
        tx.commit()?;
        report.tokens += 1;
    }
    meta_set(conn, "candles_event", &now)?;
    Ok(report)
}

/// `Some(a)` for the first multiple of `iv` at or after `x` (`None` beyond `i64`).
fn ceil_to(x: i64, iv: i64) -> Option<i64> {
    let q = (x as i128 + iv as i128 - 1).div_euclid(iv as i128) * iv as i128;
    i64::try_from(q).ok()
}

/// The candles of `token` over `[from, to)` at interval `iv`, from the aggregates: the newest `keep` of the buckets whose trades are in
/// the window. `None`: the aggregates cannot answer (they are off, the interval is not kept, the token has no rows at this basis, or
/// the window does not contain a whole bucket); the caller reads the fills. A window that starts or ends inside a bucket has that
/// partial bucket read from the fills (at most one bucket each side).
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
    // the whole buckets inside the window: [a, b)
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
