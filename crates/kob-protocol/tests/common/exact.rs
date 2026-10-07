//! Engine helpers for the tests that run new shapes before (or independently of) the committed compute-budget table:
//! building with budgets measured in the engine, and patching a built transaction (an order input's state and the
//! outputs derived from it) to show that a covenant, not only the builder, refuses a fill.
#![allow(dead_code)]

use std::collections::BTreeMap;

use kob_protocol::budget::{budget_for_units, lookup};
use kob_protocol::build::{build, build_with, Action};
use kob_protocol::tx::{assemble, finalize, sighash, sign_locally, spk_to_string, BuiltTx, FinalizeOptions, SigPlan, SignedTx};
use kob_protocol::verify::{execute, measure_units, validate_signed, InputOutcome};

use super::keys;

/// Builds `a` with each role's budget measured in the engine (the largest need over the inputs of that role): the shape
/// runs with exactly what it needs, whatever the committed table holds.
pub fn build_measured(a: &Action) -> kob_protocol::Result<BuiltTx> {
    let provisional = |r: &str| Ok(lookup(r).unwrap_or(1));
    let built = build_with(a, &provisional)?;
    let sigs = sign_locally(&built, &keys())?;
    let (tx, entries) = assemble(&built, &sigs)?;
    let units = measure_units(&tx, &entries)?;
    let mut need: BTreeMap<String, u16> = BTreeMap::new();
    for (role, u) in built.roles.iter().zip(units) {
        let b = need.entry(role.clone()).or_insert(0);
        *b = (*b).max(budget_for_units(u));
    }
    build_with(a, &|r: &str| Ok(need.get(r).copied().unwrap_or(0)))
}

/// Builds `a` with the committed table.
pub fn build_any(a: &Action) -> kob_protocol::Result<BuiltTx> {
    build(a)
}

/// Builds, signs, finalizes and validates `a` the way consensus does; panics with `label` on any failure.
pub fn run_any(label: &str, a: &Action) -> (BuiltTx, SignedTx) {
    let built = build_any(a).unwrap_or_else(|e| panic!("{label}: build: {e}"));
    let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default())
        .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
    let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
    assert!(v.fee >= v.min_fee, "{label}: fee {} below the floor {}", v.fee, v.min_fee);
    assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
    (built, signed)
}

/// Replaces the state of the order input `input` (its signing plan and its UTXO's script) with `state` (the encoded state
/// span of the same template), every output whose script is one of `outputs.0` with the matching `outputs.1` and the
/// value of every output listed in `values`, then re-signs the key inputs and runs every input in the engine (budgets not
/// enforced; the scripts only: a patched value need not be funded).
pub fn patched_outcomes(
    mut built: BuiltTx,
    input: usize,
    state: Vec<u8>,
    spk: String,
    outputs: &[(String, String)],
    values: &[(usize, u64)],
) -> Vec<InputOutcome> {
    match &mut built.plans[input] {
        SigPlan::Entry { state: s, .. } => *s = state,
        other => panic!("input {input} is not an order entry: {other:?}"),
    }
    built.tx.inputs[input].utxo.script_public_key = spk;
    for o in &mut built.tx.outputs {
        if let Some((_, to)) = outputs.iter().find(|(from, _)| *from == o.script_public_key) {
            o.script_public_key = to.clone();
        }
    }
    for (at, v) in values {
        built.tx.outputs[*at].value = *v;
    }
    let (tx, entries) = built.tx.to_tx().unwrap();
    for r in &mut built.sign {
        r.sighash = sighash(&tx, &entries, r.input_index);
    }
    let sigs = sign_locally(&built, &keys()).unwrap();
    let (tx, entries) = assemble(&built, &sigs).unwrap();
    execute(&tx, &entries, false).unwrap()
}

/// The script public key string of a P2SH script.
pub fn spk_str(spk: &kaspa_consensus_core::tx::ScriptPublicKey) -> String {
    spk_to_string(spk)
}

/// [`run_any`] with budgets measured in the engine (shapes whose roles the committed table may not price yet).
pub fn run_measured(label: &str, a: &Action) -> (BuiltTx, SignedTx) {
    let built = build_measured(a).unwrap_or_else(|e| panic!("{label}: build: {e}"));
    let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default())
        .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
    let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
    assert!(v.fee >= v.min_fee, "{label}: fee {} below the floor {}", v.fee, v.min_fee);
    assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
    (built, signed)
}
