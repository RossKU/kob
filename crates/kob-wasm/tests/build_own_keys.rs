//! Regression: the builders do not tie a destination key to a signer key. `api::build` takes an optional
//! `"ownKeys"` list (the wallet's x-only keys): every key an action pays back to the user (order maker, change,
//! token change, taker, replacement maker) must then be one of them.
use kob_wasm::api;
use serde_json::{json, Value};

const A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const B: &str = "0202020202020202020202020202020202020202020202020202020202020202";

fn send(token_change: &str, change: Option<&str>, own: Option<Value>) -> String {
    let mut v = json!({
        "action": "sendTokens",
        "token": { "covenantId": "70".repeat(32), "program": "KCC20Ref_8x8" },
        "tokens": [],
        "recipients": [{ "pubkey": B, "amount": "1", "carrier": "100000000" }],
        "tokenChange": token_change,
    });
    if let Some(c) = change {
        v["change"] = json!(c);
    }
    if let Some(o) = own {
        v["ownKeys"] = o;
    }
    v.to_string()
}

#[test]
fn a_foreign_change_key_is_refused_when_own_keys_are_given() {
    let e = api::build(&send(B, None, Some(json!([A])))).unwrap_err();
    assert!(e.contains("token change key") && e.contains("not one of the wallet keys"), "{e}");
    let e = api::build(&send(A, Some(B), Some(json!([A])))).unwrap_err();
    assert!(e.contains("change key") && e.contains("not one of the wallet keys"), "{e}");
}

#[test]
fn recipients_are_the_deliberate_foreign_destination() {
    // recipient B is not an own key, and that is fine: the build proceeds past the key check (it fails later for the
    // empty token input list, not with the key error)
    let e = api::build(&send(A, Some(A), Some(json!([A])))).unwrap_err();
    assert!(!e.contains("not one of the wallet keys"), "{e}");
}

#[test]
fn without_own_keys_the_behaviour_is_unchanged_and_bad_own_keys_are_refused() {
    let e = api::build(&send(B, None, None)).unwrap_err();
    assert!(!e.contains("not one of the wallet keys"), "{e}");
    let e = api::build(&send(A, None, Some(json!(["zz"])))).unwrap_err();
    assert!(e.contains("ownKeys"), "{e}");
}
