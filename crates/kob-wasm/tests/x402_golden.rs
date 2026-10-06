//! Golden vectors of the x402 wasm bindings.
//!
//! `crates/kob-x402/vectors/golden/x402-payments.json` holds requests and the exact results of the
//! `kob_wasm::x402` functions (KAS, KCC-20 and swap-and-pay payments, the wallet flow, preflight,
//! revocation, requirement builders, hashes and digests) for deterministic keys and fixtures. This
//! test regenerates them and compares with the committed file (`KOB_REGEN=1` rewrites it); the node
//! test `packages/kob-x402/test/wasm-golden.test.ts` replays every request through the wasm build and
//! must reproduce the same bytes.
//!
//! Schnorr signing is deterministic (`sign_schnorr_no_aux_rand`), so the vectors are stable.

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::json::to_hex;
use kob_protocol::registry::Family;
use kob_protocol::state::{BidState, TokenState, SCHEME_P2PK};
use kob_protocol::tx::{sign_locally, BuiltTx, KeyUtxo, TokenUtxo};
use kob_wasm::x402;
use kob_x402::client::swap::{OrderRef, Quote};
use kob_x402::wire::PaymentPayload;
use serde_json::{json, Value};

use common::{keys, pk, sk, CARRIER, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

const T: TemplateId = TemplateId::Kcc20Ref;
/// KAS carrier the golden merchant outputs quote: within the payer ceiling (`Limits::max_carrier_sompi`, 2 KAS).
const OFFER_CARRIER: u64 = 2 * KAS;
/// A KRON token (pay asset only) and its program.
const KRON: TemplateId = TemplateId::KronToken2433;
const KRON_COV: [u8; 32] = [0x72; 32];
/// A KaspaCom-template KCC-20 token (offers only: its 25.5 KB program would bloat the vectors).
const KC: TemplateId = TemplateId::Kcc20KaspaCom025;
const KC_COV: [u8; 32] = [0x73; 32];
const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const RH: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

fn address(n: u8) -> String {
    kaspa_addresses::Address::new(kaspa_addresses::Prefix::Testnet, kaspa_addresses::Version::PubKey, &pk(n)).to_string()
}

fn p2pk_hex(n: u8) -> String {
    kob_protocol::tx::spk_to_string(&kob_protocol::script::p2pk_spk(&pk(n)))
}

fn payer_utxo(k: &KeyUtxo) -> Value {
    json!({
        "txid": to_hex(&k.utxo.transaction_id),
        "index": k.utxo.index,
        "amount": k.utxo.amount.to_string(),
        "scriptPublicKey": p2pk_hex(PAYER),
        "blockDaaScore": k.utxo.block_daa_score.to_string(),
        "isCoinbase": false,
    })
}

fn token_json(t: &TokenUtxo) -> Value {
    serde_json::to_value(t).unwrap()
}

fn token_spec(cov: [u8; 32], ticker: &str) -> Value {
    json!({
        "covenantId": to_hex(&cov),
        "templateHash": to_hex(&template(T).hash),
        "extensionCommitment": to_hex(&EXT),
        "custody": "unconditional",
        "ticker": ticker,
        "decimals": 3,
    })
}

/// A token of either family: the program template hash, no extension commitment for KRON.
fn token_spec_on(cov: [u8; 32], prog: TemplateId, ticker: &str) -> Value {
    let mut v = json!({
        "covenantId": to_hex(&cov),
        "templateHash": to_hex(&token_template(prog).hash),
        // the custody class is the program's (registry capabilities): the KaspaCom program can mint and burn, and the
        // KRON programs carry a mint authority, so both are issuer-controlled
        "custody": if prog == KC || prog.family() == Family::Kron { "issuer-controlled" } else { "unconditional" },
        "ticker": ticker,
        "decimals": 3,
    });
    if prog.family() == Family::Kcc20 {
        v["extensionCommitment"] = json!(to_hex(&EXT));
    }
    v
}

/// Runs `f(request)` and returns the request value with the parsed result.
fn call(f: impl Fn(&str) -> Result<String, String>, name: &str, request: Value) -> (Value, Value) {
    let text = f(&request.to_string()).unwrap_or_else(|e| panic!("{name}: {e}"));
    (request, serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: result is not JSON: {e}")))
}

fn sigs_for(built: &Value) -> Value {
    let b: BuiltTx = serde_json::from_value(built.clone()).unwrap();
    serde_json::to_value(sign_locally(&b, &keys()).unwrap()).unwrap()
}

fn header_of(payment: &Value) -> String {
    let p: PaymentPayload = serde_json::from_value(payment["paymentPayload"].clone()).unwrap();
    kob_x402::client::native::payment_signature_header(&p).unwrap()
}

/// `payload` with its first payer signature re-made under `hash_type` (a valid engine input, not SIGHASH_ALL).
fn retyped(payload: &Value, hash_type: u8) -> Value {
    use kob_x402::testkit::hashtype::{resigned, signers, with_tx};
    let p: PaymentPayload = serde_json::from_value(payload.clone()).unwrap();
    let parsed = kob_x402::safe_tx::SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap();
    let entries: Vec<_> = parsed
        .hints
        .iter()
        .map(|h| {
            let h = h.as_ref().expect("the payload carries its UTXO hints");
            kaspa_consensus_core::tx::UtxoEntry::new(
                h.amount,
                h.script_public_key.clone(),
                0,
                false,
                h.covenant_id.map(kaspa_consensus_core::Hash::from_bytes),
            )
        })
        .collect();
    let s = signers(&parsed.tx, &entries, &keys())[0];
    serde_json::to_value(with_tx(&p, &resigned(&parsed.tx, &entries, &s, hash_type), &entries)).unwrap()
}

fn generate() -> Value {
    let mut cases: Vec<Value> = vec![];
    let mut push = |name: &str, function: &str, request: Value, expected: Value, extra: Value| {
        let mut c = json!({ "name": name, "function": function, "request": request, "expected": expected });
        if let (Some(o), Value::Object(e)) = (c.as_object_mut(), extra) {
            o.extend(e);
        }
        cases.push(c);
    };

    // ---------------------------------------------------------------------------- hashes and digests
    let req_native = json!({ "network": "kaspa:testnet-10", "amount": "50000000", "payTo": address(MERCHANT), "maxTimeoutSeconds": 60, "finality": "accepted" });
    let (rq, native_offer) = call(x402::native_requirements, "native offer", req_native);
    push("offer.native", "x402NativeRequirements", rq, native_offer.clone(), json!({}));

    let req_hash = x402::requirements_hash(&native_offer.to_string()).unwrap();
    push("hash.requirements", "x402RequirementsHash", json!({ "requirements": native_offer }), json!(req_hash), json!({}));
    let rh = x402::request_hash("GET", "https://api.example.test/report?x=1", "null", &req_hash).unwrap();
    push(
        "hash.request",
        "x402RequestHash",
        json!({ "method": "GET", "url": "https://api.example.test/report?x=1", "body": "null", "requirementsHash": req_hash }),
        json!(rh),
        json!({}),
    );
    let rh2 = x402::request_hash("POST", "https://api.example.test/order", r#"{"z":1,"a":[2]}"#, &req_hash).unwrap();
    push(
        "hash.request.body",
        "x402RequestHash",
        json!({ "method": "POST", "url": "https://api.example.test/order", "body": r#"{"z":1,"a":[2]}"#, "requirementsHash": req_hash }),
        json!(rh2),
        json!({}),
    );
    push(
        "spk.address",
        "x402AddressToSpk",
        json!({ "address": address(MERCHANT) }),
        json!(x402::address_to_spk(&address(MERCHANT)).unwrap()),
        json!({}),
    );
    let auth = json!({
        "network": "kaspa:testnet-10", "profile": "standard-native", "transactionId": "15".repeat(32), "paymentOutputIndex": 0,
        "amount": "50000000", "payTo": address(MERCHANT), "payToScriptPublicKey": p2pk_hex(MERCHANT), "paymentRequirementsHash": req_hash,
        "requestHash": RH, "inputIndex": 1, "expiresAt": "2099-01-01T00:00:00.000Z",
    });
    push(
        "digest.signed",
        "x402SignedAuthDigest",
        json!({ "request": auth }),
        json!(x402::signed_auth_digest(&auth.to_string()).unwrap()),
        json!({}),
    );
    let commit = json!({
        "network": "kaspa:testnet-10", "profile": "kcc20", "routePayAsset": to_hex(&TOKEN_COV), "asset": to_hex(&TOKEN_B), "amount": "2000",
        "payTo": address(MERCHANT), "payToScriptPublicKey": p2pk_hex(MERCHANT), "paymentOutputIndex": 2, "paymentRequirementsHash": req_hash,
        "requestHash": RH, "expiresAt": "2099-01-01T00:00:00.000Z",
    });
    push(
        "digest.commit",
        "x402PayloadCommitDigest",
        json!({ "request": commit }),
        json!(x402::payload_commit_digest(&commit.to_string()).unwrap()),
        json!({}),
    );

    // ------------------------------------------------------------------------------------- offers
    let kcc20_req = json!({
        "network": "kaspa:testnet-10", "asset": to_hex(&TOKEN_COV), "payTo": address(MERCHANT), "amount": "2000", "carrier": OFFER_CARRIER.to_string(),
        "maxTimeoutSeconds": 600, "finality": "accepted", "custody": "unconditional", "templateHash": to_hex(&template(T).hash),
        "extensionCommitment": to_hex(&EXT), "ticker": "AAA", "decimals": 3,
    });
    let (rq, kcc20_offer) = call(x402::kcc20_requirements, "kcc20 offer", kcc20_req);
    push("offer.kcc20", "x402Kcc20Requirements", rq.clone(), kcc20_offer.clone(), json!({}));
    push("offer.kcc20.token", "x402TokenOffer", rq, kcc20_offer["extra"]["token"].clone(), json!({}));

    let swap_kas_req = json!({
        "network": "kaspa:testnet-10", "amount": KAS.to_string(), "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kas", "payAssets": [token_spec(TOKEN_COV, "AAA")],
    });
    let (rq, swap_kas_offer) = call(x402::swap_requirements, "swap kas offer", swap_kas_req);
    push("offer.swap.kas", "x402SwapRequirements", rq, swap_kas_offer.clone(), json!({}));
    let swap_tok_req = json!({
        "network": "kaspa:testnet-10", "amount": (2 * WHOLE).to_string(), "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kcc20", "carrier": OFFER_CARRIER.to_string(), "token": token_spec(TOKEN_B, "BBB"), "payAssets": [token_spec(TOKEN_COV, "AAA")],
    });
    let (rq, swap_tok_offer) = call(x402::swap_requirements, "swap token offer", swap_tok_req);
    push("offer.swap.token", "x402SwapRequirements", rq, swap_tok_offer.clone(), json!({}));
    let swap_tok_kas_req = json!({
        "network": "kaspa:testnet-10", "amount": (2 * WHOLE).to_string(), "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kcc20", "carrier": OFFER_CARRIER.to_string(), "token": token_spec(TOKEN_B, "BBB"), "payAssets": [{ "covenantId": "KAS" }, token_spec(TOKEN_COV, "AAA")],
    });
    let (rq, swap_tok_kas_offer) = call(x402::swap_requirements, "swap token offer paid in KAS", swap_tok_kas_req);
    push("offer.swap.token.kas", "x402SwapRequirements", rq, swap_tok_kas_offer, json!({}));

    // ------------------------------------------------------------------------------ native payment
    let funding1 = common::key_utxo(12, PAYER, 20 * KAS);
    let funding2 = common::key_utxo(13, PAYER, 7 * KAS);
    let native_req = json!({
        "offer": native_offer, "requestHash": RH, "secretKey": to_hex(&sk(PAYER)), "utxos": [payer_utxo(&funding2), payer_utxo(&funding1)],
        "nowMs": NOW_MS.to_string(), "options": { "paymentId": "pay-native-golden-0001" },
    });
    let (rq, native_pay) = call(x402::pay_native, "pay native", native_req);
    let native_header = header_of(&native_pay);
    push("pay.native", "x402PayNative", rq, native_pay.clone(), json!({ "signatureHeader": native_header }));
    let native_payload = native_pay["paymentPayload"].clone();
    let pre = json!({ "offer": native_offer, "payload": native_payload, "requestHash": RH, "nowMs": NOW_MS.to_string() });
    let (rq, ok) = call(x402::preflight, "preflight native", pre.clone());
    assert_eq!(ok["ok"], true, "{ok}");
    push("preflight.native", "x402Preflight", rq, ok, json!({}));
    let mut expired = pre.clone();
    expired["nowMs"] = json!((NOW_MS + 3_600_000).to_string());
    let (rq, bad) = call(x402::preflight, "preflight native expired", expired);
    assert_eq!(bad["ok"], false);
    push("preflight.native.expired", "x402Preflight", rq, bad, json!({}));
    let rev = json!({ "payload": native_payload, "secretKeys": [to_hex(&sk(PAYER))] });
    let (rq, revoked) = call(x402::revoke, "revoke native", rev);
    push("revoke.native", "x402Revoke", rq, revoked, json!({}));

    // -------------------------------------------------------------------------------- kcc20 payment
    let payer_tok = common::tok(21, 3 * WHOLE, pk(PAYER), SCHEME_P2PK, 1_000);
    let kcc_common = json!({
        "offer": kcc20_offer, "requestHash": RH, "tokenUtxos": [token_json(&payer_tok)], "funding": [payer_utxo(&funding1)],
        "nowMs": NOW_MS.to_string(), "options": { "paymentId": "pay-kcc20-golden-0001" },
    });
    let mut all_in_one = kcc_common.clone();
    all_in_one["secretKeys"] = json!([to_hex(&sk(PAYER))]);
    let (rq, kcc_pay) = call(x402::pay_kcc20, "pay kcc20", all_in_one);
    push("pay.kcc20", "x402PayKcc20", rq, kcc_pay.clone(), json!({ "signatureHeader": header_of(&kcc_pay) }));
    let kcc_payload = kcc_pay["paymentPayload"].clone();
    let (rq, ok) = call(
        x402::preflight,
        "preflight kcc20",
        json!({ "offer": kcc20_offer, "payload": kcc_payload, "requestHash": RH, "nowMs": NOW_MS.to_string() }),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    push("preflight.kcc20", "x402Preflight", rq, ok, json!({}));
    // every other hash type on the owner witness is refused with `token_owner_scheme`, although the script engine
    // accepts each of these correctly signed transactions (profile step 8, SIGHASH_ALL rule of section 5.1)
    for t in kob_x402::testkit::hashtype::NON_ALL {
        let (rq, bad) = call(
            x402::preflight,
            "preflight kcc20 hash type",
            json!({ "offer": kcc20_offer, "payload": retyped(&kcc_payload, t), "requestHash": RH, "nowMs": NOW_MS.to_string() }),
        );
        assert_eq!((bad["ok"].clone(), bad["diagnostic"].clone()), (json!(false), json!("token_owner_scheme")), "{bad}");
        push(&format!("preflight.kcc20.hashtype.{t:#04x}"), "x402Preflight", rq, bad, json!({}));
    }
    let (rq, revoked) = call(x402::revoke, "revoke kcc20", json!({ "payload": kcc_payload, "secretKeys": [to_hex(&sk(PAYER))] }));
    push("revoke.kcc20", "x402Revoke", rq, revoked, json!({}));

    // the wallet flow: unsigned, sign requests, signatures only come back
    let mut wallet = kcc_common.clone();
    wallet["payerPublicKey"] = json!(to_hex(&pk(PAYER)));
    let (rq, unsigned) = call(x402::build_kcc20_unsigned, "build kcc20 unsigned", wallet);
    let sigs = sigs_for(&unsigned["built"]);
    let finished = x402::finish_kcc20(&unsigned["built"].to_string(), &unsigned["template"].to_string(), &sigs.to_string()).unwrap();
    let finished: Value = serde_json::from_str(&finished).unwrap();
    // the wallet flow assembles the very payload the local flow signed
    assert_eq!(finished["paymentPayload"]["payload"]["transaction"], kcc_pay["paymentPayload"]["payload"]["transaction"]);
    push("wallet.kcc20.unsigned", "x402BuildKcc20Unsigned", rq, unsigned.clone(), json!({}));
    push(
        "wallet.kcc20.finish",
        "x402FinishKcc20",
        json!({ "built": unsigned["built"], "template": unsigned["template"], "signatures": sigs }),
        finished,
        json!({}),
    );

    // ---------------------------------------------------------------------------- swap-and-pay
    let bid1 = {
        let bs = common::bid(common::MAKER_B, P245, T);
        common::order(20, (bs.used(5 * WHOLE).expect("budget") + common::DC) as u64, common::cov(0xb1), 1_000, bs)
    };
    let ask1 = {
        let a = kob_protocol::state::AskState { token_cov_id: TOKEN_B, ..common::ask_n(common::MAKER_C, P250, T, 5) };
        let c = common::cov(0x5a);
        (common::order(10, CARRIER, c, 1_000, a), common::tok_of(11, 5 * WHOLE, c, kob_protocol::state::SCHEME_COVID, 1_000, TOKEN_B))
    };
    let quote1 = Quote { lock_time: NOW, orders: vec![OrderRef::bid(bid1.clone(), 3 * WHOLE)] };
    let quote3 = Quote {
        lock_time: NOW,
        orders: vec![OrderRef::bid(bid1.clone(), 3 * WHOLE), OrderRef::ask(ask1.0.clone(), ask1.1.clone(), 2 * WHOLE)],
    };

    // SW1: token A sold into a bid, the merchant receives KAS
    let sw1 = json!({
        "offer": swap_kas_offer, "quote": quote1, "requestHash": RH, "tokenUtxos": [token_json(&payer_tok)], "funding": [],
        "nowMs": NOW_MS.to_string(), "options": { "paymentIdentifier": "pay-swap1-golden-0001" },
    });
    let mut sw1_local = sw1.clone();
    sw1_local["secretKeys"] = json!([to_hex(&sk(PAYER))]);
    let (rq, sw1_pay) = call(x402::pay_swap, "pay swap sw1", sw1_local);
    push("pay.swap.sw1", "x402PaySwap", rq, sw1_pay.clone(), json!({ "signatureHeader": header_of(&sw1_pay) }));
    let (rq, ok) = call(
        x402::preflight,
        "preflight sw1",
        json!({ "offer": swap_kas_offer, "payload": sw1_pay["paymentPayload"], "requestHash": RH, "nowMs": NOW_MS.to_string(), "maxPay": (3 * WHOLE).to_string() }),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    push("preflight.swap.sw1", "x402Preflight", rq, ok, json!({}));
    let (rq, over) = call(
        x402::preflight,
        "preflight sw1 over bound",
        json!({ "offer": swap_kas_offer, "payload": sw1_pay["paymentPayload"], "requestHash": RH, "nowMs": NOW_MS.to_string(), "maxPay": "1" }),
    );
    assert_eq!(over["ok"], false);
    assert_eq!(over["diagnostic"], "overpayment");
    push("preflight.swap.sw1.over-bound", "x402Preflight", rq, over, json!({}));
    let (rq, revoked) =
        call(x402::revoke, "revoke sw1", json!({ "payload": sw1_pay["paymentPayload"], "secretKeys": [to_hex(&sk(PAYER))] }));
    push("revoke.swap.sw1", "x402Revoke", rq, revoked, json!({}));

    // wallet flow for the swap
    let mut sw1_wallet = sw1.clone();
    sw1_wallet["payerPublicKey"] = json!(to_hex(&pk(PAYER)));
    let (rq, prepared) = call(x402::prepare_swap, "prepare swap sw1", sw1_wallet);
    let sigs = sigs_for(&prepared["built"]);
    let finished: Value = serde_json::from_str(&x402::finish_swap(&prepared.to_string(), &sigs.to_string()).unwrap()).unwrap();
    assert_eq!(finished["paymentPayload"]["payload"]["transaction"], sw1_pay["paymentPayload"]["payload"]["transaction"]);
    push("wallet.swap.prepare", "x402PrepareSwap", rq, prepared.clone(), json!({}));
    push("wallet.swap.finish", "x402FinishSwap", json!({ "prepared": prepared, "signatures": sigs }), finished, json!({}));

    // SW3: token A to token B through a bid and an ask, KAS funding for the carriers
    let sw3 = json!({
        "offer": swap_tok_offer, "quote": quote3, "requestHash": RH, "tokenUtxos": [token_json(&payer_tok)], "funding": [payer_utxo(&funding1)],
        "secretKeys": [to_hex(&sk(PAYER))], "nowMs": NOW_MS.to_string(), "options": { "paymentIdentifier": "pay-swap3-golden-0001" },
    });
    let (rq, sw3_pay) = call(x402::pay_swap, "pay swap sw3", sw3);
    push("pay.swap.sw3", "x402PaySwap", rq, sw3_pay.clone(), json!({ "signatureHeader": header_of(&sw3_pay) }));
    let (rq, ok) = call(
        x402::preflight,
        "preflight sw3",
        json!({ "offer": swap_tok_offer, "payload": sw3_pay["paymentPayload"], "requestHash": RH, "nowMs": NOW_MS.to_string() }),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    push("preflight.swap.sw3", "x402Preflight", rq, ok, json!({}));
    let (rq, revoked) =
        call(x402::revoke, "revoke sw3", json!({ "payload": sw3_pay["paymentPayload"], "secretKeys": [to_hex(&sk(PAYER))] }));
    push("revoke.swap.sw3", "x402Revoke", rq, revoked, json!({}));

    // ------------------------------------------------------------- KRON and KaspaCom-template tokens
    // offers: a KRON pay asset (no extension commitment: all zero on the wire), a KaspaCom-template merchant token
    let swap_kron_req = json!({
        "network": "kaspa:testnet-10", "amount": KAS.to_string(), "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kas", "payAssets": [token_spec_on(KRON_COV, KRON, "KRN")],
    });
    let (rq, swap_kron_offer) = call(x402::swap_requirements, "swap kas offer, KRON pay asset", swap_kron_req);
    assert_eq!(swap_kron_offer["extra"]["route"]["payAssets"][0]["extensionCommitment"], to_hex(&[0u8; 32]));
    push("offer.swap.kas.kron", "x402SwapRequirements", rq, swap_kron_offer.clone(), json!({}));
    let swap_kron_tok_req = json!({
        "network": "kaspa:testnet-10", "amount": (2 * WHOLE).to_string(), "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kcc20", "carrier": OFFER_CARRIER.to_string(), "token": token_spec(TOKEN_B, "BBB"),
        "payAssets": [token_spec_on(KRON_COV, KRON, "KRN"), token_spec(TOKEN_COV, "AAA")],
    });
    let (rq, swap_kron_tok_offer) = call(x402::swap_requirements, "swap token offer, KRON pay asset", swap_kron_tok_req);
    push("offer.swap.token.kron", "x402SwapRequirements", rq, swap_kron_tok_offer.clone(), json!({}));
    let swap_kc_req = json!({
        "network": "kaspa:testnet-10", "amount": (2 * WHOLE).to_string(), "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kcc20", "carrier": OFFER_CARRIER.to_string(), "token": token_spec_on(KC_COV, KC, "KCM"),
        "payAssets": [token_spec_on(KRON_COV, KRON, "KRN"), token_spec(TOKEN_COV, "AAA")],
    });
    let (rq, swap_kc_offer) = call(x402::swap_requirements, "swap token offer, KaspaCom-template merchant token", swap_kc_req);
    assert_eq!(swap_kc_offer["extra"]["token"]["templateHash"], to_hex(&token_template(KC).hash));
    push("offer.swap.token.kaspacom", "x402SwapRequirements", rq, swap_kc_offer, json!({}));
    // a KRON token is a pay asset, never the merchant kcc20 asset
    let kron_merchant = json!({
        "network": "kaspa:testnet-10", "amount": "5", "payTo": address(MERCHANT), "maxTimeoutSeconds": 600, "finality": "accepted",
        "receive": "kcc20", "token": token_spec_on(KRON_COV, KRON, "KRN"), "payAssets": [token_spec(TOKEN_COV, "AAA")],
    });
    assert!(x402::swap_requirements(&kron_merchant.to_string()).unwrap_err().contains("token_not_allowlisted"));

    // payments: KRON sold into a KobBidKron bid (address presence: the payer KAS UTXO authorises the token)
    let kron_bid = {
        let bs = BidState { token_cov_id: KRON_COV, ..common::bid(common::MAKER_B, P245, KRON) };
        common::order(30, (bs.used(5 * WHOLE).expect("budget") + common::DC) as u64, common::cov(0xb5), 1_000, bs)
    };
    let payer_kron = TokenUtxo {
        utxo: common::utxo(31, CARRIER, 1_000, Some(KRON_COV)),
        state: TokenState::user(Family::Kron, 3 * WHOLE, pk(PAYER), [0; 32]),
    };
    let quote_k1 = Quote { lock_time: NOW, orders: vec![OrderRef::bid(kron_bid.clone(), 3 * WHOLE)] };
    let quote_k3 = Quote {
        lock_time: NOW,
        orders: vec![OrderRef::bid(kron_bid.clone(), 3 * WHOLE), OrderRef::ask(ask1.0.clone(), ask1.1.clone(), 2 * WHOLE)],
    };
    // SW1-KRON: the merchant receives KAS
    let swk1 = json!({
        "offer": swap_kron_offer, "quote": quote_k1, "requestHash": RH, "tokenUtxos": [token_json(&payer_kron)], "funding": [payer_utxo(&funding1)],
        "secretKeys": [to_hex(&sk(PAYER))], "nowMs": NOW_MS.to_string(), "options": { "paymentIdentifier": "pay-swapk1-golden-0001" },
    });
    let (rq, swk1_pay) = call(x402::pay_swap, "pay swap KRON -> KAS", swk1);
    push("pay.swap.kron.sw1", "x402PaySwap", rq, swk1_pay.clone(), json!({ "signatureHeader": header_of(&swk1_pay) }));
    let (rq, ok) = call(
        x402::preflight,
        "preflight KRON -> KAS",
        // KRON carries a mint authority: issuer-controlled tokens need an explicit opt-in
        json!({ "offer": swap_kron_offer, "payload": swk1_pay["paymentPayload"], "requestHash": RH, "nowMs": NOW_MS.to_string(), "maxPay": (3 * WHOLE).to_string(), "allowIssuerControlled": true }),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["payerSpent"], (3 * WHOLE).to_string());
    push("preflight.swap.kron.sw1", "x402Preflight", rq, ok, json!({}));
    let (rq, revoked) =
        call(x402::revoke, "revoke KRON -> KAS", json!({ "payload": swk1_pay["paymentPayload"], "secretKeys": [to_hex(&sk(PAYER))] }));
    push("revoke.swap.kron.sw1", "x402Revoke", rq, revoked, json!({}));
    // the wallet flow: only the payer KAS input signs (the KRON token needs no signature of its own)
    let swk1_wallet = json!({
        "offer": swap_kron_offer, "quote": quote_k1, "requestHash": RH, "tokenUtxos": [token_json(&payer_kron)], "funding": [payer_utxo(&funding1)],
        "payerPublicKey": to_hex(&pk(PAYER)), "nowMs": NOW_MS.to_string(), "options": { "paymentIdentifier": "pay-swapk1-golden-0001" },
    });
    let (rq, prepared) = call(x402::prepare_swap, "prepare swap KRON -> KAS", swk1_wallet);
    assert_eq!(prepared["built"]["sign"].as_array().unwrap().len(), 1);
    let sigs = sigs_for(&prepared["built"]);
    let finished: Value = serde_json::from_str(&x402::finish_swap(&prepared.to_string(), &sigs.to_string()).unwrap()).unwrap();
    assert_eq!(finished["paymentPayload"]["payload"]["transaction"], swk1_pay["paymentPayload"]["payload"]["transaction"]);
    push("wallet.swap.kron.prepare", "x402PrepareSwap", rq, prepared.clone(), json!({}));
    push("wallet.swap.kron.finish", "x402FinishSwap", json!({ "prepared": prepared, "signatures": sigs }), finished, json!({}));
    // SW3-KRON: KRON sold into a KRON bid, the merchant receives a KCC-20 token bought from a KCC-20 ask: one cross-family transaction
    let swk3 = json!({
        "offer": swap_kron_tok_offer, "quote": quote_k3, "requestHash": RH, "tokenUtxos": [token_json(&payer_kron)], "funding": [payer_utxo(&funding1)],
        "secretKeys": [to_hex(&sk(PAYER))], "nowMs": NOW_MS.to_string(), "options": { "paymentIdentifier": "pay-swapk3-golden-0001" },
    });
    let (rq, swk3_pay) = call(x402::pay_swap, "pay swap KRON -> KCC-20 token", swk3);
    push("pay.swap.kron.sw3", "x402PaySwap", rq, swk3_pay.clone(), json!({ "signatureHeader": header_of(&swk3_pay) }));
    let (rq, ok) = call(
        x402::preflight,
        "preflight KRON -> KCC-20 token",
        json!({ "offer": swap_kron_tok_offer, "payload": swk3_pay["paymentPayload"], "requestHash": RH, "nowMs": NOW_MS.to_string(), "allowIssuerControlled": true }),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    push("preflight.swap.kron.sw3", "x402Preflight", rq, ok, json!({}));

    json!({
        "kind": "kob-x402-wasm-golden",
        "version": 1,
        "description": "Requests and exact results of the kob-wasm x402 bindings (deterministic keys and fixtures; Schnorr signing without aux randomness). Regenerate with KOB_REGEN=1 cargo test -p kob-wasm --test x402_golden.",
        "cases": cases,
    })
}

#[test]
fn x402_golden_vectors_are_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kob-x402/vectors/golden/x402-payments.json");
    let fresh = generate();
    let text = serde_json::to_string_pretty(&fresh).unwrap() + "\n";
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let committed: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("golden file missing: run with KOB_REGEN=1")).unwrap();
    assert_eq!(committed, fresh, "x402 golden vectors drifted: review, then regenerate with KOB_REGEN=1");
}
