//! In-place amend (AMEND record, payload version 4) and the change rule of the amend builders.
//!
//! The maker's `cancel` of a plain ask (SIGHASH_ALL, no output rule) continues the order's covenant id with new
//! terms; the custody, owned by that id, never moves. Every shape is validated through the rusty-kaspa v2.1.0
//! engine; the record is re-derived trusting nothing (`payload::verify_amend`) and every forgery is refused:
//! another state than the output, terms that would move the custody (scale, quantity, token, maker), a second output
//! carrying the id, a record next to a fill instead of the maker's cancel, a kind that is not a plain ask or bid.
//! A plain bid (no custody: its quantity is its escrow) is amended the same way, keeping its token and scale.

mod common;

use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action, AmendOrder, Leg};
use kob_protocol::payload::{self, decode, encode, recover_amends, recover_amends_planned, recover_orders, verify_amend, Record};
use kob_protocol::state::{AnyState, AskState, BidState};
use kob_protocol::tx::{
    finalize, sign_locally, spk_to_string, BuiltTx, CovenantJson, FinalizeOptions, OrderUtxo, SignedTx, TxOutputJson, Utxo,
};
use kob_protocol::verify::validate_signed;
use kob_protocol::Error;

const A_ID: [u8; 32] = [0xa1; 32];

fn signed(name: &str, a: &Action) -> (BuiltTx, SignedTx) {
    let built = build(a).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, &keys()).unwrap();
    let s = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    validate_signed(&s).unwrap_or_else(|e| panic!("{name}: engine rejected: {e}"));
    (built, s)
}

fn scenario(p: TemplateId, name: &str) -> Action {
    scenarios_on(p).into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no scenario {name}")).1
}

fn amend_req(a: &Action) -> &AmendOrder {
    let Action::AmendOrder(r) = a else { panic!("not an amend") };
    r
}

fn ask_of(s: &AnyState) -> &AskState {
    let (AnyState::KobAsk(a) | AnyState::KobAskKron(a)) = s else { panic!("not an ask") };
    a
}

/// The programs the amend is measured on: the reference KCC-20, the 8/8 program and both KRON programs.
const PROGRAMS: [TemplateId; 4] =
    [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433, TemplateId::KronToken2732];

#[test]
fn amend_keeps_the_covenant_id_and_never_moves_the_custody() {
    for p in PROGRAMS {
        let a = scenario(p, "amend.ask");
        let r = amend_req(&a);
        let (built, s) = signed("amend.ask", &a);
        // one input (the order, by its maker's cancel), one output: the order again, under its own covenant id
        assert_eq!(s.tx.inputs.len(), 1, "{}", p.name());
        assert_eq!(s.tx.inputs[0].utxo.covenant_id, Some(A_ID));
        assert_eq!(s.tx.outputs.len(), 1, "{}: no token output, no change output", p.name());
        assert_eq!(s.tx.outputs[0].covenant, Some(CovenantJson { authorizing_input: 0, covenant_id: A_ID }));
        assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&r.amended.spk()));
        // the carrier paid the fee, exactly the relay floor
        assert_eq!(s.tx.outputs[0].value, CARRIER - s.fee.fee, "{}", p.name());
        assert_eq!(s.fee.fee, s.fee.min_fee);
        assert!(
            s.fee.mass.storage <= s.fee.mass.fee_mass,
            "{}: storage {} vs fee mass {}",
            p.name(),
            s.fee.mass.storage,
            s.fee.mass.fee_mass
        );
        // the record re-derives the amend, from the signature script (signed) and from the signing plan (built)
        let got = recover_amends(&s.tx).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].output, got[0].input, got[0].covenant_id), (0, 0, A_ID));
        assert_eq!(got[0].order, r.amended);
        assert_eq!(got[0].previous, r.order.state);
        assert_eq!(got[0].value, s.tx.outputs[0].value);
        assert_eq!(recover_amends_planned(&built.tx, &built.plans).unwrap(), got);
        // no genesis: nothing for the placement recovery
        assert!(recover_orders(&s.tx).unwrap().is_empty());
        // an unsigned transaction proves nothing by itself
        assert!(recover_amends(&built.tx).is_err());
        let p3 = decode(&s.tx.payload).unwrap().unwrap();
        assert_eq!(p3.version, payload::PAYLOAD_VERSION);
    }
}

#[test]
fn an_amend_is_a_fraction_of_a_cancel_replace() {
    for p in PROGRAMS {
        let (_, am) = signed("amend.ask", &scenario(p, "amend.ask"));
        let (_, cr) = signed("cancelReplace.ask", &scenario(p, "cancelReplace.ask"));
        let (a, c) = (am.fee.mass.size, cr.fee.mass.size);
        println!("{}: amend {a} B ({} sompi), cancel-replace {c} B ({} sompi)", p.name(), am.fee.fee, cr.fee.fee);
        assert!(a * 2 < c, "{}: amend {a} B vs cancel-replace {c} B", p.name());
    }
}

/// The fill after an amend spends the continuation with the NEW state against the SAME custody UTXO; the old state no
/// longer opens the order, and a fill racing the amend spends the same outpoint (consensus takes one of the two).
#[test]
fn the_amended_order_fills_against_its_untouched_custody() {
    for p in PROGRAMS {
        let a = scenario(p, "amend.ask");
        let r = amend_req(&a).clone();
        let (_, s) = signed("amend.ask", &a);
        let Action::Batch(fill) = scenario(p, "take.ask.partial") else { panic!() };
        let Leg::Ask { order: old, custody, .. } = &fill.legs[0] else { panic!() };
        // the race: a fill of the old order and the amend spend the same outpoint
        assert_eq!(old.utxo.outpoint(), r.order.utxo.outpoint(), "{}", p.name());
        let (_, f0) = signed("take.ask.partial", &Action::Batch(fill.clone()));
        let spent = |t: &SignedTx| t.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect::<Vec<_>>();
        assert!(spent(&f0).contains(&(s.tx.inputs[0].transaction_id, s.tx.inputs[0].index)));
        // the fill of the amended order: the continuation UTXO, the new state, the custody as it was
        let next =
            Utxo { transaction_id: s.tx.id, index: 0, amount: s.tx.outputs[0].value, block_daa_score: 2_000, covenant_id: Some(A_ID) };
        let new_state = ask_of(&r.amended).clone();
        let mut f = fill.clone();
        f.legs[0] = Leg::Ask {
            order: OrderUtxo { utxo: next.clone(), state: new_state },
            custody: custody.clone(),
            amount: 4 * WHOLE,
            t: None,
        };
        let (_, f1) = signed("fill after amend", &Action::Batch(f));
        assert!(
            spent(&f1).contains(&(custody.utxo.transaction_id, custody.utxo.index)),
            "the custody is the one the order held before"
        );
        // the builder spends the continuation as the chain holds it
        assert_eq!(f1.tx.inputs[0].utxo.script_public_key, s.tx.outputs[0].script_public_key);
        // the old state does not open the continuation: its redeem script does not hash to the continuation's script
        let mut g = fill.clone();
        g.legs[0] = Leg::Ask {
            order: OrderUtxo { utxo: next, state: ask_of(&r.order.state).clone() },
            custody: custody.clone(),
            amount: 4 * WHOLE,
            t: None,
        };
        let built = build(&Action::Batch(g)).unwrap();
        let sigs = sign_locally(&built, &keys()).unwrap();
        let mut stale = finalize(&built, &sigs, FinalizeOptions::default()).unwrap();
        stale.tx.inputs[0].utxo.script_public_key = s.tx.outputs[0].script_public_key.clone();
        assert!(validate_signed(&stale).is_err(), "{}: a fill with the replaced state must not validate", p.name());
    }
}

/// Re-encodes the transaction's payload with `records`.
fn with_records(t: &SignedTx, records: Vec<Record>) -> SignedTx {
    let mut t = t.clone();
    t.tx.payload = encode(&records).unwrap();
    t
}

#[test]
fn forged_amend_records_are_refused() {
    let a = scenario(TemplateId::Kcc20Ref, "amend.ask");
    let r = amend_req(&a).clone();
    let (_, s) = signed("amend.ask", &a);
    let prev = r.order.state.clone();
    let rec = |st: &AskState| Record::amend(0, 0, &AnyState::KobAsk(st.clone()), None);
    let base = ask_of(&r.amended).clone();
    let refused = |t: &SignedTx, what: &str| {
        let e = recover_amends(&t.tx).expect_err(what);
        assert!(matches!(e, Error::Payload(_)), "{what}: {e}");
    };
    // a record whose state is not the output's
    refused(&with_records(&s, vec![rec(&AskState { price: 1, ..base.clone() })]), "another price than the output");
    // terms that would move the custody, or another maker / token: refused by the rule, whatever the output says
    for (what, st) in [
        ("amountLeft", AskState { amount_left: base.amount_left - 1, ..base.clone() }),
        ("scale", AskState { scale: base.scale * 10, ..base.clone() }),
        ("maker", AskState { maker: pk(MAKER_B), ..base.clone() }),
        ("token", AskState { token_cov_id: [0x99; 32], ..base.clone() }),
    ] {
        let mut t = with_records(&s, vec![rec(&st)]);
        t.tx.outputs[0].script_public_key = spk_to_string(&AnyState::KobAsk(st.clone()).spk());
        refused(&t, what);
        let mut req = r.clone();
        req.amended = AnyState::KobAsk(st);
        assert!(build(&Action::AmendOrder(req)).is_err(), "the builder refuses an amend of {what}");
    }
    // a second output carrying the order's covenant id
    let mut t = s.clone();
    t.tx.outputs.push(TxOutputJson {
        value: 1_000,
        script_public_key: t.tx.outputs[0].script_public_key.clone(),
        covenant: Some(CovenantJson { authorizing_input: 0, covenant_id: A_ID }),
    });
    refused(&t, "two outputs carry the id");
    // the record names another input / output than the continuation
    refused(&with_records(&s, vec![Record::amend(0, 1, &r.amended, None)]), "no such input");
    refused(&with_records(&s, vec![Record::amend(1, 0, &r.amended, None)]), "no such output");
    // the output is not bound to the order's id by the order input
    let mut t = s.clone();
    t.tx.outputs[0].covenant = Some(CovenantJson { authorizing_input: 0, covenant_id: [0x42; 32] });
    refused(&t, "another covenant id");
    // a record next to a FILL of the order (its continuation carries the id, but the entry is settle, not cancel)
    let (_, f) = signed("take.ask.partial", &scenario(TemplateId::Kcc20Ref, "take.ask.partial"));
    let k = f.tx.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == A_ID)).unwrap();
    let cont = AnyState::KobAsk(AskState { amount_left: base.amount_left - 4 * WHOLE, ..ask_of(&prev).clone() });
    let forged = with_records(&f, vec![Record::amend(k as u16, 0, &cont, None)]);
    let e = recover_amends(&forged.tx).unwrap_err().to_string();
    assert!(e.contains("cancel") || e.contains("amountLeft"), "{e}");
    let Record::Amend { .. } = &decode(&forged.tx.payload).unwrap().unwrap().records[0] else { panic!() };
    let e = verify_amend(&forged.tx, &decode(&forged.tx.payload).unwrap().unwrap().records[0], &prev, "settle").unwrap_err();
    assert!(e.to_string().contains("cancel"), "{e}");
    // only a plain ask or a plain bid is amended in place: a conditional order is not (its cancel-replace restarts it)
    let cb = AnyState::KobCondBid(cond_bid(MAKER_B, TemplateId::Kcc20Ref));
    assert!(encode(&[Record::amend(0, 0, &cb, None)]).is_err());
    // nor does a bid become an ask
    let bid_st = AnyState::KobBid(bid(MAKER_B, P245, TemplateId::Kcc20Ref));
    let Action::CancelOrder(c) = scenario(TemplateId::Kcc20Ref, "cancel.ask") else { panic!() };
    let req = AmendOrder {
        order: c.order.clone(),
        amended: bid_st,
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: fee(),
    };
    assert!(build(&Action::AmendOrder(req)).is_err());
}

/// The change rule of the amend builders: no tiny change output (storage mass above the fee mass); a large change
/// returns to the maker; a bid escrow takes a leftover only while its buying power stays the same.
#[test]
fn amends_leave_no_tiny_change() {
    for p in PROGRAMS {
        // without funding the carrier pays: no change at all
        let (_, s) = signed("amend.ask", &scenario(p, "amend.ask"));
        assert_eq!(s.fee.change_output, None);
        // a large change returns to the maker
        let (_, s) = signed("amend.ask.funded", &scenario(p, "amend.ask.funded"));
        let ci = s.fee.change_output.expect("a 20 KAS funding leaves a change") as usize;
        assert_eq!(s.tx.outputs[ci].script_public_key, spk_to_string(&kob_protocol::script::p2pk_spk(&pk(MAKER_A))));
        assert_eq!(s.tx.outputs[0].value, CARRIER);
        // a tiny one rides on the order
        let (_, s) = signed("amend.ask.tinyChange", &scenario(p, "amend.ask.tinyChange"));
        assert_eq!(s.fee.change_output, None);
        assert_eq!(s.tx.outputs.len(), 1);
        assert_eq!(s.tx.outputs[0].value, CARRIER + KAS / 10 - s.fee.fee);
        assert_eq!(s.fee.fee, s.fee.min_fee);
        assert!(s.fee.mass.storage <= s.fee.mass.fee_mass, "{}", p.name());
        // cancel-replace of an ask: the leftover of the released carriers rides on the replacement (its genesis id
        // re-derived with the new value, the custody owned by that id)
        let a = scenario(p, "cancelReplace.ask");
        let Action::CancelOrder(c) = &a else { panic!() };
        let rep = c.replace.as_ref().unwrap();
        let (_, s) = signed("cancelReplace.ask", &a);
        assert_eq!(s.fee.change_output, None, "{}", p.name());
        assert!(
            s.fee.mass.storage <= s.fee.mass.fee_mass,
            "{}: storage {} vs fee mass {}",
            p.name(),
            s.fee.mass.storage,
            s.fee.mass.fee_mass
        );
        let got = recover_orders(&s.tx).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].value > rep.value, "{}: the replacement took the leftover", p.name());
        let released: u64 = s.tx.inputs.iter().map(|i| i.utxo.amount).sum();
        let out: u64 = s.tx.outputs.iter().map(|o| o.value).sum();
        assert_eq!(released - out, s.fee.fee);
        assert_eq!(s.fee.fee, s.fee.min_fee);
        let custody = got[0].custody.as_ref().expect("custody");
        assert_eq!(custody.state.owner(), got[0].covenant_id);
        // cancel-replace of a bid: the escrow takes the leftover while its buying power stays the same
        let a = scenario(p, "cancelReplace.bid");
        let Action::CancelOrder(c) = &a else { panic!() };
        let rep = c.replace.as_ref().unwrap();
        let (AnyState::KobBid(b) | AnyState::KobBidKron(b)) = &rep.order else { panic!() };
        let power = |v: u64| b.buying_power(v as i64);
        let (_, s) = signed("cancelReplace.bid", &a);
        let got = recover_orders(&s.tx).unwrap();
        assert_eq!(power(got[0].value), power(rep.value), "{}: the escrow buys the same amount", p.name());
        if s.fee.change_output.is_none() {
            assert!(got[0].value > rep.value);
        }
        // a leftover that would buy one more base unit stays a change output
        let mut more = c.clone();
        let r2 = more.replace.as_mut().unwrap();
        // one sompi short of the next base unit: used(power + 1) - 1 of budget, so any leftover would cross it
        let next = b.used(power(r2.value) + 1).unwrap() - 1;
        r2.value = (next + b.delivery_carrier + b.reserve) as u64;
        more.order.utxo.amount = r2.value + 3 * KAS / 10;
        let (_, s) = signed("cancelReplace.bid at the buying-power boundary", &Action::CancelOrder(more.clone()));
        let got = recover_orders(&s.tx).unwrap();
        assert_eq!(power(got[0].value), power(more.replace.as_ref().unwrap().value), "{}", p.name());
        assert!(s.fee.change_output.is_some(), "{}: the leftover would buy more: it stays change", p.name());
    }
}

/// An order whose carrier cannot pay the amend's fee and keep its floor needs funding.
#[test]
fn a_thin_carrier_needs_funding() {
    let a = scenario(TemplateId::Kcc20Ref, "amend.ask");
    let mut r = amend_req(&a).clone();
    r.order.utxo.amount = kob_protocol::build::MIN_AMEND_CARRIER + 1_000;
    let e = build(&Action::AmendOrder(r.clone())).unwrap_err();
    assert!(matches!(e, Error::InsufficientFunds { .. }), "{e}");
    r.funding = vec![key_utxo(12, MAKER_A, 20 * KAS)];
    let (_, s) = signed("thin carrier, funded", &Action::AmendOrder(r));
    assert_eq!(s.tx.outputs[0].value, kob_protocol::build::MIN_AMEND_CARRIER + 1_000);
}

const B_ID: [u8; 32] = [0xb1; 32];

fn bid_of(s: &AnyState) -> &BidState {
    let (AnyState::KobBid(b) | AnyState::KobBidKron(b)) = s else { panic!("not a bid") };
    b
}

/// A plain bid amended in place: the maker's cancel continues its covenant id with the new terms, its escrow pays the
/// fee (or a funding input tops it up), the record re-derives, and the continuation fills under the NEW state.
#[test]
fn a_bid_is_amended_in_place() {
    for p in PROGRAMS {
        let a = scenario(p, "amend.bid");
        let r = amend_req(&a).clone();
        let (built, s) = signed("amend.bid", &a);
        assert_eq!(s.tx.inputs.len(), 1, "{}", p.name());
        assert_eq!(s.tx.inputs[0].utxo.covenant_id, Some(B_ID));
        assert_eq!(s.tx.outputs.len(), 1, "{}: no change output", p.name());
        assert_eq!(s.tx.outputs[0].covenant, Some(CovenantJson { authorizing_input: 0, covenant_id: B_ID }));
        assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&r.amended.spk()));
        // the escrow paid the fee and still funds a minimum fill at the new terms
        assert_eq!(s.tx.outputs[0].value, r.order.utxo.amount - s.fee.fee, "{}", p.name());
        assert!(s.tx.outputs[0].value as i64 >= kob_protocol::build::min_order_value(&r.amended));
        let got = recover_amends(&s.tx).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].output, got[0].input, got[0].covenant_id), (0, 0, B_ID));
        assert_eq!(got[0].order, r.amended);
        assert_eq!(recover_amends_planned(&built.tx, &built.plans).unwrap(), got);
        assert!(recover_orders(&s.tx).unwrap().is_empty());

        // the amended bid fills at its new price from the continuation UTXO
        let Action::Batch(fill) = scenario(p, "take.bid.partial") else { panic!() };
        let next =
            Utxo { transaction_id: s.tx.id, index: 0, amount: s.tx.outputs[0].value, block_daa_score: 2_000, covenant_id: Some(B_ID) };
        let mut f = fill.clone();
        f.legs[0] =
            Leg::Bid { order: OrderUtxo { utxo: next.clone(), state: bid_of(&r.amended).clone() }, amount: 4 * WHOLE, t: None };
        let (_, f1) = signed("fill after bid amend", &Action::Batch(f));
        assert_eq!(f1.tx.inputs[0].utxo.script_public_key, s.tx.outputs[0].script_public_key);
        // the old state no longer opens the order
        let mut g = fill.clone();
        g.legs[0] = Leg::Bid { order: OrderUtxo { utxo: next, state: bid_of(&r.order.state).clone() }, amount: 4 * WHOLE, t: None };
        let built = build(&Action::Batch(g)).unwrap();
        let sigs = sign_locally(&built, &keys()).unwrap();
        let mut stale = finalize(&built, &sigs, FinalizeOptions::default()).unwrap();
        stale.tx.inputs[0].utxo.script_public_key = s.tx.outputs[0].script_public_key.clone();
        assert!(validate_signed(&stale).is_err(), "{}: a fill with the replaced bid state must not validate", p.name());

        // a top-up: the funding input pays the fee and the escrow grows to the requested value
        let a = scenario(p, "amend.bid.funded");
        let r = amend_req(&a).clone();
        let (_, s) = signed("amend.bid.funded", &a);
        assert_eq!(s.tx.outputs[0].covenant, Some(CovenantJson { authorizing_input: 0, covenant_id: B_ID }));
        let v = s.tx.outputs[0].value;
        assert!(v >= r.value.unwrap(), "{}: the escrow holds the topped-up value ({v})", p.name());
        let b = bid_of(&r.amended);
        let power = |v: u64| b.buying_power(v as i64);
        assert_eq!(power(v), power(r.value.unwrap()), "{}: a leftover rides on the escrow only while it buys no more", p.name());
        assert_eq!(recover_amends(&s.tx).unwrap()[0].order, r.amended);
    }
}

#[test]
fn bid_amend_rules() {
    let a = scenario(TemplateId::Kcc20Ref, "amend.bid");
    let r = amend_req(&a).clone();
    let (_, s) = signed("amend.bid", &a);
    let base = bid_of(&r.amended).clone();
    // terms that would change the token or the scale: refused by the rule and by the builder
    for (what, st) in [
        ("scale", BidState { scale: base.scale * 10, ..base.clone() }),
        ("maker", BidState { maker: pk(MAKER_A), ..base.clone() }),
        ("token", BidState { token_cov_id: [0x99; 32], ..base.clone() }),
        ("extension", BidState { extension_commitment: [0x77; 32], ..base.clone() }),
    ] {
        let mut t = with_records(&s, vec![Record::amend(0, 0, &AnyState::KobBid(st.clone()), None)]);
        t.tx.outputs[0].script_public_key = spk_to_string(&AnyState::KobBid(st.clone()).spk());
        let e = recover_amends(&t.tx).expect_err(what);
        assert!(matches!(e, Error::Payload(_)), "{what}: {e}");
        let mut req = r.clone();
        req.amended = AnyState::KobBid(st);
        assert!(build(&Action::AmendOrder(req)).is_err(), "the builder refuses a bid amend of {what}");
    }
    // a price the escrow cannot fund one minimum fill at is refused (unfunded: the floor is that value; funded: the value)
    let mut req = r.clone();
    req.amended = AnyState::KobBid(BidState { price: 30 * KAS as i64, ..base.clone() });
    assert!(matches!(build(&Action::AmendOrder(req.clone())).unwrap_err(), Error::InsufficientFunds { .. }));
    req.funding = vec![key_utxo(23, MAKER_B, 20 * KAS)];
    req.value = Some(r.order.utxo.amount);
    let e = build(&Action::AmendOrder(req)).unwrap_err();
    assert!(e.to_string().contains("one minimum fill"), "{e}");
    // a top-up without funding cannot be paid: the wallet adds a funding input on this error
    let mut req = r.clone();
    req.value = Some(r.order.utxo.amount + 5 * KAS);
    assert!(matches!(build(&Action::AmendOrder(req)).unwrap_err(), Error::InsufficientFunds { .. }));
}
