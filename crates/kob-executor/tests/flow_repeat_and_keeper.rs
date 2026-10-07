//! Repeat entries and keeper updates, on the real script engine:
//!
//! * a fill of a repeating entry larger than the re-arms it has left is refused while at least one minimum fill of them is
//!   left (the builder and the covenant): the fill of the re-arms left is booked, so no re-arm is lost; with fewer left
//!   than a minimum fill (or none) the fill is not booked;
//! * a booked exit's `rptUntil` counts from the entry UTXO's DAA, whatever time argument the filler passes;
//! * a keeper's tip of a conditional bid (trailing buy stop) never comes out of what buying its amount needs;
//! * a trailing ratchet by the smaller of two qualifying fills of one transaction moves the stop by fewer steps, never past
//!   what the fills justify, and the next ratchet after `trailWait` reaches the justified level (the bound of a keeper's
//!   choice of evidence, `docs/spec/matcher.md` §4.5).

mod common;

use common::*;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// Builds `b` without the builder's own refusal of a repeat entry's fill beyond its re-arms, signs it and runs it through
/// the script engine.
fn unchecked(b: &Batch) -> Result<(), String> {
    let built = build_batch_unchecked(b, &kob_protocol::budget::lookup).map_err(|e| e.to_string())?;
    let sigs = sign_locally(&built, &keys()).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    kob_protocol::verify::validate_signed(&signed).map(|_| ()).map_err(|e| e.to_string())
}

/// A buy-first repeating entry of 10 whole tokens (minimum fill 3) with 5 whole tokens of re-arms left.
fn entry() -> IfdBidState {
    IfdBidState { rpt_amount: 1 + 5 * WHOLE, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) }
}

fn fill(c: &Ctx, create: &SignedTx, s: &IfdBidState, n: i64, t: Option<i64>) -> Batch {
    let mut b = batch(&c.w, c.daa(), vec![Leg::IfdBid { order: c.w.order(create, 0, s.clone()), amount: n, evidence: None, t }]);
    b.taker_tokens = vec![c.w.token(TAKER, n)];
    b
}

#[tokio::test]
async fn a_repeating_entry_fills_at_most_its_re_arms_left_and_books_them() {
    let c = Ctx::new();
    let ib = entry();
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // 7 whole tokens are more than the 5 of re-arms left: the builder refuses, and so does the entry covenant
    let big = fill(&c, &create, &ib, 7 * WHOLE, None);
    let e = build(&Action::Batch(big.clone())).unwrap_err().to_string();
    assert!(e.contains("re-arms"), "{e}");
    let e = unchecked(&big).unwrap_err();
    assert!(e.contains("input 0"), "the entry refuses the unbooked fill: {e}");
    // the re-arms left (5 whole tokens) are filled and booked: every re-arm is used
    let f = c.w.sign(&Action::Batch(fill(&c, &create, &ib, 5 * WHOLE, None)));
    c.push(&[&f]).await;
    let rest = IfdBidState { amount_left: 5 * WHOLE, rpt_amount: 1, ..ib.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBid(rest.clone())));
    let (_, exit) = find_fresh(&f, &[cov]).unwrap();
    let Some(AnyState::KobCondAsk(x)) = c.hs.tip_state(&exit) else { panic!("the exit") };
    assert_eq!((x.parent, x.amount_left), (cov.0, 5 * WHOLE), "booked");
    // re-arms used up: the rest fills unbooked
    let i = find_cov(&f, &cov).unwrap();
    let mut b =
        batch(&c.w, c.daa(), vec![Leg::IfdBid { order: c.w.order(&f, i, rest.clone()), amount: 5 * WHOLE, evidence: None, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 5 * WHOLE)];
    let g = c.w.sign(&Action::Batch(b));
    c.push(&[&g]).await;
    let (_, exit2) = find_fresh(&g, &[cov, exit]).unwrap();
    let Some(AnyState::KobCondAsk(x2)) = c.hs.tip_state(&exit2) else { panic!("the second exit") };
    assert_eq!(x2.parent, [0; 32], "not booked: no re-arm was left");
}

#[tokio::test]
async fn fewer_re_arms_than_a_minimum_fill_do_not_bound_the_fill() {
    let c = Ctx::new();
    // 2 whole tokens of re-arms, minimum fill 3: a fill of 3 or more is not booked (a smaller one is below the minimum)
    let ib = IfdBidState { rpt_amount: 1 + 2 * WHOLE, ..entry() };
    assert_eq!(rpt_fill_max(ib.rpt_amount, ib.min_fill), None);
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let f = c.w.sign(&Action::Batch(fill(&c, &create, &ib, 4 * WHOLE, None)));
    c.push(&[&f]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBid(IfdBidState { amount_left: 6 * WHOLE, ..ib.clone() })));
}

#[tokio::test]
async fn the_re_arm_window_counts_from_the_entry_whatever_time_the_filler_passes() {
    let c = Ctx::new();
    let ib = IfdBidState { expiry_daa: i64::MAX / 4, ..entry() };
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    // the filler passes t = 0: the booked exit's rptUntil is still the entry UTXO's DAA + 90 days
    let f = c.w.sign(&Action::Batch(fill(&c, &create, &ib, 3 * WHOLE, Some(0))));
    c.push(&[&f]).await;
    let (_, exit) = find_fresh(&f, &[cov]).unwrap();
    let Some(AnyState::KobCondAsk(x)) = c.hs.tip_state(&exit) else { panic!("the exit") };
    assert_eq!(x.rpt_until, entry_daa + 77_760_000);
    assert_eq!(x.rpt_until, rpt_until(ib.expiry_daa, entry_daa).unwrap());
}

/// A trailing buy stop whose order UTXO holds exactly what buying its amount at its dearest leg needs: no keeper tip is
/// funded, so a ratchet pays its keeper only what the lower stop frees.
#[tokio::test]
async fn a_buy_stop_keeper_tip_never_comes_out_of_its_buying_budget() {
    let c = Ctx::new();
    // small steps: a ratchet frees less of the budget than the keeper tip
    let base = CondBidState { trail_step: 1_000, trail_gap: 1_000, trail_wait: 0, min_rest_daa: 10, ..cond_bid(MAKER_B) };
    let need = base.spend(base.amount_left, base.worst()).unwrap() + base.delivery_carrier;
    assert!(base.keeper_tip > 0);
    assert_eq!(base.keeper_take(need, base.stop_price), Some(0), "nothing above the buy: no tip");
    assert_eq!(base.keeper_take(need + base.keeper_tip, base.stop_price), Some(base.keeper_tip), "a funded tip is paid");
    let create = c.w.create_tx(AnyState::KobCondBid(base.clone()), need as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // a resting ask one step below the stop: the trail ratchets the stop down by one step
    let low = resting(&c, SIDE_ASK, base.stop_price - base.trail_gap - base.trail_step).await;
    let lock = rested(&c, &low, base.min_rest_daa).max(c.w.utxo(&create, 0).block_daa_score + 1);
    let mut b = batch(&c.w, lock, vec![]);
    let k = low.add_to(&c, &mut b, WHOLE);
    let mut u = update(c.w.order(&create, 0, AnyState::KobCondBid(base.clone())), k);
    // the whole keeperTip would come out of the budget: the builder refuses it
    u.take = Some(base.keeper_tip);
    b.updates = vec![u.clone()];
    let e = build(&Action::Batch(b.clone())).unwrap_err().to_string();
    assert!(e.contains("keeper takes at most"), "{e}");
    // the default take is what the order holds beyond the buy at the new stop
    u.take = None;
    b.updates = vec![u];
    let t = c.w.sign(&Action::Batch(b));
    c.push(&[&t]).await;
    let oi = find_cov(&t, &cov).unwrap();
    let Some(AnyState::KobCondBid(after)) = c.hs.tip_state(&cov) else { panic!("the continuation") };
    assert!(after.stop_price < base.stop_price, "trailed");
    assert_eq!(after.stop_price, base.stop_price - base.trail_step);
    let left = t.tx.outputs[oi].value as i64;
    assert!(left >= after.spend(after.amount_left, after.worst()).unwrap() + after.delivery_carrier, "the whole buy stays funded");
    let took = need - left;
    assert!(took > 0 && took < base.keeper_tip, "the keeper took what the lower stop freed ({took}), not keeperTip");
}

/// The keeper names the smaller of two qualifying fills: the stop moves by its steps only, and after `trailWait` the next
/// ratchet reaches the level the larger fill justified.
#[tokio::test]
async fn a_ratchet_by_the_smaller_fill_lags_at_most_one_trail_wait() {
    let c = Ctx::new();
    let co = CondAskState { trail_step: 10_000_000, trail_gap: 5_000_000, trail_wait: 600, min_rest_daa: 10, ..cond_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let order_daa = c.w.utxo(&create, 0).block_daa_score;
    // two resting bids: 2.15 justifies 1 step, 2.35 justifies 3 steps
    let low = resting(&c, SIDE_BID, 215_000_000).await;
    let high = resting(&c, SIDE_BID, 235_000_000).await;
    let lock = rested(&c, &high, co.min_rest_daa).max(rested(&c, &low, co.min_rest_daa)).max(order_daa + 600);
    let mut b = batch(&c.w, lock, vec![]);
    let k_low = low.add_to(&c, &mut b, 5 * WHOLE);
    let _ = high.add_to(&c, &mut b, 5 * WHOLE);
    b.updates = vec![update(c.w.order(&create, 0, AnyState::KobCondAsk(co.clone())), k_low)];
    let smaller = c.w.sign(&Action::Batch(b));
    c.push(&[&smaller]).await;
    let one = CondAskState { stop_price: 210_000_000, ..co.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(one.clone())), "one step, never past what a fill justifies");
    // after trailWait the next qualifying fill ratchets to the justified level
    let oi = find_cov(&smaller, &cov).unwrap();
    let cont = c.w.order(&smaller, oi, AnyState::KobCondAsk(one));
    let cont_daa = c.w.utxo(&smaller, oi).block_daa_score;
    let high2 = resting(&c, SIDE_BID, 235_000_000).await;
    let mut b = batch(&c.w, (cont_daa + 600).max(rested(&c, &high2, co.min_rest_daa)), vec![]);
    let k = high2.add_to(&c, &mut b, 5 * WHOLE);
    b.updates = vec![update(cont, k)];
    let next = c.w.sign(&Action::Batch(b));
    c.push(&[&next]).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(CondAskState { stop_price: 230_000_000, ..co })));
}
