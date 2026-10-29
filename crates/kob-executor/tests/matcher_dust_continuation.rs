//! Audit 2026-10-30 E1 regression: a stop whose `keeperTip` leaves a continuation below the covenant-output bound must not
//! freeze the book it sits in. The planner never attaches such an arming update, the builder names an update it refuses, and the
//! rest of the book keeps matching.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::engine::{lowering_update, tick, TickReport};
use kob_executor::matcher::family::Families;
use kob_protocol::state::*;

fn run_quiet(orders: Vec<ListedOrder>) -> TickReport {
    let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![], keeper_only: vec![] };
    tick(&input(&b), &cfg(), &Families::default(), &signer())
}

fn honest_cross() -> Vec<ListedOrder> {
    vec![l_bid(3, bid(3, P260, T3), 5 * WHOLE), l_ask(2, ask(1, P250, 5 * WHOLE, T3))]
}

fn updates_of(r: &TickReport) -> usize {
    r.prepared.iter().map(|p| p.plan.updates.len()).sum()
}

/// A sell stop above the market (any resting ask fill at or below it is its trigger evidence), whose `keeperTip` leaves `left`
/// sompi of its UTXO value: the update's continuation.
fn dusty_stop(id: u32, value: u64, left: u64) -> ListedOrder {
    let mut s = cond_ask(9, 0, 300_000_000, WHOLE, T3);
    s.min_touch = 1;
    s.min_rest_daa = 0;
    s.keeper_tip = (value - left) as i64;
    listed(cid(id), AnyState::KobCondAsk(s), value, 1_000)
}

#[test]
fn a_stop_whose_tip_leaves_a_dust_continuation_does_not_block_the_book() {
    let r = run_quiet(honest_cross());
    assert_eq!(r.prepared.len(), 1, "control: the honest crossing is matched");

    // control: an honest stop is armed by the same batch
    let mut honest = honest_cross();
    let mut s = cond_ask(9, 0, 300_000_000, WHOLE, T3);
    s.min_touch = 1;
    s.min_rest_daa = 0;
    honest.push(listed(cid(9), AnyState::KobCondAsk(s), CARRIER, 1_000));
    let r = run_quiet(honest);
    assert_eq!((r.prepared.len(), updates_of(&r)), (1, 1), "control: the batch arms an honest stop");

    for left in [1u64, 1_000_000, 4_000_000, 7_999_999] {
        let mut hostile = honest_cross();
        hostile.push(dusty_stop(9, CARRIER, left));
        let r = run_quiet(hostile.clone());
        assert_eq!(r.prepared.len(), 1, "left {left}: the honest crossing is matched; anomalies {:?}", r.anomalies);
        assert_eq!(updates_of(&r), 0, "left {left}: the dust update is not attached");
        assert!(r.anomalies.is_empty(), "left {left}: nothing was refused by the builder: {:?}", r.anomalies);
        // every tick, not only the first
        assert_eq!(run_quiet(hostile).prepared.len(), 1);
    }
    // the bound itself (0.08 KAS) is a continuation the builder accepts
    let mut edge = honest_cross();
    edge.push(dusty_stop(9, CARRIER, kob_protocol::tx::COVENANT_OUTPUT_MIN));
    let r = run_quiet(edge);
    assert_eq!((r.prepared.len(), updates_of(&r)), (1, 1), "a continuation exactly at the bound is armed");
}

#[test]
fn a_dust_update_of_a_stop_entry_does_not_block_the_book_either() {
    let mk = |tip_leaves: Option<i64>| {
        let mut e = ifd_bid(9, P250, WHOLE, T3);
        e.entry_stop = P245;
        e.min_touch = 1;
        e.min_rest_daa = 0;
        let value = e.escrow().expect("escrow") as u64;
        if let Some(left) = tip_leaves {
            e.keeper_tip = value as i64 - left;
        }
        let mut v = honest_cross();
        v.push(listed(cid(9), AnyState::KobIfdBid(e), value, 1_000));
        v
    };
    let r = run_quiet(mk(None));
    assert_eq!(r.prepared.len(), 1);
    let r = run_quiet(mk(Some(1)));
    assert_eq!(r.prepared.len(), 1, "anomalies {:?}", r.anomalies);
    assert_eq!(updates_of(&r), 0);
}

#[test]
fn the_builder_names_the_update_it_refuses() {
    assert_eq!(lowering_update("lowering: invalid request: arming update 2: the continuation of 5 sompi is below"), Some(2));
    assert_eq!(lowering_update("lowering: invalid request: fill of leg 2: dust payout"), None);
    assert_eq!(lowering_update("lowering: invalid request: arming update x: nope"), None);
}
