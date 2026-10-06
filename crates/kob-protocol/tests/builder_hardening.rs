//! Builder hardening: a refund with funding but no change key, a hostile order state
//! traps the encoder, off-curve keys written into outputs, and finalize on a malformed `BuiltTx`.
mod common;

use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

fn find(name: &str) -> Action {
    scenarios().into_iter().find(|(n, _)| n == name).unwrap().1
}

/// An x-only key that is not on the curve.
fn off_curve() -> [u8; 32] {
    (0u8..=255)
        .map(|b| {
            let mut k = [0u8; 32];
            k[31] = b;
            k
        })
        .find(|k| secp256k1::XOnlyPublicKey::from_slice(k).is_err())
        .expect("some x has no curve point")
}

#[test]
fn refund_with_funding_and_no_change_returns_the_change_to_the_funding_key() {
    for name in ["refund.bid", "refund.ask.expiry", "refund.condAsk", "refund.ifdBid"] {
        let Action::RefundOrder(mut r) = find(name) else { panic!("{name}") };
        r.funding = vec![key_utxo(77, KEEPER, 500 * KAS)];
        r.change = None;
        let built = build(&Action::RefundOrder(r)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(built.fee.change_output.is_some(), "{name}: the funding surplus goes to a change output");
        assert!(built.fee.fee < KAS, "{name}: the fee stays near the floor, not the funding ({} sompi)", built.fee.fee);
        let keeper = spk_to_string(&kob_protocol::script::p2pk_spk(&pk(KEEPER)));
        let idx = built.fee.change_output.unwrap() as usize;
        assert_eq!(built.tx.outputs[idx].script_public_key, keeper);
    }
}

#[test]
fn refund_without_funding_still_builds() {
    let Action::RefundOrder(mut r) = find("refund.bid") else { panic!() };
    r.funding.clear();
    r.change = None;
    let built = build(&Action::RefundOrder(r)).expect("the tip pays the fee");
    assert!(built.fee.change_output.is_none());
}

fn hostile_ifd_bid(len: usize) -> AnyState {
    let mut ib = ifd_bid(MAKER_A, 10, TemplateId::Kcc20Ref);
    ib.exit_state = vec![0x5a; len];
    AnyState::KobIfdBid(ib)
}

#[test]
fn hostile_exit_state_is_refused_not_trapped() {
    for len in [0usize, 3, 100, 320, 322, 1000] {
        let st = hostile_ifd_bid(len);
        assert!(st.validate().is_err(), "len {len}: validate must refuse");
        let r = catch_unwind(AssertUnwindSafe(|| st.try_encode()));
        assert!(matches!(r, Ok(Err(_))), "len {len}: try_encode returns an error instead of panicking");
        let json = serde_json::to_string(&st).unwrap();
        let r = catch_unwind(AssertUnwindSafe(|| encode_state_json(&json)));
        assert!(matches!(r, Ok(Err(_))), "len {len}: encode_state_json returns an error");
    }
}

#[test]
fn builders_refuse_the_hostile_order_without_panicking() {
    let st = hostile_ifd_bid(3);
    let cancel = CancelOrder {
        prefund: None,
        order: order(50, 100 * KAS, cov(0xd1), 1_000, st.clone()),
        custody: None,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    };
    let r = catch_unwind(AssertUnwindSafe(|| build(&Action::CancelOrder(cancel))));
    assert!(matches!(r, Ok(Err(_))), "cancel");
    let Action::RefundOrder(mut rf) = find("refund.bid") else { panic!() };
    rf.order = order(51, 100 * KAS, cov(0xd2), 1_000, st.clone());
    let r = catch_unwind(AssertUnwindSafe(|| build(&Action::RefundOrder(rf))));
    assert!(matches!(r, Ok(Err(_))), "refund");
    let Action::CreateOrder(mut c) = find("create.bid") else { panic!() };
    c.order = st;
    let r = catch_unwind(AssertUnwindSafe(|| build(&Action::CreateOrder(c))));
    assert!(matches!(r, Ok(Err(_))), "create");
}

#[test]
fn an_honest_ifd_bid_still_validates_and_encodes() {
    let st = AnyState::KobIfdBid(ifd_bid(MAKER_A, 10, TemplateId::Kcc20Ref));
    st.validate().expect("honest state validates");
    assert_eq!(st.try_encode().unwrap(), st.encode());
}

#[test]
fn off_curve_keys_are_refused_everywhere_they_become_an_output() {
    let bad = off_curve();
    // token send: recipient, token change and change
    let mut s = scenarios().into_iter().find_map(|(_, a)| if let Action::SendTokens(s) = a { Some(s) } else { None }).unwrap();
    s.recipients[0].pubkey = bad;
    assert!(build(&Action::SendTokens(s.clone())).is_err(), "send recipient");
    s.recipients[0].pubkey = pk(TAKER);
    s.token_change = Some(bad);
    assert!(build(&Action::SendTokens(s.clone())).is_err(), "send token change");
    s.token_change = None;
    s.change = Some(bad);
    assert!(build(&Action::SendTokens(s.clone())).is_err(), "send change");
    s.change = None;
    build(&Action::SendTokens(s)).expect("the honest send builds");
    // order maker and change
    for name in ["create.bid", "create.ask"] {
        let Action::CreateOrder(mut c) = find(name) else { panic!() };
        match &mut c.order {
            AnyState::KobBid(b) => b.maker = bad,
            AnyState::KobAsk(a) => a.maker = bad,
            _ => unreachable!(),
        }
        assert!(build(&Action::CreateOrder(c)).is_err(), "{name}: maker");
        let Action::CreateOrder(mut c2) = find(name) else { panic!() };
        c2.change = Some(bad);
        assert!(build(&Action::CreateOrder(c2)).is_err(), "{name}: change");
    }
    // batch taker / change / receivers
    for name in ["take.bid.partial", "take.ask.partial"] {
        let Action::Batch(honest) = find(name) else { panic!() };
        let mut b = honest.clone();
        b.taker = Some(bad);
        assert!(build(&Action::Batch(b)).is_err(), "{name}: taker");
        let mut b = honest.clone();
        b.change = Some(bad);
        assert!(build(&Action::Batch(b)).is_err(), "{name}: change");
        let mut b = honest.clone();
        b.receivers = vec![TokenPayee { covenant_id: TOKEN_COV, pubkey: bad }];
        assert!(build(&Action::Batch(b)).is_err(), "{name}: receiver");
        build(&Action::Batch(honest)).expect("the honest batch builds");
    }
}

#[test]
fn an_off_curve_maker_on_chain_does_not_panic_a_refund() {
    // keys taken from an existing order state are not the caller's choice
    let Action::RefundOrder(mut r) = find("refund.bid") else { panic!() };
    if let AnyState::KobBid(b) = &mut r.order.state {
        b.maker = off_curve();
    }
    let _ = build(&Action::RefundOrder(r));
}

fn built_create() -> BuiltTx {
    let Action::CreateOrder(c) = find("create.bid") else { panic!() };
    build(&Action::CreateOrder(c)).unwrap()
}

#[test]
fn finalize_refuses_a_malformed_built_tx_instead_of_panicking() {
    let built = built_create();
    let sigs = sign_locally(&built, &keys()).unwrap();
    let opts = FinalizeOptions::default();
    let refused = |b: &BuiltTx| matches!(catch_unwind(AssertUnwindSafe(|| finalize(b, &sigs, opts))), Ok(Err(_)));
    // out-of-range sign request
    let mut b = built.clone();
    b.sign[0].input_index = 99;
    assert!(refused(&b), "input_index 99");
    b.sign[0].input_index = usize::MAX;
    assert!(refused(&b), "input_index usize::MAX");
    // a plan naming a raw token program as an artifact template
    let mut b = built.clone();
    b.plans[0] = SigPlan::Entry {
        template: TemplateId::KronToken2433,
        state: vec![0; 3],
        entry: "x".into(),
        args: vec![Arg::Sig(built.sign[0].pubkey)],
    };
    assert!(refused(&b), "non-artifact template in Entry");
    // an artifact template where a raw token program is expected
    let mut b = built.clone();
    b.plans[0] = SigPlan::KronToken {
        template: TemplateId::KobAsk,
        state: KronState::addr(5, [1; 32]),
        next_states: vec![],
        witnesses: vec![],
    };
    assert!(refused(&b), "artifact template in KronToken");
    // outputs beyond inputs
    let mut b = built.clone();
    b.tx.outputs.clear();
    b.tx.outputs.push(built.tx.outputs[0].clone());
    b.tx.outputs[0].value = u64::MAX;
    assert!(refused(&b), "outputs > inputs");
    // the honest one still finalizes
    finalize(&built, &sigs, opts).expect("honest");
}

#[test]
fn assemble_refuses_a_bad_sign_request() {
    let built = built_create();
    let sigs = sign_locally(&built, &keys()).unwrap();
    let mut b = built;
    b.sign[0].input_index = 7;
    assert!(matches!(catch_unwind(AssertUnwindSafe(|| assemble(&b, &sigs))), Ok(Err(_))));
}

/// The minimum fill (anti-dust): every kind's builder refuses a fill below `minFill` unless it takes everything left (a
/// bid: unless less than one minimum fill of buying power is left after it), as the covenants do.
#[test]
fn builders_refuse_fills_below_the_minimum_fill() {
    fn set_amount(b: &mut Batch, n: i64) {
        match &mut b.legs[0] {
            Leg::Ask { amount, .. }
            | Leg::Bid { amount, .. }
            | Leg::CondAsk { amount, .. }
            | Leg::CondBid { amount, .. }
            | Leg::IfdBid { amount, .. }
            | Leg::IfdAsk { amount, .. }
            | Leg::Pair { amount, .. }
            | Leg::CondPair { amount, .. }
            | Leg::IfdPair { amount, .. } => *amount = n,
        }
    }
    let refused = |b: &Batch, what: &str| {
        let e = build(&Action::Batch(b.clone())).expect_err(what).to_string();
        assert!(e.contains("minFill"), "{what}: {e}");
    };
    for name in [
        "take.ask.partial",
        "take.bid.partial",
        "cond.ask.takeProfit.partial",
        "cond.bid.limit.partial",
        "ifd.bid.partial",
        "ifd.ask.partial",
    ] {
        let Action::Batch(mut b) = find(name) else { panic!("{name}") };
        set_amount(&mut b, WHOLE - 1);
        refused(&b, name);
        // one whole token, the fixtures' minimum fill, is accepted
        let Action::Batch(mut b) = find(name) else { panic!("{name}") };
        set_amount(&mut b, WHOLE);
        build(&Action::Batch(b)).unwrap_or_else(|e| panic!("{name} at minFill: {e}"));
    }
    // the fill that takes everything left may be smaller than minFill (an ask holding less than one minimum fill)
    let Action::Batch(mut b) = find("take.ask.partial") else { panic!() };
    if let Leg::Ask { order, custody, amount, .. } = &mut b.legs[0] {
        order.state.amount_left = WHOLE / 2;
        custody.state = custody.state.with_amount(WHOLE / 2);
        *amount = WHOLE / 2;
    }
    build(&Action::Batch(b)).expect("a remainder below minFill sells out");
    // a bid whose escrow buys less than one minimum fill after this fill terminates: its last fill may be smaller
    let Action::Batch(mut b) = find("take.bid.partial") else { panic!() };
    if let Leg::Bid { order, amount, .. } = &mut b.legs[0] {
        order.utxo.amount = order.state.escrow(WHOLE / 2, 1).unwrap() as u64;
        *amount = WHOLE / 2;
    }
    b.taker_tokens[0].state = b.taker_tokens[0].state.with_amount(WHOLE / 2);
    build(&Action::Batch(b)).expect("a terminating bid fill below minFill");
}

/// Protocol v3 §7: a booked exit is never armed or trailed in a transaction that also spends its repeat entry (the
/// covenants refuse `update` when the parent is among the inputs: its merge would read the update's first push as the
/// amount sold).
#[test]
fn an_exit_update_next_to_its_repeat_entry_is_refused() {
    // a matcher's batch arming the sell-stop entry 0xf1 next to an ask fill, plus a trail of the entry's booked exit
    let Action::Batch(mut b) = find("ifd.ask.update.arm") else { panic!() };
    b.updates[0].order.utxo.covenant_id = Some(cov(0xf1));
    let exit = CondBidState { trail_step: 5_000_000, trail_wait: 0, ..booked_bid_exit(TemplateId::Kcc20Ref, 4) };
    assert!(exit.is_booked() && exit.parent == cov(0xf1));
    let trail = BatchUpdate {
        evidence_b: None,
        order: order(75, exit.escrow(2).unwrap() as u64, cov(0xe7), 2_000, AnyState::KobCondBid(exit)),
        evidence: 0,
        take: None,
    };
    b.updates.push(trail.clone());
    let e = build(&Action::Batch(b.clone())).expect_err("exit update next to its entry").to_string();
    assert!(e.contains("repeat entry"), "{e}");
    // without the entry in the transaction the trail builds
    b.updates = vec![trail];
    build(&Action::Batch(b)).expect("the exit trails on its own");
}

/// A TWAP / DCA cap below the minimum fill (`0 < maxFill < minFill`) makes an order that can never fill: every builder
/// refuses to create it (plain ask, plain bid, pair order); `maxFill = minFill` and `maxFill = 0` (off) are fine.
#[test]
fn builders_refuse_a_max_fill_below_the_min_fill() {
    let t = TemplateId::Kcc20Ref8x8;
    let a = ask(MAKER_A, P250, t);
    assert!(check_new_order(&AnyState::KobAsk(AskState { max_fill: a.min_fill - 1, ..a.clone() })).is_err());
    check_new_order(&AnyState::KobAsk(AskState { max_fill: a.min_fill, ..a.clone() })).unwrap();
    check_new_order(&AnyState::KobAsk(AskState { max_fill: 0, ..a })).unwrap();
    let b = bid(MAKER_A, P245, t);
    assert!(check_new_order(&AnyState::KobBid(BidState { max_fill: b.min_fill - 1, ..b.clone() })).is_err());
    check_new_order(&AnyState::KobBid(BidState { max_fill: b.min_fill, ..b })).unwrap();
    let p = common::pair::pair(MAKER_A, true, t, t, common::pair::RATE);
    assert!(check_new_order(&AnyState::KobPair(PairState { max_fill: p.min_fill - 1, ..p.clone() })).is_err());
    check_new_order(&AnyState::KobPair(PairState { max_fill: p.min_fill, ..p })).unwrap();
}

/// An if-done entry whose committed exit quotes at another scale than the entry is refused by the state decoder (and so by
/// the indexer and every builder), in both families: the merge amounts are computed at each side's own scale and the
/// covenant compares the exit only to the committed bytes, so such an order would leak on every merge.
#[test]
fn an_if_done_entry_with_an_exit_at_another_scale_is_refused() {
    for t in [TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433] {
        let f = t.family();
        let ib = ifd_bid(MAKER_A, 10, t);
        let ok = AnyState::KobIfdBid(ib.clone()).into_family(f);
        ok.validate().unwrap();
        let bad_exit = CondAskState { scale: SCALE * 10, ..ifd_exit(MAKER_A, t) };
        let bad = AnyState::KobIfdBid(IfdBidState { exit_state: IfdBidState::commit_exit(&bad_exit), ..ib }).into_family(f);
        assert!(bad.validate().is_err(), "{}", t.name());
        assert!(AnyState::decode(bad.template_id(), &bad.try_encode().unwrap()).is_err(), "{}", t.name());
        let ia = ifd_ask(MAKER_A, t);
        AnyState::KobIfdAsk(ia.clone()).into_family(f).validate().unwrap();
        let bad_exit = CondBidState { scale: SCALE * 10, ..ifda_exit(MAKER_A, t) };
        let bad = AnyState::KobIfdAsk(IfdAskState { exit_state: IfdAskState::commit_exit_for(f, &bad_exit), ..ia }).into_family(f);
        assert!(bad.validate().is_err(), "{}", t.name());
        assert!(AnyState::decode(bad.template_id(), &bad.try_encode().unwrap()).is_err(), "{}", t.name());
    }
}
