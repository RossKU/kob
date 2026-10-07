//! KCC-1 conformance: kaspanet/kccs `main` 411b41b `kcc-0001/vectors/conformance.json` (vendored unmodified in
//! `crates/kob-tests/vectors/kcc1/`, see PROVENANCE.md there) executed against KOB's implementation:
//!
//! | Vector section | KOB code under test |
//! |---|---|
//! | hash_function | `kob_protocol::kcc1::{hash, hash_keyed}` and the script engine's `OP_BLAKE3` / `OP_BLAKE3_WITH_KEY` (what the programs run) |
//! | canonical_type_names | `kcc1::{type_name, parse_type_name}` |
//! | data_pushes | `kob_protocol::script::push_data` (KOB's argument/redeem push), `kcc1::{push_minimal, push_explicit}`, and the `silverscript-abi` argument and state encoders KOB encodes every program with |
//! | scalar_encodings, composite_encodings | the `silverscript-abi` encoders (argument and state), `kcc1::decode_arguments` / `kcc1::decode_state` round trips |
//! | dispatch_and_arguments, zero_argument_dispatch, record_dispatch | `kcc1::function_signature` / `dispatch_tag`, AND the SilverScript compiler KOB builds its programs with (each vector compiled as a real entrypoint) |
//! | p2sh_envelope, continuation_program | `kob_protocol::script::p2sh_spk`, `kcc1::split_invocation`, `kcc2::p2sh_authority` |
//! | state_encoding, template_views | `silverscript-abi` state encoder, `kcc1::decode_state`, `silverscript_abi::template_hash` |
//! | template_hashes | `silverscript_abi::template_hash` (the hash KOB pins) and an independent recomputation with `kcc1::hash` |
//! | rejection_vectors | `kcc1::{decode_arguments, decode_state, validate_argument_type, check_entrypoints}`, the encoders, the compiler |
//! | hash_committed_virtual_element | `silverscript_abi::encode_struct_payload` (Packed) and `kcc1::hash` |
//!
//! Plus a sweep over every committed KOB program artifact (orders, router): every dispatch tag recomputed from
//! its ABI types under KCC-1 section 3.5.1, every type a KCC-1 type, every template hash recomputed under section 3.7.3.
//!
//! Run: cargo test -p kob-tests --test kcc1_conformance_tests -- --nocapture

mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint, UtxoEntry};
use kaspa_txscript::opcodes::codes::{OpBlake3, OpBlake3WithKey, OpEqual};
use kaspa_txscript::pay_to_script_hash_script;
use kob_protocol::kcc1::{self, Records};
use kob_protocol::{kcc2, script};
use serde_json::Value;
use sha2::{Digest, Sha256};
use silverscript_abi::{
    decode_hex, encode_entry_sig_script, encode_hex, encode_runtime_state_script, encode_struct_payload, template_hash, ArtifactValue,
    CompiledContractArtifact, DispatchTag, FieldArtifact, ParamArtifact, RuntimeFieldArtifact, RuntimeStateArtifact, SilAbiArtifact,
    SilContractArtifact, SilEntryArtifact, StateSpanArtifact, StructArtifact, TypeArtifact,
};
use silverscript_lang::compiler::CompileOptions;

const VECTORS: &str = include_str!("../vectors/kcc1/conformance.json");
/// sha256 of the vendored file (blob c55d6a8dbce3c0191af97f1d43ae44a34c5a9df1 at kaspanet/kccs 411b41b).
const VECTORS_SHA256: &str = "5a8ae724de9031d3390ded7873df74002ec29a771c5fbc56e1e43d2a9ae26b59";

fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("conformance.json parses")
}
fn hx(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap_or_else(|| panic!("expected hex string, got {v}"))).expect("hex")
}
fn hexs(b: &[u8]) -> String {
    encode_hex(b)
}

/// A byte string in the vector format: `{hex}`, `{repeat_hex, count}`, or `{encoded_prefix_hex, payload: unchanged}`.
fn bytes_of(v: &Value, payload: &[u8]) -> Vec<u8> {
    if let Some(h) = v.get("hex") {
        return hx(h);
    }
    if let (Some(r), Some(n)) = (v.get("repeat_hex"), v.get("count")) {
        let b = hx(r);
        assert_eq!(b.len(), 1);
        return vec![b[0]; n.as_u64().unwrap() as usize];
    }
    if let Some(p) = v.get("encoded_prefix_hex") {
        assert_eq!(v["payload"], "unchanged");
        return [hx(p), payload.to_vec()].concat();
    }
    panic!("unknown byte string form {v}")
}

// ------------------------------------------------------------------------------------- vector -> ABI model

fn ty(s: &str) -> TypeArtifact {
    kcc1::parse_type_name(s).unwrap_or_else(|e| panic!("type `{s}`: {e}"))
}

fn records_of(v: &Value) -> Records {
    let mut out = Records::new();
    let list: Vec<&Value> = match (v.get("record"), v.get("records")) {
        (Some(r), _) => vec![r],
        (_, Some(Value::Array(rs))) => rs.iter().collect(),
        _ => vec![],
    };
    for r in list {
        let name = r["name"].as_str().unwrap_or("Anonymous").to_string();
        let fields = r["fields"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, f)| FieldArtifact {
                name: f.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("f{i}")),
                ty: ty(f["type"].as_str().unwrap()),
            })
            .collect();
        out.insert(name, StructArtifact { fields });
    }
    out
}

/// A JSON vector value as an ABI value of type `t`.
fn jv(t: &TypeArtifact, records: &Records, v: &Value) -> ArtifactValue {
    match t {
        TypeArtifact::Int => ArtifactValue::Int(match v {
            Value::String(s) => s.parse().unwrap_or_else(|_| panic!("int {s}")),
            _ => v.as_i64().unwrap(),
        }),
        TypeArtifact::Bool => ArtifactValue::Bool(v.as_bool().unwrap()),
        TypeArtifact::Byte => {
            let b = hx(v);
            assert_eq!(b.len(), 1);
            ArtifactValue::Byte(b[0])
        }
        TypeArtifact::Text => ArtifactValue::Text(v.as_str().unwrap().to_string()),
        TypeArtifact::Bytes | TypeArtifact::Pubkey | TypeArtifact::Sig | TypeArtifact::Datasig | TypeArtifact::FixedBytes { .. } => {
            ArtifactValue::Bytes(hx(v))
        }
        TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item } => {
            ArtifactValue::Array(v.as_array().unwrap().iter().map(|x| jv(item, records, x)).collect())
        }
        TypeArtifact::Struct { name } => ArtifactValue::Object(
            records[name]
                .fields
                .iter()
                .map(|f| {
                    let x = v.get(&f.name).or_else(|| v.get(format!("{}_hex", f.name))).unwrap_or_else(|| panic!("field {}", f.name));
                    (f.name.clone(), jv(&f.ty, records, x))
                })
                .collect(),
        ),
        TypeArtifact::Temporal => unreachable!(),
    }
}

/// The top-level value of a vector entry (`value`, `value_decimal` or `value_hex`).
fn top_value(e: &Value) -> &Value {
    e.get("value").or_else(|| e.get("value_decimal")).or_else(|| e.get("value_hex")).expect("value")
}

/// A one-contract artifact with one entry `e(params...)` and the given runtime state, as the encoders consume it.
fn artifact(records: &Records, params: &[TypeArtifact], state: &[(&str, TypeArtifact)]) -> SilAbiArtifact {
    let entry = SilEntryArtifact {
        dispatch_tag: DispatchTag::from([0xde, 0xad, 0xbe, 0xef]),
        params: params.iter().enumerate().map(|(i, t)| ParamArtifact { name: format!("a{i}"), ty: t.clone() }).collect(),
    };
    let contract = SilContractArtifact {
        source_path: "kcc1-vector".into(),
        runtime_state: RuntimeStateArtifact {
            source: "State".into(),
            fields: state.iter().map(|(n, t)| RuntimeFieldArtifact { name: n.to_string(), ty: t.clone() }).collect(),
        },
        entries: BTreeMap::from([("e".to_string(), entry)]),
        cov_decl_to_abi: BTreeMap::new(),
        delegate_entry_abi: None,
        compiled: CompiledContractArtifact {
            bytecode: vec![0x51],
            template_hash: template_hash(&[], &[0x51]),
            state_span: StateSpanArtifact { offset: 0, len: 0 },
        },
    };
    SilAbiArtifact {
        schema_version: 1,
        compiler_version: "kcc1-vectors".into(),
        structs: records.clone(),
        contracts: BTreeMap::from([("V".to_string(), contract)]),
    }
}

/// silverscript-abi argument encoding of `args` (the dispatch-tag push stripped).
fn encode_args(abi: &SilAbiArtifact, args: &[ArtifactValue]) -> Result<Vec<u8>, String> {
    let c = &abi.contracts["V"];
    let mut s = encode_entry_sig_script(abi, "V", c, "e", &c.entries["e"], args).map_err(|e| e.to_string())?;
    let tail = s.split_off(s.len() - 5);
    assert_eq!(tail, [0x04, 0xde, 0xad, 0xbe, 0xef], "dispatch tag push");
    Ok(s)
}

/// silverscript-abi state encoding (`EncodeState`).
fn encode_state(abi: &SilAbiArtifact, values: &[(&str, ArtifactValue)]) -> Result<Vec<u8>, String> {
    let c = &abi.contracts["V"];
    let m: BTreeMap<String, ArtifactValue> = values.iter().map(|(n, v)| (n.to_string(), v.clone())).collect();
    encode_runtime_state_script(abi, &c.runtime_state, &m).map_err(|e| e.to_string())
}

fn decode_args(records: &Records, types: &[TypeArtifact], script: &[u8]) -> Result<Vec<ArtifactValue>, String> {
    kcc1::decode_arguments(types, records, script).map_err(|e| e.to_string())
}

fn decode_state(abi: &SilAbiArtifact, bytes: &[u8]) -> Result<BTreeMap<String, ArtifactValue>, String> {
    kcc1::decode_state(abi, &abi.contracts["V"].runtime_state, bytes).map_err(|e| e.to_string())
}

/// Executes `script` as a P2SH redeem script in rusty-kaspa v2.1.0's engine (version-1 transaction, compute budget).
fn run_script(redeem: &[u8]) -> Result<(), String> {
    use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
    use kaspa_consensus_core::tx::PopulatedTransaction;
    use kaspa_txscript::caches::Cache;
    use kaspa_txscript::{EngineCtx, EngineFlags, TxScriptEngine};
    let entry = UtxoEntry::new(1_000_000, pay_to_script_hash_script(redeem), 0, false, None);
    let input = TransactionInput::new_with_compute_budget(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([1; 32]), index: 0 },
        script::push_data(redeem),
        0,
        100,
    );
    let tx = Transaction::new(1, vec![input.clone()], vec![], 0, Default::default(), 0, vec![]);
    let populated = PopulatedTransaction::new(&tx, vec![entry.clone()]);
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(100);
    let mut vm = TxScriptEngine::from_transaction_input(
        &populated,
        &input,
        0,
        &entry,
        EngineCtx::new(&cache).with_reused(&reused),
        EngineFlags { sigop_script_units: 100_000.into() },
    );
    vm.execute().map_err(|e| format!("{e:?}"))
}

/// Compiles a SilverScript contract with KOB's vendored compiler.
fn compile(src: &str) -> Result<SilAbiArtifact, String> {
    common::compile_contract(src, &[], CompileOptions::default()).map_err(|e| e.to_string())
}

/// SilverScript spelling of a KCC-1 type (records by name, `byte[N]`, `T[N]`, `T[]`).
fn sil_type(t: &TypeArtifact) -> String {
    match t {
        TypeArtifact::Bytes => "byte[]".into(),
        other => kcc1::type_name(other).unwrap(),
    }
}

/// SilverScript source declaring `records` and one entry per `(name, types)`.
fn sil_source(records: &Records, entries: &[(&str, Vec<TypeArtifact>)]) -> String {
    let mut s = String::from("pragma silverscript ^0.1.0;\n\ncontract KccVectors() {\n");
    for (name, r) in records {
        s += &format!("    struct {name} {{\n");
        for f in &r.fields {
            s += &format!("        {} {};\n", sil_type(&f.ty), f.name);
        }
        s += "    }\n";
    }
    for (name, types) in entries {
        let params: Vec<String> = types.iter().enumerate().map(|(i, t)| format!("{} a{i}", sil_type(t))).collect();
        s += &format!("    entry {name}({}) {{\n        require(true);\n    }}\n", params.join(", "));
    }
    s + "}\n"
}

// ------------------------------------------------------------------------------------------------- tests

#[test]
fn kcc1_vectors_are_the_pinned_upstream_file() {
    assert_eq!(hexs(&Sha256::digest(VECTORS.as_bytes())), VECTORS_SHA256, "vendored KCC-1 vectors drifted from the pin");
    let v = vectors();
    assert_eq!(v["kcc"], 1);
    assert_eq!(v["format_version"], 1);
}

#[test]
fn kcc1_hash_function() {
    let v = vectors();
    for e in v["hash_function"].as_array().unwrap() {
        let id = e["id"].as_str().unwrap();
        let input = hx(&e["input_hex"]);
        let want = hx(&e["hash_hex"]);
        let (got, on_chain) = match e.get("key_hex") {
            None => {
                let s = [script::push_data(&input), vec![OpBlake3], script::push_data(&want), vec![OpEqual]].concat();
                (kcc1::hash(&input).to_vec(), s)
            }
            Some(k) => {
                let key = hx(k);
                let k32 = kcc1::key32(&key).unwrap();
                assert_eq!(hexs(&k32), e["key32_hex"].as_str().unwrap(), "{id}: Key32");
                let s = [
                    script::push_data(&input),
                    script::push_data(&k32),
                    vec![OpBlake3WithKey],
                    script::push_data(&want),
                    vec![OpEqual],
                ]
                .concat();
                (kcc1::hash_keyed(&input, &key).unwrap().to_vec(), s)
            }
        };
        assert_eq!(got, want, "{id}: kcc1 hash");
        run_script(&on_chain).unwrap_or_else(|err| panic!("{id}: the script engine's BLAKE3 opcode disagrees: {err}"));
        println!(
            "hash_function {id}: ok (kcc1 and OP_BLAKE3{} in the engine)",
            if e.get("key_hex").is_some() { "_WITH_KEY" } else { "" }
        );
    }
    assert!(kcc1::key32(&[0u8; 33]).is_err(), "a key longer than 32 bytes is rejected");
}

#[test]
fn kcc1_canonical_type_names() {
    let v = vectors();
    for e in v["canonical_type_names"].as_array().unwrap() {
        let t = match e["type"].as_str().unwrap() {
            "integer" => TypeArtifact::Int,
            "boolean" => TypeArtifact::Bool,
            "byte" => TypeArtifact::Byte,
            "UTF-8 string" => TypeArtifact::Text,
            "32-byte public key" => TypeArtifact::Pubkey,
            "65-byte transaction signature" => TypeArtifact::Sig,
            "64-byte data signature" => TypeArtifact::Datasig,
            "fixed byte string of length 32" => TypeArtifact::FixedBytes { len: 32 },
            "fixed array of three booleans" => TypeArtifact::FixedArray { item: Box::new(TypeArtifact::Bool), len: 3 },
            "dynamic array of integers" => TypeArtifact::DynamicArray { item: Box::new(TypeArtifact::Int) },
            "record named Transfer" => TypeArtifact::Struct { name: "Transfer".into() },
            other => panic!("unknown type description {other}"),
        };
        let name = e["type_name"].as_str().unwrap();
        assert_eq!(kcc1::type_name(&t).unwrap(), name);
        assert_eq!(kcc1::parse_type_name(name).unwrap(), t);
    }
    assert_eq!(kcc1::type_name(&TypeArtifact::Bytes).unwrap(), "byte[]", "SilverScript `bytes` is KCC-1 byte[]");
}

#[test]
fn kcc1_data_pushes() {
    let v = vectors();
    let none = Records::new();
    let arg_abi = artifact(&none, &[TypeArtifact::Bytes], &[("p", TypeArtifact::Bytes)]);
    for e in v["data_pushes"].as_array().unwrap() {
        let id = e["id"].as_str().unwrap();
        let payload = bytes_of(&e["payload"], &[]);
        let minimal = bytes_of(&e["push_minimal"], &payload);
        let explicit = bytes_of(&e["push_explicit"], &payload);
        assert_eq!(kcc1::push_minimal(&payload), minimal, "{id}: kcc1 PushMinimal");
        assert_eq!(kcc1::push_explicit(&payload), explicit, "{id}: kcc1 PushExplicit");
        assert_eq!(script::push_data(&payload), minimal, "{id}: kob_protocol::script::push_data is PushMinimal");
        assert_eq!(encode_args(&arg_abi, &[payload.clone().into()]).unwrap(), minimal, "{id}: silverscript byte[] argument");
        assert_eq!(
            encode_state(&arg_abi, &[("p", payload.clone().into())]).unwrap(),
            explicit,
            "{id}: silverscript byte[] state field"
        );
        assert_eq!(decode_args(&none, &[TypeArtifact::Bytes], &minimal).unwrap(), vec![ArtifactValue::Bytes(payload.clone())], "{id}");
        println!("data_pushes {id}: ok ({} byte payload)", payload.len());
    }
}

#[test]
fn kcc1_scalar_encodings() {
    let v = vectors();
    let none = Records::new();
    for e in v["scalar_encodings"].as_array().unwrap() {
        let t = ty(e["type"].as_str().unwrap());
        let val = jv(&t, &none, top_value(e));
        let arg = hx(&e["standalone_argument_hex"]);
        let st = hx(&e["state_payload_hex"]);
        let abi = artifact(&none, std::slice::from_ref(&t), &[("x", t.clone())]);
        let what = format!("{} {}", e["type"], top_value(e));
        assert_eq!(hexs(&encode_args(&abi, std::slice::from_ref(&val)).unwrap()), hexs(&arg), "{what}: argument");
        let state = encode_state(&abi, &[("x", val.clone())]).unwrap();
        assert_eq!(hexs(&state), hexs(&kcc1::push_explicit(&st)), "{what}: state field");
        assert_eq!(decode_args(&none, std::slice::from_ref(&t), &arg).unwrap(), vec![val.clone()], "{what}: decode argument");
        assert_eq!(decode_state(&abi, &state).unwrap()["x"], val, "{what}: decode state");
        if let ArtifactValue::Int(i) = val {
            assert_eq!(kcc1::push_minimal(&kcc1::int_argument_payload(i).unwrap()), arg, "{what}: kcc1 int argument");
            assert_eq!(kcc1::int_state_payload(i).unwrap().to_vec(), st, "{what}: kcc1 int state payload");
        }
        println!("scalar_encodings {what}: ok");
    }
}

#[test]
fn kcc1_composite_encodings() {
    let v = vectors();
    for e in v["composite_encodings"].as_array().unwrap() {
        let id = e["id"].as_str().unwrap();
        let records = records_of(e);
        let t = match e.get("type") {
            Some(t) => ty(t.as_str().unwrap()),
            None => TypeArtifact::Struct { name: e["record"]["name"].as_str().unwrap().to_string() },
        };
        let val = jv(&t, &records, top_value(e));
        let abi = artifact(&records, std::slice::from_ref(&t), &[]);
        let arg = encode_args(&abi, std::slice::from_ref(&val)).unwrap();
        let want = e.get("argument_hex").or_else(|| e.get("combined_hex")).map(hx).unwrap();
        assert_eq!(hexs(&arg), hexs(&want), "{id}: argument encoding");
        for k in ["lowered_argument_hex", "grouped_argument_hex"] {
            if let Some(parts) = e.get(k) {
                let joined: Vec<u8> = parts.as_array().unwrap().iter().flat_map(hx).collect();
                assert_eq!(joined, want, "{id}: {k} concatenate to the combined encoding");
            }
        }
        if let Some(p) = e.get("payload_hex") {
            let payload = hx(p);
            let sabi = artifact(&records, &[], &[("x", t.clone())]);
            let state = encode_state(&sabi, &[("x", val.clone())]).unwrap();
            assert_eq!(hexs(&state), hexs(&kcc1::push_explicit(&payload)), "{id}: payload (as a state field)");
            assert_eq!(decode_state(&sabi, &state).unwrap()["x"], val, "{id}: decode state");
        }
        assert_eq!(decode_args(&records, std::slice::from_ref(&t), &want).unwrap(), vec![val], "{id}: decode argument");
        println!("composite_encodings {id}: ok");
    }
}

#[test]
fn kcc1_dispatch_tags_and_arguments() {
    let v = vectors();
    let none = Records::new();
    let mut cases: Vec<(String, String, Vec<TypeArtifact>, Records, String, String)> = vec![];
    for (k, e) in [("dispatch_and_arguments", &v["dispatch_and_arguments"]), ("zero_argument_dispatch", &v["zero_argument_dispatch"])]
    {
        cases.push((
            k.to_string(),
            e["entrypoint_name"].as_str().unwrap().to_string(),
            e["argument_types"].as_array().unwrap().iter().map(|t| ty(t.as_str().unwrap())).collect(),
            none.clone(),
            e["function_signature_utf8_hex"].as_str().unwrap().to_string(),
            e["dispatch_tag_hex"].as_str().unwrap().to_string(),
        ));
    }
    for e in v["record_dispatch"].as_array().unwrap() {
        let types: Vec<TypeArtifact> = e["argument_types"].as_array().unwrap().iter().map(|t| ty(t.as_str().unwrap())).collect();
        let records = records_of(e);
        let names: Vec<String> = types.iter().map(|t| kcc1::dispatch_type_name(t, &records).unwrap()).collect();
        let want: Vec<String> = e["dispatch_type_names"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        assert_eq!(names, want, "{}: dispatch type names", e["id"]);
        cases.push((
            format!("record_dispatch {}", e["id"].as_str().unwrap()),
            e["entrypoint_name"].as_str().unwrap().to_string(),
            types,
            records,
            e["function_signature_utf8_hex"].as_str().unwrap().to_string(),
            e["dispatch_tag_hex"].as_str().unwrap().to_string(),
        ));
    }
    for (label, name, types, records, sig_hex, tag_hex) in &cases {
        let sig = kcc1::function_signature(name, types, records).unwrap();
        assert_eq!(hexs(sig.as_bytes()), *sig_hex, "{label}: FunctionSignature");
        assert_eq!(hexs(&kcc1::dispatch_tag(&sig)), *tag_hex, "{label}: dispatch tag");
        // the compiler KOB builds its programs with: the same entrypoint compiled for real
        let src = sil_source(records, &[(name.as_str(), types.clone())]);
        let art = compile(&src).unwrap_or_else(|e| panic!("{label}: compile\n{src}\n{e}"));
        let c = common::single_contract(&art);
        assert_eq!(c.entries[name].dispatch_tag.to_hex(), *tag_hex, "{label}: SilverScript compiler dispatch tag");
        println!("{label}: `{sig}` -> {tag_hex} ok (kcc1 and the SilverScript compiler)");
    }
    // the argument encodings of `step`
    let e = &v["dispatch_and_arguments"];
    let types: Vec<TypeArtifact> = e["argument_types"].as_array().unwrap().iter().map(|t| ty(t.as_str().unwrap())).collect();
    let vals: Vec<ArtifactValue> = types.iter().zip(e["argument_values"].as_array().unwrap()).map(|(t, x)| jv(t, &none, x)).collect();
    let abi = artifact(&none, &types, &[]);
    let enc = encode_args(&abi, &vals).unwrap();
    let want: Vec<u8> = e["argument_encodings_hex"].as_array().unwrap().iter().flat_map(hx).collect();
    assert_eq!(hexs(&enc), hexs(&want), "step: PushArguments");
    assert_eq!(decode_args(&none, &types, &want).unwrap(), vals, "step: decode");
    let z = &v["zero_argument_dispatch"];
    assert_eq!(
        hexs(&script::push_data(&hx(&z["dispatch_tag_hex"]))),
        z["dispatch_tag_push_hex"].as_str().unwrap(),
        "tick: OP_DATA_4 tag"
    );
    assert_eq!(z["push_arguments_hex"], "");
    assert_eq!(decode_args(&none, &[], &[]).unwrap(), vec![], "tick: empty PushArguments");
}

#[test]
fn kcc1_p2sh_envelope() {
    let v = vectors();
    let e = &v["p2sh_envelope"];
    let r = hx(&e["redeem_script_hex"]);
    let spk = script::p2sh_spk(&r);
    assert_eq!(spk.version(), e["script_public_key"]["version"].as_u64().unwrap() as u16);
    assert_eq!(hexs(spk.script()), e["script_public_key"]["script_hex"].as_str().unwrap(), "P2SH script public key");
    assert_eq!(hexs(&kcc2::p2sh_authority(&r)), e["blake2b_256_hex"].as_str().unwrap(), "Blake2b(R)");
    // the step invocation: PushArguments || OP_DATA_4 tag || PushMinimal(R)
    let d = &v["dispatch_and_arguments"];
    let args: Vec<u8> = d["argument_encodings_hex"].as_array().unwrap().iter().flat_map(hx).collect();
    let tag = hx(&d["dispatch_tag_hex"]);
    let sig_script = [args.clone(), script::push_data(&tag), script::push_data(&r)].concat();
    assert_eq!(hexs(&sig_script), e["signature_script_hex"].as_str().unwrap(), "signature script");
    let (a, t, rr) = kcc1::split_invocation(&sig_script).unwrap();
    assert_eq!((a, t.to_vec(), rr), (args.as_slice(), tag, r.clone()));
    // the layout rules: the tag must be OP_DATA_4, R its PushMinimal form
    let mut bad = sig_script.clone();
    bad.truncate(bad.len() - 2);
    bad.extend([0x4c, 0x01, 0x51]);
    assert!(kcc1::split_invocation(&bad).is_err(), "non-minimal redeem push");
    assert!(kcc1::split_invocation(&[0x51, 0x01, 0x51]).is_err(), "missing OP_DATA_4 tag");
    // the envelope accepts exactly R in the engine
    run_script(&r).expect("OP_1 redeem script runs");
}

#[test]
fn kcc1_state_encoding() {
    let v = vectors();
    let e = &v["state_encoding"];
    let none = Records::new();
    let fields: Vec<(String, TypeArtifact, ArtifactValue, Vec<u8>)> = e["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let t = ty(f["type"].as_str().unwrap());
            let val = jv(&t, &none, f.get("value").or_else(|| f.get("value_hex")).unwrap());
            (f["name"].as_str().unwrap().to_string(), t, val, hx(&f["encoded_hex"]))
        })
        .collect();
    let layout: Vec<(&str, TypeArtifact)> = fields.iter().map(|(n, t, _, _)| (n.as_str(), t.clone())).collect();
    let abi = artifact(&none, &[], &layout);
    for (n, t, val, enc) in &fields {
        let one = artifact(&none, &[], &[(n.as_str(), t.clone())]);
        assert_eq!(hexs(&encode_state(&one, &[(n, val.clone())]).unwrap()), hexs(enc), "field {n}");
    }
    let values: Vec<(&str, ArtifactValue)> = fields.iter().map(|(n, _, v, _)| (n.as_str(), v.clone())).collect();
    let enc = encode_state(&abi, &values).unwrap();
    assert_eq!(hexs(&enc), e["encoded_hex"].as_str().unwrap(), "EncodeState");
    assert_eq!(enc.len() as u64, e["encoded_length"].as_u64().unwrap());
    let back = decode_state(&abi, &enc).unwrap();
    for (n, _, val, _) in &fields {
        assert_eq!(&back[n], val, "decode {n}");
    }
}

#[test]
fn kcc1_template_hashes_views_and_continuation() {
    let v = vectors();
    let independent = |p: &[u8], s: &[u8]| {
        kcc1::hash(
            &[(p.len() as u64).to_le_bytes().to_vec(), p.to_vec(), (s.len() as u64).to_le_bytes().to_vec(), s.to_vec()].concat(),
        )
    };
    for e in v["template_hashes"].as_array().unwrap() {
        let (p, s) = (hx(&e["prefix_hex"]), hx(&e["suffix_hex"]));
        let want = e["hash_hex"].as_str().unwrap();
        assert_eq!(hexs(&template_hash(&p, &s)), want, "silverscript template_hash({}, {})", hexs(&p), hexs(&s));
        assert_eq!(hexs(&independent(&p, &s)), want, "Hash(LE64 || prefix || LE64 || suffix)");
        assert_ne!(hexs(&kcc1::hash(&[p, s].concat())), want, "not Hash(prefix || suffix)");
    }
    // template views
    let tv = &v["template_views"];
    let r = hx(&tv["program_hex"]);
    let (start, len) = (tv["state_start"].as_u64().unwrap() as usize, tv["state_len"].as_u64().unwrap() as usize);
    let none = Records::new();
    let fields: BTreeMap<String, (TypeArtifact, Vec<u8>)> = tv["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["name"].as_str().unwrap().to_string(), (ty(f["type"].as_str().unwrap()), hx(&f["encoded_hex"]))))
        .collect();
    let all: Vec<u8> = tv["fields"].as_array().unwrap().iter().flat_map(|f| hx(&f["encoded_hex"])).collect();
    assert_eq!(&r[start..start + len], all.as_slice(), "complete encoded state");
    for view in tv["views"].as_array().unwrap() {
        let (vs, vl) = (view["start"].as_u64().unwrap() as usize, view["len"].as_u64().unwrap() as usize);
        assert!(start <= vs && vs + vl <= start + len, "view inside the state");
        let (p, enc, s) = (&r[..vs], &r[vs..vs + vl], &r[vs + vl..]);
        assert_eq!(hexs(p), view["prefix_hex"].as_str().unwrap());
        assert_eq!(hexs(enc), view["encoded_state_hex"].as_str().unwrap());
        assert_eq!(hexs(s), view["suffix_hex"].as_str().unwrap());
        assert_eq!(hexs(&template_hash(p, s)), view["template_hash_hex"].as_str().unwrap(), "view template hash");
        // the view selects complete consecutive fields: decoding it as exactly those fields succeeds
        let names: Vec<&str> = view["fields"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        let layout: Vec<(&str, TypeArtifact)> = names.iter().map(|n| (*n, fields[*n].0.clone())).collect();
        decode_state(&artifact(&none, &[], &layout), enc).unwrap_or_else(|e| panic!("view {names:?}: {e}"));
        let concat: Vec<u8> = names.iter().flat_map(|n| fields[*n].1.clone()).collect();
        assert_eq!(concat, enc, "view {names:?} is the concatenation of its fields");
    }
    // continuation under the `b` view
    let c = &v["continuation_program"];
    let names: Vec<&str> = c["view_fields"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    let layout: Vec<(&str, TypeArtifact)> = names.iter().map(|n| (*n, fields[*n].0.clone())).collect();
    let abi = artifact(&none, &[], &layout);
    let next: Vec<(&str, ArtifactValue)> = names.iter().map(|n| (*n, jv(&fields[*n].0, &none, &c["next_state"][*n]))).collect();
    let enc = encode_state(&abi, &next).unwrap();
    assert_eq!(hexs(&enc), c["encoded_next_state_hex"].as_str().unwrap(), "EncodeState(next_state)");
    let r_next = [hx(&c["prefix_hex"]), enc, hx(&c["suffix_hex"])].concat();
    assert_eq!(hexs(&r_next), c["redeem_script_hex"].as_str().unwrap(), "R_next");
    assert_eq!(hexs(&kcc2::p2sh_authority(&r_next)), c["blake2b_256_hex"].as_str().unwrap(), "Blake2b(R_next)");
    let spk: ScriptPublicKey = script::p2sh_spk(&r_next);
    assert_eq!(spk.version() as u64, c["script_public_key"]["version"].as_u64().unwrap());
    assert_eq!(hexs(spk.script()), c["script_public_key"]["script_hex"].as_str().unwrap(), "continuation script public key");
}

#[test]
fn kcc1_hash_committed_virtual_element() {
    let v = vectors();
    let e = &v["hash_committed_virtual_element"];
    let none = Records::new();
    let fields: Vec<(String, TypeArtifact, ArtifactValue)> = e["record"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let t = ty(f["type"].as_str().unwrap());
            (format!("f{i}"), t.clone(), jv(&t, &none, &f["value"]))
        })
        .collect();
    let records = Records::from([(
        "Value".to_string(),
        StructArtifact { fields: fields.iter().map(|(n, t, _)| FieldArtifact { name: n.clone(), ty: t.clone() }).collect() },
    )]);
    let abi = artifact(&records, &[], &[]);
    let values: BTreeMap<String, ArtifactValue> = fields.iter().map(|(n, _, v)| (n.clone(), v.clone())).collect();
    let packed = encode_struct_payload(&abi, &abi.contracts["V"], "Value", &values).unwrap();
    assert_eq!(hexs(&packed), e["packed_hex"].as_str().unwrap(), "Packed(value)");
    assert_eq!(hexs(&kcc1::hash(&packed)), e["commitment_hex"].as_str().unwrap(), "Hash(Packed(value))");
    let mut off = 0;
    for (n, t, val) in &fields {
        let w = kcc1::fixed_width(t).unwrap();
        assert_eq!(&kcc1::decode_element(t, &packed[off..off + w]).unwrap(), val, "opening field {n}");
        off += w;
    }
    assert_eq!(off, packed.len());
}

#[test]
fn kcc1_rejection_vectors() {
    let v = vectors();
    let mut n = 0;
    for e in v["rejection_vectors"].as_array().unwrap() {
        let id = e["id"].as_str().unwrap();
        assert_eq!(e["expected"], "reject");
        let records = records_of(e);
        let why = match e["operation"].as_str().unwrap() {
            "encode_scalar" => {
                let t = ty(e["type"].as_str().unwrap());
                assert_eq!(t, TypeArtifact::Int, "{id}");
                match e["value_decimal"].as_str().unwrap().parse::<i64>() {
                    Err(_) => "not representable as a 64-bit integer".to_string(),
                    Ok(x) => {
                        assert!(kcc1::int_argument_payload(x).is_none() && kcc1::int_state_payload(x).is_none(), "{id}: kcc1");
                        let abi = artifact(&Records::new(), &[TypeArtifact::Int], &[("x", TypeArtifact::Int)]);
                        let a = encode_args(&abi, &[ArtifactValue::Int(x)]).expect_err("silverscript argument encoder must reject");
                        let s =
                            encode_state(&abi, &[("x", ArtifactValue::Int(x))]).expect_err("silverscript state encoder must reject");
                        format!("argument: {a}; state: {s}")
                    }
                }
            }
            "decode_arguments" => {
                let types: Vec<TypeArtifact> =
                    e["argument_types"].as_array().unwrap().iter().map(|t| ty(t.as_str().unwrap())).collect();
                decode_args(&records, &types, &hx(&e["argument_hex"])).expect_err(id)
            }
            "decode_state" => {
                let layout: Vec<(String, TypeArtifact)> = e["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| (f["name"].as_str().unwrap().to_string(), ty(f["type"].as_str().unwrap())))
                    .collect();
                let layout: Vec<(&str, TypeArtifact)> = layout.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
                decode_state(&artifact(&records, &[], &layout), &hx(&e["encoded_hex"])).expect_err(id)
            }
            "validate_argument_type" => {
                let t = ty(e["type"].as_str().unwrap());
                let err = kcc1::validate_argument_type(&t, &records).expect_err(id).to_string();
                // informational: what the vendored SilverScript compiler does with such an entrypoint (KOB declares none;
                // the artifact sweep below validates every KOB entry type with kcc1)
                let compiler = match compile(&sil_source(&records, &[("probe", vec![t.clone()])])) {
                    Ok(_) => "the SilverScript compiler ACCEPTS it (upstream gap, no KOB program uses such a type)".to_string(),
                    Err(c) => format!("compiler rejects: {c}"),
                };
                format!("{err}; {compiler}")
            }
            "validate_entrypoints" => {
                let entries: Vec<(String, Vec<TypeArtifact>)> = e["entrypoints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| {
                        (
                            x["name"].as_str().unwrap().to_string(),
                            x["argument_types"].as_array().unwrap().iter().map(|t| ty(t.as_str().unwrap())).collect(),
                        )
                    })
                    .collect();
                let sigs: Vec<String> = entries.iter().map(|(n, t)| kcc1::function_signature(n, t, &records).unwrap()).collect();
                let want: Vec<String> =
                    e["function_signatures"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
                assert_eq!(sigs, want, "{id}: signatures");
                for s in &sigs {
                    assert_eq!(hexs(&kcc1::dispatch_tag(s)), e["dispatch_tag_hex"].as_str().unwrap(), "{id}: colliding tag");
                }
                let err = kcc1::check_entrypoints(&entries, &records).expect_err(id).to_string();
                // the compiler KOB builds with refuses the program too (distinct names: the truncated collision)
                if entries[0].0 != entries[1].0 {
                    let refs: Vec<(&str, Vec<TypeArtifact>)> = entries.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
                    let c = compile(&sil_source(&records, &refs))
                        .expect_err("the SilverScript compiler must reject colliding dispatch tags");
                    format!("{err}; compiler: {c}")
                } else {
                    err
                }
            }
            other => panic!("{id}: unknown operation {other}"),
        };
        n += 1;
        println!("rejection {id} ({}): rejected: {why}", e["rule"].as_str().unwrap());
    }
    assert_eq!(n, v["rejection_vectors"].as_array().unwrap().len());
}

/// Every committed KOB program artifact: dispatch tags recomputed from the ABI types (KCC-1 3.5.1), every argument and
/// state type a KCC-1 type, template hashes recomputed (3.7.3), state spans canonical (3.7.1).
#[test]
fn kcc1_every_kob_artifact_conforms() {
    let root = common::repo_root();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("contracts/artifacts"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files.push(root.join("crates/kob-protocol/data/router_sil_abi.json"));
    let (mut entries, mut programs) = (0, 0);
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().display().to_string();
        let abi: SilAbiArtifact = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{rel}: {e}"));
        abi.check_consistency().unwrap_or_else(|e| panic!("{rel}: {e}"));
        for (cn, c) in &abi.contracts {
            programs += 1;
            let records = kcc1::contract_records(&abi, c);
            let mut eps = vec![];
            for (name, sig, tag, artifact_tag) in kcc1::contract_dispatch_tags(&abi, c).unwrap_or_else(|e| panic!("{rel} {cn}: {e}")) {
                assert_eq!(tag, artifact_tag, "{rel} {cn}.{name}: dispatch tag of `{sig}`");
                let e = &c.entries[&name];
                for p in &e.params {
                    kcc1::validate_argument_type(&p.ty, &records).unwrap_or_else(|err| panic!("{rel} {cn}.{name}({}): {err}", p.name));
                }
                eps.push((name, e.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>()));
                entries += 1;
            }
            kcc1::check_entrypoints(&eps, &records).unwrap_or_else(|e| panic!("{rel} {cn}: {e}"));
            for fld in &c.runtime_state.fields {
                match kcc1::type_name(&fld.ty) {
                    Ok(_) => {}
                    // the Argent router's `temporal` state fields: SilverScript's time type, encoded exactly as a KCC-1
                    // `int` (8-byte signed magnitude) but named `temporal` in the ABI (see docs/spec/kcc-conformance.md)
                    Err(_) if rel.ends_with("router_sil_abi.json") && fld.ty == TypeArtifact::Temporal => {}
                    Err(e) => panic!("{rel} {cn} state field {}: {e}", fld.name),
                }
            }
            let (p, _, s) = c.compiled.script_parts(&c.compiled.bytecode).unwrap();
            let independent = kcc1::hash(
                &[(p.len() as u64).to_le_bytes().to_vec(), p.to_vec(), (s.len() as u64).to_le_bytes().to_vec(), s.to_vec()].concat(),
            );
            assert_eq!(independent, c.compiled.template_hash, "{rel} {cn}: template hash");
            let (_, st, _) = c.compiled.script_parts(&c.compiled.bytecode).unwrap();
            let mut abi_t = abi.clone();
            // the router state's temporal fields decode as their int encoding
            for x in abi_t.contracts.values_mut().flat_map(|c| c.runtime_state.fields.iter_mut()) {
                if x.ty == TypeArtifact::Temporal {
                    x.ty = TypeArtifact::Int;
                }
            }
            kcc1::decode_state(&abi_t, &abi_t.contracts[cn].runtime_state, st)
                .unwrap_or_else(|e| panic!("{rel} {cn}: state span: {e}"));
        }
    }
    println!("{programs} programs, {entries} entrypoints: dispatch tags, types, template hashes and state spans conform to KCC-1");
    assert!(programs >= 30 && entries >= 60, "sweep coverage ({programs} programs, {entries} entries)");
}
