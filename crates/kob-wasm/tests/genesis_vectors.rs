//! `verifyGenesis` over recorded mainnet and testnet-10 token geneses (`crates/kob-protocol/vectors/genesis-open.json`: the genesis group
//! of real tokens as the web app hands it to kob-wasm, with the verdict), and the same data tampered.

use kob_wasm::api::verify_genesis;
use serde_json::{json, Value};

fn vectors() -> Vec<Value> {
    let doc: Value = serde_json::from_str(include_str!("../../kob-protocol/vectors/genesis-open.json")).expect("vectors parse");
    doc["cases"].as_array().expect("cases").clone()
}

fn run(request: &Value) -> Value {
    serde_json::from_str(&verify_genesis(&request.to_string()).expect("a well-formed request")).expect("result is JSON")
}

#[test]
fn recorded_geneses_give_their_recorded_verdicts() {
    let cases = vectors();
    assert!(cases.len() >= 8);
    let nets: std::collections::BTreeSet<&str> = cases.iter().map(|c| c["network"].as_str().unwrap()).collect();
    assert_eq!(nets.into_iter().collect::<Vec<_>>(), vec!["mainnet", "testnet-10"]);
    for c in &cases {
        assert_eq!(run(&c["request"]), c["expect"], "{}", c["name"]);
    }
    let verdict = |n: &str| cases.iter().find(|c| c["name"] == n).unwrap()["expect"].clone();
    assert_eq!(verdict("brw")["status"], "verified");
    assert_eq!(verdict("brw")["supply"], "1000000");
    // an unspent TokenSeed is rebuilt from the group's own owner and class, and a public minter's allowance is reported
    assert_eq!(verdict("kqa-publicmint-seed")["mintAllowance"], "100000");
    assert_eq!(verdict("kqa-publicmint-seed")["minterOutputs"].as_array().unwrap().len(), 1);
    assert_eq!(verdict("kqa-publicmint-seed")["rebuilt"].as_array().unwrap().len(), 1);
    assert_eq!(verdict("wiggle-publicmint-seed")["rebuilt"].as_array().unwrap().len(), 1);
    // a genesis with a hidden output fails as contaminated; one with an unspent output of a program nobody revealed (a launchpad curve cell) stays unavailable
    assert_eq!(
        (verdict("kusdt-contaminated")["status"].clone(), verdict("kusdt-contaminated")["code"].clone()),
        (json!("failed"), json!("contaminated"))
    );
    assert_eq!(
        (verdict("dawdaw-unspent-curve")["status"].clone(), verdict("dawdaw-unspent-curve")["code"].clone()),
        (json!("unavailable"), json!("unrevealed"))
    );
}

#[test]
fn tampered_data_never_verifies() {
    for c in vectors().iter().filter(|c| c["expect"]["status"] == "verified") {
        let name = c["name"].as_str().unwrap();
        let base = &c["request"];
        let status = |r: &Value| run(r)["status"].as_str().unwrap().to_string();
        // a changed value, a wrong authorising outpoint, a wrong covenant id: the covenant id no longer recomputes
        let mut r = base.clone();
        let v = r["outputs"][0]["value"].as_str().unwrap().parse::<u64>().unwrap();
        r["outputs"][0]["value"] = json!((v + 1).to_string());
        assert_eq!(run(&r)["code"], "mismatch", "{name}: value");
        let mut r = base.clone();
        r["authorizingOutpoint"]["index"] = json!(r["authorizingOutpoint"]["index"].as_u64().unwrap() + 1);
        assert_eq!(run(&r)["code"], "mismatch", "{name}: outpoint");
        let mut r = base.clone();
        r["covenantId"] = json!("00".repeat(32));
        assert_eq!(run(&r)["code"], "mismatch", "{name}: covenant id");
        // a group with an output left out or an extra one
        let mut r = base.clone();
        let extra = r["outputs"][0].clone();
        r["outputs"].as_array_mut().unwrap().push(json!({ "index": 99, "value": extra["value"], "spk": extra["spk"] }));
        assert_eq!(status(&r), "failed", "{name}: extra output");
        // a revealed script with one byte flipped does not hash to its output
        let mut r = base.clone();
        let i = r["outputs"].as_array().unwrap().iter().position(|o| o["redeemScript"].is_string()).unwrap();
        let s = r["outputs"][i]["redeemScript"].as_str().unwrap().to_string();
        let mid = (s.len() / 2) & !1;
        let flipped = if &s[mid..mid + 2] == "00" { "01" } else { "00" };
        r["outputs"][i]["redeemScript"] = json!(format!("{}{}{}", &s[..mid], flipped, &s[mid + 2..]));
        assert_eq!(run(&r)["code"], "mismatch", "{name}: redeem script");
        // a script nobody revealed stays unverified
        let mut r = base.clone();
        r["outputs"][i].as_object_mut().unwrap().remove("redeemScript");
        assert_ne!(status(&r), "verified", "{name}: unrevealed");
        // malformed hex is unavailable
        let mut r = base.clone();
        r["outputs"][0]["spk"] = json!("zz");
        assert_eq!(run(&r)["status"], "unavailable", "{name}: malformed");
    }
}

#[test]
fn a_request_that_is_not_json_is_an_error_not_a_verdict() {
    assert!(verify_genesis("not json").is_err());
}
