//! Regression tests of the audit 2026-10-29 finding F1: a NEGATIVE `interval` in the state of a trigger-evidence order.
//!
//! Every stop reader (`KobCondAsk` / `KobCondBid` / `KobIfdAsk` / `KobIfdBid` `touchAsk` / `touchBid` / `touch`, `KobCondPair` /
//! `KobIfdPair` `rd` / `evLeg`, and the KRON twins) computes the evidence quote's exposure as `UTXO DAA + interval`, where the
//! interval is read out of the evidence order's own state, and requires `tx.daa >= exposed + minRestDaa`. The evidence
//! templates (`KobAsk`, `KobBid`, `KobPair`) never validate their own initial state, so an order built with a raw transaction
//! can carry `interval = -10^12`: the rest time `minRestDaa` then counted as zero and the stop could be armed by a quote that
//! had just been created. The readers now `require(iv >= 0)` before the addition (a new order template version, see
//! `docs/spec/template-retirement.md`). These tests are the PoC of the audit turned around: every such arm update must be
//! REFUSED by the stopped order, and so must a sign flip of any other state field a reader reads.
//!
//! The honest controls (evidence one DAA old with `interval` 0) are refused by the rest time as before, and the honest
//! update (the evidence rested) is accepted.
//!
//! Run: cargo test --release -p kob-tests --test audit_rest_interval_tests -- --nocapture --test-threads=1

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use fx::pair::*;
use fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{Action, Batch, BatchUpdate, Leg};
use kob_protocol::state::*;
use kob_protocol::tx::SigPlan;
use ph::*;
use TemplateId::{Kcc20Ref, Kcc20Ref8x8, KronToken2433};

const OID: [u8; 32] = X_ID;
const ODAA: i64 = 1_000;
const NEG: i64 = -1_000_000_000_000;

fn mixes() -> Vec<(TemplateId, TemplateId)> {
    vec![(Kcc20Ref8x8, Kcc20Ref8x8), (Kcc20Ref8x8, KronToken2433), (KronToken2433, Kcc20Ref8x8), (Kcc20Ref, Kcc20Ref)]
}

/// Applies `f` to the state of the order of covenant `c` (its input and its continuation output, if it rests).
fn restate_any(e: &mut Ed, c: [u8; 32], f: &dyn Fn(&mut AnyState)) {
    let i = e.input_of_cov(c);
    let (t, st) = e.entry_state(i);
    let mut any = AnyState::decode(t, &st).unwrap();
    f(&mut any);
    e.set_entry_state(i, any.encode());
    if let Some(k) = e.cont_out(c) {
        let (t, st) = e.out_order(k).unwrap();
        let mut any = AnyState::decode(t, &st).unwrap();
        f(&mut any);
        e.set_out_spk_state(k, t, &any.encode());
    }
}

fn set_interval(a: &mut AnyState, v: i64) {
    match a {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.interval = v,
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.interval = v,
        AnyState::KobPair(x) => x.interval = v,
        s => panic!("no interval on {s:?}"),
    }
}

fn cond_arm_mode0(ask: bool, pa: TemplateId, pb: TemplateId) -> Batch {
    let c = if ask { cond_pair_ask(MAKER_A, pa, pb) } else { cond_pair_bid(MAKER_A, pa, pb) };
    let legs = if ask {
        vec![
            kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 72, 5, 1),
            kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 74, 5, 1),
            kbid_leg(TOKEN_COV, pa, 24, P260, cov(0x95), 76, 10, 1),
            kask_leg(TOKEN_B, pb, 25, P250, cov(0x96), 78, 10, 1),
        ]
    } else {
        vec![
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 5, 1),
            kask_leg(TOKEN_COV, pa, 24, P250, cov(0x95), 76, 10, 1),
            kbid_leg(TOKEN_B, pb, 25, P260, cov(0x96), 78, 10, 1),
        ]
    };
    let mut b = route(legs);
    b.updates.push(BatchUpdate {
        order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
        evidence: 0,
        evidence_b: Some(1),
        take: None,
    });
    b
}

fn cond_arm_mode1(ask: bool, pa: TemplateId, pb: TemplateId) -> Batch {
    let c = if ask { cond_pair_ask(MAKER_A, pa, pb) } else { cond_pair_bid(MAKER_A, pa, pb) };
    let ev = pair(MAKER_C, ask, pa, pb, if ask { RATE - 10 } else { RATE + 10 });
    let opp = pair(MAKER_B, !ask, pa, pb, RATE);
    let mut b = route(vec![pair_leg(ev, pa, pb, cov(0x93), 72, 1), pair_leg(opp, pa, pb, cov(0x94), 74, 1)]);
    b.updates.push(BatchUpdate {
        order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
        evidence: 0,
        evidence_b: None,
        take: None,
    });
    b
}

fn stop_entry(pa: TemplateId, pb: TemplateId, buy: bool) -> IfdPairState {
    let mut s = ifd_pair(MAKER_A, buy, pa, pb);
    s.entry_stop = RATE;
    s.price = if buy { RATE + 50 } else { RATE - 50 };
    s.armed = 0;
    s.custody = s.b_custody_needed().unwrap();
    s
}

fn ifd_ev_legs(pa: TemplateId, pb: TemplateId, mode: u8, fell: bool) -> Vec<Leg> {
    match (mode, fell) {
        (0, true) => vec![
            kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 120, WHOLE),
            kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 122, WHOLE),
            kbid_units(TOKEN_COV, pa, 24, P260, cov(0x95), 124, WHOLE),
            kask_units(TOKEN_B, pb, 25, P250, cov(0x96), 126, WHOLE),
        ],
        (0, false) => vec![
            kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 120, WHOLE),
            kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 122, WHOLE),
            kask_units(TOKEN_COV, pa, 24, P250, cov(0x95), 124, WHOLE),
            kbid_units(TOKEN_B, pb, 25, P260, cov(0x96), 126, WHOLE),
        ],
        (1, true) => vec![
            pair_leg(pair(MAKER_C, true, pa, pb, RATE - 10), pa, pb, cov(0x93), 120, 1),
            pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
        ],
        _ => vec![
            pair_leg(pair(MAKER_C, false, pa, pb, RATE + 10), pa, pb, cov(0x93), 120, 1),
            pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
        ],
    }
}

fn ifd_update(s: IfdPairState, pa: TemplateId, pb: TemplateId, mode: u8) -> Batch {
    let fell = !s.is_buy_first();
    let mut b = route(ifd_ev_legs(pa, pb, mode, fell));
    b.updates.push(BatchUpdate {
        order: order(60, ifd_value(&s), E_ID, 1_000, AnyState::KobIfdPair(s)),
        evidence: 0,
        evidence_b: (mode == 0).then_some(1),
        take: None,
    });
    b
}

// ---------------------------------------------------------------- the sign of every state field a reader reads

/// Number of fields a flip actually changed (a field that is 0 has no sign to flip), to keep the battery honest.
static FLIPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The numeric state fields a stop reader reads out of an evidence order (`touchAsk` / `touchBid` / `touch`, `rd`, `evLeg`):
/// the quote, the scale(s), `activeFrom`, `interval`, `slope`.
const FIELDS: [&str; 5] = ["price", "scale", "activeFrom", "interval", "slope"];

fn neg(v: &mut i64) -> bool {
    let changed = *v != 0;
    *v = v.wrapping_neg();
    changed
}

/// Flips the sign of `field` in an evidence order's state; false if the field is 0 (nothing changed).
fn flip_field(a: &mut AnyState, field: &str) -> bool {
    match a {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => match field {
            "price" => neg(&mut x.price),
            "scale" => neg(&mut x.scale),
            "activeFrom" => neg(&mut x.active_from),
            "interval" => neg(&mut x.interval),
            _ => neg(&mut x.slope),
        },
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => match field {
            "price" => neg(&mut x.price),
            "scale" => neg(&mut x.scale),
            "activeFrom" => neg(&mut x.active_from),
            "interval" => neg(&mut x.interval),
            _ => neg(&mut x.slope),
        },
        AnyState::KobPair(x) => match field {
            "price" => neg(&mut x.price),
            "scale" => {
                let (a, b) = (neg(&mut x.s_scale), neg(&mut x.t_scale));
                a || b
            }
            "activeFrom" => neg(&mut x.active_from),
            "interval" => neg(&mut x.interval),
            _ => neg(&mut x.slope),
        },
        s => panic!("not an evidence order: {s:?}"),
    }
}

/// Flips `field` of the evidence order `ev` in `ed`; true if the state changed.
fn flip_in(ed: &mut Ed, ev: [u8; 32], field: &str) -> bool {
    let changed = std::cell::Cell::new(false);
    restate_any(ed, ev, &|s| {
        if flip_field(s, field) {
            changed.set(true);
        }
    });
    if changed.get() {
        FLIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    changed.get()
}

/// With the evidence UTXOs one DAA old (the rest time is not met), flipping the sign of any field a reader reads in the state
/// of any evidence order must leave the stopped order refused.
fn flip_battery(r: &Run, tag: &str, honest: &Ed, stopped: [u8; 32], evs: &[[u8; 32]]) {
    let i = honest.order_in(stopped);
    let mut fresh = honest.clone();
    for ev in evs {
        let vi = fresh.input_of_cov(*ev);
        fresh.set_input_daa(vi, NOW - 1);
    }
    for ev in evs {
        for f in FIELDS {
            let mut c = fresh
                .clone()
                .named(&format!("REST-FLIP {tag}: sign of {f} of evidence order {:02x} flipped (UTXO one DAA old)", ev[0]));
            if flip_in(&mut c, *ev, f) {
                r.bad(&c, i);
            }
        }
    }
}

// ---------------------------------------------------------------- the rest time of the evidence

/// One arm update `honest` (stopped order of covenant `stopped`, evidence orders `evs`):
///  * C1: the evidence UTXOs are one DAA old, `interval` 0: refused by the stopped order (the rest time works);
///  * A1: the same with `interval` = -10^12 in the state of the evidence order(s): now REFUSED by the stopped order (every
///    input accepted it before the readers required `interval >= 0`);
///  * C2: A1 with the custody of the evidence ask / pair also fresh: refused;
///  * the sign flip battery.
fn rest_case(r: &Run, tag: &str, honest: Ed, stopped: [u8; 32], evs: &[[u8; 32]]) {
    r.ok(&honest);
    let i = honest.order_in(stopped);
    let fresh = NOW - 1;
    let mut c1 = honest.clone().named(&format!("REST-C1 {tag}: evidence UTXOs one DAA old, interval 0"));
    for ev in evs {
        let vi = c1.input_of_cov(*ev);
        c1.set_input_daa(vi, fresh);
    }
    r.bad(&c1, i);
    let mut a1 = c1.clone().named(&format!("REST-A1 {tag}: evidence UTXOs one DAA old, interval -10^12"));
    for ev in evs {
        restate_any(&mut a1, *ev, &|s| set_interval(s, NEG));
    }
    r.bad(&a1, i);
    let mut c2 = a1.clone().named(&format!("REST-C2 {tag}: as A1 with the evidence custody also one DAA old"));
    let mut custodies = 0;
    for ev in evs {
        if let Some(ci) = (0..c2.plans.len()).find(|k| {
            matches!(c2.plans[*k], SigPlan::TokenLeader { .. } | SigPlan::TokenDelegator { .. } | SigPlan::KronToken { .. })
                && c2.in_tok(*k).owner() == *ev
        }) {
            c2.set_input_daa(ci, fresh);
            custodies += 1;
        }
    }
    if custodies > 0 {
        r.bad(&c2, i);
    }
    flip_battery(r, tag, &honest, stopped, evs);
}

/// A2: mode 0 with ONLY the bid leg fresh and its interval -10^12 (the ask leg is a genuine long-resting ask at a fair price):
/// refused, so a mode-0 stop needs no custody trick either.
fn rest_bid_only(r: &Run, tag: &str, honest: &Ed, stopped: [u8; 32], bid_ev: [u8; 32]) {
    let i = honest.order_in(stopped);
    let mut a2 = honest.clone().named(&format!("REST-A2 {tag}: only the bid leg fresh with interval -10^12"));
    let vi = a2.input_of_cov(bid_ev);
    a2.set_input_daa(vi, NOW - 1);
    restate_any(&mut a2, bid_ev, &|s| set_interval(s, NEG));
    r.bad(&a2, i);
    let mut c = honest.clone().named(&format!("REST-A2-control {tag}: only the bid leg fresh, interval 0"));
    let vi = c.input_of_cov(bid_ev);
    c.set_input_daa(vi, NOW - 1);
    r.bad(&c, i);
}

#[test]
fn rest_cond_pair_refuses_a_negative_interval() {
    let subs = Subs::compile();
    for (pa, pb) in mixes() {
        ph::SLOT_GATE.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for ask in [true, false] {
            let side = if ask { "ask" } else { "bid" };
            if let Some(bt) = try_built(Action::Batch(cond_arm_mode1(ask, pa, pb))) {
                rest_case(&r, &format!("KobCondPair {side} mode 1"), Ed::new("honest", &bt), OID, &[cov(0x93)]);
            }
            if let Some(bt) = try_built(Action::Batch(cond_arm_mode0(ask, pa, pb))) {
                rest_case(&r, &format!("KobCondPair {side} mode 0"), Ed::new("honest", &bt), OID, &[cov(0x93), cov(0x94)]);
                rest_bid_only(
                    &r,
                    &format!("KobCondPair {side} mode 0"),
                    &Ed::new("honest", &bt),
                    OID,
                    if ask { cov(0x94) } else { cov(0x93) },
                );
            }
        }
    }
    assert!(FLIPPED.load(std::sync::atomic::Ordering::Relaxed) > 0, "the sign battery flipped nothing");
}

#[test]
fn rest_ifd_pair_refuses_a_negative_interval() {
    let subs = Subs::compile();
    for (pa, pb) in mixes() {
        ph::SLOT_GATE.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for buy in [true, false] {
            let side = if buy { "buy-first" } else { "sell-first" };
            for mode in [1u8, 0] {
                if let Some(bt) = try_built(Action::Batch(ifd_update(stop_entry(pa, pb, buy), pa, pb, mode))) {
                    let evs: Vec<[u8; 32]> = if mode == 0 { vec![cov(0x93), cov(0x94)] } else { vec![cov(0x93)] };
                    rest_case(&r, &format!("KobIfdPair {side} mode {mode}"), Ed::new("honest", &bt), E_ID, &evs);
                    if mode == 0 {
                        rest_bid_only(
                            &r,
                            &format!("KobIfdPair {side} mode 0"),
                            &Ed::new("honest", &bt),
                            E_ID,
                            if buy { cov(0x93) } else { cov(0x94) },
                        );
                    }
                }
            }
        }
    }
    assert!(FLIPPED.load(std::sync::atomic::Ordering::Relaxed) > 0, "the sign battery flipped nothing");
}

/// The KAS-quoted stops (`KobCondBid`, `KobIfdBid`, `KobCondAsk`, `KobIfdAsk`): an `update` arm with the evidence order of a
/// plain KobAsk / KobBid whose UTXO is one DAA old and whose interval is -10^12 is refused, as is every sign flip.
#[test]
fn rest_kas_stops_refuse_a_negative_interval() {
    use TemplateId::{KobAsk, KobBid, KobCondAsk, KobCondBid, KobIfdAsk, KobIfdBid};
    let subs = Subs::compile_asks();
    for prog in [Kcc20Ref8x8, Kcc20Ref] {
        ph::SLOT_GATE.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = Run { subs: &subs, pair: prog.name().to_string() };
        let all = fx::scenarios_on(prog);
        for (id, name) in [
            ("CB1", "cond.bid.update.arm"),
            ("IB1", "ifd.bid.update.arm"),
            ("CA1", "cond.ask.update.arm"),
            ("IA1", "ifd.ask.update.arm"),
        ] {
            let a = all.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("scenario {name} missing")).1.clone();
            let Some(bt) = try_built(a) else { continue };
            let honest = Ed::new(&format!("REST-K {id} honest {name}"), &bt);
            r.ok(&honest);
            let of = |ids: &[TemplateId]| -> Vec<usize> {
                (0..honest.plans.len())
                    .filter(|i| matches!(&honest.plans[*i], SigPlan::Entry { template, .. } if ids.contains(template)))
                    .collect()
            };
            let stops = of(&[KobCondAsk, KobCondBid, KobIfdAsk, KobIfdBid]);
            let evs = of(&[KobAsk, KobBid]);
            assert_eq!((stops.len(), evs.len()), (1, 1));
            let (si, vi) = (stops[0], evs[0]);
            let vc = cov_of(&honest.entries[vi]).unwrap();
            let mut c1 = honest.clone().named(&format!("REST-K-C1 {id}: evidence UTXO one DAA old"));
            c1.set_input_daa(vi, NOW - 1);
            r.bad(&c1, si);
            let mut a1 = c1.clone().named(&format!("REST-K-A1 {id} {name}: evidence UTXO one DAA old, interval -10^12"));
            restate_any(&mut a1, vc, &|s| set_interval(s, NEG));
            r.bad(&a1, si);
            for f in FIELDS {
                let mut c = c1.clone().named(&format!("REST-K-FLIP {id}: sign of {f} flipped"));
                if flip_in(&mut c, vc, f) {
                    r.bad(&c, si);
                }
            }
        }
    }
}
