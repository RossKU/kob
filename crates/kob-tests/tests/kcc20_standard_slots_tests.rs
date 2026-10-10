//! KOB's standard token program, the reference KCC-20 in its default configuration (`KCC20Ref`, 3 token inputs / 3 token
//! outputs per transaction), against the order templates' stray scan, whose bound `MAX_TOK_IN = 8` was sized for the 8 / 8
//! prototype. The bound stays: it is an upper bound on the token inputs a settle may carry, and every transaction of a
//! 3 / 3 token carries at most three (the token program's leader takes at most two delegates; after a verified genesis
//! every input of the token's covenant id is a holder of that program, so the leader is one). The scan therefore reads
//! every token input such a transaction can have, and the order templates do not change.
//!
//! Executed in rusty-kaspa v2.1.0's TxScriptEngine (pinned templates):
//!   - three custodies of three orders share one transfer; a fourth is beyond the builder's slots;
//!   - a stray owned by the settling ask at token-input slot 1 or 2 (all slots a 3 / 3 transaction has beside the custody)
//!     is refused by the ask's scan;
//!   - four token inputs (the custody and three key-held fillers, no stray): the 3 / 3 program's leader refuses them, so
//!     no transaction reaches the scan with more than three, while the 8 / 8 prototype takes the same four.
//!
//! Run: cargo test --release -p kob-tests --test kcc20_standard_slots_tests -- --nocapture

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use std::collections::BTreeMap;

use fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::build_with;
use kob_protocol::state::*;
use kob_protocol::tx::{SigPlan, Witness};
use kob_protocol::Family;
use ph::*;

const SA: TemplateId = TemplateId::Kcc20Ref;
const P8: TemplateId = TemplateId::Kcc20Ref8x8;

fn pinned() -> Subs {
    Subs { subs: BTreeMap::new() }
}

/// `take.ask.partial` on program `p` (a taker buys part of one ask: the ask's custody is the one token input).
fn take_ask(p: TemplateId) -> Ed {
    let (_, a) = scenarios_on(p).into_iter().find(|(n, _)| n == "take.ask.partial").expect("take.ask.partial");
    Ed::new(&format!("{}.take.ask.partial", p.name()), &built(a))
}

fn token_inputs(e: &Ed) -> Vec<usize> {
    (0..e.plans.len())
        .filter(|i| cov_of(&e.entries[*i]) == Some(TOKEN_COV) && !matches!(e.plans[*i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }))
        .collect()
}

/// The ask input and its covenant id.
fn the_ask(e: &Ed) -> (usize, [u8; 32]) {
    let i = e.inputs_of(TemplateId::KobAsk)[0];
    (i, cov_of(&e.entries[i]).expect("order covenant"))
}

/// Adds a key-held token input of the matcher (`amount` base units).
fn filler(e: &mut Ed, tag: u8, amount: i64) {
    let st = TokenState::user(Family::Kcc20, amount, pk(MATCHER), EXT);
    e.add_token_input(utxo(tag, CARRIER, 1_000, Some(TOKEN_COV)), st, Witness::P2pk(pk(MATCHER)));
}

/// Adds a stray owned by the ask's covenant id (scheme 0x04, authorised by the ask input itself).
fn stray(e: &mut Ed, tag: u8, amount: i64) {
    let (_, ask_cov) = the_ask(e);
    let st = TokenState::custody(Family::Kcc20, amount, ask_cov, EXT);
    e.add_token_input(utxo(tag, CARRIER, 1_000, Some(TOKEN_COV)), st, Witness::CovenantId);
}

/// One token output taking `amount` (what the added inputs hold) to the matcher: the transaction keeps the base's two
/// token outputs plus this one, within the standard program's three.
fn to_matcher(e: &mut Ed, amount: i64) {
    e.add_token_output(TOKEN_COV, TokenState::user(Family::Kcc20, amount, pk(MATCHER), EXT), CARRIER);
}

#[test]
fn the_scan_bound_covers_every_token_input_of_a_standard_transaction() {
    // the order templates keep MAX_TOK_IN = 8 (every KCC-20 settle path scans that many); the standard program allows 3
    assert_eq!(kob_protocol::state::MAX_TOK_IN, 8);
    assert_eq!(SA.token_slots(), Some((3, 3)));
    assert_eq!(TemplateId::Kcc20PublicMint.token_slots(), Some((3, 3)));
    assert_eq!(kob_protocol::issue::IssueProgram::default().template_id(), SA, "KOB issues the standard program");
    for kind in ["KobAsk", "KobBid", "KobCondAsk", "KobCondBid", "KobIfdBid", "KobIfdAsk", "KobPair", "KobCondPair", "KobIfdPair"] {
        let src = common::contract_source(kind);
        assert!(src.contains("int constant MAX_TOK_IN = 8;"), "{kind}: the scan bound");
    }
}

#[test]
fn three_custodies_share_a_standard_transfer_and_a_fourth_is_beyond_its_slots() {
    let subs = pinned();
    let run = Run { subs: &subs, pair: SA.name().into() };
    let (_, a) = scenarios_on(SA).into_iter().find(|(n, _)| n == "take.ask.partial").unwrap();
    let three = pad_batch(SA, &a, 2, 0).expect("a batch");
    let e = Ed::new("sweep3 on the standard program", &built(three));
    assert_eq!(token_inputs(&e).len(), 3);
    run.ok(&e);
    let four = pad_batch(SA, &a, 3, 0).unwrap();
    let err = build_with(&four, &budgets).unwrap_err().to_string();
    assert!(err.contains("3 token inputs"), "{err}");
    // the 8 / 8 prototype takes four custodies in one transaction
    let (_, a8) = scenarios_on(P8).into_iter().find(|(n, _)| n == "take.ask.partial").unwrap();
    Run { subs: &subs, pair: P8.name().into() }
        .ok(&Ed::new("sweep4 on the 8 / 8 prototype", &built(pad_batch(P8, &a8, 3, 0).unwrap())));
}

#[test]
fn a_stray_at_any_slot_of_a_standard_transaction_is_refused_by_the_scan() {
    let subs = pinned();
    let run = Run { subs: &subs, pair: SA.name().into() };
    let base = take_ask(SA);
    run.ok(&base);
    assert_eq!(token_inputs(&base).len(), 1, "the custody");
    // slot 1: the custody, then the stray
    let mut e = take_ask(SA).named("NST1 stray of the ask at token-input slot 1 (3 / 3)");
    stray(&mut e, 0xd1, 7);
    to_matcher(&mut e, 7);
    let (ask_i, _) = the_ask(&e);
    run.bad(&e, ask_i);
    // slot 2: the custody, a key-held filler, then the stray (the third and last token input the program allows)
    let mut e = take_ask(SA).named("NST2 stray of the ask at token-input slot 2 (3 / 3)");
    filler(&mut e, 0xd2, 1);
    stray(&mut e, 0xd3, 7);
    to_matcher(&mut e, 8);
    assert_eq!(token_inputs(&e).len(), 3);
    run.bad(&e, ask_i);
    // the same two fillers without a stray: accepted (the scan passes, the token program takes three inputs)
    let mut e = take_ask(SA).named("PST3 the custody and two key-held fillers (3 / 3)");
    filler(&mut e, 0xd2, 1);
    filler(&mut e, 0xd4, 2);
    to_matcher(&mut e, 3);
    run.ok(&e);
}

#[test]
fn a_fourth_token_input_is_refused_by_the_standard_program_itself() {
    let subs = pinned();
    let four = |p: TemplateId| {
        let mut e = take_ask(p).named(&format!("custody and three key-held fillers ({})", p.name()));
        for k in 0..3u8 {
            filler(&mut e, 0xe0 + k, 1 + k as i64);
        }
        to_matcher(&mut e, 6);
        assert_eq!((token_inputs(&e).len(), e.tok_outs_of(TOKEN_COV).len()), (4, 3));
        e
    };
    // 3 / 3: the leader (the first token input) refuses a fourth holder (three token outputs: within its limit), whatever
    // the orders' scan allows
    let e = four(SA);
    let res = Run { subs: &subs, pair: SA.name().into() }.exec(&e);
    let leader = token_inputs(&e)[0];
    assert!(res[leader].is_err(), "the 3 / 3 leader takes at most two delegates: {:?}", res[leader]);
    let (ask_i, _) = the_ask(&e);
    assert!(res[ask_i].is_ok(), "the ask's scan (bound 8) is not what refuses: {:?}", res[ask_i]);
    // the 8 / 8 prototype: the same four inputs pass every input
    Run { subs: &subs, pair: P8.name().into() }.ok(&four(P8));
}
