//! Conformance tests of the token program `KaspaRocketToken11458`: the token program of the KaspaRocket launchpad
//! (kasparocket.fun, mainnet and testnet-10, template `3d689d62...`). It is the `KCC20` actor of upstream's published `KCC20PublicMint` app
//! (`contracts/third-party/kcc20-reference/KCC20.public-mint.sil`, KOB's reviewed `Kcc20PublicMint`) built with 9 delegates and
//! 10 token outputs instead of 2 and 3: `contracts/third-party/kasparocket/KCC20.public-mint.10x10.sil`, seven lines apart from
//! the vendored fixture, compiled by `scripts/build-contracts.sh` (silverc v1.0.0) into `contracts/artifacts/KaspaRocketToken11458.json`,
//! reproduces the 11,458-byte redeem scripts of the launchpad's token UTXOs byte for byte. Fixture of a real launch:
//! `contracts/third-party/kasparocket/observed.json` (genesis, the move of the supply into the launch's pool, the pool's genesis,
//! the first buy). The profile is in `crates/kob-protocol/data/programs.json` (`reviewed`, `networks: [mainnet, testnet-10]`, `minters: genesis-only`, default decimals 3: the
//! KaspaRocket convention). The order grid and the pair attacks on its holders: `kcc20_public_mint_tests.rs` and
//! `audit_pair_sec_tests.rs`; the review: `docs/spec/token-programs.md` section 6.
//!
//! What the reviewer checklist of `docs/spec/token-programs.md` (section 5) asks, item by item:
//!
//! 1-3. provenance, layout, witness: the artifact reproduces the observed redeem scripts, the cut is the KCC-1 actor-type handle
//!      (34-byte prefix with the `gen__kcc20_template` context, constant per program: the program's own Sil template hash),
//!      KOB's `kcc20-transfer` builder (`SigPlan::TokenLeader`) reproduces both observed token inputs byte for byte, the
//!      observed transactions rebuild to their ids and run in the engine (the launch's LP_AMM input included);
//! 4.   owners: custody (owner scheme 4) is released only with an input of its covenant; holders (scheme 0) only with their
//!      key's signature of the input;
//! 5.   conservation: a transfer neither mints nor burns; the genesis of the observed launch has one token output (fixed supply,
//!      no companion in the token's group: the pool is its own covenant);
//! 6.   slots (10 / 10) and carrier floor (1 sompi) measured;
//! 9.   (part) KOB orders on the program built and validated in the engine (`kob_orders`), and the mass and fee of a transfer at
//!      the program's 10 / 10 against KOB's 3 / 3 standard.
//!
//! Run: cargo test -p kob-tests --test program_kasparockettoken11458 -- --nocapture

use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::opcodes::codes::OpTrue;
use kaspa_txscript::pay_to_script_hash_script;
use secp256k1::{Keypair, Secp256k1, SecretKey};

use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::json::{from_hex, to_hex};
use kob_protocol::profile::{derive_profile, DeriveOptions, ProgramInput, ProgramReview, Slots, WitnessScheme};
use kob_protocol::registry::{verify_genesis_with, Family, GenesisOutput};
use kob_protocol::script::push_data;
use kob_protocol::state::{Kcc20State, SCHEME_COVID, SCHEME_P2PK};
use kob_protocol::tx::{SigPlan, Witness};
use kob_protocol::verify::execute;

const HASH: &str = "3d689d6273b7631e69f672d5ca744cb266ae997b04974196439858675cc04fe1";
/// The program's Sil template hash (prefix 1 B, the 145-byte state of the Sil build, suffix): the `gen__kcc20_template` value
/// every UTXO of the program carries.
const SIL_HASH: &str = "ff3ae732e3a2819bf9181f5aed6b1e9d0f7abea70a1dd655b8e777cbcbd2422b";
const KAS: u64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x3d; 32]);
const ORDER_COV: Hash = Hash::from_bytes([0xc0; 32]);

fn program() -> TemplateId {
    TemplateId::KaspaRocketToken11458
}

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn text(rel: &str) -> String {
    std::fs::read_to_string(repo(rel)).unwrap().replace('\r', "")
}

fn h32(s: &str) -> [u8; 32] {
    from_hex(s).unwrap().try_into().unwrap()
}

/// Data pushes of a push-only script (`OP_0` = empty, `OP_1..OP_16` = one byte).
fn pushes(script: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    let mut i = 0;
    while i < script.len() {
        let op = script[i];
        i += 1;
        let n = match op {
            0x00 => {
                out.push(vec![]);
                continue;
            }
            0x01..=0x4b => op as usize,
            0x4c => {
                i += 1;
                script[i - 1] as usize
            }
            0x4d => {
                i += 2;
                u16::from_le_bytes([script[i - 2], script[i - 1]]) as usize
            }
            0x51..=0x60 => {
                out.push(vec![op - 0x50]);
                continue;
            }
            other => panic!("not a push-only script: opcode {other:#x}"),
        };
        out.push(script[i..i + n].to_vec());
        i += n;
    }
    out
}

// ------------------------------------------------------------------------------------------------ the observed launch

fn observed() -> serde_json::Value {
    serde_json::from_str(&text("contracts/third-party/kasparocket/observed.json")).unwrap()
}

/// An observed transaction rebuilt from the fixture with its spent outputs (lock time, gas and sequences 0).
fn rebuild(t: &serde_json::Value) -> (Transaction, Vec<UtxoEntry>) {
    let spk = |s: &serde_json::Value| ScriptPublicKey::new(0, from_hex(s.as_str().unwrap()).unwrap().into());
    let inputs: Vec<TransactionInput> = t["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint {
                    transaction_id: TransactionId::from_bytes(h32(i["outpoint"]["txid"].as_str().unwrap())),
                    index: i["outpoint"]["index"].as_u64().unwrap() as u32,
                },
                from_hex(i["signature_script"].as_str().unwrap()).unwrap(),
                0,
                i["compute_budget"].as_u64().unwrap() as u16,
            )
        })
        .collect();
    let entries: Vec<UtxoEntry> = t["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let e = &i["entry"];
            let cov = e["covenant_id"].as_str().map(|c| Hash::from_bytes(h32(c)));
            UtxoEntry::new(e["amount"].as_u64().unwrap(), spk(&e["script_public_key"]), 0, false, cov)
        })
        .collect();
    let outputs: Vec<TransactionOutput> = t["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| TransactionOutput {
            value: o["value"].as_u64().unwrap(),
            script_public_key: spk(&o["script_public_key"]),
            covenant: o["covenant"].as_object().map(|c| CovenantBinding {
                authorizing_input: c["authorizing_input"].as_u64().unwrap() as u16,
                covenant_id: Hash::from_bytes(h32(c["covenant_id"].as_str().unwrap())),
            }),
        })
        .collect();
    let payload = from_hex(t["payload"].as_str().unwrap()).unwrap();
    let tx = Transaction::new(t["version"].as_u64().unwrap() as u16, inputs, outputs, 0, SubnetworkId::default(), 0, payload);
    (tx, entries)
}

/// The redeem script (last push) of input `i` of an observed transaction.
fn redeem_of(t: &serde_json::Value, i: usize) -> Vec<u8> {
    pushes(&from_hex(t["inputs"][i]["signature_script"].as_str().unwrap()).unwrap()).pop().unwrap()
}

fn kcc20(amount: i64, owner: [u8; 32], scheme: u8) -> Kcc20State {
    Kcc20State { amount, owner, owner_scheme: scheme, borrow_scheme: 0, borrow_guard: [0; 32], extension_commitment: [0; 32] }
}

// ------------------------------------------------------------------------------------------------ items 1-3: the bytes

#[test]
fn profile_matches_the_embedded_bytes() {
    let p = program().profile().expect("profile");
    let t = token_template(program());
    assert_eq!(t.hash, p.hash());
    assert_eq!(p.template_hash, HASH);
    assert_eq!((t.prefix.len(), t.state_len, t.suffix.len()), (p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len));
    assert_eq!((p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len), (34, 112, 11_312));
    assert_eq!(&t.prefix[2..34], &h32(SIL_HASH), "the context field is the program's own Sil template hash");
    assert_eq!((p.codec, p.owners.custody, p.owners.holder, p.witness.scheme), (Family::Kcc20, 4, 0, WitnessScheme::Kcc20Transfer));
    assert_eq!(p.review.status, ProgramReview::Reviewed);
    assert_eq!(p.networks, vec!["mainnet".to_string(), "testnet-10".to_string()]);
    assert!(!p.router_intents);
    assert_eq!(p.genesis.minters, kob_protocol::profile::MinterSource::GenesisOnly);
    assert!(p.genesis.companions.is_empty(), "KaspaRocket ships no issuer actor for this build");
    let d = p.default_decimals.as_ref().expect("default decimals");
    assert_eq!((d.decimals, d.source.as_str()), (3, "KaspaRocket convention"));
}

/// `kob registry add-program contracts/artifacts/KaspaRocketToken11458.json --name KaspaRocketToken11458 --slots 10/10
/// --network mainnet --network testnet-10` derives the embedded profile's byte facts from the artifact alone.
#[test]
fn the_profile_derives_from_the_artifact() {
    let p = program().profile().unwrap();
    let d = derive_profile(
        &ProgramInput::Artifact(text("contracts/artifacts/KaspaRocketToken11458.json")),
        &DeriveOptions {
            name: Some("KaspaRocketToken11458".into()),
            slots: Some(Slots { inputs: 10, outputs: 10 }),
            networks: vec!["mainnet".into(), "testnet-10".into()],
            ..Default::default()
        },
    )
    .expect("derivable");
    assert_eq!(d.already_embedded.as_deref(), Some("KaspaRocketToken11458"));
    let facts = |x: &kob_protocol::profile::ProgramProfile| {
        (x.template_hash.clone(), x.layout, x.codec, x.witness.clone(), x.owners, x.slots, x.carrier_floor, x.networks.clone())
    };
    assert_eq!(facts(&d.profile), facts(p));
    assert_eq!(d.profile.source.context_fields, vec!["gen__kcc20_template".to_string()]);
}

/// The source is the vendored `KCC20PublicMint` actor with exactly the seven lines of the 10 / 10 build changed, and its
/// embedded suffix length and constructor context are the program's own (a fixed point argentc computes).
#[test]
fn the_source_is_the_published_actor_at_ten_slots() {
    let base = text("contracts/third-party/kcc20-reference/KCC20.public-mint.sil");
    let ours = text("contracts/third-party/kasparocket/KCC20.public-mint.10x10.sil");
    let suffix_len = token_template(program()).suffix.len() as u32;
    let reps = [
        ("int constant MAX_DELEGATES = 2;", "int constant MAX_DELEGATES = 9;".to_string()),
        ("int constant MAX_TOKEN_OUTPUTS = 3;", "int constant MAX_TOKEN_OUTPUTS = 10;".to_string()),
        (
            "byte[4] constant gen__const_kcc20_suffix_len = byte[4](0x2d0f0000);",
            format!("byte[4] constant gen__const_kcc20_suffix_len = byte[4](0x{});", to_hex(&suffix_len.to_le_bytes())),
        ),
        ("require(gen__delegates_count <= 2);", "require(gen__delegates_count <= 9);".to_string()),
        (
            "for (gen__delegates_position, 0, gen__delegates_count, 2) {",
            "for (gen__delegates_position, 0, gen__delegates_count, 9) {".to_string(),
        ),
        ("require(gen__next_outputs_output_count <= 3);", "require(gen__next_outputs_output_count <= 10);".to_string()),
        (
            "for (gen__next_outputs_output_position, 0, gen__next_outputs_output_count, 3) {",
            "for (gen__next_outputs_output_position, 0, gen__next_outputs_output_count, 10) {".to_string(),
        ),
    ];
    let mut want = base.clone();
    for (from, to) in &reps {
        assert_eq!(want.matches(from).count(), 1, "{from}");
        want = want.replacen(from, to, 1);
    }
    // our file: the pragma, a KOB header of comment lines, then the derived fixture
    let pragma = "pragma silverscript ^0.1.0;\n";
    let body = ours.strip_prefix(pragma).unwrap();
    let header_end = body.find("\n// Generated by argentc.").unwrap() + 1;
    assert!(body[..header_end].lines().all(|l| l.starts_with("// ") || l.is_empty()));
    assert_eq!(
        format!(
            "{pragma}
{}",
            &body[header_end..]
        ),
        want
    );
    // the constructor's context value is the Sil template hash of the compiled program
    let t = token_template(program());
    let ctor: serde_json::Value =
        serde_json::from_str(&text("contracts/third-party/kasparocket/KCC20.public-mint.10x10.ctor.json")).unwrap();
    let ctx: Vec<u8> = ctor[0]["value"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
    assert_eq!(ctx, h32(SIL_HASH));
    assert_eq!(silverscript_abi::template_hash(&t.prefix[..1], &t.suffix), h32(SIL_HASH));
}

/// The genesis, the supply's move into the launch's pool and the pool's first buy: their token inputs spend the embedded
/// program (the artifact rebuilt with the observed state is the observed redeem script), and the genesis output is the P2SH of
/// the redeem script its spend reveals.
#[test]
fn the_launchpads_token_utxos_are_the_embedded_program() {
    let obs = observed();
    let t = token_template(program());
    let txs = &obs["transactions"];
    for (name, i) in [("transfer", 0usize), ("buy", 1)] {
        let r = redeem_of(&txs[name], i);
        assert_eq!(r.len(), 11_458);
        let state = t.state_of(&r).unwrap_or_else(|| panic!("{name}: not an instance of the program"));
        assert_eq!(t.redeem(state), r);
        let spk = txs[name]["inputs"][i]["entry"]["script_public_key"].as_str().unwrap();
        assert_eq!(spk, to_hex(t.spk(state).script()), "{name}: the spent UTXO is the P2SH of the redeem script");
    }
    // genesis output 0 (the whole supply, owned by the launchpad's key) is spent by the transfer
    let g = &txs["genesis"];
    let spent = &txs["transfer"]["inputs"][0]["outpoint"];
    assert_eq!((spent["txid"].as_str(), spent["index"].as_u64()), (g["txid"].as_str(), Some(0)));
    let st = Kcc20State::from_redeem_with(template(program()), &redeem_of(&txs["transfer"], 0)).unwrap();
    assert_eq!((st.amount, st.owner_scheme, st.borrow_scheme, st.extension_commitment), (1_000_000_000, SCHEME_P2PK, 0, [0; 32]));
}

/// KOB's `kcc20-transfer` builder (the plans KOB signs and assembles) reproduces both observed token inputs byte for byte:
/// a holder (owner scheme 0, the owner's signature as leader witness) moving the supply into the pool's custody, and the pool's
/// custody (owner scheme 4, empty leader witness) paying a buyer.
#[test]
fn the_kcc20_transfer_builder_reproduces_the_observed_token_inputs() {
    let obs = observed();
    let txs = &obs["transactions"];
    let pool: [u8; 32] = h32(obs["lp_amm_covenant_id"].as_str().unwrap());
    let spk_of = |s: &Kcc20State| to_hex(s.spk_with(template(program())).script());
    // the transfer: the launchpad key's 1e9 units into the pool (one output)
    let tr = &txs["transfer"];
    let state = Kcc20State::from_redeem_with(template(program()), &redeem_of(tr, 0)).unwrap();
    let owner = state.owner;
    let next = vec![kcc20(1_000_000_000, pool, SCHEME_COVID)];
    assert_eq!(tr["outputs"][0]["script_public_key"].as_str().unwrap(), spk_of(&next[0]));
    let observed_sig = from_hex(tr["inputs"][0]["signature_script"].as_str().unwrap()).unwrap();
    let witness = &pushes(&observed_sig)[6];
    assert_eq!((witness.len(), witness[0]), (66, 0x00), "PATH_NORMAL then the owner's 65-byte signature");
    let plan = SigPlan::TokenLeader { template: program(), state, next_states: next, witness: Witness::P2pk(owner) };
    assert_eq!(to_hex(&plan.sigscript(Some(&witness[1..])).unwrap()), to_hex(&observed_sig), "byte-identical transfer");
    // the buy: the pool's custody pays 1,000 units to the buyer, the rest stays in custody (outputs 2 and 3)
    let buy = &txs["buy"];
    let observed_sig = from_hex(buy["inputs"][1]["signature_script"].as_str().unwrap()).unwrap();
    let owners = &pushes(&observed_sig)[1];
    let buyer: [u8; 32] = owners[32..64].try_into().unwrap();
    let state = Kcc20State::from_redeem_with(template(program()), &redeem_of(buy, 1)).unwrap();
    assert_eq!(state, kcc20(1_000_000_000, pool, SCHEME_COVID));
    let next = vec![kcc20(999_999_000, pool, SCHEME_COVID), kcc20(1_000, buyer, SCHEME_P2PK)];
    for (k, s) in next.iter().enumerate() {
        assert_eq!(buy["outputs"][2 + k]["script_public_key"].as_str().unwrap(), spk_of(s));
    }
    let plan = SigPlan::TokenLeader { template: program(), state, next_states: next, witness: Witness::CovenantId };
    assert_eq!(to_hex(&plan.sigscript(None).unwrap()), to_hex(&observed_sig), "byte-identical custody release");
}

/// The observed transactions rebuild to their ids and every input (token, P2PK, the launch's LP_AMM pool) runs in KOB's
/// engine (rusty-kaspa v2.1.0) within its committed compute budget; the genesis verifies as a fixed-supply launch with no
/// companion in the token's group.
#[test]
fn the_observed_transactions_run_in_the_engine_and_the_genesis_verifies() {
    let obs = observed();
    let txs = &obs["transactions"];
    for name in ["genesis", "lp_amm_genesis", "transfer", "buy"] {
        let (tx, entries) = rebuild(&txs[name]);
        assert_eq!(tx.id().to_string(), txs[name]["txid"].as_str().unwrap(), "{name}: the rebuilt transaction id");
        let res = execute(&tx, &entries, true).unwrap();
        assert!(res.iter().all(|r| r.is_ok()), "{name}: {res:?}");
    }
    let p = program().profile().unwrap();
    let template = kob_protocol::profile::registry_template(p, None);
    let g = &txs["genesis"];
    let token = obs["token"]["covenant_id"].as_str().unwrap();
    let (tx, _) = rebuild(g);
    let group: Vec<GenesisOutput> = tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.covenant.is_some_and(|c| to_hex(c.covenant_id.as_bytes().as_slice()) == token))
        .map(|(k, o)| GenesisOutput {
            index: k as u32,
            value: o.value,
            script_public_key: o.script_public_key.clone(),
            redeem_script: Some(redeem_of(&txs["transfer"], 0)),
        })
        .collect();
    assert_eq!(group.len(), 1, "one token output in the token's genesis group");
    let first = &tx.inputs[group[0].index as usize].previous_outpoint;
    let r = verify_genesis_with(
        &template,
        p,
        token,
        g["txid"].as_str().unwrap(),
        (first.transaction_id.as_bytes(), first.index),
        &group,
        None,
    )
    .expect("the launch genesis verifies");
    assert_eq!((r.supply, r.mint_allowance), (1_000_000_000, 0));
    assert!(r.minter_outputs.is_empty() && r.seed_outputs.is_empty() && r.companion_outputs.is_empty());
}

// ------------------------------------------------------------------------------------------------ the engine

fn keypair(seed: u8) -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[seed; 32]).unwrap())
}
fn xonly(seed: u8) -> [u8; 32] {
    keypair(seed).x_only_public_key().0.serialize()
}
fn p2pk_spk(pk: &[u8; 32]) -> ScriptPublicKey {
    kob_protocol::script::p2pk_spk(pk)
}

fn sign(tx: &Transaction, entries: &[UtxoEntry], n: usize, seed: u8) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), n, SigHashType::from_u8(1).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).unwrap();
    let mut sig = keypair(seed).sign_schnorr(msg).as_ref().to_vec();
    sig.push(1);
    sig
}

enum In {
    /// a token input in this state; a holder (scheme 0) is signed by the key of seed `.1` over input `.2` (`None`: itself)
    Tok(Kcc20State, u8, Option<usize>),
    /// a P2PK input of the key of this seed
    P2pk(u8),
    /// an `OP_TRUE` P2SH input carrying this covenant id (stands for a KOB order input)
    Cov(Hash),
}

/// Runs a transfer of the program: the first token input leads (`transfer`), the others delegate (`transfer_delegator`),
/// the token outputs `outs` (state, KAS value) are bound to the leader.
fn run(ins: &[In], outs: &[(Kcc20State, u64)]) -> Result<(), String> {
    let entries: Vec<UtxoEntry> = ins
        .iter()
        .map(|i| match i {
            In::Tok(s, ..) => UtxoEntry::new(KAS, s.spk_with(template(program())), 0, false, Some(TOKEN_COV)),
            In::P2pk(seed) => UtxoEntry::new(10 * KAS, p2pk_spk(&xonly(*seed)), 0, false, None),
            In::Cov(h) => UtxoEntry::new(KAS, pay_to_script_hash_script(&[OpTrue]), 0, false, Some(*h)),
        })
        .collect();
    let leader = ins.iter().position(|i| matches!(i, In::Tok(..))).unwrap();
    let outputs: Vec<TransactionOutput> = outs
        .iter()
        .map(|(s, v)| TransactionOutput {
            value: *v,
            script_public_key: s.spk_with(template(program())),
            covenant: Some(CovenantBinding { authorizing_input: leader as u16, covenant_id: TOKEN_COV }),
        })
        .collect();
    let inputs: Vec<TransactionInput> = (0..ins.len())
        .map(|n| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([n as u8 + 1; 32]), index: n as u32 },
                vec![],
                0,
                0,
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    let next: Vec<Kcc20State> = outs.iter().map(|(s, _)| s.clone()).collect();
    for (n, i) in ins.iter().enumerate() {
        tx.inputs[n].signature_script = match i {
            In::Tok(s, seed, of) => {
                let (witness, sig) = if s.owner_scheme == SCHEME_COVID {
                    (Witness::CovenantId, None)
                } else {
                    (Witness::P2pk(s.owner), Some(sign(&unsigned, &entries, of.unwrap_or(n), *seed)))
                };
                let plan = if n == leader {
                    SigPlan::TokenLeader { template: program(), state: s.clone(), next_states: next.clone(), witness }
                } else {
                    SigPlan::TokenDelegator { template: program(), state: s.clone(), witness }
                };
                plan.sigscript(sig.as_deref()).map_err(|e| e.to_string())?
            }
            In::P2pk(seed) => push_data(&sign(&unsigned, &entries, n, *seed)),
            In::Cov(_) => push_data(&[OpTrue]),
        };
    }
    for (i, r) in execute(&tx, &entries, false).map_err(|e| e.to_string())?.into_iter().enumerate() {
        r.map_err(|e| format!("input {i}: {e}"))?;
    }
    Ok(())
}

fn custody(amount: i64) -> Kcc20State {
    kcc20(amount, ORDER_COV.as_bytes(), SCHEME_COVID)
}
fn held(amount: i64, seed: u8) -> Kcc20State {
    kcc20(amount, xonly(seed), SCHEME_P2PK)
}

// ------------------------------------------------------------------------------------------------ item 4: owners

#[test]
fn custody_is_released_only_with_an_input_of_its_covenant() {
    let b = 9;
    let honest = [(held(250, b), 1), (custody(350), 1)];
    assert!(run(&[In::Cov(ORDER_COV), In::Tok(custody(600), 0, None)], &honest).is_ok());
    assert!(run(&[In::P2pk(b), In::Tok(custody(600), 0, None)], &honest).is_err(), "no covenant input");
    assert!(
        run(&[In::Cov(Hash::from_bytes([0xc1; 32])), In::Tok(custody(600), 0, None)], &honest).is_err(),
        "another covenant's input"
    );
    // as a delegator too: a custody beside a holder's leader input
    let ins = [In::Tok(held(100, b), b, None), In::Cov(ORDER_COV), In::Tok(custody(600), 0, None)];
    assert!(run(&ins, &[(held(700, b), 1)]).is_ok());
    let ins = [In::Tok(held(100, b), b, None), In::P2pk(b), In::Tok(custody(600), 0, None)];
    assert!(run(&ins, &[(held(700, b), 1)]).is_err(), "a delegating custody without its covenant");
    let p = program().profile().unwrap();
    assert_eq!((p.owners.custody, p.owners.holder), (SCHEME_COVID, SCHEME_P2PK));
}

#[test]
fn holders_are_moved_only_by_their_keys_signature() {
    let m = 7;
    let outs = [(custody(600), 1), (held(400, m), 1)];
    assert!(run(&[In::Tok(held(1_000, m), m, None)], &outs).is_ok());
    assert!(run(&[In::Tok(held(1_000, m), 8, None)], &outs).is_err(), "another key's signature");
    assert!(run(&[In::P2pk(m), In::Tok(held(1_000, m), m, Some(0))], &outs).is_err(), "a signature of another input");
    // delegators: the second holder must sign too
    let ins = [In::Tok(held(500, m), m, None), In::Tok(held(500, 8), 7, None)];
    assert!(run(&ins, &outs).is_err(), "a delegator signed by another key");
    let ins = [In::Tok(held(500, m), m, None), In::Tok(held(500, 8), 8, None)];
    assert!(run(&ins, &outs).is_ok());
}

// ------------------------------------------------------------------------------------------------ item 5: conservation

#[test]
fn transfers_conserve_the_amount() {
    let b = 9;
    let go = |outs: &[(Kcc20State, u64)]| run(&[In::Cov(ORDER_COV), In::Tok(custody(600), 0, None)], outs);
    assert!(go(&[(held(250, b), 1), (custody(350), 1)]).is_ok());
    assert!(go(&[(held(251, b), 1), (custody(350), 1)]).is_err(), "no minting");
    assert!(go(&[(held(249, b), 1), (custody(350), 1)]).is_err(), "no burning");
    assert!(go(&[(held(-1, b), 1), (custody(601), 1)]).is_err(), "no negative amount");
}

// ------------------------------------------------------------------------------------------------ item 6: limits

#[test]
fn slots_and_carrier_floor_are_measured() {
    let p = program().profile().unwrap();
    let b = 9;
    let at = |v: u64| run(&[In::Cov(ORDER_COV), In::Tok(custody(600), 0, None)], &[(held(250, b), v), (custody(350), v)]).is_ok();
    assert!(at(10 * KAS));
    let (mut lo, mut hi) = (0u64, 10 * KAS);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if at(mid) {
            hi = mid
        } else {
            lo = mid
        }
    }
    assert_eq!(hi, p.carrier_floor, "the profile's carrier_floor is the measured one");
    let outs_ok = |n: usize| {
        let mut outs: Vec<(Kcc20State, u64)> = (0..n - 1).map(|_| (held(1, b), 1)).collect();
        outs.push((custody(600 - (n as i64 - 1)), 1));
        run(&[In::Cov(ORDER_COV), In::Tok(custody(600), 0, None)], &outs).is_ok()
    };
    let max_out = (1..=12).take_while(|&n| outs_ok(n)).last().unwrap_or(0);
    let ins_ok = |n: usize| {
        let mut ins = vec![In::Cov(ORDER_COV)];
        ins.extend((0..n).map(|_| In::Tok(custody(100), 0, None)));
        run(&ins, &[(held(100 * n as i64, b), 1)]).is_ok()
    };
    let max_in = (1..=12).take_while(|&n| ins_ok(n)).last().unwrap_or(0);
    assert_eq!(Slots { inputs: max_in, outputs: max_out }, p.slots, "the profile's slots are the measured ones");
    // ten key-held inputs of ten keys, each signed by its owner
    let ins: Vec<In> = (1..=10u8).map(|k| In::Tok(held(10, k), k, None)).collect();
    assert!(run(&ins, &[(held(100, 20), 1)]).is_ok());
}

// ------------------------------------------------------------------------------------------------ KOB's orders on the program

mod kob_orders {
    use kob_protocol::artifacts::{template, token_template, TemplateId};
    use kob_protocol::build::{build, Action, CancelOrder, CreateOrder, SendTokens, TokenRecipient, TokenRef};
    use kob_protocol::state::{AnyState, AskState, Kcc20State, TokenState, SCHEME_P2PK};
    use kob_protocol::tx::{finalize, sign_locally, FeeOptions, FinalizeOptions, KeyUtxo, OrderUtxo, SigPlan, TokenUtxo, Utxo};
    use kob_protocol::verify::{validate_signed, Validation};

    const KAS: u64 = 100_000_000;
    const TOKEN: [u8; 32] = [0xfe; 32];

    fn sk(n: u8) -> [u8; 32] {
        [n; 32]
    }
    fn pk(n: u8) -> [u8; 32] {
        secp256k1::Keypair::from_secret_key(&secp256k1::Secp256k1::new(), &secp256k1::SecretKey::from_slice(&sk(n)).unwrap())
            .x_only_public_key()
            .0
            .serialize()
    }
    fn keys() -> std::collections::BTreeMap<[u8; 32], [u8; 32]> {
        (1..=30u8).map(|n| (pk(n), sk(n))).collect()
    }
    fn utxo(tag: u8, amount: u64, cov: Option<[u8; 32]>) -> Utxo {
        Utxo { transaction_id: [tag; 32], index: 0, amount, block_daa_score: 500, covenant_id: cov }
    }
    fn program() -> TemplateId {
        TemplateId::KaspaRocketToken11458
    }
    fn holder(tag: u8, owner: u8, amount: i64) -> TokenUtxo {
        let st = Kcc20State {
            amount,
            owner: pk(owner),
            owner_scheme: SCHEME_P2PK,
            borrow_scheme: 0,
            borrow_guard: [0; 32],
            extension_commitment: [0; 32],
        };
        TokenUtxo { utxo: utxo(tag, KAS, Some(TOKEN)), state: TokenState::Kcc20(st) }
    }
    fn ask(maker: u8) -> AskState {
        let t = token_template(program());
        AskState {
            maker: pk(maker),
            token_cov_id: TOKEN,
            token_tpl_hash: t.hash,
            tpl_prefix_len: t.prefix.len() as i64,
            tpl_suffix_len: t.suffix.len() as i64,
            scale: 1_000,
            min_fill: 1_000,
            price: 25_000_000,
            tip: 100_000,
            tif: 0,
            active_from: 0,
            expiry_daa: 499_999_999_999,
            refund_tip: kob_protocol::defaults::tips(program()).expect("tips").refund_tip as i64,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 1_000,
            amount_left: 10_000,
            extension_commitment: [0; 32],
        }
    }
    fn check(a: &Action) -> (kob_protocol::tx::BuiltTx, Validation) {
        let built = build(a).unwrap_or_else(|e| panic!("build: {e}"));
        let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        let v = validate_signed(&signed).unwrap_or_else(|e| panic!("engine: {e}"));
        (built, v)
    }

    /// A KaspaRocket token's ask is a plain `KobAsk` (the KCC-20 kinds serve the program unchanged): placing it moves the
    /// maker's tokens into the order's custody (owner scheme 4), cancelling it returns them to the maker's key (scheme 0).
    #[test]
    fn an_ask_is_placed_and_cancelled() {
        let s = AnyState::KobAsk(ask(1));
        assert_eq!(s.template_id(), TemplateId::KobAsk);
        assert_eq!(TemplateId::KobAsk.for_program(program()), TemplateId::KobAsk);
        let create = Action::CreateOrder(CreateOrder {
            order: s.clone(),
            value: 10 * KAS,
            tokens: vec![holder(1, 1, 12_000)],
            token_carrier: 10 * KAS,
            funding: vec![KeyUtxo { utxo: utxo(2, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            lock_time: 0,
            deadline: None,
            records: vec![],
            fee: FeeOptions::default(),
        });
        let (built, _) = check(&create);
        assert!(built.plans.iter().any(|p| matches!(p, SigPlan::TokenLeader { template, .. } if *template == program())));
        let tx = &built.tx;
        let order_cov = built.covenants[0].covenant_id;
        let custody_out = tx
            .outputs
            .iter()
            .position(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == TOKEN) && o.value == 10 * KAS)
            .unwrap();
        let custody = TokenUtxo {
            utxo: Utxo {
                transaction_id: tx.id,
                index: custody_out as u32,
                amount: 10 * KAS,
                block_daa_score: 600,
                covenant_id: Some(TOKEN),
            },
            state: TokenState::Kcc20(Kcc20State::custody(10_000, order_cov, [0; 32])),
        };
        assert_eq!(
            tx.outputs[custody_out].script_public_key,
            format!("0000{}", kob_protocol::json::to_hex(custody.state.as_kcc20().unwrap().spk_with(template(program())).script()))
        );
        let cancel = Action::CancelOrder(CancelOrder {
            order: OrderUtxo {
                utxo: Utxo { transaction_id: tx.id, index: 0, amount: 10 * KAS, block_daa_score: 600, covenant_id: Some(order_cov) },
                state: s,
            },
            custody: Some(custody),
            prefund: None,
            strays: vec![],
            foreign: vec![],
            tokens: vec![],
            funding: vec![KeyUtxo { utxo: utxo(3, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            replace: None,
            lock_time: 0,
            records: vec![],
            fee: FeeOptions::default(),
        });
        let (built, _) = check(&cancel);
        let SigPlan::TokenLeader { next_states, .. } = built.plans.iter().find(|p| matches!(p, SigPlan::TokenLeader { .. })).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            next_states.iter().map(|s| (s.owner, s.owner_scheme, s.amount)).collect::<Vec<_>>(),
            vec![(pk(1), SCHEME_P2PK, 10_000)]
        );
    }

    /// A key's transfer of `n` holdings to `n` recipients on `p`, built, signed and validated: its mass report and fee.
    fn send(p: TemplateId, n: usize) -> Validation {
        let tokens: Vec<TokenUtxo> = (0..n).map(|k| holder(10 + k as u8, 1, 100)).collect();
        let recipients: Vec<TokenRecipient> =
            (0..n).map(|k| TokenRecipient { pubkey: pk(11 + k as u8), amount: 100, carrier: KAS }).collect();
        let a = Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN, program: p },
            tokens: tokens
                .into_iter()
                .map(|mut t| {
                    if let TokenState::Kcc20(k) = &mut t.state {
                        k.extension_commitment = [0; 32];
                    }
                    t
                })
                .collect(),
            recipients,
            token_change: None,
            token_change_carrier: KAS,
            funding: vec![KeyUtxo { utxo: utxo(2, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            records: vec![],
            fee: FeeOptions::default(),
        });
        check(&a).1
    }

    /// Mass and fee of a transfer at the program's 10 / 10 against KOB's standard program at 3 / 3: one token input carries the
    /// 11,458-byte program (2,915 B for the standard), and each further one carries it again (every delegator reveals its redeem
    /// script).
    #[test]
    fn mass_and_fee_of_a_ten_by_ten_transfer() {
        let std3 = send(TemplateId::Kcc20Ref, 3);
        let kr3 = send(program(), 3);
        let kr10 = send(program(), 10);
        for (name, v) in [("KCC20Ref 3/3", &std3), ("KaspaRocket 3/3", &kr3), ("KaspaRocket 10/10", &kr10)] {
            println!(
                "{name}: size {} B, compute mass {}, transient {}, storage {}, fee {} sompi (floor {})",
                v.mass.size, v.mass.compute, v.mass.transient, v.mass.storage, v.fee, v.min_fee
            );
            assert!(v.mass.within_block_limits());
            assert!(v.fee >= v.min_fee);
        }
        assert!(kr3.mass.size > std3.mass.size && kr10.mass.size > kr3.mass.size);
        assert!(kr10.fee > kr3.fee && kr3.fee > std3.fee);
        // the builders refuse an eleventh token input
        let tokens: Vec<TokenUtxo> = (0..11).map(|k| holder(40 + k as u8, 1, 1)).collect();
        let a = Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN, program: program() },
            tokens,
            recipients: vec![TokenRecipient { pubkey: pk(2), amount: 11, carrier: KAS }],
            token_change: None,
            token_change_carrier: KAS,
            funding: vec![KeyUtxo { utxo: utxo(2, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            records: vec![],
            fee: FeeOptions::default(),
        });
        assert!(build(&a).is_err());
    }
}
