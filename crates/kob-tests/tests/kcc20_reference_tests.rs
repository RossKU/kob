//! The KCC-20 reference program as merged upstream (argent-lang/kcc20-reference `c8a0871`, vendored in
//! `contracts/third-party/kcc20-reference/`, see `UPSTREAM.md` there) and its two builds:
//!
//!   standalone    `contracts/kcc20/KCC20Ref.sil`: `kcc20.ag` as a single-actor app, the program KOB vendors (its
//!                 slot-limit variants, `KCC20P2`, `KCC20Opt` and `KOBToken` derive from it); state 1+112 B
//!   public mint   `KCC20.public-mint.sil`: the `KCC20` actor of upstream's `KCC20PublicMint` app, the only build
//!                 upstream publishes; state 1+145 B (compiler-owned `gen__kcc20_template` first), template-checked reads
//!
//! Executed in rusty-kaspa v2.1.0's TxScriptEngine with KIP-20 covenant context. What it pins:
//!   - the vendored files are the upstream blobs (sha256), and `KCC20Ref.sil` names that commit;
//!   - both builds compile with KOB's silverc to the template hashes recorded in `UPSTREAM.md`, with the same dispatch tags;
//!   - per-transaction capacity: with the public-mint build every holder input checks that the leader is a `KCC20`, and a
//!     `KCC20` leader takes at most 2 delegates and 3 outputs, so no transaction carries more than 3 token inputs or 3 token
//!     outputs of one token; a 16-holder batch through any other leader is refused by every holder. The standalone build
//!     has the same 3 / 3 bounds on its own leader, but its delegator does not check the leader, so a program that the
//!     token's genesis placed in the covenant family can lead 16 standalone holders (`kcc20_opt_tests.rs`,
//!     `reference_delegator_accepts_any_lineage_leader`).
//!
//! Run: cargo test --release -p kob-tests --test kcc20_reference_tests -- --nocapture

mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::tx::{
    CovenantBinding, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::OpTrue;
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use sha2::{Digest, Sha256};
use silverscript_abi::{encode_runtime_state_script, ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use common::{bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
/// The covenant id that owns every holder state here (owner scheme 0x04); an input carrying it authorises them.
const OWNER_COV: Hash = Hash::from_bytes([0xc0; 32]);
const BUDGET: u16 = 300;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;

const UPSTREAM_COMMIT: &str = "c8a087117735a1f87c5c6d115fcddeaf2562c784";
const KCC20_AG_SHA256: &str = "f6131d97368461e74b02507d5ef2d7af57c76226cf945ebbbe9735f0953414e9";
const PUBLIC_MINT_SIL_SHA256: &str = "9c61d62a1b90ad541bbf5c48ec53faaff1826cbc6f26bd42fa429df5c295316c";
const PUBLIC_MINT_ARTIFACT_SHA256: &str = "a419f2aa5f18f2917bf1cd69fbf04942ef746fc832f8e1fcd7625582d4c6c771";
/// `template_hash` of the `KCC20` contract in upstream `fixtures/public-mint/artifact.json` at `c8a0871`.
const PUBLIC_MINT_TEMPLATE: &str = "9703112ee6e3555107cd168858992b77d3b74f655205b2b463b1f9ec2ec73cf7";
/// The later upstream commit whose `PublicMint` and `TokenSeed` issuers add `rebalance_value` (the `KCC20` actor is unchanged).
const ISSUER_COMMIT: &str = "27f8403fee55863125b044c73b2c9449d997a6ca";
const ISSUER_FILES: [(&str, &str); 5] = [
    ("public-mint.27f8403.artifact.json", "b5f93220b005f39ece92347f5226b2dd4b2f8f8c65b38b359c95133fdcf1c41e"),
    ("public_mint.27f8403.ag", "5736d002fb8c3094d1951d83ba097bf653e9f9686f9b603545ec80bcab784872"),
    ("token_seed.27f8403.ag", "f429951f7e10356656dc24526b987abcc15131c9f6c4dce643f1f61032d0986b"),
    ("PublicMint.27f8403.sil", "635537763557cc8a38a71337a1e86b4dd0c987ac7a3192054337a021cd20bdd5"),
    ("TokenSeed.27f8403.sil", "a2a221a5fbf23ea230e9a236a203a9fb4f610b539d687102e20fb8bcada4e465"),
];
/// Template hash of the standalone build (argentc `9a9f4b1` and KOB's pinned argentc agree, `scripts/build-argent.sh`).
const STANDALONE_TEMPLATE: &str = "173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b";

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn read(rel: &str) -> String {
    std::fs::read_to_string(common::repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}")).replace("\r\n", "\n")
}

/// One holder state (owner scheme 0x04, owned by `OWNER_COV`, borrowing disabled).
fn st(amount: i64) -> BTreeMap<String, ArtifactValue> {
    BTreeMap::from([
        ("amount".to_string(), ArtifactValue::Int(amount)),
        ("owner".to_string(), OWNER_COV.as_bytes().to_vec().into()),
        ("owner_scheme".to_string(), ArtifactValue::Byte(4)),
        ("borrow_scheme".to_string(), ArtifactValue::Byte(0)),
        ("borrow_guard".to_string(), vec![0u8; 32].into()),
        ("extension_commitment".to_string(), vec![0xeeu8; 32].into()),
    ])
}

/// A build of the reference program, compiled by KOB's silverc.
struct Build {
    name: &'static str,
    art: SilAbiArtifact,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    hash: Vec<u8>,
    /// the public-mint build's own template hash, carried in every state (`gen__kcc20_template`)
    context: Option<Vec<u8>>,
}
impl Build {
    fn compile(name: &'static str, src: &str, context: Option<Vec<u8>>) -> Build {
        let mut args: Vec<ArtifactValue> = context.iter().map(|c| c.clone().into()).collect();
        let s = st(1000);
        for f in ["amount", "owner", "owner_scheme", "borrow_scheme", "borrow_guard", "extension_commitment"] {
            args.push(s[f].clone());
        }
        let art = compile_contract(src, &args, CompileOptions::default()).unwrap_or_else(|e| panic!("compile {name}: {e}"));
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        Build { name, art, prefix, suffix, hash, context }
    }
    fn standalone() -> Build {
        Build::compile("KCC20Ref (standalone)", &common::contract_source("KCC20Ref"), None)
    }
    /// The public-mint build with its own template hash in `gen__kcc20_template` (the hash does not cover the state).
    fn public_mint() -> Build {
        let src = read("contracts/third-party/kcc20-reference/KCC20.public-mint.sil");
        let probe = Build::compile("probe", &src, Some(vec![0u8; 32]));
        Build::compile("KCC20 (public-mint build)", &src, Some(probe.hash))
    }
    fn state_bytes(&self, amount: i64) -> Vec<u8> {
        let mut m = st(amount);
        if let Some(c) = &self.context {
            m.insert("gen__kcc20_template".to_string(), c.clone().into());
        }
        let c = common::single_contract(&self.art);
        encode_runtime_state_script(&self.art, &c.runtime_state, &m).expect("encode state")
    }
    fn redeem(&self, amount: i64) -> Vec<u8> {
        [self.prefix.clone(), self.state_bytes(amount), self.suffix.clone()].concat()
    }
    fn spk(&self, amount: i64) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(amount))
    }
}

enum Leader {
    /// a holder of the same build calling `transfer`
    Holder(i64),
    /// a P2SH program that is not the token's (every byte pushed and dropped, then OP_TRUE), as long as `len`
    Foreign(usize),
}

/// One transaction: a leader, holder delegators of `amounts`, outputs `outs` (amounts, all bound to input 0), and the
/// owner covenant input (OP_TRUE, id `OWNER_COV`) that authorises every holder.
fn build_tx(b: &Build, leader: &Leader, amounts: &[i64], outs: &[i64]) -> (Transaction, Vec<UtxoEntry>) {
    let mut entries = vec![];
    let mut sigs = vec![];
    match leader {
        Leader::Holder(a) => {
            let next: Vec<ArtifactValue> = outs.iter().map(|o| ArtifactValue::Object(st(*o))).collect();
            let mut ss = encode_entry_sig_script(&b.art, "transfer", &[ArtifactValue::Array(next), vec![0x00u8].into()]).unwrap();
            ss.extend_from_slice(&push_redeem_script(&b.redeem(*a)));
            entries.push(UtxoEntry::new((10 * KAS) as u64, b.spk(*a), 0, false, Some(TOKEN_COV)));
            sigs.push(ss);
        }
        Leader::Foreign(len) => {
            let mut junk = vec![];
            while junk.len() + 504 < *len {
                junk.extend_from_slice(&[0x4d, 0xf4, 0x01]);
                junk.extend_from_slice(&[0xab; 500]);
                junk.push(0x75);
            }
            junk.push(OpTrue);
            entries.push(UtxoEntry::new((10 * KAS) as u64, pay_to_script_hash_script(&junk), 0, false, Some(TOKEN_COV)));
            sigs.push(push_redeem_script(&junk));
        }
    }
    for a in amounts {
        let mut ss = encode_entry_sig_script(&b.art, "transfer_delegator", &[Vec::<u8>::new().into()]).unwrap();
        ss.extend_from_slice(&push_redeem_script(&b.redeem(*a)));
        entries.push(UtxoEntry::new((10 * KAS) as u64, b.spk(*a), 0, false, Some(TOKEN_COV)));
        sigs.push(ss);
    }
    entries.push(UtxoEntry::new((10 * KAS) as u64, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(OWNER_COV)));
    sigs.push(vec![]);
    let inputs = (0..entries.len())
        .map(|k| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([k as u8 + 1; 32]), index: k as u32 },
                sigs[k].clone(),
                0,
                BUDGET,
            )
        })
        .collect();
    let outputs = outs
        .iter()
        .map(|o| TransactionOutput {
            value: KAS as u64,
            script_public_key: b.spk(*o),
            covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: TOKEN_COV }),
        })
        .collect();
    (Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]), entries)
}

fn execute(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, TxScriptError>> {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).expect("covenant context");
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    (0..tx.inputs.len())
        .map(|i| {
            let mut vm = TxScriptEngine::from_transaction_input(
                &populated,
                &tx.inputs[i],
                i,
                &entries[i],
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
            );
            vm.execute().map(|_| vm.used_script_units().0)
        })
        .collect()
}

fn verdicts(b: &Build, leader: &Leader, amounts: &[i64], outs: &[i64]) -> Vec<bool> {
    let (tx, entries) = build_tx(b, leader, amounts, outs);
    execute(&tx, &entries).iter().map(|r| r.is_ok()).collect()
}

#[test]
fn vendored_reference_is_the_merged_upstream() {
    for (f, want) in [
        ("contracts/third-party/kcc20-reference/kcc20.ag", KCC20_AG_SHA256),
        ("contracts/third-party/kcc20-reference/KCC20.public-mint.sil", PUBLIC_MINT_SIL_SHA256),
        ("contracts/third-party/kcc20-reference/public-mint.artifact.json", PUBLIC_MINT_ARTIFACT_SHA256),
    ] {
        let bytes = std::fs::read(common::repo_root().join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        let bytes: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
        assert_eq!(hexs(&Sha256::digest(&bytes)), want, "{f} is not the upstream blob of {UPSTREAM_COMMIT} (UPSTREAM.md)");
    }
    let reference = read("contracts/kcc20/KCC20Ref.sil");
    assert!(reference.contains(UPSTREAM_COMMIT), "KCC20Ref.sil provenance must name {UPSTREAM_COMMIT}");
}

/// The issuer files of `27f8403` are the upstream blobs, and the `KCC20` actor did not change between the two commits: the
/// artifact's `KCC20` contract is the one of `c8a0871` (same bytes, same template hash), and so is the vendored `.sil`.
#[test]
fn vendored_issuers_of_27f8403_are_upstream_and_the_holder_program_is_unchanged() {
    for (f, want) in ISSUER_FILES {
        let path = format!("contracts/third-party/kcc20-reference/{f}");
        let bytes = std::fs::read(common::repo_root().join(&path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let bytes: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
        assert_eq!(hexs(&Sha256::digest(&bytes)), want, "{path} is not the upstream blob of {ISSUER_COMMIT} (UPSTREAM.md)");
    }
    let kcc20 = |f: &str| {
        let doc: serde_json::Value = serde_json::from_str(&read(&format!("contracts/third-party/kcc20-reference/{f}"))).unwrap();
        doc["sil_abi"]["contracts"]["KCC20"]["compiled"].clone()
    };
    let (old, new) = (kcc20("public-mint.artifact.json"), kcc20("public-mint.27f8403.artifact.json"));
    assert_eq!(old, new, "the KCC20 contract of the 27f8403 artifact is the c8a0871 one");
    let recorded: Vec<u8> = new["template_hash"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect();
    assert_eq!(hexs(&recorded), PUBLIC_MINT_TEMPLATE);
    assert_eq!(new["bytecode"].as_array().unwrap().len(), 4031);
}

/// KOB's silverc (silverscript v1.0.0) compiles the generated issuer sources of `27f8403` to the very programs of the
/// vendored artifact (argentc `9a9f4b1` + silverscript, `cargo run --example build_contracts` upstream), i.e. to the template
/// hashes KOB pins; the new entry is `rebalance_value` (dispatch tag `498fa185`) on both. The hash does not cover the state.
#[test]
fn kobs_silverc_rebuilds_the_27f8403_issuers_to_the_pinned_hashes() {
    use kob_protocol::public_mint::{
        public_mint_of, token_seed_of, IssuerVersion, KCC20_SIL_HASH, PUBLIC_MINT_SIL_HASH_27F8403, TOKEN_SEED_SIL_HASH_27F8403,
    };
    let kcc20: ArtifactValue = silverscript_abi::ArtifactValue::from(
        (0..32).map(|i| u8::from_str_radix(&KCC20_SIL_HASH[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>(),
    );
    let z32 = || ArtifactValue::from(vec![0u8; 32]);
    for (src, name, want, sizes, args) in [
        (
            "PublicMint.27f8403.sil",
            "PublicMint",
            PUBLIC_MINT_SIL_HASH_27F8403,
            (1usize, 150usize, 1923usize),
            vec![
                kcc20.clone(),
                z32(),
                ArtifactValue::Int(1000),
                ArtifactValue::Int(10),
                vec![3u8; 32].into(),
                vec![0xeeu8; 32].into(),
            ],
        ),
        (
            "TokenSeed.27f8403.sil",
            "TokenSeed",
            TOKEN_SEED_SIL_HASH_27F8403,
            (1, 132, 1275),
            vec![kcc20.clone(), z32(), vec![3u8; 32].into(), vec![0xeeu8; 32].into()],
        ),
    ] {
        let art = compile_contract(&read(&format!("contracts/third-party/kcc20-reference/{src}")), &args, CompileOptions::default())
            .unwrap_or_else(|e| panic!("compile {src}: {e}"));
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        assert_eq!(hexs(&hash), want, "{name}: KOB's silverc and the pinned hash");
        assert_eq!((prefix.len(), common::state_layout(&art).len, suffix.len()), sizes, "{name}: cut");
        let c = common::single_contract(&art);
        assert_eq!(c.entries["rebalance_value"].dispatch_tag.to_hex(), "498fa185", "{name}: rebalance_value tag");
        // and it is the program KOB's genesis check embeds
        let p = if name == "PublicMint" { public_mint_of(IssuerVersion::F27f8403) } else { token_seed_of(IssuerVersion::F27f8403) };
        assert_eq!((p.prefix.clone(), p.suffix.clone()), (prefix, suffix), "{name}: embedded program");
    }
}

#[test]
fn both_builds_compile_to_the_recorded_templates() {
    let s = Build::standalone();
    let p = Build::public_mint();
    for (b, tpl, size, span) in [(&s, STANDALONE_TEMPLATE, 2915usize, 112usize), (&p, PUBLIC_MINT_TEMPLATE, 4031, 145)] {
        let c = common::single_contract(&b.art);
        assert_eq!(hexs(&b.hash), tpl, "{}: template hash", b.name);
        assert_eq!(bytecode(&b.art).len(), size, "{}: program size", b.name);
        assert_eq!((c.compiled.state_span.offset, c.compiled.state_span.len), (1, span), "{}: state span", b.name);
        println!("BUILD {}: template {tpl}, {size} B, state 1+{span}", b.name);
    }
    for e in ["transfer", "transfer_delegator"] {
        let tag = |b: &Build| common::single_contract(&b.art).entries[e].dispatch_tag;
        assert_eq!(tag(&s), tag(&p), "{e}: dispatch tag");
    }
    // the public-mint build's state is the template hash followed by the standalone state
    assert_eq!(p.state_bytes(7), [vec![0x20], p.hash.clone(), s.state_bytes(7)].concat());
    // KOB's embedded public-mint program (built by scripts/build-contracts.sh from the same fixture) is this build, cut as
    // upstream's actor-type handle: the context push joins the prefix, the 112-byte state stays open
    let emb = kob_protocol::artifacts::template(kob_protocol::artifacts::TemplateId::Kcc20PublicMint);
    assert_eq!(emb.sil_hash.to_vec(), p.hash);
    assert_eq!(emb.prefix, [p.prefix.clone(), vec![0x20], p.hash.clone()].concat());
    assert_eq!((emb.suffix.clone(), emb.state_len), (p.suffix.clone(), 112));
    assert_eq!(emb.redeem(&s.state_bytes(7)), p.redeem(7));
}

/// The question behind proposals P2 and `KCC20Opt`: can one transaction settle 16 holders that stay on the standard 3 / 3
/// program? With the public-mint build, no: its own leader stops at 3 inputs and 3 outputs, and every holder refuses any
/// other leader. With the standalone build the leader bounds are the same, and only a leader that the token's genesis put in
/// the covenant family can lead more holders (accepted here; KOB's issuance never creates one).
#[test]
fn transfer_capacity_per_transaction() {
    let p = Build::public_mint();
    let s = Build::standalone();
    for b in [&p, &s] {
        assert!(verdicts(b, &Leader::Holder(10), &[20, 30], &[60]).iter().all(|ok| *ok), "{}: 3 -> 1", b.name);
        assert!(verdicts(b, &Leader::Holder(60), &[], &[10, 20, 30]).iter().all(|ok| *ok), "{}: 1 -> 3", b.name);
        assert!(!verdicts(b, &Leader::Holder(10), &[20, 30, 40], &[100])[0], "{}: 4 token inputs must be refused", b.name);
        assert!(!verdicts(b, &Leader::Holder(100), &[], &[10, 20, 30, 40])[0], "{}: 4 token outputs must be refused", b.name);
        println!("CAPACITY {}: 3 -> 1 and 1 -> 3 accepted; 4 token inputs and 4 token outputs refused by the leader", b.name);
    }
    let holders = vec![1000i64; 16];
    // a foreign leader as long as the holder program (the standalone build's leader read only needs the length)
    let foreign = Leader::Foreign(5000);
    let v = verdicts(&p, &foreign, &holders, &[16_000]);
    assert!(v[1..=16].iter().all(|ok| !*ok), "{}: every holder must refuse a foreign leader: {v:?}", p.name);
    println!("BATCH {}: foreign leader + 16 holders: all 16 holder inputs refused (leader template check)", p.name);
    let v = verdicts(&s, &foreign, &holders, &[16_000]);
    assert!(v.iter().all(|ok| *ok), "{}: {v:?}", s.name);
    println!("BATCH {}: foreign leader + 16 holders: accepted (the delegator does not check the leader)", s.name);
}

/// `kob registry add-program` accepts the upstream Argent app-bundle artifact (the file upstream publishes) as well as the
/// silverc artifact: both derive the profile embedded as `KCC20PublicMint`, and a file of neither schema keeps reporting its
/// own missing field.
#[test]
fn add_program_accepts_the_upstream_argent_artifact() {
    use kob_protocol::artifacts::{parse_sil_abi, ArtifactSchema};
    use kob_protocol::profile::{by_name, derive_profile, DeriveOptions, ProgramInput};

    let upstream = read("contracts/third-party/kcc20-reference/public-mint.artifact.json");
    let silverc = read("contracts/artifacts/KCC20PublicMint.json");
    assert_eq!(parse_sil_abi(&upstream).unwrap().1, ArtifactSchema::Argent);
    assert_eq!(parse_sil_abi(&silverc).unwrap().1, ArtifactSchema::Silverc);

    let opt = DeriveOptions {
        name: Some("KCC20PublicMint".into()),
        registry_id: Some("kcc20-ref-public-mint".into()),
        record_code: Some(9),
        source_path: Some("contracts/artifacts/KCC20PublicMint.json".into()),
        slots: Some(kob_protocol::profile::Slots { inputs: 3, outputs: 3 }),
        ..Default::default()
    };
    let want = by_name("KCC20PublicMint").expect("embedded profile");
    let from_argent = derive_profile(&ProgramInput::Artifact(upstream), &opt).expect("argent artifact derives");
    let from_silverc = derive_profile(&ProgramInput::Artifact(silverc), &opt).expect("silverc artifact derives");
    for d in [&from_argent, &from_silverc] {
        let p = &d.profile;
        assert_eq!(d.already_embedded.as_deref(), Some("KCC20PublicMint"));
        assert_eq!(p.template_hash, want.template_hash);
        assert!(p.template_hash.starts_with("734850b0"));
        assert_eq!((p.layout, p.codec), (want.layout, want.codec));
        assert_eq!(p.source.context_fields, want.source.context_fields);
        assert_eq!(p.source.kind, want.source.kind);
        assert_eq!(p.witness, want.witness);
        assert_eq!((p.owners, p.slots, p.name.as_str()), (want.owners, want.slots, want.name.as_str()));
    }
    assert!(from_argent.detection.contains("Argent"), "{}", from_argent.detection);
    assert_eq!(from_argent.profile.template_hash, from_silverc.profile.template_hash);

    // the zero placeholder instance is accepted for the Argent schema only: the same contract as a silverc artifact is refused
    let inner = serde_json::from_str::<serde_json::Value>(&read("contracts/third-party/kcc20-reference/public-mint.artifact.json"))
        .unwrap()["sil_abi"]
        .to_string();
    assert_eq!(parse_sil_abi(&inner).unwrap().1, ArtifactSchema::Silverc);
    let err = derive_profile(&ProgramInput::Artifact(inner), &opt).unwrap_err();
    assert!(err.to_string().contains("not the canonical"), "{err}");

    // neither schema: the silverc reading reports its own missing field
    let err = derive_profile(&ProgramInput::Artifact(r#"{"schema_version":1,"contracts":{}}"#.into()), &opt).unwrap_err();
    assert!(err.to_string().contains("compiler_version"), "{err}");
}
