//! Swap-and-pay quotes from an indexer's book (`docs/spec/x402-swap-and-pay.md` §12, informative).
//!
//! Choosing the orders is the payer's job. Any KOB indexer's book lists what a quote needs: the
//! order UTXO, its decoded state and, for asks, the exact custody. The book comes from the read
//! API, a snapshot file, or [`crate::indexer::book::snapshot`] in process. The facilitator itself
//! exposes no quote endpoint.
//!
//! # Quoted orders
//!
//! Only orders the swap verifier accepts are quoted. Each one must:
//!
//! - be a plain `KobAsk` or `KobBid` (KCC-20), or a `KobAskKron` or `KobBidKron` (KRON), and a
//!   route may mix both families;
//! - rest as good-till-cancelled, with a constant price (no decay or rise, no TWAP interval);
//! - be active and not expired at the book's DAA score.
//!
//! Legs are chosen best price first. An order the payer already lost in an `order_conflict` is
//! excluded by its outpoint.

use kob_protocol::state::{AnyState, TIF_GTC};
use kob_protocol::tx::OrderUtxo;
use kob_x402::chain::Outpoint;
use kob_x402::client::swap::{OrderRef, Quote};

use crate::matcher::book::{ListedOrder, MemoryBook};
use crate::matcher::candidate::{excluded_by_expiry, plain_kip9, token_program, QtyRules};

/// Which side of the book a leg takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    /// The payer sells the token into bids (pay asset = the token, the swap yields KAS).
    SellToBids,
    /// The payer buys the token from asks (the swap yields the token for KAS).
    BuyFromAsks,
}

/// One leg group of a quote: `amount` base units of `token`, on the side `take`.
#[derive(Clone, Debug)]
pub struct QuoteRequest {
    pub token: [u8; 32],
    pub take: Take,
    /// Base units of the token.
    pub amount: i64,
    /// Worst acceptable price, in sompi per whole token at the order's own scale.
    ///
    /// For bids it is a minimum and for asks a maximum.
    pub limit_price: Option<i64>,
    /// Order outpoints to skip (lost in an earlier `order_conflict`).
    pub exclude: Vec<Outpoint>,
}

fn outpoint_of(o: &ListedOrder) -> Outpoint {
    Outpoint::new(o.order.utxo.transaction_id, o.order.utxo.index)
}

/// A takeable level of the book, one per order.
///
/// - `price` is per whole token at `scale`.
/// - `amount` is the base units available.
/// - `min_fill` is the order's minimum fill.
/// - `rest_from` is the smallest fill that ends the order. It is exempt from the minimum fill.
/// - `part_cap` is the largest fill that does not end it. It is the KIP-9 bound of the outputs a
///   partial fill leaves, as the matcher computes it in `crate::matcher::candidate::plain_kip9`.
struct Level<'a> {
    price: i64,
    scale: i64,
    amount: i64,
    min_fill: i64,
    rest_from: i64,
    part_cap: i64,
    order: &'a ListedOrder,
}

impl Level<'_> {
    /// Whether the order accepts a fill of `n` base units.
    fn takes(&self, n: i64) -> bool {
        n > 0 && n <= self.amount && (n >= self.rest_from || (n >= self.min_fill && n <= self.part_cap))
    }

    /// The level with the KIP-9 limits of its order's fills, or `None` when no fill can be mined.
    fn kip9(self, v: i64, cust: i64) -> Option<Self> {
        let r = QtyRules { cap: self.amount, min_fill: self.min_fill, rest_from: self.rest_from, part_cap: i64::MAX };
        let r = plain_kip9(&self.order.order.state, v, cust, self.price, r)?;
        Some(Level { amount: r.cap, min_fill: r.min_fill, rest_from: r.rest_from, part_cap: r.part_cap, ..self })
    }
}

fn levels<'a>(book: &'a MemoryBook, req: &QuoteRequest, utc: u64) -> Vec<Level<'a>> {
    let t = book.daa_score as i64;
    let mut out = Vec::new();
    for o in &book.orders {
        // An order whose token program is not its own family's is not a swap leg, and the verifier
        // refuses it. That covers a KRON kind naming a KCC-20 program, and an unknown program.
        if req.exclude.contains(&outpoint_of(o)) || excluded_by_expiry(o, t, utc) || token_program(&o.order.state).is_none() {
            continue;
        }
        if crate::sanity::check(&o.order.state).is_err() {
            continue;
        }
        match (&o.order.state, req.take) {
            (AnyState::KobBid(s) | AnyState::KobBidKron(s), Take::SellToBids) => {
                if s.token_cov_id != req.token || s.tif != TIF_GTC || s.slope != 0 || s.interval != 0 || t < s.active_from {
                    continue;
                }
                // What the escrow affords while keeping the maker's delivery carrier and reserve.
                // This is the matcher's rule.
                let v = o.order.utxo.amount.min(i64::MAX as u64) as i64;
                let power = s.buying_power(v);
                let amount = if s.max_fill > 0 { power.min(s.max_fill) } else { power };
                if amount > 0 && req.limit_price.is_none_or(|p| s.price >= p) {
                    let rest_from = crate::matcher::candidate::bid_rest_from(s, v, amount);
                    let l = Level {
                        price: s.price,
                        scale: s.scale,
                        amount,
                        min_fill: s.min_fill,
                        rest_from,
                        part_cap: i64::MAX,
                        order: o,
                    };
                    out.extend(l.kip9(v, 0));
                }
            }
            (AnyState::KobAsk(s) | AnyState::KobAskKron(s), Take::BuyFromAsks) => {
                if s.token_cov_id != req.token || s.tif != TIF_GTC || s.slope != 0 || s.interval != 0 || t < s.active_from {
                    continue;
                }
                if o.custody.is_none() || !o.custody_ok() || s.amount_left <= 0 {
                    continue;
                }
                let amount = if s.max_fill > 0 { s.amount_left.min(s.max_fill) } else { s.amount_left };
                if req.limit_price.is_none_or(|p| s.price <= p) {
                    let l = Level {
                        price: s.price,
                        scale: s.scale,
                        amount,
                        min_fill: s.min_fill,
                        rest_from: s.amount_left,
                        part_cap: i64::MAX,
                        order: o,
                    };
                    let v = o.order.utxo.amount.min(i64::MAX as u64) as i64;
                    let cust = o.custody.as_ref().map(|c| c.utxo.amount.min(i64::MAX as u64) as i64).unwrap_or(0);
                    out.extend(l.kip9(v, cust));
                }
            }
            _ => {}
        }
    }
    // Prices per base unit are compared exactly, because orders of one token may differ in scale.
    let per_base = |l: &Level| (l.price as i128, l.scale.max(1) as i128);
    let cmp = |a: (i128, i128), b: (i128, i128)| (a.0 * b.1).cmp(&(b.0 * a.1));
    match req.take {
        // Best bid first, then the older UTXO, which has waited longer.
        Take::SellToBids => out.sort_by(|a, b| cmp(per_base(b), per_base(a)).then(a.order.utxo_daa().cmp(&b.order.utxo_daa()))),
        Take::BuyFromAsks => out.sort_by(|a, b| cmp(per_base(a), per_base(b)).then(a.order.utxo_daa().cmp(&b.order.utxo_daa()))),
    }
    out
}

/// The legs of one request, best price first, or `None` when the book cannot fill `amount`.
///
/// A level is skipped when the rest of the request does not reach its minimum fill, unless the
/// take ends the order.
pub fn quote_legs(book: &MemoryBook, req: &QuoteRequest, utc: u64) -> Option<Vec<OrderRef>> {
    if req.amount <= 0 {
        return None;
    }
    let mut left = req.amount;
    let mut legs = Vec::new();
    for l in levels(book, req, utc) {
        if left == 0 {
            break;
        }
        let n = l.amount.min(left);
        if !l.takes(n) {
            continue;
        }
        let leg = match &l.order.order.state {
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
                OrderRef::bid(OrderUtxo { utxo: l.order.order.utxo.clone(), state: s.clone() }, n)
            }
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
                OrderRef::ask(OrderUtxo { utxo: l.order.order.utxo.clone(), state: s.clone() }, l.order.custody.clone()?, n)
            }
            _ => continue,
        };
        legs.push(leg);
        left -= n;
    }
    (left == 0).then_some(legs)
}

/// A quote over one or more requests, such as a two-token route.
///
/// A two-token route takes the pay token's bids, then the merchant token's asks. `lock_time` is the
/// DAA score the transaction proves, a little below the virtual DAA score (see [`Quote`]).
pub fn quote(book: &MemoryBook, reqs: &[QuoteRequest], lock_time: u64, utc: u64) -> Option<Quote> {
    let mut orders = Vec::new();
    for r in reqs {
        orders.extend(quote_legs(book, r, utc)?);
    }
    Some(Quote { lock_time, orders })
}

/// The plain limit orders of a book as an intent execution's [`kob_x402::intent::BookView`].
///
/// It holds KCC-20 asks with their exact custody, which an intent buys for the merchant. It also
/// holds KCC-20 and KRON bids, which an intent sells the payer's token into. The planner filters
/// further by the intent's token programs, plain resting orders and activity.
pub fn book_view(book: &MemoryBook) -> kob_x402::intent::BookView {
    let mut v = kob_x402::intent::BookView::default();
    for o in &book.orders {
        match &o.order.state {
            AnyState::KobAsk(s) => {
                if let Some(c) = o.custody.clone().filter(|_| o.custody_ok()) {
                    v.asks.push((OrderUtxo { utxo: o.order.utxo.clone(), state: s.clone() }, c));
                }
            }
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => v.bids.push(OrderUtxo { utxo: o.order.utxo.clone(), state: s.clone() }),
            _ => {}
        }
    }
    v
}
