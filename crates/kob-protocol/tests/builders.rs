//! Every builder's output validated through the rusty-kaspa v2.1.0 engine.
//!
//! For each scenario (all builder shapes on every supported token program, both families: the
//! KCC-20 programs and the two real KRON programs): build with the
//! committed budget table, sign with the fixture keys, finalize, then validate the signed
//! transaction the way consensus does (enforced compute budgets, covenant context, storage-mass
//! commitment, fee floor, sig-op cap, block limits). The tightened budgets must be minimal
//! (budget - 1 rejects) and the table must cover them with at most one unit to spare.

mod common;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::payload::{decode, recover_orders, Record};
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::Family;

/// The two fixture sets every family-generic test runs on: the KCC-20 reference program and the
/// common KRON program.
fn families() -> [(&'static str, Vec<(String, Action)>); 2] {
    [("kcc20", common::scenarios()), ("kron", common::scenarios_on(TemplateId::KronToken2433))]
}
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, InputSignature, SignedTx};
use kob_protocol::verify::{execute, validate_signed};
use kob_protocol::Error;

fn run(name: &str, action: &Action) -> SignedTx {
    let keys = common::keys();
    let built = build(action).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, &keys).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap_or_else(|e| panic!("{name}: finalize: {e}"));
    let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{name}: engine rejected: {e}"));
    assert_eq!(signed.tx.id, built.tx.id, "{name}: signatures must not change the transaction id");
    assert!(v.fee >= v.min_fee);

    // Exact budgets: tighten, validate, and check budget - 1 rejects on every metered input.
    let tight = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    let vt = validate_signed(&tight).unwrap_or_else(|e| panic!("{name}: tightened: {e}"));
    for (i, (table, exact)) in v.budgets.iter().zip(&vt.budgets).enumerate() {
        assert!(table >= exact && *table <= exact + 1, "{name}: input {i} ({}): table {table} vs exact {exact}", built.roles[i]);
        if *exact > 0 {
            let (mut tx, entries) = tight.tx.to_tx().unwrap();
            tx.inputs[i].compute_commit = kaspa_consensus_core::mass::ComputeBudget::from(exact - 1).into();
            let r = execute(&tx, &entries, true).unwrap();
            assert!(r[i].is_err(), "{name}: input {i}: budget - 1 unexpectedly passed");
        }
    }
    signed
}

#[test]
fn every_builder_shape_validates_on_every_token_program() {
    let mut n = 0;
    for p in common::PROGRAMS {
        for (name, action) in common::scenarios_on(p) {
            run(&format!("{name}@{}", p.name()), &action);
            n += 1;
        }
    }
    println!("validated {n} transactions");
}

#[test]
fn creation_payload_recovers_the_order_and_its_custody() {
    for (fam, scenarios) in families() {
        creation_payload_recovers(fam, scenarios);
    }
}

fn creation_payload_recovers(fam: &str, scenarios: Vec<(String, Action)>) {
    for (name, action) in scenarios {
        let name = format!("{fam}/{name}");
        let Action::CreateOrder(req) = &action else { continue };
        let signed = run(&name, &action);
        let p = decode(&signed.tx.payload).unwrap().expect("KOB1 payload");
        assert!(matches!(p.records[0], Record::Order { output: 0, .. }), "{name}");
        let rec = recover_orders(&signed.tx).unwrap_or_else(|e| panic!("{name}: recover: {e}"));
        assert_eq!(rec.len(), 1, "{name}");
        assert_eq!(rec[0].order, req.order, "{name}");
        assert_eq!(rec[0].value, req.value);
        assert_eq!(rec[0].deadline, req.deadline, "{name}");
        let out0 = signed.tx.outputs[0].covenant.as_ref().unwrap();
        assert_eq!(out0.covenant_id, rec[0].covenant_id);
        if req.order.holds_tokens() {
            let c = rec[0].custody.as_ref().expect("custody");
            let amount = req.order.custody_amount().unwrap();
            assert_eq!(c.state, TokenState::custody(req.order.family(), amount, rec[0].covenant_id, c.state.extension()));
            assert!(c.state.is_covenant_owned());
        } else {
            assert!(rec[0].custody.is_none());
        }
    }
}

/// `recover_orders` (and so the indexer and the wallet) only lists an order whose genesis
/// group is the order output alone. Every builder must create exactly that: placements, cancel-replace,
/// the exits a fill creates (if-done, repeat) and pair orders (v2.6: there is no receipt genesis group).
#[test]
fn every_order_genesis_is_a_one_output_group() {
    let mut orders = 0;
    for p in common::PROGRAMS {
        for (name, action) in common::scenarios_on(p) {
            let built = build(&action).unwrap_or_else(|e| panic!("{name}@{}: build: {e}", p.name()));
            for c in &built.covenants {
                let bound =
                    built.tx.outputs.iter().filter(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == c.covenant_id)).count();
                assert_eq!(bound, c.outputs.len(), "{name}@{}: outputs bound to a new covenant id", p.name());
                match c.template {
                    Some(t) if !t.is_token() => {
                        assert_eq!(
                            c.outputs.len(),
                            1,
                            "{name}@{}: {} genesis group is not the order output alone",
                            p.name(),
                            t.name()
                        );
                        orders += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    println!("{orders} order geneses (one output each)");
    assert!(orders > 0);
}

/// A sibling output bound to the order's covenant id refuses the placement, both when it is
/// in the order's genesis group (the covenant id re-derived over the two outputs) and when it names
/// another authorising input.
#[test]
fn recovery_rejects_a_sibling_of_the_order_covenant() {
    use kaspa_consensus_core::hashing::covenant_id::covenant_id;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};
    use kob_protocol::tx::{spk_from_string, CovenantJson, TxOutputJson};
    for (fam, scenarios) in families() {
        for (name, action) in scenarios {
            let Action::CreateOrder(_) = &action else { continue };
            let name = format!("{fam}/{name}");
            let signed = run(&name, &action);
            let rec = recover_orders(&signed.tx).unwrap_or_else(|e| panic!("{name}: recover: {e}"));
            let (o, old) = (rec[0].output as usize, rec[0].covenant_id);
            let binding = signed.tx.outputs[o].covenant.clone().unwrap();
            let extra = |c| TxOutputJson {
                value: 50_000_000,
                script_public_key: signed.tx.outputs[o].script_public_key.clone(),
                covenant: Some(c),
            };
            // in the group: re-derive the id over both outputs and rebind everything that carried the old id
            let mut tx = signed.tx.clone();
            tx.outputs.push(extra(binding.clone()));
            let inp = &tx.inputs[binding.authorizing_input as usize];
            let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes(inp.transaction_id), index: inp.index };
            let group: Vec<(u32, TransactionOutput)> = tx
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, x)| x.covenant.as_ref() == Some(&binding))
                .map(|(k, x)| {
                    (
                        k as u32,
                        TransactionOutput {
                            value: x.value,
                            script_public_key: spk_from_string(&x.script_public_key).unwrap(),
                            covenant: None,
                        },
                    )
                })
                .collect();
            let new = covenant_id(op, group.iter().map(|(k, x)| (*k, x))).as_bytes();
            for x in tx.outputs.iter_mut() {
                if let Some(c) = x.covenant.as_mut().filter(|c| c.covenant_id == old) {
                    c.covenant_id = new;
                }
            }
            if let Some(c) = &rec[0].custody {
                let tt = kob_protocol::artifacts::token_template_by_hash(&rec[0].order.token_tpl_hash().unwrap()).unwrap();
                let st = TokenState::custody(rec[0].order.family(), c.state.amount(), new, c.state.extension());
                tx.outputs[c.output as usize].script_public_key = kob_protocol::tx::spk_to_string(&st.spk_with(tt));
            }
            let e = recover_orders(&tx).expect_err(&name).to_string();
            assert!(e.contains("genesis group has other outputs"), "{name}: {e}");
            // outside the group: same covenant id, another authorising input
            if let Some(other) = (0..signed.tx.inputs.len() as u16).find(|&i| i != binding.authorizing_input) {
                let mut tx = signed.tx.clone();
                tx.outputs.push(extra(CovenantJson { authorizing_input: other, covenant_id: old }));
                let e = recover_orders(&tx).expect_err(&name).to_string();
                assert!(e.contains("genesis group has other outputs"), "{name}: {e}");
            }
        }
    }
}

#[test]
fn recovery_rejects_a_forged_record() {
    for (fam, scenarios) in families() {
        forged_record_is_rejected(fam, scenarios);
    }
}

fn forged_record_is_rejected(fam: &str, scenarios: Vec<(String, Action)>) {
    let (name, action) = scenarios.into_iter().find(|(n, _)| n == "create.ask").unwrap();
    let signed = run(&format!("{fam}/{name}"), &action);
    let mut tx = signed.tx.clone();
    // Claim a different price for the same output: the P2SH no longer matches.
    let Some(mut p) = decode(&tx.payload).unwrap() else { panic!() };
    if let Record::Order { state, template, .. } = &mut p.records[0] {
        let mut s = AnyState::decode(*template, state).unwrap();
        if let AnyState::KobAsk(a) | AnyState::KobAskKron(a) = &mut s {
            a.price -= 1;
        }
        *state = s.encode();
    }
    tx.payload = kob_protocol::payload::encode(&p.records).unwrap();
    assert!(recover_orders(&tx).is_err());
}

#[test]
fn signatures_are_checked_before_assembly() {
    let (_, action) = common::scenarios().into_iter().find(|(n, _)| n == "cancel.ask").unwrap();
    let built = build(&action).unwrap();
    let keys = common::keys();
    let good = sign_locally(&built, &keys).unwrap();
    // Wrong key.
    let bad = vec![InputSignature {
        input_index: 0,
        signature: kob_protocol::tx::sign_digest(&common::sk(9), &built.sign[0].sighash).unwrap(),
    }];
    assert!(matches!(finalize(&built, &bad, FinalizeOptions::default()), Err(Error::Signature { .. })));
    // SIGHASH_ALL|ANYONECANPAY is refused.
    let mut s = good[0].signature.clone();
    s[64] = 0x81;
    let bad = vec![InputSignature { input_index: 0, signature: s }];
    assert!(finalize(&built, &bad, FinalizeOptions::default()).is_err());
    // 64-byte and KasWare's 66-byte push(sig65) forms are accepted.
    let s64 = vec![InputSignature { input_index: 0, signature: good[0].signature[..64].to_vec() }];
    let a = finalize(&built, &s64, FinalizeOptions::default()).unwrap();
    let push = [vec![0x41], good[0].signature.clone()].concat();
    let s66 = vec![InputSignature { input_index: 0, signature: push }];
    let b = finalize(&built, &s66, FinalizeOptions::default()).unwrap();
    assert_eq!(a, b);
    // Missing signature.
    assert!(finalize(&built, &[], FinalizeOptions::default()).is_err());
}

#[test]
fn tampered_outputs_are_rejected_by_the_covenants() {
    for (fam, scenarios) in families() {
        tampered_outputs(fam, &scenarios);
    }
}

fn tampered_outputs(fam: &str, scenarios: &[(String, Action)]) {
    // The builders pay makers exactly their bound: one sompi less must fail in the engine.
    for target in [
        "take.ask.partial",
        "match.cross.1x2",
        "ifd.bid.partial",
        "cond.bid.stop.trigger",
        "take.ask.market",
        "take.bid.market",
        "cond.ask.stop.auction",
        "ifd.bid.stopEntry.auction",
        "rpt.bid.merge.partial",
        "rpt.ask.merge.partial",
    ] {
        let (_, action) = scenarios.iter().find(|(n, _)| n == target).unwrap();
        let built = build(action).unwrap();
        let sigs = sign_locally(&built, &common::keys()).unwrap();
        let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap();
        let (mut tx, entries) = signed.tx.to_tx().unwrap();
        let victim = if target == "match.cross.1x2" { 1 } else { 0 };
        tx.outputs[victim].value -= 1;
        let r = execute(&tx, &entries, true).unwrap();
        assert!(r[victim].is_err(), "{fam}/{target}: underpaid positional output accepted");
    }
}

#[test]
fn builders_refuse_invalid_requests() {
    for (_, scenarios) in families() {
        refuse_invalid_requests(scenarios);
    }
}

fn refuse_invalid_requests(scenarios: Vec<(String, Action)>) {
    let find = |n: &str| scenarios.iter().find(|(x, _)| x == n).unwrap().1.clone();
    let expect_invalid = |a: Action, what: &str| match build(&a) {
        Err(Error::Invalid(_)) | Err(Error::InsufficientFunds { .. }) => {}
        other => panic!("{what}: expected a refusal, got {other:?}"),
    };
    // FOK filled partially.
    let Action::Batch(mut b) = find("take.ask.fok") else { panic!() };
    if let kob_protocol::build::Leg::Ask { amount, .. } = &mut b.legs[0] {
        *amount = 4 * common::WHOLE;
    }
    expect_invalid(Action::Batch(b), "FOK partial");
    // More than the ask holds.
    let Action::Batch(mut b) = find("take.ask.partial") else { panic!() };
    if let kob_protocol::build::Leg::Ask { amount, .. } = &mut b.legs[0] {
        *amount = 10 * common::WHOLE + 1;
    }
    expect_invalid(Action::Batch(b), "over-fill");
    // Stop leg without trigger evidence on an unarmed order.
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::CondAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = None;
    }
    expect_invalid(Action::Batch(b), "untriggered stop");
    // Evidence exposed for less than minRestDaa before the lock time (R = 50 DAA).
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::Ask { order, custody, .. } = &mut b.legs[1] {
        order.utxo.block_daa_score = common::NOW - 49;
        custody.utxo.block_daa_score = common::NOW - 49;
    }
    expect_invalid(Action::Batch(b), "evidence too young");
    // The evidence exactly R old is accepted.
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::Ask { order, custody, .. } = &mut b.legs[1] {
        order.utxo.block_daa_score = common::NOW - 50;
        custody.utxo.block_daa_score = common::NOW - 50;
    }
    build(&Action::Batch(b)).expect("evidence exposed exactly minRestDaa");
    // Evidence on the wrong side (a resting bid for a sell stop), a decaying ask, a leg that is not plain.
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::CondAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = Some(0);
    }
    expect_invalid(Action::Batch(b), "own leg as evidence");
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::Ask { order, .. } = &mut b.legs[1] {
        order.state.slope = 1;
        order.state.price_end = 1;
    }
    expect_invalid(Action::Batch(b), "decaying evidence");
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::Ask { order, .. } = &mut b.legs[1] {
        order.state.price = 200_000_001;
    }
    expect_invalid(Action::Batch(b), "evidence above the stop");
    let Action::Batch(mut b) = find("cond.ask.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::CondAsk { order, .. } = &mut b.legs[0] {
        order.state.min_touch = common::WHOLE + 1;
    }
    expect_invalid(Action::Batch(b), "evidence below minTouch");
    let Action::Batch(mut b) = find("cond.bid.stop.trigger") else { panic!() };
    if let kob_protocol::build::Leg::Bid { order, .. } = &mut b.legs[1] {
        order.state.price = 299_999_999;
    }
    expect_invalid(Action::Batch(b), "buy-stop evidence below the stop");
    // Refund before expiry.
    let Action::RefundOrder(mut r) = find("refund.bid") else { panic!() };
    r.lock_time -= 1;
    expect_invalid(Action::RefundOrder(r), "early refund");
    // Maker tokens short of amountLeft.
    let Action::CreateOrder(mut c) = find("create.ask") else { panic!() };
    c.tokens[0].state = c.tokens[0].state.with_amount(10 * common::WHOLE - 1);
    expect_invalid(Action::CreateOrder(c), "custody short of amountLeft");
    // Negative tip.
    let Action::CreateOrder(mut c) = find("create.bid") else { panic!() };
    if let AnyState::KobBid(b) | AnyState::KobBidKron(b) = &mut c.order {
        b.tip = -1;
    }
    expect_invalid(Action::CreateOrder(c), "negative tip");
    // Too many token slots on a 3/3 token (4 asks in one sweep; KRON: 5 token inputs on a 4-slot program).
    let Action::Batch(b) = find("match.cross.1x2") else { panic!() };
    let mut legs = b.legs.clone();
    let kron = matches!(&legs[1], kob_protocol::build::Leg::Ask { custody, .. } if custody.state.family() == Family::Kron);
    for k in 0..if kron { 4u8 } else { 2u8 } {
        if let kob_protocol::build::Leg::Ask { order, custody, .. } = &b.legs[1] {
            let mut o = order.clone();
            let mut c = custody.clone();
            o.utxo.transaction_id = [0x90 + k; 32];
            c.utxo.transaction_id = [0x92 + k; 32];
            o.utxo.covenant_id = Some([0x94 + k; 32]);
            common::set_owner(&mut c.state, [0x94 + k; 32]);
            legs.push(kob_protocol::build::Leg::Ask { order: o, custody: c, amount: 5 * common::WHOLE, t: None });
        }
    }
    let mut b2 = b.clone();
    b2.legs = legs;
    expect_invalid(Action::Batch(b2), "slot limit");
}

/// Merged entries get back exactly what their covenants require: one sompi less on the entry's
/// continuation is rejected by the entry or by its exit.
#[test]
fn merged_entries_are_restored_exactly() {
    for (fam, scenarios) in families() {
        merged_entries_restored(fam, &scenarios);
    }
}

fn merged_entries_restored(fam: &str, scenarios: &[(String, Action)]) {
    for (target, entry_cov) in [
        ("rpt.bid.merge.partial", common::cov(0xd1)),
        ("rpt.bid.merge.sellOut", common::cov(0xd1)),
        ("rpt.bid.merge.emptyEntry", common::cov(0xd1)),
        ("rpt.ask.merge.partial", common::cov(0xf1)),
        ("rpt.ask.merge.newCustody", common::cov(0xf1)),
    ] {
        let (_, action) = scenarios.iter().find(|(n, _)| n == target).unwrap();
        let built = build(action).unwrap();
        let sigs = sign_locally(&built, &common::keys()).unwrap();
        let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap();
        let (mut tx, entries) = signed.tx.to_tx().unwrap();
        let o = signed.tx.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == entry_cov)).unwrap();
        tx.outputs[o].value -= 1;
        let r = execute(&tx, &entries, true).unwrap();
        assert!(r[0].is_err() || r[1].is_err(), "{fam}/{target}: short entry continuation accepted");
    }
}

/// The builders refuse the v2.3 / v2.4 shapes the covenants reject (strays, oversize token
/// input sets, booked take-profits without their merge, merges on the stop leg, early kills,
/// keepers overcharging, routes on one token, repeats that cannot profit).
#[test]
fn builders_refuse_v24_violations() {
    for (fam, scenarios) in families() {
        refuse_v24_violations(fam == "kron", scenarios);
    }
}

fn refuse_v24_violations(kron: bool, scenarios: Vec<(String, Action)>) {
    use kob_protocol::build::Leg;
    let find = |n: &str| scenarios.iter().find(|(x, _)| x == n).unwrap().1.clone();
    let expect_invalid = |a: Action, what: &str| match build(&a) {
        Err(Error::Invalid(_)) | Err(Error::InsufficientFunds { .. }) => {}
        other => panic!("{what}: expected a refusal, got {other:?}"),
    };
    // A stray (wrong amount) standing in for the custody.
    let Action::Batch(mut b) = find("take.ask.partial") else { panic!() };
    if let Leg::Ask { custody, .. } = &mut b.legs[0] {
        custody.state = custody.state.with_amount(common::WHOLE);
    }
    expect_invalid(Action::Batch(b), "stray as custody");
    // Taker tokens owned by a covenant id (a stray of some order).
    let Action::Batch(mut b) = find("take.bid.partial") else { panic!() };
    common::make_covenant_owned(&mut b.taker_tokens[0].state);
    expect_invalid(Action::Batch(b), "stray taker tokens");
    // More token inputs of one token next to KOB orders than the orders accept (KCC-20 16x16 token: nine, its
    // slots allow it but the orders do not; KRON: five, over the program's own four).
    let (prog, n_in) = if kron { (TemplateId::KronToken2433, 5u8) } else { (TemplateId::Kcc20Ref16x16, 9u8) };
    let (_, a) = common::scenarios_on(prog).into_iter().find(|(n, _)| n == "take.bid.partial").unwrap();
    let Action::Batch(mut b) = a else { panic!() };
    b.taker_tokens = (0..n_in)
        .map(|k| {
            common::tok_on(prog, 120 + k, if k == 0 { 4 * common::WHOLE - n_in as i64 + 1 } else { 1 }, common::pk(common::TAKER), 900)
        })
        .collect();
    expect_invalid(Action::Batch(b), "too many token inputs");
    // A booked exit's take-profit without its entry before rptUntil.
    let Action::Batch(mut b) = find("rpt.bid.merge.partial") else { panic!() };
    if let Leg::CondAsk { merge, .. } = &mut b.legs[0] {
        *merge = None;
    }
    expect_invalid(Action::Batch(b), "take-profit without merge");
    // A merge next to a stop-leg fill.
    let Action::Batch(mut b) = find("rpt.bid.merge.partial") else { panic!() };
    if let Leg::CondAsk { leg, order, .. } = &mut b.legs[0] {
        *leg = 1;
        order.state.armed = 1;
    }
    expect_invalid(Action::Batch(b), "merge on the stop leg");
    // A merge into an entry that is not the exit's parent.
    let Action::Batch(mut b) = find("rpt.ask.merge.partial") else { panic!() };
    if let Leg::CondBid { merge: Some(m), .. } = &mut b.legs[0] {
        m.entry.utxo.covenant_id = Some(common::cov(0x99));
    }
    expect_invalid(Action::Batch(b), "foreign entry");
    // An if-done fill below minFill.
    let Action::Batch(mut b) = find("ifd.bid.partial") else { panic!() };
    if let Leg::IfdBid { order, .. } = &mut b.legs[0] {
        order.state.min_fill = 5 * common::WHOLE;
    }
    expect_invalid(Action::Batch(b), "below minFill");
    // An unarmed stop entry without trigger evidence.
    let Action::Batch(mut b) = find("ifd.ask.stopEntry.trigger") else { panic!() };
    if let Leg::IfdAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = None;
    }
    expect_invalid(Action::Batch(b), "untriggered stop entry");
    // An auction time beyond the lock time.
    let Action::Batch(mut b) = find("cond.ask.stop.auction") else { panic!() };
    if let Leg::CondAsk { t, .. } = &mut b.legs[0] {
        *t = Some(common::NOW as i64 + 1);
    }
    expect_invalid(Action::Batch(b), "future auction time");
    // A keeper taking more than keeperTip; a trail the evidence does not justify; an update without evidence.
    let Action::Batch(mut b) = find("cond.ask.update.arm") else { panic!() };
    b.updates[0].take = Some(b.updates[0].order.state.keeper_tip().unwrap() + 1);
    expect_invalid(Action::Batch(b), "keeper overcharge");
    let Action::Batch(mut b) = find("cond.ask.update.trail") else { panic!() };
    if let Leg::Bid { order, .. } = &mut b.legs[0] {
        order.state.price = 214_000_000;
    }
    expect_invalid(Action::Batch(b), "unjustified trail");
    let Action::Batch(mut b) = find("cond.ask.update.arm") else { panic!() };
    b.updates[0].evidence = 1;
    expect_invalid(Action::Batch(b), "update without an evidence leg");
    // IOC kill before its time.
    let Action::RefundOrder(mut r) = find("refund.ask.iocKill") else { panic!() };
    r.lock_time -= 1;
    expect_invalid(Action::RefundOrder(r), "early kill");
    // Close of an entry that still holds tokens needs its custody (a settle refund).
    let Action::RefundOrder(mut r) = find("close.ifdAsk.emptyRepeat") else { panic!() };
    if let AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) = &mut r.order.state {
        s.amount_left = 2 * common::WHOLE;
    }
    expect_invalid(Action::RefundOrder(r), "close with tokens");
    // A route on one token.
    let Action::SwapRoute(mut rt) = find("route.swap") else { panic!() };
    if let Leg::Ask { order, custody, .. } = &mut rt.buy[0] {
        order.state.token_cov_id = common::TOKEN_COV;
        custody.utxo.covenant_id = Some(common::TOKEN_COV);
    }
    expect_invalid(Action::SwapRoute(rt), "one-token route");
    // A repeating entry whose take-profit does not beat the entry.
    let Action::CreateOrder(mut c) = find("create.ifdBid.repeat") else { panic!() };
    if let AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) = &mut c.order {
        let exit = kob_protocol::state::CondAskState { tp_price: 260_000_000, ..s.exit().unwrap() };
        s.exit_state = kob_protocol::state::IfdBidState::commit_exit(&exit);
    }
    expect_invalid(Action::CreateOrder(c), "unprofitable repeat");
    // An if-done entry funded below its escrow.
    let Action::CreateOrder(mut c) = find("create.ifdBid") else { panic!() };
    c.value -= 1;
    expect_invalid(Action::CreateOrder(c), "under-funded entry");
}

// ---------------------------------------------------------------- delivery modes (docs/design/bid-delivery-spec.md)

mod delivery {
    use super::common::*;
    use kob_protocol::artifacts::{token_template, TemplateId};
    use kob_protocol::build::{build, check_new_order, Action, Batch, BidDelivery, Leg};
    use kob_protocol::state::*;
    use kob_protocol::tx::{Arg, BuiltTx, SigPlan, Witness};
    use kob_protocol::Family;

    const T: TemplateId = TemplateId::Kcc20Ref;

    fn scenario(name: &str) -> Action {
        scenarios().into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("scenario {name}")).1
    }
    fn batch_of(a: Action) -> Batch {
        let Action::Batch(b) = a else { panic!("a batch") };
        b
    }
    fn built(a: &Action) -> BuiltTx {
        build(a).unwrap_or_else(|e| panic!("build: {e}"))
    }
    fn err(a: &Action) -> String {
        build(a).err().unwrap_or_else(|| panic!("the builder must refuse this")).to_string()
    }
    /// The (tokIn, form) arguments of the bid entry at input `i`.
    fn bid_args(b: &BuiltTx, i: usize) -> (i64, i64) {
        let SigPlan::Entry { args, entry, .. } = &b.plans[i] else { panic!("an order entry") };
        let int = |a: &Arg| if let Arg::Int(x) = a { *x } else { panic!("an int argument") };
        match entry.as_str() {
            "fill" => (int(&args[1]), int(&args[3])),
            "settle" => (int(&args[1]), int(&args[5])),
            e => panic!("entry {e}"),
        }
    }
    fn out_spk(b: &BuiltTx, k: usize) -> kaspa_consensus_core::tx::ScriptPublicKey {
        b.tx.to_tx().unwrap().0.outputs[k].script_public_key.clone()
    }
    fn out_value(b: &BuiltTx, k: usize) -> u64 {
        b.tx.to_tx().unwrap().0.outputs[k].value
    }
    fn tok_spk(s: &TokenState) -> kaspa_consensus_core::tx::ScriptPublicKey {
        s.spk_with(token_template(T))
    }
    fn plain(n: i64, maker: u8) -> TokenState {
        TokenState::user(T, n, pk(maker), ext_for(T))
    }
    fn leg_bid(b: &Batch, i: usize) -> BidState {
        let Leg::Bid { order, .. } = &b.legs[i] else { panic!("a bid leg") };
        order.state.clone()
    }
    fn set_delivery(b: &mut Batch, i: usize, d: Option<BidDelivery>) {
        match &mut b.legs[i] {
            Leg::Bid { delivery, .. } | Leg::CondBid { delivery, .. } => *delivery = d,
            _ => panic!("a bid leg"),
        }
    }
    fn leader_witness(b: &BuiltTx, i: usize) -> Witness {
        let SigPlan::TokenLeader { witness, .. } = &b.plans[i] else { panic!("input {i} is not a token leader: {:?}", b.roles[i]) };
        witness.clone()
    }

    /// Mode 1: the first fill delivers into a new bought-custody and the continuation records `bought`; a later fill
    /// continues the custody (its KAS stays on it, the escrow moves no carrier); the fill that ends the order releases it
    /// whole to the maker as one plain UTXO.
    #[test]
    fn mode_1_accumulates_into_the_bought_custody_and_releases_it_at_the_end() {
        let first = scenario("take.bid.acc.first");
        let b = built(&first);
        let s = leg_bid(&batch_of(first.clone()), 0);
        assert_eq!(out_spk(&b, 0), tok_spk(&TokenState::custody(Family::Kcc20, 4 * WHOLE, cov(0xb1), ext_for(T))), "custody of n");
        assert_eq!(out_spk(&b, 1), AnyState::KobBid(BidState { bought: 4 * WHOLE, ..s.clone() }).spk(), "bought = n");
        assert_eq!(bid_args(&b, 0), (1, 0));
        assert_eq!(b.roles[0], "KobBid.fill.cont@KCC20Ref");

        let more = scenario("take.bid.acc.more");
        let bm = built(&more);
        let s = leg_bid(&batch_of(more.clone()), 0);
        let v = batch_of(more.clone()).legs[0].clone();
        let Leg::Bid { order, delivery: Some(d), .. } = v else { panic!() };
        let c = d.custody.unwrap();
        assert_eq!(bid_args(&bm, 0), (1, 0), "tokIn is the bought-custody");
        assert_eq!(leader_witness(&bm, 1), Witness::CovenantId, "the custody leads, authorised by the order");
        assert_eq!(out_spk(&bm, 0), tok_spk(&TokenState::custody(Family::Kcc20, 7 * WHOLE, cov(0xb1), ext_for(T))));
        let used = s.used(3 * WHOLE).unwrap();
        let spend = s.spend(3 * WHOLE, 0, 1_000).unwrap();
        assert_eq!(out_value(&bm, 0) as i64, c.utxo.amount as i64 + used - spend, "the custody keeps its KAS, no carrier");
        assert_eq!(out_value(&bm, 1) as i64, order.utxo.amount as i64 - used, "the continuation keeps the carrier");
        assert_eq!(out_spk(&bm, 1), AnyState::KobBid(BidState { bought: 7 * WHOLE, ..s.clone() }).spk());
        assert_eq!(bm.roles[0], "KobBid.fill.cont.custody@KCC20Ref");

        let close = scenario("take.bid.acc.close");
        let bc = built(&close);
        let tx = bc.tx.to_tx().unwrap().0;
        assert_eq!(out_spk(&bc, 0), tok_spk(&plain(7 * WHOLE, MAKER_B)), "released whole to the maker");
        assert_eq!(out_value(&bc, 0), c.utxo.amount, "with the custody's KAS");
        assert!(!tx.outputs.iter().any(|o| matches!(o.covenant, Some(x) if x.covenant_id.as_bytes() == cov(0xb1))), "no continuation");
        assert_eq!(tx.outputs.last().unwrap().script_public_key, kob_protocol::script::p2pk_spk(&pk(MAKER_B)), "the refund");
        assert_eq!(bc.roles[0], "KobBid.fill.close.custody@KCC20Ref");
        // a remainder below REFUND_MIN rides on the release
        let dust = built(&scenario("take.bid.acc.closeDust"));
        assert!(out_value(&dust, 0) > c.utxo.amount);
    }

    /// Mode 2: without a receive UTXO the fill creates one (form 1); with the maker's receive UTXO it tops it up (form 2),
    /// the receive UTXO leading on the borrow path, its KAS kept, the escrow releasing only `topupTip`; form 0 is plain.
    #[test]
    fn mode_2_creates_or_tops_up_the_receive_utxo() {
        let recv = built(&scenario("take.bid.recv"));
        assert_eq!(out_spk(&recv, 0), tok_spk(&plain(4 * WHOLE, MAKER_B).with_borrow(kob_protocol::build::receive_borrow(RECV_T))));
        assert_eq!(bid_args(&recv, 0).1, 1);

        let plain0 = built(&scenario("take.bid.topup.plain"));
        assert_eq!(out_spk(&plain0, 0), tok_spk(&plain(4 * WHOLE, MAKER_B)));
        assert_eq!(bid_args(&plain0, 0).1, 0);

        let a = scenario("take.bid.topup");
        let b = built(&a);
        let leg = batch_of(a.clone()).legs[0].clone();
        let Leg::Bid { order, delivery: Some(d), .. } = leg else { panic!() };
        let r = d.receive.unwrap();
        let s = order.state.clone();
        assert_eq!(bid_args(&b, 0), (1, 2), "tokIn is the receive UTXO, form 2");
        assert_eq!(leader_witness(&b, 1), Witness::Borrow, "the receive UTXO leads on the borrow path");
        assert!(b.roles[1].starts_with("KCC20Ref.leader.borrow."), "{}", b.roles[1]);
        assert_eq!(out_spk(&b, 0), tok_spk(&r.state.with_amount(6 * WHOLE)), "have + n, every other state byte kept");
        let used = s.used(4 * WHOLE).unwrap();
        let spend = s.spend(4 * WHOLE, 0, 1_000).unwrap();
        assert_eq!(out_value(&b, 0) as i64, r.utxo.amount as i64 + used - spend, "the receive UTXO keeps its KAS");
        assert_eq!(out_value(&b, 1) as i64, order.utxo.amount as i64 - used - s.topup_tip, "only topupTip leaves the escrow");
        assert_eq!(b.roles[0], "KobBid.fill.cont.topup@KCC20Ref");
        // the matcher's cross: the receive UTXO leads, the ask custodies delegate
        let x = built(&scenario("match.cross.1x2.topup"));
        assert_eq!(leader_witness(&x, 3), Witness::Borrow);
        // a conditional bid tops up the same way
        let cb = built(&scenario("cond.bid.topup"));
        assert_eq!(bid_args(&cb, 0), (1, 2));
        assert_eq!(leader_witness(&cb, 1), Witness::Borrow);
        assert_eq!(cb.roles[0], "KobCondBid.settle.leg0.cont.topup@KCC20Ref");
    }

    /// The builder takes a top-up only where the batch allows it, and otherwise creates a new receive UTXO (form 1); a
    /// forced top-up the batch does not allow is refused (bid-delivery-spec §6.3 to §6.6).
    #[test]
    fn a_top_up_is_taken_only_where_the_batch_allows_it() {
        let base = batch_of(scenario("take.bid.topup"));
        let r = |b: &Batch| match &b.legs[0] {
            Leg::Bid { delivery: Some(d), .. } => d.receive.clone().unwrap(),
            _ => panic!(),
        };
        let recv0 = r(&base);
        let form_of = |b: &Batch, i: usize| bid_args(&built(&Action::Batch(b.clone())), i).1;
        let forced = |mut b: Batch, i: usize| {
            match &mut b.legs[i] {
                Leg::Bid { delivery: Some(d), .. } => d.form = Some(2),
                _ => panic!(),
            }
            err(&Action::Batch(b))
        };
        assert_eq!(form_of(&base, 0), 2, "positive: the honest top-up");
        // 1. a fill that does not gain more than the threshold (here: threshold = n)
        let mut b = base.clone();
        let mut thr = recv_tok(24, 2 * WHOLE, pk(MAKER_B), 4 * WHOLE, 1_500);
        set_delivery(&mut b, 0, bid_dlv(None, Some(thr.clone()), None));
        assert_eq!(form_of(&b, 0), 1);
        assert!(forced(b, 0).contains("does not gain more than"));
        // 2. a threshold below the order's recvThreshold
        let mut b = base.clone();
        thr = recv_tok(24, 2 * WHOLE, pk(MAKER_B), RECV_T - 1, 1_500);
        set_delivery(&mut b, 0, bid_dlv(None, Some(thr), None));
        assert_eq!(form_of(&b, 0), 1);
        // 3. another owner, a plain UTXO, another commitment, a non-canonical guard
        for bad in [
            recv_tok(24, 2 * WHOLE, pk(MAKER_C), RECV_T, 1_500),
            tok(24, 2 * WHOLE, pk(MAKER_B), SCHEME_P2PK, 1_500),
            {
                let mut x = recv0.clone();
                if let TokenState::Kcc20(k) = &mut x.state {
                    k.extension_commitment = [0x11; 32];
                }
                x
            },
            {
                let mut x = recv0.clone();
                if let TokenState::Kcc20(k) = &mut x.state {
                    k.borrow_guard[9] = 1;
                }
                x
            },
        ] {
            let mut b = base.clone();
            set_delivery(&mut b, 0, bid_dlv(None, Some(bad), None));
            assert_eq!(form_of(&b, 0), 1);
            assert!(forced(b, 0).contains("top-up (form 2) is not possible"));
        }
        // 4. not the token's lowest-index delivering leg: a plain bid of the token before it
        let mut b = base.clone();
        let other = bid(MAKER_C, P245, T);
        b.legs.insert(
            0,
            Leg::Bid {
                order: order(25, other.escrow(10 * WHOLE, 1).unwrap() as u64, cov(0xb5), 1_000, other),
                amount: 2 * WHOLE,
                t: None,
                delivery: None,
            },
        );
        b.taker_tokens[0] = tok(21, 6 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000);
        assert_eq!(form_of(&b, 1), 1);
        assert!(forced(b, 1).contains("lowest-index delivering leg"));
        // 5. a custody whose output sits at its own index (an IOC ask's return) precedes the receive UTXO
        let mut b = base.clone();
        let ioc = AskState { tif: TIF_IOC, ..ask_n(MAKER_A, P245, T, 10) };
        b.legs.push(Leg::Ask {
            order: order(26, carrier(), cov(0xa6), 1_000, ioc),
            custody: custody(27, 10, cov(0xa6), 1_000),
            amount: 2 * WHOLE,
            t: None,
        });
        b.funding = vec![key_utxo(28, TAKER, 100 * KAS)];
        assert_eq!(form_of(&b, 0), 1);
        assert!(forced(b, 0).contains("own input index"));
        // 6. the token's slots: three taker token inputs on the 3 / 3 program leave no room
        let mut b = base.clone();
        b.taker_tokens = (0..3).map(|k| tok(30 + k, if k == 0 { 2 * WHOLE } else { WHOLE }, pk(TAKER), SCHEME_P2PK, 1_000)).collect();
        assert_eq!(form_of(&b, 0), 1);
        assert!(forced(b, 0).contains("token input 4"));
        // 7. a program without a measured top-up never tops up
        for p in PROGRAMS.into_iter().filter(|p| p.family() == Family::Kcc20 && kob_protocol::budget::topup_extra(*p).is_none()) {
            let s = mode_bid(bid(MAKER_B, P245, p), MODE_TOPUP, 0);
            let mut b = take_bid_valued(s.clone(), 4, s.escrow(10 * WHOLE, 3).unwrap() as u64, None);
            b.taker_tokens[0] = tok_on(p, 21, 4 * WHOLE, pk(TAKER), 1_000);
            let mut rr = recv_tok(24, 2 * WHOLE, pk(MAKER_B), RECV_T, 1_500);
            if let TokenState::Kcc20(k) = &mut rr.state {
                k.extension_commitment = ext_for(p);
            }
            set_delivery(&mut b, 0, bid_dlv(None, Some(rr), None));
            assert_eq!(form_of(&b, 0), 1, "{}", p.name());
        }
    }

    /// A refund-carrying terminal bid is put first, so a token with one tops up only that one (bid-delivery-spec §6.4): the
    /// top-up the first pass gave the other bid moves to it.
    #[test]
    fn a_token_with_a_refund_carrying_bid_tops_up_only_that_bid() {
        let m2 = mode_bid(bid(MAKER_B, P245, T), MODE_TOPUP, 0);
        let cont = bid(MAKER_C, P245, T);
        let recv_b = recv_tok(24, 2 * WHOLE, pk(MAKER_B), RECV_T, 1_500);
        let recv_c = recv_tok(25, 2 * WHOLE, pk(MAKER_C), RECV_T, 1_500);
        let c2 = mode_bid(cont.clone(), MODE_TOPUP, 0);
        // leg 0 continues (no refund) and could top up; leg 1 ends with a refund
        let mut b = take_bid_valued(c2.clone(), 2, c2.escrow(10 * WHOLE, 3).unwrap() as u64, bid_dlv(None, Some(recv_c), None));
        b.legs.push(Leg::Bid {
            order: order(
                26,
                (m2.used(4 * WHOLE).unwrap() + m2.delivery_carrier + 2 * KAS as i64) as u64,
                cov(0xb6),
                1_000,
                m2.clone(),
            ),
            amount: 4 * WHOLE,
            t: None,
            delivery: bid_dlv(None, Some(recv_b.clone()), None),
        });
        b.taker_tokens[0] = tok(21, 6 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000);
        let tx = built(&Action::Batch(b.clone()));
        // the refund leg is input 0 and tops up; the other one creates a receive UTXO
        assert_eq!(bid_args(&tx, 0), (2, 2));
        assert_eq!(bid_args(&tx, 1).1, 1);
        assert_eq!(out_spk(&tx, 0), tok_spk(&recv_b.state.with_amount(6 * WHOLE)));
        assert_eq!(leader_witness(&tx, 2), Witness::Borrow);
        super::run("refund-first top-up", &Action::Batch(b.clone()));
        // forcing the top-up onto the continuing bid is refused
        match &mut b.legs[0] {
            Leg::Bid { delivery: Some(d), .. } => d.form = Some(2),
            _ => panic!(),
        }
        assert!(err(&Action::Batch(b)).contains("lowest-index delivering leg"));
    }

    /// The delivery inputs a leg names must match its order's mode.
    #[test]
    fn the_delivery_inputs_must_match_the_mode() {
        let more = batch_of(scenario("take.bid.acc.more"));
        // positive: the honest one builds (and validates, `every_builder_shape_validates_on_every_token_program`)
        built(&Action::Batch(more.clone()));
        // a mode-1 order holding bought tokens without its custody
        let mut b = more.clone();
        set_delivery(&mut b, 0, None);
        assert!(err(&Action::Batch(b)).contains("needs its bought-custody"));
        // a custody of another amount, of another owner
        for c in [bought_custody(23, 3 * WHOLE, cov(0xb1), 1_500), bought_custody(23, 4 * WHOLE, cov(0xbb), 1_500)] {
            let mut b = more.clone();
            set_delivery(&mut b, 0, bid_dlv(Some(c), None, None));
            build(&Action::Batch(b)).expect_err("not the bought-custody");
        }
        // a custody named by a mode-0 or a mode-2 order, a receive UTXO or a form named by a mode-0 / mode-1 order
        let plain_bid = batch_of(scenario("take.bid.partial"));
        let r = recv_tok(24, 2 * WHOLE, pk(MAKER_B), RECV_T, 1_500);
        for d in [
            bid_dlv(Some(bought_custody(23, 4 * WHOLE, cov(0xb1), 1_500)), None, None),
            bid_dlv(None, Some(r.clone()), None),
            bid_dlv(None, None, Some(1)),
        ] {
            let mut b = plain_bid.clone();
            set_delivery(&mut b, 0, d);
            assert!(err(&Action::Batch(b)).contains("delivery."));
        }
        let mut b = more.clone();
        set_delivery(&mut b, 0, bid_dlv(Some(bought_custody(23, 4 * WHOLE, cov(0xb1), 1_500)), Some(r.clone()), None));
        assert!(err(&Action::Batch(b)).contains("mode-2 (top-up) order only"));
        let mut b = batch_of(scenario("take.bid.recv"));
        set_delivery(&mut b, 0, bid_dlv(Some(bought_custody(23, 4 * WHOLE, cov(0xb1), 1_500)), None, None));
        assert!(err(&Action::Batch(b)).contains("only a mode-1 order"));
        // a form outside 0..=2, a forced top-up without a receive UTXO
        for f in [3u8, 2] {
            let mut b = batch_of(scenario("take.bid.recv"));
            set_delivery(&mut b, 0, bid_dlv(None, None, Some(f)));
            assert!(err(&Action::Batch(b)).contains("delivery"));
        }
    }

    /// A refund of a mode-1 bid holding bought tokens releases the custody whole at its own index (`refund(custIn)`).
    #[test]
    fn a_refund_releases_the_bought_custody_at_its_own_index() {
        let a = scenario("refund.bid.acc");
        let b = built(&a);
        let Action::RefundOrder(r) = &a else { panic!() };
        let SigPlan::Entry { args, entry, .. } = &b.plans[0] else { panic!() };
        assert_eq!((entry.as_str(), args.as_slice()), ("refund", &[Arg::Int(1)][..]));
        assert_eq!(b.roles[0], "KobBid.refund.custody@KCC20Ref");
        assert_eq!(out_spk(&b, 0), kob_protocol::script::p2pk_spk(&pk(MAKER_B)));
        assert_eq!(out_spk(&b, 1), tok_spk(&plain(4 * WHOLE, MAKER_B)));
        assert_eq!(out_value(&b, 1), r.bought_custody.as_ref().unwrap().utxo.amount);
        built(&scenario("refund.condBid.acc"));
        // without the custody: refused; a refund of a bid holding none names custIn 0 and no token input
        let mut x = r.clone();
        x.bought_custody = None;
        assert!(err(&Action::RefundOrder(x.clone())).contains("bought-custody"));
        // named as the sell custody instead: refused
        x.custody = r.bought_custody.clone();
        assert!(err(&Action::RefundOrder(x)).contains("boughtCustody"));
        let plain_refund = built(&scenario("refund.bid"));
        let SigPlan::Entry { args, .. } = &plain_refund.plans[0] else { panic!() };
        assert_eq!(args.as_slice(), &[Arg::Int(0)][..]);
    }

    /// Placement (bid-delivery-spec §2): the mode and its fields, bought = 0, mode 0 for an IOC / FOK bid and for a KRON
    /// delivery, a top-up only on a program with a measured top-up; a repeating entry's exits in mode 0.
    #[test]
    fn placement_checks_the_delivery_block() {
        let ok = |s: AnyState| check_new_order(&s).unwrap_or_else(|e| panic!("{e}"));
        let bad = |s: AnyState, what: &str| {
            let e = check_new_order(&s).unwrap_err().to_string();
            assert!(e.contains(what), "{e}");
        };
        let b = bid(MAKER_A, P245, T);
        ok(AnyState::KobBid(mode_bid(b.clone(), MODE_ACCUMULATE, 0)));
        ok(AnyState::KobBid(mode_bid(b.clone(), MODE_TOPUP, 0)));
        bad(AnyState::KobBid(mode_bid(b.clone(), MODE_ACCUMULATE, WHOLE)), "bought = 0");
        bad(AnyState::KobBid(BidState { mode: 3, ..b.clone() }), "mode must be 0, 1 or 2");
        bad(AnyState::KobBid(BidState { recv_threshold: 0, ..mode_bid(b.clone(), MODE_TOPUP, 0) }), "recvThreshold > 0");
        bad(AnyState::KobBid(BidState { topup_tip: b.delivery_carrier + 1, ..mode_bid(b.clone(), MODE_TOPUP, 0) }), "topupTip");
        bad(
            AnyState::KobBid(BidState { recv_threshold: 1, ..mode_bid(b.clone(), MODE_ACCUMULATE, 0) }),
            "recvThreshold = topupTip = 0",
        );
        bad(AnyState::KobBid(BidState { tif: TIF_IOC, ..mode_bid(b.clone(), MODE_ACCUMULATE, 0) }), "IOC / FOK");
        bad(AnyState::KobBid(BidState { tif: TIF_FOK, ..mode_bid(b.clone(), MODE_TOPUP, 0) }), "IOC / FOK");
        ok(AnyState::KobBid(BidState { tif: TIF_IOC, ..b.clone() }));
        let kron = bid(MAKER_A, P245, TemplateId::KronToken2433);
        bad(AnyState::KobBidKron(mode_bid(kron, MODE_ACCUMULATE, 0)), "KRON delivery is mode 0");
        for p in PROGRAMS.into_iter().filter(|p| p.family() == Family::Kcc20 && kob_protocol::budget::topup_extra(*p).is_none()) {
            bad(AnyState::KobBid(mode_bid(bid(MAKER_A, P245, p), MODE_TOPUP, 0)), "no measured top-up");
            ok(AnyState::KobBid(mode_bid(bid(MAKER_A, P245, p), MODE_ACCUMULATE, 0)));
        }
        let c = cond_bid(MAKER_A, T);
        ok(AnyState::KobCondBid(mode_cond_bid(c.clone(), MODE_ACCUMULATE, 0)));
        bad(AnyState::KobCondBid(mode_cond_bid(c.clone(), MODE_ACCUMULATE, 3)), "bought = 0");
        let ia = IfdAskState { exit_mode: MODE_ACCUMULATE, ..ifd_ask(MAKER_A, T) };
        ok(AnyState::KobIfdAsk(ia.clone()));
        bad(AnyState::KobIfdAsk(IfdAskState { rpt_amount: 1 + 20 * WHOLE, ..ia }), "repeating entry's exits are mode 0");
    }

    /// The fill of a repeating sell-first entry whose exits are not mode 0 is refused (the covenant refuses it).
    #[test]
    fn a_repeating_entry_with_a_delivering_exit_mode_is_not_filled() {
        let Action::Batch(mut b) = scenario("ifd.ask.repeat.book") else { panic!() };
        built(&Action::Batch(b.clone()));
        if let Leg::IfdAsk { order, .. } = &mut b.legs[0] {
            order.state.exit_mode = MODE_ACCUMULATE;
        }
        assert!(err(&Action::Batch(b)).contains("exitMode = 0"));
    }

    /// The default top-up tip: `min(deliveryCarrier, ceil(1.25 × extra))`, none on a program without a top-up.
    #[test]
    fn the_default_top_up_tip_is_five_quarters_of_the_extra_fee_capped_at_the_carrier() {
        use kob_protocol::budget::{default_topup_tip, topup_extra};
        let extra = topup_extra(T).expect("KCC20Ref tops up") as i64;
        assert_eq!(default_topup_tip(T, i64::MAX), Some((extra * 5 + 3) / 4));
        assert_eq!(default_topup_tip(T, extra), Some(extra));
        assert_eq!(default_topup_tip(TemplateId::KronToken2433, 10 * KAS as i64), None);
    }

    /// Ablation of the builder's top-up rules: with them skipped (`build_batch_unchecked`), a forced top-up that is not the
    /// token's lowest-index delivering leg, or that does not gain more than the threshold, is rejected by the engine.
    #[cfg(feature = "adversarial")]
    #[test]
    fn without_the_top_up_rules_the_engine_rejects_the_top_up() {
        use kob_protocol::build::build_batch_unchecked;
        use kob_protocol::tx::{assemble, sign_locally};
        let rejected = |b: &Batch| {
            let built =
                build_batch_unchecked(b, &|r: &str| Ok(kob_protocol::budget::lookup(r).unwrap_or(200))).expect("builds unchecked");
            let sigs = sign_locally(&built, &keys()).unwrap();
            let (tx, entries) = assemble(&built, &sigs).unwrap();
            kob_protocol::verify::execute(&tx, &entries, true).unwrap().iter().any(|r| r.is_err())
        };
        let base = batch_of(scenario("take.bid.topup"));
        let mut honest = base.clone();
        if let Leg::Bid { delivery: Some(d), .. } = &mut honest.legs[0] {
            d.form = Some(2);
        }
        assert!(!rejected(&honest), "the honest forced top-up passes");
        // n <= threshold
        let mut b = honest.clone();
        set_delivery(&mut b, 0, bid_dlv(None, Some(recv_tok(24, 2 * WHOLE, pk(MAKER_B), 4 * WHOLE, 1_500)), Some(2)));
        assert!(rejected(&b), "a top-up gaining only the threshold");
        // not the lowest-index delivering leg: a plain bid's delivery is the token's first output
        let mut b = honest.clone();
        let other = bid(MAKER_C, P245, T);
        b.legs.insert(
            0,
            Leg::Bid {
                order: order(25, other.escrow(10 * WHOLE, 1).unwrap() as u64, cov(0xb5), 1_000, other),
                amount: 2 * WHOLE,
                t: None,
                delivery: None,
            },
        );
        b.taker_tokens[0] = tok(21, 6 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000);
        assert!(rejected(&b), "a top-up whose delivery is not the token's first output");
    }
}
