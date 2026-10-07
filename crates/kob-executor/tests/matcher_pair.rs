//! Pair orders in the matcher's global batch (`matcher::batch`, `matcher::pair`) on in-memory books: routed through the two
//! KAS books (a pair ASK sells A into bids of A and buys B from asks of B; a pair BID sells B into bids of B and buys A from
//! asks of A), netted against opposite pair orders (any number per side, exact rounded amounts, the surplus to the pair asks
//! or sold into the KAS bids), FOK / IOC / minimum fill, pair conditionals armed in the batch of their evidence (two
//! KAS-book fills, or a resting pair fill), if-done pair fills creating their exits. Every transaction is engine-validated
//! and leaves the operator no token. Amounts are base units (`WHOLE` = one whole token), prices base units of B per whole A.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_executor::matcher::planner::UpdateKind;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::Leg;
use kob_protocol::state::*;

const K3: TemplateId = TemplateId::KronToken2433;

/// A pair ASK of `amount` A at 1.2 B per A, KAS bids of A at 2.60 and KAS asks of B at 2.00 (the route sells a whole A for
/// 2.60 KAS and buys 1.2 whole B for 2.40 KAS).
fn route_ask_scene(pa: TemplateId, pb: TemplateId, amount: i64, tif: i64, bids: &[i64], asks: &[i64]) -> Vec<ListedOrder> {
    let mut v = vec![l_pair(1, pa, pb, pair_state(1, true, pa, pb, amount, 1_200, 0, tif), 1_000)];
    for (k, n) in bids.iter().enumerate() {
        v.push(l_bid_a(100 + k as u32, pa, 2, P260, *n));
    }
    for (k, n) in asks.iter().enumerate() {
        v.push(l_ask_b(200 + k as u32, pa, pb, 3, P200, *n));
    }
    if tif != TIF_GTC {
        v[0] = fresh(v[0].clone());
    }
    v
}

#[test]
fn a_pair_ask_is_routed_through_both_kas_books_in_every_family_pair() {
    for (pa, pb) in [(T8, T8), (T3, T8), (T8, T3), (T3, K3), (K3, T3), (K3, K3)] {
        let r = run_pair(route_ask_scene(pa, pb, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[10 * WHOLE]), &cfg());
        assert_eq!(r.prepared.len(), 1, "{} / {}", pa.name(), pb.name());
        assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
        let ids = r.prepared[0].plan.spent_ids();
        assert!(ids.contains(&cid(100)) && ids.contains(&cid(200)));
        // pair fills lead the transaction
        assert_eq!(r.prepared[0].signed.tx.inputs[0].utxo.covenant_id, Some(cid(1)));
    }
}

#[test]
fn a_pair_bid_is_routed_selling_its_b_and_buying_exactly_its_a() {
    for (pa, pb) in [(T8, T8), (T3, T8), (K3, T3)] {
        // a BID of 10 A at 1.4 B per A: its B sold into bids of B at 2.00, its A bought from asks of A at 2.50
        let y = pair_state(1, false, pa, pb, 10 * WHOLE, 1_400, 0, TIF_GTC);
        let v = vec![l_pair(1, pa, pb, y, 1_000), l_bid_b(100, pa, pb, 2, P200, 30 * WHOLE), l_ask_a(200, pa, 3, P250, 6 * WHOLE)];
        let r = run_pair(v, &cfg());
        assert_eq!(r.prepared.len(), 1, "{} / {}", pa.name(), pb.name());
        assert_eq!(amount_in(&r, cid(1)), 6 * WHOLE, "exactly the A the asks hold");
        // the A bought is exactly the bid's fill: the ask's whole 6 A
        assert_eq!(amount_in(&r, cid(200)), 6 * WHOLE);
        // the B sold is exactly floor(6 x 1.4) whole B
        assert_eq!(amount_in(&r, cid(100)), 8_400);
    }
}

/// A pair ASK and a pair BID of the same pair at crossing prices, no KAS book at all: netted.
#[test]
fn opposite_pair_orders_net_without_any_kas_book() {
    for (pa, pb) in [(T8, T8), (T3, T3), (K3, T3), (K3, K3)] {
        let x = pask(1, pa, pb, 4 * WHOLE, RATE, PTIP);
        let y = pbid(2, pa, pb, 4 * WHOLE, RATE, PTIP);
        let r = run_pair(vec![l_pair(1, pa, pb, x, 1_000), l_pair(2, pa, pb, y, 1_000)], &cfg());
        assert_eq!(r.prepared.len(), 1, "{} / {}", pa.name(), pb.name());
        assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
        assert_eq!(amount_in(&r, cid(2)), 4 * WHOLE);
        assert_eq!(r.prepared[0].plan.fills.len(), 2, "two pair legs, no KAS leg");
    }
}

#[test]
fn netting_without_tips_does_not_pay_its_fee() {
    let x = pask(1, T8, T8, 4 * WHOLE, RATE, 0);
    let y = pbid(2, T8, T8, 4 * WHOLE, RATE, 0);
    let r = run_pair(vec![l_pair(1, T8, T8, x, 1_000), l_pair(2, T8, T8, y, 1_000)], &cfg());
    assert!(r.prepared.is_empty(), "a netting that earns nothing is not built");
}

#[test]
fn netting_two_by_two() {
    // 4 outputs of one token: an 8x8 program
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, RATE, PTIP), 1_000),
        l_pair(2, T8, T8, pask(2, T8, T8, 2 * WHOLE, RATE + 10, PTIP), 1_000),
        l_pair(3, T8, T8, pbid(3, T8, T8, 2 * WHOLE, RATE + 20, PTIP), 1_000),
        l_pair(4, T8, T8, pbid(4, T8, T8, 2 * WHOLE, RATE + 10, PTIP), 1_000),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    for id in 1..=4 {
        assert_eq!(amount_in(&r, cid(id)), 2 * WHOLE, "order {id}");
    }
}

#[test]
fn netting_three_by_one_with_the_remainder_routed() {
    // three asks of 2 A each against one bid of 10 A at 1.4: 6 A netted, the bid's other 4 A routed (its B sold to bids of
    // B at 2.00, A bought from asks of A at 2.50)
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, RATE, PTIP), 1_000),
        l_pair(2, T8, T8, pask(2, T8, T8, 2 * WHOLE, RATE + 10, PTIP), 1_000),
        l_pair(3, T8, T8, pask(3, T8, T8, 2 * WHOLE, RATE + 20, PTIP), 1_000),
        l_pair(4, T8, T8, pbid(4, T8, T8, 10 * WHOLE, 1_400, PTIP), 1_000),
        l_bid_b(100, T8, T8, 5, P200, 40 * WHOLE),
        l_ask_a(200, T8, 6, P250, 4 * WHOLE),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    for id in 1..=3 {
        assert_eq!(amount_in(&r, cid(id)), 2 * WHOLE, "ask {id}");
    }
    assert_eq!(amount_in(&r, cid(4)), 10 * WHOLE, "the bid: 6 netted and 4 routed");
    assert_eq!(amount_in(&r, cid(200)), 4 * WHOLE, "the route bought the remainder");
    assert!(amount_in(&r, cid(100)) > 0, "the route sold B");
}

#[test]
fn a_netting_surplus_is_sold_to_a_kas_bid_when_that_pays() {
    // ask at 1.0, bid at 1.5: 0.5 B per A over; a KAS bid of B at 2.00 takes it
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 4 * WHOLE, RATE, PTIP), 1_000),
        l_pair(2, T8, T8, pbid(2, T8, T8, 4 * WHOLE, 1_500, PTIP), 1_000),
        l_bid_b(100, T8, T8, 5, P200, 10 * WHOLE),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
    assert_eq!(amount_in(&r, cid(2)), 4 * WHOLE);
    assert_eq!(amount_in(&r, cid(100)), 2 * WHOLE, "the 2 whole B of surplus sold for KAS");
}

#[test]
fn fok_is_all_or_nothing_and_ioc_takes_what_the_books_allow() {
    // FOK of 10 A: the books hold 4 A of bids only: nothing
    let r = run_pair(route_ask_scene(T8, T8, 10 * WHOLE, TIF_FOK, &[4 * WHOLE], &[20 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(1)), 0);
    // with enough bids: whole
    let r = run_pair(route_ask_scene(T8, T8, 10 * WHOLE, TIF_FOK, &[6 * WHOLE, 6 * WHOLE], &[20 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(1)), 10 * WHOLE);
    // IOC: as much as the books allow, the rest returned
    let r = run_pair(route_ask_scene(T8, T8, 10 * WHOLE, TIF_IOC, &[4 * WHOLE], &[20 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
}

#[test]
fn the_minimum_fill_holds_for_netted_and_routed_pair_orders() {
    // a bid with a minimum fill of 3 A against an ask of 2 A: no netting below its minimum fill
    let mut y = pbid(2, T8, T8, 10 * WHOLE, RATE, PTIP);
    y.min_fill = 3 * WHOLE;
    let v = vec![l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, RATE, PTIP), 1_000), l_pair(2, T8, T8, y, 1_000)];
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(2)), 0);
}

/// A sell stop (KobCondPair ASK, stop 0.95 B per A) armed by a resting pair ASK at 0.90 filled in the batch (mode 1).
#[test]
fn a_pair_stop_is_armed_by_a_resting_pair_fill_of_the_batch() {
    let stop = cond_pair(7, true, T8, T8, 4 * WHOLE, 0, 950);
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, 900, PTIP), 1_000),
        l_pair(2, T8, T8, pbid(2, T8, T8, 2 * WHOLE, 900, PTIP), 1_000),
        l_cond_pair(3, T8, T8, stop, 1_000),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    let armed = p.plan.updates.iter().any(|u| u.id == cid(3) && u.kind == UpdateKind::Arm && u.evidence_b.is_none());
    let filled = p.plan.fills.iter().any(|f| f.cand.id == cid(3) && f.evidence.is_some());
    assert!(armed || filled, "the stop is armed (or triggered) next to its evidence");
}

/// A sell stop armed by two KAS-book fills of the batch (mode 0): a resting ask of A at 1.90 KAS and a resting bid of B at
/// 2.00 KAS (implied rate 0.95 B per A, at the stop).
#[test]
fn a_pair_stop_is_armed_by_two_kas_book_fills_of_the_batch() {
    let stop = cond_pair(7, true, T8, T8, 4 * WHOLE, 0, 950);
    let v = vec![
        l_ask_a(10, T8, 1, 190_000_000, 2 * WHOLE),
        l_bid_a(11, T8, 2, 195_000_000, 2 * WHOLE),
        l_ask_b(20, T8, T8, 3, 195_000_000, 2 * WHOLE),
        l_bid_b(21, T8, T8, 4, 200_000_000, 2 * WHOLE),
        l_cond_pair(3, T8, T8, stop, 1_000),
    ];
    let r = run_pair(v, &cfg());
    let up = r.prepared.iter().flat_map(|p| p.plan.updates.iter()).find(|u| u.id == cid(3));
    let up = up.expect("the stop is armed");
    assert_eq!(up.kind, UpdateKind::Arm);
    assert!(up.evidence_b.is_some(), "mode 0: a fill of A and one of B");
}

#[test]
fn a_buy_first_pair_entry_fill_creates_its_exit() {
    // a buy-first entry of 4 A at 1.4 B per A: its B sold into bids of B, its A bought from asks of A
    let e = ifd_pair(1, true, T8, T8, 4 * WHOLE, 1_400);
    let v = vec![l_ifd_pair(1, T8, T8, e, 1_000), l_bid_b(100, T8, T8, 2, P200, 10 * WHOLE), l_ask_a(200, T8, 3, P250, 4 * WHOLE)];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
    let b = batch_request(&r.prepared[0]);
    assert!(b.legs.iter().any(|l| matches!(l, Leg::IfdPair { .. })));
    assert!(!r.prepared[0].lowered.built.covenants.is_empty(), "the exit is a new covenant");
}

#[test]
fn a_sell_first_pair_entry_nets_against_a_pair_bid() {
    // a sell-first entry of 4 A at 1.0 against a pair BID of 4 A at 1.0: the entry's exit custody holds exactly its
    // proceeds plus its prefund
    let e = ifd_pair(1, false, T8, T8, 4 * WHOLE, RATE);
    let mut e = e;
    e.tip = PTIP;
    let v = vec![l_ifd_pair(1, T8, T8, e, 1_000), l_pair(2, T8, T8, pbid(2, T8, T8, 4 * WHOLE, RATE, PTIP), 1_000)];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
    assert_eq!(amount_in(&r, cid(2)), 4 * WHOLE);
}

/// An unarmed pair sell stop (0.95 B per A) triggered and filled in the batch of its evidence: a resting pair ASK at 0.90
/// netted with a pair BID (mode 1); the stop itself is routed (A sold into bids of A, B bought from asks of B).
#[test]
fn an_unarmed_pair_stop_is_filled_next_to_its_evidence() {
    let stop = cond_pair(7, true, T8, T8, 2 * WHOLE, 0, 950);
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, 900, PTIP), 1_000),
        l_pair(2, T8, T8, pbid(2, T8, T8, 2 * WHOLE, 900, PTIP), 1_000),
        l_cond_pair(3, T8, T8, stop, 1_000),
        l_bid_a(100, T8, 4, P260, 2 * WHOLE),
        l_ask_b(200, T8, T8, 5, P200, 4 * WHOLE),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    let f = p.plan.fills.iter().find(|f| f.cand.id == cid(3)).expect("the stop is filled");
    assert_eq!(f.amount, 2 * WHOLE);
    let ev = f.evidence.expect("its evidence");
    assert_eq!(p.plan.fills[ev].cand.id, cid(1), "mode 1: the resting pair ASK");
    assert!(f.evidence_b.is_none());
    let b = batch_request(p);
    assert!(matches!(
        &b.legs[p.plan.fills.iter().position(|f| f.cand.id == cid(3)).unwrap()],
        Leg::CondPair { evidence: Some(_), .. }
    ));
}

/// A trailing pair sell stop (stop 0.80, step 0.05, gap 0.05) ratcheted up by a resting pair BID filled at 1.00 (mode 1):
/// the most steps below the rate less the gap.
#[test]
fn a_trailing_pair_stop_is_ratcheted_by_a_resting_pair_fill() {
    let mut stop = cond_pair(7, true, T8, T8, 4 * WHOLE, 0, 800);
    stop.trail_step = 50;
    stop.trail_gap = 50;
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, 1_000, PTIP), 1_000),
        l_pair(2, T8, T8, pbid(2, T8, T8, 2 * WHOLE, 1_000, PTIP), 1_000),
        l_cond_pair(3, T8, T8, stop, 1_000),
    ];
    let r = run_pair(v, &cfg());
    let up = r.prepared.iter().flat_map(|p| p.plan.updates.iter()).find(|u| u.id == cid(3)).expect("ratcheted");
    assert_eq!(up.kind, UpdateKind::Trail);
    assert_eq!(up.steps, 3, "0.80 -> 0.95: 0.95 + gap 0.05 <= 1.00");
}

/// An ASK of A/B (sells A) and an ASK of B/A (sells B for A): both receipts are minimums, they net.
#[test]
fn pair_orders_of_both_orientations_net() {
    let x = pask(1, T8, T8, 4 * WHOLE, RATE, PTIP);
    let mut z = pask(2, T8, T8, 4 * WHOLE, RATE, PTIP);
    std::mem::swap(&mut z.s_cov_id, &mut z.t_cov_id);
    z.s_cov_id = TOKEN_B;
    z.t_cov_id = TOKEN;
    let r = run_pair(vec![l_pair(1, T8, T8, x, 1_000), l_pair(2, T8, T8, z, 1_000)], &cfg());
    assert_eq!(r.prepared.len(), 1);
    assert_eq!(amount_in(&r, cid(1)), 4 * WHOLE);
    assert_eq!(amount_in(&r, cid(2)), 4 * WHOLE);
}

/// A repeating buy-first pair entry and the exit it booked: the exit's take-profit (1.60) nets with a pair BID at 1.70 and
/// re-arms the entry in the same transaction (the merge argument), the entry's B escrow growing by the exit's budget.
#[test]
fn a_booked_pair_exit_takes_profit_and_re_arms_its_entry() {
    let mut e = ifd_pair(1, true, T8, T8, 4 * WHOLE, 1_400);
    e.rpt_amount = 1 + 20 * WHOLE;
    e.custody = e.b_custody_needed().unwrap();
    let x = e.exit_for(2 * WHOLE, 2 * WHOLE, Some(Booking { parent: cid(1), until: NO_EXPIRY })).unwrap();
    assert_eq!(x.tp_price, 1_600);
    let v = vec![
        l_ifd_pair(1, T8, T8, e, 1_000),
        l_cond_pair(2, T8, T8, x, 1_000),
        l_pair(3, T8, T8, pbid(3, T8, T8, 2 * WHOLE, 1_700, PTIP), 1_000),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    let f = p.plan.fills.iter().find(|f| f.cand.id == cid(2)).expect("the exit takes profit");
    assert_eq!(f.cand.merge, Some(cid(1)), "with its entry's merge");
    assert_eq!(f.amount, 2 * WHOLE);
    let b = batch_request(p);
    assert!(b.legs.iter().any(|l| matches!(l, Leg::CondPair { merge: Some(_), .. })));
}

/// A repeating sell-first pair entry's booked exit (a buy-back at 0.80) re-arms its entry: its A goes into the entry's
/// custody, its prefund back. A re-arm of a sell-first entry is the largest pair shape (about 47 kB on 8x8 programs: the
/// entry's A and B custodies reveal the token program twice): the pair ask's tip pays it.
#[test]
fn a_booked_sell_first_exit_buys_back_and_re_arms_its_entry() {
    let mut e = ifd_pair(1, false, T8, T8, 4 * WHOLE, RATE);
    e.rpt_amount = 1 + 20 * WHOLE;
    e.custody = e.b_custody_needed().unwrap();
    let n = 2 * WHOLE;
    let x_cust = e.proceeds(n, e.price).unwrap() + e.pre_of(n).unwrap();
    let x = e.exit_for(n, x_cust, Some(Booking { parent: cid(1), until: NO_EXPIRY })).unwrap();
    assert_eq!(x.tp_price, 800);
    let v = vec![
        l_ifd_pair(1, T8, T8, e, 1_000),
        l_cond_pair(2, T8, T8, x, 1_000),
        l_pair(3, T8, T8, pask(3, T8, T8, 2 * WHOLE, 700, 4 * PTIP), 1_000),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let f = r.prepared[0].plan.fills.iter().find(|f| f.cand.id == cid(2)).expect("the exit buys back");
    assert_eq!(f.cand.merge, Some(cid(1)));
}

/// A pair BID of 10 A nets 6 A with three asks; the route of its other 4 A would lose (A costs 3.00 KAS at the asks of A,
/// its B fetches 2.00 KAS per B at 1.4 B per A: 2.80): the netting is built, the route is not.
#[test]
fn a_netted_order_keeps_its_netting_when_its_route_would_lose() {
    let v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, RATE, PTIP), 1_000),
        l_pair(2, T8, T8, pask(2, T8, T8, 2 * WHOLE, RATE, PTIP), 1_000),
        l_pair(3, T8, T8, pask(3, T8, T8, 2 * WHOLE, RATE, PTIP), 1_000),
        l_pair(4, T8, T8, pbid(4, T8, T8, 10 * WHOLE, 1_400, PTIP), 1_000),
        l_bid_b(100, T8, T8, 5, P200, 40 * WHOLE),
        l_ask_a(200, T8, 6, 300_000_000, 4 * WHOLE),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    assert_eq!(amount_in(&r, cid(4)), 6 * WHOLE, "netted only");
    assert_eq!(amount_in(&r, cid(200)), 0, "no losing route");
}

/// Random netting groups (1 to 4 pair orders per side of one pair, both orientations, random prices around one B per A,
/// amounts, minimum fills, tips, IOC / FOK, some KAS bids of either token for the surplus): every transaction is engine-valid,
/// profitable, leaves the operator no token, and fills every pair order within its quantity rules (`run_pair`). Environment:
/// `KOB_PAIR_NET_ITERS` (default 40).
#[test]
fn random_netting_groups_are_exact() {
    let iters: u64 = std::env::var("KOB_PAIR_NET_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |m: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % m.max(1)
    };
    let mut built = 0;
    for it in 0..iters {
        let mut v: Vec<ListedOrder> = vec![];
        let mut id = 1u32;
        let (na, nb) = (1 + next(4), 1 + next(4));
        for k in 0..(na + nb) {
            let ask = k < na;
            let amount = (1 + next(5) as i64) * WHOLE + next(3) as i64 * 137;
            let price = 900 + next(250) as i64;
            let tif = match next(6) {
                0 => TIF_IOC,
                1 => TIF_FOK,
                _ => TIF_GTC,
            };
            let mut s = pair_state(id as u8, ask, T8, T8, amount, price, PTIP * (1 + next(3) as i64), tif);
            s.min_fill = (1 + next(amount as u64 / 2) as i64).min(amount);
            if next(4) == 0 {
                // the other orientation: base B, quote A (the same economic side flips)
                std::mem::swap(&mut s.s_cov_id, &mut s.t_cov_id);
            }
            let mut o = l_pair(id, T8, T8, s, 1_000);
            if tif != TIF_GTC {
                o = fresh(o);
            }
            v.push(o);
            id += 1;
        }
        if next(2) == 0 {
            v.push(l_bid_b(900, T8, T8, 30, P200, 20 * WHOLE));
        }
        if next(2) == 0 {
            v.push(l_bid_a(901, T8, 31, P200, 20 * WHOLE));
        }
        let r = run_pair(v, &cfg());
        built += r.prepared.len();
        let _ = it;
    }
    assert!(built > 0, "some groups net");
}

/// Random books of one pair with both KAS books: pair asks and bids, pair conditionals (take-profit, stop, trailing, armed
/// or not), pair entries of both sides (limit, stop, repeating), KAS asks and bids of A and B at random prices: every
/// transaction is engine-valid, profitable and leaves the operator no token (`run_pair`). Environment:
/// `KOB_PAIR_MIX_ITERS` (default 30).
#[test]
fn random_pair_books_with_both_kas_books() {
    let iters: u64 = std::env::var("KOB_PAIR_MIX_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |m: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % m.max(1)
    };
    let mut built = 0;
    let mut pair_fills = 0;
    let mut updates = 0;
    for _ in 0..iters {
        let mut v: Vec<ListedOrder> = vec![];
        let mut id = 1u32;
        let push = |o: ListedOrder, v: &mut Vec<ListedOrder>| {
            v.push(o);
        };
        for _ in 0..(1 + next(4)) {
            let ask = next(2) == 0;
            let s = pair_state(
                id as u8,
                ask,
                T8,
                T8,
                (1 + next(6) as i64) * WHOLE,
                900 + next(400) as i64,
                PTIP * next(3) as i64,
                TIF_GTC,
            );
            push(l_pair(id, T8, T8, s, 1_000), &mut v);
            id += 1;
        }
        for _ in 0..next(3) {
            let ask = next(2) == 0;
            let (tp, stop) = if ask {
                (1_100 + next(300) as i64, 900 + next(200) as i64)
            } else {
                (800 + next(200) as i64, 1_000 + next(300) as i64)
            };
            let mut c = cond_pair(id as u8, ask, T8, T8, (1 + next(4) as i64) * WHOLE, if next(3) == 0 { 0 } else { tp }, stop);
            if next(3) == 0 {
                c.trail_step = 20;
                c.trail_gap = 20;
            }
            if next(4) == 0 {
                c.armed = 1;
            }
            push(l_cond_pair(id, T8, T8, c, 1_000), &mut v);
            id += 1;
        }
        for _ in 0..next(2) {
            let buy = next(2) == 0;
            let mut e = ifd_pair(id as u8, buy, T8, T8, (1 + next(4) as i64) * WHOLE, 950 + next(150) as i64);
            if next(3) == 0 {
                e.entry_stop = if buy { e.price - 50 } else { e.price + 50 };
            }
            if next(3) == 0 {
                e.rpt_amount = 1 + 10 * WHOLE;
            }
            e.tip = PTIP;
            e.custody = e.b_custody_needed().unwrap();
            push(l_ifd_pair(id, T8, T8, e, 1_000), &mut v);
            id += 1;
        }
        for k in 0..(1 + next(3)) {
            let pa = 180_000_000 + next(80) as i64 * 1_000_000;
            v.push(l_ask_a(500 + k as u32, T8, 40, pa, (1 + next(5) as i64) * WHOLE));
            v.push(l_bid_a(520 + k as u32, T8, 41, pa - 10_000_000 + next(30) as i64 * 1_000_000, (1 + next(5) as i64) * WHOLE));
            let pb = 180_000_000 + next(80) as i64 * 1_000_000;
            v.push(l_ask_b(540 + k as u32, T8, T8, 42, pb, (1 + next(5) as i64) * WHOLE));
            v.push(l_bid_b(560 + k as u32, T8, T8, 43, pb - 10_000_000 + next(30) as i64 * 1_000_000, (1 + next(5) as i64) * WHOLE));
        }
        let r = run_pair(v, &cfg());
        built += r.prepared.len();
        for p in &r.prepared {
            pair_fills += p.plan.fills.iter().filter(|f| f.cand.pair.is_some()).count();
            updates += p.plan.updates.len();
        }
    }
    eprintln!("random pair books: {built} transactions, {pair_fills} pair fills, {updates} updates");
    assert!(built > 0 && pair_fills > 0);
}

/// The UTXO of output `k` of a prepared transaction, accepted at `daa`.
fn out_utxo(p: &kob_executor::matcher::engine::Prepared, k: usize, daa: u64) -> kob_protocol::tx::Utxo {
    let tx = &p.signed.tx;
    kob_protocol::tx::Utxo {
        transaction_id: tx.id,
        index: k as u32,
        amount: tx.outputs[k].value,
        block_daa_score: daa,
        covenant_id: tx.outputs[k].covenant.as_ref().map(|c| c.covenant_id),
    }
}

/// The output of a prepared transaction carrying `spk` under covenant `cov`.
fn find_out(p: &kob_executor::matcher::engine::Prepared, spk: &str, cov: [u8; 32]) -> usize {
    p.signed
        .tx
        .outputs
        .iter()
        .position(|o| o.script_public_key == spk && o.covenant.as_ref().map(|c| c.covenant_id) == Some(cov))
        .unwrap_or_else(|| panic!("no output {spk} of {}", kob_protocol::json::to_hex(&cov)))
}

/// An if-done pair cycle over two ticks: a repeating buy-first entry nets 2 A against a pair ASK (tick 1: its exit is
/// created, booked, holding the 2 A); the exit's take-profit then nets against a pair BID and re-arms the entry (tick 2:
/// the entry's B escrow grows by the exit's budget, its amount back to 4 A).
#[test]
fn an_if_done_pair_cycle_buys_books_its_exit_and_re_arms() {
    use kob_protocol::artifacts::{template, token_template};
    use kob_protocol::tx::{spk_to_string, OrderUtxo, TokenUtxo};
    let mut e = ifd_pair(1, true, T8, T8, 4 * WHOLE, 1_400);
    e.rpt_amount = 1 + 20 * WHOLE;
    e.tip = PTIP;
    e.custody = e.b_custody_needed().unwrap();
    let entry = l_ifd_pair(1, T8, T8, e.clone(), 1_000);
    // tick 1: the entry buys 2 A from a pair ASK at 1.40
    let r = run_pair(vec![entry.clone(), l_pair(2, T8, T8, pask(2, T8, T8, 2 * WHOLE, 1_400, PTIP), 1_000)], &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    let n = 2 * WHOLE;
    assert_eq!(amount_in(&r, cid(1)), n);
    // the exit: genesis of a fresh KobCondPair, booked by the entry
    let nc = p.lowered.built.covenants.iter().find(|c| c.template == Some(TemplateId::KobCondPair)).expect("the exit");
    let booking = Booking { parent: cid(1), until: rpt_until(e.expiry_daa, 1_000).unwrap() };
    let x = e.exit_for(n, n, Some(booking)).unwrap();
    let xo = find_out(p, &spk_to_string(&AnyState::KobCondPair(x.clone()).spk()), nc.covenant_id);
    let tpl = token_template(TemplateId::Kcc20Ref8x8);
    let xc_state = tstate(T8, n, nc.covenant_id, true);
    let xc = find_out(p, &spk_to_string(&xc_state.spk_with(tpl)), TOKEN);
    // the entry's continuation and its B escrow rest
    let spend = e.spend(n, e.price).unwrap();
    let e2 = IfdPairState {
        amount_left: 2 * WHOLE,
        custody: e.custody - spend,
        rpt_amount: e.rpt_amount - n,
        armed: e.next_armed(1_000),
        ..e.clone()
    };
    let eo = find_out(p, &spk_to_string(&AnyState::KobIfdPair(e2.clone()).spk()), cid(1));
    let ec_state = tstate(T8, e2.custody, cid(1), true);
    let ec = find_out(p, &spk_to_string(&ec_state.spk_with(tpl)), TOKEN_B);
    let _ = template(TemplateId::KobCondPair);
    let daa = 2_000;
    let exit_l = ListedOrder {
        family: kob_executor::matcher::family::Family::Kcc20,
        order: OrderUtxo { utxo: out_utxo(p, xo, daa), state: AnyState::KobCondPair(x) },
        custody: Some(TokenUtxo { utxo: out_utxo(p, xc, daa), state: xc_state }),
        custody_b: None,
        deadline: None,
        seen_daa: daa,
        foreign: vec![],
        strays: vec![],
    };
    let entry_l = ListedOrder {
        family: kob_executor::matcher::family::Family::Kcc20,
        order: OrderUtxo { utxo: out_utxo(p, eo, daa), state: AnyState::KobIfdPair(e2) },
        custody: Some(TokenUtxo { utxo: out_utxo(p, ec, daa), state: ec_state }),
        custody_b: None,
        deadline: None,
        seen_daa: daa,
        foreign: vec![],
        strays: vec![],
    };
    // tick 2: the exit's take-profit (1.60) nets against a pair BID at 1.70 and re-arms the entry
    let r2 = run_pair(vec![entry_l, exit_l, l_pair(3, T8, T8, pbid(3, T8, T8, 2 * WHOLE, 1_700, PTIP), 1_000)], &cfg());
    assert_eq!(r2.prepared.len(), 1);
    let f = r2.prepared[0].plan.fills.iter().find(|f| f.cand.id == nc.covenant_id).expect("the exit takes profit");
    assert_eq!(f.cand.merge, Some(cid(1)), "re-arming its entry");
    assert_eq!(f.amount, n);
}
