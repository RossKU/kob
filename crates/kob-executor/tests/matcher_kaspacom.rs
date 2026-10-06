//! The matcher on KaspaCom's third-party KCC20 (25.5 KB program in every token input): sweeps are bounded by the transaction size
//! budget and the block mass limits, not by a fixed count, and everything it builds runs in the engine with the table budgets.
//! No KaspaCom-specific code exists: the program is one more strict-list template.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::engine::tick;
use kob_executor::matcher::family::{token_limits, Families};
use kob_protocol::artifacts::{token_template, TemplateId};

const KC: TemplateId = TemplateId::Kcc20KaspaCom025;

fn sweep_book(asks: u32) -> kob_executor::matcher::book::MemoryBook {
    let mut orders = vec![l_bid(1, bid(1, P260, KC), asks as i64 * WHOLE)];
    orders.extend((0..asks).map(|i| l_ask(10 + i, ask(3, P250, WHOLE, KC))));
    book(orders)
}

#[test]
fn the_limits_come_from_the_template_not_from_the_family() {
    let l = token_limits(kob_executor::matcher::family::Family::Kcc20, &token_template(KC).hash).unwrap();
    assert_eq!((l.max_in, l.max_out, l.max_output_amount), (8, 8, None));
}

#[test]
fn a_sweep_is_bounded_by_the_size_budget_and_the_mass_limits() {
    // an operator cap of 90 kB: 27.6 kB per token input -> three asks per transaction
    let b = sweep_book(8);
    let mut c = cfg();
    c.planner.max_tx_bytes = 90_000;
    let r = tick(&input(&b), &c, &Families::default(), &signer());
    assert_no_budget_slack();
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    let first = &r.prepared[0];
    let asks = first.plan.fills.iter().filter(|f| f.cand.side == kob_executor::matcher::candidate::Side::Ask).count();
    assert_eq!(asks, 3, "90 kB fit three 27.6 kB token inputs");
    assert!(first.signed.fee.mass.size <= c.planner.max_tx_bytes, "{} bytes", first.signed.fee.mass.size);
    assert!(first.validation.is_some());
    // the default budget is the physical limit (250 kB of transient mass, the block mass limits): the whole 8-input sweep in
    // one transaction
    c.planner.max_tx_bytes = 0;
    let r = tick(&input(&b), &c, &Families::default(), &signer());
    assert_no_budget_slack();
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    let first = &r.prepared[0];
    let asks = first.plan.fills.iter().filter(|f| f.cand.side == kob_executor::matcher::candidate::Side::Ask).count();
    assert_eq!(asks, 8, "the token program allows 8 inputs and the mass limits leave room");
    let v = first.validation.as_ref().unwrap();
    assert!(v.mass.within_block_limits(), "{:?}", v.mass);
    assert!(v.fee >= v.min_fee, "the node floor is paid: {} >= {}", v.fee, v.min_fee);
}
