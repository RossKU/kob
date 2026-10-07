//! The wallet's default trigger threshold of a stop scales with the stop's size (`defaults::default_min_touch`: the larger of
//! its minimum fill and 25 % of its amount): a one-whole print at the stop no longer arms a 1,000-whole stop, while a stop
//! whose user chose the minimum fill as its threshold still arms on it.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_protocol::defaults::default_min_touch;

/// A sell stop of 1,000 whole at 2.50 (minimum fill one whole) with trigger threshold `touch`, next to a one-whole ask at
/// the stop that rested, a bid crossing it, and a bid far below the stop: whether the batch arms the stop.
fn armed_by_one_whole_print(touch: i64) -> bool {
    let mut v = cond_ask(1, 0, P250, 1_000 * WHOLE, T3);
    v.min_touch = touch;
    v.min_rest_daa = 50;
    let stop = l_cond_ask(1, v, NOW - 10_000);
    let orders: Vec<ListedOrder> = vec![
        stop,
        l_ask(2, ask(2, P250, WHOLE, T3)),
        l_bid(3, bid(3, P260, T3), WHOLE),
        l_bid(4, bid(4, 200_000_000, T3), 1_000 * WHOLE),
    ];
    let b = book(orders);
    let inp = input(&b);
    let r = run(&inp, &cfg());
    assert!(!r.prepared.is_empty(), "the crossed one-whole orders are matched: {:?}", r.skipped);
    r.prepared.iter().any(|p| {
        p.plan.updates.iter().any(|u| u.id == cid(1)) || p.plan.fills.iter().any(|f| f.cand.id == cid(1) && f.evidence.is_some())
    })
}

#[test]
fn the_default_threshold_of_a_large_stop_is_a_quarter_of_its_amount() {
    assert_eq!(default_min_touch(WHOLE, 1_000 * WHOLE), 250 * WHOLE);
    assert_eq!(default_min_touch(WHOLE, 2 * WHOLE), WHOLE, "a small stop keeps its minimum fill");
    assert!(!armed_by_one_whole_print(default_min_touch(WHOLE, 1_000 * WHOLE)), "a one-whole print does not arm it");
}

#[test]
fn a_threshold_chosen_by_the_user_still_applies() {
    assert!(armed_by_one_whole_print(WHOLE), "a stop whose threshold is one whole arms on a one-whole print");
}
