//! Token issuance through the wasm API surface (`api::issue` / `api::issue_limits`): the genesis comes back as a
//! normal `BuiltTx`, signs with the shared signing split and passes `finalize` and the script-engine `validate`.

use std::collections::BTreeMap;

use kob_protocol::json::to_hex;
use kob_protocol::registry::Registry;
use kob_protocol::tx::{self, BuiltTx, InputSignature};
use kob_wasm::api;
use serde_json::{json, Value};

const KAS: u64 = 100_000_000;
const SK: [u8; 32] = [0x33; 32];
const COV_OWNER: [u8; 32] = [0xc4; 32];

fn pk() -> String {
    to_hex(&tx::pubkey_of(&SK).unwrap())
}

fn funding(index: u32, amount: u64) -> Value {
    json!({ "transactionId": "07".repeat(32), "index": index, "amount": amount.to_string(), "blockDaaScore": "123456", "pubkey": pk() })
}

/// A valid one-holder request; tests mutate it.
fn request() -> Value {
    json!({
        "name": "Test Token",
        "ticker": "TEST",
        "decimals": 8,
        "supply": "1000",
        "holders": [{ "owner": pk(), "ownerScheme": 0, "amount": "1000" }],
        "funding": [funding(1, 500 * KAS)],
        "network": "testnet-10",
    })
}

fn issue(req: &Value) -> Result<Value, String> {
    api::issue(&req.to_string()).map(|s| serde_json::from_str(&s).unwrap())
}

fn built_of(res: &Value) -> BuiltTx {
    serde_json::from_value(res["built"].clone()).unwrap()
}

fn sign(built: &BuiltTx, sk: [u8; 32]) -> Vec<InputSignature> {
    let keys = BTreeMap::from([(tx::pubkey_of(&sk).unwrap(), sk)]);
    tx::sign_locally(built, &keys).unwrap()
}

/// sign -> finalize (tightened budgets) -> validate; returns the signed tx JSON.
fn sign_finalize_validate(res: &Value) -> Value {
    let built = built_of(res);
    let sigs = serde_json::to_string(&sign(&built, SK)).unwrap();
    let signed = api::finalize(&res["built"].to_string(), &sigs, r#"{"tightenBudgets":true}"#).expect("finalize");
    let report: Value = serde_json::from_str(&api::validate(&signed).expect("validate")).unwrap();
    let fee: u64 = report["fee"].as_str().unwrap().parse().unwrap();
    let min: u64 = report["minFee"].as_str().unwrap().parse().unwrap();
    assert!(fee >= min);
    serde_json::from_str(&signed).unwrap()
}

#[test]
fn normal_issuance_signs_finalizes_and_validates() {
    let res = issue(&request()).unwrap();
    let built = built_of(&res);

    // shape of BuiltTx: one P2PK plan and one SIGHASH_ALL request per funding input
    assert_eq!(built.tx.inputs.len(), 1);
    assert_eq!(built.plans.len(), 1);
    assert_eq!(built.roles, ["p2pk"]);
    assert_eq!(built.sign.len(), 1);
    assert_eq!(built.sign[0].sighash_type, 1);
    assert!(built.sign[0].redeem_script.is_none());
    assert_eq!(to_hex(&built.sign[0].pubkey), pk());
    assert_eq!(built.tx.inputs[0].utxo.block_daa_score, 123456, "funding entry keeps its DAA score");
    assert!(built.tx.inputs[0].signature_script.is_empty());
    assert_eq!(built.tx.outputs.len(), 2, "one token output and change");
    assert_eq!(built.fee.change_output, Some(1));
    assert!(built.fee.fee >= built.fee.min_fee);

    // token facts
    let token = &res["token"];
    assert_eq!(token["program"], "KCC20Ref");
    assert_eq!(token["ticker"], "TEST");
    assert_eq!(token["supply"], "1000");
    assert_eq!(token["carrier"], (10 * KAS).to_string());
    assert_eq!(token["extensionCommitment"], "00".repeat(32));
    assert_eq!(token["outputs"].as_array().unwrap().len(), 1);
    assert_eq!(token["outputs"][0]["amount"], "1000");
    // the genesis group: covenant id agrees between the token info, the covenant list and the output binding
    let cov = token["covenantId"].as_str().unwrap();
    assert_eq!(built.covenants.len(), 1);
    assert_eq!(to_hex(&built.covenants[0].covenant_id), cov);
    assert_eq!(built.covenants[0].outputs, [0]);
    assert_eq!(built.covenants[0].authorizing_input, 0);
    assert_eq!(to_hex(&built.tx.outputs[0].covenant.as_ref().unwrap().covenant_id), cov);
    assert!(built.tx.outputs[1].covenant.is_none(), "change is a plain output");

    // documents
    assert_eq!(res["docs"]["supply"]["total_supply"], "1000");
    assert_eq!(res["docs"]["supply"]["covenant_id"], cov);
    assert_eq!(res["docs"]["supply"]["network"], "testnet-10");
    assert_eq!(res["docs"]["metadata"]["symbol"], "TEST");
    assert_eq!(res["docs"]["registryEntry"]["status"], "pending-review");
    assert_eq!(res["docs"]["registryEntry"]["verified"], false);
    assert_eq!(res["docs"]["registryEntry"]["covenant_id"], cov);
    assert_eq!(res["warnings"], json!([]));

    // the token UTXO's script is the pinned program in the issued state
    let state = json!({
        "amount": "1000", "owner": pk(), "owner_scheme": 0, "borrow_scheme": 0,
        "borrow_guard": "00".repeat(32), "extension_commitment": "00".repeat(32),
    });
    let spk = api::token_script_public_key("KCC20Ref", &state.to_string()).unwrap();
    assert_eq!(built.tx.outputs[0].script_public_key, spk);
    assert_eq!(api::encode_token_state(&state.to_string()).unwrap().len(), 224);

    // the transaction id does not depend on the signatures
    let signed = sign_finalize_validate(&res);
    assert_eq!(signed["tx"]["id"], to_hex(&built.tx.id));
    assert_eq!(signed["tx"]["inputs"][0]["utxo"]["blockDaaScore"], "123456");
}

#[test]
fn issue_is_deterministic() {
    assert_eq!(api::issue(&request().to_string()).unwrap(), api::issue(&request().to_string()).unwrap());
}

#[test]
fn multiple_holders_including_a_covenant_held_one() {
    let mut req = request();
    req["supply"] = json!("1000");
    req["holders"] = json!([
        { "owner": pk(), "ownerScheme": 0, "amount": "600" },
        { "owner": to_hex(&COV_OWNER), "ownerScheme": 4, "amount": "300" },
        { "owner": "ab".repeat(32), "ownerScheme": 1, "amount": "100" },
    ]);
    let res = issue(&req).unwrap();
    let built = built_of(&res);
    assert_eq!(built.tx.outputs.len(), 4, "three token outputs and change");
    assert_eq!(built.covenants[0].outputs, [0, 1, 2]);
    let outs = res["token"]["outputs"].as_array().unwrap();
    assert_eq!(outs.iter().map(|o| o["amount"].as_str().unwrap().parse::<u64>().unwrap()).sum::<u64>(), 1000);
    assert_eq!(outs[1]["ownerScheme"], 4);
    assert_eq!(outs[1]["borrowScheme"], 0, "covenant-held state has borrowing disabled");
    assert_eq!(outs[1]["borrowGuard"], "00".repeat(32));
    // every output is bound to the same covenant and matches the state the token info reports
    let cov = res["token"]["covenantId"].as_str().unwrap();
    for (i, o) in outs.iter().enumerate() {
        assert_eq!(to_hex(&built.tx.outputs[i].covenant.as_ref().unwrap().covenant_id), cov);
        let state = json!({
            "amount": o["amount"], "owner": o["owner"], "owner_scheme": o["ownerScheme"], "borrow_scheme": o["borrowScheme"],
            "borrow_guard": o["borrowGuard"], "extension_commitment": res["token"]["extensionCommitment"],
        });
        assert_eq!(built.tx.outputs[i].script_public_key, api::token_script_public_key("KCC20Ref", &state.to_string()).unwrap());
    }
    sign_finalize_validate(&res);
}

#[test]
fn covenant_id_depends_on_every_output_of_the_group() {
    let a = issue(&request()).unwrap();
    let mut other = request();
    other["holders"][0]["owner"] = json!("ab".repeat(32));
    other["holders"][0]["ownerScheme"] = json!(1);
    let b = issue(&other).unwrap();
    assert_ne!(a["token"]["covenantId"], b["token"]["covenantId"]);
}

#[test]
fn more_than_three_outputs_is_allowed_with_a_warning() {
    let mut req = request();
    req["supply"] = json!("900");
    req["holders"] = json!((1..=9)
        .map(|i| json!({ "owner": format!("{:02x}", i).repeat(32), "ownerScheme": 1, "amount": "100" }))
        .collect::<Vec<_>>());
    req["funding"] = json!([funding(0, 2000 * KAS)]);
    let res = issue(&req).unwrap();
    let w = res["warnings"].as_array().unwrap();
    assert!(w.iter().any(|w| w.as_str().unwrap().contains("9 genesis outputs exceed the 3-output")), "{w:?}");
    sign_finalize_validate(&res);
}

#[test]
fn several_funding_inputs_each_get_a_signature_request() {
    let mut req = request();
    req["funding"] = json!([funding(1, 8 * KAS), funding(2, 8 * KAS), funding(3, 8 * KAS)]);
    let res = issue(&req).unwrap();
    let built = built_of(&res);
    assert_eq!(built.tx.inputs.len(), 3);
    assert_eq!(built.sign.iter().map(|s| s.input_index).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(built.plans.iter().all(|p| matches!(p, tx::SigPlan::P2pk { .. })));
    // input 0 authorises the group
    assert_eq!(built.covenants[0].authorizing_input, 0);
    assert_eq!(built.tx.outputs[0].covenant.as_ref().unwrap().authorizing_input, 0);
    sign_finalize_validate(&res);
}

#[test]
fn change_goes_to_the_requested_key_and_dust_change_joins_the_fee() {
    let mut req = request();
    let other = tx::pubkey_of(&[0x44; 32]).unwrap();
    req["changeTo"] = json!(to_hex(&other));
    let res = issue(&req).unwrap();
    let built = built_of(&res);
    let spk = &built.tx.outputs[1].script_public_key;
    assert_eq!(spk, &format!("0000{}", format_args!("20{}ac", to_hex(&other))), "P2PK spk of the change key");

    // funding covers the carrier plus a little: the remainder is below the dust bound and is added to the fee
    let mut tight = request();
    tight["funding"] = json!([funding(1, 10 * KAS + KAS / 2)]);
    let res = issue(&tight).unwrap();
    let built = built_of(&res);
    assert_eq!(built.tx.outputs.len(), 1, "no change output");
    assert_eq!(built.fee.change_output, None);
    assert_eq!(built.fee.fee, KAS / 2);
    assert!(res["warnings"][0].as_str().unwrap().contains("below the dust bound"));
    sign_finalize_validate(&res);
}

#[test]
fn custom_carrier_extension_commitment_and_fee_rate() {
    let mut req = request();
    req["carrier"] = json!((5 * KAS).to_string());
    req["extensionCommitment"] = json!("5a".repeat(32));
    req["feeRate"] = json!("250");
    let res = issue(&req).unwrap();
    let built = built_of(&res);
    assert_eq!(built.tx.outputs[0].value, 5 * KAS);
    assert_eq!(built.fee.fee_rate, 250);
    assert_eq!(built.fee.min_fee, built.fee.mass.fee_mass * 250);
    assert_eq!(res["token"]["extensionCommitment"], "5a".repeat(32));
    assert_eq!(res["docs"]["registryEntry"]["extension_commitment"], "5a".repeat(32));
    sign_finalize_validate(&res);
}

#[test]
fn registry_entry_validates_against_the_shipped_registry() {
    let mut req = request();
    req["description"] = json!("A test token");
    req["website"] = json!("https://example.org");
    req["icon"] = json!("ipfs://bafy");
    let res = issue(&req).unwrap();
    let entry = &res["docs"]["registryEntry"];
    assert_eq!(entry["display"]["website"], "https://example.org");
    let mut registry = Registry::default_registry();
    registry.tokens.push(serde_json::from_value(entry.clone()).expect("registry entry parses as a registry token"));
    registry.validate().expect("issued token validates in the registry");
}

fn err(req: &Value) -> String {
    issue(req).expect_err("must be rejected")
}

#[test]
fn ticker_rules() {
    for bad in ["test", "T", "TOOLONGTICKER1", "TE ST", "T\u{c9}ST", ""] {
        let mut req = request();
        req["ticker"] = json!(bad);
        assert!(err(&req).contains("ticker"), "{bad}");
    }
    for ok in ["TE", "T3ST", "ABCDEFGHIJKL"] {
        let mut req = request();
        req["ticker"] = json!(ok);
        issue(&req).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

#[test]
fn name_decimals_and_display_rules() {
    let mut req = request();
    req["name"] = json!("   ");
    assert!(err(&req).contains("name"));
    req["name"] = json!("x".repeat(65));
    assert!(err(&req).contains("name"));
    req["name"] = json!("ev\u{202e}il");
    assert!(err(&req).contains("name"), "bidi control characters");
    let mut req = request();
    req["decimals"] = json!(19);
    assert!(err(&req).contains("decimals"));
    req["decimals"] = json!(18);
    issue(&req).unwrap();
    let mut req = request();
    req["website"] = json!("http://example.org");
    assert!(err(&req).contains("https"));
    let mut req = request();
    req["icon"] = json!("data:image/png;base64,AAAA");
    assert!(err(&req).contains("icon"));
    let mut req = request();
    req["description"] = json!("d".repeat(513));
    assert!(err(&req).contains("description"));
}

#[test]
fn supply_rules() {
    let mut req = request();
    req["supply"] = json!("1001");
    let e = err(&req);
    assert!(e.contains("supply mismatch") && e.contains("1000") && e.contains("1001"), "{e}");

    let mut req = request();
    req["supply"] = json!("0");
    req["holders"] = json!([{ "owner": pk(), "ownerScheme": 0, "amount": "0" }]);
    assert!(err(&req).contains("supply"));

    // the maximum is allowed, one above is not
    let max = kob_protocol::issue::MAX_SUPPLY;
    let mut req = request();
    req["supply"] = json!(max.to_string());
    req["holders"] = json!([{ "owner": pk(), "ownerScheme": 0, "amount": max.to_string() }]);
    issue(&req).unwrap();
    req["supply"] = json!((max + 1).to_string());
    req["holders"][0]["amount"] = json!((max + 1).to_string());
    assert!(err(&req).contains("supply"));

    // holder amounts that overflow u64 must not wrap around to the declared supply
    let mut req = request();
    req["supply"] = json!("2");
    req["holders"] = json!([
        { "owner": pk(), "ownerScheme": 0, "amount": "9223372036854775810" },
        { "owner": pk(), "ownerScheme": 0, "amount": "9223372036854775808" },
    ]);
    assert!(err(&req).contains("supply mismatch"));

    let mut req = request();
    req["holders"] = json!([]);
    assert!(err(&req).contains("holder"));
    let mut req = request();
    req["holders"][0]["ownerScheme"] = json!(9);
    assert!(err(&req).contains("owner scheme"));
    let mut req = request();
    req["holders"] = json!((0..65)
        .map(|i| json!({ "owner": format!("{:02x}", i + 1).repeat(32), "ownerScheme": 1, "amount": "1" }))
        .collect::<Vec<_>>());
    req["supply"] = json!("65");
    assert!(err(&req).contains("at most 64 genesis outputs"));
}

#[test]
fn borrow_rules() {
    // covenant-held states can never borrow, even with the explicit flag
    let mut req = request();
    req["holders"] = json!([{ "owner": to_hex(&COV_OWNER), "ownerScheme": 4, "amount": "1000", "borrowScheme": 1, "borrowGuard": "01".repeat(32) }]);
    assert!(err(&req).contains("borrow rule violated"));
    req["allowBorrow"] = json!(true);
    assert!(err(&req).contains("covenant-held"));

    // other holders need the explicit flag
    let mut req = request();
    req["holders"][0]["borrowScheme"] = json!(1);
    req["holders"][0]["borrowGuard"] = json!("01".repeat(32));
    assert!(err(&req).contains("allow-borrow"));
    req["allowBorrow"] = json!(true);
    let res = issue(&req).unwrap();
    assert_eq!(res["docs"]["supply"]["borrow"], "enabled-on-some-issued-outputs");
    assert_eq!(res["token"]["outputs"][0]["borrowScheme"], 1);

    // a guard without a scheme is refused
    let mut req = request();
    req["holders"][0]["borrowGuard"] = json!("01".repeat(32));
    assert!(err(&req).contains("borrow_guard"));
}

#[test]
fn funding_rules() {
    let mut req = request();
    req["funding"] = json!([funding(1, 5 * KAS)]);
    let e = err(&req);
    assert!(e.contains("insufficient funds") && e.contains(&(10 * KAS).to_string()), "{e}");

    // enough for the carrier but not the fee
    let mut req = request();
    req["funding"] = json!([funding(1, 10 * KAS + 1000)]);
    assert!(err(&req).contains("insufficient funds"));

    let mut req = request();
    req["funding"] = json!([]);
    assert!(err(&req).contains("funding"));

    let mut req = request();
    req["funding"] = json!([funding(1, 500 * KAS), funding(1, 500 * KAS)]);
    assert!(err(&req).contains("twice"));

    let mut req = request();
    let mut f = funding(1, 500 * KAS);
    f["covenantId"] = json!("11".repeat(32));
    req["funding"] = json!([f]);
    assert!(err(&req).contains("covenant id"));

    let mut req = request();
    req["funding"] = json!([funding(1, u64::MAX), funding(2, 10)]);
    assert!(err(&req).contains("overflow"));

    let mut req = request();
    req["feeRate"] = json!("99");
    assert!(err(&req).contains("below the relay minimum"));
}

#[test]
fn malformed_specs_are_rejected_with_a_reason() {
    assert!(api::issue("{").unwrap_err().starts_with("spec:"));
    let mut req = request();
    req["supply"] = json!("12x");
    let e = err(&req);
    assert!(e.starts_with("spec:") && e.contains("12x"), "{e}");
    let mut req = request();
    req["holders"][0]["owner"] = json!("abcd");
    assert!(err(&req).contains("32 bytes"));
}

#[test]
fn a_wrong_signature_is_refused_by_finalize() {
    let res = issue(&request()).unwrap();
    let built = built_of(&res);
    let bad = sign(&built, SK);
    // a signature by another key over the same digest does not verify for the funding key
    let other = InputSignature { input_index: 0, signature: tx::sign_digest(&[0x55; 32], &built.sign[0].sighash).unwrap() };
    let e = api::finalize(&res["built"].to_string(), &serde_json::to_string(&[other]).unwrap(), "").unwrap_err();
    assert!(e.contains("does not verify"), "{e}");
    let e = api::finalize(&res["built"].to_string(), "[]", "").unwrap_err();
    assert!(e.contains("missing"), "{e}");
    api::finalize(&res["built"].to_string(), &serde_json::to_string(&bad).unwrap(), "").unwrap();
}

#[test]
fn limits_mirror_the_protocol_constants() {
    let l: Value = serde_json::from_str(&api::issue_limits().unwrap()).unwrap();
    assert_eq!(l["maxSupply"], kob_protocol::issue::MAX_SUPPLY.to_string());
    assert_eq!(l["maxGenesisOutputs"], 64);
    assert_eq!(l["defaultCarrier"], (10 * KAS).to_string());
    assert_eq!((l["maxTokenInputs"].clone(), l["maxTokenOutputs"].clone()), (json!(3), json!(3)));
    assert_eq!(l["maxDecimals"], 18);
    assert_eq!(l["ticker"]["pattern"], "^[A-Z0-9]{2,12}$");
    assert_eq!((l["program"].as_str(), l["registryTemplateId"].as_str()), (Some("KCC20Ref"), Some("kcc20-ref-3x3")));
    assert_eq!(l["ownerSchemes"], json!([0, 1, 2, 3, 4]));
}

#[test]
fn wallets_with_little_more_than_the_carrier_still_get_a_valid_genesis() {
    // storage mass dominates the fee here and grows as the change shrinks: the fee must hold at the final change value
    let mut req = request();
    req["funding"] = json!([funding(1, 6 * KAS), funding(2, 6 * KAS)]);
    let res = issue(&req).unwrap();
    assert!(built_of(&res).fee.mass.storage > built_of(&res).fee.mass.compute);
    sign_finalize_validate(&res);

    // exactly the carrier leaves nothing for the fee: a reason, not a trap
    let mut req = request();
    req["funding"] = json!([funding(1, 10 * KAS)]);
    let e = err(&req);
    assert!(e.contains("insufficient funds") && e.contains("have 1000000000"), "{e}");

    // every amount just above the carrier either plans and validates or reports the shortfall
    for k in 0..40u64 {
        let mut req = request();
        req["funding"] = json!([funding(1, 10 * KAS + k * 5_000_000)]);
        match issue(&req) {
            Ok(res) => {
                sign_finalize_validate(&res);
            }
            Err(e) => assert!(e.contains("insufficient funds"), "k={k}: {e}"),
        }
    }
}

/// The app issues KOB's standard 3 / 3 program by default and the published public-mint build on request; the 8 / 8
/// prototype is not offered (the CLI alone issues it, by its explicit name).
#[test]
fn programs_the_app_issues() {
    let mut req = request();
    for (program, name, tpl) in
        [(None, "KCC20Ref", "kcc20-ref-3x3"), (Some("public-mint"), "KCC20PublicMint", "kcc20-ref-public-mint")]
    {
        if let Some(p) = program {
            req["program"] = json!(p);
        }
        let res = issue(&req).unwrap();
        assert_eq!((res["token"]["program"].as_str(), res["docs"]["registryEntry"]["template_id"].as_str()), (Some(name), Some(tpl)));
        assert_eq!(res["docs"]["registryEntry"]["max_token_inputs"], 3);
        sign_finalize_validate(&res);
    }
    for refused in ["8x8", "8x8-prototype", "kcc20-ref-8x8"] {
        req["program"] = json!(refused);
        assert!(issue(&req).unwrap_err().contains("prototype"), "{refused}");
    }
}
