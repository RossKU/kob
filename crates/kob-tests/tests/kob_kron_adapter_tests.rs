//! KOB KRON-family adapter harness: AskOrderKron / BidOrderKron executed in rusty-kaspa v2.1.0's
//! TxScriptEngine against the REAL KRON token program bytes (mainnet template served by
//! api.kron.technology /api/native/cp-template, 2433-byte version shared by 85 of 91 registry
//! tokens; the 2732-byte version is used for the wrong-template negative).
//!
//! Sources and pinned templates are read from `contracts/adapters/kron/`. Env: KOB_CARRIER_KAS =
//! 2|10|20 (default 10), KOB_KRON_TPL = 2433|2732 selects the primary token template (default 2433).
//! Run: cargo test --release -p kob-tests --test kob_kron_adapter_tests -- --nocapture --test-threads=1

// Harness style: explicit `&` on byte-slice arguments, index loops over parallel vectors and
// written-out identity arithmetic keep the scenario tables readable.
#![allow(clippy::needless_borrows_for_generic_args, clippy::needless_range_loop, clippy::identity_op, clippy::self_assignment)]

mod common;

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
use silverscript_lang::template::template_hash;

use common::{bytecode, compile_contract, encode_entry_sig_script, push_redeem_script};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const FAKE_COV: Hash = Hash::from_bytes([0x66; 32]);
const T_PUBKEY: u8 = 0;
const T_COVID: u8 = 2;
const T_ADDR: u8 = 3;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;
const EXPIRY_DAA: i64 = 500_000_000;
const LOT: i64 = 1_000;

fn src(name: &str) -> String {
    common::contract_source(name)
}
fn carrier() -> i64 {
    std::env::var("KOB_CARRIER_KAS").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(10) * KAS
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

// ---------------------------------------------------------------- KRON token (real bytes)

#[derive(Clone, Copy)]
struct KS {
    owner: [u8; 32],
    typ: u8,
    amount: i64,
    minter: u8,
}
fn ks<O: AsRef<[u8]>>(owner: O, typ: u8, amount: i64) -> KS {
    let mut o = [0u8; 32];
    o.copy_from_slice(owner.as_ref());
    KS { owner: o, typ, amount, minter: 0 }
}
impl KS {
    /// The 46-byte state region: 0x20 owner | 0x01 type | 0x08 amount LE | 0x01 minter
    fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0x20];
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.typ, 0x08]);
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.extend_from_slice(&[0x01, self.minter]);
        assert_eq!(v.len(), 46);
        v
    }
}
#[derive(Clone)]
struct Kron {
    suffix: Vec<u8>,
    hash: Vec<u8>,
}
impl Kron {
    fn load(file: &str) -> Self {
        let b = common::kron_template(file);
        let suffix = b[46..].to_vec();
        let hash = template_hash(&[], &suffix).to_vec();
        Kron { suffix, hash }
    }
    fn redeem(&self, s: &KS) -> Vec<u8> {
        [s.bytes(), self.suffix.clone()].concat()
    }
    fn spk(&self, s: &KS) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(s))
    }
}

/// Args shared by every token input of a tx: next states (columns), sig column, witness column.
#[derive(Clone)]
struct TokArgs {
    next: Vec<KS>,
    wit: Vec<u8>,
    sigs: Vec<u8>,
}

// ---------------------------------------------------------------- orders

fn ask_art(k: &Kron, maker: &[u8; 32], lot: i64, price: i64) -> SilAbiArtifact {
    compile_contract(
        &src("AskOrderKron"),
        &[
            maker.to_vec().into(),
            TOKEN_COV.as_bytes().to_vec().into(),
            k.hash.clone().into(),
            ArtifactValue::Int(0),
            ArtifactValue::Int(k.suffix.len() as i64),
            ArtifactValue::Int(lot),
            ArtifactValue::Int(price),
            ArtifactValue::Int(EXPIRY_DAA),
            ArtifactValue::Int(KAS / 10),
        ],
        CompileOptions::default(),
    )
    .expect("compile AskOrderKron")
}
fn bid_art(k: &Kron, maker: &[u8; 32], lot: i64, price: i64, reserve: i64) -> SilAbiArtifact {
    compile_contract(
        &src("BidOrderKron"),
        &[
            maker.to_vec().into(),
            TOKEN_COV.as_bytes().to_vec().into(),
            k.hash.clone().into(),
            ArtifactValue::Int(0),
            ArtifactValue::Int(k.suffix.len() as i64),
            ArtifactValue::Int(lot),
            ArtifactValue::Int(price),
            ArtifactValue::Int(reserve),
            ArtifactValue::Int(carrier()),
            ArtifactValue::Int(EXPIRY_DAA),
            ArtifactValue::Int(KAS / 10),
        ],
        CompileOptions::default(),
    )
    .expect("compile BidOrderKron")
}
fn spk_of(a: &SilAbiArtifact) -> ScriptPublicKey {
    pay_to_script_hash_script(&bytecode(a))
}

// ---------------------------------------------------------------- scenario model

#[derive(Clone)]
enum Role {
    Ask { art: SilAbiArtifact, n: i64, token_in: i64, tok_out: i64 },
    Bid { art: SilAbiArtifact, n: i64, tpl_in: i64 },
    Cancel { art: SilAbiArtifact, kp: Keypair, sighash: u8 },
    Tok { redeem: Vec<u8> },
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
    tok: TokArgs,
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
fn ask_in(art: &SilAbiArtifact, value: i64, c: Hash, n: i64, token_in: i64, tok_out: i64) -> Inp {
    Inp { entry: cov_utxo(value, spk_of(art), c), role: Role::Ask { art: art.clone(), n, token_in, tok_out } }
}
fn bid_in(art: &SilAbiArtifact, value: i64, c: Hash, n: i64, tpl_in: i64) -> Inp {
    Inp { entry: cov_utxo(value, spk_of(art), c), role: Role::Bid { art: art.clone(), n, tpl_in } }
}
fn tok_in(k: &Kron, value: i64, st: &KS, c: Hash) -> Inp {
    let redeem = k.redeem(st);
    Inp { entry: cov_utxo(value, pay_to_script_hash_script(&redeem), c), role: Role::Tok { redeem } }
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
/// KRON token sigscript (SDK transferSigScript): owners | types | amounts | minters | sigs | witnesses | redeem
fn kron_ss(redeem: &[u8], a: &TokArgs) -> Vec<u8> {
    let mut s = vec![];
    s.extend(push(&a.next.iter().flat_map(|x| x.owner.to_vec()).collect::<Vec<u8>>()));
    s.extend(push(&a.next.iter().map(|x| x.typ).collect::<Vec<u8>>()));
    s.extend(push(&a.next.iter().flat_map(|x| x.amount.to_le_bytes().to_vec()).collect::<Vec<u8>>()));
    s.extend(push(&a.next.iter().map(|x| x.minter).collect::<Vec<u8>>()));
    s.extend(push(&a.sigs));
    s.extend(push(&a.wit));
    s.extend(push(redeem));
    s
}

fn build(s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
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
    let mut tx = Transaction::new(1, inputs, s.outputs.clone(), s.lock_time, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    for (k, inp) in s.inputs.iter().enumerate() {
        let ss = match &inp.role {
            Role::Ask { art, n, token_in, tok_out } => {
                entry_ss(art, "settle", &[ArtifactValue::Int(*n), ArtifactValue::Int(*token_in), ArtifactValue::Int(*tok_out)])
            }
            Role::Bid { art, n, tpl_in } => entry_ss(art, "fill", &[ArtifactValue::Int(*n), ArtifactValue::Int(*tpl_in)]),
            Role::Cancel { art, kp, sighash } => entry_ss(art, "cancel", &[sign(&unsigned, &entries, k, kp, *sighash).into()]),
            Role::Tok { redeem } => kron_ss(redeem, &s.tok),
            Role::P2pk { kp } => push(&sign(&unsigned, &entries, k, kp, 0x01)),
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
        Role::Ask { .. } => "ask",
        Role::Bid { .. } => "bid",
        Role::Cancel { .. } => "cancel",
        Role::Tok { .. } => "kron.token",
        Role::P2pk { .. } => "p2pk",
    }
}

fn run_ok(s: &Scn) {
    let (tx, entries) = build(s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => panic!("{}: input {i} ({}) failed: {e:?}", s.name, role_name(&s.inputs[i].role)),
        }
    }
    let (tx, entries) = build(s, &budgets);
    let res = execute(&tx, &entries).expect("ctx");
    assert!(res.iter().all(|r| r.is_ok()), "{}: second pass failed", s.name);
    let mc = MassCalculator::new(1, 10, 1_000_000_000_000);
    let nc = mc.calc_non_contextual_masses(&tx);
    let populated = PopulatedTransaction::new(&tx, entries.clone());
    let storage = mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX);
    let size = transaction_estimated_serialized_size(&tx);
    let fee_mass = nc.compute_mass.max(2 * size); // relay floor (storage mass excluded, as in protocol_design.md)
    println!("SCENARIO {}  [PASS]", s.name);
    println!("  inputs={} outputs={}", tx.inputs.len(), tx.outputs.len());
    for (i, r) in res.iter().enumerate() {
        println!(
            "  in[{i}] {:<11} sigscript={:>5} B  script_units={:>7}  compute_budget={}",
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
fn run_bad(s: &Scn, expect: usize) {
    let (tx, entries) = build(s, &[]);
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
/// Informational: report which inputs pass/fail (no assertion).
fn run_info(s: &Scn) {
    let (tx, entries) = build(s, &[]);
    match execute(&tx, &entries) {
        Err(e) => println!("INFO {}: tx-level failure {e}", s.name),
        Ok(res) => {
            let v: Vec<String> = res
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "in[{i}] {} {}",
                        role_name(&s.inputs[i].role),
                        if r.is_ok() { "ok".to_string() } else { format!("REJECT {:?}", r.as_ref().unwrap_err()) }
                    )
                })
                .collect();
            println!("INFO {}: {}", s.name, v.join(" | "));
        }
    }
}
fn with_name(mut s: Scn, n: &str) -> Scn {
    s.name = n.into();
    s
}
/// Replace the shared token args (next states) of a scenario.
fn set_next(s: &mut Scn, next: Vec<KS>) {
    s.tok.next = next;
}

// ---------------------------------------------------------------- fixtures

struct Fx {
    k: Kron,
    ma: Keypair,
    mb: Keypair,
    mc: Keypair,
    taker: Keypair,
}
fn fx() -> Fx {
    Fx { k: Kron::load(&primary_tpl()), ma: keypair(), mb: keypair(), mc: keypair(), taker: keypair() }
}
const FEE: i64 = KAS / 10;
/// Primary token template under test (env KOB_KRON_TPL=2433|2732, default 2433) and the other one.
fn primary_tpl() -> String {
    format!("kron_token_{}.bin", std::env::var("KOB_KRON_TPL").unwrap_or_else(|_| "2433".into()))
}
fn other_tpl() -> String {
    if primary_tpl().contains("2433") {
        "kron_token_2732.bin".into()
    } else {
        "kron_token_2433.bin".into()
    }
}

/// K1: taker buys 4 of 10 lots from one ask (partial). Ask at input 1 (witness index 1).
fn k1(f: &Fx) -> Scn {
    let k = &f.k;
    let a = cov(0xa1);
    let price = 25 * KAS / 10;
    let c = carrier();
    let ask = ask_art(k, &pk(&f.ma), LOT, price);
    let taker = pk(&f.taker);
    Scn {
        name: "K1 taker buys 4/10 lots from 1 ask (partial)".into(),
        inputs: vec![
            p2pk_in(&f.taker, 1000 * KAS),
            ask_in(&ask, c, a, 4, 2, 3),
            tok_in(k, c, &ks(a.as_bytes(), T_COVID, 10 * LOT), TOKEN_COV),
        ],
        outputs: vec![
            out(1000 * KAS - 4 * price - c - FEE, p2pk_spk(&taker), None),
            out(4 * price, p2pk_spk(&pk(&f.ma)), None),
            out(c, spk_of(&ask), Some((1, a))),
            out(c, k.spk(&ks(a.as_bytes(), T_COVID, 6 * LOT)), Some((2, TOKEN_COV))),
            out(c, k.spk(&ks(&taker, T_ADDR, 4 * LOT)), Some((2, TOKEN_COV))),
        ],
        lock_time: 0,
        tok: TokArgs { next: vec![ks(a.as_bytes(), T_COVID, 6 * LOT), ks(&taker, T_ADDR, 4 * LOT)], wit: vec![1], sigs: vec![] },
    }
}

/// K2(k): taker sweeps k asks (all full except the last, partial 4/10 lots), k token inputs.
fn k2(f: &Fx, cnt: usize) -> Scn {
    let k = &f.k;
    let c = carrier();
    let taker = pk(&f.taker);
    let pa = 25 * KAS / 10;
    let makers = [&f.ma, &f.ma, &f.mc, &f.mc, &f.mb];
    let mut inputs = vec![p2pk_in(&f.taker, 1000 * KAS)];
    let mut asks = vec![];
    let mut covs = vec![];
    for i in 0..cnt {
        let cv = cov(0xa1 + i as u8);
        let art = ask_art(k, &pk(makers[i]), LOT, pa);
        covs.push(cv);
        asks.push(art);
    }
    let last = cnt - 1;
    let n_of = |i: usize| if i == last { 4 } else { 2 + i as i64 };
    let held_of = |i: usize| if i == last { 10 * LOT } else { n_of(i) * LOT };
    // input order: taker, asks (1..=cnt), tokens (cnt+1..=2cnt)
    for i in 0..cnt {
        inputs.push(ask_in(&asks[i], c, covs[i], n_of(i), (cnt + 1 + i) as i64, (cnt + 2 + i) as i64 + 0));
    }
    for i in 0..cnt {
        inputs.push(tok_in(k, c, &ks(covs[i].as_bytes(), T_COVID, held_of(i)), TOKEN_COV));
    }
    let bought: i64 = (0..cnt).map(n_of).sum();
    let mut outputs = vec![out(0, p2pk_spk(&taker), None)]; // index 0 = taker change (value fixed below)
    for i in 0..cnt {
        let full = i != last;
        let pay = n_of(i) * pa;
        outputs.push(out(if full { pay + 2 * c } else { pay }, p2pk_spk(&pk(makers[i])), None));
    }
    let left = 10 * LOT - 4 * LOT;
    // continuation + token change of the partial ask, then the taker token
    let ci = outputs.len() as i64;
    outputs.push(out(c, spk_of(&asks[last]), Some((last as u16 + 1, covs[last]))));
    let ti = outputs.len() as i64;
    outputs.push(out(c, k.spk(&ks(covs[last].as_bytes(), T_COVID, left)), Some((cnt as u16 + 1, TOKEN_COV))));
    outputs.push(out(c, k.spk(&ks(&taker, T_ADDR, bought * LOT)), Some((cnt as u16 + 1, TOKEN_COV))));
    let _ = ci;
    // fix the last ask's tok_out
    if let Role::Ask { tok_out, .. } = &mut inputs[cnt].role {
        *tok_out = ti;
    }
    let paid: i64 = outputs[1..].iter().map(|o| o.value as i64).sum::<i64>() + 0;
    // funding: taker pays payouts + carriers of new outputs (continuations are funded by the ask/token inputs)
    let ins_cov: i64 = 2 * c * cnt as i64;
    let outs_cov: i64 = outputs[1..].iter().filter(|o| o.covenant.is_some()).map(|o| o.value as i64).sum();
    let payouts: i64 = paid - outs_cov;
    outputs[0].value = (1000 * KAS + ins_cov - payouts - outs_cov - FEE) as u64;
    Scn {
        name: format!("K2 taker sweeps {cnt} asks ({} full + 1 partial), {cnt} KRON token inputs", cnt - 1),
        inputs,
        outputs,
        lock_time: 0,
        tok: TokArgs {
            next: vec![ks(covs[last].as_bytes(), T_COVID, left), ks(&taker, T_ADDR, bought * LOT)],
            wit: (1..=cnt as u8).collect(),
            sigs: vec![],
        },
    }
}

/// K3: taker (KRON tokens owned with id_type 3) sells 4 lots into one bid (partial).
fn k3(f: &Fx) -> Scn {
    let k = &f.k;
    let b = cov(0xb1);
    let c = carrier();
    let price = 24 * KAS / 10;
    let reserve = KAS;
    let bid = bid_art(k, &pk(&f.mb), LOT, price, reserve);
    let taker = pk(&f.taker);
    let bid_value = 10 * price + reserve + 3 * c;
    let pay = 4 * price;
    let left = bid_value - pay;
    let p = 5 * KAS;
    Scn {
        name: "K3 taker sells 4 lots into 1 bid (partial)".into(),
        inputs: vec![bid_in(&bid, bid_value, b, 4, 1), tok_in(k, c, &ks(&taker, T_ADDR, 7 * LOT), TOKEN_COV), p2pk_in(&f.taker, p)],
        outputs: vec![
            out(c, k.spk(&ks(&pk(&f.mb), T_ADDR, 4 * LOT)), Some((1, TOKEN_COV))),
            out(left - c, spk_of(&bid), Some((0, b))),
            out(c, k.spk(&ks(&taker, T_ADDR, 3 * LOT)), Some((1, TOKEN_COV))),
            out(p + pay - FEE, p2pk_spk(&taker), None),
        ],
        lock_time: 0,
        tok: TokArgs { next: vec![ks(&pk(&f.mb), T_ADDR, 4 * LOT), ks(&taker, T_ADDR, 3 * LOT)], wit: vec![2], sigs: vec![] },
    }
}

/// K4: taker sells into two bids at once (bid1 2 lots, bid2 4 lots, 6 lots sold).
fn k4(f: &Fx) -> Scn {
    let k = &f.k;
    let (b1, b2) = (cov(0xb1), cov(0xb2));
    let c = carrier();
    let price = 24 * KAS / 10;
    let reserve = KAS;
    let bid1 = bid_art(k, &pk(&f.mb), LOT, price, reserve);
    let bid2 = bid_art(k, &pk(&f.mc), LOT, price, reserve);
    let taker = pk(&f.taker);
    let v = 10 * price + reserve + 3 * c;
    let p = 5 * KAS;
    Scn {
        name: "K4 taker sells into 2 bids (2 + 4 lots), 6 lots sold".into(),
        inputs: vec![
            bid_in(&bid1, v, b1, 2, 2),
            bid_in(&bid2, v, b2, 4, 2),
            tok_in(k, c, &ks(&taker, T_ADDR, 6 * LOT), TOKEN_COV),
            p2pk_in(&f.taker, p),
        ],
        outputs: vec![
            out(c, k.spk(&ks(&pk(&f.mb), T_ADDR, 2 * LOT)), Some((2, TOKEN_COV))),
            out(c, k.spk(&ks(&pk(&f.mc), T_ADDR, 4 * LOT)), Some((2, TOKEN_COV))),
            out(v - 2 * price - c, spk_of(&bid1), Some((0, b1))),
            out(v - 4 * price - c, spk_of(&bid2), Some((1, b2))),
            out(p + 6 * price + c - c - FEE, p2pk_spk(&taker), None),
        ],
        lock_time: 0,
        tok: TokArgs { next: vec![ks(&pk(&f.mb), T_ADDR, 2 * LOT), ks(&pk(&f.mc), T_ADDR, 4 * LOT)], wit: vec![3], sigs: vec![] },
    }
}

/// K5: maker cancels its ask (ask at input 0, token witness index 0), tokens returned to the maker.
fn k5(f: &Fx, sighash: u8, signer: Keypair) -> Scn {
    let k = &f.k;
    let a = cov(0xa1);
    let c = carrier();
    let ask = ask_art(k, &pk(&f.ma), LOT, 25 * KAS / 10);
    let ma = pk(&f.ma);
    Scn {
        name: "K5 ask cancel (ask at input 0)".into(),
        inputs: vec![
            Inp { entry: cov_utxo(c, spk_of(&ask), a), role: Role::Cancel { art: ask.clone(), kp: signer, sighash } },
            tok_in(k, c, &ks(a.as_bytes(), T_COVID, 10 * LOT), TOKEN_COV),
        ],
        outputs: vec![out(2 * c - FEE, k.spk(&ks(&ma, T_ADDR, 10 * LOT)), Some((1, TOKEN_COV)))],
        lock_time: 0,
        tok: TokArgs { next: vec![ks(&ma, T_ADDR, 10 * LOT)], wit: vec![0], sigs: vec![] },
    }
}

/// K6: permissionless ask refund after expiry (n = 0), keeper keeps up to refundTip.
fn k6(f: &Fx, lock_time: u64) -> Scn {
    let k = &f.k;
    let a = cov(0xa1);
    let c = carrier();
    let ask = ask_art(k, &pk(&f.ma), LOT, 25 * KAS / 10);
    let ma = pk(&f.ma);
    Scn {
        name: "K6 ask refund after expiry".into(),
        inputs: vec![ask_in(&ask, c, a, 0, 1, 0), tok_in(k, c, &ks(a.as_bytes(), T_COVID, 10 * LOT), TOKEN_COV)],
        outputs: vec![out(2 * c - KAS / 10, k.spk(&ks(&ma, T_ADDR, 10 * LOT)), Some((1, TOKEN_COV)))],
        lock_time,
        tok: TokArgs { next: vec![ks(&ma, T_ADDR, 10 * LOT)], wit: vec![0], sigs: vec![] },
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn kron_positive() {
    let f = fx();
    run_ok(&k1(&f));
    run_ok(&k2(&f, 2));
    run_ok(&k2(&f, 3));
    run_ok(&k2(&f, 4));
    run_ok(&k3(&f));
    run_ok(&k4(&f));
    run_ok(&k5(&f, 0x01, f.ma));
    run_ok(&k6(&f, EXPIRY_DAA as u64));
}

#[test]
fn kron_negative_ask() {
    let f = fx();
    let k = &f.k;
    let a = cov(0xa1);
    let taker = pk(&f.taker);
    let c = carrier();

    // A1: continuation token change redirected to the taker (id 3) -> ask pins owner/type
    let mut s = with_name(k1(&f), "A1 ask: token change owned by the taker instead of the order");
    s.outputs[3].script_public_key = k.spk(&ks(&taker, T_ADDR, 6 * LOT));
    set_next(&mut s, vec![ks(&taker, T_ADDR, 6 * LOT), ks(&taker, T_ADDR, 4 * LOT)]);
    run_bad(&s, 1);

    // A2: change kept under the order's covenant id but with id_type 0 (pubkey owner = order id, unspendable/odd)
    let mut s = with_name(k1(&f), "A2 ask: token change id_type 0 (not covenant-id custody)");
    s.outputs[3].script_public_key = k.spk(&ks(a.as_bytes(), T_PUBKEY, 6 * LOT));
    set_next(&mut s, vec![ks(a.as_bytes(), T_PUBKEY, 6 * LOT), ks(&taker, T_ADDR, 4 * LOT)]);
    run_bad(&s, 1);

    // A3: change with is_minter = 1
    let mut s = with_name(k1(&f), "A3 ask: token change carries is_minter=1");
    let mut st = ks(a.as_bytes(), T_COVID, 6 * LOT);
    st.minter = 1;
    s.outputs[3].script_public_key = k.spk(&st);
    set_next(&mut s, vec![st, ks(&taker, T_ADDR, 4 * LOT)]);
    run_bad(&s, 1);

    // A4: token input not owned by the order (taker's own tokens, id 3 presence via taker P2PK input 0)
    let mut s = with_name(k1(&f), "A4 ask: token input owned by the taker, not the order");
    s.inputs[2] = tok_in(k, c, &ks(&taker, T_ADDR, 10 * LOT), TOKEN_COV);
    s.tok.wit = vec![0];
    run_bad(&s, 1);

    // A5: foreign covenant id, same template
    let mut s = with_name(k1(&f), "A5 ask: fake token family (same template, foreign covenant id)");
    s.inputs[2].entry = cov_utxo(c, s.inputs[2].entry.script_public_key.clone(), FAKE_COV);
    for o in s.outputs.iter_mut() {
        if let Some(cv) = o.covenant.as_mut() {
            if cv.covenant_id == TOKEN_COV {
                cv.covenant_id = FAKE_COV;
            }
        }
    }
    run_bad(&s, 1);

    // A6: right covenant id but a DIFFERENT token template (the other KRON program version)
    let k2t = Kron::load(&other_tpl());
    let mut s = with_name(k1(&f), "A6 ask: token spent under a different template hash (2732-byte KRON program)");
    s.inputs[2] = tok_in(&k2t, c, &ks(a.as_bytes(), T_COVID, 10 * LOT), TOKEN_COV);
    s.outputs[3].script_public_key = k2t.spk(&ks(a.as_bytes(), T_COVID, 6 * LOT));
    s.outputs[4].script_public_key = k2t.spk(&ks(&taker, T_ADDR, 4 * LOT));
    run_bad(&s, 1);

    // A7-A9 carrier / payout skims
    let mut s = with_name(k1(&f), "A7 ask: payout short by 1 sompi");
    s.outputs[1].value -= 1;
    run_bad(&s, 1);
    let mut s = with_name(k1(&f), "A8 ask: continuation KAS carrier short by 1 sompi");
    s.outputs[2].value -= 1;
    run_bad(&s, 1);
    let mut s = with_name(k1(&f), "A9 ask: token-change carrier short by 1 sompi");
    s.outputs[3].value -= 1;
    run_bad(&s, 1);

    // A10 malleated n
    for n in [5i64, 3, 0] {
        let mut s = with_name(k1(&f), &format!("A10 ask: malleated qty arg n={n} (outputs unchanged)"));
        if let Role::Ask { n: nn, .. } = &mut s.inputs[1].role {
            *nn = n;
        }
        run_bad(&s, 1);
    }

    // A11 positional payout: 2 asks of the same maker, single merged payout at output 1
    let mut s = with_name(k2(&f, 2), "A11 ask: two asks of one maker paid by ONE output (aliasing)");
    let merged = s.outputs[1].value + s.outputs[2].value;
    s.outputs[1].value = merged;
    s.outputs[2].script_public_key = p2pk_spk(&taker); // output 2 no longer pays the maker
    s.outputs[2].value = 1;
    s.outputs[0].value = s.outputs[0].value; // fee grows; irrelevant for script check
    run_bad(&s, 2);

    // A12 duplicate covenant id input
    let mut s = with_name(k1(&f), "A12 ask: two inputs share the order covenant id (singleton check)");
    let dup = s.inputs[1].clone();
    s.inputs.push(dup);
    run_bad(&s, 1);

    // A13 token program: counterfeit +1 lot in the taker output
    let mut s = with_name(k1(&f), "A13 token: +1 lot minted in taker output -> KRON token program rejects (conservation)");
    s.outputs[4].script_public_key = k.spk(&ks(&taker, T_ADDR, 5 * LOT));
    set_next(&mut s, vec![ks(a.as_bytes(), T_COVID, 6 * LOT), ks(&taker, T_ADDR, 5 * LOT)]);
    run_bad(&s, 2);

    // A14 token program: witness points at the wrong input (P2PK funding input 0 instead of the order)
    let mut s = with_name(k1(&f), "A14 token: covenant-id witness points at a non-order input -> token rejects");
    s.tok.wit = vec![0];
    run_bad(&s, 2);

    // A15 token program: 5 token inputs (KRON max 4)
    let s = with_name(k2(&f, 5), "A15 shape: 5 token inputs -> KRON token program rejects (max 4)");
    run_bad(&s, 6);
}

#[test]
fn kron_negative_bid() {
    let f = fx();
    let k = &f.k;
    let taker = pk(&f.taker);
    let mb = pk(&f.mb);
    let c = carrier();

    let mut s = with_name(k3(&f), "B1 bid: under-delivery (3999 tokens for 4 lots)");
    s.outputs[0].script_public_key = k.spk(&ks(&mb, T_ADDR, 4 * LOT - 1));
    s.outputs[2].script_public_key = k.spk(&ks(&taker, T_ADDR, 3 * LOT + 1));
    set_next(&mut s, vec![ks(&mb, T_ADDR, 4 * LOT - 1), ks(&taker, T_ADDR, 3 * LOT + 1)]);
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B2 bid: tokens delivered to the taker instead of the maker");
    s.outputs[0].script_public_key = k.spk(&ks(&taker, T_ADDR, 4 * LOT));
    set_next(&mut s, vec![ks(&taker, T_ADDR, 4 * LOT), ks(&taker, T_ADDR, 3 * LOT)]);
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B3 bid: delivery with id_type 0 (pubkey) instead of 3");
    s.outputs[0].script_public_key = k.spk(&ks(&mb, T_PUBKEY, 4 * LOT));
    set_next(&mut s, vec![ks(&mb, T_PUBKEY, 4 * LOT), ks(&taker, T_ADDR, 3 * LOT)]);
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B4 bid: delivery carries is_minter=1");
    let mut st = ks(&mb, T_ADDR, 4 * LOT);
    st.minter = 1;
    s.outputs[0].script_public_key = k.spk(&st);
    set_next(&mut s, vec![st, ks(&taker, T_ADDR, 3 * LOT)]);
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B5 bid: continuation KAS short by 1 sompi");
    s.outputs[1].value -= 1;
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B6 bid: forced termination while buying power remains (grief)");
    let v = s.outputs[1].value;
    s.outputs.remove(1);
    s.outputs[0].value += v;
    // token change/taker outputs shift by one; fix auth indices unaffected
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B7 bid: malleated qty arg n=3");
    if let Role::Bid { n, .. } = &mut s.inputs[0].role {
        *n = 3;
    }
    run_bad(&s, 0);

    let mut s = with_name(k3(&f), "B8 bid: fake token (same template, foreign covenant id)");
    s.inputs[1].entry = cov_utxo(c, s.inputs[1].entry.script_public_key.clone(), FAKE_COV);
    for o in s.outputs.iter_mut() {
        if let Some(cv) = o.covenant.as_mut() {
            if cv.covenant_id == TOKEN_COV {
                cv.covenant_id = FAKE_COV;
            }
        }
    }
    run_bad(&s, 0);

    let k2t = Kron::load(&other_tpl());
    let mut s = with_name(k3(&f), "B9 bid: delivery built from a different template (2732-byte KRON program)");
    s.inputs[1] = tok_in(&k2t, c, &ks(&taker, T_ADDR, 7 * LOT), TOKEN_COV);
    s.outputs[0].script_public_key = k2t.spk(&ks(&mb, T_ADDR, 4 * LOT));
    s.outputs[2].script_public_key = k2t.spk(&ks(&taker, T_ADDR, 3 * LOT));
    run_bad(&s, 0);

    let mut s = with_name(k4(&f), "B10 bid: aliasing (bid 2's delivery slot redirected to the taker)");
    s.outputs[1].script_public_key = k.spk(&ks(&taker, T_ADDR, 4 * LOT));
    set_next(&mut s, vec![ks(&pk(&f.mb), T_ADDR, 2 * LOT), ks(&taker, T_ADDR, 4 * LOT)]);
    run_bad(&s, 1);

    let mut s = with_name(k3(&f), "B11 bid: delivery carrier skimmed by 1 sompi");
    s.outputs[0].value -= 1;
    run_bad(&s, 0);
}

#[test]
fn kron_negative_cancel_refund() {
    let f = fx();
    let k = &f.k;
    run_bad(&with_name(k5(&f, 0x81, f.ma), "C1 cancel: SIGHASH_ALL|ANYONECANPAY refused"), 0);
    run_bad(&with_name(k5(&f, 0x01, f.taker), "C2 cancel: wrong key"), 0);
    run_bad(&with_name(k6(&f, EXPIRY_DAA as u64 - 1), "C3 refund: ask before expiry"), 0);

    let mut s = with_name(k6(&f, EXPIRY_DAA as u64), "C4 refund: keeper redirects the ask's tokens to itself");
    let thief = pk(&f.taker);
    s.outputs[0].script_public_key = k.spk(&ks(&thief, T_ADDR, 10 * LOT));
    set_next(&mut s, vec![ks(&thief, T_ADDR, 10 * LOT)]);
    run_bad(&s, 0);

    let mut s = with_name(k6(&f, EXPIRY_DAA as u64), "C5 refund: keeper takes more than refundTip");
    s.outputs[0].value -= 1;
    run_bad(&s, 0);
}

/// Informational probes of the legacy token program itself (no assertions).
#[test]
fn kron_token_probes() {
    let f = fx();
    let k = &f.k;
    let a = cov(0xa1);
    let taker = pk(&f.taker);
    let c = carrier();

    // P1: is_minter=1 input of the same token in the tx (non-genuine here: created directly in the UTXO set).
    // Does the presence of a minter input let outputs exceed inputs (mint)?
    let mut s = with_name(k1(&f), "P1 probe: extra is_minter=1 token input (taker-owned) + 1000 extra tokens minted to taker");
    let mut minter = ks(&taker, T_ADDR, 0);
    minter.minter = 1;
    s.inputs.push(tok_in(k, c, &minter, TOKEN_COV));
    s.outputs[4].script_public_key = k.spk(&ks(&taker, T_ADDR, 5 * LOT));
    set_next(&mut s, vec![ks(a.as_bytes(), T_COVID, 6 * LOT), ks(&taker, T_ADDR, 5 * LOT)]);
    s.tok.wit = vec![1, 0];
    run_info(&s);

    // P2: same but ask input at index 0 (witness byte 0x00 = non-minimal script number?)
    run_info(&with_name(k5(&f, 0x01, f.ma), "P2 probe: witness index 0 (K5 cancel)"));
}

/// Prints the template hashes (prefix "", suffix = redeem[46..]) an indexer allow-list would pin.
#[test]
fn kron_template_hashes() {
    for f in ["kron_token_2433.bin", "kron_token_2732.bin"] {
        let k = Kron::load(f);
        let hex: String = k.hash.iter().map(|b| format!("{b:02x}")).collect();
        println!("TEMPLATE {f}: suffix_len={} template_hash={hex}", k.suffix.len());
    }
}
