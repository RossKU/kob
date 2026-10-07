//! A refusal that names no order (the pre-simulation fails the transaction, not an input of it) costs the other orders of
//! the tick at most the attempts that search for the order it comes from: the engine leaves out half of the plan's fills at
//! a time, a smaller plan that passes is the batch, and the fills left out are planned in the next batches. The tick no
//! longer gives up the lowest-priority fills one by one until its attempts run out.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::{outpoint, ListedOrder};
use kob_executor::matcher::engine::{tick_with, TickReport};
use kob_executor::matcher::family::Families;
use kob_protocol::tx::SignedTx;
use kob_protocol::verify::{validate_signed, Validation};

/// Ten crossed books (tokens 0x60..0x69), a bid and an ask of 5 whole each.
fn ten_books(v: &mut Vec<ListedOrder>) {
    for k in 0..10u32 {
        let t = [0x60 + k as u8; 32];
        v.push(l_bid_of(400 + 2 * k, t, T8, 6, P260, 5 * WHOLE));
        v.push(l_ask_of(401 + 2 * k, t, T8, 7, P250, 5 * WHOLE));
    }
}

fn filled(r: &TickReport, id: u32) -> i64 {
    amount_in(r, cid(id))
}

fn ten_filled(r: &TickReport) -> usize {
    (0..20u32).filter(|i| filled(r, 400 + i) > 0).count()
}

/// A validator that refuses every transaction spending `op`, with a message that names no input.
fn refuse(op: ([u8; 32], u32)) -> impl Fn(&SignedTx) -> Result<Validation, String> {
    move |signed: &SignedTx| {
        if signed.tx.inputs.iter().any(|i| (i.transaction_id, i.index) == op) {
            return Err("engine: the transaction is refused".into());
        }
        validate_signed(signed).map_err(|e| e.to_string())
    }
}

#[test]
fn a_refusal_that_names_no_order_leaves_the_other_books_their_fills() {
    // a crossed book of token C whose ask the validator refuses, next to ten books
    let mut v = vec![l_bid_of(300, [0x5c; 32], T8, 6, P260, 5 * WHOLE), l_ask_of(301, [0x5c; 32], T8, 7, P250, 5 * WHOLE)];
    ten_books(&mut v);
    let b = book(v);
    let op = outpoint(&b.orders.iter().find(|o| o.id() == cid(301)).unwrap().order.utxo);
    let r = tick_with(&input(&b), &cfg(), &Families::default(), &signer(), &refuse(op));
    assert_eq!(ten_filled(&r), 20, "every order of the ten books is filled; anomalies {:?}", r.anomalies);
    assert_eq!(filled(&r, 301), 0);
    // the search costs a few attempts, not one attempt per order of the tick
    assert!(r.anomalies.len() <= 12, "{} refusals", r.anomalies.len());
}

#[test]
fn without_refusals_the_eleven_books_fill() {
    let mut v = vec![l_bid_of(300, [0x5c; 32], T8, 6, P260, 5 * WHOLE), l_ask_of(301, [0x5c; 32], T8, 7, P250, 5 * WHOLE)];
    ten_books(&mut v);
    let b = book(v);
    let r =
        tick_with(&input(&b), &cfg(), &Families::default(), &signer(), &|s: &SignedTx| validate_signed(s).map_err(|e| e.to_string()));
    assert_eq!(ten_filled(&r), 20);
    assert_eq!(filled(&r, 301), 5 * WHOLE);
}
