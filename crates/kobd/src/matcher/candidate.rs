//! Matching candidates, the listed orders normalised at time `t` (`docs/spec/matcher.md` §2).
//!
//! A candidate is one order on one leg. It carries its executable quote at `t` (§2.1), in sompi
//! per whole token of its `scale`, and the base units it can take now. Candidate generation
//! applies the hard eligibility rules of §2.2:
//! - activation, TWAP and DCA age, and `maxFill`;
//! - arming;
//! - the minimum fill and FOK completeness;
//! - repeat merges.
//!
//! It also applies the honest-matcher exclusions of §2.3. These are soft expiry, the IOC and FOK
//! kill, 90 days idle, and the day-order deadline by the UTC clock.
//!
//! The arithmetic is the covenants', through `kob_protocol::state` (`quote_of`,
//! `BidState::buying_power` and the like). So a candidate the planner fills is one the covenant
//! accepts, and every KAS amount of a fill ([`Cand::value`]) is the exact rounded covenant value.
//!
//! # Triggers
//!
//! An unarmed stop leg or stop entry (§4) is a candidate that carries what it needs from its
//! trigger evidence ([`TouchNeed`]). It is fillable only in a batch that also fills a qualifying
//! plain resting order ([`TouchSrc`], the covenants' `touch`). The planner in `super::batch`
//! checks that. [`updatable`] lists the orders a batch may arm or ratchet without filling them.

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
    /// Armed stop legs and stop entries.
    ///
    /// It also holds unarmed ones that a plain resting fill of the same batch triggers in this very
    /// transaction, as their evidence.
    Triggered = 2,
    /// Every other crossing order.
    Resting = 3,
}

/// One order on one leg, normalised for the planner.
///
/// Quantities are base units of the candidate's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    /// Index of the order in the book slice the candidates were made from.
    pub order: usize,
    /// Covenant id of the order.
    pub id: CovId,
    /// Template of the order, in its family and the build of its token program.
    pub kind: TemplateId,
    /// Side of the book the candidate trades on.
    pub side: Side,
    /// Conditional leg, 0 for take-profit or limit and 1 for stop. 0 for other kinds.
    pub leg: u8,
    /// Priority class (§3.1).
    pub class: Class,
    /// Executable quote at `t`, in sompi per whole token (`scale` base units), §2.1.
    ///
    /// A pair order leg carries its implied quote (§3.5).
    pub quote: i64,
    /// Priority tip, in sompi per whole token.
    pub tip: i64,
    /// Base units per whole token, the denominator of `quote` and `tip`.
    ///
    /// For a pair order leg it is the base units of the leg's token that one whole A of the order
    /// stands for. So `quote / scale` is sompi per base unit of the leg's token.
    pub scale: i64,
    /// UTXO DAA score. Older orders come first.
    pub age: u64,
    /// DAA score at which the view first listed the order.
    pub seen: u64,
    /// Most base units fillable in one transaction now.
    ///
    /// It is bounded by the order's amount, its buying power and its `maxFill`.
    pub cap: i64,
    /// Least base units of one fill (`minFill`), unless the fill ends the order (see `rest_from`).
    pub min_fill: i64,
    /// Smallest fill that ends the order and is therefore exempt from `min_fill`.
    ///
    /// It is `amountLeft`. For a `KobBid` it is the smallest fill after which less than one
    /// minimum fill of buying power is left. `i64::MAX` means no fill within `cap` ends the order.
    pub rest_from: i64,
    /// Most base units of a fill that does not end the order, which is a fill below `rest_from`.
    ///
    /// It is the KIP-9 bound of the outputs such a fill leaves: a continuation, a remaining
    /// custody or the maker's payout ([`kip9_limits`]). A fill between it and `rest_from` would
    /// leave dust and is never planned. `i64::MAX` means no bound below `cap`.
    pub part_cap: i64,
    /// `amountLeft`, or a bid's buying power (`cap`). A fill below it leaves a remainder.
    pub left: i64,
    /// For a FOK, the range `[min, max]` of base units a complete fill may take. None otherwise.
    pub fok: Option<(i64, i64)>,
    /// An IOC, whose remainder is returned at once.
    ///
    /// It must receive the largest fill the book allows.
    pub ioc: bool,
    /// Trigger need of an unarmed stop leg or stop entry.
    ///
    /// Such a candidate is filled at its trigger price, and only next to a qualifying evidence fill
    /// of the same batch. The planner checks that.
    pub trigger: Option<TouchNeed>,
    /// For a booked exit's take-profit before `rptUntil`, the entry merged (re-armed) in the same
    /// transaction.
    pub merge: Option<CovId>,
    /// The merged sell-first entry has a custody, an extra token input and output.
    pub merge_custody: bool,
    /// A booked exit's entry.
    ///
    /// The entry never shares the transaction, except as this fill's merge (§6.1).
    pub parent: Option<CovId>,
    /// Trigger evidence the fill offers to the stops of its book that it qualifies for (§4).
    ///
    /// Only a plain `KobAsk` or `KobBid` without decay, accepted with a known DAA score, has it.
    pub touch: Option<TouchSrc>,
    /// The covenant path of this fill does not read the order UTXO's DAA score.
    ///
    /// So the fill can be chained onto an unconfirmed parent (§7). The mempool reports unaccepted
    /// UTXOs with DAA score `u64::MAX`.
    pub chain_safe: bool,
    /// Estimated serialized bytes the leg adds: inputs, signature scripts and outputs.
    pub bytes: u64,
    /// The book the candidate trades in.
    ///
    /// For a pair order leg it is the market of the token the leg sells or buys, with scale 0.
    pub book: BookKey,
    /// The pair order this leg stands for in the global batch (`super::pair`).
    ///
    /// None for every KAS-book order.
    pub pair: Option<PairLeg>,
}

/// Which side of a pair order a leg stands for (`super::pair`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PairRole {
    /// The token S the order releases from its custody.
    ///
    /// It is sold to the plain KAS bids of S, or to opposite pair orders by netting.
    Sell,
    /// The token T the order receives.
    ///
    /// It is bought from the plain KAS asks of T, or from opposite pair orders by netting.
    Buy,
    /// A netting surplus of one token, offered to the plain KAS bids of that token.
    ///
    /// The netted pair orders release it beyond what they receive. The tokens are already in the
    /// transaction, so the leg's KAS value is zero. What the bids leave goes to the pair asks that
    /// buy the token, because their deliveries are minimums.
    Surplus,
}

/// A pair order leg, as its role and the order it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairLeg {
    /// Side of the pair order the leg stands for.
    pub role: PairRole,
    /// The pair order, shared by all its legs.
    pub info: std::sync::Arc<super::pair::PairInfo>,
}

impl PairLeg {
    /// Whether this leg's quantity is the order's fill `n`, in base units of its base token A.
    ///
    /// That is an ask's sale or a bid's purchase.
    pub fn is_primary(&self) -> bool {
        self.role != PairRole::Surplus && self.info.primary() == self.role
    }
}

impl Cand {
    /// True for a plain `KobAsk` or `KobBid` of either family.
    ///
    /// These are the only KAS-book orders a pair order route may use.
    pub fn is_plain(&self) -> bool {
        self.pair.is_none() && matches!(self.kind.base(), TemplateId::KobAsk | TemplateId::KobBid)
    }

    /// The role of a pair order leg (None for a KAS-book order).
    pub fn role(&self) -> Option<PairRole> {
        self.pair.as_ref().map(|x| x.role)
    }

    /// All-in rate in sompi per `scale` base units (§2.1).
    ///
    /// It is what a bid pays (`quote + tip`) or what an ask receives (`quote − tip`).
    pub fn all_in(&self) -> i64 {
        match self.side {
            Side::Bid => self.quote.saturating_add(self.tip),
            Side::Ask => self.quote.saturating_sub(self.tip),
        }
    }

    /// All-in price per token base unit, as a fraction of sompi over base units.
    ///
    /// A pair order leg carries its implied route price.
    pub fn per_base(&self) -> (i128, i128) {
        (self.all_in() as i128, self.scale.max(1) as i128)
    }

    /// The quote per base unit as a fraction (`quote / scale`), the first key of the book priority.
    pub fn quote_frac(&self) -> (i128, i128) {
        (self.quote as i128, self.scale.max(1) as i128)
    }

    /// Exact KAS of a fill of `n` base units, as the covenants compute it (`quoteOf`).
    ///
    /// A bid pays `floor(n × (quote + tip) / scale)`, and an ask receives
    /// `ceil(n × (quote − tip) / scale)`. Returns `None` where the covenant arithmetic fails, and
    /// for a pair order leg, whose KAS is its tip (`super::pair::PairInfo::tip_kas`).
    pub fn value(&self, n: i64) -> Option<i64> {
        if self.pair.is_some() || n < 0 {
            return None;
        }
        match self.side {
            Side::Bid => quote_of(n, self.quote.checked_add(self.tip)?, self.scale, Round::Down),
            Side::Ask => quote_of(n, self.quote.checked_sub(self.tip)?, self.scale, Round::Up),
        }
    }

    /// Whether a fill of `n` base units ends the order, which exempts it from the minimum fill.
    pub fn ends(&self, n: i64) -> bool {
        n >= self.rest_from
    }

    /// Whether a fill of `n` base units satisfies the candidate's quantity rules.
    ///
    /// The rules are the cap, the FOK range, the minimum fill and the KIP-9 bound of a partial
    /// fill's outputs.
    pub fn quantity_ok(&self, n: i64) -> bool {
        if n <= 0 || n > self.cap {
            return false;
        }
        if let Some((lo, hi)) = self.fok {
            if n < lo || n > hi {
                return false;
            }
        }
        self.ends(n) || (n >= self.min_fill && n <= self.part_cap)
    }
}

/// Trigger evidence that the fill of a plain resting order offers.
///
/// It mirrors `kob_protocol::build::touch_of`. The token is that of the candidate's book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSrc {
    /// [`SIDE_ASK`] for a resting ask sold at its quote, or [`SIDE_BID`] for a resting bid bought
    /// at its quote.
    pub side: i64,
    /// The resting order's quote, in sompi per whole token.
    pub price: i64,
    /// The resting order's scale. A stop reads only evidence of its own scale.
    pub scale: i64,
    /// `max(UTXO DAA + interval, custody DAA, activeFrom)`, where the custody DAA counts for asks
    /// only.
    pub exposed_since: i64,
}

/// What an unarmed stop reads from its trigger evidence, the covenants' `touch`.
///
/// The evidence is a plain resting order of the stop's token and scale, filled in the same
/// transaction on the trigger side, at or through the stop. The fill is at least `min_touch` base
/// units, and the order was exposed for at least `min_rest` DAA before the lock time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchNeed {
    /// The evidence side.
    ///
    /// It is [`SIDE_ASK`] for a sell stop, a resting ask filled at or below `stop`. It is
    /// [`SIDE_BID`] for a buy stop, a resting bid filled at or above `stop`.
    pub side: i64,
    /// The stop price, in sompi per whole token.
    pub stop: i64,
    /// The stop's scale.
    pub scale: i64,
    /// `minTouch` (base units).
    pub min_touch: i64,
    /// `minRestDaa`.
    pub min_rest: i64,
}

/// Whether a fill of `n` base units of `src` is enough evidence, exposed long enough, at time `t`.
///
/// Enough is `minTouch`, and long enough is `minRestDaa`, as in
/// `kob_protocol::build::Touch::check`. The caller keeps the token and the scale equal.
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

/// A trailing stop's ratchet, as the conditional order's state and its `trail_steps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrailOf {
    /// A `KobCondAsk`, a sell stop. It ratchets up when a resting bid is filled.
    Ask(CondAskState),
    /// A `KobCondBid`, a buy stop. It ratchets down when a resting ask is filled.
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
    /// Steps that a quote `price` of the ratcheting side justifies, 0 for none.
    pub fn steps(&self, price: i64) -> i64 {
        match self {
            TrailOf::Ask(s) => s.trail_steps(price),
            TrailOf::Bid(s) => s.trail_steps(price),
        }
    }
}

/// A listed order that a batch may arm or ratchet without filling it, next to its evidence fill.
///
/// The batch calls the order's `update` entry (`BatchUpdate`), and the batch's change takes the
/// order's `keeperTip`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpCand {
    /// Covenant id of the order.
    pub id: CovId,
    /// The book the order is listed in.
    pub book: BookKey,
    /// The order UTXO.
    pub outpoint: Outpoint,
    /// The arm of a KAS conditional, by trigger-side evidence at or through the stop.
    ///
    /// None for a pair order.
    pub arm: Option<TouchNeed>,
    /// A trailing stop whose UTXO is `trailWait` old at `t` (CSV), ratcheted by opposite-side
    /// evidence.
    pub trail: Option<TrailOf>,
    /// A pair conditional or pair stop entry (`super::pair`).
    ///
    /// Its evidence is two KAS-book fills or a resting pair fill.
    pub pair: Option<std::sync::Arc<super::pair::PairUp>>,
    /// A booked exit's entry.
    ///
    /// The update never shares a transaction with it, because the exit refuses that (§6.1).
    pub parent: Option<CovId>,
    /// `keeperTip`, which the batch takes.
    pub tip: i64,
    /// UTXO value. The continuation keeps `value − tip`.
    pub value: i64,
    /// Estimated serialized bytes the update adds, for the order input and its continuation.
    pub bytes: u64,
    /// Measured marginal fee of one update at the minimum fee rate.
    ///
    /// It is the `updateFee` of the token program in `keeper_tips.json`.
    pub fee_floor: i64,
}

/// Inputs of candidate generation.
pub struct CandCtx<'a> {
    /// Lock time `t` (§3.3).
    pub t: i64,
    /// UTC clock, for day-order deadlines.
    pub utc: u64,
    /// Order ids spent by pending transactions or backed off. They are never candidates.
    pub excluded: &'a BTreeSet<CovId>,
    /// Every listed order by id. Merges look up the booked exit's entry here.
    pub by_id: &'a BTreeMap<CovId, ListedOrder>,
    /// Orders whose UTXO is not accepted yet, in a chained step. Their DAA score is unknown.
    pub unaccepted: &'a BTreeSet<CovId>,
    /// Pair auctions quoted at a tick earlier than `t`.
    ///
    /// The ticks come from the engine's earliest-tick search ([`super::auction`]) and are at most
    /// `t`. Only the quote of these orders moves, and their eligibility is judged at `t`. None
    /// quotes every order at `t`.
    pub pair_ticks: Option<&'a BTreeMap<CovId, i64>>,
}

impl CandCtx<'_> {
    /// Tick at which a pair order's auction is quoted: its entry of [`CandCtx::pair_ticks`], never
    /// after `t`.
    pub fn pair_tick(&self, id: &CovId) -> i64 {
        self.pair_ticks.and_then(|m| m.get(id)).map_or(self.t, |&x| x.min(self.t))
    }
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

/// Byte estimate of a transaction output: value, P2SH script and covenant binding.
pub const OUTPUT_BYTES: u64 = 90;
/// Byte estimate of a covenant input besides its redeem script: outpoint, sequence, budget, args.
pub const INPUT_BYTES: u64 = 180;
/// Byte estimate of the transaction envelope, the funding input and the change output.
pub const BASE_BYTES: u64 = 420;
/// Bytes of the leader's `next_states` per token output, a 112-byte state and its push.
pub const NEXT_STATE_BYTES: u64 = 115;

/// Bytes of a token input of `program`.
pub fn token_input_bytes(program: Option<TemplateId>) -> u64 {
    INPUT_BYTES + program.map(redeem_len).unwrap_or(2_500)
}

/// Whether the honest-matcher exclusions of §2.3 by time refuse a fill of the order at `t`.
///
/// They apply to fills only. Merges are always allowed.
pub fn excluded_by_expiry(o: &ListedOrder, t: i64, utc: u64) -> bool {
    // An unaccepted continuation of a chained step is accepted at or after `t`.
    let udaa = if o.utxo_daa() == u64::MAX { t } else { o.utxo_daa() as i64 };
    let due = o.order.state.refund_due(udaa).unwrap_or(i64::MAX);
    if t >= due {
        return true;
    }
    matches!(o.deadline, Some(d) if utc >= d)
}

/// Stops of the view that a batch may arm or ratchet with an `update`, next to an evidence fill.
///
/// These are unarmed stop legs, unarmed stop entries and trailing stops. Each is accepted with a
/// known DAA score at or before `t`, active, not expired, not excluded and in a book. The planner
/// decides whether a batch's evidence serves one.
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
        let Some(mut tip) = o.order.state.keeper_tip() else { continue };
        if let AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) = &o.order.state {
            // A buy stop pays its keeper only from what its buy does not need. Taken at the current
            // stop, this is a lower bound of what a ratchet frees.
            let Some(t) = s.keeper_take(o.order.utxo.amount.min(i64::MAX as u64) as i64, s.stop_price) else { continue };
            tip = t;
        }
        // The continuation, the order's value less the keeper's take, must clear the
        // covenant-output bound. Otherwise the builder refuses the whole transaction it is attached
        // to. This guards against a hostile `keeperTip` near the order's value.
        if o.order.utxo.amount.min(i64::MAX as u64) as i64 - tip < COV_OUTPUT_MIN {
            continue;
        }
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
        // The kind in the order's family and the build of its token program. KRON holders of
        // id_type 0 get `*KronD0`.
        kind: match o.order.state.token_tpl_hash() {
            Some(h) => kind.for_token_hash(o.order.state.family(), &h),
            None => kind.in_family(o.order.state.family()),
        },
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
        part_cap: i64::MAX,
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

/// Program of the order's token.
///
/// None if it is not a supported program of the order's family.
pub fn token_program(s: &AnyState) -> Option<TemplateId> {
    kob_protocol::artifacts::token_template_by_hash(&s.token_tpl_hash()?).filter(|t| t.family == s.family()).map(|t| t.id)
}

/// Smallest fill in `1..=cap` after which the bid with escrow `v` can no longer continue.
///
/// After such a fill less than one minimum fill of buying power is left. That is the covenant's
/// termination, and it is exempt from `minFill`. Returns `i64::MAX` when every fill within `cap`
/// leaves the bid able to continue. `can_continue(v − used(n))` only turns false as `n` grows, so a
/// bisection finds the fill.
pub fn bid_rest_from(s: &BidState, v: i64, cap: i64) -> i64 {
    let ends = |n: i64| s.used(n).and_then(|u| v.checked_sub(u)).is_none_or(|left| !s.can_continue(left));
    if cap <= 0 || !ends(cap) {
        return i64::MAX;
    }
    let (mut lo, mut hi) = (0i64, cap);
    // Invariant: lo is 0 or !ends(lo), and ends(hi).
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

/// Every candidate (order × leg) of the orders at `t`, in any number of books.
///
/// Orders are indexed as in `orders`.
pub fn candidates(orders: &[ListedOrder], cx: &CandCtx) -> Vec<Cand> {
    let refs: Vec<&ListedOrder> = orders.iter().collect();
    candidates_of(&refs, cx)
}

/// [`candidates`] over borrowed orders.
///
/// The global batch reads every book of the view through it without copying the book.
pub fn candidates_of(orders: &[&ListedOrder], cx: &CandCtx) -> Vec<Cand> {
    let mut out = vec![];
    let t = cx.t;
    for (idx, &o) in orders.iter().enumerate() {
        let id = o.id();
        if o.order.utxo.covenant_id.is_none() || cx.excluded.contains(&id) || !o.custody_ok() {
            continue;
        }
        // An order in no book, such as a pair order or an unknown extension, is no KAS-book
        // candidate.
        let Some(bk) = book_key(o) else { continue };
        let unaccepted = cx.unaccepted.contains(&id) || o.order.utxo.block_daa_score == u64::MAX;
        // An order UTXO accepted after `t` cannot prove activation or freshness at `t`.
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
        // The KRON kinds carry the typed states of their KCC-20 counterparts. Match on those.
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
                // The outputs a fill leaves must clear their KIP-9 dust bounds. These are a partial
                // fill's payout, continuation and custody.
                let cust = o.custody.as_ref().map(|x| x.utxo.amount.min(i64::MAX as u64) as i64).unwrap_or(0);
                if !c.set_rules(plain_kip9(&state, v, cust, q, c.rules())) {
                    continue;
                }
                if s.tif == TIF_FOK {
                    if c.cap < s.amount_left || !c.ends(s.amount_left) {
                        continue; // a FOK that cannot complete in one fill is never filled
                    }
                    c.fok = Some((s.amount_left, s.amount_left));
                }
                // Trigger evidence. The covenants read the DAA scores of the order and its custody
                // for the exposure.
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
                // The buying power keeps the maker's delivery carrier on the delivery output, since
                // a smaller output raises the storage mass, and it keeps the reserve. It is the
                // largest n whose consumed budget ceil(n·(pMax + tip)/scale) leaves both. A bid's
                // quantity is free, bounded only by its escrow, so it is capped at what token
                // amounts can hold.
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
                // The outputs a fill leaves must clear their KIP-9 dust bounds. These are the
                // continuation and the delivery.
                if !c.set_rules(plain_kip9(&state, v, 0, q, c.rules())) {
                    continue;
                }
                if s.tif == TIF_FOK {
                    // Complete means less than one minimum fill of buying power is left after the
                    // fill, as the covenant rules.
                    if c.rest_from > c.cap {
                        continue;
                    }
                    c.fok = Some((c.rest_from, c.cap));
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
                // KIP-9. A partial fill keeps the continuation (`v`) and the custody, and pays the
                // maker its proceeds less the merged entry's budget. The fill of everything left
                // also pays the carriers, to the entry when it merges.
                let cust = o.custody.as_ref().map(|x| x.utxo.amount.min(i64::MAX as u64) as i64).unwrap_or(0);
                let kip9 = |c: &mut Cand| -> bool {
                    let cv = c.clone();
                    let pay = |n: i64| {
                        let budget = if cv.merge.is_some() { s.rpt_budget(n)? } else { 0 };
                        cv.value(n)?.checked_sub(budget)
                    };
                    let carriers = if cv.merge.is_some() { 0 } else { v.saturating_add(cust) };
                    c.set_rules(kip9_limits(
                        cv.rules(),
                        v >= COV_OUTPUT_MIN && cust >= COV_OUTPUT_MIN,
                        |n| pay(n).is_some_and(|p| p >= KAS_OUTPUT_MIN),
                        |_| true,
                        |n| pay(n).is_some_and(|p| p.saturating_add(carriers) >= KAS_OUTPUT_MIN),
                    ))
                };
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
                                    // The merge argument -(k·2^53 + m) carries m < 2^53.
                                    c.cap = c.cap.min(MERGE_SHIFT - 1);
                                    c.chain_safe = false;
                                    c.bytes += INPUT_BYTES + rl(TemplateId::KobIfdBid) + OUTPUT_BYTES;
                                }
                                _ => ok = false,
                            },
                            _ => ok = false, // the entry is not visible (yet): no take-profit without its merge
                        }
                    }
                    if ok && kip9(&mut c) {
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
                        if kip9(&mut c) {
                            out.push(c);
                        }
                    } else if !unaccepted {
                        // A resting ask filled at or below the stop in the same batch triggers it.
                        // The planner checks that.
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
                        if kip9(&mut c) {
                            out.push(c);
                        }
                    }
                }
            }
            AnyState::KobCondBid(s) => {
                if t < s.active_from || s.amount_left <= 0 {
                    continue;
                }
                // KIP-9. A partial fill delivers on `deliveryCarrier` and keeps the continuation
                // `v - spend - deliveryCarrier`. The fill of everything left delivers `v - spend`.
                // A merge pays the entry instead, and the builder's rules apply to it.
                let kip9 = |c: &mut Cand| -> bool {
                    if c.merge.is_some() {
                        return true;
                    }
                    let cv = c.clone();
                    let left = |n: i64| s.spend(n, cv.quote).and_then(|sp| v.checked_sub(sp));
                    c.set_rules(kip9_limits(
                        cv.rules(),
                        s.delivery_carrier >= COV_OUTPUT_MIN,
                        |_| true,
                        |n| left(n).is_some_and(|l| l.saturating_sub(s.delivery_carrier) >= COV_OUTPUT_MIN),
                        |n| left(n).is_some_and(|l| l >= COV_OUTPUT_MIN),
                    ))
                };
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
                    // Largest affordable fill. The escrow covers the whole amount at the worst leg.
                    // The bisection takes O(log amountLeft) steps, whatever `amountLeft` claims.
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
                                    // The merge argument -(k·2^53 + m) carries m < 2^53.
                                    c.cap = c.cap.min(MERGE_SHIFT - 1);
                                    // The exit's continuation keeps what its remaining amount
                                    // needs, v − ceil(n·rptPrice/scale) − ceil(n·rptPre/scale) > 0.
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
                    if ok && c.cap > 0 && kip9(&mut c) {
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
                        if c.cap > 0 && kip9(&mut c) {
                            out.push(c);
                        }
                    } else if !unaccepted {
                        // A resting bid filled at or above the stop in the same batch triggers it.
                        // The planner checks that.
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
                        if c.cap > 0 && kip9(&mut c) {
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
                        // A buy-stop entry. A resting bid filled at or above its trigger in the
                        // same batch triggers it.
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
                // Affordability. The entry keeps a delivery carrier, and an exit carrier when it
                // continues.
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
                    // A booking (rptAmount > n) carries n < 2^53 in its exit's merge argument.
                    limit = limit.min(MERGE_SHIFT - 1);
                }
                if let Some(m) = rpt_fill_max(s.rpt_amount, s.min_fill) {
                    // While a minimum fill of re-arms is left, a fill takes at most the re-arms
                    // left, and it is booked.
                    limit = limit.min(m);
                }
                let n = largest_fit(limit, fits);
                c.cap = n;
                if n <= 0 {
                    continue;
                }
                // KIP-9. The delivery to the exit and the exit carrier are covenant outputs. So is
                // the continuation `v - spend - deliveryCarrier - exitCarrier` of an entry that
                // continues, after a partial fill or any fill of a repeating entry. The last fill
                // of a non-repeating entry puts `v - spend - deliveryCarrier` on its exit.
                let (dc, ec, rpt) = (s.delivery_carrier, s.exit_carrier, s.rpt_amount > 0);
                let left = |n: i64| s.spend(n, quote).and_then(|sp| v.checked_sub(sp)).map(|l| l.saturating_sub(dc));
                let cont_ok = |n: i64| left(n).is_some_and(|l| l.saturating_sub(ec) >= COV_OUTPUT_MIN);
                let fixed = dc >= COV_OUTPUT_MIN && ec >= COV_OUTPUT_MIN;
                let r = kip9_limits(
                    c.rules(),
                    fixed,
                    |_| true,
                    cont_ok,
                    |n| {
                        if rpt {
                            fixed && cont_ok(n)
                        } else {
                            dc >= COV_OUTPUT_MIN && left(n).is_some_and(|l| l >= COV_OUTPUT_MIN)
                        }
                    },
                );
                if !c.set_rules(r) {
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
                        // A sell-stop entry. A resting ask filled at or below its trigger in the
                        // same batch triggers it.
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
                // The entry continuation keeps its carrier:
                // v − ceil(n·prefund/scale) − exitCarrier > 0.
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
                if let Some(m) = rpt_fill_max(s.rpt_amount, s.min_fill) {
                    limit = limit.min(m);
                }
                let n = largest_fit(limit, fits);
                c.cap = n;
                if n <= 0 || s.proceeds(1, c.quote).is_none() {
                    continue;
                }
                // KIP-9. An entry that continues keeps `v - prefund - exitCarrier`, plus its
                // custody's carrier once it has sold everything. After a partial fill it also keeps
                // its custody. Its exit is a covenant output and holds the proceeds, the prefund
                // and the exit carrier. The last fill of a non-repeating entry puts everything on
                // its exit.
                let (ec, rpt, quote) = (s.exit_carrier, s.rpt_amount > 0, c.quote);
                let exit_ok = |n: i64| match (s.proceeds(n, quote), s.prefund_of(n)) {
                    (Some(p), Some(pre)) => p.saturating_add(pre).saturating_add(ec) >= COV_OUTPUT_MIN,
                    _ => false,
                };
                let keep = |n: i64, extra: i64| {
                    s.prefund_of(n).is_some_and(|pre| v.saturating_sub(pre).saturating_sub(ec).saturating_add(extra) >= COV_OUTPUT_MIN)
                };
                let r = kip9_limits(
                    c.rules(),
                    cust >= COV_OUTPUT_MIN,
                    exit_ok,
                    |n| keep(n, 0),
                    |n| {
                        if rpt {
                            keep(n, cust) && exit_ok(n)
                        } else {
                            s.proceeds(n, quote).is_some_and(|p| p.saturating_add(v).saturating_add(cust) >= COV_OUTPUT_MIN)
                        }
                    },
                );
                if !c.set_rules(r) {
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
            // `super::pair` plans the pair orders. The KRON kinds are normalised to their KCC-20
            // twins above.
            _ => {}
        }
    }
    // A fill must be worth something to its maker, so its all-in rate must be positive.
    out.retain(|c| c.cap > 0 && c.all_in() > 0 && c.scale > 0);
    out
}

/// Largest `n` in `0..=max` for which `fits(n)` holds.
///
/// `fits` is the covenant's affordability test of a fill of `n` base units. It is evaluated at
/// `max` itself, where it may differ because the fill that takes everything skips the continuation
/// carriers. Below `max` it is monotone in `n`, a cost that only grows with `n`. So a bisection
/// finds the answer in O(log max) evaluations, whatever an order's amount claims.
pub fn largest_fit(max: i64, fits: impl Fn(i64) -> bool) -> i64 {
    if max <= 0 {
        return 0;
    }
    if fits(max) {
        return max;
    }
    // Below `max` the test is monotone in n, true up to a threshold.
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

/// Least KAS that a plain output of a fill can hold, such as a maker's P2PK payout.
///
/// Below it the output's KIP-9 storage mass alone exceeds the block limit
/// ([`kob_protocol::tx::DUST_OUTPUT_MIN`]).
pub const KAS_OUTPUT_MIN: i64 = kob_protocol::tx::DUST_OUTPUT_MIN as i64;
/// Least KAS that an output with a covenant id can hold, four times the plain bound.
///
/// Such outputs are order continuations, custodies, deliveries and exits
/// ([`kob_protocol::tx::COVENANT_OUTPUT_MIN`]).
pub const COV_OUTPUT_MIN: i64 = kob_protocol::tx::COVENANT_OUTPUT_MIN as i64;

/// Quantity rules of an order's fills.
///
/// They are the most it takes now, its minimum fill, the smallest fill that ends it and the largest
/// fill that does not. Each is the [`Cand`] field of the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QtyRules {
    /// Most base units fillable now ([`Cand::cap`]).
    pub cap: i64,
    /// Least base units of a fill that does not end the order ([`Cand::min_fill`]).
    pub min_fill: i64,
    /// Smallest fill that ends the order ([`Cand::rest_from`]).
    pub rest_from: i64,
    /// Largest fill that does not end the order ([`Cand::part_cap`]).
    pub part_cap: i64,
}

/// Quantity rules under which the outputs of an order's fills clear their KIP-9 dust bounds.
///
/// See `docs/spec/matcher.md` §2.2. Consensus refuses a transaction with an output below its
/// bound, which is 0.02 KAS for a plain output and 0.08 KAS for one with a covenant id. So the
/// builder refuses the fill, and a planner that ignored the bound would plan that fill again every
/// tick.
///
/// The tests are:
/// - `part_fixed`: the outputs that a partial fill leaves at a value independent of the fill clear
///   their bounds. A partial fill does not end the order. These outputs are the order UTXO's
///   carrier on a continuation, a remaining custody and a delivery carrier.
/// - `part_up(n)`: the outputs of a partial fill of `n` that grow with `n`, such as the maker's
///   payout, clear theirs. It is false below a threshold and true above it.
/// - `part_down(n)`: the outputs that shrink with `n`, such as a bid's or an entry's continuation,
///   clear theirs. It is true up to a threshold.
/// - `end_ok(n)`: the outputs of a fill of `n` that ends the order clear theirs. It is true up to
///   a threshold.
///
/// The partial fills become `[max(minFill, threshold of part_up), threshold of part_down]`, and
/// `part_cap` is 0 when there are none. The fills that end the order become
/// `[rest_from, threshold of end_ok]`. When there are none, `rest_from` is `i64::MAX` and the cap
/// falls to `part_cap`. Returns `None` when no fill is left.
pub fn kip9_limits(
    r: QtyRules,
    part_fixed: bool,
    part_up: impl Fn(i64) -> bool,
    part_down: impl Fn(i64) -> bool,
    end_ok: impl Fn(i64) -> bool,
) -> Option<QtyRules> {
    let mut r = r;
    let hi = r.cap.min(r.rest_from.saturating_sub(1)).min(r.part_cap);
    let mut part_cap = 0;
    if part_fixed && hi >= 1 {
        let top = last_true(1, hi, &part_down);
        if top >= 1 && part_up(top) {
            let lo = first_true(1, top, &part_up);
            if r.min_fill.max(lo) <= top {
                r.min_fill = r.min_fill.max(lo);
                part_cap = top;
            }
        }
    }
    r.part_cap = part_cap;
    if r.rest_from <= r.cap {
        let e = last_true(r.rest_from, r.cap, &end_ok);
        if e >= r.rest_from {
            r.cap = e;
            return Some(r);
        }
        r.rest_from = i64::MAX;
    }
    r.cap = r.cap.min(part_cap);
    (r.cap > 0).then_some(r)
}

/// Largest `n` in `lo..=hi` with `f(n)`, for an `f` true up to a threshold, or `lo − 1` when none.
fn last_true(lo: i64, hi: i64, f: &impl Fn(i64) -> bool) -> i64 {
    if hi < lo || !f(lo) {
        return lo - 1;
    }
    if f(hi) {
        return hi;
    }
    let (mut a, mut b) = (lo, hi); // f(a), !f(b)
    while b - a > 1 {
        let m = a + (b - a) / 2;
        if f(m) {
            a = m;
        } else {
            b = m;
        }
    }
    a
}

/// Smallest `n` in `lo..=hi` with `f(n)`, for an `f` false below a threshold and `f(hi)` true.
fn first_true(lo: i64, hi: i64, f: &impl Fn(i64) -> bool) -> i64 {
    if f(lo) {
        return lo;
    }
    let (mut a, mut b) = (lo, hi); // !f(a), f(b)
    while b - a > 1 {
        let m = a + (b - a) / 2;
        if f(m) {
            b = m;
        } else {
            a = m;
        }
    }
    b
}

/// [`kip9_limits`] of a plain `KobAsk` or `KobBid` of either family at `quote`.
///
/// The quote is in sompi per whole token, `v` is the order UTXO's value and `cust` its custody's.
/// The tests mirror the builder's outputs (`Leg::Ask` and `Leg::Bid` in `kob_protocol::build`):
/// - A GTC ask's partial fill keeps its continuation (`v`) and its custody (`cust`), and pays the
///   maker its proceeds. An IOC returns the custody to the maker and pays the proceeds with `v`.
///   The fill of everything left pays all three.
/// - A bid's fill delivers the tokens on `deliveryCarrier + used(n) − spend(n)`. A GTC bid keeps
///   the continuation `v − used(n) − deliveryCarrier` while it can continue. A fill that ends it,
///   and any IOC or FOK fill, delivers `v − spend(n)`.
pub fn plain_kip9(s: &AnyState, v: i64, cust: i64, quote: i64, r: QtyRules) -> Option<QtyRules> {
    match s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => {
            let pay = |n: i64| a.proceeds_at(n, quote);
            let gtc = a.tif == TIF_GTC;
            kip9_limits(
                r,
                cust >= COV_OUTPUT_MIN && (!gtc || v >= COV_OUTPUT_MIN),
                |n| pay(n).is_some_and(|p| p.saturating_add(if gtc { 0 } else { v }) >= KAS_OUTPUT_MIN),
                |_| true,
                |n| pay(n).is_some_and(|p| p.saturating_add(v).saturating_add(cust) >= KAS_OUTPUT_MIN),
            )
        }
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => {
            let spend = |n: i64| b.spend_at(n, quote);
            let used = |n: i64| b.used(n);
            let closing = |n: i64| spend(n).is_some_and(|sp| v.saturating_sub(sp) >= COV_OUTPUT_MIN);
            if b.tif != TIF_GTC {
                // An IOC or FOK bid never continues. Every fill delivers what its escrow keeps.
                return kip9_limits(r, true, |_| true, closing, closing);
            }
            kip9_limits(
                r,
                true,
                |n| match (used(n), spend(n)) {
                    (Some(u), Some(sp)) => b.delivery_carrier.saturating_add(u - sp) >= COV_OUTPUT_MIN,
                    _ => false,
                },
                |n| used(n).is_some_and(|u| v.saturating_sub(u).saturating_sub(b.delivery_carrier) >= COV_OUTPUT_MIN),
                closing,
            )
        }
        _ => Some(r),
    }
}

impl Cand {
    fn rules(&self) -> QtyRules {
        QtyRules { cap: self.cap, min_fill: self.min_fill, rest_from: self.rest_from, part_cap: self.part_cap }
    }
    /// Takes the KIP-9 limits ([`kip9_limits`]) into the candidate. False when no fill is left.
    fn set_rules(&mut self, r: Option<QtyRules>) -> bool {
        match r {
            Some(r) => {
                (self.cap, self.min_fill, self.rest_from, self.part_cap) = (r.cap, r.min_fill, r.rest_from, r.part_cap);
                true
            }
            None => false,
        }
    }
}

/// Whether a repeating entry can be merged.
///
/// It must be listed, not spent by a pending transaction, accepted at or before `t`, and have a
/// valid custody.
fn merge_entry_ok(e: &ListedOrder, cx: &CandCtx) -> bool {
    !cx.excluded.contains(&e.id())
        && e.custody_ok()
        && e.order.utxo.block_daa_score != u64::MAX
        && !cx.unaccepted.contains(&e.id())
        && e.order.utxo.block_daa_score as i64 <= cx.t
}
