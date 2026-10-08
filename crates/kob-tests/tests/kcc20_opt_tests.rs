//! KOB optional batch leader for the KCC-20 reference holder program (`contracts/kcc20/p2/KCC20Opt.sil`, generated
//! by `kob_protocol::kcc20::kcc20_opt_holder_source`): the 3/3 reference program whose delegator path accepts as
//! leader its own template or, only when the commitment `B_TPL` / `B_PRE` / `B_SUF` is set (all zeros disables it), the batch-leader template
//! that `B_TPL` / `B_PRE` / `B_SUF` commit to (here `contracts/kcc20/p2/KCC20Batch.sil`, unchanged). Executed with KOB
//! v2 orders in rusty-kaspa v2.1.0's TxScriptEngine with KIP-20 covenant context and script-unit metering, and compared
//! with the reference 3/3, the 8/8 and 16/16 slot-limit variants and proposal P2 (`KCC20P2.sil`).
//!
//! The KCC-20 conformance vectors run against `KCC20Opt` (disabled and enabled) in `kcc20_conformance_tests.rs`.
//! Run: cargo test --release -p kob-tests --test kcc20_opt_tests -- --nocapture --test-threads=1
#![allow(dead_code, clippy::needless_range_loop)]

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
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32, OpTrue};
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::{ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use common::{bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const EXT: [u8; 32] = [0xee; 32];
const SCHEME_P2PK: u8 = 0x00;
const SCHEME_COVID: u8 = 0x04;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;
const CARRIER: i64 = 10 * KAS;
const DC: i64 = 10 * KAS;
/// Base units per whole token (the price denominator of the orders): a 3-decimal token.
const SCALE: i64 = 1_000;
/// One whole token in base units.
const WHOLE: i64 = SCALE;
/// minFill of the orders (one whole token).
const MIN_FILL: i64 = WHOLE;
const TIP: i64 = 100_000;
const EXPIRY: i64 = 400_000_000;
const REFUND_TIP: i64 = 3_000_000;
const NOW: u64 = 1_000_000;
const NET_FEE: i64 = KAS / 10;
const P250: i64 = 250_000_000;
const P260: i64 = 260_000_000;
const ZERO: [u8; 32] = [0u8; 32];
const BLOCK_TRANSIENT: u64 = 1_000_000;
const BLOCK_COMPUTE: u64 = 500_000;

fn src(name: &str) -> String {
    common::contract_source(name)
}
/// A KOB order template as a batch-leader deployment would compile it: the stray scan covers the leader
/// plus 16 holders (the committed templates scan 8 token inputs, for KCC-20 8/8 tokens).
fn order_src(name: &str) -> String {
    let s = src(name);
    assert!(s.contains("int constant MAX_TOK_IN = 8;"), "{name}: stray-scan bound not found");
    s.replace("int constant MAX_TOK_IN = 8;", "int constant MAX_TOK_IN = 17;")
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
fn cov(b: u8) -> Hash {
    Hash::from_bytes([b; 32])
}
fn bytes(v: &[u8]) -> ArtifactValue {
    v.to_vec().into()
}
fn int(v: i64) -> ArtifactValue {
    ArtifactValue::Int(v)
}
fn ask_all_in(n: i64, p: i64) -> i64 {
    // ceil(n * (p - tip) / scale): what the ask maker receives (KobAsk.quoteOf, c = scale - 1)
    kob_protocol::state::quote_of(n, p - TIP, SCALE, kob_protocol::state::Round::Up).expect("quote")
}
fn bid_all_in(n: i64, p: i64) -> i64 {
    // floor(n * (p + tip) / scale): the most the bid maker pays (KobBid.quoteOf, c = 0)
    kob_protocol::state::quote_of(n, p + TIP, SCALE, kob_protocol::state::Round::Down).expect("quote")
}

// ---------------------------------------------------------------- token programs

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
fn tok_state(amount: i64, owner: &[u8; 32], scheme: u8) -> ArtifactValue {
    BTreeMap::from([
        ("amount".to_string(), int(amount)),
        ("owner".to_string(), bytes(owner)),
        ("owner_scheme".to_string(), ArtifactValue::Byte(scheme)),
        ("borrow_scheme".to_string(), ArtifactValue::Byte(0)),
        ("borrow_guard".to_string(), bytes(&[0u8; 32])),
        ("extension_commitment".to_string(), bytes(&EXT)),
    ])
    .into()
}

/// A KCC-20 holder program: the reference 3/3, a slot-limit variant, the P2 holder or the optional-leader holder
/// (`KCC20Opt`, disabled or enabled). Any of them can be led by a `KCC20Batch` leader whose state names this holder
/// template; whether its holders accept that leader is what the tests check.
struct Token {
    name: String,
    art: SilAbiArtifact,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    hash: Vec<u8>,
    /// batch-leader template committed by the holder program (hash, prefix len, suffix len): P2 and enabled KCC20Opt
    batch: Option<(Vec<u8>, i64, i64)>,
}
fn ref_args() -> Vec<ArtifactValue> {
    vec![int(1000), bytes(&[3u8; 32]), ArtifactValue::Byte(SCHEME_COVID), ArtifactValue::Byte(0), bytes(&[0u8; 32]), bytes(&EXT)]
}
/// Other state values (another token, another owner, borrowing enabled, another fungibility class).
fn other_args() -> Vec<ArtifactValue> {
    vec![int(5), bytes(&[9u8; 32]), ArtifactValue::Byte(SCHEME_P2PK), ArtifactValue::Byte(1), bytes(&[0x42; 32]), bytes(&[0x11; 32])]
}
/// The `KCC20Batch` template (hash, prefix len, suffix len), from a leader with arbitrary state.
fn batch_template() -> (Vec<u8>, i64, i64) {
    let b = batch_art_raw(0, &[1u8; 32], &[2u8; 32], &[2u8; 32], 1, 1);
    let (bp, bs, bh) = compiled_template_parts_and_hash(&b);
    (bh, bp.len() as i64, bs.len() as i64)
}
/// `KCC20Opt` constructor constants: the batch-leader commitment, all zeros when disabled.
fn opt_consts(enabled: bool) -> Vec<ArtifactValue> {
    if enabled {
        let (bh, bp, bs) = batch_template();
        vec![bytes(&bh), int(bp), int(bs)]
    } else {
        vec![bytes(&ZERO), int(0), int(0)]
    }
}
fn compile_opt(enabled: bool, state: Vec<ArtifactValue>) -> SilAbiArtifact {
    let mut a = opt_consts(enabled);
    a.extend(state);
    compile_contract(&src("KCC20Opt"), &a, CompileOptions::default()).expect("compile KCC20Opt")
}
impl Token {
    fn from_art(name: &str, art: SilAbiArtifact, batch: Option<(Vec<u8>, i64, i64)>) -> Self {
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        let t = Token { name: name.into(), art, prefix, suffix, hash, batch };
        assert_eq!(t.redeem(1000, &[3u8; 32], SCHEME_COVID, 0), bytecode(&t.art), "{name}: manual state encoding");
        t
    }
    fn plain(name: &str) -> Self {
        let art = compile_contract(&src(name), &ref_args(), CompileOptions::default()).expect("compile token");
        Self::from_art(name, art, None)
    }
    fn p2() -> Self {
        let (bh, bp, bs) = batch_template();
        let mut a = vec![bytes(&bh), int(bp), int(bs)];
        a.extend(ref_args());
        let art = compile_contract(&src("KCC20P2"), &a, CompileOptions::default()).expect("compile KCC20P2");
        Self::from_art("KCC20P2", art, Some((bh, bp, bs)))
    }
    /// `KCC20Opt`: disabled (B_TPL all zeros) or enabled with the `KCC20Batch` template.
    fn opt(enabled: bool) -> Self {
        let name = if enabled { "KCC20Opt (enabled)" } else { "KCC20Opt (disabled)" };
        Self::from_art(name, compile_opt(enabled, ref_args()), if enabled { Some(batch_template()) } else { None })
    }
    fn redeem(&self, amount: i64, owner: &[u8; 32], scheme: u8, borrow: u8) -> Vec<u8> {
        [self.prefix.clone(), tok_state_bytes(amount, owner, scheme, borrow), self.suffix.clone()].concat()
    }
    fn spk(&self, amount: i64, owner: &[u8; 32], scheme: u8) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(amount, owner, scheme, 0))
    }
    /// A batch leader of this token (amount, owner key; ZERO = public seed) serving this holder template.
    fn leader(&self, amount: i64, owner: &[u8; 32]) -> SilAbiArtifact {
        batch_art_raw(amount, owner, &EXT, &self.hash, self.prefix.len() as i64, self.suffix.len() as i64)
    }
}
fn batch_art_raw(amount: i64, owner: &[u8; 32], ext: &[u8; 32], h_tpl: &[u8], h_pre: i64, h_suf: i64) -> SilAbiArtifact {
    compile_contract(
        &src("KCC20Batch"),
        &[int(amount), bytes(owner), bytes(ext), bytes(h_tpl), int(h_pre), int(h_suf)],
        CompileOptions::default(),
    )
    .expect("compile KCC20Batch")
}

/// Everything the copied scenario infrastructure needs.
struct Net {
    t: Token,
}
impl Net {
    fn tok_args(&self) -> Vec<ArtifactValue> {
        vec![bytes(&TOKEN_COV.as_bytes()), bytes(&self.t.hash), int(self.t.prefix.len() as i64), int(self.t.suffix.len() as i64)]
    }
    fn ask(&self, maker: &[u8; 32], price: i64, amount: i64) -> SilAbiArtifact {
        let mut a = vec![bytes(maker)];
        a.extend(self.tok_args());
        a.extend([
            int(SCALE),
            int(MIN_FILL),
            int(price),
            int(TIP),
            int(0),
            int(0),
            int(EXPIRY),
            int(REFUND_TIP),
            int(0),
            int(0),
            int(0),
            int(0),
            int(0),
            int(amount),
            bytes(&EXT),
        ]);
        compile_contract(&order_src("KobAsk"), &a, CompileOptions::default()).expect("compile KobAsk")
    }
    fn bid(&self, maker: &[u8; 32], price: i64) -> SilAbiArtifact {
        let mut a = vec![bytes(maker)];
        a.extend(self.tok_args());
        a.extend([
            bytes(&EXT),
            int(SCALE),
            int(MIN_FILL),
            int(price),
            int(TIP),
            int(0),
            int(0),
            int(EXPIRY),
            int(REFUND_TIP),
            int(0),
            int(DC),
            int(0),
            int(0),
            int(0),
            int(0),
            int(0),
        ]);
        compile_contract(&order_src("KobBid"), &a, CompileOptions::default()).expect("compile KobBid")
    }
}

fn spk_of(a: &SilAbiArtifact) -> ScriptPublicKey {
    pay_to_script_hash_script(&bytecode(a))
}

// ---------------------------------------------------------------- scenario model

#[derive(Clone)]
enum Arg {
    V(ArtifactValue),
    Sig(Keypair, u8),
}
fn nb(n: i64) -> Arg {
    Arg::V(bytes(&n.to_le_bytes()))
}
fn iv(i: i64) -> Arg {
    Arg::V(int(i))
}
#[derive(Clone)]
enum Wit {
    CovId,
    P2pk(Keypair),
}
#[derive(Clone)]
enum Role {
    Call {
        art: SilAbiArtifact,
        entry: &'static str,
        args: Vec<Arg>,
        name: &'static str,
    },
    TokLeader {
        redeem: Vec<u8>,
        next: Vec<ArtifactValue>,
        wit: Wit,
    },
    TokDelegate {
        redeem: Vec<u8>,
        wit: Wit,
    },
    P2pk {
        kp: Keypair,
    },
    /// Arbitrary signature script (e.g. empty for an OpTrue covenant UTXO).
    Raw {
        ss: Vec<u8>,
        name: &'static str,
    },
}
#[derive(Clone)]
struct Inp {
    entry: UtxoEntry,
    role: Role,
    seq: u64,
}
#[derive(Clone)]
struct Scn {
    name: String,
    inputs: Vec<Inp>,
    outputs: Vec<TransactionOutput>,
    lock_time: u64,
    payload: Vec<u8>,
}

fn utxo(value: i64, spk: ScriptPublicKey, cov: Hash, daa: u64) -> UtxoEntry {
    UtxoEntry::new(value as u64, spk, daa, false, Some(cov))
}
fn out(value: i64, spk: ScriptPublicKey, cov: Option<(u16, Hash)>) -> TransactionOutput {
    assert!(value >= 0, "negative output value {value}");
    TransactionOutput {
        value: value as u64,
        script_public_key: spk,
        covenant: cov.map(|(a, c)| CovenantBinding { authorizing_input: a, covenant_id: c }),
    }
}
#[allow(clippy::too_many_arguments)]
fn call(art: &SilAbiArtifact, entry: &'static str, args: Vec<Arg>, name: &'static str, value: i64, c: Hash, daa: u64) -> Inp {
    Inp { entry: utxo(value, spk_of(art), c, daa), role: Role::Call { art: art.clone(), entry, args, name }, seq: 0 }
}
#[allow(clippy::too_many_arguments)]
fn tok_in(
    t: &Token,
    value: i64,
    amount: i64,
    owner: &[u8; 32],
    scheme: u8,
    c: Hash,
    leader: Option<Vec<ArtifactValue>>,
    wit: Wit,
    daa: u64,
) -> Inp {
    let redeem = t.redeem(amount, owner, scheme, 0);
    let entry = utxo(value, pay_to_script_hash_script(&redeem), c, daa);
    let role = match leader {
        Some(next) => Role::TokLeader { redeem, next, wit },
        None => Role::TokDelegate { redeem, wit },
    };
    Inp { entry, role, seq: 0 }
}
fn p2pk_in(kp: &Keypair, value: i64) -> Inp {
    Inp { entry: UtxoEntry::new(value as u64, p2pk_spk(&pk(kp)), 0, false, None), role: Role::P2pk { kp: *kp }, seq: 0 }
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

fn build(net: &Net, s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = s.inputs.iter().map(|i| i.entry.clone()).collect();
    let inputs: Vec<TransactionInput> = s
        .inputs
        .iter()
        .enumerate()
        .map(|(k, inp)| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([k as u8 + 1; 32]), index: k as u32 },
                vec![],
                inp.seq,
                budgets.get(k).copied().unwrap_or(0),
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, s.outputs.clone(), s.lock_time, Default::default(), 0, s.payload.clone());
    let unsigned = tx.clone();
    for (k, inp) in s.inputs.iter().enumerate() {
        let sig_of = |kp: &Keypair, ht: u8| sign(&unsigned, &entries, k, kp, ht);
        let ss = match &inp.role {
            Role::Call { art, entry, args, .. } => {
                let vals: Vec<ArtifactValue> = args
                    .iter()
                    .map(|a| match a {
                        Arg::V(v) => v.clone(),
                        Arg::Sig(kp, ht) => sig_of(kp, *ht).into(),
                    })
                    .collect();
                entry_ss(art, entry, &vals)
            }
            Role::TokLeader { redeem, next, wit } => {
                let w = match wit {
                    Wit::CovId => vec![0x00],
                    Wit::P2pk(kp) => [vec![0x00], sig_of(kp, 0x01)].concat(),
                };
                let mut s = encode_entry_sig_script(&net.t.art, "transfer", &[ArtifactValue::Array(next.clone()), w.into()]).unwrap();
                s.extend_from_slice(&push(redeem));
                s
            }
            Role::TokDelegate { redeem, wit } => {
                let w = match wit {
                    Wit::CovId => vec![],
                    Wit::P2pk(kp) => sig_of(kp, 0x01),
                };
                let mut s = encode_entry_sig_script(&net.t.art, "transfer_delegator", &[w.into()]).unwrap();
                s.extend_from_slice(&push(redeem));
                s
            }
            Role::P2pk { kp } => push(&sig_of(kp, 0x01)),
            Role::Raw { ss, .. } => ss.clone(),
        };
        tx.inputs[k].signature_script = ss;
    }
    (tx, entries)
}

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

fn role_name(r: &Role) -> &'static str {
    match r {
        Role::Call { name, .. } => name,
        Role::TokLeader { .. } => "kcc20.transfer",
        Role::TokDelegate { .. } => "kcc20.delegator",
        Role::P2pk { .. } => "p2pk",
        Role::Raw { name, .. } => name,
    }
}

/// Positive scenario: every input must pass. Prints per-input units and tx size / mass / fee.
fn run_ok(net: &Net, s: &Scn) -> u64 {
    let (tx, entries) = build(net, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => panic!("{}: input {i} ({}) failed: {e:?}", s.name, role_name(&s.inputs[i].role)),
        }
    }
    let (tx, entries) = build(net, s, &budgets);
    let res = execute(&tx, &entries).expect("ctx");
    assert!(res.iter().all(|r| r.is_ok()), "{}: second pass failed", s.name);
    let mc = MassCalculator::new(1, 10, 1_000_000_000_000);
    let nc = mc.calc_non_contextual_masses(&tx);
    let populated = PopulatedTransaction::new(&tx, entries.clone());
    let storage = mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX);
    let size = transaction_estimated_serialized_size(&tx);
    let fee_mass = nc.compute_mass.max(2 * size);
    println!("SCENARIO {}  [PASS]", s.name);
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
        "  inputs={} outputs={} payload={} B  tx_size={} B  compute_mass={}  storage_mass={}  => min_fee={} sompi ({:.5} KAS)",
        tx.inputs.len(),
        tx.outputs.len(),
        tx.payload.len(),
        size,
        nc.compute_mass,
        storage,
        fee_mass * MIN_FEE_PER_GRAM,
        (fee_mass * MIN_FEE_PER_GRAM) as f64 / 1e8
    );
    fee_mass * MIN_FEE_PER_GRAM
}

/// Negative scenario: input `expect` must fail.
fn run_bad(net: &Net, s: &Scn, expect: usize) {
    let (tx, entries) = build(net, s, &[]);
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
/// Negative scenario rejected before script execution (covenant context).
fn run_bad_ctx(net: &Net, s: &Scn) {
    let (tx, entries) = build(net, s, &[]);
    match execute(&tx, &entries) {
        Err(e) => println!("NEGATIVE {}  [REJECTED by covenant context: {e}]", s.name),
        Ok(res) => {
            panic!("{}: expected covenant-context rejection, got {:?}", s.name, res.iter().map(|r| r.is_ok()).collect::<Vec<_>>())
        }
    }
}

fn with_name(mut s: Scn, name: &str) -> Scn {
    s.name = name.into();
    s
}
fn set_arg(s: &mut Scn, idx: usize, pos: usize, a: Arg) {
    if let Role::Call { args, .. } = &mut s.inputs[idx].role {
        args[pos] = a;
    }
}
/// Moves the UTXO of input idx to DAA score daa (its age / idle clock).
fn set_daa(s: &mut Scn, idx: usize, daa: u64) {
    let e = &s.inputs[idx].entry;
    s.inputs[idx].entry = UtxoEntry::new(e.amount, e.script_public_key.clone(), daa, false, e.covenant_id);
}
/// Expiry scenarios: order UTXO recent enough that the 90-day idle bound is later than expiry.
fn fresh(mut s: Scn) -> Scn {
    set_daa(&mut s, 0, (EXPIRY - 1_000) as u64);
    s
}
fn set_leader_next(s: &mut Scn, idx: usize, new_next: Vec<ArtifactValue>) {
    if let Role::TokLeader { next, .. } = &mut s.inputs[idx].role {
        *next = new_next;
    }
}

// ---------------------------------------------------------------- scenarios

/// Measures a passing scenario: (tx bytes, compute mass, storage mass, min fee).
fn measure(net: &Net, s: &Scn) -> (u64, u64, u64, u64) {
    let (tx, entries) = build(net, s, &[]);
    let res = execute(&tx, &entries).expect("ctx");
    let budgets: Vec<u16> =
        res.iter().map(|r| r.as_ref().map(|u| u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16).unwrap_or(0)).collect();
    let (tx, entries) = build(net, s, &budgets);
    let mc = MassCalculator::new(1, 10, 1_000_000_000_000);
    let nc = mc.calc_non_contextual_masses(&tx);
    let populated = PopulatedTransaction::new(&tx, entries);
    let storage = mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX);
    let size = transaction_estimated_serialized_size(&tx);
    (size, nc.compute_mass, storage, nc.compute_mass.max(2 * size) * MIN_FEE_PER_GRAM)
}
/// Runs a positive scenario and prints its block fit (transient = 4 x bytes of 1,000,000;
/// compute of 500,000; the binding one decides how many such transactions fit a block).
fn run_fit(net: &Net, s: &Scn) {
    run_ok(net, s);
    let (size, compute, storage, fee) = measure(net, s);
    let t = 4 * size;
    let per_block = (BLOCK_TRANSIENT / t).min(BLOCK_COMPUTE / compute.max(1));
    println!(
        "  FIT {}: {size} B, compute {compute}, storage {storage}, fee {:.5} KAS, transient {:.1}% / compute {:.1}% of a block, {per_block} per block",
        s.name,
        fee as f64 / 1e8,
        100.0 * t as f64 / BLOCK_TRANSIENT as f64,
        100.0 * compute as f64 / BLOCK_COMPUTE as f64
    );
}

fn leader_in(t: &Token, owner: &Keypair, entry: &'static str, args: Vec<Arg>, amount: i64) -> Inp {
    call(&t.leader(amount, &pk(owner)), entry, args, "leader", CARRIER, TOKEN_COV, 500)
}
fn leader_out(t: &Token, owner: &[u8; 32], auth: u16) -> TransactionOutput {
    out(CARRIER, spk_of(&t.leader(0, owner)), Some((auth, TOKEN_COV)))
}
fn outs_arg(states: &[Vec<u8>]) -> Arg {
    Arg::V(bytes(&states.concat()))
}

/// 16-ask sweep: a taker buys 31 whole tokens from 16 asks (15 full at 2 tokens, the last partial 1/2).
/// `with_leader`: through a batch leader held by `matcher` (P2 or enabled KCC20Opt token, or any token whose lineage holds one); else a plain 16-slot
/// holder leader (16/16 variant).
fn sweep16(net: &Net, matcher: &Keypair, taker: &Keypair, with_leader: bool) -> Scn {
    sweep(net, matcher, taker, with_leader, 16)
}
/// k-ask sweep (k-1 full at 2 whole tokens, the last partial 1/2).
fn sweep(net: &Net, matcher: &Keypair, taker: &Keypair, with_leader: bool, k: usize) -> Scn {
    let t = &net.t;
    let tk = pk(taker);
    let makers: Vec<Keypair> = (0..k).map(|_| keypair()).collect();
    let covs: Vec<Hash> = (0..k).map(|i| cov(0xa0 + i as u8)).collect();
    let hbase = k + if with_leader { 1 } else { 0 };
    let last = k - 1;
    let bought = (2 * k as i64 - 1) * WHOLE;
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut pays = 0;
    for i in 0..k {
        let ask = net.ask(&pk(&makers[i]), P250, 2 * WHOLE);
        let n = if i == last { WHOLE } else { 2 * WHOLE };
        inputs.push(call(
            &ask,
            "settle",
            vec![nb(n), iv((hbase + i) as i64), iv(if i == last { k as i64 + 1 } else { 0 }), iv(0)],
            "ask.settle",
            CARRIER,
            covs[i],
            1_000,
        ));
        let pay = ask_all_in(n, P250);
        pays += pay;
        outputs.push(out(if i == last { pay } else { pay + 2 * CARRIER }, p2pk_spk(&pk(&makers[i])), None));
    }
    let ask_last = net.ask(&pk(&makers[last]), P250, WHOLE);
    outputs.push(out(CARRIER, spk_of(&ask_last), Some((last as u16, covs[last]))));
    let rem = tok_state_bytes(WHOLE, &covs[last].as_bytes(), SCHEME_COVID, 0);
    let got = tok_state_bytes(bought, &tk, SCHEME_P2PK, 0);
    let auth = if with_leader { k as u16 } else { hbase as u16 };
    outputs.push(out(CARRIER, t.spk(WHOLE, &covs[last].as_bytes(), SCHEME_COVID), Some((auth, TOKEN_COV))));
    outputs.push(out(CARRIER, t.spk(bought, &tk, SCHEME_P2PK), Some((auth, TOKEN_COV))));
    if with_leader {
        inputs.push(leader_in(t, matcher, "batch", vec![outs_arg(&[rem, got]), iv(2), Arg::Sig(*matcher, 0x01)], 0));
        outputs.push(leader_out(t, &pk(matcher), k as u16));
    }
    for i in 0..k {
        let leader = if !with_leader && i == 0 {
            Some(vec![tok_state(WHOLE, &covs[last].as_bytes(), SCHEME_COVID), tok_state(bought, &tk, SCHEME_P2PK)])
        } else {
            None
        };
        inputs.push(tok_in(t, CARRIER, 2 * WHOLE, &covs[i].as_bytes(), SCHEME_COVID, TOKEN_COV, leader, Wit::CovId, 1_000));
    }
    inputs.push(p2pk_in(taker, 1000 * KAS));
    outputs.push(out(1000 * KAS - pays - CARRIER - NET_FEE, p2pk_spk(&tk), None));
    Scn {
        name: format!("{k}-ask sweep via {}", if with_leader { "batch leader".to_string() } else { t.name.clone() }),
        inputs,
        outputs,
        lock_time: NOW,
        payload: vec![],
    }
}

/// 16-order N:M cross: 8 bids (2.60, 2 whole tokens each) x 8 asks (2.50, 2 whole tokens each) through a leader.
fn cross8x8(net: &Net, matcher: &Keypair) -> Scn {
    let t = &net.t;
    let bmakers: Vec<Keypair> = (0..8).map(|_| keypair()).collect();
    let amakers: Vec<Keypair> = (0..8).map(|_| keypair()).collect();
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut states = vec![];
    let mut income = 0;
    // the budget 2 whole tokens consume: ceil(2 * WHOLE * (P260 + TIP) / SCALE) (KobBid used(n))
    let used = kob_protocol::state::quote_of(2 * WHOLE, P260 + TIP, SCALE, kob_protocol::state::Round::Up).expect("quote");
    for i in 0..8 {
        let bid = net.bid(&pk(&bmakers[i]), P260);
        let v = used + DC;
        inputs.push(call(&bid, "fill", vec![nb(2 * WHOLE), iv(17), iv(0)], "bid.fill", v, cov(0xb0 + i as u8), 1_000));
        outputs.push(out(v - bid_all_in(2 * WHOLE, P260), t.spk(2 * WHOLE, &pk(&bmakers[i]), SCHEME_P2PK), Some((16, TOKEN_COV))));
        states.push(tok_state_bytes(2 * WHOLE, &pk(&bmakers[i]), SCHEME_P2PK, 0));
        income += bid_all_in(2 * WHOLE, P260);
    }
    for i in 0..8 {
        let ask = net.ask(&pk(&amakers[i]), P250, 2 * WHOLE);
        inputs.push(call(
            &ask,
            "settle",
            vec![nb(2 * WHOLE), iv(17 + i as i64), iv(0), iv(0)],
            "ask.settle",
            CARRIER,
            cov(0xa0 + i as u8),
            1_000,
        ));
        outputs.push(out(ask_all_in(2 * WHOLE, P250) + 2 * CARRIER, p2pk_spk(&pk(&amakers[i])), None));
        income -= ask_all_in(2 * WHOLE, P250);
    }
    inputs.push(leader_in(t, matcher, "batch", vec![outs_arg(&states), iv(8), Arg::Sig(*matcher, 0x01)], 0));
    outputs.push(leader_out(t, &pk(matcher), 16));
    for i in 0..8 {
        inputs.push(tok_in(t, CARRIER, 2 * WHOLE, &cov(0xa0 + i as u8).as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000));
    }
    inputs.push(p2pk_in(matcher, 10 * KAS));
    outputs.push(out(10 * KAS + income - NET_FEE, p2pk_spk(&pk(matcher)), None));
    Scn { name: "16-order cross: 8 bids x 8 asks via batch leader".into(), inputs, outputs, lock_time: NOW, payload: vec![] }
}

/// k P2PK holders (owner `owner`, 1 whole token each) merged into one output for `to`, led by a batch
/// leader held by `matcher` (input 0).
fn leader_merge(net: &Net, matcher: &Keypair, owner: &Keypair, signer: &Keypair, k: usize, to: &[u8; 32]) -> Scn {
    let t = &net.t;
    let st = tok_state_bytes(k as i64 * WHOLE, to, SCHEME_P2PK, 0);
    let mut inputs = vec![leader_in(t, matcher, "batch", vec![outs_arg(&[st]), iv(0), Arg::Sig(*matcher, 0x01)], 0)];
    for _ in 0..k {
        inputs.push(tok_in(t, CARRIER, WHOLE, &pk(owner), SCHEME_P2PK, TOKEN_COV, None, Wit::P2pk(*signer), 1_000));
    }
    Scn {
        name: format!("leader merges {k} holders"),
        inputs,
        outputs: vec![
            leader_out(t, &pk(matcher), 0),
            out(k as i64 * CARRIER - NET_FEE, t.spk(k as i64 * WHOLE, to, SCHEME_P2PK), Some((0, TOKEN_COV))),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// Plain holder transfer of the token program: `k` inputs (owner A, leader first) -> 1 output.
fn holder_transfer(net: &Net, a: &Keypair, b: &[u8; 32], k: usize) -> Scn {
    let t = &net.t;
    let mut inputs = vec![];
    for i in 0..k {
        let leader = if i == 0 { Some(vec![tok_state(k as i64 * WHOLE, b, SCHEME_P2PK)]) } else { None };
        inputs.push(tok_in(t, CARRIER, WHOLE, &pk(a), SCHEME_P2PK, TOKEN_COV, leader, Wit::P2pk(*a), 1_000));
    }
    Scn {
        name: format!("{} holder transfer {k}->1 ({})", t.name, if k == 1 { "payment" } else { "merge" }),
        inputs,
        outputs: vec![out(k as i64 * CARRIER - NET_FEE, t.spk(k as i64 * WHOLE, b, SCHEME_P2PK), Some((0, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
    }
}

/// Clone: the leader at input 0 (owner `src_owner`) makes a new leader for `new_owner`.
fn clone_scn(net: &Net, src_owner: &[u8; 32], new_owner: &[u8; 32], w: Arg, funder: &Keypair) -> Scn {
    let t = &net.t;
    let src = t.leader(0, src_owner);
    Scn {
        name: "clone leader".into(),
        inputs: vec![
            call(&src, "clone", vec![Arg::V(bytes(new_owner)), w], "leader.clone", CARRIER, TOKEN_COV, 500),
            p2pk_in(funder, 20 * KAS),
        ],
        outputs: vec![
            out(CARRIER, spk_of(&src), Some((0, TOKEN_COV))),
            out(CARRIER, spk_of(&t.leader(0, new_owner)), Some((0, TOKEN_COV))),
            out(10 * KAS - NET_FEE, p2pk_spk(&pk(funder)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

// ---------------------------------------------------------------- tests

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn tpl_hash(a: &SilAbiArtifact) -> Vec<u8> {
    compiled_template_parts_and_hash(a).2
}
/// Fee of a passing scenario (P2SH sizes as in `run_fit`: max(compute mass, 2 x bytes) x 100 sompi per gram).
fn fee_of(net: &Net, s: &Scn) -> u64 {
    run_fit(net, s);
    measure(net, s).3
}
fn kas(f: Option<u64>) -> String {
    f.map(|f| format!("{:.5}", f as f64 / 1e8)).unwrap_or_else(|| "n/a".into())
}

/// Template sharing. A holder template is the program without its state (KOB template hash: prefix and suffix around
/// the 112-byte state). The reference has no constructor constants, so every reference token has one template;
/// `KCC20Opt` adds three constants, so it has one template per commitment: one for every token with the batch leader
/// disabled and one for every token that enables the universal `KCC20Batch` leader (whose own template does not depend
/// on the token: the holder template it serves is in its state).
#[test]
fn opt_template_sharing() {
    let ref_a = compile_contract(&src("KCC20Ref"), &ref_args(), CompileOptions::default()).unwrap();
    let ref_b = compile_contract(&src("KCC20Ref"), &other_args(), CompileOptions::default()).unwrap();
    assert_eq!(tpl_hash(&ref_a), tpl_hash(&ref_b), "reference: one template for every token");
    assert_ne!(bytecode(&ref_a), bytecode(&ref_b));

    let off_a = compile_opt(false, ref_args());
    let off_b = compile_opt(false, other_args());
    assert_eq!(tpl_hash(&off_a), tpl_hash(&off_b), "disabled: one template for every token without a leader");
    assert_ne!(tpl_hash(&off_a), tpl_hash(&ref_a), "the disabled program is not the reference template");

    let on_a = compile_opt(true, ref_args());
    let on_b = compile_opt(true, other_args());
    assert_eq!(tpl_hash(&on_a), tpl_hash(&on_b), "enabled: one template for every token using the universal leader");
    assert_ne!(tpl_hash(&on_a), tpl_hash(&off_a));

    // the leader serves any holder template: its template is the same for different tokens, owners, fungibility
    // classes and holder templates (all state)
    let lead_a = batch_art_raw(0, &ZERO, &EXT, &tpl_hash(&on_a), 1, 3000);
    let lead_b = batch_art_raw(0, &[7u8; 32], &[0x11; 32], &tpl_hash(&ref_a), 1, 2977);
    assert_eq!(tpl_hash(&lead_a), tpl_hash(&lead_b), "one batch-leader template for every token");
    assert_eq!(tpl_hash(&lead_a), batch_template().0);

    // the same-template check of the delegator assumes a one-byte prefix and the 112-byte state right after it
    for (name, a) in [("disabled", &off_a), ("enabled", &on_a)] {
        let (p, s, h) = compiled_template_parts_and_hash(a);
        assert_eq!(p.len(), 1, "{name}: prefix length");
        assert_eq!(p.len() + 112 + s.len(), bytecode(a).len());
        println!(
            "TEMPLATE KCC20Opt {name}: prefix {} B, suffix {} B, program {} B, hash {}",
            p.len(),
            s.len(),
            bytecode(a).len(),
            hex(&h)
        );
    }
    let p2 = Token::p2();
    let (bh, bp, bs) = batch_template();
    println!("TEMPLATE KCC20Ref (3/3): program {} B, hash {}", bytecode(&ref_a).len(), hex(&tpl_hash(&ref_a)));
    println!("TEMPLATE KCC20P2: program {} B, hash {}", bytecode(&p2.art).len(), hex(&p2.hash));
    println!("TEMPLATE KCC20Batch (universal leader): prefix {bp} B, suffix {bs} B, hash {}", hex(&bh));
}

/// Holder-led transfers behave alike in both modes; with B_TPL all zeros no other leader is accepted, not even the
/// batch leader whose state names this holder template.
#[test]
fn opt_disabled_refuses_every_other_leader() {
    let net = Net { t: Token::opt(false) };
    let t = &net.t;
    let m = keypair();
    let a = keypair();
    let taker = keypair();
    run_fit(&net, &holder_transfer(&net, &a, &pk(&a), 1));
    run_fit(&net, &holder_transfer(&net, &a, &pk(&a), 3));
    run_bad(&net, &with_name(holder_transfer(&net, &a, &pk(&a), 4), "D0 holder leader with 4 inputs (3/3 program)"), 0);

    // the batch leader (seeded in the lineage) is refused by every holder it would lead
    let s = with_name(leader_merge(&net, &m, &a, &a, 3, &pk(&a)), "D1 batch leader merges 3 holders of a disabled token");
    run_bad(&net, &s, 1);
    run_bad(&net, &s, 2);
    run_bad(&net, &s, 3);
    run_bad(&net, &with_name(sweep16(&net, &m, &taker, true), "D2 16-ask sweep via batch leader, disabled token"), 17);
    run_bad(&net, &with_name(sweep(&net, &m, &taker, true, 8), "D3 8-ask sweep via batch leader, disabled token"), 9);
    // the P2 holder's leader allowlist would accept it: the zero commitment is what refuses it
    let p2 = Net { t: Token::p2() };
    run_fit(&p2, &with_name(leader_merge(&p2, &m, &a, &a, 3, &pk(&a)), "D1p the same merge on the P2 holder"));

    // foreign leaders: an OP_TRUE covenant UTXO (short signature script) and a long foreign P2SH script
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "D4 OP_TRUE leader writes 1,000,000 whole tokens");
    s.inputs[0] = Inp {
        entry: utxo(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), TOKEN_COV, 500),
        role: Role::Raw { ss: vec![], name: "foreign.leader" },
        seq: 0,
    };
    s.outputs[0] = out(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), Some((0, TOKEN_COV)));
    s.outputs[1].script_public_key = t.spk(1_000_000 * WHOLE, &pk(&a), SCHEME_P2PK);
    run_bad(&net, &s, 1);
    run_bad(&net, &with_name(long_foreign_leader(&net, &m, &a), "D5 long foreign P2SH leader writes 1,000,000 whole tokens"), 1);
}

/// A foreign leader whose P2SH signature script is longer than every template the holder could compare it with.
fn long_foreign_leader(net: &Net, m: &Keypair, a: &Keypair) -> Scn {
    let mut junk = vec![];
    for _ in 0..13 {
        junk.extend_from_slice(&[0x4d, 0xf4, 0x01]);
        junk.extend_from_slice(&[0xab; 500]);
        junk.push(0x75);
    }
    junk.push(OpTrue);
    let mut s = leader_merge(net, m, a, a, 2, &pk(a));
    s.inputs[0] = Inp {
        entry: utxo(CARRIER, pay_to_script_hash_script(&junk), TOKEN_COV, 500),
        role: Role::Raw { ss: push(&junk), name: "foreign.leader" },
        seq: 0,
    };
    s.outputs[0] = out(CARRIER, pay_to_script_hash_script(&junk), Some((0, TOKEN_COV)));
    s.outputs[1].script_public_key = net.t.spk(1_000_000 * WHOLE, &pk(a), SCHEME_P2PK);
    s
}

#[test]
fn opt_enabled_leader_lifecycle_and_batches() {
    let net = Net { t: Token::opt(true) };
    let m = keypair();
    let m2 = keypair();
    let taker = keypair();
    let a = keypair();
    run_fit(&net, &holder_transfer(&net, &a, &pk(&a), 1));
    run_fit(&net, &holder_transfer(&net, &a, &pk(&a), 3));
    run_fit(&net, &with_name(clone_scn(&net, &ZERO, &pk(&m), Arg::V(bytes(&[])), &m), "E1 a matcher clones the public seed"));
    run_fit(&net, &with_name(clone_scn(&net, &pk(&m), &pk(&m2), Arg::Sig(m, 0x01), &m), "E2 a matcher clones its own leader"));
    let s = Scn {
        name: "E3 a matcher retires a leader".into(),
        inputs: vec![leader_in(&net.t, &m, "retire", vec![Arg::Sig(m, 0x01)], 0)],
        outputs: vec![out(CARRIER - NET_FEE, p2pk_spk(&pk(&m)), None)],
        lock_time: NOW,
        payload: vec![],
    };
    run_fit(&net, &s);
    run_fit(
        &net,
        &with_name(leader_merge(&net, &m, &m2, &m2, 3, &pk(&m2)), "E4 batch merges 3 P2PK holders (owner-signed); leader continues"),
    );
    run_fit(&net, &with_name(leader_merge(&net, &m, &m2, &m2, 16, &pk(&m2)), "E5 batch merges 16 P2PK holders (owner-signed)"));
    run_fit(&net, &sweep16(&net, &m, &taker, true));
    run_fit(&net, &cross8x8(&net, &m));
    run_fit(&net, &sweep(&net, &m, &taker, true, 8));
}

#[test]
fn opt_enabled_negative() {
    let net = Net { t: Token::opt(true) };
    let t = &net.t;
    let m = keypair();
    let victim = keypair();
    let other = keypair();
    let taker = keypair();
    let a = keypair();

    // conservation
    let mut s = with_name(sweep16(&net, &m, &taker, true), "N1 leader writes +1 whole token to the taker");
    let rem = tok_state_bytes(WHOLE, &cov(0xaf).as_bytes(), SCHEME_COVID, 0);
    let got = tok_state_bytes(32 * WHOLE, &pk(&taker), SCHEME_P2PK, 0);
    set_arg(&mut s, 16, 0, outs_arg(&[rem, got]));
    s.outputs[18].script_public_key = t.spk(32 * WHOLE, &pk(&taker), SCHEME_P2PK);
    run_bad(&net, &s, 16);
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 3, &pk(&a)), "N1b leader writes 1 base unit less than it consumes");
    let st = tok_state_bytes(3 * WHOLE - 1, &pk(&a), SCHEME_P2PK, 0);
    set_arg(&mut s, 0, 0, outs_arg(&[st]));
    s.outputs[1].script_public_key = t.spk(3 * WHOLE - 1, &pk(&a), SCHEME_P2PK);
    run_bad(&net, &s, 0);

    // holders still authorize themselves
    run_bad(
        &net,
        &with_name(leader_merge(&net, &m, &victim, &m, 2, &pk(&other)), "N2 P2PK holder signed by the matcher, not its owner"),
        1,
    );
    let mut s = with_name(leader_merge(&net, &m, &victim, &victim, 2, &pk(&other)), "N2b order-owned holder without its order input");
    s.inputs[1] = tok_in(t, CARRIER, WHOLE, &cov(0xa7).as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000);
    run_bad(&net, &s, 1);

    // only the committed leader template: foreign scripts and another leader program are refused
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N3 OP_TRUE leader writes 1,000,000 whole tokens");
    s.inputs[0] = Inp {
        entry: utxo(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), TOKEN_COV, 500),
        role: Role::Raw { ss: vec![], name: "foreign.leader" },
        seq: 0,
    };
    s.outputs[0] = out(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), Some((0, TOKEN_COV)));
    s.outputs[1].script_public_key = t.spk(1_000_000 * WHOLE, &pk(&a), SCHEME_P2PK);
    run_bad(&net, &s, 1);
    run_bad(&net, &with_name(long_foreign_leader(&net, &m, &a), "N3b long foreign P2SH leader writes 1,000,000 whole tokens"), 1);
    let other_src = src("KCC20Batch").replace("int constant MAX_SLOTS = 16;", "int constant MAX_SLOTS = 15;");
    assert_ne!(other_src, src("KCC20Batch"));
    let other_lead = |owner: &[u8; 32]| {
        compile_contract(
            &other_src,
            &[int(0), bytes(owner), bytes(&EXT), bytes(&t.hash), int(t.prefix.len() as i64), int(t.suffix.len() as i64)],
            CompileOptions::default(),
        )
        .expect("compile other leader")
    };
    assert_ne!(tpl_hash(&other_lead(&pk(&m))), t.batch.as_ref().unwrap().0);
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N3c another batch-leader template (15 slots) leads holders");
    s.inputs[0] = call(
        &other_lead(&pk(&m)),
        "batch",
        vec![outs_arg(&[tok_state_bytes(2 * WHOLE, &pk(&a), SCHEME_P2PK, 0)]), iv(0), Arg::Sig(m, 0x01)],
        "other.leader",
        CARRIER,
        TOKEN_COV,
        500,
    );
    s.outputs[0] = out(CARRIER, spk_of(&other_lead(&pk(&m))), Some((0, TOKEN_COV)));
    run_bad(&net, &s, 1);

    // no leader can appear later: a holder leader cannot write a leader (enabling is decided by the lineage's genesis)
    let mut s = with_name(holder_transfer(&net, &a, &pk(&a), 1), "N4 holder leader writes a batch-leader seed");
    s.outputs.push(out(CARRIER, spk_of(&t.leader(0, &ZERO)), Some((0, TOKEN_COV))));
    run_bad(&net, &s, 0);

    // leader rules
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N5 leader UTXO with amount 1 cannot batch");
    s.inputs[0] =
        leader_in(t, &m, "batch", vec![outs_arg(&[tok_state_bytes(2 * WHOLE, &pk(&a), SCHEME_P2PK, 0)]), iv(0), Arg::Sig(m, 0x01)], 1);
    s.outputs[0] = out(CARRIER, spk_of(&t.leader(1, &pk(&m))), Some((0, TOKEN_COV)));
    run_bad(&net, &s, 0);
    run_bad(&net, &with_name(leader_merge(&net, &m, &a, &a, 17, &pk(&a)), "N6 batch leader with 17 holder inputs"), 0);
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N7 batch signed by a key other than the leader's owner");
    set_arg(&mut s, 0, 2, Arg::Sig(other, 0x01));
    run_bad(&net, &s, 0);
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N8 continuation hands the leader to another key");
    s.outputs[0].script_public_key = spk_of(&t.leader(0, &pk(&other)));
    run_bad(&net, &s, 0);
    let s = Scn {
        name: "N9 batch leader placed after a holder (not cov[0])".into(),
        inputs: vec![
            tok_in(
                t,
                CARRIER,
                WHOLE,
                &pk(&a),
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(WHOLE, &pk(&a), SCHEME_P2PK)]),
                Wit::P2pk(a),
                1_000,
            ),
            leader_in(t, &m, "batch", vec![outs_arg(&[tok_state_bytes(WHOLE, &pk(&a), SCHEME_P2PK, 0)]), iv(0), Arg::Sig(m, 0x01)], 0),
        ],
        outputs: vec![out(CARRIER, t.spk(WHOLE, &pk(&a), SCHEME_P2PK), Some((0, TOKEN_COV))), leader_out(t, &pk(&m), 1)],
        lock_time: NOW,
        payload: vec![],
    };
    run_bad(&net, &s, 1);

    // order custody and output rules
    let mut s = with_name(sweep16(&net, &m, &taker, true), "N10 sweep: one ask paid 1 sompi short");
    s.outputs[3].value -= 1;
    run_bad(&net, &s, 3);
    let mut bad = tok_state_bytes(2 * WHOLE, &pk(&a), 0x05, 0);
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N11 output owner scheme 0x05");
    set_arg(&mut s, 0, 0, outs_arg(&[bad.clone()]));
    s.outputs[1].script_public_key = pay_to_script_hash_script(&[t.prefix.clone(), bad.clone(), t.suffix.clone()].concat());
    run_bad(&net, &s, 0);
    bad = tok_state_bytes(2 * WHOLE, &pk(&a), SCHEME_P2PK, 0);
    let n = bad.len();
    bad[n - 1] ^= 0x01;
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "N11b output of another fungibility class");
    set_arg(&mut s, 0, 0, outs_arg(&[bad.clone()]));
    s.outputs[1].script_public_key = pay_to_script_hash_script(&[t.prefix.clone(), bad, t.suffix.clone()].concat());
    run_bad(&net, &s, 0);
}

/// The reference holder program does not check its leader: its delegator path validates only its own owner (KCC-20
/// section 2, "a delegator validates only its local owner"), and its read of the leader state is unused. Any program
/// that the lineage's genesis put in the covenant family can lead reference holders, including the batch leader (no
/// holder change at all) and a script that does not validate the transition.
#[test]
fn reference_delegator_accepts_any_lineage_leader() {
    let net = Net { t: Token::plain("KCC20Ref") };
    let m = keypair();
    let a = keypair();
    let taker = keypair();
    run_fit(&net, &with_name(sweep16(&net, &m, &taker, true), "R1 16-ask sweep via batch leader, unchanged reference holders"));
    run_fit(&net, &with_name(cross8x8(&net, &m), "R2 16-order cross via batch leader, unchanged reference holders"));
    let s = with_name(
        long_foreign_leader(&net, &m, &a),
        "R3 long foreign P2SH leader writes 1,000,000 whole tokens for reference holders",
    );
    let (tx, entries) = build(&net, &s, &[]);
    let res = execute(&tx, &entries).expect("ctx");
    assert!(res.iter().all(|r| r.is_ok()), "{}: {:?}", s.name, res);
    println!("ACCEPTED {} (every input passes: the reference delegator does not check the leader's template)", s.name);
    let mut s = with_name(leader_merge(&net, &m, &a, &a, 2, &pk(&a)), "R4 OP_TRUE leader with an empty signature script");
    s.inputs[0] = Inp {
        entry: utxo(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), TOKEN_COV, 500),
        role: Role::Raw { ss: vec![], name: "foreign.leader" },
        seq: 0,
    };
    s.outputs[0] = out(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), Some((0, TOKEN_COV)));
    let (tx, entries) = build(&net, &s, &[]);
    let res = execute(&tx, &entries).expect("ctx");
    println!(
        "RESULT {}: {:?}",
        s.name,
        res.iter().map(|r| r.as_ref().map(|_| "ok").map_err(|e| format!("{e:?}"))).collect::<Vec<_>>()
    );
}

/// Sizes and fees, P2SH-standard: holder program size, 1->1 payment and 3->1 merge, and the 8- and 16-order batches,
/// for the reference 3/3, 8/8 and 16/16 programs, P2, KCC20Opt disabled and enabled, and the unchanged reference with a
/// batch leader in its lineage.
#[test]
fn opt_measurements() {
    let m = keypair();
    let taker = keypair();
    let a = keypair();
    let to = pk(&keypair());
    let lead = Token::opt(true).leader(0, &[5u8; 32]);
    println!("SIZE KCC20Batch leader: {} B", bytecode(&lead).len());
    // (label, token, batches through: Some(true) batch leader, Some(false) holder leader, None: no 8/16-order batch)
    let rows: Vec<(&str, Token, Option<bool>)> = vec![
        ("KCC20Ref 3/3", Token::plain("KCC20Ref"), None),
        ("KCC20Ref_8x8", Token::plain("KCC20Ref_8x8"), Some(false)),
        ("KCC20Ref_16x16", Token::plain("KCC20Ref_16x16"), Some(false)),
        ("KCC20P2", Token::p2(), Some(true)),
        ("KCC20Opt disabled", Token::opt(false), None),
        ("KCC20Opt enabled", Token::opt(true), Some(true)),
        ("KCC20Ref 3/3 + lineage leader", Token::plain("KCC20Ref"), Some(true)),
    ];
    let mut out_rows = vec![];
    for (label, t, via) in rows {
        let net = Net { t };
        let size = bytecode(&net.t.art).len();
        let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
            &push(&bytecode(&net.t.art)),
            &pay_to_script_hash_script(&bytecode(&net.t.art)),
        );
        assert!(so <= 15);
        let f1 = fee_of(&net, &holder_transfer(&net, &a, &to, 1));
        let f3 = fee_of(&net, &holder_transfer(&net, &a, &to, 3));
        let (f8, f16, fx) = match via {
            None => (None, None, None),
            Some(true) => (
                Some(fee_of(&net, &sweep(&net, &m, &taker, true, 8))),
                Some(fee_of(&net, &sweep16(&net, &m, &taker, true))),
                Some(fee_of(&net, &cross8x8(&net, &m))),
            ),
            Some(false) => {
                let f8 = fee_of(&net, &sweep(&net, &m, &taker, false, 8));
                let f16 = if label.contains("16x16") { Some(fee_of(&net, &sweep16(&net, &m, &taker, false))) } else { None };
                (Some(f8), f16, None)
            }
        };
        out_rows.push(format!(
            "ROW {label:<30} | holder {size:>5} B | sig-op bound {so} | 1->1 {} | 3->1 {} | 8-ask sweep {} | 16-ask sweep {} | 8x8 cross {} KAS",
            kas(Some(f1)),
            kas(Some(f3)),
            kas(f8),
            kas(f16),
            kas(fx)
        ));
    }
    for r in out_rows {
        println!("{r}");
    }
}

#[test]
fn opt_holder_source_matches_generator() {
    let base = common::contract_source("KCC20Ref");
    let generated = kob_protocol::kcc20::kcc20_opt_holder_source(&base).expect("generate");
    assert_eq!(common::contract_source("KCC20Opt").replace("\r\n", "\n"), generated, "KCC20Opt.sil differs from the generator");
    // the committed artifact is the disabled instance
    let ctor: Vec<ArtifactValue> =
        serde_json::from_str(&std::fs::read_to_string(common::repo_root().join("contracts/kcc20/p2/KCC20Opt.ctor.json")).unwrap())
            .unwrap();
    assert_eq!(
        serde_json::to_value(&ctor[..3]).unwrap(),
        serde_json::to_value(opt_consts(false)).unwrap(),
        "KCC20Opt.ctor.json: B_TPL / B_PRE / B_SUF all zeros"
    );
}
