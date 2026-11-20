//! Measures the cost of real operations (a table, not a regression gate; `#[ignore]`d).
//!
//! Builds every builder scenario of the golden-vector set on every token program KOB builds for,
//! the pair order shapes on a set of program pairs, and plain batches padded with extra asks / bids
//! (the marginal cost of a fill in a batch), signs them with the fixture keys, tightens the compute
//! budgets in the engine and writes one CSV row per transaction: masses, the relay floor and every
//! input's signature script with what it is (order template and entry, token program and role,
//! P2PK). Run:
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
        SigPlan::RetiredCancel { template, .. } => format!("{}.cancel(retired)", template.name()),
        SigPlan::CreatorLock { .. } => "creator-lock".into(),
        SigPlan::CreatorLockAuto { .. } => "creator-lock-auto".into(),
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

/// The fill whose relay fee the top-up extra prices: a matcher crosses a GTC mode-2 bid of `p` (continuing) with one ask
/// selling out, the delivery a top-up of the maker's receive UTXO (`topup`) or a new receive UTXO.
fn topup_cross(p: TemplateId, topup: bool) -> Action {
    use common::*;
    use kob_protocol::build::{Batch, Leg};
    use kob_protocol::state::*;
    let bs = mode_bid(bid(MAKER_B, P260, p), MODE_TOPUP, 0);
    let bv = bs.escrow(10 * WHOLE, 3).unwrap() as u64;
    let mut r = recv_tok(24, 2 * WHOLE, pk(MAKER_B), RECV_T, 1_500);
    r.state = TokenState::user(p, 2 * WHOLE, pk(MAKER_B), ext_for(p)).with_borrow(kob_protocol::build::receive_borrow(RECV_T));
    let mut c = custody(11, 4, cov(0xa1), 1_000);
    c.state = TokenState::custody(p.family(), 4 * WHOLE, cov(0xa1), ext_for(p));
    let delivery = if topup { bid_dlv(None, Some(r), Some(2)) } else { bid_dlv(None, None, Some(1)) };
    let legs = vec![
        Leg::Bid { order: order(20, bv, cov(0xb1), 1_000, bs), amount: 4 * WHOLE, t: None, delivery },
        Leg::Ask { order: order(10, carrier(), cov(0xa1), 1_000, ask_n(MAKER_A, P250, p, 4)), custody: c, amount: 4 * WHOLE, t: None },
    ];
    let Action::Batch(mut b) = scenarios_on(p).into_iter().find(|(n, _)| n == "match.cross.1x2").expect("the cross").1 else {
        panic!("a batch")
    };
    b = Batch { legs, ..b };
    Action::Batch(b)
}

/// The relay fee at the floor rate of `a` (exact budgets), or why it does not validate.
fn floor_fee(a: &Action) -> Result<u64, String> {
    let provisional = |r: &str| Ok(kob_protocol::budget::lookup(r).unwrap_or(400));
    let built = kob_protocol::build::build_with(a, &provisional).map_err(|e| e.to_string())?;
    let sigs = sign_locally(&built, &common::keys()).map_err(|e| e.to_string())?;
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).map_err(|e| e.to_string())?;
    kob_protocol::verify::validate_signed(&signed).map_err(|e| e.to_string())?;
    Ok(kob_protocol::tx::min_fee(&signed.fee.mass, signed.fee.fee_rate))
}

/// `data/topup_extra.json` (bid-delivery-spec §9): per KCC-20 token program, the relay fee at the floor rate of a top-up fill
/// less that of a new-delivery fill (form 1) of the same order, measured through the builders and validated in the engine.
/// A program whose top-up the engine (or the program's slots) refuses gets no entry, and has no top-up. `KOB_REGEN=1`
/// rewrites the file.
#[test]
fn topup_extra_table_is_measured_and_current() {
    let mut generated = std::collections::BTreeMap::new();
    for p in common::PROGRAMS.into_iter().filter(|p| p.family() == kob_protocol::Family::Kcc20) {
        let new = floor_fee(&topup_cross(p, false)).unwrap_or_else(|e| panic!("{}: the new-delivery fill: {e}", p.name()));
        match floor_fee(&topup_cross(p, true)) {
            Ok(top) => {
                assert!(top > new, "{}: a top-up costs more than a new delivery ({top} vs {new})", p.name());
                generated.insert(p.name().to_string(), (top - new).to_string());
            }
            Err(e) => println!("{}: no top-up: {e}", p.name()),
        }
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/topup_extra.json");
    let mut json = serde_json::to_string_pretty(&generated).unwrap();
    json.push('\n');
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &json).unwrap();
        println!("wrote {} programs to {}: {json}", generated.len(), path.display());
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
    assert_eq!(committed, json, "topup_extra.json is stale: run `KOB_REGEN=1 cargo test -p kob-protocol --test cost_table topup`");
}
