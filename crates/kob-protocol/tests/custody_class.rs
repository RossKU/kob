//! An order's custody is known by the whole state its order pins: token, owner, amount and class
//! (R8F3c-1).
//!
//! A unit of the token's covenant id with another extension commitment, owned by the order and of
//! the same amount, is a decoy: a cancel that spent it as the custody would end the covenant and
//! strand the real custody for ever (a 0x04 owner needs an input of that covenant id). Every
//! builder that spends a custody refuses it.

mod common;

use common::pair::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, sign_locally, BuiltTx, FinalizeOptions, OrderUtxo, TokenUtxo};
use kob_protocol::verify::validate_signed;

/// The covenant id of the order.
const O: [u8; 32] = [0xa1; 32];
/// The decoy's class.
const DECOY_EXT: [u8; 32] = [0x99; 32];

fn engine(built: &BuiltTx, label: &str) {
    let signed = finalize(built, &sign_locally(built, &keys()).unwrap(), FinalizeOptions::default())
        .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
    validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}"));
}

/// Every token-holding kind on the KCC-20 program `t` with its value.
fn holding(t: TemplateId) -> Vec<(&'static str, AnyState, u64)> {
    let ia = IfdAskState { min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A, t) };
    vec![
        ("ask", AnyState::KobAsk(ask(MAKER_A, P250, t)), CARRIER),
        ("condAsk", AnyState::KobCondAsk(cond_ask(MAKER_A, t)), CARRIER),
        ("ifdAsk", AnyState::KobIfdAsk(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64),
        ("pair", AnyState::KobPair(pair(MAKER_A, true, t, t, RATE)), PV),
        ("pairCondBid", AnyState::KobCondPair(cond_pair_bid(MAKER_A, t, t)), PV),
        ("pairIfdAsk", AnyState::KobIfdPair(ifd_pair(MAKER_A, false, t, t)), ifd_value(&ifd_pair(MAKER_A, false, t, t))),
    ]
}

/// The real custodies of `s` (in record order), owned by `O`.
fn real(t: TemplateId, s: &AnyState) -> Vec<TokenUtxo> {
    s.custodies().into_iter().enumerate().map(|(k, (tok, amount))| tutxo(t, tok, 71 + k as u8, amount, O, true)).collect()
}

/// The same UTXO as `c` at another outpoint, with another class.
fn decoy(c: &TokenUtxo, tag: u8) -> TokenUtxo {
    let mut d = tutxo(TemplateId::Kcc20Ref, c.utxo.covenant_id.unwrap(), tag, c.state.amount(), O, true);
    d.state = c.state.clone();
    if let TokenState::Kcc20(k) = &mut d.state {
        k.extension_commitment = DECOY_EXT;
    }
    d
}

fn cancel(o: OrderUtxo<AnyState>, custody: &[TokenUtxo], strays: Vec<TokenUtxo>) -> CancelOrder {
    CancelOrder {
        retired: None,
        prefund: custody.get(1).cloned(),
        order: o,
        custody: custody.first().cloned(),
        strays,
        foreign: vec![],
        tokens: vec![],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    }
}

fn position(o: OrderUtxo<AnyState>, custody: &[TokenUtxo]) -> CancelPosition {
    CancelPosition {
        orders: vec![CancelItem {
            order: o,
            retired: None,
            custody: custody.first().cloned(),
            prefund: custody.get(1).cloned(),
            strays: vec![],
        }],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        lock_time: 0,
        token_carrier: None,
        records: vec![],
        fee: fee(),
    }
}

#[test]
fn a_cancel_refuses_a_decoy_of_another_class_as_the_custody() {
    for t in [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8] {
        for (name, s, value) in holding(t) {
            let label = format!("{name}@{}", t.name());
            let o = order(70, value, O, 1_000, s.clone());
            let good = real(t, &s);
            assert!(!good.is_empty(), "{label}: holds a custody");
            // the real custody cancels, and the engine accepts it
            let built = build_cancel_order(&cancel(o.clone(), &good, vec![]), &kob_protocol::budget::lookup)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            build_cancel_position(&position(o.clone(), &good), &kob_protocol::budget::lookup)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            // each custody replaced by its decoy is refused, by the cancel and by the position cancel
            for k in 0..good.len() {
                let mut bad = good.clone();
                bad[k] = decoy(&good[k], 90 + k as u8);
                let e =
                    build_cancel_order(&cancel(o.clone(), &bad, vec![]), &kob_protocol::budget::lookup).expect_err(&label).to_string();
                assert!(e.contains("extension commitment"), "{label}: {e}");
                let e =
                    build_cancel_position(&position(o.clone(), &bad), &kob_protocol::budget::lookup).expect_err(&label).to_string();
                assert!(e.contains("extension commitment"), "{label}: {e}");
                // a keeper's refund refuses it too
                if s.refund_due(0).is_some() {
                    let r = RefundOrder {
                        prefund: bad.get(1).cloned(),
                        bought_custody: None,
                        order: o.clone(),
                        custody: bad.first().cloned(),
                        foreign: vec![],
                        lock_time: EXPIRY as u64,
                        funding: vec![key_utxo(4, KEEPER, 5 * KAS)],
                        change: Some(pk(KEEPER)),
                        fee: fee(),
                    };
                    assert!(build_refund_order(&r, &kob_protocol::budget::lookup).is_err(), "{label}: refund");
                }
            }
            // the decoy cannot leave with the real custody either: one KCC-20 transfer keeps one class
            let stray = decoy(&good[0], 95);
            assert!(
                build_cancel_order(&cancel(o.clone(), &good, vec![stray]), &kob_protocol::budget::lookup).is_err(),
                "{label}: a stray of another class leaves in its own sweep"
            );
        }
    }
}

/// Two identical candidates (the custody and a stray of the same class and amount): either is the custody, provided the
/// other leaves with it, and every unit goes back to the maker.
#[test]
fn a_twin_of_the_custody_leaves_with_it() {
    let t = TemplateId::Kcc20Ref8x8;
    let s = AnyState::KobAsk(ask(MAKER_A, P250, t));
    let o = order(70, CARRIER, O, 1_000, s.clone());
    let good = real(t, &s);
    let twin = tutxo(t, TOKEN_COV, 79, good[0].state.amount(), O, true);
    assert_eq!(twin.state, good[0].state);
    for (c, st) in [(good[0].clone(), twin.clone()), (twin.clone(), good[0].clone())] {
        let built = build_cancel_order(&cancel(o.clone(), &[c], vec![st]), &kob_protocol::budget::lookup).unwrap();
        engine(&built, "twin");
        assert!(built.tx.outputs.iter().all(|x| x.covenant.as_ref().is_none_or(|c| c.covenant_id != O)), "the order ends");
    }
}

/// A cancel-replace whose replacement pins another class than the custody it takes could never fill.
#[test]
fn a_replacement_pins_the_class_of_the_tokens_it_takes() {
    let t = TemplateId::Kcc20Ref8x8;
    let a = ask(MAKER_A, P250, t);
    let s = AnyState::KobAsk(a.clone());
    let o = order(70, CARRIER, O, 1_000, s.clone());
    let good = real(t, &s);
    let mut c = cancel(o, &good, vec![]);
    let rep = |ext: [u8; 32]| Replacement {
        order: AnyState::KobAsk(AskState { extension_commitment: ext, ..a.clone() }),
        value: CARRIER,
        token_carrier: None,
        deadline: None,
    };
    c.replace = Some(rep(EXT));
    build_cancel_order(&c, &kob_protocol::budget::lookup).unwrap();
    c.replace = Some(rep(DECOY_EXT));
    let e = build_cancel_order(&c, &kob_protocol::budget::lookup).unwrap_err().to_string();
    assert!(e.contains("extension commitment"), "{e}");
}

/// The one rule a decoder shares with the builders: a spend that ends an order releases every token UTXO its state holds,
/// each by its whole state. The real custodies pass; a decoy of another class in place of any of them, or a missing one,
/// does not.
#[test]
fn an_ending_spend_releases_exactly_what_the_state_holds() {
    let t = TemplateId::Kcc20Ref8x8;
    for (name, s, _) in holding(t) {
        let good = real(t, &s);
        let held = held_custodies(&s, O).unwrap();
        assert_eq!(held.len(), good.len(), "{name}");
        assert!(held.iter().zip(&good).all(|(h, g)| h.state == g.state && Some(h.token.covenant_id) == g.utxo.covenant_id), "{name}");
        check_releases_held(&s, O, &good).unwrap_or_else(|e| panic!("{name}: {e}"));
        for k in 0..good.len() {
            let mut bad = good.clone();
            bad[k] = decoy(&good[k], 90);
            assert!(check_releases_held(&s, O, &bad).is_err(), "{name}: a decoy in place of custody {k}");
            let mut short = good.clone();
            short.remove(k);
            assert!(check_releases_held(&s, O, &short).is_err(), "{name}: custody {k} left behind");
        }
    }
    // a mode-1 bid's bought-custody is held too
    let b = BidState { mode: MODE_ACCUMULATE, bought: 4 * WHOLE, ..bid(MAKER_A, P245, t) };
    let s = AnyState::KobBid(b);
    let held = held_custodies(&s, O).unwrap();
    assert_eq!((held.len(), held[0].bought, held[0].state.amount()), (1, true, 4 * WHOLE));
    assert!(check_releases_held(&s, O, &[tutxo(t, TOKEN_COV, 72, 4 * WHOLE, O, true)]).is_ok());
    assert!(check_releases_held(&s, O, &[]).is_err());
}

fn sweep_of(o: OrderUtxo<AnyState>, strays: Vec<TokenUtxo>) -> SweepOrder {
    SweepOrder {
        order: o,
        strays,
        foreign: vec![],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        token_carrier: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    }
}

/// R8FX-1, R8FX2-2: a spend that keeps the order alive never takes a unit of exactly a custody's state, with no exception.
/// The real custody handed in as a stray (an indexer that left it out of the view) is refused, and so is a twin (the same
/// state at another outpoint), whatever a caller says it keeps: which of the two is "the custody" cannot be told from the
/// caller's word. A twin stays with the order, and the cancel takes it along with the custody. An amend spends no token.
#[test]
fn a_spend_that_keeps_the_order_never_takes_its_custody() {
    let t = TemplateId::Kcc20Ref8x8;
    for (name, s, value) in holding(t) {
        let label = format!("{name}@{}", t.name());
        let o = order(70, value, O, 1_000, s.clone());
        let good = real(t, &s);
        let mut twin = good[0].clone();
        twin.utxo.transaction_id = [0x7e; 32];
        for strays in [vec![good[0].clone()], vec![twin.clone()], vec![twin.clone(), good[0].clone()]] {
            let e = build_sweep_order(&sweep_of(o.clone(), strays), &kob_protocol::budget::lookup).expect_err(&label).to_string();
            assert!(e.contains("never moves a unit of exactly the state the order pins"), "{label}: {e}");
        }
        // the hostile-node shape of R8FX2-2: an invented twin named as the custody and the real one listed as a stray
        let request =
            serde_json::json!({"order": o, "strays": [good[0]], "kept": [twin], "funding": [key_utxo(3, MAKER_A, 10 * KAS)]});
        let r: Result<SweepOrder, _> = serde_json::from_value(request);
        if let Ok(r) = r {
            assert!(build_sweep_order(&r, &kob_protocol::budget::lookup).is_err(), "{label}: no field lets a custody go");
        }
    }
    // the cancel takes the twin along with the custody
    let s = AnyState::KobAsk(ask(MAKER_A, P250, t));
    let o = order(70, CARRIER, O, 1_000, s.clone());
    let good = real(t, &s);
    let mut twin = good[0].clone();
    twin.utxo.transaction_id = [0x7e; 32];
    let built = build_cancel_order(&cancel(o.clone(), &good, vec![twin]), &kob_protocol::budget::lookup).unwrap();
    engine(&built, "a cancel with the custody's twin");
    // an amend names the custody of an unbound curve only to bind it, and never spends a token
    let a = ask(MAKER_A, P250, t);
    let o = order(70, CARRIER, O, 1_000, AnyState::KobAsk(a.clone()));
    let good = real(t, &o.state);
    let r = AmendOrder {
        order: o,
        amended: AnyState::KobAsk(AskState { price: P255, ..a }),
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: fee(),
        custody: Some(good[0].clone()),
    };
    let built = build_amend_order(&r, &kob_protocol::budget::lookup).unwrap();
    assert!(built.tx.inputs.iter().all(|i| i.utxo.covenant_id.is_none_or(|c| c == O)), "an amend spends no token input");
}

/// The unit `c` with a non-zero borrow guard and borrowing disabled: for the order covenants the same custody (R9FA-1).
fn guarded(c: &TokenUtxo, tag: u8) -> TokenUtxo {
    let mut d = c.clone();
    d.utxo.transaction_id = [tag; 32];
    if let TokenState::Kcc20(k) = &mut d.state {
        k.borrow_guard = [0x5a; 32];
    }
    d
}

/// R9FA-1: the order covenants compare a sell custody without its borrow guard, so the tools use that comparison where
/// it keeps value safe. A sweep never takes a unit that differs from the custody in its guard alone (the covenant would
/// take it as the custody). A cancel releases such a unit: as a stray next to the custody, or as the custody itself when it
/// is the only one left (an order a hand-built fill left with a guarded custody can still be cancelled). A fill by KOB's
/// builders still uses only the exact, zero-guard custody. A bought-custody is pinned exactly, guard included.
#[test]
fn a_custody_is_what_the_covenant_accepts_whatever_its_guard() {
    let t = TemplateId::Kcc20Ref8x8;
    for (name, s, value) in holding(t) {
        let label = format!("{name}@{}", t.name());
        let o = order(70, value, O, 1_000, s.clone());
        let good = real(t, &s);
        let d = guarded(&good[0], 0x5d);
        // TPLGEN row 2: the pinned templates compare the guard, so a guarded unit is a plain stray. The sweep returns it,
        // the cancel takes it next to the custody, and it never stands in for the custody (the builders refuse it, and
        // the covenant would). The tolerant reading of the earlier templates stays for the retired ones (cancel-only).
        let sw = build_sweep_order(&sweep_of(o.clone(), vec![d.clone()]), &kob_protocol::budget::lookup)
            .unwrap_or_else(|e| panic!("{label}: sweep: {e}"));
        engine(&sw, &format!("{label} sweep"));
        let built = build_cancel_order(&cancel(o.clone(), &good, vec![d.clone()]), &kob_protocol::budget::lookup)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        engine(&built, &label);
        let mut only = good.clone();
        only[0] = d.clone();
        let e = build_cancel_order(&cancel(o.clone(), &only, vec![]), &kob_protocol::budget::lookup).expect_err(&label).to_string();
        assert!(e.contains("not exactly the one its order pins"), "{label}: {e}");
        let only = good.clone();
        // R9FA2-1: everything comes back to the maker. The order's KAS (an if-done ask's prefund with it) is in no
        // covenant output, and every token unit, the guarded one included, leaves as a plain unit of the maker.
        let maker = kob_protocol::tx::spk_to_string(&kob_protocol::script::p2pk_spk(&pk(MAKER_A)));
        assert!(built.tx.outputs.iter().all(|x| x.covenant.as_ref().is_none_or(|c| c.covenant_id != O)), "{label}: the order ends");
        let kas_back: u64 =
            built.tx.outputs.iter().filter(|x| x.covenant.is_none() && x.script_public_key == maker).map(|x| x.value).sum();
        assert!(kas_back + built.fee.fee >= value, "{label}: the order's KAS returns to the maker ({kas_back} of {value})");
        // a keeper's refund at expiry ends the order with the guarded custody too
        if s.refund_due(0).is_some() {
            let r = RefundOrder {
                prefund: only.get(1).cloned(),
                bought_custody: None,
                order: o.clone(),
                custody: only.first().cloned(),
                foreign: vec![],
                lock_time: EXPIRY as u64,
                funding: vec![key_utxo(4, KEEPER, 5 * KAS)],
                change: Some(pk(KEEPER)),
                fee: fee(),
            };
            let built = build_refund_order(&r, &kob_protocol::budget::lookup).unwrap_or_else(|e| panic!("{label}: refund: {e}"));
            engine(&built, &format!("{label} refund"));
        }
    }
    // a fill never takes a guarded custody
    let mut leg = ev_ask(P250, 1, t);
    if let kob_protocol::build::Leg::Ask { custody, .. } = &mut leg {
        *custody = guarded(custody, 0x5e);
    }
    let b = Batch {
        lock_time: NOW,
        legs: vec![leg],
        updates: vec![],
        taker_tokens: vec![],
        taker: Some(pk(MAKER_B)),
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![key_utxo(3, MAKER_B, 100 * KAS)],
        change: None,
        records: vec![],
        fee: fee(),
    };
    let e = build_batch(&b, &kob_protocol::budget::lookup).unwrap_err().to_string();
    assert!(e.contains("guard") || e.contains("custody"), "{e}");
    // a bought-custody with a guard is not the bought-custody
    let bs = BidState { mode: MODE_ACCUMULATE, bought: 4 * WHOLE, ..bid(MAKER_A, P245, t) };
    let s = AnyState::KobBid(bs);
    let bc = tutxo(t, TOKEN_COV, 72, 4 * WHOLE, O, true);
    assert!(check_releases_held(&s, O, &[guarded(&bc, 0x5f)]).is_err());
}

/// R9FA-1 / R9FA2-1, by execution: a hand-made partial fill of a `KobAsk` whose custody input is a unit that differs
/// from the honest custody ONLY in its borrow guard (borrowing disabled), with the rest output carrying that guard, as
/// the contract copies it. KOB's fill builders refuse such a custody, so the transaction is the builder's honest fill
/// with the custody input's state, its script and the rest output's state and script replaced, then re-signed.
/// The engine runs the real order and token scripts on it.
#[test]
fn the_engine_takes_a_guarded_unit_as_the_custody_of_a_fill() {
    use kob_protocol::tx::{sighash, spk_to_string, SigPlan};
    let t = TemplateId::Kcc20Ref;
    let honest = Batch {
        lock_time: NOW,
        legs: vec![ev_ask(P250, 2, t)],
        updates: vec![],
        taker_tokens: vec![],
        taker: Some(pk(MAKER_B)),
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![key_utxo(3, MAKER_B, 100 * KAS)],
        change: None,
        records: vec![],
        fee: fee(),
    };
    let mut built = build_batch(&honest, &kob_protocol::budget::lookup).unwrap();
    engine(&built, "honest partial fill");
    let Leg::Ask { order, .. } = &honest.legs[0] else { panic!() };
    let order_cov = order.utxo.covenant_id.unwrap();
    let guard = [0x5a; 32];
    // the custody input (the token leader) and the rest output (the unit owned by the order, scheme 4)
    let k = built.plans.iter().position(|p| matches!(p, SigPlan::TokenLeader { .. })).expect("a token leader");
    let (old_rest, new_rest) = {
        let SigPlan::TokenLeader { state, next_states, template, .. } = &mut built.plans[k] else { unreachable!() };
        assert_eq!((state.owner, state.borrow_scheme), (order_cov, 0), "the leader is the custody");
        state.borrow_guard = guard;
        let r = next_states.iter_mut().find(|s| s.owner == order_cov).expect("the rest of the custody");
        let old = spk_to_string(&r.spk_with(kob_protocol::artifacts::template(*template)));
        r.borrow_guard = guard;
        (old, spk_to_string(&r.spk_with(kob_protocol::artifacts::template(*template))))
    };
    let honest_spk = built.tx.inputs[k].utxo.script_public_key.clone();
    built.tx.inputs[k].utxo.script_public_key = spk_to_string(&built.plans[k].spk());
    assert_ne!(built.tx.inputs[k].utxo.script_public_key, honest_spk, "the custody input is another UTXO (the guarded unit)");
    let o = built.tx.outputs.iter().position(|x| x.script_public_key == old_rest).expect("the rest output");
    // control: the guarded custody with the rest WITHOUT the guard is refused (the scripts do run on these states)
    {
        let mut bad = built.clone();
        if let SigPlan::TokenLeader { next_states, .. } = &mut bad.plans[k] {
            next_states.iter_mut().filter(|s| s.owner == order_cov).for_each(|s| s.borrow_guard = [0; 32]);
        }
        let (tx, entries) = bad.tx.to_tx().unwrap();
        for r in bad.sign.iter_mut() {
            r.sighash = sighash(&tx, &entries, r.input_index);
        }
        let s = finalize(&bad, &sign_locally(&bad, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        assert!(validate_signed(&s).is_err(), "a rest that drops the guard must be refused");
    }
    built.tx.outputs[o].script_public_key = new_rest;
    let (tx, entries) = built.tx.to_tx().unwrap();
    for r in built.sign.iter_mut() {
        r.sighash = sighash(&tx, &entries, r.input_index);
    }
    let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
    // TPLGEN row 2: the templates before it accepted this fill (run on 6ca8c0544); the zero-guard check refuses it
    let e = validate_signed(&signed).expect_err("a guarded unit is no custody").to_string();
    assert!(e.contains("input 0"), "{e}");
    // and the order this leaves (rest custody guarded) is ended by the tools: covered by
    // `a_custody_is_what_the_covenant_accepts_whatever_its_guard`
}

/// The same surgery on a sell-first if-done entry's partial fill (`KobIfdAsk`, R9FA2-1): the engine takes the guarded
/// unit as the entry's custody, so the state whose cancel and refund the tools must handle is one the chain reaches.
#[test]
fn the_engine_takes_a_guarded_unit_as_the_custody_of_an_if_done_entry_fill() {
    use kob_protocol::tx::{sighash, spk_to_string, SigPlan};
    let (_, a) = scenarios_on(TemplateId::Kcc20Ref).into_iter().find(|(n, _)| n == "ifd.ask.partial").expect("scenario");
    let mut built = kob_protocol::build::build(&a).unwrap();
    engine(&built, "honest if-done entry fill");
    let order_cov = built
        .plans
        .iter()
        .zip(&built.tx.inputs)
        .find_map(|(p, i)| matches!(p, SigPlan::Entry { .. }).then_some(i.utxo.covenant_id).flatten())
        .unwrap();
    let k = built
        .plans
        .iter()
        .position(|p| matches!(p, SigPlan::TokenLeader { state, .. } if state.owner == order_cov))
        .expect("the entry's custody leads its token");
    let (old_rest, new_rest) = {
        let SigPlan::TokenLeader { state, next_states, template, .. } = &mut built.plans[k] else { unreachable!() };
        state.borrow_guard = [0x5a; 32];
        let r = next_states.iter_mut().find(|s| s.owner == order_cov).expect("the rest of the custody");
        let old = spk_to_string(&r.spk_with(kob_protocol::artifacts::template(*template)));
        r.borrow_guard = [0x5a; 32];
        (old, spk_to_string(&r.spk_with(kob_protocol::artifacts::template(*template))))
    };
    built.tx.inputs[k].utxo.script_public_key = spk_to_string(&built.plans[k].spk());
    let o = built.tx.outputs.iter().position(|x| x.script_public_key == old_rest).expect("the rest output");
    built.tx.outputs[o].script_public_key = new_rest;
    let (tx, entries) = built.tx.to_tx().unwrap();
    for r in built.sign.iter_mut() {
        r.sighash = sighash(&tx, &entries, r.input_index);
    }
    let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
    // TPLGEN row 2: accepted before it (run on 6ca8c0544), refused now
    let e = validate_signed(&signed).expect_err("a guarded unit is no custody").to_string();
    assert!(e.contains("input 0"), "{e}");
}
