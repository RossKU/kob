//! Market data derived from the indexed fills: trades, OHLCV candles, 24 h statistics and depth.
//!
//! These are the reads behind `GET /v1/trades/{token}`, `/v1/candles/{token}`,
//! `/v1/stats/{token}` and `/v1/depth/{token}`, see `docs/ops/executor.md`, Part B, 5.3.
//!
//! # Trades
//!
//! The indexer stores one `fill` event per order and fill. It holds the `amount` in token base
//! units and the `price` of the order's quote at the fill, in sompi per whole token, that is,
//! per `scale` base units of the order. A trade is every fill event of ONE transaction on ONE
//! token.
//!
//! - The `amount` of a trade is the token base units moved from sellers to buyers. It is the sum
//!   of the amounts of the ask-side fills.
//! - The `quote` of a trade is the sum of `quoteOf(amount, price, scale)` over the same fills,
//!   rounded down, in sompi.
//! - A transaction without ask-side fills is measured on its bid side. An example is a wallet that
//!   takes a resting bid directly, as in swap-and-pay.
//!
//! The **resting** side is the side whose orders are older. Age is `since_daa`, the DAA of the
//! placement of the order, or of its latest in-place amend before the fill. An amend replaces the
//! terms and restarts the age of the order, see `docs/spec/matcher.md`, 1.1. The resting side sets
//! the trade price, which is its volume-weighted price. The other side is the aggressor. `side`
//! is `buy` when the aggressor bought, that is, when asks were resting. With one side only, that
//! side is resting and the aggressor is the direct taker.
//!
//! # Only KAS-book fills make prices
//!
//! Trades, candles, last prices and statistics are per token in KAS. They come only from fills
//! of the KAS-quoted kinds: plain, conditional and if-done. A pair-order fill (`KobPair`,
//! `KobCondPair`, `KobIfdPair`) is stored with a NULL price and never enters these views. Its
//! volume is in `pair_fills` (`indexer::pairs`). The KAS-book counterparties of a routed pair fill
//! are ordinary KAS fills of their own tokens.
//!
//! # Price basis
//!
//! Prices are sompi per `price_basis` token base units. The basis is the standard scale of the
//! token, `10^decimals` of its registry entry and at most `10^9`, when the registry knows its
//! decimals. Otherwise it is the scale of the first order seen. A fill at another scale is
//! converted as `price × basis / scale`. Every view also carries `decimals`, which is the
//! decimals of the registry or `null` without an entry, so a client can show whole tokens. All
//! arithmetic is integer (i128, saturating) and rounds down.

use crate::hex::Hash32;
use crate::indexer::db::DbResult;
use crate::indexer::reads::{self, BookSide, ReadCtx};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

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
    /// The DAA at which the order's terms of this fill were set: its placement, or its latest
    /// in-place amend before the fill.
    pub since_daa: i64,
}

/// A trade, see the module docs. Amounts are in base units and sompi, the price is per basis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trade {
    pub id: i64,
    pub txid: Vec<u8>,
    pub ts: i64,
    pub daa: i64,
    pub price: i128,
    pub amount: i128,
    pub quote: i128,
    /// Whether the aggressor bought.
    pub buy: bool,
    pub fills: usize,
}

/// `price` sompi per `scale` base units as sompi per `basis` base units (rounded down).
fn per_basis(price: i64, scale: i64, basis: i128) -> i128 {
    (price as i128).saturating_mul(basis) / scale.max(1) as i128
}

/// The sums of one side: (base units, sompi at the fills' quotes, base units x price per basis,
/// oldest terms DAA).
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
        oldest = oldest.min(r.since_daa);
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
    // The volume moved: the tokens of the ask side, or of the bid side when no ask filled.
    let (amount, quote_moved) = if a_amt > 0 { (a_amt, a_quote) } else { (b_amt, b_quote) };
    if amount <= 0 || basis <= 0 {
        return None;
    }
    // The resting side sets the price. Ties (the same terms DAA) go to the asks.
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

/// The length in milliseconds of the candle interval `name`, if the API serves it.
pub fn interval_ms(name: &str) -> Option<i64> {
    INTERVALS.iter().find(|(n, _)| *n == name).map(|(_, ms)| *ms)
}

/// An OHLCV candle of one bucket. `o`, `h`, `l` and `c` are prices per basis.
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

/// OHLCV per bucket (`floor(ts / interval) * interval`), ascending. Empty buckets are omitted.
///
/// `trades` must be oldest first.
pub fn candles_of(trades: &[Trade], interval: i64) -> Vec<Candle> {
    let mut out: VecDeque<Candle> = VecDeque::new();
    for t in trades {
        add_trade(&mut out, t, interval);
    }
    out.into()
}

/// Adds one trade, newer than every trade before it, to the candles.
///
/// It goes into the last candle when that is the bucket of the trade, else into a new candle.
pub(crate) fn add_trade(out: &mut VecDeque<Candle>, t: &Trade, interval: i64) {
    let b = t.ts.div_euclid(interval) * interval;
    match out.back_mut() {
        Some(c) if c.t == b => {
            c.h = c.h.max(t.price);
            c.l = c.l.min(t.price);
            c.c = t.price;
            c.volume = c.volume.saturating_add(t.amount);
            c.quote_volume = c.quote_volume.saturating_add(t.quote);
            c.trades += 1;
        }
        _ => out.push_back(Candle {
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

// SQL.

/// The select of the fill events of a token (`?1`), with the scale and the terms DAA of each order.
pub(crate) const FILL_SELECT: &str =
    "SELECT e.id, e.txid, e.ts, e.daa, e.side, e.amount, e.price, COALESCE(o.scale, 1), \
     COALESCE((SELECT a.daa FROM order_events a WHERE a.order_id = e.order_id AND a.kind = 'amend' AND a.id < e.id ORDER BY a.id DESC LIMIT 1), o.genesis_daa) \
     FROM order_events e JOIN orders o ON o.id = e.order_id \
     WHERE e.kind = 'fill' AND e.token_id = (SELECT id FROM ids WHERE h = ?1) AND e.amount IS NOT NULL AND e.amount > 0 AND e.price IS NOT NULL AND e.side IN (1, 2)";

/// Reads a [`FillRow`] from a row of [`FILL_SELECT`].
pub(crate) fn fill_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FillRow> {
    Ok(FillRow {
        id: r.get(0)?,
        txid: r.get(1)?,
        ts: r.get(2)?,
        daa: r.get(3)?,
        side: r.get(4)?,
        amount: r.get(5)?,
        price: r.get(6)?,
        scale: r.get(7)?,
        since_daa: r.get(8)?,
    })
}

/// The price basis of a token.
///
/// It is the standard scale of the token (`10^decimals`, at most `10^9`) when its decimals are
/// known, else the scale of its first order, else 1.
pub fn price_basis(conn: &Connection, token: &Hash32, decimals: Option<u32>) -> DbResult<i128> {
    if let Some(d) = decimals {
        return Ok(kob_protocol::defaults::default_scale(d) as i128);
    }
    let scale: Option<i64> = conn
        .query_row(
            "SELECT COALESCE(scale, 1) FROM orders WHERE token_id = (SELECT id FROM ids WHERE h = ?1) ORDER BY genesis_block, covenant_id LIMIT 1",
            params![token.0.to_vec()],
            |r| r.get(0),
        )
        .ok();
    Ok(scale.filter(|s| *s > 0).unwrap_or(1) as i128)
}

/// Fills of `token` with `id < before` (newest first), at most `limit` rows, oldest first.
fn fills_before(conn: &Connection, token: &Hash32, before: Option<i64>, limit: usize) -> DbResult<Vec<FillRow>> {
    let sql = format!("{FILL_SELECT} AND e.id < ?2 ORDER BY e.id DESC LIMIT ?3");
    let mut st = conn.prepare(&sql)?;
    let mut v = st
        .query_map(params![token.0.to_vec(), before.unwrap_or(i64::MAX), limit as i64], fill_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    v.reverse();
    Ok(v)
}

/// The most fill rows that a trade-list read of a time window keeps in memory.
///
/// The 24 h statistics and the last trade before a time use it. The newest rows win. A query that
/// hits the cap reports it and drops the oldest trade group, which may be partial. Candles are NOT
/// read through the cap, see [`window_candles`]: a cap on rows there would cut every chart at the
/// newest fills, so a chart would lose its oldest days as fills accumulate.
pub const MAX_FILL_ROWS: usize = 20_000;

/// Streams the trades of `token` over `[from, to)` in the order of the fills.
///
/// Every fill of the window is read in time order `(ts, id)`, served by the `(kind, token, ts)`
/// index, and folded into the trade of its transaction. Nothing is held but the fills of one
/// transaction.
pub(crate) fn for_each_trade(
    conn: &Connection,
    token: &Hash32,
    basis: i128,
    from: i64,
    to: i64,
    mut f: impl FnMut(Trade) -> DbResult<()>,
) -> DbResult<()> {
    let sql = format!("{FILL_SELECT} AND e.ts >= ?2 AND e.ts < ?3 ORDER BY e.ts, e.id");
    let mut st = conn.prepare(&sql)?;
    let mut rows = st.query(params![token.0.to_vec(), from, to])?;
    // The fills of one transaction share its block time and are stored together, so they arrive
    // as one run.
    let mut group: Vec<FillRow> = Vec::new();
    while let Some(r) = rows.next()? {
        let fill = fill_row(r)?;
        if group.first().is_some_and(|g| g.txid != fill.txid) {
            if let Some(t) = trade_of(&group, basis) {
                f(t)?;
            }
            group.clear();
        }
        group.push(fill);
    }
    if let Some(t) = trade_of(&group, basis) {
        f(t)?;
    }
    Ok(())
}

/// The candles of `token` over `[from, to)`, read from the fills.
///
/// Every fill of the window is streamed ([`for_each_trade`]) and its trade is folded into its
/// bucket. Only the newest `keep` candles are held, so memory is bounded by `keep` and not by the
/// fills read. The time spent grows with the fills of the window. The 20 s interrupt of the pooled
/// read and the rate limit of the heavy routes bound a client. No fill is left out, so the oldest
/// candle of a window that reaches back to the first trade is the bucket of the first trade.
///
/// This is the reference. The maintained aggregates ([`super::candles`]) give the same answer.
/// Every window they cannot answer is read here: a token that is not backfilled yet, a changed
/// price basis, a lean store, an interval the API does not serve, or a window inside one bucket.
pub fn scan_candles(
    conn: &Connection,
    token: &Hash32,
    basis: i128,
    iv: i64,
    from: i64,
    to: i64,
    keep: usize,
) -> DbResult<Vec<Candle>> {
    let mut out: VecDeque<Candle> = VecDeque::new();
    if to <= from || iv <= 0 || keep == 0 {
        return Ok(vec![]);
    }
    for_each_trade(conn, token, basis, from, to, |t| {
        add_trade(&mut out, &t, iv);
        if out.len() > keep {
            out.pop_front();
        }
        Ok(())
    })?;
    Ok(out.into())
}

/// The candles of `token` over `[from, to)`, from the maintained aggregates when they hold the
/// token at this basis, else from the fills ([`scan_candles`]).
///
/// The candles are the same either way.
fn window_candles(conn: &Connection, token: &Hash32, basis: i128, iv: i64, from: i64, to: i64, keep: usize) -> DbResult<Vec<Candle>> {
    match super::candles::read(conn, token, basis, iv, from, to, keep)? {
        Some(v) => Ok(v),
        None => scan_candles(conn, token, basis, iv, from, to, keep),
    }
}

/// Fills of `token` with `from <= ts < to`, and whether older rows in the window were left out.
///
/// It returns at most `cap` rows, the NEWEST ones, oldest first. The `(kind, token_cov_id, ts)`
/// index serves it.
fn fills_between(conn: &Connection, token: &Hash32, from: i64, to: i64, cap: usize) -> DbResult<(Vec<FillRow>, bool)> {
    let sql = format!("{FILL_SELECT} AND e.ts >= ?2 AND e.ts < ?3 ORDER BY e.ts DESC, e.id DESC LIMIT ?4");
    let mut st = conn.prepare(&sql)?;
    let mut v = st.query_map(params![token.0.to_vec(), from, to, cap as i64 + 1], fill_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = v.len() > cap;
    v.truncate(cap);
    v.reverse();
    Ok((v, truncated))
}

/// The trades of a window: its fills grouped by transaction.
///
/// When the row cap cut the window, the oldest group, which may be incomplete, is dropped.
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

/// The newest fill time of the token in ms, if any.
fn newest_fill_ts(conn: &Connection, token: &Hash32) -> DbResult<Option<i64>> {
    Ok(conn.query_row(
        // `price IS NOT NULL` leaves out the fill of a pair order, which has no KAS price and is
        // not a KAS trade of its token.
        "SELECT MAX(ts) FROM order_events WHERE kind = 'fill' AND token_id = (SELECT id FROM ids WHERE h = ?1) AND price IS NOT NULL",
        params![token.0.to_vec()],
        |r| r.get::<_, Option<i64>>(0),
    )?)
}

/// The newest KAS trade time of the token in ms, if any.
///
/// Pair fills carry no KAS price and are excluded.
pub fn newest_trade_ts(conn: &Connection, token: &Hash32) -> DbResult<Option<i64>> {
    newest_fill_ts(conn, token)
}

/// The newest `keep` KAS candles of `token` over `[start, end)`, oldest first.
///
/// Prices are per `basis` base units and the buckets are `interval` ms long. The pair charts are
/// built from these ([`super::pairs::pair_candles`]).
pub fn candle_series(
    conn: &Connection,
    token: &Hash32,
    basis: i128,
    interval: i64,
    start: i64,
    end: i64,
    keep: usize,
) -> DbResult<Vec<Candle>> {
    window_candles(conn, token, basis, interval, start, end, keep)
}

/// The price, per `basis`, of the last KAS trade of `token` before `ts` (ms), if any.
pub fn last_trade_price_before(conn: &Connection, token: &Hash32, basis: i128, ts: i64) -> DbResult<Option<i128>> {
    let last: Option<i64> = conn.query_row(
        "SELECT MAX(ts) FROM order_events WHERE kind = 'fill' AND token_id = (SELECT id FROM ids WHERE h = ?1) AND price IS NOT NULL AND ts < ?2",
        params![token.0.to_vec(), ts],
        |r| r.get(0),
    )?;
    let Some(t) = last else { return Ok(None) };
    let (trades, _) = window_trades(conn, token, t, t + 1, basis)?;
    Ok(trades.last().map(|t| t.price))
}

/// The newest chain block time, in ms, that the indexer has recorded an event for.
fn newest_event_ts(conn: &Connection) -> DbResult<Option<i64>> {
    Ok(conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get::<_, Option<i64>>(0))?)
}

// API views.

/// One trade as the API serves it.
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

/// The trades of one token, newest first, with the cursor of the next page.
#[derive(Debug, Clone, Serialize)]
pub struct TradesView {
    pub token: String,
    pub price_basis: String,
    /// The decimals of the token from the registry, to show base units as whole tokens. `null`
    /// without an entry.
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

/// Recent trades of `token`, newest first.
///
/// `before` is a trade id and is exclusive. `decimals` is that of the token from the registry,
/// if known.
pub fn trades(
    conn: &Connection,
    ctx: &ReadCtx,
    token: &Hash32,
    decimals: Option<u32>,
    before: Option<i64>,
    limit: usize,
) -> DbResult<TradesView> {
    let basis = price_basis(conn, token, decimals)?;
    // Over-fetch, because a trade has several fills.
    let want_rows = (limit + 1) * 12;
    let mut rows = fills_before(conn, token, before, want_rows)?;
    // Older fills may exist, and the row limit may have cut the oldest trade.
    let mut older = rows.len() == want_rows;
    let mut cut = older;
    // The rest of the fills of the oldest trade are read, so a page ends at a whole trade. A
    // transaction with more fills than the page reads would otherwise be dropped from every page,
    // leaving an empty page without a cursor. The fills of one transaction are stored together, so
    // they are the run of that txid right before the rows read. At most [`MAX_FILL_ROWS`] rows are
    // read in all.
    while cut && rows.len() < MAX_FILL_ROWS {
        let n = want_rows.min(MAX_FILL_ROWS - rows.len());
        let txid = rows[0].txid.clone();
        let more = fills_before(conn, token, Some(rows[0].id), n)?;
        let same = more.iter().rev().take_while(|r| r.txid == txid).count();
        let ended = more.len() < n;
        older = !ended || same < more.len();
        cut = !ended && same == more.len();
        // `more` is oldest first, so the run of the trade is its end.
        let skip = more.len() - same;
        rows.splice(0..0, more.into_iter().skip(skip));
    }
    let first_txid = rows.first().map(|r| r.txid.clone());
    let mut ts = trades_of(rows, basis);
    // A trade still cut at the bound is dropped, unless it is the only one: then it is shown in
    // part, and the next page starts at its older fills.
    if cut && ts.len() > 1 {
        if let (Some(t0), Some(first)) = (ts.first(), first_txid) {
            if t0.txid == first {
                ts.remove(0);
            }
        }
    }
    ts.reverse();
    let more = ts.len() > limit || older;
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

/// One candle as the API serves it. Prices and volumes are strings.
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

/// The candles of one token at one interval.
#[derive(Debug, Clone, Serialize)]
pub struct CandlesView {
    pub token: String,
    pub interval: String,
    pub price_basis: String,
    pub decimals: Option<u32>,
    pub items: Vec<CandleView>,
}

/// OHLCV candles of `token`.
///
/// Without `from`, the window is the `limit` intervals ending with the bucket of the newest trade.
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
    // Saturating: `to=-9223372036854775808` wrapped in release and panicked in debug here.
    let start = from.unwrap_or_else(|| end.saturating_sub(iv.saturating_mul(limit as i64)));
    // Every fill of the window counts, with no row cap, and the newest `limit` candles of it are
    // kept. An empty or inverted window has none.
    let items = if end > start { window_candles(conn, token, basis, iv, start, end, limit)? } else { vec![] };
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

/// The 24 h statistics and the touch of the book of one token.
#[derive(Debug, Clone, Serialize)]
pub struct StatsView {
    pub token: String,
    pub price_basis: String,
    pub decimals: Option<u32>,
    /// The end of the 24 h window in ms: the newer of the newest chain block time the indexer
    /// knows and its wall clock ([`ReadCtx::window_end`]). The figures of a quiet market age out.
    pub ts: Option<i64>,
    pub last: Option<String>,
    pub last_ts: Option<i64>,
    pub last_side: Option<&'static str>,
    /// The price 24 h ago, the reference of `change_24h_bps`: the last trade at or before the start
    /// of the window (`ts` − 24 h). It is the first trade of the window only when the token had no
    /// earlier trade. `null` when the window holds no trade.
    pub open_24h: Option<String>,
    pub high_24h: Option<String>,
    pub low_24h: Option<String>,
    /// (`last` − `open_24h`) × 10 000 / `open_24h`, truncated toward zero. `null` when the window
    /// holds no trade.
    pub change_24h_bps: Option<i64>,
    /// Base units.
    pub volume_24h: String,
    /// Sompi.
    pub quote_volume_24h: String,
    pub trades_24h: u64,
    /// The 24 h window held more fills than one query reads ([`MAX_FILL_ROWS`]), so the figures
    /// cover the newest ones.
    pub window_truncated: bool,
    pub best_bid: Option<String>,
    pub best_ask: Option<String>,
    pub mid: Option<String>,
    pub spread_bps: Option<i64>,
    /// The orders of the two sides of the book (`/v1/books`): the resting orders a matcher may
    /// fill now, which is the set of `best_bid`, `best_ask` and the depth.
    pub open_asks: i64,
    pub open_bids: i64,
    /// Sompi: what the resting asks up to 2 % above the mid and the resting bids down to 2 % below
    /// it are worth. A bid counts at the buying power of its escrow, so this is an upper bound. It
    /// is how much can trade without moving the price more than 2 %. With one side only, the
    /// reference is the best price of that side. `null` for an empty book.
    pub depth_2pct_quote: Option<String>,
    /// Sompi: the part of the 2 % band on the bid side, the resting bids down to 2 % below the
    /// reference price. The reference and the value are those of `depth_2pct_quote`. `"0"` for a
    /// missing side or an empty book. It is a string like every sompi figure and is always present.
    pub depth_2pct_bid_sompi: String,
    /// Sompi: the part of the 2 % band on the ask side, the resting asks up to 2 % above the
    /// reference price. `"0"` for a missing side or an empty book.
    pub depth_2pct_ask_sompi: String,
}

const DAY_MS: i64 = 86_400_000;

/// The touch of the book of a token and the depth near it, from ONE scan of its live orders per
/// side. It is the scan that the `book` read makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Touch {
    /// Sompi per `basis` base units.
    bid: Option<i128>,
    ask: Option<i128>,
    /// Sompi that the resting asks up to [`DEPTH_BAND_PERCENT`] % above the reference price and the
    /// resting bids down to that much below it are worth.
    depth: Option<i128>,
    /// The two parts of `depth` (bids, asks). 0 for a missing side or no reference.
    depth_bid: i128,
    depth_ask: i128,
    /// The orders of the levels of each side, from the same book.
    bid_orders: i64,
    ask_orders: i64,
}

/// The band of `depth_2pct_quote`: the percent above (asks) and below (bids) the reference price.
const DEPTH_BAND_PERCENT: i128 = 2;

/// The reference price of the depth band.
///
/// It is the mid of the touch. With one side only, it is the best price of that side. There is none
/// for an empty book or a price that is not positive.
fn depth_reference(bid: Option<i128>, ask: Option<i128>) -> Option<i128> {
    bid.zip(ask).map(|(b, a)| a.saturating_add(b) / 2).or(bid).or(ask).filter(|r| *r > 0)
}

/// The depth band around the reference price, from the levels of both sides, best first, as
/// (price per basis, value in sompi).
///
/// The result is the value of the bids down to [`DEPTH_BAND_PERCENT`] % below the reference and
/// of the asks up to that much above it, both bounds inclusive, as (bid, ask). It is `None` for a
/// book with no reference.
fn depth_bands(bids: &[(i128, i128)], asks: &[(i128, i128)]) -> Option<(i128, i128)> {
    let r = depth_reference(bids.first().map(|l| l.0), asks.first().map(|l| l.0))?;
    let up = r.saturating_mul(100 + DEPTH_BAND_PERCENT);
    let down = r.saturating_mul(100 - DEPTH_BAND_PERCENT);
    let a = asks.iter().take_while(|l| l.0.saturating_mul(100) <= up).fold(0i128, |s, l| s.saturating_add(l.1));
    let b = bids.iter().take_while(|l| l.0.saturating_mul(100) >= down).fold(0i128, |s, l| s.saturating_add(l.1));
    Some((b, a))
}

fn touch(conn: &Connection, ctx: &ReadCtx, token: &Hash32, basis: i128) -> DbResult<Touch> {
    let asks = reads::book_side_sorted(conn, ctx, token, 1)?;
    let bids = reads::book_side_sorted(conn, ctx, token, 2)?;
    let price = |l: &reads::RawBookLevel| per_basis(l.0 .0, l.0 .1, basis);
    // Levels are in book order (exact price per base unit, best first), so the first one is the
    // best and the band is a prefix.
    let ask = asks.first().map(price);
    let bid = bids.first().map(price);
    let value = |l: &reads::RawBookLevel| (price(l), (l.1 .0).saturating_mul(l.0 .0 as i128) / (l.0 .1.max(1) as i128));
    let (bid_levels, ask_levels): (Vec<_>, Vec<_>) = (bids.iter().map(value).collect(), asks.iter().map(value).collect());
    let bands = depth_bands(&bid_levels, &ask_levels);
    let (depth_bid, depth_ask) = bands.unwrap_or((0, 0));
    let depth = bands.map(|(b, a)| a.saturating_add(b));
    let orders = |ls: &[reads::RawBookLevel]| ls.iter().map(|l| l.1 .1).sum::<i64>();
    Ok(Touch { bid, ask, depth, depth_bid, depth_ask, bid_orders: orders(&bids), ask_orders: orders(&asks) })
}

/// The 24 h statistics and the touch of the book of `token`.
pub fn stats(conn: &Connection, ctx: &ReadCtx, token: &Hash32, decimals: Option<u32>) -> DbResult<StatsView> {
    let basis = price_basis(conn, token, decimals)?;
    let now = ctx.window_end(newest_event_ts(conn)?);
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
    // The price 24 h ago is the last trade at or before the start of the window, else, with no
    // earlier trade, the first trade of the window.
    let open = match (now, window.first()) {
        (Some(n), Some(first)) => Some(last_trade_price_before(conn, token, basis, n - DAY_MS + 1)?.unwrap_or(first.price)),
        _ => None,
    };
    let high = window.iter().map(|t| t.price).max();
    let low = window.iter().map(|t| t.price).min();
    let change = match (open, last) {
        (Some(o), Some(l)) if o > 0 => i64::try_from(l.saturating_sub(o).saturating_mul(10_000) / o).ok(),
        _ => None,
    };
    let Touch { bid, ask, depth: depth_2pct, depth_bid, depth_ask, bid_orders: open_bids, ask_orders: open_asks } =
        touch(conn, ctx, token, basis)?;
    let mid = bid.zip(ask).map(|(b, a)| a.saturating_add(b) / 2);
    let spread = match (bid, ask, mid) {
        (Some(b), Some(a), Some(m)) if m > 0 => i64::try_from(a.saturating_sub(b).saturating_mul(10_000) / m).ok(),
        _ => None,
    };
    Ok(StatsView {
        token: token.to_hex(),
        price_basis: basis.to_string(),
        decimals,
        ts: now,
        last: last.map(|v| v.to_string()),
        last_ts,
        last_side,
        open_24h: open.map(|v| v.to_string()),
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
        depth_2pct_quote: depth_2pct.map(|v| v.to_string()),
        depth_2pct_bid_sompi: depth_bid.to_string(),
        depth_2pct_ask_sompi: depth_ask.to_string(),
    })
}

/// One level of the depth, with the cumulative amount and quote up to it.
#[derive(Debug, Clone, Serialize)]
pub struct DepthLevel {
    /// Sompi per `price_basis` base units.
    pub price: String,
    /// Base units.
    pub amount: String,
    pub orders: i64,
    pub cum_amount: String,
    /// Sompi: the cumulative amounts at their level prices, rounded down per level.
    pub cum_quote: String,
    pub estimated: bool,
}

/// The depth snapshot of one token: the levels of both sides, best first.
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

/// One aggregated book level in per-basis terms, before merging: (price, amount, orders,
/// estimated).
pub type RawLevel = (i128, i128, i64, bool);

/// Merges levels of equal per-basis price, given best first, and adds cumulative sums.
///
/// It returns at most `levels` rows.
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

/// The depth snapshot of `token`, with at most `levels` merged levels per side.
pub fn depth(conn: &Connection, ctx: &ReadCtx, token: &Hash32, decimals: Option<u32>, levels: usize) -> DbResult<DepthView> {
    let basis = price_basis(conn, token, decimals)?;
    // Fetch generously, because several (price, scale) rows can merge into one per-basis level.
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

    #[test]
    fn depth_reference_is_the_mid_else_the_side_that_exists() {
        assert_eq!(depth_reference(Some(100), Some(110)), Some(105));
        assert_eq!(depth_reference(Some(100), None), Some(100));
        assert_eq!(depth_reference(None, Some(110)), Some(110));
        assert_eq!(depth_reference(None, None), None);
        // A price that is not positive is no reference.
        assert_eq!(depth_reference(Some(0), None), None);
    }

    #[test]
    fn depth_bands_split_the_two_percent_band_by_side() {
        // Both sides: the reference is the mid of 1000 and 1020, which is 1010, and the band is
        // 989.8 ..= 1030.2. The asks at 1020 and 1030 are in and 1031 is out. The bids at 1000 and
        // 990 are in and 989 is out.
        let bids = [(1000, 5), (990, 7), (989, 100)];
        let asks = [(1020, 3), (1030, 11), (1031, 100)];
        assert_eq!(depth_bands(&bids, &asks), Some((12, 14)));
        // One side only: the reference is the best price of that side. For bids it is 1000 with the
        // band down to 980. For asks it is 1020 with the band up to 1040.4. The other side is 0.
        assert_eq!(depth_bands(&bids, &[]), Some((112, 0)));
        assert_eq!(depth_bands(&[], &asks), Some((0, 114)));
        // An empty book has no reference.
        assert_eq!(depth_bands(&[], &[]), None);
        // A side with no level in the band is 0: a far bid under a near ask has the reference 600
        // and the band 588 ..= 612.
        assert_eq!(depth_bands(&[(200, 9)], &[(1000, 4)]), Some((0, 0)));
    }

    #[test]
    fn depth_band_bounds_are_inclusive_at_exactly_two_percent() {
        // One side only, with the reference 10 000: an ask at exactly +2 % (10 200) counts and one
        // sompi more does not. A bid at exactly -2 % (9 800) counts and one less does not.
        assert_eq!(depth_bands(&[], &[(10_000, 1), (10_200, 2), (10_201, 4)]), Some((0, 3)));
        assert_eq!(depth_bands(&[(10_000, 1), (9_800, 2), (9_799, 4)], &[]), Some((3, 0)));
    }

    /// A fill of `amount` base units at `price` sompi per whole token of 100 base units (scale
    /// 100). The basis is 100.
    fn f(id: i64, tx: u8, ts: i64, side: i64, amount: i64, price: i64, gdaa: i64) -> FillRow {
        FillRow { id, txid: vec![tx; 32], ts, daa: ts / 100, side, amount, price, scale: 100, since_daa: gdaa }
    }

    #[test]
    fn resting_side_sets_the_price_and_the_aggressor_side() {
        // The ask is older, at 250 per whole token, and the bid is newer, at 260. The ask rested
        // and a buyer crossed it.
        let t = trade_of(&[f(1, 1, 1000, 1, 200, 250, 10), f(2, 1, 1000, 2, 200, 260, 20)], 100).unwrap();
        assert_eq!((t.price, t.amount, t.quote, t.buy, t.fills), (250, 200, 500, true, 2));
        // The bid rested, being older. The seller aggressed, and the price is the price of the bid.
        let t = trade_of(&[f(3, 2, 1000, 1, 100, 250, 30), f(4, 2, 1000, 2, 100, 260, 20)], 100).unwrap();
        assert_eq!((t.price, t.buy), (260, false));
    }

    /// A fill of an order with a long history of fills finds the latest amend of the order through
    /// the amend index, not by walking its events.
    #[test]
    fn the_age_of_a_fill_is_read_through_the_amend_index() {
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let mut st = conn.prepare(&format!("EXPLAIN QUERY PLAN {FILL_SELECT}")).unwrap();
        let plan: Vec<String> = st.query_map([vec![0u8; 32]], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect();
        assert!(plan.iter().any(|p| p.contains("order_events_amends")), "{plan:?}");
    }

    /// One transaction with more fills than a trades page reads is served whole, with a cursor to
    /// the older trades, instead of an empty page without one.
    #[test]
    fn a_trade_with_more_fills_than_a_page_reads_is_served_whole() {
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let token = Hash32([0x51; 32]);
        conn.execute("INSERT INTO ids (h) VALUES (?1), (x'00')", [&token.0[..]]).unwrap();
        conn.execute(
            "INSERT INTO orders (covenant_id, contract, template_id, family, side, token_id, scale, in_book, genesis_state, genesis_block, genesis_daa, listed) \
             VALUES (x'01', 'KobAsk', (SELECT id FROM ids WHERE h = x'00'), 1, 1, (SELECT id FROM ids WHERE h = ?1), 100, 1, x'00', 1, 1, 1)",
            [&token.0[..]],
        )
        .unwrap();
        // An older transaction of 3 fills, then one of 200: more than the 72 rows a page of 5 reads.
        let fills = |tx: u8, n: i64, ts: i64| {
            conn.execute(
                "WITH RECURSIVE k(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM k WHERE i < ?1) \
                 INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price) \
                 SELECT (SELECT id FROM orders), 1, ?2, ?2, ?3, 0, 'fill', (SELECT id FROM ids WHERE h = ?4), 1, 10, 250 FROM k",
                rusqlite::params![n, ts, vec![tx; 32], &token.0[..]],
            )
            .unwrap();
        };
        fills(0xaa, 3, 1_000);
        fills(0xbb, 200, 2_000);
        let ctx = ReadCtx { node_daa: None, settle_depth_daa: 100, now_unix: None };
        let page = trades(&conn, &ctx, &token, None, None, 5).unwrap();
        assert_eq!(page.items.iter().map(|t| (t.fills, t.amount.as_str())).collect::<Vec<_>>(), [(200, "2000")]);
        let next = page.next_cursor.expect("a cursor to the older trade");
        let page = trades(&conn, &ctx, &token, None, next.parse().ok(), 5).unwrap();
        assert_eq!(page.items.iter().map(|t| (t.fills, t.amount.as_str())).collect::<Vec<_>>(), [(3, "30")]);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn one_sided_trade_is_the_direct_takers() {
        // A wallet sold into a resting bid, so there is no ask fill. The bid side measures the
        // volume and is resting.
        let t = trade_of(&[f(5, 3, 1000, 2, 300, 240, 5)], 100).unwrap();
        assert_eq!((t.amount, t.quote, t.price, t.buy), (300, 720, 240, false));
        assert!(trade_of(&[], 100).is_none());
    }

    #[test]
    fn vwap_of_several_resting_fills() {
        // Two asks of the same transaction at 250 x 100 and 270 x 300 base units, and one newer
        // bid: the VWAP is 265 per whole token.
        let t = trade_of(&[f(1, 1, 5, 1, 100, 250, 1), f(2, 1, 5, 1, 300, 270, 2), f(3, 1, 5, 2, 400, 300, 9)], 100).unwrap();
        assert_eq!((t.price, t.amount, t.quote), (265, 400, 1060));
    }

    /// Fills of another scale are converted to the basis. Quotes round down per fill. The
    /// maker-favour rounding of the covenants is that of the fill itself, and a display sum never
    /// rounds up.
    #[test]
    fn fills_of_another_scale_and_amounts_off_the_whole_token() {
        // 50 base units at 250 per 100 (scale 100), and 5 base units at 2 500 per 1 000 (scale
        // 1 000), which is the same price per base unit.
        let other = FillRow { scale: 1_000, ..f(2, 1, 5, 1, 5, 2_500, 1) };
        let t = trade_of(&[f(1, 1, 5, 1, 50, 250, 1), other], 100).unwrap();
        assert_eq!((t.price, t.amount, t.quote), (250, 55, 125 + 12));
        // A basis of 1 000 gives the same trade per 1 000 base units.
        let t = trade_of(&[f(1, 1, 5, 1, 50, 250, 1)], 1_000).unwrap();
        assert_eq!((t.price, t.quote), (2_500, 125));
        // Huge numbers saturate instead of overflowing.
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
