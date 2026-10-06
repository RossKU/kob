//! Conditional orders (stop, trailing, take-profit, OCO) and stop entries with trigger evidence: a stop
//! arms, ratchets or fills at its trigger only in a transaction that fills a plain resting order qualifying as its
//! evidence, and the indexer records which one; all with real builders validated by the script engine.

mod common;

use common::*;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

#[tokio::test]
async fn a_resting_fill_arms_a_stop_in_the_same_batch_then_it_fills_in_a_band() {
    let c = Ctx::new();
    let co = cond_ask(MAKER_A); // take-profit 3.00, stop 2.00, band 300 DAA, minRestDaa 600, minTouch 1 base unit
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");

    // a resting ask at 1.90 (at or below the stop), exposed for minRestDaa, is filled: the matcher arms the stop next to it
    let r = resting(&c, SIDE_ASK, 190_000_000).await;
    let mut b = batch(&c.w, rested(&c, &r, co.min_rest_daa), vec![]);
    let k = r.add_to(&c, &mut b, 5 * WHOLE);
    b.updates = vec![update(c.w.order(&create, 0, AnyState::KobCondAsk(co.clone())), k)];
    let arm = c.w.sign(&Action::Batch(b));
    c.push(&[&arm]).await;
    let armed = CondAskState { armed: 1, ..co.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(armed.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "arm"]);
    let ev = c.hs.event_detail(&cov, "arm")["evidence"].clone();
    assert_eq!(ev["input"], k);
    assert_eq!(ev["order"], r.cov(&c).to_hex());
    assert_eq!(ev["side"], SIDE_ASK);
    assert_eq!(ev["price"], "190000000");
    assert_eq!(c.hs.event_kinds(&r.cov(&c)), ["create", "fill"], "the evidence is an ordinary fill of the resting ask");
    assert!(c.hs.event_detail(&r.cov(&c), "fill").get("evidence").is_none());
    let ai = find_cov(&arm, &cov).expect("the armed continuation");
    let arm_daa = c.w.utxo(&arm, ai).block_daa_score;

    // the armed stop leg fills 4 whole tokens inside its 30 s auction band: the continuation records the origin
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let t = arm_daa as i64 + 100;
    let leg = Leg::CondAsk {
        order: c.w.order(&arm, ai, armed.clone()),
        custody,
        amount: 4 * WHOLE,
        leg: 1,
        evidence: None,
        t: Some(t),
        merge: None,
    };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, t as u64, vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    let expect = CondAskState { armed: arm_daa as i64, amount_left: 6 * WHOLE, ..co.clone() };
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobCondAsk(expect.clone())),
        "armed becomes the auction origin (the arming UTXO's DAA)"
    );
    assert!(c.hs.event_detail(&cov, "fill").get("evidence").is_none(), "an armed stop needs no evidence");
    // the auction view: the stop band opens from the stop toward 3% below it
    let v = view(&c, &cov);
    let au = v.auction.expect("an armed stop leg is an auction");
    assert_eq!((au.kind, au.origin_daa), ("stop", arm_daa as i64));
    assert_eq!(au.start_price, "200000000");
    assert_eq!(au.end_price, expect.stop_floor().to_string());
    // the fill quote is the stop price at t inside the band
    let price: i64 = c.hs.query("SELECT price FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    assert_eq!(price, armed.stop_at(false, t, arm_daa as i64).unwrap());
    assert_eq!(price, 198_000_000, "a third of the way into a 3% band: 1% below the stop");
}

#[tokio::test]
async fn a_stop_leg_fills_at_its_trigger_in_the_batch_of_its_evidence() {
    let c = Ctx::new();
    let co = cond_ask(MAKER_A);
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // a resting ask at 1.95 is filled for 2 whole tokens and, in the same transaction, the stop sells 4 at its trigger
    let r = resting(&c, SIDE_ASK, 195_000_000).await;
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let stop = Leg::CondAsk {
        order: c.w.order(&create, 0, co.clone()),
        custody,
        amount: 4 * WHOLE,
        leg: 1,
        evidence: None,
        t: None,
        merge: None,
    };
    let mut b = batch(&c.w, rested(&c, &r, co.min_rest_daa), vec![stop]);
    let k = r.add_to(&c, &mut b, 2 * WHOLE);
    if let Leg::CondAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = Some(k);
    }
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    // the continuation is armed (its auction starts from this UTXO), 6 whole tokens left
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(CondAskState { amount_left: 6 * WHOLE, armed: 1, ..co.clone() })));
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
    let d = c.hs.event_detail(&cov, "fill");
    assert_eq!(d["evidence"]["input"], k);
    assert_eq!(d["evidence"]["order"], r.cov(&c).to_hex());
    assert_eq!(d["evidence"]["price"], "195000000");
    // a banded stop fills at the stop itself when it triggers (the band opens from the arming)
    let price: i64 = c.hs.query("SELECT price FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    assert_eq!(price, co.stop_at(true, 0, 0).unwrap());
    assert_eq!(price, 200_000_000);
    assert_eq!(c.hs.event_kinds(&r.cov(&c)), ["create", "fill"]);
}

#[tokio::test]
async fn a_stop_arms_only_on_qualifying_evidence_and_nothing_persists() {
    let c = Ctx::new();
    let co = cond_ask(MAKER_A); // stop 2.00, minRestDaa 600
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let order = || c.w.order(&create, 0, AnyState::KobCondAsk(co.clone()));
    let low = resting(&c, SIDE_ASK, 190_000_000).await;
    let high = resting(&c, SIDE_ASK, 210_000_000).await;
    let arming = |r: &Resting, lock: u64| {
        let mut b = batch(&c.w, lock, vec![]);
        let k = r.add_to(&c, &mut b, WHOLE);
        b.updates = vec![update(order(), k)];
        b
    };
    // too young: exposed for less than minRestDaa before the lock time
    assert!(build(&Action::Batch(arming(&low, low.daa(&c) + 100))).is_err());
    // above the stop: a resting ask at 2.10 is no evidence for a stop at 2.00
    assert!(build(&Action::Batch(arming(&high, rested(&c, &high, co.min_rest_daa)))).is_err());
    // below the order's minTouch (2 whole tokens here, the evidence fills 1)
    let picky = CondAskState { min_touch: 2 * WHOLE, ..co.clone() };
    let mut b = arming(&low, rested(&c, &low, co.min_rest_daa));
    b.updates[0].order = OrderUtxo { state: AnyState::KobCondAsk(picky), ..order() };
    assert!(build(&Action::Batch(b)).is_err());

    // a qualifying resting fill without an update arms nothing: the fact does not outlive its transaction
    let lone = c.w.sign(&Action::Batch(batch(&c.w, rested(&c, &low, co.min_rest_daa), vec![low.leg(&c, WHOLE)])));
    c.push(&[&lone]).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(co.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create"]);
    assert_eq!(c.hs.event_kinds(&low.cov(&c)), ["create", "fill"]);
}

#[tokio::test]
async fn trailing_stop_ratchets_by_every_justified_step() {
    let c = Ctx::new();
    let co = CondAskState { trail_step: 10_000_000, trail_gap: 5_000_000, trail_wait: 600, ..cond_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // a resting bid at 2.35 is filled: (2.35 - 0.05 - 2.00) / 0.10 = 3 steps
    let r = resting(&c, SIDE_BID, 235_000_000).await;
    let order_daa = c.w.utxo(&create, 0).block_daa_score;
    let lock = rested(&c, &r, co.min_rest_daa).max(order_daa + 600);
    let mut b = batch(&c.w, lock, vec![]);
    let k = r.add_to(&c, &mut b, 5 * WHOLE);
    b.updates = vec![update(c.w.order(&create, 0, AnyState::KobCondAsk(co.clone())), k)];
    let trail = c.w.sign(&Action::Batch(b));
    c.push(&[&trail]).await;
    assert_eq!(co.trail_steps(235_000_000), 3);
    let expect = CondAskState { stop_price: 200_000_000 + 3 * 10_000_000, ..co.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(expect)));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "trail"]);
    let ev = c.hs.event_detail(&cov, "trail")["evidence"].clone();
    assert_eq!((ev["side"].as_i64(), ev["price"].as_str()), (Some(SIDE_BID), Some("235000000")));
    assert_eq!(ev["order"], r.cov(&c).to_hex());
    assert_eq!(view(&c, &cov).state.unwrap()["state"]["stopPrice"], "230000000");
}

#[tokio::test]
async fn oco_take_profit_leg_fills_without_arming_and_can_be_cancelled() {
    let c = Ctx::new();
    let co = cond_ask(MAKER_A);
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::CondAsk {
        order: c.w.order(&create, 0, co.clone()),
        custody,
        amount: 3 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: None,
    };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let expect = CondAskState { amount_left: 7 * WHOLE, ..co.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(expect.clone())), "both legs stay, unarmed");
    assert_eq!(c.hs.status(&cov), "partial");
    // cancel the rest
    let oi = find_cov(&fill, &cov).unwrap();
    let ci = find_custody(&fill, &cov, 7 * WHOLE).unwrap();
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order: c.w.order(&fill, oi, AnyState::KobCondAsk(expect)),
        custody: Some(c.w.token_at(&fill, ci, Kcc20State::custody(7 * WHOLE, cov.0, EXT))),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "cancel"]);
}

#[tokio::test]
async fn buy_side_conditional_take_profit_and_stop_entries() {
    let c = Ctx::new();
    let cb = cond_bid(MAKER_B);
    let escrow = cb.escrow(2).unwrap() as u64;
    let create = c.w.create_tx(AnyState::KobCondBid(cb.clone()), escrow, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // a seller hits the limit leg (take-profit, 2.00) for 4 whole tokens
    let mut b = batch(
        &c.w,
        c.daa(),
        vec![Leg::CondBid {
            order: c.w.order(&create, 0, cb.clone()),
            amount: 4 * WHOLE,
            leg: 0,
            evidence: None,
            t: None,
            merge: None,
        }],
    );
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    let expect = CondBidState { amount_left: 6 * WHOLE, ..cb.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondBid(expect)));
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);

    // buy-side stop entry of an if-done order is armed by a resting bid filled at or above its trigger
    let ibs = IfdBidState { entry_stop: 255_000_000, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) };
    let ce = c.w.create_tx(AnyState::KobIfdBid(ibs.clone()), ibs.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&ce]).await;
    let ecov = c.w.cov(&ce, 0);
    let r = resting(&c, SIDE_BID, 256_000_000).await;
    let mut b = batch(&c.w, rested(&c, &r, ibs.min_rest_daa), vec![]);
    let k = r.add_to(&c, &mut b, 3 * WHOLE);
    b.updates = vec![update(c.w.order(&ce, 0, AnyState::KobIfdBid(ibs.clone())), k)];
    let arm = c.w.sign(&Action::Batch(b));
    c.push(&[&arm]).await;
    let armed = IfdBidState { armed: 1, ..ibs.clone() };
    assert_eq!(c.hs.tip_state(&ecov), Some(AnyState::KobIfdBid(armed.clone())));
    assert_eq!(c.hs.event_kinds(&ecov), ["create", "arm"]);
    assert_eq!(c.hs.event_detail(&ecov, "arm")["evidence"]["order"], r.cov(&c).to_hex());
    // the armed entry fills inside its band; the continuation and the exit follow
    let ai = find_cov(&arm, &ecov).expect("the armed entry");
    let arm_daa = c.w.utxo(&arm, ai).block_daa_score as i64;
    let t = arm_daa + 100;
    let mut b = batch(
        &c.w,
        t as u64,
        vec![Leg::IfdBid { order: c.w.order(&arm, ai, armed.clone()), amount: 4 * WHOLE, evidence: None, t: Some(t) }],
    );
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    let expect = IfdBidState { amount_left: 6 * WHOLE, armed: arm_daa, ..ibs.clone() };
    assert_eq!(c.hs.tip_state(&ecov), Some(AnyState::KobIfdBid(expect.clone())), "a stop entry records its auction origin");
    let au = view(&c, &ecov).auction.expect("armed stop entry is an auction");
    assert_eq!(au.kind, "entry");
    assert_eq!((au.start_price.as_str(), au.end_price.as_str()), ("255000000", "260000000"));
    let (_, exit) = find_fresh(&fill, &[ecov]).expect("the exit");
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondAsk(ibs.exit_for(4 * WHOLE, None).unwrap())));
}

/// A conditional order without a take-profit leg (`tpPrice` 0: a stop-market, stop-limit or trailing stop) is measured at its
/// stop for the operator's minimum order value. Measured at the absent take-profit it was worth 0, so every such stop was
/// unlisted (`order_value_below_minimum`): never armed, never triggered, never refunded by a keeper (TN10 soak, 2026-10-06).
#[tokio::test]
async fn a_stop_without_a_take_profit_is_listed_at_its_stop_value() {
    let rules = kob_executor::tokens::ListingRules {
        min_order_value_sompi: 15 * KAS as i64,
        max_expiry_span_daa: 1 << 40,
        ..Default::default()
    };
    let proc = kob_executor::indexer::processor::Processor { tokens: std::sync::Arc::new(allowlist()), rules };
    let hs = Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc);
    let c = Ctx::with(hs);
    // a sell stop: 10 whole tokens at the 2.00 stop (20 KAS), no take-profit
    let ask = CondAskState { tp_price: 0, ..cond_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobCondAsk(ask.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let v = view(&c, &c.w.cov(&create, 0));
    assert!(v.listed, "a 20 KAS stop is above the 15 KAS minimum: {:?}", v.unlisted_reason);
    // a buy stop: 10 whole tokens at the 3.00 stop (30 KAS), no take-profit
    let bid = CondBidState { tp_price: 0, ..cond_bid(MAKER_B) };
    let escrow = bid.escrow(2).unwrap() as u64;
    let create_b = c.w.create_tx(AnyState::KobCondBid(bid.clone()), escrow, MAKER_B, 0);
    c.push(&[&create_b]).await;
    let v = view(&c, &c.w.cov(&create_b, 0));
    assert!(v.listed, "a 30 KAS buy stop is above the 15 KAS minimum: {:?}", v.unlisted_reason);
    // still measured: 5 whole tokens at the 2.00 stop (10 KAS) are below the minimum
    let small = CondAskState { amount_left: 5 * WHOLE, ..ask };
    let create_s = c.w.create_tx(AnyState::KobCondAsk(small), CARRIER, MAKER_A, 5 * WHOLE);
    c.push(&[&create_s]).await;
    let v = view(&c, &c.w.cov(&create_s, 0));
    assert!(!v.listed);
    assert_eq!(v.unlisted_reason.as_deref(), Some("order_value_below_minimum"));
}
