//! Compute budgets of the `KobCurveAsk` roles (`KobCurveAsk.settle.rest@P`, `….settle.close@P`, `KobCurveAsk.cancel@P`),
//! on every KCC-20 program, measured at the worst shape: 1,000 levels (the hash of the redeem script grows with the
//! table), 8 levels crossed, each batch also padded with plain asks to the token slot limit (the stray scan). A smaller
//! curve is over-provisioned, which costs no fee (a fill's fee is set by its size). `KOB_REGEN=1` merges these roles into
//! `data/compute_budgets.json`; `budget_table.rs` leaves them to this file.

mod common;

use std::collections::BTreeMap;

use common::curve::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::budget::{budget_for_units, lookup, table};
use kob_protocol::build::{build_with, Action};
use kob_protocol::state::CurveAskState;
use kob_protocol::tx::{assemble, sign_locally};
use kob_protocol::verify::{execute, measure_units};
use kob_protocol::Family;

const PREFIX: &str = "KobCurveAsk.";

fn provisional(role: &str) -> kob_protocol::Result<u16> {
    Ok(lookup(role).unwrap_or(1))
}

fn kcc20_programs() -> Vec<TemplateId> {
    TemplateId::TOKEN_PROGRAMS.into_iter().filter(|p| p.family() == Family::Kcc20).collect()
}

/// The curve shapes of `p` at the worst size, each batch also with the most pad asks that build.
fn shapes(p: TemplateId) -> Vec<(String, Action)> {
    let big = curve(p, kob_protocol::state::CURVE_MAX_LEVELS);
    let mid = CurveAskState { amount_left: big.total - WHOLE / 2, ..big.clone() };
    let end8 = CurveAskState { amount_left: 7 * WHOLE + WHOLE / 2, ..big.clone() };
    let base = vec![
        ("rest8".to_string(), Action::Batch(take_curve(&mid, 7 * WHOLE + WHOLE / 4))),
        ("close8".into(), Action::Batch(take_curve(&end8, end8.amount_left))),
        ("cross8".into(), Action::Batch(cross_curve(&mid, 7 * WHOLE + WHOLE / 4, cross_bid(p, 0, 0), None))),
        ("cancel".into(), Action::CancelOrder(cancel_curve(&mid))),
        ("amend".into(), Action::AmendOrder(amend_curve(&mid))),
    ];
    let mut out = vec![];
    for (n, a) in base {
        if matches!(a, Action::Batch(_)) {
            let mut k = 0;
            while k < 24 && pad_batch(p, &a, k + 1, 0).is_some_and(|x| build_with(&x, &provisional).is_ok()) {
                k += 1;
            }
            if k > 0 {
                out.push((format!("{n}+{k}asks@{}", p.name()), pad_batch(p, &a, k, 0).expect("a batch")));
            }
        }
        out.push((format!("{n}@{}", p.name()), a));
    }
    out
}

fn units(name: &str, a: &Action) -> Vec<(String, u64)> {
    let built = build_with(a, &provisional).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, &keys()).unwrap();
    let (tx, entries) = assemble(&built, &sigs).unwrap();
    let u = measure_units(&tx, &entries).unwrap_or_else(|e| panic!("{name}: engine: {e}"));
    built.roles.into_iter().zip(u).collect()
}

#[test]
fn curve_budgets_are_generated_exact_and_current() {
    let all: Vec<(String, Action)> = kcc20_programs().into_iter().flat_map(shapes).collect();
    // role -> (budget, worst shape, units)
    let mut worst: BTreeMap<String, (u16, String, u64)> = BTreeMap::new();
    for (name, a) in &all {
        for (role, u) in units(name, a) {
            if !role.starts_with(PREFIX) {
                continue;
            }
            let e = worst.entry(role).or_insert((0, String::new(), 0));
            if u > e.2 || e.1.is_empty() {
                *e = (budget_for_units(u), name.clone(), u);
            }
        }
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/compute_budgets.json");
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        let mut t: BTreeMap<String, u16> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        t.retain(|r, _| !r.starts_with(PREFIX));
        t.extend(worst.iter().map(|(r, (b, _, _))| (r.clone(), *b)));
        let mut json = serde_json::to_string_pretty(&t).unwrap();
        json.push('\n');
        std::fs::write(&path, json).unwrap();
        println!("wrote {} curve roles", worst.len());
        return;
    }
    let committed: BTreeMap<&String, &u16> = table().iter().filter(|(r, _)| r.starts_with(PREFIX)).collect();
    let generated: BTreeMap<&String, &u16> = worst.iter().map(|(r, (b, _, _))| (r, b)).collect();
    assert_eq!(committed, generated, "curve budgets are stale: run `KOB_REGEN=1 cargo test -p kob-protocol --test curve_budgets`");
    // exact: at each role's worst shape, one unit less is refused
    let by_name: BTreeMap<&str, &Action> = all.iter().map(|(n, a)| (n.as_str(), a)).collect();
    for (role, (budget, shape, used)) in &worst {
        println!("{role}: budget {budget} ({used} units at {shape})");
        for (b, pass) in [(*budget, true), (*budget - 1, false)] {
            let with = |r: &str| if r == role { Ok(b) } else { lookup(r) };
            let built = build_with(by_name[shape.as_str()], &with).unwrap();
            let sigs = sign_locally(&built, &keys()).unwrap();
            let (tx, entries) = assemble(&built, &sigs).unwrap();
            let res = execute(&tx, &entries, true).unwrap();
            let at: Vec<usize> = built.roles.iter().enumerate().filter(|(_, r)| *r == role).map(|(i, _)| i).collect();
            if pass {
                assert!(res.iter().all(|r| r.is_ok()), "{role} at {b} ({shape}): {res:?}");
            } else {
                assert!(at.iter().any(|i| matches!(&res[*i], Err(e) if e.contains("ExceededCommittedScriptUnits"))), "{role}: {b} is enough");
            }
        }
    }
}
