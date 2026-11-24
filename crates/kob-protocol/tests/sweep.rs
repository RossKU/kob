//! Tests the strays of an order (`matcher.md` §1.2): the maker's sweep and the return of foreign
//! strays.
//!
//! The maker's sweep goes IN PLACE (`SweepOrder`: the maker's `cancel` continues the order with the
//! same script, and a `SWEEP` record names the continuation) for every order kind. Foreign strays
//! (tokens of another covenant id owned by the order id) are returned to the maker by the maker's
//! cancel and sweep and by a permissionless refund / kill. A mode-1 order's bought-custody is never
//! swept (a claim moves it), and a cancel that ends the order must release it. Every transaction is
//! validated in the rusty-kaspa v2.1.0 engine.

mod common;

use common::pair::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::payload::{decode, verify_sweep, Record};
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, sign_locally, BuiltTx, FinalizeOptions, OrderUtxo, TokenUtxo};
use kob_protocol::verify::validate_signed;

/// The covenant id of the swept order.
const O: [u8; 32] = [0xa1; 32];
/// A third token, foreign to every order here.
const TOKEN_F: [u8; 32] = [0x72; 32];

const PROGRAMS: [TemplateId; 3] = [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433];

fn engine(built: &BuiltTx, label: &str) {
    let signed = finalize(built, &sign_locally(built, &keys()).unwrap(), FinalizeOptions::default())
        .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
    let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
    assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
}

/// Every order kind on program `t` with the KAS value it holds (the pair orders trade A and B of the same program).
fn kinds(t: TemplateId) -> Vec<(&'static str, AnyState, u64)> {
    let f = t.family();
    let bs = bid(MAKER_A, P245, t);
    let cb = cond_bid(MAKER_A, t);
    let ib = IfdBidState { min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10, t) };
    let ia = IfdAskState { min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A, t) };
    vec![
        ("ask", AnyState::KobAsk(ask(MAKER_A, P250, t)).into_family(f), CARRIER),
        ("bid", AnyState::KobBid(bs.clone()).into_family(f), bs.escrow(10 * WHOLE, 3).unwrap() as u64),
        ("condAsk", AnyState::KobCondAsk(cond_ask(MAKER_A, t)).into_family(f), CARRIER),
        ("condBid", AnyState::KobCondBid(cb.clone()).into_family(f), cb.escrow(2).unwrap() as u64),
        ("ifdBid", AnyState::KobIfdBid(ib.clone()).into_family(f), ib.escrow().unwrap() as u64),
        ("ifdAsk", AnyState::KobIfdAsk(ia.clone()).into_family(f), ia.escrow(CARRIER as i64).unwrap() as u64),
        ("pair", AnyState::KobPair(pair(MAKER_A, true, t, t, RATE)), PV),
        ("pairCondBid", AnyState::KobCondPair(cond_pair_bid(MAKER_A, t, t)), PV),
        ("pairIfdAsk", AnyState::KobIfdPair(ifd_pair(MAKER_A, false, t, t)), ifd_value(&ifd_pair(MAKER_A, false, t, t))),
    ]
}

fn foreign(pf: TemplateId, tags: &[u8]) -> ForeignStrays {
    ForeignStrays {
        token: TokenRef { covenant_id: TOKEN_F, program: pf },
        utxos: tags.iter().map(|t| tutxo(pf, TOKEN_F, *t, 7 + *t as i64, O, true)).collect(),
    }
}

fn sweep(order: OrderUtxo<AnyState>, strays: Vec<TokenUtxo>, foreign: Vec<ForeignStrays>, funded: bool) -> SweepOrder {
    SweepOrder {
        order,
        strays,
        foreign,
        funding: if funded { vec![key_utxo(3, MAKER_A, 10 * KAS)] } else { vec![] },
        change: None,
        token_carrier: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    }
}

#[test]
fn every_kind_is_swept_in_place_and_stays_the_same_order() {
    for t in PROGRAMS {
        // the foreign token is of the other family's program (a KRON stray on a KCC-20 order and the reverse)
        let pf = if t.family() == kob_protocol::Family::Kron { TemplateId::Kcc20Ref } else { TemplateId::KronToken2433 };
        for (name, state, value) in kinds(t) {
            let label = format!("{name}@{}", t.name());
            let o = order(70, value, O, 1_000, state.clone());
            let mut strays = vec![tutxo(t, TOKEN_COV, 76, 3, O, true), tutxo(t, TOKEN_COV, 78, 4, O, true)];
            if name.starts_with("pair") {
                strays.push(tutxo(t, TOKEN_B, 77, 5, O, true));
            }
            let req = sweep(o, strays, vec![foreign(pf, &[80, 81])], true);
            let built = build_sweep_order(&req, &kob_protocol::budget::lookup).unwrap_or_else(|e| panic!("{label}: build: {e}"));
            engine(&built, &label);
            // the continuation is the same script under the same covenant id, the record names it
            let tx = &built.tx;
            assert_eq!(tx.outputs[0].script_public_key, tx.inputs[0].utxo.script_public_key, "{label}");
            assert_eq!(tx.outputs[0].value, value, "{label}: a funded sweep keeps the order's value");
            let p = decode(&tx.payload).unwrap().unwrap();
            assert_eq!(p.records, vec![Record::Sweep { output: 0, input: 0 }], "{label}");
            let s = verify_sweep(tx, &p.records[0], &state, "cancel").unwrap_or_else(|e| panic!("{label}: verify: {e}"));
            assert_eq!(s.covenant_id, O);
            assert!(verify_sweep(tx, &p.records[0], &state, "settle").is_err(), "{label}: only the maker's cancel sweeps");
            // every swept token goes to the maker: 3 + 4 own units, (pair orders) 5 of B, (7 + 80) + (7 + 81) foreign units
            let want = 7 + if name.starts_with("pair") { 5 } else { 0 } + 87 + 88;
            assert_eq!(maker_units(&built), want, "{label}");
        }
    }
}

#[test]
fn a_plain_ask_pays_its_sweep_from_its_carrier_every_other_kind_needs_funding() {
    let t = TemplateId::Kcc20Ref8x8;
    for (name, state, value) in kinds(t) {
        let o = order(70, value, O, 1_000, state);
        let req = sweep(o, vec![tutxo(t, TOKEN_COV, 76, 3, O, true)], vec![], false);
        match name {
            "ask" => {
                let built = build_sweep_order(&req, &kob_protocol::budget::lookup).unwrap();
                engine(&built, name);
                let left = built.tx.outputs[0].value;
                assert!(left < value && left >= MIN_AMEND_CARRIER, "the carrier pays the fee: {left}");
            }
            _ => assert!(build_sweep_order(&req, &kob_protocol::budget::lookup).is_err(), "{name}: an unfunded sweep is refused"),
        }
    }
}

#[test]
fn sweep_refusals() {
    let t = TemplateId::Kcc20Ref;
    let state = AnyState::KobAsk(ask(MAKER_A, P250, t));
    let o = || order(70, CARRIER, O, 1_000, state.clone());
    let b = |r: &SweepOrder| build_sweep_order(r, &kob_protocol::budget::lookup);
    // nothing to sweep
    assert!(b(&sweep(o(), vec![], vec![], true)).is_err());
    // more strays of one token than the program's token inputs (3/3): the rest goes in another sweep
    let many: Vec<TokenUtxo> = (0..4).map(|k| tutxo(t, TOKEN_COV, 90 + k, 1, O, true)).collect();
    assert!(b(&sweep(o(), many.clone(), vec![], true)).is_err());
    assert!(b(&sweep(o(), many[..3].to_vec(), vec![], true)).is_ok());
    // two extension commitments of one token
    let mut other = tutxo(t, TOKEN_COV, 95, 1, O, true);
    if let TokenState::Kcc20(k) = &mut other.state {
        k.extension_commitment = [0x11; 32];
    }
    assert!(b(&sweep(o(), vec![tutxo(t, TOKEN_COV, 96, 1, O, true), other], vec![], true)).is_err());
    // a stray owned by another id, a foreign token passed as a stray, a foreign group naming the order's own token
    assert!(b(&sweep(o(), vec![tutxo(t, TOKEN_COV, 97, 1, [0x5a; 32], true)], vec![], true)).is_err());
    assert!(b(&sweep(o(), vec![tutxo(t, TOKEN_F, 98, 1, O, true)], vec![], true)).is_err());
    let own =
        ForeignStrays { token: TokenRef { covenant_id: TOKEN_COV, program: t }, utxos: vec![tutxo(t, TOKEN_COV, 99, 1, O, true)] };
    assert!(b(&sweep(o(), vec![], vec![own], true)).is_err());
}

#[test]
fn foreign_strays_go_back_to_the_maker_with_a_cancel_and_with_a_permissionless_refund() {
    for t in PROGRAMS {
        let pf = if t.family() == kob_protocol::Family::Kron { TemplateId::Kcc20Ref8x8 } else { TemplateId::KronToken2732 };
        // the maker's cancel of an ask with custody, an own stray and foreign strays
        let a = AnyState::KobAsk(ask(MAKER_A, P250, t)).into_family(t.family());
        let c = CancelOrder {
            retired: None,
            prefund: None,
            order: order(70, CARRIER, O, 1_000, a.clone()),
            custody: Some(tutxo(t, TOKEN_COV, 71, 10 * WHOLE, O, true)),
            strays: vec![tutxo(t, TOKEN_COV, 76, 3, O, true)],
            foreign: vec![foreign(pf, &[80, 81])],
            tokens: vec![],
            funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
            change: None,
            replace: None,
            lock_time: 0,
            records: vec![],
            fee: fee(),
        };
        let built = build_cancel_order(&c, &kob_protocol::budget::lookup).unwrap_or_else(|e| panic!("{}: {e}", t.name()));
        engine(&built, &format!("cancel@{}", t.name()));
        // a keeper's refund of the expired ask (custody) and of a bid (no custody) carries the foreign strays to the maker
        for (name, state, value, custody) in [
            ("ask", a.clone(), CARRIER, Some(tutxo(t, TOKEN_COV, 71, 10 * WHOLE, O, true))),
            (
                "bid",
                AnyState::KobBid(bid(MAKER_A, P245, t)).into_family(t.family()),
                bid(MAKER_A, P245, t).escrow(10 * WHOLE, 3).unwrap() as u64,
                None,
            ),
        ] {
            let label = format!("refund {name}@{}", t.name());
            let r = RefundOrder {
                prefund: None,
                bought_custody: None,
                order: order(70, value, O, 1_000, state),
                custody,
                foreign: vec![foreign(pf, &[80])],
                lock_time: EXPIRY as u64,
                funding: vec![key_utxo(4, KEEPER, 5 * KAS)],
                change: Some(pk(KEEPER)),
                fee: fee(),
            };
            let built = build_refund_order(&r, &kob_protocol::budget::lookup).unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            // the foreign units and their carrier go to the maker, never to the keeper
            let maker_out = built.tx.outputs.iter().filter(|o| o.value == CARRIER).count();
            assert!(maker_out >= 1, "{label}: the foreign stray's carrier returns with it");
            // the order's own token is never foreign
            let mut bad = r.clone();
            bad.foreign = vec![ForeignStrays { token: TokenRef { covenant_id: TOKEN_COV, program: t }, utxos: vec![] }];
            assert!(build_refund_order(&bad, &kob_protocol::budget::lookup).is_err(), "{label}");
        }
    }
}

/// Token units the transaction's token transfers deliver to the maker's key (each token's next states, counted once: a KRON
/// plan repeats its group's next states on every token input).
fn maker_units(built: &BuiltTx) -> i64 {
    let maker = pk(MAKER_A);
    let mut seen = std::collections::BTreeSet::new();
    let mut units = 0;
    for (p, inp) in built.plans.iter().zip(&built.tx.inputs) {
        match p {
            kob_protocol::tx::SigPlan::TokenLeader { next_states, .. } => {
                units +=
                    next_states.iter().filter(|s| s.owner == maker && s.owner_scheme == SCHEME_P2PK).map(|s| s.amount).sum::<i64>();
            }
            kob_protocol::tx::SigPlan::KronToken { next_states, .. } if seen.insert(inp.utxo.covenant_id) => {
                units += next_states
                    .iter()
                    .filter(|s| s.owner == maker && s.id_type != kob_protocol::family::KRON_TYPE_COVID)
                    .map(|s| s.amount)
                    .sum::<i64>();
            }
            _ => {}
        }
    }
    units
}

// ---------------------------------------------------------------- a mode-1 order's bought-custody (bid-delivery-spec §5, §7)

/// A mode-1 bid holding 4 whole in its bought-custody, a `KobPair` ask holding 3 whole of B in its own, each with that custody.
fn held(t: TemplateId) -> Vec<(&'static str, OrderUtxo<AnyState>, Option<TokenUtxo>, TokenUtxo)> {
    let b = BidState { mode: MODE_ACCUMULATE, bought: 4 * WHOLE, ..bid(MAKER_A, P245, t) };
    let x = PairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..pair(MAKER_A, true, t, t, RATE) };
    vec![
        (
            "bid",
            order(70, b.escrow(10 * WHOLE, 1).unwrap() as u64, O, 1_000, AnyState::KobBid(b)),
            None,
            tutxo(t, TOKEN_COV, 72, 4 * WHOLE, O, true),
        ),
        (
            "pair",
            order(70, PV, O, 1_000, AnyState::KobPair(x)),
            Some(tutxo(t, TOKEN_COV, 71, 10 * WHOLE, O, true)),
            tutxo(t, TOKEN_B, 72, 3 * WHOLE, O, true),
        ),
    ]
}

/// A sweep continues the order with the same `bought`, so it never moves the bought-custody (a claim does); a stray of
/// another amount is swept as before.
#[test]
fn a_sweep_never_moves_the_bought_custody() {
    for t in [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8] {
        for (name, o, _, bc) in held(t) {
            let label = format!("{name}@{}", t.name());
            let e = build_sweep_order(&sweep(o.clone(), vec![bc.clone()], vec![], true), &kob_protocol::budget::lookup)
                .expect_err(&label)
                .to_string();
            assert!(e.contains("bought-custody"), "{label}: {e}");
            let stray = tutxo(t, bc.utxo.covenant_id.unwrap(), 76, 3, O, true);
            let built = build_sweep_order(&sweep(o.clone(), vec![stray], vec![], true), &kob_protocol::budget::lookup)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            assert_eq!(built.tx.outputs[0].script_public_key, built.tx.inputs[0].utxo.script_public_key, "{label}: bought unchanged");
        }
    }
}

/// The maker's cancel of an order holding a bought-custody (CL4) takes it with the order: without it the cancel would leave it
/// owned by a covenant id that no longer exists, so the builder refuses. The same for a position cancel.
#[test]
fn a_cancel_releases_the_bought_custody() {
    for t in [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8] {
        for (name, o, custody, bc) in held(t) {
            let label = format!("{name}@{}", t.name());
            let cancel = |strays: Vec<TokenUtxo>| CancelOrder {
                retired: None,
                prefund: None,
                order: o.clone(),
                custody: custody.clone(),
                strays,
                foreign: vec![],
                tokens: vec![],
                funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
                change: None,
                replace: None,
                lock_time: 0,
                records: vec![],
                fee: fee(),
            };
            let e = build_cancel_order(&cancel(vec![]), &kob_protocol::budget::lookup).expect_err(&label).to_string();
            assert!(e.contains("bought-custody"), "{label}: {e}");
            // a look-alike of another amount does not stand in for it
            let other = tutxo(t, bc.utxo.covenant_id.unwrap(), 76, 3, O, true);
            assert!(build_cancel_order(&cancel(vec![other]), &kob_protocol::budget::lookup).is_err(), "{label}");
            let built = build_cancel_order(&cancel(vec![bc.clone()]), &kob_protocol::budget::lookup)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            let sell = custody.as_ref().map_or(0, |c| c.state.amount());
            assert_eq!(maker_units(&built), sell + bc.state.amount(), "{label}: every custody to the maker");
            assert!(
                built.tx.outputs.iter().all(|x| x.covenant.as_ref().is_none_or(|c| c.covenant_id != O)),
                "{label}: the order ends"
            );
            let position = |strays: Vec<TokenUtxo>| CancelPosition {
                orders: vec![CancelItem { order: o.clone(), retired: None, custody: custody.clone(), prefund: None, strays }],
                funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
                change: None,
                lock_time: 0,
                token_carrier: None,
                records: vec![],
                fee: fee(),
            };
            assert!(build_cancel_position(&position(vec![]), &kob_protocol::budget::lookup).is_err(), "{label}");
            let built = build_cancel_position(&position(vec![bc.clone()]), &kob_protocol::budget::lookup)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &format!("position {label}"));
        }
    }
}

/// R9FB2-1: a sweep whose order input claims the genesis id of the continuation's group is a genesis on chain;
/// `verify_sweep` refuses it, and the builder refuses that claimed id.
#[test]
fn a_genesis_bound_to_the_claimed_id_is_no_sweep() {
    let t = TemplateId::Kcc20Ref;
    let o = order(70, CARRIER, O, 1_000, AnyState::KobAsk(ask(MAKER_A, P250, t)));
    let state = o.state.clone();
    let req = sweep(o, vec![tutxo(t, TOKEN_COV, 76, 3, O, true)], vec![], true);
    let mut built = build_sweep_order(&req, &kob_protocol::budget::lookup).unwrap();
    let c = built.tx.outputs[0].covenant.clone().unwrap();
    let group: Vec<(u32, u64, String)> = built
        .tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, x)| x.covenant.as_ref() == Some(&c))
        .map(|(k, x)| (k as u32, x.value, x.script_public_key.clone()))
        .collect();
    let inp = &built.tx.inputs[0];
    let g = kob_protocol::payload::genesis_covenant_id(inp.transaction_id, inp.index, &group).unwrap();
    built.tx.outputs[0].covenant = Some(kob_protocol::tx::CovenantJson { authorizing_input: c.authorizing_input, covenant_id: g });
    built.tx.inputs[0].utxo.covenant_id = Some(g);
    let p = decode(&built.tx.payload).unwrap().unwrap();
    let e = verify_sweep(&built.tx, &p.records[0], &state, "cancel").unwrap_err();
    assert!(e.to_string().contains("would start a new covenant"), "{e}");
    let mut bad = req.clone();
    bad.order.utxo.covenant_id = Some(g);
    bad.strays = vec![tutxo(t, TOKEN_COV, 76, 3, g, true)];
    let e = build_sweep_order(&bad, &kob_protocol::budget::lookup).unwrap_err();
    assert!(e.to_string().contains("would start a new covenant"), "{e}");
}
