//! Read-side queries over the indexer database, used by the REST/WebSocket API.
//!
//! Conventions (stable API contract):
//! - ids, hashes and keys are lowercase hex;
//! - every sompi amount (`value`, `payout`, `price`, `tip`, `budget_rate`, `reserve`, `cur_value`) and every token base-unit
//!   quantity (`amount`, `amount_left`, `filled_amount`, `initial_amount`, `min_fill`) is a decimal string so JavaScript
//!   clients cannot lose precision;
//! - prices and tips are sompi per WHOLE token: per `scale` base units of the order's token (`scale` = `10^decimals` of the
//!   token, at most `10^9`; a pair order's price is token B base units per whole A);
//! - scales, counts, DAA scores, block sequence numbers and indices are JSON numbers;
//! - every view derived from a chain position carries `confirmations` (node DAA minus the DAA of
//!   the chain block) and `settled` (confirmations >= the configured settle depth). Both are
//!   `null`/`false` while the node's DAA is unknown.

use crate::hex::{self, Hash32};
use crate::indexer::db::{DbError, DbResult};
use crate::tokens::TokenAllowlist;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::{armed_origin, AnyState};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Context for deriving `confirmations`, `settled` and `expired`.
#[derive(Debug, Clone, Copy)]
pub struct ReadCtx {
    pub node_daa: Option<u64>,
    pub settle_depth_daa: u64,
    /// Wall clock (UTC unix seconds), for day-order deadlines.
    pub now_unix: Option<u64>,
}

impl ReadCtx {
    pub fn confirmations(&self, daa: i64) -> Option<i64> {
        self.node_daa.map(|n| (n as i64).saturating_sub(daa).max(0))
    }

    pub fn settled(&self, daa: i64) -> bool {
        self.confirmations(daa).is_some_and(|c| c >= self.settle_depth_daa as i64)
    }
}

fn hx(b: Vec<u8>) -> String {
    hex::encode(&b)
}

fn hxo(b: Option<Vec<u8>>) -> Option<String> {
    b.map(|b| hex::encode(&b))
}

/// A 64-bit amount (sompi or token base units) as a decimal string.
fn sompi(v: Option<i64>) -> Option<String> {
    v.map(|v| v.to_string())
}

/// A page of results with an opaque continuation cursor.
#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, Serialize)]
pub struct TokenView {
    pub ticker: String,
    pub covenant_id: String,
    pub template_hash: Option<String>,
    pub extension_commitment: Option<String>,
    /// `kcc20` or `kron`, when the allowlist names the token's family.
    pub family: Option<&'static str>,
    pub decimals: Option<u32>,
    /// The standard scale of the token's orders (`10^decimals`, at most `10^9`): an order is listed only at this scale, so the
    /// listed orders of a token quote the same whole token. `null` without `decimals`.
    pub scale: Option<i64>,
    /// `official` (confirmed genuine), `unverified` (a token of a strict-list program KOB has not confirmed; tickers collide, identify
    /// it by covenant id plus template hash).
    pub standing: &'static str,
    /// The registry's own `official` flag as declared, and the registry document `standing` was derived from: `standing` is this
    /// operator's reading of that registry, not chain data. A client verifies against `registry.sha256` (or ignores it).
    pub official_declared: bool,
    pub registry: Option<crate::tokens::RegistryInfo>,
    /// The registry's `genesis_verified`: every genesis output of the token was checked against the pinned program. `null`: the
    /// registry does not say. `false` withdraws the `official` badge.
    pub genesis_verified: Option<bool>,
    /// Warnings a client must show: `genesis_unverified` (a hidden genesis output could mint look-alike tokens later) unless the
    /// registry says `genesis_verified: true`.
    pub warnings: Vec<&'static str>,
    /// Capabilities of the token's program that reach balances or supply (`freeze`, `seize`, `blacklist`, `mint-authority`, ...): a
    /// UI labels tokens whose program can freeze or seize.
    pub powers: Vec<String>,
    /// Registry id of the token's program, when known.
    pub template_id: Option<String>,
    /// Listed, open or partially filled asks (side 1).
    pub open_asks: i64,
    /// Listed, open or partially filled bids (side 2).
    pub open_bids: i64,
}

pub fn token_list(conn: &Connection, tokens: &TokenAllowlist) -> DbResult<Vec<TokenView>> {
    let mut counts: BTreeMap<(Vec<u8>, i64), i64> = BTreeMap::new();
    let mut st = conn.prepare(
        "SELECT o.token_cov_id, o.side, COUNT(*) FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id \
         WHERE o.listed = 1 AND o.in_book = 1 AND s.status IN ('open','partial') AND s.state_known = 1 AND o.token_cov_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM order_flags f WHERE f.covenant_id = o.covenant_id AND f.txid = s.cur_txid AND f.idx = s.cur_idx) \
         GROUP BY o.token_cov_id, o.side",
    )?;
    let rows = st.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?;
    for r in rows {
        let (t, side, n) = r?;
        counts.insert((t, side), n);
    }
    let mut out: Vec<TokenView> = tokens
        .list()
        .into_iter()
        .map(|t| {
            let id = t.covenant_id.0.to_vec();
            TokenView {
                ticker: t.ticker.clone(),
                covenant_id: t.covenant_id.to_hex(),
                template_hash: t.template_hash.map(|h| h.to_hex()),
                extension_commitment: t.extension_commitment.map(|h| h.to_hex()),
                family: t.family.map(|f| f.as_str()),
                decimals: t.decimals,
                scale: t.scale(),
                standing: tokens.standing_of(t),
                official_declared: t.official,
                registry: tokens.info().cloned(),
                genesis_verified: tokens.genesis_verified(&t.covenant_id),
                warnings: tokens.warnings_for(&t.covenant_id),
                powers: t.powers.clone(),
                template_id: t.template_id.clone(),
                open_asks: counts.get(&(id.clone(), 1)).copied().unwrap_or(0),
                open_bids: counts.get(&(id, 2)).copied().unwrap_or(0),
            }
        })
        .collect();
    // The open token list: tokens of a strict-list program that have listed orders but no registry entry (unverified, no ticker);
    // the quote token B of a listed pair order too (its program from the token identities its placement recorded).
    if tokens.is_open() {
        let mut st = conn.prepare(
            "SELECT DISTINCT token_cov_id, token_tpl_hash, family FROM orders WHERE listed = 1 AND token_cov_id IS NOT NULL \
             AND token_tpl_hash IS NOT NULL \
             UNION SELECT DISTINCT o.quote_cov_id, e.tpl_hash, 0 FROM orders o JOIN token_events e ON e.token_cov_id = o.quote_cov_id \
             WHERE o.listed = 1 AND o.quote_cov_id IS NOT NULL AND e.tpl_hash IS NOT NULL ORDER BY 1, 3 DESC",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, i64>(2)?)))?;
        let mut seen: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
        for r in rows {
            let (cov, tpl, fam) = r?;
            let (Ok(cov32), Ok(tpl32)) = (<[u8; 32]>::try_from(cov.as_slice()), <[u8; 32]>::try_from(tpl.as_slice())) else {
                continue;
            };
            let cid = crate::hex::Hash32(cov32);
            if tokens.get(&cid).is_some() || !seen.insert(cov.clone()) {
                continue;
            }
            let strict = tokens.strict_template(&crate::hex::Hash32(tpl32));
            out.push(TokenView {
                ticker: String::new(),
                covenant_id: cid.to_hex(),
                template_hash: Some(hex::encode(&tpl32)),
                extension_commitment: None,
                family: strict.map(|t| t.family.as_str()).or(if fam == 2 { Some("kron") } else { Some("kcc20") }),
                decimals: None,
                scale: None,
                standing: "unverified",
                official_declared: false,
                registry: tokens.info().cloned(),
                genesis_verified: None,
                warnings: tokens.warnings_for(&cid),
                powers: strict.map(|t| t.powers.clone()).unwrap_or_default(),
                template_id: strict.map(|t| t.id.clone()),
                open_asks: counts.get(&(cov.clone(), 1)).copied().unwrap_or(0),
                open_bids: counts.get(&(cov, 2)).copied().unwrap_or(0),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Books

#[derive(Debug, Clone, Serialize)]
pub struct LevelView {
    /// Sompi per whole token (`scale` base units).
    pub price: String,
    /// Base units at this level (the sum of the orders' remaining amounts; a bid's is its buying power).
    pub amount: String,
    /// True when any order at this level has no exact remaining amount (bids: their buying power is an upper bound).
    pub amount_estimated: bool,
    pub orders: i64,
    /// Base units per whole token of the orders at this level. Levels are per (`price`, `scale`): the listed orders of a token
    /// with `decimals` all have its standard scale; orders of another scale (a token without `decimals`) quote another whole
    /// token, so their `price` is not comparable as such. Levels are ordered by the price per base unit (`price / scale`).
    pub scale: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BookOrderView {
    pub covenant_id: String,
    pub contract: String,
    pub maker: Option<String>,
    /// The limit (start price of an auction).
    pub price: String,
    /// Quote at the node's current DAA score: equals `price` unless the order is an auction.
    pub quote: Option<String>,
    pub auction: Option<AuctionView>,
    /// Priority tip, sompi per whole token.
    pub tip: Option<String>,
    /// Smallest fill, base units (unless the fill takes everything left).
    pub min_fill: Option<String>,
    /// Base units per whole token (the price denominator).
    pub scale: Option<i64>,
    /// Base units left (a bid: its buying power, an upper bound).
    pub amount_left: Option<String>,
    pub amount_estimated: bool,
    pub status: String,
    pub cur_value: Option<String>,
    pub expiry_daa: Option<i64>,
    pub expired: bool,
    /// Day orders: UTC unix seconds after which conforming matchers do not fill.
    pub deadline: Option<i64>,
    pub deadline_passed: bool,
    pub genesis_daa: i64,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum BookSide {
    Levels(Vec<LevelView>),
    Orders(Vec<BookOrderView>),
}

#[derive(Debug, Clone, Serialize)]
pub struct BookView {
    pub token: String,
    pub aggregated: bool,
    pub node_daa: Option<u64>,
    /// Ascending by price per base unit (`price / scale`).
    pub asks: BookSide,
    /// Descending by price per base unit.
    pub bids: BookSide,
}

/// Ask-side orders are liquidity only while exactly one live custody UTXO holds `amountLeft` base units
/// (`docs/spec/matcher.md` 1.2); bid-side orders hold KAS and need no custody.
pub(crate) const CUSTODY_OK: &str = "(o.side = 2 OR (\
     (SELECT COUNT(*) FROM token_utxos t WHERE t.owner = o.covenant_id AND t.role = 'custody' AND t.spent_block IS NULL) = 1 \
     AND (SELECT t.amount FROM token_utxos t WHERE t.owner = o.covenant_id AND t.role = 'custody' AND t.spent_block IS NULL) \
         = s.remaining_amount))";

/// SQL order key of a book: the price per base unit (`price / scale`, a REAL: display order; the aggregated levels compare
/// exactly, [`book_side_levels`]).
const PER_BASE_UNIT: &str = "(o.price * 1.0 / MAX(COALESCE(o.scale, 1), 1))";

/// Orders flagged possibly frozen (the engine pre-simulation failed at the token program, `indexer::flags`) are not liquidity.
/// The same predicate is in every book, count and stats query (aliases `o` = orders, `s` = order_state).
macro_rules! not_frozen {
    () => {
        "NOT EXISTS (SELECT 1 FROM order_flags f WHERE f.covenant_id = o.covenant_id AND f.txid = s.cur_txid AND f.idx = s.cur_idx)"
    };
}

/// [`not_frozen!`] for other read modules.
pub(crate) const NOT_FROZEN: &str = not_frozen!();

const BOOK_WHERE: &str = concat!(
    "o.token_cov_id = ?1 AND o.side = ?2 AND o.listed = 1 AND o.in_book = 1 \
     AND s.status IN ('open','partial') AND s.state_known = 1 AND o.price IS NOT NULL AND ",
    not_frozen!()
);

fn book_side_orders(conn: &Connection, ctx: &ReadCtx, token: &Hash32, side: i64, depth: usize) -> DbResult<Vec<BookOrderView>> {
    let dir = if side == 1 { "ASC" } else { "DESC" };
    let sql = format!(
        "SELECT o.covenant_id, o.contract, o.maker, o.price, o.tip, o.min_fill, o.scale, o.expiry_daa, o.genesis_daa, \
                s.amount_exact, s.status, s.remaining_amount, s.cur_value, o.deadline, u.state, u.created_daa \
         FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id \
         LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
         WHERE {BOOK_WHERE} AND {CUSTODY_OK} \
         ORDER BY {PER_BASE_UNIT} {dir}, o.genesis_block ASC, o.covenant_id ASC LIMIT ?3"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![token.0.to_vec(), side, depth as i64], |r| {
        let expiry: Option<i64> = r.get(7)?;
        let genesis_daa: i64 = r.get(8)?;
        let exact: i64 = r.get(9)?;
        let status: String = r.get(10)?;
        let remaining: Option<i64> = r.get(11)?;
        let contract: String = r.get(1)?;
        let price: i64 = r.get(3)?;
        let deadline: Option<i64> = r.get(13)?;
        let state = tip_state(&contract, r.get(14)?);
        let utxo_daa: Option<i64> = r.get(15)?;
        let auction = state.as_ref().zip(utxo_daa).and_then(|(s, d)| auction_of(s, d, ctx.node_daa));
        Ok(BookOrderView {
            covenant_id: hx(r.get(0)?),
            contract,
            maker: hxo(r.get(2)?),
            price: price.to_string(),
            quote: Some(auction.as_ref().map(|a| a.current_price.clone()).unwrap_or_else(|| price.to_string())),
            auction,
            tip: sompi(r.get(4)?),
            min_fill: sompi(r.get(5)?),
            scale: r.get(6)?,
            amount_left: sompi(remaining),
            amount_estimated: exact == 0 || remaining.is_none(),
            expired: expiry.is_some_and(|e| ctx.node_daa.is_some_and(|n| n as i64 >= e)),
            expiry_daa: expiry,
            status,
            cur_value: sompi(r.get(12)?),
            deadline_passed: deadline_passed(deadline, ctx),
            deadline,
            genesis_daa,
            confirmations: ctx.confirmations(genesis_daa),
            settled: ctx.settled(genesis_daa),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Aggregated levels of one side: per (`price`, `scale`), the amounts summed in 128 bits (base units of hostile orders can sum
/// past an `i64`, where SQLite's `SUM` fails), ordered EXACTLY by the price per base unit (`price / scale`, compared by
/// cross-multiplication; asks ascending, bids descending; at one price per base unit by `price`, then `scale`, in the same
/// direction). Every live listed order of the side is read (the book of one token).
fn book_side_levels(conn: &Connection, token: &Hash32, side: i64, depth: usize) -> DbResult<Vec<LevelView>> {
    let sql = format!(
        "SELECT o.price, COALESCE(o.scale, 1), s.remaining_amount, s.amount_exact \
         FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id WHERE {BOOK_WHERE} AND {CUSTODY_OK}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![token.0.to_vec(), side], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, i64>(3)?))
    })?;
    // (price, scale) -> (amount, orders, estimated)
    let mut levels: BTreeMap<(i64, i64), (i128, i64, bool)> = BTreeMap::new();
    for r in rows {
        let (price, scale, remaining, exact) = r?;
        let l = levels.entry((price, scale.max(1))).or_insert((0, 0, false));
        l.0 += remaining.unwrap_or(0).max(0) as i128;
        l.1 += 1;
        l.2 |= remaining.is_none() || exact == 0;
    }
    let mut out: Vec<_> = levels.into_iter().collect();
    out.sort_by(|((pa, sa), _), ((pb, sb), _)| {
        let o = (*pa as i128 * *sb as i128).cmp(&(*pb as i128 * *sa as i128)).then(pa.cmp(pb)).then(sa.cmp(sb));
        if side == 1 {
            o
        } else {
            o.reverse()
        }
    });
    Ok(out
        .into_iter()
        .take(depth)
        .map(|((price, scale), (amount, orders, estimated))| LevelView {
            price: price.to_string(),
            amount: amount.to_string(),
            amount_estimated: estimated,
            orders,
            scale,
        })
        .collect())
}

fn deadline_passed(deadline: Option<i64>, ctx: &ReadCtx) -> bool {
    matches!((deadline, ctx.now_unix), (Some(d), Some(n)) if n as i64 >= d)
}

/// The decoded tip state of an order UTXO (None when unknown or undecodable).
pub fn tip_state(contract: &str, state: Option<Vec<u8>>) -> Option<AnyState> {
    let id = TemplateId::from_name(contract)?;
    AnyState::decode(id, &state?).ok()
}

/// An auction in progress: a decaying ask, a rising bid, an armed stop leg or an armed stop entry.
#[derive(Debug, Clone, Serialize)]
pub struct AuctionView {
    /// `decay` (ask), `rise` (bid), `stop` (armed stop leg), `entry` (armed stop entry); a pair order's auctions have the
    /// same kinds with prices in token B base units per whole A (`pair_decay`, `pair_rise`, `pair_stop`, `pair_entry`).
    pub kind: &'static str,
    /// DAA score the price path starts from.
    pub origin_daa: i64,
    pub start_price: String,
    /// The bound the price path ends at.
    pub end_price: String,
    /// The quote at the node's current DAA score (the start price until `origin_daa`).
    pub current_price: String,
    pub elapsed_daa: i64,
    /// The path has reached its end price.
    pub complete: bool,
}

/// Auction path of a tip state at the node's DAA score. All arithmetic is guarded: the values come
/// from chain content.
pub fn auction_of(state: &AnyState, utxo_daa: i64, node_daa: Option<u64>) -> Option<AuctionView> {
    let now = node_daa.map(|n| n as i64);
    crate::indexer::processor::guarded(|| {
        let view = |kind: &'static str, origin: i64, start: i64, end: i64, cur: &dyn Fn(i64) -> i64| {
            let t = now.map(|n| n.max(origin)).unwrap_or(origin);
            let current = cur(t);
            AuctionView {
                kind,
                origin_daa: origin,
                start_price: start.to_string(),
                end_price: end.to_string(),
                current_price: current.to_string(),
                elapsed_daa: t - origin,
                complete: current == end,
            }
        };
        match state {
            AnyState::KobAsk(a) | AnyState::KobAskKron(a) if a.slope > 0 && a.decay_step > 0 => {
                Some(view("decay", a.origin(utxo_daa)?, a.price, a.price_end, &|t| a.price_at(t, utxo_daa).unwrap_or(a.price_end)))
            }
            AnyState::KobBid(b) | AnyState::KobBidKron(b) if b.slope > 0 && b.decay_step > 0 => {
                Some(view("rise", b.origin(utxo_daa)?, b.price, b.price_end, &|t| b.price_at(t, utxo_daa).unwrap_or(b.price_end)))
            }
            AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) if c.armed != 0 && c.band_daa > 0 => {
                let o = armed_origin(c.armed, utxo_daa)?;
                Some(view("stop", o, c.stop_price, c.stop_floor(), &|t| c.stop_at(false, t, utxo_daa).unwrap_or(c.stop_floor())))
            }
            AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) if c.armed != 0 && c.band_daa > 0 => {
                let o = armed_origin(c.armed, utxo_daa)?;
                Some(view("stop", o, c.stop_price, c.stop_ceiling(), &|t| c.stop_at(false, t, utxo_daa).unwrap_or(c.stop_ceiling())))
            }
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) if b.entry_stop > 0 && b.armed != 0 && b.band_daa > 0 => {
                let o = armed_origin(b.armed, utxo_daa)?;
                Some(view("entry", o, b.entry_stop, b.price, &|t| b.price_at(false, t, utxo_daa).unwrap_or(b.price)))
            }
            AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) if a.entry_stop > 0 && a.armed != 0 && a.band_daa > 0 => {
                let o = armed_origin(a.armed, utxo_daa)?;
                Some(view("entry", o, a.entry_stop, a.price, &|t| a.price_at(false, t, utxo_daa).unwrap_or(a.price)))
            }
            // the pair kinds: prices in token B base units per whole A
            AnyState::KobPair(p) if p.slope > 0 && p.decay_step > 0 => {
                let kind = if p.is_ask() { "pair_decay" } else { "pair_rise" };
                Some(view(kind, p.origin(utxo_daa)?, p.price, p.price_end, &|t| p.price_at(t, utxo_daa).unwrap_or(p.price_end)))
            }
            AnyState::KobCondPair(c) if c.armed != 0 && c.band_daa > 0 => {
                let o = armed_origin(c.armed, utxo_daa)?;
                Some(view("pair_stop", o, c.stop_price, c.stop_worst(), &|t| c.stop_at(false, t, utxo_daa).unwrap_or(c.stop_worst())))
            }
            AnyState::KobIfdPair(e) if e.entry_stop > 0 && e.armed != 0 && e.band_daa > 0 => {
                let o = armed_origin(e.armed, utxo_daa)?;
                Some(view("pair_entry", o, e.entry_stop, e.price, &|t| e.price_at(false, t, utxo_daa).unwrap_or(e.price)))
            }
            _ => None,
        }
    })
    .flatten()
}

/// The listed book of one token: asks ascending, bids descending (ties by age, then covenant id).
pub fn book(conn: &Connection, ctx: &ReadCtx, token: &Hash32, depth: usize, aggregate: bool) -> DbResult<BookView> {
    let (asks, bids) = if aggregate {
        (BookSide::Levels(book_side_levels(conn, token, 1, depth)?), BookSide::Levels(book_side_levels(conn, token, 2, depth)?))
    } else {
        (
            BookSide::Orders(book_side_orders(conn, ctx, token, 1, depth)?),
            BookSide::Orders(book_side_orders(conn, ctx, token, 2, depth)?),
        )
    };
    Ok(BookView { token: token.to_hex(), aggregated: aggregate, node_daa: ctx.node_daa, asks, bids })
}

// ---------------------------------------------------------------------------------------------
// Orders

#[derive(Debug, Clone, Serialize)]
pub struct GenesisView {
    pub txid: Option<String>,
    pub out: Option<i64>,
    pub block_seq: i64,
    pub daa: i64,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutpointView {
    pub txid: String,
    pub index: i64,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderView {
    pub covenant_id: String,
    pub contract: String,
    pub template_hash: String,
    pub family: i64,
    /// 1: sells the token, 2: buys the token.
    pub side: i64,
    pub maker: Option<String>,
    pub token: Option<String>,
    pub token_template_hash: Option<String>,
    /// KCC-20 extension commitment of the token (bid kinds: the state; ask kinds: the placement custody).
    pub extension_commitment: Option<String>,
    /// Base units per whole token (the price denominator; a pair order: of its base token A).
    pub scale: Option<i64>,
    /// Smallest fill, base units (unless the fill takes everything left; a bid: unless it ends the bid).
    pub min_fill: Option<String>,
    /// The limit, sompi per whole token (`null` for the conditional kinds and the pair orders: `pair.price`).
    pub price: Option<String>,
    /// Priority tip, sompi per whole token (a pair order: KAS per whole A, released to the filler).
    pub tip: Option<String>,
    pub tif: Option<i64>,
    pub expiry_daa: Option<i64>,
    pub active_from: Option<i64>,
    pub in_book: bool,
    /// Bids: the rate a fill consumes the escrow at, `pMax + tip` sompi per whole token (buy-first entries: `price + tip`).
    pub budget_rate: Option<String>,
    pub reserve: Option<String>,
    /// Base units at creation (`amountLeft`; `null` for a bid, whose quantity is its escrow).
    pub initial_amount: Option<String>,
    pub listed: bool,
    pub unlisted_reason: Option<String>,
    pub origin: String,
    pub parent: Option<String>,
    pub genesis: GenesisView,
    pub status: String,
    /// Base units filled so far.
    pub filled_amount: String,
    /// Base units left: `amountLeft` of the current state; a bid's buying power (the most its escrow buys in one fill).
    pub amount_left: Option<String>,
    pub amount_estimated: bool,
    pub current: Option<OutpointView>,
    pub state_known: bool,
    /// The decoded state of the current UTXO (`{"kind", "state"}`, 64-bit fields as strings), when proven.
    pub state: Option<Value>,
    /// DAA score of the block that created the current UTXO.
    pub current_daa: Option<i64>,
    /// Day orders: UTC unix seconds after which conforming matchers do not fill (placement record).
    pub deadline: Option<i64>,
    pub deadline_passed: bool,
    /// DAA score from which anyone may refund (soft expiry, 90 days idle, IOC / FOK kill time).
    pub refund_due_daa: Option<i64>,
    /// IOC / FOK: the kill time (`max(UTXO DAA, activeFrom) + 600`, or the expiry if earlier).
    pub kill_daa: Option<i64>,
    /// Auction in progress (decay, rise, armed stop leg or stop entry): start and current price.
    pub auction: Option<AuctionView>,
    /// Quote now: the auction price, else the limit.
    pub quote: Option<String>,
    /// Repeat IFD / IFO: the entry role (`rpt_amount`, the amount left to re-arm) or the booked exit role.
    pub repeat: Option<RepeatView>,
    /// Exact custody (ask kinds): the token UTXO holding `amountLeft` base units. Single-order lookups, and the live orders of a
    /// `maker=` list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custody: Option<CustodyView>,
    /// Stray token UTXOs owned by the order id (never liquidity). Single-order lookups, and the live orders of a `maker=`
    /// list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strays: Option<Vec<TokenUtxoView>>,
    pub last_block: i64,
    pub last_daa: i64,
    pub confirmations: Option<i64>,
    pub settled: bool,
    /// Open or partially filled past `expiry_daa`: a refund is pending.
    pub expired: bool,
    /// The engine pre-simulation of this order's next fill was rejected by the token program (a frozen or blacklisted balance, or a
    /// program that changed its rules): the order is not listed as live liquidity. Cancel or refund still work if the program lets
    /// the covenant move the tokens. Clears when a later pre-simulation passes or the order moves.
    pub possibly_frozen: bool,
    /// What `possibly_frozen` rests on: this operator's own engine pre-simulation (a judgement of the operator's matcher, not chain
    /// data; another operator may see none). `null` when the order is not flagged.
    pub possibly_frozen_basis: Option<&'static str>,
    /// Why the order is flagged (`possibly_frozen`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frozen_reason: Option<String>,
    /// Covenant ids of orders created by this order (IFD exits). Only set on single-order lookups.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<String>>,
    /// Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`): the pair, the side, the price in token B and the custodies.
    /// `token` is the base token A, `price` / `quote` are null (no KAS price), `tip` is the KAS tip per whole A.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair: Option<PairView>,
}

/// One custody a pair order holds (`AnyState::custodies`).
#[derive(Debug, Clone, Serialize)]
pub struct PairCustodyView {
    /// The custody's token (A or B).
    pub token: String,
    /// `base` (A) or `quote` (B).
    pub role: &'static str,
    /// The exact amount the current state requires (base units of that token).
    pub expected_amount: String,
    /// The live custody UTXO of that token (`role = custody`), when there is exactly one (single-order lookups and the live
    /// orders of a `maker=` list).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utxo: Option<TokenUtxoView>,
    /// Exactly one live custody UTXO of that token holds the expected amount (set with `utxo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// A pair order's pair, side, price and custodies (token base units and prices as decimal strings).
#[derive(Debug, Clone, Serialize)]
pub struct PairView {
    /// The base token A (the order's `token`) and the quote token B.
    pub base: String,
    pub quote: String,
    pub base_family: Option<&'static str>,
    pub quote_family: Option<&'static str>,
    pub base_template_hash: String,
    pub quote_template_hash: String,
    /// Base units per whole A (the price's denominator) and per whole B.
    pub base_scale: i64,
    pub quote_scale: i64,
    /// `ask` (sells A for B; a sell-first entry) or `bid` (buys A with B; a buy-first entry).
    pub side: &'static str,
    /// The limit in B base units per whole A (`KobPair`: its price, the start of a decay; `KobIfdPair`: the entry's limit;
    /// `KobCondPair`: its take-profit / limit leg, `null` without one).
    pub price: Option<String>,
    /// `price` as a reduced fraction per A base unit (`price / base_scale`).
    pub price_num: Option<String>,
    pub price_den: Option<String>,
    /// `KobCondPair`: the stop (B per whole A, `null` without a stop leg); `KobIfdPair`: the entry stop (`null` for a limit
    /// entry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_price: Option<String>,
    /// The quote now (an auction's current price, else `price`), B per whole A.
    pub quote_now: Option<String>,
    /// Base units of A still to trade (current state; null once closed).
    pub amount_left: Option<String>,
    /// The custodies the current state holds, in record order (a sell-first entry: A, then its B prefund).
    pub custodies: Vec<PairCustodyView>,
    /// `KobIfdPair` sell-first: the B prefund per whole A.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefund: Option<String>,
    /// KAS on each token delivery.
    pub delivery_carrier: String,
}

fn family_name(code: i64) -> Option<&'static str> {
    kob_protocol::state::family_of_code(code).map(|f| if f == kob_protocol::family::Family::Kron { "kron" } else { "kcc20" })
}

/// The pair view of a pair order state (`current`: the tip state is known, so its custodies and amount are current).
pub fn pair_view(s: &AnyState, current: bool, quote_now: Option<String>) -> Option<PairView> {
    let t = s.pair_tokens()?;
    let (side, price, stop, prefund, carrier) = match s {
        AnyState::KobPair(p) => (p.is_ask(), Some(p.price), None, None, p.delivery_carrier),
        AnyState::KobCondPair(c) => (
            c.is_ask(),
            (c.tp_price > 0).then_some(c.tp_price),
            Some((c.stop_price > 0).then_some(c.stop_price)),
            None,
            c.delivery_carrier,
        ),
        AnyState::KobIfdPair(e) => (
            !e.is_buy_first(),
            Some(e.price),
            Some((e.entry_stop > 0).then_some(e.entry_stop)),
            (!e.is_buy_first()).then_some(e.prefund),
            e.delivery_carrier,
        ),
        _ => return None,
    };
    let frac = price.and_then(|p| reduce(p as i128, t.a.scale as i128));
    let custodies = if current {
        s.custodies()
            .into_iter()
            .map(|(tok, amount)| PairCustodyView {
                token: hex::encode(&tok),
                role: if tok == t.a.cov_id { "base" } else { "quote" },
                expected_amount: amount.to_string(),
                utxo: None,
                ok: None,
            })
            .collect()
    } else {
        vec![]
    };
    Some(PairView {
        base: hex::encode(&t.a.cov_id),
        quote: hex::encode(&t.b.cov_id),
        base_family: family_name(t.a.family),
        quote_family: family_name(t.b.family),
        base_template_hash: hex::encode(&t.a.tpl_hash),
        quote_template_hash: hex::encode(&t.b.tpl_hash),
        base_scale: t.a.scale,
        quote_scale: t.b.scale,
        side: if side { "ask" } else { "bid" },
        price: price.map(|p| p.to_string()),
        price_num: frac.map(|f| f.0.to_string()),
        price_den: frac.map(|f| f.1.to_string()),
        stop_price: stop.flatten().map(|v| v.to_string()),
        quote_now: quote_now.or_else(|| price.map(|p| p.to_string())),
        amount_left: if current { s.amount_left().map(|a| a.to_string()) } else { None },
        custodies,
        prefund: prefund.map(|p| p.to_string()),
        delivery_carrier: carrier.to_string(),
    })
}

fn gcd_u128(a: u128, b: u128) -> u128 {
    if b == 0 {
        a
    } else {
        gcd_u128(b, a % b)
    }
}

/// `(num / g, den / g)` of two positive values (`None` when either is not positive).
pub fn reduce(num: i128, den: i128) -> Option<(u128, u128)> {
    if num <= 0 || den <= 0 {
        return None;
    }
    let (n, d) = (num as u128, den as u128);
    let g = gcd_u128(n, d).max(1);
    Some((n / g, d / g))
}

const ORDER_SELECT: &str = "SELECT o.covenant_id, o.contract, o.template_hash, o.family, o.side, o.maker, o.token_cov_id, \
    o.token_tpl_hash, o.scale, o.min_fill, o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, \
    o.reserve, o.initial_amount, o.genesis_txid, o.genesis_out, o.genesis_block, o.genesis_daa, o.parent, o.listed, \
    o.unlisted_reason, o.origin, s.status, s.filled_amount, s.remaining_amount, s.cur_txid, s.cur_idx, s.cur_value, \
    s.state_known, s.last_block, s.last_daa, o.ext_commit, o.deadline, s.amount_exact, u.state, u.created_daa, fl.reason, \
    o.genesis_state \
    FROM orders o LEFT JOIN order_state s ON s.covenant_id = o.covenant_id \
    LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
    LEFT JOIN order_flags fl ON fl.covenant_id = o.covenant_id AND fl.txid = s.cur_txid AND fl.idx = s.cur_idx";

fn order_from_row(r: &Row<'_>, ctx: &ReadCtx) -> rusqlite::Result<OrderView> {
    let genesis_daa: i64 = r.get(22)?;
    let status: Option<String> = r.get(27)?;
    let status = status.unwrap_or_else(|| "unknown".to_string());
    let contract: String = r.get(1)?;
    let remaining: Option<i64> = r.get(29)?;
    let amount_exact: i64 = r.get::<_, Option<i64>>(38)?.unwrap_or(0);
    let last_daa: Option<i64> = r.get(35)?;
    let last_daa = last_daa.unwrap_or(genesis_daa);
    let expiry: Option<i64> = r.get(13)?;
    let active = status == "open" || status == "partial";
    let cur_txid: Option<Vec<u8>> = r.get(30)?;
    let cur_idx: Option<i64> = r.get(31)?;
    let cur_value: Option<i64> = r.get(32)?;
    let current = match (cur_txid, cur_idx) {
        (Some(t), Some(i)) => Some(OutpointView { txid: hx(t), index: i, value: sompi(cur_value) }),
        _ => None,
    };
    let deadline: Option<i64> = r.get(37)?;
    let state = tip_state(&contract, r.get(39)?);
    let current_daa: Option<i64> = r.get(40)?;
    let frozen_reason: Option<String> = r.get(41)?;
    let tif: Option<i64> = r.get(12)?;
    // pair orders: the pair, the side and the price come from the state (the current one, else the genesis state of a closed
    // order, whose custodies and amount are then not shown)
    let pair_state = TemplateId::from_name(&contract).filter(|t| t.is_pair()).and_then(|t| {
        let genesis: Option<Vec<u8>> = r.get(42).ok().flatten();
        state.clone().map(|s| (s, true)).or_else(|| Some((AnyState::decode(t, &genesis?).ok()?, false)))
    });
    let (mut refund_due, mut kill) = (None, None);
    let (mut auction, mut quote, mut repeat) = (None, None, None);
    if let (Some(s), Some(d)) = (&state, current_daa) {
        refund_due = crate::indexer::processor::guarded(|| s.refund_due(d)).flatten();
        if matches!(tif, Some(1) | Some(2)) {
            kill = refund_due;
        }
        auction = auction_of(s, d, ctx.node_daa);
        quote = auction.as_ref().map(|a| a.current_price.clone());
        repeat = repeat_of(s);
    }
    // a pair order has no KAS quote: its quote now (B per whole A) is `pair.quote_now`
    let pair = pair_state.and_then(|(s, cur)| pair_view(&s, cur, quote.take()));
    if quote.is_none() && pair.is_none() {
        quote = sompi(r.get(10)?);
    }
    Ok(OrderView {
        covenant_id: hx(r.get(0)?),
        contract,
        template_hash: hx(r.get(2)?),
        family: r.get(3)?,
        side: r.get(4)?,
        maker: hxo(r.get(5)?),
        token: hxo(r.get(6)?),
        token_template_hash: hxo(r.get(7)?),
        extension_commitment: hxo(r.get(36)?),
        scale: r.get(8)?,
        min_fill: sompi(r.get(9)?),
        price: sompi(r.get(10)?),
        tip: sompi(r.get(11)?),
        tif,
        expiry_daa: expiry,
        active_from: r.get(14)?,
        in_book: r.get::<_, i64>(15)? != 0,
        budget_rate: sompi(r.get(16)?),
        reserve: sompi(r.get(17)?),
        initial_amount: sompi(r.get(18)?),
        genesis: GenesisView {
            txid: hxo(r.get(19)?),
            out: r.get(20)?,
            block_seq: r.get(21)?,
            daa: genesis_daa,
            confirmations: ctx.confirmations(genesis_daa),
            settled: ctx.settled(genesis_daa),
        },
        parent: hxo(r.get(23)?),
        listed: r.get::<_, i64>(24)? != 0,
        unlisted_reason: r.get(25)?,
        origin: r.get(26)?,
        expired: active && expiry.is_some_and(|e| ctx.node_daa.is_some_and(|n| n as i64 >= e)),
        possibly_frozen: frozen_reason.is_some() && active,
        possibly_frozen_basis: (frozen_reason.is_some() && active).then_some("operator-engine-pre-simulation"),
        frozen_reason: frozen_reason.filter(|_| active),
        status,
        filled_amount: r.get::<_, Option<i64>>(28)?.unwrap_or(0).to_string(),
        amount_left: sompi(remaining),
        amount_estimated: amount_exact == 0 || remaining.is_none(),
        current,
        state_known: r.get::<_, Option<i64>>(33)?.unwrap_or(0) != 0,
        state: state.as_ref().and_then(|s| serde_json::to_value(s).ok()),
        current_daa,
        deadline_passed: active && deadline_passed(deadline, ctx),
        deadline,
        refund_due_daa: refund_due,
        kill_daa: kill,
        auction,
        quote,
        repeat,
        custody: None,
        strays: None,
        last_block: r.get::<_, Option<i64>>(34)?.unwrap_or(0),
        confirmations: ctx.confirmations(last_daa),
        settled: ctx.settled(last_daa),
        last_daa,
        children: None,
        pair,
    })
}

/// Repeat IFD / IFO position data of a tip state: an entry (`rptAmount > 0`) or a booked exit.
#[derive(Debug, Clone, Serialize)]
pub struct RepeatView {
    /// `entry` or `exit`.
    pub role: &'static str,
    /// Entry: `rptAmount` (0 = not repeating; else 1 + the base units it may still re-arm).
    pub rpt_amount: Option<String>,
    /// Entry: base units it may still re-arm (`rptAmount - 1`, never below 0).
    pub rearm_amount: Option<String>,
    /// Exit: the entry it re-arms.
    pub parent: Option<String>,
    /// Exit: from this DAA score a take-profit may skip the entry's merge.
    pub rpt_until: Option<String>,
    /// Exit: sompi per whole token returned to the entry, rounded up per merge (buy-first: the entry's budget rate
    /// `price + tip`; sell-first: its proceeds rate `price - tip`).
    pub rpt_price: Option<String>,
}

fn repeat_of(s: &AnyState) -> Option<RepeatView> {
    let entry = |rpt: i64| RepeatView {
        role: "entry",
        rpt_amount: Some(rpt.to_string()),
        rearm_amount: Some(rpt.saturating_sub(1).max(0).to_string()),
        parent: None,
        rpt_until: None,
        rpt_price: None,
    };
    let exit = |parent: &[u8; 32], until: i64, price: i64| RepeatView {
        role: "exit",
        rpt_amount: None,
        rearm_amount: None,
        parent: Some(hex::encode(parent)),
        rpt_until: Some(until.to_string()),
        rpt_price: Some(price.to_string()),
    };
    match s {
        AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) if b.rpt_amount > 0 => Some(entry(b.rpt_amount)),
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) if a.rpt_amount > 0 => Some(entry(a.rpt_amount)),
        AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) if c.parent != [0; 32] => {
            Some(exit(&c.parent, c.rpt_until, c.rpt_price))
        }
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) if c.parent != [0; 32] => {
            Some(exit(&c.parent, c.rpt_until, c.rpt_price))
        }
        _ => None,
    }
}

/// A token UTXO owned by an order covenant id.
#[derive(Debug, Clone, Serialize)]
pub struct TokenUtxoView {
    pub txid: String,
    pub index: i64,
    pub token: String,
    pub owner: String,
    /// Token base units.
    pub amount: String,
    /// KAS carrier.
    pub value: String,
    /// `custody` or `stray`.
    pub role: String,
    pub created_daa: i64,
    pub spent: bool,
    pub spent_txid: Option<String>,
    pub confirmations: Option<i64>,
    pub settled: bool,
    /// The token state the indexer proved against the output's script public key, as JSON (the shape kob-wasm's builders take:
    /// what a client needs to spend the UTXO in a cancel, cancel-replace or refund). Absent when the indexer holds no proven state
    /// (a token it does not track); a client then rebuilds a custody state from owner + amount + the order's extension commitment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// The token program `state` was proven under (`KCC20Ref_8x8`, `KronToken2433`, ...): what a client needs to move a FOREIGN
    /// stray (its own token's program is in the order's state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// A token other than the order's own (a pair order: neither of its two): a FOREIGN stray (`matcher.md` 1.2). Its program
    /// authorises it with any spend of the order, so whoever builds that spend can move it; the maker's cancel or sweep, and
    /// keepers that return foreign strays to the maker with a refund, do.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub foreign: bool,
}

/// The columns [`token_utxo_from_row`] reads (`t` = `token_utxos`, `h` = the output's proven `token_holdings` row).
const TOKEN_UTXO_COLUMNS: &str =
    "t.txid, t.idx, t.token_cov_id, t.owner, t.amount, t.value, t.role, t.created_daa, t.spent_block, t.spent_txid, h.program, h.state, \
     (fo.token_cov_id IS NOT NULL AND t.token_cov_id != fo.token_cov_id AND (fo.quote_cov_id IS NULL OR t.token_cov_id != fo.quote_cov_id))";
const TOKEN_UTXO_FROM: &str =
    "FROM token_utxos t LEFT JOIN token_holdings h ON h.txid = t.txid AND h.idx = t.idx LEFT JOIN orders fo ON fo.covenant_id = t.owner";

/// A proven token state span as JSON, decoded with the program the indexer proved it for.
fn proven_state_json(program: Option<String>, state: Option<Vec<u8>>) -> Option<Value> {
    let tpl = TemplateId::from_name(&program?).filter(|t| t.is_token()).map(kob_protocol::artifacts::token_template)?;
    let decoded = kob_protocol::state::TokenState::decode_with(tpl, &state?).ok()?;
    serde_json::to_value(&decoded).ok()
}

fn token_utxo_from_row(r: &Row<'_>, ctx: &ReadCtx) -> rusqlite::Result<TokenUtxoView> {
    let daa: i64 = r.get(7)?;
    let spent_block: Option<i64> = r.get(8)?;
    Ok(TokenUtxoView {
        txid: hx(r.get(0)?),
        index: r.get(1)?,
        token: hx(r.get(2)?),
        owner: hx(r.get(3)?),
        amount: r.get::<_, i64>(4)?.to_string(),
        value: r.get::<_, i64>(5)?.to_string(),
        role: r.get(6)?,
        created_daa: daa,
        spent: spent_block.is_some(),
        spent_txid: hxo(r.get(9)?),
        confirmations: ctx.confirmations(daa),
        settled: ctx.settled(daa),
        state: proven_state_json(r.get(10)?, r.get(11)?),
        program: r.get::<_, Option<String>>(10)?.filter(|p| TemplateId::from_name(p).is_some_and(|t| t.is_token())),
        foreign: r.get::<_, Option<bool>>(12)?.unwrap_or(false),
    })
}

/// The exact-custody check of an ask-side order (`docs/spec/matcher.md` 1.2).
#[derive(Debug, Clone, Serialize)]
pub struct CustodyView {
    /// `amountLeft` of the current state (base units), when known.
    pub expected_amount: Option<String>,
    /// The live custody UTXO (`role = custody`), when there is exactly one.
    pub utxo: Option<TokenUtxoView>,
    /// Exactly one live custody UTXO holds the expected amount (or the order needs none).
    pub ok: bool,
}

fn live_token_utxos(conn: &Connection, ctx: &ReadCtx, owner: &Hash32, role: &str) -> DbResult<Vec<TokenUtxoView>> {
    let sql = format!(
        "SELECT {TOKEN_UTXO_COLUMNS} {TOKEN_UTXO_FROM} WHERE t.owner = ?1 AND t.role = ?2 AND t.spent_block IS NULL \
         ORDER BY t.created_block, t.txid, t.idx"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![owner.0.to_vec(), role], |r| token_utxo_from_row(r, ctx))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One order by covenant id, with the covenant ids of any exit orders it created, its custody and strays.
pub fn order(conn: &Connection, ctx: &ReadCtx, covenant_id: &Hash32) -> DbResult<Option<OrderView>> {
    let sql = format!("{ORDER_SELECT} WHERE o.covenant_id = ?1");
    let v = conn.query_row(&sql, [covenant_id.0.to_vec()], |r| order_from_row(r, ctx)).optional()?;
    let Some(mut v) = v else { return Ok(None) };
    let mut st = conn.prepare("SELECT covenant_id FROM orders WHERE parent = ?1 ORDER BY genesis_block, covenant_id")?;
    let kids = st.query_map([covenant_id.0.to_vec()], |r| Ok(hx(r.get(0)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    v.children = Some(kids);
    attach_holdings(conn, ctx, covenant_id, &mut v)?;
    Ok(Some(v))
}

/// The order's exact custody (ask kinds) and its live strays, as the single-order view carries them.
fn attach_holdings(conn: &Connection, ctx: &ReadCtx, covenant_id: &Hash32, v: &mut OrderView) -> DbResult<()> {
    if let Some(p) = v.pair.as_mut() {
        // a pair order: each custody of its current state (one per token), checked against the live custody rows of that token
        let live = live_token_utxos(conn, ctx, covenant_id, "custody")?;
        let mut all_ok = live.len() == p.custodies.len();
        for c in p.custodies.iter_mut() {
            let mut mine: Vec<TokenUtxoView> = live.iter().filter(|u| u.token == c.token).cloned().collect();
            let ok = mine.len() == 1 && mine[0].amount == c.expected_amount;
            all_ok &= ok;
            c.ok = Some(ok);
            c.utxo = if mine.len() == 1 { mine.pop() } else { None };
        }
        if v.state.is_some() {
            let first = p.custodies.first();
            v.custody = Some(CustodyView {
                expected_amount: first.map(|c| c.expected_amount.clone()),
                utxo: first.and_then(|c| c.utxo.clone()),
                ok: all_ok,
            });
        }
    } else if v.side == 1 {
        let expected = v
            .state
            .as_ref()
            .and_then(|s| serde_json::from_value::<AnyState>(s.clone()).ok())
            .and_then(|s| crate::indexer::processor::guarded(|| s.custody_amount()).flatten());
        let mut live = live_token_utxos(conn, ctx, covenant_id, "custody")?;
        let ok = match expected {
            Some(0) => live.is_empty(),
            Some(e) => live.len() == 1 && live[0].amount == e.to_string(),
            None => false,
        };
        v.custody = Some(CustodyView {
            expected_amount: expected.map(|e| e.to_string()),
            utxo: if live.len() == 1 { live.pop() } else { None },
            ok,
        });
    }
    v.strays = Some(live_token_utxos(conn, ctx, covenant_id, "stray")?);
    Ok(())
}

/// A live stray with the order it is stuck on.
#[derive(Debug, Clone, Serialize)]
pub struct StrayView {
    #[serde(flatten)]
    pub utxo: TokenUtxoView,
    pub order_status: Option<String>,
    pub maker: Option<String>,
    /// The order has terminated: the maker's cancel can no longer sweep this stray.
    pub lost: bool,
}

/// Stray token UTXOs (sent to an order id outside the protocol), newest first.
pub fn strays(conn: &Connection, ctx: &ReadCtx, maker: Option<&[u8]>, limit: usize) -> DbResult<Vec<StrayView>> {
    let sql = format!(
        "SELECT {TOKEN_UTXO_COLUMNS}, s.status, o.maker \
         {TOKEN_UTXO_FROM} LEFT JOIN orders o ON o.covenant_id = t.owner LEFT JOIN order_state s ON s.covenant_id = t.owner \
         WHERE t.role = 'stray' AND t.spent_block IS NULL AND (?1 IS NULL OR o.maker = ?1) \
         ORDER BY t.created_block DESC, t.txid, t.idx LIMIT ?2"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![maker, limit as i64], |r| {
        let status: Option<String> = r.get(13)?;
        let lost = !matches!(status.as_deref(), Some("open") | Some("partial"));
        Ok(StrayView { utxo: token_utxo_from_row(r, ctx)?, order_status: status, maker: hxo(r.get(14)?), lost })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Token registry events (first sightings of a token identity), oldest first.
#[derive(Debug, Clone, Serialize)]
pub struct TokenEventView {
    pub token: String,
    pub template_hash: Option<String>,
    pub extension_commitment: Option<String>,
    pub kind: String,
    pub txid: String,
    pub daa: i64,
}

pub fn token_events(conn: &Connection, token: Option<&Hash32>, limit: usize) -> DbResult<Vec<TokenEventView>> {
    let mut st = conn.prepare(
        "SELECT token_cov_id, tpl_hash, ext_commit, kind, txid, daa FROM token_events \
         WHERE (?1 IS NULL OR token_cov_id = ?1) ORDER BY id ASC LIMIT ?2",
    )?;
    let rows = st.query_map(rusqlite::params![token.map(|t| t.0.to_vec()), limit as i64], |r| {
        Ok(TokenEventView {
            token: hx(r.get(0)?),
            template_hash: hxo(r.get(1)?),
            extension_commitment: hxo(r.get(2)?),
            kind: r.get(3)?,
            txid: hx(r.get(4)?),
            daa: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Default)]
pub struct OrderFilter {
    pub maker: Option<Vec<u8>>,
    pub token: Option<Hash32>,
    /// open | partial | filled | cancelled | refunded | closed | active (open or partial).
    pub status: Option<String>,
}

pub const ORDER_STATUSES: [&str; 8] = ["open", "partial", "filled", "cancelled", "refunded", "killed", "closed", "active"];

/// Cursor of the orders list: `<genesis_block>:<covenant id hex>`.
pub fn parse_order_cursor(s: &str) -> Option<(i64, Vec<u8>)> {
    let (b, c) = s.split_once(':')?;
    Some((b.parse().ok()?, hex::decode(c).ok()?))
}

/// Orders newest first, filtered; keyset-paginated on (genesis block, covenant id).
pub fn orders(
    conn: &Connection,
    ctx: &ReadCtx,
    f: &OrderFilter,
    limit: usize,
    cursor: Option<(i64, Vec<u8>)>,
) -> DbResult<Page<OrderView>> {
    let mut conds: Vec<String> = Vec::new();
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(m) = &f.maker {
        args.push(SqlValue::Blob(m.clone()));
        conds.push(format!("o.maker = ?{}", args.len()));
    }
    if let Some(t) = &f.token {
        // a pair order trades two tokens: it is listed under either (base A, quote B)
        args.push(SqlValue::Blob(t.0.to_vec()));
        conds.push(format!("(o.token_cov_id = ?{n} OR o.quote_cov_id = ?{n})", n = args.len()));
    }
    if let Some(s) = &f.status {
        if s == "active" {
            conds.push("s.status IN ('open','partial')".to_string());
        } else {
            args.push(SqlValue::Text(s.clone()));
            conds.push(format!("s.status = ?{}", args.len()));
        }
    }
    if let Some((b, c)) = cursor {
        args.push(SqlValue::Integer(b));
        args.push(SqlValue::Blob(c));
        conds.push(format!("(o.genesis_block, o.covenant_id) < (?{}, ?{})", args.len() - 1, args.len()));
    }
    let where_ = if conds.is_empty() { String::new() } else { format!(" WHERE {}", conds.join(" AND ")) };
    args.push(SqlValue::Integer(limit as i64 + 1));
    let sql = format!("{ORDER_SELECT}{where_} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT ?{}", args.len());
    let mut st = conn.prepare(&sql)?;
    let mut items = st.query_map(params_from_iter(args.iter()), |r| order_from_row(r, ctx))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|o| format!("{}:{}", o.genesis.block_seq, o.covenant_id))
    } else {
        None
    };
    // a maker's own list (the wallet's My orders) carries the custody and strays of its LIVE orders, as the single-order view
    // does: a cancel sweeps them and the wallet shows them per order (one indexed lookup per live order of one maker)
    if f.maker.is_some() {
        for v in items.iter_mut().filter(|v| v.status == "open" || v.status == "partial") {
            if let Ok(id) = Hash32::parse(&v.covenant_id) {
                attach_holdings(conn, ctx, &id, v)?;
            }
        }
    }
    Ok(Page { items, next_cursor })
}

// ---------------------------------------------------------------------------------------------
// Events and fills

#[derive(Debug, Clone, Serialize)]
pub struct EventView {
    pub id: i64,
    pub covenant_id: String,
    pub block_seq: i64,
    pub daa: i64,
    pub ts: i64,
    pub txid: String,
    pub tx_pos: i64,
    pub kind: String,
    pub token: Option<String>,
    pub side: Option<i64>,
    /// Base units: filled (`fill`), `amountLeft` at creation (`create`), returned (`kill`), left (`refund`, `amend`,
    /// `sweep`), merged back (`rearm`).
    pub amount: Option<String>,
    /// Sompi per whole token: the fill's quote (an auction: at the fill's time argument), the order's limit otherwise.
    pub price: Option<String>,
    pub payout: Option<String>,
    pub closes: bool,
    pub detail: Option<Value>,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

const EVENT_SELECT: &str =
    "SELECT id, covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail FROM order_events";

fn event_from_row(r: &Row<'_>, ctx: &ReadCtx) -> rusqlite::Result<EventView> {
    let daa: i64 = r.get(3)?;
    let detail: Option<String> = r.get(14)?;
    Ok(EventView {
        id: r.get(0)?,
        covenant_id: hx(r.get(1)?),
        block_seq: r.get(2)?,
        daa,
        ts: r.get(4)?,
        txid: hx(r.get(5)?),
        tx_pos: r.get(6)?,
        kind: r.get(7)?,
        token: hxo(r.get(8)?),
        side: r.get(9)?,
        amount: sompi(r.get(10)?),
        price: sompi(r.get(11)?),
        payout: sompi(r.get(12)?),
        closes: r.get::<_, i64>(13)? != 0,
        detail: detail.map(|d| serde_json::from_str(&d).unwrap_or(Value::String(d))),
        confirmations: ctx.confirmations(daa),
        settled: ctx.settled(daa),
    })
}

/// Lifecycle events of one order, oldest first. `after` is the id of the last event already seen.
pub fn order_events(
    conn: &Connection,
    ctx: &ReadCtx,
    covenant_id: &Hash32,
    limit: usize,
    after: Option<i64>,
) -> DbResult<Page<EventView>> {
    let sql = format!("{EVENT_SELECT} WHERE covenant_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3");
    let mut st = conn.prepare(&sql)?;
    let mut items = st
        .query_map(rusqlite::params![covenant_id.0.to_vec(), after.unwrap_or(0), limit as i64 + 1], |r| event_from_row(r, ctx))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|e| e.id.to_string())
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

/// Recent fills, newest first. `before` is the id of the oldest fill already seen.
pub fn fills(
    conn: &Connection,
    ctx: &ReadCtx,
    token: Option<&Hash32>,
    side: Option<i64>,
    before: Option<i64>,
    limit: usize,
) -> DbResult<Page<EventView>> {
    let mut conds = vec!["kind = 'fill'".to_string()];
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(t) = token {
        args.push(SqlValue::Blob(t.0.to_vec()));
        conds.push(format!("token_cov_id = ?{}", args.len()));
    }
    if let Some(s) = side {
        args.push(SqlValue::Integer(s));
        conds.push(format!("side = ?{}", args.len()));
    }
    if let Some(b) = before {
        args.push(SqlValue::Integer(b));
        conds.push(format!("id < ?{}", args.len()));
    }
    args.push(SqlValue::Integer(limit as i64 + 1));
    let sql = format!("{EVENT_SELECT} WHERE {} ORDER BY id DESC LIMIT ?{}", conds.join(" AND "), args.len());
    let mut st = conn.prepare(&sql)?;
    let mut items = st.query_map(params_from_iter(args.iter()), |r| event_from_row(r, ctx))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|e| e.id.to_string())
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

// ---------------------------------------------------------------------------------------------
// Counters

#[derive(Debug, Clone, Serialize)]
pub struct Counters {
    pub orders_total: i64,
    pub orders_listed: i64,
    pub orders_by_status: BTreeMap<String, i64>,
    pub rejects_total: i64,
    pub fills_total: i64,
    pub last_block_seq: i64,
    pub last_block_daa: i64,
}

fn count(conn: &Connection, sql: &str) -> DbResult<i64> {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map_err(DbError::from)
}

pub fn counters(conn: &Connection) -> DbResult<Counters> {
    let mut by_status = BTreeMap::new();
    let mut st = conn.prepare("SELECT status, COUNT(*) FROM order_state GROUP BY status")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (s, n) = r?;
        by_status.insert(s, n);
    }
    let last = conn
        .query_row("SELECT seq, daa FROM blocks ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .optional()?;
    let (last_block_seq, last_block_daa) = last.unwrap_or((0, 0));
    Ok(Counters {
        orders_total: count(conn, "SELECT COUNT(*) FROM orders")?,
        orders_listed: count(conn, "SELECT COUNT(*) FROM orders WHERE listed = 1")?,
        orders_by_status: by_status,
        rejects_total: count(conn, "SELECT COUNT(*) FROM rejects")?,
        fills_total: count(conn, "SELECT COUNT(*) FROM order_events WHERE kind = 'fill'")?,
        last_block_seq,
        last_block_daa,
    })
}

/// Number of KOB1 payloads that did not produce an order.
pub fn rejects_count(conn: &Connection) -> DbResult<i64> {
    count(conn, "SELECT COUNT(*) FROM rejects")
}

// ---------------------------------------------------------------------------------------------
// Token holdings (`GET /v1/token-utxos`)

/// A token UTXO of a tracked token with its PROVEN state (the state's P2SH equals the output's script public key).
/// Same leading fields as [`TokenUtxoView`]; `role` is `owned` for a holder that is not a KOB order.
#[derive(Debug, Clone, Serialize)]
pub struct HoldingView {
    pub txid: String,
    pub index: i64,
    pub token: String,
    /// `kcc20` or `kron`.
    pub family: &'static str,
    /// Token program template (`KCC20Ref_8x8`, `KronToken2433`, ...): what `tokenScriptPublicKey` / the builders take.
    pub program: String,
    pub template_hash: String,
    /// The state's 32-byte owner (x-only key, script hash or covenant id).
    pub owner: String,
    /// kcc20: `owner_scheme` (0 key, 4 covenant id); kron: `id_type` (0 key, 1 script hash, 2 covenant id, 3 address presence).
    pub owner_kind: i64,
    /// Token base units.
    pub amount: String,
    /// KAS carrier.
    pub value: String,
    /// `owned`, `custody` or `stray`.
    pub role: String,
    /// The token state as JSON, exactly the shape kob-wasm's token state functions and the builders take.
    pub state: Value,
    pub state_hex: String,
    pub created_daa: i64,
    pub spent: bool,
    pub spent_txid: Option<String>,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct HoldingFilter {
    /// The state's owner (32 bytes).
    pub owner: Option<Vec<u8>>,
    pub token: Option<Hash32>,
    /// Include spent holdings.
    pub include_spent: bool,
}

/// Cursor of the holdings list: `<created_block>:<txid hex>:<index>`.
pub fn parse_holding_cursor(s: &str) -> Option<(i64, Vec<u8>, i64)> {
    let mut it = s.splitn(3, ':');
    let b = it.next()?.parse().ok()?;
    let t = hex::decode(it.next()?).ok()?;
    let i = it.next()?.parse().ok()?;
    Some((b, t, i))
}

/// Holdings, oldest first, keyset-paginated on (created block, txid, index).
pub fn holdings(
    conn: &Connection,
    ctx: &ReadCtx,
    f: &HoldingFilter,
    limit: usize,
    cursor: Option<(i64, Vec<u8>, i64)>,
) -> DbResult<Page<HoldingView>> {
    let mut conds: Vec<String> = Vec::new();
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(o) = &f.owner {
        args.push(SqlValue::Blob(o.clone()));
        conds.push(format!("owner = ?{}", args.len()));
    }
    if let Some(t) = &f.token {
        args.push(SqlValue::Blob(t.0.to_vec()));
        conds.push(format!("token_cov_id = ?{}", args.len()));
    }
    if !f.include_spent {
        conds.push("spent_block IS NULL".to_string());
    }
    if let Some((b, t, i)) = cursor {
        args.push(SqlValue::Integer(b));
        let nb = args.len();
        args.push(SqlValue::Blob(t));
        let nt = args.len();
        args.push(SqlValue::Integer(i));
        let ni = args.len();
        conds.push(format!("(created_block, txid, idx) > (?{nb}, ?{nt}, ?{ni})"));
    }
    args.push(SqlValue::Integer(limit as i64 + 1));
    let sql = format!(
        "SELECT txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, created_block, created_daa, spent_block, spent_txid \
         FROM token_holdings {} ORDER BY created_block, txid, idx LIMIT ?{}",
        if conds.is_empty() { String::new() } else { format!("WHERE {}", conds.join(" AND ")) },
        args.len()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        let program: String = r.get(3)?;
        let state: Vec<u8> = r.get(9)?;
        let daa: i64 = r.get(12)?;
        let spent_block: Option<i64> = r.get(13)?;
        let block: i64 = r.get(11)?;
        let family: i64 = r.get(4)?;
        let tpl = TemplateId::from_name(&program).filter(|t| t.is_token()).map(kob_protocol::artifacts::token_template);
        let decoded = tpl.and_then(|t| kob_protocol::state::TokenState::decode_with(t, &state).ok());
        let view = HoldingView {
            txid: hx(r.get(0)?),
            index: r.get(1)?,
            token: hx(r.get(2)?),
            family: if family == 2 { "kron" } else { "kcc20" },
            template_hash: tpl.map(|t| hex::encode(&t.hash)).unwrap_or_default(),
            program,
            owner: hx(r.get(5)?),
            owner_kind: r.get(6)?,
            amount: r.get::<_, i64>(7)?.to_string(),
            value: r.get::<_, i64>(8)?.to_string(),
            role: r.get(10)?,
            state: decoded.and_then(|s| serde_json::to_value(&s).ok()).unwrap_or(Value::Null),
            state_hex: hex::encode(&state),
            created_daa: daa,
            spent: spent_block.is_some(),
            spent_txid: hxo(r.get(14)?),
            confirmations: ctx.confirmations(daa),
            settled: ctx.settled(daa),
        };
        Ok((block, view))
    })?;
    let mut all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if all.len() > limit {
        all.truncate(limit);
        all.last().map(|(b, v)| format!("{b}:{}:{}", v.txid, v.index))
    } else {
        None
    };
    Ok(Page { items: all.into_iter().map(|(_, v)| v).collect(), next_cursor })
}
