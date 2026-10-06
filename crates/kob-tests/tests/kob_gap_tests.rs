//! KOB v1 gap-review harness: AskOrder / BidOrder + reference KCC-20 executed in rusty-kaspa v2.1.0's
//! TxScriptEngine with KIP-20 covenant context and script-unit metering.
//!
//! Superset of kob_protocol_tests.rs (its five tests are repeated here) plus swap-and-pay shapes, order
//! creation, cancel-replace and budget enforcement.
//! Contract sources are read from `contracts/` (env KOB_CARRIER_KAS = 2|10|20, default 10 KAS).
//! Run: cargo test --release -p kob-tests --test kob_gap_tests -- --nocapture --test-threads=1

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

// ---------------------------------------------------------------- token (reference KCC-20)

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
    fn new() -> Self {
        let art = compile_contract(
            &src("KCC20Ref"),
            &[
                ArtifactValue::Int(1000),
                vec![3u8; 32].into(),
                ArtifactValue::Byte(SCHEME_COVID),
                ArtifactValue::Byte(0),
                vec![0u8; 32].into(),
                EXT.to_vec().into(),
            ],
            CompileOptions::default(),
        )
        .expect("compile KCC20Ref");
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        let t = Token { art, prefix, suffix, hash };
        assert_eq!(t.redeem(1000, &[3u8; 32], SCHEME_COVID, 0), bytecode(&t.art), "manual KCC-20 state encoding must match compiler");
        t
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
    build_p(t, s, budgets, &[])
}
fn build_p(t: &Token, s: &Scn, budgets: &[u16], payload: &[u8]) -> (Transaction, Vec<UtxoEntry>) {
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
    let mut tx = Transaction::new(1, inputs, s.outputs.clone(), s.lock_time, Default::default(), 0, payload.to_vec());
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

/// Positive scenario: all inputs must pass. Prints per-input script units, compute budget,
/// sigscript bytes, and tx-level size / compute / storage mass / minimum fee.
fn run_ok(t: &Token, s: &Scn) {
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
/// higher price) in one tx: 3 token inputs = the KCC-20 default maximum.
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
    for (name, bc) in [("AskOrder", bytecode(&ask)), ("BidOrder", bytecode(&bid)), ("KCC20Ref", bytecode(&f.t.art))] {
        let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
            &push(&bc),
            &pay_to_script_hash_script(&bc),
        );
        println!("SIZE {name}: redeem script {} B, static sig-op upper bound {so}", bc.len());
    }
    println!("SIZE KCC20Ref template: prefix {} B, state 112 B, suffix {} B", f.t.prefix.len(), f.t.suffix.len());
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

    // 4 asks in one tx: the default KCC-20 bound (3 token inputs) rejects the leader
    let mut s = with_name(s2(&f), "N13 shape: 4 token inputs of one token -> KCC-20 leader rejects (max 3)");
    let cd = cov(0xa4);
    let ask_d = ask_art(t, &pk(&f.maker_c), LOT, 26 * KAS / 10);
    s.inputs.insert(3, ask_in(&ask_d, carrier(), cd, 1, 7, 0));
    // re-index: tokens now at 4,5,6,7 ; fix arg indices of the first three asks
    for (k, ti) in [(0usize, 4i64), (1, 5), (2, 6)] {
        if let Role::AskFill { token_in, change_out, .. } = &mut s.inputs[k].role {
            *token_in = ti;
            if *change_out != 0 {
                *change_out += 1;
            }
        }
    }
    s.inputs.insert(7, tok_in(t, carrier(), LOT, &cd.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId));
    s.outputs.insert(3, out(26 * KAS / 10 + 2 * carrier(), p2pk_spk(&pk(&f.maker_c)), None));
    for o in s.outputs.iter_mut() {
        if let Some(c) = o.covenant.as_mut() {
            if c.covenant_id == TOKEN_COV {
                c.authorizing_input = 4;
            }
        }
    }
    s.outputs[6].script_public_key = t.spk(13 * LOT, &taker, SCHEME_P2PK);
    set_leader_next(
        &mut s,
        4,
        vec![tok_state(6 * LOT, &cov(0xa3).as_bytes(), SCHEME_COVID, 0), tok_state(13 * LOT, &taker, SCHEME_P2PK, 0)],
    );
    run_bad(t, &s, 4);
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

// ================================================================ GAP REVIEW ADDITIONS (2026-09-29)
// Swap-and-pay shapes (x402), order creation (genesis + KOB1 payload), cancel-replace,
// enforced per-input compute budgets, per-input static sig-op scan, block-limit checks.

const TOKEN_A: Hash = TOKEN_COV;
const TOKEN_B: Hash = Hash::from_bytes([0x71; 32]);
const BLOCK_COMPUTE: u64 = 500_000;
const BLOCK_STORAGE: u64 = 500_000;
const BLOCK_TRANSIENT: u64 = 1_000_000;

fn ask_art_c(t: &Token, tok: Hash, maker: &[u8; 32], lot_size: i64, lot_price: i64) -> SilAbiArtifact {
    compile_contract(
        &src("AskOrder"),
        &[
            maker.to_vec().into(),
            tok.as_bytes().to_vec().into(),
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
fn bid_art_c(t: &Token, tok: Hash, maker: &[u8; 32], lot_size: i64, lot_price: i64, reserve: i64) -> SilAbiArtifact {
    compile_contract(
        &src("BidOrder"),
        &[
            maker.to_vec().into(),
            tok.as_bytes().to_vec().into(),
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

fn execute_limited(tx: &Transaction, entries: &[UtxoEntry], enforce: bool) -> Result<Vec<Result<u64, TxScriptError>>, String> {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    Ok((0..tx.inputs.len())
        .map(|i| {
            let input = tx.inputs[i].clone();
            let utxo = &entries[i];
            let limit = if enforce { input.compute_commit.allowed_script_units() } else { u64::MAX.into() };
            let mut vm = TxScriptEngine::from_transaction_input_with_script_units_limit(
                &populated,
                &input,
                i,
                utxo,
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                engine_flags_gap(),
                limit,
            );
            vm.execute().map(|_| vm.used_script_units().0)
        })
        .collect())
}

struct Metrics {
    size: u64,
    compute: u64,
    storage: u64,
    fee_sompi: u64,
}

/// Positive run with payload, enforced budgets (consensus-style limit), per-input sig-op scan and block-limit checks.
fn run_ok_p(t: &Token, s: &Scn, payload: &[u8]) -> Metrics {
    let (tx, entries) = build_p(t, s, &[], payload);
    let res = execute_limited(&tx, &entries, false).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => panic!("{}: input {i} ({}) failed: {e:?}", s.name, role_name(&s.inputs[i].role)),
        }
    }
    let (tx, entries) = build_p(t, s, &budgets, payload);
    let res = execute_limited(&tx, &entries, true).expect("ctx");
    for (i, r) in res.iter().enumerate() {
        assert!(r.is_ok(), "{}: enforced-budget pass failed at in[{i}]: {:?}", s.name, r);
    }
    // budget-1 on the heaviest input must fail (proves the limit is really enforced)
    let (imax, _) = budgets.iter().enumerate().max_by_key(|(_, b)| **b).unwrap();
    if budgets[imax] > 0 {
        let mut b2 = budgets.clone();
        b2[imax] -= 1;
        let (tx2, e2) = build_p(t, s, &b2, payload);
        let r2 = execute_limited(&tx2, &e2, true).expect("ctx");
        assert!(r2[imax].is_err(), "{}: budget-1 at in[{imax}] unexpectedly passed", s.name);
    }
    let mc = MassCalculator::new(1, 10, 1_000_000_000_000);
    let nc = mc.calc_non_contextual_masses(&tx);
    let populated = PopulatedTransaction::new(&tx, entries.clone());
    let storage = mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX);
    let size = transaction_estimated_serialized_size(&tx);
    let fee_mass = nc.compute_mass.max(2 * size);
    let frontier_mass = fee_mass.max(storage);
    println!("GAP-SCENARIO {}  [PASS, budgets enforced]", s.name);
    println!("  inputs={} outputs={} payload={} B", tx.inputs.len(), tx.outputs.len(), payload.len());
    let mut max_sigops = 0;
    for (i, r) in res.iter().enumerate() {
        let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
            &tx.inputs[i].signature_script,
            &entries[i].script_public_key,
        );
        max_sigops = max_sigops.max(so);
        println!(
            "  in[{i}] {:<16} sigscript={:>5} B  script_units={:>7}  budget={:>2}  static_sigops={}",
            role_name(&s.inputs[i].role),
            tx.inputs[i].signature_script.len(),
            r.as_ref().unwrap(),
            budgets[i],
            so
        );
    }
    let in_sum: u64 = entries.iter().map(|e| e.amount).sum();
    let out_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
    let fee_paid = in_sum - out_sum;
    println!(
        "  tx_size={} B compute_mass={} 2xsize={} storage_mass={} transient={} | min_fee={} sompi ({:.5} KAS) frontier_mass={} fee_paid={} sompi ({:.4} KAS) | max static sig-ops/input={}",
        size,
        nc.compute_mass,
        2 * size,
        storage,
        nc.transient_mass,
        fee_mass * MIN_FEE_PER_GRAM,
        (fee_mass * MIN_FEE_PER_GRAM) as f64 / 1e8,
        frontier_mass,
        fee_paid,
        fee_paid as f64 / 1e8,
        max_sigops
    );
    println!(
        "  block share: compute {:.1}%  storage {:.1}%  transient {:.1}%  => max {} such tx/block",
        100.0 * nc.compute_mass as f64 / BLOCK_COMPUTE as f64,
        100.0 * storage as f64 / BLOCK_STORAGE as f64,
        100.0 * nc.transient_mass as f64 / BLOCK_TRANSIENT as f64,
        [BLOCK_COMPUTE / nc.compute_mass.max(1), BLOCK_STORAGE / storage.max(1), BLOCK_TRANSIENT / nc.transient_mass.max(1)]
            .iter()
            .min()
            .unwrap()
    );
    assert!(max_sigops <= 15, "{}: static sig-op cap exceeded", s.name);
    assert!(nc.compute_mass <= BLOCK_COMPUTE && storage <= BLOCK_STORAGE && nc.transient_mass <= BLOCK_TRANSIENT);
    assert!(
        fee_paid >= fee_mass * MIN_FEE_PER_GRAM,
        "{}: fee_paid {} < min relay fee {}",
        s.name,
        fee_paid,
        fee_mass * MIN_FEE_PER_GRAM
    );
    Metrics { size, compute: nc.compute_mass, storage, fee_sompi: fee_mass * MIN_FEE_PER_GRAM }
}

fn sum_in(s: &Scn) -> i64 {
    s.inputs.iter().map(|i| i.entry.amount as i64).sum()
}
fn sum_out(s: &Scn) -> i64 {
    s.outputs.iter().map(|o| o.value as i64).sum()
}
/// Append a P2PK change output taking everything except `fee`.
fn close_with_change(mut s: Scn, to: &[u8; 32], fee: i64) -> Scn {
    let change = sum_in(&s) - sum_out(&s) - fee;
    assert!(change > 0, "{}: negative change {change}", s.name);
    s.outputs.push(out(change, p2pk_spk(to), None));
    s
}

/// SW1: payer holds token A, merchant wants KAS. Payer sells 4 lots of A into a Bid;
/// merchant receives `pay_merchant` KAS; payer keeps token change (+ KAS change).
fn sw1(f: &Fx, merchant: &[u8; 32], pay_merchant: i64, payer_change: bool) -> Scn {
    let t = &f.t;
    let b = cov(0xb1);
    let price = 24 * KAS / 10;
    let reserve = KAS;
    let bid = bid_art_c(t, TOKEN_A, &pk(&f.maker_b), LOT, price, reserve);
    let payer = pk(&f.taker);
    let bid_value = 10 * price + reserve + 3 * carrier();
    let s = Scn {
        name: format!(
            "SW1 A->KAS: payer sells 4 lots into 1 bid, merchant paid {:.2} KAS{}",
            pay_merchant as f64 / 1e8,
            if payer_change { " + payer KAS change" } else { " (no KAS change)" }
        ),
        inputs: vec![
            bid_in(&bid, bid_value, b, 4, 1),
            tok_in(
                t,
                carrier(),
                7 * LOT,
                &payer,
                SCHEME_P2PK,
                TOKEN_A,
                Some(vec![tok_state(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(3 * LOT, &payer, SCHEME_P2PK, 0)]),
                Wit::P2pk(f.taker),
            ),
        ],
        outputs: vec![
            out(carrier(), t.spk(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK), Some((1, TOKEN_A))),
            out(bid_value - 4 * price - carrier(), spk_of(&bid), Some((0, b))),
            out(carrier(), t.spk(3 * LOT, &payer, SCHEME_P2PK), Some((1, TOKEN_A))),
            out(pay_merchant, p2pk_spk(merchant), None),
        ],
        lock_time: 0,
    };
    if payer_change {
        close_with_change(s, &payer, 5 * KAS / 100)
    } else {
        s
    }
}

/// SW2: payer holds KAS, merchant wants token B. Payer fills an Ask of B; the released B goes to the merchant.
fn sw2(f: &Fx, merchant: &[u8; 32]) -> Scn {
    let t = &f.t;
    let a = cov(0xa1);
    let price = 25 * KAS / 10;
    let ask = ask_art_c(t, TOKEN_B, &pk(&f.maker_a), LOT, price);
    let s = Scn {
        name: "SW2 KAS->B: payer buys 4 lots from 1 ask of B, tokens delivered to merchant".into(),
        inputs: vec![
            ask_in(&ask, carrier(), a, 4, 1, 2),
            tok_in(
                t,
                carrier(),
                10 * LOT,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_B,
                Some(vec![tok_state(6 * LOT, &a.as_bytes(), SCHEME_COVID, 0), tok_state(4 * LOT, merchant, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
            p2pk_in(&f.taker, 100 * KAS),
        ],
        outputs: vec![
            out(4 * price, p2pk_spk(&pk(&f.maker_a)), None),
            out(carrier(), spk_of(&ask), Some((0, a))),
            out(carrier(), t.spk(6 * LOT, &a.as_bytes(), SCHEME_COVID), Some((1, TOKEN_B))),
            out(carrier(), t.spk(4 * LOT, merchant, SCHEME_P2PK), Some((1, TOKEN_B))),
        ],
        lock_time: 0,
    };
    close_with_change(s, &pk(&f.taker), 5 * KAS / 100)
}

/// SW3: payer holds A, merchant wants B. One tx: sell 4 lots of A into a Bid(A), buy 3 lots from an Ask(B),
/// B goes to the merchant. Two token families, each with its own leader. No payer KAS input.
fn sw3(f: &Fx, merchant: &[u8; 32]) -> Scn {
    let t = &f.t;
    let (b, a) = (cov(0xb1), cov(0xa1));
    let (pa, pb) = (15 * KAS, 5 * KAS);
    let reserve = KAS;
    let bid = bid_art_c(t, TOKEN_A, &pk(&f.maker_b), LOT, pa, reserve);
    let ask = ask_art_c(t, TOKEN_B, &pk(&f.maker_a), LOT, pb);
    let payer = pk(&f.taker);
    let bid_value = 10 * pa + reserve + 3 * carrier();
    let s = Scn {
        name: "SW3 A->KAS->B: bid(A) x1 + ask(B) x1, merchant receives B".into(),
        inputs: vec![
            bid_in(&bid, bid_value, b, 4, 2),
            ask_in(&ask, carrier(), a, 3, 3, 4),
            tok_in(
                t,
                carrier(),
                7 * LOT,
                &payer,
                SCHEME_P2PK,
                TOKEN_A,
                Some(vec![tok_state(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK, 0), tok_state(3 * LOT, &payer, SCHEME_P2PK, 0)]),
                Wit::P2pk(f.taker),
            ),
            tok_in(
                t,
                carrier(),
                10 * LOT,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_B,
                Some(vec![tok_state(7 * LOT, &a.as_bytes(), SCHEME_COVID, 0), tok_state(3 * LOT, merchant, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
        ],
        outputs: vec![
            out(carrier(), t.spk(4 * LOT, &pk(&f.maker_b), SCHEME_P2PK), Some((2, TOKEN_A))), // bid delivery @0
            out(3 * pb, p2pk_spk(&pk(&f.maker_a)), None),                                     // ask payout @1
            out(bid_value - 4 * pa - carrier(), spk_of(&bid), Some((0, b))),
            out(carrier(), spk_of(&ask), Some((1, a))),
            out(carrier(), t.spk(7 * LOT, &a.as_bytes(), SCHEME_COVID), Some((3, TOKEN_B))),
            out(carrier(), t.spk(3 * LOT, merchant, SCHEME_P2PK), Some((3, TOKEN_B))),
            out(carrier(), t.spk(3 * LOT, &payer, SCHEME_P2PK), Some((2, TOKEN_A))),
        ],
        lock_time: 0,
    };
    close_with_change(s, &payer, 5 * KAS / 100)
}

/// SW4: largest A->KAS->B under default 3/3: 2 bids(A) (A family 1 in / 3 out) + 3 asks(B)
/// (B family 3 in / 2 out). 9 inputs.
fn sw4(f: &Fx, merchant: &[u8; 32]) -> Scn {
    let t = &f.t;
    let (b1, b2, a1, a2, a3) = (cov(0xb1), cov(0xb2), cov(0xa1), cov(0xa2), cov(0xa3));
    let (pa, pb) = (15 * KAS, 5 * KAS);
    let reserve = KAS;
    let (mb1, mb2, ma1, ma2, ma3) = (keypair(), keypair(), keypair(), keypair(), keypair());
    let bid1 = bid_art_c(t, TOKEN_A, &pk(&mb1), LOT, pa, reserve);
    let bid2 = bid_art_c(t, TOKEN_A, &pk(&mb2), LOT, pa, reserve);
    let ask1 = ask_art_c(t, TOKEN_B, &pk(&ma1), LOT, pb);
    let ask2 = ask_art_c(t, TOKEN_B, &pk(&ma2), LOT, pb);
    let ask3 = ask_art_c(t, TOKEN_B, &pk(&ma3), LOT, pb);
    let payer = pk(&f.taker);
    let bid1v = 10 * pa + reserve + 3 * carrier();
    let bid2v = 2 * pa + reserve + carrier();
    let c = carrier();
    let s = Scn {
        name: "SW4 A->KAS->B max: 2 bids(A) + 3 asks(B), A 1in/3out, B 3in/2out".into(),
        inputs: vec![
            bid_in(&bid1, bid1v, b1, 2, 5),
            bid_in(&bid2, bid2v, b2, 2, 5),
            ask_in(&ask1, c, a1, 2, 6, 0),
            ask_in(&ask2, c, a2, 2, 7, 0),
            ask_in(&ask3, c, a3, 1, 8, 7),
            tok_in(
                t,
                c,
                6 * LOT,
                &payer,
                SCHEME_P2PK,
                TOKEN_A,
                Some(vec![
                    tok_state(2 * LOT, &pk(&mb1), SCHEME_P2PK, 0),
                    tok_state(2 * LOT, &pk(&mb2), SCHEME_P2PK, 0),
                    tok_state(2 * LOT, &payer, SCHEME_P2PK, 0),
                ]),
                Wit::P2pk(f.taker),
            ),
            tok_in(
                t,
                c,
                2 * LOT,
                &a1.as_bytes(),
                SCHEME_COVID,
                TOKEN_B,
                Some(vec![tok_state(4 * LOT, &a3.as_bytes(), SCHEME_COVID, 0), tok_state(5 * LOT, merchant, SCHEME_P2PK, 0)]),
                Wit::CovId,
            ),
            tok_in(t, c, 2 * LOT, &a2.as_bytes(), SCHEME_COVID, TOKEN_B, None, Wit::CovId),
            tok_in(t, c, 5 * LOT, &a3.as_bytes(), SCHEME_COVID, TOKEN_B, None, Wit::CovId),
        ],
        outputs: vec![
            out(c, t.spk(2 * LOT, &pk(&mb1), SCHEME_P2PK), Some((5, TOKEN_A))), // @0 bid1 delivery
            out(reserve + c, t.spk(2 * LOT, &pk(&mb2), SCHEME_P2PK), Some((5, TOKEN_A))), // @1 bid2 delivery (exhausted)
            out(2 * pb + 2 * c, p2pk_spk(&pk(&ma1)), None),                     // @2 ask1 payout (full)
            out(2 * pb + 2 * c, p2pk_spk(&pk(&ma2)), None),                     // @3 ask2 payout (full)
            out(pb, p2pk_spk(&pk(&ma3)), None),                                 // @4 ask3 payout (partial)
            out(bid1v - 2 * pa - c, spk_of(&bid1), Some((0, b1))),              // @5 bid1 continuation
            out(c, spk_of(&ask3), Some((4, a3))),                               // @6 ask3 continuation
            out(c, t.spk(4 * LOT, &a3.as_bytes(), SCHEME_COVID), Some((6, TOKEN_B))), // @7 ask3 token change
            out(c, t.spk(5 * LOT, merchant, SCHEME_P2PK), Some((6, TOKEN_B))),  // @8 merchant B
            out(c, t.spk(2 * LOT, &payer, SCHEME_P2PK), Some((5, TOKEN_A))),    // @9 payer A change
        ],
        lock_time: 0,
    };
    close_with_change(s, &payer, 10 * KAS / 100)
}

fn kob1_payload(kind: u8, args: &[&[u8]]) -> Vec<u8> {
    let mut p = b"KOB1".to_vec();
    p.push(kind);
    for a in args {
        p.extend_from_slice(a);
    }
    p
}

/// OC1: maker creates an ask in ONE tx: ask genesis output + transfer of maker tokens to owner = new ask covenant id.
fn oc1(f: &Fx) -> (Scn, Vec<u8>) {
    let t = &f.t;
    let m = pk(&f.maker_a);
    let ask = ask_art_c(t, TOKEN_A, &m, LOT, 25 * KAS / 10);
    let order_out = out(carrier(), spk_of(&ask), None);
    // input 1 (maker's P2PK KAS UTXO) authorizes the genesis; outpoint must match build_p's synthetic outpoints
    let op1 = TransactionOutpoint { transaction_id: TransactionId::from_bytes([2u8; 32]), index: 1 };
    let ask_cov = kaspa_consensus_core::hashing::covenant_id::covenant_id(op1, [(0u32, &order_out)].into_iter());
    let mut order_out = order_out;
    order_out.covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: ask_cov });
    let s = Scn {
        name: "OC1 ask creation: genesis + token move to covenant-id owner in one tx (+KOB1 payload)".into(),
        inputs: vec![
            tok_in(
                t,
                carrier(),
                12 * LOT,
                &m,
                SCHEME_P2PK,
                TOKEN_A,
                Some(vec![tok_state(10 * LOT, &ask_cov.as_bytes(), SCHEME_COVID, 0), tok_state(2 * LOT, &m, SCHEME_P2PK, 0)]),
                Wit::P2pk(f.maker_a),
            ),
            p2pk_in(&f.maker_a, 100 * KAS),
        ],
        outputs: vec![
            order_out,
            out(carrier(), t.spk(10 * LOT, &ask_cov.as_bytes(), SCHEME_COVID), Some((0, TOKEN_A))),
            out(carrier(), t.spk(2 * LOT, &m, SCHEME_P2PK), Some((0, TOKEN_A))),
        ],
        lock_time: 0,
    };
    let payload = kob1_payload(
        0x01,
        &[
            &m,
            &TOKEN_A.as_bytes(),
            &t.hash,
            &(t.prefix.len() as i64).to_le_bytes(),
            &(t.suffix.len() as i64).to_le_bytes(),
            &LOT.to_le_bytes(),
            &(25 * KAS / 10).to_le_bytes(),
            &EXPIRY_DAA.to_le_bytes(),
            &(KAS / 10).to_le_bytes(),
        ],
    );
    (close_with_change(s, &m, KAS / 100), payload)
}

/// OC2: bid creation (P2PK funds -> bid genesis output).
fn oc2(f: &Fx) -> (Scn, Vec<u8>) {
    let t = &f.t;
    let m = pk(&f.maker_b);
    let price = 24 * KAS / 10;
    let bid = bid_art_c(t, TOKEN_A, &m, LOT, price, KAS);
    let bid_value = 10 * price + KAS + 3 * carrier();
    let order_out = out(bid_value, spk_of(&bid), None);
    let op0 = TransactionOutpoint { transaction_id: TransactionId::from_bytes([1u8; 32]), index: 0 };
    let bid_cov = kaspa_consensus_core::hashing::covenant_id::covenant_id(op0, [(0u32, &order_out)].into_iter());
    let mut order_out = order_out;
    order_out.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: bid_cov });
    let s = Scn {
        name: "OC2 bid creation: genesis (+KOB1 payload)".into(),
        inputs: vec![p2pk_in(&f.maker_b, 200 * KAS)],
        outputs: vec![order_out],
        lock_time: 0,
    };
    let payload = kob1_payload(
        0x02,
        &[
            &m,
            &TOKEN_A.as_bytes(),
            &t.hash,
            &(t.prefix.len() as i64).to_le_bytes(),
            &(t.suffix.len() as i64).to_le_bytes(),
            &EXT,
            &LOT.to_le_bytes(),
            &price.to_le_bytes(),
            &KAS.to_le_bytes(),
            &carrier().to_le_bytes(),
            &EXPIRY_DAA.to_le_bytes(),
            &(KAS / 10).to_le_bytes(),
        ],
    );
    (close_with_change(s, &m, KAS / 100), payload)
}

/// CR1: cancel-and-replace an ask in one tx: maker cancel (SIGHASH_ALL) + new ask genesis at a new price;
/// tokens move from old-covenant-id ownership to the new covenant id.
fn cr1(f: &Fx) -> (Scn, Vec<u8>) {
    let t = &f.t;
    let m = pk(&f.maker_a);
    let old = cov(0xa1);
    let ask_old = ask_art_c(t, TOKEN_A, &m, LOT, 25 * KAS / 10);
    let ask_new = ask_art_c(t, TOKEN_A, &m, LOT, 24 * KAS / 10);
    let order_out = out(carrier(), spk_of(&ask_new), None);
    let op0 = TransactionOutpoint { transaction_id: TransactionId::from_bytes([1u8; 32]), index: 0 };
    let new_cov = kaspa_consensus_core::hashing::covenant_id::covenant_id(op0, [(0u32, &order_out)].into_iter());
    let mut order_out = order_out;
    order_out.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: new_cov });
    let s = Scn {
        name: "CR1 cancel-replace ask (new price) in one tx".into(),
        inputs: vec![
            Inp {
                entry: cov_utxo(carrier(), spk_of(&ask_old), old),
                role: Role::Cancel { art: ask_old.clone(), kp: f.maker_a, sighash: 0x01 },
            },
            tok_in(
                t,
                carrier(),
                10 * LOT,
                &old.as_bytes(),
                SCHEME_COVID,
                TOKEN_A,
                Some(vec![tok_state(10 * LOT, &new_cov.as_bytes(), SCHEME_COVID, 0)]),
                Wit::CovId,
            ),
        ],
        outputs: vec![order_out, out(carrier() - KAS / 50, t.spk(10 * LOT, &new_cov.as_bytes(), SCHEME_COVID), Some((1, TOKEN_A)))],
        lock_time: 0,
    };
    (s, kob1_payload(0x01, &[&[0u8; 144]]))
}

#[test]
fn gap_swap_and_pay() {
    let f = fx();
    let merchant = pk(&keypair());
    let m1 = run_ok_p(&f.t, &sw1(&f, &merchant, 9 * KAS, true), &[]);
    let m1b = run_ok_p(&f.t, &sw1(&f, &merchant, 96 * KAS / 10 - 5 * KAS / 100, false), &[]);
    let m2 = run_ok_p(&f.t, &sw2(&f, &merchant), &[]);
    let m3 = run_ok_p(&f.t, &sw3(&f, &merchant), &[]);
    let m4 = run_ok_p(&f.t, &sw4(&f, &merchant), &[]);
    // x402 requestHash binding payload (X402:<64 hex>) on the heaviest shape
    let x402 = [b"X402:".to_vec(), vec![b'a'; 64]].concat();
    let m4p = run_ok_p(&f.t, &sw4(&f, &merchant), &x402);
    for (n, m) in [("SW1", &m1), ("SW1b", &m1b), ("SW2", &m2), ("SW3", &m3), ("SW4", &m4), ("SW4+x402 payload", &m4p)] {
        println!(
            "SUMMARY {n}: size {} B, compute {}, storage {}, min fee {:.5} KAS",
            m.size,
            m.compute,
            m.storage,
            m.fee_sompi as f64 / 1e8
        );
    }
}

#[test]
fn gap_swap_resign_required() {
    // Facilitator swaps the named ask for another one after the payer signed: the payer's token-A leader
    // signature (SIGHASH_ALL) no longer verifies -> any rebuild needs the payer to re-sign.
    let f = fx();
    let merchant = pk(&keypair());
    let s = sw3(&f, &merchant);
    let (mut tx, entries) = build(&f.t, &s, &[]);
    tx.inputs[1].previous_outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0x99; 32]), index: 7 };
    let res = execute_limited(&tx, &entries, false).expect("ctx");
    assert!(res[2].is_err(), "payer signature survived an input substitution");
    println!(
        "NEGATIVE SWR1 facilitator substitutes ask outpoint after signing -> payer token leader in[2] rejects: {:?}",
        res[2].as_ref().unwrap_err()
    );
    let (mut tx, entries) = build(&f.t, &s, &[]);
    tx.outputs[5].script_public_key = f.t.spk(3 * LOT, &pk(&keypair()), SCHEME_P2PK);
    let res = execute_limited(&tx, &entries, false).expect("ctx");
    println!(
        "NEGATIVE SWR2 merchant B output redirected after signing -> failing inputs: {:?}",
        res.iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect::<Vec<_>>()
    );
}

#[test]
fn gap_order_lifecycle() {
    let f = fx();
    let (s, p) = oc1(&f);
    run_ok_p(&f.t, &s, &p);
    let (s, p) = oc2(&f);
    run_ok_p(&f.t, &s, &p);
    let (s, p) = cr1(&f);
    run_ok_p(&f.t, &s, &p);
    let (mut s, p) = oc1(&f);
    s.outputs[0].value += 1; // changes the genesis hash but not the claimed id
    let (tx, entries) = build_p(&f.t, &s, &[], &p);
    match execute_limited(&tx, &entries, false) {
        Err(e) => println!("NEGATIVE OC-N1 genesis id mismatch rejected at tx level: {e}"),
        Ok(_) => panic!("wrong genesis covenant id accepted"),
    }
}

fn engine_flags_gap() -> EngineFlags {
    EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() }
}
