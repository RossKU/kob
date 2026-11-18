//! The KAS book of a market checked against the node (`verifyBook` of the web app,
//! `web/src/app/book-verify.ts`).
//!
//! A market, close or crossing order is an auction that starts at the best price of the book, and
//! one indexer is no authority for that price: a phantom ask or bid would set the start, and
//! hiding the real ones would move it. The script of an order commits its whole state, so an
//! unspent output at the script re-derived from the claimed state proves the state.
//!
//! Each side is sorted best price first and checked in rounds of [`BOOK_CHECK_PER_SIDE`] rows,
//! one node read per round, until [`BOOK_CHECK_MIN_CONFIRMED`] orders are confirmed and the
//! planned size is covered, at most [`BOOK_CHECK_MAX_ROWS`] rows. Rows never checked are left out,
//! and a side with none confirmed is empty: the check fails closed. A curve is checked once, by
//! its lowest level row; its other levels follow from the confirmed state.

use std::collections::{BTreeMap, BTreeSet};

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::json::to_hex;
use kob_protocol::state::{AnyState, CurveAskState};
use serde_json::Value;

use super::data::INDEXER_TIMEOUT;
use super::http;
use super::market::TokenMarket;
use super::plan::{Book, BookOrder, Level, Side};
use crate::common::{lookup, order_spk, order_template, spk_bytes};
use crate::inputs;
use crate::wrpc::NodeUtxo;

/// Rows of a side read per round, best price first.
pub const BOOK_CHECK_PER_SIDE: usize = 3;
/// A side is verified once this many of its orders are confirmed and the planned size is covered.
pub const BOOK_CHECK_MIN_CONFIRMED: usize = 3;
/// Most rows looked at per side.
pub const BOOK_CHECK_MAX_ROWS: usize = 18;
/// Rows of another scale looked at per side, to check decimals that come from the indexer.
const SCALE_PROBE_PER_SIDE: usize = 3;

/// A book row that the node does not stand behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// Its covenant id (zero for an aggregated row).
    pub cov: [u8; 32],
    /// Its side.
    pub side: Side,
    /// The price the indexer lists.
    pub price: i64,
    /// `not-on-node`, `unproven`, `changed`, `other-market` or `unchecked`.
    pub reason: &'static str,
}

/// The book of a KAS market checked against the node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifiedBook {
    /// The confirmed rows at the market's scale.
    pub book: Book,
    /// What was left out and why.
    pub dropped: Vec<Dropped>,
    /// Rows behind the verified depth, left out unchecked.
    pub beyond: usize,
    /// Confirmed orders at the market's scale.
    pub confirmed: usize,
    /// Confirmed orders of other scales, by scale (only with `probe_scales`).
    pub other_scales: BTreeMap<i64, usize>,
}

fn i64_of(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn cov_of(r: &Value) -> Option<[u8; 32]> {
    r.get("covenant_id").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32)
}

/// Side, limit price, tip, minimum fill, active-from and expiry of the state of a KAS book order.
///
/// The price is the limit (an auction's start), as the indexer's `price`. A curve's is the price
/// of its current level.
fn book_facts(s: &AnyState) -> Option<(Side, i64, i64, i64, i64, i64)> {
    Some(match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => (Side::Sell, x.price, x.tip, x.min_fill, x.active_from, x.expiry_daa),
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => (Side::Buy, x.price, x.tip, x.min_fill, x.active_from, x.expiry_daa),
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) if x.tp_price > 0 => {
            (Side::Sell, x.tp_price, x.tip, x.min_fill, x.active_from, x.expiry_daa)
        }
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) if x.tp_price > 0 => {
            (Side::Buy, x.tp_price, x.tip, x.min_fill, x.active_from, x.expiry_daa)
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) if x.entry_stop == 0 => {
            (Side::Sell, x.price, x.tip, x.min_fill, x.active_from, x.expiry_daa)
        }
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) if x.entry_stop == 0 => {
            (Side::Buy, x.price, x.tip, x.min_fill, x.active_from, x.expiry_daa)
        }
        AnyState::KobCurveAsk(x) => (Side::Sell, x.price_now()?, x.tip, x.min_fill, x.active_from, 0),
        _ => return None,
    })
}

/// The levels left of a curve: table index, price and base units left in it (`curveLevelsLeft`).
/// The last level holds the rest of the total.
pub fn curve_levels_left(c: &CurveAskState) -> Vec<(usize, i64, i64)> {
    let (n, sold) = (c.table.len(), c.total - c.amount_left);
    let mut pos = 0i64;
    let mut out = vec![];
    for (j, price) in c.table.iter().enumerate() {
        let size = if j + 1 < n { c.level_size } else { c.total.saturating_sub(c.level_size.saturating_mul(n as i64 - 1)) };
        let end = pos.saturating_add(size);
        let open = if end > sold { end - sold.max(pos) } else { 0 };
        if open > 0 {
            out.push((j, *price, open));
        }
        pos = end;
    }
    out
}

/// What the node confirms of one book row.
struct Confirmed {
    /// An ask's custody (node-confirmed), else a bid's amount capped at what its KAS buys.
    amount: i64,
    tip: i64,
    min_fill: i64,
    scale: i64,
    /// A curve's levels left: table index, price, base units.
    levels: Option<Vec<(usize, i64, i64)>>,
}

/// An order as the indexer claims it, ready for the node read.
pub(crate) struct Claim {
    cov: [u8; 32],
    state: AnyState,
    outpoint: ([u8; 32], u32),
    ospk: ScriptPublicKey,
    custody: Option<Custody>,
}

/// A custody to look up: its script, token and outpoint.
type Custody = (ScriptPublicKey, [u8; 32], ([u8; 32], u32));

/// The indexer's view of a row's order and the scripts to look up, or why there is none.
fn claim_of(indexer: &str, network: &str, m: &TokenMarket, cov: &[u8; 32]) -> Result<Claim, &'static str> {
    let v = http::get_ok(indexer, &format!("/v1/orders/{}", to_hex(cov)), INDEXER_TIMEOUT).map_err(|_| "unproven")?;
    claim_from(&v, network, m, cov)
}

/// The claim of the indexer's order view `v` (see `claim_of`).
fn claim_from(v: &Value, network: &str, m: &TokenMarket, cov: &[u8; 32]) -> Result<Claim, &'static str> {
    let parsed = inputs::parse(v, "indexer order", network).map_err(|_| "unproven")?;
    let view = parsed.views.into_iter().next().ok_or("unproven")?;
    if view.covenant_id != *cov {
        return Err("changed");
    }
    let state = view.state.ok_or("unproven")?;
    let (txid, index, _) = view.current.ok_or("not-on-node")?;
    if kob_protocol::retired::retired_by_hash(&view.template_hash).is_some() {
        return Err("unproven");
    }
    let tpl = order_template(&view.template_hash).ok_or("unproven")?;
    let ospk = order_spk(tpl, &state).map_err(|_| "unproven")?;
    // The custody holds the market's commitment. An unbound curve names no token until its first
    // fill: its custody's token is the market's, or the node does not find it.
    let targets = crate::recover::custody_targets(&state, *cov, Some(m.ext)).map_err(|_| "unproven")?;
    let unbound = matches!(&state, AnyState::KobCurveAsk(c) if c.is_unbound());
    let custody = match (targets.first(), &view.custody) {
        (None, _) => None,
        (Some(_), None) => return Err("unproven"),
        // and it is the output of its own transaction (its genesis): a stray the indexer names is not its custody
        (Some(_), Some(cv)) if unbound && cv.txid != txid => return Err("other-market"),
        (Some((_, spk, token)), Some(cv)) => Some((spk.clone(), if unbound { m.cov } else { *token }, (cv.txid, cv.index))),
    };
    Ok(Claim { cov: *cov, state, outpoint: (txid, index), ospk, custody })
}

/// Whether the node stands behind the row `row` of `side`, and what it confirms. `Ok(None)` is an
/// order that is over or not yet live: no liquidity, and no finding either.
///
/// `scale` is the scale the order must have: the market's, or the row's own for a scale probe.
fn judge(
    m: &TokenMarket,
    daa: i64,
    side: Side,
    row: &Value,
    scale: i64,
    c: &Claim,
    at: &BTreeMap<Vec<u8>, Vec<NodeUtxo>>,
) -> Result<Option<Confirmed>, &'static str> {
    let found = |spk: &ScriptPublicKey, cov: [u8; 32], (txid, index): ([u8; 32], u32)| {
        at.get(&spk_bytes(spk)).and_then(|us| us.iter().find(|u| u.txid == txid && u.index == index && u.covenant_id == Some(cov)))
    };
    let node_order = found(&c.ospk, c.cov, c.outpoint).ok_or("not-on-node")?;
    let s = &c.state;
    let (s_side, price, tip, min_fill, active_from, expiry) = book_facts(s).ok_or("changed")?;
    if s_side != side || Some(price) != i64_of(row.get("price")) {
        return Err("changed");
    }
    // The confirmed state commits the token: an order of another token, program or commitment is
    // not of this market, whatever the indexer lists it under.
    let unbound = matches!(s, AnyState::KobCurveAsk(x) if x.is_unbound());
    let token = if unbound { c.custody.as_ref().map(|x| x.1).unwrap_or_default() } else { s.token_cov_id() };
    let ext_ok = m.family != kob_protocol::Family::Kcc20 || s.extension_commitment() == Some(m.ext);
    if token != m.cov || s.token_tpl_hash() != Some(m.tpl_hash) || !ext_ok {
        return Err("other-market");
    }
    if (expiry > 0 && daa >= expiry) || active_from > daa {
        return Ok(None);
    }
    if s.scale() != scale || i64_of(row.get("scale")).is_some_and(|r| r != s.scale()) {
        return Err("changed");
    }
    if i64_of(row.get("cur_value")).is_some_and(|v| v != node_order.amount as i64) {
        return Err("changed");
    }
    // An ask is liquidity only with the custody the node holds, and its amount is that.
    let custody = match &c.custody {
        Some((spk, token, outpoint)) => {
            found(spk, *token, *outpoint).ok_or("unproven")?;
            s.custody_amount()
        }
        None => None,
    };
    if side == Side::Sell && custody.is_none() {
        return Err("unproven");
    }
    let curve = match s {
        AnyState::KobCurveAsk(x) => Some(curve_levels_left(x)),
        _ => None,
    };
    if let (Some(left), None) = (custody, &curve) {
        let estimated = row.get("amount_estimated").and_then(Value::as_bool).unwrap_or(false);
        if !estimated && i64_of(row.get("amount_left")).is_some_and(|a| a != left) {
            return Err("changed");
        }
    }
    // A bid has no custody: its amount is believed up to what its confirmed KAS buys at its price.
    let cap = if price > 0 { (node_order.amount as i128 * s.scale() as i128 / price as i128).min(i64::MAX as i128) as i64 } else { 0 };
    let amount = custody.unwrap_or_else(|| i64_of(row.get("amount_left")).unwrap_or(1).min(cap));
    Ok(Some(Confirmed { amount, tip, min_fill, scale: s.scale(), levels: curve }))
}

/// The book of `m` at the clock `daa`, with the orders the node does not confirm left out.
///
/// `depth` is the size in base units the caller plans (0: the first confirmed orders suffice).
/// `probe_scales` also checks a few rows of other scales, counted in `other_scales`, so decimals
/// that come from the indexer can be compared with the orders on the node. `Ok(None)` is a book
/// the indexer does not answer; an `Err` is a node that cannot be asked.
pub fn verified_book(
    indexer: &str,
    node: &str,
    network: &str,
    m: &TokenMarket,
    daa: i64,
    depth: i64,
    probe_scales: bool,
) -> Result<Option<VerifiedBook>, String> {
    let Ok(v) = http::get_ok(indexer, &format!("/v1/books/{}?depth=100&aggregate=false", to_hex(&m.cov)), INDEXER_TIMEOUT) else {
        return Ok(None);
    };
    verify_rows(
        &v,
        m,
        daa,
        depth,
        probe_scales,
        &|cov: &[u8; 32]| claim_of(indexer, network, m, cov),
        &|spks: &[ScriptPublicKey]| lookup(node, network, spks),
    )
    .map(Some)
}

type NodeRead<'a> = &'a dyn Fn(&[ScriptPublicKey]) -> Result<BTreeMap<Vec<u8>, Vec<NodeUtxo>>, String>;
type ClaimRead<'a> = &'a dyn Fn(&[u8; 32]) -> Result<Claim, &'static str>;

struct SideCheck {
    side: Side,
    rows: Vec<Value>,
    aggregated: bool,
    cands: Vec<Value>,
    probes: Vec<Value>,
    next: usize,
    confirmed: usize,
    depth: i64,
    kept: Vec<BookOrder>,
}

impl SideCheck {
    fn new(side: Side, rows: Vec<Value>, scale: i64, probe_scales: bool) -> SideCheck {
        let aggregated = !rows.is_empty() && !rows.iter().all(|r| cov_of(r).is_some());
        // One candidate per order; a curve by its lowest level row.
        let mut lowest: BTreeMap<[u8; 32], i64> = BTreeMap::new();
        for r in &rows {
            if let (Some(c), Some(l)) = (cov_of(r), r.get("curve_level").and_then(Value::as_i64)) {
                let e = lowest.entry(c).or_insert(l);
                *e = (*e).min(l);
            }
        }
        let (mut seen, mut cands, mut probes) = (BTreeSet::new(), vec![], vec![]);
        for r in rows.iter().filter(|_| !aggregated) {
            let (Some(c), Some(_)) = (cov_of(r), i64_of(r.get("price"))) else { continue };
            if lowest.get(&c).is_some_and(|l| r.get("curve_level").and_then(Value::as_i64) != Some(*l)) || !seen.insert(c) {
                continue;
            }
            match i64_of(r.get("scale")) {
                Some(s) if s != scale => probes.push(r.clone()),
                _ => cands.push(r.clone()),
            }
        }
        // Best first: asks ascending, bids descending, the indexer's order within a price.
        cands.sort_by(|a, b| {
            let (pa, pb) = (i64_of(a.get("price")).unwrap_or(0), i64_of(b.get("price")).unwrap_or(0));
            if side == Side::Sell {
                pa.cmp(&pb)
            } else {
                pb.cmp(&pa)
            }
        });
        probes.truncate(if probe_scales { SCALE_PROBE_PER_SIDE } else { 0 });
        SideCheck { side, rows, aggregated, cands, probes, next: 0, confirmed: 0, depth: 0, kept: vec![] }
    }

    fn satisfied(&self, want: i64) -> bool {
        self.aggregated
            || self.next >= self.cands.len().min(BOOK_CHECK_MAX_ROWS)
            || (self.confirmed >= BOOK_CHECK_MIN_CONFIRMED && self.depth >= want)
    }

    fn keep(&mut self, row: &Value, x: &Confirmed) {
        self.confirmed += 1;
        let Some(levels) = &x.levels else {
            self.depth = self.depth.saturating_add(x.amount);
            let price = i64_of(row.get("price")).unwrap_or(0);
            self.kept.push(BookOrder { price, amount: x.amount, tip: x.tip, min_fill: Some(x.min_fill) });
            return;
        };
        // A curve: each level row counts at the price its confirmed state gives that level, at
        // most the size left in it.
        let cov = cov_of(row);
        for lv in self.rows.iter().filter(|r| cov_of(r) == cov) {
            let Some(l) = lv.get("curve_level").and_then(Value::as_i64) else { continue };
            let Some((_, price, left)) = levels.iter().find(|(j, ..)| *j as i64 == l) else { continue };
            if i64_of(lv.get("price")) != Some(*price) {
                continue;
            }
            let amount = i64_of(lv.get("amount_left")).unwrap_or(0).min(*left);
            if amount > 0 {
                self.depth = self.depth.saturating_add(amount);
                self.kept.push(BookOrder { price: *price, amount, tip: x.tip, min_fill: Some(x.min_fill) });
            }
        }
    }
}

fn dropped(side: Side, r: &Value, reason: &'static str) -> Dropped {
    Dropped { cov: cov_of(r).unwrap_or_default(), side, price: i64_of(r.get("price")).unwrap_or(0), reason }
}

fn verify_rows(
    v: &Value,
    m: &TokenMarket,
    daa: i64,
    want: i64,
    probe_scales: bool,
    claim: ClaimRead,
    node: NodeRead,
) -> Result<VerifiedBook, String> {
    let rows = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut sides = [
        SideCheck::new(Side::Sell, rows("asks"), m.scale, probe_scales),
        SideCheck::new(Side::Buy, rows("bids"), m.scale, probe_scales),
    ];
    let mut out = VerifiedBook::default();
    for s in sides.iter().filter(|s| s.aggregated) {
        out.dropped.extend(s.rows.iter().map(|r| dropped(s.side, r, "unchecked")));
    }
    for round in 0..BOOK_CHECK_MAX_ROWS / BOOK_CHECK_PER_SIDE {
        // (side, row, a scale probe)
        let mut batch: Vec<(usize, Value, bool)> = vec![];
        for (k, s) in sides.iter_mut().enumerate() {
            if round == 0 {
                batch.extend(s.probes.iter().map(|r| (k, r.clone(), true)));
            }
            if s.satisfied(want) {
                continue;
            }
            let end = (s.next + BOOK_CHECK_PER_SIDE).min(BOOK_CHECK_MAX_ROWS).min(s.cands.len());
            batch.extend(s.cands[s.next..end].iter().map(|r| (k, r.clone(), false)));
            s.next = end;
        }
        if batch.is_empty() {
            break;
        }
        let claims: Vec<Result<Claim, &'static str>> = batch.iter().map(|(_, r, _)| claim(&cov_of(r).unwrap_or_default())).collect();
        let mut spks = vec![];
        for c in claims.iter().flatten() {
            spks.push(c.ospk.clone());
            spks.extend(c.custody.iter().map(|x| x.0.clone()));
        }
        let at = if spks.is_empty() { BTreeMap::new() } else { node(&spks)? };
        for ((k, row, probe), c) in batch.into_iter().zip(claims) {
            let s = &mut sides[k];
            let scale = if probe { i64_of(row.get("scale")).unwrap_or(0) } else { m.scale };
            let verdict = c.and_then(|c| judge(m, daa, s.side, &row, scale, &c, &at));
            match (verdict, probe) {
                (Ok(Some(x)), true) => *out.other_scales.entry(x.scale).or_default() += 1,
                (_, true) | (Ok(None), false) => {}
                (Ok(Some(x)), false) => s.keep(&row, &x),
                (Err(reason), false) => out.dropped.push(dropped(s.side, &row, reason)),
            }
        }
    }
    for s in &mut sides {
        if s.aggregated {
            continue;
        }
        if s.confirmed == 0 {
            out.dropped.extend(s.cands[s.next..].iter().map(|r| dropped(s.side, r, "unchecked")));
        } else {
            out.beyond += s.cands.len() - s.next;
        }
        out.confirmed += s.confirmed;
        let side = s.side;
        s.kept.sort_by(|a, b| if side == Side::Sell { a.price.cmp(&b.price) } else { b.price.cmp(&a.price) });
        let mut levels: Vec<Level> = vec![];
        for o in &s.kept {
            match levels.last_mut() {
                Some(l) if l.price == o.price => {
                    l.amount = l.amount.saturating_add(o.amount);
                    l.orders = l.orders.saturating_add(1);
                }
                _ => levels.push(Level { price: o.price, amount: o.amount, orders: 1, tip: 0 }),
            }
        }
        let orders = Some(std::mem::take(&mut s.kept));
        match side {
            Side::Sell => (out.book.asks, out.book.ask_orders) = (levels, orders),
            Side::Buy => (out.book.bids, out.book.bid_orders) = (levels, orders),
        }
    }
    Ok(out)
}

/// Findings about the rows the check left out, for the plan.
pub fn dropped_issue(v: &VerifiedBook) -> Option<super::plan::Issue> {
    if v.dropped.is_empty() {
        return None;
    }
    let shown: Vec<String> = v
        .dropped
        .iter()
        .take(3)
        .map(|d| {
            let side = if d.side == Side::Sell { "ask" } else { "bid" };
            format!("{side} {} at {} ({})", to_hex(&d.cov), d.price, d.reason)
        })
        .collect();
    let more = v.dropped.len().saturating_sub(shown.len());
    Some(super::plan::warning(
        "BOOK_ORDERS_DROPPED",
        format!(
            "{} order(s) the indexer lists are not confirmed on the node and are left out of the book: {}{}",
            v.dropped.len(),
            shown.join(", "),
            if more > 0 { format!(", and {more} more") } else { String::new() }
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::artifacts::{token_template, TemplateId};
    use kob_protocol::state::{AskState, BidState, TokenState};
    use serde_json::json;

    const TOKEN: [u8; 32] = [0x70; 32];
    const EXT: [u8; 32] = [0xee; 32];
    const P: TemplateId = TemplateId::Kcc20Ref;
    const DAA: i64 = 1_000_000;

    fn market() -> TokenMarket {
        TokenMarket::new(TOKEN, "TST", 3, P, Some(EXT), None).unwrap()
    }

    fn ask(token: [u8; 32], price: i64, amount: i64) -> AnyState {
        let t = token_template(P);
        AnyState::KobAsk(AskState {
            maker: [0x11; 32],
            token_cov_id: token,
            token_tpl_hash: t.hash,
            tpl_prefix_len: t.prefix.len() as i64,
            tpl_suffix_len: t.suffix.len() as i64,
            scale: 1000,
            min_fill: 1000,
            price,
            tip: 0,
            tif: 0,
            active_from: 0,
            expiry_daa: DAA + 1000,
            refund_tip: 0,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 1,
            amount_left: amount,
            extension_commitment: EXT,
        })
    }

    fn bid(price: i64, scale: i64) -> AnyState {
        let t = token_template(P);
        AnyState::KobBid(BidState {
            maker: [0x11; 32],
            token_cov_id: TOKEN,
            token_tpl_hash: t.hash,
            tpl_prefix_len: t.prefix.len() as i64,
            tpl_suffix_len: t.suffix.len() as i64,
            extension_commitment: EXT,
            scale,
            min_fill: scale,
            price,
            tip: 0,
            tif: 0,
            active_from: 0,
            expiry_daa: DAA + 1000,
            refund_tip: 0,
            reserve: 0,
            delivery_carrier: 0,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 1,
            mode: 0,
            recv_threshold: 0,
            topup_tip: 0,
            bought: 0,
        })
    }

    /// An order the indexer claims (`tag` names its outpoints) and, when `on_node`, its UTXOs.
    struct Fixture {
        claims: BTreeMap<[u8; 32], AnyState>,
        node: Vec<NodeUtxo>,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture { claims: BTreeMap::new(), node: vec![] }
        }
        fn add(&mut self, tag: u8, s: AnyState, value: u64, on_node: bool) -> [u8; 32] {
            let cov = [tag; 32];
            if on_node {
                self.node.push(utxo(&s.spk(), tag, value, cov));
                if let Some(a) = s.custody_amount().filter(|a| *a > 0) {
                    let c = TokenState::custody(P.family(), a, cov, EXT).spk_with(token_template(P));
                    self.node.push(utxo(&c, tag + 1, 100_000_000, s.token_cov_id()));
                }
            }
            self.claims.insert(cov, s);
            cov
        }
        fn run(&self, book: Value, depth: i64, probe: bool) -> VerifiedBook {
            let claim = |cov: &[u8; 32]| -> Result<Claim, &'static str> {
                let s = self.claims.get(cov).ok_or("unproven")?.clone();
                let custody = s.custody_amount().filter(|a| *a > 0).map(|a| {
                    let spk = TokenState::custody(P.family(), a, *cov, EXT).spk_with(token_template(P));
                    (spk, s.token_cov_id(), ([cov[0] + 1; 32], 0))
                });
                Ok(Claim { cov: *cov, ospk: s.spk(), state: s, outpoint: ([cov[0]; 32], 0), custody })
            };
            let node = |spks: &[ScriptPublicKey]| -> Result<BTreeMap<Vec<u8>, Vec<NodeUtxo>>, String> {
                let mut at: BTreeMap<Vec<u8>, Vec<NodeUtxo>> = BTreeMap::new();
                for spk in spks {
                    let k = spk_bytes(spk);
                    at.insert(k.clone(), self.node.iter().filter(|u| u.spk == k).cloned().collect());
                }
                Ok(at)
            };
            verify_rows(&book, &market(), DAA, depth, probe, &claim, &node).unwrap()
        }
    }

    fn utxo(spk: &ScriptPublicKey, tag: u8, amount: u64, cov: [u8; 32]) -> NodeUtxo {
        NodeUtxo {
            address: None,
            txid: [tag; 32],
            index: 0,
            amount,
            spk: spk_bytes(spk),
            daa: 990_000,
            coinbase: false,
            covenant_id: Some(cov),
        }
    }

    fn row(cov: [u8; 32], price: i64, amount: i64) -> Value {
        json!({"covenant_id": to_hex(&cov), "price": price.to_string(), "amount_left": amount.to_string(), "scale": 1000})
    }

    #[test]
    fn a_phantom_ask_is_left_out_and_the_real_one_is_the_touch() {
        let mut f = Fixture::new();
        let phantom = f.add(0x10, ask(TOKEN, 100_000_000, 5000), 100_000_000, false);
        let real = f.add(0x20, ask(TOKEN, 300_000_000, 5000), 100_000_000, true);
        let v = f.run(json!({"asks": [row(phantom, 100_000_000, 5000), row(real, 300_000_000, 5000)], "bids": []}), 0, false);
        assert_eq!(v.book.asks.len(), 1);
        assert_eq!((v.book.asks[0].price, v.book.asks[0].amount), (300_000_000, 5000));
        assert_eq!(v.confirmed, 1);
        assert_eq!(v.dropped, vec![Dropped { cov: phantom, side: Side::Sell, price: 100_000_000, reason: "not-on-node" }]);
        assert!(dropped_issue(&v).is_some_and(|i| i.code == "BOOK_ORDERS_DROPPED"));
    }

    #[test]
    fn another_tokens_order_a_changed_amount_and_a_side_with_none_confirmed() {
        let mut f = Fixture::new();
        let other = f.add(0x10, ask([0x71; 32], 100_000_000, 5000), 100_000_000, true);
        let bigger = f.add(0x20, ask(TOKEN, 200_000_000, 5000), 100_000_000, true);
        let v = f.run(json!({"asks": [row(other, 100_000_000, 5000), row(bigger, 200_000_000, 9000)], "bids": []}), 0, false);
        assert!(v.book.asks.is_empty() && v.book.ask_orders == Some(vec![]), "{:?}", v.book);
        let reasons: Vec<&str> = v.dropped.iter().map(|d| d.reason).collect();
        assert_eq!(reasons, ["other-market", "changed"]);
    }

    #[test]
    fn a_bid_is_capped_by_its_confirmed_kas_and_sorted_best_first() {
        let mut f = Fixture::new();
        // 2 KAS at 2 KAS per token buy 1 token (1000 base units), whatever the row claims.
        let low = f.add(0x10, bid(100_000_000, 1000), 500_000_000, true);
        let high = f.add(0x20, bid(200_000_000, 1000), 200_000_000, true);
        let v = f.run(json!({"asks": [], "bids": [row(low, 100_000_000, 4000), row(high, 200_000_000, 50_000)]}), 0, false);
        let got: Vec<(i64, i64)> = v.book.bids.iter().map(|l| (l.price, l.amount)).collect();
        assert_eq!(got, [(200_000_000, 1000), (100_000_000, 4000)]);
    }

    #[test]
    fn rounds_stop_once_three_are_confirmed_and_the_depth_is_covered() {
        let mut f = Fixture::new();
        let mut rows = vec![];
        for k in 0..6u8 {
            let cov = f.add(0x10 + 2 * k, ask(TOKEN, 100_000_000 + k as i64, 1000), 100_000_000, true);
            rows.push(row(cov, 100_000_000 + k as i64, 1000));
        }
        let v = f.run(json!({"asks": rows.clone(), "bids": []}), 0, false);
        assert_eq!((v.confirmed, v.beyond, v.book.asks.len()), (3, 3, 3));
        let v = f.run(json!({"asks": rows, "bids": []}), 5000, false);
        assert_eq!((v.confirmed, v.beyond, v.book.asks.len()), (6, 0, 6));
    }

    #[test]
    fn a_probe_counts_confirmed_orders_of_another_scale() {
        let mut f = Fixture::new();
        let real = f.add(0x10, bid(100_000_000, 100_000_000), 500_000_000, true);
        let mut r = row(real, 100_000_000, 1000);
        r["scale"] = json!(100_000_000);
        let v = f.run(json!({"asks": [], "bids": [r.clone()]}), 0, true);
        assert_eq!((v.confirmed, v.other_scales.get(&100_000_000).copied()), (0, Some(1)));
        assert!(v.book.bids.is_empty());
        // Without the probe the other scale is not looked at.
        let v = f.run(json!({"asks": [], "bids": [r]}), 0, false);
        assert!(v.other_scales.is_empty());
    }

    #[test]
    fn curve_levels_left_the_last_level_holds_the_rest() {
        let t = token_template(P);
        let mut c = CurveAskState {
            maker: [0x11; 32],
            token_cov_id: TOKEN,
            token_tpl_hash: t.hash,
            tpl_prefix_len: 0,
            tpl_suffix_len: 0,
            scale: 1000,
            min_fill: 1,
            tip: 0,
            active_from: 0,
            level_size: 100,
            total: 350,
            amount_left: 350,
            extension_commitment: EXT,
            table: vec![10, 20, 30],
        };
        assert_eq!(curve_levels_left(&c), [(0, 10, 100), (1, 20, 100), (2, 30, 150)]);
        c.amount_left = 180;
        assert_eq!(curve_levels_left(&c), [(1, 20, 30), (2, 30, 150)]);
    }

    /// An unbound curve (created in its token's genesis) is claimed under its custody's token only
    /// with the custody of its own transaction: a look-alike from another one is not this market's.
    #[test]
    fn an_unbound_curves_custody_is_the_output_of_its_own_transaction() {
        let t = token_template(P);
        let c = CurveAskState {
            maker: [0x11; 32],
            token_cov_id: [0; 32],
            token_tpl_hash: t.hash,
            tpl_prefix_len: t.prefix.len() as i64,
            tpl_suffix_len: t.suffix.len() as i64,
            scale: 1000,
            min_fill: 1,
            tip: 0,
            active_from: 0,
            level_size: 100,
            total: 200,
            amount_left: 200,
            extension_commitment: EXT,
            table: vec![10, 20],
        };
        let cov = [0x40; 32];
        let view = |custody_tx: u8| {
            json!({
                "covenant_id": to_hex(&cov), "template_hash": to_hex(&kob_protocol::artifacts::template(TemplateId::KobCurveAsk).hash),
                "status": "open", "state_known": true, "state": serde_json::to_value(AnyState::KobCurveAsk(c.clone())).unwrap(),
                "current": {"txid": to_hex(&[0x41; 32]), "index": 2, "value": "100000000"}, "current_daa": 1000,
                "extension_commitment": to_hex(&EXT), "strays": [],
                "custody": {"ok": true, "utxo": {
                    "txid": to_hex(&[custody_tx; 32]), "index": 0, "token": to_hex(&TOKEN), "owner": to_hex(&cov), "amount": "200",
                    "value": "100000000", "role": "custody", "created_daa": 1000, "spent": false, "settled": true,
                }},
            })
        };
        let claim = claim_from(&view(0x41), "testnet-10", &market(), &cov).ok().expect("claimed");
        let (_, token, outpoint) = claim.custody.expect("its custody");
        assert_eq!((token, outpoint), (TOKEN, ([0x41; 32], 0)));
        assert_eq!(claim_from(&view(0x42), "testnet-10", &market(), &cov).err(), Some("other-market"));
    }
}
