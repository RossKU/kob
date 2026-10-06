//! Matcher: an engine pre-simulation that the TOKEN program rejects (a frozen or blacklisted balance) flags the order it can be
//! attributed to instead of stalling the book; an order-covenant failure does not. The validator is injected (no real freezable
//! program is on the strict list): it fails the input a token program would.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::outpoint;
use kob_executor::matcher::engine::{tick_with, TickReport};
use kob_executor::matcher::family::Families;
use kob_protocol::tx::SignedTx;
use kob_protocol::verify::{validate_signed, Validation};

/// Validator failing the (token) input that spends `custody` of the frozen order, like a token program that rejects the spend.
fn freeze(spent: (Vec<u8>, u32)) -> impl Fn(&SignedTx) -> Result<Validation, String> {
    move |signed: &SignedTx| {
        if let Some(i) = signed.tx.inputs.iter().position(|inp| (inp.transaction_id.to_vec(), inp.index) == spent) {
            return Err(format!("engine: input {i}: ScriptFailed(\"OpFrozenBalance\")"));
        }
        validate_signed(signed).map_err(|e| e.to_string())
    }
}

fn run_with(b: &kob_executor::matcher::book::MemoryBook, v: &dyn Fn(&SignedTx) -> Result<Validation, String>) -> TickReport {
    tick_with(&input(b), &cfg(), &Families::default(), &signer(), v)
}

#[test]
fn a_custody_the_token_program_rejects_flags_the_ask_and_the_rest_still_trades() {
    let frozen = l_ask(2, ask(2, P250, 5 * WHOLE, T3));
    let custody_op = outpoint(&frozen.custody.as_ref().unwrap().utxo);
    let b = book(vec![l_bid(1, bid(1, P260, T3), 8 * WHOLE), frozen, l_ask(3, ask(3, P255, 10 * WHOLE, T3))]);
    let r = run_with(&b, &freeze((custody_op.0.to_vec(), custody_op.1)));
    assert_eq!(r.suspects.len(), 1, "{:?} anomalies {:?}", r.suspects, r.anomalies);
    assert_eq!(r.suspects[0].id, cid(2));
    assert!(r.suspects[0].reason.contains("token program"), "{}", r.suspects[0].reason);
    // the bid is filled by the other ask; the frozen one is not in the transaction
    assert_eq!(r.prepared.len(), 1);
    assert_eq!(r.prepared[0].plan.amount_of(&cid(2)), 0);
    assert_eq!(r.prepared[0].plan.amount_of(&cid(3)), 8 * WHOLE);
    assert!(r.cleared.contains(&cid(1)) && r.cleared.contains(&cid(3)) && !r.cleared.contains(&cid(2)));
}

#[test]
fn an_order_covenant_rejection_is_not_a_freeze() {
    // the failing input is the ask's own covenant (role KobAsk...), not a token program: nothing is flagged
    let b = book(vec![
        l_bid(1, bid(1, P260, T3), 8 * WHOLE),
        l_ask(2, ask(2, P250, 5 * WHOLE, T3)),
        l_ask(3, ask(3, P255, 10 * WHOLE, T3)),
    ]);
    let order_op = outpoint(&b.orders.iter().find(|o| o.id() == cid(2)).unwrap().order.utxo);
    let v = move |signed: &SignedTx| {
        if let Some(i) = signed.tx.inputs.iter().position(|inp| (inp.transaction_id, inp.index) == order_op) {
            return Err(format!("engine: input {i}: ScriptFailed(\"stale state\")"));
        }
        validate_signed(signed).map_err(|e| e.to_string())
    };
    let r = run_with(&b, &v);
    assert!(r.suspects.is_empty(), "{:?}", r.suspects);
}

#[test]
fn a_book_without_rejections_flags_nothing_and_clears_what_it_trades() {
    let b = book(vec![
        l_bid(1, bid(1, P260, T3), 8 * WHOLE),
        l_ask(2, ask(2, P250, 5 * WHOLE, T3)),
        l_ask(3, ask(3, P255, 10 * WHOLE, T3)),
    ]);
    let r = run(&input(&b), &cfg());
    assert!(r.suspects.is_empty());
    assert!(r.cleared.contains(&cid(1)) && r.cleared.contains(&cid(2)) && r.cleared.contains(&cid(3)));
}
