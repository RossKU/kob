//! A pair order (`KobPair` / `KobCondPair`) whose order UTXO funds no more delivery carriers than the fill of everything left
//! can only be filled in full: a partial fill must leave a continuation that funds its carrier and tip. The planner matches
//! such an order only in full (`PairInfo::part_cap`), never plans a partial fill the builder refuses, and the other books
//! of the tick keep their fills.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_executor::matcher::engine::{lowering_fill_leg, lowering_leg, tick, TickReport};
use kob_executor::matcher::family::Families;
use kob_protocol::state::*;

const X: u32 = 1;
const TOKEN_C: [u8; 32] = [0x5c; 32];

/// A pair ASK of 10 whole A at one whole B per whole A, minimum fill one whole A, with `carriers` delivery carriers on its
/// order UTXO (no tip). Each partial fill spends one carrier.
fn pair_ask(carriers: u64) -> ListedOrder {
    let s = pair_state(1, true, T8, T8, 10 * WHOLE, RATE, 0, TIF_GTC);
    let mut o = l_pair(X, T8, T8, s.clone(), 1_000);
    o.order.utxo.amount = carriers * PDC as u64 + s.tip_kas(s.amount_left).unwrap() as u64;
    o
}

/// The route of a fill of `n` whole A: a KAS bid of A at 2.60 and a KAS ask of B at 2.00.
fn route(n: i64) -> Vec<ListedOrder> {
    vec![l_bid_a(100, T8, 2, P260, n * WHOLE), l_ask_b(200, T8, T8, 3, P200, n * WHOLE)]
}

/// An unrelated crossed book of token C: a bid at 2.60 and an ask at 2.50, 5 whole each.
fn other_book() -> Vec<ListedOrder> {
    vec![l_bid_of(300, TOKEN_C, T8, 6, P260, 5 * WHOLE), l_ask_of(301, TOKEN_C, T8, 7, P250, 5 * WHOLE)]
}

fn raw(orders: Vec<ListedOrder>) -> TickReport {
    let b = book(orders);
    let inp = input(&b);
    let cfg = kob_executor::matcher::engine::EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
    tick(&inp, &cfg, &Families::default(), &signer())
}

fn filled(r: &TickReport, id: u32) -> i64 {
    amount_in(r, cid(id))
}

fn no_continuation_refusal(r: &TickReport) {
    assert!(!r.anomalies.iter().any(|a| a.2.contains("continuation must be positive")), "{:?}", r.anomalies);
}

#[test]
fn a_pair_order_with_carriers_left_is_filled_partially() {
    for c in [3, 2] {
        let mut v = vec![pair_ask(c)];
        v.extend(route(4));
        let r = raw(v);
        assert!(r.anomalies.is_empty(), "{c}: {:?}", r.anomalies);
        assert_eq!(filled(&r, X), 4 * WHOLE, "{c} carriers: partial fill");
    }
}

#[test]
fn a_pair_order_with_one_carrier_left_is_matched_only_in_full() {
    let mut v = vec![pair_ask(1)];
    v.extend(route(10));
    let r = raw(v);
    no_continuation_refusal(&r);
    assert_eq!(filled(&r, X), 10 * WHOLE, "the counterparty for the whole rest fills it");
    let mut v = vec![pair_ask(1)];
    v.extend(route(4));
    let r = raw(v);
    no_continuation_refusal(&r);
    assert_eq!(filled(&r, X), 0, "no partial fill is planned");
}

#[test]
fn a_pair_order_matched_only_in_full_leaves_the_other_books_their_fills() {
    let mut v = vec![pair_ask(1)];
    v.extend(route(4));
    v.extend(other_book());
    let r = raw(v);
    no_continuation_refusal(&r);
    assert_eq!(filled(&r, X), 0);
    assert_eq!((filled(&r, 300), filled(&r, 301)), (5 * WHOLE, 5 * WHOLE), "the unrelated book is matched");
}

#[test]
fn ten_unrelated_books_are_matched_next_to_a_pair_order_matched_only_in_full() {
    let books = |v: &mut Vec<ListedOrder>| {
        for k in 0..10u32 {
            let t = [0x60 + k as u8; 32];
            v.push(l_bid_of(400 + 2 * k, t, T8, 6, P260, 5 * WHOLE));
            v.push(l_ask_of(401 + 2 * k, t, T8, 7, P250, 5 * WHOLE));
        }
    };
    let count = |r: &TickReport| (0..20u32).filter(|i| filled(r, 400 + i) > 0).count();
    let mut v = vec![];
    books(&mut v);
    assert_eq!(count(&raw(v)), 20, "control");
    let mut v = vec![pair_ask(1)];
    v.extend(route(4));
    books(&mut v);
    let r = raw(v);
    no_continuation_refusal(&r);
    assert_eq!(count(&r), 20, "every honest order of the ten books is filled");
}

#[test]
fn a_conditional_pair_order_with_one_carrier_left_is_matched_only_in_full() {
    let s = cond_pair(1, true, T8, T8, 10 * WHOLE, RATE, 0);
    let mut o = l_cond_pair(X, T8, T8, s.clone(), 1_000);
    o.order.utxo.amount = PDC as u64 + s.tip_kas(s.amount_left).unwrap() as u64;
    let mut v = vec![o.clone()];
    v.extend(route(4));
    v.extend(other_book());
    let r = raw(v);
    no_continuation_refusal(&r);
    assert_eq!(filled(&r, X), 0);
    assert_eq!((filled(&r, 300), filled(&r, 301)), (5 * WHOLE, 5 * WHOLE), "the unrelated book is matched");
    let mut v = vec![o];
    v.extend(route(10));
    let r = raw(v);
    no_continuation_refusal(&r);
    assert_eq!(filled(&r, X), 10 * WHOLE, "the whole rest fills");
}

#[test]
fn a_refused_fill_names_its_leg_apart_from_a_refused_order() {
    let e = "lowering: invalid request: fill of leg 4: pair continuation must be positive (0)";
    assert_eq!(lowering_fill_leg(e), Some(4));
    assert_eq!(lowering_leg(e), None);
    assert_eq!(lowering_fill_leg("lowering: invalid request: order of leg 2: token prefix/suffix lengths"), None);
}
