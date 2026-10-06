//! Matcher scenarios (`docs/spec/matcher.md` §2–§8, §11): every order type and mixes, each planned
//! batch built by `kob_protocol` and validated through the rusty-kaspa v2.1.0 script engine.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::candidate::Class;
use kob_executor::matcher::engine::tick;
use kob_executor::matcher::family::{Families, Family};
use kob_executor::matcher::planner::UpdateKind;
use kob_executor::testkit::TOKEN_COV_KRON;
use kob_protocol::state::*;

#[test]
fn resting_cross_pays_spread_and_tips_to_the_matcher() {
    let b = book(vec![
        l_bid(1, bid(1, P260, T3), 8 * WHOLE),
        l_ask(2, ask(2, P250, 5 * WHOLE, T3)),
        l_ask(3, ask(3, P255, 10 * WHOLE, T3)),
    ]);
    let r = run(&input(&b), &cfg());
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    let p = &r.prepared[0];
    assert_eq!(p.plan.amount_of(&cid(1)), 8 * WHOLE);
    assert_eq!(p.plan.amount_of(&cid(2)), 5 * WHOLE, "the cheapest ask first");
    assert_eq!(p.plan.amount_of(&cid(3)), 3 * WHOLE);
    let margin = 8 * (P260 + TIP) - 5 * (P250 - TIP) - 3 * (P255 - TIP);
    assert_eq!(p.plan.margin, margin);
    assert_eq!(p.accounting.profit, margin - p.accounting.fee as i64);
    assert!(p.accounting.profit > 0);
}

#[test]
fn nothing_crosses_nothing_is_built() {
    let b = book(vec![l_bid(1, bid(1, P245, T3), 8 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let r = run(&input(&b), &cfg());
    assert!(r.prepared.is_empty());
}

#[test]
fn price_then_tip_then_age() {
    // Two asks at the same price: the higher tip wins; equal tips: the older UTXO wins.
    let mut a_low_tip = ask(2, P250, 3 * WHOLE, T3);
    a_low_tip.tip = 0;
    let a_high_tip = ask(3, P250, 3 * WHOLE, T3);
    let mut b = book(vec![l_bid(1, bid(1, P260, T3), 3 * WHOLE), l_ask(2, a_low_tip), l_ask(3, a_high_tip)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(3)), 3 * WHOLE);
    assert_eq!(amount_of(&r, cid(2)), 0);

    let mut young = l_ask(4, ask(4, P250, 3 * WHOLE, T3));
    young.order.utxo.block_daa_score = 2_000;
    let old = l_ask(5, ask(5, P250, 3 * WHOLE, T3));
    b = book(vec![l_bid(1, bid(1, P260, T3), 3 * WHOLE), young, old]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(5)), 3 * WHOLE, "older first");
    assert_eq!(amount_of(&r, cid(4)), 0);
}

#[test]
fn fok_is_all_or_none() {
    // FOK bid for 10 whole tokens, only 6 crossing: not included at all.
    let mut fb = bid(1, P260, T8);
    fb.tif = TIF_FOK;
    let b = book(vec![
        fresh(l_bid(1, fb.clone(), 10 * WHOLE)),
        l_ask(2, ask(2, P250, 3 * WHOLE, T8)),
        l_ask(3, ask(3, P250, 3 * WHOLE, T8)),
    ]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_all(&r, cid(1)), 0);
    // With 12 crossing whole tokens it is filled completely in one transaction.
    let b = book(vec![fresh(l_bid(1, fb, 10 * WHOLE)), l_ask(2, ask(2, P250, 6 * WHOLE, T8)), l_ask(3, ask(3, P250, 6 * WHOLE, T8))]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 10 * WHOLE);
    // FOK ask of 10 whole tokens against 6 of bids: never partial.
    let mut fa = ask(4, P250, 10 * WHOLE, T8);
    fa.tif = TIF_FOK;
    let b = book(vec![fresh(l_ask(4, fa.clone())), l_bid(5, bid(5, P260, T8), 6 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_all(&r, cid(4)), 0);
    let b = book(vec![fresh(l_ask(4, fa)), l_bid(5, bid(5, P260, T8), 6 * WHOLE), l_bid(6, bid(6, P255, T8), 6 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(4)), 10 * WHOLE);
}

#[test]
fn oversize_fok_is_never_filled() {
    // 3/3 token: a FOK bid needing four asks cannot complete within 3 token inputs.
    let mut fb = bid(1, P260, T3);
    fb.tif = TIF_FOK;
    let asks: Vec<_> = (0..4).map(|i| l_ask(10 + i, ask(2, P250, 3 * WHOLE, T3))).collect();
    let mut orders = vec![fresh(l_bid(1, fb, 10 * WHOLE))];
    orders.extend(asks);
    let r = run(&input(&book(orders)), &cfg());
    assert_eq!(amount_all(&r, cid(1)), 0);
}

#[test]
fn ioc_is_maximal_and_first() {
    // An IOC bid and a better-priced resting bid both want the only ask: the IOC (class 1) wins.
    let mut ib = bid(1, P255, T8);
    ib.tif = TIF_IOC;
    let b = book(vec![
        fresh(l_bid(1, ib.clone(), 5 * WHOLE)),
        l_bid(2, bid(2, P260, T8), 5 * WHOLE),
        l_ask(3, ask(3, P250, 5 * WHOLE, T8)),
    ]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 5 * WHOLE);
    assert_eq!(amount_all(&r, cid(2)), 0);
    assert!(r.prepared[0].plan.fills.iter().any(|f| f.cand.class == Class::Immediate));
    // IOC for 10 whole tokens against five asks of 2 on 8/8: the whole 10.
    let mut orders = vec![fresh(l_bid(1, ib.clone(), 10 * WHOLE))];
    orders.extend((0..5).map(|i| l_ask(10 + i, ask(3, P250, 2 * WHOLE, T8))));
    let r = run(&input(&book(orders)), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 10 * WHOLE);
    // On a 3/3 token the largest single transaction takes three asks (6 whole tokens), not chained.
    let mut ib3 = bid(1, P255, T3);
    ib3.tif = TIF_IOC;
    let mut orders = vec![fresh(l_bid(1, ib3, 10 * WHOLE))];
    orders.extend((0..5).map(|i| l_ask(10 + i, ask(3, P250, 2 * WHOLE, T3))));
    let r = run(&input(&book(orders)), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 6 * WHOLE);
    assert_eq!(amount_all(&r, cid(1)), 6 * WHOLE, "an IOC is never chained");
}

#[test]
fn market_auction_fills_at_the_first_profitable_tick() {
    // Market sell from 2.60 down 3% over 200 DAA starting at NOW; a bid at 2.56 crosses once the
    // quote has decayed below it (all-in, tips included).
    let m = market_ask(1, P260, 4 * WHOLE, NOW as i64, T3);
    let b0 = book(vec![l_ask(1, m.clone()), l_bid(2, bid(2, 256_000_000, T3), 4 * WHOLE)]);
    // At t = NOW (clock NOW + 5): quote 2.60 does not cross.
    let r = run(&input(&b0), &cfg());
    assert!(r.prepared.is_empty());
    // Later the auction reaches the bid.
    let mut b1 = b0.clone();
    b1.daa_score = NOW + 5 + 150;
    let r = run(&input(&b1), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
    let t = (NOW + 150) as i64;
    let q = m.price_at(t, 1_000).expect("the auction price");
    assert!(q <= 256_000_000 && q >= m.price_end);
    assert_eq!(r.prepared[0].plan.lock_time, NOW + 150);
}

#[test]
fn expired_killed_and_day_orders_are_not_filled() {
    // Soft expiry passed.
    let mut a = ask(1, P250, 5 * WHOLE, T3);
    a.expiry_daa = NOW as i64;
    let b = book(vec![l_ask(1, a), l_bid(2, bid(2, P260, T3), 5 * WHOLE)]);
    assert!(run(&input(&b), &cfg()).prepared.is_empty());
    // IOC past its kill time max(UTXO DAA, activeFrom) + 600.
    let mut i = ask(1, P250, 5 * WHOLE, T3);
    i.tif = TIF_IOC;
    i.expiry_daa = NO_EXPIRY;
    let b = book(vec![l_ask(1, i), l_bid(2, bid(2, P260, T3), 5 * WHOLE)]);
    assert!(run(&input(&b), &cfg()).prepared.is_empty());
    // Day order: deadline passed by the UTC clock while expiryDaa is ahead.
    let mut d = l_ask(1, ask(1, P250, 5 * WHOLE, T3));
    d.deadline = Some(UTC - 1);
    let b = book(vec![d.clone(), l_bid(2, bid(2, P260, T3), 5 * WHOLE)]);
    assert!(run(&input(&b), &cfg()).prepared.is_empty());
    d.deadline = Some(UTC + 3_600);
    let b = book(vec![d, l_bid(2, bid(2, P260, T3), 5 * WHOLE)]);
    assert_eq!(amount_of(&run(&input(&b), &cfg()), cid(1)), 5 * WHOLE);
}

#[test]
fn strays_are_never_spent_and_bad_custody_is_not_listed() {
    let mut a = l_ask(1, ask(1, P250, 5 * WHOLE, T3));
    a.strays = vec![custody(7 * WHOLE, cid(1), 1_500)];
    let b = book(vec![a.clone(), l_bid(2, bid(2, P260, T3), 5 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 5 * WHOLE);
    // A "custody" that does not hold exactly amountLeft base units (a stray in its place) is not listable.
    let mut bad = a;
    bad.custody = Some(custody(7 * WHOLE, cid(1), 1_500));
    let b = book(vec![bad, l_bid(2, bid(2, P260, T3), 5 * WHOLE)]);
    assert!(run(&input(&b), &cfg()).prepared.is_empty());
}

#[test]
fn twap_and_dca_slices() {
    let mut tw = ask(1, P250, 10 * WHOLE, T3);
    tw.interval = 600;
    tw.max_fill = 2 * WHOLE;
    let b = book(vec![l_ask(1, tw.clone()), l_bid(2, bid(2, P260, T3), 10 * WHOLE)]);
    assert_eq!(amount_of(&run(&input(&b), &cfg()), cid(1)), 2 * WHOLE, "at most maxFill per slice");
    // A slice UTXO younger than `interval` waits.
    let mut young = l_ask(1, tw);
    young.order.utxo.block_daa_score = NOW - 100;
    let b = book(vec![young, l_bid(2, bid(2, P260, T3), 10 * WHOLE)]);
    assert_eq!(amount_all(&run(&input(&b), &cfg()), cid(1)), 0);
    let mut dca = bid(3, P260, T3);
    dca.interval = 600;
    dca.max_fill = WHOLE;
    let b = book(vec![l_bid(3, dca, 5 * WHOLE), l_ask(4, ask(4, P250, 5 * WHOLE, T3))]);
    assert_eq!(amount_of(&run(&input(&b), &cfg()), cid(3)), WHOLE);
}

/// A tick over `inp` and over its KRON twin: both engine-validated and checked (`check_invariants`, `check_triggers`).
fn both(inp: &kob_executor::matcher::engine::TickInput) -> Vec<kob_executor::matcher::engine::TickReport> {
    let k = to_kron(inp);
    let rk = tick(&k, &cfg(), &Families::default(), &signer());
    assert!(rk.anomalies.is_empty(), "KRON: {:?}", rk.anomalies);
    for p in &rk.prepared {
        check_invariants(p, &k);
    }
    vec![run(inp, &cfg()), rk]
}

/// The plan index of order `id`'s fill in a report's first transaction.
fn fill_at(r: &kob_executor::matcher::engine::TickReport, id: [u8; 32]) -> usize {
    r.prepared[0].plan.fills.iter().position(|f| f.cand.id == id).expect("filled")
}

#[test]
fn a_stop_fills_next_to_its_evidence_in_the_same_batch() {
    // OCO sell: TP 3.00, stop 2.40. A resting ask at 2.38 (exposed since DAA 1 000, minRestDaa 600) is bought by a 2.45 bid
    // funded for 9 whole tokens: that fill is the evidence, and the stop sells its 4 into what the bid has left, at its
    // trigger price, in the same transaction (both families).
    let c = cond_ask(1, 300_000_000, 240_000_000, 4 * WHOLE, T3);
    let b = book(vec![
        l_cond_ask(1, c, 2_000),
        l_ask(2, ask(2, 238_000_000, 5 * WHOLE, T3)),
        l_bid(3, bid(3, 245_000_000, T3), 9 * WHOLE),
    ]);
    for r in both(&input(&b)) {
        assert_eq!(r.prepared.len(), 1);
        assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
        assert_eq!(amount_of(&r, cid(2)), 5 * WHOLE);
        let p = &r.prepared[0].plan;
        let f = &p.fills[fill_at(&r, cid(1))];
        assert_eq!((f.cand.leg, f.cand.class), (1, Class::Triggered));
        assert_eq!(f.cand.quote, 240_000_000, "a banded stop fills at its trigger");
        assert_eq!(f.evidence, Some(fill_at(&r, cid(2))), "the resting ask's fill is its evidence");
        assert!(p.updates.is_empty(), "filled, not armed");
    }
    // Buy side: a buy stop at 2.55 next to a resting bid at 2.56 that buys from a 2.54 ask of 9 whole tokens.
    let cb = cond_bid(4, 240_000_000, 255_000_000, 3 * WHOLE, T3);
    let b = book(vec![
        l_cond_bid(4, cb, 2_000),
        l_bid(5, bid(5, 256_000_000, T3), 5 * WHOLE),
        l_ask(6, ask(6, 254_000_000, 9 * WHOLE, T3)),
    ]);
    for r in both(&input(&b)) {
        assert_eq!(amount_of(&r, cid(4)), 3 * WHOLE);
        assert_eq!(r.prepared[0].plan.fills[fill_at(&r, cid(4))].evidence, Some(fill_at(&r, cid(5))));
    }
}

#[test]
fn no_trigger_without_qualifying_evidence() {
    // The same sell stop and a 2.45 bid with room for it, next to crossings that are no evidence.
    let stop = cond_ask(1, 300_000_000, 240_000_000, 4 * WHOLE, T3);
    let bid9 = || l_bid(3, bid(3, 245_000_000, T3), 9 * WHOLE);
    // too young: the ask rested 100 DAA (minRestDaa 600)
    let mut young = l_ask(2, ask(2, 238_000_000, 5 * WHOLE, T3));
    young.order.utxo.block_daa_score = NOW - 100;
    young.custody.as_mut().unwrap().utxo.block_daa_score = NOW - 100;
    // decaying: a Dutch ask at its 2.00 floor quotes no price of its own
    let mut dutch = ask(2, 300_000_000, 5 * WHOLE, T3);
    dutch.slope = 1_000_000;
    dutch.price_end = 200_000_000;
    dutch.decay_step = 10_000;
    // below the threshold: 5 whole tokens (5,000 base units) against minTouch 5,001
    let big = CondAskState { min_touch: 5 * WHOLE + 1, ..stop.clone() };
    // wrong side: the resting bid at 2.399 is below the stop, but a sell stop reads asks (the ask quotes 2.4005; the
    // 0.02 KAS tips on both sides make them cross)
    let low_bid = l_bid(4, BidState { price: 239_900_000, tip: 2_000_000, ..bid(4, 0, T3) }, 5 * WHOLE);
    // a conditional's fill is never evidence: the take-profit leg of another OCO at 2.38
    let tp = l_cond_ask(5, cond_ask(5, 238_000_000, 0, 5 * WHOLE, T3), 2_000);
    let cases: Vec<(&str, CondAskState, Vec<ListedOrder>)> = vec![
        ("too young", stop.clone(), vec![young, bid9()]),
        ("decaying", stop.clone(), vec![l_ask(2, dutch), bid9()]),
        ("below minTouch", big, vec![l_ask(2, ask(2, 238_000_000, 5 * WHOLE, T3)), bid9()]),
        ("wrong side", stop.clone(), vec![l_ask(2, AskState { tip: 2_000_000, ..ask(2, 240_050_000, 5 * WHOLE, T3) }), low_bid]),
        ("a conditional fill", stop.clone(), vec![tp, bid9()]),
    ];
    for (what, s, mut orders) in cases {
        orders.push(l_cond_ask(1, s, 2_000));
        let r = run(&input(&book(orders)), &cfg());
        assert!(!r.prepared.is_empty(), "{what}: the crossing itself is planned");
        assert_eq!(amount_all(&r, cid(1)), 0, "{what}: the stop must not trigger");
        assert!(r.prepared.iter().all(|p| p.plan.updates.is_empty()), "{what}: nor be armed");
    }
}

#[test]
fn a_stop_the_batch_does_not_fill_is_armed_by_an_update_then_auctioned_next_tick() {
    // The 2.45 bid buys exactly the 5 whole tokens of the resting ask: nothing is left for the stop, so the batch arms it
    // (`update`, next to its evidence) and takes its keeper tip (both families).
    let c = cond_ask(1, 300_000_000, 240_000_000, 4 * WHOLE, T3);
    let stop = l_cond_ask(1, c.clone(), 2_000);
    let b = book(vec![stop.clone(), l_ask(2, ask(2, 238_000_000, 5 * WHOLE, T3)), l_bid(3, bid(3, 245_000_000, T3), 5 * WHOLE)]);
    let mut armed_tx = None;
    for (k, r) in both(&input(&b)).into_iter().enumerate() {
        assert_eq!(r.prepared.len(), 1);
        assert_eq!(amount_all(&r, cid(1)), 0);
        let p = &r.prepared[0];
        assert_eq!(p.plan.updates.len(), 1);
        let u = &p.plan.updates[0];
        assert_eq!((u.id, u.kind, u.evidence), (cid(1), UpdateKind::Arm, fill_at(&r, cid(2))));
        // the order's own keeperTip (the KRON twin's default tip is the KRON program's)
        if k == 0 {
            assert_eq!(u.take, ktip(T3));
        }
        assert_eq!(p.plan.tips, u.take, "the tip is income of the batch");
        if armed_tx.is_none() {
            armed_tx = Some(p.clone());
        }
    }
    // The continuation is armed (armed = 1: the auction starts at its own DAA score) and keeps value - keeperTip.
    let p = armed_tx.unwrap();
    let next = AnyState::KobCondAsk(CondAskState { armed: 1, ..c.clone() });
    let tx = &p.lowered.built.tx;
    let at = tx
        .outputs
        .iter()
        .position(|o| o.script_public_key == kob_protocol::tx::spk_to_string(&next.spk()))
        .expect("armed continuation");
    assert_eq!(tx.outputs[at].value, stop.order.utxo.amount - ktip(T3) as u64);
    // Next tick: the armed stop is in its band auction (half open 150 DAA after acceptance: 2.40 - 1.5% = 2.364) and sells
    // into a 2.37 bid with class-2 priority, no evidence needed.
    let accepted = NOW + 10;
    let mut armed = stop.clone();
    armed.order = kob_protocol::tx::OrderUtxo {
        utxo: kob_protocol::tx::Utxo {
            transaction_id: tx.id,
            index: at as u32,
            amount: tx.outputs[at].value,
            block_daa_score: accepted,
            covenant_id: Some(cid(1)),
        },
        state: next,
    };
    let mut b2 = book(vec![armed, l_bid(4, bid(4, 237_000_000, T3), 4 * WHOLE)]);
    b2.daa_score = accepted + 150 + 5;
    let r = run(&input(&b2), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
    let f = &r.prepared[0].plan.fills[fill_at(&r, cid(1))];
    assert_eq!((f.cand.leg, f.cand.class, f.evidence), (1, Class::Triggered, None));
    assert!(f.cand.quote < 240_000_000, "an auction quote below the trigger");
}

/// A sell stop at 2.40 (`keeperTip` = `tip`) listed next to a crossing the stop cannot take part of: a 2.45 bid buys exactly
/// the 5 whole tokens of the 2.38 ask, so the stops are only armed (an update each).
fn arm_book(stops: &[(u32, i64)], spread: bool) -> MemoryBook {
    let mut v = vec![];
    for &(id, tip) in stops {
        v.push(l_cond_ask(id, CondAskState { keeper_tip: tip, ..cond_ask(id as u8, 300_000_000, 240_000_000, 4 * WHOLE, T3) }, 2_000));
    }
    let (mut a, mut b) = (ask(2, 238_000_000, 5 * WHOLE, T3), bid(3, if spread { 245_000_000 } else { 238_000_000 }, T3));
    if !spread {
        // no tips, equal prices: the crossing pays no spread at all
        (a.tip, b.tip) = (0, 0);
    }
    v.push(l_ask(2, a));
    v.push(l_bid(3, b, 5 * WHOLE));
    book(v)
}

fn update_ids(r: &kob_executor::matcher::engine::TickReport) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = r.prepared.iter().flat_map(|p| p.plan.updates.iter().map(|u| u.id)).collect();
    v.sort();
    v
}

#[test]
fn a_zero_tip_stop_is_armed_when_the_crossing_spread_pays_for_it() {
    // Arming is the default: the update's cost is charged against the batch, and the batch's spread covers it.
    let b = arm_book(&[(1, 0)], true);
    let r = run(&input(&b), &cfg());
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    let p = &r.prepared[0];
    assert_eq!(update_ids(&r), vec![cid(1)]);
    assert_eq!((p.plan.updates[0].kind, p.plan.updates[0].take, p.plan.tips), (UpdateKind::Arm, 0, 0));
    assert!(p.plan.updates[0].cost > 0 && p.accounting.profit >= 1);
    // arming off: the same batch without the update
    let mut off = cfg();
    off.planner.arm = false;
    let q = &run(&input(&b), &off).prepared[0];
    assert!(q.plan.updates.is_empty());
    assert_eq!(q.plan.profit() - p.plan.profit(), p.plan.updates[0].cost, "its cost is charged against the batch");
}

#[test]
fn updates_are_dropped_worst_first_only_to_keep_min_profit() {
    // Three stops: ids 1 and 5 pay no tip, id 4 pays less than the update costs. Every update costs the same, so the tip
    // minus cost order is 1 = 5 < 4; ties drop the later one in tip order (highest tip first, then id): 5, then 1, then 4.
    let b = arm_book(&[(1, 0), (4, 400_000), (5, 0)], true);
    let plan_with = |min_profit: i64| {
        let mut k = cfg();
        k.planner.min_profit = min_profit;
        run(&input(&b), &k)
    };
    let mut off = cfg();
    off.planner.arm = false;
    let none = run(&input(&b), &off).prepared[0].plan.profit();
    let r = plan_with(0);
    assert_eq!(update_ids(&r), vec![cid(1), cid(4), cid(5)], "every qualifying update is included while the batch stays profitable");
    let all = r.prepared[0].plan.clone();
    let cost = all.updates[0].cost;
    assert!(all.updates.iter().all(|u| u.cost == cost));
    assert_eq!(all.profit(), none - 3 * cost + 400_000);
    // the profit must reach `all + cost`: the worst one (id 5) goes, the other two stay
    let r = plan_with(all.profit() + cost);
    assert_eq!(update_ids(&r), vec![cid(1), cid(4)]);
    // one more sompi: id 1 goes too
    let r = plan_with(all.profit() + cost + 1);
    assert_eq!(update_ids(&r), vec![cid(4)]);
    // the same inputs, the same batch
    assert_eq!(update_ids(&plan_with(all.profit() + cost + 1)), vec![cid(4)]);
    // `none - 1`: only the bare crossing reaches it, so every update goes (the tip of id 4 is below its cost)
    let r = plan_with(none - 1);
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    assert!(update_ids(&r).is_empty());
    // the updates are the only thing dropped: the crossing stays
    assert_eq!(amount_of(&r, cid(2)), 5 * WHOLE);
}

#[test]
fn a_standalone_arming_transaction_is_paid_by_tips_only() {
    // No fill pays spread (equal prices, no tips): the batch is only there to arm. A stop with no tip is not armed and
    // the batch is not built; one whose tip covers the batch is armed, and only the stops whose tips cover their own cost.
    let r = run(&input(&arm_book(&[(1, 0)], false)), &cfg());
    assert!(r.prepared.is_empty(), "nothing pays the transaction");
    let r = run(&input(&arm_book(&[(1, 400_000)], false)), &cfg());
    assert!(r.prepared.is_empty(), "a tip below the update's cost does not arm a standalone transaction");
    let r = run(&input(&arm_book(&[(1, 0), (4, 5_000_000)], false)), &cfg());
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    assert_eq!(update_ids(&r), vec![cid(4)], "the zero-tip stop rides for free only next to a fill that pays spread");
    assert!(r.prepared[0].accounting.profit >= 1);
    // the same two stops next to a crossing that pays spread are both armed
    let r = run(&input(&arm_book(&[(1, 0), (4, 5_000_000)], true)), &cfg());
    assert_eq!(update_ids(&r), vec![cid(1), cid(4)]);
}

#[test]
fn a_thin_crossing_is_kept_for_the_updates_its_evidence_pays_for() {
    // One whole token of a resting ask at 2.38 and of a bid at `p`: the lowest `p` at which the crossing pays its fee alone
    // is found by search; just below it, the crossing is still built when it arms a listed stop, because the stop's keeper
    // tip pays for the update, and not when arming is off.
    let c = cond_ask(1, 300_000_000, 240_000_000, 4 * WHOLE, T3);
    let with_bid = |p: i64, stop: bool| {
        let mut v = vec![l_ask(2, ask(2, 238_000_000, WHOLE, T3)), l_bid(3, bid(3, p, T3), WHOLE)];
        if stop {
            v.push(l_cond_ask(1, c.clone(), 2_000));
        }
        book(v)
    };
    let matched = |p: i64, stop: bool, k: &kob_executor::matcher::engine::EngineConfig| {
        run(&input(&with_bid(p, stop)), k).prepared.into_iter().find(|x| x.plan.amount_of(&cid(2)) > 0)
    };
    let (mut lo, mut hi) = (238_000_000i64, 246_000_000i64);
    assert!(matched(hi, false, &cfg()).is_some() && matched(lo, false, &cfg()).is_none());
    while hi - lo > 1_000 {
        let mid = (lo + hi) / 2;
        if matched(mid, false, &cfg()).is_some() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let p = matched(lo, true, &cfg()).expect("the evidence of a profitable update is kept");
    assert_eq!(p.plan.updates.iter().map(|u| u.id).collect::<Vec<_>>(), vec![cid(1)]);
    assert!(p.accounting.profit >= 1);
    let mut off = cfg();
    off.planner.arm = false;
    assert!(matched(lo, true, &off).is_none(), "without updates the thin crossing does not pay");
}

#[test]
fn a_trailing_stop_is_ratcheted_by_an_update_in_a_batch() {
    // Trailing sell stop 2.00 (step 0.05, gap 0.10, wait 600 DAA): a resting bid at 2.62 buys a 2.55 ask; its fill justifies
    // k = floor((2.62 - 0.10 - 2.00) / 0.05) = 10 steps, so the batch moves the stop to 2.50 (both families).
    let mut c = cond_ask(1, 300_000_000, 200_000_000, 4 * WHOLE, T3);
    c.trail_step = 5_000_000;
    c.trail_gap = 10_000_000;
    c.trail_wait = 600;
    assert_eq!(c.trail_steps(262_000_000), 10);
    let with = |daa: u64| {
        vec![l_ask(2, ask(2, P255, 5 * WHOLE, T3)), l_bid(3, bid(3, 262_000_000, T3), 5 * WHOLE), l_cond_ask(1, c.clone(), daa)]
    };
    for r in both(&input(&book(with(2_000)))) {
        let p = &r.prepared[0];
        assert_eq!(p.plan.updates.len(), 1);
        let u = &p.plan.updates[0];
        assert_eq!((u.id, u.kind, u.steps, u.evidence), (cid(1), UpdateKind::Trail, 10, fill_at(&r, cid(3))));
    }
    let next = AnyState::KobCondAsk(CondAskState { stop_price: 250_000_000, ..c.clone() });
    let r = run(&input(&book(with(2_000))), &cfg());
    let spk = kob_protocol::tx::spk_to_string(&next.spk());
    assert!(r.prepared[0].lowered.built.tx.outputs.iter().any(|o| o.script_public_key == spk), "the ratcheted continuation");
    // A UTXO younger than trailWait is not ratcheted (the update's CSV would not hold).
    let r = run(&input(&book(with(NOW - 100))), &cfg());
    assert!(r.prepared.iter().all(|p| p.plan.updates.is_empty()));
}

#[test]
fn a_two_family_batch_also_arms_a_stop() {
    // A KRON crossing and a KCC-20 crossing in one transaction; the KCC-20 ask's fill arms a listed sell stop (an update
    // next to it), whatever else the batch trades.
    let kron = to_kron(&input(&book(vec![l_bid(11, bid(1, P260, T3), 5 * WHOLE), l_ask(12, ask(2, P250, 5 * WHOLE, T3))])));
    let mut both = input(&book(vec![
        l_cond_ask(1, cond_ask(1, 300_000_000, 240_000_000, 4 * WHOLE, T3), 2_000),
        l_ask(2, ask(2, 238_000_000, 5 * WHOLE, T3)),
        l_bid(3, bid(3, 245_000_000, T3), 5 * WHOLE),
    ]));
    both.orders.extend(kron.orders);
    let r = tick(&both, &cfg(), &Families::default(), &signer());
    assert!(r.anomalies.is_empty() && r.skipped.is_empty(), "{:?} {:?}", r.anomalies, r.skipped);
    assert_eq!(r.prepared.len(), 1, "one transaction");
    let p = &r.prepared[0];
    check_invariants(p, &both);
    assert_eq!(p.plan.families().len(), 2, "both families in one batch");
    assert_eq!(p.plan.updates.iter().map(|u| u.id).collect::<Vec<_>>(), vec![cid(1)]);
}

#[test]
fn armed_stop_auction_and_take_profit_leg() {
    // Armed sell stop 2.40 (origin = its UTXO DAA NOW − 150: half the band open): fills at the
    // auction quote against a 2.35 bid.
    let mut c = cond_ask(1, 300_000_000, 240_000_000, 4 * WHOLE, T3);
    c.armed = 1;
    let b = book(vec![l_cond_ask(1, c.clone(), NOW - 145), l_bid(2, bid(2, 237_000_000, T3), 4 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
    assert_eq!(r.prepared[0].plan.fills.iter().find(|f| f.cand.id == cid(1)).unwrap().cand.leg, 1);
    // The take-profit leg is an ordinary resting limit, armed or not.
    let c2 = cond_ask(3, 250_000_000, 200_000_000, 4 * WHOLE, T3);
    let b = book(vec![l_cond_ask(3, c2, 2_000), l_bid(4, bid(4, P260, T3), 4 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(3)), 4 * WHOLE);
    // Buy side: an armed buy-stop and the limit leg of a buy OCO.
    let mut cb = cond_bid(5, 240_000_000, 250_000_000, 3 * WHOLE, T3);
    cb.armed = 1;
    let b = book(vec![l_cond_bid(5, cb, NOW - 200), l_ask(6, ask(6, 252_000_000, 3 * WHOLE, T3))]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(5)), 3 * WHOLE);
    let cb2 = cond_bid(7, 255_000_000, 300_000_000, 3 * WHOLE, T3);
    let b = book(vec![l_cond_bid(7, cb2, 2_000), l_ask(8, ask(8, P250, 3 * WHOLE, T3))]);
    assert_eq!(amount_of(&run(&input(&b), &cfg()), cid(7)), 3 * WHOLE);
}

#[test]
fn if_done_entries_create_exits() {
    // Buy-first entry at 2.60 buys 4 of 10 whole tokens from an ask at 2.50: one exit for exactly 4.
    let e = ifd_bid(1, P260, 10 * WHOLE, T3);
    let b = book(vec![l_ifd_bid(1, e, 1_000), l_ask(2, ask(2, P250, 4 * WHOLE, T3))]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
    let covs = &r.prepared[0].lowered.built.covenants;
    assert_eq!(covs.len(), 1, "one exit per fill");
    assert_eq!(covs[0].template, Some(kob_protocol::artifacts::TemplateId::KobCondAsk));
    // minFill: an entry with a minimum fill of 5 whole tokens and only 4 crossing is not filled.
    let mut e5 = ifd_bid(1, P260, 10 * WHOLE, T3);
    e5.min_fill = 5 * WHOLE;
    let b = book(vec![l_ifd_bid(1, e5, 1_000), l_ask(2, ask(2, P250, 4 * WHOLE, T3))]);
    assert_eq!(amount_all(&run(&input(&b), &cfg()), cid(1)), 0);
    // Sell-first entry at 2.50 sells into a 2.60 bid.
    let s = ifd_ask(3, P250, 10 * WHOLE, T3);
    let b = book(vec![l_ifd_ask(3, s, 1_000), l_bid(4, bid(4, P260, T3), 6 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(3)), 6 * WHOLE);
    assert_eq!(r.prepared[0].lowered.built.covenants.len(), 1);
}

#[test]
fn kron_books_are_planned_and_built_with_the_kron_builders() {
    // a KRON book next to a KCC-20 book of the same shape: one global batch fills both books in one transaction, each token
    // with its own program, leader, slots and state encoding
    let kcc = input(&book(vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]));
    let mut both = to_kron(&input(&book(vec![l_bid(11, bid(1, P260, T3), 5 * WHOLE), l_ask(12, ask(2, P250, 5 * WHOLE, T3))])));
    both.orders.extend(kcc.orders.clone());
    let r = tick(&both, &cfg(), &Families::default(), &signer());
    assert!(r.anomalies.is_empty() && r.skipped.is_empty(), "{:?} {:?}", r.anomalies, r.skipped);
    assert_eq!(r.prepared.len(), 1, "one transaction for both books");
    let p = &r.prepared[0];
    assert!(p.validation.is_some(), "engine-validated");
    check_invariants(p, &both);
    assert_eq!(p.plan.fills.len(), 4);
    assert_eq!(p.plan.families().len(), 2, "both families in one batch");
    for id in [1, 2, 11, 12] {
        assert_eq!(p.plan.amount_of(&cid(id)), 5 * WHOLE);
    }
    for (tokc, fam) in [(TOKEN_COV_KRON, Family::Kron), (TOKEN, Family::Kcc20)] {
        // its token inputs and outputs are of its own token, and KRON's are 46-byte states
        let ins = p.signed.tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(tokc)).count();
        assert!(ins >= 1 && ins <= fam.max_tok_in());
        assert!(p.signed.tx.outputs.iter().any(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == tokc)));
    }
    // the operator profit is both spreads less one fee
    assert!(p.accounting.profit > 0);
    assert_eq!(p.plan.margin, 2 * (5 * (P260 + TIP) - 5 * (P250 - TIP)));
}

#[test]
fn kron_slot_limits_and_output_cap_bound_a_transaction() {
    // six small asks and one bid that buys them all: KRON takes 4 token inputs (a 5-output limit) per transaction
    let mut orders: Vec<ListedOrder> = (1..=6).map(|i| l_ask(i, ask(2, P250 + i as i64 * 1_000, 2 * WHOLE, T3))).collect();
    orders.push(l_bid(20, bid(1, P260, T3), 12 * WHOLE));
    let k = to_kron(&input(&book(orders)));
    let r = tick(&k, &cfg(), &Families::default(), &signer());
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    let first = r.prepared.iter().find(|p| p.step == 0).expect("a first step");
    let tok_in = first.signed.tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(TOKEN_COV_KRON)).count();
    assert!(tok_in <= 4, "{tok_in} KRON token inputs");
    for p in &r.prepared {
        check_invariants(p, &k);
        for o in &p.signed.tx.outputs {
            if o.covenant.as_ref().is_some_and(|c| c.covenant_id == TOKEN_COV_KRON) {
                // every KRON token output holds 1..=1e9 units (the builders enforce it; the engine agrees)
                assert!(p.validation.is_some());
            }
        }
    }
    // everything is eventually planned across chained steps of at most 4 token inputs each
    let total: i64 = (1..=6).map(|i| amount_all(&r, cid(i))).sum();
    assert!(total >= 4 * WHOLE, "planned {total} base units");
}

// ------------------------------------------------------------------ repeat IFD / IFO (§6.1)

/// A repeating buy-first entry (id 50) holding `entry` base units and one booked exit (id `exit_id`)
/// of `exit` base units, take-profit 3.00, stop 2.20.
fn repeat_buy_first(exit_id: u32, exit: i64, entry: i64, until: i64) -> (ListedOrder, ListedOrder) {
    let e = IfdBidState { rpt_amount: 17 * WHOLE, amount_left: entry, ..ifd_bid(1, P260, 10 * WHOLE, T3) };
    // the entry's escrow and one spare exit carrier
    let ev = (e.escrow().expect("escrow") + EC) as u64;
    let entry_order = listed(cid(50), AnyState::KobIfdBid(e.clone()), ev, 1_000);
    let x = e.exit_for(exit, Some(Booking { parent: cid(50), until })).unwrap();
    let exit_order = listed(cid(exit_id), AnyState::KobCondAsk(x), CARRIER, 2_000);
    (entry_order, exit_order)
}

#[test]
fn booked_exit_take_profit_merges_its_entry() {
    let (entry, exit) = repeat_buy_first(51, 4 * WHOLE, 3 * WHOLE, EXPIRY);
    let b = book(vec![entry, exit, l_bid(2, bid(2, 305_000_000, T3), 3 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(51)), 3 * WHOLE);
    let f = r.prepared[0].plan.fills.iter().find(|f| f.cand.id == cid(51)).unwrap();
    assert_eq!((f.cand.leg, f.cand.merge), (0, Some(cid(50))), "take-profit carries its entry's merge");
    // The entry input is in the transaction exactly once.
    let n = r.prepared[0].signed.tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(cid(50))).count();
    assert_eq!(n, 1);
}

#[test]
fn booked_exit_without_its_entry_waits_until_rpt_until() {
    // The entry is not visible: no take-profit before rptUntil.
    let (_, exit) = repeat_buy_first(51, 4 * WHOLE, 3 * WHOLE, EXPIRY);
    let b = book(vec![exit.clone(), l_bid(2, bid(2, 305_000_000, T3), 3 * WHOLE)]);
    assert_eq!(amount_all(&run(&input(&b), &cfg()), cid(51)), 0);
    // After rptUntil the take-profit is plain.
    let (_, late) = repeat_buy_first(51, 4 * WHOLE, 3 * WHOLE, (NOW - 10) as i64);
    let b = book(vec![late, l_bid(2, bid(2, 305_000_000, T3), 3 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(51)), 3 * WHOLE);
    assert_eq!(r.prepared[0].plan.fills[0].cand.merge, None);
}

#[test]
fn stop_loss_of_a_booked_exit_never_carries_the_entry() {
    let (entry, mut exit) = repeat_buy_first(51, 4 * WHOLE, 3 * WHOLE, EXPIRY);
    if let AnyState::KobCondAsk(x) = &mut exit.order.state {
        x.armed = 1;
    }
    exit.order.utxo.block_daa_score = NOW - 150;
    let b = book(vec![entry, exit, l_bid(2, bid(2, 218_000_000, T3), 4 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(51)), 4 * WHOLE);
    let p = &r.prepared[0];
    assert_eq!(p.plan.fills.iter().find(|f| f.cand.id == cid(51)).unwrap().cand.leg, 1);
    assert!(p.signed.tx.inputs.iter().all(|i| i.utxo.covenant_id != Some(cid(50))), "no entry next to a stop-loss fill");
}

#[test]
fn two_booked_exits_of_one_entry_merge_in_separate_transactions() {
    let (entry, x1) = repeat_buy_first(51, 2 * WHOLE, 3 * WHOLE, EXPIRY);
    let (_, mut x2) = repeat_buy_first(52, 2 * WHOLE, 3 * WHOLE, EXPIRY);
    x2.order.utxo.block_daa_score = 2_100;
    let b = book(vec![entry, x1, x2, l_bid(2, bid(2, 305_000_000, T3), 4 * WHOLE)]);
    let r = run(&input(&b), &cfg());
    let merged: Vec<_> = r.prepared.iter().flat_map(|p| p.plan.fills.iter().filter(|f| f.cand.merge.is_some())).collect();
    assert_eq!(merged.len(), 1, "one merge per entry per transaction; the other waits for the next");
    assert_eq!(merged[0].cand.id, cid(51), "the older exit first");
}

#[test]
fn sell_first_repeat_merges_into_the_entry_custody() {
    for entry in [0, 3 * WHOLE] {
        let e = IfdAskState { rpt_amount: 17 * WHOLE, amount_left: entry, ..ifd_ask(1, P250, 10 * WHOLE, T3) };
        let ev = (e.escrow(CARRIER as i64).expect("escrow") + EC) as u64;
        let entry_order = listed(cid(60), AnyState::KobIfdAsk(e.clone()), ev, 1_000);
        let x = e.exit_for(4 * WHOLE, Some(Booking { parent: cid(60), until: EXPIRY })).unwrap();
        let xv = (x.rpt_proceeds(4 * WHOLE).expect("proceeds") + x.rpt_prefund(4 * WHOLE).expect("prefund") + e.exit_carrier) as u64;
        let exit = listed(cid(61), AnyState::KobCondBid(x), xv, 2_000);
        let b = book(vec![entry_order, exit, l_ask(2, ask(2, 235_000_000, 3 * WHOLE, T3))]);
        let r = run(&input(&b), &cfg());
        assert_eq!(amount_of(&r, cid(61)), 3 * WHOLE, "entry amount {entry}");
        let f = r.prepared[0].plan.fills.iter().find(|f| f.cand.id == cid(61)).unwrap();
        assert_eq!(f.cand.merge, Some(cid(60)));
        assert_eq!(f.cand.merge_custody, entry > 0);
    }
}

// ------------------------------------------------------------------ stop entries

#[test]
fn stop_entries_trigger_and_auction() {
    // Buy-stop entry at 2.55 (limit 2.60): a resting bid at 2.56 buys 5 of 9 whole tokens of a 2.54 ask; that fill (a bid at or
    // above the trigger) arms the entry inside its own fill of the other 4 (both families).
    let e = IfdBidState { entry_stop: P255, ..ifd_bid(1, P260, 10 * WHOLE, T3) };
    let b = book(vec![
        l_ifd_bid(1, e.clone(), 2_000),
        l_bid(5, bid(5, 256_000_000, T3), 5 * WHOLE),
        l_ask(2, ask(2, 254_000_000, 9 * WHOLE, T3)),
    ]);
    for r in both(&input(&b)) {
        assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
        assert_eq!(r.prepared[0].plan.fills[fill_at(&r, cid(1))].evidence, Some(fill_at(&r, cid(5))));
    }
    // Armed stop entry: auction from the trigger toward the limit.
    let armed = IfdBidState { armed: 1, ..e.clone() };
    let b = book(vec![l_ifd_bid(1, armed, NOW - 200), l_ask(2, ask(2, 257_000_000, 4 * WHOLE, T3))]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_of(&r, cid(1)), 4 * WHOLE);
    // Without room for its fill the entry is armed by an update instead.
    let b =
        book(vec![l_ifd_bid(1, e, 2_000), l_bid(5, bid(5, 256_000_000, T3), 5 * WHOLE), l_ask(2, ask(2, 254_000_000, 5 * WHOLE, T3))]);
    let r = run(&input(&b), &cfg());
    assert_eq!(amount_all(&r, cid(1)), 0);
    assert_eq!(r.prepared[0].plan.updates.iter().map(|u| (u.id, u.kind)).collect::<Vec<_>>(), vec![(cid(1), UpdateKind::Arm)]);
    // Sell-stop entry at 2.50 (limit 2.45) triggered by a resting ask at 2.49 that a 2.51 bid of 9 whole tokens buys.
    let s = IfdAskState { entry_stop: P250, ..ifd_ask(3, P245, 10 * WHOLE, T3) };
    let b =
        book(vec![l_ifd_ask(3, s, 2_000), l_bid(4, bid(4, 251_000_000, T3), 9 * WHOLE), l_ask(6, ask(6, 249_000_000, 5 * WHOLE, T3))]);
    for r in both(&input(&b)) {
        assert_eq!(amount_of(&r, cid(3)), 4 * WHOLE);
        assert_eq!(r.prepared[0].plan.fills[fill_at(&r, cid(3))].evidence, Some(fill_at(&r, cid(6))));
    }
}

// ------------------------------------------------------------------ decay / rising

#[test]
fn dutch_ask_and_rising_bid() {
    let mut du = ask(1, 300_000_000, 4 * WHOLE, T3);
    du.slope = 1_000_000;
    du.price_end = 200_000_000;
    du.decay_step = 10_000;
    // After ~1,000,000 DAA the quote is at its floor 2.00; a 2.10 bid crosses.
    let b = book(vec![l_ask(1, du), l_bid(2, bid(2, 210_000_000, T3), 4 * WHOLE)]);
    assert_eq!(amount_of(&run(&input(&b), &cfg()), cid(1)), 4 * WHOLE);
    let mut rb = bid(3, 200_000_000, T3);
    rb.slope = 1_000_000;
    rb.price_end = P260;
    rb.decay_step = 10_000;
    let b = book(vec![l_bid(3, rb, 4 * WHOLE), l_ask(4, ask(4, P255, 4 * WHOLE, T3))]);
    assert_eq!(amount_of(&run(&input(&b), &cfg()), cid(3)), 4 * WHOLE);
}

// ------------------------------------------------------------------ chaining (§7)

#[test]
fn oversize_crossing_is_chained() {
    // 8/8 token: one bid for 12 whole tokens against twelve asks of one: the first step takes what fits in
    // one transaction, the next steps continue on the bid's unaccepted continuation.
    let bd = bid(1, P260, T8);
    // Funded for 12 whole tokens delivered in up to two fills.
    let mut orders = vec![listed(cid(1), AnyState::KobBid(bd.clone()), bd.escrow(12 * WHOLE, 2).expect("escrow") as u64, 1_000)];
    orders.extend((0..12).map(|i| l_ask(100 + i, ask(2, P250 + i as i64 * 100_000, WHOLE, T8))));
    let r = run(&input(&book(orders)), &cfg());
    assert!(r.prepared.len() >= 2, "chained steps: {}", r.prepared.len());
    assert_eq!(r.prepared[0].step, 0);
    for w in r.prepared.windows(2) {
        assert_eq!(w[1].parent, Some(w[0].txid()), "each step spends its parent's outputs");
    }
    assert_eq!(amount_all(&r, cid(1)), 12 * WHOLE);
    // The first step took the cheapest asks.
    assert_eq!(r.prepared[0].plan.amount_of(&cid(100)), WHOLE);
}

#[test]
fn chaining_can_be_disabled() {
    let mut orders = vec![l_bid(1, bid(1, P260, T8), 12 * WHOLE)];
    orders.extend((0..12).map(|i| l_ask(100 + i, ask(2, P250, WHOLE, T8))));
    let mut k = cfg();
    k.chain_unconfirmed = false;
    let r = run(&input(&book(orders)), &k);
    assert_eq!(r.prepared.len(), 1);
    assert!(amount_of(&r, cid(1)) < 12 * WHOLE);
}

// ------------------------------------------------------------------ mixes

#[test]
fn mixed_book_classes_and_legs() {
    // IOC sell, an armed stop sell, a TP-only conditional, a resting ask, a buy-first entry, a FOK
    // bid and two resting bids on an 8/8 token.
    let mut ioc = ask(1, P255, 2 * WHOLE, T8);
    ioc.tif = TIF_IOC;
    let mut stop = cond_ask(2, 0, 250_000_000, 2 * WHOLE, T8);
    stop.armed = 1;
    let tp = cond_ask(3, 252_000_000, 0, 2 * WHOLE, T8);
    let mut fok = bid(6, P260, T8);
    fok.tif = TIF_FOK;
    let orders = vec![
        fresh(l_ask(1, ioc)),
        l_cond_ask(2, stop, NOW - 100),
        l_cond_ask(3, tp, 2_000),
        l_ask(4, ask(4, P250, 3 * WHOLE, T8)),
        l_ifd_bid(5, ifd_bid(5, 256_000_000, 2 * WHOLE, T8), 1_000),
        fresh(l_bid(6, fok, 3 * WHOLE)),
        l_bid(7, bid(7, 258_000_000, T8), 2 * WHOLE),
        l_bid(8, bid(8, 245_000_000, T8), 2 * WHOLE),
    ];
    let r = run(&input(&book(orders)), &cfg());
    assert!(!r.prepared.is_empty());
    assert_eq!(amount_of(&r, cid(1)), 2 * WHOLE, "the IOC sold everything it could");
    let fok_amount = amount_all(&r, cid(6));
    // a FOK bid is complete when less than one minimum fill of buying power is left: more than 2 of its 3 whole tokens
    assert!(fok_amount == 0 || (fok_amount > 2 * WHOLE && fok_amount <= 3 * WHOLE), "{fok_amount}");
    assert_eq!(amount_all(&r, cid(8)), 0, "2.45 crosses nothing");
}

// ------------------------------------------------------------------ a full repeat cycle

/// The exit a batch created, recovered from the built transaction exactly as the indexer derives
/// it (§1.1: no new placement record): state from the entry, id from the genesis, custody from the
/// delivery output.
fn exit_from_tx(
    p: &kob_executor::matcher::engine::Prepared,
    entry: &IfdBidState,
    entry_id: [u8; 32],
    n: i64,
    entry_daa: u64,
    accepted: u64,
) -> ListedOrder {
    let built = &p.lowered.built;
    let g =
        built.covenants.iter().find(|c| c.template == Some(kob_protocol::artifacts::TemplateId::KobCondAsk)).expect("exit genesis");
    let until = rpt_until(entry.expiry_daa, p.plan.lock_time as i64, entry_daa as i64).expect("rptUntil");
    let state = entry.exit_for(n, Some(Booking { parent: entry_id, until })).unwrap();
    let idx = g.outputs[0] as usize;
    assert_eq!(built.tx.outputs[idx].script_public_key, kob_protocol::tx::spk_to_string(&AnyState::KobCondAsk(state.clone()).spk()));
    let program = kob_protocol::artifacts::template(T3);
    let cst = Kcc20State::custody(n, g.covenant_id, EXT);
    let cspk = kob_protocol::tx::spk_to_string(&cst.spk_with(program));
    let cidx = built.tx.outputs.iter().position(|o| o.script_public_key == cspk).expect("exit custody");
    let u = |i: usize, cov| kob_protocol::tx::Utxo {
        transaction_id: built.tx.id,
        index: i as u32,
        amount: built.tx.outputs[i].value,
        block_daa_score: accepted,
        covenant_id: cov,
    };
    ListedOrder {
        family: kob_executor::matcher::family::Family::Kcc20,
        order: kob_protocol::tx::OrderUtxo { utxo: u(idx, Some(g.covenant_id)), state: AnyState::KobCondAsk(state) },
        custody: Some(kob_protocol::tx::TokenUtxo { utxo: u(cidx, Some(TOKEN)), state: cst.into() }),
        custody_b: None,
        deadline: None,
        seen_daa: accepted,
        foreign: vec![],
        strays: vec![],
    }
}

#[test]
fn repeat_cycle_entry_fill_then_take_profit_with_merge() {
    // Tick 1: a repeating buy-first entry (2 whole tokens per cycle) buys 2 whole tokens from an ask.
    let p = 250_000_000;
    let exit_tpl = CondAskState { expiry_daa: NO_EXPIRY, ..cond_ask(1, 260_000_000, 240_000_000, WHOLE, T3) };
    let e = IfdBidState {
        rpt_amount: 7 * WHOLE,
        amount_left: 2 * WHOLE,
        min_fill: 2 * WHOLE,
        exit_state: IfdBidState::commit_exit(&exit_tpl),
        ..ifd_bid(1, p, 2 * WHOLE, T3)
    };
    // the entry's escrow with room to spare (a delivery carrier and two exit carriers more)
    let ev = (e.escrow().expect("escrow") + DC + 2 * EC) as u64;
    let entry = listed(cid(70), AnyState::KobIfdBid(e.clone()), ev, 1_000);
    let b1 = book(vec![entry.clone(), l_ask(2, ask(2, 248_000_000, 2 * WHOLE, T3))]);
    let r1 = run(&input(&b1), &cfg());
    assert_eq!(amount_of(&r1, cid(70)), 2 * WHOLE);
    let tx1 = &r1.prepared[0];
    // The entry continues with nothing left and rptAmount 5 whole tokens; its exit is booked.
    let accepted = NOW + 20;
    let exit = exit_from_tx(tx1, &e, cid(70), 2 * WHOLE, 1_000, accepted);
    let e2 = IfdBidState { amount_left: 0, rpt_amount: 5 * WHOLE, ..e.clone() };
    let e2spk = kob_protocol::tx::spk_to_string(&AnyState::KobIfdBid(e2.clone()).spk());
    let eidx = tx1.lowered.built.tx.outputs.iter().position(|o| o.script_public_key == e2spk).expect("entry continuation");
    let mut entry2 = entry.clone();
    entry2.order = kob_protocol::tx::OrderUtxo {
        utxo: kob_protocol::tx::Utxo {
            transaction_id: tx1.txid(),
            index: eidx as u32,
            amount: tx1.lowered.built.tx.outputs[eidx].value,
            block_daa_score: accepted,
            covenant_id: Some(cid(70)),
        },
        state: AnyState::KobIfdBid(e2),
    };
    // Tick 2: a bid at the take-profit: the exit sells and merges its entry (re-armed with 2 whole tokens).
    let mut b2 = book(vec![entry2, exit.clone(), l_bid(3, bid(3, 270_000_000, T3), 2 * WHOLE)]);
    b2.daa_score = accepted + 10;
    let r2 = run(&input(&b2), &cfg());
    assert_eq!(amount_of(&r2, exit.id()), 2 * WHOLE);
    let f = r2.prepared[0].plan.fills.iter().find(|f| f.cand.id == exit.id()).unwrap();
    assert_eq!(f.cand.merge, Some(cid(70)));
    let e3 = IfdBidState { amount_left: 2 * WHOLE, armed: 0, rpt_amount: 5 * WHOLE, ..e };
    let e3spk = kob_protocol::tx::spk_to_string(&AnyState::KobIfdBid(e3).spk());
    assert!(
        r2.prepared[0].lowered.built.tx.outputs.iter().any(|o| o.script_public_key == e3spk),
        "the entry is back with its 2 whole tokens"
    );
}

// ---------------------------------------------------------------------------------------------
// fee policy (`kob_executor::fee`, docs/ops/executor.md "Fee policy"): the rate a batch is built at, and its profit at it

fn rates(normal: u64, high: u64, max_tx_fee: u64) -> kob_executor::fee::FeeRates {
    kob_executor::fee::FeeRates { floor: 100, low: 100, normal, high, max_tx_fee, estimated: true }
}

fn priced(normal: u64, high: u64, max_tx_fee: u64) -> kob_executor::matcher::engine::EngineConfig {
    let mut c = cfg();
    c.planner.fee_rate = normal;
    c.fees = rates(normal, high, max_tx_fee);
    c
}

/// The exact profit of a prepared batch at the fee it pays, and the rate it was built at.
fn rate_of(p: &kob_executor::matcher::engine::Prepared) -> u64 {
    let f = &p.lowered.built.fee;
    assert_eq!(f.fee, p.accounting.fee, "the accounting is the built transaction's");
    assert!(f.fee >= f.fee_rate * f.mass.fee_mass, "fee {} below its rate {} x mass {}", f.fee, f.fee_rate, f.mass.fee_mass);
    f.fee_rate
}

#[test]
fn urgent_batches_pay_the_high_rate_resting_ones_the_planned_rate() {
    let c = priced(150, 300, 0);
    // resting: the normal (planned) rate
    let r = run(&input(&book(vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))])), &c);
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    assert_eq!(rate_of(&r.prepared[0]), 150);
    let resting_fee = r.prepared[0].accounting.fee;
    // an IOC fill: the high rate, and the batch is still profitable at it
    let mut ib = bid(1, P260, T3);
    ib.tif = TIF_IOC;
    let r = run(&input(&book(vec![fresh(l_bid(1, ib, 5 * WHOLE)), l_ask(2, ask(2, P250, 5 * WHOLE, T3))])), &c);
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    let p = &r.prepared[0];
    assert!(p.plan.fills.iter().any(|f| f.cand.class == kob_executor::matcher::candidate::Class::Immediate));
    assert_eq!(rate_of(p), 300);
    assert!(p.accounting.fee > resting_fee, "twice the rate: a larger fee ({} vs {resting_fee})", p.accounting.fee);
    assert!(p.accounting.profit >= c.planner.min_profit);
    // the default configuration (no policy): the planned rate everywhere, as before
    let mut ib = bid(1, P260, T3);
    ib.tif = TIF_IOC;
    let r = run(&input(&book(vec![fresh(l_bid(1, ib, 5 * WHOLE)), l_ask(2, ask(2, P250, 5 * WHOLE, T3))])), &cfg());
    assert_eq!(rate_of(&r.prepared[0]), 100);
}

#[test]
fn an_urgent_batch_too_thin_for_the_high_rate_goes_at_the_highest_rate_it_pays() {
    // 5 whole tokens, 0.05 KAS per whole token of spread plus tips: about 0.26 KAS of margin; at 100,000 sompi per gram the fee would be
    // tens of KAS, so the batch goes at the rate its margin pays, never below the planned 150
    let c = priced(150, 100_000, 0);
    let mut ib = bid(1, P255, T3);
    ib.tif = TIF_IOC;
    let r = run(&input(&book(vec![fresh(l_bid(1, ib, 5 * WHOLE)), l_ask(2, ask(2, P250, 5 * WHOLE, T3))])), &c);
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    let p = &r.prepared[0];
    let rate = rate_of(p);
    let mass = p.lowered.built.fee.mass.fee_mass as i64;
    assert!(rate > 150 && rate < 100_000, "rate {rate}");
    assert!(p.accounting.profit >= c.planner.min_profit, "profitable at the rate it pays: {:?}", p.accounting);
    // the highest such rate: one or two more sompi per gram would eat the rest of the margin
    assert!(p.accounting.profit < c.planner.min_profit + 3 * mass, "profit {} left at rate {rate} (mass {mass})", p.accounting.profit);
}

#[test]
fn a_batch_is_only_built_when_profitable_at_the_rate_it_pays() {
    let b = book(vec![l_bid(1, bid(1, P255, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    // profitable at the floor
    let r = run(&input(&b), &priced(100, 100, 0));
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    let p = &r.prepared[0];
    let margin = p.accounting.profit + p.accounting.fee as i64;
    // a normal rate at which the fee exceeds the margin: nothing is built (the planner prices every allocation at it)
    let mass = p.lowered.built.fee.mass.fee_mass;
    let too_high = (margin as u64 / mass) * 3 / 2;
    let r = run(&input(&b), &priced(too_high, too_high, 0));
    assert!(
        r.prepared.is_empty(),
        "built at {too_high} sompi per gram: {:?}",
        r.prepared.iter().map(|p| p.accounting).collect::<Vec<_>>()
    );
    // just below the break-even rate it is still built, and pays that rate
    let ok = (margin as u64 / mass) * 2 / 3;
    let r = run(&input(&b), &priced(ok, ok, 0));
    assert_eq!(r.prepared.len(), 1, "{:?}", r.skipped);
    assert_eq!(rate_of(&r.prepared[0]), ok);
    assert!(r.prepared[0].accounting.profit > 0);
}

#[test]
fn the_total_cap_lowers_the_rate_never_below_the_floor() {
    let mut ib = bid(1, P260, T3);
    ib.tif = TIF_IOC;
    let b = book(vec![fresh(l_bid(1, ib, 5 * WHOLE)), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let floor_fee = run(&input(&b), &priced(100, 100, 0)).prepared[0].accounting.fee;
    // a cap of three times the floor fee: the high rate (1,000) is cut to about 300
    let r = run(&input(&b), &priced(100, 1_000, 3 * floor_fee));
    let p = &r.prepared[0];
    let rate = rate_of(p);
    assert!(p.accounting.fee <= 3 * floor_fee, "fee {} over the cap {}", p.accounting.fee, 3 * floor_fee);
    assert!((250..=300).contains(&rate), "rate {rate}");
    // a cap below the floor fee: the floor
    let r = run(&input(&b), &priced(100, 1_000, floor_fee / 2));
    assert_eq!(rate_of(&r.prepared[0]), 100);
}
