//! KCC-23 conformance: kaspanet/kccs `main` 1516a6c (#26, KCC-23 status Draft) `kcc-0023/vectors/conformance.json` (vendored
//! unmodified in `crates/kob-tests/vectors/kcc23/`, see PROVENANCE.md there) executed against KOB's implementation:
//!
//! | Vector section | KOB code under test |
//! |---|---|
//! | binding (payload digest, rest preimage and digest, preparation id, covenant id) | rusty-kaspa v2.1.0 hashing KOB builds with, and `kob_protocol::kcc23::verify_binding` |
//! | binding_mutations | `kcc23::verify_binding` refuses each (changed metadata: the recomputed preparation id differs; version 0) |
//! | declarations | `kcc23::decode` / `kcc23::encode` and `kcc23::TokenDeclaration::from_payload` |
//!
//! Plus KOB's own writer: `kcc23::TokenMetadata` of the vector's example token serialises to the vector's `metadata_text`.
//!
//! Run: cargo test -p kob-tests --test kcc23_conformance_tests

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::tx::{id as tx_id, payload_digest, transaction_v1_rest_preimage, v1_rest_digest};
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{CovenantBinding, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput};
use kaspa_consensus_core::Hash;
use kob_protocol::kcc23::{self, BindingError, TokenDeclaration, TokenMetadata};
use serde_json::Value;
use sha2::{Digest, Sha256};
use silverscript_abi::{decode_hex, encode_hex};
use std::str::FromStr;

const VECTORS: &str = include_str!("../vectors/kcc23/conformance.json");
/// sha256 of the vendored file (blob 143654cbdcab4cffa305cbe025213cceb477d3d8 at kaspanet/kccs 1516a6c).
const VECTORS_SHA256: &str = "53bbaad58b0e508ed80c4b6d88524df3d51a270d7d32e1c4b3c82426ba6739e3";

fn v() -> Value {
    serde_json::from_str(VECTORS).expect("conformance.json parses")
}
fn hx(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap_or_else(|| panic!("expected hex, got {v}"))).expect("hex")
}
fn hash(v: &Value) -> Hash {
    Hash::from_str(v.as_str().expect("hash string")).expect("hash")
}
fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| panic!("expected an integer, got {v}"))
}

/// A transaction of the vector format.
fn tx(j: &Value) -> Transaction {
    let inputs = j["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let op = &i["previous_outpoint"];
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint::new(hash(&op["transaction_id"]), num(&op["index"]) as u32),
                hx(&i["signature_script_hex"]),
                num(&i["sequence"]),
                num(&i["compute_budget"]) as u16,
            )
        })
        .collect();
    let outputs = j["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let spk = &o["script_public_key"];
            let mut out = TransactionOutput::new(num(&o["value"]), ScriptPublicKey::from_vec(num(&spk["version"]) as u16, hx(&spk["script_hex"])));
            if let Some(c) = o["covenant"].as_object() {
                out.covenant = Some(CovenantBinding::new(num(&c["authorizing_input"]) as u16, hash(&c["covenant_id"])));
            }
            out
        })
        .collect();
    let subnet: [u8; 20] = hx(&j["subnetwork_id_hex"]).try_into().expect("20-byte subnetwork id");
    let t = Transaction::new(
        num(&j["version"]) as u16,
        inputs,
        outputs,
        num(&j["lock_time"]),
        SubnetworkId::from_bytes(subnet),
        num(&j["gas"]),
        hx(&j["payload_hex"]),
    );
    t.set_storage_mass(num(&j["storage_mass"]));
    t
}

#[test]
fn vendored_file_is_the_upstream_blob() {
    let digest = Sha256::digest(VECTORS.as_bytes());
    assert_eq!(encode_hex(&digest), VECTORS_SHA256, "vendored conformance.json drifted from kaspanet/kccs 1516a6c (see PROVENANCE.md)");
    let v = v();
    assert_eq!((num(&v["kcc"]), num(&v["format_version"])), (23, 1));
    assert_eq!(v["codec_prefix_hex"], "00");
    assert_eq!(kcc23::CODEC_JSON, 0x00);
}

#[test]
fn binding_vector() {
    let v = v();
    let b = &v["binding"];
    let e = &b["expected"];
    let prep = tx(&b["preparation_transaction"]);
    let genesis = tx(&b["genesis_transaction"]);
    // the hashing KOB builds with (rusty-kaspa v2.1.0) reproduces every intermediate value of the vector
    assert_eq!(payload_digest(&prep.payload), hash(&e["payload_digest"]));
    assert_eq!(encode_hex(&transaction_v1_rest_preimage(&prep)), e["rest_preimage_hex"].as_str().unwrap());
    assert_eq!(v1_rest_digest(&prep), hash(&e["rest_digest"]));
    assert_eq!(tx_id(&prep), hash(&e["preparation_transaction_id"]));
    let group: Vec<u32> = b["genesis_output_indices"].as_array().unwrap().iter().map(|i| num(i) as u32).collect();
    let id = covenant_id(genesis.inputs[0].previous_outpoint, group.iter().map(|&i| (i, &genesis.outputs[i as usize])));
    assert_eq!(id, hash(&e["covenant_id"]));
    // the binding check returns the declaration, byte for byte
    assert_eq!(b["valid"], true);
    let text = kcc23::verify_binding(&prep, &genesis, id).expect("the binding vector verifies");
    assert_eq!(text, b["metadata_text"].as_str().unwrap());
    assert_eq!(prep.payload, [&[0u8][..], text.as_bytes()].concat());
    // KOB's writer produces exactly the vector's metadata for the example token
    let m = TokenMetadata { name: "Example Token".into(), symbol: "EXT".into(), decimals: 8, ..Default::default() };
    assert_eq!(m.json(), b["metadata_text"].as_str().unwrap());
    assert_eq!(m.payload().unwrap(), prep.payload);
    let d = TokenDeclaration::from_payload(&prep.payload).unwrap();
    assert_eq!((d.name.as_deref(), d.symbol.as_deref(), d.decimals), (Some("Example Token"), Some("EXT"), Some(8)));
}

#[test]
fn binding_mutations_fail() {
    let v = v();
    let b = &v["binding"];
    let expected = hash(&b["expected"]["covenant_id"]);
    let genesis = tx(&b["genesis_transaction"]);
    let mutations = v["binding_mutations"].as_array().unwrap();
    assert_eq!(mutations.len(), 2);
    for m in mutations {
        assert_eq!(m["valid"], false);
        let mut prep_json = b["preparation_transaction"].clone();
        for (path, value) in m["replace"].as_object().unwrap() {
            let field = path.strip_prefix("/preparation_transaction/").unwrap_or_else(|| panic!("unexpected mutation path {path}"));
            prep_json[field] = value.clone();
        }
        let prep = tx(&prep_json);
        if let Some(id) = m.get("expected_preparation_transaction_id") {
            assert_eq!(tx_id(&prep), hash(id), "{}", m["name"]);
        }
        let err = kcc23::verify_binding(&prep, &genesis, expected).expect_err(m["name"].as_str().unwrap());
        match m["name"].as_str().unwrap() {
            "changed_metadata" => assert!(matches!(err, BindingError::NotBound { .. }), "{err}"),
            "version_0" => assert_eq!(err, BindingError::PreparationVersion(0)),
            other => panic!("unknown mutation {other}"),
        }
    }
}

#[test]
fn declaration_vectors() {
    let v = v();
    let cases = v["declarations"].as_array().unwrap();
    assert_eq!(cases.len(), 6);
    for c in cases {
        let payload = hx(&c["payload_hex"]);
        let name = c["name"].as_str().unwrap();
        match c["valid"].as_bool().unwrap() {
            true => {
                let text = kcc23::decode(&payload).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(kcc23::encode(text).unwrap(), payload, "{name}: encode(decode(p)) == p");
                TokenDeclaration::from_payload(&payload).unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            false => {
                assert!(kcc23::decode(&payload).is_err(), "{name} must be refused");
                assert!(TokenDeclaration::from_payload(&payload).is_err(), "{name} must be refused by the token reader");
            }
        }
    }
}
