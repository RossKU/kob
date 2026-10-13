//! Pre-mainnet internal security audit (2026-10-12) of the pair templates (`contracts/v2/KobPair.sil`, `KobCondPair.sil`,
//! `KobIfdPair.sil`): reproducers for the suspicions of that review that were not refuted by reading the code, executed in
//! the script engine against the templates compiled from source (`common/pair_harness.rs`).
//!
//! - `AUD-EXT` (characterisation, a finding): the trigger evidence of a pair stop (`KobCondPair.rd` / `evLeg`,
//!   `KobIfdPair.rd` / `evLeg`) compares the evidence order's token covenant id and scale, never its KCC-20 extension
//!   commitment (nor that of the evidence custody `tk`). Units of A's covenant id with ANOTHER commitment (another token,
//!   which only A's issuer can create in its genesis) traded by resting orders arm a stop of the real A. In an UPDATE the
//!   stopped order spends no token, so the one-commitment-per-transfer rule of the KCC-20 program does not tie the
//!   evidence to the order's own commitment. The tests assert that the whole transaction is ACCEPTED today; flip them
//!   when the evidence pins the commitment.
//! - `AUD-RB` (a check without an ablation entry): `KobIfdPair.sil` `require(rptAmount <= 1 || rptAmount - 1 < minFill)`
//!   (the re-arm bound: while at least one minimum fill of re-arms is left a fill takes at most `rptAmount - 1`, so it is
//!   booked) had no attack scenario. NIAUD-RB1 / RB2 are its witnesses (buy-first and sell-first), PIAUD-RB1 / RB2 the
//!   honest twins (fewer than `minFill` re-arms left: an unbooked larger fill is fine).
//!
//! Run: cargo test -p kob-tests --test audit_pair_sec_tests -- --nocapture --test-threads=1

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
use TemplateId::{Kcc20PublicMint, Kcc20Ref, Kcc20Ref8x8, KronToken2433};

/// Another extension commitment than the fixtures' (`EXT`): another token of A's covenant id.
const OTHER_EXT: [u8; 32] = [0xdd; 32];
const OID: [u8; 32] = X_ID;
const ODAA: u64 = 1_000;

/// The pairs whose A is a KCC-20 token, on both KCC-20 programs (8 / 8 and the tradable 3 / 3 reference program).
fn a_kcc_mixes() -> Vec<(TemplateId, TemplateId)> {
    vec![
        (Kcc20Ref8x8, Kcc20Ref8x8),
        (Kcc20Ref8x8, KronToken2433),
        (Kcc20Ref, Kcc20Ref),
        (Kcc20Ref, KronToken2433),
        (Kcc20PublicMint, Kcc20PublicMint),
        (Kcc20PublicMint, KronToken2433),
    ]
}

fn reext_state(any: &mut AnyState, tok: [u8; 32], ext: [u8; 32]) -> bool {
    match any {
        AnyState::KobAsk(x) if x.token_cov_id == tok => {
            x.extension_commitment = ext;
            true
        }
        AnyState::KobBid(x) if x.token_cov_id == tok => {
            x.extension_commitment = ext;
            true
        }
        AnyState::KobPair(p) => {
            let mut c = false;
            if p.s_cov_id == tok {
                p.s_ext = ext;
                c = true;
            }
            if p.t_cov_id == tok {
                p.t_ext = ext;
                c = true;
            }
            c
        }
        _ => false,
    }
}

/// Moves every unit of token `tok` in `e` to the extension commitment `ext`: its token inputs and outputs, and the state
/// (input and continuation) of every order that names `tok`, except the order of covenant `keep` (the stop under test,
/// which names the real token and spends no token in an update).
fn reext(e: &mut Ed, tok: [u8; 32], ext: [u8; 32], keep: [u8; 32]) {
    for i in 0..e.plans.len() {
        if cov_of(&e.entries[i]) == Some(tok) && !matches!(e.plans[i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }) {
            let st = e.in_tok(i);
            e.set_in_tok(i, map_tok(st, |k| Kcc20State { extension_commitment: ext, ..k }, |k| k));
        }
    }
    for k in e.tok_outs_of(tok) {
        let st = e.out_state(k);
        e.set_out_state(k, map_tok(st, |x| Kcc20State { extension_commitment: ext, ..x }, |x| x));
    }
    for i in 0..e.plans.len() {
        if !matches!(e.plans[i], SigPlan::Entry { .. }) || cov_of(&e.entries[i]) == Some(keep) {
            continue;
        }
        let (t, st) = e.entry_state(i);
        let Ok(mut any) = AnyState::decode(t, &st) else { continue };
        if reext_state(&mut any, tok, ext) {
            e.set_entry_state(i, any.encode());
        }
    }
    for k in 0..e.tx.outputs.len() {
        if e.tx.outputs[k].covenant.map(|c| c.covenant_id.as_bytes()) == Some(keep) {
            continue;
        }
        if let Some((t, st)) = e.out_order(k) {
            let Ok(mut any) = AnyState::decode(t, &st) else { continue };
            if reext_state(&mut any, tok, ext) {
                e.set_out_spk_state(k, t, &any.encode());
            }
        }
    }
}

/// Runs `e` (the evidence moved to another commitment of A) and returns whether EVERY input accepts; `None` when the
/// shape does not fit the token program's slots.
fn accepted(r: &Run, e: &Ed) -> Option<(bool, Vec<Result<u64, String>>)> {
    let (tx, en) = e.finish(r.subs);
    let res = r.run_fitting(e, &tx, &en)?;
    Some((res.iter().all(|x| x.is_ok()), res))
}

/// The evidence order at cov 0x93 names A of `ext` (the edit reached the evidence, not a no-op).
fn evidence_ext(e: &Ed) -> [u8; 32] {
    let vi = e.input_of_cov(cov(0x93));
    let (t, st) = e.entry_state(vi);
    match AnyState::decode(t, &st).unwrap() {
        AnyState::KobAsk(x) => x.extension_commitment,
        AnyState::KobBid(x) => x.extension_commitment,
        AnyState::KobPair(p) => {
            if p.s_cov_id == TOKEN_COV {
                p.s_ext
            } else {
                p.t_ext
            }
        }
        s => panic!("evidence state {s:?}"),
    }
}

fn report_finding(r: &Run, e: &Ed, stop_in: usize) {
    assert_eq!(evidence_ext(e), OTHER_EXT, "{}: the evidence was not moved to the other commitment", e.name);
    // control: the same transaction with the evidence order still naming the real commitment is refused by the evidence
    // order (or the token program), so the edit really trades units of the other commitment
    {
        let mut c = e.clone().named(&format!("{} [control: evidence state names EXT]", e.name));
        let vi = c.input_of_cov(cov(0x93));
        let (t, st) = c.entry_state(vi);
        let mut any = AnyState::decode(t, &st).unwrap();
        reext_state(&mut any, TOKEN_COV, EXT);
        c.set_entry_state(vi, any.encode());
        if let Some((all, _)) = accepted(r, &c) {
            assert!(!all, "{} {}: control accepted (the edit did not reach the token)", c.name, r.pair);
        }
    }
    let Some((all, res)) = accepted(r, e) else {
        println!("SKIP-SLOTS {} {}", e.name, r.pair);
        return;
    };
    assert!(
        all,
        "{} {}: expected the evidence of another commitment to be ACCEPTED (finding AUD-EXT characterisation); results {res:?}",
        e.name, r.pair
    );
    println!("FINDING-ACCEPTED {} {}  [the stop at in[{stop_in}] arms on units of another extension commitment]", e.name, r.pair);
}

// ---------------------------------------------------------------- AUD-EXT: KobCondPair arm updates

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
        order: order(60, PV, OID, ODAA, AnyState::KobCondPair(c)),
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
        order: order(60, PV, OID, ODAA, AnyState::KobCondPair(c)),
        evidence: 0,
        evidence_b: None,
        take: None,
    });
    b
}

#[test]
fn aud_ext_cond_pair_arms_on_evidence_of_another_extension_commitment() {
    let subs = Subs::compile();
    for (pa, pb) in a_kcc_mixes() {
        ph::SLOT_GATE.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for ask in [true, false] {
            let side = if ask { "a" } else { "b" };
            for (mode, b) in [(0, cond_arm_mode0(ask, pa, pb)), (1, cond_arm_mode1(ask, pa, pb))] {
                let Some(bt) = try_built(Action::Batch(b)) else {
                    println!("SKIP-SLOTS AUD-EXT-C{mode}{side} {} (build)", r.pair);
                    continue;
                };
                let honest = Ed::new(&format!("PAUD-EXT-C{mode}{side} honest arm update, mode {mode}"), &bt);
                r.ok(&honest);
                let i = honest.order_in(OID);
                let mut e = honest.clone().named(&format!(
                    "AUD-EXT-C{mode}{side} KobCondPair arm update on evidence trading A of another extension commitment, mode {mode}"
                ));
                reext(&mut e, TOKEN_COV, OTHER_EXT, OID);
                report_finding(&r, &e, i);
            }
        }
    }
}

// ---------------------------------------------------------------- AUD-EXT: KobIfdPair arm updates

/// A stop entry (trigger RATE, limit RATE +- 50) of side `buy`, unarmed.
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

fn ifd_update(s: IfdPairState, pa: TemplateId, pb: TemplateId, mode: u8) -> Action {
    let fell = !s.is_buy_first();
    let mut b = route(ifd_ev_legs(pa, pb, mode, fell));
    b.updates.push(BatchUpdate {
        order: order(60, ifd_value(&s), E_ID, 1_000, AnyState::KobIfdPair(s)),
        evidence: 0,
        evidence_b: (mode == 0).then_some(1),
        take: None,
    });
    Action::Batch(b)
}

#[test]
fn aud_ext_ifd_pair_arms_on_evidence_of_another_extension_commitment() {
    let subs = Subs::compile();
    for (pa, pb) in a_kcc_mixes() {
        ph::SLOT_GATE.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for buy in [true, false] {
            let side = if buy { "b" } else { "a" };
            for mode in [0u8, 1] {
                let Some(bt) = try_built(ifd_update(stop_entry(pa, pb, buy), pa, pb, mode)) else {
                    println!("SKIP-SLOTS AUD-EXT-I{mode}{side} {} (build)", r.pair);
                    continue;
                };
                let honest = Ed::new(&format!("PAUD-EXT-I{mode}{side} honest entry arm update, mode {mode}"), &bt);
                r.ok(&honest);
                let i = honest.order_in(E_ID);
                let mut e = honest.clone().named(&format!(
                    "AUD-EXT-I{mode}{side} KobIfdPair arm update on evidence trading A of another extension commitment, mode {mode}"
                ));
                reext(&mut e, TOKEN_COV, OTHER_EXT, E_ID);
                report_finding(&r, &e, i);
            }
        }
    }
}

// ---------------------------------------------------------------- AUD-RB: the KobIfdPair re-arm bound

fn qof(n: i64, r: i64, d: i64, c: i64) -> i64 {
    let (m, q) = (n % d, n / d);
    q * r + m * (r / d) + (m * (r % d) + c) / d
}

fn ifd_leg_units(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64) -> Leg {
    let mut l = ifd_pair_leg(s, pa, pb, E_ID, 70, 1);
    if let Leg::IfdPair { amount, .. } = &mut l {
        *amount = n;
    }
    l
}

fn buy_fill(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    let amt = s.spend(n, s.price).unwrap();
    route(vec![
        ifd_leg_units(s, pa, pb, n),
        kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 74, amt),
        kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 76, n),
    ])
}

fn sell_fill(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    let proceeds = qof(n, s.price, s.a_scale, s.a_scale - 1);
    let mut legs = vec![ifd_leg_units(s, pa, pb, n), kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 74, n)];
    if proceeds > 0 {
        legs.push(kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 76, proceeds));
    }
    route(legs)
}

/// Sets `rptAmount` of the entry (input and continuation): an unbooked fill leaves it unchanged.
fn set_rpt(e: &mut Ed, rpt: i64) {
    let i = e.order_in(E_ID);
    let (t, st) = e.entry_state(i);
    let AnyState::KobIfdPair(mut s) = AnyState::decode(t, &st).unwrap() else { unreachable!() };
    s.rpt_amount = rpt;
    e.set_entry_state(i, s.encode());
    let k = e.cont_out(E_ID).expect("entry continuation");
    let (t, st) = e.out_order(k).unwrap();
    let AnyState::KobIfdPair(mut c) = AnyState::decode(t, &st).unwrap() else { unreachable!() };
    c.rpt_amount = rpt;
    e.set_out_spk_state(k, t, &c.encode());
}

#[test]
fn aud_rb_ifd_pair_rearm_bound() {
    let subs = Subs::compile();
    for (pa, pb) in [
        (Kcc20Ref8x8, KronToken2433),
        (KronToken2433, Kcc20Ref8x8),
        (Kcc20Ref, KronToken2433),
        (Kcc20Ref, Kcc20Ref),
        (Kcc20PublicMint, KronToken2433),
        (Kcc20PublicMint, Kcc20PublicMint),
    ] {
        ph::SLOT_GATE.store(true, std::sync::atomic::Ordering::Relaxed);
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for buy in [true, false] {
            let (tag, s) = if buy { ("1", ifd_pair(MAKER_A, true, pa, pb)) } else { ("2", ifd_pair(MAKER_A, false, pa, pb)) };
            // a NON-repeating entry (rptAmount 0) filled for 3 whole A of its 10: the exit is not booked
            let n = 3 * WHOLE;
            let b = if buy { buy_fill(s.clone(), pa, pb, n) } else { sell_fill(s.clone(), pa, pb, n) };
            let Some(bt) = try_built(Action::Batch(b)) else {
                println!("SKIP-SLOTS AUD-RB{tag} {} (build)", r.pair);
                continue;
            };
            let honest = Ed::new(&format!("PAUD-RB{tag}0 non-repeating fill of 3 whole A"), &bt);
            r.ok(&honest);
            let i = honest.order_in(E_ID);
            // honest twin: W - 1 re-arms left (rptAmount = W): fewer than minFill, so an unbooked fill of 3 W is fine
            let mut ok =
                honest.clone().named(&format!("PIAUD-RB{tag} repeating entry with fewer than minFill re-arms left, unbooked fill"));
            set_rpt(&mut ok, s.min_fill);
            r.ok(&ok);
            // attack: 2 W re-arms left (>= minFill) but the fill takes 3 W unbooked: the re-arms would be lost for good
            let mut bad =
                honest.clone().named(&format!("NIAUD-RB{tag} fill above the re-arms left while a minimum fill of re-arms is left"));
            set_rpt(&mut bad, 1 + 2 * WHOLE);
            r.bad(&bad, i);
        }
    }
}
