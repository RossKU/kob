//! The custody of a pair order is pinned by its KCC-20 extension commitment (`KobPair.sil`, `KobCondPair.sil`,
//! `KobIfdPair.sil`), executed in the script engine against the templates compiled from source (`common/pair_harness.rs`).
//!
//! A KCC-20 token is (covenant id, program, extension commitment): units of the same covenant id with another commitment
//! are another token (only its issuer can create them, in its genesis). A pair order's custody is read by covenant id,
//! program, owner (the order's id), owner scheme / borrow-disabled and its exact amount, and since 2026-10-07 by the
//! commitment its state names: `sExt` of a `KobPair` / `KobCondPair` (the custody of S), `aExt` / `bExt` of a `KobIfdPair`
//! (its A custody, its B escrow or prefund, and the custody of an exit it merges). An order handed a custody of exactly
//! its amount but another commitment (owned by the order's id: anyone holding such units can send them there) would
//! otherwise refund / return those units to its maker and leave the real custody owned by an ended covenant.
//!
//! Every honest refund, fill and merge of the pair grids (`fx::pair::pair_branch_shapes`, `pair_scenarios`) on the family
//! mixes with a KCC-20 token runs as a positive; then, for every token group whose inputs are all custodies of pair orders,
//! the same transaction with every input and output of that group of another commitment (consistent for the token
//! program) is refused at the input of every pair order that holds one of those custodies.
//!
//! Run: cargo test -p kob-tests --test kob_pair_custody_ext_tests -- --nocapture

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use std::collections::BTreeSet;

use fx::pair::*;
use fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build_with, Action, RefundOrder};
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::SigPlan;
use ph::*;

/// Another extension commitment than the fixtures' (`EXT`).
const OTHER_EXT: [u8; 32] = [0xdd; 32];

/// The token groups of `e` whose inputs are all KCC-20 custodies owned by pair orders spent in `e` by a settling entry (not
/// their maker's cancel): (token, the inputs of the orders that hold them).
fn custody_groups(e: &Ed) -> Vec<([u8; 32], BTreeSet<usize>)> {
    let mut v = vec![];
    for tok in [TOKEN_COV, TOKEN_B] {
        let ins: Vec<usize> = (0..e.plans.len())
            .filter(|&i| cov_of(&e.entries[i]) == Some(tok) && !matches!(e.plans[i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }))
            .collect();
        if ins.is_empty() {
            continue;
        }
        let mut owners = BTreeSet::new();
        let mut all = true;
        for &i in &ins {
            let TokenState::Kcc20(k) = e.in_tok(i) else {
                all = false;
                break;
            };
            let order =
                (0..e.plans.len()).find(|&j| matches!(e.plans[j], SigPlan::Entry { .. }) && cov_of(&e.entries[j]) == Some(k.owner));
            match order {
                // the maker's cancel moves whatever it signs for: only the settling entries pin the custody
                Some(j)
                    if k.owner_scheme == 0x04
                        && is_pair(e.entry_state(j).0)
                        && !matches!(&e.plans[j], SigPlan::Entry { entry, .. } if entry == "cancel") =>
                {
                    owners.insert(j);
                }
                _ => {
                    all = false;
                    break;
                }
            }
        }
        if all {
            v.push((tok, owners));
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

/// Runs every scenario of `shapes`: the honest build passes; each custody group of another commitment is refused by
/// every pair order holding one of its custodies. Returns (scenarios, forged groups).
fn sweep(r: &Run, shapes: Vec<(String, Action)>) -> (usize, usize) {
    let (mut ran, mut forged) = (0, 0);
    for (name, a) in shapes {
        let honest = Ed::new(&name, &built(a));
        r.ok(&honest);
        ran += 1;
        for (tok, owners) in custody_groups(&honest) {
            let mut e = honest.clone().named(&format!("{name} custody of token {:02x} with another extension commitment", tok[0]));
            reclass(&mut e, tok, OTHER_EXT);
            let res = r.exec(&e);
            for &j in &owners {
                assert!(res[j].is_err(), "{} {}: the pair order at in[{j}] accepted a custody of another commitment", e.name, r.pair);
            }
            let failing: BTreeSet<usize> = res.iter().enumerate().filter(|(_, x)| x.is_err()).map(|(i, _)| i).collect();
            println!("NEGATIVE {} {}  [REJECTED at {owners:?}; failing {failing:?}]", e.name, r.pair);
            forged += 1;
        }
    }
    (ran, forged)
}

/// Every refund, fill, update and merge shape of the three pair kinds (and the route / netting scenarios), on every
/// family mix with a KCC-20 token.
#[test]
fn a_pair_order_takes_only_a_custody_of_its_extension_commitment() {
    let subs = Subs::compile();
    let (mut ran, mut forged) = (0, 0);
    for (pa, pb) in family_mixes() {
        if pa.family() == Family::Kron && pb.family() == Family::Kron {
            continue; // KRON has no extension commitment
        }
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        let mut shapes = pair_scenarios(pa, pb);
        shapes.extend(pair_branch_shapes(pa, pb));
        let (a, b) = sweep(&r, shapes);
        println!("{}: {a} scenarios, {b} custody groups of another commitment refused", r.pair);
        ran += a;
        forged += b;
    }
    assert!(ran > 300 && forged > 300, "{ran} scenarios, {forged} forged custody groups");
}

/// The refund of every kind with an explicit stand-in custody (another outpoint, owned by the order, exactly its amount):
/// of another commitment it is refused at the order's input, and only there; of the same commitment it passes (it pays the
/// maker the custody's full worth at the sender's expense).
#[test]
fn a_refund_takes_a_stand_in_custody_only_of_the_same_commitment() {
    let subs = Subs::compile();
    for (pa, pb) in family_mixes() {
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for (name, a) in pair_refund_grid(pa, pb) {
            let Action::RefundOrder(RefundOrder { order, .. }) = &a else { panic!("{name}: a refund") };
            let oid = order.utxo.covenant_id.expect("order covenant");
            let honest = Ed::new(&name, &built(a.clone()));
            r.ok(&honest);
            for (tok, owners) in custody_groups(&honest) {
                let i = (0..honest.plans.len())
                    .find(|&i| {
                        cov_of(&honest.entries[i]) == Some(tok)
                            && !matches!(honest.plans[i], SigPlan::Entry { .. } | SigPlan::P2pk { .. })
                    })
                    .expect("custody input");
                let oi = honest.order_in(oid);
                assert_eq!(owners, BTreeSet::from([oi]));
                let mut same = honest.clone().named(&format!("{name} stand-in custody of {:02x}, same commitment", tok[0]));
                same.tx.inputs[i].previous_outpoint.transaction_id = kaspa_consensus_core::Hash::from_bytes([0xd8; 32]);
                r.ok(&same);
                let mut other = same.clone().named(&format!("{name} stand-in custody of {:02x}, another commitment", tok[0]));
                reclass(&mut other, tok, OTHER_EXT);
                let res = r.exec(&other);
                let failing: BTreeSet<usize> = res.iter().enumerate().filter(|(_, x)| x.is_err()).map(|(i, _)| i).collect();
                assert_eq!(failing, BTreeSet::from([oi]), "{} {}: only the order input refuses it", other.name, r.pair);
                println!("NEGATIVE {} {}  [REJECTED at in[{oi}] only]", other.name, r.pair);
            }
        }
    }
}

/// A pair order of a KRON S carries no commitment (`sExt` zero); its custody has none to compare.
#[test]
fn a_kron_custody_has_no_commitment_to_pin() {
    let (pa, pb) = (TemplateId::KronToken2433, TemplateId::Kcc20Ref8x8);
    let s = pair(MAKER_A, true, pa, pb, RATE);
    assert_eq!(s.s_ext, [0; 32]);
    let mut bad = s.clone();
    bad.s_ext = EXT;
    assert!(AnyState::KobPair(bad).validate().is_err(), "a KRON S with a commitment is refused");
}

/// The builders follow the covenants: a refund or a fill handed a custody of another commitment than the order pins is
/// refused before anything is signed.
#[test]
fn the_builders_refuse_a_custody_of_another_commitment() {
    let (pa, pb) = family_mixes()[0];
    let mut refused = 0;
    let shapes: Vec<(String, Action)> = pair_refund_grid(pa, pb).into_iter().chain(pair_fill_grid(pa, pb)).collect();
    for (name, a) in shapes {
        let mut a = a;
        let mut touched = false;
        let mut other = |c: &mut kob_protocol::tx::TokenUtxo| {
            if let TokenState::Kcc20(k) = &mut c.state {
                k.extension_commitment = OTHER_EXT;
                touched = true;
            }
        };
        match &mut a {
            Action::RefundOrder(r) => {
                if let Some(c) = r.custody.as_mut() {
                    other(c);
                }
            }
            Action::Batch(b) => {
                for l in &mut b.legs {
                    match l {
                        kob_protocol::build::Leg::Pair { custody, .. } | kob_protocol::build::Leg::CondPair { custody, .. } => {
                            other(custody)
                        }
                        kob_protocol::build::Leg::IfdPair { a_custody, b_custody, .. } => {
                            if let Some(c) = a_custody.as_mut() {
                                other(c);
                            }
                            if let Some(c) = b_custody.as_mut() {
                                other(c);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => continue,
        }
        if !touched {
            continue;
        }
        let e = build_with(&a, &budgets).expect_err(&name);
        assert!(e.to_string().contains("extension commitment"), "{name}: {e}");
        refused += 1;
    }
    assert!(refused > 50, "{refused}");
}
