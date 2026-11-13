//! Pair books (`/v1/pairs`): token-for-token markets of the pair orders, and the implied quotes of
//! the KAS route.
//!
//! The pair orders are `KobPair`, `KobCondPair` and `KobIfdPair` (protocol v3).
//!
//! A pair `base/quote` is ORIENTED. The pair orders of A/B are those whose base token is A and
//! quote token is B. Their price is B base units per whole A, that is, per `scale(A)` base units.
//! An order of B/A is in the B/A book and is never inverted into this one.
//!
//! # Levels
//!
//! The book of `base/quote` is shown as three kinds of level:
//! - **Direct levels** (`source: "direct"`) are `KobPair` orders. An ASK (side 1, sells A for B)
//!   is an ask. A BID (side 2, buys A with its B escrow) is a bid. The price is its quote now,
//!   `PairState::price_at`, which is a decaying ask or a rising bid at the evaluation time. The
//!   amount is `amountLeft` base units of A.
//! - **Entry levels** (`source: "entry"`) are `KobIfdPair` entries that rest at a limit. They are
//!   a limit entry, or a stop entry once armed, whose price is its auction quote now. A buy-first
//!   entry (side BID) is a bid and a sell-first entry (side ASK) is an ask. The amount is
//!   `amountLeft`. Unarmed stop entries and every `KobCondPair` (stops, take-profits, OCO and the
//!   exits of entries) are LEFT OUT, as the KAS books leave out their KAS counterparts. Nothing
//!   fills a stop before its trigger, and the take-profit leg of a conditional is not resting
//!   book liquidity.
//! - **Route levels** (`source: "route"`) are what a taker could get in one transaction through
//!   the two KAS books.
//!   - Route bids (sell base for quote) walk the KAS bids of the base token, best first, at the
//!     all-in `(quote + tip) / scale` sompi per base unit. They walk against the KAS asks of the
//!     quote token, at the all-in `(quote − tip) / scale` sompi per quote unit.
//!   - Route asks (buy base with quote) walk the KAS bids of the quote token against the KAS asks
//!     of the base token.
//!   - Each step consumes the smaller KAS side of the current pair of orders. The KAS of an order
//!     is `floor(amount × all-in / scale)`, and the amount of a bid is its buying power.
//!
//! # Prices
//!
//! Every price is `price_num / price_den` quote base units per base base unit, a reduced fraction.
//! It is exact whenever both terms fit in 64 bits. Otherwise it is rounded conservatively to terms
//! that do: route and direct bids down, asks up. Route amounts are floored, so a taker never gets
//! more than shown.
//!
//! Implied quotes are indicative. They ignore the minimum fills and the per-fill rounding of the
//! KAS orders, network fees, token slot limits and the profit rule of the matcher.
//!
//! # Which orders count
//!
//! Only listed, live orders are used. They are open or partially filled, have exact custody (every
//! custody that the state holds, as one live custody UTXO per token at its exact amount), and are
//! not flagged possibly frozen. They are active at the DAA score of the node, and not past their
//! refund time (expiry, 90 days idle) or their day-order deadline. The route uses plain `KobAsk`
//! and `KobBid` orders.
//!
//! A level is resting liquidity that a matcher can fill now, so the book is never crossed by what
//! cannot or does not rest:
//! - **IOC and FOK orders** (`tif` 1 and 2: market, IOC and FOK takers, KAS or pair) are takers in
//!   flight. They are filled against the books or killed, and are never levels.
//! - A **TWAP or DCA order** (`interval` > 0) is a level only once its next slice is due, that is,
//!   its UTXO is `interval` DAA old (the CSV of the covenant), and for at most one slice
//!   (`maxFill`).
//!
//! A taker waits for a matcher for about the lag of the indexer. Shown as a level, it would cross
//! the pair book for that long. The measured size of this is in `docs/ops/executor.md`, 5.4.
//!
//! # Pair prices
//!
//! A price of a pair is never recorded from pair fills. The pair charts are derived from the two
//! KAS series ([`pair_candles`]). A pair fill records volume only ([`pair_fills`],
//! `price_source: "none"`).

use super::db::DbResult;
use super::market::{self, Candle};
use super::packed::{GENESIS_O, ORDER_IDS_O};
use super::reads::{pinned_template, reduce, tip_state, ReadCtx, CUSTODY_OK, NOT_FROZEN};
use crate::hex::Hash32;
use kob_protocol::state::{AnyState, TIF_GTC};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One price level of a pair book. Its numbers are decimal strings, because base units can exceed
/// 2^53.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PairLevel {
    /// `direct` (`KobPair`), `entry` (`KobIfdPair` resting at its limit) or `route` (the implied
    /// quote of the KAS route).
    pub source: &'static str,
    pub price_num: String,
    pub price_den: String,
    /// Base units of the base token.
    pub amount: String,
    /// The orders behind the level. For a route level these are the KAS orders it combines.
    pub orders: i64,
}

/// The pair book of one pair: the levels of both sides, best first.
#[derive(Debug, Clone, Serialize)]
pub struct PairBook {
    pub base: String,
    pub quote: String,
    /// The DAA score the quotes are evaluated at: the node's, else the cursor of the indexer.
    pub daa_score: u64,
    /// Ways to buy base with quote, in ascending price.
    pub asks: Vec<PairLevel>,
    /// Ways to sell base for quote, in descending price.
    pub bids: Vec<PairLevel>,
}

/// A pair with listed live pair orders. It is oriented: `base` is A and `quote` is B.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PairSummary {
    pub base: String,
    pub quote: String,
    /// Listed live `KobPair` asks, which sell A for B.
    pub direct_asks: i64,
    /// Listed live `KobPair` bids, which buy A with B.
    pub direct_bids: i64,
    /// Listed live `KobIfdPair` entries: sell-first (side ASK) and buy-first (side BID).
    pub entry_asks: i64,
    pub entry_bids: i64,
    /// Listed live `KobCondPair` orders: stops, take-profits, OCO and exits.
    pub conditionals: i64,
}

/// A level under construction: exact price terms bounded to 64 bits, the amount, and the orders
/// behind it.
#[derive(Debug, Clone)]
struct Lvl {
    source: &'static str,
    num: u128,
    den: u128,
    amount: u128,
    orders: BTreeSet<(u8, usize)>,
}

/// `num / den` reduced and, when a term exceeds 64 bits, rounded to terms that fit.
///
/// With `up` the result is never below the exact value, else never above it. It is `None` for a
/// term that is not positive.
pub fn bounded_price(num: u128, den: u128, up: bool) -> Option<(u128, u128)> {
    let (mut n, mut d) = reduce(i128::try_from(num).ok()?, i128::try_from(den).ok()?)?;
    let max = u64::MAX as u128;
    if n <= max && d <= max {
        return Some((n, d));
    }
    while n > max || d > max {
        if up {
            n = n.div_ceil(2);
            d = (d / 2).max(1);
        } else {
            n /= 2;
            d = d.div_ceil(2);
        }
    }
    if n == 0 {
        return Some((0, 1));
    }
    reduce(n as i128, d as i128)
}

/// `a / b` compared with `c / d`. All terms are at most 64 bits, so the products fit.
fn cmp_price(a: (u128, u128), b: (u128, u128)) -> std::cmp::Ordering {
    (a.0 * b.1).cmp(&(b.0 * a.1))
}

/// `floor(k × m / d)` without overflow for `k < 2^127`, `0 < m, d < 2^63` and `k / d × m < 2^127`.
fn mul_div(k: u128, m: i64, d: i64) -> u128 {
    let (m, d) = (m as u128, d as u128);
    (k / d).saturating_mul(m).saturating_add((k % d) * m / d)
}

/// A KAS order of the route: all-in sompi per whole token (`rate` per `scale` base units), and
/// the base units available.
#[derive(Debug, Clone, Copy)]
struct KasOrder {
    rate: i64,
    scale: i64,
    amount: i64,
}

impl KasOrder {
    /// Sompi the order moves for its whole amount: `floor(amount × rate / scale)`. This is the
    /// quote rule, exact in 128 bits.
    fn kas(&self) -> u128 {
        kob_protocol::state::quote_exact(self.amount, self.rate, self.scale, kob_protocol::state::Round::Down)
            .map(|v| v.max(0) as u128)
            .unwrap_or(0)
    }
}

/// The DAA score that quotes are evaluated at.
fn eval_daa(conn: &Connection, ctx: &ReadCtx) -> DbResult<u64> {
    Ok(match ctx.node_daa {
        Some(n) => n,
        None => super::book::cursor_daa(conn)?,
    })
}

/// Whether a live order with tip `state` can be filled at `now`.
///
/// `udaa` is the DAA score of the UTXO. The order must be active, not due for a refund, and not
/// past its day-order deadline.
fn eligible(state: &AnyState, udaa: i64, deadline: Option<i64>, now: i64, ctx: &ReadCtx) -> bool {
    if crate::sanity::check(state).is_err() {
        return false;
    }
    if crate::model::terms_of(state).active_from > now {
        return false;
    }
    let due = crate::indexer::processor::guarded(|| state.refund_due(udaa)).flatten().unwrap_or(i64::MAX);
    if now >= due {
        return false;
    }
    !matches!((deadline, ctx.now_unix), (Some(d), Some(n)) if n as i64 >= d)
}

/// Whether the live custody rows of a pair order are exactly what its state holds
/// ([`AnyState::custodies`]).
///
/// That is one live custody UTXO per custody token, at the exact amount, and no other custody row.
pub fn pair_custody_ok(conn: &Connection, cov: &[u8], state: &AnyState) -> DbResult<bool> {
    let mut st = conn.prepare_cached(
        "SELECT tk.h, t.amount FROM token_utxos t JOIN ids tk ON tk.id = t.token_id \
         WHERE t.order_id = (SELECT id FROM orders WHERE covenant_id = ?1) AND t.role = 'custody' AND t.spent_block IS NULL",
    )?;
    let mut live: Vec<(Vec<u8>, i64)> = st.query_map([cov], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut want: Vec<(Vec<u8>, i64)> = state.custodies().into_iter().filter(|c| c.1 != 0).map(|(t, a)| (t.to_vec(), a)).collect();
    live.sort();
    want.sort();
    Ok(live == want)
}

/// A listed live pair order of the pair, with its tip state and the DAA score of its UTXO.
struct LivePair {
    state: AnyState,
    udaa: i64,
}

/// The listed live pair orders of `base / quote`, that orientation only, that are eligible now and
/// have an exact custody.
fn pair_orders(conn: &Connection, base: &Hash32, quote: &Hash32, now: i64, ctx: &ReadCtx) -> DbResult<Vec<LivePair>> {
    let sql = format!(
        "SELECT o.covenant_id, o.contract, kob_ustate(u.state, {GENESIS_O}), u.created_daa, o.deadline FROM order_state s \
         CROSS JOIN orders o ON o.id = s.order_id JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx{ORDER_IDS_O} \
         WHERE o.listed = 1 AND o.token_id = (SELECT id FROM ids WHERE h = ?1) AND o.quote_id = (SELECT id FROM ids WHERE h = ?2) AND s.status IN ('open','partial') AND s.state_known = 1 \
         AND u.spent_block IS NULL AND {NOT_FROZEN} AND {pinned} ORDER BY o.genesis_block, o.covenant_id LIMIT 2000",
        pinned = pinned_template()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![base.0.to_vec(), quote.0.to_vec()], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<Vec<u8>>>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut out = vec![];
    for r in rows {
        let (cov, contract, state, udaa, deadline) = r?;
        let Some(s) = tip_state(&contract, state) else { continue };
        if !s.is_pair() || !eligible(&s, udaa, deadline, now, ctx) || !pair_custody_ok(conn, &cov, &s)? {
            continue;
        }
        out.push(LivePair { state: s, udaa });
    }
    Ok(out)
}

/// The plain KAS orders of one token and side (1 asks, 2 bids) that a route can use, best first.
fn kas_orders(conn: &Connection, token: &Hash32, side: i64, limit: usize, now: i64, ctx: &ReadCtx) -> DbResult<Vec<KasOrder>> {
    let (kinds, dir) = if side == 1 { ("'KobAsk','KobAskKron'", "ASC") } else { ("'KobBid','KobBidKron'", "DESC") };
    let sql = format!(
        "SELECT o.contract, kob_ustate(u.state, {GENESIS_O}), u.created_daa, s.remaining_amount, o.deadline FROM order_state s \
         CROSS JOIN orders o ON o.id = s.order_id JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx{ORDER_IDS_O} \
         WHERE o.token_id = (SELECT id FROM ids WHERE h = ?1) AND o.side = ?2 AND o.listed = 1 AND o.in_book = 1 AND o.contract IN ({kinds}) \
         AND s.status IN ('open','partial') AND s.state_known = 1 AND u.spent_block IS NULL AND {NOT_FROZEN} AND {CUSTODY_OK} \
         AND {pinned} ORDER BY (o.price * 1.0 / MAX(COALESCE(o.scale, 1), 1)) {dir}, o.genesis_block ASC, o.covenant_id ASC LIMIT ?3",
        pinned = pinned_template()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![token.0.to_vec(), side, limit as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<Vec<u8>>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut out = vec![];
    for r in rows {
        let (contract, state, udaa, remaining, deadline) = r?;
        let Some(s) = tip_state(&contract, state) else { continue };
        if !eligible(&s, udaa, deadline, now, ctx) {
            continue;
        }
        // The quote at the evaluation time, because auctions move. It is all-in per whole token
        // with checked arithmetic: what the maker of an ask receives (`price - tip`), or what the
        // maker of a bid pays (`price + tip`). The amount of a bid is its buying power. Only
        // resting orders count: a TWAP slice once due, and at most one slice (see the module doc).
        let resting = |tif: i64, interval: i64| tif == TIF_GTC && (interval <= 0 || now >= udaa.saturating_add(interval));
        let slice = |amount: i64, max_fill: i64| if max_fill > 0 { amount.min(max_fill) } else { amount };
        let k = crate::indexer::processor::guarded(|| match s.clone().into_family(kob_protocol::family::Family::Kcc20) {
            AnyState::KobAsk(a) if resting(a.tif, a.interval) => {
                let q = a.price_at(now, udaa)?;
                Some((q.checked_sub(a.tip)?, a.scale, slice(a.amount_left, a.max_fill)))
            }
            AnyState::KobBid(b) if resting(b.tif, b.interval) => {
                let q = b.price_at(now, udaa)?;
                Some((q.checked_add(b.tip)?, b.scale, slice(remaining?, b.max_fill)))
            }
            _ => None,
        })
        .flatten();
        if let Some((rate, scale, amount)) = k {
            if rate > 0 && scale > 0 && amount > 0 {
                out.push(KasOrder { rate, scale, amount });
            }
        }
    }
    // Best first: bids pay the most per base unit and asks cost the least. Ties keep the book
    // order.
    out.sort_by(|a, b| {
        let (x, y) = (a.rate as i128 * b.scale as i128, b.rate as i128 * a.scale as i128);
        if side == 1 {
            x.cmp(&y)
        } else {
            y.cmp(&x)
        }
    });
    Ok(out)
}

/// Adds a level, merging it into the last one when the price and the source are the same.
fn push_level(levels: &mut Vec<Lvl>, l: Lvl) {
    if let Some(last) = levels.last_mut() {
        if last.source == l.source && (last.num, last.den) == (l.num, l.den) {
            last.amount = last.amount.saturating_add(l.amount);
            last.orders.extend(l.orders);
            return;
        }
    }
    levels.push(l);
}

/// Route levels of the KAS orders of `base` against those of `quote`.
///
/// With `bids`, the route sells base: the bids of the base token against the asks of the quote
/// token. Otherwise it buys base: the asks of the base token against the bids of the quote token.
fn route_levels(base: &[KasOrder], quote: &[KasOrder], bids: bool, depth: usize) -> Vec<Lvl> {
    let mut out: Vec<Lvl> = vec![];
    let (mut i, mut j) = (0usize, 0usize);
    let mut left_b: Vec<u128> = base.iter().map(KasOrder::kas).collect();
    let mut left_q: Vec<u128> = quote.iter().map(KasOrder::kas).collect();
    while i < base.len() && j < quote.len() {
        let (x, y) = (base[i], quote[j]);
        let k = left_b[i].min(left_q[j]);
        // Quote units per base unit are (sompi per base unit) / (sompi per quote unit), which is
        // (x.rate / x.scale) / (y.rate / y.scale).
        let num = x.rate as u128 * y.scale as u128;
        let den = x.scale as u128 * y.rate as u128;
        // The base units that k sompi buy or sell at the rate of x, rounded down.
        let amount = mul_div(k, x.scale, x.rate);
        if let Some((n, d)) = bounded_price(num, den, !bids) {
            if amount > 0 {
                let l = Lvl { source: "route", num: n, den: d, amount, orders: [(0u8, i), (1u8, j)].into_iter().collect() };
                if out.len() == depth && out.last().is_some_and(|o| (o.num, o.den) != (n, d)) {
                    break;
                }
                push_level(&mut out, l);
            }
        }
        left_b[i] -= k;
        left_q[j] -= k;
        if left_b[i] == 0 {
            i += 1;
        }
        if left_q[j] == 0 {
            j += 1;
        }
    }
    out
}

fn to_view(l: Lvl) -> PairLevel {
    PairLevel {
        source: l.source,
        price_num: l.num.to_string(),
        price_den: l.den.to_string(),
        amount: l.amount.to_string(),
        orders: l.orders.len() as i64,
    }
}

/// The sort rank of the source of a level at one price: pair orders first (direct, then entry),
/// the route last.
fn source_rank(s: &str) -> u8 {
    match s {
        "direct" => 0,
        "entry" => 1,
        _ => 2,
    }
}

/// Sorts levels and keeps `depth` of them.
///
/// Asks are ascending and bids descending. At one price the pair orders come first.
fn finish(mut levels: Vec<Lvl>, asks: bool, depth: usize) -> Vec<PairLevel> {
    levels.sort_by(|a, b| {
        let o = cmp_price((a.num, a.den), (b.num, b.den));
        (if asks { o } else { o.reverse() }).then(source_rank(a.source).cmp(&source_rank(b.source)))
    });
    levels.into_iter().take(depth).map(to_view).collect()
}

/// The resting level of a pair order now: `(ask, source, quote B per whole A, scale(A), amount of
/// A)`.
///
/// It is `None` for the orders that the book leaves out: a `KobCondPair`, an unarmed stop entry, an
/// IOC or FOK taker, a TWAP slice that is not due yet, and an order with nothing left.
fn resting_level(p: &LivePair, now: i64) -> Option<(bool, &'static str, i64, i64, i64)> {
    crate::indexer::processor::guarded(|| match &p.state {
        AnyState::KobPair(s) if s.tif == TIF_GTC && (s.interval <= 0 || now >= p.udaa.saturating_add(s.interval)) => {
            let amount = if s.max_fill > 0 { s.amount_left.min(s.max_fill) } else { s.amount_left };
            Some((s.is_ask(), "direct", s.price_at(now, p.udaa)?, s.a_scale(), amount))
        }
        AnyState::KobIfdPair(e) if e.entry_stop == 0 || e.armed != 0 => {
            Some((!e.is_buy_first(), "entry", e.price_at(false, now, p.udaa)?, e.a_scale, e.amount_left))
        }
        _ => None,
    })
    .flatten()
    .filter(|l| l.2 > 0 && l.3 > 0 && l.4 > 0)
}

/// Whether the token has any listed order: an index seek on the token, then the first listed row.
///
/// The live-set scans below walk every open order of the store whatever the token, so a leg
/// without a listed order must not start one. The cost of a scan is in `docs/ops/executor.md`,
/// 5.4.
fn has_listed(conn: &Connection, token: &Hash32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM orders WHERE token_id = (SELECT id FROM ids WHERE h = ?1) AND listed = 1 LIMIT 1")?
        .exists([&token.0[..]])?)
}

/// The pair book of `base / quote`, given as token covenant ids, with at most `depth` levels per
/// side.
pub fn pair_book(conn: &Connection, ctx: &ReadCtx, base: &Hash32, quote: &Hash32, depth: usize) -> DbResult<PairBook> {
    let daa = eval_daa(conn, ctx)?;
    let now = daa.min(i64::MAX as u64) as i64;
    let depth = depth.max(1);
    let mut asks: Vec<Lvl> = vec![];
    let mut bids: Vec<Lvl> = vec![];
    // Pair orders are listed under their base token. A route needs a listed order on both legs.
    let base_live = base != quote && has_listed(conn, base)?;
    let routes = base_live && has_listed(conn, quote)?;
    if base_live {
        // Direct and entry levels: the pair orders of the pair, grouped by source and exact price
        // (B per A base unit).
        let mut grouped: BTreeMap<(bool, &'static str, (u128, u128)), Lvl> = BTreeMap::new();
        for (k, p) in pair_orders(conn, base, quote, now, ctx)?.iter().enumerate() {
            let Some((ask, source, price, scale, amount)) = resting_level(p, now) else { continue };
            let Some(pr) = bounded_price(price as u128, scale as u128, ask) else { continue };
            let l = grouped.entry((ask, source, pr)).or_insert_with(|| Lvl {
                source,
                num: pr.0,
                den: pr.1,
                amount: 0,
                orders: BTreeSet::new(),
            });
            l.amount = l.amount.saturating_add(amount as u128);
            l.orders.insert((2, k));
        }
        for ((ask, _, _), l) in grouped {
            if ask {
                asks.push(l);
            } else {
                bids.push(l);
            }
        }
        // Route levels: the two KAS books in one transaction.
        if routes {
            let n = (4 * depth + 16).min(400);
            let base_bids = kas_orders(conn, base, 2, n, now, ctx)?;
            let quote_asks = kas_orders(conn, quote, 1, n, now, ctx)?;
            bids.extend(route_levels(&base_bids, &quote_asks, true, depth));
            let base_asks = kas_orders(conn, base, 1, n, now, ctx)?;
            let quote_bids = kas_orders(conn, quote, 2, n, now, ctx)?;
            asks.extend(route_levels(&base_asks, &quote_bids, false, depth));
        }
    }
    Ok(PairBook {
        base: base.to_hex(),
        quote: quote.to_hex(),
        daa_score: daa,
        asks: finish(asks, true, depth),
        bids: finish(bids, false, depth),
    })
}

/// The pairs with at least one listed live pair order, optionally only those naming `token` as
/// base or quote.
///
/// Pairs are oriented as the orders are: `base` is A and `quote` is B, and an order of B/A is
/// counted under B/A. They are sorted by (base, quote).
pub fn pairs(conn: &Connection, ctx: &ReadCtx, token: Option<&Hash32>) -> DbResult<Vec<PairSummary>> {
    let now = eval_daa(conn, ctx)?.min(i64::MAX as u64) as i64;
    let sql = format!(
        "SELECT o.covenant_id, o.contract, kob_ustate(u.state, {GENESIS_O}), u.created_daa, o.deadline, o_tk.h, o_qt.h FROM order_state s \
         CROSS JOIN orders o ON o.id = s.order_id JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx{ORDER_IDS_O} \
         WHERE o.listed = 1 AND o.quote_id IS NOT NULL AND s.status IN ('open','partial') AND s.state_known = 1 \
         AND u.spent_block IS NULL AND (?1 IS NULL OR o_tk.h = ?1 OR o_qt.h = ?1) AND {NOT_FROZEN} \
         AND {pinned} ORDER BY o.genesis_block, o.covenant_id LIMIT 20000",
        pinned = pinned_template()
    );
    let mut st = conn.prepare(&sql)?;
    type Row = (Vec<u8>, String, Option<Vec<u8>>, i64, Option<i64>, Vec<u8>, Vec<u8>);
    let rows: Vec<Row> = st
        .query_map(params![token.map(|t| t.0.to_vec())], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut counts: BTreeMap<(Hash32, Hash32), PairSummary> = BTreeMap::new();
    for (cov, contract, state, udaa, deadline, a, b) in rows {
        let (Some(a), Some(b)) = (Hash32::from_slice(&a), Hash32::from_slice(&b)) else { continue };
        let Some(s) = tip_state(&contract, state) else { continue };
        if a == b || !s.is_pair() || !eligible(&s, udaa, deadline, now, ctx) || !pair_custody_ok(conn, &cov, &s)? {
            continue;
        }
        let e = counts.entry((a, b)).or_insert_with(|| PairSummary {
            base: a.to_hex(),
            quote: b.to_hex(),
            direct_asks: 0,
            direct_bids: 0,
            entry_asks: 0,
            entry_bids: 0,
            conditionals: 0,
        });
        match &s {
            AnyState::KobPair(p) if p.is_ask() => e.direct_asks += 1,
            AnyState::KobPair(_) => e.direct_bids += 1,
            AnyState::KobIfdPair(x) if x.is_buy_first() => e.entry_bids += 1,
            AnyState::KobIfdPair(_) => e.entry_asks += 1,
            _ => e.conditionals += 1,
        }
    }
    Ok(counts.into_values().collect())
}

// Pair charts.

/// An exact rate of a pair candle.
///
/// `value` is `floor(num / den)` B base units per `price_basis` base units of A, where
/// `price_basis` is the basis of the base token. `num / den` is the reduced exact fraction.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PairRate {
    pub value: String,
    pub num: String,
    pub den: String,
}

/// One pair candle derived from the two KAS series, see [`pair_candles`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PairCandleView {
    pub t: i64,
    pub o: PairRate,
    pub h: PairRate,
    pub l: PairRate,
    pub c: PairRate,
    /// Whether A traded in KAS in this bucket, and likewise B for `b_traded`. When it is `false`,
    /// that side carries its last close forward.
    pub a_traded: bool,
    pub b_traded: bool,
    /// The pair volume of the bucket: the pair-order fills of this pair (`pair_fills`), in base
    /// units of A and of B, and the number of transactions. A netting fill, which both its orders
    /// record, counts once.
    pub pair_volume_a: String,
    pub pair_volume_b: String,
    pub pair_fills: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairCandlesView {
    pub base: String,
    pub quote: String,
    pub interval: String,
    /// The base units of A that a rate is per: the KAS price basis of A (`market::price_basis`).
    pub price_basis: String,
    /// The KAS price basis of B: the base units of B that its KAS price is per.
    pub quote_price_basis: String,
    pub decimals: Option<u32>,
    pub quote_decimals: Option<u32>,
    /// Always `kas_books`: derived from the KAS trades of A and of B, never from pair fills.
    pub price_source: &'static str,
    pub items: Vec<PairCandleView>,
}

/// `pa / pb` as B base units per `basis_a` base units of A.
///
/// `pa` is the KAS price of A, in sompi per `basis_a` base units of A. `pb` is the KAS price of B,
/// in sompi per `basis_b` base units of B. The rate is `pa × basis_b / pb`, exact (reduced) and
/// floored.
pub fn pair_rate(pa: i128, pb: i128, basis_b: i128) -> Option<PairRate> {
    let num = pa.checked_mul(basis_b)?;
    let (n, d) = reduce(num, pb)?;
    Some(PairRate { value: (n / d).to_string(), num: n.to_string(), den: d.to_string() })
}

/// One `pair_fills` row as the volume reads it: `(txid, ts, side, amount_a, amount_b)`.
type PairFillRow = (Vec<u8>, i64, i64, i64, Option<i64>);

/// The pair volume of `pair_fills` rows, one entry per TRANSACTION in the order first seen:
/// `(ts, A base units, B base units)`.
///
/// Every pair order that a transaction fills records its own row, so a netting fill (an ask and a
/// bid of the pair filling each other) is two rows of one trade. The volume of a transaction is
/// the larger of the sums of its two sides. Each side's rows are summed in 128 bits, and a
/// negative or missing amount counts as 0. So a netting fill counts once, and the routed rest of
/// a partly netted order still counts.
fn tx_volumes(rows: impl IntoIterator<Item = PairFillRow>) -> Vec<(i64, i128, i128)> {
    // txid -> (ts, [A of asks, A of bids], [B of asks, B of bids]). `order` keeps the first-seen
    // order.
    let mut by: HashMap<Vec<u8>, (i64, [i128; 2], [i128; 2])> = HashMap::new();
    let mut order: Vec<Vec<u8>> = Vec::new();
    for (txid, ts, side, a, b) in rows {
        let k = usize::from(side != 1);
        let e = by.entry(txid.clone()).or_insert_with(|| {
            order.push(txid);
            (ts, [0; 2], [0; 2])
        });
        e.1[k] = e.1[k].saturating_add(a.max(0) as i128);
        e.2[k] = e.2[k].saturating_add(b.unwrap_or(0).max(0) as i128);
    }
    order.into_iter().filter_map(|t| by.remove(&t)).map(|(ts, a, b)| (ts, a[0].max(a[1]), b[0].max(b[1]))).collect()
}

/// The `pair_fills` rows of `query`, with its parameters `params`, read for [`tx_volumes`].
fn pair_fill_rows(conn: &Connection, query: &str, params: impl rusqlite::Params) -> DbResult<Vec<PairFillRow>> {
    let mut st = conn.prepare_cached(query)?;
    let rows = st.query_map(params, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The pair volume per bucket in `[start, end)`: `t -> (A base units, B base units, transactions)`.
///
/// A netting fill counts once, see [`tx_volumes`].
fn pair_volume(
    conn: &Connection,
    a: &Hash32,
    b: &Hash32,
    iv: i64,
    start: i64,
    end: i64,
) -> DbResult<BTreeMap<i64, (i128, i128, u64)>> {
    let rows = pair_fill_rows(
        conn,
        "SELECT txid, ts, side, amount_a, amount_b FROM pair_fills WHERE base_cov_id = ?1 AND quote_cov_id = ?2 AND ts >= ?3 \
         AND ts < ?4 ORDER BY ts LIMIT 200000",
        params![a.0.to_vec(), b.0.to_vec(), start, end],
    )?;
    let mut m: BTreeMap<i64, (i128, i128, u64)> = BTreeMap::new();
    for (ts, x, y) in tx_volumes(rows) {
        let e = m.entry(ts.div_euclid(iv) * iv).or_default();
        e.0 = e.0.saturating_add(x);
        e.1 = e.1.saturating_add(y);
        e.2 += 1;
    }
    Ok(m)
}

/// Pair candles of `base / quote`, derived from the two KAS series.
///
/// No pair price is ever recorded from pair fills. For every bucket in which A or B traded in KAS
/// (`market` candles, with prices per the basis of each token):
/// - The open is `open(A) / open(B)` and the close is `close(A) / close(B)`.
/// - The high is `high(A) / low(B)` and the low is `low(A) / high(B)`. These are the bounds of
///   the implied rate that the two series allow in the bucket, not observed trades.
///
/// Each is B base units per `price_basis` base units of A, see [`pair_rate`]: an exact reduced
/// fraction with `value` floored. A side without a KAS trade in the bucket carries its last close
/// forward, also from before the window. Buckets before both tokens have a price are omitted.
/// Without `from`, the window is the `limit` intervals ending with the bucket of the newest trade
/// of either token.
#[allow(clippy::too_many_arguments)]
pub fn pair_candles(
    conn: &Connection,
    base: &Hash32,
    quote: &Hash32,
    decimals: (Option<u32>, Option<u32>),
    interval: &str,
    iv: i64,
    from: Option<i64>,
    to: Option<i64>,
    limit: usize,
) -> DbResult<PairCandlesView> {
    let basis_a = market::price_basis(conn, base, decimals.0)?;
    let basis_b = market::price_basis(conn, quote, decimals.1)?;
    let newest = market::newest_trade_ts(conn, base)?.max(market::newest_trade_ts(conn, quote)?);
    let end = match (to, newest) {
        (Some(t), _) => t,
        (None, Some(n)) => (n.div_euclid(iv) + 1) * iv,
        (None, None) => 0,
    };
    let start = from.unwrap_or_else(|| end.saturating_sub(iv.saturating_mul(limit as i64)));
    let mut items = vec![];
    if end > start {
        // The newest `limit` candles of each side hold every bucket of the newest `limit` of the
        // union: a bucket of A among them has fewer than `limit` newer buckets of A. The older ones
        // are dropped and only their last close is carried, read as the last trade before the
        // first bucket kept.
        let mut ca: BTreeMap<i64, Candle> =
            market::candle_series(conn, base, basis_a, iv, start, end, limit)?.into_iter().map(|c| (c.t, c)).collect();
        let mut cb: BTreeMap<i64, Candle> =
            market::candle_series(conn, quote, basis_b, iv, start, end, limit)?.into_iter().map(|c| (c.t, c)).collect();
        let mut buckets: BTreeSet<i64> = ca.keys().chain(cb.keys()).copied().collect();
        // The start of the window, or the first bucket kept when older ones were dropped. It is
        // then after the start.
        let mut first = start;
        if buckets.len() > limit {
            while buckets.len() > limit {
                buckets.pop_first();
            }
            first = buckets.first().copied().unwrap_or(start);
            ca.retain(|t, _| *t >= first);
            cb.retain(|t, _| *t >= first);
        }
        let vol = pair_volume(conn, base, quote, iv, first, end)?;
        let mut last_a = market::last_trade_price_before(conn, base, basis_a, first)?;
        let mut last_b = market::last_trade_price_before(conn, quote, basis_b, first)?;
        for t in buckets {
            // The (o, h, l, c) of each side: its candle, or its last close carried forward.
            let side = |c: Option<&Candle>, last: Option<i128>| c.map(|c| (c.o, c.h, c.l, c.c)).or(last.map(|p| (p, p, p, p)));
            let (a, b) = (side(ca.get(&t), last_a), side(cb.get(&t), last_b));
            if let Some(c) = ca.get(&t) {
                last_a = Some(c.c);
            }
            if let Some(c) = cb.get(&t) {
                last_b = Some(c.c);
            }
            let (Some(a), Some(b)) = (a, b) else { continue };
            let r = |pa: i128, pb: i128| pair_rate(pa, pb, basis_b);
            let (Some(o), Some(h), Some(l), Some(c)) = (r(a.0, b.0), r(a.1, b.2), r(a.2, b.1), r(a.3, b.3)) else { continue };
            let v = vol.get(&t).copied().unwrap_or_default();
            items.push(PairCandleView {
                t,
                o,
                h,
                l,
                c,
                a_traded: ca.contains_key(&t),
                b_traded: cb.contains_key(&t),
                pair_volume_a: v.0.to_string(),
                pair_volume_b: v.1.to_string(),
                pair_fills: v.2,
            });
        }
        if items.len() > limit {
            items.drain(..items.len() - limit);
        }
    }
    Ok(PairCandlesView {
        base: base.to_hex(),
        quote: quote.to_hex(),
        interval: interval.to_string(),
        price_basis: basis_a.to_string(),
        quote_price_basis: basis_b.to_string(),
        decimals: decimals.0,
        quote_decimals: decimals.1,
        price_source: "kas_books",
        items,
    })
}

// Pair fills.

/// One pair-order fill (`pair_fills`): volume, never a price source.
#[derive(Debug, Clone, Serialize)]
pub struct PairFillView {
    pub id: i64,
    pub txid: String,
    pub ts: i64,
    pub daa: i64,
    /// The filled pair order and its kind.
    pub order: String,
    pub contract: String,
    /// `ask` (the order sold A) or `bid` (it bought A).
    pub side: &'static str,
    /// Base units of A filled.
    pub amount_a: String,
    /// Base units of B the maker received (ask) or paid (bid).
    pub amount_b: Option<String>,
    /// The quote of the order at the fill, in B base units per whole A (`a_scale` base units), and
    /// as a fraction per A base unit.
    pub price: Option<String>,
    pub price_num: Option<String>,
    pub price_den: Option<String>,
    pub a_scale: i64,
    /// KAS tip the fill released to the filler (sompi).
    pub tip_kas: Option<String>,
    /// `route` (the transaction also filled KAS-book orders of A or B, whose KAS trades are in
    /// `/v1/trades`), `netting` (an opposite pair order of the pair, and no KAS-book fill) or
    /// `inventory` (the own tokens of the filler).
    pub counterparty: String,
    /// Always `none`: a pair fill never sets a price, candle or last price.
    pub price_source: String,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

/// The 24 h pair volume.
///
/// The window ends at the newer of the newest chain block time the indexer knows and its wall
/// clock ([`ReadCtx::window_end`]).
#[derive(Debug, Clone, Serialize)]
pub struct PairVolume {
    /// Base units of A and of B traded. Each transaction counts once, so a netting fill, which both
    /// its orders record, counts once.
    pub amount_a: String,
    pub amount_b: String,
    /// Transactions that filled pair orders of the pair. A netting fill is one.
    pub fills: u64,
}

/// The pair fills of one pair, newest first, with the 24 h pair volume.
#[derive(Debug, Clone, Serialize)]
pub struct PairFillsView {
    pub base: String,
    pub quote: String,
    pub volume_24h: PairVolume,
    /// Newest first.
    pub items: Vec<PairFillView>,
    /// The `before` cursor of the next page, a fill id, when there may be more.
    pub next_cursor: Option<String>,
}

/// Pair fills of `base / quote`, newest first, and the 24 h pair volume.
///
/// `before` is a fill id and is exclusive.
pub fn pair_fills(
    conn: &Connection,
    ctx: &ReadCtx,
    base: &Hash32,
    quote: &Hash32,
    before: Option<i64>,
    limit: usize,
) -> DbResult<PairFillsView> {
    let mut st = conn.prepare_cached(
        "SELECT id, txid, ts, daa, covenant_id, contract, side, amount_a, amount_b, price, a_scale, tip_kas, counterparty, price_source \
         FROM pair_fills WHERE base_cov_id = ?1 AND quote_cov_id = ?2 AND id < ?3 ORDER BY id DESC LIMIT ?4",
    )?;
    let rows = st.query_map(params![base.0.to_vec(), quote.0.to_vec(), before.unwrap_or(i64::MAX), limit as i64 + 1], |r| {
        let price: Option<i64> = r.get(9)?;
        let a_scale: i64 = r.get(10)?;
        let frac = price.and_then(|p| reduce(p as i128, a_scale as i128));
        let daa: i64 = r.get(3)?;
        Ok(PairFillView {
            id: r.get(0)?,
            txid: crate::hex::encode(&r.get::<_, Vec<u8>>(1)?),
            ts: r.get(2)?,
            daa,
            order: crate::hex::encode(&r.get::<_, Vec<u8>>(4)?),
            contract: r.get(5)?,
            side: if r.get::<_, i64>(6)? == 1 { "ask" } else { "bid" },
            amount_a: r.get::<_, i64>(7)?.to_string(),
            amount_b: r.get::<_, Option<i64>>(8)?.map(|v| v.to_string()),
            price: price.map(|v| v.to_string()),
            price_num: frac.map(|f| f.0.to_string()),
            price_den: frac.map(|f| f.1.to_string()),
            a_scale,
            tip_kas: r.get::<_, Option<i64>>(11)?.map(|v| v.to_string()),
            counterparty: r.get(12)?,
            price_source: r.get(13)?,
            confirmations: ctx.confirmations(daa),
            settled: ctx.settled(daa),
        })
    })?;
    let mut items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more { items.last().map(|f| f.id.to_string()) } else { None };
    let newest: Option<i64> = conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get(0))?;
    let since = ctx.window_end(newest).unwrap_or(0) - 86_400_000;
    let rows = pair_fill_rows(
        conn,
        "SELECT txid, ts, side, amount_a, amount_b FROM pair_fills WHERE base_cov_id = ?1 AND quote_cov_id = ?2 AND ts >= ?3",
        params![base.0.to_vec(), quote.0.to_vec(), since],
    )?;
    let (mut va, mut vb, mut n) = (0i128, 0i128, 0u64);
    for (_, x, y) in tx_volumes(rows) {
        va = va.saturating_add(x);
        vb = vb.saturating_add(y);
        n += 1;
    }
    Ok(PairFillsView {
        base: base.to_hex(),
        quote: quote.to_hex(),
        volume_24h: PairVolume { amount_a: va.to_string(), amount_b: vb.to_string(), fills: n },
        items,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_are_exact_when_they_fit_and_conservative_otherwise() {
        assert_eq!(bounded_price(6, 4, false), Some((3, 2)));
        assert_eq!(bounded_price(6, 4, true), Some((3, 2)));
        assert_eq!(bounded_price(0, 4, true), None);
        let big = (u64::MAX as u128) * 3 + 1;
        let (n, d) = bounded_price(big, 7, false).unwrap();
        assert!(n <= u64::MAX as u128 && d <= u64::MAX as u128);
        // Down: never above the exact value. Up: never below it.
        assert!(n * 7 <= big * d, "rounded down");
        let (n, d) = bounded_price(big, 7, true).unwrap();
        assert!(n * 7 >= big * d, "rounded up");
    }

    /// The pair volume of a transaction counts once, however many of its pair orders record it.
    #[test]
    fn a_netting_fill_is_one_trade_of_the_pair_volume() {
        let tx = |n: u8| vec![n; 32];
        let v = tx_volumes([
            // A netting fill: the ask and the bid each record 2 000 A for 4 000 B.
            (tx(1), 10, 1, 2_000, Some(4_000)),
            (tx(1), 10, 2, 2_000, Some(4_000)),
            // A route fill of one ask.
            (tx(2), 20, 1, 1_000, Some(2_000)),
            // An ask of 10 000 nets 6 000 against a bid and routes the other 4 000, so the volume
            // is 10 000 A and 20 000 B.
            (tx(3), 30, 1, 10_000, Some(20_000)),
            (tx(3), 30, 2, 6_000, Some(12_000)),
            // Two asks routed in one transaction: both sold, 3 000 + 500.
            (tx(4), 40, 1, 3_000, Some(6_000)),
            (tx(4), 40, 1, 500, None),
            // A hostile negative amount counts as 0.
            (tx(5), 50, 2, -7, Some(-1)),
        ]);
        assert_eq!(v, vec![(10, 2_000, 4_000), (20, 1_000, 2_000), (30, 10_000, 20_000), (40, 3_500, 6_000), (50, 0, 0)]);
    }

    #[test]
    fn mul_div_floors_without_overflow() {
        assert_eq!(mul_div(10, 3, 4), 7);
        let k = (i64::MAX as u128) * (i64::MAX as u128) / 2;
        assert_eq!(mul_div(k, i64::MAX, i64::MAX), k);
    }

    #[test]
    fn route_walk_consumes_the_smaller_kas_side_and_merges_equal_prices() {
        // Base bids pay 2 sompi per base unit (1 000 base units at 2 000 per whole token of 1 000).
        // Quote asks cost 1 sompi per quote unit (500, then 5 000 base units at 1 000 per whole
        // token).
        let base = [KasOrder { rate: 2_000, scale: 1_000, amount: 1_000 }];
        let quote = [KasOrder { rate: 1_000, scale: 1_000, amount: 500 }, KasOrder { rate: 1_000, scale: 1_000, amount: 5_000 }];
        let l = route_levels(&base, &quote, true, 10);
        // One price (2 quote per base) and all 1000 base units: the two asks merge into one level
        // of three orders.
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].num, l[0].den, l[0].amount, l[0].orders.len()), (2, 1, 1000, 3));
        // A worse second ask starts a new level.
        let quote = [KasOrder { rate: 1_000, scale: 1_000, amount: 500 }, KasOrder { rate: 2_000, scale: 1_000, amount: 5_000 }];
        let l = route_levels(&base, &quote, true, 10);
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].num, l[0].den, l[0].amount), (2, 1, 250));
        assert_eq!((l[1].num, l[1].den, l[1].amount), (1, 1, 750));
        assert_eq!(route_levels(&base, &quote, true, 1).len(), 1, "depth bounds the walk");
    }

    /// The KAS of an order is the floor of the quote rule, exact in 128 bits. Orders of different
    /// scales walk at their price per base unit.
    #[test]
    fn route_amounts_follow_the_quote_rule_at_any_scale() {
        // 999 base units at 2 001 sompi per whole token of 1 000: floor(1 998 999 / 1 000) = 1 998
        // sompi.
        assert_eq!(KasOrder { rate: 2_001, scale: 1_000, amount: 999 }.kas(), 1_998);
        // The largest amounts never overflow.
        assert_eq!(KasOrder { rate: i64::MAX, scale: 1, amount: i64::MAX }.kas(), (i64::MAX as u128) * (i64::MAX as u128));
        // A base bid of scale 100 (2 sompi per base unit) against a quote ask of scale 10^8 (1
        // sompi per base unit).
        let base = [KasOrder { rate: 200, scale: 100, amount: 1_000 }];
        let quote = [KasOrder { rate: 100_000_000, scale: 100_000_000, amount: 10_000 }];
        let l = route_levels(&base, &quote, true, 10);
        assert_eq!((l[0].num, l[0].den, l[0].amount), (2, 1, 1_000));
    }

    /// A pair rate is the KAS price of A over that of B, in B base units per A basis. It is exact
    /// and floored.
    #[test]
    fn pair_rates_divide_the_two_kas_prices_exactly() {
        // A at 3 000 sompi per 1 000 base units, B at 2 000 sompi per 100 base units:
        // 3 000 × 100 / 2 000 = 150 B base units.
        let r = pair_rate(3_000, 2_000, 100).unwrap();
        assert_eq!((r.value.as_str(), r.num.as_str(), r.den.as_str()), ("150", "150", "1"));
        // 1 000 × 100 / 3 000 = 100 / 3, floored to 33, with the exact value 100/3.
        let r = pair_rate(1_000, 3_000, 100).unwrap();
        assert_eq!((r.value.as_str(), r.num.as_str(), r.den.as_str()), ("33", "100", "3"));
        assert!(pair_rate(1, 0, 100).is_none());
    }
}
