//! Tests the in-place amend (AMEND record, payload version 4) and the change rule of the amend
//! builders.
//!
//! The maker's `cancel` of a plain ask (SIGHASH_ALL, no output rule) continues the order's covenant
//! id with new terms; the custody, owned by that id, never moves. Every shape is validated through
//! the rusty-kaspa v2.1.0 engine. The record is re-derived trusting nothing
//! (`payload::verify_amend`) and every forgery is refused:
//!
//! * another state than the output;
//! * terms that would move the custody (scale, quantity, token, maker);
//! * a second output carrying the id;
//! * a record next to a fill instead of the maker's cancel;
//! * a kind that is not a plain ask or bid.
//!
//! A plain bid (no custody: its quantity is its escrow) is amended the same way, keeping its token
//! and scale. A mode-1 order's claim (bid-delivery-spec §7) is the same cancel continuing the
//! order with bought = 0: CL1 to CL3.

mod common;

use common::pair::{cond_pair_ask, pair, pv, tutxo, RATE};
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, build_claim_order, Action, AmendOrder, ClaimOrder, Leg, Replacement};
use kob_protocol::payload::{self, decode, encode, recover_amends, recover_amends_planned, recover_orders, verify_amend, Record};
use kob_protocol::state::{
    AnyState, AskState, BidState, CondBidState, CondPairState, IfdAskState, PairState, TokenState, MODE_ACCUMULATE, SCHEME_P2PK,
};
use kob_protocol::tx::{
    finalize, sign_locally, spk_to_string, BuiltTx, CovenantJson, FinalizeOptions, OrderUtxo, SignedTx, TokenUtxo, TxOutputJson, Utxo,
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
        ("extensionCommitment", AskState { extension_commitment: [0x55; 32], ..base.clone() }),
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
        custody: None,
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
        f.legs[0] = Leg::Bid {
            order: OrderUtxo { utxo: next.clone(), state: bid_of(&r.amended).clone() },
            amount: 4 * WHOLE,
            t: None,
            delivery: None,
        };
        let (_, f1) = signed("fill after bid amend", &Action::Batch(f));
        assert_eq!(f1.tx.inputs[0].utxo.script_public_key, s.tx.outputs[0].script_public_key);
        // the old state no longer opens the order
        let mut g = fill.clone();
        g.legs[0] = Leg::Bid {
            order: OrderUtxo { utxo: next, state: bid_of(&r.order.state).clone() },
            amount: 4 * WHOLE,
            t: None,
            delivery: None,
        };
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

/// The ask pins the extension commitment of its custody (`KobAsk.settle` takes only a custody of that commitment) and the
/// custody does not move in an amend: an amend that changes `extensionCommitment` would leave an order that nothing but
/// the maker's cancel can end (no fill, no IOC end, no refund). The term check, the builder and the record's
/// verification (the indexer's rule) refuse it; the same amend that keeps the commitment builds and fills.
#[test]
fn an_ask_amend_keeps_its_extension_commitment() {
    let a = scenario(TemplateId::Kcc20Ref, "amend.ask");
    let r = amend_req(&a).clone();
    let prev = ask_of(&r.order.state).clone();
    let mut next = ask_of(&r.amended).clone();
    assert_ne!(prev.extension_commitment, [0x55; 32]);
    next.extension_commitment = [0x55; 32];
    let other = AnyState::KobAsk(next.clone());
    let e = payload::check_amend_terms(&r.order.state, &other).unwrap_err();
    assert!(e.to_string().contains("extensionCommitment"), "{e}");
    let mut req = r.clone();
    req.amended = other.clone();
    let e = build(&Action::AmendOrder(req)).unwrap_err();
    assert!(e.to_string().contains("extensionCommitment"), "{e}");
    // a transaction carrying such a record (built from the valid amend, its output and record replaced) is refused
    let (_, s) = signed("amend.ask", &a);
    let mut t = with_records(&s, vec![Record::amend(0, 0, &other, None)]);
    t.tx.outputs[0].script_public_key = spk_to_string(&other.spk());
    let e = recover_amends(&t.tx).unwrap_err();
    assert!(e.to_string().contains("extensionCommitment"), "{e}");
    // the amend that keeps it is accepted
    payload::check_amend_terms(&r.order.state, &r.amended).unwrap();
}

// ---------------------------------------------------------------- claims (bid-delivery-spec §7, CL1 to CL3)

/// The KCC-20 programs (modes 1 and 2 exist for KCC-20 only), from the 3 / 3 reference to the largest.
fn kcc20() -> Vec<TemplateId> {
    common::PROGRAMS.into_iter().filter(|p| p.family() == kob_protocol::Family::Kcc20).collect()
}

fn engine(name: &str, built: &BuiltTx) -> SignedTx {
    let sigs = sign_locally(built, &keys()).unwrap();
    let s = finalize(built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap_or_else(|e| panic!("{name}: finalize: {e}"));
    let v = validate_signed(&s).unwrap_or_else(|e| panic!("{name}: engine rejected: {e}\nroles {:?}", built.roles));
    assert!(v.mass.within_block_limits(), "{name}: {:?}", v.mass);
    s
}

/// A mode-1 bid (covenant B_ID) holding 4 whole tokens in its bought-custody, and that custody.
fn held_bid(p: TemplateId) -> (OrderUtxo<AnyState>, TokenUtxo) {
    let a4 = mode_bid(bid(MAKER_B, P245, p), MODE_ACCUMULATE, 4 * WHOLE);
    let v = a4.escrow(10 * WHOLE, 1).unwrap() as u64;
    (order(20, v, B_ID, 1_000, AnyState::KobBid(a4)), bought_custody(23, 4 * WHOLE, B_ID, 1_500))
}

fn wallet(tag: u8, n: i64, maker: u8) -> TokenUtxo {
    tok(tag, n * WHOLE, pk(maker), SCHEME_P2PK, 1_000)
}

fn claim(order: OrderUtxo<AnyState>, custody: TokenUtxo, tokens: Vec<TokenUtxo>, place: Option<AnyState>, end: bool) -> ClaimOrder {
    let maker = order.state.maker();
    let key = (1..=40u8).find(|k| pk(*k) == maker).unwrap();
    ClaimOrder {
        order,
        bought_custody: custody,
        tokens,
        place: place.map(|o| Replacement { order: o, value: CARRIER, token_carrier: None, deadline: None }),
        end,
        value: None,
        token_carrier: None,
        funding: vec![key_utxo(30, key, 100 * KAS)],
        change: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    }
}

fn try_claim(r: &ClaimOrder) -> Result<BuiltTx, Error> {
    build_claim_order(r, &kob_protocol::budget::lookup)
}

/// The outputs bound to covenant `c`.
fn bound(s: &SignedTx, c: [u8; 32]) -> Vec<usize> {
    s.tx.outputs.iter().enumerate().filter(|(_, o)| o.covenant.as_ref().is_some_and(|x| x.covenant_id == c)).map(|(j, _)| j).collect()
}

/// Whether an output holds token state `st` of program `p` with `value` sompi (any value when `None`).
fn holds(s: &SignedTx, p: TemplateId, st: &TokenState, value: Option<u64>) -> bool {
    let spk = spk_to_string(&st.spk_with(kob_protocol::artifacts::token_template(p)));
    s.tx.outputs.iter().any(|o| o.script_public_key == spk && value.is_none_or(|v| o.value == v))
}

/// `n` base units of program `p` as a plain UTXO of `maker`.
fn plain(p: TemplateId, n: i64, maker: u8) -> TokenState {
    TokenState::user(p, n, pk(maker), EXT)
}

/// CL1 claim, CL2 claim and merge (up to the program's token inputs), CL3 claim and place (the order continues or ends) of a
/// `KobBid` on every KCC-20 program, in the engine; the claimed order then fills as a fresh mode-1 order.
#[test]
fn claim_shapes_of_a_bid_run_in_the_engine_on_every_kcc20_program() {
    for p in kcc20() {
        let max_in = p.token_slots().unwrap().0.min(kob_protocol::Family::Kcc20.max_tok_in());
        let (o, c) = held_bid(p);
        let b = bid_of(&o.state).clone();
        let after = AnyState::KobBid(BidState { bought: 0, ..b.clone() });
        let floor = o.utxo.amount + b.delivery_carrier as u64;
        let label = |what: &str| format!("{what}@{}", p.name());

        // CL1: the continuation at output 0 (bought = 0, one more carrier), the custody to the maker with its KAS
        let s = engine(&label("CL1"), &try_claim(&claim(o.clone(), c.clone(), vec![], None, false)).unwrap());
        println!("{}: {} B, fee {} sompi", label("CL1"), s.fee.mass.size, s.fee.fee);
        assert_eq!(bound(&s, B_ID), vec![0], "{}", p.name());
        assert_eq!(s.tx.outputs[0].covenant, Some(CovenantJson { authorizing_input: 0, covenant_id: B_ID }));
        assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&after.spk()));
        assert!(s.tx.outputs[0].value >= floor, "{}", p.name());
        assert!(holds(&s, p, &plain(p, 4 * WHOLE, MAKER_B), Some(c.utxo.amount)), "{}", p.name());
        assert_eq!(s.tx.inputs[1].transaction_id, c.utxo.transaction_id, "the custody leads its token");
        assert!(recover_orders(&s.tx).unwrap().is_empty() && s.tx.payload.is_empty(), "a claim needs no record");
        // the claimed order fills as a fresh mode-1 order: its first fill pays a new custody's carrier
        let next = s.tx.outputs[0].value;
        let mut fill = take_bid_valued(BidState { bought: 0, ..b.clone() }, 3, next, None);
        let Leg::Bid { order: fo, .. } = &mut fill.legs[0] else { panic!() };
        fo.utxo = Utxo { transaction_id: s.tx.id, index: 0, amount: next, block_daa_score: 2_000, covenant_id: Some(B_ID) };
        let f = engine(&label("fill after CL1"), &build(&Action::Batch(fill)).unwrap());
        assert!(holds(&f, p, &TokenState::custody(p.family(), 3 * WHOLE, B_ID, EXT), None), "{}: a new custody", p.name());

        // CL2: the custody and max_in - 1 wallet UTXOs into one plain UTXO with all their KAS
        let w: Vec<TokenUtxo> = (0..max_in - 1).map(|k| wallet(40 + k as u8, 1, MAKER_B)).collect();
        let s = engine(&label("CL2"), &try_claim(&claim(o.clone(), c.clone(), w.clone(), None, false)).unwrap());
        println!(
            "{} ({} wallet UTXOs): {} B, compute {}, fee {} sompi",
            label("CL2"),
            w.len(),
            s.fee.mass.size,
            s.fee.mass.compute,
            s.fee.fee
        );
        let kas = c.utxo.amount + w.iter().map(|t| t.utxo.amount).sum::<u64>();
        assert!(holds(&s, p, &plain(p, (4 + w.len() as i64) * WHOLE, MAKER_B), Some(kas)), "{}", p.name());
        assert_eq!(bound(&s, B_ID), vec![0]);
        assert_eq!(s.tx.inputs.len(), 1 + max_in + 1, "{}: order, custody, wallet UTXOs, funding", p.name());
        // slot refusal: one wallet UTXO more is a token input the program does not take
        let mut more = w.clone();
        more.push(wallet(60, 1, MAKER_B));
        let e = try_claim(&claim(o.clone(), c.clone(), more, None, false)).unwrap_err().to_string();
        assert!(e.contains("wallet UTXOs"), "{}: {e}", p.name());

        // CL3: custody + 1 wallet whole: a 3-whole ask of the maker, 2 whole back, the order continuing or ending
        for end in [false, true] {
            let ask3 = AnyState::KobAsk(ask_n(MAKER_B, P250, p, 3));
            let r = claim(o.clone(), c.clone(), vec![wallet(41, 1, MAKER_B)], Some(ask3.clone()), end);
            let s = engine(&label(if end { "CL3 end" } else { "CL3" }), &try_claim(&r).unwrap());
            println!("{}: {} B, fee {} sompi", label(if end { "CL3 end" } else { "CL3" }), s.fee.mass.size, s.fee.fee);
            let got = recover_orders(&s.tx).unwrap();
            assert_eq!(got.len(), 1, "{}", p.name());
            assert_eq!(got[0].order, ask3);
            let cu = got[0].custody.as_ref().expect("the placed custody");
            assert_eq!(cu.state, TokenState::custody(p.family(), 3 * WHOLE, got[0].covenant_id, EXT));
            assert!(holds(&s, p, &plain(p, 2 * WHOLE, MAKER_B), None), "{}: the rest to the maker", p.name());
            if end {
                assert!(bound(&s, B_ID).is_empty(), "{}: the order ends", p.name());
                let change = s.tx.outputs[s.fee.change_output.unwrap() as usize].value;
                assert!(change > o.utxo.amount, "{}: the escrow goes back with the change", p.name());
            } else {
                assert_eq!(bound(&s, B_ID), vec![0]);
                assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&after.spk()));
                assert_eq!(got[0].output, 1);
            }
        }
    }
}

/// CL1 and CL3 of a `KobCondBid` on every KCC-20 program.
#[test]
fn claim_shapes_of_a_cond_bid_run_in_the_engine() {
    const E_ID: [u8; 32] = [0xe1; 32];
    for p in kcc20() {
        let c4 = CondBidState { amount_left: 6 * WHOLE, ..mode_cond_bid(cond_bid(MAKER_B, p), MODE_ACCUMULATE, 4 * WHOLE) };
        let o = order(40, c4.escrow(1).unwrap() as u64, E_ID, 2_000, AnyState::KobCondBid(c4.clone()));
        let c = bought_custody(43, 4 * WHOLE, E_ID, 2_500);
        let after = AnyState::KobCondBid(CondBidState { bought: 0, ..c4.clone() });
        let s = engine(&format!("cond CL1@{}", p.name()), &try_claim(&claim(o.clone(), c.clone(), vec![], None, false)).unwrap());
        assert_eq!(bound(&s, E_ID), vec![0]);
        assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&after.spk()));
        assert!(s.tx.outputs[0].value >= o.utxo.amount + c4.delivery_carrier as u64);
        assert!(holds(&s, p, &plain(p, 4 * WHOLE, MAKER_B), Some(c.utxo.amount)));
        let place = Some(AnyState::KobAsk(ask_n(MAKER_B, P250, p, 4)));
        let s = engine(&format!("cond CL3 end@{}", p.name()), &try_claim(&claim(o, c, vec![], place, true)).unwrap());
        assert!(bound(&s, E_ID).is_empty());
        assert_eq!(recover_orders(&s.tx).unwrap()[0].custody.as_ref().unwrap().state.amount(), 4 * WHOLE);
    }
}

/// CL1 and CL2 of `KobPair` and `KobCondPair` (an ask: sells A, buys B into its bought-custody): the sell custody of A is not
/// an input, and the order continues with bought = 0.
#[test]
fn claim_shapes_of_a_pair_order_keep_its_sell_custody() {
    const X: [u8; 32] = [0x83; 32];
    for p in [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8] {
        let kp = PairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..pair(MAKER_A, true, p, p, RATE) };
        let kc = CondPairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..cond_pair_ask(MAKER_A, p, p) };
        let cases = [
            ("pair", AnyState::KobPair(kp.clone()), AnyState::KobPair(PairState { bought: 0, ..kp.clone() }), kp.delivery_carrier),
            (
                "condPair",
                AnyState::KobCondPair(kc.clone()),
                AnyState::KobCondPair(CondPairState { bought: 0, ..kc.clone() }),
                kc.delivery_carrier,
            ),
        ];
        for (name, st, after, carrier) in cases {
            let o = order(70, pv(), X, 1_000, st.clone());
            let c = tutxo(p, TOKEN_B, 60, 3 * WHOLE, X, true);
            for n in [0, 1] {
                let w: Vec<TokenUtxo> = (0..n).map(|k| tutxo(p, TOKEN_B, 61 + k as u8, WHOLE, pk(MAKER_A), false)).collect();
                let label = format!("{name} CL{}@{}", n + 1, p.name());
                let s = engine(&label, &try_claim(&claim(o.clone(), c.clone(), w, None, false)).unwrap());
                assert_eq!(bound(&s, X), vec![0], "{label}");
                assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&after.spk()), "{label}");
                assert!(s.tx.outputs[0].value >= pv() + carrier as u64, "{label}");
                assert!(s.tx.inputs.iter().all(|i| i.utxo.covenant_id != Some(TOKEN_COV)), "{label}: the sell custody stays");
                assert!(holds(&s, p, &TokenState::user(p, (3 + n) * WHOLE, pk(MAKER_A), EXT), None), "{label}");
            }
            // a pair order ends only by its cancel, which returns its sell custody too
            let r = claim(o, c, vec![], Some(AnyState::KobAsk(ask_n(MAKER_A, P250, p, 1))), true);
            assert!(try_claim(&r).unwrap_err().to_string().contains("CancelOrder"), "{name}");
        }
    }
}

/// Every claim rule with its pair: the valid request builds, the one that breaks the rule is refused.
#[test]
fn claim_refusals() {
    let p = TemplateId::Kcc20Ref;
    let (o, c) = held_bid(p);
    let base = || claim(o.clone(), c.clone(), vec![], None, false);
    let refused = |r: &ClaimOrder, what: &str, needle: &str| {
        let e = try_claim(r).expect_err(what).to_string();
        assert!(e.contains(needle), "{what}: {e}");
    };
    try_claim(&base()).unwrap();
    // the order holds a bought-custody: mode 1 with bought > 0
    for (what, st) in [("mode 1, bought 0", mode_bid(bid(MAKER_B, P245, p), MODE_ACCUMULATE, 0)), ("mode 0", bid(MAKER_B, P245, p))] {
        let mut r = base();
        r.order.state = AnyState::KobBid(st);
        refused(&r, what, "no bought-custody");
    }
    // the custody is exactly the order's: amount, owner, commitment
    let mut r = base();
    r.bought_custody = bought_custody(23, 4 * WHOLE - 1, B_ID, 1_500);
    refused(&r, "a look-alike of another amount", "custody holds");
    let mut r = base();
    r.bought_custody = bought_custody(23, 4 * WHOLE, [0x5a; 32], 1_500);
    refused(&r, "a custody of another order", "not owned by its order");
    let mut r = base();
    if let TokenState::Kcc20(k) = &mut r.bought_custody.state {
        k.extension_commitment = [0x55; 32];
    }
    refused(&r, "a custody of another commitment", "another extension commitment than the order pins");
    // wallet tokens: the maker's plain UTXOs of the bought token's commitment
    let mut r = base();
    r.tokens = vec![wallet(41, 1, MAKER_A)];
    refused(&r, "another key's tokens", "wallet tokens");
    let mut r = base();
    r.tokens = vec![recv_tok(41, WHOLE, pk(MAKER_B), RECV_T, 1_000)];
    refused(&r, "a receive UTXO", "wallet tokens");
    let mut r = base();
    let mut w = wallet(41, 1, MAKER_B);
    if let TokenState::Kcc20(k) = &mut w.state {
        k.extension_commitment = [0x55; 32];
    }
    r.tokens = vec![w];
    refused(&r, "another commitment", "wallet tokens");
    let mut r = base();
    r.tokens = vec![wallet(41, 1, MAKER_B)];
    try_claim(&r).unwrap();
    // the continuation holds at least the order's value plus one deliveryCarrier
    let floor = o.utxo.amount + bid_of(&o.state).delivery_carrier as u64;
    let mut r = base();
    r.value = Some(floor - 1);
    refused(&r, "a continuation one sompi short", "at least");
    r.value = Some(floor + KAS);
    let s = engine("a larger continuation", &try_claim(&r).unwrap());
    assert!(s.tx.outputs[0].value >= floor + KAS);
    // funding
    let mut r = base();
    r.funding = vec![];
    refused(&r, "no funding", "funding");
    // only a claim and place of a bid ends the order
    refused(&claim(o.clone(), c.clone(), vec![], None, true), "an ending claim without a placement", "CancelOrder");
    // the placed order: the maker's, holding the claimed token, of its commitment, fillable, within the claimed units
    let placed = |st: AnyState| claim(o.clone(), c.clone(), vec![], Some(st), false);
    try_claim(&placed(AnyState::KobAsk(ask_n(MAKER_B, P250, p, 4)))).unwrap();
    refused(&placed(AnyState::KobAsk(ask_n(MAKER_A, P250, p, 4))), "another maker's order", "maker's");
    refused(&placed(AnyState::KobBid(bid(MAKER_B, P245, p))), "a bid (no custody)", "custody");
    refused(&placed(AnyState::KobAsk(ask_n(MAKER_B, P250, p, 5))), "more than the claim holds", "claim holds");
    let mut a = ask_n(MAKER_B, P250, p, 4);
    a.extension_commitment = [0x55; 32];
    refused(&placed(AnyState::KobAsk(a)), "an ask pinning another commitment", "pins another");
    let ia = IfdAskState { amount_left: 4 * WHOLE, ..ifd_ask(MAKER_B, p) };
    let min = kob_protocol::build::min_order_value(&AnyState::KobIfdAsk(ia.clone())) as u64;
    let mut r = placed(AnyState::KobIfdAsk(ia));
    r.place.as_mut().unwrap().value = min - 1;
    refused(&r, "a placed order below its fillable value", "fillable");
    r.place.as_mut().unwrap().value = min;
    engine("a placed sell-first entry", &try_claim(&r).unwrap());
}
