//! Top-of-book history behind `GET /v1/quotes/{token}`: the mid line of the price chart.
//!
//! Once a wall-clock minute the follower samples the best bid and ask of every KAS book
//! ([`sample`]) and stores a row only when either changed since the token's last row. A side is the
//! best level's (price, scale), so a read converts it at the token's current price basis. The same
//! pass deletes rows older than [`RETENTION_MS`]. Samples are observations: a reorg does not
//! revert them.

use super::db::DbResult;
use super::market::{per_basis, price_basis};
use super::reads::{book_side_sorted, ReadCtx};
use crate::hex::Hash32;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::BTreeSet;

/// How long rows are kept (ms): 30 days.
pub const RETENTION_MS: i64 = 30 * 86_400_000;

const MINUTE: i64 = 60_000;

/// The best level of one side, (price, scale); `None` for an empty side.
type Side = Option<(i64, i64)>;

fn best(conn: &Connection, ctx: &ReadCtx, token: &Hash32, side: i64) -> DbResult<Side> {
    Ok(book_side_sorted(conn, ctx, token, side, Some(1), false)?.0.first().map(|l| l.0))
}

/// The tokens to sample: those with a live listed order, and those that have rows (so an emptied
/// book gets its row and old rows go). The second set is one index seek per token.
fn candidates(conn: &Connection) -> DbResult<BTreeSet<Vec<u8>>> {
    let mut out = BTreeSet::new();
    let mut st = conn.prepare_cached(
        "SELECT DISTINCT t.h FROM order_state s JOIN orders o ON o.id = s.order_id JOIN ids t ON t.id = o.token_id \
         WHERE s.status IN ('open','partial') AND o.listed = 1 AND o.in_book = 1",
    )?;
    for h in st.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
        out.insert(h?);
    }
    let mut st = conn.prepare_cached(
        "WITH RECURSIVE k(h) AS (SELECT MIN(token_cov_id) FROM book_quotes \
         UNION ALL SELECT (SELECT MIN(token_cov_id) FROM book_quotes WHERE token_cov_id > k.h) FROM k WHERE k.h IS NOT NULL) \
         SELECT h FROM k WHERE h IS NOT NULL",
    )?;
    for h in st.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
        out.insert(h?);
    }
    Ok(out)
}

fn side_of(p: Option<i64>, s: Option<i64>) -> Side {
    p.zip(s)
}

/// The newest row of a token: (t, bid, ask).
fn last(conn: &Connection, token: &[u8]) -> DbResult<Option<(i64, Side, Side)>> {
    Ok(conn
        .prepare_cached(
            "SELECT t, bid_price, bid_scale, ask_price, ask_scale FROM book_quotes WHERE token_cov_id = ?1 ORDER BY t DESC LIMIT 1",
        )?
        .query_row([token], |r| Ok((r.get(0)?, side_of(r.get(1)?, r.get(2)?), side_of(r.get(3)?, r.get(4)?))))
        .optional()?)
}

/// Samples the top of every KAS book at `now_ms` and returns the rows written or deleted.
///
/// A row is written only when the bid or the ask differs from the token's last row (no row counts
/// as two empty sides). Call it in one transaction, at most once a minute.
pub fn sample(conn: &Connection, ctx: &ReadCtx, now_ms: i64) -> DbResult<usize> {
    let t = now_ms - now_ms.rem_euclid(MINUTE);
    let mut changed = 0;
    for token in candidates(conn)? {
        changed += conn
            .prepare_cached("DELETE FROM book_quotes WHERE token_cov_id = ?1 AND t < ?2")?
            .execute(params![token, t.saturating_sub(RETENTION_MS)])?;
        let Ok(id) = <[u8; 32]>::try_from(token.as_slice()) else { continue };
        let id = Hash32(id);
        let (bid, ask) = (best(conn, ctx, &id, 2)?, best(conn, ctx, &id, 1)?);
        let prev = last(conn, &token)?;
        // A clock that stepped back writes nothing before the newest row.
        if prev.is_some_and(|p| p.0 > t) || prev.map_or((None, None), |p| (p.1, p.2)) == (bid, ask) {
            continue;
        }
        conn.prepare_cached("INSERT OR REPLACE INTO book_quotes VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?.execute(params![
            token,
            t,
            bid.map(|b| b.0),
            bid.map(|b| b.1),
            ask.map(|a| a.0),
            ask.map(|a| a.1)
        ])?;
        changed += 1;
    }
    Ok(changed)
}

/// The top-of-book history of one token at one interval. An item is `[t, bid, ask]`: the bucket
/// start and the last values in it, sompi per `price_basis` base units, `null` for an empty side.
#[derive(Debug, Clone, Serialize)]
pub struct QuotesView {
    pub token: String,
    pub interval: String,
    pub price_basis: String,
    pub decimals: Option<u32>,
    pub items: Vec<(i64, Option<String>, Option<String>)>,
}

/// The quotes of `token`, ascending, only the buckets with a change, at most `limit`.
///
/// Without `to` the window ends with the bucket of the newest row; without `from` it starts `limit`
/// intervals before. The value in force at the start of the window comes first, at its bucket.
#[allow(clippy::too_many_arguments)]
pub fn quotes(
    conn: &Connection,
    token: &Hash32,
    decimals: Option<u32>,
    interval: &str,
    iv: i64,
    from: Option<i64>,
    to: Option<i64>,
    limit: usize,
) -> DbResult<QuotesView> {
    let basis = price_basis(conn, token, decimals)?;
    let key = token.0.to_vec();
    let newest: Option<i64> =
        conn.prepare_cached("SELECT MAX(t) FROM book_quotes WHERE token_cov_id = ?1")?.query_row([&key], |r| r.get(0))?;
    let end = match (to, newest) {
        (Some(t), _) => t,
        (None, Some(n)) => n.div_euclid(iv).saturating_add(1).saturating_mul(iv),
        (None, None) => 0,
    };
    let start = from.unwrap_or_else(|| end.saturating_sub(iv.saturating_mul(limit as i64)));
    let (start, end) = super::market::whole_buckets(start, end, iv);
    // At most the `limit` intervals that end the window are read: the answer holds at most `limit` buckets, so an older
    // `from` would only read rows (one a minute) that no answer shows (R6C2b-2).
    let start = start.max(end.saturating_sub(iv.saturating_mul(limit as i64)));
    let mut rows: Vec<(i64, Side, Side)> = Vec::new();
    if end > start {
        let carry = conn
            .prepare_cached(
                "SELECT bid_price, bid_scale, ask_price, ask_scale FROM book_quotes WHERE token_cov_id = ?1 AND t < ?2 ORDER BY t DESC LIMIT 1",
            )?
            .query_row(params![key, start], |r| Ok((side_of(r.get(0)?, r.get(1)?), side_of(r.get(2)?, r.get(3)?))))
            .optional()?;
        // The last row of each bucket: SQLite takes the bare columns from the row of MAX(t).
        let mut st = conn.prepare_cached(
            "SELECT t - (t % ?2) AS b, bid_price, bid_scale, ask_price, ask_scale, MAX(t) FROM book_quotes \
             WHERE token_cov_id = ?1 AND t >= ?3 AND t < ?4 GROUP BY b ORDER BY b",
        )?;
        let v = st.query_map(params![key, iv, start, end], |r| {
            Ok((r.get::<_, i64>(0)?, side_of(r.get(1)?, r.get(2)?), side_of(r.get(3)?, r.get(4)?)))
        })?;
        rows = v.collect::<rusqlite::Result<_>>()?;
        let b0 = start.div_euclid(iv).saturating_mul(iv);
        if let Some((bid, ask)) = carry.filter(|_| rows.first().map(|r| r.0) != Some(b0)) {
            rows.insert(0, (b0, bid, ask));
        }
        if rows.len() > limit {
            rows.drain(..rows.len() - limit);
        }
    }
    let text = |s: Side| s.map(|(p, sc)| per_basis(p, sc, basis).to_string());
    Ok(QuotesView {
        token: token.to_hex(),
        interval: interval.to_string(),
        price_basis: basis.to_string(),
        decimals,
        items: rows.into_iter().map(|(t, b, a)| (t, text(b), text(a))).collect(),
    })
}

#[cfg(test)]
#[path = "quotes_tests.rs"]
mod tests;
