//! KCC-20 conformance: kaspanet/kccs PR #31 `kcc-0020/vectors/conformance.json` (vendored unmodified in
//! `crates/kob-tests/vectors/kcc20/`, see PROVENANCE.md there) executed against KOB's reference KCC-20 build
//! (`contracts/kcc20/KCC20Ref.sil`, 3 token inputs / 3 outputs) and the KOB 8/8 slot-limit variant
//! (`contracts/kcc20/variants/KCC20Ref_8x8.sil`, the program KOB issues), in rusty-kaspa v2.1.0's TxScriptEngine
//! with KIP-20 covenant context.
//!
//! What is checked, per vector section:
//!   state_record / dispatch / state_encoding / transfer_arguments  byte-exact against the compiled artifacts and the
//!                                                                  silverscript-abi encoder (offline checks)
//!   standard_transfer                                              every case executed as a real transaction; a matching
//!                                                                  accept control passes, each case changes only the listed fields
//!   owner_witness                                                  witness layouts for schemes 00-04 executed with real keys,
//!                                                                  P2PKH hash vectors recomputed (unkeyed BLAKE3)
//!   borrow_witness / hash_chain                                    layouts, the 3-link chain recomputed, one accepted borrow per scheme
//!   borrowed_receive                                               all 10 cases executed
//!
//! The vectors' signatures and public keys are placeholder byte patterns ("signature checks are stipulated to
//! succeed"). Where a case needs a signature to verify, this harness builds the SAME structure with real keys (same
//! witness layout, same lengths, same state fields except the key material); the placeholder-only bytes are asserted
//! against the layout instead. Hash-chain cases keep the vector's revealed links and swap only the one-time key.
//!
//! Slot limits: the vector `over-max-token-inputs` (4 inputs vs max 3) applies to the 3/3 reference. For the 8/8 variant it
//! is ADAPTED: 4 inputs are accepted and 9 inputs are rejected (counted as `adapted` in the summary).
//!
//! Run: cargo test --release -p kob-tests --test kcc20_conformance_tests -- --nocapture --test-threads=1

#![allow(clippy::too_many_arguments)]

mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::sighash::{calc_ecdsa_signature_hash, calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::OpTrue;
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use serde_json::Value;
use sha2::{Digest, Sha256};
use silverscript_abi::{decode_hex, encode_hex, encode_runtime_state_script, ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use common::{bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script};

const VECTORS: &str = include_str!("../vectors/kcc20/conformance.json");
/// sha256 of the vendored file (blob e74b90d2dba8a4ea8f3dab9253ad68e17a1e04b6 at kaspanet/kccs cfb74cf).
const VECTORS_SHA256: &str = "467c5e7d24c0b44c7cf25b122f61922493466afd7529e0bd7fd23ac7847e724e";

const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const OWNER_COV: Hash = Hash::from_bytes([0xc0; 32]);
const KAS: i64 = 100_000_000;
const BUDGET: u16 = 300;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;

/// (source name, max token inputs, max token outputs, label)
const PROGRAMS: [(&str, usize, usize); 2] = [("KCC20Ref", 3, 3), ("KCC20Ref_8x8", 8, 8)];

// ---------------------------------------------------------------- vector access

fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("conformance.json parses")
}
fn hx(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap_or_else(|| panic!("expected hex string, got {v}"))).expect("hex")
}
fn h32(v: &Value) -> [u8; 32] {
    hx(v).try_into().expect("32-byte hex")
}
fn hb(v: &Value) -> u8 {
    let b = hx(v);
    assert_eq!(b.len(), 1, "one-byte hex");
    b[0]
}
fn hexs(b: &[u8]) -> String {
    encode_hex(b)
}

fn keypair() -> Keypair {
    let secp = Secp256k1::new();
    let mut rng = thread_rng();
    let mut sk = [0u8; 32];
    loop {
        rng.fill_bytes(&mut sk);
        if let Ok(s) = SecretKey::from_slice(&sk) {
            return Keypair::from_secret_key(&secp, &s);
        }
    }
}
fn pk(kp: &Keypair) -> [u8; 32] {
    kp.x_only_public_key().0.serialize()
}
fn pk33(kp: &Keypair) -> [u8; 33] {
    kp.public_key().serialize()
}
fn b3(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

// ---------------------------------------------------------------- tally

struct Tally {
    prog: &'static str,
    section: &'static str,
    total: u32,
    pass: u32,
    adapted: u32,
    skip: u32,
}
impl Tally {
    fn new(prog: &'static str, section: &'static str) -> Self {
        Tally { prog, section, total: 0, pass: 0, adapted: 0, skip: 0 }
    }
    fn ok(&mut self, what: &str) {
        self.total += 1;
        self.pass += 1;
        println!("  [PASS]  {}/{}: {what}", self.prog, self.section);
    }
    fn adapted(&mut self, what: &str) {
        self.total += 1;
        self.pass += 1;
        self.adapted += 1;
        println!("  [PASS*] {}/{}: {what} (ADAPTED to this program's slot limits)", self.prog, self.section);
    }
    #[allow(dead_code)]
    fn skip(&mut self, what: &str, why: &str) {
        self.total += 1;
        self.skip += 1;
        println!("  [SKIP]  {}/{}: {what}: {why}", self.prog, self.section);
    }
    fn done(&self) {
        println!(
            "CONFORMANCE {:<13} {:<22} total={:<3} pass={:<3} (adapted={}) skip={} fail=0",
            self.prog, self.section, self.total, self.pass, self.adapted, self.skip
        );
        assert_eq!(self.skip, 0, "no vector may be skipped silently");
    }
}

// ---------------------------------------------------------------- program and state model

#[derive(Clone, Debug, PartialEq)]
struct St {
    amount: i64,
    owner: [u8; 32],
    scheme: u8,
    borrow: u8,
    guard: [u8; 32],
    ext: [u8; 32],
}
impl St {
    fn from_vec(v: &Value) -> St {
        St {
            amount: v["amount"].as_i64().expect("amount"),
            owner: h32(&v["owner_hex"]),
            scheme: hb(&v["owner_scheme_hex"]),
            borrow: hb(&v["borrow_scheme_hex"]),
            guard: h32(&v["borrow_guard_hex"]),
            ext: h32(&v["extension_commitment_hex"]),
        }
    }
    fn value(&self) -> ArtifactValue {
        ArtifactValue::Object(self.map())
    }
    fn map(&self) -> BTreeMap<String, ArtifactValue> {
        BTreeMap::from([
            ("amount".to_string(), ArtifactValue::Int(self.amount)),
            ("owner".to_string(), self.owner.to_vec().into()),
            ("owner_scheme".to_string(), ArtifactValue::Byte(self.scheme)),
            ("borrow_scheme".to_string(), ArtifactValue::Byte(self.borrow)),
            ("borrow_guard".to_string(), self.guard.to_vec().into()),
            ("extension_commitment".to_string(), self.ext.to_vec().into()),
        ])
    }
}

struct Prog {
    name: &'static str,
    max_in: usize,
    art: SilAbiArtifact,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
}
impl Prog {
    fn new(name: &'static str, max_in: usize) -> Prog {
        let art = compile_contract(
            &common::contract_source(name),
            &[
                ArtifactValue::Int(1000),
                vec![3u8; 32].into(),
                ArtifactValue::Byte(4),
                ArtifactValue::Byte(0),
                vec![0u8; 32].into(),
                vec![0u8; 32].into(),
            ],
            CompileOptions::default(),
        )
        .unwrap_or_else(|e| panic!("compile {name}: {e}"));
        let (prefix, suffix, _) = compiled_template_parts_and_hash(&art);
        Prog { name, max_in, art, prefix, suffix }
    }
    fn state_bytes(&self, s: &St) -> Vec<u8> {
        let c = common::single_contract(&self.art);
        encode_runtime_state_script(&self.art, &c.runtime_state, &s.map()).expect("encode state")
    }
    fn redeem(&self, s: &St) -> Vec<u8> {
        [self.prefix.clone(), self.state_bytes(s), self.suffix.clone()].concat()
    }
    fn spk(&self, s: &St) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(s))
    }
}

// ---------------------------------------------------------------- scenario model

/// Authorisation material (the witness bytes after the path byte for the leader, the whole witness for a delegator).
#[derive(Clone)]
enum Body {
    /// 65-byte Schnorr signature by kp (p2pk-schnorr owner, schnorr-signature borrow guard)
    Schnorr(Keypair),
    /// x-only pubkey ++ Schnorr signature (p2pkh-schnorr/v1)
    P2pkhSchnorr(Keypair),
    /// compressed pubkey ++ ECDSA signature (p2pkh-ecdsa/v1)
    P2pkhEcdsa(Keypair),
    /// one authority input index (p2sh/v1)
    Index(u8),
    /// no bytes (covenant-id/v1, amount-threshold borrow)
    Empty,
    /// hash-chain borrow: next_borrow_guard ++ one-time pubkey ++ signature
    HashChain { next: [u8; 32], kp: Keypair },
    /// KCC-2 vector cases: the given public-key bytes (any length) ++ a signature by `signer` (ECDSA or Schnorr)
    Raw { key: Vec<u8>, signer: Keypair, ecdsa: bool },
}
impl Body {
    fn len(&self) -> usize {
        match self {
            Body::Schnorr(_) => 65,
            Body::P2pkhSchnorr(_) => 97,
            Body::P2pkhEcdsa(_) => 98,
            Body::Index(_) => 1,
            Body::Empty => 0,
            Body::HashChain { .. } => 129,
            Body::Raw { key, .. } => key.len() + 65,
        }
    }
}

#[derive(Clone)]
enum Extra {
    /// P2SH(OpTrue) authority input (no covenant)
    P2sh,
    /// OpTrue UTXO carrying covenant id OWNER_COV
    CovOwner,
    /// KCC-2 vector cases: a UTXO at this script public key (spent with `push(OP_TRUE)` when it is P2SH(OP_TRUE); the
    /// verdicts of these cases are the leader's, so an unspendable bystander does not matter)
    Spk(ScriptPublicKey),
    /// KCC-2 vector cases: an OpTrue UTXO carrying this covenant id
    Cov(Hash),
}

#[derive(Clone)]
struct Scn {
    leader: St,
    leader_kas: i64,
    path: u8,
    body: Body,
    delegates: Vec<(St, Body)>,
    extras: Vec<Extra>,
    next: Vec<St>,
    succ_kas: Option<i64>,
}

fn p2sh_spk() -> ScriptPublicKey {
    pay_to_script_hash_script(&[OpTrue])
}
/// The 32-byte script hash inside a P2SH script public key.
fn p2sh_hash() -> [u8; 32] {
    let s = p2sh_spk();
    s.script()[2..34].try_into().unwrap()
}

fn sign_schnorr(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(1).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(1);
    s
}
fn sign_ecdsa(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_ecdsa_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(1).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let secp = Secp256k1::new();
    let mut s = secp.sign_ecdsa(&msg, &kp.secret_key()).serialize_compact().to_vec();
    s.push(1);
    s
}

fn body_bytes(b: &Body, tx: &Transaction, entries: &[UtxoEntry], k: usize) -> Vec<u8> {
    let out = match b {
        Body::Schnorr(kp) => sign_schnorr(tx, entries, k, kp),
        Body::P2pkhSchnorr(kp) => [pk(kp).to_vec(), sign_schnorr(tx, entries, k, kp)].concat(),
        Body::P2pkhEcdsa(kp) => [pk33(kp).to_vec(), sign_ecdsa(tx, entries, k, kp)].concat(),
        Body::Index(i) => vec![*i],
        Body::Empty => vec![],
        Body::HashChain { next, kp } => [next.to_vec(), pk(kp).to_vec(), sign_schnorr(tx, entries, k, kp)].concat(),
        Body::Raw { key, signer, ecdsa } => {
            [key.clone(), if *ecdsa { sign_ecdsa(tx, entries, k, signer) } else { sign_schnorr(tx, entries, k, signer) }].concat()
        }
    };
    assert_eq!(out.len(), b.len(), "witness body length");
    out
}

fn token_utxo(p: &Prog, s: &St, value: i64) -> (UtxoEntry, Vec<u8>) {
    let redeem = p.redeem(s);
    (UtxoEntry::new(value as u64, pay_to_script_hash_script(&redeem), 0, false, Some(TOKEN_COV)), redeem)
}

fn build(p: &Prog, s: &Scn) -> (Transaction, Vec<UtxoEntry>) {
    let mut entries = vec![];
    let mut redeems = vec![];
    let (e, r) = token_utxo(p, &s.leader, s.leader_kas);
    entries.push(e);
    redeems.push(r);
    for (d, _) in &s.delegates {
        let (e, r) = token_utxo(p, d, 10 * KAS);
        entries.push(e);
        redeems.push(r);
    }
    for x in &s.extras {
        entries.push(match x {
            Extra::P2sh => UtxoEntry::new((10 * KAS) as u64, p2sh_spk(), 0, false, None),
            Extra::CovOwner => {
                UtxoEntry::new((10 * KAS) as u64, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(OWNER_COV))
            }
            Extra::Spk(spk) => UtxoEntry::new((10 * KAS) as u64, spk.clone(), 0, false, None),
            Extra::Cov(id) => UtxoEntry::new((10 * KAS) as u64, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(*id)),
        });
    }
    let inputs: Vec<TransactionInput> = (0..entries.len())
        .map(|k| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([k as u8 + 1; 32]), index: k as u32 },
                vec![],
                0,
                BUDGET,
            )
        })
        .collect();
    let outputs: Vec<TransactionOutput> = s
        .next
        .iter()
        .enumerate()
        .map(|(j, n)| TransactionOutput {
            value: if j == 0 { s.succ_kas.unwrap_or(s.leader_kas) } else { KAS } as u64,
            script_public_key: p.spk(n),
            covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: TOKEN_COV }),
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    // leader
    let lw = [vec![s.path], body_bytes(&s.body, &unsigned, &entries, 0)].concat();
    let mut ss =
        encode_entry_sig_script(&p.art, "transfer", &[ArtifactValue::Array(s.next.iter().map(St::value).collect()), lw.into()])
            .expect("encode transfer");
    ss.extend_from_slice(&push_redeem_script(&redeems[0]));
    tx.inputs[0].signature_script = ss;
    // delegators
    for (i, (_, body)) in s.delegates.iter().enumerate() {
        let k = 1 + i;
        let w = body_bytes(body, &unsigned, &entries, k);
        let mut ss = encode_entry_sig_script(&p.art, "transfer_delegator", &[w.into()]).expect("encode delegator");
        ss.extend_from_slice(&push_redeem_script(&redeems[k]));
        tx.inputs[k].signature_script = ss;
    }
    // extras
    for (i, x) in s.extras.iter().enumerate() {
        let k = 1 + s.delegates.len() + i;
        tx.inputs[k].signature_script = match x {
            Extra::P2sh => push_redeem_script(&[OpTrue]),
            Extra::Spk(spk) if *spk == p2sh_spk() => push_redeem_script(&[OpTrue]),
            Extra::CovOwner | Extra::Spk(_) | Extra::Cov(_) => vec![],
        };
    }
    (tx, entries)
}

/// Result of executing every input: Err(context error) or per-input results.
fn execute(tx: &Transaction, entries: &[UtxoEntry]) -> Result<Vec<Result<u64, TxScriptError>>, String> {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    Ok((0..tx.inputs.len())
        .map(|i| {
            let input = tx.inputs[i].clone();
            let mut vm = TxScriptEngine::from_transaction_input(
                &populated,
                &input,
                i,
                &entries[i],
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
            );
            vm.execute().map(|_| vm.used_script_units().0)
        })
        .collect())
}

/// The transaction is valid iff the covenant context builds and every input passes.
fn accepts(p: &Prog, s: &Scn) -> Result<(), String> {
    let (tx, entries) = build(p, s);
    match execute(&tx, &entries)? {
        res if res.iter().all(|r| r.is_ok()) => Ok(()),
        res => {
            let bad: Vec<String> =
                res.iter().enumerate().filter_map(|(i, r)| r.as_ref().err().map(|e| format!("in[{i}]: {e:?}"))).collect();
            Err(bad.join("; "))
        }
    }
}
fn expect_accept(p: &Prog, s: &Scn, what: &str) {
    if let Err(e) = accepts(p, s) {
        panic!("{}: {what}: expected ACCEPT but got {e}", p.name);
    }
}
/// A reject must come from the leader (input 0) or the covenant context, never only from a bystander input.
fn expect_reject(p: &Prog, s: &Scn, what: &str) -> String {
    let (tx, entries) = build(p, s);
    match execute(&tx, &entries) {
        Err(e) => e,
        Ok(res) => {
            assert!(res[0].is_err(), "{}: {what}: expected the leader to REJECT but it passed (others: {:?})", p.name, res);
            format!("in[0]: {:?}", res[0].as_ref().unwrap_err())
        }
    }
}

// ---------------------------------------------------------------- tests

fn programs() -> Vec<Prog> {
    PROGRAMS.iter().map(|(n, i, _)| Prog::new(n, *i)).collect()
}

#[test]
fn conformance_vectors_are_the_pinned_upstream_file() {
    let digest = Sha256::digest(VECTORS.as_bytes());
    assert_eq!(hexs(&digest), VECTORS_SHA256, "vendored conformance.json drifted from kaspanet/kccs cfb74cf (see PROVENANCE.md)");
    let v = vectors();
    assert_eq!(v["kcc"], 20);
    assert_eq!(v["format_version"], 1);
    for section in [
        "state_record",
        "dispatch",
        "state_encoding",
        "transfer_arguments",
        "standard_transfer",
        "owner_witness",
        "borrow_witness",
        "hash_chain",
        "borrowed_receive",
    ] {
        assert!(v.get(section).is_some(), "vector section {section} missing");
    }
    println!(
        "VECTORS format_version=1 sha256={VECTORS_SHA256} sections: state_record(1) dispatch({}) state_encoding(1) transfer_arguments(1) \
         standard_transfer({} cases) owner_witness({}) borrow_witness({}) hash_chain(1) borrowed_receive({} cases)",
        v["dispatch"].as_array().unwrap().len(),
        v["standard_transfer"]["cases"].as_array().unwrap().len(),
        v["owner_witness"].as_array().unwrap().len(),
        v["borrow_witness"].as_array().unwrap().len(),
        v["borrowed_receive"]["cases"].as_array().unwrap().len(),
    );
}

/// state_record, dispatch, state_encoding and transfer_arguments: offline byte-exact checks.
#[test]
fn conformance_state_dispatch_encoding() {
    let v = vectors();
    for p in programs() {
        let contract = common::single_contract(&p.art);

        // ---- state_record
        let mut t = Tally::new(p.name, "state_record");
        let rec = &v["state_record"];
        assert_eq!(rec["name"], "KCC20State");
        let types: Vec<&str> = rec["field_types"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        let fields = &contract.runtime_state.fields;
        assert_eq!(fields.len(), types.len(), "field count");
        let mut dispatch_name = String::from("{");
        for (i, (f, ty)) in fields.iter().zip(&types).enumerate() {
            let j = serde_json::to_value(&f.ty).unwrap();
            let want = match *ty {
                "int" => serde_json::json!({"kind": "int"}),
                "byte" => serde_json::json!({"kind": "byte"}),
                "byte[32]" => serde_json::json!({"kind": "fixed_bytes", "len": 32}),
                other => panic!("unexpected vector field type {other}"),
            };
            assert_eq!(j, want, "field {i} ({}) type", f.name);
            if i > 0 {
                dispatch_name.push(',');
            }
            dispatch_name.push_str(ty);
        }
        dispatch_name.push('}');
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["amount", "owner", "owner_scheme", "borrow_scheme", "borrow_guard", "extension_commitment"]
        );
        assert_eq!(dispatch_name, rec["dispatch_type_name"].as_str().unwrap());
        assert_eq!(contract.compiled.state_span.len as u64, rec["encoded_state_length"].as_u64().unwrap());
        t.ok("field types, field order, dispatch type name, encoded length 112 vs compiled artifact");
        t.done();

        // ---- dispatch
        let mut t = Tally::new(p.name, "dispatch");
        for d in v["dispatch"].as_array().unwrap() {
            let name = d["entrypoint"].as_str().unwrap();
            let sig = d["function_signature"].as_str().unwrap();
            let tag = d["dispatch_tag_hex"].as_str().unwrap();
            // tag = first 4 bytes of blake3(signature)
            assert_eq!(hexs(&b3(sig.as_bytes())[..4]), tag, "{name}: tag is blake3(signature)[..4]");
            // signature is what the artifact's ABI says
            let entry = contract.entries.get(name).unwrap_or_else(|| panic!("{}: entry {name} missing", p.name));
            assert_eq!(entry.dispatch_tag.to_hex(), tag, "{}: compiled dispatch tag of {name}", p.name);
            let params: Vec<String> = entry
                .params
                .iter()
                .map(|q| match serde_json::to_value(&q.ty).unwrap() {
                    Value::Object(o) if o["kind"] == "bytes" => "byte[]".to_string(),
                    Value::Object(o) if o["kind"] == "dynamic_array" => format!("{}[]", dispatch_name),
                    other => panic!("unexpected param type {other}"),
                })
                .collect();
            assert_eq!(format!("{name}({})", params.join(",")), sig, "{}: ABI signature of {name}", p.name);
            t.ok(&format!("{name} {sig} -> {tag}"));
        }
        assert_eq!(contract.entries.len(), 2, "exactly the two standard entrypoints");
        t.done();

        // ---- state_encoding
        let mut t = Tally::new(p.name, "state_encoding");
        let se = &v["state_encoding"];
        let st = St::from_vec(&se["state"]);
        let enc = p.state_bytes(&st);
        let expect_all = hx(&se["encoded_hex"]);
        assert_eq!(enc, expect_all, "state push encoding (abi encoder)");
        assert_eq!(enc.len(), 112);
        // per-field pushes
        let mut off = 0;
        let mut fields_hex = vec![];
        while off < enc.len() {
            let n = enc[off] as usize;
            fields_hex.push(hexs(&enc[off..off + 1 + n]));
            off += 1 + n;
        }
        let want: Vec<String> = se["encoded_fields_hex"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        assert_eq!(fields_hex, want, "per-field pushes");
        assert_eq!(want.concat(), hexs(&expect_all));
        // and the compiler itself lays out a program's state identically
        let art = compile_contract(
            &common::contract_source(p.name),
            &[
                ArtifactValue::Int(st.amount),
                st.owner.to_vec().into(),
                ArtifactValue::Byte(st.scheme),
                ArtifactValue::Byte(st.borrow),
                st.guard.to_vec().into(),
                st.ext.to_vec().into(),
            ],
            CompileOptions::default(),
        )
        .unwrap();
        let l = common::state_layout(&art);
        assert_eq!(&bytecode(&art)[l.start..l.start + l.len], expect_all.as_slice(), "compiler state span");
        t.ok("state record push-encodes byte-exact (abi encoder and compiled program state span)");
        t.done();

        // ---- transfer_arguments
        let mut t = Tally::new(p.name, "transfer_arguments");
        let ta = &v["transfer_arguments"];
        let states: Vec<ArtifactValue> = ta["next_states"].as_array().unwrap().iter().map(|s| St::from_vec(s).value()).collect();
        let witness = hx(&ta["witness_hex"]);
        assert_eq!(witness.len(), 66);
        assert_eq!(push_redeem_script(&witness), hx(&ta["witness_push_hex"]), "witness push");
        let ss = encode_entry_sig_script(&p.art, "transfer", &[ArtifactValue::Array(states), witness.clone().into()]).unwrap();
        let mut want = vec![];
        for g in ta["grouped_field_pushes_hex"].as_array().unwrap() {
            want.extend(hx(g));
        }
        want.extend(hx(&ta["witness_push_hex"]));
        want.extend(hx(&ta["dispatch_tag_push_hex"]));
        assert_eq!(hexs(&ss), hexs(&want), "transfer sigscript (grouped pushes ++ witness push ++ dispatch tag push)");
        t.ok("transfer arguments: grouped field pushes, witness push and dispatch-tag push are byte-exact");
        // delegator: witness push then tag push
        let dss = encode_entry_sig_script(&p.art, "transfer_delegator", &[witness.clone().into()]).unwrap();
        let tag2 = hx(&serde_json::json!(v["dispatch"][1]["dispatch_tag_hex"]));
        assert_eq!(dss, [hx(&ta["witness_push_hex"]), push_redeem_script(&tag2)].concat(), "delegator sigscript");
        t.ok("transfer_delegator arguments: witness push ++ dispatch-tag push");
        t.done();
    }
}

// ---------------------------------------------------------------- standard_transfer

struct StdCase {
    leader: St,
    delegate_amounts: Vec<i64>,
    next: Vec<St>,
    path: u8,
}

/// Instantiate the vector's `common` block, then apply a case's overrides. `kp` replaces the vector's placeholder owner key.
fn std_case(v: &Value, case: Option<&Value>, kp: &Keypair) -> StdCase {
    let sv = &v["standard_transfer"];
    let common = &sv["common"];
    let over = |k: &str| case.and_then(|c| c.get(k)).or_else(|| common.get(k));
    let mut leader = St::from_vec(&common["leader_state"]);
    assert_eq!(leader.scheme, 0);
    leader.owner = pk(kp);
    let ta = &v["transfer_arguments"]["next_states"];
    let amounts: Vec<i64> = over("next_state_amounts").unwrap().as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
    let schemes: Vec<u8> = over("next_state_owner_schemes_hex").unwrap().as_array().unwrap().iter().map(hb).collect();
    let exts: Vec<[u8; 32]> = over("next_state_extension_commitments_hex").unwrap().as_array().unwrap().iter().map(h32).collect();
    let mut next = vec![];
    for i in 0..amounts.len() {
        let mut s = St::from_vec(&ta[i]);
        s.amount = amounts[i];
        s.scheme = schemes[i];
        s.ext = exts[i];
        next.push(s);
    }
    let delegate_amounts = over("delegate_amounts").unwrap().as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
    let w = hx(over("witness_hex").unwrap());
    assert_eq!(w.len(), 66, "vector witness layout: path byte + 65-byte signature");
    StdCase { leader, delegate_amounts, next, path: w[0] }
}

fn std_scn(c: &StdCase, kp: &Keypair) -> Scn {
    let delegates = c
        .delegate_amounts
        .iter()
        .map(|a| (St { amount: *a, owner: pk(kp), scheme: 0, borrow: 0, guard: [0; 32], ext: c.leader.ext }, Body::Schnorr(*kp)))
        .collect();
    Scn {
        leader: c.leader.clone(),
        leader_kas: 10 * KAS,
        path: c.path,
        body: Body::Schnorr(*kp),
        delegates,
        extras: vec![],
        next: c.next.clone(),
        succ_kas: None,
    }
}

#[test]
fn conformance_standard_transfer() {
    let v = vectors();
    let kp = keypair();
    for p in programs() {
        let mut t = Tally::new(p.name, "standard_transfer");
        // accept control: the vector's common block (600 -> 00 owner, 400 -> 01 owner)
        let control = std_case(&v, None, &kp);
        expect_accept(&p, &std_scn(&control, &kp), "common transfer");
        t.ok("accept control (common: 1000 -> 600 + 400, path byte 0x00, p2pk-schnorr owner)");
        // boundary control: the largest number of token inputs the program allows is accepted
        let mut at_max = std_case(&v, None, &kp);
        at_max.delegate_amounts = vec![100; p.max_in - 1];
        at_max.next[1].amount = 400 + 100 * (p.max_in as i64 - 1);
        expect_accept(&p, &std_scn(&at_max, &kp), "max token inputs");
        t.ok(&format!("accept control at max_token_inputs = {} ({} delegators)", p.max_in, p.max_in - 1));

        for case in v["standard_transfer"]["cases"].as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            assert_eq!(case["result"], "reject");
            assert_eq!(case["signature_dependent"], false);
            if id == "over-max-token-inputs" {
                let vc = std_case(&v, Some(case), &kp);
                if p.max_in == 3 {
                    let why = expect_reject(&p, &std_scn(&vc, &kp), id);
                    t.ok(&format!("{id} (4 inputs > 3) rejected: {why}"));
                } else {
                    // ADAPTED for max_token_inputs = p.max_in: the vector's 4 inputs are legal, max_in + 1 are not.
                    expect_accept(&p, &std_scn(&vc, &kp), "4 token inputs on a 8/8 program");
                    let mut over = std_case(&v, None, &kp);
                    over.delegate_amounts = vec![100; p.max_in];
                    over.next[1].amount = 400 + 100 * p.max_in as i64;
                    let why = expect_reject(&p, &std_scn(&over, &kp), "max_in + 1 inputs");
                    t.adapted(&format!("{id}: 4 inputs accepted, {} inputs rejected: {why}", p.max_in + 1));
                }
                continue;
            }
            let vc = std_case(&v, Some(case), &kp);
            // the case must differ from common only in the listed fields (structural guarantee that the reason class is the one named)
            let base = std_case(&v, None, &kp);
            let mut listed = 0;
            for k in [
                "next_state_amounts",
                "next_state_owner_schemes_hex",
                "witness_hex",
                "next_state_extension_commitments_hex",
                "delegate_amounts",
            ] {
                if case.get(k).is_some() {
                    listed += 1;
                }
            }
            assert_eq!(listed, 1, "{id}: exactly one field family changes");
            assert_ne!((vc.next.clone(), vc.path), (base.next.clone(), base.path));
            let why = expect_reject(&p, &std_scn(&vc, &kp), id);
            t.ok(&format!("{id} rejected ({}): {why}", case["reason"].as_str().unwrap()));
        }
        t.done();
    }
}

// ---------------------------------------------------------------- owner_witness

fn owner_scn(scheme: u8, kp: &Keypair, other: &Keypair) -> (Scn, Body) {
    // 1000 tokens in the leader plus a 100-token delegator (index 1), so extras sit from index 2 (matches the p2sh vector "0002")
    let mut leader = St { amount: 1000, owner: pk(kp), scheme, borrow: 0, guard: [0; 32], ext: [0; 32] };
    let mut extras = vec![];
    let body = match scheme {
        0 => Body::Schnorr(*kp),
        1 => {
            leader.owner = b3(&pk(kp));
            Body::P2pkhSchnorr(*kp)
        }
        2 => {
            leader.owner = b3(&pk33(kp));
            Body::P2pkhEcdsa(*kp)
        }
        3 => {
            leader.owner = p2sh_hash();
            extras.push(Extra::P2sh);
            Body::Index(2)
        }
        4 => {
            leader.owner = OWNER_COV.as_bytes();
            extras.push(Extra::CovOwner);
            Body::Empty
        }
        _ => unreachable!(),
    };
    let delegate = (St { amount: 100, owner: pk(other), scheme: 0, borrow: 0, guard: [0; 32], ext: [0; 32] }, Body::Schnorr(*other));
    let scn = Scn {
        leader: leader.clone(),
        leader_kas: 10 * KAS,
        path: 0,
        body: body.clone(),
        delegates: vec![delegate],
        extras,
        next: vec![St { amount: 1100, owner: [0x22; 32], scheme: 0, borrow: 0, guard: [0; 32], ext: [0; 32] }],
        succ_kas: None,
    };
    (scn, body)
}

#[test]
fn conformance_owner_witness() {
    let v = vectors();
    let kp = keypair();
    let other = keypair();
    for p in programs() {
        let mut t = Tally::new(p.name, "owner_witness");
        for w in v["owner_witness"].as_array().unwrap() {
            let scheme = hb(&w["owner_scheme_hex"]);
            let name = w["scheme"].as_str().unwrap();
            let vw = hx(&w["witness_hex"]);
            assert_eq!(vw.len() as u64, w["length"].as_u64().unwrap(), "{name}: vector length field");
            let (scn, body) = owner_scn(scheme, &kp, &other);
            // layout: path byte 0x00 ++ body; same length as the vector's witness
            assert_eq!(1 + body.len(), vw.len(), "{name}: witness length");
            assert_eq!(vw[0], 0x00, "{name}: path byte");
            match (&body, scheme) {
                (Body::Index(i), 3) => assert_eq!(vec![0u8, *i], vw, "{name}: exact witness"),
                (Body::Empty, 4) => assert_eq!(vec![0u8], vw, "{name}: exact witness"),
                _ => {}
            }
            // P2PKH hash vectors are unkeyed BLAKE3 of the pubkey bytes at the layout's pubkey position
            if let Some(h) = w.get("p2pkh_hash_hex") {
                let pubkey_bytes = match scheme {
                    1 => &vw[1..33],
                    2 => &vw[1..34],
                    _ => unreachable!(),
                };
                assert_eq!(hexs(&b3(pubkey_bytes)), h.as_str().unwrap(), "{name}: p2pkh hash is unkeyed BLAKE3 of the pubkey");
                assert_ne!(hexs(blake3::keyed_hash(&[0u8; 32], pubkey_bytes).as_bytes()), h.as_str().unwrap());
            }
            expect_accept(&p, &scn, name);
            // negative controls: the program checks what the vector layout says it checks
            match scheme {
                0 => {
                    let mut s = scn.clone();
                    s.body = Body::Schnorr(other);
                    expect_reject(&p, &s, "wrong signer");
                }
                1 => {
                    let mut s = scn.clone();
                    s.leader.owner = *blake3::keyed_hash(&[0u8; 32], &pk(&kp)).as_bytes();
                    expect_reject(&p, &s, "keyed P2PKH hash (the pre-707acca bug) must not authorise");
                    let mut s = scn.clone();
                    s.leader.owner = b3(&pk(&other));
                    expect_reject(&p, &s, "wrong pubkey hash");
                }
                2 => {
                    let mut s = scn.clone();
                    s.leader.owner = *blake3::keyed_hash(&[0u8; 32], &pk33(&kp)).as_bytes();
                    expect_reject(&p, &s, "keyed P2PKH hash must not authorise");
                    let mut s = scn.clone();
                    s.body = Body::P2pkhEcdsa(other);
                    expect_reject(&p, &s, "wrong pubkey");
                }
                3 => {
                    let mut s = scn.clone();
                    s.body = Body::Index(0);
                    expect_reject(&p, &s, "authority index is not a P2SH(owner) input");
                }
                4 => {
                    let mut s = scn.clone();
                    s.extras.clear();
                    expect_reject(&p, &s, "no covenant-id owner input in the transaction");
                }
                _ => unreachable!(),
            }
            t.ok(&format!(
                "{name} (scheme 0x{scheme:02x}): witness length {} executed and accepted; negative controls reject",
                vw.len()
            ));
        }
        t.done();
    }
}

// ---------------------------------------------------------------- borrow_witness, hash_chain

#[test]
fn conformance_borrow_witness_and_hash_chain() {
    let v = vectors();
    let kp = keypair();
    let other = keypair();
    let hc = &v["hash_chain"];
    // ---- hash chain vectors: x_i = blake3(x_{i-1} ++ pubkey_i), initial guard = x_3
    let mut prev = h32(&hc["x_0_hex"]);
    let mut last = prev;
    for l in hc["links"].as_array().unwrap() {
        let pubkey = h32(&l["pubkey_hex"]);
        let x = b3(&[prev.to_vec(), pubkey.to_vec()].concat());
        assert_eq!(hexs(&x), l["x_hex"].as_str().unwrap(), "hash-chain link {}", l["i"]);
        prev = x;
        last = x;
    }
    assert_eq!(hexs(&last), hc["initial_borrow_guard_hex"].as_str().unwrap());
    let x1 = h32(&hc["links"][0]["x_hex"]);
    let x2 = h32(&hc["links"][1]["x_hex"]);
    let x3 = h32(&hc["links"][2]["x_hex"]);
    let pk3_vec = h32(&hc["links"][2]["pubkey_hex"]);
    // the mismatch case's revealed link hash
    let bad = h32(&v["borrowed_receive"]["cases"][8]["revealed_link_hash_hex"]);
    assert_eq!(bad, b3(&[x1.to_vec(), pk3_vec.to_vec()].concat()), "revealed_link_hash of the mismatch case");
    assert_ne!(bad, x3);

    for p in programs() {
        let mut t = Tally::new(p.name, "borrow_witness");
        for w in v["borrow_witness"].as_array().unwrap() {
            let scheme = hb(&w["borrow_scheme_hex"]);
            let name = w["scheme"].as_str().unwrap();
            let vw = hx(&w["witness_hex"]);
            assert_eq!(vw.len() as u64, w["length"].as_u64().unwrap());
            assert_eq!(vw[0], 0x01, "{name}: path byte 0x01 = borrow");
            // leader 1000 (owner scheme 01, owner never authenticated on the borrow path); a delegator supplies the added tokens
            let mut leader = St { amount: 1000, owner: [0x11; 32], scheme: 1, borrow: scheme, guard: [0; 32], ext: [0; 32] };
            let body = match scheme {
                1 => {
                    leader.guard = {
                        let mut g = [0u8; 32];
                        g[..8].copy_from_slice(&500i64.to_le_bytes());
                        g
                    };
                    Body::Empty
                }
                2 => {
                    leader.guard = pk(&kp);
                    Body::Schnorr(kp)
                }
                3 => {
                    leader.guard = b3(&[x2.to_vec(), pk(&kp).to_vec()].concat());
                    Body::HashChain { next: x2, kp }
                }
                _ => unreachable!(),
            };
            assert_eq!(1 + body.len(), vw.len(), "{name}: witness length");
            if scheme == 3 {
                // layout: 01 ++ next_borrow_guard(32) ++ one-time pubkey(32) ++ signature(65); the vector reveals x_2
                assert_eq!(&vw[1..33], &x2);
            }
            let mut succ = leader.clone();
            succ.amount = 1501;
            if scheme == 3 {
                succ.guard = x2;
            }
            let scn = Scn {
                leader,
                leader_kas: 10 * KAS,
                path: 0x01,
                body,
                delegates: vec![(
                    St { amount: 501, owner: pk(&other), scheme: 0, borrow: 0, guard: [0; 32], ext: [0; 32] },
                    Body::Schnorr(other),
                )],
                extras: vec![],
                next: vec![succ],
                succ_kas: None,
            };
            expect_accept(&p, &scn, name);
            t.ok(&format!("{name} (borrow scheme 0x{scheme:02x}): witness length {} executed and accepted", vw.len()));
        }
        t.done();
        let mut t = Tally::new(p.name, "hash_chain");
        t.ok("x_0 .. x_3 recomputed (blake3(x_{i-1} ++ pubkey_i)); initial_borrow_guard = x_3; revealed_link_hash of the mismatch case recomputed");
        t.done();
    }
}

// ---------------------------------------------------------------- borrowed_receive

#[test]
fn conformance_borrowed_receive() {
    let v = vectors();
    let br = &v["borrowed_receive"];
    let common = &br["common"];
    let x2 = h32(&v["hash_chain"]["links"][1]["x_hex"]);
    let vec_initial = h32(&v["hash_chain"]["initial_borrow_guard_hex"]);
    let kp = keypair();
    let other = keypair();
    let leader_amount = common["leader_amount"].as_i64().unwrap();
    // the vector's chain guard (x_3 = blake3(x_2 ++ b3..)) names a placeholder one-time key; with a real one-time key the guard
    // for the same revealed link x_2 is G = blake3(x_2 ++ pubkey)
    let real_guard = b3(&[x2.to_vec(), pk(&kp).to_vec()].concat());
    let map_guard = |g: [u8; 32]| if g == vec_initial { real_guard } else { g };

    for p in programs() {
        let mut t = Tally::new(p.name, "borrowed_receive");
        for case in br["cases"].as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            let borrow = hb(&case["borrow_scheme_hex"]);
            let leader = St {
                amount: leader_amount,
                owner: h32(&common["owner_hex"]),
                scheme: hb(&common["owner_scheme_hex"]),
                borrow,
                guard: map_guard(h32(&case["leader_borrow_guard_hex"])),
                ext: h32(&common["extension_commitment_hex"]),
            };
            let mut succ = leader.clone();
            succ.amount = case["successor_amount"].as_i64().unwrap();
            succ.guard = map_guard(h32(&case["successor_borrow_guard_hex"]));
            if let Some(o) = case.get("successor_owner_hex") {
                succ.owner = h32(o);
            }
            let vw = hx(&case["witness_hex"]);
            let path = vw[0];
            assert_eq!(path, 0x01);
            let body = if borrow == 3 {
                assert_eq!(vw.len(), 130, "{id}: hash-chain witness layout");
                Body::HashChain { next: vw[1..33].try_into().unwrap(), kp }
            } else if borrow == 2 {
                Body::Schnorr(kp)
            } else {
                assert_eq!(vw.len(), 1, "{id}: path-only witness");
                Body::Empty
            };
            let leader_kas = case.get("leader_kas_value_sompi").map(|x| x.as_i64().unwrap()).unwrap_or(10 * KAS);
            let succ_kas = case.get("successor_kas_value_sompi").map(|x| x.as_i64().unwrap());
            let added = succ.amount - leader_amount;
            assert!(added > 0);
            let scn = Scn {
                leader: leader.clone(),
                leader_kas,
                path,
                body,
                delegates: vec![(
                    St { amount: added, owner: pk(&other), scheme: 0, borrow: 0, guard: [0; 32], ext: leader.ext },
                    Body::Schnorr(other),
                )],
                extras: vec![],
                next: vec![succ],
                succ_kas,
            };
            match case["result"].as_str().unwrap() {
                "accept" => {
                    expect_accept(&p, &scn, id);
                    t.ok(&format!("{id} accepted (signature_dependent={})", case["signature_dependent"]));
                }
                "reject" => {
                    let why = expect_reject(&p, &scn, id);
                    t.ok(&format!("{id} rejected ({}): {why}", case["reason"].as_str().unwrap()));
                }
                r => panic!("unknown result {r}"),
            }
        }
        t.done();
    }
}

// ---------------------------------------------------------------- KCC-2 authority vectors in the programs

const KCC2_VECTORS: &str = include_str!("../vectors/kcc2/authority-schemes.json");

/// The secp256k1 key with this secret scalar (big-endian hex, left-padded).
fn key_of(secret_hex: &str) -> Keypair {
    let mut b = [0u8; 32];
    let h = decode_hex(&format!("{secret_hex:0>64}")).expect("hex");
    b.copy_from_slice(&h);
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&b).expect("secret"))
}

/// The leader's verdict alone (`Ok` when input 0 passes): KCC-2 cases test the owner authority the leader checks.
fn leader_verdict(p: &Prog, s: &Scn) -> Result<(), String> {
    let (tx, entries) = build(p, s);
    let res = execute(&tx, &entries)?;
    res[0].as_ref().map(|_| ()).map_err(|e| format!("{e:?}"))
}

/// kaspanet/kccs 411b41b `kcc-0002/vectors/authority-schemes.json`: every construction is the authority KOB's KCC-20
/// programs check for that `owner_scheme`, every approval check gives the vector's verdict when the same case is executed
/// as a real normal-path transfer, and every registry byte is accepted as a successor `owner_scheme` exactly when assigned.
///
/// The vectors' keys are generator multiples: `79be...` = 1G (x-only and `02`-compressed), `c6047f...` = 2G, `0379be...`
/// = -G (secret n-1). `signature_valid: false` is a signature by an unrelated key (secret 3) under the stated key. Covenant
/// ids are fixtures: the active input's id is the token's own lineage, any other id a separate covenant input.
/// `covenant-output-only` is checked only by the pure function (an output cannot carry another lineage's id without an
/// input of it or a KIP-20 genesis, so the case cannot be built as a valid KIP-20 transaction).
#[test]
fn kcc2_authority_vectors_in_kcc20_programs() {
    let v: Value = serde_json::from_str(KCC2_VECTORS).expect("authority-schemes.json");
    let keys: Vec<Keypair> =
        ["1", "2", "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364140"].iter().map(|s| key_of(s)).collect();
    let unrelated = key_of("3");
    let signer_for = |public_key: &[u8], valid: bool| -> Keypair {
        if !valid {
            return unrelated;
        }
        *keys
            .iter()
            .find(|k| pk(k).as_slice() == public_key || pk33(k).as_slice() == public_key)
            .unwrap_or_else(|| panic!("no secret for {}", hexs(public_key)))
    };
    let constructions: BTreeMap<String, &Value> =
        v["constructions"].as_array().unwrap().iter().map(|c| (c["id"].as_str().unwrap().to_string(), c)).collect();
    assert_eq!(hexs(&p2sh_hash()), constructions["p2sh"]["authority_value"].as_str().unwrap(), "harness P2SH(OP_TRUE) is the vector");
    let base = |owner: [u8; 32], scheme: u8, body: Body, extras: Vec<Extra>| Scn {
        leader: St { amount: 1000, owner, scheme, borrow: 0, guard: [0; 32], ext: [0; 32] },
        leader_kas: 10 * KAS,
        path: 0,
        body,
        delegates: vec![],
        extras,
        next: vec![St { amount: 1000, owner: [0x22; 32], scheme: 0, borrow: 0, guard: [0; 32], ext: [0; 32] }],
        succ_kas: None,
    };
    for p in programs() {
        let mut t = Tally::new(p.name, "kcc2_authority");
        for c in v["approval_checks"].as_array().unwrap() {
            let id = c["id"].as_str().unwrap();
            let con = constructions[c["construction"].as_str().unwrap()];
            let scheme = hb(&con["scheme_byte"]);
            let authority = h32(&con["authority_value"]);
            let ctx = &c["context"];
            let expected = c["expected"].as_bool().unwrap();
            let scn = match scheme {
                0..=2 => {
                    let key = hx(&ctx["public_key"]);
                    let signer = signer_for(&key, ctx["signature_valid"].as_bool().unwrap());
                    let body = match scheme {
                        0 => Body::Schnorr(signer),
                        _ => Body::Raw { key, signer, ecdsa: scheme == 2 },
                    };
                    base(authority, scheme, body, vec![])
                }
                3 => {
                    let spks: Vec<ScriptPublicKey> = ctx["input_script_public_keys"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| ScriptPublicKey::new(s["version"].as_u64().unwrap() as u16, hx(&s["script"]).into()))
                        .collect();
                    let want = p2sh_spk();
                    let at = spks.iter().position(|s| *s == want).unwrap_or(0);
                    // inputs: the token leader at 0, the listed inputs from 1; the witness names the matching one (or the first)
                    base(authority, scheme, Body::Index(1 + at as u8), spks.into_iter().map(Extra::Spk).collect())
                }
                4 => {
                    let active = ctx["active_input_index"].as_u64().unwrap() as usize;
                    let ids = ctx["input_covenant_ids"].as_array().unwrap();
                    let own = ids[active].as_str().map(|s| h32(&Value::from(s)));
                    let translate = |x: [u8; 32]| if Some(x) == own { TOKEN_COV.as_bytes() } else { x };
                    if !ctx["output_covenant_ids"].as_array().unwrap().is_empty() {
                        assert!(!expected, "{id}");
                        t.ok(&format!("{id}: refused by construction (OpCovInputCount counts inputs only; a KIP-20 output cannot carry another lineage's id here; the pure check is in kcc2_conformance_tests)"));
                        continue;
                    }
                    let extras = ids
                        .iter()
                        .enumerate()
                        .filter(|(i, x)| *i != active && !x.is_null())
                        .map(|(_, x)| Extra::Cov(Hash::from_bytes(translate(h32(x)))))
                        .collect();
                    base(translate(authority), scheme, Body::Empty, extras)
                }
                _ => unreachable!(),
            };
            let verdict = leader_verdict(&p, &scn);
            assert_eq!(verdict.is_ok(), expected, "{}: {id}: expected {expected}, leader {verdict:?}", p.name);
            t.ok(&format!("{id} (scheme 0x{scheme:02x}): {}", if expected { "approved" } else { "refused" }));
        }
        // registry: a normal p2pk transfer whose successor carries each registry byte as its owner scheme
        let kp = keys[0];
        for r in v["registry_checks"].as_array().unwrap() {
            let b = hb(&r["scheme_byte"]);
            let mut scn = base(pk(&kp), 0, Body::Schnorr(kp), vec![]);
            scn.next[0].scheme = b;
            let verdict = leader_verdict(&p, &scn);
            assert_eq!(verdict.is_ok(), r["assigned"].as_bool().unwrap(), "{}: successor owner_scheme {b:#04x}: {verdict:?}", p.name);
            t.ok(&format!("successor owner_scheme {b:#04x}: {}", if verdict.is_ok() { "accepted (assigned)" } else { "rejected" }));
        }
        t.done();
    }
}
