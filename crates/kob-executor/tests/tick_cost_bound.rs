//! Regression: the CPU an attacker can force on the operator's tick is bounded.
//!
//! `plan_book` was about O(n^3) per allocation and repeated for every fill of the plan; 1 000 bids and 1 000 asks all crossing
//! at a loss cost 48 s per tick. Now the planner keeps the best `max_candidates_per_group` candidates per (class, side), keeps its
//! token-slot accounting incrementally and stops at a wall-clock budget.
//!
//! The bounds are stated in work, not time ([`TickReport::work`], [`kob_executor::matcher::batch::PlanWork`]): the allocation
//! passes and candidate pairs the planner ran, and that no plan ran into its book budget. Those counts are the same on every
//! machine, so a regression fails on a fast one and a slow CI runner does not fail a fixed planner (the wall-clock bounds
//! these tests had failed on windows-latest only). The times are printed for information.
//! An expired order whose `refundTip` cannot cover the refund fee was built, signed and engine-validated on every tick.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::keepers::{tick as keeper_tick, KeeperConfig, KeeperInput};
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::engine::{tick, EngineConfig, TickReport};
use kob_executor::matcher::family::Families;
use std::time::Instant;

/// Book and tick budgets for the work-bounded tests: generous, so that no plan of a fixed planner reaches them on any machine
/// (the counts are then deterministic), and finite, so that a regression fails in bounded time (its plan is cut short).
fn budgeted(cfg: EngineConfig) -> EngineConfig {
    EngineConfig { book_budget_ms: 30_000, tick_budget_ms: 120_000, ..cfg }
}

/// The tick's planning stayed within `max` ([allocation passes, candidate pairs tried, netting steps]) and no plan was cut short
/// by its budget (the planner finished every batch by itself). The bounds are about 4x the counts measured on the fixed
/// planner (the constants at the end of the file).
fn assert_work(rep: &TickReport, tag: &str, max: [u64; 3]) {
    let w = rep.work;
    println!("{tag}: {w:?}");
    assert_eq!(w.cut_short, 0, "{tag}: a plan ran into its book budget: {w:?}");
    assert!(w.allocations <= max[0], "{tag}: {} allocation passes, bound {}", w.allocations, max[0]);
    assert!(w.pair_tries <= max[1], "{tag}: {} candidate pairs tried, bound {}", w.pair_tries, max[1]);
    assert!(w.net_steps <= max[2], "{tag}: {} netting steps, bound {}", w.net_steps, max[2]);
}

fn dust_refund_orders(n: u32) -> Vec<ListedOrder> {
    (0..n)
        .map(|i| {
            let mut a = ask(1 + (i % 4) as u8, P250 + i as i64, WHOLE, T3);
            a.expiry_daa = 2_000; // long past
            a.refund_tip = 0; // the refund pays nothing: a keeper loses the fee
            l_ask(10_000 + i, a)
        })
        .collect()
}

#[test]
fn a_thousand_crossing_pairs_at_a_loss_are_planned_within_the_budget() {
    let n = 1_000u32;
    let mut orders = vec![];
    for i in 0..n {
        // bids just above, asks just below the same level: everything crosses, every fill loses money
        let mut b = bid(1, P250 + 1_000 + (i as i64 % 50) * 10, T8);
        b.tip = 0;
        let mut a = ask(2, P250 - 1_000 - (i as i64 % 50) * 10, WHOLE, T8);
        a.tip = 0;
        orders.push(l_bid(20_000 + i, b, WHOLE));
        orders.push(l_ask(30_000 + i, a));
    }
    let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
    let inp = input(&b);
    let t0 = Instant::now();
    let rep = tick(&inp, &budgeted(cfg()), &Families::default(), &signer());
    let took = t0.elapsed();
    println!("matcher tick, {n} bids + {n} asks all crossing at a loss: {took:?}, {} tx", rep.prepared.len());
    // was 48.6 s before the fix
    assert_work(&rep, "1000 x 1000 at a loss", WORK_LOSS);
    for p in &rep.prepared {
        assert!(p.accounting.profit >= 0, "never a losing batch");
    }
}

#[test]
fn a_big_profitable_book_keeps_every_hard_rule() {
    // 300 orders per side, mixed IOC / FOK, the profitable head of the book is still matched and every plan is engine-valid
    let mut orders = vec![];
    for i in 0..300u32 {
        let mut b = bid(1 + (i % 5) as u8, P260 - (i as i64 % 30) * 10_000, T8);
        b.tip = 100_000;
        orders.push(l_bid(20_000 + i, b, 3 * WHOLE));
        let mut a = ask(1 + (i % 5) as u8, P250 + (i as i64 % 30) * 10_000, 3 * WHOLE, T8);
        a.tip = 100_000;
        orders.push(l_ask(30_000 + i, a));
    }
    let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
    let inp = input(&b);
    let rep = run(&inp, &cfg());
    assert!(!rep.prepared.is_empty(), "the profitable head of the book is matched");
}

#[test]
fn unprofitable_refunds_are_not_built_and_are_remembered() {
    let n = 40u32;
    let orders = dust_refund_orders(n);
    let mut kin = KeeperInput {
        orders,
        clock: kob_executor::matcher::book::Clock { daa: NOW + 5, utc: UTC },
        funding: vec![funding(500 * KAS)],
        excluded: Default::default(),
        excluded_outpoints: Default::default(),
        known_unprofitable: Default::default(),
    };
    let t0 = Instant::now();
    let rep = keeper_tick(&kin, &KeeperConfig::default(), &signer());
    let cold = t0.elapsed();
    assert!(rep.jobs.is_empty());
    assert_eq!(rep.skipped.len() as u32, n);
    // a zero tip is below the fee floor: rejected before any transaction is built
    assert!(rep.skipped.iter().all(|(_, why)| why.contains("does not cover the fee")), "{:?}", rep.skipped);
    println!("keeper tick over {n} dust-refund orders: {cold:?}");
    // the work bound: nothing was built, signed or validated (the regression built every one of them on every tick)
    assert!(rep.unprofitable.is_empty() && rep.failed.is_empty(), "built: {:?} {:?}", rep.unprofitable, rep.failed);

    // a tip above the floor but below the real fee is built once, then remembered per (order, outpoint)
    let mut close: Vec<ListedOrder> = vec![];
    for i in 0..5u32 {
        let mut a = ask(1, P250 + i as i64, WHOLE, T3);
        a.expiry_daa = 2_000;
        a.refund_tip = 900_000; // above the cheap floor, below the real fee of a refund (a refund with custody weighs several kB)
        close.push(l_ask(50_000 + i, a));
    }
    kin.orders = close;
    let first = keeper_tick(&kin, &KeeperConfig::default(), &signer());
    assert!(first.jobs.is_empty());
    assert_eq!(first.unprofitable.len(), 5, "{:?}", first.skipped);
    kin.known_unprofitable = first.unprofitable.iter().copied().collect();
    let second = keeper_tick(&kin, &KeeperConfig::default(), &signer());
    assert!(second.jobs.is_empty());
    assert!(second.unprofitable.is_empty(), "not built again: {:?}", second.skipped);
}

#[test]
fn the_attempt_budget_bounds_the_builds_of_one_tick() {
    let orders: Vec<ListedOrder> = (0..40u32)
        .map(|i| {
            let mut a = ask(1, P250 + i as i64, WHOLE, T3);
            a.expiry_daa = 2_000;
            a.refund_tip = 900_000;
            l_ask(60_000 + i, a)
        })
        .collect();
    let kin = KeeperInput {
        orders,
        clock: kob_executor::matcher::book::Clock { daa: NOW + 5, utc: UTC },
        funding: vec![funding(500 * KAS)],
        excluded: Default::default(),
        excluded_outpoints: Default::default(),
        known_unprofitable: Default::default(),
    };
    let cfg = KeeperConfig { max_attempts: 3, ..KeeperConfig::default() };
    let rep = keeper_tick(&kin, &cfg, &signer());
    assert!(rep.unprofitable.len() <= 3, "at most 3 builds: {}", rep.unprofitable.len());
}

// ---------------------------------------------------------------------------------------------
// pair orders: the netting walk, the route legs and the keeper's dust refunds are bounded as well

/// `n` pair asks below and `n` pair bids above the same rate (every ask nets with every bid), with or without the tip that pays
/// a netting fee, and `kas` plain orders of each KAS book they may route through (the plain books cross each other as well).
fn crossing_pair_book(n: u32, kas: u32, tipped: bool) -> Vec<ListedOrder> {
    use common::pair::*;
    let tip = if tipped { PTIP } else { 0 };
    let mut orders = vec![];
    for i in 0..n {
        orders.push(l_pair(40_000 + i, T8, T8, pask(1 + (i % 4) as u8, T8, T8, 3 * WHOLE, RATE - 10 - (i as i64 % 50), tip), 1_000));
        orders.push(l_pair(50_000 + i, T8, T8, pbid(5 + (i % 4) as u8, T8, T8, 3 * WHOLE, RATE + 10 + (i as i64 % 50), tip), 1_000));
    }
    for i in 0..kas {
        orders.push(l_bid_a(60_000 + i, T8, 9, P260, 3 * WHOLE));
        orders.push(l_ask_b(61_000 + i, T8, T8, 9, P200, 3 * WHOLE));
        orders.push(l_bid_b(62_000 + i, T8, T8, 9, P200, 3 * WHOLE));
        orders.push(l_ask_a(63_000 + i, T8, 9, P250, 3 * WHOLE));
    }
    orders
}

#[test]
fn pair_orders_crossing_each_other_are_planned_within_the_budget() {
    use common::pair::*;
    let n = 100u32;
    let b = MemoryBook { daa_score: NOW + 5, orders: crossing_pair_book(n, 50, true), wallet_tokens: vec![] };
    let inp = input(&b);
    let cfg = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
    let t0 = Instant::now();
    let rep = tick(&inp, &budgeted(cfg), &Families::default(), &signer());
    let took = t0.elapsed();
    println!(
        "matcher tick, {n} pair asks + {n} pair bids crossing each other (+ 200 KAS orders): {took:?}, {} tx",
        rep.prepared.len()
    );
    assert_work(&rep, "100 + 100 tipped pair orders", WORK_PAIR);
    assert!(!rep.prepared.is_empty(), "the tipped pair orders net");
    assert!(rep.anomalies.is_empty(), "{:?}", rep.anomalies);
    check_pair(&rep, &inp);
}

/// Regression (found by the test port): 300 untipped pair asks and 300 untipped pair bids that cross each other made the global
/// planner spend its whole 5 s book budget on every batch and build nothing, although the 200 plain KAS orders of the same
/// view cross each other with a profit. Fixed: the route takes no more pair orders per direction than the KAS books can fill,
/// a netting group is bounded by the token slots and pays for itself or nets nothing, and the profit step judges a netting
/// group as one unit. A large pair book must not starve the plain crossings: the plain crossings are built, no plan runs into
/// its book budget, and the planner's work stays within a bound stated in allocation passes, candidate pairs and netting
/// steps. Checked against the fixes undone one by one (2026-10-06): the profit step judging netted orders one by one again
/// gives 1,298 allocation passes (bound 1,000), the netting group unbounded by the token slots 10.1 M netting steps (bound
/// 2.6 M; 57 s against 6 s, all other counts unchanged), and with all three fixes undone every plan runs into its 30 s book
/// budget (`cut_short` 4 of 5): each fails this test, by count and not by time.
#[test]
fn a_big_untipped_pair_book_does_not_starve_the_plain_crossings() {
    use common::pair::*;
    let b = MemoryBook { daa_score: NOW + 5, orders: crossing_pair_book(300, 50, false), wallet_tokens: vec![] };
    let inp = input(&b);
    let cfg = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
    let t0 = Instant::now();
    let rep = tick(&inp, &budgeted(cfg), &Families::default(), &signer());
    let took = t0.elapsed();
    println!("untipped 300 + 300 pair orders: {took:?}, {} tx", rep.prepared.len());
    assert!(!rep.prepared.is_empty(), "the plain crossings are built");
    assert!(rep.anomalies.is_empty(), "{:?}", rep.anomalies);
    assert_work(&rep, "300 + 300 untipped pair orders", WORK_STARVE);
}

#[test]
fn unprofitable_pair_refunds_are_not_built() {
    use common::pair::*;
    let n = 40u32;
    let orders: Vec<ListedOrder> = (0..n)
        .map(|i| {
            let mut x = pask(1 + (i % 4) as u8, T3, T8, WHOLE, RATE + i as i64, PTIP);
            x.expiry_daa = 2_000; // long past
            x.refund_tip = 0; // the refund pays nothing: a keeper loses the fee
            l_pair(70_000 + i, T3, T8, x, 1_000)
        })
        .collect();
    let kin = KeeperInput {
        orders,
        clock: kob_executor::matcher::book::Clock { daa: NOW + 5, utc: UTC },
        funding: vec![funding(500 * KAS)],
        excluded: Default::default(),
        excluded_outpoints: Default::default(),
        known_unprofitable: Default::default(),
    };
    let t0 = Instant::now();
    let rep = keeper_tick(&kin, &KeeperConfig::default(), &signer());
    let cold = t0.elapsed();
    assert!(rep.jobs.is_empty());
    assert_eq!(rep.skipped.len() as u32, n);
    assert!(rep.skipped.iter().all(|(_, why)| why.contains("does not cover the fee")), "{:?}", rep.skipped);
    println!("keeper tick over {n} dust pair refunds: {cold:?}");
    assert!(rep.unprofitable.is_empty() && rep.failed.is_empty(), "built: {:?} {:?}", rep.unprofitable, rep.failed);
}

// Work bounds [allocation passes, candidate pairs tried, netting steps], about 4x the counts measured on the fixed planner
// (`TickReport::work`, deterministic: the same on every machine while no plan is cut short).
const WORK_LOSS: [u64; 3] = [240, 2_000, 100]; // measured 57, 456, 0
const WORK_PAIR: [u64; 3] = [1_000, 2_400_000, 450_000]; // measured 260, 583,668, 110,923
const WORK_STARVE: [u64; 3] = [1_000, 3_000_000, 2_600_000]; // measured 258, 774,476, 637,806
