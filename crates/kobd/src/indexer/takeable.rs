//! What one fill can take now from an order of a book, and what the taker pays for it (R5A-1, R6C2a-2).
//!
//! A book shows an order a taker cannot fill now nowhere, and an amount only as far as a taker can
//! really get it. The writer decides both with the matcher's own rules, never a copy of them:
//! [`kas_offer`] runs `matcher::candidate::candidates_of` and [`pair_offer`] `matcher::pair::infos`
//! on the order as the matcher sees it (`indexer::book::listed_order_of`): every fill condition of its
//! covenant (minimum and maximum fill against the amount left, the tip against the quote, a curve's
//! levels and its walk, the escrow, prefund and carriers of a bid or an if-done entry) and the KIP-9
//! minimum of every output the fill builds. So a book and an executor never disagree on what is
//! fillable.
//!
//! [`kas_takeable`] is kept for the listing at placement, before the order's rows exist: an if-done
//! entry's listing value is measured on what its escrow pays (`processor::listing`).

use crate::matcher::candidate::largest_fit;
use kob_protocol::state::AnyState;

/// The largest fill in `1..=max` that `afford` accepts, with the minimum fill (`n >= min_fill`
/// unless it takes `max`); 0 when there is none. `afford` is monotone below `max`.
fn cap(max: i64, min_fill: i64, afford: impl Fn(i64) -> bool) -> i64 {
    if max <= 0 {
        return 0;
    }
    if afford(max) {
        return max;
    }
    let min_fill = min_fill.max(1);
    let n = largest_fit(max - 1, |n| n < min_fill || afford(n));
    if n >= min_fill && afford(n) {
        n
    } else {
        0
    }
}

/// A KAS-book kind's fill now at quote `p` from an order UTXO of `value` sompi: `Some` for the
/// if-done entries, `None` for every other kind, whose amount is funded as stored.
///
/// A repeating sell-first entry that sells out also keeps its custody's carrier; it is not counted
/// here, so the bound is the conservative one (an honest entry's prefund covers it without).
pub(crate) fn kas_takeable(s: &AnyState, value: i64, p: i64) -> Option<i64> {
    let v = value as i128;
    match s {
        AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => {
            let (dc, ec) = (b.delivery_carrier as i128, b.exit_carrier as i128);
            Some(cap(b.amount_left, b.min_fill, |n| {
                let Some(spend) = b.spend(n, p) else { return false };
                let left = v - spend as i128 - dc;
                if n < b.amount_left || b.rpt_amount > 0 {
                    left - ec > 0
                } else {
                    left > 0
                }
            }))
        }
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => {
            if a.proceeds(1, p).is_none() {
                return Some(0);
            }
            Some(cap(a.amount_left, a.min_fill, |n| {
                if n < a.amount_left || a.rpt_amount > 0 {
                    a.prefund_of(n).is_some_and(|pre| v - pre as i128 - a.exit_carrier as i128 > 0)
                } else {
                    true
                }
            }))
        }
        _ => None,
    }
}

/// What one fill can take now from a listed order of a KAS book, and what the taker pays for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Offer {
    /// Base units one fill (one transaction) can take: a curve's reachable levels summed.
    pub amount: i64,
    /// The sompi the taker pays (an ask) or gets (a bid) for them, at their quotes, rounded down.
    pub value: i128,
    /// The smallest fill a taker can make (a curve: its minimum fill as KIP-9 raises it, R7C2a-2); 0 when none.
    pub take_min: i64,
}

/// The [`Offer`] of a listed order of a KAS book at DAA `t`, by the matcher's own candidate rules
/// (`matcher::candidate::candidates_of`, R6C2a-2): every fill condition of the order's covenant
/// (the minimum and maximum fill against the amount left, the tip against the quote, a curve's
/// levels and its walk of at most `CURVE_MAX_CROSS` levels, the escrow, prefund and carriers of a
/// bid or an if-done entry) and the KIP-9 minimum of every output the fill builds (R6C2a-I1). An
/// order no fill can take has an amount of 0. The day-order deadline is the window's, not this.
pub(crate) fn kas_offer(o: &crate::matcher::book::ListedOrder, t: i64) -> Offer {
    use crate::matcher::candidate::{candidates_of, CandCtx};
    let (excluded, unaccepted) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    let by_id = std::collections::BTreeMap::new();
    let cx = CandCtx { t, utc: 0, excluded: &excluded, by_id: &by_id, unaccepted: &unaccepted, pair_ticks: None };
    let mut out = Offer::default();
    for c in candidates_of(&[o], &cx) {
        if out.take_min == 0 {
            // a curve's smallest valid fill is the smallest `n` its `CurveLeg::fill_ok` accepts (R8C2a-1): its minimum
            // fill when KIP-9 leaves a partial fill of that size, else the whole rest
            out.take_min = c.curve.as_ref().map_or(c.min_fill, |l| smallest_fill(&l.rules)).max(1);
        }
        let v = kob_protocol::state::quote_exact(c.cap, c.quote, c.scale, kob_protocol::state::Round::Down).unwrap_or(0);
        out.amount = out.amount.saturating_add(c.cap);
        out.value = out.value.saturating_add(v.max(0));
    }
    out
}

/// The smallest fill `CurveLeg::fill_ok` accepts under `r`: the minimum fill when a partial fill of that size exists
/// (`part_cap >= min_fill`), else the fill that ends the order (`rest_from`).
pub(crate) fn smallest_fill(r: &crate::matcher::candidate::QtyRules) -> i64 {
    if r.part_cap >= r.min_fill {
        r.min_fill
    } else {
        r.rest_from
    }
}

/// The largest fill of a listed pair order at DAA `t` and its quote then, by the matcher's pair rules
/// (`matcher::pair::infos`); `None` when no fill can take anything.
pub(crate) fn pair_offer(o: &crate::matcher::book::ListedOrder, t: i64) -> Option<(i64, i64)> {
    use crate::matcher::candidate::CandCtx;
    let (excluded, unaccepted) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    let by_id = std::collections::BTreeMap::new();
    let cx = CandCtx { t, utc: 0, excluded: &excluded, by_id: &by_id, unaccepted: &unaccepted, pair_ticks: None };
    crate::matcher::pair::infos(o, &cx).into_iter().filter(|i| i.cap > 0).map(|i| (i.cap, i.price)).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{ifd_ask, ifd_bid, WHOLE};
    use kob_protocol::state::{IfdAskState, IfdBidState};

    /// An entry placed with the escrow the protocol computes takes its whole amount; one placed with
    /// a carrier only takes nothing; one in between takes what its escrow pays, from the minimum
    /// fill up.
    #[test]
    fn an_if_done_bid_takes_what_its_escrow_pays() {
        let b = ifd_bid(1, 10);
        let full = b.escrow().unwrap();
        let s = AnyState::KobIfdBid(b.clone());
        assert_eq!(kas_takeable(&s, full, b.price), Some(10 * WHOLE));
        assert_eq!(kas_takeable(&s, 8_000_000, b.price), Some(0), "a carrier only");
        // about three whole tokens' worth plus the carriers of one fill
        let three = b.spend(3 * WHOLE, b.price).unwrap() + b.delivery_carrier + b.exit_carrier + 1;
        assert_eq!(kas_takeable(&s, three, b.price), Some(3 * WHOLE));
        // the minimum fill is the whole amount: nothing less is a fill
        let one = IfdBidState { min_fill: 10 * WHOLE, ..b.clone() };
        assert_eq!(kas_takeable(&AnyState::KobIfdBid(one), three, b.price), Some(0));
        // the last fill of a non-repeating entry keeps no exit carrier
        let last = b.spend(10 * WHOLE, b.price).unwrap() + b.delivery_carrier + 1;
        assert_eq!(kas_takeable(&s, last, b.price), Some(10 * WHOLE));
        let rpt = AnyState::KobIfdBid(IfdBidState { rpt_amount: 1, ..b.clone() });
        assert!(kas_takeable(&rpt, last, b.price).unwrap() < 10 * WHOLE, "a repeating entry continues, so it keeps one");
    }

    /// A non-repeating sell-first entry sells out whatever its prefund; a fill it outlives moves the
    /// prefund and an exit carrier out of its KAS.
    #[test]
    fn an_if_done_ask_takes_what_its_prefund_allows() {
        let a = ifd_ask(1);
        let s = AnyState::KobIfdAsk(a.clone());
        assert_eq!(kas_takeable(&s, 1, a.price), Some(a.amount_left), "a sell-out takes no prefund from the entry");
        let rpt = IfdAskState { rpt_amount: 1, ..a.clone() };
        let r = AnyState::KobIfdAsk(rpt.clone());
        assert_eq!(kas_takeable(&r, 1, a.price), Some(0));
        let full = rpt.escrow(8_000_000).unwrap();
        assert_eq!(kas_takeable(&r, full, a.price), Some(a.amount_left));
    }
}
