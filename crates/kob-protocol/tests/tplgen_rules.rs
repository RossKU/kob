//! TPLGEN (the pre-mainnet template generation): the builder-side rules that go with the new
//! contract rules, testable without the new artifacts.
//!
//! * Row 4: a sell trail never moves the stop past `MAX_STOP`.

mod common;

use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::{CondAskState, MAX_STOP_PRICE};

const T: TemplateId = TemplateId::Kcc20Ref8x8;

#[test]
fn a_sell_trail_stops_at_max_stop() {
    let step = 1_000;
    let s = |stop: i64| CondAskState { stop_price: stop, trail_step: step, trail_gap: 0, tp_price: 0, ..cond_ask(MAKER_A, T) };
    // far below: the evidence alone decides
    assert_eq!(s(MAX_STOP_PRICE - 10 * step).trail_steps(MAX_STOP_PRICE + 50 * step), 10);
    // one step below MAX_STOP: one step, onto MAX_STOP exactly
    assert_eq!(s(MAX_STOP_PRICE - step).trail_steps(i64::MAX / 4), 1);
    // less than a step below, or at MAX_STOP: no step
    assert_eq!(s(MAX_STOP_PRICE - step + 1).trail_steps(i64::MAX / 4), 0);
    assert_eq!(s(MAX_STOP_PRICE).trail_steps(i64::MAX / 4), 0);
}

/// The pair twin takes k from the filler and requires it to be the largest the evidence allows, unless a cap binds:
/// at `MAX_STOP` the cap binds (R10CA-2), so the honest k just below it stays valid, and no k may pass it.
#[test]
fn a_pair_sell_trail_stops_at_max_stop() {
    use common::pair::cond_pair_ask;
    use kob_protocol::state::PairEvidence;
    let step = 1_000;
    let s = |stop: i64| kob_protocol::state::CondPairState {
        stop_price: stop,
        trail_step: step,
        trail_gap: 0,
        tp_price: 0,
        ..cond_pair_ask(MAKER_A, T, T)
    };
    // evidence far above MAX_STOP: it justifies more steps than the cap allows
    let ev = PairEvidence::Pair { price: i64::MAX / 8 };
    // one step below: exactly one step, onto MAX_STOP, and the cap makes it the largest
    let st = s(MAX_STOP_PRICE - step);
    assert_eq!(st.trail_k(ev), Some(1));
    assert!(st.trail_check(ev, 1));
    assert!(!st.trail_check(ev, 2), "past MAX_STOP");
    // two steps below: k = 2 is the largest; k = 1 is not (the cap does not bind there)
    let st = s(MAX_STOP_PRICE - 2 * step);
    assert_eq!(st.trail_k(ev), Some(2));
    assert!(st.trail_check(ev, 2) && !st.trail_check(ev, 1));
    // less than a step below: no trail
    assert_eq!(s(MAX_STOP_PRICE - step + 1).trail_k(ev), None);
    // far below with modest evidence: the evidence decides, as before
    let st = s(1_000_000);
    let near = PairEvidence::Pair { price: 1_000_000 + 5 * step };
    let k = st.trail_k(near).expect("a step");
    assert!(st.trail_check(near, k) && !st.trail_check(near, k + 1));
}

/// R10CC-1, by execution: the state span of a template runs as script (`0x6b ‖ state ‖ 0x6c ‖ code`, the template
/// hash covers only the code), and the stop readers read the evidence order's fields at fixed offsets without
/// checking that the bytes between them are data pushes. A hand-built evidence `KobAsk` whose `maker` field (state
/// bytes 0..33, which no reader reads) is replaced by 33 bytes of code swaps the fill amount `nb` (the first
/// signature-script push, which the stop reads as `n`) for one whole token before the ask's own code runs. The stop
/// sees `n = 1000` whole tokens (its `minTouch`); the ask sells one.
#[test]
fn hidden_code_in_an_evidence_state_span() {
    use kob_protocol::build::{build, Action};
    use kob_protocol::state::AnyState;
    use kob_protocol::tx::{finalize, sighash, sign_locally, spk_to_string, Arg, FinalizeOptions, SigPlan};
    use kob_protocol::verify::validate_signed;
    let t = TemplateId::Kcc20Ref;
    let Action::Batch(b) = scenarios_on(t).into_iter().find(|(n, _)| n == "cond.ask.update.arm").unwrap().1 else { panic!() };
    let mut built = build(&Action::Batch(b.clone())).unwrap();
    {
        let s = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&s).expect("the honest arm");
    }
    let claimed: i64 = 1000 * WHOLE;
    let e = built
        .plans
        .iter()
        .position(
            |p| matches!(p, SigPlan::Entry { template, entry, .. } if template.name().starts_with("KobAsk") && entry == "settle"),
        )
        .unwrap();
    let st = built
        .plans
        .iter()
        .position(
            |p| matches!(p, SigPlan::Entry { template, entry, .. } if template.name().starts_with("KobCondAsk") && entry == "update"),
        )
        .unwrap();
    // the evidence: the ask's state, its real fill (one whole token) and the outputs its code builds
    let SigPlan::Entry { template: et, state: es, args: ea, .. } = built.plans[e].clone() else { unreachable!() };
    let AnyState::KobAsk(ask) = AnyState::decode(et, &es).unwrap() else { panic!() };
    let Arg::Bytes(nb) = &ea[0] else { panic!("nb") };
    let real = i64::from_le_bytes(nb.as_slice().try_into().unwrap());
    assert_eq!(real, WHOLE);
    let hidden_maker: [u8; 32] =
        kob_protocol::json::hex32("4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a").unwrap(); // sha256(0x01)
                                                                                                                // 33 bytes for the maker push: drop nb (depth 3), push the real amount, roll it back to depth 3, push sha256(0x01) as
                                                                                                                // the "maker", pad with OP_NOP
    let mut code = vec![0x53, 0x7a, 0x75, 0x08];
    code.extend_from_slice(&real.to_le_bytes());
    code.extend_from_slice(&[0x53, 0x7a, 0x53, 0x7a, 0x53, 0x7a, 0x51, 0xa8]);
    code.resize(33, 0x61);
    let mut hidden = es.clone();
    assert_eq!(hidden[0], 0x20, "the maker push");
    hidden[..33].copy_from_slice(&code);
    let mut ea2 = ea.clone();
    ea2[0] = Arg::Bytes(claimed.to_le_bytes().to_vec());
    // the ask's own outputs, as its code now builds them: the proceeds to the hidden "maker", the rest with it as maker
    let p2pk = |k: &[u8; 32]| spk_to_string(&kob_protocol::script::p2pk_spk(k));
    let old_rest =
        spk_to_string(&AnyState::KobAsk(kob_protocol::state::AskState { amount_left: ask.amount_left - real, ..ask.clone() }).spk());
    // the ask builds its continuation from its OWN redeem script with amountLeft replaced (`contSpk`): the hidden code
    // is carried into the rest (span bytes [235..243) are amountLeft)
    let mut rest_span = hidden.clone();
    rest_span[235..243].copy_from_slice(&(ask.amount_left - real).to_le_bytes());
    let new_rest = spk_to_string(&kob_protocol::script::p2sh_spk(&kob_protocol::artifacts::template(et).redeem(&rest_span)));
    for o in built.tx.outputs.iter_mut() {
        if o.script_public_key == p2pk(&ask.maker) {
            o.script_public_key = p2pk(&hidden_maker);
        } else if o.script_public_key == old_rest {
            o.script_public_key = new_rest.clone();
        }
    }
    built.plans[e] = SigPlan::Entry { template: et, state: hidden, entry: "settle".into(), args: ea2 };
    built.tx.inputs[e].utxo.script_public_key = spk_to_string(&built.plans[e].spk());
    // the stop: its minTouch is the claimed amount
    let SigPlan::Entry { template: stt, state: ss, entry: sen, args: sa } = built.plans[st].clone() else { unreachable!() };
    let AnyState::KobCondAsk(stop) = AnyState::decode(stt, &ss).unwrap() else { panic!() };
    let mut big = stop.clone();
    big.min_touch = claimed;
    let cont = |s: &kob_protocol::state::CondAskState| {
        spk_to_string(&AnyState::KobCondAsk(kob_protocol::state::CondAskState { armed: 1, ..s.clone() }).spk())
    };
    let (old_c, new_c) = (cont(&stop), cont(&big));
    let mut found = false;
    for o in built.tx.outputs.iter_mut().filter(|o| o.script_public_key == old_c) {
        o.script_public_key = new_c.clone();
        found = true;
    }
    assert!(found, "the stop's continuation");
    built.plans[st] = SigPlan::Entry { template: stt, state: AnyState::KobCondAsk(big).encode(), entry: sen, args: sa };
    built.tx.inputs[st].utxo.script_public_key = spk_to_string(&built.plans[st].spk());
    let (tx, entries) = built.tx.to_tx().unwrap();
    for r in built.sign.iter_mut() {
        r.sighash = sighash(&tx, &entries, r.input_index);
    }
    let s = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
    let v = validate_signed(&s);
    // The templates before TPLGEN accepted it: the stop armed on a one-token print it read as 1000 tokens (run on
    // 2c8cf8468). With the state-span check every entry but the cancel refuses a span that is not data only.
    let e = v.expect_err("the hidden-code evidence must be refused").to_string();
    assert!(e.contains("input 0"), "the evidence input fails: {e}");
}

/// The canonical pushes of a state span: (offset, header length, data length) per field.
fn pushes(span: &[u8]) -> Vec<(usize, usize, usize)> {
    let (mut i, mut out) = (0, vec![]);
    while i < span.len() {
        let (h, n) = match span[i] {
            b @ 1..=75 => (1, b as usize),
            0x4c => (2, span[i + 1] as usize),
            0x4d => (3, span[i + 1] as usize | (span[i + 2] as usize) << 8),
            b => panic!("byte {i} is 0x{b:02x}, not a push"),
        };
        out.push((i, h, n));
        i += h + n;
    }
    assert_eq!(i, span.len());
    out
}

/// R10CC-1 on every order template the scenarios spend: the order's state span with its LAST 8-byte field
/// replaced by code (`OP_1 OP_SHA256`, a 32-byte value, then `OP_NOP` padding: same length, the template hash
/// unchanged). Every entry but the maker's cancel refuses it; the maker's cancel (which reads only the maker) still
/// ends it, so a rigged order never locks its funds.
#[test]
fn a_rigged_state_span_is_refused_by_every_entry_but_the_cancel() {
    use kob_protocol::build::build;
    use kob_protocol::tx::{finalize, sighash, sign_locally, spk_to_string, FinalizeOptions, SigPlan};
    use kob_protocol::verify::validate_signed;
    let mut seen: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    let mut all: Vec<(String, kob_protocol::build::Action)> = vec![];
    for t in [TemplateId::Kcc20Ref, TemplateId::KronToken2433] {
        all.extend(scenarios_on(t));
        all.extend(delivery_scenarios(t));
        all.extend(common::pair::pair_scenarios(t, t));
        all.extend(common::curve::curve_scenarios(t, 4));
    }
    all.extend(scenarios_kron());
    {
        for (name, a) in all {
            let Ok(built) = build(&a) else { continue };
            for k in 0..built.plans.len() {
                let SigPlan::Entry { template, state, entry, args } = built.plans[k].clone() else { continue };
                if template.kind_code().is_none() {
                    continue;
                }
                let mut b = built.clone();
                let ps = pushes(&state);
                let Some(&(o, 1, 8)) = ps.iter().rev().find(|p| p.1 == 1 && p.2 == 8) else { continue };
                let mut rigged = state.clone();
                rigged[o..o + 9].copy_from_slice(&[0x51, 0xa8, 0x61, 0x61, 0x61, 0x61, 0x61, 0x61, 0x61]);
                b.plans[k] = SigPlan::Entry { template, state: rigged, entry: entry.clone(), args };
                b.tx.inputs[k].utxo.script_public_key = spk_to_string(&b.plans[k].spk());
                let (tx, entries) = b.tx.to_tx().unwrap();
                for r in b.sign.iter_mut() {
                    r.sighash = sighash(&tx, &entries, r.input_index);
                }
                let s = finalize(&b, &sign_locally(&b, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
                let v = validate_signed(&s);
                let label = format!("{name}: input {k} {} {entry}", template.name());
                let e = seen.entry(format!("{} {entry}", template.name())).or_default();
                // the fill entries of the evidence kinds (what a stop reads as evidence) check the span; nothing else does
                let evidence_fill = matches!(
                    (template.base(), entry.as_str()),
                    (TemplateId::KobAsk, "settle") | (TemplateId::KobBid, "fill") | (TemplateId::KobPair, "settle")
                );
                if entry == "cancel" {
                    v.unwrap_or_else(|e| panic!("{label}: the maker's cancel must end a rigged order: {e}"));
                    e.0 += 1;
                } else if evidence_fill {
                    let err = v.expect_err(&label).to_string();
                    assert!(err.contains(&format!("input {k}")), "{label}: {err}");
                    e.1 += 1;
                } else if v.is_ok() {
                    // another entry runs with the rigged values: only its own maker's order is affected
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
            }
        }
    }
    // every order kind and its every entry was tried
    let kinds: std::collections::BTreeSet<String> = seen.keys().map(|k| k.split(' ').next().unwrap().to_string()).collect();
    assert!(kinds.len() >= 16, "kinds tried: {kinds:?}");
    for (k, (c, o)) in &seen {
        assert!(c + o > 0, "{k}");
    }
    println!("{seen:#?}");
}

/// The check reads only the push headers: a field whose VALUE is made of bytes that look like opcodes (`OP_RETURN`,
/// `OP_TOALTSTACK`, `OP_FROMALTSTACK`, `OP_PUSHDATA1`) sits inside its push and is accepted.
#[test]
fn field_values_that_look_like_opcodes_pass() {
    use kob_protocol::build::{build, Action, Leg};
    use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions};
    use kob_protocol::verify::validate_signed;
    for t in [TemplateId::Kcc20Ref, TemplateId::KronToken2433] {
        let Action::Batch(mut b) = scenarios_on(t).into_iter().find(|(n, _)| n == "take.ask.partial").unwrap().1 else { panic!() };
        let Leg::Ask { order, .. } = &mut b.legs[0] else { panic!() };
        // price and tip of bytes 0x6a 0x6b 0x6c 0x4c (little-endian), minFill 0x4c
        order.state.price = i64::from_le_bytes([0x6a, 0x6b, 0x6c, 0x4c, 0, 0, 0, 0]);
        order.state.tip = 0x6a;
        order.state.min_fill = 0x4c;
        let built = build(&Action::Batch(b)).unwrap_or_else(|e| panic!("{}: {e}", t.name()));
        let s = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&s).unwrap_or_else(|e| panic!("{}: {e}", t.name()));
    }
}

/// Row 1: every entry of every order kind refuses a transaction on a KIP-21 user lane (`OpTxSubnetId` is not the
/// 20 zero bytes of the native subnetwork): each honest spend of the scenarios, copied onto lane `[1, 2, 3, 4, 0…]`,
/// is refused at its first order input; the native original passes.
#[test]
fn every_order_entry_refuses_a_user_lane() {
    use kob_protocol::build::build;
    use kob_protocol::tx::{finalize, sighash, sign_locally, FinalizeOptions, SigPlan};
    use kob_protocol::verify::validate_signed;
    let mut all: Vec<(String, kob_protocol::build::Action)> = vec![];
    for t in [TemplateId::Kcc20Ref, TemplateId::KronToken2433] {
        all.extend(scenarios_on(t));
        all.extend(delivery_scenarios(t));
        all.extend(common::pair::pair_scenarios(t, t));
        all.extend(common::curve::curve_scenarios(t, 4));
    }
    all.extend(scenarios_kron());
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    for (name, a) in all {
        let Ok(built) = build(&a) else { continue };
        let first = built.plans.iter().position(|p| matches!(p, SigPlan::Entry { template, .. } if template.kind_code().is_some()));
        let Some(k) = first else { continue };
        let SigPlan::Entry { template, entry, .. } = &built.plans[k] else { unreachable!() };
        let label = format!("{name}: {} {entry}", template.name());
        let s = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&s).unwrap_or_else(|e| panic!("{label}: the native original: {e}"));
        // the signed transaction moved to a lane, every signature made again over the lane's digest (the builders
        // and `to_tx` refuse a user lane, so this is done on the consensus transaction)
        let on = |lane: [u8; 20]| {
            let (mut tx, entries) = s.tx.to_tx().unwrap();
            tx.subnetwork_id = kaspa_consensus_core::subnets::SubnetworkId::from_bytes(lane);
            for r in &built.sign {
                let d = sighash(&tx, &entries, r.input_index);
                let sig = kob_protocol::tx::sign_digest(&keys()[&r.pubkey], &d).unwrap();
                tx.inputs[r.input_index].signature_script = built.plans[r.input_index].sigscript(Some(&sig)).unwrap();
            }
            kob_protocol::verify::execute(&tx, &entries, true).unwrap()
        };
        let native = on([0; 20]);
        assert!(native.iter().all(|o| o.is_ok()), "{label}: the re-signed native control: {native:?}");
        let mut lane = [0u8; 20];
        lane[..4].copy_from_slice(&[1, 2, 3, 4]);
        let r = on(lane);
        assert!(r[k].is_err(), "{label}: a user lane is accepted at input {k}");
        // every other order input of the transaction refuses it too
        for (i, p) in built.plans.iter().enumerate() {
            if matches!(p, SigPlan::Entry { template, .. } if template.kind_code().is_some()) {
                assert!(r[i].is_err(), "{label}: input {i}");
            }
        }
        seen.insert(format!("{} {entry}", template.name()));
    }
    let kinds: std::collections::BTreeSet<&str> = seen.iter().map(|k| k.split(' ').next().unwrap()).collect();
    assert!(kinds.len() >= 16 && seen.iter().any(|k| k.ends_with("cancel")), "{seen:?}");
}

/// Row 3 (R8F1a-1): a hand-made zero-amount ask or conditional ask with a 0-unit custody cannot be refunded (its refund
/// pinned no maker output); an honest one of 1 base unit still is.
#[test]
fn a_zero_amount_ask_is_not_refunded() {
    use kob_protocol::build::{build_refund_order, RefundOrder};
    use kob_protocol::state::AnyState;
    use kob_protocol::tx::{finalize, sign_locally, spk_to_string, FinalizeOptions, SigPlan};
    use kob_protocol::verify::validate_signed;
    let t = TemplateId::Kcc20Ref8x8;
    let o = cov(0x7a);
    for (left, ok) in [(1i64, true)] {
        for s in [
            AnyState::KobAsk(kob_protocol::state::AskState { amount_left: left, min_fill: 0, ..ask(MAKER_A, P250, t) }),
            AnyState::KobCondAsk(kob_protocol::state::CondAskState { amount_left: left, min_fill: 0, ..cond_ask(MAKER_A, t) }),
        ] {
            let r = RefundOrder {
                prefund: None,
                bought_custody: None,
                order: order(70, CARRIER, o, 1_000, s.clone()),
                custody: Some(common::pair::tutxo(t, TOKEN_COV, 71, left, o, true)),
                foreign: vec![],
                lock_time: EXPIRY as u64,
                funding: vec![key_utxo(4, KEEPER, 5 * KAS)],
                change: Some(pk(KEEPER)),
                fee: fee(),
            };
            let label = format!("{} amountLeft {left}", s.template_id().name());
            let built = build_refund_order(&r, &kob_protocol::budget::lookup).unwrap_or_else(|e| panic!("{label}: {e}"));
            let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
            let v = validate_signed(&signed);
            assert_eq!(v.is_ok(), ok, "{label}: {:?}", v.err());
            // the same refund of an order of 0 (the builders refuse one, so it is made from this one): refused at the order
            let mut b = built.clone();
            for k in 0..b.plans.len() {
                match &mut b.plans[k] {
                    SigPlan::Entry { template, state, .. } if template.kind_code().is_some() => {
                        let z = match AnyState::decode(*template, state).unwrap() {
                            AnyState::KobAsk(a) => AnyState::KobAsk(kob_protocol::state::AskState { amount_left: 0, ..a }),
                            AnyState::KobCondAsk(a) => AnyState::KobCondAsk(kob_protocol::state::CondAskState { amount_left: 0, ..a }),
                            x => panic!("{x:?}"),
                        };
                        *state = z.encode();
                    }
                    SigPlan::TokenLeader { state, next_states, template, .. } => {
                        let old: Vec<String> = next_states
                            .iter()
                            .map(|s| spk_to_string(&s.spk_with(kob_protocol::artifacts::template(*template))))
                            .collect();
                        state.amount = 0;
                        for s in next_states.iter_mut() {
                            s.amount = 0;
                        }
                        for (o, s) in old.iter().zip(next_states.iter()) {
                            let new = spk_to_string(&s.spk_with(kob_protocol::artifacts::template(*template)));
                            for out in b.tx.outputs.iter_mut().filter(|x| &x.script_public_key == o) {
                                out.script_public_key = new.clone();
                            }
                        }
                    }
                    _ => {}
                }
                b.tx.inputs[k].utxo.script_public_key = spk_to_string(&b.plans[k].spk());
            }
            let (tx, entries) = b.tx.to_tx().unwrap();
            for r in b.sign.iter_mut() {
                r.sighash = kob_protocol::tx::sighash(&tx, &entries, r.input_index);
            }
            let z = finalize(&b, &sign_locally(&b, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
            let e = validate_signed(&z).expect_err(&format!("{label}: the refund of an order of 0")).to_string();
            assert!(e.contains("input 0"), "{label}: {e}");
        }
    }
}

/// Row 4 in the engine (`KobCondAsk.update`, R9FA2-2): a sell trail stops at MAX_STOP. From one step below, a print far
/// above moves the stop exactly onto MAX_STOP; from MAX_STOP, or above it (a hand-made state), no trail is accepted.
#[test]
fn a_sell_trail_stops_at_max_stop_in_the_engine() {
    use kob_protocol::build::{build, Action};
    use kob_protocol::state::AnyState;
    use kob_protocol::tx::{finalize, sighash, sign_locally, spk_to_string, FinalizeOptions, SigPlan};
    use kob_protocol::verify::validate_signed;
    for t in [TemplateId::Kcc20Ref, TemplateId::KronToken2433] {
        let Action::Batch(mut b) = scenarios_on(t).into_iter().find(|(n, _)| n == "cond.ask.update.trail").unwrap().1 else {
            panic!()
        };
        let step = 5_000_000;
        // the evidence: a resting bid quoting far above MAX_STOP
        let ev = ev_bid(MAX_STOP_PRICE + 1_000 * step, 1, t);
        b.legs[b.updates[0].evidence] = ev;
        let set = |s: &mut AnyState, stop: i64| match s {
            AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => {
                x.stop_price = stop;
                x.tp_price = 0;
            }
            _ => panic!(),
        };
        set(&mut b.updates[0].order.state, MAX_STOP_PRICE - step);
        let built = build(&Action::Batch(b.clone())).unwrap_or_else(|e| panic!("{}: {e}", t.name()));
        let s = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&s).unwrap_or_else(|e| panic!("{}: one step below: {e}", t.name()));
        // the continuation's stop is MAX_STOP exactly
        let k = built.plans.iter().position(|p| matches!(p, SigPlan::Entry { entry, .. } if entry == "update")).unwrap();
        let SigPlan::Entry { template, state, .. } = &built.plans[k] else { unreachable!() };
        let mut onto = AnyState::decode(*template, state).unwrap();
        set(&mut onto, MAX_STOP_PRICE);
        if let AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) = &mut onto {
            x.armed = 0;
        }
        assert!(
            built.tx.outputs.iter().any(|o| o.script_public_key == spk_to_string(&onto.spk())),
            "{}: the stop moves onto MAX_STOP",
            t.name()
        );
        // the builder refuses a trail from MAX_STOP or above it
        for stop in [MAX_STOP_PRICE, MAX_STOP_PRICE + 1] {
            let mut c = b.clone();
            set(&mut c.updates[0].order.state, stop);
            assert!(build(&Action::Batch(c)).is_err(), "{}: builder at {stop}", t.name());
            // the covenant refuses it too: the transaction above with the order's stop at `stop`
            let mut x = built.clone();
            let SigPlan::Entry { template, state, entry, args } = x.plans[k].clone() else { unreachable!() };
            let mut st = AnyState::decode(template, &state).unwrap();
            set(&mut st, stop);
            x.plans[k] = SigPlan::Entry { template, state: st.encode(), entry, args };
            x.tx.inputs[k].utxo.script_public_key = spk_to_string(&x.plans[k].spk());
            let (tx, entries) = x.tx.to_tx().unwrap();
            for r in x.sign.iter_mut() {
                r.sighash = sighash(&tx, &entries, r.input_index);
            }
            let s = finalize(&x, &sign_locally(&x, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
            let e = validate_signed(&s).expect_err(&format!("{}: covenant at {stop}", t.name())).to_string();
            assert!(e.contains(&format!("input {k}")), "{}: {e}", t.name());
        }
    }
}

/// Row 4 in the engine for the pair twin (`KobCondPair`, R10CA-2): the filler names k, and the covenant takes it only as
/// the largest step the evidence allows unless a cap binds. Near MAX_STOP the cap binds: from one step below, k = 1 (onto
/// MAX_STOP) is accepted though the print would justify more; from two steps below, k = 2 is the largest and k = 1 is
/// refused (no cap binds there); a k that passes MAX_STOP is refused.
#[test]
fn a_pair_sell_trail_stops_at_max_stop_in_the_engine() {
    use common::pair::{pair, pair_leg};
    use kob_protocol::build::{build, Action, Leg};
    use kob_protocol::state::AnyState;
    use kob_protocol::tx::{finalize, sighash, sign_locally, spk_to_string, Arg, FinalizeOptions, SigPlan};
    use kob_protocol::verify::validate_signed;
    let (pa, pb) = (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref);
    let Action::Batch(b0) =
        common::pair::pair_scenarios(pa, pb).into_iter().find(|(n, _)| n == "pair.update.ask.trail.ev1").unwrap().1
    else {
        panic!()
    };
    let step = 10;
    let run = |stop: i64, k_override: Option<i64>| -> (Result<(), String>, i64) {
        let mut b = b0.clone();
        // the evidence: a resting pair BID quoting far above MAX_STOP (B per whole A)
        let ev = b.updates[0].evidence;
        let Leg::Pair { order, .. } = &b.legs[ev] else { panic!() };
        let c = order.utxo.covenant_id.unwrap();
        b.legs[ev] = pair_leg(pair(MAKER_C, false, pa, pb, MAX_STOP_PRICE + 1_000 * step), pa, pb, c, 72, 1);
        if let AnyState::KobCondPair(x) = &mut b.updates[0].order.state {
            x.stop_price = stop;
            x.tp_price = 0;
        }
        let mut built = match build(&Action::Batch(b)) {
            Ok(x) => x,
            Err(e) => return (Err(format!("builder: {e}")), 0),
        };
        let u = built
            .plans
            .iter()
            .position(|p| matches!(p, SigPlan::Entry { template, .. } if *template == TemplateId::KobCondPair))
            .unwrap();
        let SigPlan::Entry { template, state, entry, mut args } = built.plans[u].clone() else { unreachable!() };
        // k is the settle argument the builder set (its 15th)
        let ki = args.len() - 2;
        let Arg::Int(k) = args[ki] else { panic!("k") };
        if let Some(k2) = k_override {
            // the filler names another k: the continuation's stop follows it
            args[ki] = Arg::Int(k2);
            let AnyState::KobCondPair(s) = AnyState::decode(template, &state).unwrap() else { panic!() };
            let old = spk_to_string(
                &AnyState::KobCondPair(kob_protocol::state::CondPairState { stop_price: s.stop_price + k * step, ..s.clone() }).spk(),
            );
            let new = spk_to_string(
                &AnyState::KobCondPair(kob_protocol::state::CondPairState { stop_price: s.stop_price + k2 * step, ..s.clone() }).spk(),
            );
            for o in built.tx.outputs.iter_mut().filter(|o| o.script_public_key == old) {
                o.script_public_key = new.clone();
            }
            built.plans[u] = SigPlan::Entry { template, state, entry, args };
            let (tx, entries) = built.tx.to_tx().unwrap();
            for r in built.sign.iter_mut() {
                r.sighash = sighash(&tx, &entries, r.input_index);
            }
        }
        let s = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        (validate_signed(&s).map(|_| ()).map_err(|e| e.to_string()), k)
    };
    // one step below: the builder names k = 1, onto MAX_STOP, and the covenant takes it as the cap
    let (r, k) = run(MAX_STOP_PRICE - step, None);
    assert_eq!(k, 1);
    r.expect("one step below MAX_STOP");
    // a k past MAX_STOP is refused
    assert!(run(MAX_STOP_PRICE - step, Some(2)).0.is_err(), "past MAX_STOP");
    // two steps below: k = 2 is the largest; k = 1 is not (the cap does not bind there)
    let (r, k) = run(MAX_STOP_PRICE - 2 * step, None);
    assert_eq!(k, 2);
    r.expect("two steps below");
    assert!(run(MAX_STOP_PRICE - 2 * step, Some(1)).0.is_err(), "k = 1 is not the largest");
}
