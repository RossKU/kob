//! The custody of a KCC-20 ask-side order is pinned by its extension commitment (`KobAsk.sil`, `KobCondAsk.sil`:
//! `extensionCommitment`, the last state field; `KobIfdAsk.sil`: the commitment of its committed exit), executed in the
//! script engine against the templates compiled from source (`common/pair_harness.rs`, `Subs::compile_asks`: KobAsk,
//! KobCondAsk, KobCondBid, KobIfdAsk and KobIfdBid, each against the compiled templates it embeds).
//!
//! A KCC-20 token is (covenant id, program, extension commitment): units of the same covenant id with another commitment
//! are another token (only its issuer can create them, in its genesis). An ask's custody is read by covenant id, program,
//! owner (the order's id), owner scheme / borrow-disabled and its exact amount, and since 2026-10-07 by the commitment its
//! state names. An order handed a custody of exactly its amount but another commitment (owned by the order's id: anyone
//! holding such units can send them there) would otherwise refund or return those units to its maker (refund, IOC end,
//! a sell-out fill) and leave the real custody owned by an ended covenant. A buy-first entry (`KobIfdBid`) writes its own
//! `extensionCommitment` into every exit it creates and merges only an exit that carries it.
//!
//! Every honest scenario of the KAS-book fixtures (creation, fills, refunds, IOC / FOK ends, conditional legs, updates
//! next to their evidence, if-done fills and repeat merges) and every branch shape runs as a positive; then, for every
//! token group that holds the custody of an ask-side order settling in the transaction (not its maker's cancel), the same
//! transaction with every input and output of that group of another commitment (consistent for the token program) is
//! refused at the input of every such order.
//!
//! Run: cargo test -p kob-tests --test kob_ask_custody_ext_tests -- --nocapture

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use std::collections::{BTreeMap, BTreeSet};

use fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build_with, Action, Leg, RefundOrder};
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::SigPlan;
use ph::*;

/// Another extension commitment than the fixtures' (`EXT`).
const OTHER_EXT: [u8; 32] = [0xdd; 32];

/// The order kinds that hold a custody of their token.
const CUSTODY_KINDS: [TemplateId; 3] = [TemplateId::KobAsk, TemplateId::KobCondAsk, TemplateId::KobIfdAsk];

/// The KCC-20 programs the grid runs on: the 3/3 reference program (its scenarios take the 8/8 program where they need
/// more slots) and the 8/8 program.
const PROGRAMS: [TemplateId; 2] = [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8];

/// Every scenario of the KAS-book fixtures and every branch shape on the KCC-20 programs.
fn grid() -> Vec<(String, Action)> {
    let mut v = vec![];
    for p in PROGRAMS {
        let tag = p.name();
        v.extend(scenarios_on(p).into_iter().map(|(n, a)| (format!("{tag}.{n}"), a)));
        v.extend(branches::branch_shapes(p).into_iter().map(|(n, a)| (format!("{tag}.{n}"), a)));
    }
    v
}

fn is_cancel(p: &SigPlan) -> bool {
    matches!(p, SigPlan::Entry { entry, .. } if entry == "cancel")
}

/// The KCC-20 token groups of `e` that hold the custody of an ask-side order settling in `e` (not its maker's cancel):
/// (token, the inputs of the orders whose custody the group holds).
fn custody_groups(e: &Ed) -> Vec<([u8; 32], BTreeSet<usize>)> {
    let mut v = vec![];
    for (tok, prog) in &e.programs {
        if prog.family() != Family::Kcc20 {
            continue;
        }
        let mut owners = BTreeSet::new();
        for i in 0..e.plans.len() {
            if cov_of(&e.entries[i]) != Some(*tok) || matches!(e.plans[i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }) {
                continue;
            }
            let TokenState::Kcc20(k) = e.in_tok(i) else { continue };
            if k.owner_scheme != 0x04 {
                continue;
            }
            let order =
                (0..e.plans.len()).find(|&j| matches!(e.plans[j], SigPlan::Entry { .. }) && cov_of(&e.entries[j]) == Some(k.owner));
            if let Some(j) = order {
                if CUSTODY_KINDS.contains(&e.entry_state(j).0) && !is_cancel(&e.plans[j]) {
                    owners.insert(j);
                }
            }
        }
        if !owners.is_empty() {
            v.push((*tok, owners));
        }
    }
    v
}

/// Gives every input and output of token `tok` the extension commitment `ext` (the token program carries one commitment
/// per transfer, so it accepts the result).
fn reclass(e: &mut Ed, tok: [u8; 32], ext: [u8; 32]) {
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
}

/// What a settling entry of an ask-side order does in `e` (for the coverage count).
fn what(e: &Ed, j: usize) -> String {
    let (t, _) = e.entry_state(j);
    let SigPlan::Entry { entry, .. } = &e.plans[j] else { unreachable!() };
    let n = fill_n(&e.plans[j]);
    let kind = match (entry.as_str(), n) {
        ("settle", Some(0)) => "refund",
        ("settle", Some(n)) if n < 0 => "merge",
        ("settle", Some(_)) => "fill",
        (other, _) => other,
    };
    format!("{}.{kind}", t.name())
}

/// Runs every scenario of the grid: the honest build passes; each custody group of another commitment is refused by every
/// ask-side order holding a custody of that group. Returns (scenarios, groups of another commitment, coverage).
fn sweep(r: &Run, shapes: Vec<(String, Action)>) -> (usize, usize, BTreeMap<String, usize>) {
    let (mut ran, mut forged, mut cover) = (0, 0, BTreeMap::new());
    for (name, a) in shapes {
        let honest = Ed::new(&name, &built(a));
        r.ok(&honest);
        ran += 1;
        for (tok, owners) in custody_groups(&honest) {
            let mut e = honest.clone().named(&format!("{name} custody of token {:02x} with another extension commitment", tok[0]));
            reclass(&mut e, tok, OTHER_EXT);
            let res = r.exec(&e);
            for &j in &owners {
                assert!(res[j].is_err(), "{} {}: the order at in[{j}] accepted a custody of another commitment", e.name, r.pair);
                *cover.entry(what(&honest, j)).or_insert(0) += 1;
            }
            let failing: BTreeSet<usize> = res.iter().enumerate().filter(|(_, x)| x.is_err()).map(|(i, _)| i).collect();
            println!("NEGATIVE {} {}  [REJECTED at {owners:?}; failing {failing:?}]", e.name, r.pair);
            forged += 1;
        }
    }
    (ran, forged, cover)
}

/// Every creation, fill, refund, IOC / FOK end, conditional leg, update (the evidence ask's custody), if-done fill and
/// repeat merge of the fixtures, on both KCC-20 programs.
#[test]
fn an_ask_takes_only_a_custody_of_its_extension_commitment() {
    let subs = Subs::compile_asks();
    let r = Run { subs: &subs, pair: "KCC-20".into() };
    let (ran, forged, cover) = sweep(&r, grid());
    println!("{ran} scenarios, {forged} custody groups of another commitment refused");
    for (k, n) in &cover {
        println!("  {k}: {n}");
    }
    assert!(ran > 200 && forged > 150, "{ran} scenarios, {forged} forged custody groups");
    for k in [
        "KobAsk.fill",
        "KobAsk.refund",
        "KobCondAsk.fill",
        "KobCondAsk.refund",
        "KobIfdAsk.fill",
        "KobIfdAsk.refund",
        "KobIfdAsk.merge",
    ] {
        assert!(cover.get(k).is_some_and(|n| *n > 0), "no custody group of another commitment ran against {k}: {cover:?}");
    }
}

/// The refund of every ask-side kind with an explicit stand-in custody (another outpoint, owned by the order, exactly its
/// amount): of another commitment it is refused at the order's input, and only there; of the same commitment it passes
/// (it pays the maker the custody's full worth at the sender's expense).
#[test]
fn a_refund_takes_a_stand_in_custody_only_of_the_same_commitment() {
    let subs = Subs::compile_asks();
    let r = Run { subs: &subs, pair: "KCC-20".into() };
    let mut kinds = BTreeSet::new();
    for (name, a) in grid() {
        let Action::RefundOrder(RefundOrder { order, custody: Some(_), .. }) = &a else { continue };
        let oid = order.utxo.covenant_id.expect("order covenant");
        let honest = Ed::new(&name, &built(a.clone()));
        r.ok(&honest);
        for (tok, owners) in custody_groups(&honest) {
            let i = honest.first_token_input(tok);
            let oi = honest.order_in(oid);
            assert_eq!(owners, BTreeSet::from([oi]));
            kinds.insert(honest.entry_state(oi).0);
            let mut same = honest.clone().named(&format!("{name} stand-in custody, same commitment"));
            same.tx.inputs[i].previous_outpoint.transaction_id = kaspa_consensus_core::Hash::from_bytes([0xd8; 32]);
            r.ok(&same);
            let mut other = same.clone().named(&format!("{name} stand-in custody, another commitment"));
            reclass(&mut other, tok, OTHER_EXT);
            let res = r.exec(&other);
            let failing: BTreeSet<usize> = res.iter().enumerate().filter(|(_, x)| x.is_err()).map(|(i, _)| i).collect();
            assert_eq!(failing, BTreeSet::from([oi]), "{}: only the order input refuses it", other.name);
            println!("NEGATIVE {} KCC-20  [REJECTED at in[{oi}] only]", other.name);
        }
    }
    assert_eq!(kinds, BTreeSet::from(CUSTODY_KINDS), "a refund of every ask-side kind ran");
}

/// A buy-first entry writes its own `extensionCommitment` into the exit it creates (the commitment of the custody it
/// delivers): an exit output of another commitment is refused at the entry. A merge takes only an exit of that commitment:
/// an exit (with its custody and continuation) of another commitment, which accepts its own fill, is refused at the entry.
#[test]
fn a_buy_first_entry_books_and_merges_exits_of_its_commitment() {
    let subs = Subs::compile_asks();
    let r = Run { subs: &subs, pair: "KCC-20".into() };
    let (mut fills, mut merges) = (0, 0);
    let set_ext = |st: &[u8], ext: [u8; 32]| CondAskState { extension_commitment: ext, ..CondAskState::decode(st).unwrap() }.encode();
    for (name, a) in grid() {
        let honest = Ed::new(&name, &built(a));
        for i in honest.inputs_of(TemplateId::KobIfdBid) {
            match fill_n(&honest.plans[i]) {
                // a fill: its exit output is the KobCondAsk genesis this input authorises
                Some(n) if n > 0 => {
                    let k = (0..honest.tx.outputs.len())
                        .find(|&k| {
                            honest.out_order(k).is_some_and(|(t, _)| t == TemplateId::KobCondAsk)
                                && honest.tx.outputs[k].covenant.is_some_and(|b| b.authorizing_input as usize == i)
                        })
                        .unwrap_or_else(|| panic!("{name}: the exit of in[{i}]"));
                    let (_, st) = honest.out_order(k).unwrap();
                    let mut e = honest.clone().named(&format!("{name} exit of another extension commitment"));
                    e.set_out_spk_state(k, TemplateId::KobCondAsk, &set_ext(&st, OTHER_EXT));
                    e.rebind_genesis(i as u16);
                    r.bad(&e, i);
                    fills += 1;
                }
                // a merge: the exit is the KobCondAsk input whose parent is this entry
                Some(_) => {
                    let me = cov_of(&honest.entries[i]).unwrap();
                    let x = honest
                        .inputs_of(TemplateId::KobCondAsk)
                        .into_iter()
                        .find(|&x| CondAskState::decode(&honest.entry_state(x).1).unwrap().parent == me)
                        .unwrap_or_else(|| panic!("{name}: the booked exit merged by in[{i}]"));
                    let xs = honest.entry_state(x).1;
                    let xid = cov_of(&honest.entries[x]).unwrap();
                    let mut e = honest.clone().named(&format!("{name} merged exit of another extension commitment"));
                    e.set_entry_state(x, set_ext(&xs, OTHER_EXT));
                    // its continuation (a partial take-profit) and its custody group follow
                    for k in 0..e.tx.outputs.len() {
                        if let Some((TemplateId::KobCondAsk, st)) = e.out_order(k) {
                            if e.tx.outputs[k].covenant.is_some_and(|b| b.covenant_id.as_bytes() == xid) {
                                e.set_out_spk_state(k, TemplateId::KobCondAsk, &set_ext(&st, OTHER_EXT));
                            }
                        }
                    }
                    let tok = CondAskState::decode(&xs).unwrap().token_cov_id;
                    reclass(&mut e, tok, OTHER_EXT);
                    let res = r.exec(&e);
                    assert!(res[x].is_ok(), "{}: the exit accepts its own custody: {:?}", e.name, res[x]);
                    assert!(res[i].is_err(), "{}: the entry at in[{i}] merged an exit of another commitment", e.name);
                    println!("NEGATIVE {} KCC-20  [REJECTED at in[{i}]]", e.name);
                    merges += 1;
                }
                None => {}
            }
        }
    }
    assert!(fills > 10 && merges > 3, "{fills} exits, {merges} merges");
}

/// The builders follow the covenants: a placement, fill, refund or merge handed a custody of another commitment than the
/// order pins is refused before anything is signed.
#[test]
fn the_builders_refuse_a_custody_of_another_commitment() {
    let mut refused = BTreeMap::new();
    let other = |c: &mut kob_protocol::tx::TokenUtxo| -> bool {
        match &mut c.state {
            TokenState::Kcc20(k) => {
                k.extension_commitment = OTHER_EXT;
                true
            }
            _ => false,
        }
    };
    for (name, a) in grid() {
        let mut a = a;
        let mut touched = None;
        match &mut a {
            Action::CreateOrder(c) if CUSTODY_KINDS.contains(&c.order.template_id()) => {
                for t in &mut c.tokens {
                    if other(t) {
                        touched = Some("create");
                    }
                }
            }
            Action::RefundOrder(r) if CUSTODY_KINDS.contains(&r.order.state.template_id()) => {
                if let Some(c) = r.custody.as_mut() {
                    if other(c) {
                        touched = Some("refund");
                    }
                }
            }
            Action::Batch(b) => {
                for l in &mut b.legs {
                    match l {
                        Leg::Ask { custody, .. } | Leg::CondAsk { custody, .. } | Leg::IfdAsk { custody, .. } => {
                            if other(custody) {
                                touched = Some("fill");
                            }
                        }
                        Leg::CondBid { merge: Some(m), .. } => {
                            if let Some(c) = m.custody.as_mut() {
                                if other(c) {
                                    touched = Some("merge");
                                }
                            }
                        }
                        _ => {}
                    }
                    if touched.is_some() {
                        break;
                    }
                }
            }
            _ => continue,
        }
        let Some(kind) = touched else { continue };
        let e = build_with(&a, &budgets).expect_err(&name);
        assert!(e.to_string().contains("extension commitment"), "{name}: {e}");
        *refused.entry(kind).or_insert(0) += 1;
    }
    println!("{refused:?}");
    for k in ["create", "refund", "fill", "merge"] {
        assert!(refused.get(k).is_some_and(|n| *n > 0), "no {k} with a custody of another commitment: {refused:?}");
    }
}

/// A KRON ask carries no commitment (zero), and a KRON ask state with one is refused; a KCC-20 ask's commitment is the last
/// state field, so the KRON twin keeps the KCC-20 layout without it.
#[test]
fn a_kron_ask_has_no_commitment_to_pin() {
    let k = TemplateId::KronToken2433;
    let a = ask(MAKER_A, P250, k);
    assert_eq!(a.extension_commitment, [0; 32]);
    let s = AnyState::KobAsk(a.clone()).into_family(Family::Kron);
    s.validate().unwrap();
    assert_eq!(s.encode().len() + 33, AnyState::KobAsk(ask(MAKER_A, P250, TemplateId::Kcc20Ref)).encode().len());
    let bad = AnyState::KobAskKron(AskState { extension_commitment: EXT, ..a });
    assert!(bad.validate().is_err(), "a KRON ask with a commitment is refused");
    assert_eq!(AnyState::KobAsk(ask(MAKER_A, P250, TemplateId::Kcc20Ref)).custody_ext(TOKEN_COV), Some(EXT));
    let ia = ifd_ask(MAKER_A, TemplateId::Kcc20Ref);
    assert_eq!(AnyState::KobIfdAsk(ia.clone()).custody_ext(TOKEN_COV), Some(ia.exit().unwrap().extension_commitment));
}
