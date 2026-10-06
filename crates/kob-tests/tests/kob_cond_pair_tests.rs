//! `KobCondPair` (contracts/v2/KobCondPair.sil) security suite, executed in the script engine against the template
//! compiled from source (`common/pair_harness.rs`). This file owns everything of `KobCondPair` EXCEPT the repeat-IFD
//! merge checks (the suite agent of `kob_ifd_pair_tests.rs` owns those): the KobPair-style settlement of a leg fill
//! (ceil / floor, exact custody, the stray guards of both tokens on every path, output aliasing), the trigger evidence
//! (both modes), trailing, the auction, OCO, refunds / cancels / kills, update encodings and keeperTip, overflow, and the
//! C1 fake-quote attacks read as evidence.
//!
//! The order A/B: A = `TOKEN_COV` (program `pa`), B = `TOKEN_B` (`pb`), scale 1,000 = `WHOLE`. Side ASK sells A (S = A)
//! with a sell stop below the market and a take-profit above it; side BID holds a B escrow (S = B) and buys A with a buy
//! stop above the market and a limit below it. Positives are honest `kob-protocol` builds on all four family mixes;
//! attacks are honest builds edited input by input (`ph::Ed`, `Run::bad`) or patched after assembly, each REJECTED by the
//! order's own input, on the two mixed pairs (and per family where a check is per family).
//!
//! Scenario ids: `PC..` positives, `NC..` attacks. Run: cargo test -p kob-tests --test kob_cond_pair_tests -- --nocapture --test-threads=1

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use fx::pair::*;
use fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{Action, Batch, BatchUpdate, CancelOrder, Leg, RefundOrder};
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::{Arg, TokenUtxo};
use ph::*;

pub const OID: [u8; 32] = X_ID;
pub const ODAA: i64 = 1_000;
pub const MAX_IDLE: i64 = 77_760_000;
/// A fill amount with a remainder (ceil and floor of its quote differ).
pub const NR: i64 = 4 * WHOLE + 1;

// ---------------------------------------------------------------- fixtures

pub fn s_prog(ask: bool, pa: TemplateId, pb: TemplateId) -> TemplateId {
    if ask {
        pa
    } else {
        pb
    }
}
pub fn t_prog(ask: bool, pa: TemplateId, pb: TemplateId) -> TemplateId {
    s_prog(!ask, pa, pb)
}
pub fn s_tok(ask: bool) -> [u8; 32] {
    if ask {
        TOKEN_COV
    } else {
        TOKEN_B
    }
}
pub fn t_tok(ask: bool) -> [u8; 32] {
    s_tok(!ask)
}
/// The side whose S (`want_s`) / T token is of family `fam` on a mixed pair.
pub fn side_with(fam: Family, want_s: bool, pa: TemplateId) -> bool {
    let ask_s = pa.family() == fam;
    if want_s {
        ask_s
    } else {
        !ask_s
    }
}

pub fn ca(pa: TemplateId, pb: TemplateId) -> CondPairState {
    cond_pair_ask(MAKER_A, pa, pb)
}
pub fn cb(pa: TemplateId, pb: TemplateId) -> CondPairState {
    cond_pair_bid(MAKER_A, pa, pb)
}
pub fn cside(ask: bool, pa: TemplateId, pb: TemplateId) -> CondPairState {
    if ask {
        ca(pa, pb)
    } else {
        cb(pa, pb)
    }
}
/// `c` with `left` base units left (the custody follows: an ask holds that, a bid its escrow for its fills).
pub fn cleft(c: CondPairState, left: i64) -> CondPairState {
    let mut x = CondPairState { amount_left: left, ..c };
    x.custody = if x.is_ask() { left } else { x.bid_escrow(x.max_fills()).unwrap() };
    x
}

pub fn user(p: TemplateId, amount: i64, key: u8) -> TokenState {
    tstate(p, amount, pk(key), false)
}

pub fn ed(name: &str, a: Action) -> Ed {
    Ed::new(name, &built(a))
}
pub fn edb(name: &str, b: Batch) -> Ed {
    Ed::new(name, &built(Action::Batch(b)))
}
pub fn edu(name: &str, b: Batch) -> Ed {
    Ed::new(name, &unchecked(b))
}

/// A TP / limit-leg fill of `n` at an inventory counterparty (the matcher's own tokens): no evidence needed. The maker
/// receives T (ASK) or pays S (BID); the matcher gives T and takes S.
pub fn tp_inv(c: CondPairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    let ask = c.is_ask();
    let lp = c.tp_price;
    let need = if ask { c.t_out_min(n, lp).unwrap() } else { n };
    let have = (need + 10 * WHOLE).min(if t_prog(ask, pa, pb).family() == Family::Kron { 1_000_000_000 } else { i64::MAX });
    let mut b = route(vec![cond_pair_leg_amount(c, pa, pb, OID, 70, n, 0)]);
    b.taker_tokens = vec![tutxo(t_prog(ask, pa, pb), t_tok(ask), 60, have, pk(MATCHER), false)];
    b
}

pub fn tp_ed(name: &str, c: CondPairState, pa: TemplateId, pb: TemplateId, n: i64) -> (Ed, usize) {
    let e = edb(name, tp_inv(c, pa, pb, n));
    let i = e.order_in(OID);
    (e, i)
}

pub fn cin(e: &Ed, i: usize) -> usize {
    e.arg_int(i, 1) as usize
}
pub fn cst(e: &Ed, i: usize) -> CondPairState {
    match AnyState::decode(TemplateId::KobCondPair, &e.entry_state(i).1).unwrap() {
        AnyState::KobCondPair(s) => s,
        _ => unreachable!(),
    }
}
pub fn restate(e: &mut Ed, i: usize, f: impl Fn(&mut CondPairState)) {
    let mut s = cst(e, i);
    f(&mut s);
    e.set_entry_state(i, s.encode());
}

pub fn refund_of(c: CondPairState, pa: TemplateId, pb: TemplateId, lock: u64) -> Action {
    let ask = c.is_ask();
    let custody = tutxo(s_prog(ask, pa, pb), s_tok(ask), 71, c.custody, OID, true);
    Action::RefundOrder(RefundOrder {
        order: order(70, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
        foreign: vec![],
        custody: Some(custody),
        prefund: None,
        lock_time: lock,
        funding: vec![key_utxo(250, KEEPER, 5 * KAS)],
        change: Some(pk(KEEPER)),
        fee: fee(),
    })
}
pub fn cancel_of(c: CondPairState, pa: TemplateId, pb: TemplateId, strays: Vec<TokenUtxo>) -> Action {
    let ask = c.is_ask();
    let custody = tutxo(s_prog(ask, pa, pb), s_tok(ask), 71, c.custody, OID, true);
    Action::CancelOrder(CancelOrder {
        order: order(70, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
        custody: Some(custody),
        prefund: None,
        foreign: vec![],
        strays,
        tokens: vec![],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    })
}

pub fn suite(mixes: &[(TemplateId, TemplateId)], f: impl Fn(&Run, TemplateId, TemplateId)) {
    let subs = Subs::compile();
    for (pa, pb) in mixes {
        let r = Run { subs: &subs, pair: pair_name(*pa, *pb) };
        f(&r, *pa, *pb);
    }
}

// ---------------------------------------------------------------- positives

fn positives(r: &Run, pa: TemplateId, pb: TemplateId) {
    // TP / limit leg (no evidence), both sides, rest and sold out
    r.ok(&edb("PC01 ask take-profit leg, rest", tp_inv(ca(pa, pb), pa, pb, 4 * WHOLE)));
    r.ok(&edb("PC02 ask take-profit leg, sold out", tp_inv(cleft(ca(pa, pb), 4 * WHOLE), pa, pb, 4 * WHOLE)));
    r.ok(&edb("PC03 bid limit leg, rest", tp_inv(cb(pa, pb), pa, pb, 4 * WHOLE)));
    r.ok(&edb("PC04 bid limit leg, done", tp_inv(cleft(cb(pa, pb), 4 * WHOLE), pa, pb, 4 * WHOLE)));
    // stop armed in the fill, both modes, both sides
    r.ok(&edb("PC05 sell stop armed in the fill, mode 0", stop_ev0(true, pa, pb)));
    r.ok(&edb("PC06 buy stop armed in the fill, mode 0", stop_ev0(false, pa, pb)));
    r.ok(&edb("PC07 sell stop armed in the fill, mode 1", stop_ev1(true, pa, pb)));
    r.ok(&edb("PC08 buy stop armed in the fill, mode 1", stop_ev1(false, pa, pb)));
    // an already-armed stop's auction (bandDaa > 0), sold out
    let armed = CondPairState { armed: NOW as i64 - 100, amount_left: 4 * WHOLE, custody: 4 * WHOLE, ..ca(pa, pb) };
    r.ok(&edb(
        "PC09 armed stop auction, sold out",
        route(vec![
            cond_pair_leg(armed, pa, pb, OID, 70, 4, 1),
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 4),
        ]),
    ));
    // armed stop that trades the whole band at once (bandDaa 0)
    let whole = CondPairState { armed: 1, band_daa: 0, amount_left: 4 * WHOLE, custody: 4 * WHOLE, ..ca(pa, pb) };
    r.ok(&edb(
        "PC10 armed stop, whole band at once",
        route(vec![
            cond_pair_leg(whole, pa, pb, OID, 70, 4, 1),
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 4),
        ]),
    ));
    // refunds / cancels / kills
    r.ok(&ed("PC11 ask refund at expiry", refund_of(ca(pa, pb), pa, pb, EXPIRY as u64)));
    r.ok(&ed("PC12 bid refund at expiry", refund_of(cb(pa, pb), pa, pb, EXPIRY as u64)));
    r.ok(&ed(
        "PC13 refund after 90 days idle",
        refund_of(CondPairState { expiry_daa: NO_EXPIRY, ..ca(pa, pb) }, pa, pb, (ODAA + MAX_IDLE) as u64),
    ));
    let strays = || vec![tutxo(pa, TOKEN_COV, 76, 3, OID, true), tutxo(pb, TOKEN_B, 77, 5, OID, true)];
    r.ok(&ed("PC14 ask cancel sweeping strays of both tokens", cancel_of(ca(pa, pb), pa, pb, strays())));
    r.ok(&ed("PC15 bid cancel sweeping strays of both tokens", cancel_of(cb(pa, pb), pa, pb, strays())));
    // one evidence fill arming two stops (legal by design: the evidence is read, never consumed), both modes, both sides
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        for mode1 in [false, true] {
            let m = if mode1 { 1 } else { 0 };
            r.ok(&edb(&format!("PC16{side}{m} one evidence fill arms two stops, mode {m}"), two_armed(ask, pa, pb, mode1)));
        }
    }
    // updates: arm and trail without a fill (both modes); OCO is the ask / bid fixtures (tp + stop legs present)
    for (n, a) in pair_scenarios(pa, pb) {
        if n.starts_with("pair.update.") || n == "pair.cond.ask.tp" || n == "pair.cond.bid.limit" {
            r.ok(&Ed::new(&format!("PC.{n}"), &built(a)));
        }
    }
}

/// One evidence fill arming two orders: two stops of side `ask` (`OID` and `[0x83; 32]`), each sold out for 4 whole A on its
/// stop leg, armed by the same pair of KAS-book fills (mode 0: A at cov 0x93, B at cov 0x94) or by the same resting pair
/// order (mode 1, cov 0x93, netted against a pair order of the other side at cov 0x94).
fn two_armed(ask: bool, pa: TemplateId, pb: TemplateId, mode1: bool) -> Batch {
    let c = cleft(cside(ask, pa, pb), 4 * WHOLE);
    let mut legs = vec![cond_pair_leg(c.clone(), pa, pb, OID, 70, 4, 1), cond_pair_leg(c, pa, pb, [0x83; 32], 80, 4, 1)];
    let evb = if mode1 {
        legs.push(pair_leg(pair(MAKER_C, ask, pa, pb, if ask { RATE - 10 } else { RATE + 10 }), pa, pb, cov(0x93), 72, 1));
        legs.push(pair_leg(pair(MAKER_B, !ask, pa, pb, RATE), pa, pb, cov(0x94), 74, 9));
        None
    } else {
        legs.extend(if ask {
            [
                kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 72, 1, 1),
                kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 74, 5, 1),
                kbid_leg(TOKEN_COV, pa, 24, P260, cov(0x95), 76, 9, 9),
                kask_leg(TOKEN_B, pb, 25, P250, cov(0x96), 78, 9, 9),
            ]
        } else {
            [
                kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
                kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 1, 1),
                kbid_leg(TOKEN_B, pb, 24, P260, cov(0x95), 76, 9, 9),
                kask_leg(TOKEN_COV, pa, 25, P250, cov(0x96), 78, 9, 9),
            ]
        });
        Some(3)
    };
    let mut b = route(legs);
    for k in [0, 1] {
        if let Leg::CondPair { evidence, evidence_b, .. } = &mut b.legs[k] {
            *evidence = Some(2);
            *evidence_b = evb;
        }
    }
    b
}

#[test]
fn cond_pair_positives() {
    suite(&family_mixes(), positives);
}

// ---------------------------------------------------------------- settlement (ceil / floor, custody, strays, aliasing)

fn settlement(r: &Run, pa: TemplateId, pb: TemplateId) {
    let a = CondPairState { tp_price: RATE + 1, ..ca(pa, pb) };
    let b = CondPairState { tp_price: RATE + 1, ..cb(pa, pb) };
    // the maker's token at output i (the delivery)
    let (e, i) = tp_ed("PC20 ask TP fill at the exact ceil", a.clone(), pa, pb, NR);
    let _ = i;
    r.ok(&e);
    // NC01 the ask maker paid one base unit short (floor, not ceil)
    let (mut e, i) = tp_ed("NC01 ask maker paid one unit short", a.clone(), pa, pb, NR);
    e.set_arg(i, 5, Arg::Int(e.arg_int(i, 5) - 1));
    e.add_out_amount(i, -1);
    e.add_out_amount(e.tok_out_of(TOKEN_B, pk(MATCHER)), 1);
    r.bad(&e, i);
    // NC02 the bid pays one unit more than its floor
    let (mut e, i) = tp_ed("NC02 bid pays one unit more than its floor", b.clone(), pa, pb, NR);
    e.set_arg(i, 4, Arg::Int(e.arg_int(i, 4) + 1));
    let c = cin(&e, i);
    e.add_out_amount(c, -1);
    e.add_out_amount(e.tok_out_of(TOKEN_B, pk(MATCHER)), 1);
    restate_cont(&mut e, i, |s| s.custody -= 1);
    r.bad(&e, i);
    // NC03 the bid receives n - 1 of A
    let (mut e, i) = tp_ed("NC03 bid receives n - 1 of A", b.clone(), pa, pb, NR);
    e.set_arg(i, 5, Arg::Int(NR - 1));
    e.add_out_amount(i, -1);
    e.add_out_amount(e.tok_out_of(TOKEN_COV, pk(MATCHER)), 1);
    r.bad(&e, i);
    // NC04 custody holds one unit more than `custody` (both sides)
    for (id, c) in [("NC04a ask", a.clone()), ("NC04b bid", b.clone())] {
        let (mut e, i) = tp_ed(&format!("{id} custody holds custody + 1"), c.clone(), pa, pb, NR);
        let ci = cin(&e, i);
        let st = e.in_tok(ci);
        e.set_in_tok(ci, st.with_amount(st.amount() + 1));
        e.add_out_amount(e.tok_out_of(s_tok(c.is_ask()), pk(MATCHER)), 1);
        r.bad(&e, i);
    }
    // NC05 a BID TP fill beyond amountLeft
    let (mut e, i) = tp_ed("NC05 bid filled beyond amountLeft", cleft(b.clone(), 5 * WHOLE), pa, pb, 5 * WHOLE);
    restate(&mut e, i, |s| s.amount_left = 4 * WHOLE);
    r.bad(&e, i);
    // strays of S and T on the fill path (rest) and the refund; the update spends neither (covered in cond_pair_update)
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let c = if ask { a.clone() } else { b.clone() };
        let (sp, tp) = (s_prog(ask, pa, pb), t_prog(ask, pa, pb));
        let (mut e, i) = tp_ed(&format!("NC06{side} S stray of the order in a TP fill"), c.clone(), pa, pb, NR);
        e.add_stray(s_tok(ask), sp, OID, 3, 99);
        r.bad(&e, i);
        let (mut e, i) = tp_ed(&format!("NC07{side} T stray of the order in a TP fill"), c.clone(), pa, pb, NR);
        e.add_stray(t_tok(ask), tp, OID, 3, 99);
        r.bad(&e, i);
        let mut e = ed(&format!("NC08{side} S stray of the order in a refund"), refund_of(cside(ask, pa, pb), pa, pb, EXPIRY as u64));
        e.add_stray(s_tok(ask), sp, OID, 3, 99);
        r.bad(&e, 0);
        let mut e = ed(&format!("NC09{side} T stray of the order in a refund"), refund_of(cside(ask, pa, pb), pa, pb, EXPIRY as u64));
        e.add_stray(t_tok(ask), tp, OID, 3, 99);
        r.bad(&e, 0);
        // aliasing: a second output bound to the order id (a partial fill rests, so one continuation only)
        let (mut e, i) = tp_ed(&format!("NC10{side} a second output bound to the order id"), c.clone(), pa, pb, NR);
        e.forge_bound(OID, i, KAS);
        r.bad(&e, i);
        // the rest moved off the custody index
        let (mut e, i) = tp_ed(&format!("NC11{side} custody rest at another index"), c.clone(), pa, pb, NR);
        let ci = cin(&e, i);
        let m = e.tok_out_of(s_tok(ask), pk(MATCHER));
        e.swap_outputs(ci, m);
        r.bad(&e, i);
    }
    // NC17 a sibling UTXO of the order's covenant id spent beside it (the order must be the id's only input)
    for (side, c) in [("a", a.clone()), ("b", b.clone())] {
        let (mut e, i) = tp_ed(&format!("NC17{side} a sibling UTXO of the order's covenant id in the fill"), c, pa, pb, NR);
        let j = e.add_p2pk_input(MATCHER, 0xea);
        let en = &e.entries[j];
        e.entries[j] = kaspa_consensus_core::tx::UtxoEntry::new(
            en.amount,
            en.script_public_key.clone(),
            en.block_daa_score,
            false,
            Some(kaspa_consensus_core::Hash::from_bytes(OID)),
        );
        r.bad(&e, i);
    }
    // the unrolled stray scan slot by slot, on the side whose T is the KCC-20 token (8 inputs per token): NC18t<k> the
    // order's T stray at scan slot k (slot 0: the matcher's T inventory input itself owned by the order; k >= 1: k - 1
    // key-held units of the matcher in front of it); NC19t the stray as a 9th T input, beyond the scan (the KCC-20
    // program refuses a 9th input itself). PC18t: the 8-input shape with a key-held unit at slot 7.
    let ask = side_with(Family::Kcc20, false, pa);
    let c = if ask { a.clone() } else { b.clone() };
    let (mut e, _) = tp_ed("PC18t eight T inputs (every scan slot), no stray", c.clone(), pa, pb, NR);
    stray_at_slot(&mut e, t_tok(ask), OID, 7, false);
    r.ok(&e);
    for k in 0..=8usize {
        let name = if k < 8 {
            format!("NC18t{k} T stray of the order at scan slot {k}")
        } else {
            "NC19t T stray of the order as a 9th T input (beyond the scan)".into()
        };
        let (mut e, i) = tp_ed(&name, c.clone(), pa, pb, NR);
        stray_at_slot(&mut e, t_tok(ask), OID, k, true);
        r.bad(&e, i);
    }
    // the maker's token at output i: NC13 delivered to the matcher, NC14 without its token covenant (the tokens to the
    // matcher), both sides
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let c = if ask { a.clone() } else { b.clone() };
        let (mut e, i) = tp_ed(&format!("NC13{side} the maker's T delivered to the matcher"), c.clone(), pa, pb, NR);
        let st = e.out_state(i);
        e.set_out_state(i, st.with_user_owner(pk(MATCHER)));
        r.bad(&e, i);
        let (mut e, i) = tp_ed(&format!("NC14{side} the maker's T without its token covenant"), c.clone(), pa, pb, NR);
        let st = e.out_state(i);
        let spk = st.spk_with(kob_protocol::artifacts::token_template(t_prog(ask, pa, pb)));
        e.tok_out.remove(&i);
        e.tx.outputs[i].covenant = None;
        e.tx.outputs[i].script_public_key = spk;
        let m = e.tok_out_of(t_tok(ask), pk(MATCHER));
        e.add_out_amount(m, st.amount());
        r.bad(&e, i);
        let (mut e, i) = tp_ed(&format!("NC16{side} the custody rest without the S covenant"), c.clone(), pa, pb, NR);
        let ci = cin(&e, i);
        let st = e.out_state(ci);
        let spk = st.spk_with(kob_protocol::artifacts::token_template(s_prog(ask, pa, pb)));
        e.tok_out.remove(&ci);
        e.tx.outputs[ci].covenant = None;
        e.tx.outputs[ci].script_public_key = spk;
        let m = e.tok_out_of(s_tok(ask), pk(MATCHER));
        e.add_out_amount(m, st.amount());
        r.bad(&e, i);
    }
    // NC15 an armed ASK stop whose band is the whole stop (slipBps 10000): legPrice 0, the maker paid nothing
    let armed = CondPairState { armed: 1, band_daa: 0, ..cleft(ca(pa, pb), 4 * WHOLE) };
    let mut bt = route(vec![cond_pair_leg_amount(armed, pa, pb, OID, 70, 4 * WHOLE, 1)]);
    bt.taker_tokens = vec![tutxo(pb, TOKEN_B, 60, 10 * WHOLE, pk(MATCHER), false)];
    let mut e = edb("NC15 armed sell stop at legPrice 0 (slipBps 10000): the maker paid nothing", bt);
    let i = e.order_in(OID);
    restate(&mut e, i, |s| s.slip_bps = 10_000);
    let t_out = e.arg_int(i, 5);
    e.set_arg(i, 5, Arg::Int(0));
    e.add_out_amount(i, -t_out);
    let m = e.tok_out_of(TOKEN_B, pk(MATCHER));
    e.add_out_amount(m, t_out);
    r.bad(&e, i);
}

#[test]
fn cond_pair_settlement() {
    suite(&mixed_pairs(), settlement);
}

// ---------------------------------------------------------------- trigger evidence

/// The four proven stop-fill fixtures (cond at `OID`, mode-0 evidence A / B at cov 0x93 / 0x94, mode-1 pair evidence at
/// cov 0x93 netted against cov 0x94), filled for 4 whole A.
fn stop_ev0(ask: bool, pa: TemplateId, pb: TemplateId) -> Batch {
    let c = cside(ask, pa, pb);
    let legs = if ask {
        vec![
            cond_pair_leg(c, pa, pb, OID, 70, 4, 1),
            kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 72, 5, 1),
            kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 74, 5, 1),
            kbid_leg(TOKEN_COV, pa, 24, P260, cov(0x95), 76, 10, 5),
            kask_leg(TOKEN_B, pb, 25, P250, cov(0x96), 78, 10, 5),
        ]
    } else {
        vec![
            cond_pair_leg(c, pa, pb, OID, 70, 4, 1),
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 5, 1),
            kbid_leg(TOKEN_B, pb, 24, P260, cov(0x95), 76, 10, 5),
            kask_leg(TOKEN_COV, pa, 25, P250, cov(0x96), 78, 10, 5),
        ]
    };
    with_evidence(route(legs), 1, Some(2))
}
fn stop_ev1_px(ask: bool, pa: TemplateId, pb: TemplateId, ev_price: i64) -> Batch {
    let c = cside(ask, pa, pb);
    let legs = if ask {
        vec![
            cond_pair_leg(c, pa, pb, OID, 70, 4, 1),
            pair_leg(pair(MAKER_C, true, pa, pb, ev_price), pa, pb, cov(0x93), 72, 1),
            pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 74, 5),
        ]
    } else {
        vec![
            cond_pair_leg(c, pa, pb, OID, 70, 4, 1),
            pair_leg(pair(MAKER_C, false, pa, pb, ev_price), pa, pb, cov(0x93), 72, 1),
            pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 74, 5),
        ]
    };
    with_evidence(route(legs), 1, None)
}
fn stop_ev1(ask: bool, pa: TemplateId, pb: TemplateId) -> Batch {
    stop_ev1_px(ask, pa, pb, if ask { RATE - 10 } else { RATE + 10 })
}

fn ev0_ed(ask: bool, pa: TemplateId, pb: TemplateId, id: &str) -> (Ed, usize) {
    let e = edb(id, stop_ev0(ask, pa, pb));
    let i = e.order_in(OID);
    (e, i)
}
/// Sets the A evidence order's KAS-book state (cov 0x93) through `f`.
fn set_a(e: &mut Ed, f: impl Fn(&mut AnyState)) {
    let ai = e.input_of_cov(cov(0x93));
    let (t, st) = e.entry_state(ai);
    let mut any = AnyState::decode(t, &st).unwrap();
    f(&mut any);
    e.set_entry_state(ai, any.encode());
}

fn evidence_ev0(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let c = cside(ask, pa, pb);
        // NC20 the A evidence not filled (n = 0)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC20{side} A evidence not filled (n = 0)"));
        let ai = e.input_of_cov(cov(0x93));
        e.set_arg(ai, 0, nb(0));
        set_min_touch(&mut e, i, 0);
        r.bad(&e, i);
        // NC21 the A evidence is decaying
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC21{side} A evidence is decaying"));
        set_a(&mut e, |s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.slope = 1,
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.slope = 1,
            _ => {}
        });
        r.bad(&e, i);
        // NC22 the A evidence of another scale
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC22{side} A evidence of another scale"));
        set_a(&mut e, |s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.scale = 100,
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.scale = 100,
            _ => {}
        });
        r.bad(&e, i);
        // NC23 the A evidence of another token
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC23{side} A evidence of another token"));
        set_a(&mut e, |s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.token_cov_id = [0x72; 32],
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.token_cov_id = [0x72; 32],
            _ => {}
        });
        r.bad(&e, i);
        // NC24 the A evidence below minTouch (one base unit under)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC24{side} A evidence below minTouch"));
        let ai = e.input_of_cov(cov(0x93));
        e.set_arg(ai, 0, nb(WHOLE - 1));
        r.bad(&e, i);
        // NC25 the B evidence below the B-side threshold ceil(minTouch * stop / sA)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC25{side} B evidence below the B threshold"));
        let bi = e.input_of_cov(cov(0x94));
        e.set_arg(bi, 0, nb(c.min_touch_b().unwrap() - 1));
        r.bad(&e, i);
        // ---- the B evidence leg (read by evLeg): NC28 another scale, NC29 decaying, NC2T another token, NC2N not filled,
        // NC2E exposed too briefly
        let set_b = |e: &mut Ed, f: &dyn Fn(&mut AnyState)| {
            let bi = e.input_of_cov(cov(0x94));
            let (t, st) = e.entry_state(bi);
            let mut any = AnyState::decode(t, &st).unwrap();
            f(&mut any);
            e.set_entry_state(bi, any.encode());
        };
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC28{side} B evidence of another scale"));
        set_b(&mut e, &|s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.scale = 100,
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.scale = 100,
            _ => {}
        });
        r.bad(&e, i);
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC29{side} B evidence is decaying"));
        set_b(&mut e, &|s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.slope = 1,
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.slope = 1,
            _ => {}
        });
        r.bad(&e, i);
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC2T{side} B evidence of another token"));
        set_b(&mut e, &|s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.token_cov_id = [0x72; 32],
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.token_cov_id = [0x72; 32],
            _ => {}
        });
        r.bad(&e, i);
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC2N{side} B evidence not filled (n = 0)"));
        let bi = e.input_of_cov(cov(0x94));
        e.set_arg(bi, 0, nb(0));
        set_min_touch(&mut e, i, 0);
        r.bad(&e, i);
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC2E{side} B evidence exposed less than minRestDaa"));
        let bi = e.input_of_cov(cov(0x94));
        e.set_input_daa(bi, NOW - 10);
        r.bad(&e, i);
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC2I{side} B evidence interval pushes its exposure past the lock time"));
        set_b(&mut e, &|s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.interval = NOW as i64,
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.interval = NOW as i64,
            _ => {}
        });
        r.bad(&e, i);
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC2A{side} B evidence activeFrom after the lock time"));
        set_b(&mut e, &|s| match s {
            AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.active_from = NOW as i64 + 10,
            AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.active_from = NOW as i64 + 10,
            _ => {}
        });
        r.bad(&e, i);
        // NC2C the A evidence is CANCELLED by its maker, its signature ground so that sigscript bytes [1..9) read as a
        // fill amount >= minTouch (only the 0x08 first-push rule tells a cancel from a fill)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC2C{side} A evidence cancelled, its signature ground to read as a fill"));
        let ai = e.input_of_cov(cov(0x93));
        e.set_entry(ai, "cancel", vec![Arg::Sig(pk(if ask { MAKER_C } else { MAKER_B }))]);
        grind_fill(r, &mut e, ai);
        r.bad(&e, i);
        // NC26 wrong side: the evidence is the order's own counterparties (the settle legs: ASK reads a bid of A and an
        // ask of B, BID an ask of A and a bid of B), each filled in this transaction
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC26{side} evidence on the wrong side (the order's own counterparties)"));
        let (a_cp, b_cp) = if ask { (cov(0x95), cov(0x96)) } else { (cov(0x96), cov(0x95)) };
        let (ea, eb) = (e.input_of_cov(a_cp), e.input_of_cov(b_cp));
        e.set_arg(i, 7, Arg::Int(ea as i64));
        e.set_arg(i, 8, Arg::Int(eb as i64));
        r.bad(&e, i);
    }
}

#[test]
fn cond_pair_evidence_ev0() {
    suite(&mixed_pairs(), evidence_ev0);
}

fn evidence_ev1(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let ev1_ed = |id: &str| {
            let e = edb(id, stop_ev1(ask, pa, pb));
            let i = e.order_in(OID);
            (e, i)
        };
        let set_ev = |e: &mut Ed, f: &dyn Fn(&mut PairState)| {
            let vi = e.input_of_cov(cov(0x93));
            let (_, st) = e.entry_state(vi);
            let AnyState::KobPair(mut s) = AnyState::decode(TemplateId::KobPair, &st).unwrap() else { unreachable!() };
            f(&mut s);
            e.set_entry_state(vi, s.encode());
        };
        // NC30 the pair evidence not filled
        let (mut e, i) = ev1_ed(&format!("NC30{side} pair evidence not filled"));
        let vi = e.input_of_cov(cov(0x93));
        e.set_arg(vi, 0, nb(0));
        set_min_touch(&mut e, i, 0);
        r.bad(&e, i);
        // NC31 the pair evidence is decaying
        let (mut e, i) = ev1_ed(&format!("NC31{side} pair evidence is decaying"));
        set_ev(&mut e, &|s| s.slope = 1);
        r.bad(&e, i);
        // NC32 the pair evidence of another pair
        let (mut e, i) = ev1_ed(&format!("NC32{side} pair evidence of another pair"));
        set_ev(&mut e, &|s| {
            if s.is_ask() {
                s.s_cov_id = [0x72; 32];
            } else {
                s.t_cov_id = [0x72; 32];
            }
        });
        r.bad(&e, i);
        // NC33 the pair evidence of another scale
        let (mut e, i) = ev1_ed(&format!("NC33{side} pair evidence of another scale"));
        set_ev(&mut e, &|s| {
            s.s_scale = 100;
            s.t_scale = 100;
        });
        r.bad(&e, i);
        // NC34 the pair evidence below minTouch (the order's minTouch above the 1 whole A it trades)
        let (mut e, i) = ev1_ed(&format!("NC34{side} pair evidence below minTouch"));
        set_min_touch(&mut e, i, 2 * WHOLE);
        r.bad(&e, i);
        // NC35 wrong side: a sell stop reads a pair ASK; give its own counterparty (a pair BID)
        let (mut e, i) = ev1_ed(&format!("NC35{side} pair evidence on the wrong side"));
        set_ev(&mut e, &|s| s.side = if s.is_ask() { SIDE_BID } else { SIDE_ASK });
        r.bad(&e, i);
        // NC3C the pair evidence CANCELLED by its maker, its signature ground to read as a fill amount >= minTouch
        let (mut e, i) = ev1_ed(&format!("NC3C{side} pair evidence cancelled, its signature ground to read as a fill"));
        let vi = e.input_of_cov(cov(0x93));
        e.set_entry(vi, "cancel", vec![Arg::Sig(pk(MAKER_C))]);
        grind_fill(r, &mut e, vi);
        r.bad(&e, i);
        // NC3E the pair evidence exposed too briefly (its UTXO DAA)
        let (mut e, i) = ev1_ed(&format!("NC3E{side} pair evidence exposed less than minRestDaa"));
        let vi = e.input_of_cov(cov(0x93));
        e.set_input_daa(vi, NOW - 10);
        r.bad(&e, i);
        // NC36 the implied-rate edge: one unit past the stop refused (the boundary positive is PC37)
        let (mut e, i) = ev1_ed(&format!("NC36{side} pair evidence one unit past the stop"));
        set_ev(&mut e, &|s| s.price = if s.is_ask() { RATE + 1 } else { RATE - 1 });
        r.bad(&e, i);
        // PC37 the implied-rate boundary accepted: the pair evidence priced exactly at the stop
        r.ok(&edb(&format!("PC37{side} pair evidence at the stop exactly"), stop_ev1_px(ask, pa, pb, RATE)));
    }
}

#[test]
fn cond_pair_evidence_ev1() {
    suite(&mixed_pairs(), evidence_ev1);
}

// ---------------------------------------------------------------- evidence: exposure, location, look-alike

fn evidence_exposure(r: &Run, pa: TemplateId, pb: TemplateId) {
    let honest = |id: &str| ev0_ed(true, pa, pb, id);
    // NC50 unexposed: the A evidence UTXO too fresh (the UTXO-DAA exposure term)
    let (mut e, i) = honest("NC50 A evidence exposed less than minRestDaa (UTXO DAA)");
    let ai = e.input_of_cov(cov(0x93));
    e.set_input_daa(ai, NOW - 10);
    r.bad(&e, i);
    // NC52 the A evidence activeFrom after the lock time (the activeFrom exposure term)
    let (mut e, i) = honest("NC52 A evidence activeFrom after the lock time");
    set_a(&mut e, |s| {
        if let AnyState::KobAsk(x) | AnyState::KobAskKron(x) = s {
            x.active_from = NOW as i64 + 10;
        }
    });
    r.bad(&e, i);
    // NC53 the A evidence interval too long (its interval pushes exposure past the lock time)
    let (mut e, i) = honest("NC53 A evidence interval pushes exposure past the lock time");
    set_a(&mut e, |s| {
        if let AnyState::KobAsk(x) | AnyState::KobAskKron(x) = s {
            x.interval = NOW as i64;
        }
    });
    r.bad(&e, i);
    // evidence location: NC54 at the order itself, NC55 at a token input, NC56 at a P2PK input
    let (mut e, i) = honest("NC54 evidence at the order itself");
    e.set_arg(i, 7, Arg::Int(i as i64));
    r.bad(&e, i);
    let (mut e, i) = honest("NC55 evidence at a token input");
    let tk = e.arg_int(i, 9) as usize;
    e.set_arg(i, 7, Arg::Int(tk as i64));
    r.bad(&e, i);
    let (mut e, i) = honest("NC56 evidence at a P2PK input");
    let j = e.add_p2pk_input(MATCHER, 0xea);
    e.set_arg(i, 7, Arg::Int(j as i64));
    r.bad(&e, i);
    // the custody of the ask leg (tk): the A leg of a sell stop (read by rd), the B leg of a buy stop (read by evLeg)
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        // NC51 the ask evidence's custody exposed too briefly (the custody exposure term)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC51{side} ask evidence custody exposed less than minRestDaa"));
        let tk = e.arg_int(i, 9) as usize;
        e.set_input_daa(tk, NOW - 10);
        r.bad(&e, i);
        // NC57 tk is a token input of the right token not owned by the evidence ask (the order's own custody / a
        // custody of another order)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC57{side} ask evidence custody owned by another covenant"));
        let tk = e.arg_int(i, 9) as usize;
        let tok = cov_of(&e.entries[tk]).unwrap();
        let wrong = (0..e.plans.len())
            .find(|k| {
                *k != tk
                    && cov_of(&e.entries[*k]) == Some(tok)
                    && !matches!(e.plans[*k], kob_protocol::tx::SigPlan::Entry { .. } | kob_protocol::tx::SigPlan::P2pk { .. })
                    && e.in_tok(*k).is_covenant_owned()
            })
            .expect("another custody of the token");
        e.set_arg(i, 9, Arg::Int(wrong as i64));
        r.bad(&e, i);
        // NC5D the B evidence CANCELLED by its maker, its signature ground to read as a fill (evLeg's 0x08 rule)
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC5D{side} B evidence cancelled, its signature ground to read as a fill"));
        let bi = e.input_of_cov(cov(0x94));
        e.set_entry(bi, "cancel", vec![Arg::Sig(pk(if ask { MAKER_B } else { MAKER_C }))]);
        grind_fill_n(r, &mut e, bi, c_min_b(ask, pa, pb));
        r.bad(&e, i);
    }
    // NC58 a look-alike template as the A evidence: a P2SH UTXO of the evidence's covenant id whose redeem script carries
    // a genuine-looking KobAsk state at the KobAsk offsets but other prefix / suffix bytes (another template hash); its
    // sigscript starts with the 8-byte fill push like a real fill
    // NC5P the A evidence is a planted non-P2SH UTXO of its covenant id carrying the GENUINE KobAsk redeem bytes
    for (id, look) in
        [("NC58 A evidence of a look-alike template", true), ("NC5P A evidence planted (not P2SH, genuine redeem bytes)", false)]
    {
        let (e, i) = honest(id);
        let ai = e.input_of_cov(cov(0x93));
        let (t, st) = e.entry_state(ai);
        let acov = cov_of(&e.entries[ai]).unwrap();
        let tpl = kob_protocol::artifacts::template(t);
        let rs = if look {
            let mut rs = vec![kaspa_txscript::opcodes::codes::OpDrop];
            rs.extend(fake_rs(tpl.prefix.len() - 1, &st, tpl.suffix.len()));
            rs
        } else {
            [tpl.prefix.as_slice(), &st, tpl.suffix.as_slice()].concat()
        };
        let mut ss = vec![0x08u8];
        ss.extend_from_slice(&WHOLE.to_le_bytes());
        ss.extend(kob_protocol::script::push_data(&rs));
        r.bad_patched(&e, i, &move |tx, en| {
            plant_input(tx, en, ai, acov, &rs, look);
            if !look {
                use kaspa_txscript::opcodes::codes::{Op2Drop, OpTrue};
                let x = &en[ai];
                en[ai] = kaspa_consensus_core::tx::UtxoEntry::new(
                    x.amount,
                    kaspa_consensus_core::tx::ScriptPublicKey::new(0, vec![Op2Drop, OpTrue].into()),
                    x.block_daa_score,
                    false,
                    x.covenant_id,
                );
            }
            tx.inputs[ai].signature_script = ss.clone();
        });
    }
}

/// The B threshold of the mode-0 trigger of side `ask`.
fn c_min_b(ask: bool, pa: TemplateId, pb: TemplateId) -> i64 {
    cside(ask, pa, pb).min_touch_b().unwrap()
}

/// [`grind_fill`] with a least amount `min`.
fn grind_fill_n(r: &Run, e: &mut Ed, ai: usize, min: i64) {
    for _ in 0..400 {
        let (tx, _) = e.finish(r.subs);
        let ss = &tx.inputs[ai].signature_script;
        let n = i64::from_le_bytes(ss[1..9].try_into().unwrap());
        if n >= min {
            return;
        }
        let c = e.change_out();
        let v = e.value(c);
        e.set_value(c, v - 1);
    }
    panic!("{}: no signature reads as a fill", e.name);
}

#[test]
fn cond_pair_evidence_exposure() {
    suite(&mixed_pairs(), evidence_exposure);
}

// ---------------------------------------------------------------- trailing and update encodings

/// A trailing update of a cond order (mode 1): the order is `c`, trailed by the evidence pair at `cov(0x93)` netted
/// against `cov(0x94)`. Returns (ed, the update input). The honest k the builder chose is at arg 12.
fn trail_update(id: &str, c: CondPairState, pa: TemplateId, pb: TemplateId) -> (Ed, usize) {
    let ask = c.is_ask();
    // trailing evidence is the OTHER direction: ASK trails on a pair BID, BID on a pair ASK
    let ev = if ask { pair(MAKER_C, false, pa, pb, RATE + 10) } else { pair(MAKER_C, true, pa, pb, RATE - 10) };
    let opp = if ask { pair(MAKER_B, true, pa, pb, RATE) } else { pair(MAKER_B, false, pa, pb, RATE) };
    let mut b = route(vec![pair_leg(ev, pa, pb, cov(0x93), 72, 1), pair_leg(opp, pa, pb, cov(0x94), 74, 1)]);
    b.updates.push(BatchUpdate {
        order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
        evidence: 0,
        evidence_b: None,
        take: None,
    });
    let e = edb(id, b);
    let i = e.order_in(OID);
    (e, i)
}

fn trailing(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        // a trailing order: step 10, gap 20, stop far from the market so a multi-step ratchet is justified
        let stop = if ask { RATE - 300 } else { RATE + 300 };
        let base = CondPairState { trail_step: 10, trail_gap: 20, trail_wait: 0, stop_price: stop, ..cside(ask, pa, pb) };
        // positive: the builder's maximal k
        let (e, _) = trail_update(&format!("PC40{side} trailing ratchet, maximal k"), base.clone(), pa, pb);
        r.ok(&e);
        // NC60 k one step beyond maximal (validity fails: s' + gap no longer reached by the rate)
        let (mut e, i) = trail_update(&format!("NC60{side} trailing k one step too large"), base.clone(), pa, pb);
        let k = e.arg_int(i, 12);
        e.set_arg(i, 12, Arg::Int(k + 1));
        let stop2 = if ask { stop + (k + 1) * 10 } else { stop - (k + 1) * 10 };
        restate_cont(&mut e, i, |s| s.stop_price = stop2);
        r.bad(&e, i);
        // NC61 k one step short of maximal (maximality fails)
        let (mut e, i) = trail_update(&format!("NC61{side} trailing k not maximal"), base.clone(), pa, pb);
        let k = e.arg_int(i, 12);
        if k >= 2 {
            e.set_arg(i, 12, Arg::Int(k - 1));
            let stop2 = if ask { stop + (k - 1) * 10 } else { stop - (k - 1) * 10 };
            restate_cont(&mut e, i, |s| s.stop_price = stop2);
            r.bad(&e, i);
        }
        // NC62 k = 0 as a trail (the trail branch requires k > 0; k = 0 is an arm that moves nothing)
        let (mut e, i) =
            trail_update(&format!("NC62{side} trailing with k = 0 (drains keeperTip, moves nothing)"), base.clone(), pa, pb);
        e.set_arg(i, 12, Arg::Int(0));
        restate_cont(&mut e, i, |s| s.stop_price = stop);
        // k = 0 turns this into an arm; an already-trailing stop is unarmed so it would arm — the attack is that it
        // ratchets without moving the stop. The cond refuses a trail with k <= 0 only inside the trail branch, so k = 0
        // is an arm: this is a POSITIVE (arm), not an attack. Skip.
        let _ = (&mut e, i);
        // NC63 trailWait not elapsed (ageDaa < trailWait): set trailWait high
        // (the builder sets the input's sequence to trailWait; one DAA short of it)
        let hi = CondPairState { trail_wait: 100, ..base.clone() };
        let (mut e, i) = trail_update(&format!("NC63{side} trail before trailWait"), hi, pa, pb);
        e.tx.inputs[i].sequence = 99;
        r.bad(&e, i);
        // NC64 trailing past the TP cap / floor: set the TP so newStop crosses it
        let capped = if ask {
            CondPairState { tp_price: stop + 15, ..base.clone() } // one step (10) ok, two (20) would reach/pass tp
        } else {
            CondPairState { tp_price: stop - 15, ..base.clone() }
        };
        let (mut e, i) = trail_update(&format!("NC64{side} trailing past the TP cap / floor"), capped, pa, pb);
        // force k = 2 (newStop crosses the cap)
        e.set_arg(i, 12, Arg::Int(2));
        let stop2 = if ask { stop + 20 } else { stop - 20 };
        restate_cont(&mut e, i, |s| s.stop_price = stop2);
        r.bad(&e, i);
    }
}

/// Sets the update continuation's stopPrice (the only mutable field a trail changes) through `f`.
fn restate_cont(e: &mut Ed, i: usize, f: impl Fn(&mut CondPairState)) {
    let c = cov_of(&e.entries[i]).unwrap();
    if let Some(k) = e.cont_out(c) {
        let (_, st) = e.out_order(k).unwrap();
        let AnyState::KobCondPair(mut s) = AnyState::decode(TemplateId::KobCondPair, &st).unwrap() else { unreachable!() };
        f(&mut s);
        e.set_out_spk_state(k, TemplateId::KobCondPair, &s.encode());
    }
}

#[test]
fn cond_pair_trailing() {
    suite(&mixed_pairs(), trailing);
}

fn updates(r: &Run, pa: TemplateId, pb: TemplateId) {
    // an arm update of a sell stop (mode 0), the base for encoding attacks
    let arm = |id: &str| {
        let c = ca(pa, pb);
        let mut b = route(vec![
            kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 72, 5, 1),
            kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 74, 5, 1),
            kbid_leg(TOKEN_COV, pa, 24, P260, cov(0x95), 76, 10, 1),
            kask_leg(TOKEN_B, pb, 25, P250, cov(0x96), 78, 10, 1),
        ]);
        b.updates.push(BatchUpdate {
            order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
            evidence: 0,
            evidence_b: Some(1),
            take: None,
        });
        let e = edb(id, b);
        let i = e.order_in(OID);
        (e, i)
    };
    let arm_bid = |id: &str| {
        let c = cb(pa, pb);
        let mut b = route(vec![
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 5, 1),
            kask_leg(TOKEN_COV, pa, 24, P250, cov(0x95), 76, 10, 1),
            kbid_leg(TOKEN_B, pb, 25, P260, cov(0x96), 78, 10, 1),
        ]);
        b.updates.push(BatchUpdate {
            order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
            evidence: 0,
            evidence_b: Some(1),
            take: None,
        });
        let e = edb(id, b);
        let i = e.order_in(OID);
        (e, i)
    };
    r.ok(&{
        let (e, _) = arm("PC50 arm update, mode 0");
        e
    });
    r.ok(&{
        let (e, _) = arm_bid("PC51 arm update of a buy stop, mode 0");
        e
    });
    // NC70 upd = 1 with n != 0 (the fill-or-update discriminant)
    let (mut e, i) = arm("NC70 update with n != 0");
    e.set_arg(i, 0, nb(1));
    r.bad(&e, i);
    // NC71 upd = 2 (outside 0 / 1)
    let (mut e, i) = arm("NC71 upd = 2 (not 0 or 1)");
    e.set_arg(i, 11, Arg::Int(2));
    r.bad(&e, i);
    // NC72 an update of an already-armed stop (armed != 0)
    let (mut e, i) = arm("NC72 update of an already-armed stop");
    restate(&mut e, i, |s| s.armed = 1);
    restate_cont(&mut e, i, |s| s.armed = 1);
    r.bad(&e, i);
    // NC73 an update of a BID order without a stop leg (stopPrice == 0): its arm comparison a >= ceil(0) always holds,
    // so only `stopPrice > 0` keeps a keeper from taking keeperTip for nothing
    let (mut e, i) = arm_bid("NC73 update of an order with no stop leg");
    restate(&mut e, i, |s| s.stop_price = 0);
    restate_cont(&mut e, i, |s| s.stop_price = 0);
    r.bad(&e, i);
    // NC74 the keeper takes more than keeperTip (the continuation one sompi short)
    let (mut e, i) = arm("NC74 keeper takes more than keeperTip");
    let c = cov_of(&e.entries[i]).unwrap();
    let k = e.cont_out(c).unwrap();
    e.fund_out(k, -1);
    r.bad(&e, i);
    // NC75 hostile negative keeperTip (the keeper funds it into the continuation)
    let (mut e, i) = arm("NC75 hostile negative keeperTip");
    let kt = cst(&e, i).keeper_tip;
    restate(&mut e, i, |s| s.keeper_tip = -1);
    restate_cont(&mut e, i, |s| s.keeper_tip = -1);
    let c = cov_of(&e.entries[i]).unwrap();
    let k = e.cont_out(c).unwrap();
    e.fund_out(k, kt + 1);
    r.bad(&e, i);
    // NC76 an update that spends an S input owned by the order (update spends no token of this id)
    let (mut e, i) = arm("NC76 update spends an S input of the order");
    let st = e.add_stray(TOKEN_COV, pa, OID, 3, 99);
    // the custody argument names the stray: only the update rule (no custody exempted) refuses it
    e.set_arg(i, 1, Arg::Int(st as i64));
    r.bad(&e, i);
    // NC77 an update that spends a T input owned by the order
    let (mut e, i) = arm("NC77 update spends a T input of the order");
    e.add_stray(TOKEN_B, pb, OID, 3, 99);
    r.bad(&e, i);
    // NC78 an update before activeFrom
    let (mut e, i) = arm("NC78 update before activeFrom");
    restate(&mut e, i, |s| s.active_from = NOW as i64 + 1);
    restate_cont(&mut e, i, |s| s.active_from = NOW as i64 + 1);
    r.bad(&e, i);
}

#[test]
fn cond_pair_updates() {
    suite(&mixed_pairs(), updates);
}

// ---------------------------------------------------------------- refunds, cancels

fn lifecycle(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let c = || cside(ask, pa, pb);
        // NC80 refund before expiry
        let mut e = ed(
            &format!("NC80{side} refund one DAA before expiry"),
            refund_of(CondPairState { expiry_daa: 5_000_000, ..c() }, pa, pb, 5_000_000),
        );
        e.tx.lock_time = 4_999_999;
        r.bad(&e, 0);
        // NC81 refund before 90 days idle
        let mut e = ed(
            &format!("NC81{side} refund before 90 days idle"),
            refund_of(CondPairState { expiry_daa: NO_EXPIRY, ..c() }, pa, pb, (ODAA + MAX_IDLE) as u64),
        );
        e.tx.lock_time = (ODAA + MAX_IDLE - 1) as u64;
        r.bad(&e, 0);
        // NC82 a refund of a zero custody (output i unpinned)
        let mut e = ed(&format!("NC82{side} refund of a zero custody"), refund_of(c(), pa, pb, EXPIRY as u64));
        restate(&mut e, 0, |s| s.custody = 0);
        let ci = cin_refund(&e);
        let st = e.in_tok(ci);
        e.set_in_tok(ci, st.with_amount(0));
        let st0 = e.out_state(0);
        let v = e.value(0);
        e.unbind(0, KEEPER);
        e.set_value(0, v);
        e.add_token_output(s_tok(ask), st0.with_amount(0).with_user_owner(pk(KEEPER)), 0);
        r.bad(&e, 0);
        // NC83 the keeper takes more than refundTip
        let mut e = ed(&format!("NC83{side} refund: keeper over refundTip"), refund_of(c(), pa, pb, EXPIRY as u64));
        let rt = c().refund_tip;
        let v = e.value(0);
        e.set_value(0, v - rt as u64 - 1);
        let ch = e.change_out();
        let vc = e.value(ch);
        e.set_value(ch, vc + rt as u64 + 1);
        r.bad(&e, 0);
        // NC84 the custody refunded to the keeper
        let mut e = ed(&format!("NC84{side} refund: the custody to the keeper"), refund_of(c(), pa, pb, EXPIRY as u64));
        let st = e.out_state(0);
        e.set_out_state(0, st.with_user_owner(pk(KEEPER)));
        r.bad(&e, 0);
        // NC85 a refund leaving an output bound to the order id
        let mut e = ed(&format!("NC85{side} refund leaving an output bound to the order id"), refund_of(c(), pa, pb, EXPIRY as u64));
        e.forge_bound(OID, 0, KAS);
        r.bad(&e, 0);
        // NC86 cancel with a non-ALL sighash, NC87 cancel by another key
        let mut e = ed(&format!("NC86{side} cancel with a non-ALL sighash"), cancel_of(c(), pa, pb, vec![]));
        e.sighash_types.insert(0, 2);
        r.bad(&e, 0);
        let mut e = ed(&format!("NC87{side} cancel signed by another key"), cancel_of(c(), pa, pb, vec![]));
        e.sign_as(0, MAKER_B);
        r.bad(&e, 0);
    }
}

/// The custody input of a refund (settle arg 1).
fn cin_refund(e: &Ed) -> usize {
    e.arg_int(0, 1) as usize
}

#[test]
fn cond_pair_lifecycle() {
    suite(&mixed_pairs(), lifecycle);
}

// ---------------------------------------------------------------- overflow

fn overflow(r: &Run, pa: TemplateId, pb: TemplateId) {
    let a = CondPairState { tp_price: RATE + 1, ..ca(pa, pb) };
    let (mut e, i) = tp_ed("NC90 the leg-price product overflows", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.tp_price = i64::MAX / 3);
    r.bad(&e, i);
    let (mut e, i) = tp_ed("NC91 the tip product overflows", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.tip = i64::MAX / 3);
    r.bad(&e, i);
    // the stop-band product stopPrice * slipBps must fit (MAX_STOP); a stop above it fails closed
    let armed = CondPairState {
        armed: 1,
        band_daa: 0,
        amount_left: 4 * WHOLE,
        custody: 4 * WHOLE,
        stop_price: 922_337_203_685_478,
        ..ca(pa, pb)
    };
    let (mut e, i) = tp_ed("NC92 stopPrice above MAX_STOP (band product overflows)", armed.clone(), pa, pb, 4 * WHOLE);
    // rebuild as a stop fill: set leg 1
    e.set_arg(i, 6, Arg::Int(1));
    r.bad(&e, i);
}

#[test]
fn cond_pair_overflow() {
    suite(&mixed_pairs(), overflow);
}

// ---------------------------------------------------------------- the C1 fake quotes read as evidence

/// A mode-1 arm of a sell stop (`ask`) or a buy stop by the resting pair order `ev` (cov 0x93, filled for one whole A:
/// a pair ASK arms a sell stop, a pair BID a buy stop), netted against a pair order of the other side (cov 0x94): the
/// stop leg filled for 4 whole A in the same transaction (`upd` false), or an arm update without a fill (`upd` true).
fn fake_batch(ask: bool, pa: TemplateId, pb: TemplateId, ev: PairState, upd: bool) -> Batch {
    let c = cside(ask, pa, pb);
    let opp = pair(MAKER_B, !ask, pa, pb, RATE);
    if upd {
        let mut b = route(vec![pair_leg(ev, pa, pb, cov(0x93), 72, 1), pair_leg(opp, pa, pb, cov(0x94), 74, 1)]);
        b.updates.push(BatchUpdate {
            order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(c)),
            evidence: 0,
            evidence_b: None,
            take: None,
        });
        b
    } else {
        let legs = vec![
            cond_pair_leg(c, pa, pb, OID, 70, 4, 1),
            pair_leg(ev, pa, pb, cov(0x93), 72, 1),
            pair_leg(opp, pa, pb, cov(0x94), 74, 5),
        ];
        with_evidence(route(legs), 1, None)
    }
}

/// The KobPair state of entry input `i`.
fn pst_in(e: &Ed, i: usize) -> PairState {
    let AnyState::KobPair(s) = AnyState::decode(TemplateId::KobPair, &e.entry_state(i).1).unwrap() else { unreachable!() };
    s
}
fn set_pst_in(e: &mut Ed, i: usize, f: impl Fn(&mut PairState)) {
    let mut s = pst_in(e, i);
    f(&mut s);
    e.set_entry_state(i, s.encode());
}
/// Edits the continuation of the pair order of covenant `c` (its state) through `f`.
fn set_pst_cont(e: &mut Ed, c: [u8; 32], f: impl Fn(&mut PairState)) {
    let k = e.cont_out(c).expect("pair continuation");
    let (_, st) = e.out_order(k).unwrap();
    let AnyState::KobPair(mut s) = AnyState::decode(TemplateId::KobPair, &st).unwrap() else { unreachable!() };
    f(&mut s);
    e.set_out_spk_state(k, TemplateId::KobPair, &s.encode());
}

/// The fake-quote evidence `vi` is refused by its own KobPair input; the conditional accepts the evidence (checked on
/// the committed contracts: the conditional reads only the pair order's state, so a fake quote is stopped by KobPair
/// alone, and ablating that KobPair check must flip the scenario on every input).
fn bad_fake(r: &Run, e: &Ed, vi: usize) {
    if !ablating() {
        let ci = e.order_in(OID);
        let res = r.exec(e);
        assert!(res[ci].is_ok(), "{} {}: the conditional itself refuses the fake evidence: {:?}", e.name, r.pair, res[ci]);
    }
    r.bad(e, vi);
}

/// The C1 fake quotes (KobPair suite NP10 / NP11 / NP12) as the trigger evidence of a mode-1 stop: every one would arm
/// the stop (in a fill, and in an arm update where the keeper takes keeperTip) if KobPair let it be filled. The committed
/// KobPair refuses the fill, so the evidence input rejects (the pair catalog's Q4 / Q5 / Q6 expect these scenarios: with
/// the KobPair check ablated, the fake quote fills AND arms). Honest twins PC43..PC45 from the same builds.
fn fake_quotes(r: &Run, pa: TemplateId, pb: TemplateId) {
    for upd in [false, true] {
        let u = if upd { "u" } else { "" };
        let how = if upd { "an arm update" } else { "the stop fill" };
        let id = |s: &str| with_suffix(s, u);
        // NC40 / NC43 a sell stop armed by a pair ASK whose custody is amountLeft - 1 / + 1 (the custody input holds
        // exactly `custody`; only KobPair's `custody == amountLeft` refuses it)
        let ev = pair(MAKER_C, true, pa, pb, RATE - 10);
        let honest = edb(&id(&format!("PC43 sell stop armed by a pair ASK ({how})")), fake_batch(true, pa, pb, ev.clone(), upd));
        r.ok(&honest);
        for (base, d) in [("NC40", -1i64), ("NC43", 1)] {
            let mut e = honest.clone().named(&id(&format!("{base} {how} armed by a pair ASK whose custody is amountLeft {d:+}")));
            let vi = e.input_of_cov(cov(0x93));
            set_pst_in(&mut e, vi, |s| s.custody += d);
            let c = e.arg_int(vi, 1) as usize;
            let st = e.in_tok(c);
            e.set_in_tok(c, st.with_amount(st.amount() + d));
            e.add_out_amount(c, d);
            set_pst_cont(&mut e, cov(0x93), |s| s.custody += d);
            bad_fake(r, &e, vi);
        }
        // NC41 a buy stop armed by an unfunded pair BID: its last whole A filled from an escrow one unit below the quote
        // (outAmount = -1: no S output, the matcher short of one unit); only KobPair's `outAmount >= 0` refuses it
        let b0 = with_left(pair(MAKER_C, false, pa, pb, RATE + 10), 1);
        let q = b0.s_out(WHOLE, RATE + 10).unwrap();
        let ev = PairState { custody: q, ..b0 };
        let honest = edb(
            &id(&format!("PC44 buy stop armed by a pair BID paying its exact escrow ({how})")),
            fake_batch(false, pa, pb, ev, upd),
        );
        r.ok(&honest);
        let mut e = honest.clone().named(&id(&format!("NC41 {how} armed by an unfunded pair BID (escrow one unit below its quote)")));
        let vi = e.input_of_cov(cov(0x93));
        set_pst_in(&mut e, vi, |s| s.custody = q - 1);
        let c = e.arg_int(vi, 1) as usize;
        let st = e.in_tok(c);
        e.set_in_tok(c, st.with_amount(q - 1));
        // the unit the escrow lacks: the pair ASK on the other side (cov 0x94) is paid its surplus (the bid's RATE + 10
        // over its RATE), one unit of it less
        let oi = e.input_of_cov(cov(0x94));
        let (o, no) = (pst_in(&e, oi), fill_n(&e.plans[oi]).unwrap());
        let ceil = ((no as i128 * o.price as i128 + o.s_scale as i128 - 1) / o.s_scale as i128) as i64;
        assert!(e.arg_int(oi, 5) > ceil, "{}: the pair ASK has no surplus to give up", e.name);
        e.set_arg(oi, 5, Arg::Int(e.arg_int(oi, 5) - 1));
        e.add_out_amount(oi, -1);
        bad_fake(r, &e, vi);
        // NC42 a stop armed by a carrier wall: the evidence pair UTXO does not fund deliveryCarrier + tip (the matcher funds
        // the delivery carrier, the continuation keeps nothing); only KobPair's `inputs[self] >= deliveryCarrier + tipKas`
        // refuses it. a: a pair ASK arming a sell stop, b: a pair BID arming a buy stop.
        for (side, ask) in [("a", true), ("b", false)] {
            let ev = PairState { tip: 10_000, ..pair(MAKER_C, ask, pa, pb, if ask { RATE - 10 } else { RATE + 10 }) };
            let honest =
                edb(&id(&format!("PC45{side} stop armed by a tipped pair order ({how})")), fake_batch(ask, pa, pb, ev.clone(), upd));
            r.ok(&honest);
            let mut e = honest
                .clone()
                .named(&id(&format!("NC42{side} {how} armed by a carrier wall (the pair UTXO does not fund its carrier and tip)")));
            let vi = e.input_of_cov(cov(0x93));
            let v = (ev.delivery_carrier + ev.tip_kas(WHOLE).unwrap() - 1) as u64;
            let k = e.cont_out(cov(0x93)).unwrap();
            let cv = e.value(k);
            let old = e.entries[vi].amount;
            e.set_input_value(vi, v);
            e.set_value(k, 0);
            let ch = e.change_out();
            let vc = e.value(ch);
            e.set_value(ch, vc + cv - (old - v));
            bad_fake(r, &e, vi);
        }
    }
}

#[test]
fn cond_pair_evidence_fake_quotes() {
    suite(&mixed_pairs(), fake_quotes);
}

/// Grinds the signature of input `ai` (a maker's cancel) until its sigscript bytes [1..9) read as a fill amount of at
/// least one whole token (the matcher's change shifts by one sompi per try; every try is a fresh signature).
fn grind_fill(r: &Run, e: &mut Ed, ai: usize) {
    for _ in 0..400 {
        let (tx, _) = e.finish(r.subs);
        let ss = &tx.inputs[ai].signature_script;
        let n = i64::from_le_bytes(ss[1..9].try_into().unwrap());
        if n >= WHOLE {
            return;
        }
        let c = e.change_out();
        let v = e.value(c);
        e.set_value(c, v - 1);
    }
    panic!("{}: no signature reads as a fill", e.name);
}

/// Sets the order's minTouch on its input and on its continuation (a stop fill rests).
fn set_min_touch(e: &mut Ed, i: usize, v: i64) {
    restate(e, i, |s| s.min_touch = v);
    restate_cont(e, i, |s| s.min_touch = v);
}

/// NC59: the ask evidence's custody (tk) is a UTXO of the OTHER token owned by the evidence ask: its covenant marker is
/// right, only the token check refuses it. Run where both tokens share one program (KCC-20 / KCC-20): on mixed families
/// the marker is read at the other family's offset and fails as well (defence in depth).
fn evidence_tk(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let (mut e, i) = ev0_ed(ask, pa, pb, &format!("NC59{side} ask evidence custody of another token"));
        let tk = e.arg_int(i, 9) as usize;
        let tok = cov_of(&e.entries[tk]).unwrap();
        let owner = if ask { cov(0x93) } else { cov(0x94) };
        let (other, op) = if tok == TOKEN_COV { (TOKEN_B, pb) } else { (TOKEN_COV, pa) };
        let st = e.add_stray(other, op, owner, 7, 98);
        e.set_arg(i, 9, Arg::Int(st as i64));
        r.bad(&e, i);
    }
}

#[test]
fn cond_pair_evidence_tk() {
    suite(&family_mixes()[..1], evidence_tk);
}

// ---------------------------------------------------------------- the custody: identity, codec, planted inputs

/// A take-profit fill of NR that sells out with nothing left at the custody's index (an ask: custody NR; a bid: its
/// exact quote at the leg price).
fn cclosing(ask: bool, pa: TemplateId, pb: TemplateId) -> CondPairState {
    let c = CondPairState { tp_price: if ask { RATE + 1 } else { RATE - 1 }, ..cleft(cside(ask, pa, pb), NR) };
    if ask {
        c
    } else {
        let q = c.s_out(NR, c.tp_price).unwrap();
        CondPairState { custody: q, ..c }
    }
}
fn crest(ask: bool, pa: TemplateId, pb: TemplateId) -> CondPairState {
    CondPairState { tp_price: if ask { RATE + 1 } else { RATE - 1 }, ..cside(ask, pa, pb) }
}

fn custody(r: &Run, pa: TemplateId, pb: TemplateId) {
    // ---- per family of S
    for fam in [Family::Kcc20, Family::Kron] {
        let ask = side_with(fam, true, pa);
        let f = fam_tag(s_prog(ask, pa, pb));
        // NC100s the custody owned by another covenant (a custody of another order); the token program also refuses it
        let (mut e, i) = tp_ed(&format!("NC100s{f} custody owned by another covenant"), crest(ask, pa, pb), pa, pb, NR);
        let ci = cin(&e, i);
        let st = e.in_tok(ci);
        e.set_in_tok(ci, map_tok(st, |k| Kcc20State { owner: [0x83; 32], ..k }, |k| KronState { owner: [0x83; 32], ..k }));
        r.bad(&e, i);
        // NC101s the custody marker: KCC-20 borrowing enabled / KRON owner type 3 (address)
        let (mut e, i) = tp_ed(&format!("NC101s{f} custody under another owner type"), crest(ask, pa, pb), pa, pb, NR);
        let ci = cin(&e, i);
        let st = e.in_tok(ci);
        e.set_in_tok(ci, map_tok(st, |k| Kcc20State { borrow_scheme: 1, ..k }, |k| KronState { id_type: 3, ..k }));
        r.bad(&e, i);
        if fam == Family::Kron {
            // NC102sr a minter custody
            let (mut e, i) = tp_ed("NC102sr KRON custody flagged as a minter", crest(ask, pa, pb), pa, pb, NR);
            let ci = cin(&e, i);
            let st = e.in_tok(ci);
            e.set_in_tok(ci, map_tok(st, |k| k, |k| KronState { is_minter: 1, ..k }));
            r.bad(&e, i);
        } else {
            // NC102sk hostile sFamily 3 (read as KCC-20)
            let (mut e, i) = tp_ed("NC102sk hostile sFamily 3", crest(ask, pa, pb), pa, pb, NR);
            restate(&mut e, i, |s| s.s_family = 3);
            restate_cont(&mut e, i, |s| s.s_family = 3);
            r.bad(&e, i);
            // NC103tk hostile tFamily 3 (read as KCC-20)
            let tk_ask = side_with(Family::Kcc20, false, pa);
            let (mut e, i) = tp_ed("NC103tk hostile tFamily 3", crest(tk_ask, pa, pb), pa, pb, NR);
            restate(&mut e, i, |s| s.t_family = 3);
            restate_cont(&mut e, i, |s| s.t_family = 3);
            r.bad(&e, i);
        }
    }
    // ---- generic: the custody's token, P2SH, template (sold out: no S output at all)
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let sp = s_prog(ask, pa, pb);
        let stok = s_tok(ask);
        // NC104 the custody is a UTXO of another token C (same program) owned by the order
        let (mut e, i) = tp_ed(&format!("NC104{side} custody of another token owned by the order"), cclosing(ask, pa, pb), pa, pb, NR);
        let ci = cin(&e, i);
        let en = &e.entries[ci];
        e.entries[ci] = kaspa_consensus_core::tx::UtxoEntry::new(
            en.amount,
            en.script_public_key.clone(),
            en.block_daa_score,
            false,
            Some(kaspa_consensus_core::Hash::from_bytes([0x72; 32])),
        );
        e.programs.remove(&stok);
        e.programs.insert([0x72; 32], sp);
        let m = e.tok_out_of(stok, pk(MATCHER));
        let st = e.out_state(m);
        e.tok_out.insert(m, ([0x72; 32], st));
        r.bad(&e, i);
        // NC105 / NC106 a planted custody (a UTXO of the S covenant id that is no S token): not P2SH / a look-alike
        for (id, p2sh) in [("NC105", false), ("NC106", true)] {
            let what = if p2sh { "P2SH of a look-alike template" } else { "not P2SH (its pushed redeem script is never run)" };
            let (mut e, i) = tp_ed(&format!("{id}{side} planted custody: {what}"), cclosing(ask, pa, pb), pa, pb, NR);
            let ci = cin(&e, i);
            let m = e.tok_out_of(stok, pk(MATCHER));
            e.unbind(m, MATCHER);
            e.programs.remove(&stok);
            let state = e.in_tok(ci).encode();
            let tpl = kob_protocol::artifacts::token_template(sp);
            let rs = if p2sh {
                fake_rs(tpl.prefix.len(), &state, tpl.suffix.len())
            } else {
                [tpl.prefix.as_slice(), &state, tpl.suffix.as_slice()].concat()
            };
            e.make_p2pk(ci, 30);
            r.bad_patched(&e, i, &move |tx, en| plant_input(tx, en, ci, stok, &rs, p2sh));
        }
    }
}

#[test]
fn cond_pair_custody() {
    suite(&mixed_pairs(), custody);
}

// ---------------------------------------------------------------- fill parameters, legs, the stop band and auction

/// Sets the maker's T of an ASK leg fill to `want` (the difference to / from the matcher's T).
fn set_t_out(e: &mut Ed, i: usize, want: i64) {
    let have = e.arg_int(i, 5);
    e.set_arg(i, 5, Arg::Int(want));
    e.add_out_amount(i, want - have);
    let m = e.tok_out_of(TOKEN_B, pk(MATCHER));
    e.add_out_amount(m, have - want);
}

/// An armed ASK stop (origin `armed`, band `band_daa`) sold out on its stop leg at an inventory counterparty.
fn armed_ask(pa: TemplateId, pb: TemplateId, armed: i64, band_daa: i64) -> CondPairState {
    CondPairState { armed, band_daa, ..cleft(ca(pa, pb), 4 * WHOLE) }
}
fn stop_inv(c: CondPairState, pa: TemplateId, pb: TemplateId, name: &str) -> (Ed, usize) {
    let mut b = route(vec![cond_pair_leg_amount(c, pa, pb, OID, 70, 4 * WHOLE, 1)]);
    b.taker_tokens = vec![tutxo(pb, TOKEN_B, 60, 10 * WHOLE, pk(MATCHER), false)];
    let e = edb(name, b);
    let i = e.order_in(OID);
    (e, i)
}

fn fills(r: &Run, pa: TemplateId, pb: TemplateId) {
    let a = crest(true, pa, pb);
    let b = crest(false, pa, pb);
    // NC110 before activeFrom; NC111 below minFill; NC112 hostile negative tip; NC113 hostile negative deliveryCarrier
    let (mut e, i) = tp_ed("NC110 fill before activeFrom", b.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.active_from = NOW as i64 + 1);
    restate_cont(&mut e, i, |s| s.active_from = NOW as i64 + 1);
    r.bad(&e, i);
    let (mut e, i) = tp_ed("NC111 partial fill below minFill", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.min_fill = NR + 1);
    restate_cont(&mut e, i, |s| s.min_fill = NR + 1);
    r.bad(&e, i);
    let (mut e, i) = tp_ed("NC112 hostile negative tip", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.tip = -10);
    restate_cont(&mut e, i, |s| s.tip = -10);
    let k = e.cont_out(OID).unwrap();
    e.fund_out(k, 41);
    r.bad(&e, i);
    let (mut e, i) = tp_ed("NC113 hostile negative deliveryCarrier", a.clone(), pa, pb, NR);
    let dc = cst(&e, i).delivery_carrier;
    restate(&mut e, i, |s| s.delivery_carrier = -1);
    restate_cont(&mut e, i, |s| s.delivery_carrier = -1);
    let k = e.cont_out(OID).unwrap();
    e.fund_out(k, dc + 1);
    r.bad(&e, i);
    // NC114 hostile negative scale of A (an ask's ceil of a negative quote: the maker paid one unit)
    let (mut e, i) = tp_ed("NC114 hostile negative scale of A", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.s_scale = -1_000);
    restate_cont(&mut e, i, |s| s.s_scale = -1_000);
    set_t_out(&mut e, i, 1);
    r.bad(&e, i);
    // NC115 (held by legPrice > 0) a leg-0 fill of an order without a TP leg (tpPrice 0)
    let (mut e, i) = tp_ed("NC115 take-profit fill of an order with no TP leg", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.tp_price = 0);
    restate_cont(&mut e, i, |s| s.tp_price = 0);
    r.bad(&e, i);
    // NC127 an armed sell stop with stopPrice 0 (hostile field) filled on its stop leg for one unit of B: the leg price
    // is 0 (held by `stopPrice > 0` and `legPrice > 0` together)
    let (mut e, i) = stop_inv(armed_ask(pa, pb, 1, 0), pa, pb, "NC127 armed stop leg with stopPrice 0 (the maker paid one unit)");
    restate(&mut e, i, |s| s.stop_price = 0);
    set_t_out(&mut e, i, 1);
    r.bad(&e, i);
    // NC128 a negative fill n = -1 under a hostile minFill (held by `n > 0` and the ask's `sOut == n`)
    let (mut e, i) = tp_ed("NC128 negative fill amount", a.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.min_fill = -5);
    restate_cont(&mut e, i, |s| s.min_fill = -5);
    e.set_arg(i, 0, nb(-1));
    r.bad(&e, i);
    // NC116 leg 2: an unarmed stop filled at the stop without evidence (the maker paid at the stop, not the TP)
    let (mut e, i) = tp_ed("NC116 leg 2: an unarmed stop leg filled without evidence", a.clone(), pa, pb, NR);
    e.set_arg(i, 6, Arg::Int(2));
    set_t_out(&mut e, i, a.t_out_min(NR, a.stop_price).unwrap());
    r.bad(&e, i);
    // NC117 hostile side 3 (a TP fill read as a bid)
    let (mut e, i) = tp_ed("NC117 hostile side 3", b.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.side = 3);
    restate_cont(&mut e, i, |s| s.side = 3);
    r.bad(&e, i);
    // ---- the stop leg of an armed ASK (band 300 bps)
    // NC118 hostile negative slipBps (whole band at once): the leg price above the stop
    let (mut e, i) = stop_inv(armed_ask(pa, pb, 1, 0), pa, pb, "NC118 hostile negative slipBps");
    restate(&mut e, i, |s| s.slip_bps = -300);
    let c = armed_ask(pa, pb, 1, 0);
    set_t_out(&mut e, i, c.t_out_min(4 * WHOLE, c.stop_price + c.stop_price * 300 / 10_000).unwrap());
    r.bad(&e, i);
    // NC119 slipBps above 10000 on a BID stop (it pays above stop + 100%)
    let cbid = CondPairState { armed: 1, band_daa: 0, ..cleft(cb(pa, pb), 4 * WHOLE) };
    let mut bt = route(vec![cond_pair_leg_amount(cbid.clone(), pa, pb, OID, 70, 4 * WHOLE, 1)]);
    bt.taker_tokens = vec![tutxo(pa, TOKEN_COV, 60, 10 * WHOLE, pk(MATCHER), false)];
    let mut e = edb("NC119 slipBps above 10000 (a buy stop pays above twice its stop)", bt);
    let i = e.order_in(OID);
    restate(&mut e, i, |s| s.slip_bps = 20_000);
    let q_old = e.arg_int(i, 4);
    let q_new = cbid.s_out(4 * WHOLE, cbid.stop_price * 3).unwrap();
    // the escrow cannot pay that much: pay one unit under what it holds (a return of one unit stays)
    let q_new = q_new.min(cbid.custody - 1);
    e.set_arg(i, 4, Arg::Int(q_new));
    let ci = cin(&e, i);
    let _ = ci;
    let m = e.tok_out_of(TOKEN_B, pk(MATCHER));
    e.add_out_amount(m, q_new - q_old);
    if let Some(ret) = e.tok_outs_of(TOKEN_B).into_iter().find(|k| e.out_state(*k).owner() == pk(MAKER_A)) {
        e.add_out_amount(ret, q_old - q_new);
    }
    r.bad(&e, i);
    // the auction of an armed stop (origin NOW - 100 of 300 DAA): NC120 t after tx.daa (the band grows), NC121 t before
    // the origin, NC122 the band taken whole mid-way
    let auc = || armed_ask(pa, pb, NOW as i64 - 100, 300);
    let c = auc();
    let at = |name: &str, t: i64, bps: i64| {
        let (mut e, i) = stop_inv(c.clone(), pa, pb, name);
        e.set_arg(i, 3, Arg::Int(t));
        let lp = c.stop_price - c.stop_price * bps / 10_000;
        set_t_out(&mut e, i, c.t_out_min(4 * WHOLE, lp).unwrap());
        (e, i)
    };
    let (e, i) = at("NC120 auction time t after tx.daa (a larger band)", NOW as i64 + 200, 300);
    r.bad(&e, i);
    let (e, i) = at("NC121 auction time t before the origin (paid at the price the covenant computes)", NOW as i64 - 200, -100);
    r.bad(&e, i);
    let (e, i) = at("NC122 auction mid-way paid at the whole band", NOW as i64, 300);
    r.bad(&e, i);
    // NC123 the fill that arms a stop with bandDaa > 0 trades at the stop itself: paid with the whole band instead
    let (mut e, i) = (edb("NC123 arming fill paid with the whole band", stop_ev0(true, pa, pb)), 0);
    let i2 = e.order_in(OID);
    let _ = i;
    let c0 = ca(pa, pb);
    let have = e.arg_int(i2, 5);
    let want = c0.t_out_min(4 * WHOLE, c0.stop_price - c0.stop_price * 300 / 10_000).unwrap();
    e.set_arg(i2, 5, Arg::Int(want));
    e.add_out_amount(i2, want - have);
    let extra = have - want;
    let m = e.add_token_output(TOKEN_B, user(pb, extra, MATCHER), 0);
    e.fund_out(m, CARRIER as i64);
    r.bad(&e, i2);
    // NC124 an ASK releases n + 1 (the extra unit to the matcher)
    let (mut e, i) = tp_ed("NC124 ask releases n + 1 of A", a.clone(), pa, pb, NR);
    e.set_arg(i, 4, Arg::Int(NR + 1));
    let ci = cin(&e, i);
    e.add_out_amount(ci, -1);
    e.add_out_amount(e.tok_out_of(TOKEN_COV, pk(MATCHER)), 1);
    restate_cont(&mut e, i, |s| s.custody -= 1);
    r.bad(&e, i);
    // NC125 a BID "pays" -1 (its escrow grows by a unit the matcher adds)
    let (mut e, i) = tp_ed("NC125 bid pays a negative amount", b.clone(), pa, pb, NR);
    let q = e.arg_int(i, 4);
    e.set_arg(i, 4, Arg::Int(-1));
    let ci = cin(&e, i);
    e.add_out_amount(ci, q + 1);
    restate_cont(&mut e, i, |s| s.custody += q + 1);
    let m = e.tok_out_of(TOKEN_B, pk(MATCHER));
    e.unbind(m, MATCHER);
    e.add_token_input(utxo(61, CARRIER, 1_000, Some(TOKEN_B)), user(pb, 1, MATCHER), kob_protocol::tx::Witness::P2pk(pk(MATCHER)));
    r.bad(&e, i);
    // NC126 an unfunded BID: its escrow one unit below its quote (sold out)
    let cl = cclosing(false, pa, pb);
    let (mut e, i) = tp_ed("NC126 unfunded bid: escrow one unit below its quote", cl.clone(), pa, pb, NR);
    restate(&mut e, i, |s| s.custody -= 1);
    let ci = cin(&e, i);
    let st = e.in_tok(ci);
    e.set_in_tok(ci, st.with_amount(st.amount() - 1));
    e.add_out_amount(e.tok_out_of(TOKEN_B, pk(MATCHER)), -1);
    r.bad(&e, i);
}

#[test]
fn cond_pair_fills() {
    suite(&mixed_pairs(), fills);
}

// ---------------------------------------------------------------- the T template source, rests, returns, carriers

fn carriers(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let (tp, ttok) = (t_prog(ask, pa, pb), t_tok(ask));
        let rest = |id: &str| tp_ed(&with_suffix(id, side), CondPairState { tip: 10_000, ..crest(ask, pa, pb) }, pa, pb, NR);
        // NC130 the T template source planted (a look-alike T output bound to T's id)
        let (mut e, i) = rest("NC130 T template source planted (look-alike T output)");
        let j = e.add_p2pk_input(30, 0xeb);
        e.set_arg(i, 2, Arg::Int(j as i64));
        let st = e.out_state(i);
        let ttpl = kob_protocol::artifacts::token_template(tp);
        let rs = fake_rs(ttpl.prefix.len(), &st.encode(), ttpl.suffix.len());
        let m = e.tok_out_of(ttok, pk(MATCHER));
        e.add_out_amount(m, st.amount());
        e.tok_out.remove(&i);
        let delivery = kob_protocol::script::p2sh_spk(&rs);
        r.bad_patched(&e, i, &move |tx, en| {
            plant_input(tx, en, j, ttok, &rs, true);
            tx.outputs[i].script_public_key = delivery.clone();
            tx.outputs[i].covenant = Some(kaspa_consensus_core::tx::CovenantBinding {
                authorizing_input: j as u16,
                covenant_id: kaspa_consensus_core::Hash::from_bytes(ttok),
            });
        });
        // NC131 (held by the hash) the T template read from an input of another program (the S custody)
        let (mut e, i) = rest("NC131 T template source at an input of another program");
        let ci = cin(&e, i);
        e.set_arg(i, 2, Arg::Int(ci as i64));
        r.bad(&e, i);
        // NC132 the continuation keeps amountLeft; NC133 drained by one sompi; NC134 the custody rest's carrier; NC135
        // the delivery carrier
        let (mut e, i) = rest("NC132 continuation keeps amountLeft");
        restate_cont(&mut e, i, |s| s.amount_left += NR);
        r.bad(&e, i);
        let (mut e, i) = rest("NC133 continuation drained by one sompi");
        let k = e.cont_out(OID).unwrap();
        e.fund_out(k, -1);
        r.bad(&e, i);
        let (mut e, i) = rest("NC134 custody rest drained of its carrier");
        let ci = cin(&e, i);
        e.fund_out(ci, -1);
        r.bad(&e, i);
        let (mut e, i) = rest("NC135 delivery carrier short");
        e.fund_out(i, -1);
        r.bad(&e, i);
        // sold out: NC137 every carrier back to the maker but the tip
        let (mut e, i) = tp_ed(
            &format!("NC137{side} sold out: the maker's carriers one sompi short"),
            CondPairState { tip: 10_000, ..cclosing(ask, pa, pb) },
            pa,
            pb,
            NR,
        );
        e.fund_out(i, -1);
        r.bad(&e, i);
    }
    // NC136 a BID done with escrow slack: the return keeps its carrier (NC136a) and the maker gets the order value but
    // the tip (NC136b)
    let done = || CondPairState { tip: 10_000, tp_price: RATE - 1, ..cleft(cb(pa, pb), NR) };
    let (mut e, i) = tp_ed("NC136a bid done: the escrow return drained of its carrier", done(), pa, pb, NR);
    let ci = cin(&e, i);
    e.fund_out(ci, -1);
    r.bad(&e, i);
    let (mut e, i) = tp_ed("NC136b bid done: the maker's KAS one sompi short", done(), pa, pb, NR);
    e.fund_out(i, -1);
    r.bad(&e, i);
    // NC138 a BID rests with an empty escrow (a partial fill that uses the escrow up)
    let b = crest(false, pa, pb);
    let q = b.s_out(NR, b.tp_price).unwrap();
    let (mut e, i) = tp_ed("NC138 bid rests with an empty escrow", b, pa, pb, NR);
    restate(&mut e, i, |s| s.custody = q);
    let ci = cin(&e, i);
    let st = e.in_tok(ci);
    e.set_in_tok(ci, st.with_amount(q));
    e.unbind(ci, MATCHER);
    restate_cont(&mut e, i, |s| s.custody = 0);
    r.bad(&e, i);
}

#[test]
fn cond_pair_carriers() {
    suite(&mixed_pairs(), carriers);
}

// ---------------------------------------------------------------- trigger evidence: mode argument, B quote, trail gap / step

fn evidence_misc(r: &Run, pa: TemplateId, pb: TemplateId) {
    // NC140 evMode 2 (neither mode): an arm with the pair evidence read in mode 1
    let (mut e, i) = (edb("NC140 evMode 2", stop_ev1(true, pa, pb)), 0);
    let i2 = e.order_in(OID);
    let _ = i;
    e.set_arg(i2, 10, Arg::Int(2));
    r.bad(&e, i2);
    // NC141 a buy stop armed by a B ask quoting 0 (the implied rate unbounded: a >= ceil(stop * 0) always holds)
    let mut e = edb("NC141 buy stop armed by a B evidence quoting 0", stop_ev0(false, pa, pb));
    let i = e.order_in(OID);
    let bi = e.input_of_cov(cov(0x94));
    let (t, st) = e.entry_state(bi);
    let mut any = AnyState::decode(t, &st).unwrap();
    match &mut any {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.price = 0,
        _ => {}
    }
    e.set_entry_state(bi, any.encode());
    r.bad(&e, i);
    // NC142 hostile side 3 in an arm update (read as a BID in the arming block)
    let c = CondPairState { side: 3, ..cb(pa, pb) };
    let mut b = route(vec![
        kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
        kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 5, 1),
        kask_leg(TOKEN_COV, pa, 24, P250, cov(0x95), 76, 10, 1),
        kbid_leg(TOKEN_B, pb, 25, P260, cov(0x96), 78, 10, 1),
    ]);
    b.updates.push(BatchUpdate {
        order: order(60, PV, OID, ODAA as u64, AnyState::KobCondPair(cb(pa, pb))),
        evidence: 0,
        evidence_b: Some(1),
        take: None,
    });
    let mut e = edb("NC142 hostile side 3 armed by an update", b);
    let i = e.order_in(OID);
    restate(&mut e, i, |s| s.side = c.side);
    restate_cont(&mut e, i, |s| s.side = 3);
    r.bad(&e, i);
    // NC143 hostile negative trailGap: a sell stop trails above the rate (k maximal for gap -20)
    let base = CondPairState { trail_step: 10, trail_gap: 20, trail_wait: 0, stop_price: RATE - 300, ..ca(pa, pb) };
    let (mut e, i) = trail_update("NC143 hostile negative trailGap", base.clone(), pa, pb);
    restate(&mut e, i, |s| s.trail_gap = -20);
    // pair BID evidence at RATE + 10: valid s' - 20 <= 1010, maximal s' - 10 > 1010  =>  s' = 1030, k = 33
    e.set_arg(i, 12, Arg::Int(33));
    restate_cont(&mut e, i, |s| {
        s.trail_gap = -20;
        s.stop_price = RATE + 30;
    });
    r.bad(&e, i);
    // NC144 (held by validity / maximality) a zero trailStep
    let (mut e, i) = trail_update("NC144 trailing with a zero step", base.clone(), pa, pb);
    restate(&mut e, i, |s| s.trail_step = 0);
    restate_cont(&mut e, i, |s| s.trail_step = 0);
    r.bad(&e, i);
    // NC145 (held by validity) a BID trail whose s' - gap is negative
    let bbase = CondPairState { trail_step: 10, trail_gap: 20, trail_wait: 0, stop_price: RATE + 300, ..cb(pa, pb) };
    let (mut e, i) = trail_update("NC145 buy-stop trail below zero (s' - gap < 0)", bbase, pa, pb);
    restate(&mut e, i, |s| s.trail_gap = 5_000);
    restate_cont(&mut e, i, |s| s.trail_gap = 5_000);
    r.bad(&e, i);
}

#[test]
fn cond_pair_evidence_misc() {
    suite(&mixed_pairs(), evidence_misc);
}
