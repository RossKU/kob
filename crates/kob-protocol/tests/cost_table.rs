//! Real-operation cost table (measurement, not a regression gate; `#[ignore]`d).
//!
//! Builds every builder scenario of the golden-vector set on every token program KOB builds for, the pair order
//! shapes on a set of program pairs, and plain batches padded with extra asks / bids (the marginal cost of a fill
//! in a batch), signs them with the fixture keys, tightens the compute budgets in the engine and writes one CSV row
//! per transaction: masses, the relay floor and every input's signature script with what it is (order template and
//! entry, token program and role, P2PK). Run:
//!
//! ```text
//! KOB_COST_OUT=target/cost_table.csv cargo test -p kob-protocol --test cost_table -- --ignored --nocapture
//! ```

mod common;

use std::fmt::Write as _;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, SigPlan};

fn input_kind(p: &SigPlan) -> String {
    match p {
        SigPlan::P2pk { .. } => "p2pk".into(),
        SigPlan::Entry { template, entry, .. } => format!("{}.{entry}", template.name()),
        SigPlan::TokenLeader { template, .. } => format!("{}.leader", template.name()),
        SigPlan::TokenDelegator { template, .. } => format!("{}.delegator", template.name()),
        SigPlan::KronToken { template, .. } => format!("{}.token", template.name()),
        SigPlan::Router { actor, entry, .. } => format!("router.{actor}.{entry}"),
        SigPlan::Retired { template_hash, entry, .. } => format!("retired.{}.{entry}", kob_protocol::json::to_hex(template_hash)),
    }
}

fn row(out: &mut String, program: &str, name: &str, action: &Action) {
    let keys = common::keys();
    let built = match build(action) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(out, "{program},{name},ERROR,{}", e.to_string().replace(',', ";"));
            return;
        }
    };
    let sigs = sign_locally(&built, &keys).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    kob_protocol::verify::validate_signed(&signed).unwrap_or_else(|e| panic!("{program} {name}: {e}"));
    let m = signed.fee.mass;
    let inputs: Vec<String> =
        built.plans.iter().zip(&signed.tx.inputs).map(|(p, i)| format!("{}:{}", input_kind(p), i.signature_script.len())).collect();
    let outputs: usize = signed.tx.outputs.len();
    let _ = writeln!(
        out,
        "{program},{name},{},{},{},{},{},{},{},{},{},{}",
        m.size,
        m.compute,
        m.transient_normalized,
        m.storage,
        m.fee_mass,
        signed.fee.min_fee,
        signed.tx.payload.len(),
        signed.tx.inputs.len(),
        outputs,
        inputs.join(" ")
    );
}

#[test]
#[ignore = "measurement: writes the cost table (KOB_COST_OUT)"]
fn cost_table() {
    let mut out = String::from(
        "program,scenario,bytes,compute,transient_norm,storage,fee_mass,relay_fee_sompi,payload,inputs,outputs,input_sigscripts\n",
    );
    for program in common::PROGRAMS {
        let tag = program.name();
        for (name, action) in common::scenarios_on(program) {
            row(&mut out, tag, &name, &action);
        }
        // marginal fills in a batch: the 1 bid x 2 asks match padded with plain asks / bids of one whole token
        let base = common::scenarios_on(program).into_iter().find(|(n, _)| n == "match.cross.1x2").map(|(_, a)| a);
        if let Some(base) = base {
            for (asks, bids) in [(0, 0), (1, 0), (2, 0), (0, 1), (0, 2), (1, 1), (2, 2)] {
                if let Some(a) = common::pad_batch(program, &base, asks, bids) {
                    row(&mut out, tag, &format!("pad.match.cross.1x2+a{asks}+b{bids}"), &a);
                }
            }
        }
    }
    use TemplateId::*;
    for (pa, pb) in [
        (Kcc20Ref, Kcc20Ref),
        (Kcc20Ref8x8, Kcc20Ref8x8),
        (Kcc20P2, Kcc20P2),
        (Kcc20KaspaCom025, Kcc20KaspaCom025),
        (Kcc20Ref, KronToken2433),
        (Kcc20Ref8x8, KronToken2433),
        (KronToken2433, Kcc20Ref8x8),
        (KronToken2433, Kcc20Ref),
        (KronToken2433, KronToken2732),
    ] {
        let tag = common::pair::pair_name(pa, pb);
        for (name, action) in common::pair::pair_scenarios(pa, pb) {
            row(&mut out, &tag, &name, &action);
        }
    }
    let path = std::env::var("KOB_COST_OUT").unwrap_or_else(|_| "../../target/cost_table.csv".into());
    std::fs::write(&path, &out).unwrap();
    println!("wrote {path} ({} rows)", out.lines().count() - 1);
}
