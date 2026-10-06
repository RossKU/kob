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
