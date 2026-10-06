//! Regression: hostile numeric order state must never crash, stall or blind the matcher.
//!
//! * `slope != 0` with `decayStep = 0` used to divide by zero in `matcher::candidate` and end `kob-executor run`.
//! * A zero quantity divisor (a bid with a tip keeps a positive all-in) used to divide by zero in `planner::walk`; an if-done
//!   exit inherited its parent's listing without a look at its own numbers. Today a bid's `minFill` must be positive (its
//!   termination rule reads one minimum fill of buying power) and `scale` a power of ten in `1..=10^9`.
//! * A huge `amountLeft` (9e10) on a bid-side order used to make candidate generation linear in it on every tick.
//!
//! Fixed by the numeric gate `kob_executor::sanity` (at listing, in the book read, in the candidate generator), closed forms instead
//! of count-down loops, a per-tick time budget and a `catch_unwind` per book with quarantine of the guilty order.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::MemoryBook;
use kob_executor::matcher::engine::tick;
use kob_executor::matcher::family::Families;
use kob_executor::sanity;
use kob_protocol::state::*;
use std::time::{Duration, Instant};

#[test]
fn a_zero_min_fill_bid_is_quarantined_and_the_rest_of_the_book_still_trades() {
    // v3: the zero divisor of old (a zero lot size) is gone; a bid's `minFill = 0` is what the covenant refuses instead
    let mut b0 = bid(2, P260, T3);
    b0.min_fill = 0;
    b0.tip = 1_000_000;
    let hostile = listed(cid(1), AnyState::KobBid(b0), 5 * KAS * 100, 1_000);
    let honest_bid = l_bid(3, bid(3, P260, T3), 5 * WHOLE);
    let sane_ask = l_ask(2, ask(1, P250, 5 * WHOLE, T3));
    let b = MemoryBook { daa_score: NOW + 5, orders: vec![hostile, honest_bid, sane_ask], wallet_tokens: vec![] };
    let inp = input(&b);
    let r = run(&inp, &cfg());
    assert!(r.quarantined.iter().any(|(id, why)| *id == cid(1) && why.contains("minFill")), "{:?}", r.quarantined);
    assert_eq!(r.prepared.len(), 1, "the honest crossing pair is still matched");
}

#[test]
fn a_bid_scale_that_is_not_a_power_of_ten_is_quarantined() {
    let mut b0 = bid(2, P260, T3);
    b0.scale = 1_500;
    let hostile = listed(cid(1), AnyState::KobBid(b0), 5 * KAS * 100, 1_000);
    let b = MemoryBook {
        daa_score: NOW + 5,
        orders: vec![hostile, l_bid(3, bid(3, P260, T3), 5 * WHOLE), l_ask(2, ask(1, P250, 5 * WHOLE, T3))],
        wallet_tokens: vec![],
    };
    let r = run(&input(&b), &cfg());
    assert!(r.quarantined.iter().any(|(id, why)| *id == cid(1) && why.contains("scale")), "{:?}", r.quarantined);
    assert_eq!(r.prepared.len(), 1, "the honest crossing pair is still matched");
}

#[test]
fn a_huge_amount_left_costs_one_tick_no_time() {
    // used to be 150 to 260 s per tick at a remaining quantity of 9e10 (and effectively infinite with a 1 sompi stop leg)
    for amount in [1i64 << 33, 1 << 40, i64::MAX, 90_000_000_000] {
        let mut c = cond_bid(1, P250, P245, amount.min(1 << 40), T3);
        c.tip = 0;
        let o = listed(cid(1), AnyState::KobCondBid(c), 10 * KAS, 1_000);
        let b = MemoryBook { daa_score: NOW + 5, orders: vec![o], wallet_tokens: vec![] };
        let inp = input(&b);
        let t0 = Instant::now();
        let _ = tick(&inp, &cfg(), &Families::default(), &signer());
        assert!(t0.elapsed() < Duration::from_secs(2), "amountLeft {amount}: {:?}", t0.elapsed());
    }
    // the one-sompi stop leg: armed, stopPrice = 1, amountLeft as large as the gate allows
    let mut c = cond_bid(1, P250, P245, 1 << 40, T3);
    c.tip = 0;
    c.armed = 1;
    c.stop_price = 1;
    c.slip_bps = 0;
    let o = listed(cid(1), AnyState::KobCondBid(c), 10 * KAS, 1_000);
    let b = MemoryBook { daa_score: NOW + 5, orders: vec![o], wallet_tokens: vec![] };
    let t0 = Instant::now();
    let _ = tick(&input(&b), &cfg(), &Families::default(), &signer());
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
}

#[test]
fn the_affordable_fill_is_found_in_closed_form() {
    use kob_executor::matcher::candidate::largest_fit;
    // fits(n): 100 - 7n - (3 unless n == left) > 0
    let left = 1i64 << 40;
    let f = |n: i64| 100i128 - 7 * n as i128 - if n < left { 3 } else { 0 } > 0;
    assert_eq!(largest_fit(left, f), 13);
    assert_eq!(largest_fit(20, |n| 100i128 - 7 * n as i128 > 0), 14);
    assert_eq!(largest_fit(0, |_| true), 0);
    assert_eq!(largest_fit(5, |_| true), 5);
    assert_eq!(largest_fit(5, |_| false), 0);
    assert_eq!(largest_fit(1, |_| false), 0);
}

#[test]
fn the_numeric_gate_applies_the_covenants_acceptance_rules() {
    let good = AnyState::KobAsk(ask(1, P250, 10 * WHOLE, T3));
    assert_eq!(sanity::check(&good), Ok(()));
    let bad = |f: &dyn Fn(&mut AskState)| {
        let mut a = ask(1, P250, 10 * WHOLE, T3);
        f(&mut a);
        sanity::check(&AnyState::KobAsk(a))
    };
    assert!(bad(&|a| a.scale = 0).is_err());
    assert!(bad(&|a| a.scale = 1_500).is_err(), "not a power of ten");
    assert!(bad(&|a| a.scale = 10_000_000_000).is_err(), "above 10^9");
    assert!(bad(&|a| a.min_fill = -1).is_err());
    assert!(bad(&|a| a.min_fill = 0).is_ok(), "an ask's minimum fill may be zero (any fill)");
    assert!(bad(&|a| a.tip = -1).is_err());
    assert!(bad(&|a| a.slope = -1).is_err());
    assert!(bad(&|a| {
        a.slope = 1;
        a.decay_step = 0
    })
    .is_err());
    assert!(bad(&|a| a.amount_left = i64::MAX).is_err());
    assert!(bad(&|a| a.price = i64::MAX).is_err());
    assert!(bad(&|a| a.tif = 3).is_err());
    assert!(bad(&|a| {
        a.scale = i64::MAX;
        a.min_fill = 2
    })
    .is_err());
    // a decaying auction with sane numbers passes
    assert!(bad(&|a| {
        a.slope = 5;
        a.price_end = 1;
        a.decay_step = 10
    })
    .is_ok());
}

#[test]
fn a_panic_inside_planning_costs_a_skipped_book_never_the_process() {
    // the last line of defence: a validator that panics stands in for any defect the gate did not foresee
    let b = MemoryBook {
        daa_score: NOW + 5,
        orders: vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))],
        wallet_tokens: vec![],
    };
    let inp = input(&b);
    let boom: kob_executor::matcher::engine::Validator = &|_| panic!("simulated defect");
    let rep = kob_executor::matcher::engine::tick_with(&inp, &cfg(), &Families::default(), &signer(), boom);
    assert!(rep.prepared.is_empty());
    // no order is to blame for a defect that fires on every plan, yet the book is answered: an order is dropped until nothing crosses
    assert!(
        !rep.quarantined.is_empty() || rep.skipped.iter().any(|(_, why)| why.contains("panicked")),
        "{:?} {:?}",
        rep.quarantined,
        rep.skipped
    );
}

#[test]
fn a_tick_budget_bounds_the_wall_clock() {
    let mut orders = vec![];
    for i in 0..30u32 {
        orders.push(l_bid(100 + i, bid(1, P260, T3), 5 * WHOLE));
        orders.push(l_ask(200 + i, ask(2, P250, 5 * WHOLE, T3)));
    }
    let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
    let mut k = cfg();
    k.book_budget_ms = 1;
    k.tick_budget_ms = 1;
    let t0 = Instant::now();
    let rep = tick(&input(&b), &k, &Families::default(), &signer());
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    // whatever came out is a complete, valid transaction or nothing
    for p in &rep.prepared {
        assert!(p.validation.is_some());
    }
}

#[test]
fn the_order_that_panics_the_book_is_found_and_quarantined_and_the_process_lives() {
    // a defect that only triggers on one order: stand-in validator that panics for any transaction spending that order
    let guilty = l_bid(1, bid(1, P260, T3), 5 * WHOLE);
    let guilty_tx = guilty.order.utxo.transaction_id;
    let b = MemoryBook {
        daa_score: NOW + 5,
        orders: vec![guilty.clone(), l_ask(2, ask(2, P250, 5 * WHOLE, T3)), l_ask(3, ask(3, P255, 5 * WHOLE, T3))],
        wallet_tokens: vec![],
    };
    let inp = input(&b);
    let boom: kob_executor::matcher::engine::Validator = &move |s| {
        if s.tx.inputs.iter().any(|i| i.transaction_id == guilty_tx) {
            panic!("simulated defect on one order");
        }
        kob_protocol::verify::validate_signed(s).map_err(|e| e.to_string())
    };
    let rep = kob_executor::matcher::engine::tick_with(&inp, &cfg(), &Families::default(), &signer(), boom);
    assert_eq!(rep.quarantined.len(), 1, "{:?} {:?}", rep.quarantined, rep.skipped);
    assert_eq!(rep.quarantined[0].0, cid(1));
    assert_eq!(rep.quarantined[0].1, "planning panicked");
    assert!(rep.prepared.iter().all(|p| p.plan.fills.iter().all(|f| f.cand.id != cid(1))), "nothing spends the guilty order");
}

/// The numeric gate bounds a conditional order by its worst leg price. The stop band multiplies first, like the covenants
/// (`stopPrice * bps / 10000`); dividing first gave a ceiling of `stop` for any stop below 10 000 and so under-estimated the
/// products the gate exists to bound.
#[test]
fn the_stop_band_ceiling_multiplies_first_like_the_covenants() {
    // scale 1 (prices per base unit): the full value is amount x rate; 9_999 x amount fits the notional bound 2^61,
    // 19_998 x amount does not
    let amount = (1i64 << 61) / 15_000;
    let mk = |stop: i64, slip: i64| {
        let mut c = cond_bid(1, 0, stop, amount, T3);
        c.scale = 1;
        c.min_fill = 1;
        c.slip_bps = slip;
        c.tip = 0;
        c.delivery_carrier = 0;
        AnyState::KobCondBid(c)
    };
    assert_eq!(sanity::check(&mk(9_999, 0)), Ok(()), "no band: the ceiling is the stop");
    let e = sanity::check(&mk(9_999, 10_000)).unwrap_err();
    assert!(e.contains("notional"), "a 100% band doubles the ceiling: {e}");
}
