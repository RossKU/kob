//! Whole-transaction tests of KobCurveAsk (docs/design/curve-ask-spec.md) through the script
//! engine.
//!
//! Covered: fills inside one level and across up to 8, the derived level, per-level rounding,
//! minFill, a matcher cross with a KobBid, the cancel, the creation inside the token's genesis
//! (unbound) and every attack the contract refuses.
//!
//! The token program is `KOB_KCC20_PROGRAM` (default the reference 3 / 3; `KCC20PublicMint`,
//! `KaspaRocketToken11458`, `KForgeToken7214`). The stray scan per slot runs on `KCC20Ref_8x8`.
//!
//! Output lines follow the ablation protocol (tools/ablation, suite `curve-ask`): `NEGATIVE <id>
//! ... @<program>` for a refused attack, under `KOB_ABLATION` `ABLATION-PASS <id> ...
//! all_inputs_ok=<bool>` when it is accepted and `ABLATION-POS-FAIL <id> ...` when a positive
//! scenario is refused.
//!
//! Run: cargo test -p kob-tests --test kob_curve_ask_tests -- --nocapture --test-threads=1
#![allow(dead_code)]

#[path = "../common/mod.rs"]
mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32, OpDrop, OpTrue};
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use kob_protocol::artifacts::TemplateId;
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::{ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use common::{
    bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script, state_layout,
};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const OTHER_COV: Hash = Hash::from_bytes([0x71; 32]);
const CURVE_COV: Hash = Hash::from_bytes([0xc1; 32]);
const EXT: [u8; 32] = [0xee; 32];
const SCHEME_P2PK: u8 = 0x00;
const SCHEME_COVID: u8 = 0x04;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const CARRIER: i64 = KAS;
const DC: i64 = KAS;
const SCALE: i64 = 1_000;
const TOK: i64 = SCALE;
const TIP: i64 = 100_000;
const EXPIRY: i64 = 400_000_000;
const REFUND_TIP: i64 = 3_000_000;
const NOW: u64 = 1_000_000;
const NET_FEE: i64 = KAS / 10;
const P250: i64 = 250_000_000;
/// Redeem-script window of amountLeft and of tokenCovId (spec section 1).
const LEFT_AT: usize = 173;
const COV_AT: usize = 35;

fn ablation() -> bool {
    std::env::var_os("KOB_ABLATION").is_some()
}
fn keypair() -> Keypair {
    let secp = Secp256k1::new();
    let mut sk = [0u8; 32];
    loop {
        thread_rng().fill_bytes(&mut sk);
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
fn bytes(v: &[u8]) -> ArtifactValue {
    v.to_vec().into()
}
fn int(v: i64) -> ArtifactValue {
    ArtifactValue::Int(v)
}
fn push(data: &[u8]) -> Vec<u8> {
    push_redeem_script(data)
}
/// KobAsk's quoteOf (c = scale - 1 rounds up, 0 down); None where the covenant overflows.
fn quote_at(n: i64, r: i64, scale: i64, up: bool) -> Option<i64> {
    let c = if up { scale - 1 } else { 0 };
    let (q, m) = (n / scale, n % scale);
    q.checked_mul(r)?.checked_add(m.checked_mul(r / scale)?)?.checked_add(m.checked_mul(r % scale)?.checked_add(c)? / scale)
}

// ---------------------------------------------------------------- grids

/// HIP-2: px_0 = start, px_i = round(px_{i-1} * 1.003).
fn hip2_grid(start: i64, levels: usize) -> Vec<i64> {
    let mut v = vec![start];
    while v.len() < levels {
        let p = *v.last().unwrap();
        v.push((p * 1003 + 500) / 1000);
    }
    v
}
fn arith_grid(start: i64, step: i64, levels: usize) -> Vec<i64> {
    (0..levels as i64).map(|i| start + i * step).collect()
}

// ---------------------------------------------------------------- the curve and its off-chain mirror

#[derive(Clone)]
struct CurveP {
    maker: [u8; 32],
    token_cov: [u8; 32],
    scale: i64,
    min_fill: i64,
    tip: i64,
    active_from: i64,
    level_size: i64,
    total: i64,
    amount_left: i64,
    table: Vec<i64>,
}
impl CurveP {
    fn new(maker: [u8; 32], table: Vec<i64>, level_size: i64, total: i64) -> Self {
        CurveP {
            maker,
            token_cov: TOKEN_COV.as_bytes(),
            scale: SCALE,
            min_fill: 1,
            tip: 0,
            active_from: 0,
            level_size,
            total,
            amount_left: total,
            table,
        }
    }
    fn left(&self, amount_left: i64) -> Self {
        CurveP { amount_left, ..self.clone() }
    }
    fn unbound(&self) -> Self {
        CurveP { token_cov: [0; 32], ..self.clone() }
    }
}

/// The covenant's walk: the proceeds of n base units, or None where it refuses (more than 8 levels, past the table, a
/// level price below the tip).
fn curve_cost(p: &CurveP, n: i64) -> Option<i64> {
    let (mut pos, end) = (p.total - p.amount_left, p.total - p.amount_left + n);
    let (mut cost, mut crossed) = (0, 0);
    while pos < end {
        if crossed == 8 {
            return None;
        }
        let lvl = pos / p.level_size;
        let upto = ((lvl + 1) * p.level_size).min(end);
        let px = *p.table.get(usize::try_from(lvl).ok()?)?;
        if px < p.tip {
            return None;
        }
        cost += quote_at(upto - pos, px - p.tip, p.scale, true)?;
        pos = upto;
        crossed += 1;
    }
    Some(cost)
}
/// The same walk with no cross cap, bounded by the table.
fn curve_cost_uncapped(p: &CurveP, n: i64) -> i64 {
    let (mut pos, end, mut c) = (p.total - p.amount_left, p.total - p.amount_left + n, 0);
    while pos < end {
        let lvl = pos / p.level_size;
        let upto = ((lvl + 1) * p.level_size).min(end);
        c += quote_at(upto - pos, p.table[lvl as usize] - p.tip, p.scale, true).unwrap();
        pos = upto;
    }
    c
}
fn levels_touched(p: &CurveP, n: i64) -> i64 {
    let s = p.total - p.amount_left;
    (s + n - 1) / p.level_size - s / p.level_size + 1
}
/// The most base units a KAS budget buys in one fill.
fn max_n_for_budget(p: &CurveP, budget: i64) -> i64 {
    let (mut lo, mut hi) = (0, p.amount_left);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        match curve_cost(p, mid) {
            Some(c) if c <= budget => lo = mid,
            _ => hi = mid - 1,
        }
    }
    lo
}

// ---------------------------------------------------------------- token states and programs

#[derive(Clone, Copy, Debug, PartialEq)]
struct St {
    amount: i64,
    owner: [u8; 32],
    scheme: u8,
    borrow: u8,
    ext: [u8; 32],
}
fn plain(amount: i64, owner: &[u8; 32]) -> St {
    St { amount, owner: *owner, scheme: SCHEME_P2PK, borrow: 0, ext: EXT }
}
fn custody(amount: i64, owner: Hash) -> St {
    St { amount, owner: owner.as_bytes(), scheme: SCHEME_COVID, borrow: 0, ext: EXT }
}
impl St {
    fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0x08];
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.push(0x20);
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.scheme, 0x01, self.borrow, 0x20]);
        v.extend_from_slice(&[0u8; 32]);
        v.push(0x20);
        v.extend_from_slice(&self.ext);
        v
    }
    fn val(&self) -> ArtifactValue {
        BTreeMap::from([
            ("amount".to_string(), int(self.amount)),
            ("owner".to_string(), bytes(&self.owner)),
            ("owner_scheme".to_string(), ArtifactValue::Byte(self.scheme)),
            ("borrow_scheme".to_string(), ArtifactValue::Byte(self.borrow)),
            ("borrow_guard".to_string(), bytes(&[0u8; 32])),
            ("extension_commitment".to_string(), bytes(&self.ext)),
        ])
        .into()
    }
}

struct Token {
    name: String,
    art: SilAbiArtifact,
    contract: Option<String>,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    hash: Vec<u8>,
}
impl Token {
    /// A KCC-20 program compiled from source ("KCC20Ref", "KCC20Ref_8x8", "KCC20Ref_16x16").
    fn named(name: &str) -> Self {
        let art = compile_contract(
            &common::contract_source(name),
            &[int(1000), bytes(&[3u8; 32]), ArtifactValue::Byte(4), ArtifactValue::Byte(0), bytes(&[0u8; 32]), bytes(&EXT)],
            CompileOptions::default(),
        )
        .unwrap_or_else(|e| panic!("compile {name}: {e}"));
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        Token { name: name.into(), art, contract: None, prefix, suffix, hash }
    }
    fn pinned(id: TemplateId) -> Self {
        let t = kob_protocol::artifacts::template(id);
        assert_eq!(t.state_len, 112);
        Token {
            name: id.name().to_string(),
            art: t.artifact.clone(),
            contract: Some(t.contract_name.clone()),
            prefix: t.prefix.clone(),
            suffix: t.suffix.clone(),
            hash: t.hash.to_vec(),
        }
    }
    /// The program of `KOB_KCC20_PROGRAM` (the reference 3 / 3 from source by default).
    fn from_env() -> Self {
        match common::kcc20_33_program() {
            TemplateId::Kcc20Ref => Token::named("KCC20Ref"),
            id => Token::pinned(id),
        }
    }
    fn entry_ss(&self, entry: &str, args: &[ArtifactValue]) -> Vec<u8> {
        match &self.contract {
            None => encode_entry_sig_script(&self.art, entry, args),
            Some(c) => silverscript_abi::encode_contract_entry_sig_script(&self.art, c, entry, args),
        }
        .unwrap_or_else(|e| panic!("token {entry}: {e}"))
    }
    fn redeem(&self, s: &St) -> Vec<u8> {
        [self.prefix.clone(), s.bytes(), self.suffix.clone()].concat()
    }
    fn spk(&self, s: &St) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(s))
    }
}

/// The token, the curve source (committed or an ablation copy) and the KobBid source.
struct Net {
    t: Token,
    src: String,
}
impl Net {
    fn new() -> Self {
        Net::on(Token::from_env())
    }
    fn on(t: Token) -> Self {
        Net { t, src: common::contract_source("KobCurveAsk") }
    }
    fn args(&self, p: &CurveP) -> Vec<ArtifactValue> {
        vec![
            bytes(&p.maker),
            bytes(&p.token_cov),
            bytes(&self.t.hash),
            int(self.t.prefix.len() as i64),
            int(self.t.suffix.len() as i64),
            int(p.scale),
            int(p.min_fill),
            int(p.tip),
            int(p.active_from),
            int(p.level_size),
            int(p.total),
            int(p.amount_left),
            bytes(&EXT),
            ArtifactValue::Array(p.table.iter().map(|&x| int(x)).collect()),
        ]
    }
    fn curve(&self, p: &CurveP) -> SilAbiArtifact {
        compile_contract(&self.src, &self.args(p), CompileOptions::default()).expect("compile KobCurveAsk")
    }
    /// A mode-0 KobBid of `maker` at `price` + TIP.
    fn bid(&self, maker: [u8; 32], price: i64) -> SilAbiArtifact {
        let mut a = vec![bytes(&maker), bytes(&TOKEN_COV.as_bytes()), bytes(&self.t.hash)];
        a.extend([int(self.t.prefix.len() as i64), int(self.t.suffix.len() as i64), bytes(&EXT), int(SCALE), int(1)]);
        a.extend([int(price), int(TIP), int(0), int(0), int(EXPIRY), int(REFUND_TIP), int(0), int(DC)]);
        a.extend([int(0), int(0), int(0), int(0), int(0), int(0), int(0), int(0), int(0)]);
        compile_contract(&common::contract_source("KobBid"), &a, CompileOptions::default()).expect("compile KobBid")
    }
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
    Call {
        art: SilAbiArtifact,
        entry: &'static str,
        args: Vec<ArtifactValue>,
        name: &'static str,
    },
    /// The order's cancel: the key's signature of sighash type `ht`.
    Cancel {
        art: SilAbiArtifact,
        kp: Keypair,
        ht: u8,
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
    Raw {
        ss: Vec<u8>,
        name: &'static str,
    },
}
#[derive(Clone)]
struct Inp {
    entry: UtxoEntry,
    role: Role,
    /// the outpoint it spends (None: a distinct one per input)
    outpoint: Option<TransactionOutpoint>,
}
#[derive(Clone)]
struct Scn {
    name: String,
    inputs: Vec<Inp>,
    outputs: Vec<TransactionOutput>,
    lock_time: u64,
}
fn utxo(value: i64, spk: ScriptPublicKey, cov: Option<Hash>) -> UtxoEntry {
    UtxoEntry::new(value as u64, spk, 1_000, false, cov)
}
fn out(value: i64, spk: ScriptPublicKey, cov: Option<(u16, Hash)>) -> TransactionOutput {
    assert!(value >= 0, "negative output value {value}");
    TransactionOutput {
        value: value as u64,
        script_public_key: spk,
        covenant: cov.map(|(a, c)| CovenantBinding { authorizing_input: a, covenant_id: c }),
    }
}
fn settle_in(art: &SilAbiArtifact, n: i64, tok_in: usize, tok_out: usize, value: i64, c: Hash) -> Inp {
    settle_in_rs(art, n, tok_in, tok_out, bytecode(art).len() as i64, value, c)
}
fn settle_in_rs(art: &SilAbiArtifact, n: i64, tok_in: usize, tok_out: usize, rs_len: i64, value: i64, c: Hash) -> Inp {
    Inp {
        entry: utxo(value, spk_of(art), Some(c)),
        role: Role::Call {
            art: art.clone(),
            entry: "settle",
            args: vec![bytes(&n.to_le_bytes()), int(tok_in as i64), int(tok_out as i64), int(rs_len)],
            name: "curve.settle",
        },
        outpoint: None,
    }
}
fn tok_in(t: &Token, s: &St, leader: Option<&[St]>, wit: Wit) -> Inp {
    let redeem = t.redeem(s);
    let entry = utxo(CARRIER, pay_to_script_hash_script(&redeem), Some(TOKEN_COV));
    let role = match leader {
        Some(next) => Role::TokLeader { redeem, next: next.iter().map(St::val).collect(), wit },
        None => Role::TokDelegate { redeem, wit },
    };
    Inp { entry, role, outpoint: None }
}
fn p2pk_in(kp: &Keypair, value: i64) -> Inp {
    Inp { entry: UtxoEntry::new(value as u64, p2pk_spk(&pk(kp)), 0, false, None), role: Role::P2pk { kp: *kp }, outpoint: None }
}
fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair, ht: u8) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(ht).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).unwrap();
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(ht);
    s
}
fn build(t: &Token, s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = s.inputs.iter().map(|i| i.entry.clone()).collect();
    let inputs: Vec<TransactionInput> = (0..s.inputs.len())
        .map(|k| {
            TransactionInput::new_with_compute_budget(
                s.inputs[k].outpoint.unwrap_or(TransactionOutpoint {
                    transaction_id: TransactionId::from_bytes([(k as u8).wrapping_add(1); 32]),
                    index: k as u32,
                }),
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
            Role::Call { art, entry, args, .. } => {
                let mut ss = encode_entry_sig_script(art, entry, args).expect("encode entry");
                ss.extend_from_slice(&push(&bytecode(art)));
                ss
            }
            Role::Cancel { art, kp, ht } => {
                let mut ss = encode_entry_sig_script(art, "cancel", &[sig_of(kp, *ht).into()]).expect("encode cancel");
                ss.extend_from_slice(&push(&bytecode(art)));
                ss
            }
            Role::TokLeader { redeem, next, wit } => {
                let w = match wit {
                    Wit::CovId => vec![0x00],
                    Wit::P2pk(kp) => [vec![0x00], sig_of(kp, 0x01)].concat(),
                };
                let mut ss = t.entry_ss("transfer", &[ArtifactValue::Array(next.clone()), w.into()]);
                ss.extend_from_slice(&push(redeem));
                ss
            }
            Role::TokDelegate { redeem, wit } => {
                let w = match wit {
                    Wit::CovId => vec![],
                    Wit::P2pk(kp) => sig_of(kp, 0x01),
                };
                let mut ss = t.entry_ss("transfer_delegator", &[w.into()]);
                ss.extend_from_slice(&push(redeem));
                ss
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
        Role::Cancel { .. } => "curve.cancel",
        Role::TokLeader { .. } => "kcc20.transfer",
        Role::TokDelegate { .. } => "kcc20.delegator",
        Role::P2pk { .. } => "p2pk",
        Role::Raw { name, .. } => name,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Cost {
    size: u64,
    fee: u64,
    /// script units of input 0
    units: u64,
}
/// Every input accepts; returns the size, the relay floor (measured budgets) and input 0's units.
fn run_ok(t: &Token, s: &Scn) -> Cost {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => {
                if ablation() {
                    println!("ABLATION-POS-FAIL {} @{} (input {i}: {e:?})", s.name, t.name);
                    return Cost::default();
                }
                panic!("{} @{}: input {i} ({}) failed: {e:?}", s.name, t.name, role_name(&s.inputs[i].role))
            }
        }
    }
    let (tx, entries) = build(t, s, &budgets);
    let res = execute(&tx, &entries).expect("ctx");
    assert!(res.iter().all(|r| r.is_ok()), "{}: second pass failed", s.name);
    let (vin, vout): (u64, u64) = (entries.iter().map(|e| e.amount).sum(), tx.outputs.iter().map(|o| o.value).sum());
    assert!(vin >= vout, "{}: outputs exceed inputs", s.name);
    let m = kob_protocol::tx::masses(&tx, &entries);
    let fee = kob_protocol::tx::min_fee(&m, kob_protocol::tx::MIN_FEE_RATE);
    let units = *res[0].as_ref().unwrap();
    println!(
        "POSITIVE {} @{}  [PASS] size={} compute={} storage={} relay_fee={} sompi ({:.5} KAS) in[0] units={}",
        s.name,
        t.name,
        m.size,
        m.compute,
        m.storage,
        fee,
        fee as f64 / 1e8,
        units
    );
    Cost { size: m.size, fee, units }
}
/// Input `expect` must refuse.
fn run_bad(t: &Token, s: &Scn, expect: usize) {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: tx-level failure {e}", s.name));
    let r = &res[expect];
    if r.is_ok() && ablation() {
        println!("ABLATION-PASS {} @{} all_inputs_ok={}", s.name, t.name, res.iter().all(|r| r.is_ok()));
        return;
    }
    assert!(
        r.is_err(),
        "{} @{}: expected input {expect} ({}) to REJECT but it passed",
        s.name,
        t.name,
        role_name(&s.inputs[expect].role)
    );
    let others: Vec<String> = res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
    println!(
        "NEGATIVE {} @{}  [REJECTED at in[{expect}] {}: {:?}]{}",
        s.name,
        t.name,
        role_name(&s.inputs[expect].role),
        r.as_ref().unwrap_err(),
        if others.is_empty() { String::new() } else { format!(" (also failing: {})", others.join(",")) }
    );
}
fn named(mut s: Scn, name: &str) -> Scn {
    s.name = format!("{name} {}", s.name);
    s
}

// ---------------------------------------------------------------- scenarios

struct Fx {
    maker: Keypair,
    maker_b: Keypair,
    taker: Keypair,
}
fn fx() -> Fx {
    Fx { maker: keypair(), maker_b: keypair(), taker: keypair() }
}

/// A taker buys n base units and pays `pay` (None: exactly the covenant's proceeds). A partial fill continues the
/// curve at output 1 with its rest custody at output 2 and the taker's tokens at 3; a sell-out pays both carriers.
fn fill_with(net: &Net, f: &Fx, p: &CurveP, n: i64, pay: Option<i64>) -> Scn {
    let t = &net.t;
    let a = CURVE_COV;
    let curve = net.curve(p);
    let taker = pk(&f.taker);
    let pay = pay.unwrap_or_else(|| curve_cost(p, n).expect("fill within the cap"));
    let rest = p.amount_left - n;
    let name = format!(
        "buy {n} at sold {} of {} ({} levels, {} touched)",
        p.total - p.amount_left,
        p.total,
        p.table.len(),
        levels_touched(p, n)
    );
    let funding = 2000 * KAS;
    if rest <= 0 {
        return Scn {
            name,
            inputs: vec![
                settle_in(&curve, n, 1, 0, CARRIER, a),
                tok_in(t, &custody(p.amount_left, a), Some(&[plain(p.amount_left, &taker)]), Wit::CovId),
                p2pk_in(&f.taker, funding),
            ],
            outputs: vec![
                out(pay + 2 * CARRIER, p2pk_spk(&p.maker), None),
                out(CARRIER, t.spk(&plain(p.amount_left, &taker)), Some((1, TOKEN_COV))),
                out(funding - pay - CARRIER - NET_FEE, p2pk_spk(&taker), None),
            ],
            lock_time: NOW,
        };
    }
    let cont = CurveP { token_cov: if p.token_cov == [0; 32] { TOKEN_COV.as_bytes() } else { p.token_cov }, ..p.left(rest) };
    Scn {
        name,
        inputs: vec![
            settle_in(&curve, n, 1, 2, CARRIER, a),
            tok_in(t, &custody(p.amount_left, a), Some(&[custody(rest, a), plain(n, &taker)]), Wit::CovId),
            p2pk_in(&f.taker, funding),
        ],
        outputs: vec![
            out(pay, p2pk_spk(&p.maker), None),
            out(CARRIER, spk_of(&net.curve(&cont)), Some((0, a))),
            out(CARRIER, t.spk(&custody(rest, a)), Some((1, TOKEN_COV))),
            out(CARRIER, t.spk(&plain(n, &taker)), Some((1, TOKEN_COV))),
            out(funding - pay - CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
    }
}
fn fill(net: &Net, f: &Fx, p: &CurveP, n: i64) -> Scn {
    fill_with(net, f, p, n, None)
}
/// Moves `d` sompi from the maker's payout (output 0) to the taker's change (the last output).
fn underpay(mut s: Scn, d: i64) -> Scn {
    s.outputs[0].value -= d as u64;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += d as u64;
    s
}
/// Sets the next states of the custody leader (input 1).
fn set_next(s: &mut Scn, next: &[St]) {
    if let Role::TokLeader { next: nx, .. } = &mut s.inputs[1].role {
        *nx = next.iter().map(St::val).collect();
    } else {
        panic!("input 1 is not the token leader");
    }
}

/// A matcher crosses a mode-0 KobBid (maker B, at `bid_price` + TIP, funded for exactly n) with the curve.
fn cross(net: &Net, f: &Fx, p: &CurveP, n: i64, bid_price: i64) -> Scn {
    let t = &net.t;
    let (b, a) = (Hash::from_bytes([0xb1; 32]), CURVE_COV);
    let mb = pk(&f.maker_b);
    let bid = net.bid(mb, bid_price);
    let used = quote_at(n, bid_price + TIP, SCALE, true).unwrap();
    let spend = quote_at(n, bid_price + TIP, SCALE, false).unwrap();
    let bid_value = used + DC;
    let pay = curve_cost(p, n).expect("cross within the cap");
    let rest = p.amount_left - n;
    assert!(rest > 0 && spend >= pay + NET_FEE, "the cross must leave a rest and pay the matcher's fee");
    let curve = net.curve(p);
    Scn {
        name: format!("cross a KobBid at {bid_price} with {n} at sold {}", p.total - p.amount_left),
        inputs: vec![
            Inp {
                entry: utxo(bid_value, spk_of(&bid), Some(b)),
                role: Role::Call {
                    art: bid.clone(),
                    entry: "fill",
                    args: vec![bytes(&n.to_le_bytes()), int(2), int(0), int(0)],
                    name: "bid.fill",
                },
                outpoint: None,
            },
            settle_in(&curve, n, 2, 3, CARRIER, a),
            tok_in(t, &custody(p.amount_left, a), Some(&[plain(n, &mb), custody(rest, a)]), Wit::CovId),
        ],
        outputs: vec![
            out(bid_value - spend, t.spk(&plain(n, &mb)), Some((2, TOKEN_COV))),
            out(pay, p2pk_spk(&p.maker), None),
            out(CARRIER, spk_of(&net.curve(&p.left(rest))), Some((1, a))),
            out(CARRIER, t.spk(&custody(rest, a)), Some((2, TOKEN_COV))),
            out(spend - pay - NET_FEE, p2pk_spk(&pk(&keypair())), None),
        ],
        lock_time: NOW,
    }
}

/// The maker's cancel: the custody and both carriers back to the maker.
fn cancel(net: &Net, f: &Fx, p: &CurveP, signer: &Keypair, ht: u8) -> Scn {
    let t = &net.t;
    let a = CURVE_COV;
    let to = pk(signer);
    Scn {
        name: format!("cancel of {} left", p.amount_left),
        inputs: vec![
            Inp {
                entry: utxo(CARRIER, spk_of(&net.curve(p)), Some(a)),
                role: Role::Cancel { art: net.curve(p), kp: *signer, ht },
                outpoint: None,
            },
            tok_in(t, &custody(p.amount_left, a), Some(&[plain(p.amount_left, &to)]), Wit::CovId),
            p2pk_in(&f.maker, KAS),
        ],
        outputs: vec![
            out(CARRIER, t.spk(&plain(p.amount_left, &to)), Some((1, TOKEN_COV))),
            out(2 * KAS - NET_FEE / 10, p2pk_spk(&to), None),
        ],
        lock_time: NOW,
    }
}

/// 50 levels of 2 whole tokens (100 tokens), HIP-2 grid from 2.50 KAS.
fn hip2_50(f: &Fx) -> CurveP {
    CurveP::new(pk(&f.maker), hip2_grid(P250, 50), 2 * TOK, 100 * TOK)
}

#[test]
fn curve_layout_and_template() {
    let f = fx();
    let net = Net::new();
    let p = hip2_50(&f).left(77_777);
    let a = net.curve(&p);
    let bc = bytecode(&a);
    let l = state_layout(&a);
    assert_eq!(l.start, 1);
    assert_eq!(bc[LEFT_AT - 1], 0x08);
    assert_eq!(i64::from_le_bytes(bc[LEFT_AT..LEFT_AT + 8].try_into().unwrap()), 77_777, "amountLeft window");
    assert_eq!(&bc[COV_AT..COV_AT + 32], &TOKEN_COV.as_bytes(), "tokenCovId window");
    assert_eq!(&bc[181..214], &[[0x20u8].as_slice(), &EXT].concat()[..], "extensionCommitment");
    // the table: one PUSHDATA2 of 400 B
    assert_eq!(&bc[214..217], &[0x4d, 0x90, 0x01]);
    for (j, &px) in p.table.iter().enumerate() {
        assert_eq!(i64::from_le_bytes(bc[217 + 8 * j..225 + 8 * j].try_into().unwrap()), px, "table[{j}]");
    }
    assert_eq!(l.len, 216 + 400, "state span");
    // one template for every table length (minimal pushes: direct, PUSHDATA1, PUSHDATA2), the pinned one
    let tpl = |levels: usize| {
        let q = CurveP::new(pk(&f.maker), hip2_grid(P250, levels), TOK, levels as i64 * TOK);
        compiled_template_parts_and_hash(&net.curve(&q)).2
    };
    let pinned = kob_protocol::artifacts::template(TemplateId::KobCurveAsk).hash.to_vec();
    if net.t.name == "KCC20Ref" && !ablation() {
        for levels in [1usize, 9, 10, 31, 32, 100, 1000] {
            assert_eq!(tpl(levels), pinned, "{levels} levels: the template hash does not depend on the table");
        }
    }
    // static sig-op bound
    let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(&push(&bc), &spk_of(&a));
    assert!(so <= 15, "sig-op bound {so}");
    println!("layout ok: state {} B at 50 levels, redeem {} B, sig-op bound {so}", l.len, bc.len());
}

#[test]
fn curve_sizes_and_fees() {
    let f = fx();
    let net = Net::new();
    println!("| levels | redeem B | state B | fill in 1 level: tx B / fee KAS / units | across 8 levels: tx B / fee KAS / units | sell-out: tx B / fee KAS | cancel: tx B / fee KAS |");
    for levels in [10usize, 50, 100] {
        let p = CurveP::new(pk(&f.maker), hip2_grid(P250, levels), TOK, levels as i64 * TOK);
        let a = net.curve(&p);
        let one = run_ok(&net.t, &fill(&net, &f, &p.left(p.total - 2 * TOK), TOK / 2));
        let start = p.left(p.total - TOK / 2);
        let n = 7 * TOK + TOK / 4;
        assert_eq!(levels_touched(&start, n), 8);
        let many = run_ok(&net.t, &fill(&net, &f, &start, n));
        let close = run_ok(&net.t, &fill(&net, &f, &p.left(3 * TOK), 3 * TOK));
        let c = run_ok(&net.t, &cancel(&net, &f, &p, &f.maker, 1));
        let fmt = |c: &Cost| format!("{} / {:.5} / {}", c.size, c.fee as f64 / 1e8, c.units);
        let fmt2 = |c: &Cost| format!("{} / {:.5}", c.size, c.fee as f64 / 1e8);
        println!(
            "| {levels} | {} | {} | {} | {} | {} | {} |",
            bytecode(&a).len(),
            state_layout(&a).len,
            fmt(&one),
            fmt(&many),
            fmt2(&close),
            fmt2(&c)
        );
    }
}

#[test]
fn curve_positive_fills() {
    let f = fx();
    let net = Net::new();
    let t = &net.t;
    let p = hip2_50(&f);
    // inside one level: a whole level, a fraction (rounded up), from mid-level to its end
    run_ok(t, &named(fill(&net, &f, &p, 2 * TOK), "P1 a whole level"));
    run_ok(t, &named(fill(&net, &f, &p, 333), "P2 a fraction of a level"));
    run_ok(t, &named(fill(&net, &f, &p.left(p.total - 333), 2 * TOK - 333), "P3 mid-level to the level's end"));
    // across 2, 5 and 8 levels from mid-level
    for n in [2 * TOK + 1, 9 * TOK + 7, 14 * TOK + 766] {
        run_ok(t, &named(fill(&net, &f, &p.left(p.total - 1_234), n), "P4 across levels"));
    }
    // a partial fill, then the continuation filled at the next levels (the level follows amountLeft)
    run_ok(t, &named(fill(&net, &f, &p, 3 * TOK), "P5 first fill"));
    let s2 = fill(&net, &f, &p.left(p.total - 3 * TOK), 4 * TOK);
    assert_eq!(s2.outputs[0].value as i64, p.table[1] + p.table[2] * 2 + p.table[3], "each level at its own price");
    run_ok(t, &named(s2, "P5 continuation at the next levels"));
    // sell-out of the last 8 levels in one fill
    run_ok(t, &named(fill(&net, &f, &p.left(16 * TOK), 16 * TOK), "P6 sell-out of the last 8 levels"));
    // a partial last level
    let pl = CurveP::new(pk(&f.maker), hip2_grid(P250, 50), 2 * TOK, 98 * TOK + 500);
    let s = fill(&net, &f, &pl.left(2 * TOK + 500), 2 * TOK + 500);
    assert_eq!(
        s.outputs[0].value as i64 - 2 * CARRIER,
        quote_at(2 * TOK, pl.table[48], SCALE, true).unwrap() + quote_at(500, pl.table[49], SCALE, true).unwrap()
    );
    run_ok(t, &named(s, "P7 partial last level, sell-out"));
    // an arithmetic grid with a tip and a minimum fill; the last remainder below minFill
    let mut ar = CurveP::new(pk(&f.maker), arith_grid(100_000_000, 5_000_000, 10), TOK, 10 * TOK);
    ar.tip = TIP;
    ar.min_fill = 500;
    run_ok(t, &named(fill(&net, &f, &ar, 3 * TOK + 250), "P8 arithmetic grid with a tip"));
    run_ok(t, &named(fill(&net, &f, &ar.left(200), 200), "P9 the last 200 below minFill"));
    // market buy: the most 20 KAS buys in one fill
    let n = max_n_for_budget(&p, 20 * KAS);
    let c = curve_cost(&p, n).unwrap();
    assert!(c <= 20 * KAS && curve_cost(&p, n + 1).is_none_or(|c1| c1 > 20 * KAS));
    run_ok(t, &named(fill(&net, &f, &p, n), "P10 market buy for 20 KAS"));
    // a matcher cross with a KobBid at 2.60 (5 tokens across 3 levels)
    run_ok(t, &named(cross(&net, &f, &p.left(p.total - TOK), 5 * TOK, 260_000_000), "P11 matcher cross"));
    // the active time
    let mut timed = p.clone();
    timed.active_from = NOW as i64;
    run_ok(t, &named(fill(&net, &f, &timed, TOK), "P12 fill at activeFrom"));
    // a one-level curve and a 1,000-level curve
    let one = CurveP::new(pk(&f.maker), vec![P250], 10 * TOK, 10 * TOK);
    run_ok(t, &named(fill(&net, &f, &one, 4 * TOK), "P13 one level"));
    let big = CurveP::new(pk(&f.maker), hip2_grid(P250, 1000), TOK, 1000 * TOK);
    run_ok(t, &named(fill(&net, &f, &big.left(500 * TOK + 1), 7 * TOK), "P14 1,000 levels across 8"));
    // the maker's cancel
    run_ok(t, &named(cancel(&net, &f, &p.left(40 * TOK), &f.maker, 1), "P15 maker cancel"));
}

/// The fill-side attacks (ids are the ablation catalog's).
#[test]
fn curve_negatives() {
    let f = fx();
    let net = Net::new();
    let t = &net.t;
    let p = hip2_50(&f);
    let q = p.left(p.total - 1_234);
    let taker = pk(&f.taker);
    let a = CURVE_COV;

    // NC1 underpay by 1 sompi: in one level, across levels, a fraction (the per-level ceil); NC2 sell-out; NC3 cross
    run_bad(t, &named(underpay(fill(&net, &f, &p, 333), 1), "NC1a underpaid fraction"), 0);
    run_bad(t, &named(underpay(fill(&net, &f, &q, 9 * TOK + 7), 1), "NC1b underpaid across 5 levels"), 0);
    run_bad(t, &named(underpay(fill(&net, &f, &q, 14 * TOK + 766), 1), "NC1c underpaid across 8 levels"), 0);
    run_bad(t, &named(underpay(fill(&net, &f, &p.left(16 * TOK), 16 * TOK), 1), "NC2 sell-out underpaid"), 0);
    let mut s = cross(&net, &f, &p.left(p.total - TOK), 5 * TOK, 260_000_000);
    s.outputs[1].value -= 1;
    s.outputs[4].value += 1;
    run_bad(t, &named(s, "NC3 cross underpaid"), 1);
    // NC4 rounding: every level-piece rounded down
    let s = fill(&net, &f, &q, 9 * TOK + 7);
    let down = {
        let (mut pos, end, mut c) = (q.total - q.amount_left, q.total - q.amount_left + 9 * TOK + 7, 0);
        while pos < end {
            let lvl = pos / q.level_size;
            let upto = ((lvl + 1) * q.level_size).min(end);
            c += quote_at(upto - pos, q.table[lvl as usize], SCALE, false).unwrap();
            pos = upto;
        }
        c
    };
    assert!(down < s.outputs[0].value as i64);
    run_bad(t, &named(fill_with(&net, &f, &q, 9 * TOK + 7, Some(down)), "NC4 every level rounded down"), 0);

    // NC5 a level boundary skipped (level 2's units at level 1's price)
    let skipped = {
        let (mut pos, end, mut c) = (q.total - q.amount_left, q.total - q.amount_left + 6 * TOK, 0);
        while pos < end {
            let lvl = pos / q.level_size;
            let upto = ((lvl + 2) * q.level_size).min(end);
            c += quote_at(upto - pos, q.table[lvl as usize], SCALE, true).unwrap();
            pos = upto;
        }
        c
    };
    run_bad(t, &named(fill_with(&net, &f, &q, 6 * TOK, Some(skipped)), "NC5 a level boundary skipped"), 0);

    // NC6 the continuation one level back (the custody holds less than the state says)
    let mut s = fill(&net, &f, &p, 3 * TOK);
    s.outputs[1].script_public_key = spk_of(&net.curve(&p.left(p.total - TOK)));
    run_bad(t, &named(s, "NC6 continuation one level back"), 0);
    // NC7 a second fill at the previous level's price
    let stale = quote_at(4 * TOK, p.table[1], SCALE, true).unwrap();
    run_bad(t, &named(fill_with(&net, &f, &p.left(p.total - 3 * TOK), 4 * TOK, Some(stale)), "NC7 previous level's price"), 0);
    // NC8 nine levels in one fill, paid for the first eight
    let n9 = 16 * TOK;
    assert_eq!(levels_touched(&q, n9), 9);
    let first8 = {
        let s0 = q.total - q.amount_left;
        curve_cost(&q, (s0 / q.level_size + 8) * q.level_size - s0).unwrap()
    };
    run_bad(t, &named(fill_with(&net, &f, &q, n9, Some(first8)), "NC8 nine levels paid for eight"), 0);
    run_bad(t, &named(fill_with(&net, &f, &q, n9, Some(curve_cost_uncapped(&q, n9))), "NC8b nine levels paid in full"), 0);
    // NC9 a level past the table (total beyond nLevels * levelSize)
    let over = CurveP::new(pk(&f.maker), hip2_grid(P250, 50), 2 * TOK, 100 * TOK + 500);
    run_bad(t, &named(fill_with(&net, &f, &over.left(500), 500, Some(KAS)), "NC9 a level past the table"), 0);

    // NC10 custody skim: the rest custody one base unit short, the taker one more
    let mut s = fill(&net, &f, &p, 3 * TOK);
    let rest = p.total - 3 * TOK;
    set_next(&mut s, &[custody(rest - 1, a), plain(3 * TOK + 1, &taker)]);
    s.outputs[2].script_public_key = t.spk(&custody(rest - 1, a));
    s.outputs[3].script_public_key = t.spk(&plain(3 * TOK + 1, &taker));
    run_bad(t, &named(s, "NC10 custody skim"), 0);
    // NC11 the rest custody with no covenant binding (an unbound copy of the holder script)
    let mut s = fill(&net, &f, &p, 3 * TOK);
    set_next(&mut s, &[plain(3 * TOK, &taker)]);
    s.outputs[2].covenant = None;
    s.outputs.swap(2, 3);
    if let Role::Call { args, .. } = &mut s.inputs[0].role {
        args[2] = int(3);
    }
    run_bad(t, &named(s, "NC11 rest custody without the token covenant"), 0);
    // NC12 the rest custody's carrier taken
    let mut s = fill(&net, &f, &p, 3 * TOK);
    s.outputs[2].value -= KAS as u64 / 2;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += KAS as u64 / 2;
    run_bad(t, &named(s, "NC12 rest custody carrier taken"), 0);
    // NC13 the continuation's carrier taken
    let mut s = fill(&net, &f, &p, 3 * TOK);
    s.outputs[1].value -= KAS as u64 / 2;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += KAS as u64 / 2;
    run_bad(t, &named(s, "NC13 continuation carrier taken"), 0);
    // NC14 a second output of the curve's id (the taker's)
    let mut s = fill(&net, &f, &p, 3 * TOK);
    s.outputs.push(out(KAS, p2pk_spk(&taker), Some((0, a))));
    let l = s.outputs.len() - 2;
    s.outputs[l].value -= KAS as u64;
    run_bad(t, &named(s, "NC14 a second output of the curve's id"), 0);
    // NC15 a sell-out that keeps an output of the curve's id
    let mut s = fill(&net, &f, &p.left(4 * TOK), 4 * TOK);
    s.outputs.push(out(KAS, p2pk_spk(&taker), Some((0, a))));
    let l = s.outputs.len() - 2;
    s.outputs[l].value -= KAS as u64;
    run_bad(t, &named(s, "NC15 sell-out keeps an output of the curve's id"), 0);
    // NC16 the payout to another key
    let mut s = fill(&net, &f, &p, 3 * TOK);
    s.outputs[0].script_public_key = p2pk_spk(&taker);
    run_bad(t, &named(s, "NC16 payout to the taker"), 0);
    // NC17 before activeFrom
    let mut timed = p.clone();
    timed.active_from = NOW as i64 + 1;
    run_bad(t, &named(fill(&net, &f, &timed, TOK), "NC17 before activeFrom"), 0);
    // NC18 below minFill (not the last)
    let mut mf = p.clone();
    mf.min_fill = 500;
    run_bad(t, &named(fill(&net, &f, &mf, 499), "NC18 below minFill"), 0);
    // NC19 n = 0 under minFill 0 (the maker's field): a free re-spend that moves the order
    let mut z = p.clone();
    z.min_fill = 0;
    let curve = net.curve(&z);
    let s = Scn {
        name: "a fill of n = 0".into(),
        inputs: vec![
            settle_in(&curve, 0, 1, 2, CARRIER, a),
            tok_in(t, &custody(z.amount_left, a), Some(&[custody(z.amount_left, a)]), Wit::CovId),
            p2pk_in(&f.taker, 10 * KAS),
        ],
        outputs: vec![
            out(0, p2pk_spk(&z.maker), None),
            out(CARRIER, spk_of(&curve), Some((0, a))),
            out(CARRIER, t.spk(&custody(z.amount_left, a)), Some((1, TOKEN_COV))),
            out(10 * KAS - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
    };
    run_bad(t, &named(s, "NC19"), 0);
    // NC20 a negative scale (the maker's field): sells for nothing
    let mut neg = p.clone();
    neg.scale = -SCALE;
    run_bad(t, &named(fill_with(&net, &f, &neg, 3 * TOK, Some(0)), "NC20 a negative scale"), 0);
    // NC21 amountLeft above total: a fill from before the first level takes level 0's price for more than a level
    let mut ab = p.clone();
    ab.amount_left = p.total + 2 * TOK - 1;
    let n = 4 * TOK - 1;
    let cheap = quote_at(n, p.table[0], SCALE, true).unwrap();
    run_bad(t, &named(fill_with(&net, &f, &ab, n, Some(cheap)), "NC21 amountLeft above total"), 0);
    // NC22 a level priced below the tip (the proceeds of that level are negative)
    let mut lowp = p.clone();
    lowp.tip = TIP;
    lowp.table[0] = TIP / 2;
    let s = fill_with(
        &net,
        &f,
        &lowp,
        3 * TOK,
        Some(quote_at(TOK, lowp.table[1] - TIP, SCALE, true).unwrap() - quote_at(2 * TOK, TIP / 2, SCALE, false).unwrap()),
    );
    run_bad(t, &named(s, "NC22 a level below the tip"), 0);
    // NC24 a negative tip (the maker's field): every level pays price + |tip|
    let mut nt = p.clone();
    nt.tip = -TIP;
    let pay = quote_at(2 * TOK, p.table[0] + TIP, SCALE, true).unwrap() + quote_at(TOK, p.table[1] + TIP, SCALE, true).unwrap();
    run_bad(t, &named(fill_with(&net, &f, &nt, 3 * TOK, Some(pay)), "NC24 a negative tip"), 0);
    // NC23 a wrong rsLen (3 more: the push header read as script)
    let curve = net.curve(&p);
    let mut s = fill(&net, &f, &p, 3 * TOK);
    let real = bytecode(&curve).len() as i64;
    s.inputs[0] = settle_in_rs(&curve, 3 * TOK, 1, 2, real + 3, CARRIER, a);
    let rs_bad = {
        let ss = {
            let (tx, _) = build(t, &s, &[]);
            tx.inputs[0].signature_script.clone()
        };
        let rs = ss[ss.len() - (real as usize + 3)..].to_vec();
        let mut v = rs[..COV_AT].to_vec();
        v.extend_from_slice(&TOKEN_COV.as_bytes());
        v.extend_from_slice(&rs[COV_AT + 32..LEFT_AT]);
        v.extend_from_slice(&(p.total - 3 * TOK).to_le_bytes());
        v.extend_from_slice(&rs[LEFT_AT + 8..]);
        v
    };
    s.outputs[1].script_public_key = pay_to_script_hash_script(&rs_bad);
    run_bad(t, &named(s, "NC23 rsLen off by 3"), 0);
}

/// Custody attacks (ids are the ablation catalog's).
#[test]
fn curve_custody_negatives() {
    let f = fx();
    let net = Net::new();
    let t = &net.t;
    let p = hip2_50(&f);
    let taker = pk(&f.taker);
    let a = CURVE_COV;
    let x = Hash::from_bytes([0xd5; 32]);

    // NK1 a look-alike custody (another template with the custody's state and the token's covenant id), sold out
    let mut s = fill(&net, &f, &p.left(4 * TOK), 4 * TOK);
    let fake = fake_holder(t, &t.redeem(&custody(4 * TOK, a)));
    s.inputs[1] = Inp {
        entry: utxo(CARRIER, pay_to_script_hash_script(&fake), Some(TOKEN_COV)),
        role: Role::Raw { ss: push(&fake), name: "holder.lookalike" },
        outpoint: None,
    };
    s.outputs[1].covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: TOKEN_COV });
    run_bad(t, &named(s, "NK1 look-alike custody"), 0);
    // NK2 a planted non-P2SH UTXO of the token's covenant id that pushes a genuine custody redeem
    let mut s = fill(&net, &f, &p.left(4 * TOK), 4 * TOK);
    let real = t.redeem(&custody(4 * TOK, a));
    s.inputs[1] = Inp {
        entry: utxo(CARRIER, ScriptPublicKey::new(0, vec![OpDrop, OpTrue].into()), Some(TOKEN_COV)),
        role: Role::Raw { ss: push(&real), name: "planted" },
        outpoint: None,
    };
    run_bad(t, &named(s, "NK2 planted non-P2SH custody"), 0);
    // NK3 a sibling UTXO of the curve's id beside the fill (planted)
    let mut s = fill(&net, &f, &p, 3 * TOK);
    s.inputs.push(Inp { entry: utxo(KAS, p2pk_spk(&taker), Some(a)), role: Role::P2pk { kp: f.taker }, outpoint: None });
    let l = s.outputs.len() - 1;
    s.outputs[l].value += KAS as u64;
    run_bad(t, &named(s, "NK3 a sibling UTXO of the curve's id"), 0);
    // NK4 the custody of another token (same program, owned by the curve, exact amount)
    let mut s = fill(&net, &f, &p.left(4 * TOK), 4 * TOK);
    let mut other = tok_in(t, &custody(4 * TOK, a), Some(&[plain(4 * TOK, &taker)]), Wit::CovId);
    other.entry = utxo(CARRIER, other.entry.script_public_key.clone(), Some(OTHER_COV));
    s.inputs[1] = other;
    s.outputs[1].covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: OTHER_COV });
    run_bad(t, &named(s, "NK4 custody of another token"), 0);
    // NK5..NK8 owner, scheme, borrow, commitment of the custody (sold out, all 4 tokens to the taker)
    let swap = |owner: [u8; 32], scheme: u8, borrow: u8, ext: [u8; 32], extra_cov: bool, wit: Wit, what: &str| {
        let mut s = fill(&net, &f, &p.left(4 * TOK), 4 * TOK);
        let st = St { amount: 4 * TOK, owner, scheme, borrow, ext };
        s.inputs[1] = tok_in(t, &st, Some(&[St { ext, ..plain(4 * TOK, &taker) }]), wit);
        s.outputs[1].script_public_key = t.spk(&St { ext, ..plain(4 * TOK, &taker) });
        if extra_cov {
            s.inputs.push(Inp { entry: utxo(KAS, p2pk_spk(&taker), Some(x)), role: Role::P2pk { kp: f.taker }, outpoint: None });
            let l = s.outputs.len() - 1;
            s.outputs[l].value += KAS as u64;
        }
        named(s, what)
    };
    run_bad(t, &swap(x.as_bytes(), SCHEME_COVID, 0, EXT, true, Wit::CovId, "NK5 custody owned by another covenant id"), 0);
    run_bad(t, &swap(taker, SCHEME_P2PK, 0, EXT, false, Wit::P2pk(f.taker), "NK6 custody of the taker's key"), 0);
    run_bad(t, &swap(a.as_bytes(), SCHEME_COVID, 1, EXT, false, Wit::CovId, "NK7 custody with borrowing enabled"), 0);
    run_bad(t, &swap(a.as_bytes(), SCHEME_COVID, 0, [0xef; 32], false, Wit::CovId, "NK8 custody of another commitment"), 0);
    // NK9 the custody holds more than amountLeft (a larger UTXO of the id standing in), the rest kept by the taker
    let mut s = fill(&net, &f, &p.left(4 * TOK), 4 * TOK);
    s.inputs[1] = tok_in(t, &custody(5 * TOK, a), Some(&[plain(5 * TOK, &taker)]), Wit::CovId);
    s.outputs[1].script_public_key = t.spk(&plain(5 * TOK, &taker));
    run_bad(t, &named(s, "NK9 custody larger than amountLeft"), 0);
    // NK10 a second UTXO of the curve's id taken beside the custody
    let mut s = fill(&net, &f, &p, 3 * TOK);
    let rest = p.total - 3 * TOK;
    set_next(&mut s, &[custody(rest, a), plain(8 * TOK, &taker)]);
    s.inputs.insert(2, tok_in(t, &custody(5 * TOK, a), None, Wit::CovId));
    s.outputs[3].script_public_key = t.spk(&plain(8 * TOK, &taker));
    let l = s.outputs.len() - 1;
    s.outputs[l].value += CARRIER as u64;
    run_bad(t, &named(s, "NK10 a second UTXO of the curve's id"), 0);
}

/// A stray of the curve at token slot k (8 / 8 program), and a 9th token input the scan does not reach.
fn stray_at(net: &Net, f: &Fx, p: &CurveP, k: usize, fillers: usize) -> Scn {
    let t = &net.t;
    let a = CURVE_COV;
    let taker = pk(&f.taker);
    let n = 3 * TOK;
    let rest = p.amount_left - n;
    let curve = net.curve(p);
    // token inputs in slot order: the stray at k, the custody at 0 (or 1 when the stray is at 0), fillers elsewhere
    let slots = (fillers + 2).max(k + 1);
    let cust_slot = if k == 0 { 1 } else { 0 };
    let mut gained = 0;
    let mut toks = vec![];
    for j in 0..slots {
        let (st, wit) = if j == k {
            gained += TOK;
            (custody(TOK, a), Wit::CovId)
        } else if j == cust_slot {
            (custody(p.amount_left, a), Wit::CovId)
        } else {
            gained += 1;
            (plain(1, &taker), Wit::P2pk(f.taker))
        };
        toks.push((st, wit));
    }
    let next = [custody(rest, a), plain(n + gained, &taker)];
    let mut inputs = vec![settle_in(&curve, n, 1 + cust_slot, 2, CARRIER, a)];
    for (j, (st, wit)) in toks.into_iter().enumerate() {
        inputs.push(tok_in(t, &st, (j == 0).then_some(&next[..]), wit));
    }
    let funding = 100 * KAS;
    inputs.push(p2pk_in(&f.taker, funding));
    let pay = curve_cost(p, n).unwrap();
    let carriers = (slots as i64 - 1) * CARRIER;
    Scn {
        name: format!("stray of the curve at token slot {k} ({slots} token inputs)"),
        inputs,
        outputs: vec![
            out(pay, p2pk_spk(&p.maker), None),
            out(CARRIER, spk_of(&net.curve(&p.left(rest))), Some((0, a))),
            out(CARRIER, t.spk(&custody(rest, a)), Some((1, TOKEN_COV))),
            out(CARRIER + carriers, t.spk(&plain(n + gained, &taker)), Some((1, TOKEN_COV))),
            out(funding - pay - CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
    }
}

#[test]
fn curve_stray_guard_slots_8x8() {
    let f = fx();
    let net = Net::on(Token::named("KCC20Ref_8x8"));
    let p = hip2_50(&f);
    for k in 0..8 {
        run_bad(&net.t, &named(stray_at(&net, &f, &p, k, 0), &format!("NS{k}")), 0);
    }
    // NS8: 9 token inputs, the stray at slot 8 (the 8 / 8 program refuses a 9th input on its own)
    run_bad(&net.t, &named(stray_at(&net, &f, &p, 8, 0), "NS8"), 0);
}

/// Cancel attacks.
#[test]
fn curve_cancel_negatives() {
    let f = fx();
    let net = Net::new();
    let p = hip2_50(&f).left(40 * TOK);
    let thief = keypair();
    run_bad(&net.t, &named(cancel(&net, &f, &p, &thief, 1), "NX1 cancel by another key"), 0);
    // the maker's SIGHASH_NONE signature, the outputs routed to someone else
    let mut s = cancel(&net, &f, &p, &f.maker, 2);
    let to = pk(&thief);
    set_next(&mut s, &[plain(p.amount_left, &to)]);
    s.outputs[0].script_public_key = net.t.spk(&plain(p.amount_left, &to));
    run_bad(&net.t, &named(s, "NX2 cancel signed SIGHASH_NONE"), 0);
}

// ---------------------------------------------------------------- created inside the token's genesis

/// Inputs 0 (the curve) and 1 (its custody) spend outputs `curve_out` and `custody_out` of one transaction `txid`.
fn same_tx(mut s: Scn, txid: [u8; 32], curve_out: u32, custody_out: u32) -> Scn {
    let id = TransactionId::from_bytes(txid);
    s.inputs[0].outpoint = Some(TransactionOutpoint { transaction_id: id, index: curve_out });
    s.inputs[1].outpoint = Some(TransactionOutpoint { transaction_id: id, index: custody_out });
    s
}

#[test]
fn curve_created_in_the_token_genesis() {
    let f = fx();
    let net = Net::new();
    let t = &net.t;
    let creator = pk(&f.maker);
    let unbound = hip2_50(&f).unbound();
    let bound = |left: i64| CurveP { token_cov: TOKEN_COV.as_bytes(), ..unbound.left(left) };

    // one transaction: the token's group (outputs 0 and 1) and the curve's own group (output 2), one P2PK input
    let op0 = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0x55; 32]), index: 0 };
    let funding = 200 * KAS;
    let mut o_curve = out(CARRIER, spk_of(&net.curve(&unbound)), None);
    let x = covenant_id(op0, [(2u32, &o_curve)].into_iter());
    o_curve.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: x });
    let o_custody = out(CARRIER, t.spk(&custody(unbound.total, x)), None);
    let o_keep = out(CARRIER, t.spk(&plain(900 * TOK, &creator)), None);
    let tid = covenant_id(op0, [(0u32, &o_custody), (1u32, &o_keep)].into_iter());
    let to_token = |mut o: TransactionOutput| {
        o.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: tid });
        o
    };
    let mut creator_in = p2pk_in(&f.maker, funding);
    creator_in.outpoint = Some(op0);
    let genesis = Scn {
        name: "PG1 token genesis with the curve in one transaction".into(),
        inputs: vec![creator_in],
        outputs: vec![
            to_token(o_custody),
            to_token(o_keep),
            o_curve,
            out(funding - 3 * CARRIER - NET_FEE / 10, p2pk_spk(&creator), None),
        ],
        lock_time: NOW,
    };
    run_ok(t, &genesis);
    assert!(!bytecode(&net.curve(&unbound)).windows(32).any(|w| w == tid.as_bytes()), "the curve script does not name the token");
    let mut bad = genesis.clone();
    bad.outputs[2].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: Hash::from_bytes([0x99; 32]) });
    let (tx, entries) = build(t, &bad, &[]);
    assert!(execute(&tx, &entries).is_err(), "a genesis id that is not the hash of its group is refused");

    // the first fill of the unbound curve binds it; the bound continuation fills as usual
    let g = [0x66; 32];
    run_ok(t, &same_tx(named(fill(&net, &f, &unbound, 3 * TOK), "PG2 first fill binds"), g, 2, 0));
    run_ok(t, &named(fill(&net, &f, &bound(97 * TOK), 4 * TOK), "PG3 bound continuation"));
    run_ok(
        t,
        &same_tx(named(fill(&net, &f, &unbound.left(100 * TOK), 100 * TOK - 84 * TOK), "PG4 first fill of an unbound curve"), g, 2, 0),
    );

    // NB1 the custody from another transaction; NB2 the continuation left unbound; NB3 a bound curve, another token's
    // custody from the continuation's own transaction (the taker of the fill before planted it)
    run_bad(t, &named(fill(&net, &f, &unbound, 3 * TOK), "NB1 custody from another transaction"), 0);
    let mut s = same_tx(fill(&net, &f, &unbound, 3 * TOK), g, 2, 0);
    s.outputs[1].script_public_key = spk_of(&net.curve(&unbound.left(97 * TOK)));
    run_bad(t, &named(s, "NB2 continuation left unbound"), 0);
    let b = bound(97 * TOK);
    let mut s = same_tx(fill(&net, &f, &b, 4 * TOK), [0x67; 32], 1, 4);
    let mut other =
        tok_in(t, &custody(97 * TOK, CURVE_COV), Some(&[custody(93 * TOK, CURVE_COV), plain(4 * TOK, &pk(&f.taker))]), Wit::CovId);
    other.entry = utxo(CARRIER, other.entry.script_public_key.clone(), Some(OTHER_COV));
    other.outpoint = s.inputs[1].outpoint;
    s.inputs[1] = other;
    s.outputs[1].script_public_key = spk_of(&net.curve(&CurveP { token_cov: OTHER_COV.as_bytes(), ..b.left(93 * TOK) }));
    s.outputs[2].covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: OTHER_COV });
    s.outputs[3].covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: OTHER_COV });
    run_bad(t, &named(s, "NB3 bound curve, another token's custody from its own transaction"), 0);

    // PG5 the maker's cancel of the unbound curve: cancel(sig) reads the maker's key only, and the custody (genesis
    // output 0, owned by the curve) moves with the curve input under its token's covenant id; NX3 another key's cancel
    run_ok(t, &same_tx(named(cancel(&net, &f, &unbound, &f.maker, 1), "PG5 cancel of an unbound curve"), g, 2, 0));
    let thief = keypair();
    run_bad(t, &same_tx(named(cancel(&net, &f, &unbound, &thief, 1), "NX3 cancel of an unbound curve by another key"), g, 2, 0), 0);
}

// ---------------------------------------------------------------- look-alike holders (kob_v2_tests)

fn filler(mut len: usize) -> Vec<u8> {
    let mut v = vec![];
    while len > 0 {
        let take = len.min(300);
        let rest = len - take;
        let (take, rest) = if rest > 0 && rest < 2 { (take - 2, rest + 2) } else { (take, rest) };
        if take <= 76 {
            v.push((take - 1) as u8);
            v.extend(std::iter::repeat_n(0xaau8, take - 1));
        } else if take <= 257 {
            v.extend([0x4c, (take - 2) as u8]);
            v.extend(std::iter::repeat_n(0xaau8, take - 2));
        } else {
            v.push(0x4d);
            v.extend_from_slice(&((take - 3) as u16).to_le_bytes());
            v.extend(std::iter::repeat_n(0xaau8, take - 3));
        }
        len = rest;
    }
    v
}
fn push_count(b: &[u8]) -> usize {
    let (mut i, mut n) = (0usize, 0usize);
    while i < b.len() {
        let op = b[i] as usize;
        i += match op {
            0 => 1,
            1..=75 => 1 + op,
            0x4c => 2 + b[i + 1] as usize,
            0x4d => 3 + u16::from_le_bytes([b[i + 1], b[i + 2]]) as usize,
            _ => panic!("not a push: {op:#x}"),
        };
        n += 1;
    }
    n
}
/// Filler prefix, the genuine 112-byte state, a suffix that drops every push and returns true.
fn fake_holder(t: &Token, real: &[u8]) -> Vec<u8> {
    use kaspa_txscript::opcodes::codes::{OpEndIf, OpFalse, OpIf};
    let (pre, suf) = (t.prefix.len(), t.suffix.len());
    let mut v = filler(pre);
    v.extend_from_slice(&real[pre..pre + 112]);
    let drops = push_count(&v);
    v.extend(std::iter::repeat_n(OpDrop, drops));
    let pad = suf - drops - 1;
    v.extend([OpFalse, OpIf]);
    v.extend(filler(pad - 3));
    v.push(OpEndIf);
    v.push(OpTrue);
    assert_eq!(v.len(), real.len());
    v
}

// ---------------------------------------------------------------- placement gates

/// The placement gates of a curve (`CurveAskState::check_numbers`, spec section 5) on the scenario's numbers.
fn sec_state(p: &CurveP) -> kob_protocol::state::CurveAskState {
    kob_protocol::state::CurveAskState {
        maker: p.maker,
        token_cov_id: p.token_cov,
        token_tpl_hash: [0x11; 32],
        tpl_prefix_len: 1,
        tpl_suffix_len: 1,
        scale: p.scale,
        min_fill: p.min_fill,
        tip: p.tip,
        active_from: p.active_from,
        level_size: p.level_size,
        total: p.total,
        amount_left: p.amount_left,
        extension_commitment: EXT,
        table: p.table.clone(),
    }
}

/// A fill crosses at most 8 levels and takes at least minFill: the gates refuse a minFill above 7
/// levels + 1 (what one fill always reaches), and at that bound the worst position still takes a
/// fill in the engine: after a purchase that stops one base unit short of a level's end, a fill of
/// 7 levels + 1.
#[test]
fn sec_curve_min_fill_beyond_the_cross_cap() {
    let f = fx();
    let net = Net::new();
    let t = &net.t;
    let l = 2 * TOK;
    // (a) and (b) of the reproducer: refused at placement
    assert!(sec_state(&CurveP { min_fill: 8 * l + 1, ..hip2_50(&f) }).check_numbers().is_err());
    assert!(sec_state(&CurveP { min_fill: 7 * l + 2, ..hip2_50(&f) }).check_numbers().is_err());
    // the bound: fillable from the worst position
    let p = CurveP { min_fill: 7 * l + 1, ..hip2_50(&f) };
    sec_state(&p).check_numbers().expect("the bound passes");
    run_ok(t, &named(fill(&net, &f, &p, 8 * l - 1), "SEC-L1 a taker buys 8 levels less one base unit"));
    let after = p.left(p.total - (8 * l - 1));
    let g = sec_state(&after);
    g.check_numbers().expect("the continuation passes");
    assert_eq!(g.max_fill(), 7 * l + 1);
    run_ok(t, &named(fill(&net, &f, &after, 7 * l + 1), "SEC-L1 the continuation: a fill of 7 levels + 1"));
}
