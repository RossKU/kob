//! Normalises listed orders into matching candidates at time `t` (`docs/spec/matcher.md` §2).
//!
//! A candidate is one order on one leg with its executable quote at `t` (§2.1, sompi per whole token of its `scale`), the
//! base units it can take now, the hard eligibility rules of §2.2 (activation, TWAP / DCA age and `maxFill`, arming, the
//! minimum fill, FOK completeness, repeat merges), and the honest-matcher exclusions of §2.3 (soft expiry, IOC / FOK kill,
//! 90 days idle, day-order deadline by the UTC clock). Everything mirrors the covenant arithmetic through
//! `kob_protocol::state` (`quote_of`, `BidState::buying_power`, ...), so a candidate the planner fills is one the covenant
//! accepts, and every KAS amount of a fill ([`Cand::value`]) is the exact rounded covenant value.
//!
//! Triggers (§4): an unarmed stop leg or stop entry is a candidate that carries what it needs from its trigger evidence
//! ([`TouchNeed`]); it is fillable only in a batch that also fills a qualifying plain resting order ([`TouchSrc`], the
//! covenants' `touch`), which the planner checks (`super::batch`). The orders a batch may arm or ratchet without filling
//! them are listed by [`updatable`].

use std::collections::{BTreeMap, BTreeSet};

use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::family::Family;
use kob_protocol::state::*;

use super::book::{book_key, outpoint, BookKey, CovId, ListedOrder, Outpoint};

/// Side of the book a candidate trades on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Buys tokens (pays KAS).
    Bid,
    /// Sells tokens (receives KAS).
    Ask,
}

/// Priority class (§3.1): a lower class only uses the liquidity the higher classes left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// IOC, FOK, market and streaming orders (`tif` 1 or 2).
    Immediate = 1,
    /// Armed stop legs and stop entries, or unarmed ones triggered in this very transaction by a plain resting fill of
    /// the batch (their evidence).
    Triggered = 2,
    /// Every other crossing order.
    Resting = 3,
}

/// One order on one leg, normalised for the planner. Quantities are base units of the candidate's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    /// Index of the order in the book slice the candidates were made from.
    pub order: usize,
    pub id: CovId,
    pub kind: TemplateId,
    pub side: Side,
    /// Conditional leg (0 take-profit / limit, 1 stop); 0 for other kinds.
    pub leg: u8,
    pub class: Class,
    /// Executable quote at `t`, sompi per whole token (`scale` base units), §2.1. A pair order leg: its implied quote (§3.5).
    pub quote: i64,
    /// Priority tip, sompi per whole token.
    pub tip: i64,
    /// Base units per whole token: the denominator of `quote` and `tip` (a pair order leg: the base units of its token that
    /// one whole A of the order stands for, so that `quote / scale` is sompi per base unit of the leg's token).
    pub scale: i64,
    /// UTXO DAA score (older first).
    pub age: u64,
    /// DAA at which the view first listed the order.
    pub seen: u64,
    /// Most base units fillable in one transaction now (the order's amount, its buying power, its `maxFill`).
    pub cap: i64,
    /// Least base units of one fill (`minFill`), unless the fill ends the order (see `rest_from`).
    pub min_fill: i64,
    /// The smallest fill that ends the order and is therefore exempt from `min_fill`: `amountLeft`, or for a `KobBid` the
    /// smallest fill after which less than one minimum fill of buying power is left (`i64::MAX`: no fill within `cap`).
    pub rest_from: i64,
    /// `amountLeft` (a bid: its buying power, `cap`): a fill below it leaves a remainder.
    pub left: i64,
    /// FOK: the range of base units a complete fill may take (`[min, max]`); None otherwise.
    pub fok: Option<(i64, i64)>,
    /// IOC (remainder returned at once): must receive the largest fill the book allows.
    pub ioc: bool,
    /// An unarmed stop leg / stop entry: filled at its trigger price, and only next to a qualifying evidence fill of
    /// the same batch (the planner checks it).
    pub trigger: Option<TouchNeed>,
    /// Booked exit take-profit before `rptUntil`: the entry merged (re-armed) in the same transaction.
    pub merge: Option<CovId>,
    /// The merged sell-first entry's custody (an extra token input and output).
    pub merge_custody: bool,
    /// A booked exit's entry: never in the same transaction except as this fill's merge (§6.1).
    pub parent: Option<CovId>,
    /// Plain `KobAsk` / `KobBid` without decay, accepted with a known DAA score: its fill is trigger evidence for the
    /// stops of its book it qualifies for (§4).
    pub touch: Option<TouchSrc>,
    /// The covenant path of this fill does not read the order UTXO's DAA score, so the fill can be
    /// chained onto an unconfirmed parent (§7; the mempool reports unaccepted UTXOs with DAA u64::MAX).
    pub chain_safe: bool,
    /// Estimated serialized bytes the leg adds (inputs, signature scripts, outputs).
    pub bytes: u64,
    /// The book the candidate trades in (a pair order leg: the market of the token it sells or buys, scale 0).
    pub book: BookKey,
    /// The legs that stand for one pair order in the global batch (`super::pair`); None for every KAS-book order.
    pub pair: Option<PairLeg>,
}

/// Which side of a pair order a leg stands for (`super::pair`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PairRole {
    /// The token S the order releases (its custody): sold to the plain KAS bids of S, or to opposite pair orders (netting).
    Sell,
    /// The token T the order receives: bought from the plain KAS asks of T, or from opposite pair orders (netting).
    Buy,
    /// A netting surplus of one token (released by the netted pair orders beyond what they receive), offered to the plain
    /// KAS bids of that token: the tokens are already in the transaction, so its KAS value is zero; what the bids leave goes
    /// to the pair asks that buy the token (their deliveries are minimums).
    Surplus,
}

/// A pair order leg: its role and the order it belongs to (shared by its legs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairLeg {
    pub role: PairRole,
    pub info: std::sync::Arc<super::pair::PairInfo>,
}

impl PairLeg {
    /// The leg whose quantity is the order's fill n (the base units of its base token A): an ask's sale, a bid's purchase.
    pub fn is_primary(&self) -> bool {
        self.role != PairRole::Surplus && self.info.primary() == self.role
    }
}

impl Cand {
    /// True for a plain `KobAsk` / `KobBid` (either family): the only KAS-book orders a pair order route may use.
    pub fn is_plain(&self) -> bool {
        self.pair.is_none() && matches!(self.kind.base(), TemplateId::KobAsk | TemplateId::KobBid)
    }

    /// The role of a pair order leg (None for a KAS-book order).
    pub fn role(&self) -> Option<PairRole> {
        self.pair.as_ref().map(|x| x.role)
    }

    /// The all-in rate as sompi per `scale` base units: what a bid pays (`quote + tip`) or an ask receives (`quote − tip`),
    /// §2.1.
    pub fn all_in(&self) -> i64 {
        match self.side {
            Side::Bid => self.quote.saturating_add(self.tip),
            Side::Ask => self.quote.saturating_sub(self.tip),
        }
    }

    /// The all-in price per token base unit as a fraction (sompi over base units). A pair order leg carries its implied
    /// route price.
    pub fn per_base(&self) -> (i128, i128) {
        (self.all_in() as i128, self.scale.max(1) as i128)
    }

    /// The quote per base unit as a fraction (`quote / scale`), the first key of the book priority.
    pub fn quote_frac(&self) -> (i128, i128) {
        (self.quote as i128, self.scale.max(1) as i128)
    }

    /// The exact KAS of a fill of `n` base units, as the covenants compute it (`quoteOf`): what a bid pays,
    /// `floor(n × (quote + tip) / scale)`, or what an ask receives, `ceil(n × (quote − tip) / scale)`. `None` for a pair
    /// order leg (its KAS is its tip, `super::pair::PairInfo::tip_kas`) and where the covenant arithmetic fails.
    pub fn value(&self, n: i64) -> Option<i64> {
        if self.pair.is_some() || n < 0 {
            return None;
        }
        match self.side {
            Side::Bid => quote_of(n, self.quote.checked_add(self.tip)?, self.scale, Round::Down),
            Side::Ask => quote_of(n, self.quote.checked_sub(self.tip)?, self.scale, Round::Up),
        }
    }

    /// Whether a fill of `n` base units ends the order (and is therefore exempt from its minimum fill).
    pub fn ends(&self, n: i64) -> bool {
        n >= self.rest_from
    }

    /// Whether a fill of `n` base units satisfies the candidate's quantity rules (FOK, the minimum fill).
    pub fn quantity_ok(&self, n: i64) -> bool {
        if n <= 0 || n > self.cap {
            return false;
        }
        if let Some((lo, hi)) = self.fok {
            if n < lo || n > hi {
                return false;
            }
        }
        n >= self.min_fill || self.ends(n)
    }
}

/// What the fill of a plain resting order offers as trigger evidence (mirrors `kob_protocol::build::touch_of`): the
/// token is the candidate's book's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSrc {
    /// [`SIDE_ASK`] (a resting ask sold at its quote) or [`SIDE_BID`] (a resting bid bought at its quote).
    pub side: i64,
    /// The resting order's quote (sompi per whole token).
    pub price: i64,
    /// The resting order's scale (a stop reads only evidence of its own scale).
    pub scale: i64,
    /// `max(UTXO DAA + interval, custody DAA (asks), activeFrom)`.
    pub exposed_since: i64,
}

/// What an unarmed stop reads from its trigger evidence (the covenants' `touch`): a plain resting order of its token
/// and scale filled in the same transaction, on the trigger side and at or through the stop, at least `min_touch`
/// base units, exposed for at least `min_rest` DAA before the lock time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchNeed {
    /// The evidence side: [`SIDE_ASK`] for a sell stop (a resting ask filled at or below `stop`), [`SIDE_BID`] for a
    /// buy stop (a resting bid filled at or above `stop`).
    pub side: i64,
    pub stop: i64,
    /// The stop's scale.
    pub scale: i64,
    /// `minTouch` (base units).
    pub min_touch: i64,
    /// `minRestDaa`.
    pub min_rest: i64,
}

/// Whether a fill of `n` base units of `src` is enough evidence (`minTouch`) exposed long enough (`minRestDaa`) at lock
/// time `t` (`kob_protocol::build::Touch::check`; the caller keeps the token and scale equal).
pub fn touch_ok(src: &TouchSrc, n: i64, min_touch: i64, min_rest: i64, t: i64) -> bool {
    n >= min_touch && src.exposed_since.saturating_add(min_rest) <= t
}

impl TouchNeed {
    /// Whether a fill of `n` base units of `src` (same token) triggers the stop at lock time `t`.
    pub fn served_by(&self, src: &TouchSrc, n: i64, t: i64) -> bool {
        src.side == self.side
            && src.scale == self.scale
            && if self.side == SIDE_ASK { src.price <= self.stop } else { src.price >= self.stop }
            && touch_ok(src, n, self.min_touch, self.min_rest, t)
    }
}

/// A trailing stop's ratchet: the conditional order's state (its `trail_steps`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrailOf {
    /// A `KobCondAsk` (sell stop): ratchets up on a resting BID filled.
    Ask(CondAskState),
    /// A `KobCondBid` (buy stop): ratchets down on a resting ASK filled.
    Bid(CondBidState),
}

impl TrailOf {
    /// The evidence side that ratchets it.
    pub fn side(&self) -> i64 {
        match self {
            TrailOf::Ask(_) => SIDE_BID,
            TrailOf::Bid(_) => SIDE_ASK,
        }
    }
    /// Steps a quote `price` of the ratcheting side justifies (0: none).
    pub fn steps(&self, price: i64) -> i64 {
        match self {
            TrailOf::Ask(s) => s.trail_steps(price),
            TrailOf::Bid(s) => s.trail_steps(price),
        }
    }
}

/// A listed order a batch may arm or ratchet WITHOUT filling it, next to its evidence fill (`BatchUpdate`, the order's
/// `update` entry; the batch's change takes its `keeperTip`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpCand {
    pub id: CovId,
    pub book: BookKey,
    pub outpoint: Outpoint,
    /// The arm of a KAS conditional (trigger-side evidence at or through the stop); None for a pair order.
    pub arm: Option<TouchNeed>,
    /// A trailing stop whose UTXO is `trailWait` old at `t` (CSV): ratcheted by opposite-side evidence.
    pub trail: Option<TrailOf>,
    /// A pair conditional or pair stop entry (`super::pair`): its evidence is two KAS-book fills or a resting pair fill.
    pub pair: Option<std::sync::Arc<super::pair::PairUp>>,
    /// A booked exit's entry: the update never shares a transaction with it (the exit refuses, §6.1).
    pub parent: Option<CovId>,
    /// `keeperTip`: what the batch takes.
    pub tip: i64,
    /// UTXO value (the continuation keeps `value − tip`).
    pub value: i64,
    /// Estimated serialized bytes the update adds (the order input and its continuation).
    pub bytes: u64,
    /// Measured marginal fee of one update at the minimum fee rate (`keeper_tips.json` `updateFee` of the token program).
    pub fee_floor: i64,
}

/// Inputs of candidate generation.
pub struct CandCtx<'a> {
    /// Lock time `t` (§3.3).
    pub t: i64,
    /// UTC clock (day-order deadlines).
    pub utc: u64,
    /// Order ids spent by pending transactions or backed off: never candidates.
    pub excluded: &'a BTreeSet<CovId>,
    /// Every listed order by id (for merges: the booked exit's entry).
    pub by_id: &'a BTreeMap<CovId, ListedOrder>,
    /// Orders whose UTXO is not accepted yet (a chained step): their DAA is unknown.
    pub unaccepted: &'a BTreeSet<CovId>,
}

/// Full redeem-script length of a template instance.
pub fn redeem_len(id: TemplateId) -> u64 {
    if id.is_artifact() {
        let t = template(id);
        (t.prefix.len() + t.state_len + t.suffix.len()) as u64
    } else {
        let t = token_template(id);
        (t.prefix.len() + t.state_len + t.suffix.len()) as u64
    }
}

/// Byte estimate of a transaction output (value, P2SH script, covenant binding).
pub const OUTPUT_BYTES: u64 = 90;
/// Byte estimate of a covenant input besides its redeem script (outpoint, sequence, budget, args).
pub const INPUT_BYTES: u64 = 180;
/// Byte estimate of the transaction envelope, the funding input and the change output.
pub const BASE_BYTES: u64 = 420;
/// Leader `next_states` per token output (112-byte state + push).
pub const NEXT_STATE_BYTES: u64 = 115;

/// Bytes of a token input of `program`.
pub fn token_input_bytes(program: Option<TemplateId>) -> u64 {
    INPUT_BYTES + program.map(redeem_len).unwrap_or(2_500)
}

/// §2.3 honest-matcher exclusions (fills only; merges are always allowed).
pub fn excluded_by_expiry(o: &ListedOrder, t: i64, utc: u64) -> bool {
    // An unaccepted continuation (chained step) will be accepted at or after `t`.
    let udaa = if o.utxo_daa() == u64::MAX { t } else { o.utxo_daa() as i64 };
    let due = o.order.state.refund_due(udaa).unwrap_or(i64::MAX);
    if t >= due {
        return true;
    }
    matches!(o.deadline, Some(d) if utc >= d)
}

/// The unarmed stop legs / stop entries (and trailing stops) of the view a batch may arm (or ratchet) with an `update`
/// next to an evidence fill: accepted with a known DAA score at or before `t`, active, not expired, not excluded, in a
/// book. Whether a batch's evidence serves one is the planner's to decide.
pub fn updatable(orders: &[&ListedOrder], cx: &CandCtx) -> Vec<UpCand> {
    let t = cx.t;
    let mut out = vec![];
    for &o in orders {
        let id = o.id();
        if o.order.utxo.covenant_id.is_none() || cx.excluded.contains(&id) || cx.unaccepted.contains(&id) || !o.custody_ok() {
            continue;
        }
        let udaa = o.utxo_daa();
        if udaa == u64::MAX || udaa as i64 > t || excluded_by_expiry(o, t, cx.utc) {
            continue;
        }
        let Some(book) = book_key(o) else { continue };
        let old_enough = |wait: i64| t - udaa as i64 >= wait.max(0);
        let (arm, trail, active_from, parent) = match o.base_state() {
            AnyState::KobCondAsk(s) if s.armed == 0 && s.stop_price > 0 && s.amount_left > 0 && (0..=10_000).contains(&s.slip_bps) => {
                let need =
                    TouchNeed { side: SIDE_ASK, stop: s.stop_price, scale: s.scale, min_touch: s.min_touch, min_rest: s.min_rest_daa };
                let trail = (s.trail_step > 0 && old_enough(s.trail_wait)).then(|| TrailOf::Ask(s.clone()));
                (need, trail, s.active_from, s.is_booked().then_some(s.parent))
            }
            AnyState::KobCondBid(s) if s.armed == 0 && s.stop_price > 0 && s.amount_left > 0 && (0..=10_000).contains(&s.slip_bps) => {
                let need =
                    TouchNeed { side: SIDE_BID, stop: s.stop_price, scale: s.scale, min_touch: s.min_touch, min_rest: s.min_rest_daa };
                let trail = (s.trail_step > 0 && old_enough(s.trail_wait)).then(|| TrailOf::Bid(s.clone()));
                (need, trail, s.active_from, s.is_booked().then_some(s.parent))
            }
            AnyState::KobIfdBid(s) if s.armed == 0 && s.entry_stop > 0 && s.amount_left > 0 => (
                TouchNeed { side: SIDE_BID, stop: s.entry_stop, scale: s.scale, min_touch: s.min_touch, min_rest: s.min_rest_daa },
                None,
                s.active_from,
                None,
            ),
            AnyState::KobIfdAsk(s) if s.armed == 0 && s.entry_stop > 0 && s.amount_left > 0 => (
                TouchNeed { side: SIDE_ASK, stop: s.entry_stop, scale: s.scale, min_touch: s.min_touch, min_rest: s.min_rest_daa },
                None,
                s.active_from,
                None,
            ),
            _ => continue,
        };
        if t < active_from {
            continue;
        }
        let Some(tip) = o.order.state.keeper_tip() else { continue };
        let fee_floor = token_program(&o.order.state)
            .and_then(|p| kob_protocol::defaults::tips(p).ok())
            .map(|x| x.update_fee.min(i64::MAX as u64) as i64)
            .unwrap_or(0);
        out.push(UpCand {
            id,
            book,
            outpoint: outpoint(&o.order.utxo),
            arm: Some(arm),
            trail,
            pair: None,
            parent,
            tip,
            value: o.order.utxo.amount.min(i64::MAX as u64) as i64,
            bytes: INPUT_BYTES + redeem_len(o.order.state.template_id()) + OUTPUT_BYTES,
            fee_floor,
        });
    }
    out
}

fn base(o: &ListedOrder, idx: usize, kind: TemplateId, side: Side, book: BookKey) -> Cand {
    Cand {
        order: idx,
        id: o.id(),
        kind: kind.in_family(o.order.state.family()),
        side,
        leg: 0,
        class: Class::Resting,
        quote: 0,
        tip: 0,
        scale: book.scale.max(1),
        age: o.utxo_daa(),
        seen: o.seen_daa,
        cap: 0,
        min_fill: 1,
        rest_from: i64::MAX,
        left: 0,
        fok: None,
        ioc: false,
        trigger: None,
        merge: None,
        merge_custody: false,
        parent: None,
        touch: None,
        chain_safe: true,
        bytes: 0,
        book,
        pair: None,
    }
}

/// Program of the order's token (None if it is not a supported program of the order's family).
pub fn token_program(s: &AnyState) -> Option<TemplateId> {
    kob_protocol::artifacts::token_template_by_hash(&s.token_tpl_hash()?).filter(|t| t.family == s.family()).map(|t| t.id)
}

/// The smallest fill in `1..=cap` after which the bid of escrow `v` can no longer continue (less than one minimum fill of
/// buying power left: the covenant's termination, exempt from `minFill`), or `i64::MAX` when every fill within `cap`
/// leaves it able to continue. `can_continue(v − used(n))` only turns false as `n` grows, so a bisection finds it.
pub fn bid_rest_from(s: &BidState, v: i64, cap: i64) -> i64 {
    let ends = |n: i64| s.used(n).and_then(|u| v.checked_sub(u)).is_none_or(|left| !s.can_continue(left));
    if cap <= 0 || !ends(cap) {
        return i64::MAX;
    }
    let (mut lo, mut hi) = (0i64, cap);
    // invariant: !ends(lo) (or lo = 0), ends(hi)
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if ends(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Every candidate (order × leg) of the orders at `t`, of any number of books. Orders are indexed as in `orders`.
pub fn candidates(orders: &[ListedOrder], cx: &CandCtx) -> Vec<Cand> {
    let refs: Vec<&ListedOrder> = orders.iter().collect();
    candidates_of(&refs, cx)
}

/// [`candidates`] over borrowed orders (the global batch reads every book of the view without copying it).
pub fn candidates_of(orders: &[&ListedOrder], cx: &CandCtx) -> Vec<Cand> {
    let mut out = vec![];
    let t = cx.t;
    for (idx, &o) in orders.iter().enumerate() {
        let id = o.id();
        if o.order.utxo.covenant_id.is_none() || cx.excluded.contains(&id) || !o.custody_ok() {
            continue;
        }
        // an order that belongs to no book (a pair order, an unknown extension) is not a KAS-book candidate
        let Some(bk) = book_key(o) else { continue };
        let unaccepted = cx.unaccepted.contains(&id) || o.order.utxo.block_daa_score == u64::MAX;
        // An order UTXO accepted after `t` cannot prove anything at `t` (activation, freshness).
        if !unaccepted && o.utxo_daa() as i64 > t {
            continue;
        }
        if excluded_by_expiry(o, t, cx.utc) {
            continue;
        }
        let udaa = if unaccepted { t } else { o.utxo_daa() as i64 };
        let fam = o.order.state.family();
        let rl = |k: TemplateId| redeem_len(k.in_family(fam));
        let program = token_program(&o.order.state);
        let tok_in = token_input_bytes(program);
        // The KRON kinds carry the typed states of their KCC-20 counterparts: match on those.
        let state = o.order.state.clone().into_family(Family::Kcc20);
        let custody_bytes = |has: bool| if has { tok_in + NEXT_STATE_BYTES } else { 0 };
        let v = o.order.utxo.amount.min(i64::MAX as u64) as i64;
        match &state {
            AnyState::KobAsk(s) => {
                if t < s.active_from || s.amount_left <= 0 {
                    continue;
                }
                if s.interval > 0 && (unaccepted || t < udaa + s.interval) {
                    continue; // TWAP slice: the UTXO must be `interval` DAA old (CSV)
                }
                let mut c = base(o, idx, TemplateId::KobAsk, Side::Ask, bk);
                let Some(q) = s.price_at(t, udaa) else { continue };
                c.quote = q;
                c.tip = s.tip;
                c.scale = s.scale;
                c.left = s.amount_left;
                c.cap = if s.max_fill > 0 { s.amount_left.min(s.max_fill) } else { s.amount_left };
                c.min_fill = s.min_fill;
                c.rest_from = s.amount_left;
                c.class = if s.tif == TIF_GTC { Class::Resting } else { Class::Immediate };
                c.ioc = s.tif == TIF_IOC;
                if s.tif == TIF_FOK {
                    if c.cap < s.amount_left {
                        continue; // a FOK that cannot complete in one fill is never filled
                    }
                    c.fok = Some((s.amount_left, s.amount_left));
                }
                // trigger evidence: the covenants read the order's and its custody's DAA scores (exposure)
                let cdaa = o.custody.as_ref().map(|x| x.utxo.block_daa_score).unwrap_or(u64::MAX);
                if s.slope == 0 && !unaccepted && cdaa != u64::MAX && cdaa as i64 <= t {
                    c.touch = Some(TouchSrc {
                        side: SIDE_ASK,
                        price: s.price,
                        scale: s.scale,
                        exposed_since: (udaa.saturating_add(s.interval)).max(cdaa as i64).max(s.active_from),
                    });
                }
                c.chain_safe = s.interval == 0;
                c.bytes = INPUT_BYTES + rl(TemplateId::KobAsk) + custody_bytes(true) + 3 * OUTPUT_BYTES;
                out.push(c);
            }
            AnyState::KobBid(s) => {
                if t < s.active_from || s.min_fill <= 0 {
                    continue;
                }
                if s.interval > 0 && (unaccepted || t < udaa + s.interval) {
                    continue;
                }
                // The buying power keeps the maker's delivery carrier on the delivery output (a smaller output raises the
                // storage mass) and the reserve: the largest n whose consumed budget ceil(n·(pMax + tip)/scale) leaves both.
                // A bid's quantity is free (its escrow bounds it): never more than the token amounts can hold.
                let power = s.buying_power(v).min(i64::MAX / 4);
                let cap = if s.max_fill > 0 { power.min(s.max_fill) } else { power };
                if cap <= 0 {
                    continue;
                }
                let mut c = base(o, idx, TemplateId::KobBid, Side::Bid, bk);
                let Some(q) = s.price_at(t, udaa) else { continue };
                c.quote = q;
                c.tip = s.tip;
                c.scale = s.scale;
                c.cap = cap;
                c.left = cap;
                c.min_fill = s.min_fill;
                c.rest_from = bid_rest_from(s, v, cap);
                c.class = if s.tif == TIF_GTC { Class::Resting } else { Class::Immediate };
                c.ioc = s.tif == TIF_IOC;
                if s.tif == TIF_FOK {
                    // Complete: less than one minimum fill of buying power left after the fill (the covenant's rule).
                    if c.rest_from > cap {
                        continue;
                    }
                    c.fok = Some((c.rest_from, cap));
                }
                if s.slope == 0 && !unaccepted {
                    c.touch = Some(TouchSrc {
                        side: SIDE_BID,
                        price: s.price,
                        scale: s.scale,
                        exposed_since: (udaa.saturating_add(s.interval)).max(s.active_from),
                    });
                }
                c.chain_safe = s.interval == 0;
                c.bytes = INPUT_BYTES + rl(TemplateId::KobBid) + 2 * OUTPUT_BYTES + NEXT_STATE_BYTES;
                out.push(c);
            }
            AnyState::KobCondAsk(s) => {
                if t < s.active_from || s.amount_left <= 0 {
                    continue;
                }
                let mk = |leg: u8| {
                    let mut c = base(o, idx, TemplateId::KobCondAsk, Side::Ask, bk);
                    c.leg = leg;
                    c.parent = s.is_booked().then_some(s.parent);
                    c.tip = s.tip;
                    c.scale = s.scale;
                    c.left = s.amount_left;
                    c.cap = s.amount_left;
                    c.min_fill = s.min_fill;
                    c.rest_from = s.amount_left;
                    // Continuations write the auction origin from the UTXO DAA when armed = 1.
                    c.chain_safe = s.armed != 1;
                    c.bytes = INPUT_BYTES + rl(TemplateId::KobCondAsk) + custody_bytes(true) + 3 * OUTPUT_BYTES;
                    c
                };
                // Take-profit leg.
                if s.tp_price > 0 {
                    let mut c = mk(0);
                    c.quote = s.tp_price;
                    let mut ok = true;
                    if s.is_booked() && t < s.rpt_until {
                        match cx.by_id.get(&s.parent) {
                            Some(e) if merge_entry_ok(e, cx) => match &e.order.state {
                                AnyState::KobIfdBid(es) | AnyState::KobIfdBidKron(es)
                                    if es.token_cov_id == s.token_cov_id
                                        && es.budget_rate() == Some(s.rpt_price)
                                        && s.tp_price - s.tip > s.rpt_price =>
                                {
                                    c.merge = Some(s.parent);
                                    // the merge argument -(k·2^53 + m) carries m < 2^53
                                    c.cap = c.cap.min(MERGE_SHIFT - 1);
                                    c.chain_safe = false;
                                    c.bytes += INPUT_BYTES + rl(TemplateId::KobIfdBid) + OUTPUT_BYTES;
                                }
                                _ => ok = false,
                            },
                            _ => ok = false, // the entry is not visible (yet): no take-profit without its merge
                        }
                    }
                    if ok {
                        out.push(c);
                    }
                }
                // Stop leg.
                if s.stop_price > 0 && (0..=10_000).contains(&s.slip_bps) {
                    let mut c = mk(1);
                    c.class = Class::Triggered;
                    if s.armed != 0 {
                        let origin = armed_origin(s.armed, udaa).unwrap_or(i64::MAX);
                        if unaccepted && s.armed == 1 {
                            continue;
                        }
                        if s.band_daa > 0 && t < origin {
                            continue; // the auction has not started at t
                        }
                        let Some(q) = s.stop_at(false, t, udaa) else { continue };
                        c.quote = q;
                        out.push(c);
                    } else if !unaccepted {
                        // triggered by a resting ask filled at or below the stop in the same batch (the planner checks)
                        c.trigger = Some(TouchNeed {
                            side: SIDE_ASK,
                            stop: s.stop_price,
                            scale: s.scale,
                            min_touch: s.min_touch,
                            min_rest: s.min_rest_daa,
                        });
                        let Some(q) = s.stop_at(true, t, udaa) else { continue };
                        c.quote = q;
                        c.chain_safe = false;
                        out.push(c);
                    }
                }
            }
            AnyState::KobCondBid(s) => {
                if t < s.active_from || s.amount_left <= 0 {
                    continue;
                }
                let mk = |leg: u8, quote: i64| {
                    let mut c = base(o, idx, TemplateId::KobCondBid, Side::Bid, bk);
                    c.leg = leg;
                    c.parent = s.is_booked().then_some(s.parent);
                    c.quote = quote;
                    c.tip = s.tip;
                    c.scale = s.scale;
                    c.left = s.amount_left;
                    c.min_fill = s.min_fill;
                    c.rest_from = s.amount_left;
                    // Largest affordable fill (the escrow covers the whole amount at the worst leg); a bisection, O(log
                    // amountLeft) whatever `amountLeft` claims.
                    c.cap = largest_fit(s.amount_left, |n| {
                        s.spend(n, quote)
                            .and_then(|sp| v.checked_sub(sp))
                            .is_some_and(|left| left - if n < s.amount_left { s.delivery_carrier } else { 0 } > 0)
                    });
                    c.chain_safe = s.armed != 1;
                    c.bytes = INPUT_BYTES + rl(TemplateId::KobCondBid) + 2 * OUTPUT_BYTES + NEXT_STATE_BYTES;
                    c
                };
                if s.tp_price > 0 {
                    let mut c = mk(0, s.tp_price);
                    let mut ok = c.cap > 0;
                    if s.is_booked() && t < s.rpt_until {
                        match cx.by_id.get(&s.parent) {
                            Some(e) if merge_entry_ok(e, cx) => match &e.order.state {
                                AnyState::KobIfdAsk(es) | AnyState::KobIfdAskKron(es)
                                    if es.token_cov_id == s.token_cov_id
                                        && es.proceeds_rate() == s.rpt_price
                                        && es.prefund == s.rpt_pre
                                        && s.rpt_price > s.tp_price + s.tip =>
                                {
                                    c.merge = Some(s.parent);
                                    c.merge_custody = es.amount_left > 0;
                                    c.chain_safe = false;
                                    c.bytes += INPUT_BYTES + rl(TemplateId::KobIfdAsk) + 2 * OUTPUT_BYTES + NEXT_STATE_BYTES;
                                    if c.merge_custody {
                                        c.bytes += tok_in;
                                    }
                                    // the merge argument -(k·2^53 + m) carries m < 2^53
                                    c.cap = c.cap.min(MERGE_SHIFT - 1);
                                    // The exit's continuation keeps what its remaining amount needs:
                                    // v − ceil(n·rptPrice/scale) − ceil(n·rptPre/scale) > 0.
                                    if c.cap < s.amount_left {
                                        c.cap = largest_fit(c.cap, |n| match (s.rpt_proceeds(n), s.rpt_prefund(n)) {
                                            (Some(p), Some(q)) => (v as i128) - (p as i128) - (q as i128) > 0,
                                            _ => false,
                                        });
                                    }
                                }
                                _ => ok = false,
                            },
                            _ => ok = false,
                        }
                    }
                    if ok && c.cap > 0 {
                        out.push(c);
                    }
                }
                if s.stop_price > 0 && (0..=10_000).contains(&s.slip_bps) {
                    if s.armed != 0 {
                        if unaccepted && s.armed == 1 {
                            continue;
                        }
                        let origin = armed_origin(s.armed, udaa).unwrap_or(i64::MAX);
                        if s.band_daa > 0 && t < origin {
                            continue;
                        }
                        let Some(q) = s.stop_at(false, t, udaa) else { continue };
                        let mut c = mk(1, q);
                        c.class = Class::Triggered;
                        if c.cap > 0 {
                            out.push(c);
                        }
                    } else if !unaccepted {
                        // triggered by a resting bid filled at or above the stop in the same batch (the planner checks)
                        let Some(q) = s.stop_at(true, t, udaa) else { continue };
                        let mut c = mk(1, q);
                        c.class = Class::Triggered;
                        c.trigger = Some(TouchNeed {
                            side: SIDE_BID,
                            stop: s.stop_price,
                            scale: s.scale,
                            min_touch: s.min_touch,
                            min_rest: s.min_rest_daa,
                        });
                        c.chain_safe = false;
                        if c.cap > 0 {
                            out.push(c);
                        }
                    }
                }
            }
            AnyState::KobIfdBid(s) => {
                if t < s.active_from || s.amount_left <= 0 {
                    continue;
                }
                let mut c = base(o, idx, TemplateId::KobIfdBid, Side::Bid, bk);
                c.tip = s.tip;
                c.scale = s.scale;
                c.left = s.amount_left;
                c.min_fill = s.min_fill;
                c.rest_from = s.amount_left;
                // Booked exits write rptUntil from max(UTXO DAA, t); stop entries their origin.
                c.chain_safe = s.entry_stop == 0 && s.rpt_amount == 0;
                if s.entry_stop > 0 {
                    c.class = Class::Triggered;
                    if s.armed != 0 {
                        if unaccepted {
                            continue;
                        }
                        let origin = armed_origin(s.armed, udaa).unwrap_or(i64::MAX);
                        if s.band_daa > 0 && t < origin {
                            continue;
                        }
                        let Some(q) = s.price_at(false, t, udaa) else { continue };
                        c.quote = q;
                    } else {
                        if unaccepted {
                            continue;
                        }
                        // a buy-stop entry: a resting bid filled at or above its trigger in the same batch
                        c.trigger = Some(TouchNeed {
                            side: SIDE_BID,
                            stop: s.entry_stop,
                            scale: s.scale,
                            min_touch: s.min_touch,
                            min_rest: s.min_rest_daa,
                        });
                        let Some(q) = s.price_at(true, t, udaa) else { continue };
                        c.quote = q;
                    }
                } else {
                    c.quote = s.price;
                }
                // Affordability: the entry keeps a delivery carrier and (continuing) an exit carrier.
                let quote = c.quote;
                let fits = |n: i64| {
                    let Some(left) = s.spend(n, quote).and_then(|sp| v.checked_sub(sp)) else { return false };
                    let left = left - s.delivery_carrier;
                    if n < s.amount_left || s.rpt_amount > 0 {
                        left - s.exit_carrier > 0
                    } else {
                        left > 0
                    }
                };
                let mut limit = s.amount_left;
                if s.rpt_amount > 0 {
                    // a booking (rptAmount > n) carries n < 2^53 in its exit's merge argument
                    limit = limit.min(MERGE_SHIFT - 1);
                }
                let n = largest_fit(limit, fits);
                c.cap = n;
                if n <= 0 {
                    continue;
                }
                let exit_tpl = template(TemplateId::KobCondAsk.in_family(fam));
                c.bytes = INPUT_BYTES
                    + rl(TemplateId::KobIfdBid)
                    + (exit_tpl.prefix.len() + exit_tpl.suffix.len()) as u64
                    + 3 * OUTPUT_BYTES
                    + NEXT_STATE_BYTES;
                out.push(c);
            }
            AnyState::KobIfdAsk(s) => {
                if t < s.active_from || s.amount_left <= 0 {
                    continue; // an empty repeating entry waits for its exits
                }
                if s.price < s.tip {
                    continue; // the covenant requires price >= tip
                }
                let mut c = base(o, idx, TemplateId::KobIfdAsk, Side::Ask, bk);
                c.tip = s.tip;
                c.scale = s.scale;
                c.left = s.amount_left;
                c.min_fill = s.min_fill;
                c.rest_from = s.amount_left;
                c.chain_safe = s.entry_stop == 0 && s.rpt_amount == 0;
                if s.entry_stop > 0 {
                    c.class = Class::Triggered;
                    if s.armed != 0 {
                        if unaccepted {
                            continue;
                        }
                        let origin = armed_origin(s.armed, udaa).unwrap_or(i64::MAX);
                        if s.band_daa > 0 && t < origin {
                            continue;
                        }
                        let Some(q) = s.price_at(false, t, udaa) else { continue };
                        c.quote = q;
                    } else {
                        if unaccepted {
                            continue;
                        }
                        // a sell-stop entry: a resting ask filled at or below its trigger in the same batch
                        c.trigger = Some(TouchNeed {
                            side: SIDE_ASK,
                            stop: s.entry_stop,
                            scale: s.scale,
                            min_touch: s.min_touch,
                            min_rest: s.min_rest_daa,
                        });
                        let Some(q) = s.price_at(true, t, udaa) else { continue };
                        c.quote = q;
                    }
                } else {
                    c.quote = s.price;
                }
                // The entry continuation keeps its carrier: v − ceil(n·prefund/scale) − exitCarrier > 0.
                let cust = o.custody.as_ref().map(|x| x.utxo.amount.min(i64::MAX as u64) as i64).unwrap_or(0);
                let fits = |n: i64| {
                    if n < s.amount_left || s.rpt_amount > 0 {
                        let Some(pre) = s.prefund_of(n) else { return false };
                        (v as i128) - (pre as i128) - (s.exit_carrier as i128) + if n == s.amount_left { cust as i128 } else { 0 } > 0
                    } else {
                        true
                    }
                };
                let mut limit = s.amount_left;
                if s.rpt_amount > 0 {
                    limit = limit.min(MERGE_SHIFT - 1);
                }
                let n = largest_fit(limit, fits);
                c.cap = n;
                if n <= 0 || s.proceeds(1, c.quote).is_none() {
                    continue;
                }
                let exit_tpl = template(TemplateId::KobCondBid.in_family(fam));
                c.bytes = INPUT_BYTES
                    + rl(TemplateId::KobIfdAsk)
                    + (exit_tpl.prefix.len() + exit_tpl.suffix.len()) as u64
                    + custody_bytes(true)
                    + 3 * OUTPUT_BYTES;
                out.push(c);
            }
            // pair orders are planned by `super::pair`; the KRON kinds were normalised to their KCC-20 twins above
            _ => {}
        }
    }
    // a fill must be worth something to its maker: an ask's all-in above its tip, a bid's all-in positive
    out.retain(|c| c.cap > 0 && c.all_in() > 0 && c.scale > 0);
    out
}

/// Largest `n` in `0..=max` for which `fits(n)` holds, where `fits` is the covenant's affordability test of a fill of `n`
/// base units: evaluated at `max` itself (which may differ: the fill that takes everything skips the continuation
/// carriers) and otherwise monotone in `n` (a cost that only grows with `n`), so a bisection finds the answer in O(log
/// max) evaluations whatever an order's amount claims.
pub fn largest_fit(max: i64, fits: impl Fn(i64) -> bool) -> i64 {
    if max <= 0 {
        return 0;
    }
    if fits(max) {
        return max;
    }
    // below `max` the test is monotone in n: true up to a threshold
    let mut hi = max - 1;
    if hi == 0 {
        return 0;
    }
    if fits(hi) {
        return hi;
    }
    let mut lo = 0;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// A repeating entry can be merged: listed, not spent by a pending transaction, custody valid.
fn merge_entry_ok(e: &ListedOrder, cx: &CandCtx) -> bool {
    !cx.excluded.contains(&e.id())
        && e.custody_ok()
        && e.order.utxo.block_daa_score != u64::MAX
        && !cx.unaccepted.contains(&e.id())
        && e.order.utxo.block_daa_score as i64 <= cx.t
}
