//! The earliest auction tick at which a batch still pays (`docs/spec/matcher.md` §3.3).
//!
//! Every auction covenant takes its auction time `t` as an argument of the fill. It checks
//! `origin <= t` and `tx.daa >= t` (CLTV), so `t` is a lower bound of the current DAA score that
//! the transaction proves. Each auction leg of one transaction may carry its own `t`. The builders
//! check `origin <= t <= lockTime` (`kob_protocol::build`).
//!
//! The planner allocates at `t = lockTime`, the tick its walks cross at ([`super::batch`]). The
//! engine then prices the auction legs of the built batch at the earliest ticks at which it still
//! pays its fee and the operator's `min_profit`. A rising bid then pays the lowest price that
//! covers the batch's costs and a decaying ask receives the highest, so a market order fills at
//! about its start price whatever the matcher's latency.
//!
//! * **KAS-book auctions** ([`KasAuction`]) are decaying `KobAsk`, rising `KobBid`, TWAP / DCA
//!   slices, armed stop legs of `KobCondAsk` / `KobCondBid` and armed stop entries of `KobIfdBid` /
//!   `KobIfdAsk` while their band opens. They keep the allocation: the same fills of the same
//!   orders, only their quotes move. Moving a leg's `t` changes only the KAS that leg pays or
//!   receives. The operator's change takes the difference and every output of the maker grows. So
//!   [`earliest_ticks`] prices them with the covenants' own amounts against the batch's exact
//!   surplus.
//! * **Pair auctions** ([`pair_auction_from`]) are a `KobPair` market or Dutch order, an armed
//!   `KobCondPair` stop leg and a `KobIfdPair` stop entry. They move token amounts, which the batch
//!   must route again. The engine plans the batch again with their quotes at earlier ticks
//!   ([`super::candidate::CandCtx::pair_ticks`]). It keeps the earliest tick at which the planner
//!   still fills them in full and the batch pays.
//!
//! Several auctions of one batch move together, at the same progress `e` from their earliest ticks:
//! `t = min(lockTime, from + e)`, with the smallest `e` that pays. This is one rule for every
//! auction and a pure function of the plan and the batch's surplus. It is monotone, since every
//! leg's KAS only grows with its `t`, so the search is a bisection.

use std::collections::BTreeMap;

use kob_protocol::state::*;

use super::book::{CovId, ListedOrder};
use super::candidate::Side;
use super::planner::{Fill, Plan};

/// The quote function of an auction leg (the KCC-20 state of its order).
#[derive(Clone, Debug)]
enum Quote {
    Ask(AskState),
    Bid(BidState),
    CondAsk(CondAskState),
    CondBid(CondBidState),
    IfdBid(IfdBidState),
    IfdAsk(IfdAskState),
}

/// A KAS-book auction leg of a plan: a fill whose quote moves with its auction time.
#[derive(Clone, Debug)]
pub struct KasAuction {
    /// Index of the fill in [`Plan::fills`].
    pub fill: usize,
    /// The earliest tick the covenant accepts: the auction's origin, never before `activeFrom`.
    pub from: i64,
    quote: Quote,
    udaa: i64,
}

impl KasAuction {
    /// The leg's quote at auction time `t` (`None` where the covenant's arithmetic fails).
    pub fn quote_at(&self, t: i64) -> Option<i64> {
        let u = self.udaa;
        match &self.quote {
            Quote::Ask(s) => s.price_at(t, u),
            Quote::Bid(s) => s.price_at(t, u),
            Quote::CondAsk(s) => s.stop_at(false, t, u),
            Quote::CondBid(s) => s.stop_at(false, t, u),
            Quote::IfdBid(s) => s.price_at(false, t, u),
            Quote::IfdAsk(s) => s.price_at(false, t, u),
        }
    }
}

/// The KAS-book auction of a fill, if its quote moves with `t`. `lock` is the batch's lock time.
///
/// These fills have no auction:
///
/// - a pair order,
/// - a stop triggered in the batch, which trades at its trigger,
/// - a fill that is trigger evidence,
/// - an unarmed stop or a stop whose band is open at once,
/// - an order whose UTXO is an unaccepted continuation, because its origin is not known yet.
pub fn kas_auction(k: usize, f: &Fill, o: &ListedOrder, lock: i64) -> Option<KasAuction> {
    if f.cand.pair.is_some() || f.evidence.is_some() || f.cand.trigger.is_some() || o.utxo_daa() == u64::MAX {
        return None;
    }
    let udaa = i64::try_from(o.utxo_daa()).ok()?;
    let armed = |armed: i64, band: i64, active_from: i64| -> Option<i64> {
        (armed != 0 && band > 0).then(|| armed_origin(armed, udaa).map(|x| x.max(active_from))).flatten()
    };
    let (from, quote) = match o.base_state() {
        AnyState::KobAsk(s) if s.slope != 0 => (s.origin(udaa)?.max(s.active_from), Quote::Ask(s)),
        AnyState::KobBid(s) if s.slope != 0 => (s.origin(udaa)?.max(s.active_from), Quote::Bid(s)),
        AnyState::KobCondAsk(s) if f.cand.leg == 1 => (armed(s.armed, s.band_daa, s.active_from)?, Quote::CondAsk(s)),
        AnyState::KobCondBid(s) if f.cand.leg == 1 => (armed(s.armed, s.band_daa, s.active_from)?, Quote::CondBid(s)),
        AnyState::KobIfdBid(s) if s.entry_stop > 0 => (armed(s.armed, s.band_daa, s.active_from)?, Quote::IfdBid(s)),
        AnyState::KobIfdAsk(s) if s.entry_stop > 0 => (armed(s.armed, s.band_daa, s.active_from)?, Quote::IfdAsk(s)),
        _ => return None,
    };
    (from <= lock).then_some(KasAuction { fill: k, from, quote, udaa })
}

/// The earliest tick of a pair order's auction fill, if its quote moves with `t`.
///
/// The fill is the primary leg of a pair order in a plan. The orders are a `KobPair` market or
/// Dutch order, and an armed `KobCondPair` stop leg or `KobIfdPair` stop entry whose band opens
/// over time.
pub fn pair_auction_from(f: &Fill, o: &ListedOrder, lock: i64) -> Option<i64> {
    if f.cand.pair.is_none() || f.evidence.is_some() || o.utxo_daa() == u64::MAX {
        return None;
    }
    let udaa = i64::try_from(o.utxo_daa()).ok()?;
    let armed = |armed: i64, band: i64, active_from: i64| -> Option<i64> {
        (armed != 0 && band > 0).then(|| armed_origin(armed, udaa).map(|x| x.max(active_from))).flatten()
    };
    let from = match &o.order.state {
        AnyState::KobPair(s) if s.slope != 0 => s.origin(udaa)?.max(s.active_from),
        AnyState::KobCondPair(s) if f.cand.leg == 1 => armed(s.armed, s.band_daa, s.active_from)?,
        AnyState::KobIfdPair(s) if s.entry_stop > 0 => armed(s.armed, s.band_daa, s.active_from)?,
        _ => return None,
    };
    (from <= lock).then_some(from)
}

/// The tick of an auction at progress `e` from its earliest tick, never after the lock time.
pub fn tick_at(from: i64, e: i64, lock: i64) -> i64 {
    from.saturating_add(e).min(lock)
}

/// The smallest `e` in `0..=span` for which `ok(e)` holds.
///
/// `ok` must be monotone (false below some point, true from it on), and `ok(span)` is assumed true.
/// Returns `None` when `ok` fails everywhere below `span`, which leaves the lock time in place.
pub fn first_paying(span: i64, mut ok: impl FnMut(i64) -> bool) -> Option<i64> {
    let (mut lo, mut hi) = (0i64, span);
    let mut best = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if ok(mid) {
            best = Some(mid);
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    best
}

/// What a leg moves into the operator's change at quote `q` for its fill.
///
/// A bid's payment is positive and an ask's proceeds are negative. The amounts are the covenant's
/// own ([`super::candidate::Cand::value`]).
fn kas_of(f: &Fill, q: i64) -> Option<i64> {
    let c = super::candidate::Cand { quote: q, ..f.cand.clone() };
    let v = c.value(f.amount)?;
    Some(if c.side == Side::Bid { v } else { -v })
}

/// The plan with its KAS-book auction legs moved to the earliest ticks at which the batch still
/// pays.
///
/// Every leg moves by the same progress `e` from its earliest tick (`t = min(lock, from + e)`). `e`
/// is the smallest value at which what the legs give up against their lock-time quotes stays within
/// `budget`, the batch's exact surplus above `min_profit` in sompi. Each moved fill carries its
/// tick ([`Fill::t`]) and its quote there, and the plan's margin drops by what the legs give up.
/// Returns `None` when no leg moves: there is no auction leg, no surplus, or none pays below the
/// lock time.
pub fn earliest_ticks(plan: &Plan, by_id: &BTreeMap<CovId, ListedOrder>, lock: u64, budget: i64) -> Option<Plan> {
    let lock = i64::try_from(lock).ok()?;
    if budget <= 0 {
        return None;
    }
    let legs: Vec<KasAuction> = plan
        .fills
        .iter()
        .enumerate()
        .filter(|(_, f)| f.t.is_none())
        .filter_map(|(k, f)| kas_auction(k, f, by_id.get(&f.cand.id)?, lock))
        .collect();
    // what each leg moves at the lock time (its planned quote)
    let base: Vec<i64> = legs.iter().map(|a| kas_of(&plan.fills[a.fill], a.quote_at(lock)?)).collect::<Option<_>>()?;
    let span = legs.iter().map(|a| lock - a.from).max()?;
    // what the legs give up at progress e (None: a quote the covenant cannot compute there)
    let given_up = |e: i64| -> Option<i64> {
        let mut sum = 0i64;
        for (a, b) in legs.iter().zip(&base) {
            let v = kas_of(&plan.fills[a.fill], a.quote_at(tick_at(a.from, e, lock))?)?;
            sum = sum.checked_add(b.checked_sub(v)?)?;
        }
        Some(sum)
    };
    let e = first_paying(span, |e| given_up(e).is_some_and(|g| g <= budget))?;
    let mut out = plan.clone();
    let mut moved = false;
    for a in &legs {
        let t = tick_at(a.from, e, lock);
        if t >= lock {
            continue;
        }
        let f = &mut out.fills[a.fill];
        f.cand.quote = a.quote_at(t)?;
        f.t = Some(t as u64);
        moved = true;
    }
    out.margin = out.margin.checked_sub(given_up(e)?)?;
    moved.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_paying_progress_is_found_by_bisection() {
        // feasible from 37 on
        for span in [37i64, 38, 200, 1_000_000] {
            let mut calls = 0;
            assert_eq!(
                first_paying(span, |e| {
                    calls += 1;
                    e >= 37
                }),
                (span > 37).then_some(37)
            );
            assert!(calls <= 64 - span.leading_zeros() as usize + 1, "a bisection: {calls} calls over {span}");
        }
        // nothing below the span pays: the lock time stays
        assert_eq!(first_paying(200, |_| false), None);
        assert_eq!(first_paying(0, |_| true), None, "no auction time before the lock time");
        // everything pays: the auction's earliest tick
        assert_eq!(first_paying(200, |_| true), Some(0));
    }

    #[test]
    fn a_tick_is_never_before_its_earliest_nor_after_the_lock_time() {
        assert_eq!(tick_at(1_000, 0, 1_200), 1_000);
        assert_eq!(tick_at(1_000, 150, 1_200), 1_150);
        assert_eq!(tick_at(1_000, 500, 1_200), 1_200);
        assert_eq!(tick_at(i64::MAX - 1, 5, i64::MAX), i64::MAX);
    }
}
