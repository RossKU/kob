//! KaspaCom-token variant of kob_protocol_tests.rs: KOB's UNMODIFIED v1 AskOrder / BidOrder run against
//! the real KaspaCom KCC20 program (kcc20-tx-builder 0.2.5, Apache-2.0, commit 80e1f7b1, vendored as
//! `contracts/third-party/kaspacom-kcc20/KCC20.placeholder.json`: 25,552 B program, 112-byte KCC-20 state at
//! offset 1, dispatch tags 79c71c23 / fd3ef14a, up to 8 token inputs and 8 token outputs) in
//! rusty-kaspa v2.1.0's TxScriptEngine with KIP-20 covenant context and script-unit metering.
//!
//! Only `Token::new` differs from kob_protocol_tests.rs (it loads the artifact instead of compiling the
//! reference KCC-20); the scenario set S1..S9, the negatives and the sweeps are the same, adapted to the
//! 8-slot limit: the reference's "4 token inputs rejected" negative (N13) is a positive here, the
//! negative moved to 9 token inputs (and 9 token outputs), the program's real limits.
//!
//! Contract sources are read from `contracts/` (env KOB_CARRIER_KAS = 2|10|20 sets the order carrier,
//! default 10 KAS).
//! Run: cargo test --release -p kob-tests --test kob_kaspacom_tests -- --nocapture --test-threads=1

mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::mass::{transaction_estimated_serialized_size, MassCalculator};
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32};
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::{ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use kob_protocol::artifacts::TemplateId;

use common::{bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const FAKE_COV: Hash = Hash::from_bytes([0x66; 32]);
const EXT: [u8; 32] = [0xee; 32];
const SCHEME_P2PK: u8 = 0x00;
const SCHEME_COVID: u8 = 0x04;
const SIGOP_SCRIPT_UNITS: u64 = 100_000; // mass_per_sig_op (1000 g) * 100 units/g
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;
const EXPIRY_DAA: i64 = 500_000_000;
const KASPACOM_ARTIFACT: &str = "contracts/third-party/kaspacom-kcc20/KCC20.placeholder.json";
const PINNED_HASH: &str = "911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c";

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn src(name: &str) -> String {
    common::contract_source(name)
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
fn p2pk_spk(pk: &[u8; 32]) -> ScriptPublicKey {
    let mut s = vec![OpData32];
    s.extend_from_slice(pk);
    s.push(OpCheckSig);
    ScriptPublicKey::new(0, s.into())
}

// ---------------------------------------------------------------- token (KaspaCom KCC20 0.2.5)

struct Token {
    art: SilAbiArtifact,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    hash: Vec<u8>,
}

fn tok_state_bytes(amount: i64, owner: &[u8; 32], scheme: u8, borrow: u8) -> Vec<u8> {
    let mut v = vec![0x08];
    v.extend_from_slice(&amount.to_le_bytes());
    v.push(0x20);
    v.extend_from_slice(owner);
    v.extend_from_slice(&[0x01, scheme, 0x01, borrow, 0x20]);
    v.extend_from_slice(&[0u8; 32]);
    v.push(0x20);
    v.extend_from_slice(&EXT);
    v
}
impl Token {
    /// Loads the KaspaCom kcc20-tx-builder 0.2.5 KCC20 artifact (vendored under `contracts/third-party/`)
    /// instead of compiling the reference KCC-20, and checks it is the pinned program of `kob-protocol`.
    fn new() -> Self {
        let json = std::fs::read_to_string(common::repo_root().join(KASPACOM_ARTIFACT)).expect("read KaspaCom artifact");
        let art: SilAbiArtifact = serde_json::from_str(&json).expect("parse KaspaCom artifact");
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        let bc = bytecode(&art);
        assert_eq!(bc.len(), 25_552, "program size");
        assert_eq!((prefix.len(), suffix.len()), (1, 25_439), "template split around the 112-byte state at offset 1");
        assert_eq!(bc.len(), prefix.len() + 112 + suffix.len());
        // The state span of the vendored example instance has the KCC-20 draft shape
        // (amount, owner, owner_scheme, borrow_scheme, borrow_guard, extension_commitment).
        let st = &bc[prefix.len()..prefix.len() + 112];
        for (at, want) in [(0usize, 0x08u8), (9, 0x20), (42, 0x01), (44, 0x01), (46, 0x20), (79, 0x20)] {
            assert_eq!(st[at], want, "state push opcode at {at}");
        }
        // Recomputed template hash = the artifact's recorded one = the pinned one = the one kob-protocol embeds.
        let recomputed = silverscript_abi::template_hash(&prefix, &suffix);
        let lib = kob_protocol::artifacts::token_template(TemplateId::Kcc20KaspaCom025);
        assert_eq!(hash, recomputed.to_vec(), "recorded template hash must equal the recomputed one");
        assert_eq!(hexs(&recomputed), PINNED_HASH, "template hash pinned in registry/tokens.json");
        assert_eq!(recomputed, lib.hash, "kob_protocol::artifacts::token_template(Kcc20KaspaCom025).hash");
        assert_eq!(lib.slots, (8, 8));
        assert_eq!((lib.prefix.clone(), lib.suffix.clone()), (prefix.clone(), suffix.clone()));
        // The two draft dispatch tags (transfer / transfer_delegator) are in the program.
        for tag in [[0x79u8, 0xc7, 0x1c, 0x23], [0xfd, 0x3e, 0xf1, 0x4a]] {
            assert!(bc.windows(4).any(|w| w == tag), "dispatch tag {}", hexs(&tag));
        }
        Token { art, prefix, suffix, hash }
    }
    fn redeem(&self, amount: i64, owner: &[u8; 32], scheme: u8, borrow: u8) -> Vec<u8> {
        [self.prefix.clone(), tok_state_bytes(amount, owner, scheme, borrow), self.suffix.clone()].concat()
    }
    fn spk(&self, amount: i64, owner: &[u8; 32], scheme: u8) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(amount, owner, scheme, 0))
    }
}
fn tok_state(amount: i64, owner: &[u8; 32], scheme: u8, borrow: u8) -> ArtifactValue {
    BTreeMap::from([
        ("amount".to_string(), ArtifactValue::Int(amount)),
        ("owner".to_string(), owner.to_vec().into()),
        ("owner_scheme".to_string(), ArtifactValue::Byte(scheme)),
        ("borrow_scheme".to_string(), ArtifactValue::Byte(borrow)),
        ("borrow_guard".to_string(), vec![0u8; 32].into()),
        ("extension_commitment".to_string(), EXT.to_vec().into()),
    ])
    .into()
}

// ---------------------------------------------------------------- orders

fn ask_art(t: &Token, maker: &[u8; 32], lot_size: i64, lot_price: i64) -> SilAbiArtifact {
    compile_contract(
        &src("AskOrder"),
        &[
            maker.to_vec().into(),
            TOKEN_COV.as_bytes().to_vec().into(),
            t.hash.clone().into(),
            ArtifactValue::Int(t.prefix.len() as i64),
            ArtifactValue::Int(t.suffix.len() as i64),
            ArtifactValue::Int(lot_size),
            ArtifactValue::Int(lot_price),
            ArtifactValue::Int(EXPIRY_DAA),
            ArtifactValue::Int(KAS / 10),
        ],
        CompileOptions::default(),
    )
    .expect("compile AskOrder")
}
fn bid_art(t: &Token, maker: &[u8; 32], lot_size: i64, lot_price: i64, reserve: i64) -> SilAbiArtifact {
    compile_contract(
        &src("BidOrder"),
        &[
            maker.to_vec().into(),
            TOKEN_COV.as_bytes().to_vec().into(),
            t.hash.clone().into(),
            ArtifactValue::Int(t.prefix.len() as i64),
            ArtifactValue::Int(t.suffix.len() as i64),
            EXT.to_vec().into(),
            ArtifactValue::Int(lot_size),
            ArtifactValue::Int(lot_price),
            ArtifactValue::Int(reserve),
            ArtifactValue::Int(carrier()),
            ArtifactValue::Int(EXPIRY_DAA),
            ArtifactValue::Int(KAS / 10),
        ],
        CompileOptions::default(),
    )
    .expect("compile BidOrder")
}
fn spk_of(a: &SilAbiArtifact) -> ScriptPublicKey {
    pay_to_script_hash_script(&bytecode(a))
}

// ---------------------------------------------------------------- scenario model

#[derive(Clone)]
enum Wit {
    CovId,
    P2pk(Keypair),
}
#[derive(Clone)]
enum Role {
    AskFill { art: SilAbiArtifact, n: i64, token_in: i64, change_out: i64 },
    AskRefund { art: SilAbiArtifact, token_in: i64 },
    BidFill { art: SilAbiArtifact, n: i64, tpl_in: i64 },
    BidRefund { art: SilAbiArtifact },
    Cancel { art: SilAbiArtifact, kp: Keypair, sighash: u8 },
    TokLeader { redeem: Vec<u8>, next: Vec<ArtifactValue>, wit: Wit },
    TokDelegate { redeem: Vec<u8>, wit: Wit },
    P2pk { kp: Keypair },
}
#[derive(Clone)]
struct Inp {
    entry: UtxoEntry,
    role: Role,
}
#[derive(Clone)]
struct Scn {
    name: String,
    inputs: Vec<Inp>,
    outputs: Vec<TransactionOutput>,
    lock_time: u64,
}

fn cov_utxo(value: i64, spk: ScriptPublicKey, cov: Hash) -> UtxoEntry {
    UtxoEntry::new(value as u64, spk, 0, false, Some(cov))
}
fn out(value: i64, spk: ScriptPublicKey, cov: Option<(u16, Hash)>) -> TransactionOutput {
    TransactionOutput {
        value: value as u64,
        script_public_key: spk,
        covenant: cov.map(|(a, c)| CovenantBinding { authorizing_input: a, covenant_id: c }),
    }
}
fn ask_in(art: &SilAbiArtifact, value: i64, cov: Hash, n: i64, token_in: i64, change_out: i64) -> Inp {
    Inp { entry: cov_utxo(value, spk_of(art), cov), role: Role::AskFill { art: art.clone(), n, token_in, change_out } }
}
fn bid_in(art: &SilAbiArtifact, value: i64, cov: Hash, n: i64, tpl_in: i64) -> Inp {
    Inp { entry: cov_utxo(value, spk_of(art), cov), role: Role::BidFill { art: art.clone(), n, tpl_in } }
}
fn tok_in(
    t: &Token,
    value: i64,
    amount: i64,
    owner: &[u8; 32],
    scheme: u8,
    cov: Hash,
    role_leader: Option<Vec<ArtifactValue>>,
    wit: Wit,
) -> Inp {
    let redeem = t.redeem(amount, owner, scheme, 0);
    let entry = cov_utxo(value, pay_to_script_hash_script(&redeem), cov);
    let role = match role_leader {
        Some(next) => Role::TokLeader { redeem, next, wit },
        None => Role::TokDelegate { redeem, wit },
    };
    Inp { entry, role }
}
fn p2pk_in(kp: &Keypair, value: i64) -> Inp {
    Inp { entry: UtxoEntry::new(value as u64, p2pk_spk(&pk(kp)), 0, false, None), role: Role::P2pk { kp: *kp } }
}

fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair, ht: u8) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let hty = SigHashType::from_u8(ht).expect("sighash type");
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, hty, &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(ht);
    s
}
fn push(data: &[u8]) -> Vec<u8> {
    push_redeem_script(data)
}
fn entry_ss(art: &SilAbiArtifact, entry: &str, args: &[ArtifactValue]) -> Vec<u8> {
    let mut s = encode_entry_sig_script(art, entry, args).expect("encode entry");
    s.extend_from_slice(&push(&bytecode(art)));
    s
}
fn tok_ss(t: &Token, redeem: &[u8], entry: &str, args: &[ArtifactValue]) -> Vec<u8> {
    let mut s = encode_entry_sig_script(&t.art, entry, args).expect("encode token entry");
    s.extend_from_slice(&push(redeem));
    s
}

fn build(t: &Token, s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = s.inputs.iter().map(|i| i.entry.clone()).collect();
    let inputs: Vec<TransactionInput> = s
        .inputs
        .iter()
        .enumerate()
        .map(|(k, _)| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([k as u8 + 1; 32]), index: k as u32 },
                vec![],
                0,
                budgets.get(k).copied().unwrap_or(0),
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, s.outputs.clone(), s.lock_time, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    for (k, inp) in s.inputs.iter().enumerate() {
        let sig_of = |kp: &Keypair, ht: u8| sign(&unsigned, &entries, k, kp, ht);
        let ss = match &inp.role {
            Role::AskFill { art, n, token_in, change_out } => {
                entry_ss(art, "settle", &[ArtifactValue::Int(*n), ArtifactValue::Int(*token_in), ArtifactValue::Int(*change_out)])
            }
            Role::AskRefund { art, token_in } => {
                entry_ss(art, "settle", &[ArtifactValue::Int(0), ArtifactValue::Int(*token_in), ArtifactValue::Int(0)])
            }
            Role::BidFill { art, n, tpl_in } => entry_ss(art, "fill", &[ArtifactValue::Int(*n), ArtifactValue::Int(*tpl_in)]),
            Role::BidRefund { art } => entry_ss(art, "refund", &[]),
            Role::Cancel { art, kp, sighash } => entry_ss(art, "cancel", &[sig_of(kp, *sighash).into()]),
            Role::TokLeader { redeem, next, wit } => {
                let w = match wit {
                    Wit::CovId => vec![0x00],
                    Wit::P2pk(kp) => [vec![0x00], sig_of(kp, 0x01)].concat(),
                };
                tok_ss(t, redeem, "transfer", &[ArtifactValue::Array(next.clone()), w.into()])
            }
            Role::TokDelegate { redeem, wit } => {
                let w = match wit {
                    Wit::CovId => vec![],
                    Wit::P2pk(kp) => sig_of(kp, 0x01),
                };
                tok_ss(t, redeem, "transfer_delegator", &[w.into()])
            }
            Role::P2pk { kp } => push(&sig_of(kp, 0x01)),
        };
        tx.inputs[k].signature_script = ss;
    }
    (tx, entries)
}

/// Executes every input; returns per-input Ok(used script units) or Err.
fn execute(tx: &Transaction, entries: &[UtxoEntry]) -> Result<Vec<Result<u64, TxScriptError>>, String> {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    Ok((0..tx.inputs.len())
        .map(|i| {
            let input = tx.inputs[i].clone();
            let utxo = &entries[i];
            let mut vm = TxScriptEngine::from_transaction_input(
                &populated,
                &input,
                i,
                utxo,
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
            );
            vm.execute().map(|_| vm.used_script_units().0)
        })
        .collect())
}

fn role_name(r: &Role) -> &'static str {
    match r {
        Role::AskFill { .. } => "ask.fill",
        Role::AskRefund { .. } => "ask.refund",
        Role::BidFill { .. } => "bid.fill",
        Role::BidRefund { .. } => "bid.refund",
        Role::Cancel { .. } => "cancel",
        Role::TokLeader { .. } => "kcc20.transfer",
        Role::TokDelegate { .. } => "kcc20.delegator",
        Role::P2pk { .. } => "p2pk",
    }
}

/// Measurements of a passing scenario.
#[derive(Clone, Copy, Debug)]
struct Rep {
    size: u64,
    compute: u64,
    storage: u64,
    /// The node relay floor in sompi: 100 sompi/gram x max(compute mass, 2 x size).
    min_fee: u64,
}

/// Positive scenario: all inputs must pass. Prints per-input script units, compute budget,
/// sigscript bytes, and tx-level size / compute / storage mass / minimum fee.
fn run_ok(t: &Token, s: &Scn) -> Rep {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => panic!("{}: input {i} ({}) failed: {e:?}", s.name, role_name(&s.inputs[i].role)),
        }
    }
    let (tx, entries) = build(t, s, &budgets);
    let res = execute(&tx, &entries).expect("ctx");
    assert!(res.iter().all(|r| r.is_ok()), "{}: second pass failed", s.name);
    let mc = MassCalculator::new(1, 10, 1_000_000_000_000);
    let nc = mc.calc_non_contextual_masses(&tx);
    let populated = PopulatedTransaction::new(&tx, entries.clone());
    let storage = mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX);
    let size = transaction_estimated_serialized_size(&tx);
    let fee_mass = nc.compute_mass.max(2 * size);
    println!("SCENARIO {}  [PASS]", s.name);
    println!("  inputs={} outputs={}", tx.inputs.len(), tx.outputs.len());
    for (i, r) in res.iter().enumerate() {
        println!(
            "  in[{i}] {:<16} sigscript={:>5} B  script_units={:>7}  compute_budget={}",
            role_name(&s.inputs[i].role),
            tx.inputs[i].signature_script.len(),
            r.as_ref().unwrap(),
            budgets[i]
        );
    }
    println!(
        "  tx_size={} B  compute_mass={}  2xsize={}  storage_mass={}  => min_fee={} sompi ({:.5} KAS)",
        size,
        nc.compute_mass,
        2 * size,
        storage,
        fee_mass * MIN_FEE_PER_GRAM,
        (fee_mass * MIN_FEE_PER_GRAM) as f64 / 1e8
    );
    Rep { size, compute: nc.compute_mass, storage, min_fee: fee_mass * MIN_FEE_PER_GRAM }
}

/// Negative scenario: input `expect` must fail (other inputs are reported).
fn run_bad(t: &Token, s: &Scn, expect: usize) {
    let (tx, entries) = build(t, s, &[]);
    match execute(&tx, &entries) {
        Err(e) => panic!("{}: tx-level failure {e} (expected input {expect} to reject)", s.name),
        Ok(res) => {
            let r = &res[expect];
            assert!(r.is_err(), "{}: expected input {expect} ({}) to REJECT but it passed", s.name, role_name(&s.inputs[expect].role));
            let others: Vec<String> =
                res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
            println!(
                "NEGATIVE {}  [REJECTED at in[{expect}] {}: {:?}]{}",
                s.name,
                role_name(&s.inputs[expect].role),
                r.as_ref().unwrap_err(),
                if others.is_empty() { String::new() } else { format!(" (also failing: {})", others.join(",")) }
            );
        }
    }
}

// ---------------------------------------------------------------- fixtures

struct Fx {
    t: Token,
    maker_a: Keypair,
    maker_b: Keypair,
    maker_c: Keypair,
    taker: Keypair,
}
fn fx() -> Fx {
    Fx { t: Token::new(), maker_a: keypair(), maker_b: keypair(), maker_c: keypair(), taker: keypair() }
}
fn cov(b: u8) -> Hash {
    Hash::from_bytes([b; 32])
}
const LOT: i64 = 1_000; // token units per lot
/// KAS carried by every covenant UTXO in these fixtures (env KOB_CARRIER_KAS, default 2).
fn carrier() -> i64 {
    std::env::var("KOB_CARRIER_KAS").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(10) * KAS
}

/// S1: taker buys 4 of 10 lots from one ask (partial fill).
fn s1(f: &Fx) -> Scn {
    let t = &f.t;
    let a = cov(0xa1);
    let price = 25 * KAS / 10;
    let ask = ask_art(t, &pk(&f.maker_a), LOT, price);
    let taker = pk(&f.taker);
    Scn {
        name: "S1 taker buys 4/10 lots from 1 ask (partial)".into(),
        inputs: vec![
            ask_in(&ask, carrier(), a, 4, 1, 2),
            tok_in(
                t,
                carrier(),
                10 * LOT,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(6 * LOT, &a.as_bytes(), SCHEME_COVID, 0), tok_state(4 * LOT, &taker, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(4 * price, p2pk_spk(&pk(&f.maker_a)), None),
            out(carrier(), spk_of(&ask), Some((0, a))),
            out(carrier(), t.spk(6 * LOT, &a.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))),
            out(carrier(), t.spk(4 * LOT, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - 4 * price - carrier() - KAS / 10, p2pk_spk(&taker), None),
        ],
        lock_time: 0,
    }
}

/// S2: taker sweeps 3 asks (A full 5 lots, B full 3 lots, same maker as A; C partial 4/10 at a
/// higher price) in one tx: 3 token inputs (see `sweep` for up to 8).
fn s2(f: &Fx) -> Scn {
    let t = &f.t;
    let (ca, cb, cc) = (cov(0xa1), cov(0xa2), cov(0xa3));
    let (pa, pc) = (25 * KAS / 10, 26 * KAS / 10);
    let ask_a = ask_art(t, &pk(&f.maker_a), LOT, pa);
    let ask_b = ask_art(t, &pk(&f.maker_a), LOT, pa);
    let ask_c = ask_art(t, &pk(&f.maker_c), LOT, pc);
    let taker = pk(&f.taker);
    let bought = 5 * pa + 3 * pa + 4 * pc;
    Scn {
        name: "S2 taker sweeps 3 asks (2 full + 1 partial), 3 token inputs".into(),
        inputs: vec![
            ask_in(&ask_a, carrier(), ca, 5, 3, 0),
            ask_in(&ask_b, carrier(), cb, 3, 4, 0),
            ask_in(&ask_c, carrier(), cc, 4, 5, 4),
            tok_in(
                t,
                carrier(),
                5 * LOT,
                &ca.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(6 * LOT, &cc.as_bytes(), SCHEME_COVID, 0), tok_state(12 * LOT, &taker, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
            tok_in(t, carrier(), 3 * LOT, &cb.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId),
            tok_in(t, carrier(), 10 * LOT, &cc.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(5 * pa + 2 * carrier(), p2pk_spk(&pk(&f.maker_a)), None),
            out(3 * pa + 2 * carrier(), p2pk_spk(&pk(&f.maker_a)), None),
            out(4 * pc, p2pk_spk(&pk(&f.maker_c)), None),
            out(carrier(), spk_of(&ask_c), Some((2, cc))),
            out(carrier(), t.spk(6 * LOT, &cc.as_bytes(), SCHEME_COVID), Some((3, TOKEN_COV))),
            out(carrier(), t.spk(12 * LOT, &taker, SCHEME_P2PK), Some((3, TOKEN_COV))),
            out(1000 * KAS - bought - carrier() - KAS / 10, p2pk_spk(&taker), None),
        ],
        lock_time: 0,
    }
}

/// S3: taker sells 4 lots into one bid (partial); fee paid from proceeds, no funding input.
fn s3(f: &Fx) -> Scn {
    let t = &f.t;
    let b = cov(0xb1);
    let price = 24 * KAS / 10;
    let reserve = KAS;
    let bid = bid_art(t, &pk(&f.maker_b), LOT, price, reserve);
    let taker = pk(&f.taker);
    let bid_value = 10 * price + reserve + 3 * carrier();
    Scn {
        name: "S3 taker sells 4 lots into 1 bid (partial)".into(),
        inputs: vec![
            bid_in(&bid, bid_value, b, 4, 1),
            tok_in(
                t,
                carrier(),
                7 * LOT,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(3 * LOT, &taker, SCHEME_P2PK, 0)]),
                Wit::P2pk(f.taker),
            ),
        ],
        outputs: vec![
            out(carrier(), t.spk(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(bid_value - 4 * price - carrier(), spk_of(&bid), Some((0, b))),
            out(carrier(), t.spk(3 * LOT, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(4 * price - KAS / 10, p2pk_spk(&taker), None),
        ],
        lock_time: 0,
    }
}

/// S4: taker sells 6 lots into 2 bids: B1 partial (continues), B2 exhausted (terminates).
fn s4(f: &Fx) -> Scn {
    let t = &f.t;
    let (b1, b2) = (cov(0xb1), cov(0xb2));
    let (p1, p2) = (24 * KAS / 10, 245 * KAS / 100);
    let reserve = KAS;
    let bid1 = bid_art(t, &pk(&f.maker_b), LOT, p1, reserve);
    let bid2 = bid_art(t, &pk(&f.maker_c), LOT, p2, reserve);
    let taker = pk(&f.taker);
    let (v1, v2) = (10 * p1 + reserve + 3 * carrier(), 2 * p2 + reserve + carrier());
    Scn {
        name: "S4 taker sells into 2 bids (1 partial + 1 exhausted)".into(),
        inputs: vec![
            bid_in(&bid1, v1, b1, 4, 2),
            bid_in(&bid2, v2, b2, 2, 2),
            tok_in(
                t,
                carrier(),
                6 * LOT,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(2 * LOT, &pk(&f.maker_c), SCHEME_P2PK, 0)]),
                Wit::P2pk(f.taker),
            ),
        ],
        outputs: vec![
            out(carrier(), t.spk(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(reserve + carrier(), t.spk(2 * LOT, &pk(&f.maker_c), SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(v1 - 4 * p1 - carrier(), spk_of(&bid1), Some((0, b1))),
            out(4 * p1 + 2 * p2 + carrier() - KAS / 10, p2pk_spk(&taker), None),
        ],
        lock_time: 0,
    }
}

/// S5: permissionless keeper crosses 1 bid x 2 asks (ask A full, ask C partial) with no capital
/// of its own; it keeps the price spread minus the network fee.
fn s5(f: &Fx) -> Scn {
    let t = &f.t;
    let (b, ca, cc) = (cov(0xb1), cov(0xa1), cov(0xa3));
    let (pb, pa, pc) = (26 * KAS / 10, 25 * KAS / 10, 255 * KAS / 100);
    let reserve = KAS;
    let bid = bid_art(t, &pk(&f.maker_b), LOT, pb, reserve);
    let ask_a = ask_art(t, &pk(&f.maker_a), LOT, pa);
    let ask_c = ask_art(t, &pk(&f.maker_c), LOT, pc);
    let keeper = keypair();
    let bid_value = 8 * pb + reserve + carrier();
    let spread = 8 * pb - 5 * pa - 3 * pc;
    Scn {
        name: "S5 keeper crosses 1 bid x 2 asks (no keeper capital)".into(),
        inputs: vec![
            bid_in(&bid, bid_value, b, 8, 3),
            ask_in(&ask_a, carrier(), ca, 5, 3, 0),
            ask_in(&ask_c, carrier(), cc, 3, 4, 4),
            tok_in(
                t,
                carrier(),
                5 * LOT,
                &ca.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(8 * LOT, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(7 * LOT, &cc.as_bytes(), SCHEME_COVID, 0)]),
                Wit::CovId,
            ),
            tok_in(t, carrier(), 10 * LOT, &cc.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId),
        ],
        outputs: vec![
            out(reserve + carrier(), t.spk(8 * LOT, &pk(&f.maker_b), SCHEME_P2PK), Some((3, TOKEN_COV))),
            out(5 * pa + 2 * carrier(), p2pk_spk(&pk(&f.maker_a)), None),
            out(3 * pc, p2pk_spk(&pk(&f.maker_c)), None),
            out(carrier(), spk_of(&ask_c), Some((2, cc))),
            out(carrier(), t.spk(7 * LOT, &cc.as_bytes(), SCHEME_COVID), Some((3, TOKEN_COV))),
            out(spread - KAS / 10, p2pk_spk(&pk(&keeper)), None),
        ],
        lock_time: 0,
    }
}

/// S6: maker cancels an ask (SIGHASH_ALL): tokens + carriers back to the maker.
fn s6(f: &Fx, sighash: u8, signer: Keypair) -> Scn {
    let t = &f.t;
    let a = cov(0xa1);
    let ask = ask_art(t, &pk(&f.maker_a), LOT, 25 * KAS / 10);
    let m = pk(&f.maker_a);
    Scn {
        name: format!("S6 maker cancels ask (sighash {sighash:#04x})"),
        inputs: vec![
            Inp { entry: cov_utxo(carrier(), spk_of(&ask), a), role: Role::Cancel { art: ask.clone(), kp: signer, sighash } },
            tok_in(
                t,
                carrier(),
                10 * LOT,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(10 * LOT, &m, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
        ],
        outputs: vec![
            out(carrier(), t.spk(10 * LOT, &m, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(carrier() - KAS / 10, p2pk_spk(&m), None),
        ],
        lock_time: 0,
    }
}
/// S7: maker cancels a bid.
fn s7(f: &Fx) -> Scn {
    let t = &f.t;
    let b = cov(0xb1);
    let bid = bid_art(t, &pk(&f.maker_b), LOT, 24 * KAS / 10, KAS);
    Scn {
        name: "S7 maker cancels bid".into(),
        inputs: vec![Inp {
            entry: cov_utxo(25 * KAS, spk_of(&bid), b),
            role: Role::Cancel { art: bid.clone(), kp: f.maker_b, sighash: 0x01 },
        }],
        outputs: vec![out(25 * KAS - KAS / 10, p2pk_spk(&pk(&f.maker_b)), None)],
        lock_time: 0,
    }
}
/// S8: anyone refunds an expired ask (tx lockTime >= expiry is the only provable time fact).
fn s8(f: &Fx, lock_time: u64) -> Scn {
    let t = &f.t;
    let a = cov(0xa1);
    let ask = ask_art(t, &pk(&f.maker_a), LOT, 25 * KAS / 10);
    let m = pk(&f.maker_a);
    Scn {
        name: format!("S8 keeper refunds expired ask (lockTime {lock_time})"),
        inputs: vec![
            Inp { entry: cov_utxo(carrier(), spk_of(&ask), a), role: Role::AskRefund { art: ask.clone(), token_in: 1 } },
            tok_in(
                t,
                carrier(),
                10 * LOT,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(10 * LOT, &m, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
        ],
        outputs: vec![out(2 * carrier() - KAS / 10, t.spk(10 * LOT, &m, SCHEME_P2PK), Some((1, TOKEN_COV)))],
        lock_time,
    }
}
/// S9: anyone refunds an expired bid.
fn s9(f: &Fx, lock_time: u64) -> Scn {
    let t = &f.t;
    let b = cov(0xb1);
    let bid = bid_art(t, &pk(&f.maker_b), LOT, 24 * KAS / 10, KAS);
    Scn {
        name: format!("S9 keeper refunds expired bid (lockTime {lock_time})"),
        inputs: vec![Inp { entry: cov_utxo(25 * KAS, spk_of(&bid), b), role: Role::BidRefund { art: bid.clone() } }],
        outputs: vec![out(25 * KAS - KAS / 10, p2pk_spk(&pk(&f.maker_b)), None)],
        lock_time,
    }
}

/// Sweep of `k` asks of one token in one tx: ask i holds 2 lots at (25 + i) / 10 KAS per lot; the first
/// k-1 are bought fully, the last one partially (1 of 2 lots). k token inputs (custodies), the leader is
/// the first; outputs: k positional payouts, the last ask's continuation, its token change, the taker's
/// tokens, the taker's KAS change. KaspaCom's program allows k <= 8 (the reference KCC-20 only 3).
fn sweep(f: &Fx, k: usize, fee: i64) -> Scn {
    let t = &f.t;
    let taker = pk(&f.taker);
    let ids: Vec<Hash> = (0..k).map(|i| cov(0xa1 + i as u8)).collect();
    let price = |i: usize| (25 + i as i64) * KAS / 10;
    let maker = |i: usize| if i.is_multiple_of(2) { pk(&f.maker_a) } else { pk(&f.maker_c) };
    let asks: Vec<SilAbiArtifact> = (0..k).map(|i| ask_art(t, &maker(i), LOT, price(i))).collect();
    let last = k - 1;
    let mut inputs = vec![];
    for i in 0..k {
        let (n, change_out) = if i == last { (1, k as i64 + 1) } else { (2, 0) };
        inputs.push(ask_in(&asks[i], carrier(), ids[i], n, (k + i) as i64, change_out));
    }
    let next =
        vec![tok_state(LOT, &ids[last].as_bytes(), SCHEME_COVID, 0), tok_state((2 * k as i64 - 1) * LOT, &taker, SCHEME_P2PK, 0)];
    for (i, id) in ids.iter().enumerate() {
        let lead = if i == 0 { Some(next.clone()) } else { None };
        inputs.push(tok_in(t, carrier(), 2 * LOT, &id.as_bytes(), SCHEME_COVID, TOKEN_COV, lead, Wit::CovId));
    }
    inputs.push(p2pk_in(&f.taker, 1000 * KAS));
    let mut outputs = vec![];
    let mut bought = 0;
    for i in 0..k {
        if i == last {
            outputs.push(out(price(i), p2pk_spk(&maker(i)), None));
            bought += price(i);
        } else {
            outputs.push(out(2 * price(i) + 2 * carrier(), p2pk_spk(&maker(i)), None));
            bought += 2 * price(i);
        }
    }
    outputs.push(out(carrier(), spk_of(&asks[last]), Some((last as u16, ids[last]))));
    outputs.push(out(carrier(), t.spk(LOT, &ids[last].as_bytes(), SCHEME_COVID), Some((k as u16, TOKEN_COV))));
    outputs.push(out(carrier(), t.spk((2 * k as i64 - 1) * LOT, &taker, SCHEME_P2PK), Some((k as u16, TOKEN_COV))));
    outputs.push(out(1000 * KAS - bought - carrier() - fee, p2pk_spk(&taker), None));
    Scn { name: format!("S2x taker sweeps {k} asks ({} full + 1 partial), {k} token inputs", k - 1), inputs, outputs, lock_time: 0 }
}

/// Pure token transfer by the taker: `n_in` P2PK token inputs (owner = taker, one signature each, the first
/// is the leader) into `n_out` token outputs, `max(n_in, n_out)` lots in total: every input but the first and
/// every output but the last carries exactly 1 lot, the first input / last output take the remainder.
fn token_slots(f: &Fx, n_in: usize, n_out: usize, fee: i64) -> Scn {
    let t = &f.t;
    let total = n_in.max(n_out) as i64;
    let owner = |j: usize| [0x30 + j as u8; 32];
    let out_lots = |j: usize| if j + 1 == n_out { total - (n_out as i64 - 1) } else { 1 };
    let in_lots = |i: usize| if i == 0 { total - (n_in as i64 - 1) } else { 1 };
    let next: Vec<ArtifactValue> = (0..n_out).map(|j| tok_state(out_lots(j) * LOT, &owner(j), SCHEME_P2PK, 0)).collect();
    let taker = pk(&f.taker);
    let inputs = (0..n_in)
        .map(|i| {
            tok_in(
                t,
                (if i == 0 { total - (n_in as i64 - 1) } else { 1 }) * carrier(),
                in_lots(i) * LOT,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                (i == 0).then(|| next.clone()),
                Wit::P2pk(f.taker),
            )
        })
        .collect();
    let outputs = (0..n_out)
        .map(|j| {
            let value = if j + 1 == n_out { (total - (n_out as i64 - 1)) * carrier() - fee } else { carrier() };
            out(value, t.spk(out_lots(j) * LOT, &owner(j), SCHEME_P2PK), Some((0, TOKEN_COV)))
        })
        .collect();
    Scn { name: format!("T{n_in}x{n_out} taker moves {n_in} token inputs into {n_out} token outputs"), inputs, outputs, lock_time: 0 }
}

fn with_name(mut s: Scn, name: &str) -> Scn {
    s.name = name.into();
    s
}
fn set_ask_n(s: &mut Scn, idx: usize, new_n: i64) {
    if let Role::AskFill { n, .. } = &mut s.inputs[idx].role {
        *n = new_n;
    }
}
fn set_bid_n(s: &mut Scn, idx: usize, new_n: i64) {
    if let Role::BidFill { n, .. } = &mut s.inputs[idx].role {
        *n = new_n;
    }
}
fn set_leader_next(s: &mut Scn, idx: usize, new_next: Vec<ArtifactValue>) {
    if let Role::TokLeader { next, .. } = &mut s.inputs[idx].role {
        *next = new_next;
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn kob_sizes_and_sigops() {
    let f = fx();
    let ask = ask_art(&f.t, &pk(&f.maker_a), LOT, KAS);
    let bid = bid_art(&f.t, &pk(&f.maker_b), LOT, KAS, KAS);
    for (name, bc) in [("AskOrder", bytecode(&ask)), ("BidOrder", bytecode(&bid)), ("KaspaComKCC20", bytecode(&f.t.art))] {
        let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
            &push(&bc),
            &pay_to_script_hash_script(&bc),
        );
        println!("SIZE {name}: redeem script {} B, static sig-op upper bound {so}", bc.len());
    }
    println!("SIZE KaspaComKCC20 template: prefix {} B, state 112 B, suffix {} B", f.t.prefix.len(), f.t.suffix.len());
}

#[test]
fn kob_positive_match_shapes() {
    let f = fx();
    run_ok(&f.t, &s1(&f));
    run_ok(&f.t, &s2(&f));
    run_ok(&f.t, &s3(&f));
    run_ok(&f.t, &s4(&f));
    run_ok(&f.t, &s5(&f));
    run_ok(&f.t, &s6(&f, 0x01, f.maker_a));
    run_ok(&f.t, &s7(&f));
    run_ok(&f.t, &s8(&f, EXPIRY_DAA as u64));
    run_ok(&f.t, &s9(&f, EXPIRY_DAA as u64));
}

#[test]
fn kob_negative_ask() {
    let f = fx();
    let t = &f.t;
    let a = cov(0xa1);
    let taker = pk(&f.taker);

    let mut s = with_name(s1(&f), "N1 ask: maker underpaid by 1 sompi");
    s.outputs[0].value -= 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s2(&f), "N2 ask: payout aliasing (ask B's payout slot redirected; one output cannot serve two asks)");
    s.outputs[1].script_public_key = p2pk_spk(&taker);
    run_bad(t, &s, 1);

    let mut s = with_name(s1(&f), "N3 ask: unsold tokens leave order custody (change owned by taker)");
    s.outputs[2].script_public_key = t.spk(6 * LOT, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6 * LOT, &taker, SCHEME_P2PK, 0), tok_state(4 * LOT, &taker, SCHEME_P2PK, 0)]);
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N4 ask: order terminated while tokens remain (continuation dropped)");
    s.outputs.remove(1);
    s.outputs[1].script_public_key = t.spk(6 * LOT, &taker, SCHEME_P2PK);
    if let Role::AskFill { change_out, .. } = &mut s.inputs[0].role {
        *change_out = 1;
    }
    for o in s.outputs.iter_mut() {
        if let Some(c) = o.covenant.as_mut() {
            c.authorizing_input = 1;
        }
    }
    set_leader_next(&mut s, 1, vec![tok_state(6 * LOT, &taker, SCHEME_P2PK, 0), tok_state(4 * LOT, &taker, SCHEME_P2PK, 0)]);
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N5 ask: continuation KAS carrier skimmed by 1 sompi");
    s.outputs[1].value -= 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N5b ask: token-change KAS carrier skimmed by 1 sompi");
    s.outputs[2].value -= 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s2(&f), "N6 ask: full fill but KAS carriers not returned to maker");
    s.outputs[0].value = (5 * (25 * KAS / 10)) as u64;
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N7 ask: qty*price overflow is rejected by guard");
    let huge = ask_art(t, &pk(&f.maker_a), LOT, i64::MAX / 2);
    s.inputs[0] = ask_in(&huge, carrier(), a, 4, 1, 2);
    s.outputs[1].script_public_key = spk_of(&huge);
    s.outputs[0].value = 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N8 ask: malleated qty arg n=5 (outputs unchanged)");
    set_ask_n(&mut s, 0, 5);
    run_bad(t, &s, 0);
    let mut s = with_name(s1(&f), "N8b ask: malleated qty arg n=3 (outputs unchanged)");
    set_ask_n(&mut s, 0, 3);
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N8c ask: malleated n=0 turns the fill into a refund (before expiry)");
    set_ask_n(&mut s, 0, 0);
    run_bad(t, &s, 0);

    let mut s = with_name(s1(&f), "N9 ask: token change made borrowable (borrow_scheme 0x01)");
    s.outputs[2].script_public_key = pay_to_script_hash_script(&t.redeem(6 * LOT, &a.as_bytes(), SCHEME_COVID, 0x01));
    set_leader_next(
        &mut s,
        1,
        vec![tok_state(6 * LOT, &a.as_bytes(), SCHEME_COVID, 0x01), tok_state(4 * LOT, &taker, SCHEME_P2PK, 0)],
    );
    run_bad(t, &s, 0);

    // duplicate covenant id: a second UTXO carrying the same order covenant id (malformed genesis)
    let mut s = with_name(s1(&f), "N10 ask: two inputs share the order covenant id (singleton check)");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(t, &s, 0);

    // token input not owned by the order (taker's own tokens offered as the order's balance)
    let mut s = with_name(s1(&f), "N11 ask: token input not owned by the order");
    s.inputs[1] = tok_in(
        t,
        carrier(),
        10 * LOT,
        &taker,
        SCHEME_P2PK,
        TOKEN_COV,
        Some(vec![tok_state(6 * LOT, &a.as_bytes(), SCHEME_COVID, 0), tok_state(4 * LOT, &taker, SCHEME_P2PK, 0)]),
        Wit::P2pk(f.taker),
    );
    run_bad(t, &s, 0);

    // counterfeit: taker output mints +1 lot from nothing -> the KCC-20 leader rejects (conservation)
    let mut s = with_name(s1(&f), "N12 token: counterfeit (+1 lot minted in taker output) -> KCC-20 leader rejects");
    s.outputs[3].script_public_key = t.spk(5 * LOT, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6 * LOT, &a.as_bytes(), SCHEME_COVID, 0), tok_state(5 * LOT, &taker, SCHEME_P2PK, 0)]);
    run_bad(t, &s, 1);

    // The reference KCC-20 (3 token inputs) rejects a 4-ask sweep; KaspaCom's program takes up to 8 token
    // inputs (positive in `kob_kaspacom_sweeps`). The real bound is 8: 9 asks = 9 token inputs of one
    // token -> the KaspaCom leader rejects (the v1 orders have no bound of their own).
    run_bad(t, &sweep(&f, 9, KAS), 9);
}

#[test]
fn kob_negative_bid() {
    let f = fx();
    let t = &f.t;
    let taker = pk(&f.taker);

    let mut s = with_name(s3(&f), "N20 bid: under-delivery (3999 tokens for 4 lots)");
    s.outputs[0].script_public_key = t.spk(4 * LOT - 1, &pk(&f.maker_b), SCHEME_P2PK);
    s.outputs[2].script_public_key = t.spk(3 * LOT + 1, &taker, SCHEME_P2PK);
    set_leader_next(
        &mut s,
        1,
        vec![tok_state(4 * LOT - 1, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(3 * LOT + 1, &taker, SCHEME_P2PK, 0)],
    );
    run_bad(t, &s, 0);

    let mut s = with_name(s3(&f), "N21 bid: tokens delivered to the taker instead of the maker");
    s.outputs[0].script_public_key = t.spk(4 * LOT, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(4 * LOT, &taker, SCHEME_P2PK, 0), tok_state(3 * LOT, &taker, SCHEME_P2PK, 0)]);
    run_bad(t, &s, 0);

    let mut s = with_name(s3(&f), "N22 bid: continuation KAS short by 1 sompi");
    s.outputs[1].value -= 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s3(&f), "N23 bid: forced termination while buying power remains (grief)");
    let v = s.outputs[1].value;
    s.outputs.remove(1);
    s.outputs[0].value += v;
    run_bad(t, &s, 0);

    let mut s = with_name(s3(&f), "N24 bid: malleated qty arg n=3 (outputs unchanged)");
    set_bid_n(&mut s, 0, 3);
    run_bad(t, &s, 0);

    // counterfeit token family: same template, different covenant id
    let mut s = with_name(s3(&f), "N25 bid: fake token (same template, foreign covenant id)");
    s.inputs[1].entry = cov_utxo(carrier(), s.inputs[1].entry.script_public_key.clone(), FAKE_COV);
    for o in s.outputs.iter_mut() {
        if let Some(c) = o.covenant.as_mut() {
            if c.covenant_id == TOKEN_COV {
                c.covenant_id = FAKE_COV;
            }
        }
    }
    run_bad(t, &s, 0);

    let mut s = with_name(s4(&f), "N26 bid: aliasing (bid 2's delivery slot redirected to the taker)");
    s.outputs[1].script_public_key = t.spk(2 * LOT, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 2, vec![tok_state(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(2 * LOT, &taker, SCHEME_P2PK, 0)]);
    run_bad(t, &s, 1);

    let mut s = with_name(s4(&f), "N27 bid: exhausted bid's leftover KAS (reserve) not returned");
    s.outputs[1].value = 1;
    run_bad(t, &s, 1);

    let mut s = with_name(s3(&f), "N29 bid: delivery carrier skimmed by 1 sompi on a partial fill");
    s.outputs[0].value -= 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s3(&f), "N28 bid: n*lotSize overflow is rejected by guard");
    let huge = bid_art(t, &pk(&f.maker_b), i64::MAX / 2, 1, KAS);
    s.inputs[0] = bid_in(&huge, 25 * KAS, cov(0xb1), 4, 1);
    s.outputs[1].script_public_key = spk_of(&huge);
    run_bad(t, &s, 0);
}

#[test]
fn kob_negative_cancel_refund() {
    let f = fx();
    let t = &f.t;
    run_bad(t, &with_name(s6(&f, 0x81, f.maker_a), "N30 cancel: SIGHASH_ALL|ANYONECANPAY refused"), 0);
    run_bad(t, &with_name(s6(&f, 0x01, f.taker), "N31 cancel: wrong key"), 0);
    run_bad(t, &with_name(s8(&f, EXPIRY_DAA as u64 - 1), "N32 refund: ask before expiry (lockTime = expiry-1)"), 0);
    run_bad(t, &with_name(s9(&f, EXPIRY_DAA as u64 - 1), "N33 refund: bid before expiry"), 0);

    let mut s = with_name(s8(&f, EXPIRY_DAA as u64), "N34 refund: keeper redirects the ask's tokens to itself");
    let thief = pk(&f.taker);
    s.outputs[0].script_public_key = t.spk(10 * LOT, &thief, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(10 * LOT, &thief, SCHEME_P2PK, 0)]);
    run_bad(t, &s, 0);

    let mut s = with_name(s8(&f, EXPIRY_DAA as u64), "N35 refund: keeper takes more than refundTip");
    s.outputs[0].value -= 1;
    run_bad(t, &s, 0);

    let mut s = with_name(s9(&f, EXPIRY_DAA as u64), "N36 refund: bid refund paid to a non-maker key");
    s.outputs[0].script_public_key = p2pk_spk(&pk(&f.taker));
    run_bad(t, &s, 0);
}

#[test]
fn kob_kaspacom_template_is_the_pinned_program() {
    // `Token::new` asserts size, split, state shape, dispatch tags and the three-way hash equality.
    let f = fx();
    println!(
        "PINNED KaspaCom KCC20 0.2.5: template hash {}, prefix {} B, suffix {} B",
        hexs(&f.t.hash),
        f.t.prefix.len(),
        f.t.suffix.len()
    );
}

/// Sweeps of 1..=8 asks (= 1..=8 token inputs, the program's limit): every order and token input validates
/// under exact budgets; the sweep must fit the block mass limits and its fee floor is reported.
#[test]
fn kob_kaspacom_sweeps() {
    let f = fx();
    let fee = KAS;
    for k in 1..=8usize {
        let s = sweep(&f, k, fee);
        let rep = run_ok(&f.t, &s);
        println!(
            "SWEEP k={k}: size={} B compute={} storage={} min_fee={} sompi (paid {fee}); block limits: compute<=500000 {} transient(4xsize)<=1000000 {}",
            rep.size,
            rep.compute,
            rep.storage,
            rep.min_fee,
            rep.compute <= 500_000,
            4 * rep.size <= 1_000_000
        );
        assert!(rep.min_fee <= fee as u64, "k={k}: fee floor {} above the paid fee", rep.min_fee);
    }
}

/// The program's own slot limits: 8 token inputs / 8 token outputs pass, 9 of either are rejected by the
/// leader (input 0).
#[test]
fn kob_kaspacom_token_slot_limits() {
    let f = fx();
    let fee = KAS;
    for (n_in, n_out) in [(1, 8), (8, 1), (8, 8)] {
        let rep = run_ok(&f.t, &token_slots(&f, n_in, n_out, fee));
        assert!(rep.min_fee <= fee as u64);
    }
    run_bad(&f.t, &token_slots(&f, 1, 9, fee), 0);
    run_bad(&f.t, &token_slots(&f, 9, 1, fee), 0);
}
