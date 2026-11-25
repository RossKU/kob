//! Tests that a refusal that names no order (the pre-simulation fails the transaction, not an input
//! of it) costs the other orders of the tick at most the attempts that search for the order it
//! comes from. The engine leaves out half of the plan's fills at a time, a smaller plan that passes
//! is the batch, and the fills left out are planned in the next batches. The tick does not give up
//! the lowest-priority fills one by one until its attempts run out.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_protocol::tx::SignedTx;
use kob_protocol::verify::{validate_signed, Validation};
use kobd::matcher::book::{outpoint, ListedOrder};
use kobd::matcher::engine::{tick_with, TickReport};
use kobd::matcher::family::Families;

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

/// R9FD-1: an engine refusal that names an input is put on the order whose UTXO that input spends, found by its
/// outpoint. The builder may put the legs in another order than the plan's fills (a bid that ends with a refund of its
/// own goes first), so the input's index is no fill index; before, the quarantine could land on the other order.
#[test]
fn a_refusal_naming_an_input_quarantines_the_order_that_input_spends() {
    // a bid that the fill ends with a refund (2 KAS more escrow than the fill uses) and the ask it crosses
    let mut b0 = l_bid_of(300, [0x5c; 32], T8, 6, P260, 5 * WHOLE);
    b0.order.utxo.amount += 200_000_000;
    let a0 = l_ask_of(301, [0x5c; 32], T8, 7, P250, 5 * WHOLE);
    let b = book(vec![b0, a0]);
    let named = std::sync::Mutex::new(None);
    let refuse_first = |signed: &SignedTx| -> Result<Validation, String> {
        let mut g = named.lock().unwrap();
        if g.is_none() {
            let i = &signed.tx.inputs[0];
            *g = Some((i.transaction_id, i.index));
            return Err("engine: input 0: the covenant refuses it".into());
        }
        validate_signed(signed).map_err(|e| e.to_string())
    };
    let r = tick_with(&input(&b), &cfg(), &Families::default(), &signer(), &refuse_first);
    let op = named.lock().unwrap().expect("a transaction was validated");
    let owner = b.orders.iter().find(|o| outpoint(&o.order.utxo) == op).map(|o| o.id());
    assert!(owner.is_some(), "input 0 spends an order UTXO");
    let q: Vec<_> = r.quarantined.iter().map(|(id, _)| *id).collect();
    assert_eq!(q, vec![owner.unwrap()], "the quarantine lands on the order input 0 spends; {:?}", r.anomalies);
}
