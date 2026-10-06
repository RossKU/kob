//! Regression: the custody class of a token is derived from its PROGRAM's capabilities in the shipped
//! registry, never from a label. A KaspaCom program (mint-authority, public-mint, burn) is `issuer-controlled`:
//!  * `x402TokenOffer` cannot label it `unconditional` (and defaults to `issuer-controlled`);
//!  * the payer side ignores the offer's own `custody` claim: an offer that lies is refused before anything is signed,
//!    and an honest `issuer-controlled` offer needs the payer's explicit `allowIssuerControlled`.
#![cfg(feature = "x402")]

use kob_wasm::x402;
use serde_json::{json, Value};

const KASPACOM_TEMPLATE: &str = "911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c";

fn addr() -> String {
    kaspa_addresses::Address::new(kaspa_addresses::Prefix::Testnet, kaspa_addresses::Version::PubKey, &[8u8; 32]).to_string()
}

fn offer_req(custody: Option<&str>) -> Value {
    let mut v = json!({
        "network": "kaspa:testnet-10",
        "asset": "73".repeat(32),
        "payTo": addr(),
        "amount": "5",
        "templateHash": KASPACOM_TEMPLATE,
        "extensionCommitment": "ee".repeat(32),
    });
    if let Some(c) = custody {
        v["custody"] = json!(c);
    }
    v
}

#[test]
fn a_kaspacom_program_cannot_be_offered_as_unconditional() {
    // default: derived from the registry capabilities
    let v: Value = serde_json::from_str(&x402::token_offer(&offer_req(None).to_string()).expect("offer builds")).unwrap();
    assert_eq!(v["custody"], "issuer-controlled");
    // an explicit stricter label is fine
    let v: Value = serde_json::from_str(&x402::token_offer(&offer_req(Some("issuer-controlled")).to_string()).unwrap()).unwrap();
    assert_eq!(v["custody"], "issuer-controlled");
    // a looser one is refused
    let e = x402::token_offer(&offer_req(Some("unconditional")).to_string()).unwrap_err();
    assert!(e.contains("cannot be labelled unconditional"), "{e}");
}

#[test]
fn a_registry_program_without_capabilities_stays_unconditional() {
    // the reference 3x3 program (registry: no capabilities) keeps the default
    let mut req = offer_req(None);
    req["templateHash"] = json!("f4ac029d2c3c74dd3dcaeb64245f7d0a0977e27c2956f3977540a11dc7c45b1f");
    let v: Value = serde_json::from_str(&x402::token_offer(&req.to_string()).unwrap()).unwrap();
    assert_eq!(v["custody"], "unconditional");
}

fn lying_offer() -> Value {
    // a merchant that advertises the KaspaCom program as unconditional custody (hand-edited: the SDK refuses to build it)
    let honest: Value =
        serde_json::from_str(&x402::kcc20_requirements(&offer_req(Some("issuer-controlled")).to_string()).unwrap()).unwrap();
    let mut lie = honest;
    lie["extra"]["token"]["custody"] = json!("unconditional");
    lie
}

fn pay_request(offer: Value, allow_issuer: bool) -> String {
    json!({
        "offer": offer,
        "requestHash": "11".repeat(32),
        "secretKeys": ["0202020202020202020202020202020202020202020202020202020202020202"],
        "tokenUtxos": [],
        "funding": [],
        "nowMs": "1800000000000",
        "allowIssuerControlled": allow_issuer,
    })
    .to_string()
}

#[test]
fn the_payer_does_not_believe_the_offers_custody_claim() {
    // lying offer: refused for the custody mismatch, even when the payer opts in to issuer-controlled tokens
    for allow in [false, true] {
        let e = x402::pay_kcc20(&pay_request(lying_offer(), allow)).unwrap_err();
        assert!(e.contains("token_custody_policy"), "allow={allow}: {e}");
    }
    // honest issuer-controlled offer: refused unless the payer allows it
    let honest: Value = serde_json::from_str(&x402::kcc20_requirements(&offer_req(None).to_string()).unwrap()).unwrap();
    let e = x402::pay_kcc20(&pay_request(honest.clone(), false)).unwrap_err();
    assert!(e.contains("token_custody_policy"), "{e}");
    // allowed: the policy gate passes and the build fails later, for the missing token UTXOs
    let e = x402::pay_kcc20(&pay_request(honest, true)).unwrap_err();
    assert!(!e.contains("token_custody_policy"), "{e}");
}

#[test]
fn the_wallet_flow_runs_the_same_gate_before_asking_for_signatures() {
    let e = x402::build_kcc20_unsigned(&pay_request(lying_offer(), true)).unwrap_err();
    assert!(e.contains("token_custody_policy"), "{e}");
}

#[test]
fn a_payer_pin_cannot_downgrade_the_registry_class() {
    let mut req: Value = serde_json::from_str(&pay_request(lying_offer(), true)).unwrap();
    req["tokens"] = json!([{
        "covenantId": "73".repeat(32),
        "templateHash": KASPACOM_TEMPLATE,
        "extensionCommitment": "ee".repeat(32),
        "custody": "unconditional",
    }]);
    let e = x402::pay_kcc20(&req.to_string()).unwrap_err();
    assert!(e.contains("cannot be labelled unconditional"), "{e}");
}
