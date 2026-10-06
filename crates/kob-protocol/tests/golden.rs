//! Golden vectors: Rust-generated request -> unsigned transaction -> signatures -> signed
//! transaction for every builder, plus state and payload codec vectors. `kob-wasm`'s node test
//! reproduces every vector byte-for-byte from the wasm build. `KOB_REGEN=1` rewrites the file.

mod common;

use kob_protocol::build::{build, Action};
use kob_protocol::defaults::{day_order, tips_table};
use kob_protocol::payload::{decode, encode, Record};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions};
use kob_protocol::vectors::{DayOrderVector, Golden, PayloadVector, StateVector, TxVector};

/// A few shapes on the newer 2,732-byte KRON program (the whole set runs on the common 2,433-byte one).
fn kron_2732() -> Vec<(String, Action)> {
    let keep = ["create.ask", "create.bid", "cancel.ask", "take.ask.partial", "take.bid.partial", "refund.ask.expiry"];
    common::scenarios_on(kob_protocol::artifacts::TemplateId::KronToken2732)
        .into_iter()
        .filter(|(n, _)| keep.contains(&n.as_str()))
        .map(|(n, a)| (format!("kron2732.{n}"), a))
        .collect()
}

/// The pair order shapes (`KobPair`, `KobCondPair`, `KobIfdPair`; routes, netting, evidence modes, merges): the reference
/// KCC-20 program on both sides, and the cross-family pairs on the KRON programs.
fn pair_vectors() -> Vec<(String, Action)> {
    use kob_protocol::artifacts::TemplateId::{Kcc20Ref, KronToken2433, KronToken2732};
    let mut v = vec![];
    for (tag, pa, pb) in [
        ("", Kcc20Ref, Kcc20Ref),
        ("kcc20-kron.", Kcc20Ref, KronToken2433),
        ("kron-kcc20.", KronToken2433, Kcc20Ref),
        ("kron-kron.", KronToken2433, KronToken2732),
    ] {
        for (n, a) in common::pair::pair_scenarios(pa, pb) {
            let rest = n.strip_prefix("pair.").expect("pair scenario");
            v.push((format!("pair.{tag}{rest}"), a));
        }
    }
    v
}

fn vectors() -> Golden {
    let keys = common::keys();
    let mut transactions = vec![];
    let mut states = vec![];
    let mut payloads = vec![];
    for (name, action) in common::scenarios().into_iter().chain(common::scenarios_kron()).chain(kron_2732()).chain(pair_vectors()) {
        let built = build(&action).unwrap_or_else(|e| panic!("{name}: {e}"));
        let signatures = sign_locally(&built, &keys).unwrap();
        let opts = FinalizeOptions { tighten_budgets: true };
        let signed = finalize(&built, &signatures, opts).unwrap();
        kob_protocol::verify::validate_signed(&signed).unwrap_or_else(|e| panic!("{name}: {e}"));
        if !signed.tx.payload.is_empty() {
            let p = decode(&signed.tx.payload).unwrap().expect("KOB1");
            assert_eq!(encode(&p.records).unwrap(), signed.tx.payload);
            payloads.push(PayloadVector { name: name.clone(), payload: signed.tx.payload.clone(), decoded: p });
        }
        if let Action::CreateOrder(c) = &action {
            states.push(StateVector { name: name.clone(), state: c.order.clone(), encoded: c.order.encode() });
        }
        transactions.push(TxVector { name, request: action, built, signatures, finalize: opts, signed });
    }
    let x = vec![Record::X402 { reference: vec![0xab; 32] }, Record::Note { text: "x402-client/1".into() }];
    let p = encode(&x).unwrap();
    payloads.push(PayloadVector { name: "x402.only".into(), decoded: decode(&p).unwrap().unwrap(), payload: p });
    let p = b"X402:00112233".to_vec();
    payloads.push(PayloadVector { name: "x402.legacy".into(), decoded: decode(&p).unwrap().unwrap(), payload: p });
    let day_orders =
        [(1_000_000u64, 1_790_694_000u64, None), (52_000_000, 1_790_726_399, Some(9_000)), (7, 1_790_640_000, Some(10_020))]
            .into_iter()
            .map(|(d0, t0, rate_milli)| DayOrderVector { d0, t0, rate_milli, result: day_order(d0, t0, rate_milli) })
            .collect();
    Golden {
        format: 2,
        protocol: kob_protocol::VERSION.into(),
        templates: kob_protocol::artifacts::template_infos(),
        transactions,
        states,
        payloads,
        keeper_tips: tips_table().clone(),
        pair_keeper_tips: kob_protocol::defaults::pair_tips_table().clone(),
        day_orders,
    }
}

#[test]
#[cfg_attr(feature = "deploy-tn10", ignore = "the golden vectors are the reference build (placeholder R_ID)")]
fn golden_vectors_are_current() {
    let v = vectors();
    let mut text = serde_json::to_string_pretty(&v).unwrap();
    text.push('\n');
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors/golden.json");
    if std::env::var("KOB_REGEN").is_ok_and(|x| x == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        println!("wrote {} bytes to {}", text.len(), path.display());
        return;
    }
    let committed = std::fs::read_to_string(&path).expect("vectors/golden.json (run with KOB_REGEN=1)").replace("\r\n", "\n");
    assert!(committed == text, "golden vectors are stale: run `KOB_REGEN=1 cargo test -p kob-protocol --test golden`");
}
