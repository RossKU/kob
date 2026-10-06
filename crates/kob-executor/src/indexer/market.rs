//! Market data derived from the indexed fills: trades, OHLCV candles, 24 h statistics and depth snapshots
//! (`GET /v1/trades/{token}`, `/v1/candles/{token}`, `/v1/stats/{token}`, `/v1/depth/{token}`; `docs/ops/executor.md` B 5.3).
//!
//! **Trades.** The indexer stores one `fill` event per order and fill: its `amount` (token base units) and its `price` (the
//! order's quote at the fill, sompi per whole token, i.e. per `scale` base units of the order). A trade is every fill event of
//! ONE transaction on ONE token. Its `amount` (token base units moved from sellers to buyers) is the sum of the ask-side fills'
//! amounts, and its `quote` the same fills' `quoteOf(amount, price, scale)` (rounded down, sompi). A transaction without
//! ask-side fills (a wallet taking a resting bid directly, e.g. swap-and-pay) is measured on its bid side. The **resting** side
//! is the side whose orders are older (smaller `genesis_daa`); it sets the trade price (its volume-weighted price) and the other
//! side is the aggressor (`side`: `buy` when the aggressor bought, i.e. asks were resting). With one side only, that side is
//! resting and the aggressor is the direct taker.
//!
//! **Only KAS-book fills make prices** (founder rule): trades, candles, last prices and statistics are per token in KAS and
//! come only from fills of the KAS-quoted kinds (plain, conditional and if-done). A pair-order fill (`KobPair`, `KobCondPair`,
//! `KobIfdPair`) is stored with a NULL price and never enters these views; its volume is in `pair_fills`
//! (`indexer::pairs`). The KAS-book counterparties of a routed pair fill are ordinary KAS fills of their own tokens.
//!
//! **Price basis.** Prices are sompi per `price_basis` token base units: the token's standard scale (`10^decimals` of its
//! registry entry, at most `10^9`: sompi per whole token) when the registry knows its decimals, else the scale of the first
//! order seen. A fill at another scale is converted (`price × basis / scale`). Every view also carries `decimals` (the
//! registry's, `null` without an entry) so a client can show whole tokens. Integer arithmetic (i128, saturating), rounded
//! down.

use crate::hex::Hash32;
use crate::indexer::db::DbResult;
use crate::indexer::reads::{self, BookSide, ReadCtx};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::BTreeMap;

/// One fill event joined with its order's scale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillRow {
    pub id: i64,
    pub txid: Vec<u8>,
    pub ts: i64,
    pub daa: i64,
    /// 1 ask side (sells the token), 2 bid side.
    pub side: i64,
    /// Base units filled.
    pub amount: i64,
    /// Sompi per whole token (`scale` base units).
    pub price: i64,
    /// Base units per whole token of the filled order.
    pub scale: i64,
    pub genesis_daa: i64,
}

/// A trade (see the module docs). Amounts in base units / sompi, price per basis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trade {
    pub id: i64,
    pub txid: Vec<u8>,
    pub ts: i64,
    pub daa: i64,
    pub price: i128,
    pub amount: i128,
    pub quote: i128,
    /// true when the aggressor bought
    pub buy: bool,
    pub fills: usize,
}

/// `price` sompi per `scale` base units as sompi per `basis` base units (rounded down).
fn per_basis(price: i64, scale: i64, basis: i128) -> i128 {
    (price as i128).saturating_mul(basis) / scale.max(1) as i128
}

/// Per side: (base units, sompi at the fills' quotes, base units x price per basis, oldest genesis DAA).
fn side_sums(rows: &[&FillRow], basis: i128) -> (i128, i128, i128, i64) {
    let mut amount = 0i128;
    let mut quote = 0i128;
    let mut weighted = 0i128;
    let mut oldest = i64::MAX;
    for r in rows {
        let n = r.amount.max(0) as i128;
        amount = amount.saturating_add(n);
        quote = quote.saturating_add(n.saturating_mul(r.price as i128) / r.scale.max(1) as i128);
        weighted = weighted.saturating_add(n.saturating_mul(per_basis(r.price, r.scale, basis)));
        oldest = oldest.min(r.genesis_daa);
    }
    (amount, quote, weighted, oldest)
}

/// Builds the trade of the fills of one transaction (all of one token). `None` when nothing moved.
pub fn trade_of(fills: &[FillRow], basis: i128) -> Option<Trade> {
    let first = fills.first()?;
    let asks: Vec<&FillRow> = fills.iter().filter(|f| f.side == 1).collect();
    let bids: Vec<&FillRow> = fills.iter().filter(|f| f.side == 2).collect();
    let (a_amt, a_quote, a_w, a_age) = side_sums(&asks, basis);
    let (b_amt, b_quote, b_w, b_age) = side_sums(&bids, basis);
    // the volume moved: the ask side's tokens, or the bid side's when no ask filled
    let (amount, quote_moved) = if a_amt > 0 { (a_amt, a_quote) } else { (b_amt, b_quote) };
    if amount <= 0 || basis <= 0 {
        return None;
    }
    // the resting side sets the price; ties (same genesis DAA) go to the asks
    let asks_rest = if a_amt > 0 && b_amt > 0 { a_age <= b_age } else { a_amt > 0 };
    let (r_amt, r_w) = if asks_rest { (a_amt, a_w) } else { (b_amt, b_w) };
    Some(Trade {
        id: fills.iter().map(|f| f.id).min().unwrap_or(first.id),
        txid: first.txid.clone(),
        ts: fills.iter().map(|f| f.ts).max().unwrap_or(first.ts),
        daa: fills.iter().map(|f| f.daa).max().unwrap_or(first.daa),
        price: r_w / r_amt,
        amount,
        quote: quote_moved,
        buy: asks_rest,
        fills: fills.len(),
    })
}

/// Groups fills (any order) into trades by transaction, oldest first.
pub fn trades_of(mut fills: Vec<FillRow>, basis: i128) -> Vec<Trade> {
    fills.sort_by_key(|f| f.id);
    let mut groups: BTreeMap<Vec<u8>, Vec<FillRow>> = BTreeMap::new();
    let mut order: Vec<Vec<u8>> = Vec::new();
    for f in fills {
        if !groups.contains_key(&f.txid) {
            order.push(f.txid.clone());
        }
        groups.entry(f.txid.clone()).or_default().push(f);
    }
    let mut out: Vec<Trade> = order.iter().filter_map(|t| trade_of(&groups[t], basis)).collect();
    out.sort_by_key(|t| t.id);
    out
}

/// Candle intervals the API serves: name and length in milliseconds.
pub const INTERVALS: [(&str, i64); 4] = [("1m", 60_000), ("5m", 300_000), ("1h", 3_600_000), ("1d", 86_400_000)];

pub fn interval_ms(name: &str) -> Option<i64> {
    INTERVALS.iter().find(|(n, _)| *n == name).map(|(_, ms)| *ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candle {
    pub t: i64,
    pub o: i128,
    pub h: i128,
    pub l: i128,
    pub c: i128,
    pub volume: i128,
    pub quote_volume: i128,
    pub trades: u64,
}

/// OHLCV per bucket (`floor(ts / interval) * interval`), ascending; empty buckets are omitted. `trades` must be oldest first.
pub fn candles_of(trades: &[Trade], interval: i64) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for t in trades {
        let b = t.ts.div_euclid(interval) * interval;
        match out.last_mut() {
            Some(c) if c.t == b => {
                c.h = c.h.max(t.price);
                c.l = c.l.min(t.price);
                c.c = t.price;
                c.volume = c.volume.saturating_add(t.amount);
                c.quote_volume = c.quote_volume.saturating_add(t.quote);
                c.trades += 1;
            }
            _ => out.push(Candle {
                t: b,
                o: t.price,
                h: t.price,
                l: t.price,
                c: t.price,
                volume: t.amount,
                quote_volume: t.quote,
                trades: 1,
            }),
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------ SQL

const FILL_SELECT: &str =
    "SELECT e.id, e.txid, e.ts, e.daa, e.side, e.amount, e.price, COALESCE(o.scale, 1), o.genesis_daa \
     FROM order_events e JOIN orders o ON o.covenant_id = e.covenant_id \
     WHERE e.kind = 'fill' AND e.token_cov_id = ?1 AND e.amount IS NOT NULL AND e.amount > 0 AND e.price IS NOT NULL AND e.side IN (1, 2)";

fn fill_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FillRow> {
    Ok(FillRow {
        id: r.get(0)?,
        txid: r.get(1)?,
        ts: r.get(2)?,
        daa: r.get(3)?,
        side: r.get(4)?,
        amount: r.get(5)?,
        price: r.get(6)?,
        scale: r.get(7)?,
        genesis_daa: r.get(8)?,
    })
}

/// The price basis of a token: its standard scale (`10^decimals`, at most `10^9`) when its decimals are known, else the scale
/// of its first order, else 1.
pub fn price_basis(conn: &Connection, token: &Hash32, decimals: Option<u32>) -> DbResult<i128> {
    if let Some(d) = decimals {
        return Ok(kob_protocol::defaults::default_scale(d) as i128);
    }
    let scale: Option<i64> = conn
        .query_row(
            "SELECT COALESCE(scale, 1) FROM orders WHERE token_cov_id = ?1 ORDER BY genesis_block, covenant_id LIMIT 1",
            params![token.0.to_vec()],
            |r| r.get(0),
        )
        .ok();
    Ok(scale.filter(|s| *s > 0).unwrap_or(1) as i128)
}

/// Fills of `token` with `id < before` (newest first), at most `limit` rows, returned oldest first.
fn fills_before(conn: &Connection, token: &Hash32, before: Option<i64>, limit: usize) -> DbResult<Vec<FillRow>> {
    let sql = format!("{FILL_SELECT} AND e.id < ?2 ORDER BY e.id DESC LIMIT ?3");
    let mut st = conn.prepare(&sql)?;
    let mut v = st
        .query_map(params![token.0.to_vec(), before.unwrap_or(i64::MAX), limit as i64], fill_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    v.reverse();
    Ok(v)
}

/// Most fill rows one market query reads. The caller chooses the time window, so the window alone cannot bound the
/// work: a token with hundreds of thousands of fills would be read into memory by `from=0`. The newest rows win; a query that
/// hits the cap reports it and drops the oldest, possibly partial, trade group.
pub const MAX_FILL_ROWS: usize = 20_000;

/// Fills of `token` with `from <= ts < to`: at most `cap` rows, the NEWEST ones, returned oldest first, and whether older rows
/// in the window were left out. Served by the `(kind, token_cov_id, ts)` index.
fn fills_between(conn: &Connection, token: &Hash32, from: i64, to: i64, cap: usize) -> DbResult<(Vec<FillRow>, bool)> {
    let sql = format!("{FILL_SELECT} AND e.ts >= ?2 AND e.ts < ?3 ORDER BY e.ts DESC, e.id DESC LIMIT ?4");
    let mut st = conn.prepare(&sql)?;
    let mut v = st.query_map(params![token.0.to_vec(), from, to, cap as i64 + 1], fill_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = v.len() > cap;
    v.truncate(cap);
    v.reverse();
    Ok((v, truncated))
}

/// Trades of a window: the fills grouped by transaction; when the row cap cut the window, the oldest group (which may be
/// incomplete) is dropped.
fn window_trades(conn: &Connection, token: &Hash32, from: i64, to: i64, basis: i128) -> DbResult<(Vec<Trade>, bool)> {
    let (rows, truncated) = fills_between(conn, token, from, to, MAX_FILL_ROWS)?;
    let first_txid = rows.first().map(|r| r.txid.clone());
    let mut ts = trades_of(rows, basis);
    if truncated {
        if let (Some(t0), Some(first)) = (ts.first(), first_txid) {
            if t0.txid == first {
                ts.remove(0);
            }
        }
    }
    Ok((ts, truncated))
}

/// Newest trade time of the token (ms), if any.
fn newest_fill_ts(conn: &Connection, token: &Hash32) -> DbResult<Option<i64>> {
    Ok(conn.query_row(
        // price IS NOT NULL: a pair order's fill (no KAS price, founder rule) is not a KAS trade of its token
        "SELECT MAX(ts) FROM order_events WHERE kind = 'fill' AND token_cov_id = ?1 AND price IS NOT NULL",
        params![token.0.to_vec()],
        |r| r.get::<_, Option<i64>>(0),
    )?)
}

/// Newest KAS trade time of the token (ms), if any (pair fills excluded: they carry no KAS price).
pub fn newest_trade_ts(conn: &Connection, token: &Hash32) -> DbResult<Option<i64>> {
    newest_fill_ts(conn, token)
}

/// The KAS candles of `token` over `[start, end)` (prices per `basis` base units; `interval` ms buckets), oldest first. The
/// pair charts are built from these ([`super::pairs::pair_candles`]).
pub fn candle_series(conn: &Connection, token: &Hash32, basis: i128, interval: i64, start: i64, end: i64) -> DbResult<Vec<Candle>> {
    if end <= start || interval <= 0 {
        return Ok(vec![]);
    }
    let (trades, _) = window_trades(conn, token, start, end, basis)?;
    Ok(candles_of(&trades, interval))
}

/// The price (per `basis`) of the last KAS trade of `token` before `ts` (ms), if any.
pub fn last_trade_price_before(conn: &Connection, token: &Hash32, basis: i128, ts: i64) -> DbResult<Option<i128>> {
    let last: Option<i64> = conn.query_row(
        "SELECT MAX(ts) FROM order_events WHERE kind = 'fill' AND token_cov_id = ?1 AND price IS NOT NULL AND ts < ?2",
        params![token.0.to_vec(), ts],
        |r| r.get(0),
    )?;
    let Some(t) = last else { return Ok(None) };
    let (trades, _) = window_trades(conn, token, t, t + 1, basis)?;
    Ok(trades.last().map(|t| t.price))
}

/// Newest chain block time the indexer has recorded an event for (ms).
fn newest_event_ts(conn: &Connection) -> DbResult<Option<i64>> {
    Ok(conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get::<_, Option<i64>>(0))?)
}

// ------------------------------------------------------------------------------------------------ views

#[derive(Debug, Clone, Serialize)]
pub struct TradeView {
    pub id: i64,
    pub txid: String,
    pub ts: i64,
    pub daa: i64,
    /// Sompi per `price_basis` base units.
    pub price: String,
    /// Base units.
    pub amount: String,
    /// Sompi.
    pub quote: String,
    pub side: &'static str,
    pub fills: usize,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradesView {
    pub token: String,
    pub price_basis: String,
    /// The token's decimals (registry), to show base units as whole tokens; `null` without an entry.
    pub decimals: Option<u32>,
    pub items: Vec<TradeView>,
    pub next_cursor: Option<String>,
}

fn trade_view(t: &Trade, ctx: &ReadCtx) -> TradeView {
    TradeView {
        id: t.id,
        txid: crate::hex::encode(&t.txid),
        ts: t.ts,
        daa: t.daa,
        price: t.price.to_string(),
        amount: t.amount.to_string(),
        quote: t.quote.to_string(),
        side: if t.buy { "buy" } else { "sell" },
        fills: t.fills,
        confirmations: ctx.confirmations(t.daa),
        settled: ctx.settled(t.daa),
    }
}

/// Recent trades, newest first; `before` is a trade id (exclusive). `decimals`: the token's (registry), if known.
pub fn trades(
    conn: &Connection,
    ctx: &ReadCtx,
    token: &Hash32,
    decimals: Option<u32>,
    before: Option<i64>,
    limit: usize,
) -> DbResult<TradesView> {
    let basis = price_basis(conn, token, decimals)?;
    // over-fetch: several fills per trade; the oldest group may be cut by the row limit, so it is dropped unless the data ended
    let want_rows = (limit + 1) * 12;
    let rows = fills_before(conn, token, before, want_rows)?;
    let exhausted = rows.len() < want_rows;
    let first_txid = rows.first().map(|r| r.txid.clone());
    let mut ts = trades_of(rows, basis);
    if !exhausted {
        if let (Some(t0), Some(first)) = (ts.first(), first_txid) {
            if t0.txid == first {
                ts.remove(0);
            }
        }
    }
    ts.reverse();
    let more = ts.len() > limit || !exhausted;
    ts.truncate(limit);
    let next_cursor = if more { ts.last().map(|t| t.id.to_string()) } else { None };
    Ok(TradesView {
        token: token.to_hex(),
        price_basis: basis.to_string(),
        decimals,
        items: ts.iter().map(|t| trade_view(t, ctx)).collect(),
        next_cursor,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CandleView {
    pub t: i64,
    pub o: String,
    pub h: String,
    pub l: String,
    pub c: String,
    /// Base units.
    pub volume: String,
    /// Sompi.
    pub quote_volume: String,
    pub trades: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandlesView {
    pub token: String,
    pub interval: String,
    pub price_basis: String,
    pub decimals: Option<u32>,
    pub items: Vec<CandleView>,
}

/// OHLCV candles. Without `from`, the window is the `limit` intervals ending with the bucket of the newest trade.
#[allow(clippy::too_many_arguments)]
pub fn candles(
    conn: &Connection,
    token: &Hash32,
    decimals: Option<u32>,
    interval: &str,
    iv: i64,
    from: Option<i64>,
    to: Option<i64>,
    limit: usize,
) -> DbResult<CandlesView> {
    let basis = price_basis(conn, token, decimals)?;
    let newest = newest_fill_ts(conn, token)?;
    let end = match (to, newest) {
        (Some(t), _) => t,
        (None, Some(n)) => (n.div_euclid(iv) + 1) * iv,
        (None, None) => 0,
    };
    let start = from.unwrap_or(end - iv * limit as i64);
    let items = if end > start {
        let (trades, _) = window_trades(conn, token, start, end, basis)?;
        let mut cs = candles_of(&trades, iv);
        if cs.len() > limit {
            cs.drain(..cs.len() - limit);
        }
        cs
    } else {
        vec![]
    };
    Ok(CandlesView {
        token: token.to_hex(),
        interval: interval.to_string(),
        price_basis: basis.to_string(),
        decimals,
        items: items
            .iter()
            .map(|c| CandleView {
                t: c.t,
                o: c.o.to_string(),
                h: c.h.to_string(),
                l: c.l.to_string(),
                c: c.c.to_string(),
                volume: c.volume.to_string(),
                quote_volume: c.quote_volume.to_string(),
                trades: c.trades,
            })
            .collect(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsView {
    pub token: String,
    pub price_basis: String,
    pub decimals: Option<u32>,
    /// newest chain block time the indexer knows (ms): the end of the 24 h window
    pub ts: Option<i64>,
    pub last: Option<String>,
    pub last_ts: Option<i64>,
    pub last_side: Option<&'static str>,
    pub open_24h: Option<String>,
    pub high_24h: Option<String>,
    pub low_24h: Option<String>,
    pub change_24h_bps: Option<i64>,
    /// Base units.
    pub volume_24h: String,
    /// Sompi.
    pub quote_volume_24h: String,
    pub trades_24h: u64,
    /// The 24 h window held more fills than one query reads ([`MAX_FILL_ROWS`]): the figures cover the newest ones.
    pub window_truncated: bool,
    pub best_bid: Option<String>,
    pub best_ask: Option<String>,
    pub mid: Option<String>,
    pub spread_bps: Option<i64>,
    pub open_asks: i64,
    pub open_bids: i64,
}

const DAY_MS: i64 = 86_400_000;

fn best_levels(conn: &Connection, ctx: &ReadCtx, token: &Hash32, basis: i128) -> DbResult<(Option<i128>, Option<i128>)> {
    let b = reads::book(conn, ctx, token, 5, true)?;
    let best = |s: &BookSide, ask: bool| -> Option<i128> {
        match s {
            BookSide::Levels(ls) => {
                let it = ls.iter().filter_map(|l| l.price.parse::<i64>().ok().map(|p| per_basis(p, l.scale, basis)));
                if ask {
                    it.min()
                } else {
                    it.max()
                }
            }
            BookSide::Orders(_) => None,
        }
    };
    Ok((best(&b.bids, false), best(&b.asks, true)))
}

pub fn stats(conn: &Connection, ctx: &ReadCtx, token: &Hash32, decimals: Option<u32>) -> DbResult<StatsView> {
    let basis = price_basis(conn, token, decimals)?;
    let now = newest_event_ts(conn)?;
    let (mut last, mut last_ts, mut last_side) = (None, None, None);
    if let Some(newest) = newest_fill_ts(conn, token)? {
        let (tail, _) = window_trades(conn, token, newest, newest + 1, basis)?;
        if let Some(t) = tail.last() {
            last = Some(t.price);
            last_ts = Some(t.ts);
            last_side = Some(if t.buy { "buy" } else { "sell" });
        }
    }
    let (window, window_truncated) = match now {
        Some(n) => window_trades(conn, token, n - DAY_MS, n + 1, basis)?,
        None => (vec![], false),
    };
    let open = window.first().map(|t| t.price);
    let high = window.iter().map(|t| t.price).max();
    let low = window.iter().map(|t| t.price).min();
    let change = match (open, last) {
        (Some(o), Some(l)) if o > 0 && !window.is_empty() => i64::try_from(l.saturating_sub(o).saturating_mul(10_000) / o).ok(),
        _ => None,
    };
    let (bid, ask) = best_levels(conn, ctx, token, basis)?;
    let mid = bid.zip(ask).map(|(b, a)| a.saturating_add(b) / 2);
    let spread = match (bid, ask, mid) {
        (Some(b), Some(a), Some(m)) if m > 0 => i64::try_from(a.saturating_sub(b).saturating_mul(10_000) / m).ok(),
        _ => None,
    };
    let (mut open_asks, mut open_bids) = (0i64, 0i64);
    let mut st = conn.prepare(
        "SELECT o.side, COUNT(*) FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id \
         WHERE o.token_cov_id = ?1 AND o.listed = 1 AND o.in_book = 1 AND s.status IN ('open','partial') AND s.state_known = 1 AND NOT EXISTS (SELECT 1 FROM order_flags f WHERE f.covenant_id = o.covenant_id AND f.txid = s.cur_txid AND f.idx = s.cur_idx) GROUP BY o.side",
    )?;
    for r in st.query_map(params![token.0.to_vec()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (side, n) = r?;
        if side == 1 {
            open_asks = n;
        } else {
            open_bids = n;
        }
    }
    Ok(StatsView {
        token: token.to_hex(),
        price_basis: basis.to_string(),
        decimals,
        ts: now,
        last: last.map(|v| v.to_string()),
        last_ts,
        last_side,
        open_24h: if window.is_empty() { None } else { open.map(|v| v.to_string()) },
        high_24h: high.map(|v| v.to_string()),
        low_24h: low.map(|v| v.to_string()),
        change_24h_bps: change,
        volume_24h: window.iter().fold(0i128, |s, t| s.saturating_add(t.amount)).to_string(),
        quote_volume_24h: window.iter().fold(0i128, |s, t| s.saturating_add(t.quote)).to_string(),
        trades_24h: window.len() as u64,
        window_truncated,
        best_bid: bid.map(|v| v.to_string()),
        best_ask: ask.map(|v| v.to_string()),
        mid: mid.map(|v| v.to_string()),
        spread_bps: spread,
        open_asks,
        open_bids,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct DepthLevel {
    /// Sompi per `price_basis` base units.
    pub price: String,
    /// Base units.
    pub amount: String,
    pub orders: i64,
    pub cum_amount: String,
    /// Sompi: the cumulative amounts at their level prices (rounded down per level).
    pub cum_quote: String,
    pub estimated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DepthView {
    pub token: String,
    pub price_basis: String,
    pub decimals: Option<u32>,
    pub ts: Option<i64>,
    pub daa: Option<u64>,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

/// One aggregated book level in per-basis terms, before merging: (price, amount, orders, estimated).
pub type RawLevel = (i128, i128, i64, bool);

/// Merges levels of equal per-basis price (input best first) and adds cumulative sums; at most `levels` rows.
pub fn depth_side(raw: &[RawLevel], basis: i128, levels: usize) -> Vec<DepthLevel> {
    let mut merged: Vec<RawLevel> = Vec::new();
    for &(p, a, n, e) in raw {
        match merged.last_mut() {
            Some(m) if m.0 == p => {
                m.1 = m.1.saturating_add(a);
                m.2 += n;
                m.3 |= e;
            }
            _ => merged.push((p, a, n, e)),
        }
    }
    let (mut cum_a, mut cum_q) = (0i128, 0i128);
    merged
        .into_iter()
        .take(levels)
        .map(|(p, a, n, e)| {
            cum_a = cum_a.saturating_add(a);
            cum_q = cum_q.saturating_add(a.saturating_mul(p) / basis.max(1));
            DepthLevel {
                price: p.to_string(),
                amount: a.to_string(),
                orders: n,
                cum_amount: cum_a.to_string(),
                cum_quote: cum_q.to_string(),
                estimated: e,
            }
        })
        .collect()
}

pub fn depth(conn: &Connection, ctx: &ReadCtx, token: &Hash32, decimals: Option<u32>, levels: usize) -> DbResult<DepthView> {
    let basis = price_basis(conn, token, decimals)?;
    // fetch generously: several (price, scale) rows can merge into one per-basis level
    let b = reads::book(conn, ctx, token, levels.saturating_mul(4).max(1), true)?;
    let raw = |s: &BookSide, ask: bool| -> Vec<RawLevel> {
        let mut v: Vec<RawLevel> = match s {
            BookSide::Levels(ls) => ls
                .iter()
                .filter_map(|l| {
                    let p = l.price.parse::<i64>().ok()?;
                    Some((per_basis(p, l.scale, basis), l.amount.parse::<i128>().ok()?, l.orders, l.amount_estimated))
                })
                .collect(),
            BookSide::Orders(_) => vec![],
        };
        v.sort_by(|a, b| if ask { a.0.cmp(&b.0) } else { b.0.cmp(&a.0) });
        v
    };
    Ok(DepthView {
        token: token.to_hex(),
        price_basis: basis.to_string(),
        decimals,
        ts: newest_event_ts(conn)?,
        daa: ctx.node_daa,
        bids: depth_side(&raw(&b.bids, false), basis, levels),
        asks: depth_side(&raw(&b.asks, true), basis, levels),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fill of `amount` base units at `price` sompi per whole token of 100 base units (scale 100); the basis is 100.
    fn f(id: i64, tx: u8, ts: i64, side: i64, amount: i64, price: i64, gdaa: i64) -> FillRow {
        FillRow { id, txid: vec![tx; 32], ts, daa: ts / 100, side, amount, price, scale: 100, genesis_daa: gdaa }
    }

    #[test]
    fn resting_side_sets_the_price_and_the_aggressor_side() {
        // ask (older) at 250 per whole token, bid (newer) at 260: the ask rested, a buyer crossed it
        let t = trade_of(&[f(1, 1, 1000, 1, 200, 250, 10), f(2, 1, 1000, 2, 200, 260, 20)], 100).unwrap();
        assert_eq!((t.price, t.amount, t.quote, t.buy, t.fills), (250, 200, 500, true, 2));
        // the bid rested (older): the seller aggressed, the price is the bid's
        let t = trade_of(&[f(3, 2, 1000, 1, 100, 250, 30), f(4, 2, 1000, 2, 100, 260, 20)], 100).unwrap();
        assert_eq!((t.price, t.buy), (260, false));
    }

    #[test]
    fn one_sided_trade_is_the_direct_takers() {
        // a wallet sold into a resting bid (no ask fill): the bid side measures the volume and is resting
        let t = trade_of(&[f(5, 3, 1000, 2, 300, 240, 5)], 100).unwrap();
        assert_eq!((t.amount, t.quote, t.price, t.buy), (300, 720, 240, false));
        assert!(trade_of(&[], 100).is_none());
    }

    #[test]
    fn vwap_of_several_resting_fills() {
        // two asks of the same tx at 250 x 100 and 270 x 300 base units, one newer bid: VWAP 265 per whole token
        let t = trade_of(&[f(1, 1, 5, 1, 100, 250, 1), f(2, 1, 5, 1, 300, 270, 2), f(3, 1, 5, 2, 400, 300, 9)], 100).unwrap();
        assert_eq!((t.price, t.amount, t.quote), (265, 400, 1060));
    }

    /// Fills of another scale are converted to the basis; quotes round down per fill (the maker-favour rounding of the
    /// covenants is the fill's own; a display sum never rounds up).
    #[test]
    fn fills_of_another_scale_and_amounts_off_the_whole_token() {
        // 50 base units at 250 per 100 (scale 100) and 5 base units at 2 500 per 1 000 (scale 1 000: the same price per base unit)
        let other = FillRow { scale: 1_000, ..f(2, 1, 5, 1, 5, 2_500, 1) };
        let t = trade_of(&[f(1, 1, 5, 1, 50, 250, 1), other], 100).unwrap();
        assert_eq!((t.price, t.amount, t.quote), (250, 55, 125 + 12));
        // a basis of 1 000: the same trade per 1 000 base units
        let t = trade_of(&[f(1, 1, 5, 1, 50, 250, 1)], 1_000).unwrap();
        assert_eq!((t.price, t.quote), (2_500, 125));
        // huge numbers saturate instead of overflowing
        let big = FillRow { amount: i64::MAX, price: i64::MAX, scale: 1, ..f(1, 1, 5, 1, 0, 0, 1) };
        let t = trade_of(&[big.clone(), FillRow { id: 2, ..big }], 1_000_000_000).unwrap();
        assert!(t.price > 0 && t.quote > 0 && t.amount == 2 * i64::MAX as i128);
    }

    #[test]
    fn candles_bucket_and_omit_gaps() {
        let tr = trades_of(
            vec![
                f(1, 1, 60_000, 1, 100, 250, 1),
                f(2, 2, 90_000, 1, 100, 260, 1),
                f(3, 3, 119_999, 1, 200, 240, 1),
                f(4, 4, 300_000, 1, 100, 255, 1),
            ],
            100,
        );
        let cs = candles_of(&tr, 60_000);
        assert_eq!(cs.len(), 2);
        assert_eq!((cs[0].t, cs[0].o, cs[0].h, cs[0].l, cs[0].c, cs[0].volume, cs[0].trades), (60_000, 250, 260, 240, 240, 400, 3));
        assert_eq!((cs[1].t, cs[1].o, cs[1].c, cs[1].trades), (300_000, 255, 255, 1));
        assert_eq!(interval_ms("5m"), Some(300_000));
        assert_eq!(interval_ms("2m"), None);
    }

    #[test]
    fn depth_merges_equal_prices_and_accumulates() {
        let d = depth_side(&[(100, 10, 1, false), (100, 5, 2, true), (99, 1, 1, false)], 10, 50);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].price.as_str(), d[0].amount.as_str(), d[0].orders, d[0].estimated), ("100", "15", 3, true));
        assert_eq!((d[1].cum_amount.as_str(), d[1].cum_quote.as_str()), ("16", "159"));
        assert_eq!(depth_side(&[(1, 1, 1, false), (2, 1, 1, false)], 1, 1).len(), 1);
    }
}
