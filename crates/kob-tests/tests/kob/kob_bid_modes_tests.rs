//! KobBid delivery modes (docs/design/bid-delivery-spec.md, sections 1 to 7 and 9) in whole transactions through the script
//! engine, on the reference 3 / 3 program (`KCC20Ref`) and the published public-mint build (`KCC20PublicMint`); the stray
//! scan per slot on the 8 / 8 variant (`KCC20Ref_8x8`).
//!
//! Mode 0 new, mode 1 accumulate (bought-custody), mode 2 top-up (forms 0 / 1 / 2). Every mode x form continuing and ending,
//! the refund slot of a terminal fill, the stray guard with the bought-custody excluded, the refund entry that returns the
//! custody, the claim shapes CL1..CL4 of section 7 through `cancel`, the ordering rules of section 6 (what the token program
//! refuses, the orders accept), the slot budget of a 3 / 3 program, script size, state layout and sig-op bound.
//!
//! Output lines follow the ablation protocol (tools/ablation): `NEGATIVE <id> ... @<program>  [REJECTED ...]` for an
//! attack, and under `KOB_ABLATION` `ABLATION-PASS <id> ... @<program> all_inputs_ok=<bool>` when it is accepted and
//! `ABLATION-POS-FAIL <id> ...` when a positive scenario is rejected. Scenario ids are the first word of each name.
//!
//! Run: cargo test -p kob-tests --test kob_bid_modes_tests -- --nocapture --test-threads=1
#![allow(dead_code)]

#[path = "../common/mod.rs"]
mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
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
use kob_protocol::artifacts::TemplateId;
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::{ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use common::{bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script, state_layout};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const EXT: [u8; 32] = [0xee; 32];
const EXT2: [u8; 32] = [0xef; 32];
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const SCALE: i64 = 1_000;
const TIP: i64 = 100_000;
const EXPIRY: i64 = 400_000_000;
const REFUND_TIP: i64 = 3_000_000;
const REFUND_MIN: i64 = 100_000_000;
/// tx.daa of every transaction (its lock time); UTXOs are at DAA 1,000.
const NOW: u64 = 1_000_000;
const DC: i64 = KAS; // delivery carrier
const CARRIER: i64 = KAS; // KAS on the token UTXOs of the sellers and of the asks
const P250: i64 = 250_000_000;
const P240: i64 = 240_000_000;
const THR: i64 = 999; // recvThreshold = minFill - 1
const TT: i64 = 2_000_000; // topupTip: 0.02 KAS
const GTC: i64 = 0;
const IOC: i64 = 1;
const FOK: i64 = 2;
/// Whole tokens a bid of the fixtures is funded for.
const QTY: i64 = 10;
/// Redeem-script offset of the 8 payload bytes of `bought` (spec section 2).
const BOUGHT_AT: usize = 314;
/// Script size of KobBid before the delivery modes, and the ceiling of section 9.
const KOBBID_BEFORE: usize = 1_629;
const SIZE_CEILING: usize = KOBBID_BEFORE + 1_150;

const BID_COV: Hash = Hash::from_bytes([0xb1; 32]);
const BID2_COV: Hash = Hash::from_bytes([0xb2; 32]);
const BID3_COV: Hash = Hash::from_bytes([0xb3; 32]);
const ASK_COV: Hash = Hash::from_bytes([0xa1; 32]);

fn ablation() -> bool {
    std::env::var_os("KOB_ABLATION").is_some()
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
fn bytes(v: &[u8]) -> ArtifactValue {
    v.to_vec().into()
}
fn int(v: i64) -> ArtifactValue {
    ArtifactValue::Int(v)
}
fn push(data: &[u8]) -> Vec<u8> {
    push_redeem_script(data)
}

// ---------------------------------------------------------------- token states

#[derive(Clone, Copy, Debug, PartialEq)]
struct St {
    amount: i64,
    owner: [u8; 32],
    scheme: u8,
    borrow: u8,
    guard: [u8; 32],
    ext: [u8; 32],
}
fn thr_guard(t: i64) -> [u8; 32] {
    let mut g = [0u8; 32];
    g[..8].copy_from_slice(&t.to_le_bytes());
    g
}
/// A key-owned plain holder.
fn plain(amount: i64, owner: &[u8; 32]) -> St {
    St { amount, owner: *owner, scheme: 0, borrow: 0, guard: [0; 32], ext: EXT }
}
/// A key-owned receive UTXO: borrow scheme amount-threshold with threshold `t`.
fn recv(amount: i64, owner: &[u8; 32], t: i64) -> St {
    St { amount, owner: *owner, scheme: 0, borrow: 1, guard: thr_guard(t), ext: EXT }
}
/// A custody owned by a covenant id (a bought-custody when the id is a bid's).
fn custody(amount: i64, owner: Hash) -> St {
    St { amount, owner: owner.as_bytes(), scheme: 4, borrow: 0, guard: [0; 32], ext: EXT }
}
impl St {
    fn with(mut self, f: impl FnOnce(&mut St)) -> St {
        f(&mut self);
        self
    }
    fn plus(self, n: i64) -> St {
        self.with(|s| s.amount += n)
    }
    fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0x08];
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.push(0x20);
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.scheme, 0x01, self.borrow, 0x20]);
        v.extend_from_slice(&self.guard);
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
            ("borrow_guard".to_string(), bytes(&self.guard)),
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
    /// A KCC-20 program compiled from source: "KCC20Ref" (3 / 3) or a slot variant such as "KCC20Ref_8x8".
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
    fn tok_args(&self) -> Vec<ArtifactValue> {
        vec![bytes(&TOKEN_COV.as_bytes()), bytes(&self.hash), int(self.prefix.len() as i64), int(self.suffix.len() as i64)]
    }
}
/// The two 3 / 3 programs every scenario runs on.
fn tokens() -> Vec<Token> {
    vec![Token::named("KCC20Ref"), Token::pinned(TemplateId::Kcc20PublicMint)]
}

// ---------------------------------------------------------------- scenario model

#[derive(Clone)]
enum Wit {
    /// owner path, covenant-id owner (no bytes)
    CovId,
    /// owner path, the owner key's signature
    P2pk(Keypair),
    /// borrow path of an amount-threshold UTXO: the path byte 0x01 alone (leader only)
    Borrow,
    /// owner path with no witness bytes at all (an input nobody authorised)
    Nothing,
}
#[derive(Clone)]
enum Role {
    Call { art: SilAbiArtifact, entry: &'static str, args: Vec<ArtifactValue>, name: &'static str },
    TokLeader { redeem: Vec<u8>, next: Vec<ArtifactValue>, wit: Wit },
    TokDelegate { redeem: Vec<u8>, wit: Wit },
    P2pk { kp: Keypair },
    /// An order entry whose one argument is the key's SIGHASH_ALL signature (cancel).
    CallSig { art: SilAbiArtifact, entry: &'static str, kp: Keypair },
    /// A crafted input: this signature script as it is.
    RawSs { ss: Vec<u8>, name: &'static str },
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
}
fn out(value: i64, spk: ScriptPublicKey, cov: Option<(usize, Hash)>) -> TransactionOutput {
    assert!(value >= 0, "negative output value {value}");
    TransactionOutput {
        value: value as u64,
        script_public_key: spk,
        covenant: cov.map(|(a, c)| CovenantBinding { authorizing_input: a as u16, covenant_id: c }),
    }
}
fn tok_leader(t: &Token, value: i64, s: &St, next: &[St], wit: Wit) -> Inp {
    let redeem = t.redeem(s);
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&redeem), 1_000, false, Some(TOKEN_COV)),
        role: Role::TokLeader { redeem, next: next.iter().map(St::val).collect(), wit },
    }
}
fn tok_delegate(t: &Token, value: i64, s: &St, wit: Wit) -> Inp {
    let redeem = t.redeem(s);
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&redeem), 1_000, false, Some(TOKEN_COV)),
        role: Role::TokDelegate { redeem, wit },
    }
}
fn tok_out(t: &Token, value: i64, s: &St, leader: usize) -> TransactionOutput {
    out(value, t.spk(s), Some((leader, TOKEN_COV)))
}
fn p2pk_in(kp: &Keypair, value: i64) -> Inp {
    Inp { entry: UtxoEntry::new(value as u64, p2pk_spk(&pk(kp)), 0, false, None), role: Role::P2pk { kp: *kp } }
}
fn call(art: &SilAbiArtifact, entry: &'static str, args: Vec<ArtifactValue>, name: &'static str, value: i64, c: Hash) -> Inp {
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&bytecode(art)), 1_000, false, Some(c)),
        role: Role::Call { art: art.clone(), entry, args, name },
    }
}
fn cancel_in(art: &SilAbiArtifact, value: i64, c: Hash, kp: &Keypair) -> Inp {
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&bytecode(art)), 1_000, false, Some(c)),
        role: Role::CallSig { art: art.clone(), entry: "cancel", kp: *kp },
    }
}
fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(1).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(1);
    s
}
fn build(t: &Token, s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = s.inputs.iter().map(|i| i.entry.clone()).collect();
    let inputs: Vec<TransactionInput> = (0..s.inputs.len())
        .map(|k| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([k as u8 + 1; 32]), index: k as u32 },
                vec![],
                0,
                budgets.get(k).copied().unwrap_or(0),
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, s.outputs.clone(), NOW, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    for (k, inp) in s.inputs.iter().enumerate() {
        let sig_of = |kp: &Keypair| sign(&unsigned, &entries, k, kp);
        let ss = match &inp.role {
            Role::Call { art, entry, args, .. } => {
                let mut ss = encode_entry_sig_script(art, entry, args).expect("encode entry");
                ss.extend_from_slice(&push(&bytecode(art)));
                ss
            }
            Role::TokLeader { redeem, next, wit } => {
                let w = match wit {
                    Wit::CovId | Wit::Nothing => vec![0x00],
                    Wit::P2pk(kp) => [vec![0x00], sig_of(kp)].concat(),
                    Wit::Borrow => vec![0x01],
                };
                let mut ss = t.entry_ss("transfer", &[ArtifactValue::Array(next.clone()), w.into()]);
                ss.extend_from_slice(&push(redeem));
                ss
            }
            Role::TokDelegate { redeem, wit } => {
                let w = match wit {
                    Wit::CovId | Wit::Nothing | Wit::Borrow => vec![],
                    Wit::P2pk(kp) => sig_of(kp),
                };
                let mut ss = t.entry_ss("transfer_delegator", &[w.into()]);
                ss.extend_from_slice(&push(redeem));
                ss
            }
            Role::P2pk { kp } => push(&sig_of(kp)),
            Role::RawSs { ss, .. } => ss.clone(),
            Role::CallSig { art, entry, kp } => {
                let mut ss = encode_entry_sig_script(art, entry, &[sig_of(kp).into()]).expect("encode entry");
                ss.extend_from_slice(&push(&bytecode(art)));
                ss
            }
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
        Role::RawSs { name, .. } => name,
        Role::CallSig { entry, .. } => entry,
    }
}
#[derive(Default, Clone, Copy)]
struct Cost {
    size: u64,
    compute: u64,
    transient: u64,
    storage: u64,
    fee: u64,
}
/// Every input accepts. Returns the masses and the relay-floor fee with measured compute budgets.
fn ok(t: &Token, s: &Scn) -> Cost {
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
    println!(
        "POSITIVE {} @{}  [PASS] in={} out={} size={} compute={} transient_norm={} storage={} relay_fee={} sompi",
        s.name,
        t.name,
        tx.inputs.len(),
        tx.outputs.len(),
        m.size,
        m.compute,
        m.transient_normalized,
        m.storage,
        fee
    );
    Cost { size: m.size, compute: m.compute, transient: m.transient_normalized, storage: m.storage, fee }
}
/// Input `expect` must reject. `only`: every other input accepts (the attack is stopped by that input alone).
fn bad_at(t: &Token, s: &Scn, expect: usize, only: bool) {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: tx-level failure {e}", s.name));
    let r = &res[expect];
    if r.is_ok() && ablation() {
        println!("ABLATION-PASS {} @{} all_inputs_ok={}", s.name, t.name, res.iter().all(|r| r.is_ok()));
        return;
    }
    assert!(r.is_err(), "{} @{}: expected input {expect} ({}) to REJECT but it passed", s.name, t.name, role_name(&s.inputs[expect].role));
    let others: Vec<String> = res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
    if only {
        assert!(others.is_empty(), "{} @{}: inputs {} also reject: {res:?}", s.name, t.name, others.join(","));
    }
    println!(
        "NEGATIVE {} @{}  [REJECTED at in[{expect}] {}: {:?}]{}",
        s.name,
        t.name,
        role_name(&s.inputs[expect].role),
        r.as_ref().unwrap_err(),
        if others.is_empty() { String::new() } else { format!(" (also failing: {})", others.join(",")) }
    );
}
/// Only input `expect` rejects.
fn bad_only(t: &Token, s: &Scn, expect: usize) {
    bad_at(t, s, expect, true)
}
fn bad(t: &Token, s: &Scn, expect: usize) {
    bad_at(t, s, expect, false)
}
fn named(mut s: Scn, name: &str) -> Scn {
    s.name = name.into();
    s
}

// ---------------------------------------------------------------- the bid and its model

struct Fx {
    t: Token,
    maker: Keypair,
    maker2: Keypair,
    asker: Keypair,
    taker: Keypair,
    matcher: Keypair,
}
fn fx(t: Token) -> Fx {
    Fx { t, maker: keypair(), maker2: keypair(), asker: keypair(), taker: keypair(), matcher: keypair() }
}

/// The terms of a KobBid: price 2.50, tip 0.001, minFill one whole token, delivery carrier 1 KAS. mode 2 gets
/// recvThreshold THR and topupTip TT.
#[derive(Clone, Copy)]
struct BidP {
    maker: [u8; 32],
    min_fill: i64,
    tif: i64,
    expiry: i64,
    reserve: i64,
    dc: i64,
    mode: i64,
    thr: i64,
    ttip: i64,
    bought: i64,
}
impl BidP {
    fn new(maker: &Keypair, mode: i64) -> Self {
        let (thr, ttip) = if mode == 2 { (THR, TT) } else { (0, 0) };
        BidP { maker: pk(maker), min_fill: SCALE, tif: GTC, expiry: EXPIRY, reserve: 0, dc: DC, mode, thr, ttip, bought: 0 }
    }
    fn bought(self, b: i64) -> Self {
        BidP { bought: b, ..self }
    }
    fn tif(self, tif: i64) -> Self {
        BidP { tif, ..self }
    }
    fn expiry(self, expiry: i64) -> Self {
        BidP { expiry, ..self }
    }
}
fn bid_args(f: &Fx, p: &BidP) -> Vec<ArtifactValue> {
    let mut a = vec![bytes(&p.maker)];
    a.extend(f.t.tok_args());
    a.extend([bytes(&EXT), int(SCALE), int(p.min_fill), int(P250), int(TIP), int(p.tif), int(0), int(p.expiry), int(REFUND_TIP)]);
    a.extend([int(p.reserve), int(p.dc), int(0), int(0), int(0), int(0), int(0)]);
    a.extend([int(p.mode), int(p.thr), int(p.ttip), int(p.bought)]);
    a
}
fn bid(f: &Fx, p: &BidP) -> SilAbiArtifact {
    compile_contract(&common::contract_source("KobBid"), &bid_args(f, p), CompileOptions::default()).expect("compile KobBid")
}
fn spk_of(a: &SilAbiArtifact) -> ScriptPublicKey {
    pay_to_script_hash_script(&bytecode(a))
}
fn fill_args(n: i64, tok_in: usize, form: i64) -> Vec<ArtifactValue> {
    vec![bytes(&n.to_le_bytes()), int(tok_in as i64), int(0), int(form)]
}
/// The escrow of a bid funded for QTY whole tokens and one delivery carrier.
fn v0() -> i64 {
    QTY * (P250 + TIP) + DC
}
/// What `whole` whole tokens cost the bid (used == allIn at whole tokens).
fn spend(whole: i64) -> i64 {
    whole * (P250 + TIP)
}
fn quote(n: i64, r: i64, c: i64) -> i64 {
    let (m, q) = (n % SCALE, n / SCALE);
    q * r + m * (r / SCALE) + (m * (r % SCALE) + c) / SCALE
}

/// What the contract demands of an honest fill of `n` (spec section 4): the delivery state and its KAS, the continuation
/// (terms and value) or the refund of a terminal fill (amount, when it is REFUND_MIN or more).
struct Plan {
    d: St,
    d_kas: i64,
    cont: Option<(BidP, i64)>,
    refund: Option<i64>,
}
/// `cont_lead`: the token UTXO the fill continues (the bought-custody, or the receive UTXO of a top-up) and its KAS.
fn plan(p: &BidP, v: i64, n: i64, form: i64, cont_lead: Option<(St, i64)>) -> Plan {
    let used = quote(n, P250 + TIP, SCALE - 1);
    let all_in = quote(n, P250 + TIP, 0);
    let left = v - used;
    let m = p.maker;
    let (mut have_kas, mut on_d, mut released, mut resv) = (0, p.dc, p.dc, p.dc);
    let mut next = *p;
    let mut d = plain(n, &m);
    let mut cust_out = false;
    if p.mode == 1 {
        if p.bought > 0 {
            have_kas = cont_lead.expect("custody").1;
            (on_d, released, resv) = (0, 0, 0);
        }
        d = plain(p.bought + n, &m);
        next.bought = p.bought + n;
        cust_out = true;
    } else if p.mode == 2 && form == 1 {
        d = recv(n, &m, p.thr);
    } else if p.mode == 2 && form != 0 {
        let (r, k) = cont_lead.expect("receive UTXO");
        d = r.plus(n);
        have_kas = k;
        on_d = 0;
        released = p.ttip;
    }
    let can_continue = left - resv - p.reserve >= quote(p.min_fill, P250 + TIP, SCALE - 1);
    if p.tif == GTC && can_continue {
        if cust_out {
            d = custody(d.amount, BID_COV);
        }
        Plan { d, d_kas: have_kas + on_d + used - all_in, cont: Some((next, left - released)), refund: None }
    } else {
        let rem = v - all_in - released;
        if rem >= REFUND_MIN {
            Plan { d, d_kas: have_kas + on_d, cont: None, refund: Some(rem) }
        } else {
            Plan { d, d_kas: have_kas + rem + on_d, cont: None, refund: None }
        }
    }
}

/// The first token input of a taker's sale.
#[derive(Clone, Copy)]
enum Lead {
    /// the seller's own token leads; tokIn names it
    Seller,
    /// the bid's bought-custody (the bid's `bought`, with this KAS) leads by its covenant id; the seller's token is a delegate
    Custody(i64),
    /// the maker's receive UTXO leads on the borrow path; the seller's token is a delegate
    Recv(St, i64),
}
/// The last outputs: `change` to `to` (the KAS left over; the engine does not look at the fee) and then the refunds of
/// terminal bids, `refunds[i]` belonging to the bid at input i (its slot M - 1 - i).
fn close_with(mut s: Scn, to: &Keypair, refunds: &[(i64, [u8; 32])]) -> Scn {
    let vin: i64 = s.inputs.iter().map(|i| i.entry.amount as i64).sum();
    let vout: i64 = s.outputs.iter().map(|o| o.value as i64).sum::<i64>() + refunds.iter().map(|r| r.0).sum::<i64>();
    s.outputs.push(out(vin - vout, p2pk_spk(&pk(to)), None));
    for (v, m) in refunds.iter().rev() {
        s.outputs.push(out(*v, p2pk_spk(m), None));
    }
    s
}
fn close(s: Scn, to: &Keypair) -> Scn {
    close_with(s, to, &[])
}

/// A taker sells `n` base units (out of 10,000 she holds) into the bid `p` holding `v`: the honest transaction.
/// inputs: 0 bid fill (tokIn 1), 1 token leader, [2 the seller's token as a delegate], funding. outputs: 0 the delivery,
/// [1 the continuation], the seller's change, the taker's KAS, [the refund].
fn sell(f: &Fx, name: &str, p: &BidP, v: i64, n: i64, form: i64, lead: Lead) -> Scn {
    let tk = pk(&f.taker);
    let cont_lead = match lead {
        Lead::Seller => None,
        Lead::Custody(k) => Some((custody(p.bought, BID_COV), k)),
        Lead::Recv(r, k) => Some((r, k)),
    };
    let pl = plan(p, v, n, form, cont_lead);
    let change = plain(10_000 - n, &tk);
    let b = bid(f, p);
    let mut inputs = vec![call(&b, "fill", fill_args(n, 1, form), "bid.fill", v, BID_COV)];
    match lead {
        Lead::Seller => inputs.push(tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[pl.d, change], Wit::P2pk(f.taker))),
        Lead::Custody(k) => {
            inputs.push(tok_leader(&f.t, k, &custody(p.bought, BID_COV), &[pl.d, change], Wit::CovId));
            inputs.push(tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)));
        }
        Lead::Recv(r, k) => {
            inputs.push(tok_leader(&f.t, k, &r, &[pl.d, change], Wit::Borrow));
            inputs.push(tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)));
        }
    }
    inputs.push(p2pk_in(&f.taker, 5 * KAS));
    let mut outputs = vec![tok_out(&f.t, pl.d_kas, &pl.d, 1)];
    if let Some((np, cv)) = pl.cont {
        outputs.push(out(cv, spk_of(&bid(f, &np)), Some((0, BID_COV))));
    }
    outputs.push(tok_out(&f.t, CARRIER, &change, 1));
    let refunds: Vec<(i64, [u8; 32])> = pl.refund.map(|r| (r, p.maker)).into_iter().collect();
    close_with(Scn { name: name.into(), inputs, outputs }, &f.taker, &refunds)
}
/// Replaces the state of token output `o` (and the leader's `next` entry `k` for it).
fn set_tok_out(f: &Fx, s: &mut Scn, o: usize, leader: usize, k: usize, st: St) {
    s.outputs[o].script_public_key = f.t.spk(&st);
    if let Role::TokLeader { next, .. } = &mut s.inputs[leader].role {
        next[k] = st.val();
    }
}
/// Replaces the fill arguments of input 0.
fn set_args(s: &mut Scn, args: Vec<ArtifactValue>) {
    if let Role::Call { args: a, .. } = &mut s.inputs[0].role {
        *a = args;
    }
}
/// Moves `d` sompi from output `from` to output `to`.
fn shift(s: &mut Scn, from: usize, to: usize, d: i64) {
    s.outputs[from].value = (s.outputs[from].value as i64 - d) as u64;
    s.outputs[to].value = (s.outputs[to].value as i64 + d) as u64;
}
fn last(s: &Scn) -> usize {
    s.outputs.len() - 1
}

fn ask(f: &Fx, maker: &Keypair, left: i64) -> SilAbiArtifact {
    let mut a = vec![bytes(&pk(maker))];
    a.extend(f.t.tok_args());
    a.extend([int(SCALE), int(SCALE), int(P240), int(TIP), int(0), int(0), int(EXPIRY), int(REFUND_TIP)]);
    a.extend([int(0), int(0), int(0), int(0), int(0), int(left), bytes(&EXT)]);
    compile_contract(&common::contract_source("KobAsk"), &a, CompileOptions::default()).expect("compile KobAsk")
}
fn settle_args(n: i64, cust_in: usize, rest_out: usize) -> Vec<ArtifactValue> {
    vec![bytes(&n.to_le_bytes()), int(cust_in as i64), int(rest_out as i64), int(0)]
}
/// A matcher crosses the bid `p` (holding `v`) with a resting KobAsk of 10 whole for `n`. `lead` as in `sell`
/// (`Seller` = the ask's custody leads). outputs: 0 delivery, 1 the asker's pay, 2 the bid's continuation, 3 the ask's
/// continuation, 4 the ask's custody rest, then the matcher's KAS.
fn cross(f: &Fx, name: &str, p: &BidP, v: i64, n: i64, form: i64, lead: Lead) -> Scn {
    let (a, a_next) = (ask(f, &f.asker, 10_000), ask(f, &f.asker, 10_000 - n));
    let cust = custody(10_000, ASK_COV);
    let rest = custody(10_000 - n, ASK_COV);
    let cont_lead = match lead {
        Lead::Seller => None,
        Lead::Custody(k) => Some((custody(p.bought, BID_COV), k)),
        Lead::Recv(r, k) => Some((r, k)),
    };
    let pl = plan(p, v, n, form, cont_lead);
    let (np, cv) = pl.cont.expect("the bid continues");
    let b = bid(f, p);
    let mut inputs = vec![call(&b, "fill", fill_args(n, 2, form), "bid.fill", v, BID_COV)];
    match lead {
        Lead::Seller => {
            inputs.push(call(&a, "settle", settle_args(n, 2, 4), "ask.settle", CARRIER, ASK_COV));
            inputs.push(tok_leader(&f.t, CARRIER, &cust, &[pl.d, rest], Wit::CovId));
        }
        Lead::Custody(k) => {
            inputs.push(call(&a, "settle", settle_args(n, 3, 4), "ask.settle", CARRIER, ASK_COV));
            inputs.push(tok_leader(&f.t, k, &custody(p.bought, BID_COV), &[pl.d, rest], Wit::CovId));
            inputs.push(tok_delegate(&f.t, CARRIER, &cust, Wit::CovId));
        }
        Lead::Recv(r, k) => {
            inputs.push(call(&a, "settle", settle_args(n, 3, 4), "ask.settle", CARRIER, ASK_COV));
            inputs.push(tok_leader(&f.t, k, &r, &[pl.d, rest], Wit::Borrow));
            inputs.push(tok_delegate(&f.t, CARRIER, &cust, Wit::CovId));
        }
    }
    inputs.push(p2pk_in(&f.matcher, KAS));
    close(
        Scn {
            name: name.into(),
            inputs,
            outputs: vec![
                tok_out(&f.t, pl.d_kas, &pl.d, 2),
                out(n / SCALE * (P240 - TIP), p2pk_spk(&pk(&f.asker)), None),
                out(cv, spk_of(&bid(f, &np)), Some((0, BID_COV))),
                out(CARRIER, spk_of(&a_next), Some((1, ASK_COV))),
                tok_out(&f.t, CARRIER, &rest, 2),
            ],
        },
        &f.matcher,
    )
}

/// Escrow of a mode-1 bid after a first fill of `w` whole tokens (the carrier left with that fill).
fn v_after_first(w: i64) -> i64 {
    v0() - spend(w) - DC
}

// ================================================================ positives: every mode x form, continuing and ending

/// Every mode and form, continuing and ending, sold by a taker and crossed with a resting ask; ending fills with the
/// remainder riding on the delivery and with the refund slot. Prints the relay fee of each against mode 0.
#[test]
fn modes_fill_positive() {
    for t in tokens() {
        let f = fx(t);
        let m = pk(&f.maker);
        let n = 3_000;
        let r = recv(7_000, &m, THR);
        let p0 = BidP::new(&f.maker, 0);
        let p1 = BidP::new(&f.maker, 1);
        let p2 = BidP::new(&f.maker, 2);
        let v1 = v_after_first(4);

        // mode 0: form is not read (any value)
        let c0 = ok(&f.t, &sell(&f, "PM0a mode 0 continuing fill: new plain UTXO of n", &p0, v0(), n, 0, Lead::Seller));
        ok(&f.t, &sell(&f, "PM0b mode 0 ignores form (form 7)", &p0, v0(), n, 7, Lead::Seller));
        ok(&f.t, &sell(&f, "PM0c mode 0 ending fill: the remainder rides on the delivery", &p0, v0(), 10_000, 0, Lead::Seller));
        ok(&f.t, &sell(&f, "PM0d mode 0 IOC ending fill: the remainder goes to the refund slot", &p0.tif(IOC), v0(), n, 0, Lead::Seller));
        ok(&f.t, &cross(&f, "PM0e mode 0 bid x ask", &p0, v0(), n, 0, Lead::Seller));

        // mode 1
        let c1a = ok(&f.t, &sell(&f, "PM1a mode 1 first fill creates the bought-custody (form ignored: 2)", &p1, v0(), n, 2, Lead::Seller));
        let p1b = p1.bought(4_000);
        let c1b = ok(&f.t, &sell(&f, "PM1b mode 1 later fill continues the bought-custody", &p1b, v1, n, 0, Lead::Custody(DC)));
        let c1c = ok(&f.t, &sell(&f, "PM1c mode 1 ending fill: bought + n to the maker as one plain UTXO", &p1b, v1, 6_000, 0, Lead::Custody(DC)));
        ok(&f.t, &sell(&f, "PM1d mode 1 IOC ending fill with a custody: plain bought + n and the refund slot", &p1b.tif(IOC), v1, n, 0, Lead::Custody(DC)));
        ok(&f.t, &sell(&f, "PM1e mode 1 IOC first fill ends: plain n, no custody", &p1.tif(IOC), v0(), n, 0, Lead::Seller));
        ok(&f.t, &sell(&f, "PM1f mode 1 first fill that buys everything: plain n", &p1, v0(), 10_000, 0, Lead::Seller));
        ok(&f.t, &cross(&f, "PM1g mode 1 bid x ask, first fill", &p1, v0(), n, 0, Lead::Seller));
        ok(&f.t, &cross(&f, "PM1h mode 1 bid x ask, the custody continues", &p1b, v1, n, 0, Lead::Custody(DC)));
        // a custody that carries more KAS than the carrier (anyone may add KAS to it)
        ok(&f.t, &sell(&f, "PM1i mode 1 later fill, custody with 3 KAS", &p1b, v1, n, 0, Lead::Custody(3 * KAS)));

        // mode 2
        let c2a = ok(&f.t, &sell(&f, "PM2a mode 2 form 0: new plain UTXO", &p2, v0(), n, 0, Lead::Seller));
        let c2b = ok(&f.t, &sell(&f, "PM2b mode 2 form 1: new receive UTXO", &p2, v0(), n, 1, Lead::Seller));
        let c2c = ok(&f.t, &sell(&f, "PM2c mode 2 form 2: top-up, the filler keeps topupTip", &p2, v0(), n, 2, Lead::Recv(r, CARRIER)));
        ok(&f.t, &sell(&f, "PM2d mode 2 form 2 ending top-up: the remainder rides on the receive UTXO", &p2, v0(), 10_000, 2, Lead::Recv(r, CARRIER)));
        ok(&f.t, &sell(&f, "PM2e mode 2 form 1 ending fill (IOC): refund slot", &p2.tif(IOC), v0(), n, 1, Lead::Seller));
        ok(&f.t, &sell(&f, "PM2f mode 2 form 2 ending top-up (IOC): refund slot", &p2.tif(IOC), v0(), n, 2, Lead::Recv(r, CARRIER)));
        ok(&f.t, &sell(&f, "PM2g mode 2 form 0 ending fill", &p2, v0(), 10_000, 0, Lead::Seller));
        ok(&f.t, &sell(&f, "PM2h mode 2 form 1 ending fill", &p2, v0(), 10_000, 1, Lead::Seller));
        ok(&f.t, &sell(&f, "PM2i top-up of a receive UTXO with a higher threshold (3,000)", &p2, v0(), 4_000, 2, Lead::Recv(recv(7_000, &m, 3_000), CARRIER)));
        ok(&f.t, &sell(&f, "PM2j top-up of a receive UTXO holding 0 units", &p2, v0(), n, 2, Lead::Recv(recv(0, &m, THR), CARRIER)));
        ok(&f.t, &cross(&f, "PM2k mode 2 bid x ask, top-up", &p2, v0(), n, 2, Lead::Recv(r, CARRIER)));
        ok(&f.t, &cross(&f, "PM2l mode 2 bid x ask, form 1", &p2, v0(), n, 1, Lead::Seller));
        // topupTip 0 and topupTip == deliveryCarrier are both within the bound
        let p2z = BidP { ttip: 0, ..p2 };
        ok(&f.t, &sell(&f, "PM2m top-up with topupTip 0", &p2z, v0(), n, 2, Lead::Recv(r, CARRIER)));
        let p2d = BidP { ttip: DC, ..p2 };
        ok(&f.t, &sell(&f, "PM2n top-up with topupTip == deliveryCarrier", &p2d, v0(), n, 2, Lead::Recv(r, CARRIER)));

        println!(
            "FEE @{}: taker sell of 3 whole, relay floor: mode 0 {} | mode 1 first {} ({:+}) later {} ({:+}) ending {} ({:+}) | mode 2 form 0 {} ({:+}) form 1 {} ({:+}) top-up {} ({:+}) sompi",
            f.t.name,
            c0.fee,
            c1a.fee,
            c1a.fee as i64 - c0.fee as i64,
            c1b.fee,
            c1b.fee as i64 - c0.fee as i64,
            c1c.fee,
            c1c.fee as i64 - c0.fee as i64,
            c2a.fee,
            c2a.fee as i64 - c0.fee as i64,
            c2b.fee,
            c2b.fee as i64 - c0.fee as i64,
            c2c.fee,
            c2c.fee as i64 - c0.fee as i64
        );
    }
}

/// The life of a mode-1 order: three fills, the custody created, continued and released (the escrow's KAS accounted).
#[test]
fn modes_accumulate_lifecycle() {
    for t in tokens() {
        let f = fx(t);
        let p = BidP::new(&f.maker, 1);
        let s1 = sell(&f, "PL1 fill 1 (4 whole): the custody is created", &p, v0(), 4_000, 0, Lead::Seller);
        ok(&f.t, &s1);
        assert_eq!(s1.outputs[0].script_public_key, f.t.spk(&custody(4_000, BID_COV)));
        let v1 = s1.outputs[1].value as i64;
        assert_eq!(v1, v_after_first(4));
        let p4 = p.bought(4_000);
        assert_eq!(s1.outputs[1].script_public_key, spk_of(&bid(&f, &p4)));
        let s2 = sell(&f, "PL2 fill 2 (3 whole): the custody continues with 7,000, no carrier leaves", &p4, v1, 3_000, 0, Lead::Custody(DC));
        ok(&f.t, &s2);
        let v2 = s2.outputs[1].value as i64;
        assert_eq!(v2, v1 - spend(3));
        let s3 = sell(&f, "PL3 fill 3 (3 whole) ends the order: 10,000 to the maker", &p.bought(7_000), v2, 3_000, 0, Lead::Custody(DC));
        ok(&f.t, &s3);
        assert_eq!(s3.outputs[0].script_public_key, f.t.spk(&plain(10_000, &pk(&f.maker))));
    }
}

// ================================================================ negatives of the fill

/// What the fill refuses: the modes do not leak into each other, the form, the hostile state fields, the bought-custody
/// and the receive UTXO. Every scenario is refused by the bid alone (the token program and the other inputs accept).
#[test]
fn modes_fill_negatives() {
    for t in tokens() {
        let f = fx(t);
        let (m, tk, o) = (pk(&f.maker), pk(&f.taker), pk(&f.maker2));
        let n = 3_000;
        let r = recv(7_000, &m, THR);
        let p0 = BidP::new(&f.maker, 0);
        let p1 = BidP::new(&f.maker, 1);
        let p2 = BidP::new(&f.maker, 2);
        let v1 = v_after_first(4);
        let p1b = p1.bought(4_000);

        // ---- the modes do not leak (a filler cannot move an order to another mode)
        // NM01: a mode-0 order's fill delivering into a custody of the order (mode 1's shape)
        let mut s = sell(&f, "NM01 mode 0 order, custody delivery", &p0, v0(), n, 0, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, custody(n, BID_COV));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM02 mode 0 order, receive-state delivery (form 1)", &p0, v0(), n, 1, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, recv(n, &m, THR));
        bad_only(&f.t, &s, 0);
        // the mode-2 shape of a top-up on a mode-0 order (no carrier taken, the filler keeps topupTip)
        let mut s = sell(&f, "NM03 mode 0 order, top-up of the maker's receive UTXO (form 2)", &p2, v0(), n, 2, Lead::Recv(r, CARRIER));
        s.inputs[0] = call(&bid(&f, &p0), "fill", fill_args(n, 1, 2), "bid.fill", v0(), BID_COV);
        s.outputs[1].script_public_key = spk_of(&bid(&f, &p0));
        bad_only(&f.t, &s, 0);
        // the same two on a mode-0 order whose state carries mode 2's fields anyway (placement does not check them on chain)
        let p0x = BidP { thr: THR, ttip: TT, ..p0 };
        let mut s = sell(&f, "NM02b mode 0 order with recvThreshold set, receive-state delivery (form 1)", &p0x, v0(), n, 1, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, recv(n, &m, THR));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM03b mode 0 order with recvThreshold set, top-up (form 2)", &p2, v0(), n, 2, Lead::Recv(r, CARRIER));
        s.inputs[0] = call(&bid(&f, &p0x), "fill", fill_args(n, 1, 2), "bid.fill", v0(), BID_COV);
        s.outputs[1].script_public_key = spk_of(&bid(&f, &p0x));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM04 mode 1 first fill delivers n to the maker's key", &p1, v0(), n, 0, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, plain(n, &m));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM05 mode 1 order, top-up form into a receive UTXO", &p2, v0(), n, 2, Lead::Recv(r, CARRIER));
        s.inputs[0] = call(&bid(&f, &p1), "fill", fill_args(n, 1, 2), "bid.fill", v0(), BID_COV);
        s.outputs[1].script_public_key = spk_of(&bid(&f, &p1));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM06 mode 2 order, custody delivery", &p2, v0(), n, 0, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, custody(n, BID_COV));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM07 mode 2 top-up taking 1 sompi more than topupTip", &p2, v0(), n, 2, Lead::Recv(r, CARRIER));
        shift(&mut s, 1, 3, 1);
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NM07b mode 2 form 1 continuation 1 sompi short (more than the carrier taken)", &p2, v0(), n, 1, Lead::Seller);
        shift(&mut s, 1, 3, 1);
        bad_only(&f.t, &s, 0);

        // ---- form (mode 2 reads it; 0, 1, 2 only)
        let mut s = sell(&f, "NF01 mode 2 form 3 (unknown) delivering as a top-up", &p2, v0(), n, 2, Lead::Recv(r, CARRIER));
        set_args(&mut s, fill_args(n, 1, 3));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NF02 mode 2 form -1 delivering as a top-up", &p2, v0(), n, 2, Lead::Recv(r, CARRIER));
        set_args(&mut s, fill_args(n, 1, -1));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NF03 mode 2 form 0 delivering a receive state", &p2, v0(), n, 1, Lead::Seller);
        set_args(&mut s, fill_args(n, 1, 0));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NF04 mode 2 form 1 delivering a plain state", &p2, v0(), n, 0, Lead::Seller);
        set_args(&mut s, fill_args(n, 1, 1));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NF05 mode 2 form 1 receive UTXO with a lower threshold than recvThreshold", &p2, v0(), n, 1, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, recv(n, &m, 10));
        bad_only(&f.t, &s, 0);
        let mut s = sell(&f, "NF06 mode 2 form 1 receive UTXO for another key", &p2, v0(), n, 1, Lead::Seller);
        set_tok_out(&f, &mut s, 0, 1, 0, recv(n, &o, THR));
        bad_only(&f.t, &s, 0);

        // ---- hostile state fields (nothing checks the state at placement)
        let pm3 = BidP { mode: 3, ..p0 };
        bad_only(&f.t, &sell(&f, "NS01 mode 3 order, plain delivery", &pm3, v0(), n, 0, Lead::Seller), 0);
        let pmn = BidP { mode: -1, ..p0 };
        bad_only(&f.t, &sell(&f, "NS02 mode -1 order, plain delivery", &pmn, v0(), n, 0, Lead::Seller), 0);
        let p0b = p0.bought(5);
        bad_only(&f.t, &sell(&f, "NS03 mode 0 order with bought 5, plain delivery", &p0b, v0(), n, 0, Lead::Seller), 0);
        let p2b = p2.bought(5);
        bad_only(&f.t, &sell(&f, "NS04 mode 2 order with bought 5, form 1", &p2b, v0(), n, 1, Lead::Seller), 0);
        let p1n = p1.bought(-5);
        let mut s = sell(&f, "NS05 mode 1 order with bought -5: first fill delivers a custody of n - 5", &p1, v0(), n, 0, Lead::Seller);
        s.inputs[0] = call(&bid(&f, &p1n), "fill", fill_args(n, 1, 0), "bid.fill", v0(), BID_COV);
        set_tok_out(&f, &mut s, 0, 1, 0, custody(n - 5, BID_COV));
        s.outputs[1].script_public_key = spk_of(&bid(&f, &p1n.bought(n - 5)));
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n + 5, &tk));
        bad_only(&f.t, &s, 0);
        let p2t0 = BidP { thr: 0, ..p2 };
        bad_only(&f.t, &sell(&f, "NS06 mode 2 recvThreshold 0, form 1 (a receive UTXO of threshold 0)", &p2t0, v0(), n, 1, Lead::Seller), 0);
        bad_only(
            &f.t,
            &sell(&f, "NS07 mode 2 recvThreshold 0, top-up of a threshold-10 receive UTXO", &p2t0, v0(), n, 2, Lead::Recv(recv(7_000, &m, 10), CARRIER)),
            0,
        );
        let p2big = BidP { ttip: DC + 1, ..p2 };
        bad_only(&f.t, &sell(&f, "NS08 mode 2 topupTip = deliveryCarrier + 1, top-up", &p2big, v0(), n, 2, Lead::Recv(r, CARRIER)), 0);

        // ---- the bought-custody (mode 1, bought > 0)
        let later = |name: &str| sell(&f, name, &p1b, v1, n, 0, Lead::Custody(DC));
        let mut s = later("NC01 custody continues with bought + n - 1");
        set_tok_out(&f, &mut s, 0, 1, 0, custody(4_000 + n - 1, BID_COV));
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n + 1, &tk));
        bad_only(&f.t, &s, 0);
        // a 1-unit UTXO of the order's id stands in for the custody (the real one would be stranded)
        let mut s = later("NC02 a 1-unit UTXO of the order's id named as the custody");
        s.inputs[1] = tok_leader(&f.t, DC, &custody(1, BID_COV), &[custody(4_000 + n, BID_COV), plain(10_000 - n - 3_999, &tk)], Wit::CovId);
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n - 3_999, &tk));
        bad_only(&f.t, &s, 0);
        let mut s = later("NC03 continuing fill delivers bought + n to the maker's key");
        set_tok_out(&f, &mut s, 0, 1, 0, plain(4_000 + n, &m));
        bad_only(&f.t, &s, 0);
        // a later fill that creates a second custody of n and leaves the first one out: tokIn names the seller's token
        let s = close(
            Scn {
                name: "NC04 later fill creates a second custody of n, the first stays out".into(),
                inputs: vec![
                    call(&bid(&f, &p1b), "fill", fill_args(n, 1, 0), "bid.fill", v1, BID_COV),
                    tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[custody(n, BID_COV), plain(10_000 - n, &tk)], Wit::P2pk(f.taker)),
                    p2pk_in(&f.taker, 2 * KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, DC, &custody(n, BID_COV), 1),
                    out(v1 - spend(3), spk_of(&bid(&f, &p1b.bought(4_000 + n))), Some((0, BID_COV))),
                    tok_out(&f.t, CARRIER, &plain(10_000 - n, &tk), 1),
                ],
            },
            &f.taker,
        );
        bad_only(&f.t, &s, 0);
        let mut s = later("NC05 tokIn names the seller's token while the custody is spent");
        set_args(&mut s, fill_args(n, 2, 0));
        bad_only(&f.t, &s, 0);
        let mut s = later("NC06 continuation with bought unchanged");
        s.outputs[1].script_public_key = spk_of(&bid(&f, &p1b));
        bad_only(&f.t, &s, 0);
        let mut s = later("NC07 custody continues with 1 sompi less KAS");
        shift(&mut s, 0, 3, 1);
        bad_only(&f.t, &s, 0);
        let mut s = later("NC08 later fill takes the carrier from the escrow (continuation short by the carrier)");
        shift(&mut s, 1, 3, DC);
        bad_only(&f.t, &s, 0);
        let end = |name: &str| sell(&f, name, &p1b, v1, 6_000, 0, Lead::Custody(DC));
        let mut s = end("NC09 ending fill releases the custody to another key");
        set_tok_out(&f, &mut s, 0, 1, 0, plain(10_000, &o));
        bad_only(&f.t, &s, 0);
        let mut s = end("NC10 ending fill releases bought + n - 1");
        set_tok_out(&f, &mut s, 0, 1, 0, plain(9_999, &m));
        set_tok_out(&f, &mut s, 1, 1, 1, plain(4_001, &tk));
        bad_only(&f.t, &s, 0);
        let mut s = end("NC11 ending fill keeps the tokens in a custody of the dead order");
        set_tok_out(&f, &mut s, 0, 1, 0, custody(10_000, BID_COV));
        bad_only(&f.t, &s, 0);
        let mut s = end("NC12 ending fill releases the custody with 1 sompi less KAS");
        shift(&mut s, 0, 2, 1);
        bad_only(&f.t, &s, 0);
        let mut s = later("NC13 custody continues borrow-enabled");
        set_tok_out(&f, &mut s, 0, 1, 0, custody(4_000 + n, BID_COV).with(|s| s.borrow = 1));
        bad_only(&f.t, &s, 0);
        let mut s = later("NC14 custody continues under another commitment");
        set_tok_out(&f, &mut s, 0, 1, 0, custody(4_000 + n, BID_COV).with(|s| s.ext = EXT2));
        bad(&f.t, &s, 0);
        // a custody of the order's id whose state differs from the order's custody (borrow-enabled): not the custody
        let cb = custody(4_000, BID_COV).with(|s| s.borrow = 1);
        let mut s = later("NC15 a borrow-enabled UTXO of the order's id named as the custody");
        s.inputs[1] = tok_leader(&f.t, DC, &cb, &[custody(4_000 + n, BID_COV), plain(10_000 - n, &tk)], Wit::CovId);
        bad_only(&f.t, &s, 0);
        // a dust fill (n < minFill) that leaves buying power: with a custody no carrier is reserved, so the half carrier
        // left after the fill still buys about 200 base units, and the fill does not take all the buying power
        let vd = quote(500, P250 + TIP, SCALE - 1) + DC / 2;
        let mut s = sell(&f, "NC16 a 500-unit fill (minFill 1,000) leaves half a carrier of buying power", &p1b.tif(IOC), vd, 500, 0, Lead::Custody(DC));
        s.inputs[0] = call(&bid(&f, &p1b), "fill", fill_args(500, 1, 0), "bid.fill", vd, BID_COV);
        bad_only(&f.t, &s, 0);
        // the seller keeps the bought tokens: the custody continues with n, the order with bought = n
        let mut s = later("NC17 later fill continues the custody with n only, the seller keeps the 4,000 bought");
        set_tok_out(&f, &mut s, 0, 1, 0, custody(n, BID_COV));
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n + 4_000, &tk));
        s.outputs[1].script_public_key = spk_of(&bid(&f, &p1b.bought(n)));
        bad_only(&f.t, &s, 0);
        // a terminal fill with a refund slot: the released custody keeps its KAS
        let mut s = sell(&f, "NC18 IOC ending fill with a refund: the custody released with 1 sompi less KAS", &p1b.tif(IOC), v1, n, 0, Lead::Custody(DC));
        shift(&mut s, 0, 2, 1);
        bad_only(&f.t, &s, 0);

        // ---- the receive UTXO of a top-up (mode 2 form 2)
        let top = |name: &str, rr: St| sell(&f, name, &p2, v0(), n, 2, Lead::Recv(rr, CARRIER));
        let mut s = top("NT01 top-up delivers have + n - 1", r);
        set_tok_out(&f, &mut s, 0, 1, 0, r.plus(n - 1));
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n + 1, &tk));
        bad_only(&f.t, &s, 0);
        bad_only(&f.t, &top("NT02 top-up into another key's receive UTXO", recv(7_000, &o, THR)), 0);
        bad_only(&f.t, &top("NT03 top-up into the maker's UTXO with threshold 10 < recvThreshold", recv(7_000, &m, 10)), 0);
        bad_only(&f.t, &top("NT04 top-up into a UTXO whose guard has bytes after the threshold", r.with(|s| s.guard[20] = 1)), 0);
        bad(&f.t, &top("NT05 top-up into a receive UTXO of another commitment", r.with(|s| s.ext = EXT2)), 0);
        let mut s = top("NT06 successor carries 1 sompi less than the receive UTXO", r);
        shift(&mut s, 0, 3, 1);
        bad(&f.t, &s, 0);
        let mut s = top("NT07 tokIn names the taker's token, not the receive UTXO", r);
        set_args(&mut s, fill_args(n, 2, 2));
        bad_only(&f.t, &s, 0);
        // the receive UTXO named by the top-up is a plain UTXO of the maker (the token program refuses the borrow path too)
        bad(&f.t, &top("NT08 top-up naming a plain UTXO of the maker", plain(7_000, &m)), 0);
        // a top-up whose delivery changes the receive UTXO's state bytes: borrow disabled / threshold raised
        let mut s = top("NT09 top-up delivers the receive UTXO with borrowing disabled", r);
        set_tok_out(&f, &mut s, 0, 1, 0, plain(7_000 + n, &m));
        bad(&f.t, &s, 0);
        // the filler takes the full carrier instead of topupTip (a top-up moves no carrier)
        let mut s = top("NT11 top-up continuation short by deliveryCarrier - topupTip (the filler takes the carrier)", r);
        shift(&mut s, 1, 3, DC - TT);
        bad_only(&f.t, &s, 0);
        // look-alike receive state: a non-P2SH input of the token's id pushes a 0-unit receive state while the real one leads
        let b2 = bid(&f, &p2);
        let real = recv(1_000, &m, THR);
        let fake = Inp {
            entry: UtxoEntry::new(KAS as u64, ScriptPublicKey::new(0, vec![0x75, 0x51].into()), 1_000, false, Some(TOKEN_COV)),
            role: Role::RawSs { ss: push(&f.t.redeem(&recv(0, &m, THR))), name: "lookalike" },
        };
        let s = close(
            Scn {
                name: "NT10 look-alike receive state (0 units) while the real receive UTXO (1,000) leads".into(),
                inputs: vec![
                    call(&b2, "fill", fill_args(5_000, 2, 2), "bid.fill", v0(), BID_COV),
                    tok_leader(&f.t, CARRIER, &real, &[recv(5_000, &m, THR), plain(6_000, &tk)], Wit::Borrow),
                    fake,
                    tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                    p2pk_in(&f.taker, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, CARRIER, &recv(5_000, &m, THR), 1),
                    out(v0() - spend(5) - TT, spk_of(&b2), Some((0, BID_COV))),
                    tok_out(&f.t, CARRIER, &plain(6_000, &tk), 1),
                ],
            },
            &f.taker,
        );
        if f.t.contract.is_none() {
            bad_only(&f.t, &s, 0);
        }
    }
}

// ================================================================ stray guard (spec section 5)

/// The fill refuses every token input owned by the order's id but the bought-custody it names and authenticates.
#[test]
fn modes_stray_guard() {
    for t in tokens() {
        let f = fx(t);
        let tk = pk(&f.taker);
        let n = 3_000;
        let p1b = BidP::new(&f.maker, 1).bought(4_000);
        let v1 = v_after_first(4);
        // a stray of the order's id next to the custody, as a third token input
        let mut s = sell(&f, "NG01 a stray of the order's id next to the custody", &p1b, v1, n, 0, Lead::Custody(DC));
        s.inputs.insert(3, tok_delegate(&f.t, CARRIER, &custody(5, BID_COV), Wit::CovId));
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n + 5, &tk));
        s.outputs[3].value += CARRIER as u64;
        bad_only(&f.t, &s, 0);
        // with bought == 0 nothing is excluded: a stray named as tokIn of a first fill (every mode) rides to the seller
        for (id, mode, form) in [("NG02", 1, 0), ("NG03", 0, 0), ("NG04", 2, 0), ("NG05", 2, 1)] {
            let p = BidP::new(&f.maker, mode);
            let pl = plan(&p, v0(), n, form, None);
            let (np, cv) = pl.cont.unwrap();
            let s = close(
                Scn {
                    name: format!("{id} mode {mode} form {form}, bought 0: a stray of the order's id named as tokIn"),
                    inputs: vec![
                        call(&bid(&f, &p), "fill", fill_args(n, 1, form), "bid.fill", v0(), BID_COV),
                        tok_leader(&f.t, CARRIER, &custody(5, BID_COV), &[pl.d, plain(10_000 - n + 5, &tk)], Wit::CovId),
                        tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                        p2pk_in(&f.taker, 2 * KAS),
                    ],
                    outputs: vec![
                        tok_out(&f.t, pl.d_kas, &pl.d, 1),
                        out(cv, spk_of(&bid(&f, &np)), Some((0, BID_COV))),
                        tok_out(&f.t, CARRIER, &plain(10_000 - n + 5, &tk), 1),
                    ],
                },
                &f.taker,
            );
            bad_only(&f.t, &s, 0);
        }
        // a top-up with a stray of the order's id next to the receive UTXO
        let r = recv(7_000, &pk(&f.maker), THR);
        let mut s = sell(&f, "NG06 top-up with a stray of the order's id as a delegate", &BidP::new(&f.maker, 2), v0(), n, 2, Lead::Recv(r, CARRIER));
        s.inputs.insert(3, tok_delegate(&f.t, CARRIER, &custody(5, BID_COV), Wit::CovId));
        set_tok_out(&f, &mut s, 2, 1, 1, plain(10_000 - n + 5, &tk));
        s.outputs[3].value += CARRIER as u64;
        bad_only(&f.t, &s, 0);
    }
}

/// The scan per slot on the 8 / 8 program: a stray at token slot k (0..7) next to the custody at another slot.
#[test]
fn modes_stray_guard_slots_8x8() {
    let f = fx(Token::named("KCC20Ref_8x8"));
    let tk = pk(&f.taker);
    let n = 1_000;
    let p1b = BidP::new(&f.maker, 1).bought(4_000);
    let v1 = v_after_first(4);
    let b = bid(&f, &p1b);
    let nb = bid(&f, &p1b.bought(4_000 + n));
    for k in 0..8usize {
        let slots = (k + 1).max(3);
        let c = if k == 0 { 1 } else { 0 };
        let sellers = slots - 2;
        let change = plain(2_000 * sellers as i64 + 5 - n, &tk);
        let d = custody(4_000 + n, BID_COV);
        let st = |j: usize| {
            if j == c {
                (custody(4_000, BID_COV), DC, Wit::CovId)
            } else if j == k {
                (custody(5, BID_COV), CARRIER, Wit::CovId)
            } else {
                (plain(2_000, &tk), CARRIER, Wit::P2pk(f.taker))
            }
        };
        let mut inputs = vec![call(&b, "fill", fill_args(n, 1 + c, 0), "bid.fill", v1, BID_COV)];
        for j in 0..slots {
            let (s, v, w) = st(j);
            inputs.push(if j == 0 { tok_leader(&f.t, v, &s, &[d, change], w) } else { tok_delegate(&f.t, v, &s, w) });
        }
        inputs.push(p2pk_in(&f.taker, 2 * KAS));
        let s = close(
            Scn {
                name: format!("NSS{k} a stray of the order's id at token slot {k}, the custody at slot {c}"),
                inputs,
                outputs: vec![
                    tok_out(&f.t, DC, &d, 1),
                    out(v1 - spend(1), spk_of(&nb), Some((0, BID_COV))),
                    tok_out(&f.t, CARRIER, &change, 1),
                ],
            },
            &f.taker,
        );
        bad_only(&f.t, &s, 0);
        // the same transaction with the stray replaced by a seller UTXO passes
        if k >= 2 {
            let mut s = s.clone();
            s.name = format!("PSS{k} the same shape without the stray (slot {k} a seller UTXO)");
            s.inputs[1 + k] = tok_delegate(&f.t, CARRIER, &plain(2_000, &tk), Wit::P2pk(f.taker));
            let change2 = plain(2_000 * (sellers as i64 + 1) - n, &tk);
            set_tok_out(&f, &mut s, 2, 1, 1, change2);
            ok(&f.t, &s);
        }
    }
}

// ================================================================ the order's end: refund, cancel and the claim shapes

/// refund(): with bought > 0 the bought-custody goes whole, with its KAS, to the maker's key at its own index; the
/// claim shapes CL1..CL4 of section 7 through `cancel`.
#[test]
fn modes_refund_cancel_claim() {
    for t in tokens() {
        let f = fx(t);
        let (m, tk, o) = (pk(&f.maker), pk(&f.taker), pk(&f.maker2));
        let v1 = v_after_first(4);
        let have = custody(4_000, BID_COV);
        let p1b = BidP::new(&f.maker, 1).bought(4_000);
        let due = bid(&f, &p1b.expiry(1_000));
        // refund after expiry (anyone): the escrow at output 0, the custody at its own index (input 1)
        let refund = |name: &str, cust: Option<(St, St, i64)>| {
            let mut s = Scn {
                name: name.into(),
                inputs: vec![call(&due, "refund", vec![], "bid.refund", v1, BID_COV)],
                outputs: vec![out(v1, p2pk_spk(&m), None)],
            };
            match cust {
                Some((c, to, dv)) => {
                    s.inputs.push(tok_leader(&f.t, DC, &c, &[to], Wit::CovId));
                    s.outputs.push(tok_out(&f.t, dv, &to, 1));
                }
                None => {
                    s.inputs.push(p2pk_in(&f.taker, KAS));
                    s.outputs.push(out(KAS, p2pk_spk(&tk), None));
                }
            }
            s
        };
        ok(&f.t, &refund("PR01 refund: the escrow and the whole custody back to the maker", Some((have, plain(4_000, &m), DC))));
        let mut s = refund("PR02 refund: the custody with more KAS than it had", Some((have, plain(4_000, &m), DC + 5)));
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(out(KAS - 5, p2pk_spk(&tk), None));
        ok(&f.t, &s);
        bad(&f.t, &refund("NR01 refund that leaves the custody behind", None), 0);
        bad_only(&f.t, &refund("NR02 refund that sends the custody to another key", Some((have, plain(4_000, &o), DC))), 0);
        bad_only(&f.t, &refund("NR03 refund that skims the custody's KAS", Some((have, plain(4_000, &m), DC - 1))), 0);
        bad_only(&f.t, &refund("NR04 refund that keeps the custody owned by the dead order", Some((have, have, DC))), 0);
        bad_only(&f.t, &refund("NR05 refund that releases the custody borrow-enabled", Some((have, recv(4_000, &m, THR), DC))), 0);
        // the custody input is not the order's custody (a 1-unit UTXO of the order's id)
        bad_only(&f.t, &refund("NR06 refund naming a 1-unit UTXO of the order's id as the custody", Some((custody(1, BID_COV), plain(1, &m), DC))), 0);
        // one unit of the custody to another key
        let mut s = refund("NR07 refund that releases bought - 1 to the maker and 1 unit to another key", Some((have, plain(3_999, &m), DC)));
        if let Role::TokLeader { next, .. } = &mut s.inputs[1].role {
            next.push(plain(1, &o).val());
        }
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(tok_out(&f.t, KAS, &plain(1, &o), 1));
        bad_only(&f.t, &s, 0);
        // a stray of the order's id next to the custody: it would ride along (only the custody may be a token input)
        let mut s = refund("NR08 refund with a stray of the order's id next to the custody", Some((have, plain(4_000, &m), DC)));
        s.inputs.push(tok_delegate(&f.t, CARRIER, &custody(5, BID_COV), Wit::CovId));
        if let Role::TokLeader { next, .. } = &mut s.inputs[1].role {
            next.push(plain(5, &tk).val());
        }
        s.outputs.push(tok_out(&f.t, CARRIER, &plain(5, &tk), 1));
        bad_only(&f.t, &s, 0);
        // the release off the custody's own index (output 2, a KAS output at 1)
        let mut s = refund("NR09 refund with the custody's release off its own index", Some((have, plain(4_000, &m), DC)));
        s.outputs.insert(1, out(KAS, p2pk_spk(&tk), None));
        s.inputs.push(p2pk_in(&f.taker, KAS));
        bad_only(&f.t, &s, 0);
        // a 10,000-unit stray of the order's id named as the custody (bought 4,000): 4,000 to the maker, 6,000 to another key
        let mut s = refund("NR12 refund naming a 10,000-unit stray of the order's id as the custody", Some((custody(10_000, BID_COV), plain(4_000, &m), DC)));
        if let Role::TokLeader { next, .. } = &mut s.inputs[1].role {
            next.push(plain(6_000, &o).val());
        }
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(tok_out(&f.t, KAS, &plain(6_000, &o), 1));
        bad_only(&f.t, &s, 0);
        // output 1 carries the release's script but is not bound to the token: the tokens go to another key
        let mut s = refund("NR13 refund whose output 1 is an unbound copy of the release", Some((have, plain(4_000, &o), DC)));
        s.outputs.insert(1, out(DC, f.t.spk(&plain(4_000, &m)), None));
        s.outputs[2].covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: TOKEN_COV });
        s.inputs.push(p2pk_in(&f.taker, DC));
        bad_only(&f.t, &s, 0);
        // before expiry
        let mut s = refund("NR10 refund of an unexpired order", Some((have, plain(4_000, &m), DC)));
        s.inputs[0] = call(&bid(&f, &p1b), "refund", vec![], "bid.refund", v1, BID_COV);
        bad_only(&f.t, &s, 0);
        // bought == 0 (any mode): as before, no token input
        for (id, mode) in [("PR03", 0), ("PR04", 1), ("PR05", 2)] {
            let b = bid(&f, &BidP::new(&f.maker, mode).expiry(1_000));
            let s = Scn {
                name: format!("{id} refund of a mode-{mode} order holding no custody"),
                inputs: vec![call(&b, "refund", vec![], "bid.refund", v0(), BID_COV), p2pk_in(&f.taker, KAS)],
                outputs: vec![out(v0() - REFUND_TIP, p2pk_spk(&m), None), out(KAS + REFUND_TIP, p2pk_spk(&tk), None)],
            };
            ok(&f.t, &s);
        }
        let b = bid(&f, &BidP::new(&f.maker, 1).expiry(1_000));
        let s = Scn {
            name: "NR11 refund of a mode-1 order holding no custody, with a stray of its id".into(),
            inputs: vec![
                call(&b, "refund", vec![], "bid.refund", v0(), BID_COV),
                tok_leader(&f.t, CARRIER, &custody(5, BID_COV), &[plain(5, &tk)], Wit::CovId),
            ],
            outputs: vec![out(v0(), p2pk_spk(&m), None), tok_out(&f.t, CARRIER, &plain(5, &tk), 1)],
        };
        bad_only(&f.t, &s, 0);

        // ---- the claim shapes of section 7: the maker's SIGHASH_ALL signature on `cancel`
        let b1 = bid(&f, &p1b);
        let fresh = bid(&f, &p1b.bought(0));
        // CL1 claim: the order continues with bought = 0 and one more carrier; the custody to the maker as one plain UTXO
        let cl1 = |kp: &Keypair, name: &str| {
            close(
                Scn {
                    name: name.into(),
                    inputs: vec![cancel_in(&b1, v1, BID_COV, kp), tok_leader(&f.t, DC, &have, &[plain(4_000, &m)], Wit::CovId), p2pk_in(&f.maker, 2 * KAS)],
                    outputs: vec![out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))), tok_out(&f.t, DC, &plain(4_000, &m), 1)],
                },
                &f.maker,
            )
        };
        ok(&f.t, &cl1(&f.maker, "PCL1 claim: custody to the maker, the order continues with bought = 0"));
        bad(&f.t, &cl1(&f.taker, "NCL1 claim signed by another key"), 0);
        // the claimed order then fills as a fresh one: its first fill creates a new custody with the added carrier
        ok(&f.t, &sell(&f, "PCL1b first fill of the claimed order", &p1b.bought(0), v1 + DC, 3_000, 0, Lead::Seller));
        // CL2 claim and merge: custody + 2 wallet UTXOs -> one plain UTXO
        let cl2 = |kp: &Keypair, name: &str| {
            close(
                Scn {
                    name: name.into(),
                    inputs: vec![
                        cancel_in(&b1, v1, BID_COV, kp),
                        tok_leader(&f.t, DC, &have, &[plain(7_000, &m)], Wit::CovId),
                        tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                        tok_delegate(&f.t, CARRIER, &plain(2_000, &m), Wit::P2pk(f.maker)),
                        p2pk_in(&f.maker, 2 * KAS),
                    ],
                    outputs: vec![out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))), tok_out(&f.t, 3 * CARRIER, &plain(7_000, &m), 1)],
                },
                &f.maker,
            )
        };
        ok(&f.t, &cl2(&f.maker, "PCL2 claim and merge with 2 wallet UTXOs into one plain UTXO"));
        bad(&f.t, &cl2(&f.taker, "NCL2 claim and merge signed by another key"), 0);
        // CL3 claim and place: the order ends; custody + 1 wallet UTXO become the custody of a new KobAsk, change to the maker
        let ask45 = ask(&f, &f.maker, 4_500);
        let cl3 = |kp: &Keypair, name: &str, cont: bool| {
            let mut ask_out = out(CARRIER, spk_of(&ask45), None);
            let ask_at = if cont { 1 } else { 0 };
            let id = genesis_id(3, ask_at, &ask_out);
            ask_out.covenant = Some(CovenantBinding { authorizing_input: 3, covenant_id: id });
            let cust = custody(4_500, id);
            let mut outputs = vec![];
            if cont {
                outputs.push(out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))));
            }
            outputs.extend([ask_out, tok_out(&f.t, CARRIER, &cust, 1), tok_out(&f.t, CARRIER, &plain(500, &m), 1)]);
            if !cont {
                outputs.push(out(v1, p2pk_spk(&m), None));
            }
            close(
                Scn {
                    name: name.into(),
                    inputs: vec![
                        cancel_in(&b1, v1, BID_COV, kp),
                        tok_leader(&f.t, DC, &have, &[cust, plain(500, &m)], Wit::CovId),
                        tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                        p2pk_in(&f.maker, 4 * KAS),
                    ],
                    outputs,
                },
                &f.maker,
            )
        };
        ok(&f.t, &cl3(&f.maker, "PCL3a claim and place, the order continues: a KobAsk for 4,500 and 500 change", true));
        ok(&f.t, &cl3(&f.maker, "PCL3b claim and place, the escrow back to the maker: a KobAsk for 4,500 and 500 change", false));
        bad(&f.t, &cl3(&f.taker, "NCL3 claim and place signed by another key", true), 0);
        // CL4 cancel: escrow and custody to the maker
        let cl4 = |kp: &Keypair, name: &str| Scn {
            name: name.into(),
            inputs: vec![cancel_in(&b1, v1, BID_COV, kp), tok_leader(&f.t, DC, &have, &[plain(4_000, &m)], Wit::CovId)],
            outputs: vec![out(v1, p2pk_spk(&m), None), tok_out(&f.t, DC, &plain(4_000, &m), 1)],
        };
        ok(&f.t, &cl4(&f.maker, "PCL4 cancel: the maker takes the escrow and the custody"));
        bad(&f.t, &cl4(&f.taker, "NCL4 cancel signed by another key"), 0);
        // the 3 / 3 bound of CL2: custody + 3 wallet UTXOs is a fourth token input
        let s = close(
            Scn {
                name: "NCL5 claim + 3 wallet UTXOs (4 token inputs on a 3 / 3 program)".into(),
                inputs: vec![
                    cancel_in(&b1, v1, BID_COV, &f.maker),
                    tok_leader(&f.t, DC, &have, &[plain(8_000, &m)], Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    tok_delegate(&f.t, CARRIER, &plain(2_000, &m), Wit::P2pk(f.maker)),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    p2pk_in(&f.maker, 2 * KAS),
                ],
                outputs: vec![out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))), tok_out(&f.t, 4 * CARRIER, &plain(8_000, &m), 1)],
            },
            &f.maker,
        );
        bad_only(&f.t, &s, 1);
    }
}
/// The consensus genesis id of the one-output group `out` authorised by input `auth` (outpoints in build() are ([k+1; 32], k)).
fn genesis_id(auth: usize, out_idx: usize, o: &TransactionOutput) -> Hash {
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes([auth as u8 + 1; 32]), index: auth as u32 };
    kaspa_consensus_core::hashing::covenant_id::covenant_id(op, [(out_idx as u32, o)].into_iter())
}

// ================================================================ ordering rules and slots (spec section 6)

/// What the token program enforces around a top-up, with every order accepting: the receive UTXO leads, the topped-up
/// order is the token's lowest-index delivering order, one top-up per token, a gain above the threshold. A refund-carrying
/// terminal bid is the only one that may top up. The slot budget of a 3 / 3 program.
#[test]
fn modes_ordering_and_slots() {
    for t in tokens() {
        let f = fx(t);
        let (m, m2, tk) = (pk(&f.maker), pk(&f.maker2), pk(&f.taker));
        let n = 3_000;
        let r = recv(7_000, &m, THR);
        let p2 = BidP::new(&f.maker, 2);
        let b2 = bid(&f, &p2);
        let v = v0();

        // O1 the receive UTXO is a delegate (another input leads): the borrow path is the leader's only
        let s = close(
            Scn {
                name: "NO01 receive UTXO as a delegate, the seller's token leads".into(),
                inputs: vec![
                    call(&b2, "fill", fill_args(n, 2, 2), "bid.fill", v, BID_COV),
                    tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[r.plus(n), plain(10_000 - n, &tk)], Wit::P2pk(f.taker)),
                    tok_delegate(&f.t, CARRIER, &r, Wit::Nothing),
                    p2pk_in(&f.taker, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, CARRIER, &r.plus(n), 1),
                    out(v - spend(3) - TT, spk_of(&b2), Some((0, BID_COV))),
                    tok_out(&f.t, CARRIER, &plain(10_000 - n, &tk), 1),
                ],
            },
            &f.taker,
        );
        // the token refuses it, and the bid too: a delegate has no successor (OpAuthOutputIdx)
        bad(&f.t, &s, 2);
        bad(&f.t, &s, 0);

        // O2 n <= the receive UTXO's threshold: the token program refuses the top-up (the builder uses form 1)
        let rh = recv(7_000, &m, n);
        bad_only(&f.t, &sell(&f, "NO02 top-up of n == the receive UTXO's threshold", &p2, v, n, 2, Lead::Recv(rh, CARRIER)), 1);
        ok(&f.t, &sell(&f, "PO02 the same fill as a new receive UTXO (form 1)", &p2, v, n, 1, Lead::Seller));

        // O3 mixed batch: a mode-2 top-up and a mode-1 custody fill against one resting ask (3 token in / 3 out)
        let v1 = v_after_first(4);
        let bt = bid(&f, &p2);
        let pa = BidP::new(&f.maker2, 1).bought(4_000);
        let (ba, ba2) = (bid(&f, &pa), bid(&f, &pa.bought(7_000)));
        let succ = r.plus(2_000);
        let (a, a_next) = (ask(&f, &f.asker, 10_000), ask(&f, &f.asker, 5_000));
        let rest = custody(5_000, ASK_COV);
        let cust_a = custody(7_000, BID2_COV);
        let batch = |top_first: bool, name: &str| {
            let (it, ia) = if top_first { (0, 1) } else { (1, 0) };
            let mut inputs = vec![
                call(&bt, "fill", fill_args(2_000, 3, 2), "bidT.fill", v, BID_COV),
                call(&ba, "fill", fill_args(3_000, 4, 0), "bidA.fill", v1, BID2_COV),
            ];
            let mut outs = vec![tok_out(&f.t, CARRIER, &succ, 3), tok_out(&f.t, DC, &cust_a, 3)];
            let mut next = vec![succ, cust_a, rest];
            if !top_first {
                inputs.swap(0, 1);
                outs.swap(0, 1);
                next.swap(0, 1);
            }
            inputs.extend([
                call(&a, "settle", settle_args(5_000, 5, 6), "ask.settle", CARRIER, ASK_COV),
                tok_leader(&f.t, CARRIER, &r, &next, Wit::Borrow),
                tok_delegate(&f.t, DC, &custody(4_000, BID2_COV), Wit::CovId),
                tok_delegate(&f.t, CARRIER, &custody(10_000, ASK_COV), Wit::CovId),
                p2pk_in(&f.matcher, KAS),
            ]);
            outs.extend([
                out(5 * (P240 - TIP), p2pk_spk(&pk(&f.asker)), None),
                out(v - spend(2) - TT, spk_of(&bt), Some((it, BID_COV))),
                out(v1 - spend(3), spk_of(&ba2), Some((ia, BID2_COV))),
                out(CARRIER, spk_of(&a_next), Some((2, ASK_COV))),
                tok_out(&f.t, CARRIER, &rest, 3),
            ]);
            close(Scn { name: name.into(), inputs, outputs: outs }, &f.matcher)
        };
        ok(&f.t, &batch(true, "PO03 mixed batch: mode-2 top-up first + mode-1 custody x one resting ask (3 / 3)"));
        // the token refuses it, and the top-up bid at input 1: its output 1 is not the successor
        let s = batch(false, "NO03 mixed batch with the mode-1 order at the lower index");
        bad(&f.t, &s, 3);
        bad(&f.t, &s, 1);

        // O4 two top-ups of one token: the second receive UTXO is a delegate (the token refuses it); one top-up + one form 1 passes
        let p2m2 = BidP::new(&f.maker2, 2);
        let bm2 = bid(&f, &p2m2);
        let r2 = recv(500, &m2, THR);
        let two = |both: bool, name: &str| {
            let mut inputs = vec![
                call(&b2, "fill", fill_args(2_000, 2, 2), "bid1.fill", v, BID_COV),
                call(&bm2, "fill", fill_args(3_000, if both { 3 } else { 2 }, if both { 2 } else { 1 }), "bid2.fill", v, BID2_COV),
            ];
            let d2 = if both { r2.plus(3_000) } else { recv(3_000, &m2, THR) };
            inputs.push(tok_leader(&f.t, CARRIER, &r, &[r.plus(2_000), d2, plain(5_000, &tk)], Wit::Borrow));
            if both {
                inputs.push(tok_delegate(&f.t, CARRIER, &r2, Wit::Nothing));
            }
            inputs.extend([tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)), p2pk_in(&f.taker, KAS)]);
            close(
                Scn {
                    name: name.into(),
                    inputs,
                    outputs: vec![
                        tok_out(&f.t, CARRIER, &r.plus(2_000), 2),
                        tok_out(&f.t, if both { CARRIER } else { DC }, &d2, 2),
                        out(v - spend(2) - TT, spk_of(&b2), Some((0, BID_COV))),
                        out(v - spend(3) - if both { TT } else { DC }, spk_of(&bm2), Some((1, BID2_COV))),
                        tok_out(&f.t, CARRIER, &plain(5_000, &tk), 2),
                    ],
                },
                &f.taker,
            )
        };
        ok(&f.t, &two(false, "PO04 two buyers: bid 0 topped up, bid 1 a new receive UTXO"));
        // the token refuses the second borrowed input, and bid 2: a delegate has no successor
        let s = two(true, "NO04 two buyers, both topped up (two borrowed inputs of one token)");
        bad(&f.t, &s, 3);
        bad(&f.t, &s, 1);

        // O7 (DLV-07) two bids of one maker both name the receive UTXO as form 2: bid 1's delivery at output 1 is a copy of
        // its state, not its successor. Form 1 for bid 1 is the honest shape.
        let bb = bid(&f, &p2.expiry(EXPIRY + 1));
        let seven = |both: bool, name: &str| {
            let d2 = if both { r.plus(3_000) } else { recv(3_000, &m, THR) };
            close(
                Scn {
                    name: name.into(),
                    inputs: vec![
                        call(&b2, "fill", fill_args(2_000, 2, 2), "bid1.fill", v, BID_COV),
                        call(&bb, "fill", fill_args(3_000, 2, if both { 2 } else { 1 }), "bid2.fill", v, BID2_COV),
                        tok_leader(&f.t, CARRIER, &r, &[r.plus(2_000), d2, plain(18_000 - d2.amount, &tk)], Wit::Borrow),
                        tok_delegate(&f.t, CARRIER, &plain(20_000, &tk), Wit::P2pk(f.taker)),
                        p2pk_in(&f.taker, KAS),
                    ],
                    outputs: vec![
                        tok_out(&f.t, CARRIER, &r.plus(2_000), 2),
                        tok_out(&f.t, if both { CARRIER } else { DC }, &d2, 2),
                        out(v - spend(2) - TT, spk_of(&b2), Some((0, BID_COV))),
                        out(v - spend(3) - if both { TT } else { DC }, spk_of(&bb), Some((1, BID2_COV))),
                        tok_out(&f.t, CARRIER, &plain(18_000 - d2.amount, &tk), 2),
                    ],
                },
                &f.taker,
            )
        };
        ok(&f.t, &seven(false, "PO07 one maker, two bids: bid 0 tops up the receive UTXO, bid 1 a new receive UTXO"));
        bad_only(&f.t, &seven(true, "NO07 one maker, two bids both top up the receive UTXO (bid 1's delivery not its successor)"), 1);

        // O8 (DLV-07, one bid) the bid at input 1 tops up; with `fake`, the receive UTXO's real successor is output 0 (the
        // seller pays a gain of THR + 1) and the bid's output 1 is a copy of the receive state grown by n.
        let eight = |fake: bool, name: &str| {
            let d = r.plus(n);
            let succ = r.plus(THR + 1);
            let first = if fake { succ } else { d };
            let change = plain(20_000 + 7_000 - first.amount - if fake { d.amount } else { 0 }, &tk);
            let mut next = vec![first];
            let mut outputs = vec![tok_out(&f.t, CARRIER, &first, 2)];
            if fake {
                next.push(d);
                outputs.push(tok_out(&f.t, CARRIER, &d, 2));
            } else {
                outputs.insert(0, out(KAS, p2pk_spk(&tk), None));
            }
            next.push(change);
            outputs.extend([out(v - spend(3) - TT, spk_of(&b2), Some((1, BID_COV))), tok_out(&f.t, CARRIER, &change, 2)]);
            close(
                Scn {
                    name: name.into(),
                    inputs: vec![
                        p2pk_in(&f.taker, KAS),
                        call(&b2, "fill", fill_args(n, 2, 2), "bid.fill", v, BID_COV),
                        tok_leader(&f.t, CARRIER, &r, &next, Wit::Borrow),
                        tok_delegate(&f.t, CARRIER, &plain(20_000, &tk), Wit::P2pk(f.taker)),
                        p2pk_in(&f.taker, KAS),
                    ],
                    outputs,
                },
                &f.taker,
            )
        };
        ok(&f.t, &eight(false, "PO08 a top-up by the bid at input 1 (output 0 a KAS output)"));
        bad_only(&f.t, &eight(true, "NO08 one bid: the receive UTXO's successor at output 0, the bid's output 1 a copy of its state"), 1);

        // O5 a refund-carrying terminal bid (IOC, input 0) next to a GTC mode-2 bid (input 1): only the terminal bid may top up
        let pr = BidP::new(&f.maker, 2).tif(IOC);
        let br = bid(&f, &pr);
        let pg = BidP::new(&f.maker2, 2);
        let bg = bid(&f, &pg);
        let rg = recv(7_000, &m2, THR);
        let five = |terminal_tops: bool, name: &str| {
            // bid R buys 2,000, bid G 3,000; one receive UTXO leads (R's or G's), the seller's token is a delegate
            let (lead, d_r, d_g, kas_r, kas_g, rel_r, rel_g, form_r, form_g) = if terminal_tops {
                (r, r.plus(2_000), recv(3_000, &m2, THR), CARRIER, DC, TT, DC, 2, 1)
            } else {
                (rg, recv(2_000, &m, THR), rg.plus(3_000), DC, CARRIER, DC, TT, 1, 2)
            };
            let rem_r = v - spend(2) - rel_r;
            close_with(
                Scn {
                    name: name.into(),
                    inputs: vec![
                        call(&br, "fill", fill_args(2_000, 2, form_r), "bidR.fill", v, BID_COV),
                        call(&bg, "fill", fill_args(3_000, 2, form_g), "bidG.fill", v, BID2_COV),
                        tok_leader(&f.t, CARRIER, &lead, &[d_r, d_g, plain(5_000, &tk)], Wit::Borrow),
                        tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                        p2pk_in(&f.taker, 2 * KAS),
                    ],
                    outputs: vec![
                        tok_out(&f.t, kas_r, &d_r, 2),
                        tok_out(&f.t, kas_g, &d_g, 2),
                        out(v - spend(3) - rel_g, spk_of(&bg), Some((1, BID2_COV))),
                        tok_out(&f.t, CARRIER, &plain(5_000, &tk), 2),
                    ],
                },
                &f.taker,
                &[(rem_r, m)],
            )
        };
        ok(&f.t, &five(true, "PO05 the refund-carrying terminal bid (input 0) tops up, the GTC bid gets a new receive UTXO"));
        // the token refuses it, and the GTC bid: its output 1 is not the successor
        let s = five(false, "NO05 the GTC bid at input 1 tops up next to a refund-carrying terminal bid");
        bad(&f.t, &s, 2);
        bad(&f.t, &s, 1);

        // O6 slots on a 3 / 3 program: two mode-1 orders with custodies x one resting ask fit; a third does not
        let pa = BidP::new(&f.maker, 1).bought(4_000);
        let pb = BidP::new(&f.maker2, 1).bought(2_000);
        let (ca, cb) = (custody(6_000, BID_COV), custody(5_000, BID2_COV));
        let s = close(
            Scn {
                name: "PO06 two mode-1 orders x one resting ask: each continues its own custody (3 / 3 slots)".into(),
                inputs: vec![
                    call(&bid(&f, &pa), "fill", fill_args(2_000, 3, 0), "bidA.fill", v1, BID_COV),
                    call(&bid(&f, &pb), "fill", fill_args(3_000, 4, 0), "bidB.fill", v1, BID2_COV),
                    call(&a, "settle", settle_args(5_000, 5, 6), "ask.settle", CARRIER, ASK_COV),
                    tok_leader(&f.t, DC, &custody(4_000, BID_COV), &[ca, cb, rest], Wit::CovId),
                    tok_delegate(&f.t, DC, &custody(2_000, BID2_COV), Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &custody(10_000, ASK_COV), Wit::CovId),
                    p2pk_in(&f.matcher, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, DC, &ca, 3),
                    tok_out(&f.t, DC, &cb, 3),
                    out(5 * (P240 - TIP), p2pk_spk(&pk(&f.asker)), None),
                    out(v1 - spend(2), spk_of(&bid(&f, &pa.bought(6_000))), Some((0, BID_COV))),
                    out(v1 - spend(3), spk_of(&bid(&f, &pb.bought(5_000))), Some((1, BID2_COV))),
                    out(CARRIER, spk_of(&a_next), Some((2, ASK_COV))),
                    tok_out(&f.t, CARRIER, &rest, 3),
                ],
            },
            &f.matcher,
        );
        ok(&f.t, &s);
        let pc = BidP::new(&f.asker, 1).bought(1_000);
        let cc = custody(2_000, BID3_COV);
        let rest3 = custody(4_000, ASK_COV);
        let s = close(
            Scn {
                name: "NO06 three mode-1 orders x one ask on a 3 / 3 program (4 token inputs)".into(),
                inputs: vec![
                    call(&bid(&f, &pa), "fill", fill_args(2_000, 4, 0), "bidA.fill", v1, BID_COV),
                    call(&bid(&f, &pb), "fill", fill_args(3_000, 5, 0), "bidB.fill", v1, BID2_COV),
                    call(&bid(&f, &pc), "fill", fill_args(1_000, 6, 0), "bidC.fill", v1, BID3_COV),
                    call(&a, "settle", settle_args(6_000, 7, 8), "ask.settle", CARRIER, ASK_COV),
                    tok_leader(&f.t, DC, &custody(4_000, BID_COV), &[ca, cb, cc, rest3], Wit::CovId),
                    tok_delegate(&f.t, DC, &custody(2_000, BID2_COV), Wit::CovId),
                    tok_delegate(&f.t, DC, &custody(1_000, BID3_COV), Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &custody(10_000, ASK_COV), Wit::CovId),
                    p2pk_in(&f.matcher, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, DC, &ca, 4),
                    tok_out(&f.t, DC, &cb, 4),
                    tok_out(&f.t, DC, &cc, 4),
                    out(6 * (P240 - TIP), p2pk_spk(&pk(&f.asker)), None),
                    out(v1 - spend(2), spk_of(&bid(&f, &pa.bought(6_000))), Some((0, BID_COV))),
                    out(v1 - spend(3), spk_of(&bid(&f, &pb.bought(5_000))), Some((1, BID2_COV))),
                    out(v1 - spend(1), spk_of(&bid(&f, &pc.bought(2_000))), Some((2, BID3_COV))),
                    out(CARRIER, spk_of(&ask(&f, &f.asker, 4_000)), Some((3, ASK_COV))),
                    tok_out(&f.t, CARRIER, &rest3, 4),
                ],
            },
            &f.matcher,
        );
        bad_only(&f.t, &s, 4);
    }
}

// ================================================================ budgets (spec section 9) and layout (section 2)

/// Script size, state layout and offsets, the `bought` splice window, the static sig-op bound, and the relay fee of a fill
/// per mode on all four published programs.
#[test]
fn modes_budgets_and_layout() {
    let f = fx(Token::named("KCC20Ref"));
    let le = |s: &[u8], a: usize| i64::from_le_bytes(s[a..a + 8].try_into().unwrap());
    let p = BidP { mode: 2, thr: 777, ttip: 12_345, ..BidP::new(&f.maker, 0) };
    let a = bid(&f, &p);
    let bc = bytecode(&a);
    let l = state_layout(&a);
    assert_eq!((l.start, l.len), (1, 321), "state span");
    // the delivery block: state offsets 285, 294, 303, 312 (redeem + 1), each an 8-byte int push
    for (at, v) in [(286, 2), (295, 777), (304, 12_345), (313, 0)] {
        assert_eq!(bc[at], 0x08, "push opcode at redeem {at}");
        assert_eq!(le(&bc, at + 1), v, "int at redeem {at}");
    }
    assert_eq!(BOUGHT_AT, 314);
    let (b0, b5) = (bytecode(&bid(&f, &BidP::new(&f.maker, 1))), bytecode(&bid(&f, &BidP::new(&f.maker, 1).bought(5))));
    assert_eq!(le(&b5, BOUGHT_AT), 5);
    assert_eq!((&b0[..BOUGHT_AT], &b0[BOUGHT_AT + 8..]), (&b5[..BOUGHT_AT], &b5[BOUGHT_AT + 8..]), "only the bought window differs");
    // the existing fields keep their offsets (the trigger evidence reads them): tokenCovId, extensionCommitment, scale
    let s = &bc[1..];
    assert_eq!(&s[34..66], &TOKEN_COV.as_bytes());
    assert_eq!(&s[118..150], &EXT);
    assert_eq!(le(s, 151), SCALE);
    let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
        &push(&bc),
        &pay_to_script_hash_script(&bc),
    );
    println!("SIZE KobBid: redeem script {} B (state 321 B, template {} B), static sig-op upper bound {so}", bc.len(), bc.len() - 321);
    assert!(so <= 15, "sig-op cap");
    assert!(bc.len() <= SIZE_CEILING, "KobBid {} B exceeds {} B (section 9)", bc.len(), SIZE_CEILING);

    for t in [
        Token::named("KCC20Ref"),
        Token::pinned(TemplateId::Kcc20PublicMint),
        Token::pinned(TemplateId::KForgeToken7214),
        Token::pinned(TemplateId::KaspaRocketToken11458),
    ] {
        let f = fx(t);
        let m = pk(&f.maker);
        let r = recv(7_000, &m, THR);
        let n = 3_000;
        let v1 = v_after_first(4);
        let c0 = ok(&f.t, &sell(&f, "BF0 mode 0 fill", &BidP::new(&f.maker, 0), v0(), n, 0, Lead::Seller));
        let c1a = ok(&f.t, &sell(&f, "BF1a mode 1 first fill", &BidP::new(&f.maker, 1), v0(), n, 0, Lead::Seller));
        let c1b = ok(&f.t, &sell(&f, "BF1b mode 1 later fill", &BidP::new(&f.maker, 1).bought(4_000), v1, n, 0, Lead::Custody(DC)));
        let c2b = ok(&f.t, &sell(&f, "BF2b mode 2 form 1", &BidP::new(&f.maker, 2), v0(), n, 1, Lead::Seller));
        let c2c = ok(&f.t, &sell(&f, "BF2c mode 2 top-up", &BidP::new(&f.maker, 2), v0(), n, 2, Lead::Recv(r, CARRIER)));
        let x0 = ok(&f.t, &cross(&f, "BX0 mode 0 bid x ask", &BidP::new(&f.maker, 0), v0(), n, 0, Lead::Seller));
        let x2b = ok(&f.t, &cross(&f, "BX2b mode 2 bid x ask, form 1", &BidP::new(&f.maker, 2), v0(), n, 1, Lead::Seller));
        let x2c = ok(&f.t, &cross(&f, "BX2c mode 2 bid x ask, top-up", &BidP::new(&f.maker, 2), v0(), n, 2, Lead::Recv(r, CARRIER)));
        println!(
            "FEE @{}: taker sell mode 0 {} | mode 1 first {} later {} | mode 2 form 1 {} top-up {} sompi; top-up extra over form 1: taker {} / bid x ask {} sompi",
            f.t.name,
            c0.fee,
            c1a.fee,
            c1b.fee,
            c2b.fee,
            c2c.fee,
            c2c.fee as i64 - c2b.fee as i64,
            x2c.fee as i64 - x2b.fee as i64
        );
        println!("FEE @{}: bid x ask mode 0 {} sompi; compute mass mode 0 {} transient {}", f.t.name, x0.fee, c0.compute, c0.transient);
    }
}
