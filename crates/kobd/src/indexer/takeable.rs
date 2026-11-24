//! What one fill can take now from an order whose stated amount its UTXOs need not fund (R5A-1).
//!
//! A book shows an order a taker cannot fill now nowhere, and an amount only as far as a taker can
//! really get it. Most kinds fund their amount by construction: an ask's custody holds it exactly
//! (`reads::CUSTODY_OK`, `pairs::pair_custody_ok`), and a plain bid's quantity is its buying power.
//! The kinds below state an amount that the covenant pays for out of a value the placer chose:
//! - `KobIfdBid` (and KRON) pays the seller from its KAS escrow and keeps a delivery and an exit
//!   carrier (`KobIfdBid.sil` fill, `build::plan_ifd_bid`).
//! - `KobIfdAsk` (and KRON) moves the prefund and an exit carrier of each fill that it outlives
//!   out of its KAS (`KobIfdAsk.sil` settle, `build::plan_ifd_ask`).
//! - `KobPair` pays a bid's fill from its B custody and funds the delivery carrier and the tip of a
//!   partial fill (`PairState::fill`).
//! - `KobIfdPair` pays a buy-first fill from its B custody, a sell-first one's prefund too, and
//!   keeps the carriers and the tip in KAS (the matcher's `pair` rules).
//!
//! The answer is the largest single fill the covenant accepts at quote `p`, with the minimum fill:
//! the whole amount when that fits, else the largest partial fill, else 0. The arithmetic is the
//! matcher's (`matcher::candidate`, `matcher::pair`), so a book never shows more than an executor
//! would fill. It leaves out the per-fill caps of a repeating entry's re-arms, which bound one fill
//! and not what the order holds.

use crate::matcher::candidate::{largest_fit, COV_OUTPUT_MIN};
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

/// A pair kind's fill now at quote `p` (B per whole A) from an order UTXO of `value` sompi whose
/// live custodies are `custodies` (token, amount, carrier): `Some` for `KobPair` and `KobIfdPair`.
pub(crate) fn pair_takeable(s: &AnyState, value: i64, custodies: &[(Vec<u8>, i64, i64)], p: i64) -> Option<i64> {
    let carrier = |token: &[u8; 32]| custodies.iter().find(|c| c.0 == token[..]).map(|c| c.2 as i128).unwrap_or(0);
    let v = value as i128;
    match s {
        AnyState::KobPair(x) => {
            if p <= 0 {
                return Some(0);
            }
            Some(x.max_takeable(p, value))
        }
        AnyState::KobIfdPair(x) => {
            if p <= 0 {
                return Some(0);
            }
            let buy = x.is_buy_first();
            let tk = x.tokens();
            let a_car = if buy { 0 } else { carrier(&tk.a.cov_id) };
            let b_car = carrier(&tk.b.cov_id);
            let dust = COV_OUTPUT_MIN as i128;
            Some(cap(x.amount_left, x.min_fill, |n| {
                let Some(tip) = x.tip_kas(n) else { return false };
                let cont = x.amount_left - n > 0 || x.rpt_amount > 0;
                let b_new = if buy {
                    match x.spend(n, p) {
                        Some(sp) if sp <= x.custody => x.custody - sp,
                        _ => return false,
                    }
                } else {
                    if x.proceeds(n, p).is_none() {
                        return false;
                    }
                    match if cont { x.pre_of(n) } else { Some(x.custody) } {
                        Some(u) if u <= x.custody => x.custody - u,
                        _ => return false,
                    }
                };
                let (dc, ec, tip) = (x.delivery_carrier as i128, x.exit_carrier as i128, tip as i128);
                if cont {
                    let mut keep = v - dc - ec - tip;
                    if !buy && x.amount_left - n == 0 {
                        keep += a_car;
                    }
                    if x.custody > 0 && b_new == 0 {
                        keep += b_car;
                    }
                    keep >= dust
                } else {
                    let mut left = v + a_car - dc - tip;
                    if b_new == 0 {
                        left += b_car;
                    }
                    left >= dust
                }
            }))
        }
        _ => None,
    }
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

    /// The pair state a template compiles with, as a starting point.
    fn compiled(id: kob_protocol::artifacts::TemplateId) -> AnyState {
        let t = kob_protocol::artifacts::template(id);
        AnyState::decode(id, &t.contract().compiled.bytecode[1..1 + t.state_len]).unwrap()
    }

    /// A buy-first pair entry or a pair bid pays from its B custody: with none (or one base unit)
    /// it takes nothing, however large the amount it states; with the custody its builder computes
    /// it takes its whole amount.
    #[test]
    fn a_pair_bid_takes_what_its_b_custody_pays() {
        let AnyState::KobIfdPair(e) = compiled(kob_protocol::artifacts::TemplateId::KobIfdPair) else { panic!() };
        let e = kob_protocol::state::IfdPairState {
            side: kob_protocol::state::SIDE_BID,
            amount_left: 1_000_000,
            min_fill: 1_000_000,
            price: 5_000,
            a_scale: 1_000,
            tip: 0,
            custody: 0,
            delivery_carrier: 0,
            exit_carrier: 0,
            rpt_amount: 0,
            ..e
        };
        let v = 10 * COV_OUTPUT_MIN;
        assert_eq!(pair_takeable(&AnyState::KobIfdPair(e.clone()), v, &[], e.price), Some(0));
        let funded = kob_protocol::state::IfdPairState { custody: e.b_custody_needed().unwrap(), ..e.clone() };
        let b = funded.tokens().b.cov_id.to_vec();
        let rows = [(b.clone(), funded.custody, COV_OUTPUT_MIN)];
        assert_eq!(pair_takeable(&AnyState::KobIfdPair(funded.clone()), v, &rows, e.price), Some(1_000_000));

        let AnyState::KobPair(p) = compiled(kob_protocol::artifacts::TemplateId::KobPair) else { panic!() };
        let p = kob_protocol::state::PairState {
            side: kob_protocol::state::SIDE_BID,
            amount_left: 1_000_000,
            min_fill: 1_000_000,
            max_fill: 0,
            price: 5_000,
            slope: 0,
            t_scale: 1_000,
            tip: 0,
            tif: kob_protocol::state::TIF_GTC,
            delivery_carrier: 0,
            custody: 1,
            ..p
        };
        assert_eq!(pair_takeable(&AnyState::KobPair(p.clone()), v, &[], p.price), Some(0));
        let p = kob_protocol::state::PairState { custody: p.bid_escrow(p.amount_left, 1).unwrap(), ..p };
        assert_eq!(pair_takeable(&AnyState::KobPair(p.clone()), v, &[], p.price), Some(1_000_000));
    }
}
