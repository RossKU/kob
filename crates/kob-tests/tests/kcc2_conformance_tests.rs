//! KCC-2 conformance: kaspanet/kccs `main` 411b41b `kcc-0002/vectors/authority-schemes.json` (vendored unmodified in
//! `crates/kob-tests/vectors/kcc2/`, see PROVENANCE.md there) executed against KOB's off-chain implementation:
//!
//! | Vector section | KOB code under test |
//! |---|---|
//! | constructions | `kob_protocol::kcc2` (scheme bytes and names, `p2pkh_authority` = unkeyed `Hash(pubkey)`, `p2sh_authority`, `p2sh_envelope`), `kob_protocol::script::p2sh_spk`, the KCC-1 push forms, and the KCC-20 state encoder (`Kcc20State::encode`: `owner` and `owner_scheme` bytes) |
//! | approval_checks | `kcc2::{signature_approval, p2sh_approval, covenant_approval}` |
//! | registry_checks | `kcc2::classify`, the issuance owner-scheme set (the x402 owner-proof parser: `crates/kob-x402/tests/sighash.rs`) |
//!
//! The same cases are executed in KOB's KCC-20 programs by `kcc20_conformance_tests::kcc2_authority_vectors_in_kcc20_programs`.
//!
//! Run: cargo test -p kob-tests --test kcc2_conformance_tests -- --nocapture

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::kcc2::{self, SchemeClass};
use kob_protocol::state::{Kcc20State, StateCodec};
use kob_protocol::{issue, kcc1, script};
use serde_json::Value;
use sha2::{Digest, Sha256};
use silverscript_abi::{decode_hex, encode_hex};

const VECTORS: &str = include_str!("../vectors/kcc2/authority-schemes.json");
/// sha256 of the vendored file (blob 6db2925ae5d54f1ab29dcc0f0c403e078d6bcafb at kaspanet/kccs 411b41b).
const VECTORS_SHA256: &str = "1ff8e2169203b7e52a065fee20c8551b059fb955dc69a91c332ffd6f7cde5835";

fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("authority-schemes.json parses")
}
fn hx(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap_or_else(|| panic!("expected hex, got {v}"))).expect("hex")
}
fn h32(v: &Value) -> [u8; 32] {
    hx(v).try_into().expect("32 bytes")
}
fn byte(v: &Value) -> u8 {
    let b = hx(v);
    assert_eq!(b.len(), 1);
    b[0]
}
fn construction<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["constructions"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("construction {id}"))
}

#[test]
fn kcc2_vectors_are_the_pinned_upstream_file() {
    assert_eq!(encode_hex(&Sha256::digest(VECTORS.as_bytes())), VECTORS_SHA256, "vendored KCC-2 vectors drifted from the pin");
}

#[test]
fn kcc2_constructions() {
    let v = vectors();
    for c in v["constructions"].as_array().unwrap() {
        let id = c["id"].as_str().unwrap();
        let scheme = byte(&c["scheme_byte"]);
        assert_eq!(kcc2::classify(scheme), SchemeClass::Assigned, "{id}");
        assert_eq!(kcc2::scheme_name(scheme), c["scheme"].as_str(), "{id}: scheme name");
        let authority = h32(&c["authority_value"]);
        let derived: [u8; 32] = match scheme {
            kcc2::P2PK_SCHNORR => h32(&c["public_key"]),
            kcc2::P2PKH_SCHNORR | kcc2::P2PKH_ECDSA => {
                let key = hx(&c["public_key"]);
                assert_eq!(Some(key.len()), kcc2::public_key_len(scheme), "{id}: key length");
                let h = kcc2::p2pkh_authority(scheme, &key).expect("valid key length");
                assert_eq!(h, kcc1::hash(&key), "{id}: unkeyed Hash(pubkey)");
                assert_ne!(h, kcc1::hash_keyed(&key, b"PublicKeyHash").unwrap(), "{id}: not the retired keyed form");
                h
            }
            kcc2::P2SH => {
                let r = hx(&c["redeem_script"]);
                let spk = ScriptPublicKey::new(
                    c["script_public_key"]["version"].as_u64().unwrap() as u16,
                    hx(&c["script_public_key"]["script"]).into(),
                );
                assert_eq!(kcc2::p2sh_envelope(&kcc2::p2sh_authority(&r)), spk, "{id}: envelope");
                assert_eq!(script::p2sh_spk(&r), spk, "{id}: kob_protocol::script::p2sh_spk");
                kcc2::p2sh_authority(&r)
            }
            kcc2::COVENANT_ID => h32(&c["covenant_id"]),
            _ => unreachable!(),
        };
        assert_eq!(derived, authority, "{id}: authority value");
        let want_type = if scheme == kcc2::P2PK_SCHNORR { "pubkey" } else { "byte[32]" };
        assert_eq!(c["value_type"], want_type, "{id}: KCC-1 value type");
        assert_eq!(kcc1::push_minimal(&authority), hx(&c["argument_encoding"]), "{id}: argument encoding");
        assert_eq!(kcc1::push_explicit(&authority), hx(&c["state_encoding"]), "{id}: state encoding");
        // KCC-20 state: `owner` holds the authority value (PushExplicit), `owner_scheme` the separate scheme byte
        let st = Kcc20State {
            amount: 1,
            owner: authority,
            owner_scheme: scheme,
            borrow_scheme: 0,
            borrow_guard: [0; 32],
            extension_commitment: [0; 32],
        };
        let enc = st.encode();
        assert_eq!(&enc[9..42], hx(&c["state_encoding"]).as_slice(), "{id}: KCC-20 owner field");
        assert_eq!(&enc[42..44], &[0x01, scheme], "{id}: KCC-20 owner_scheme field");
        assert_eq!(Kcc20State::decode(&enc).unwrap(), st, "{id}: KCC-20 state round trip");
        println!("construction {id}: ok");
    }
    // wrong key lengths have no P2PKH authority
    assert!(kcc2::p2pkh_authority(kcc2::P2PKH_SCHNORR, &[2; 33]).is_none());
    assert!(kcc2::p2pkh_authority(kcc2::P2PKH_ECDSA, &[2; 32]).is_none());
    assert!(kcc2::p2pkh_authority(kcc2::P2PKH_ECDSA, &[4; 33]).is_none(), "uncompressed prefix");
}

#[test]
fn kcc2_approval_checks() {
    let v = vectors();
    for c in v["approval_checks"].as_array().unwrap() {
        let id = c["id"].as_str().unwrap();
        let con = construction(&v, c["construction"].as_str().unwrap());
        let scheme = byte(&con["scheme_byte"]);
        let authority = h32(&con["authority_value"]);
        let ctx = &c["context"];
        let got = match scheme {
            kcc2::P2PK_SCHNORR | kcc2::P2PKH_SCHNORR | kcc2::P2PKH_ECDSA => {
                kcc2::signature_approval(scheme, &authority, &hx(&ctx["public_key"]), ctx["signature_valid"].as_bool().unwrap())
            }
            kcc2::P2SH => {
                let spks: Vec<ScriptPublicKey> = ctx["input_script_public_keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| ScriptPublicKey::new(s["version"].as_u64().unwrap() as u16, hx(&s["script"]).into()))
                    .collect();
                kcc2::p2sh_approval(&authority, &spks)
            }
            kcc2::COVENANT_ID => {
                let ids: Vec<Option<[u8; 32]>> =
                    ctx["input_covenant_ids"].as_array().unwrap().iter().map(|x| (!x.is_null()).then(|| h32(x))).collect();
                assert!((ctx["active_input_index"].as_u64().unwrap() as usize) < ids.len(), "{id}: active input listed");
                kcc2::covenant_approval(&authority, &ids)
            }
            _ => unreachable!(),
        };
        assert_eq!(got, c["expected"].as_bool().unwrap(), "{id}");
        println!("approval {id}: {got} ok");
    }
}

#[test]
fn kcc2_registry_checks() {
    let v = vectors();
    for r in v["registry_checks"].as_array().unwrap() {
        let b = byte(&r["scheme_byte"]);
        let (assigned, custom) = (r["assigned"].as_bool().unwrap(), r["custom"].as_bool().unwrap());
        let class = kcc2::classify(b);
        assert_eq!(class == SchemeClass::Assigned, assigned, "{b:#04x} assigned");
        assert_eq!(class == SchemeClass::Custom, custom, "{b:#04x} custom");
        // KOB's KCC-20 issuance enables exactly the assigned schemes (no custom scheme is defined by KCC-20)
        assert_eq!(issue::OWNER_SCHEMES_ENABLED.contains(&b), assigned, "{b:#04x}: issuance owner schemes");
        println!("registry {b:#04x}: {class:?} ok");
    }
    // every byte, not only the listed ones
    for b in 0..=255u8 {
        let want = if b <= 4 {
            SchemeClass::Assigned
        } else if b < 0x80 {
            SchemeClass::Reserved
        } else {
            SchemeClass::Custom
        };
        assert_eq!(kcc2::classify(b), want, "{b:#04x}");
    }
}

/// Tracking test for the upstream KCC-20 reference program (argent-lang/kcc20-reference PR #1, head `600646873e`, unchanged
/// on 2026-10-02): its `p2pkh_hash` is the unkeyed `blake3(public_key)` (`OP_BLAKE3`), which is KCC-2's `Hash(pubkey)`
/// after #30, so KOB's copies need no change. If upstream re-vendoring ever brings a keyed form (`OP_BLAKE3_WITH_KEY`,
/// a `PublicKeyHash` key) back, or the provenance commit moves, this fails and `docs/spec/kcc-conformance.md` section 4
/// must be revisited (template hashes, registry, Argent KOBToken, budgets, tips, vectors, deploy files, retirement).
#[test]
fn kcc20_programs_use_the_unkeyed_p2pkh_hash() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = [
        "contracts/kcc20/KCC20Ref.sil",
        "contracts/kcc20/variants/KCC20Ref_4x5.sil",
        "contracts/kcc20/variants/KCC20Ref_8x8.sil",
        "contracts/kcc20/variants/KCC20Ref_16x16.sil",
        "contracts/kcc20/p2/KCC20P2.sil",
        "contracts/kcc20/p2/KCC20Opt.sil",
        "contracts/argent/KOBToken/sil/KCC20.sil",
        "contracts/argent/kcc20_8x8.ag",
        "tools/wallet-gate/contracts/KCC20Ref.sil",
    ];
    for f in files {
        let src = std::fs::read_to_string(root.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        let unkeyed = src.matches("return blake3(gen__glob_public_key);").count() + src.matches("return blake3(public_key);").count();
        assert_eq!(unkeyed, 1, "{f}: p2pkh_hash must be the unkeyed blake3 of the public key");
        for keyed in ["blake3WithKey", "Blake3WithKey", "PublicKeyHash"] {
            assert!(!src.contains(keyed), "{f}: keyed hash form `{keyed}`");
        }
    }
    let reference = std::fs::read_to_string(root.join("contracts/kcc20/KCC20Ref.sil")).unwrap();
    assert!(
        reference.contains("commit 600646873ebfa2ec87a8ae57783c9912caf52857"),
        "the reference program's provenance moved: re-run the KCC-1/KCC-2 conformance review (docs/spec/kcc-conformance.md)"
    );
}
