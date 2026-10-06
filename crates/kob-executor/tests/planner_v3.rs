//! Protocol v3 (no lots) planner rules, each engine-validated through the tick runner (`common::run`: both families,
//! `check_invariants`; `common::pair::run_pair` for pair orders):
//!
//! * the minimum fill (`minFill`): no fill below it unless the fill takes the order's rest (a `KobBid`: unless the fill ends
//!   the bid, less than one minimum fill of buying power left), and `maxFill` (TWAP / DCA) caps a fill;
//! * the profit test reads the exact rounded KAS of every leg (a bid pays `floor(n × (p + tip) / scale)`, an ask receives
//!   `ceil(n × (p − tip) / scale)`): a crossing whose spread per whole token is positive but whose fill is too small for the
//!   rounding is not planned, and `plan.margin` is the sum of the exact rounded leg values;
//! * a pair ASK routed through the KAS books buys exactly `ceil(n × rate / scale)` of B when the asks' minimum fills allow it, and more (the excess, to
//!   its maker's delivery) only when an ask's minimum fill forces it.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_protocol::artifacts::token_template;
use kob_protocol::state::*;
use kob_protocol::tx::spk_to_string;

/// A plain ask of `amount` base units at `price` with minimum fill `min_fill`.
fn ask_mf(maker: u8, price: i64, amount: i64, min_fill: i64) -> AskState {
    AskState { min_fill, ..ask(maker, price, amount, T3) }
}

/// An IOC bid at `price` funded for `amount` base units, placed just now (the active walker of its book).
fn ioc_bid(id: u32, price: i64, amount: i64) -> kob_executor::matcher::book::ListedOrder {
    fresh(l_bid(id, BidState { tif: TIF_IOC, ..bid(id as u8, price, T3) }, amount))
}

// ------------------------------------------------------------------ the minimum fill and maxFill

#[test]
fn a_passive_order_is_never_filled_below_its_minimum_fill_unless_the_fill_takes_its_rest() {
    // a resting ask of 5,000 base units with a minimum fill of 3,000: an IOC bid that can take only 2,400 leaves it alone
    let r = run(&input(&book(vec![l_ask(2, ask_mf(2, P250, 5_000, 3_000)), ioc_bid(1, P260, 2_400)])), &cfg());
    assert_eq!(amount_all(&r, cid(2)), 0, "no chunk below the ask's minimum fill");
    assert!(r.prepared.is_empty());
    // exactly the minimum fill is enough
    let r = run(&input(&book(vec![l_ask(2, ask_mf(2, P250, 5_000, 3_000)), ioc_bid(1, P260, 3_000)])), &cfg());
    assert_eq!(amount_of(&r, cid(2)), 3_000);
    // an ask of 2,500 with a minimum fill of 3,000 fills only whole: not by 2,400 ...
    let r = run(&input(&book(vec![l_ask(2, ask_mf(2, P250, 2_500, 3_000)), ioc_bid(1, P260, 2_400)])), &cfg());
    assert_eq!(amount_all(&r, cid(2)), 0);
    // ... but by a chunk that takes its whole rest (below its minimum fill: the fill ends the order)
    for power in [2_500, 4_000] {
        let r = run(&input(&book(vec![l_ask(2, ask_mf(2, P250, 2_500, 3_000)), ioc_bid(1, P260, power)])), &cfg());
        assert_eq!(amount_of(&r, cid(2)), 2_500, "bid power {power}: the whole rest");
        assert_eq!(amount_of(&r, cid(1)), 2_500);
    }
}

#[test]
fn a_bid_fills_below_its_minimum_fill_only_when_the_fill_ends_it() {
    // a bid with a minimum fill of 3 whole tokens and an ask of 2: used(n) = n x 260,100 sompi exactly (2.60 + 0.001 per
    // whole token), so after a fill of 2,000 a bid funded for 5,000 still has 3,000 of buying power (one minimum fill: it
    // continues, and the covenant refuses the small fill), while one funded for 4,999 has 2,999 left (it ends: the fill is
    // exempt from its minimum fill)
    let s = BidState { min_fill: 3 * WHOLE, ..bid(1, P260, T3) };
    assert!(s.can_continue(s.used(5_000).unwrap() - s.used(2_000).unwrap() + s.delivery_carrier));
    assert!(!s.can_continue(s.used(4_999).unwrap() - s.used(2_000).unwrap() + s.delivery_carrier));
    let r = run(&input(&book(vec![l_bid(1, s.clone(), 5_000), l_ask(2, ask(2, P250, 2 * WHOLE, T3))])), &cfg());
    assert_eq!(amount_all(&r, cid(1)), 0, "a fill of 2,000 would leave a bid able to continue below its minimum fill");
    let r = run(&input(&book(vec![l_bid(1, s.clone(), 4_999), l_ask(2, ask(2, P250, 2 * WHOLE, T3))])), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 2 * WHOLE, "the fill ends the bid: exempt from its minimum fill");
    // the bid's continuation is gone: its fill terminated it (no output carries the bid's script)
    let spk = spk_to_string(&AnyState::KobBid(s).spk());
    assert!(r.prepared[0].signed.tx.outputs.iter().all(|o| o.script_public_key != spk), "the bid ended");
}

#[test]
fn max_fill_caps_every_slice_of_a_twap_ask_and_a_dca_bid() {
    // a TWAP ask of 5 whole tokens in slices of at most 1,500 base units (not a whole number of tokens)
    let tw = AskState { interval: 600, max_fill: 1_500, ..ask(2, P250, 5 * WHOLE, T3) };
    let r = run(&input(&book(vec![l_ask(2, tw), l_bid(1, bid(1, P260, T3), 5 * WHOLE)])), &cfg());
    assert_eq!(amount_all(&r, cid(2)), 1_500, "one slice of maxFill");
    // a DCA bid buying at most 700 per slice (minimum fill 500) from an ask that allows any fill
    let dca = BidState { interval: 600, max_fill: 700, min_fill: 500, ..bid(3, P260, T3) };
    let r = run(&input(&book(vec![l_bid(3, dca, 5 * WHOLE), l_ask(4, ask_mf(4, P250, 5 * WHOLE, 1))])), &cfg());
    assert_eq!(amount_all(&r, cid(3)), 700, "one slice of maxFill");
    assert_eq!(amount_all(&r, cid(4)), 700);
}

// ------------------------------------------------------------------ the exact rounding of the profit test

/// The exact KAS a bid pays and an ask receives for `n` base units at `rate` per `scale` (independent of the library:
/// 128-bit floor and ceil).
fn floor_of(n: i64, rate: i64, scale: i64) -> i64 {
    (n as i128 * rate as i128 / scale as i128) as i64
}
fn ceil_of(n: i64, rate: i64, scale: i64) -> i64 {
    ((n as i128 * rate as i128 + scale as i128 - 1) / scale as i128) as i64
}

/// One token at scale 10^9 (a second book of the fixture token): an ask of `n` base units at 2 sompi per whole token and a
/// bid at 3, no tips, any fill. The spread per whole token is positive; a fill of n pays floor(3n / 10^9) − ceil(2n / 10^9).
fn tiny_pair(n: i64) -> Vec<kob_executor::matcher::book::ListedOrder> {
    const G: i64 = 1_000_000_000;
    let a = AskState { scale: G, min_fill: 1, tip: 0, ..ask(5, 2, n, T3) };
    let b = BidState { scale: G, min_fill: 1, tip: 0, ..bid(6, 3, T3) };
    vec![l_ask(20, a), l_bid(21, b, n)]
}

#[test]
fn a_crossing_the_rounding_makes_lose_is_not_planned_and_the_margin_is_the_exact_rounded_sum() {
    // a profitable crossing at scale 1,000 with amounts and prices that round: 4,321 base units at 2.60000333 (+ tip) and
    // 2.50000777 (- tip) per whole token
    let (pb, pa, n) = (260_000_333, 250_000_777, 4_321);
    let main = || vec![l_bid(1, bid(1, pb, T3), n), l_ask(2, ask(2, pa, n, T3))];
    let main_margin = floor_of(n, pb + TIP, SCALE) - ceil_of(n, pa - TIP, SCALE);
    // the planner alone, priced without fees: only the exact rounded amounts decide whether a chunk is worth adding
    let plan_of = |orders: &[kob_executor::matcher::book::ListedOrder]| {
        use kob_executor::matcher::batch::{plan_batch, BatchInput};
        use kob_executor::matcher::family::Family;
        use kob_executor::matcher::planner::{PlannerConfig, PHYSICAL_TX_BYTES};
        use std::collections::{BTreeMap, BTreeSet};
        let by_id: BTreeMap<[u8; 32], kob_executor::matcher::book::ListedOrder> = orders.iter().map(|o| (o.id(), o.clone())).collect();
        let fams: BTreeSet<Family> = [Family::Kcc20, Family::Kron].into_iter().collect();
        let none = BTreeSet::new();
        let bi = BatchInput {
            by_id: &by_id,
            lock_time: NOW,
            utc: UTC,
            excluded: &none,
            unaccepted: &none,
            families: &fams,
            max_bytes: PHYSICAL_TX_BYTES,
            only: None,
            deadline: None,
        };
        plan_batch(&bi, &PlannerConfig { fee_rate: 0, ..PlannerConfig::default() }).expect("the main crossing is planned")
    };
    // 0.3 of a whole token at 3 vs 2 sompi per whole token: floor(0.9) - ceil(0.6) = -1 sompi: never planned
    let mut lose = main();
    lose.extend(tiny_pair(300_000_000));
    let p = plan_of(&lose);
    assert_eq!((p.amount_of(&cid(20)), p.amount_of(&cid(21))), (0, 0), "a chunk whose rounded amounts lose");
    assert_eq!((p.amount_of(&cid(1)), p.amount_of(&cid(2))), (n, n));
    assert_eq!(p.margin, main_margin, "the exact rounded KAS of both legs");
    // one whole token of the same pair: floor(3) - ceil(2) = +1 sompi: planned, and counted exactly
    let mut win = main();
    win.extend(tiny_pair(1_000_000_000));
    let p = plan_of(&win);
    assert_eq!((p.amount_of(&cid(20)), p.amount_of(&cid(21))), (1_000_000_000, 1_000_000_000));
    assert_eq!(p.margin, main_margin + 1, "both books' exact rounded legs");
    // the margin is the sum of the plan's own leg values (`Cand::value`), each the covenant's rounded amount
    let legs = |p: &kob_executor::matcher::planner::Plan| -> i64 {
        p.fills
            .iter()
            .map(|f| match f.cand.side {
                kob_executor::matcher::candidate::Side::Bid => f.cand.value(f.amount).unwrap(),
                kob_executor::matcher::candidate::Side::Ask => -f.cand.value(f.amount).unwrap(),
            })
            .sum()
    };
    assert_eq!(p.margin, legs(&p));
    // the tick runner (both families, engine-validated, at the real fee): the main crossing at its exact rounded margin, the
    // losing chunk left out
    let r = run(&input(&book(lose)), &cfg());
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    assert_eq!((amount_all(&r, cid(20)), amount_all(&r, cid(21))), (0, 0));
    let p = &r.prepared[0];
    assert_eq!((p.plan.amount_of(&cid(1)), p.plan.amount_of(&cid(2))), (n, n));
    assert_eq!(p.plan.margin, main_margin);
    assert_eq!(p.plan.margin, legs(&p.plan));
    assert_eq!(p.accounting.profit, main_margin - p.accounting.fee as i64);
}

// ------------------------------------------------------------------ pair orders buy exact B

/// The maker's B delivery (at the pair order's input index) of a prepared transaction, as the script of `amount` base
/// units of B (KCC-20 8x8) to maker 1.
fn delivers(p: &kob_executor::matcher::engine::Prepared, id: [u8; 32], amount: i64) -> bool {
    let tx = &p.signed.tx;
    let i = tx.inputs.iter().position(|i| i.utxo.covenant_id == Some(id)).expect("the pair order is spent");
    tx.outputs[i].script_public_key == spk_to_string(&tstate(T8, amount, pk(1), false).spk_with(token_template(T8)))
}

#[test]
fn a_pair_ask_buys_exactly_its_b_and_more_only_when_an_ask_minimum_fill_forces_it() {
    // 3,777 base units of A at 0.333 whole B per whole A: the delivery needs ceil(3,777 x 333 / 1,000) = 1,258 B
    let x = pair_state(1, true, T8, T8, 3_777, 333, 0, TIF_GTC);
    assert_eq!(x.t_out_min(3_777, 333), Some(1_258));
    let scene = |ask_min_fill: i64| {
        let mut b = l_ask_b(200, T8, T8, 3, P200, 10 * WHOLE);
        if let AnyState::KobAsk(s) = &mut b.order.state {
            s.min_fill = ask_min_fill;
        }
        vec![l_pair(X1, T8, T8, x.clone(), 1_000), l_bid_a(100, T8, 2, P260, 3_777), b]
    };
    // the ask allows any fill: exactly 1,258 bought and delivered
    for mf in [1, 1_000, 1_258] {
        let r = run_pair(scene(mf), &cfg());
        assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
        assert_eq!(amount_in(&r, cid(X1)), 3_777);
        assert_eq!(amount_in(&r, cid(200)), 1_258, "ask minimum fill {mf}: exactly ceil(n x rate / scale)");
        assert!(delivers(&r.prepared[0], cid(X1), 1_258), "the maker receives exactly 1,258 B");
    }
    // an ask whose minimum fill is 2,000 forces the route to buy 2,000: the 742 of excess ride on the maker's delivery
    let r = run_pair(scene(2_000), &cfg());
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    assert_eq!(amount_in(&r, cid(X1)), 3_777);
    assert_eq!(amount_in(&r, cid(200)), 2_000, "the ask's minimum fill");
    assert!(delivers(&r.prepared[0], cid(X1), 2_000), "the excess goes to the maker, never to the operator");
    assert!(r.prepared[0].accounting.profit > 0);
}

const X1: u32 = 1;
