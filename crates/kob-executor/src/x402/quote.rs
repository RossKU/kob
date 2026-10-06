//! Swap-and-pay quotes from an indexer's book (`docs/spec/x402-swap-and-pay.md` §12, informative).
//!
//! Choosing the orders is the payer's job; any KOB indexer's book (the read API, a snapshot file, or
//! [`crate::indexer::book::snapshot`] in process) lists what a quote needs: the order UTXO, its
//! decoded state and, for asks, the exact custody. The facilitator itself exposes no quote endpoint.
//!
//! Only the orders the swap verifier accepts are quoted: plain `KobAsk` / `KobBid` (KCC-20) and
//! `KobAskKron` / `KobBidKron` (KRON; a route may mix both) that rest
//! (GTC), with a constant price (no decay / rise, no TWAP interval), active and not expired at the
//! book's DAA score. Legs are chosen best price first; an order the payer already lost in an
//! `order_conflict` is excluded by outpoint.

use kob_protocol::state::{AnyState, TIF_GTC};
use kob_protocol::tx::OrderUtxo;
use kob_x402::chain::Outpoint;
use kob_x402::client::swap::{OrderRef, Quote};

use crate::matcher::book::{ListedOrder, MemoryBook};
use crate::matcher::candidate::{excluded_by_expiry, token_program};

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
    /// Worst acceptable quote, sompi per whole token at the order's own scale (bids: at least; asks: at most).
    pub limit_price: Option<i64>,
    /// Order outpoints to skip (lost in an earlier `order_conflict`).
    pub exclude: Vec<Outpoint>,
}

fn outpoint_of(o: &ListedOrder) -> Outpoint {
    Outpoint::new(o.order.utxo.transaction_id, o.order.utxo.index)
}

/// A takeable level: price per whole token at `scale`, the base units available, the order's minimum fill and the smallest
/// fill that ends it (exempt from the minimum fill).
struct Level<'a> {
    price: i64,
    scale: i64,
    amount: i64,
    min_fill: i64,
    rest_from: i64,
    order: &'a ListedOrder,
}

impl Level<'_> {
    /// Whether the order accepts a fill of `n` base units.
    fn takes(&self, n: i64) -> bool {
        n > 0 && n <= self.amount && (n >= self.min_fill || n >= self.rest_from)
    }
}

fn levels<'a>(book: &'a MemoryBook, req: &QuoteRequest, utc: u64) -> Vec<Level<'a>> {
    let t = book.daa_score as i64;
    let mut out = Vec::new();
    for o in &book.orders {
        // an order whose token program is not one of its own family's (a KRON kind naming a KCC-20
        // program, or an unknown program) is not a swap leg: the verifier refuses it
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
                // what the escrow affords while keeping the maker's delivery carrier and reserve (the matcher's rule)
                let v = o.order.utxo.amount.min(i64::MAX as u64) as i64;
                let power = s.buying_power(v);
                let amount = if s.max_fill > 0 { power.min(s.max_fill) } else { power };
                if amount > 0 && req.limit_price.is_none_or(|p| s.price >= p) {
                    let rest_from = crate::matcher::candidate::bid_rest_from(s, v, amount);
                    out.push(Level { price: s.price, scale: s.scale, amount, min_fill: s.min_fill, rest_from, order: o });
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
                    out.push(Level {
                        price: s.price,
                        scale: s.scale,
                        amount,
                        min_fill: s.min_fill,
                        rest_from: s.amount_left,
                        order: o,
                    });
                }
            }
            _ => {}
        }
    }
    // the price per base unit, compared exactly (orders of one token may differ in scale)
    let per_base = |l: &Level| (l.price as i128, l.scale.max(1) as i128);
    let cmp = |a: (i128, i128), b: (i128, i128)| (a.0 * b.1).cmp(&(b.0 * a.1));
    match req.take {
        // best bid first, then the older UTXO (it has waited longer)
        Take::SellToBids => out.sort_by(|a, b| cmp(per_base(b), per_base(a)).then(a.order.utxo_daa().cmp(&b.order.utxo_daa()))),
        Take::BuyFromAsks => out.sort_by(|a, b| cmp(per_base(a), per_base(b)).then(a.order.utxo_daa().cmp(&b.order.utxo_daa()))),
    }
    out
}

/// The legs of one request, best price first; `None` when the book cannot fill `amount`. A level whose minimum fill the
/// rest of the request does not reach is skipped (unless the take ends the order).
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

/// A quote over one or more requests (a two-token route: the pay token's bids, then the merchant
/// token's asks). `lock_time` is the DAA score the transaction proves (a little below the virtual
/// DAA score, see [`Quote`]).
pub fn quote(book: &MemoryBook, reqs: &[QuoteRequest], lock_time: u64, utc: u64) -> Option<Quote> {
    let mut orders = Vec::new();
    for r in reqs {
        orders.extend(quote_legs(book, r, utc)?);
    }
    Some(Quote { lock_time, orders })
}

/// The plain limit orders of a book as an intent execution's [`kob_x402::intent::BookView`]: KCC-20 asks with their
/// exact custody (what an intent buys for the merchant), KCC-20 and KRON bids (what it sells the payer's token into); the
/// planner filters further (the intent's token programs, plain resting orders, activity).
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
