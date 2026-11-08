//! A dump of every builder shape as JSON, for external replay tools (a red-team harness that attacks the exact bytes).
//!
//! Every builder scenario of the golden-vector set on every token program, and the pair scenarios on the program pairs of
//! `carrier_fees.rs`, at the default order carrier: built, signed with the fixture keys (`[n; 32]`, throwaway test keys that
//! hold nothing on any network), budgets tightened and validated in the engine. Each record carries the request, the
//! signed transaction in kaspa-wasm "safe JSON" (each input with the UTXO it spends: amount, script public key, DAA score,
//! covenant id) and the same transaction and spent entries as rusty-kaspa's own serde form. `keys.json` lists the keys.
//! The router intents are dumped by `intent_builders.rs` (`dump_router_intents`). `#[ignore]`d:
//!
//! ```text
//! KOB_TX_DUMP_DIR=target/kob-tx-dump cargo test -p kob-protocol --test tx_dump -- --ignored --nocapture
//! ```

mod common;

use std::path::PathBuf;

use common::{with_carriers, Carriers};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::defaults::DEFAULT_ORDER_CARRIER;
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions};
use serde_json::{json, Value};

/// The program pairs of the pair shapes (as `carrier_fees.rs`).
const PAIRS: [(TemplateId, TemplateId); 8] = [
    (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8),
    (TemplateId::Kcc20P2, TemplateId::Kcc20P2),
    (TemplateId::Kcc20KaspaCom025, TemplateId::Kcc20KaspaCom025),
    (TemplateId::Kcc20PublicMint, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref, TemplateId::KronToken2433),
    (TemplateId::KronToken2433, TemplateId::Kcc20Ref),
    (TemplateId::KronToken2433, TemplateId::KronToken2732),
];

fn shapes() -> Vec<(String, Action)> {
    let mut v = vec![];
    for p in common::PROGRAMS {
        v.extend(common::scenarios_on(p).into_iter().map(|(n, a)| (format!("{}/{n}", p.name()), a)));
    }
    for (pa, pb) in PAIRS {
        let tag = common::pair::pair_name(pa, pb);
        v.extend(common::pair::pair_scenarios(pa, pb).into_iter().map(|(n, a)| (format!("{tag}/{n}"), a)));
    }
    // the budget grids measure script units, not shapes (thousands of builds)
    v.retain(|(n, _)| !n.contains("/grid."));
    v
}

fn record(name: &str, action: &Action) -> Result<Value, String> {
    let built = build(action).map_err(|e| e.to_string())?;
    let sigs = sign_locally(&built, &common::keys()).map_err(|e| e.to_string())?;
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).map_err(|e| e.to_string())?;
    kob_protocol::verify::validate_signed(&signed).map_err(|e| e.to_string())?;
    let (tx, entries) = signed.tx.to_tx().map_err(|e| e.to_string())?;
    Ok(json!({
        "name": name,
        "request": action,
        "signed": signed,
        "consensus": { "transaction": tx, "entries": entries },
        "txid": hex(&tx.id().as_bytes()),
    }))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The fixture keys: `n`, the secret key `[n; 32]` and its x-only public key, and the role a scenario gives it.
pub fn keys_json() -> Value {
    let role = |n: u8| match n {
        1 => "maker A",
        2 => "maker B",
        3 => "maker C",
        4 => "taker",
        5 => "matcher",
        6 => "keeper",
        7 => "founder",
        8 => "merchant",
        11 => "x402 payer",
        12 => "keeper (intents)",
        _ => "",
    };
    Value::Array(
        (1..=40u8)
            .map(|n| json!({ "n": n, "secret_key": hex(&common::sk(n)), "xonly_public_key": hex(&common::pk(n)), "role": role(n) }))
            .collect(),
    )
}

pub fn out_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var("KOB_TX_DUMP_DIR").expect("set KOB_TX_DUMP_DIR to the output directory"));
    std::fs::create_dir_all(&dir).expect("create the output directory");
    dir
}

#[test]
#[ignore = "writes the dump: KOB_TX_DUMP_DIR=<dir> cargo test -p kob-protocol --test tx_dump -- --ignored"]
fn dump_every_builder_shape() {
    let dir = out_dir();
    let (mut ok, mut failed) = (vec![], vec![]);
    with_carriers(Carriers::order_side(DEFAULT_ORDER_CARRIER), || {
        for (name, action) in shapes() {
            match record(&name, &action) {
                Ok(r) => ok.push(r),
                // a shape a program refuses (no room for it, say) is listed with its reason, never silently left out
                Err(e) => failed.push(json!({ "name": name, "error": e })),
            }
        }
    });
    let n = ok.len();
    let doc = json!({
        "format": "kob-tx-dump/1",
        "network": "none (synthetic fixture UTXOs; the keys are throwaway test keys)",
        "order_carrier_sompi": DEFAULT_ORDER_CARRIER,
        "transactions": ok,
        "not_built": failed,
    });
    std::fs::write(dir.join("shapes.json"), serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    std::fs::write(dir.join("keys.json"), serde_json::to_string_pretty(&keys_json()).unwrap()).unwrap();
    println!("{n} transactions, {} not built, in {}", doc["not_built"].as_array().map_or(0, Vec::len), dir.display());
    assert!(n > 1000, "every shape is in the dump ({n})");
}
