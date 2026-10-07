//! Conformance against the elldeeone/kaspa-x402 v1.0.0-rc.1 interoperability vectors, reproduced from
//! Rust only (no TypeScript). The vectors are vendored unmodified under
//! `vectors/kaspa-x402-rc1/` (see its `PROVENANCE.md`).
//!
//! Scope. KOB implements the `standard-native` profile (and its own `kcc20` / swap extensions), not the
//! KIP-10 `additive` profile. The vectors are used where they exercise profile-independent code:
//!
//! * `exact/interop-v1.json`: both transaction-id derivations (the additive artifact is the only
//!   published version-1 id vector, so it also proves that the pinned rusty-kaspa v2.1.0 hashes the
//!   same way as the binding's), the payment-requirements canonical JSON and hash, the request
//!   authorization digest and Schnorr signature, every expiry case that concerns standard-native, and
//!   the finality ordering. The two additive-only expiry cases (`authorization_exceeds_challenge`,
//!   `expired_challenge`) are not implemented and are skipped explicitly.
//! * `x402-http/exact-transaction.json`: header codecs (byte-identical re-encoding).
//! * `exact/consensus-profiles.json`: the standard-native transaction (masses, fee, script units, ids)
//!   and its mutation cases for the standard-native profile. The additive mutations
//!   (`additive*`) are out of scope: KOB does not implement additive.
//! * `negative/*.json` and `settlement-response/*.json`: the semantic negatives that concern the exact
//!   envelope; each is mapped to our public `Reason` plus local `Diag` below. Vectors about batch
//!   settlement or additive are not applicable and are named where skipped.
//!
//! One finding is documented in `consensus_profile_standard_native`: the vector's fee (200000 sompi) is
//! consensus-valid but below the relay floor of a rusty-kaspa v2.1.0 node, 100 sompi per gram of
//! `max(compute, normalized transient)` = 203600 sompi (storage mass is not part of the node's floor), so
//! the mutation cases run on a floor-compliant rebuild of the same geometry (same keys, input, amount and
//! outputs).

mod exact_support;

use exact_support::*;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kob_x402::canonical::{canonical_json, requirements_hash, sha256};
use kob_x402::chain::{ChainUtxo, Outpoint};
use kob_x402::common::{
    check_economics_and_scripts, check_expiry, parse_iso_ms, payment_identifier, signed_auth_digest, signed_auth_object, SignedAuth,
};
use kob_x402::error::{Diag, Reason, X402Error};
use kob_x402::exact::{spec_txid_v0, spec_txid_v0_preimage, spec_txid_v1, verify_schnorr_digest};
use kob_x402::safe_tx::{spk_from_hex, SafeTx};
use kob_x402::verify::{verify_payment, Verified};
use kob_x402::wire::{
    hex, parse_hash32, ExactPayload, Finality, Network, PaymentPayload, PaymentRequired, PaymentRequirements, Profile,
    SettlementResponse,
};
use serde_json::{json, Value};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROVENANCE: &str = include_str!("../vectors/kaspa-x402-rc1/PROVENANCE.md");
const INTEROP: &str = include_str!("../vectors/kaspa-x402-rc1/exact/interop-v1.json");
const PROFILES: &str = include_str!("../vectors/kaspa-x402-rc1/exact/consensus-profiles.json");
const HTTP: &str = include_str!("../vectors/kaspa-x402-rc1/x402-http/exact-transaction.json");

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).expect("vector is JSON")
}

fn negative(name: &str) -> Value {
    let path = format!("{ROOT}/vectors/kaspa-x402-rc1/negative/{name}.json");
    json_of(&std::fs::read_to_string(path).expect("vendored negative vector"))
}

fn bytes(s: &str) -> Vec<u8> {
    kob_protocol::json::from_hex(s).expect("hex")
}

fn hash(s: &str) -> [u8; 32] {
    parse_hash32(s).expect("32-byte hex")
}

fn u64_of(v: &Value) -> u64 {
    v.as_str().expect("decimal string").parse().expect("u64")
}

// ------------------------------------------------------------------------------------------ provenance

#[test]
fn vendored_files_are_unmodified_copies() {
    // every listed file hashes to the PROVENANCE value, and every vendored file is listed
    let block = PROVENANCE.split("```text").nth(1).unwrap().split("```").next().unwrap();
    let mut listed = Vec::new();
    for line in block.lines().filter(|l| !l.trim().is_empty()) {
        let (digest, path) = line.split_once("  ").expect("`hash  path`");
        let data = std::fs::read(format!("{ROOT}/vectors/kaspa-x402-rc1/{path}")).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(hex(&sha256(&data)), digest, "{path} differs from the vendored source");
        listed.push(path.to_string());
    }
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let base = std::path::PathBuf::from(format!("{ROOT}/vectors/kaspa-x402-rc1"));
    let mut found = Vec::new();
    walk(&base, &base, &mut found);
    found.retain(|f| f != "PROVENANCE.md" && f != ".gitattributes");
    found.sort();
    listed.sort();
    assert_eq!(found, listed, "vendored files and PROVENANCE.md list must agree");
    for needle in
        ["https://github.com/elldeeone/kaspa-x402", "v1.0.0-rc.1", "040b1ec8335abadbb3c69cf1ea720ae45816b0f7", "MIT", "2026-09-29"]
    {
        assert!(PROVENANCE.contains(needle), "PROVENANCE.md lacks {needle}");
    }
}

// ----------------------------------------------------------------------------------- (a) transaction ids

fn consensus_tx(artifact: &Value) -> (Transaction, Vec<UtxoEntry>) {
    // the artifact carries `id`: the decoder fails unless it equals the id recomputed by consensus-core
    let parsed = SafeTx::parse(&artifact.to_string(), 1 << 20).expect("safe projection").to_consensus().expect("consensus tx");
    let entries = parsed
        .hints
        .iter()
        .map(|h| {
            let h = h.as_ref().expect("utxo hint");
            UtxoEntry::new(h.amount, h.script_public_key.clone(), 0, false, None)
        })
        .collect();
    (parsed.tx, entries)
}

#[test]
fn transaction_ids_from_spec_preimages_equal_consensus() {
    let v = json_of(INTEROP);
    let profiles = &v["transactionEncoding"]["profiles"];

    // standard-native, version 0: BLAKE2b-256 keyed "TransactionID"
    let sn = &profiles["standardNative"];
    let (tx, _) = consensus_tx(&sn["artifact"]);
    assert_eq!(tx.version, 0);
    assert_eq!(sn["txid"]["algorithm"], "blake2b-256-keyed");
    assert_eq!(sn["txid"]["domain"], "TransactionID");
    let want_pre = bytes(sn["txid"]["preimage"].as_str().unwrap());
    assert_eq!(spec_txid_v0_preimage(&tx), want_pre, "independent preimage");
    assert_eq!(kaspa_consensus_core::hashing::tx::transaction_v0_id_preimage(&tx), want_pre, "consensus-core preimage");
    let want = hash(sn["txid"]["digest"].as_str().unwrap());
    assert_eq!(spec_txid_v0(&tx), want);
    assert_eq!(tx.id().as_bytes(), want);
    assert_eq!(hex(&want), sn["transactionId"].as_str().unwrap());
    // the id excludes signature scripts, sigop counts and the storage mass
    let mut alt = tx.clone();
    alt.inputs[0].signature_script.clear();
    alt.set_storage_mass(1);
    alt.finalize();
    assert_eq!(spec_txid_v0(&alt), want);
    assert_eq!(alt.id().as_bytes(), want);

    // additive, version 1: keyed BLAKE3 with zero-padded domain keys (proves the pinned rusty-kaspa agrees)
    let ad = &profiles["additive"];
    let (tx, _) = consensus_tx(&ad["artifact"]);
    assert_eq!(tx.version, 1);
    let t = &ad["txid"];
    assert_eq!(t["algorithm"], "blake3-256-keyed");
    let s = spec_txid_v1(&tx);
    assert_eq!(hex(&s.payload_digest), t["payloadDigest"].as_str().unwrap());
    assert_eq!(hex(&s.rest_preimage), t["restPreimage"].as_str().unwrap());
    assert_eq!(hex(&s.rest_digest), t["restDigest"].as_str().unwrap());
    assert_eq!(hex(&s.preimage), t["preimage"].as_str().unwrap());
    assert_eq!(hex(&s.id), t["digest"].as_str().unwrap());
    assert_eq!(s.id, tx.id().as_bytes(), "pinned rusty-kaspa v2.1.0 computes the binding's v1 id");
    assert_eq!(kaspa_consensus_core::hashing::tx::transaction_v1_rest_preimage(&tx), s.rest_preimage);
    assert_eq!(kaspa_consensus_core::hashing::tx::v1_rest_digest(&tx).as_bytes(), s.rest_digest);
    // a stated artifact id that is wrong is rejected by the decoder
    let mut bad = ad["artifact"].clone();
    bad["id"] = json!("00".repeat(32));
    assert_eq!(
        SafeTx::parse(&bad.to_string(), 1 << 20).unwrap().to_consensus().err().unwrap().diag,
        Diag::InvalidKaspaExactTransactionId
    );
}

// ----------------------------------------------------------------- (b) requirements canonical JSON and hash

#[test]
fn payment_requirements_canonical_json_and_hash() {
    let v = json_of(INTEROP);
    let pr = &v["paymentRequirements"];
    let req: PaymentRequirements = serde_json::from_value(pr["value"].clone()).unwrap();
    // nothing is lost by the typed model (unknown top-level fields and `extra` are kept)
    assert_eq!(serde_json::to_value(&req).unwrap(), pr["value"]);
    let text = canonical_json(&serde_json::to_value(&req).unwrap()).unwrap();
    assert_eq!(text, pr["canonicalJsonUtf8"].as_str().unwrap());
    let want = pr["sha256"].as_str().unwrap();
    assert_eq!(hex(&sha256(text.as_bytes())), want);
    assert_eq!(hex(&requirements_hash(&req).unwrap()), want);
    // key order of the source object is irrelevant; a changed value changes the hash
    let mut swapped = req.clone();
    swapped.amount = "20000001".into();
    assert_ne!(hex(&requirements_hash(&swapped).unwrap()), want);
}

// --------------------------------------------------------- (c) request authorization digest and signature

#[test]
fn request_authorization_digest_and_signature() {
    let v = json_of(INTEROP);
    let a = &v["requestAuthorization"];
    let i = &a["input"];
    let txid = hash(i["transactionId"].as_str().unwrap());
    let reqh = hash(i["paymentRequirementsHash"].as_str().unwrap());
    let rh = hash(i["requestHash"].as_str().unwrap());
    let sa = SignedAuth {
        network: Network::parse(i["network"].as_str().unwrap()).unwrap(),
        profile: Profile::parse(i["profile"].as_str().unwrap()).unwrap(),
        transaction_id: &txid,
        payment_output_index: i["paymentOutputIndex"].as_u64().unwrap() as u32,
        amount: i["amount"].as_str().unwrap(),
        pay_to: i["payTo"].as_str().unwrap(),
        pay_to_spk_hex: i["payToScriptPublicKey"].as_str().unwrap(),
        requirements_hash: &reqh,
        request_hash: &rh,
        challenge_id: i["challengeId"].as_str(),
        input_index: i["inputIndex"].as_u64().unwrap() as u32,
        expires_at: i["expiresAt"].as_str().unwrap(),
    };
    let text = canonical_json(&signed_auth_object(&sa)).unwrap();
    assert_eq!(text, a["canonicalJsonUtf8"].as_str().unwrap());
    let digest = signed_auth_digest(&sa).unwrap();
    assert_eq!(hex(&digest), a["sha256"].as_str().unwrap());
    let signer = hash(a["signerPublicKey"].as_str().unwrap());
    let sig = bytes(a["signature"].as_str().unwrap());
    assert_eq!(a["expected"], "valid-schnorr-signature");
    assert!(verify_schnorr_digest(&sig, &digest, &signer), "the vector's BIP340 signature verifies");
    // and only for this digest / key
    let mut other = digest;
    other[0] ^= 1;
    assert!(!verify_schnorr_digest(&sig, &other, &signer));
    assert!(!verify_schnorr_digest(&sig, &digest, &pubkey_of(1)));
    // every bound field changes the digest (removing or altering any of them invalidates the payment)
    type Tweak = Box<dyn Fn(&mut SignedAuth)>;
    let variants: Vec<Tweak> = vec![
        Box::new(|s| s.payment_output_index = 1),
        Box::new(|s| s.amount = "20000001"),
        Box::new(|s| s.pay_to = "kaspatest:other"),
        Box::new(|s| s.pay_to_spk_hex = "0000"),
        Box::new(|s| s.input_index = 0),
        Box::new(|s| s.expires_at = "2099-01-01T00:00:00.001Z"),
        Box::new(|s| s.challenge_id = None),
        Box::new(|s| s.profile = Profile::StandardNative),
        Box::new(|s| s.network = Network::Mainnet),
    ];
    for (n, f) in variants.iter().enumerate() {
        let mut s = sa_copy(&sa);
        f(&mut s);
        assert_ne!(signed_auth_digest(&s).unwrap(), digest, "variant {n} left the digest unchanged");
    }
    let mut s = sa_copy(&sa);
    let other_tx = [0u8; 32];
    s.transaction_id = &other_tx;
    assert_ne!(signed_auth_digest(&s).unwrap(), digest);
    let other_rh = [1u8; 32];
    s.transaction_id = &txid;
    s.request_hash = &other_rh;
    assert_ne!(signed_auth_digest(&s).unwrap(), digest);
    s.request_hash = &rh;
    s.requirements_hash = &other_rh;
    assert_ne!(signed_auth_digest(&s).unwrap(), digest);
}

fn sa_copy<'a>(s: &SignedAuth<'a>) -> SignedAuth<'a> {
    SignedAuth {
        network: s.network,
        profile: s.profile,
        transaction_id: s.transaction_id,
        payment_output_index: s.payment_output_index,
        amount: s.amount,
        pay_to: s.pay_to,
        pay_to_spk_hex: s.pay_to_spk_hex,
        requirements_hash: s.requirements_hash,
        request_hash: s.request_hash,
        challenge_id: s.challenge_id,
        input_index: s.input_index,
        expires_at: s.expires_at,
    }
}

fn pubkey_of(n: u8) -> [u8; 32] {
    kob_x402::testkit::pubkey(n)
}

// ------------------------------------------------------------------------ (d) expiry and finality ordering

#[test]
fn expiry_cases() {
    let v = json_of(INTEROP);
    let e = &v["expiry"];
    let now = parse_iso_ms(e["referenceTime"].as_str().unwrap()).unwrap();
    let max = e["maxTimeoutSeconds"].as_u64().unwrap();
    let mut checked = 0;
    for c in e["cases"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let expected = c["expected"].as_str().unwrap();
        if c["profile"] == "additive" && c.get("challengeExpiresAt").is_some() && expected != "valid" {
            // `authorization_exceeds_challenge` / `expired_challenge` belong to the additive challenge, not implemented
            assert!(matches!(expected, "authorization_exceeds_challenge" | "expired_challenge"), "{name}");
            continue;
        }
        let got = check_expiry(now, max, c["authorizationExpiresAt"].as_str().unwrap());
        match (expected, got) {
            ("valid", Ok(ms)) => assert_eq!(Some(ms), parse_iso_ms(c["authorizationExpiresAt"].as_str().unwrap()), "{name}"),
            ("expired_authorization", Err(err)) => assert_eq!(err.diag, Diag::ExpiredAuthorization, "{name}"),
            ("authorization_exceeds_max_timeout", Err(err)) => assert_eq!(err.diag, Diag::AuthorizationExceedsMaxTimeout, "{name}"),
            (other, res) => panic!("{name}: expected {other}, got {res:?}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 4, "standard-valid, standard-expired, standard-beyond-timeout, additive-valid-at-challenge");
    // the wire mapping of the two decisions
    let err = check_expiry(now, max, "2098-12-31T23:59:00.000Z").unwrap_err();
    assert_eq!((err.reason, err.diag), (Reason::InvalidTransactionState, Diag::ExpiredAuthorization));
    let err = check_expiry(now, max, "2099-01-01T00:00:00.001Z").unwrap_err();
    assert_eq!((err.reason, err.diag), (Reason::InvalidPayload, Diag::AuthorizationExceedsMaxTimeout));
}

#[test]
fn finality_ordering_cases() {
    let v = json_of(INTEROP);
    let f = &v["finality"];
    let order: Vec<Finality> =
        f["ordering"].as_array().unwrap().iter().map(|s| Finality::parse(s.as_str().unwrap()).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "mempool < accepted < confirmed");
    for c in f["cases"].as_array().unwrap() {
        let actual = Finality::parse(c["actual"].as_str().unwrap()).unwrap();
        let required = Finality::parse(c["required"].as_str().unwrap()).unwrap();
        assert_eq!(actual.satisfies(required), c["expected"].as_bool().unwrap(), "{c}");
    }
}

// ----------------------------------------------------------------------------------- (e) HTTP header codecs

#[test]
fn http_headers_decode_and_reencode_byte_identically() {
    use kob_x402::wire::{header_decode, header_encode};
    let v = json_of(HTTP);
    let h = &v["headers"];

    let pr: PaymentRequired = header_decode(h["paymentRequired"].as_str().unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&pr).unwrap(), v["paymentRequired"]);
    assert_eq!(header_encode(&pr).unwrap(), h["paymentRequired"].as_str().unwrap());

    let pp: PaymentPayload = header_decode(h["paymentSignature"].as_str().unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&pp).unwrap(), v["paymentPayload"]);
    assert_eq!(header_encode(&pp).unwrap(), h["paymentSignature"].as_str().unwrap());
    // the payload's transaction is a JSON text (the safe projection), not a nested object
    assert!(pp.payload.transaction.starts_with('{'));
    SafeTx::parse(&pp.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap();

    let sr: SettlementResponse = header_decode(h["paymentResponse"].as_str().unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&sr).unwrap(), v["settlementResponse"]);
    assert_eq!(header_encode(&sr).unwrap(), h["paymentResponse"].as_str().unwrap());

    // deterministic key order: encoding does not depend on how the value was built
    let reordered: Value = serde_json::from_str(&serde_json::to_string(&v["paymentRequired"]).unwrap()).unwrap();
    assert_eq!(header_encode(&reordered).unwrap(), h["paymentRequired"].as_str().unwrap());
    // garbage headers fail closed
    assert!(header_decode::<PaymentPayload>("!!!not base64!!!").is_err());
    assert!(header_decode::<PaymentPayload>("e30=").is_err());
}

// ------------------------------------------------------------------------ (f) consensus profile: standard-native

fn profile_tx(t: &Value) -> (Transaction, Vec<UtxoEntry>) {
    let mut inputs = vec![];
    let mut entries = vec![];
    for i in t["inputs"].as_array().unwrap() {
        let op = TransactionOutpoint::new(
            kaspa_consensus_core::tx::TransactionId::from_bytes(hash(i["previousOutpoint"]["txid"].as_str().unwrap())),
            i["previousOutpoint"]["index"].as_u64().unwrap() as u32,
        );
        let sigop = i["sigOpCount"].as_u64().expect("standard-native inputs commit a sigop count") as u8;
        inputs.push(TransactionInput::new(op, bytes(i["signatureScript"].as_str().unwrap()), u64_of(&i["sequence"]), sigop));
        let u = &i["utxo"];
        entries.push(UtxoEntry::new(
            u64_of(&u["amount"]),
            spk_from_hex(u["scriptPublicKey"].as_str().unwrap()).unwrap(),
            u64_of(&u["blockDaaScore"]),
            u["isCoinbase"].as_bool().unwrap(),
            None,
        ));
    }
    let outputs = t["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            assert!(o["covenant"].is_null());
            TransactionOutput::new(u64_of(&o["amount"]), spk_from_hex(o["scriptPublicKey"].as_str().unwrap()).unwrap())
        })
        .collect();
    let tx = Transaction::new(
        t["version"].as_u64().unwrap() as u16,
        inputs,
        outputs,
        u64_of(&t["lockTime"]),
        SUBNETWORK_ID_NATIVE,
        u64_of(&t["gas"]),
        bytes(t["payload"].as_str().unwrap()),
    );
    tx.set_storage_mass(u64_of(&t["storageMass"]));
    (tx, entries)
}

#[test]
fn consensus_profile_standard_native() {
    let v = json_of(PROFILES);
    let sn = &v["expected"]["standardNative"];
    let (tx, entries) = profile_tx(&sn["transaction"]);

    // identifiers
    assert_eq!(hex(&tx.id().as_bytes()), sn["transactionId"].as_str().unwrap());
    assert_eq!(spec_txid_v0(&tx), tx.id().as_bytes());
    assert_eq!(hex(&kaspa_consensus_core::hashing::tx::hash(&tx).as_bytes()), sn["transactionHash"].as_str().unwrap());
    let interop = json_of(INTEROP);
    assert_eq!(sn["transactionId"], interop["transactionEncoding"]["profiles"]["standardNative"]["transactionId"]);

    // masses, fee, script units
    let m = kob_protocol::tx::masses(&tx, &entries);
    assert_eq!(m.storage, u64_of(&sn["storageMass"]));
    assert_eq!(m.storage, 63557);
    assert_eq!(m.compute, u64_of(&sn["computeMass"]));
    assert_eq!(m.transient, u64_of(&sn["transientMass"]));
    assert_eq!(m.size, sn["estimatedSerializedSize"].as_u64().unwrap());
    let total_in: u64 = entries.iter().map(|e| e.amount).sum();
    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
    assert_eq!(total_in - total_out, u64_of(&sn["fee"]));
    assert_eq!(total_in - total_out, 200_000);
    assert_eq!(tx.inputs.len() as u64, sn["inputs"].as_u64().unwrap());
    assert_eq!(tx.outputs.len() as u64, sn["outputs"].as_u64().unwrap());
    assert_eq!(tx.outputs[0].value, u64_of(&sn["amount"]));
    assert_eq!(tx.inputs[0].compute_commit.sig_op_count(), Some(sn["computeCommitments"][0]["sigOpCount"].as_u64().unwrap() as u8));
    let outcomes = kob_protocol::verify::execute(&tx, &entries, true).unwrap();
    let units: Vec<u64> = outcomes.into_iter().map(|o| o.expect("the payer signature passes the script engine")).collect();
    assert_eq!(units, sn["scriptUnits"].as_array().unwrap().iter().map(|u| u.as_u64().unwrap()).collect::<Vec<_>>());

    // engine validation through our verifier. The vector is consensus-valid, but its fee (200000) is below
    // the relay floor of a rusty-kaspa v2.1.0 node: 100 sompi per gram of max(compute 2036, 2 x size)
    // = 203600 (storage mass, 63557 here, is not part of the node's floor). check_economics_and_scripts
    // accepts exactly what the node relays, so it refuses the vector (the live TN10 probe confirms that
    // the node does too); the vector stays vendored as published.
    let env = Env::new();
    let op = &tx.inputs[0].previous_outpoint;
    env.chain.insert_utxo(
        Outpoint::new(op.transaction_id.as_bytes(), op.index),
        ChainUtxo {
            amount: entries[0].amount,
            script_public_key: entries[0].script_public_key.clone(),
            block_daa_score: 0,
            is_coinbase: false,
            covenant_id: None,
        },
    );
    let floor = kob_protocol::tx::min_fee(&m, kob_protocol::tx::MIN_FEE_RATE);
    assert_eq!((m.compute, m.fee_mass, floor), (2_036, 2_036, 203_600), "the node's relay floor of the vector");
    assert!(m.storage > m.fee_mass, "the storage mass would dominate a storage-inclusive fee");
    let err = check_economics_and_scripts(&env.ctx(), &tx, &entries).unwrap_err();
    assert_eq!((err.reason, err.diag), (Reason::InvalidPayload, Diag::InvalidKaspaExactFee), "{err}");
    // with the floor relaxed to the vector's own economics the transaction passes the whole engine path
    let fee = kob_protocol::verify::validate(&tx, &entries).err().map(|e| e.to_string());
    assert!(fee.as_deref().is_some_and(|m| m.contains("below the floor")), "only the fee floor stops it: {fee:?}");
    kob_protocol::verify::execute(&tx, &entries, true).unwrap();
}

#[test]
fn consensus_profile_mutations_standard_native() {
    let v = json_of(PROFILES);
    let muts = &v["expected"]["mutations"];
    assert_eq!(muts["standardBadSignature"], "consensus-rejected");
    assert_eq!(muts["standardGas"], "consensus-rejected");
    assert_eq!(muts["standardWrongMass"], "consensus-rejected");
    assert_eq!(muts["standardWrongVersion"], "consensus-rejected");
    assert_eq!(muts["forgedUtxoAmount"], "consensus-rejected");
    assert_eq!(muts["standardMerchantOverpayment"], "profile-rejected");
    assert_eq!(muts["standardExcessiveFee"], "profile-rejected");
    assert_eq!(muts["standardDuplicateMerchantOutput"], "profile-rejected-after-consensus-acceptance");
    assert_eq!(muts["standardPayload"], "profile-rejected-after-consensus-acceptance");
    // additive mutations (additiveBadSignature, additiveBelowThreshold, additiveDuplicateMerchantBenefit,
    // additiveExcessiveDelta, additiveOverbudget, additiveUnderbudget, additiveWrongMass, additiveWrongScript,
    // additiveWrongVersion) belong to the KIP-10 additive profile, which KOB does not implement.

    // Same geometry as the vector (payer key 7, one 50_000_000 input, 20_000_000 to merchant key 8, change to
    // the payer), rebuilt at a fee margin above the KOB floor so each mutation is judged on its own rule.
    let fx = fixture_with(&[50_000_000], 150);
    let control = fx.verify().expect("floor-compliant control verifies");
    assert_eq!(control.tx.outputs.len(), 2);
    assert_eq!(control.tx.outputs[0].value, 20_000_000);
    let (tx0, entries0) = fx.tx();
    let ctx = fx.env.ctx();
    let engine = |tx: &Transaction, entries: &[UtxoEntry]| check_economics_and_scripts(&ctx, tx, entries);
    engine(&tx0, &entries0).expect("control passes the engine path");
    let diag = |r: Result<Verified, X402Error>| r.err().map(|e| (e.reason, e.diag));

    // standardBadSignature: consensus-rejected (engine), and so by the verifier
    let (p, tx, entries) = mutate(&fx, &Repack { resign: false, reauthorize: false, ..Repack::default() }, |tx, _| {
        tx.inputs[0].signature_script[1] ^= 0x01
    });
    assert_eq!(engine(&tx, &entries).unwrap_err().diag, Diag::InvalidKaspaExactSignature);
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactSignature)));

    // standardGas: consensus-rejected (the un-re-signed transaction fails the engine; consensus isolation also forbids gas)
    let (p, tx, entries) = mutate(&fx, &Repack { resign: false, reauthorize: false, ..Repack::default() }, |tx, _| tx.gas = 1);
    assert!(engine(&tx, &entries).is_err());
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction)));
    // ... and re-signed gas: the engine alone would accept, the profile rejects
    let (p, tx, entries) = mutate(&fx, &Repack::default(), |tx, _| tx.gas = 1);
    assert!(engine(&tx, &entries).is_ok());
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction)));

    // standardWrongMass: consensus-rejected (committed storage mass differs from the computed one)
    let (p, tx, entries) =
        mutate(&fx, &Repack { fix_mass: false, resign: false, reauthorize: false, ..Repack::default() }, |tx, _| {
            tx.set_storage_mass(tx.storage_mass() + 1)
        });
    assert_eq!(engine(&tx, &entries).unwrap_err().diag, Diag::InvalidKaspaExactMass);
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactMass)));

    // standardWrongVersion: version 1 with the version-0 sigop commitment (consensus-rejected)
    // (the consensus mass code even panics on this mix, so the artifact decoder must and does refuse it before any mass or
    // script work: a version-1 input needs a computeBudget and no sigOpCount)
    let (p, _, _) =
        mutate(&fx, &Repack { fix_mass: false, resign: false, reauthorize: false, ..Repack::default() }, |tx, _| tx.version = 1);
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction)));

    // forgedUtxoAmount: consensus-rejected when the entry amount is forged (the sighash commits to it) ...
    let mut forged = entries0.clone();
    forged[0] = UtxoEntry::new(forged[0].amount + 1, forged[0].script_public_key.clone(), 0, false, None);
    assert!(engine(&tx0, &forged).is_err());
    // ... and the verifier never trusts the artifact's hint in the first place
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.inputs[0].utxo.as_mut().unwrap().amount = (entries0[0].amount + 1).to_string();
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactUtxo)));

    // standardMerchantOverpayment: consensus-valid, profile-rejected
    let (p, tx, entries) = mutate(&fx, &Repack::default(), |tx, _| {
        tx.outputs[0].value += 1;
        tx.outputs[1].value -= 1;
    });
    assert!(engine(&tx, &entries).is_ok());
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::Overpayment)));

    // standardExcessiveFee: 1_000_000 more fee (change reduced), rejected against the verifier's fee bound
    let bound = control.fee + 500_000;
    let (p, tx, entries) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[1].value -= 1_000_000);
    assert!(engine(&tx, &entries).is_ok(), "consensus-valid at the default policy");
    let mut tight = fixture_with(&[50_000_000], 150);
    tight.env.policy.limits.max_fee_sompi = bound;
    tight.verify().expect("the control is within the bound");
    assert_eq!(diag(tight.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactFee)));

    // standardDuplicateMerchantOutput: a second merchant output (10_000_000 moved out of the change). A third output raises
    // the storage mass, so this case needs a larger input and fee margin to stay consensus-valid.
    let big = fixture_with(&[100_000_000], 400);
    let bigctx = big.env.ctx();
    let (p, tx, entries) = mutate(&big, &Repack::default(), |tx, _| {
        tx.outputs[1].value -= 10_000_000;
        tx.outputs.push(TransactionOutput::new(10_000_000, spk_of(MERCHANT)));
    });
    check_economics_and_scripts(&bigctx, &tx, &entries).expect("consensus accepts a second merchant output");
    assert_eq!(diag(big.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction)));
    // ... and the two-output flavour (the change paid to the merchant)
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[1].script_public_key = spk_of(MERCHANT));
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput)));

    // standardPayload: a one-byte payload, consensus-valid, profile-rejected
    let (p, tx, entries) = mutate(&fx, &Repack::default(), |tx, _| tx.payload = vec![1]);
    assert!(engine(&tx, &entries).is_ok(), "consensus accepts it");
    assert_eq!(diag(fx.verify_payload(&p)), Some((Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction)));
}

// -------------------------------------------------------------------- (g) semantic and schema negatives

/// A payload for `offered` whose accepted object equals the offer, so the envelope check under test is the
/// first one that can fail. The transaction artifact is a valid one; it is never reached.
fn run_offer(offered: &PaymentRequirements, x402_version: u64) -> Result<Verified, X402Error> {
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.accepted = offered.clone();
    p.x402_version = x402_version;
    verify_payment(&fx.env.ctx(), offered, &p, &fx.request_hash)
}

fn first_offer(negative: &Value) -> (PaymentRequirements, u64) {
    let pr: PaymentRequired = serde_json::from_value(negative["value"].clone()).expect("PaymentRequired");
    (pr.accepts[0].clone(), pr.x402_version)
}

fn expect(r: Result<Verified, X402Error>, reason: Reason, diag: Diag, vector: &str) {
    match r {
        Ok(_) => panic!("{vector}: verified"),
        Err(e) => assert_eq!((e.reason, e.diag), (reason, diag), "{vector}: {e}"),
    }
}

#[test]
fn negative_vectors_of_the_exact_envelope() {
    // vector -> (expectedError of the binding) -> our public reason and local diagnostic.
    // amounts: canonical positive uint64 only
    for (name, err) in [("amount-overflow", "invalid_kaspa_x402_amount"), ("float-amount", "invalid_kaspa_x402_amount")] {
        let n = negative(name);
        assert_eq!(n["expectedError"], err);
        let (o, ver) = first_offer(&n);
        expect(run_offer(&o, ver), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Amount, name);
    }
    // asset: KAS only
    let n = negative("invalid-asset");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_asset");
    let (o, ver) = first_offer(&n);
    assert_eq!(o.asset, "BTC");
    expect(run_offer(&o, ver), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Asset, "invalid-asset");
    // network: colon identifiers only (public reason invalid_network)
    let n = negative("non-colon-network");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_network");
    let (o, ver) = first_offer(&n);
    assert_eq!(o.network, "testnet-10");
    expect(run_offer(&o, ver), Reason::InvalidNetwork, Diag::InvalidKaspaX402Binding, "non-colon-network");
    // scheme: exact only (public reason invalid_scheme)
    let n = negative("wrong-scheme");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_scheme");
    let (o, ver) = first_offer(&n);
    expect(run_offer(&o, ver), Reason::InvalidScheme, Diag::InvalidKaspaX402Binding, "wrong-scheme");
    // x402 version: 2 only
    let n = negative("wrong-x402-version");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_version");
    let (o, ver) = first_offer(&n);
    assert_eq!(ver, 1);
    expect(run_offer(&o, ver), Reason::InvalidX402Version, Diag::InvalidKaspaX402Payload, "wrong-x402-version");
    // binding: the escrow binding cannot ride the exact scheme
    let n = negative("wrong-binding");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_binding");
    let (o, ver) = first_offer(&n);
    expect(run_offer(&o, ver), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Binding, "wrong-binding");
    // additive profile offer (exact-additive-partial-head): not implemented by KOB -> unsupported profile
    let n = negative("exact-additive-partial-head");
    let (o, ver) = first_offer(&n);
    assert_eq!(o.extra_str("profile"), Some("additive"));
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.accepted = o.clone();
    p.x402_version = ver;
    p.payload.profile = "additive".into();
    expect(
        verify_payment(&fx.env.ctx(), &o, &p, &fx.request_hash),
        Reason::UnsupportedScheme,
        Diag::UnsupportedKaspaExactProfile,
        "exact-additive-partial-head",
    );
    // exact-additive-missing-challenge (`challengeId` required by the additive payload schema): additive, not implemented;
    // the same object must not verify as standard-native either.
    let n = negative("exact-additive-missing-challenge");
    let pp: PaymentPayload = serde_json::from_value(n["value"].clone()).unwrap();
    assert_eq!(pp.payload.profile, "additive");
    assert!(pp.payload.challenge_id.is_none());
    expect(
        verify_payment(&fx.env.ctx(), &pp.accepted, &pp, &fx.request_hash),
        Reason::UnsupportedScheme,
        Diag::UnsupportedKaspaExactProfile,
        "exact-additive-missing-challenge",
    );
}

#[test]
fn negative_vectors_accepted_not_offered_and_payment_identifier() {
    // accepted-not-offered: the server's offer differs from the retry's accepted object
    let n = negative("accepted-not-offered");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_accepted");
    let pr: PaymentRequired = serde_json::from_value(n["paymentRequired"].clone()).unwrap();
    let pp: PaymentPayload = serde_json::from_value(n["paymentPayload"].clone()).unwrap();
    assert_ne!(pr.accepts[0].amount, pp.accepted.amount);
    let fx = fixture();
    expect(
        verify_payment(&fx.env.ctx(), &pr.accepts[0], &pp, &pp.payload.request_hash),
        Reason::InvalidPaymentRequirements,
        Diag::InvalidKaspaX402Accepted,
        "accepted-not-offered",
    );

    // missing-payment-identifier: the offer marks the extension required, the retry omits it
    let n = negative("missing-payment-identifier");
    assert_eq!(n["expectedError"], "missing_kaspa_payment_identifier");
    assert_eq!(n["paymentRequired"]["extensions"]["payment-identifier"]["info"]["required"], true);
    let pp: PaymentPayload = serde_json::from_value(n["paymentPayload"].clone()).unwrap();
    assert!(pp.extensions.is_none());
    let e = payment_identifier(&fx.env.ctx(), &pp).unwrap_err();
    assert_eq!((e.reason, e.diag), (Reason::InvalidPayload, Diag::MissingKaspaPaymentIdentifier));
    // ... end to end on a real payment with the extension removed
    let mut p = fx.payload.clone();
    p.extensions = None;
    expect(fx.verify_payload(&p), Reason::InvalidPayload, Diag::MissingKaspaPaymentIdentifier, "missing-payment-identifier");

    // payment-identifier-conflict: same id, different request fingerprint. The verifier only validates and extracts the
    // id; binding it to one requestHash is the facilitator ledger's job (Diag::KaspaPaymentIdentifierConflict, tested in
    // kob-executor). What the SDK guarantees is that every new payment takes a fresh random id (one payment keeps its id
    // only by re-sending the same payload), so a payer never collides on its own and nobody else can name its id.
    let n = negative("payment-identifier-conflict");
    assert_eq!(n["expectedError"], "kaspa_payment_identifier_conflict");
    let (first, second) = (&n["first"], &n["second"]);
    assert_eq!(first["extensionInfo"]["id"], second["extensionInfo"]["id"]);
    assert_ne!(first["requestHash"], second["requestHash"]);
    let id = first["extensionInfo"]["id"].as_str().unwrap();
    let mut p = fx.payload.clone();
    p.extensions = Some(json!({ "payment-identifier": { "info": { "required": true, "id": id } } }));
    assert_eq!(payment_identifier(&fx.env.ctx(), &p).unwrap().as_deref(), Some(id));
    let a = kob_x402::client::native::random_payment_id();
    let b = kob_x402::client::native::random_payment_id();
    assert_ne!(a, b);
}

#[test]
fn negative_vectors_of_payload_and_outpoint_shape() {
    // exact-transaction-missing-transaction: schema violation of the payload object -> it does not even decode
    let n = negative("exact-transaction-missing-transaction");
    assert_eq!(n["expectedError"], "invalid_kaspa_x402_payload");
    assert!(serde_json::from_value::<ExactPayload>(n["value"].clone()).is_err());
    // (a body that fails to decode is answered invalid_payload / invalid_kaspa_x402_payload by the facilitator)

    // scheme-payload-mismatch and exact-transfer-unsupported: a payload that is not an exact-transaction
    for name in ["scheme-payload-mismatch", "exact-transfer-unsupported"] {
        let n = negative(name);
        assert_eq!(n["expectedError"], "invalid_kaspa_payment_payload_type");
        assert!(serde_json::from_value::<PaymentPayload>(n["value"].clone()).is_err(), "{name}: not decodable as exact-transaction");
        // the same mismatch on an otherwise complete payload
        let fx = fixture();
        let mut p = fx.payload.clone();
        p.payload.kind = n["value"]["payload"]["type"].as_str().unwrap().to_string();
        expect(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaX402Payload, name);
    }

    // outpoint-index-overflow / txid-wrong-length come from batch payloads; the same field rules apply to the
    // outpoints of the safe transaction projection
    let fx = fixture();
    let n = negative("outpoint-index-overflow");
    assert_eq!(n["expectedError"], "invalid_kaspa_outpoint");
    let mut t: Value = serde_json::from_str(&fx.payload.payload.transaction).unwrap();
    t["inputs"][0]["previousOutpoint"]["index"] = n["value"]["fundingOutpoint"]["index"].clone();
    let mut p = fx.payload.clone();
    p.payload.transaction = t.to_string();
    expect(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction, "outpoint-index-overflow");
    let n = negative("txid-wrong-length");
    assert_eq!(n["expectedError"], "invalid_kaspa_outpoint");
    let mut t: Value = serde_json::from_str(&fx.payload.payload.transaction).unwrap();
    t["inputs"][0]["previousOutpoint"]["transactionId"] = n["value"]["fundingOutpoint"]["txid"].clone();
    let mut p = fx.payload.clone();
    p.payload.transaction = t.to_string();
    expect(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction, "txid-wrong-length");
    // the same fields as JSON numbers / non-canonical uint64 strings are refused as well
    let mut t: Value = serde_json::from_str(&fx.payload.payload.transaction).unwrap();
    t["outputs"][0]["value"] = json!("020000000");
    let mut p = fx.payload.clone();
    p.payload.transaction = t.to_string();
    expect(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction, "non-canonical value");
}

#[test]
fn settlement_response_vectors() {
    let read = |name: &str| {
        let p = format!("{ROOT}/vectors/kaspa-x402-rc1/settlement-response/{name}.json");
        json_of(&std::fs::read_to_string(p).unwrap())
    };
    // failure.json: a generic failure response
    let f = read("failure");
    let resp: SettlementResponse = serde_json::from_value(f["response"].clone()).unwrap();
    assert!(!resp.success);
    assert_eq!(serde_json::to_value(&resp).unwrap(), f["response"]);
    // what we produce for a failed payment has the same top-level shape, plus the local diagnostic
    let err = X402Error::state(Diag::InvalidKaspaExactUtxo, "input spent");
    let ours = SettlementResponse::failure(Network::Testnet10, resp.payer.clone(), &err);
    let (a, b) = (serde_json::to_value(&ours).unwrap(), f["response"].clone());
    for k in ["success", "errorReason", "transaction", "network", "payer"] {
        assert_eq!(a[k], b[k], "{k}");
    }
    assert_eq!(a["extensions"]["kaspa"]["diagnostic"], "invalid_kaspa_exact_utxo");

    // corrective-402.json: the corrective body is an ordinary PaymentRequired; unknown schemes and extras survive a round trip
    let c = read("corrective-402");
    let resp: SettlementResponse = serde_json::from_value(c["response"].clone()).unwrap();
    assert_eq!(resp.error_reason.as_deref(), Some("invalid_transaction_state"));
    let pr: PaymentRequired = serde_json::from_value(c["correctivePaymentRequired"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&pr).unwrap(), c["correctivePaymentRequired"]);
    // it offers batch-settlement, which KOB does not implement: a standard-native payment does not satisfy it
    assert_eq!(pr.accepts[0].scheme, "batch-settlement");
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.accepted = pr.accepts[0].clone();
    expect(
        verify_payment(&fx.env.ctx(), &pr.accepts[0], &p, &fx.request_hash),
        Reason::InvalidScheme,
        Diag::InvalidKaspaX402Binding,
        "corrective-402 (batch offer)",
    );
}
