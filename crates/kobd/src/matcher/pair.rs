//! Pair orders as participants of the global batch (`docs/spec/matcher.md` §3.5).
//!
//! A pair order of tokens A/B (`KobPair`, `KobCondPair`, `KobIfdPair`) enforces only its own
//! guarantees, as `docs/spec/order-types.md` describes under pair orders:
//! - An ask sells its base A, so its custody S is A. It receives at least `ceil(n × p / scale(A))`
//!   of B for n base units of A.
//! - A bid holds a B escrow, so S is B. It pays exactly `floor(n × p / scale(A))` of B and
//!   receives exactly n of A.
//!
//! Fillers may route through the KAS books, net any number of opposite pair orders, or fill from
//! inventory. The reference planner holds no tokens, so it routes and nets.
//!
//! # Netting
//!
//! `super::batch` nets the [`PairInfo`] participants of one token pair. The pair orders that sell
//! token X for token Y meet those that sell Y for X, best limits first, any number per side. Each
//! participant's fill is computed exactly at its own covenant rounding. An ask's receipt is a
//! minimum, and a bid's payment and receipt are exact.
//!
//! The netted orders may release more of a token than they receive. That surplus goes to the pair
//! asks that buy the token, because their deliveries are minimums, and the builder hands it to the
//! first one. When it is profitable, the surplus is sold to the plain KAS bids of that token
//! instead ([`PairRole::Surplus`]). Netting runs first in every allocation, and what is left of a
//! netted order is routed.
//!
//! # Route
//!
//! A pair order enters the KAS books as two legs that share its one covenant input:
//! - Its Sell leg sells the token S it releases into the plain KAS bids of S. It is an ask in
//!   every book of S's market.
//! - Its Buy leg buys the token T it receives from the plain KAS asks of T. It is a bid in every
//!   book of T's market.
//!
//! Each leg is ranked at its implied quote at `t`. The Sell leg is ranked at what the T for one
//! whole A of the order costs at the best plain ask of T, less the order's KAS tip. The Buy leg is
//! ranked at what the S of one whole A fetches at the best plain bid of S.
//!
//! The batch reconciles the legs to one fill n. The S is released exactly, and the T bought covers
//! the order's receipt. It covers an exact receipt exactly. For an ask, whose receipt is a minimum,
//! it covers at least the receipt, and the excess goes to the ask's delivery.
//!
//! # Prices and income
//!
//! Prices are recorded only from KAS-book fills, the indexer's rule, because a netting fill moves
//! no KAS. The operator keeps the KAS spread of the route legs' counterparties, the netting surplus
//! it sells and the pair orders' KAS tips, less the fee.
//!
//! # Triggers
//!
//! Pair conditionals (`KobCondPair` stop legs) and pair stop entries (`KobIfdPair`) arm from
//! trigger evidence of the same transaction ([`PairNeed`]). The evidence has one of two modes:
//! - Mode 0 is two KAS-book fills, of a plain resting order of A and one of B, read as their
//!   implied rate.
//! - Mode 1 is the fill of a resting `KobPair` of the same pair ([`PairTouch`]).
//!
//! A booked exit's take-profit re-arms its entry in the same transaction ([`MergeOf`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::family::Family;
use kob_protocol::state::*;

use super::book::{BookKey, CovId, ListedOrder, Market};
use super::candidate::*;
use super::family::token_limits;

/// Least KAS an output of a fill can hold, such as a continuation, an exit UTXO or a delivery's
/// carrier.
///
/// Each such output carries a covenant id. Below 0.08 KAS its storage mass alone exceeds the block
/// limit, and the transaction could never be mined. This is
/// [`kob_protocol::tx::COVENANT_OUTPUT_MIN`], the bound the builders and `seal` apply. A fill that
/// would leave less is not planned. An order whose partial fills all would is matched only in full.
const DUST: i64 = COV_OUTPUT_MIN;

/// How a pair order's fill of n base units of its base token A maps to the token amounts it
/// releases and receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PairShape {
    /// Sells n of A (S = A) and receives at least `ceil(n × p / scale(A))` of B (T = B).
    ///
    /// This is a `KobPair` or `KobCondPair` ask. Its delivery takes the batch's surplus of B.
    AskMin,
    /// Sells n of A and receives exactly `ceil(n × p / scale(A))` of B.
    ///
    /// This is a sell-first `KobIfdPair`, whose receipt is its exit's custody.
    AskExact,
    /// Pays exactly `floor(n × p / scale(A))` of B (S = B) and receives exactly n of A (T = A).
    ///
    /// This is a `KobPair` or `KobCondPair` bid, or a buy-first `KobIfdPair`.
    Bid,
}

/// A booked exit's take-profit with its entry's merge.
///
/// It is a `KobCondPair` leg 0 before `rptUntil`. The entry is re-armed in the same transaction,
/// by `KobIfdPair.fill` with the merge argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOf {
    /// The entry's covenant id.
    pub entry: CovId,
    /// Token inputs and outputs the entry's custodies add, per token: (A in, A out, B in, B out).
    pub slots: (u32, u32, u32, u32),
    /// The entry UTXO's KAS. Its continuation keeps at least this.
    pub entry_value: i64,
    /// The entry's exit carrier. A merge that creates a custody pays it from the entry.
    pub exit_carrier: i64,
    /// Custodies the merge creates, for tokens of which the entry had none.
    pub new_custodies: i64,
}

/// What an unarmed pair stop reads from its trigger evidence.
///
/// The stop is a `KobCondPair` stop leg or a `KobIfdPair` stop entry. Its evidence is one of:
/// - mode 0, two KAS-book fills of the same transaction, of a plain resting order of A and one of
///   B, at the implied rate `a × scale(B) / b`;
/// - mode 1, the fill of a resting `KobPair` of the same pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairNeed {
    /// Whether the evidence's A side is an ask.
    ///
    /// When true the rate fell. A sell stop reads an ask of A and a bid of B, or a pair ask. When
    /// false the rate rose. A buy stop reads a bid of A and an ask of B, or a pair bid.
    pub ask_a: bool,
    /// Covenant id of token A.
    pub a: [u8; 32],
    /// Covenant id of token B.
    pub b: [u8; 32],
    /// The extension commitment of A's market, the order's own ([`markets`]).
    ///
    /// The planner only reads evidence of the order's own markets, never of another commitment of
    /// the same covenant id. The covenants refuse such evidence too.
    pub a_ext: [u8; 32],
    /// The extension commitment of B's market, the order's own.
    pub b_ext: [u8; 32],
    /// Scale of A.
    pub a_scale: i64,
    /// Scale of B.
    pub b_scale: i64,
    /// `minTouch`, in base units of A.
    pub min_touch: i64,
    /// The B value of `minTouch` at the stop, the B threshold of mode 0.
    pub min_touch_b: Option<i64>,
    /// `minRestDaa`.
    pub min_rest: i64,
    /// The order's state, a `KobCondPair` or `KobIfdPair`. Its own `arms` and `trail_k` decide.
    pub state: AnyState,
}

/// What the fill of a resting `KobPair` offers as mode-1 trigger evidence.
///
/// It mirrors `pair_evidence` of the builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairTouch {
    /// True for a pair ask, which sells A, and false for a pair bid.
    pub ask: bool,
    /// Covenant id of token A.
    pub a: [u8; 32],
    /// Covenant id of token B.
    pub b: [u8; 32],
    /// The extension commitment of the resting order's market of A ([`markets`]).
    pub a_ext: [u8; 32],
    /// The extension commitment of the resting order's market of B.
    pub b_ext: [u8; 32],
    /// Scale of A.
    pub a_scale: i64,
    /// Scale of B.
    pub b_scale: i64,
    /// The order's `price`, in B base units per whole A.
    pub price: i64,
    /// `max(UTXO DAA + interval, activeFrom, custody DAA)`.
    pub exposed_since: i64,
}

/// One pair order on one leg, normalised at `t` for the planner.
///
/// A `KobCondPair` has one per take-profit or stop leg it can fill. The Sell and Buy legs of the
/// order share it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairInfo {
    /// Covenant id of the order.
    pub id: CovId,
    /// Template of the order.
    pub kind: TemplateId,
    /// Conditional leg, 0 for take-profit or limit and 1 for stop. 0 for the other kinds.
    pub leg: u8,
    /// How a fill maps to the token amounts the order releases and receives.
    pub shape: PairShape,
    /// The order's state.
    pub state: AnyState,
    /// The quote of this fill at `t`, in B base units per whole A.
    ///
    /// It is the order's price, its auction's price or its leg price.
    pub price: i64,
    /// The price that ranks the order.
    ///
    /// For a booked exit it is the effective price, because its merge moves the repeat budget.
    pub rank_price: i64,
    /// Scale of A.
    pub a_scale: i64,
    /// KAS tip, in sompi per whole A.
    pub tip: i64,
    /// The market of A: the token, its program and the extension commitment of the outputs the
    /// batch makes of it.
    pub a: Market,
    /// The market of B.
    pub b: Market,
    /// Most base units of A fillable now, by the covenant's affordability rules.
    pub cap: i64,
    /// The largest fill below `cap` that the covenant accepts, 0 for none.
    ///
    /// A partial fill must leave a continuation that funds its delivery carrier and tip. Once the
    /// order UTXO's prefunded carriers are used up, only `cap` itself, the fill of everything
    /// left, is possible. So the fills the covenant accepts are `[minFill, part_cap]` and `cap`,
    /// not every n up to `cap`.
    pub part_cap: i64,
    /// `amountLeft`, in base units of A.
    pub amount_left: i64,
    /// `minFill`, in base units of A.
    pub min_fill: i64,
    /// A FOK order, filled only whole.
    pub fok: bool,
    /// An IOC order.
    pub ioc: bool,
    /// The S the order holds, its exact custody. Its rest is `custody − S released`.
    pub custody: i64,
    /// Both KAS books exist, so the order may be routed. Its legs are in the book lists.
    pub route: bool,
    /// A pair market order whose auction price still moves at `t`.
    ///
    /// It is never cut below what the books reach.
    pub relaxing: bool,
    /// For an unarmed stop, its trigger need. It is filled only next to its evidence.
    pub need: Option<PairNeed>,
    /// For a resting `KobPair` without decay, the mode-1 evidence its fill offers.
    pub touch: Option<PairTouch>,
    /// For a booked exit's take-profit, the merge of its entry.
    pub merge: Option<MergeOf>,
    /// The order UTXO's KAS.
    pub value: i64,
}

fn qo(n: i64, p: i64, scale: i64, r: Round) -> Option<i64> {
    quote_of(n, p, scale, r)
}

impl PairInfo {
    /// The leg whose quantity is the fill n, an ask's Sell leg or a bid's Buy leg.
    pub fn primary(&self) -> PairRole {
        match self.shape {
            PairShape::AskMin | PairShape::AskExact => PairRole::Sell,
            PairShape::Bid => PairRole::Buy,
        }
    }
    /// True for an ask, which sells A.
    pub fn sells_a(&self) -> bool {
        self.shape != PairShape::Bid
    }
    /// The market of the token S that the order sells.
    pub fn s_market(&self) -> Market {
        if self.sells_a() {
            self.a
        } else {
            self.b
        }
    }
    /// The market of the token T that the order buys.
    pub fn t_market(&self) -> Market {
        if self.sells_a() {
            self.b
        } else {
            self.a
        }
    }
    /// One whole A of the order in each token, as (S base units, T base units) at its rank price.
    ///
    /// The implied quotes and the netting order use it.
    pub fn whole(&self) -> (i64, i64) {
        if self.sells_a() {
            (self.a_scale, self.rank_price)
        } else {
            (self.rank_price, self.a_scale)
        }
    }
    /// True when the order's receipt is exact, as a bid's A or a sell-first entry's exit custody.
    ///
    /// Otherwise the order is an ask whose receipt is a minimum, and it takes the batch's surplus
    /// of its token.
    pub fn buy_exact(&self) -> bool {
        self.shape != PairShape::AskMin
    }
    /// S released into the transaction for a fill of n.
    ///
    /// The amounts are the covenant's, including a booked exit's merge.
    pub fn sell_qty(&self, n: i64) -> Option<i64> {
        if n <= 0 {
            return Some(0);
        }
        match (self.shape, &self.merge, &self.state) {
            (PairShape::Bid, Some(_), AnyState::KobCondPair(s)) => {
                // A re-arming bid exit pays at most proceeds - 1 on a partial fill, or
                // custody - back - 1 on a sell-out.
                let floor = qo(n, self.price, self.a_scale, Round::Down)?;
                let back = s.rpt_back(n)?;
                if n < s.amount_left {
                    Some(floor.min(s.rpt_proceeds(n)?.checked_sub(1)?))
                } else {
                    Some(floor.min(s.custody.checked_sub(back)?.checked_sub(1)?))
                }
            }
            (PairShape::Bid, _, _) => qo(n, self.price, self.a_scale, Round::Down),
            _ => Some(n),
        }
    }
    /// T the order receives for a fill of n, a minimum for an ask and exact otherwise.
    pub fn buy_qty(&self, n: i64) -> Option<i64> {
        if n <= 0 {
            return Some(0);
        }
        match (self.shape, &self.merge, &self.state) {
            (PairShape::AskMin, Some(_), AnyState::KobCondPair(s)) => {
                // A re-arming ask exit delivers more than the entry's budget. The maker's profit is
                // at least one unit.
                let t = qo(n, self.price, self.a_scale, Round::Up)?;
                Some(t.max(s.rpt_proceeds(n)?.checked_add(1)?))
            }
            (PairShape::Bid, _, _) => Some(n),
            _ => qo(n, self.price, self.a_scale, Round::Up),
        }
    }
    /// The largest n in `[0, cap]` whose `f(n)` stays within `q`, for a monotone `f`.
    fn n_within(&self, q: i64, f: impl Fn(i64) -> Option<i64>) -> i64 {
        if q <= 0 {
            return 0;
        }
        let ok = |n: i64| f(n).is_some_and(|v| v <= q);
        if ok(self.cap) {
            return self.cap;
        }
        let (mut lo, mut hi) = (0i64, self.cap);
        // Invariant: ok(lo) and !ok(hi).
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if ok(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }
    /// The largest fill whose S release is at most `q`.
    pub fn n_of_sell(&self, q: i64) -> i64 {
        if self.sells_a() {
            return q.clamp(0, self.cap);
        }
        self.n_within(q, |n| self.sell_qty(n))
    }
    /// The largest fill whose receipt is covered by `q` of T.
    pub fn n_of_buy(&self, q: i64) -> i64 {
        if !self.sells_a() {
            return q.clamp(0, self.cap);
        }
        self.n_within(q, |n| self.buy_qty(n))
    }
    /// Whether a fill of n passes the covenant's quantity rules.
    ///
    /// The rules are the cap and `part_cap`, `minFill` unless the fill takes everything left, and
    /// a FOK only whole.
    pub fn quantity_ok(&self, n: i64) -> bool {
        n > 0
            && n <= self.cap
            && (n == self.cap || n <= self.part_cap)
            && min_fill_ok(n, self.amount_left, self.min_fill)
            && (!self.fok || n == self.amount_left)
    }
    /// The largest fill of at most `n` that the order accepts ([`Self::quantity_ok`]), or 0.
    pub fn fit_down(&self, n: i64) -> i64 {
        if n <= 0 {
            return 0;
        }
        if self.quantity_ok(n) {
            return n;
        }
        let m = n.min(self.cap).min(self.part_cap);
        if m > 0 && self.quantity_ok(m) {
            m
        } else {
            0
        }
    }
    /// True when the order can only be filled in full.
    ///
    /// That is a FOK, or an order whose carriers are used up so that no partial fill is possible.
    pub fn whole_only(&self) -> bool {
        self.fok || self.part_cap < self.min_fill.min(self.cap)
    }
    /// KAS tip that a fill of n releases to the filler, `floor(n × tip / scale(A))`.
    pub fn tip_kas(&self, n: i64) -> i64 {
        qo(n, self.tip, self.a_scale, Round::Down).unwrap_or(0)
    }
    /// The fill n that a Sell leg quantity stands for.
    ///
    /// A bid's S release is rounded, so it is the largest n the quantity covers.
    fn n_at_sell(&self, q: i64) -> i64 {
        if self.sells_a() {
            q
        } else {
            self.n_of_sell(q)
        }
    }
    /// Token inputs and outputs of the Sell leg's token at a Sell quantity of `q`.
    ///
    /// They are the custody input, a merge's entry custodies of that token, the custody's rest or
    /// return, and a re-arming bid exit's B profit.
    pub fn sell_slots(&self, q: i64) -> (u32, u32) {
        if q <= 0 {
            return (0, 0);
        }
        let n = self.n_at_sell(q);
        let left = self.amount_left - n;
        match (&self.merge, self.shape) {
            // A bid exit (S = B): its custody in, its rest on a partial fill, the maker's B profit,
            // and the entry's B prefund in and out.
            (Some(m), PairShape::Bid) => (1 + m.slots.2, u32::from(left > 0) + 1 + m.slots.3),
            // An ask exit (S = A): its custody in and its rest on a partial fill. A re-arm never
            // returns tokens.
            (Some(_), _) => (1, u32::from(left > 0)),
            (None, _) => {
                // The custody's rest on a GTC partial fill, or its return for an IOC or a bid's
                // unspent escrow, at its own index.
                let out = match self.kind {
                    TemplateId::KobIfdPair => {
                        // An entry's custody of S keeps a rest when it holds something after the
                        // fill.
                        if self.sells_a() {
                            u32::from(left > 0)
                        } else {
                            u32::from(self.custody - q > 0)
                        }
                    }
                    _ => u32::from(self.custody - q > 0),
                };
                (u32::from(self.custody > 0), out)
            }
        }
    }
    /// Token inputs and outputs of the Buy leg's token at a Buy quantity of `q`.
    ///
    /// They are the delivery or the exit's custody, a merge's entry custodies, and a sell-first
    /// entry's B prefund in and out.
    pub fn buy_slots(&self, q: i64) -> (u32, u32) {
        if q <= 0 {
            return (0, 0);
        }
        match (&self.merge, self.shape, &self.state) {
            // An ask exit (T = B): the maker's delivery, and the entry's B custody in and out.
            (Some(m), PairShape::AskMin, _) => (m.slots.2, 1 + m.slots.3),
            // A bid exit (T = A): no delivery to the maker, and the entry's A custody in and out.
            (Some(m), _, _) => (m.slots.0, m.slots.1),
            // A sell-first entry: the exit's custody of B, and its own B prefund in and out at its
            // index.
            (None, PairShape::AskExact, AnyState::KobIfdPair(s)) => {
                let n = self.n_of_buy(q);
                let cont = n < s.amount_left || s.rpt_amount > 0;
                let used = if cont { s.pre_of(n).unwrap_or(s.custody) } else { s.custody };
                let pre_in = u32::from(s.custody > 0);
                (pre_in, 1 + u32::from(pre_in == 1 && s.custody - used > 0))
            }
            _ => (0, 1),
        }
    }
    /// Whether the evidence arms this order's stop, by its own covenant rule.
    pub fn arms(&self, ev: PairEvidence) -> bool {
        need_arms(&self.state, ev)
    }
}

/// Whether `ev` arms the pair stop of `state`, a `KobCondPair` stop leg or `KobIfdPair` stop entry.
pub fn need_arms(state: &AnyState, ev: PairEvidence) -> bool {
    match state {
        AnyState::KobCondPair(s) => s.arms(ev) == Some(true),
        AnyState::KobIfdPair(s) => s.arms(ev) == Some(true),
        _ => false,
    }
}

/// A KAS-book fill of the allocation that may be mode-0 evidence.
#[derive(Clone, Copy, Debug)]
pub struct KasTouch {
    /// Index of the fill's candidate.
    pub ix: usize,
    /// Covenant id of the fill's token.
    pub token: [u8; 32],
    /// The extension commitment of the fill's book. A KAS book is one commitment of its token.
    pub ext: [u8; 32],
    /// What the fill offers as evidence.
    pub src: TouchSrc,
    /// Base units of the fill.
    pub n: i64,
}

/// A resting `KobPair` fill of the allocation that may be mode-1 evidence.
#[derive(Clone, Copy, Debug)]
pub struct PairFillTouch {
    /// Index of the order's primary leg.
    pub ix: usize,
    /// What the fill offers as evidence.
    pub src: PairTouch,
    /// Base units of A of the fill.
    pub n: i64,
}

/// Evidence of a pair stop, as candidate indices.
///
/// The first is the leg of A, or the `KobPair` in mode 1. The second is the leg of B in mode 0.
pub type PairEv = (usize, Option<usize>);

/// The evidence candidates of `need`, with the evidence each offers, in allocation order.
///
/// Mode 1 comes first: a resting `KobPair` of the pair whose side is that of `ask_a`. Then come
/// every qualifying (A, B) pair of KAS-book fills, an A fill on the `ask_a` side and a B fill on
/// the other.
pub fn evidence_options(
    need: &PairNeed,
    ask_a: bool,
    kas: &[KasTouch],
    pairs: &[PairFillTouch],
    t: i64,
) -> Vec<(PairEv, PairEvidence)> {
    let mut out = vec![];
    for p in pairs {
        let s = &p.src;
        // The same tokens and the same extension commitments. Evidence of another commitment of
        // the same covenant id is never used.
        if s.ask == ask_a
            && (s.a, s.b, s.a_ext, s.b_ext, s.a_scale, s.b_scale)
                == (need.a, need.b, need.a_ext, need.b_ext, need.a_scale, need.b_scale)
            && p.n >= need.min_touch
            && s.exposed_since.saturating_add(need.min_rest) <= t
        {
            out.push(((p.ix, None), PairEvidence::Pair { price: s.price }));
        }
    }
    let Some(mb) = need.min_touch_b else { return out };
    let side_a = if ask_a { SIDE_ASK } else { SIDE_BID };
    for x in kas {
        if x.token != need.a
            || x.ext != need.a_ext
            || x.src.side != side_a
            || x.src.scale != need.a_scale
            || !touch_ok(&x.src, x.n, need.min_touch, need.min_rest, t)
        {
            continue;
        }
        for y in kas {
            if y.token != need.b
                || y.ext != need.b_ext
                || y.src.side == side_a
                || y.src.scale != need.b_scale
                || y.src.price <= 0
                || !touch_ok(&y.src, y.n, mb, need.min_rest, t)
            {
                continue;
            }
            out.push(((x.ix, Some(y.ix)), PairEvidence::KasBooks { a: x.src.price, b: y.src.price }));
        }
    }
    out
}

/// The first evidence that arms the stop of `need`.
///
/// Mode 1 is preferred, because it makes the smaller transaction.
pub fn arm_evidence(need: &PairNeed, kas: &[KasTouch], pairs: &[PairFillTouch], t: i64) -> Option<PairEv> {
    evidence_options(need, need.ask_a, kas, pairs, t).into_iter().find(|(_, ev)| need_arms(&need.state, *ev)).map(|(e, _)| e)
}

/// The evidence that ratchets a trailing pair stop the most, and the steps it justifies.
///
/// It is evidence of the opposite side. On a tie the first one wins.
pub fn trail_evidence(need: &PairNeed, kas: &[KasTouch], pairs: &[PairFillTouch], t: i64) -> Option<(PairEv, i64)> {
    let AnyState::KobCondPair(s) = &need.state else { return None };
    let mut best: Option<(PairEv, i64)> = None;
    for (e, ev) in evidence_options(need, !need.ask_a, kas, pairs, t) {
        if let Some(k) = s.trail_k(ev) {
            if best.is_none_or(|(_, bk)| k > bk) {
                best = Some((e, k));
            }
        }
    }
    best
}

/// The extension commitment of the outputs of a token that the order makes. KRON has none.
fn ext_or_zero(t: &PairToken, ext: Option<[u8; 32]>) -> [u8; 32] {
    if t.family_of() == Some(Family::Kron) {
        [0; 32]
    } else {
        ext.unwrap_or([0; 32])
    }
}

/// The markets of A and B of a pair order.
///
/// A market is the token, its program and the extension commitment its outputs carry. The
/// commitment is a custody's own, else the state's.
pub fn markets(o: &ListedOrder) -> Option<(Market, Market)> {
    let t = o.order.state.pair_tokens()?;
    let ext_of = |tok: &PairToken| -> Option<[u8; 32]> {
        if tok.family_of() == Some(Family::Kron) {
            return Some([0; 32]);
        }
        if let Some(c) = o.custody_of(&tok.cov_id) {
            return Some(c.state.extension());
        }
        tok.ext.map(|e| ext_or_zero(tok, Some(e)))
    };
    let m = |tok: &PairToken| -> Option<Market> {
        Some(Market { family: tok.family_of()?, token: tok.cov_id, template: tok.tpl_hash, extension: ext_of(tok)? })
    };
    Some((m(&t.a)?, m(&t.b)?))
}

/// Bytes of a pair order's fill.
///
/// They count its input and redeem script, its custodies, its outputs, and an entry's exit
/// genesis.
fn order_bytes(kind: TemplateId, s_prog: Option<TemplateId>, t_prog: Option<TemplateId>, custodies: u64, merge_bytes: u64) -> u64 {
    let mut b = INPUT_BYTES + redeem_len(kind) + custodies * token_input_bytes(s_prog) + 3 * OUTPUT_BYTES + 2 * NEXT_STATE_BYTES;
    if kind == TemplateId::KobIfdPair {
        let x = template(TemplateId::KobCondPair);
        b += (x.prefix.len() + x.suffix.len()) as u64;
        // A sell-first entry's B prefund custody.
        b += token_input_bytes(t_prog) / 2;
    }
    b + merge_bytes
}

/// The token program of a pair token, a supported program of the family its state names.
fn program(m: &Market) -> Option<TemplateId> {
    kob_protocol::artifacts::token_template_by_hash(&m.template).filter(|t| t.family == m.family).map(|t| t.id)
}

/// Whether the pair order may be filled at `t` at all.
///
/// It must be listed with its exact custodies, accepted at or before `t`, not excluded and not
/// due.
fn eligible(o: &ListedOrder, t: i64, cx: &CandCtx) -> bool {
    let id = o.id();
    o.order.utxo.covenant_id.is_some()
        && !cx.excluded.contains(&id)
        && !cx.unaccepted.contains(&id)
        && o.custody_ok()
        && o.order.utxo.block_daa_score != u64::MAX
        && o.utxo_daa() as i64 <= t
        && !excluded_by_expiry(o, t, cx.utc)
}

/// Whether a booked exit may merge this repeating pair entry.
///
/// It must be listed, accepted at or before `t`, not spent by a pending transaction, and hold its
/// exact custodies.
fn merge_entry_ok(e: &ListedOrder, cx: &CandCtx) -> bool {
    !cx.excluded.contains(&e.id())
        && e.custody_ok()
        && e.order.utxo.block_daa_score != u64::MAX
        && !cx.unaccepted.contains(&e.id())
        && e.order.utxo.block_daa_score as i64 <= cx.t
}

/// The normalised pair participants of one listed order at `t`, without their route quotes.
///
/// A `KobCondPair` has one per leg it can fill.
pub fn infos(o: &ListedOrder, cx: &CandCtx) -> Vec<PairInfo> {
    let t = cx.t;
    if !eligible(o, t, cx) || crate::sanity::check(&o.order.state).is_err() {
        return vec![];
    }
    let Some((ma, mb)) = markets(o) else { return vec![] };
    if ma.token == mb.token || token_limits(ma.family, &ma.template).is_none() || token_limits(mb.family, &mb.template).is_none() {
        return vec![];
    }
    let id = o.id();
    let udaa = o.utxo_daa() as i64;
    // The tick its auction is quoted at. The engine's earliest-tick search may set an earlier one
    // (§3.3). Eligibility is judged at t.
    let ta = cx.pair_tick(&id);
    let v = o.order.utxo.amount.min(i64::MAX as u64) as i64;
    let base = |kind, shape, state: AnyState| PairInfo {
        id,
        kind,
        leg: 0,
        shape,
        state,
        price: 0,
        rank_price: 0,
        a_scale: 0,
        tip: 0,
        a: ma,
        b: mb,
        cap: 0,
        part_cap: 0,
        amount_left: 0,
        min_fill: 0,
        fok: false,
        ioc: false,
        custody: 0,
        route: false,
        relaxing: false,
        need: None,
        touch: None,
        merge: None,
        value: v,
    };
    let mut out = vec![];
    match &o.order.state {
        AnyState::KobPair(s) => {
            if t < s.active_from || s.amount_left <= 0 || s.custody <= 0 {
                return out;
            }
            if s.interval > 0 && t < udaa.saturating_add(s.interval) {
                return out; // TWAP / DCA slice: the UTXO must be `interval` DAA old (CSV)
            }
            if s.slope != 0 && s.origin(udaa).is_none_or(|x| ta < x) {
                return out; // never quoted before its auction starts (the covenant requires t >= origin)
            }
            let Some(p) = s.price_at(ta, udaa) else { return out };
            if p <= 0 {
                return out;
            }
            let shape = if s.is_ask() { PairShape::AskMin } else { PairShape::Bid };
            let mut x = base(TemplateId::KobPair, shape, o.order.state.clone());
            x.price = p;
            x.rank_price = p;
            x.a_scale = s.a_scale();
            x.tip = s.tip;
            x.amount_left = s.amount_left;
            x.min_fill = s.min_fill;
            x.custody = s.custody;
            x.fok = s.tif == TIF_FOK;
            x.ioc = s.tif == TIF_IOC;
            // The covenant's affordability. A rest keeps something in the custody and funds its
            // carrier and tip. The KAS outputs the fill leaves are not dust. They are the
            // continuation of a rest, else the maker's delivery.
            let cv = o.custody.as_ref().map(|c| c.utxo.amount.min(i64::MAX as u64) as i64).unwrap_or(0);
            let fits = |n: i64| {
                s.fill(n, p, v).is_ok_and(|f| {
                    let kas = if f.rest {
                        v - s.delivery_carrier - f.tip_kas
                    } else if f.out_amount > 0 {
                        v - f.tip_kas
                    } else {
                        v.saturating_add(cv) - f.tip_kas
                    };
                    kas >= DUST
                }) && (s.max_fill == 0 || n <= s.max_fill)
            };
            (x.cap, x.part_cap) = cap_of(s.amount_left, s.min_fill, fits);
            if x.fok && x.cap < s.amount_left {
                return out;
            }
            x.relaxing = s.slope != 0 && s.tif != TIF_GTC && s.price_at(ta.saturating_add(1), udaa).is_some_and(|q| q != p);
            if s.slope == 0 {
                let cdaa = o.custody.as_ref().map(|c| c.utxo.block_daa_score).unwrap_or(u64::MAX);
                if cdaa != u64::MAX && cdaa as i64 <= t {
                    let tk = s.tokens();
                    x.touch = Some(PairTouch {
                        ask: s.is_ask(),
                        a: tk.a.cov_id,
                        b: tk.b.cov_id,
                        a_ext: ma.extension,
                        b_ext: mb.extension,
                        a_scale: tk.a.scale,
                        b_scale: tk.b.scale,
                        price: s.price,
                        exposed_since: udaa.saturating_add(s.interval).max(s.active_from).max(cdaa as i64),
                    });
                }
            }
            if x.cap > 0 {
                out.push(x);
            }
        }
        AnyState::KobCondPair(s) => {
            if t < s.active_from || s.amount_left <= 0 || s.custody <= 0 {
                return out;
            }
            let shape = if s.is_ask() { PairShape::AskMin } else { PairShape::Bid };
            let tk = s.tokens();
            let cv = o.custody.as_ref().map(|c| c.utxo.amount.min(i64::MAX as u64) as i64).unwrap_or(0);
            let mk = |leg: u8, lp: i64, merge: Option<MergeOf>| -> Option<PairInfo> {
                if lp <= 0 {
                    return None;
                }
                let mut x = base(TemplateId::KobCondPair, shape, o.order.state.clone());
                x.leg = leg;
                x.price = lp;
                x.rank_price = lp;
                x.a_scale = s.a_scale();
                x.tip = s.tip;
                x.amount_left = s.amount_left;
                x.min_fill = s.min_fill;
                x.custody = s.custody;
                x.merge = merge;
                if x.merge.is_some() {
                    x.rank_price = if s.is_ask() { lp.max(s.rpt_price) } else { lp.min(s.rpt_price) };
                }
                x.cap = s.amount_left;
                if x.merge.is_some() {
                    x.cap = x.cap.min(MERGE_SHIFT - 1);
                }
                let tip = |n: i64| s.tip_kas(n);
                let ok = |n: i64| -> bool {
                    let (Some(so), Some(tp)) = (x.sell_qty(n), tip(n)) else { return false };
                    let left = s.amount_left - n;
                    if !s.fill_ok(n) {
                        return false;
                    }
                    if x.merge.is_some() {
                        let Some(tq) = x.buy_qty(n) else { return false };
                        if s.is_ask() {
                            // The custody releases n, and a partial fill keeps its rest.
                            let out = s.custody - n;
                            return out >= 0 && (left == 0 || out > 0) && tq > 0 && (left == 0 || v - s.delivery_carrier - tp >= DUST);
                        }
                        let (Some(pr), Some(back)) = (s.rpt_proceeds(n), s.rpt_back(n)) else { return false };
                        let out = if left > 0 { s.custody - pr - back } else { 0 };
                        let d_out = if left > 0 { pr - so } else { s.custody - back - so };
                        return so >= 0
                            && out >= 0
                            && (left == 0 || out > 0)
                            && d_out > 0
                            && (left == 0 || v - s.delivery_carrier - tp >= DUST);
                    }
                    let out = s.custody - if s.is_ask() { n } else { so };
                    // The continuation of a partial fill is not dust. Else the maker's delivery is
                    // not, with the custody's KAS when the custody closes.
                    let kas = if left > 0 {
                        v - s.delivery_carrier - tp
                    } else if out > 0 {
                        v - tp
                    } else {
                        v.saturating_add(cv) - tp
                    };
                    out >= 0 && (left == 0 || out > 0) && kas >= DUST
                };
                (x.cap, x.part_cap) = cap_of(x.cap, s.min_fill, ok);
                (x.cap > 0).then_some(x)
            };
            // Take-profit or limit leg.
            if s.tp_price > 0 {
                let mut merge = None;
                let mut ok = true;
                if s.is_booked() && t < s.rpt_until {
                    ok = false;
                    if let Some(e) = cx.by_id.get(&s.parent) {
                        if let (true, AnyState::KobIfdPair(es)) = (merge_entry_ok(e, cx), &e.order.state) {
                            let buy = es.is_buy_first();
                            if buy == s.is_ask() && es.books_exit(s.parent, s) {
                                let a_has = !buy && es.amount_left > 0;
                                let b_has = es.custody > 0;
                                let new_c = i64::from(!buy && !a_has) + i64::from(!b_has && (buy || es.prefund > 0));
                                let slots = if buy {
                                    (0, 0, u32::from(b_has), 1)
                                } else {
                                    (u32::from(a_has), 1, u32::from(b_has), u32::from(b_has || es.prefund > 0))
                                };
                                let ev = e.order.utxo.amount.min(i64::MAX as u64) as i64;
                                if ev - new_c * es.exit_carrier > 0 {
                                    merge = Some(MergeOf {
                                        entry: s.parent,
                                        slots,
                                        entry_value: ev,
                                        exit_carrier: es.exit_carrier,
                                        new_custodies: new_c,
                                    });
                                    ok = true;
                                }
                            }
                        }
                    }
                }
                if ok {
                    if let Some(x) = mk(0, s.tp_price, merge) {
                        out.push(x);
                    }
                }
            }
            // Stop leg.
            if s.stop_price > 0 && s.stop_price <= MAX_STOP_PRICE && (0..=10_000).contains(&s.slip_bps) {
                if s.armed != 0 {
                    let origin = armed_origin(s.armed, udaa).unwrap_or(i64::MAX);
                    if !(s.band_daa > 0 && ta < origin) {
                        if let Some(lp) = s.stop_at(false, ta, udaa) {
                            if let Some(x) = mk(1, lp, None) {
                                out.push(x);
                            }
                        }
                    }
                } else if let Some(lp) = s.stop_at(true, t, udaa) {
                    if let Some(mut x) = mk(1, lp, None) {
                        x.need = Some(PairNeed {
                            ask_a: s.is_ask(),
                            a: tk.a.cov_id,
                            b: tk.b.cov_id,
                            a_ext: ma.extension,
                            b_ext: mb.extension,
                            a_scale: tk.a.scale,
                            b_scale: tk.b.scale,
                            min_touch: s.min_touch,
                            min_touch_b: s.min_touch_b(),
                            min_rest: s.min_rest_daa,
                            state: o.order.state.clone(),
                        });
                        out.push(x);
                    }
                }
            }
        }
        AnyState::KobIfdPair(s) => {
            if t < s.active_from || s.amount_left <= 0 {
                return out; // an empty repeating entry waits for its exits
            }
            let buy = s.is_buy_first();
            let mut trigger = false;
            let mut t_arg = 0;
            if s.entry_stop > 0 {
                if (buy && s.entry_stop > s.price) || (!buy && s.entry_stop < s.price) {
                    return out;
                }
                if s.armed == 0 {
                    trigger = true;
                } else if s.band_daa > 0 {
                    let origin = armed_origin(s.armed, udaa).unwrap_or(i64::MAX);
                    if ta < origin {
                        return out;
                    }
                    t_arg = ta;
                }
            }
            if s.rpt_amount > 0 && t_arg == 0 {
                t_arg = t;
            }
            let Some(p) = s.price_at(trigger, t_arg, udaa) else { return out };
            if p <= 0 {
                return out;
            }
            let tk = s.tokens();
            let shape = if buy { PairShape::Bid } else { PairShape::AskExact };
            let mut x = base(TemplateId::KobIfdPair, shape, o.order.state.clone());
            x.price = p;
            x.rank_price = p;
            x.a_scale = s.a_scale;
            x.tip = s.tip;
            x.amount_left = s.amount_left;
            x.min_fill = s.min_fill;
            x.custody = if buy { s.custody } else { s.amount_left };
            let a_car = if buy { 0 } else { o.custody_of(&tk.a.cov_id).map(|c| c.utxo.amount as i64).unwrap_or(0) };
            let b_car = o.custody_of(&tk.b.cov_id).map(|c| c.utxo.amount as i64).unwrap_or(0);
            let ok = |n: i64| -> bool {
                if !s.fill_ok(n) || (s.rpt_amount > n && n >= MERGE_SHIFT) {
                    return false;
                }
                let Some(tip) = s.tip_kas(n) else { return false };
                let cont = s.amount_left - n > 0 || s.rpt_amount > 0;
                let b_new = if buy {
                    match s.spend(n, p) {
                        Some(sp) if sp <= s.custody => s.custody - sp,
                        _ => return false,
                    }
                } else {
                    if s.proceeds(n, p).is_none() {
                        return false;
                    }
                    let used = if cont { s.pre_of(n) } else { Some(s.custody) };
                    match used {
                        Some(u) if u <= s.custody => s.custody - u,
                        _ => return false,
                    }
                };
                let a_new = if buy { 0 } else { s.amount_left - n };
                if cont {
                    let mut keep = v - s.delivery_carrier - s.exit_carrier - tip;
                    if !buy && a_new == 0 {
                        keep += a_car;
                    }
                    if s.custody > 0 && b_new == 0 {
                        keep += b_car;
                    }
                    keep >= DUST
                } else {
                    let mut left = v + a_car - s.delivery_carrier - tip;
                    if b_new == 0 {
                        left += b_car;
                    }
                    left >= DUST
                }
            };
            let mut cap = s.amount_left;
            if s.rpt_amount > 0 {
                cap = cap.min(MERGE_SHIFT - 1);
            }
            if let Some(m) = rpt_fill_max(s.rpt_amount, s.min_fill) {
                // While a minimum fill of re-arms is left, a fill takes at most the re-arms left,
                // and it is booked.
                cap = cap.min(m);
            }
            (x.cap, x.part_cap) = cap_of(cap, s.min_fill, ok);
            if trigger {
                x.need = Some(PairNeed {
                    ask_a: !buy,
                    a: tk.a.cov_id,
                    b: tk.b.cov_id,
                    a_ext: ma.extension,
                    b_ext: mb.extension,
                    a_scale: tk.a.scale,
                    b_scale: tk.b.scale,
                    min_touch: s.min_touch,
                    min_touch_b: s.min_touch_b(),
                    min_rest: s.min_rest_daa,
                    state: o.order.state.clone(),
                });
            }
            if x.cap > 0 {
                out.push(x);
            }
        }
        _ => {}
    }
    out
}

/// The fills in `1..=max` that the covenant accepts (`ok`), as `(cap, part_cap)`.
///
/// `cap` is the whole `max` if possible, else the largest partial fill. `part_cap` is the largest
/// partial fill, below `max`. Each is 0 when there is none.
///
/// Every affordability rule of a partial fill is monotone in n. A partial fill below the minimum
/// fill is refused too, and the bisection must not read that as the threshold. The fill of
/// everything left is not a partial fill. It needs no continuation, so it can be possible when no
/// partial fill is, once the order UTXO's prefunded delivery carriers are used up.
fn cap_of(max: i64, min_fill: i64, ok: impl Fn(i64) -> bool) -> (i64, i64) {
    if max <= 0 {
        return (0, 0);
    }
    let part = if max > 1 {
        let n = largest_fit(max - 1, |n| n < min_fill || ok(n));
        if n > 0 && ok(n) {
            n
        } else {
            0
        }
    } else {
        0
    };
    if ok(max) {
        (max, part)
    } else {
        (part, part)
    }
}

/// The best plain resting KAS quotes of every market, all-in per base unit.
///
/// They are the lowest ask and the highest bid.
pub fn best_quotes(direct: &[Cand]) -> BTreeMap<(Market, Side), (i128, i128)> {
    let mut m: BTreeMap<(Market, Side), (i128, i128)> = BTreeMap::new();
    for c in direct {
        if !c.is_plain() || c.trigger.is_some() {
            continue;
        }
        let p = c.per_base();
        let e = m.entry((c.book.market(), c.side)).or_insert(p);
        let better = match c.side {
            Side::Ask => p.0 * e.1 < e.0 * p.1,
            Side::Bid => p.0 * e.1 > e.0 * p.1,
        };
        if better {
            *e = p;
        }
    }
    m
}

fn ceil_div(a: i128, b: i128) -> i128 {
    (a + b - 1).div_euclid(b)
}

/// A pair participant before its legs.
///
/// It holds the order's index in the slice, the order, its normalised fill and its route quotes.
/// The quotes are the KAS cost of the T for one whole A and what the S of one whole A fetches.
type Participant<'a> = (usize, &'a ListedOrder, PairInfo, Option<(i64, i64)>);

/// The two legs (Sell, Buy) of every pair participant among `orders` at `t`.
///
/// The legs are ranked at their implied quotes from the best plain KAS quotes of `direct`. A
/// participant without both KAS books is not routed, and its legs only net. Pure and
/// deterministic.
pub fn legs(orders: &[&ListedOrder], direct: &[Cand], cx: &CandCtx) -> Vec<(Cand, Cand)> {
    let best = best_quotes(direct);
    // What the plain KAS books can take at all: the base units of the plain resting bids and asks
    // of every market.
    let mut depth: BTreeMap<(Market, Side), i128> = BTreeMap::new();
    for c in direct.iter().filter(|c| c.is_plain() && c.trigger.is_none()) {
        *depth.entry((c.book.market(), c.side)).or_default() += c.cap.max(0) as i128;
    }
    // Every participant with its route quotes. The T for one whole A is costed at the best plain
    // ask of T, rounded up. The S of one whole A is valued at the best plain bid of S, rounded
    // down. They only rank the legs (§3.5).
    let mut parts: Vec<Participant> = vec![];
    for (idx, &o) in orders.iter().enumerate() {
        for info in infos(o, cx) {
            let (s_whole, t_whole) = info.whole();
            if s_whole <= 0 || t_whole <= 0 {
                continue;
            }
            let (sm, tm) = (info.s_market(), info.t_market());
            let quotes = match (best.get(&(tm, Side::Ask)).copied(), best.get(&(sm, Side::Bid)).copied()) {
                (Some((an, ad)), Some((bn, bd))) => {
                    let cost = (t_whole as i128).checked_mul(an).map(|c| ceil_div(c, ad));
                    let fetch = (s_whole as i128).checked_mul(bn).map(|f| f.div_euclid(bd));
                    match (cost.and_then(|c| i64::try_from(c).ok()), fetch.and_then(|f| i64::try_from(f).ok())) {
                        (Some(c), Some(f)) if f > 0 && c.saturating_sub(info.tip) > 0 => Some((c, f)),
                        _ => None,
                    }
                }
                _ => None,
            };
            parts.push((idx, o, info, quotes));
        }
    }
    // The route takes no more pair orders per direction than the KAS books of both its tokens can
    // fill. The most profitable go first, by the KAS margin of one whole A at the top of both
    // books, then by the tip. An order is routed while the S sold before it is below the plain
    // bids of S and the T bought before it is below the plain asks of T. The others can still
    // net. So a large or hostile pair book costs the planner what the KAS books can absorb, not
    // its own size.
    let mut by_dir: BTreeMap<(Market, Market), Vec<usize>> = BTreeMap::new();
    for (k, (_, _, info, q)) in parts.iter().enumerate() {
        if q.is_some() && info.need.is_none() {
            by_dir.entry((info.s_market(), info.t_market())).or_default().push(k);
        }
    }
    let mut routed = vec![true; parts.len()];
    for ((sm, tm), mut ks) in by_dir {
        let key = |k: usize| -> (i128, i128) {
            let (_, _, info, q) = &parts[k];
            let (c, f) = q.expect("quoted");
            ((f as i128 - c as i128 + info.tip as i128) * 1_000_000 / info.whole().0.max(1) as i128, info.tip as i128)
        };
        ks.sort_by(|a, b| key(*b).cmp(&key(*a)).then(a.cmp(b)));
        let (cap_s, cap_t) = (depth.get(&(sm, Side::Bid)).copied().unwrap_or(0), depth.get(&(tm, Side::Ask)).copied().unwrap_or(0));
        let (mut sold, mut bought) = (0i128, 0i128);
        for k in ks {
            let info = &parts[k].2;
            if sold >= cap_s || bought >= cap_t {
                routed[k] = false;
                continue;
            }
            sold += info.sell_qty(info.cap).unwrap_or(0) as i128;
            bought += info.buy_qty(info.cap).unwrap_or(0) as i128;
        }
    }
    let mut out = vec![];
    for (k, (idx, o, mut info, quotes)) in parts.into_iter().enumerate() {
        {
            let (s_whole, t_whole) = info.whole();
            let quotes = quotes.filter(|_| routed[k]);
            let (sm, tm) = (info.s_market(), info.t_market());
            info.route = quotes.is_some();
            let (quote_s, quote_t) = quotes.unwrap_or((0, 0));
            let s_prog = program(&sm);
            let t_prog = program(&tm);
            let merge_bytes = info.merge.as_ref().map_or(0, |m| {
                INPUT_BYTES
                    + redeem_len(TemplateId::KobIfdPair)
                    + (m.slots.0 + m.slots.2) as u64 * token_input_bytes(s_prog)
                    + (m.slots.1 + m.slots.3) as u64 * (OUTPUT_BYTES + NEXT_STATE_BYTES)
                    + OUTPUT_BYTES
            });
            let bytes = order_bytes(info.kind, s_prog, t_prog, u64::from(info.custody > 0), merge_bytes);
            let (Some(sell_cap), Some(buy_cap)) = (info.sell_qty(info.cap), info.buy_qty(info.cap)) else { continue };
            let class = match (&info.need, info.kind, info.leg, &info.state) {
                (Some(_), _, _, _) => Class::Triggered,
                (None, TemplateId::KobCondPair, 1, _) => Class::Triggered,
                (None, TemplateId::KobIfdPair, _, AnyState::KobIfdPair(s)) if s.entry_stop > 0 => Class::Triggered,
                _ if info.fok || info.ioc => Class::Immediate,
                _ => Class::Resting,
            };
            let parent = match &info.state {
                AnyState::KobCondPair(s) if s.is_booked() => Some(s.parent),
                _ => None,
            };
            let merge = info.merge.as_ref().map(|m| m.entry);
            let info = Arc::new(info);
            let book =
                |m: Market| BookKey { family: m.family, token: m.token, template: m.template, extension: m.extension, scale: 0 };
            let sell = Cand {
                order: idx,
                id: info.id,
                kind: info.kind,
                side: Side::Ask,
                leg: info.leg,
                class,
                quote: quote_s,
                tip: info.tip,
                scale: s_whole,
                age: o.utxo_daa(),
                seen: o.seen_daa,
                cap: sell_cap,
                min_fill: info.sell_qty(info.min_fill.min(info.amount_left)).unwrap_or(1).max(1),
                rest_from: info.sell_qty(info.amount_left).unwrap_or(i64::MAX),
                part_cap: i64::MAX,
                left: info.custody,
                fok: None,
                ioc: info.ioc,
                trigger: None,
                merge,
                merge_custody: false,
                parent,
                touch: None,
                chain_safe: false,
                bytes,
                book: book(sm),
                pair: Some(PairLeg { role: PairRole::Sell, info: info.clone() }),
            };
            let buy = Cand {
                side: Side::Bid,
                quote: quote_t,
                tip: 0,
                scale: t_whole,
                cap: buy_cap,
                min_fill: 1,
                rest_from: buy_cap,
                part_cap: i64::MAX,
                left: buy_cap,
                book: book(tm),
                pair: Some(PairLeg { role: PairRole::Buy, info }),
                ..sell.clone()
            };
            out.push((sell, buy));
        }
    }
    out
}

/// A valid `KobPair` state, the template's compiled one, for the surplus offers.
///
/// Surplus offers stand for no order.
fn placeholder_state() -> AnyState {
    static S: std::sync::OnceLock<AnyState> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let t = template(TemplateId::KobPair);
        AnyState::decode(TemplateId::KobPair, &t.contract().compiled.bytecode[1..1 + t.state_len]).expect("compiled KobPair state")
    })
    .clone()
}

/// The netting surplus offer of a token.
///
/// There is one per market a netting group trades. Every allocation sets its cap.
pub fn surplus_cand(m: Market, marker: CovId) -> Cand {
    let info = PairInfo {
        id: marker,
        kind: TemplateId::KobPair,
        leg: 0,
        shape: PairShape::AskMin,
        state: placeholder_state(),
        price: 0,
        rank_price: 0,
        a_scale: 1,
        tip: 0,
        a: m,
        b: m,
        cap: 0,
        part_cap: 0,
        amount_left: 0,
        min_fill: 0,
        fok: false,
        ioc: false,
        custody: 0,
        route: true,
        relaxing: false,
        need: None,
        touch: None,
        merge: None,
        value: 0,
    };
    Cand {
        order: usize::MAX,
        id: marker,
        kind: TemplateId::KobPair,
        side: Side::Ask,
        leg: 0,
        class: Class::Resting,
        quote: 0,
        tip: 0,
        scale: 1,
        age: 0,
        seen: 0,
        cap: 0,
        min_fill: 1,
        rest_from: i64::MAX,
        part_cap: i64::MAX,
        left: i64::MAX,
        fok: None,
        ioc: false,
        trigger: None,
        merge: None,
        merge_custody: false,
        parent: None,
        touch: None,
        chain_safe: false,
        bytes: 0,
        book: BookKey { family: m.family, token: m.token, template: m.template, extension: m.extension, scale: 0 },
        pair: Some(PairLeg { role: PairRole::Surplus, info: Arc::new(info) }),
    }
}

/// Pair stops of the view that a batch may arm or ratchet with an `update`, next to its evidence.
///
/// These are pair conditionals and pair stop entries. Each is accepted with a known DAA score at
/// or before `t`, active, not expired, not excluded, and holds its exact custodies.
pub fn updatable(orders: &[&ListedOrder], cx: &CandCtx) -> Vec<UpCand> {
    let t = cx.t;
    let mut out = vec![];
    for &o in orders {
        let id = o.id();
        if o.order.utxo.covenant_id.is_none() || cx.excluded.contains(&id) || cx.unaccepted.contains(&id) || !o.custody_ok() {
            continue;
        }
        let udaa = o.utxo_daa();
        if udaa == u64::MAX || udaa as i64 > t || excluded_by_expiry(o, t, cx.utc) || crate::sanity::check(&o.order.state).is_err() {
            continue;
        }
        let Some((ma, mb)) = markets(o) else { continue };
        let old_enough = |wait: i64| t - udaa as i64 >= wait.max(0);
        let (need, trail, active_from, parent) = match &o.order.state {
            AnyState::KobCondPair(s)
                if s.armed == 0 && s.stop_price > 0 && s.amount_left > 0 && (0..=10_000).contains(&s.slip_bps) =>
            {
                let tk = s.tokens();
                let need = PairNeed {
                    ask_a: s.is_ask(),
                    a: tk.a.cov_id,
                    b: tk.b.cov_id,
                    a_ext: ma.extension,
                    b_ext: mb.extension,
                    a_scale: tk.a.scale,
                    b_scale: tk.b.scale,
                    min_touch: s.min_touch,
                    min_touch_b: s.min_touch_b(),
                    min_rest: s.min_rest_daa,
                    state: o.order.state.clone(),
                };
                (need, s.trail_step > 0 && old_enough(s.trail_wait), s.active_from, s.is_booked().then_some(s.parent))
            }
            AnyState::KobIfdPair(s) if s.armed == 0 && s.entry_stop > 0 && s.amount_left > 0 => {
                let buy = s.is_buy_first();
                if (buy && s.entry_stop > s.price) || (!buy && s.entry_stop < s.price) {
                    continue;
                }
                let tk = s.tokens();
                let need = PairNeed {
                    ask_a: !buy,
                    a: tk.a.cov_id,
                    b: tk.b.cov_id,
                    a_ext: ma.extension,
                    b_ext: mb.extension,
                    a_scale: tk.a.scale,
                    b_scale: tk.b.scale,
                    min_touch: s.min_touch,
                    min_touch_b: s.min_touch_b(),
                    min_rest: s.min_rest_daa,
                    state: o.order.state.clone(),
                };
                (need, false, s.active_from, None)
            }
            _ => continue,
        };
        if t < active_from {
            continue;
        }
        let Some(tip) = o.order.state.keeper_tip() else { continue };
        // The continuation must clear the covenant-output bound, see `candidate::updatable`.
        if o.order.utxo.amount.min(i64::MAX as u64) as i64 - tip < COV_OUTPUT_MIN {
            continue;
        }
        let fee_floor = program(&ma)
            .and_then(|p| kob_protocol::defaults::tips(p).ok())
            .map(|x| x.update_fee.min(i64::MAX as u64) as i64)
            .unwrap_or(0);
        out.push(UpCand {
            id,
            book: BookKey { family: ma.family, token: ma.token, template: ma.template, extension: ma.extension, scale: 0 },
            outpoint: super::book::outpoint(&o.order.utxo),
            arm: None,
            trail: None,
            pair: Some(Arc::new(PairUp { need, trail })),
            parent,
            tip,
            value: o.order.utxo.amount.min(i64::MAX as u64) as i64,
            bytes: INPUT_BYTES + redeem_len(o.order.state.template_id()) + OUTPUT_BYTES,
            fee_floor,
        });
    }
    out
}

/// A pair order that an update arms or ratchets.
///
/// It holds what the order reads from its evidence, and whether it may trail now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairUp {
    /// What the order reads from its trigger evidence.
    pub need: PairNeed,
    /// A trailing `KobCondPair` whose UTXO is `trailWait` old at `t`.
    pub trail: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(shape: PairShape, price: i64, a_scale: i64, cap: i64) -> PairInfo {
        let m = Market { family: Family::Kcc20, token: [1; 32], template: [2; 32], extension: [3; 32] };
        let mut x = match surplus_cand(m, [9; 32]).pair {
            Some(l) => (*l.info).clone(),
            None => unreachable!(),
        };
        x.shape = shape;
        x.price = price;
        x.rank_price = price;
        x.a_scale = a_scale;
        x.cap = cap;
        x.part_cap = cap;
        x.amount_left = cap;
        x.min_fill = 1;
        x.b = Market { token: [4; 32], ..m };
        x
    }

    /// The inverses are exact.
    ///
    /// They give the largest n whose S release, or receipt, stays within q, for every shape, scale
    /// and price.
    #[test]
    fn the_inverse_quantities_are_the_largest_fills_within() {
        let mut seed: u64 = 7;
        let mut next = |m: u64| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % m.max(1)
        };
        for _ in 0..2_000 {
            let scale = 10i64.pow(next(10) as u32);
            let price = 1 + next(5_000_000) as i64;
            let cap = 1 + next(10_000_000) as i64;
            for shape in [PairShape::AskMin, PairShape::AskExact, PairShape::Bid] {
                let x = info(shape, price, scale, cap);
                let q = next(20_000_000) as i64;
                let n = x.n_of_sell(q);
                assert!(x.sell_qty(n).unwrap() <= q);
                assert!(n == cap || x.sell_qty(n + 1).unwrap() > q, "{shape:?} n_of_sell({q}) = {n}");
                let n = x.n_of_buy(q);
                assert!(x.buy_qty(n).unwrap() <= q);
                assert!(n == cap || x.buy_qty(n + 1).unwrap() > q, "{shape:?} n_of_buy({q}) = {n}");
            }
        }
    }

    /// An ask releases its fill exactly and receives at least the ceil.
    ///
    /// A bid pays exactly the floor and receives its fill.
    #[test]
    fn the_shapes_are_the_covenant_amounts() {
        let ask = info(PairShape::AskMin, 1_234, 1_000, 10_000);
        assert_eq!((ask.sell_qty(7), ask.buy_qty(7)), (Some(7), Some(9)));
        assert!(!ask.buy_exact() && ask.primary() == PairRole::Sell);
        let bid = info(PairShape::Bid, 1_234, 1_000, 10_000);
        assert_eq!((bid.sell_qty(7), bid.buy_qty(7)), (Some(8), Some(7)));
        assert!(bid.buy_exact() && bid.primary() == PairRole::Buy);
        let entry = info(PairShape::AskExact, 1_234, 1_000, 10_000);
        assert!(entry.buy_exact() && entry.primary() == PairRole::Sell);
        assert_eq!(entry.tip_kas(1_000), 0);
    }
    /// The planner reads pair stop evidence only of the order's own KCC-20 extension commitments.
    ///
    /// A fill of another commitment of A's or B's covenant id, or a resting `KobPair` of other
    /// commitments, is never evidence, in mode 0 or mode 1. The same fills under the order's own
    /// commitments are.
    #[test]
    fn pair_stop_evidence_is_only_of_the_orders_own_commitments() {
        let (a, b, ea, eb, other) = ([1; 32], [4; 32], [3; 32], [5; 32], [9; 32]);
        let need = PairNeed {
            ask_a: true,
            a,
            b,
            a_ext: ea,
            b_ext: eb,
            a_scale: 1_000,
            b_scale: 100,
            min_touch: 1,
            min_touch_b: Some(1),
            min_rest: 0,
            state: placeholder_state(),
        };
        let src = |side, scale| TouchSrc { side, price: 1_000, scale, exposed_since: 0 };
        let kas = |ext_a: [u8; 32], ext_b: [u8; 32]| {
            vec![
                KasTouch { ix: 0, token: a, ext: ext_a, src: src(SIDE_ASK, 1_000), n: 10 },
                KasTouch { ix: 1, token: b, ext: ext_b, src: src(SIDE_BID, 100), n: 10 },
            ]
        };
        let pair = |ext_a: [u8; 32], ext_b: [u8; 32]| {
            vec![PairFillTouch {
                ix: 2,
                src: PairTouch {
                    ask: true,
                    a,
                    b,
                    a_ext: ext_a,
                    b_ext: ext_b,
                    a_scale: 1_000,
                    b_scale: 100,
                    price: 7,
                    exposed_since: 0,
                },
                n: 10,
            }]
        };
        let count = |k: &[KasTouch], p: &[PairFillTouch]| evidence_options(&need, true, k, p, 100).len();
        // The order's own commitments: one mode-0 pair of fills and the mode-1 resting pair order.
        assert_eq!(count(&kas(ea, eb), &pair(ea, eb)), 2);
        // Another commitment of A's covenant id, of B's, or of both: nothing.
        assert_eq!(count(&kas(other, eb), &[]), 0);
        assert_eq!(count(&kas(ea, other), &[]), 0);
        assert_eq!(count(&[], &pair(other, eb)), 0);
        assert_eq!(count(&[], &pair(ea, other)), 0);
        assert_eq!(count(&kas(other, other), &pair(other, other)), 0);
    }
}
