//! DESIGN EXPERIMENTS, not a shipped suite (docs/design/borrow-topup.md).
//!
//! Part A: the KCC-20 borrow rules as the engine applies them (reference 3 / 3 from source and the pinned public-mint build).
//! Part B: the prototype `contracts/prototypes/borrow/KobBidTopup.sil` (KobBid with a top-up delivery) in whole transactions,
//!         with the masses and relay fee of a top-up fill against a new-UTXO fill.
//! Run: cargo test --release -p kob-tests --test borrow_topup_experiments -- --nocapture --test-threads=1
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

use common::{bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const EXT: [u8; 32] = [0xee; 32];
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const SCALE: i64 = 1_000;
const TIP: i64 = 100_000;
const EXPIRY: i64 = 400_000_000;
const REFUND_TIP: i64 = 3_000_000;
const NOW: u64 = 1_000_000;
const DC: i64 = KAS; // the default delivery carrier (1 KAS)
const CARRIER: i64 = KAS;
const P250: i64 = 250_000_000;
const P240: i64 = 240_000_000;
const THR: i64 = 999; // recvThreshold = minFill - 1
const TT: i64 = 2_000_000; // topupTip: 0.02 KAS

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

// ---------------------------------------------------------------- token

#[derive(Clone, Copy)]
struct St {
    amount: i64,
    owner: [u8; 32],
    scheme: u8,
    borrow: u8,
    guard: [u8; 32],
}
fn thr_guard(t: i64) -> [u8; 32] {
    let mut g = [0u8; 32];
    g[..8].copy_from_slice(&t.to_le_bytes());
    g
}
/// A key-owned plain holder.
fn plain(amount: i64, owner: &[u8; 32]) -> St {
    St { amount, owner: *owner, scheme: 0, borrow: 0, guard: [0; 32] }
}
/// A key-owned receive UTXO: borrow scheme amount-threshold with threshold `t`.
fn recv(amount: i64, owner: &[u8; 32], t: i64) -> St {
    St { amount, owner: *owner, scheme: 0, borrow: 1, guard: thr_guard(t) }
}
/// A custody owned by a covenant id.
fn custody(amount: i64, owner: Hash) -> St {
    St { amount, owner: owner.as_bytes(), scheme: 4, borrow: 0, guard: [0; 32] }
}
impl St {
    fn with(mut self, f: impl FnOnce(&mut St)) -> St {
        f(&mut self);
        self
    }
    fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0x08];
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.push(0x20);
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.scheme, 0x01, self.borrow, 0x20]);
        v.extend_from_slice(&self.guard);
        v.push(0x20);
        v.extend_from_slice(&EXT);
        v
    }
    fn val(&self) -> ArtifactValue {
        BTreeMap::from([
            ("amount".to_string(), int(self.amount)),
            ("owner".to_string(), bytes(&self.owner)),
            ("owner_scheme".to_string(), ArtifactValue::Byte(self.scheme)),
            ("borrow_scheme".to_string(), ArtifactValue::Byte(self.borrow)),
            ("borrow_guard".to_string(), bytes(&self.guard)),
            ("extension_commitment".to_string(), bytes(&EXT)),
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
    fn reference() -> Self {
        let code = common::contract_source("KCC20Ref");
        let art = compile_contract(
            &code,
            &[int(1000), bytes(&[3u8; 32]), ArtifactValue::Byte(4), ArtifactValue::Byte(0), bytes(&[0u8; 32]), bytes(&EXT)],
            CompileOptions::default(),
        )
        .expect("compile KCC20Ref");
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        Token { name: "KCC20Ref".into(), art, contract: None, prefix, suffix, hash }
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
fn tokens() -> Vec<Token> {
    vec![Token::reference(), Token::pinned(TemplateId::Kcc20PublicMint)]
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
                    Wit::CovId => vec![0x00],
                    Wit::P2pk(kp) => [vec![0x00], sig_of(kp)].concat(),
                    Wit::Borrow => vec![0x01],
                    Wit::Nothing => vec![0x00],
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
struct Cost {
    size: u64,
    compute: u64,
    transient: u64,
    storage: u64,
    fee: u64,
    inputs: usize,
    outputs: usize,
}
/// Every input passes; returns the masses and the relay-floor fee of the transaction with measured compute budgets.
fn ok(t: &Token, s: &Scn) -> Cost {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => panic!("{} @{}: input {i} ({}) failed: {e:?}", s.name, t.name, role_name(&s.inputs[i].role)),
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
        "OK   {:<74} @{:<15} in={} out={} size={:>5} compute={:>6} transient_norm={:>6} storage={:>6} relay_fee={:.5} KAS",
        s.name,
        t.name,
        tx.inputs.len(),
        tx.outputs.len(),
        m.size,
        m.compute,
        m.transient_normalized,
        m.storage,
        fee as f64 / 1e8
    );
    Cost {
        size: m.size,
        compute: m.compute,
        transient: m.transient_normalized,
        storage: m.storage,
        fee,
        inputs: tx.inputs.len(),
        outputs: tx.outputs.len(),
    }
}
/// Input `expect` must fail; every other input is reported.
fn bad(t: &Token, s: &Scn, expect: usize) {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: tx-level failure {e}", s.name));
    let r = &res[expect];
    assert!(r.is_err(), "{} @{}: expected input {expect} ({}) to REJECT but it passed", s.name, t.name, role_name(&s.inputs[expect].role));
    let others: Vec<String> = res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
    println!(
        "BAD  {:<74} @{:<15} rejected at in[{expect}] {} ({:?}){}",
        s.name,
        t.name,
        role_name(&s.inputs[expect].role),
        r.as_ref().unwrap_err(),
        if others.is_empty() { String::new() } else { format!("; also failing: {}", others.join(",")) }
    );
}
/// Only input `expect` fails: every other input (the order covenants included) accepts the transaction.
fn bad_only(t: &Token, s: &Scn, expect: usize) {
    let (tx, entries) = build(t, s, &[]);
    let res = execute(&tx, &entries).unwrap_or_else(|e| panic!("{}: tx-level failure {e}", s.name));
    for (i, r) in res.iter().enumerate() {
        assert_eq!(r.is_err(), i == expect, "{} @{}: input {i} ({}): {r:?}", s.name, t.name, role_name(&s.inputs[i].role));
    }
    println!("BAD  {:<74} @{:<15} rejected ONLY at in[{expect}] {}", s.name, t.name, role_name(&s.inputs[expect].role));
}

// ================================================================ Part A: the token program alone

/// A1..A9: who may top up, how many borrowed inputs, what the successor must satisfy.
#[test]
fn a_kcc20_borrow_rules() {
    let (holder, seller, other) = (keypair(), keypair(), keypair());
    let (h, sl, o) = (pk(&holder), pk(&seller), pk(&other));
    for t in tokens() {
        let r = recv(7_000, &h, THR);
        let n = 4_000;
        // A1 anyone tops up: the receive UTXO leads on the borrow path (no holder signature), a delegate the seller signs funds it
        let base = |name: &str| Scn {
            name: name.into(),
            inputs: vec![
                tok_leader(&t, KAS, &r, &[r.with(|s| s.amount += n), plain(10_000 - n, &sl)], Wit::Borrow),
                tok_delegate(&t, KAS, &plain(10_000, &sl), Wit::P2pk(seller)),
            ],
            outputs: vec![tok_out(&t, KAS, &r.with(|s| s.amount += n), 0), tok_out(&t, KAS, &plain(10_000 - n, &sl), 0)],
        };
        ok(&t, &base("A1 top-up by a third party: borrow leader + signed delegate"));

        // A2 the gain must exceed the threshold (strictly)
        let gain = |g: i64, name: &str| Scn {
            name: name.into(),
            inputs: vec![
                tok_leader(&t, KAS, &r, &[r.with(|s| s.amount += g), plain(10_000 - g, &sl)], Wit::Borrow),
                tok_delegate(&t, KAS, &plain(10_000, &sl), Wit::P2pk(seller)),
            ],
            outputs: vec![tok_out(&t, KAS, &r.with(|s| s.amount += g), 0), tok_out(&t, KAS, &plain(10_000 - g, &sl), 0)],
        };
        bad(&t, &gain(THR, "A2 gain == threshold"), 0);
        ok(&t, &gain(THR + 1, "A2 gain == threshold + 1"));
        bad(&t, &gain(0, "A2 gain 0 (pure outpoint churn)"), 0);

        // A3 the successor is the leader's FIRST authorised output: the same outputs in the other order are refused
        let mut s = base("A3 successor is the second token output");
        s.outputs.swap(0, 1);
        if let Role::TokLeader { next, .. } = &mut s.inputs[0].role {
            next.swap(0, 1);
        }
        bad(&t, &s, 0);

        // A4 the receive UTXO is not the first token input: it is a delegate, and a delegate needs its owner
        let s = Scn {
            name: "A4 receive UTXO as a delegate without the holder's signature".into(),
            inputs: vec![
                tok_leader(&t, KAS, &plain(10_000, &sl), &[r.with(|s| s.amount += n), plain(10_000 - n, &sl)], Wit::P2pk(seller)),
                tok_delegate(&t, KAS, &r, Wit::Nothing),
            ],
            outputs: vec![tok_out(&t, KAS, &r.with(|s| s.amount += n), 0), tok_out(&t, KAS, &plain(10_000 - n, &sl), 0)],
        };
        bad(&t, &s, 1);

        // A5 two receive UTXOs (two buyers) in one transaction: only the leader borrows, the second is refused
        let r2 = recv(500, &o, THR);
        let s = Scn {
            name: "A5 a second borrowed input of the same token (another buyer)".into(),
            inputs: vec![
                tok_leader(
                    &t,
                    KAS,
                    &r,
                    &[r.with(|s| s.amount += 2_000), r2.with(|s| s.amount += 2_000), plain(6_000, &sl)],
                    Wit::Borrow,
                ),
                tok_delegate(&t, KAS, &r2, Wit::Nothing),
                tok_delegate(&t, KAS, &plain(10_000, &sl), Wit::P2pk(seller)),
            ],
            outputs: vec![
                tok_out(&t, KAS, &r.with(|s| s.amount += 2_000), 0),
                tok_out(&t, KAS, &r2.with(|s| s.amount += 2_000), 0),
                tok_out(&t, KAS, &plain(6_000, &sl), 0),
            ],
        };
        bad_only(&t, &s, 1);

        // A6 KAS on the successor: at least the leader's
        let mut s = base("A6 successor carries 1 sompi less than the receive UTXO");
        s.outputs[0].value -= 1;
        bad(&t, &s, 0);
        let mut s = base("A6 successor carries more KAS");
        s.outputs[0].value += 5;
        s.inputs[1].entry = UtxoEntry::new(KAS as u64 + 5, s.inputs[1].entry.script_public_key.clone(), 1_000, false, Some(TOKEN_COV));
        ok(&t, &s);

        // A7 the successor keeps owner, owner scheme, borrow scheme and guard
        for (name, f) in [
            ("A7 successor owner changed", Box::new(|s: &mut St| s.owner = o) as Box<dyn Fn(&mut St)>),
            ("A7 successor borrow scheme disabled", Box::new(|s: &mut St| s.borrow = 0)),
            ("A7 successor threshold lowered", Box::new(|s: &mut St| s.guard = thr_guard(1))),
            ("A7 successor threshold raised", Box::new(|s: &mut St| s.guard = thr_guard(1_000_000))),
            ("A7 successor owner scheme changed", Box::new(|s: &mut St| s.scheme = 1)),
        ] {
            let succ = r.with(|s| s.amount += n).with(|s| f(s));
            let s = Scn {
                name: name.into(),
                inputs: vec![
                    tok_leader(&t, KAS, &r, &[succ, plain(10_000 - n, &sl)], Wit::Borrow),
                    tok_delegate(&t, KAS, &plain(10_000, &sl), Wit::P2pk(seller)),
                ],
                outputs: vec![tok_out(&t, KAS, &succ, 0), tok_out(&t, KAS, &plain(10_000 - n, &sl), 0)],
            };
            bad(&t, &s, 0);
        }

        // A8 a plain (borrow disabled) holder cannot be topped up by a third party
        let p = plain(7_000, &h);
        let s = Scn {
            name: "A8 borrow path on a plain holder".into(),
            inputs: vec![
                tok_leader(&t, KAS, &p, &[p.with(|s| s.amount += n), plain(10_000 - n, &sl)], Wit::Borrow),
                tok_delegate(&t, KAS, &plain(10_000, &sl), Wit::P2pk(seller)),
            ],
            outputs: vec![tok_out(&t, KAS, &p.with(|s| s.amount += n), 0), tok_out(&t, KAS, &plain(10_000 - n, &sl), 0)],
        };
        bad(&t, &s, 0);

        // A9 option A: the holder spends a receive UTXO on the owner path; the change keeps the borrow settings, the payee is plain
        let s = Scn {
            name: "A9 owner-path spend of a receive UTXO, change keeps the settings".into(),
            inputs: vec![tok_leader(&t, 2 * KAS, &r, &[plain(3_000, &o), r.with(|s| s.amount = 4_000)], Wit::P2pk(holder))],
            outputs: vec![tok_out(&t, KAS, &plain(3_000, &o), 0), tok_out(&t, KAS, &r.with(|s| s.amount = 4_000), 0)],
        };
        ok(&t, &s);
        // A9b the program itself lets the owner merge inputs of different borrow settings into any state (the wallet rule decides)
        let s = Scn {
            name: "A9b owner merges a receive UTXO and a plain one into a plain one".into(),
            inputs: vec![
                tok_leader(&t, KAS, &r, &[plain(8_000, &h)], Wit::P2pk(holder)),
                tok_delegate(&t, KAS, &plain(1_000, &h), Wit::P2pk(holder)),
            ],
            outputs: vec![tok_out(&t, 2 * KAS, &plain(8_000, &h), 0)],
        };
        ok(&t, &s);
        // A9c a receive UTXO as an owner-signed delegate (a sell where another input leads)
        let s = Scn {
            name: "A9c receive UTXO as an owner-signed delegate".into(),
            inputs: vec![
                tok_leader(&t, KAS, &plain(1_000, &h), &[plain(8_000, &o)], Wit::P2pk(holder)),
                tok_delegate(&t, KAS, &r, Wit::P2pk(holder)),
            ],
            outputs: vec![tok_out(&t, 2 * KAS, &plain(8_000, &o), 0)],
        };
        ok(&t, &s);
    }
}

// ================================================================ Part B: the prototype bid

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
const BID_COV: Hash = Hash::from_bytes([0xb1; 32]);
const BID2_COV: Hash = Hash::from_bytes([0xb2; 32]);
const ASK_COV: Hash = Hash::from_bytes([0xa1; 32]);
const QTY: i64 = 10; // whole tokens the bid is funded for

/// The prototype bid: price 2.50, tip 0.001, minFill one whole token, GTC, delivery carrier 1 KAS.
fn bid(f: &Fx, maker: &Keypair, recv_threshold: i64) -> SilAbiArtifact {
    let mut a = vec![bytes(&pk(maker))];
    a.extend(f.t.tok_args());
    a.extend([
        bytes(&EXT),
        int(SCALE),
        int(SCALE), // minFill
        int(P250),
        int(TIP),
        int(0), // GTC
        int(0),
        int(EXPIRY),
        int(REFUND_TIP),
        int(0), // reserve
        int(DC),
        int(0),
        int(0),
        int(0),
        int(0),
        int(0),
        int(recv_threshold),
        int(TT),
    ]);
    compile_contract(&common::contract_source("KobBidTopup"), &a, CompileOptions::default()).expect("compile KobBidTopup")
}
fn bid_value() -> i64 {
    QTY * (P250 + TIP) + DC
}
/// What `whole` whole tokens cost the bid (used == allIn at whole tokens).
fn spend(whole: i64) -> i64 {
    whole * (P250 + TIP)
}
fn fill_args(n: i64, tpl_in: usize, form: i64) -> Vec<ArtifactValue> {
    vec![bytes(&n.to_le_bytes()), int(tpl_in as i64), int(0), int(form)]
}
fn ask(f: &Fx, left_whole: i64) -> SilAbiArtifact {
    let mut a = vec![bytes(&pk(&f.asker))];
    a.extend(f.t.tok_args());
    a.extend([
        int(SCALE),
        int(SCALE),
        int(P240),
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
        int(left_whole * SCALE),
        bytes(&EXT),
    ]);
    compile_contract(&common::contract_source("KobAsk"), &a, CompileOptions::default()).expect("compile KobAsk")
}
fn spk_of(a: &SilAbiArtifact) -> ScriptPublicKey {
    pay_to_script_hash_script(&bytecode(a))
}
/// Closes the scenario: the last output takes the KAS left over (no fee: the engine does not look at it).
fn close(mut s: Scn, to: &Keypair) -> Scn {
    let vin: i64 = s.inputs.iter().map(|i| i.entry.amount as i64).sum();
    let vout: i64 = s.outputs.iter().map(|o| o.value as i64).sum();
    s.outputs.push(out(vin - vout, p2pk_spk(&pk(to)), None));
    s
}

/// A taker sells `w` whole tokens into the bid, delivered as a NEW token UTXO (today's shape; `thr` 0 = plain, > 0 = receive UTXO).
fn taker_new(f: &Fx, thr: i64, w: i64) -> Scn {
    let (m, tk) = (pk(&f.maker), pk(&f.taker));
    let b = bid(f, &f.maker, thr);
    let n = w * SCALE;
    let d = if thr > 0 { recv(n, &m, thr) } else { plain(n, &m) };
    let v = bid_value();
    close(
        Scn {
            name: format!("B taker sells {w} into the bid, NEW delivery (recvThreshold {thr})"),
            inputs: vec![
                call(&b, "fill", fill_args(n, 1, i64::from(thr > 0)), "bid.fill", v, BID_COV),
                tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[d, plain(10_000 - n, &tk)], Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs: vec![
                tok_out(&f.t, DC, &d, 1),
                out(v - spend(w) - DC, spk_of(&b), Some((0, BID_COV))),
                tok_out(&f.t, CARRIER, &plain(10_000 - n, &tk), 1),
            ],
        },
        &f.taker,
    )
}
/// The same sale delivered as a TOP-UP of the maker's receive UTXO (7,000 units, 1 KAS).
fn taker_topup(f: &Fx, w: i64) -> Scn {
    let (m, tk) = (pk(&f.maker), pk(&f.taker));
    let b = bid(f, &f.maker, THR);
    let n = w * SCALE;
    let r = recv(7_000, &m, THR);
    let succ = r.with(|s| s.amount += n);
    let v = bid_value();
    close(
        Scn {
            name: format!("B taker sells {w} into the bid, TOP-UP of the receive UTXO"),
            inputs: vec![
                call(&b, "fill", fill_args(n, 1, 2), "bid.fill", v, BID_COV),
                tok_leader(&f.t, CARRIER, &r, &[succ, plain(10_000 - n, &tk)], Wit::Borrow),
                tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs: vec![
                tok_out(&f.t, CARRIER, &succ, 1),
                out(v - spend(w) - TT, spk_of(&b), Some((0, BID_COV))),
                tok_out(&f.t, CARRIER, &plain(10_000 - n, &tk), 1),
            ],
        },
        &f.taker,
    )
}
/// A matcher crosses the bid with a resting KobAsk (10 whole in custody, `w` filled); `topup` chooses the delivery.
fn cross(f: &Fx, w: i64, topup: bool) -> Scn {
    let m = pk(&f.maker);
    let b = bid(f, &f.maker, THR);
    let (a, a_next) = (ask(f, 10), ask(f, 10 - w));
    let n = w * SCALE;
    let v = bid_value();
    let cust = custody(10_000, ASK_COV);
    let rest = custody(10_000 - n, ASK_COV);
    let pay = w * (P240 - TIP);
    let r = recv(7_000, &m, THR);
    let (inputs, lead, d, dv, cont) = if topup {
        let succ = r.with(|s| s.amount += n);
        (
            vec![
                call(&b, "fill", fill_args(n, 2, 2), "bid.fill", v, BID_COV),
                call(&a, "settle", vec![bytes(&n.to_le_bytes()), int(3), int(4), int(0)], "ask.settle", CARRIER, ASK_COV),
                tok_leader(&f.t, CARRIER, &r, &[succ, rest], Wit::Borrow),
                tok_delegate(&f.t, CARRIER, &cust, Wit::CovId),
                p2pk_in(&f.matcher, KAS),
            ],
            2,
            succ,
            CARRIER,
            v - spend(w) - TT,
        )
    } else {
        let d = recv(n, &m, THR);
        (
            vec![
                call(&b, "fill", fill_args(n, 2, 1), "bid.fill", v, BID_COV),
                call(&a, "settle", vec![bytes(&n.to_le_bytes()), int(2), int(4), int(0)], "ask.settle", CARRIER, ASK_COV),
                tok_leader(&f.t, CARRIER, &cust, &[d, rest], Wit::CovId),
                p2pk_in(&f.matcher, KAS),
            ],
            2,
            d,
            DC,
            v - spend(w) - DC,
        )
    };
    close(
        Scn {
            name: format!("B matcher crosses bid x ask ({w} whole), {}", if topup { "TOP-UP" } else { "NEW delivery" }),
            inputs,
            outputs: vec![
                tok_out(&f.t, dv, &d, lead),
                out(pay, p2pk_spk(&pk(&f.asker)), None),
                out(cont, spk_of(&b), Some((0, BID_COV))),
                out(CARRIER, spk_of(&a_next), Some((1, ASK_COV))),
                tok_out(&f.t, CARRIER, &rest, lead),
            ],
        },
        &f.matcher,
    )
}
fn set_out_state(f: &Fx, s: &mut Scn, out_idx: usize, leader: usize, st: St) {
    s.outputs[out_idx].script_public_key = f.t.spk(&st);
    if let Role::TokLeader { next, .. } = &mut s.inputs[leader].role {
        next[0] = st.val();
    }
}

/// B1: positives with costs. The top-up form is accepted by the prototype bid, the token program and a KobAsk in the same
/// transaction; its masses and relay fee are printed next to the new-UTXO form.
#[test]
fn b_topup_fill_positive_and_costs() {
    for t in tokens() {
        let f = fx(t);
        let plain_new = ok(&f.t, &taker_new(&f, 0, 4));
        let recv_new = ok(&f.t, &taker_new(&f, THR, 4));
        let top = ok(&f.t, &taker_topup(&f, 4));
        let x_new = ok(&f.t, &cross(&f, 4, false));
        let x_top = ok(&f.t, &cross(&f, 4, true));
        println!(
            "COST @{}: taker sell   new(plain) fee {} / new(receive) fee {} / top-up fee {} sompi (top-up {:+}); storage {} / {} / {}",
            f.t.name,
            plain_new.fee,
            recv_new.fee,
            top.fee,
            top.fee as i64 - plain_new.fee as i64,
            plain_new.storage,
            recv_new.storage,
            top.storage
        );
        println!(
            "COST @{}: bid x ask    new fee {} / top-up fee {} sompi (top-up {:+}); size {} / {}; storage {} / {}",
            f.t.name,
            x_new.fee,
            x_top.fee,
            x_top.fee as i64 - x_new.fee as i64,
            x_new.size,
            x_top.size,
            x_new.storage,
            x_top.storage
        );
        // the terminal fill (all 10 whole): the carrier the top-up did not need, less the tip, is under REFUND_MIN and rides on the delivery
        let (m, tk) = (pk(&f.maker), pk(&f.taker));
        let b = bid(&f, &f.maker, THR);
        let r = recv(7_000, &m, THR);
        let succ = r.with(|s| s.amount += 10_000);
        let s = Scn {
            name: "B terminal top-up fill: the unspent carrier less the tip rides on the delivery".into(),
            inputs: vec![
                call(&b, "fill", fill_args(10_000, 1, 2), "bid.fill", bid_value(), BID_COV),
                tok_leader(&f.t, CARRIER, &r, &[succ], Wit::Borrow),
                tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs: vec![
                tok_out(&f.t, CARRIER + DC - TT, &succ, 1),
                out(spend(10) + CARRIER + KAS + TT, p2pk_spk(&tk), None),
            ],
        };
        ok(&f.t, &s);
        let mut short = s.clone();
        short.name = "B terminal top-up fill leaving 1 sompi less on the delivery".into();
        short.outputs[0].value -= 1;
        bad(&f.t, &short, 0);
    }
}

/// B2: what the prototype bid refuses, and what only the token program refuses.
#[test]
fn b_topup_fill_negatives() {
    for t in tokens() {
        let f = fx(t);
        let (m, tk, o) = (pk(&f.maker), pk(&f.taker), pk(&f.maker2));
        let r = recv(7_000, &m, THR);
        let n = 4_000;
        // N1 one unit short on the delivery (the taker keeps it)
        let mut s = taker_topup(&f, 4);
        s.name = "N1 top-up delivers have + n - 1".into();
        set_out_state(&f, &mut s, 0, 1, r.with(|s| s.amount += n - 1));
        s.outputs[2].script_public_key = f.t.spk(&plain(10_000 - n + 1, &tk));
        if let Role::TokLeader { next, .. } = &mut s.inputs[1].role {
            next[1] = plain(10_000 - n + 1, &tk).val();
        }
        bad_only(&f.t, &s, 0);

        // N2 the "receive UTXO" is somebody else's (the tokens would land on another key)
        let ro = recv(7_000, &o, THR);
        let mut s = taker_topup(&f, 4);
        s.name = "N2 top-up into another key's receive UTXO".into();
        s.inputs[1] = tok_leader(&f.t, CARRIER, &ro, &[ro.with(|s| s.amount += n), plain(10_000 - n, &tk)], Wit::Borrow);
        s.outputs[0] = tok_out(&f.t, CARRIER, &ro.with(|s| s.amount += n), 1);
        bad_only(&f.t, &s, 0);

        // N3 a receive UTXO of the maker with a lower threshold than the order asks for (cheap to churn)
        let rl = recv(7_000, &m, 10);
        let mut s = taker_topup(&f, 4);
        s.name = "N3 top-up into the maker's UTXO with threshold 10 < recvThreshold".into();
        s.inputs[1] = tok_leader(&f.t, CARRIER, &rl, &[rl.with(|s| s.amount += n), plain(10_000 - n, &tk)], Wit::Borrow);
        s.outputs[0] = tok_out(&f.t, CARRIER, &rl.with(|s| s.amount += n), 1);
        bad_only(&f.t, &s, 0);
        // N3b a higher threshold is accepted (the maker's other orders may have set it)
        let rh = recv(7_000, &m, 3_000);
        let mut s = taker_topup(&f, 4);
        s.name = "N3b top-up into the maker's UTXO with threshold 3000 > recvThreshold".into();
        s.inputs[1] = tok_leader(&f.t, CARRIER, &rh, &[rh.with(|s| s.amount += n), plain(10_000 - n, &tk)], Wit::Borrow);
        s.outputs[0] = tok_out(&f.t, CARRIER, &rh.with(|s| s.amount += n), 1);
        ok(&f.t, &s);
        // N3c a non-canonical guard (bytes after the threshold) is refused by the bid
        let mut rg = recv(7_000, &m, THR);
        rg.guard[20] = 1;
        let mut s = taker_topup(&f, 4);
        s.name = "N3c top-up into a UTXO whose guard has bytes after the threshold".into();
        s.inputs[1] = tok_leader(&f.t, CARRIER, &rg, &[rg.with(|s| s.amount += n), plain(10_000 - n, &tk)], Wit::Borrow);
        s.outputs[0] = tok_out(&f.t, CARRIER, &rg.with(|s| s.amount += n), 1);
        bad_only(&f.t, &s, 0);

        // N4 the KAS of the receive UTXO: 1 sompi skimmed
        let mut s = taker_topup(&f, 4);
        s.name = "N4 successor carries 1 sompi less than the receive UTXO".into();
        s.outputs[0].value -= 1;
        s.outputs[3].value += 1;
        bad(&f.t, &s, 0);
        bad(&f.t, &s, 1);

        // N5 the carrier must stay in the escrow on a top-up
        let mut s = taker_topup(&f, 4);
        s.name = "N5 top-up continuation 1 sompi short (more than topupTip taken)".into();
        s.outputs[1].value -= 1;
        s.outputs[3].value += 1;
        bad_only(&f.t, &s, 0);

        // N6 an order that did not opt in (recvThreshold 0) refuses the top-up form and a receive-state delivery
        let b0 = bid(&f, &f.maker, 0);
        let mut s = taker_topup(&f, 4);
        s.name = "N6 top-up of an order with recvThreshold 0".into();
        s.inputs[0] = call(&b0, "fill", fill_args(n, 1, 2), "bid.fill", bid_value(), BID_COV);
        s.outputs[1].script_public_key = spk_of(&b0);
        bad_only(&f.t, &s, 0);
        let mut s = taker_new(&f, THR, 4);
        s.name = "N6b receive-state delivery from an order with recvThreshold 0".into();
        s.inputs[0] = call(&b0, "fill", fill_args(n, 1, 1), "bid.fill", bid_value(), BID_COV);
        s.outputs[1].script_public_key = spk_of(&b0);
        bad_only(&f.t, &s, 0);
        // N6c the opted-in order still accepts a plain delivery (form 0: what an unchanged router execution pins)
        let mut s = taker_new(&f, 0, 4);
        s.name = "N6c plain delivery (form 0) from an order with recvThreshold > 0".into();
        let b = bid(&f, &f.maker, THR);
        s.inputs[0] = call(&b, "fill", fill_args(n, 1, 0), "bid.fill", bid_value(), BID_COV);
        s.outputs[1].script_public_key = spk_of(&b);
        ok(&f.t, &s);
        // N6d a form-0 fill cannot deliver a receive state
        let mut s = taker_new(&f, THR, 4);
        s.name = "N6d receive-state delivery claimed as form 0".into();
        s.inputs[0] = call(&b, "fill", fill_args(n, 1, 0), "bid.fill", bid_value(), BID_COV);
        bad_only(&f.t, &s, 0);

        // N7 a plain UTXO of the maker named as the receive UTXO (it would need the maker's signature anyway)
        let p = plain(7_000, &m);
        let mut s = taker_topup(&f, 4);
        s.name = "N7 top-up naming a plain UTXO of the maker".into();
        s.inputs[1] = tok_leader(&f.t, CARRIER, &p, &[p.with(|s| s.amount += n), plain(10_000 - n, &tk)], Wit::Borrow);
        s.outputs[0] = tok_out(&f.t, CARRIER, &p.with(|s| s.amount += n), 1);
        bad(&f.t, &s, 0);

        // N8 the top-up names the taker's own token input as the receive UTXO
        let mut s = taker_topup(&f, 4);
        s.name = "N8 tokenTplIn points at the taker's input, not the receive UTXO".into();
        s.inputs[0] = call(&b, "fill", fill_args(n, 2, 2), "bid.fill", bid_value(), BID_COV);
        bad_only(&f.t, &s, 0);

        // N9 a fill at or below the UTXO's threshold cannot top up (the token refuses; the bid alone would accept)
        let rh = recv(7_000, &m, 4_000);
        let mut s = taker_topup(&f, 4);
        s.name = "N9 top-up of n == the UTXO's threshold".into();
        s.inputs[1] = tok_leader(&f.t, CARRIER, &rh, &[rh.with(|s| s.amount += n), plain(10_000 - n, &tk)], Wit::Borrow);
        s.outputs[0] = tok_out(&f.t, CARRIER, &rh.with(|s| s.amount += n), 1);
        bad_only(&f.t, &s, 1);
    }
}

/// B3: batches. Two bids of one token in one transaction.
#[test]
fn b_topup_batches() {
    for t in tokens() {
        let f = fx(t);
        let (m, m2, tk) = (pk(&f.maker), pk(&f.maker2), pk(&f.taker));
        let (b1, b2) = (bid(&f, &f.maker, THR), bid(&f, &f.maker2, THR));
        let v = bid_value();
        let r1 = recv(7_000, &m, THR);
        let r2 = recv(500, &m2, THR);
        // two buyers: the first is topped up, the second gets a new receive UTXO; 3 token outputs = the 3 / 3 limit
        let s = close(
            Scn {
                name: "B3 two buyers: bid 0 topped up, bid 1 delivered new (3 token outputs)".into(),
                inputs: vec![
                    call(&b1, "fill", fill_args(2_000, 2, 2), "bid1.fill", v, BID_COV),
                    call(&b2, "fill", fill_args(3_000, 2, 1), "bid2.fill", v, BID2_COV),
                    tok_leader(
                        &f.t,
                        CARRIER,
                        &r1,
                        &[r1.with(|s| s.amount += 2_000), recv(3_000, &m2, THR), plain(5_000, &tk)],
                        Wit::Borrow,
                    ),
                    tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                    p2pk_in(&f.taker, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, CARRIER, &r1.with(|s| s.amount += 2_000), 2),
                    tok_out(&f.t, DC, &recv(3_000, &m2, THR), 2),
                    out(v - spend(2) - TT, spk_of(&b1), Some((0, BID_COV))),
                    out(v - spend(3) - DC, spk_of(&b2), Some((1, BID2_COV))),
                    tok_out(&f.t, CARRIER, &plain(5_000, &tk), 2),
                ],
            },
            &f.taker,
        );
        ok(&f.t, &s);

        // two buyers, both topped up: the bids accept, the token program refuses the second receive UTXO (a delegate)
        let s = close(
            Scn {
                name: "B3 two buyers, both topped up".into(),
                inputs: vec![
                    call(&b1, "fill", fill_args(2_000, 2, 2), "bid1.fill", v, BID_COV),
                    call(&b2, "fill", fill_args(3_000, 3, 2), "bid2.fill", v, BID2_COV),
                    tok_leader(
                        &f.t,
                        CARRIER,
                        &r1,
                        &[r1.with(|s| s.amount += 2_000), r2.with(|s| s.amount += 3_000), plain(5_000, &tk)],
                        Wit::Borrow,
                    ),
                    tok_delegate(&f.t, CARRIER, &r2, Wit::Nothing),
                    tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                    p2pk_in(&f.taker, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, CARRIER, &r1.with(|s| s.amount += 2_000), 2),
                    tok_out(&f.t, CARRIER, &r2.with(|s| s.amount += 3_000), 2),
                    out(v - spend(2) - TT, spk_of(&b1), Some((0, BID_COV))),
                    out(v - spend(3) - TT, spk_of(&b2), Some((1, BID2_COV))),
                    tok_out(&f.t, CARRIER, &plain(5_000, &tk), 2),
                ],
            },
            &f.taker,
        );
        bad_only(&f.t, &s, 3);

        // one buyer, two bids, both naming the same receive UTXO: each bid sees have + its n. Funded with n1 + n2 only, the token
        // program refuses (conservation); the bids alone accept, so the maker's safety here rests on the token's conservation
        let b1b = bid(&f, &f.maker, THR);
        let two = |fund: i64, name: &str| {
            close(
                Scn {
                    name: name.into(),
                    inputs: vec![
                        call(&b1, "fill", fill_args(2_000, 2, 2), "bid1.fill", v, BID_COV),
                        call(&b1b, "fill", fill_args(3_000, 2, 2), "bid1b.fill", v, BID2_COV),
                        tok_leader(
                            &f.t,
                            CARRIER,
                            &r1,
                            &[r1.with(|s| s.amount += 2_000), r1.with(|s| s.amount += 3_000), plain(fund - 12_000, &tk)],
                            Wit::Borrow,
                        ),
                        tok_delegate(&f.t, CARRIER, &plain(fund, &tk), Wit::P2pk(f.taker)),
                        p2pk_in(&f.taker, 3 * KAS),
                    ],
                    outputs: vec![
                        tok_out(&f.t, CARRIER, &r1.with(|s| s.amount += 2_000), 2),
                        tok_out(&f.t, CARRIER, &r1.with(|s| s.amount += 3_000), 2),
                        out(v - spend(2) - TT, spk_of(&b1), Some((0, BID_COV))),
                        out(v - spend(3) - TT, spk_of(&b1b), Some((1, BID2_COV))),
                        tok_out(&f.t, CARRIER, &plain(fund - 12_000, &tk), 2),
                    ],
                },
                &f.taker,
            )
        };
        // 7000 (receive) + fund = 9000 + 10000 + (fund - 12000): balanced only when the seller ALSO gives the 7,000 a second time
        ok(&f.t, &two(13_000, "B3 one buyer, two bids on one receive UTXO, seller funds have + n1 + n2 (a gift)"));
        // the honest amount n1 + n2 = 5,000 cannot fund it: no state assignment conserves (shown with the outputs the bids demand)
        let mut s = two(13_000, "B3 one buyer, two bids on one receive UTXO, seller funds n1 + n2 only");
        s.inputs[3] = tok_delegate(&f.t, CARRIER, &plain(6_000, &tk), Wit::P2pk(f.taker));
        bad_only(&f.t, &s, 2);
    }
}

/// C: the marginal cost of a top-up on the larger published builds, and the size of the prototype against KobBid.
#[test]
fn c_costs_on_larger_programs_and_sizes() {
    for id in [TemplateId::KForgeToken7214, TemplateId::KaspaRocketToken11458] {
        let f = fx(Token::pinned(id));
        let x_new = ok(&f.t, &cross(&f, 4, false));
        let x_top = ok(&f.t, &cross(&f, 4, true));
        println!(
            "COST @{}: bid x ask    new fee {} / top-up fee {} sompi (top-up {:+}); storage {} / {}",
            f.t.name,
            x_new.fee,
            x_top.fee,
            x_top.fee as i64 - x_new.fee as i64,
            x_new.storage,
            x_top.storage
        );
    }
    let f = fx(Token::reference());
    let proto = bytecode(&bid(&f, &f.maker, THR));
    let mut a = vec![bytes(&pk(&f.maker))];
    a.extend(f.t.tok_args());
    a.extend([bytes(&EXT), int(SCALE), int(SCALE), int(P250), int(TIP)]);
    a.extend([int(0), int(0), int(EXPIRY), int(REFUND_TIP), int(0), int(DC), int(0), int(0), int(0), int(0), int(0)]);
    a.extend(MODE0);
    let today = bytecode(&compile_contract(&common::contract_source("KobBid"), &a, CompileOptions::default()).expect("compile KobBid"));
    println!("SIZE KobBid {} B, KobBidTopup {} B ({:+} B)", today.len(), proto.len(), proto.len() as i64 - today.len() as i64);
}

/// D: why the top-up reads its receive UTXO behind a P2SH check. With a look-alike input of the token's covenant id (possible
/// only in a token whose genesis group hides a non-template output), a top-up could name a state of 0 units as "the receive
/// UTXO" while the real one (1,000 units) leads on the borrow path: one output of 5,000 then satisfies the token (gain 4,000 >
/// threshold) and an unchecked bid (0 + 5,000), and the seller keeps 1,000 of the maker's units. The reference build accepts
/// every other input of that transaction; the prototype bid refuses it.
#[test]
fn d_lookalike_receive_state_is_refused_by_the_bid() {
    let f = fx(Token::reference());
    let (m, tk) = (pk(&f.maker), pk(&f.taker));
    let b = bid(&f, &f.maker, THR);
    let v = bid_value();
    let real = recv(1_000, &m, THR);
    let fake_redeem = f.t.redeem(&recv(0, &m, THR));
    let succ = recv(5_000, &m, THR);
    let fake = Inp {
        // spk: OP_DROP OP_TRUE (not the P2SH of the script its signature script pushes)
        entry: UtxoEntry::new(KAS as u64, ScriptPublicKey::new(0, vec![0x75, 0x51].into()), 1_000, false, Some(TOKEN_COV)),
        role: Role::RawSs { ss: push(&fake_redeem), name: "lookalike" },
    };
    let s = close(
        Scn {
            name: "D look-alike receive state (0 units) while the real receive UTXO (1,000) leads".into(),
            inputs: vec![
                call(&b, "fill", fill_args(5_000, 2, 2), "bid.fill", v, BID_COV),
                tok_leader(&f.t, CARRIER, &real, &[succ, plain(6_000, &tk)], Wit::Borrow),
                fake,
                tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs: vec![
                tok_out(&f.t, CARRIER, &succ, 1),
                out(v - spend(5) - TT, spk_of(&b), Some((0, BID_COV))),
                tok_out(&f.t, CARRIER, &plain(6_000, &tk), 1),
            ],
        },
        &f.taker,
    );
    bad_only(&f.t, &s, 0);
}

// ================================================================ Part F: the accumulating bid (KobBidAcc)

const BOUGHT_AT: usize = 287;

/// KobBidAcc with the terms of `bid`: price 2.50, tip 0.001, minFill one whole token, GTC, carrier 1 KAS.
fn acc(f: &Fx, maker: &Keypair, bought: i64, expiry: i64) -> SilAbiArtifact {
    let mut a = today_args(f, maker, expiry);
    a.push(int(bought));
    compile_contract(&common::contract_source("KobBidAcc"), &a, CompileOptions::default()).expect("compile KobBidAcc")
}
/// The shipped KobBid with the same terms, in delivery mode 0 (the cost baseline; since the bid delivery modes the shipped
/// KobBid carries the delivery block, docs/design/bid-delivery-spec.md section 2).
fn today(f: &Fx, maker: &Keypair) -> SilAbiArtifact {
    let mut a = today_args(f, maker, EXPIRY);
    a.extend(MODE0);
    compile_contract(&common::contract_source("KobBid"), &a, CompileOptions::default()).expect("compile KobBid")
}
/// The delivery block of the shipped KobBid in mode 0: mode, recvThreshold, topupTip, bought.
const MODE0: [ArtifactValue; 4] = [ArtifactValue::Int(0), ArtifactValue::Int(0), ArtifactValue::Int(0), ArtifactValue::Int(0)];
/// fill(nb, tokIn, t, form) of the shipped KobBid (form 0: read in mode 2 only).
fn today_fill(n: i64, idx: usize) -> Vec<ArtifactValue> {
    let mut a = args3(n, idx);
    a.push(int(0));
    a
}
fn today_args(f: &Fx, maker: &Keypair, expiry: i64) -> Vec<ArtifactValue> {
    let mut a = vec![bytes(&pk(maker))];
    a.extend(f.t.tok_args());
    a.extend([bytes(&EXT), int(SCALE), int(SCALE), int(P250), int(TIP), int(0), int(0), int(expiry), int(REFUND_TIP)]);
    a.extend([int(0), int(DC), int(0), int(0), int(0), int(0), int(0)]);
    a
}
fn args3(n: i64, idx: usize) -> Vec<ArtifactValue> {
    vec![bytes(&n.to_le_bytes()), int(idx as i64), int(0)]
}
/// A taker sells into an accumulating bid that already holds `cust_in` (input 1, leading by its covenant id): output 0 is
/// `out0`, the continuation (if any) is `cont` with its value, the seller brings `seller` units and keeps `change`.
#[allow(clippy::too_many_arguments)]
fn acc_later(
    f: &Fx,
    name: &str,
    b: &SilAbiArtifact,
    n: i64,
    v_in: i64,
    cust_in: St,
    out0: St,
    cont: Option<(&SilAbiArtifact, i64)>,
    seller: i64,
    change: i64,
) -> Scn {
    let tk = pk(&f.taker);
    let mut outputs = vec![tok_out(&f.t, DC, &out0, 1)];
    if let Some((c, v)) = cont {
        outputs.push(out(v, spk_of(c), Some((0, BID_COV))));
    }
    outputs.push(tok_out(&f.t, CARRIER, &plain(change, &tk), 1));
    close(
        Scn {
            name: name.into(),
            inputs: vec![
                call(b, "fill", args3(n, 1), "bid.fill", v_in, BID_COV),
                tok_leader(&f.t, DC, &cust_in, &[out0, plain(change, &tk)], Wit::CovId),
                tok_delegate(&f.t, CARRIER, &plain(seller, &tk), Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs,
        },
        &f.taker,
    )
}
/// The first fill of an accumulating bid (bought 0) funded with `v`: the seller's token leads, output 0 is `out0` with the carrier.
fn acc_first(f: &Fx, name: &str, n: i64, out0: St, v: i64) -> Scn {
    let tk = pk(&f.taker);
    let (b, next) = (acc(f, &f.maker, 0, EXPIRY), acc(f, &f.maker, n, EXPIRY));
    close(
        Scn {
            name: name.into(),
            inputs: vec![
                call(&b, "fill", args3(n, 1), "bid.fill", v, BID_COV),
                tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[out0, plain(10_000 - n, &tk)], Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs: vec![
                tok_out(&f.t, DC, &out0, 1),
                out(v - n / SCALE * (P250 + TIP) - DC, spk_of(&next), Some((0, BID_COV))),
                tok_out(&f.t, CARRIER, &plain(10_000 - n, &tk), 1),
            ],
        },
        &f.taker,
    )
}
/// Today's fill of the shipped KobBid by a taker: the cost baseline.
fn today_taker(f: &Fx, w: i64) -> Scn {
    let (m, tk) = (pk(&f.maker), pk(&f.taker));
    let b = today(f, &f.maker);
    let n = w * SCALE;
    let v = bid_value();
    close(
        Scn {
            name: format!("F today: taker sells {w} into the shipped KobBid"),
            inputs: vec![
                call(&b, "fill", today_fill(n, 1), "bid.fill", v, BID_COV),
                tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[plain(n, &m), plain(10_000 - n, &tk)], Wit::P2pk(f.taker)),
                p2pk_in(&f.taker, KAS),
            ],
            outputs: vec![
                tok_out(&f.t, DC, &plain(n, &m), 1),
                out(v - spend(w) - DC, spk_of(&b), Some((0, BID_COV))),
                tok_out(&f.t, CARRIER, &plain(10_000 - n, &tk), 1),
            ],
        },
        &f.taker,
    )
}
/// A matcher crosses a bid with a resting KobAsk for `w` whole: `acc_bought` None = the shipped KobBid (new plain delivery),
/// Some(b) = the accumulating bid holding b units (b = 0: its first fill).
fn cross_acc(f: &Fx, w: i64, acc_bought: Option<i64>) -> Scn {
    let m = pk(&f.maker);
    let (a, a_next) = (ask(f, 10), ask(f, 10 - w));
    let n = w * SCALE;
    let v = bid_value();
    let cust = custody(10_000, ASK_COV);
    let rest = custody(10_000 - n, ASK_COV);
    let pay = w * (P240 - TIP);
    let settle = |tok_in: i64| vec![bytes(&n.to_le_bytes()), int(tok_in), int(4), int(0)];
    let (inputs, d, cont, cont_v) = match acc_bought {
        Some(have) if have > 0 => {
            let (b, bn) = (acc(f, &f.maker, have, EXPIRY), acc(f, &f.maker, have + n, EXPIRY));
            let d = custody(have + n, BID_COV);
            (
                vec![
                    call(&b, "fill", args3(n, 2), "bid.fill", v, BID_COV),
                    call(&a, "settle", settle(3), "ask.settle", CARRIER, ASK_COV),
                    tok_leader(&f.t, DC, &custody(have, BID_COV), &[d, rest], Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &cust, Wit::CovId),
                    p2pk_in(&f.matcher, KAS),
                ],
                d,
                bn,
                v - spend(w),
            )
        }
        other => {
            let fill = if other.is_none() { today_fill(n, 2) } else { args3(n, 2) };
            let (b, bn, d) = match other {
                Some(_) => (acc(f, &f.maker, 0, EXPIRY), acc(f, &f.maker, n, EXPIRY), custody(n, BID_COV)),
                None => (today(f, &f.maker), today(f, &f.maker), plain(n, &m)),
            };
            (
                vec![
                    call(&b, "fill", fill, "bid.fill", v, BID_COV),
                    call(&a, "settle", settle(2), "ask.settle", CARRIER, ASK_COV),
                    tok_leader(&f.t, CARRIER, &cust, &[d, rest], Wit::CovId),
                    p2pk_in(&f.matcher, KAS),
                ],
                d,
                bn,
                v - spend(w) - DC,
            )
        }
    };
    close(
        Scn {
            name: format!(
                "F matcher crosses bid x ask ({w} whole), {}",
                match acc_bought {
                    None => "shipped KobBid",
                    Some(0) => "accumulating bid, first fill",
                    Some(_) => "accumulating bid, later fill",
                }
            ),
            inputs,
            outputs: vec![
                tok_out(&f.t, DC, &d, 2),
                out(pay, p2pk_spk(&pk(&f.asker)), None),
                out(cont_v, spk_of(&cont), Some((0, BID_COV))),
                out(CARRIER, spk_of(&a_next), Some((1, ASK_COV))),
                tok_out(&f.t, CARRIER, &rest, 2),
            ],
        },
        &f.matcher,
    )
}

/// F1: the life of an accumulating bid and what each fill costs against the shipped KobBid, on the four published programs.
#[test]
fn f_accumulating_bid_lifecycle_and_costs() {
    for t in [
        Token::reference(),
        Token::pinned(TemplateId::Kcc20PublicMint),
        Token::pinned(TemplateId::KForgeToken7214),
        Token::pinned(TemplateId::KaspaRocketToken11458),
    ] {
        let f = fx(t);
        let m = pk(&f.maker);
        // the splice window of `bought`
        let (b0, b5) = (bytecode(&acc(&f, &f.maker, 0, EXPIRY)), bytecode(&acc(&f, &f.maker, 5, EXPIRY)));
        assert_eq!(b5[BOUGHT_AT - 1], 0x08);
        assert_eq!(i64::from_le_bytes(b5[BOUGHT_AT..BOUGHT_AT + 8].try_into().unwrap()), 5);
        assert_eq!((&b0[..BOUGHT_AT], &b0[BOUGHT_AT + 8..]), (&b5[..BOUGHT_AT], &b5[BOUGHT_AT + 8..]));
        let v0 = bid_value();
        let v1 = v0 - spend(4) - DC;
        let v2 = v1 - spend(3);
        // fill 1 of 3: creates the custody with the carrier
        let c1 = ok(&f.t, &acc_first(&f, "F fill 1 (4 whole): the custody is created", 4_000, custody(4_000, BID_COV), v0));
        // fill 2 of 3: the custody continues with 7,000; no carrier leaves the escrow
        let (b1, b2) = (acc(&f, &f.maker, 4_000, EXPIRY), acc(&f, &f.maker, 7_000, EXPIRY));
        let c2 = ok(
            &f.t,
            &acc_later(
                &f,
                "F fill 2 (3 whole): the custody continues with 7,000",
                &b1,
                3_000,
                v1,
                custody(4_000, BID_COV),
                custody(7_000, BID_COV),
                Some((&b2, v2)),
                10_000,
                7_000,
            ),
        );
        // fill 3 of 3 ends the bid: the whole 10,000 go to the maker's key as ONE plain UTXO
        let c3 = ok(
            &f.t,
            &acc_later(
                &f,
                "F fill 3 (3 whole) ends the bid: 10,000 to the maker as one UTXO",
                &b2,
                3_000,
                v2,
                custody(7_000, BID_COV),
                plain(10_000, &m),
                None,
                10_000,
                7_000,
            ),
        );
        let t0 = ok(&f.t, &today_taker(&f, 3));
        let x0 = ok(&f.t, &cross_acc(&f, 3, None));
        let x1 = ok(&f.t, &cross_acc(&f, 3, Some(0)));
        let x2 = ok(&f.t, &cross_acc(&f, 3, Some(4_000)));
        println!(
            "ACC @{}: taker sell  today {} | acc first {} ({:+}) | acc later {} ({:+}) | acc last {} ({:+}) sompi",
            f.t.name,
            t0.fee,
            c1.fee,
            c1.fee as i64 - t0.fee as i64,
            c2.fee,
            c2.fee as i64 - t0.fee as i64,
            c3.fee,
            c3.fee as i64 - t0.fee as i64
        );
        println!(
            "ACC @{}: bid x ask   today {} | acc first {} ({:+}) | acc later {} ({:+}) sompi; storage {} / {} / {}",
            f.t.name,
            x0.fee,
            x1.fee,
            x1.fee as i64 - x0.fee as i64,
            x2.fee,
            x2.fee as i64 - x0.fee as i64,
            x0.storage,
            x1.storage,
            x2.storage
        );
        println!("SIZE KobBid {} B, KobBidAcc {} B", bytecode(&today(&f, &f.maker)).len(), b0.len());
    }
}

/// F2: what the accumulating bid refuses; batches on a 3 / 3 program; refund, cancel and a maker-signed claim.
#[test]
fn f_accumulating_bid_negatives_batch_release() {
    for t in tokens() {
        let f = fx(t);
        let (m, tk, o) = (pk(&f.maker), pk(&f.taker), pk(&f.maker2));
        let v0 = bid_value();
        let v1 = v0 - spend(4) - DC;
        let v2 = v1 - spend(3);
        let (b1, b2) = (acc(&f, &f.maker, 4_000, EXPIRY), acc(&f, &f.maker, 7_000, EXPIRY));
        let have = custody(4_000, BID_COV);
        let later = |name: &str, cust_in: St, out0: St, cont: Option<(&SilAbiArtifact, i64)>, seller: i64, change: i64| {
            acc_later(&f, name, &b1, 3_000, v1, cust_in, out0, cont, seller, change)
        };
        // G1 one unit short in the custody
        bad_only(&f.t, &later("G1 custody continues with bought + n - 1", have, custody(6_999, BID_COV), Some((&b2, v2)), 10_000, 7_001), 0);
        // G2 a look-alike of the order's id stands in for the custody (the real custody would be stranded)
        bad_only(
            &f.t,
            &later("G2 a 1-unit UTXO of the order's id named as the custody", custody(1, BID_COV), custody(7_000, BID_COV), Some((&b2, v2)), 10_000, 3_001),
            0,
        );
        // G3 / G4 no choice of form: a continuing fill may not deliver to the maker's key, on a later fill or on the first
        bad_only(&f.t, &later("G3 continuing fill delivers 7,000 to the maker's key", have, plain(7_000, &m), Some((&b2, v2)), 10_000, 7_000), 0);
        bad_only(&f.t, &acc_first(&f, "G4 first fill delivers n to the maker's key (today's form)", 4_000, plain(4_000, &m), v0), 0);
        // G4b a later fill that creates a second custody of n and leaves the first one out
        let s = close(
            Scn {
                name: "G4b later fill creates a second custody of n, the first stays out".into(),
                inputs: vec![
                    call(&b1, "fill", args3(3_000, 1), "bid.fill", v1, BID_COV),
                    tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[custody(3_000, BID_COV), plain(7_000, &tk)], Wit::P2pk(f.taker)),
                    p2pk_in(&f.taker, 2 * KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, DC, &custody(3_000, BID_COV), 1),
                    out(v2, spk_of(&b2), Some((0, BID_COV))),
                    tok_out(&f.t, CARRIER, &plain(7_000, &tk), 1),
                ],
            },
            &f.taker,
        );
        bad_only(&f.t, &s, 0);
        // G5 the custody is not spent: custIn names the seller's input
        let mut s = later("G5 custIn names the seller's token", have, custody(7_000, BID_COV), Some((&b2, v2)), 10_000, 7_000);
        s.inputs[0] = call(&b1, "fill", args3(3_000, 2), "bid.fill", v1, BID_COV);
        bad_only(&f.t, &s, 0);
        // G6 the continuation keeps the old `bought`
        bad_only(&f.t, &later("G6 continuation with bought unchanged", have, custody(7_000, BID_COV), Some((&b1, v2)), 10_000, 7_000), 0);
        // G7 the custody's KAS
        let mut s = later("G7 custody continues with 1 sompi less", have, custody(7_000, BID_COV), Some((&b2, v2)), 10_000, 7_000);
        s.outputs[0].value -= 1;
        s.outputs[3].value += 1;
        bad_only(&f.t, &s, 0);
        // G8 the end releases to another key; G8b one unit short
        let end = |name: &str, out0: St, change: i64| acc_later(&f, name, &b2, 3_000, v2, custody(7_000, BID_COV), out0, None, 10_000, change);
        bad_only(&f.t, &end("G8 terminal fill releases the custody to another key", plain(10_000, &o), 7_000), 0);
        bad_only(&f.t, &end("G8b terminal fill releases bought + n - 1", plain(9_999, &m), 7_001), 0);
        bad_only(&f.t, &end("G8c terminal fill keeps the tokens in a custody of the dead order", custody(10_000, BID_COV), 7_000), 0);
        // G9 the custody may not become borrow-enabled
        let be = custody(7_000, BID_COV).with(|s| s.borrow = 1);
        bad_only(&f.t, &later("G9 custody continues borrow-enabled", have, be, Some((&b2, v2)), 10_000, 7_000), 0);
        // G10 a second UTXO of the order's id next to the custody is refused (it stays inert until the maker cancels)
        let mut s = later("G10 a stray of the order's id as a third token input", have, custody(7_000, BID_COV), Some((&b2, v2)), 10_000, 7_005);
        s.inputs.insert(3, tok_delegate(&f.t, CARRIER, &custody(5, BID_COV), Wit::CovId));
        s.outputs[3].value += CARRIER as u64;
        bad_only(&f.t, &s, 0);

        // batch: two accumulating bids of two makers, each with its custody, against ONE resting ask: 3 token inputs, 3 token outputs
        let (ba, ba2) = (acc(&f, &f.maker, 4_000, EXPIRY), acc(&f, &f.maker, 6_000, EXPIRY));
        let (bb, bb2) = (acc(&f, &f.maker2, 2_000, EXPIRY), acc(&f, &f.maker2, 5_000, EXPIRY));
        let (ca, cb) = (custody(6_000, BID_COV), custody(5_000, BID2_COV));
        let (a, a_next) = (ask(&f, 10), ask(&f, 5));
        let rest = custody(5_000, ASK_COV);
        let two = close(
            Scn {
                name: "F two accumulating bids x one resting ask: each bid tops up its own custody (3 / 3 slots)".into(),
                inputs: vec![
                    call(&ba, "fill", args3(2_000, 3), "bidA.fill", v1, BID_COV),
                    call(&bb, "fill", args3(3_000, 4), "bidB.fill", v1, BID2_COV),
                    call(&a, "settle", vec![bytes(&5_000i64.to_le_bytes()), int(5), int(6), int(0)], "ask.settle", CARRIER, ASK_COV),
                    tok_leader(&f.t, DC, &custody(4_000, BID_COV), &[ca, cb, rest], Wit::CovId),
                    tok_delegate(&f.t, DC, &custody(2_000, BID2_COV), Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &custody(10_000, ASK_COV), Wit::CovId),
                    p2pk_in(&f.matcher, KAS),
                ],
                outputs: vec![
                    tok_out(&f.t, DC, &ca, 3),
                    tok_out(&f.t, DC, &cb, 3),
                    out(5 * (P240 - TIP), p2pk_spk(&pk(&f.asker)), None),
                    out(v1 - spend(2), spk_of(&ba2), Some((0, BID_COV))),
                    out(v1 - spend(3), spk_of(&bb2), Some((1, BID2_COV))),
                    out(CARRIER, spk_of(&a_next), Some((2, ASK_COV))),
                    tok_out(&f.t, CARRIER, &rest, 3),
                ],
            },
            &f.matcher,
        );
        ok(&f.t, &two);
        // a third bid with a custody is a fourth token input: the 3 / 3 program refuses it (every bid and the ask accept)
        let (bc, bc2) = (acc(&f, &f.asker, 1_000, EXPIRY), acc(&f, &f.asker, 2_000, EXPIRY));
        const BID3_COV: Hash = Hash::from_bytes([0xb3; 32]);
        let cc = custody(2_000, BID3_COV);
        let rest3 = custody(4_000, ASK_COV);
        let a_next3 = ask(&f, 4);
        let three = close(
            Scn {
                name: "F three accumulating bids x one ask on a 3 / 3 program (4 token inputs)".into(),
                inputs: vec![
                    call(&ba, "fill", args3(2_000, 4), "bidA.fill", v1, BID_COV),
                    call(&bb, "fill", args3(3_000, 5), "bidB.fill", v1, BID2_COV),
                    call(&bc, "fill", args3(1_000, 6), "bidC.fill", v1, BID3_COV),
                    call(&a, "settle", vec![bytes(&6_000i64.to_le_bytes()), int(7), int(8), int(0)], "ask.settle", CARRIER, ASK_COV),
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
                    out(v1 - spend(2), spk_of(&ba2), Some((0, BID_COV))),
                    out(v1 - spend(3), spk_of(&bb2), Some((1, BID2_COV))),
                    out(v1 - spend(1), spk_of(&bc2), Some((2, BID3_COV))),
                    out(CARRIER, spk_of(&a_next3), Some((3, ASK_COV))),
                    tok_out(&f.t, CARRIER, &rest3, 4),
                ],
            },
            &f.matcher,
        );
        bad_only(&f.t, &three, 4);

        // refund after expiry (anyone): the escrow at output i, the whole custody to the maker's key at the custody's index
        let be = acc(&f, &f.maker, 4_000, 1_000);
        let refund = |with_custody: bool, to: [u8; 32], dv: i64, name: &str| {
            let mut s = Scn {
                name: name.into(),
                inputs: vec![call(&be, "refund", vec![int(1)], "bid.refund", v1, BID_COV)],
                outputs: vec![out(v1, p2pk_spk(&m), None)],
            };
            if with_custody {
                s.inputs.push(tok_leader(&f.t, DC, &have, &[plain(4_000, &to)], Wit::CovId));
                s.outputs.push(tok_out(&f.t, dv, &plain(4_000, &to), 1));
            } else {
                s.inputs.push(p2pk_in(&f.taker, KAS));
                s.outputs.push(out(KAS, p2pk_spk(&tk), None));
            }
            s
        };
        ok(&f.t, &refund(true, m, DC, "F refund: escrow and the whole custody back to the maker"));
        bad(&f.t, &refund(false, m, DC, "G11 refund that leaves the custody behind"), 0);
        bad_only(&f.t, &refund(true, o, DC, "G12 refund that sends the custody to another key"), 0);
        bad_only(&f.t, &refund(true, m, DC - 1, "G13 refund that skims the custody's KAS"), 0);
        // a refund before expiry is still refused
        let mut early = refund(true, m, DC, "G14 refund of an unexpired order");
        early.inputs[0] = call(&b1, "refund", vec![int(1)], "bid.refund", v1, BID_COV);
        bad_only(&f.t, &early, 0);

        // cancel (maker's signature): escrow and custody wherever the maker signs them to
        let cancel = |kp: &Keypair, name: &str| Scn {
            name: name.into(),
            inputs: vec![
                Inp { entry: UtxoEntry::new(v1 as u64, spk_of(&b1), 1_000, false, Some(BID_COV)), role: Role::CallSig { art: b1.clone(), entry: "cancel", kp: *kp } },
                tok_leader(&f.t, DC, &have, &[plain(4_000, &m)], Wit::CovId),
            ],
            outputs: vec![out(v1, p2pk_spk(&m), None), tok_out(&f.t, DC, &plain(4_000, &m), 1)],
        };
        ok(&f.t, &cancel(&f.maker, "F cancel: the maker takes the escrow and the custody"));
        bad(&f.t, &cancel(&f.taker, "G15 cancel signed by another key"), 0);

        // claim (maker's signature, the cancel entry): the custody goes to the maker, the order continues under its id with
        // bought = 0 and one more delivery carrier, which the maker adds; the claimed order then fills as a fresh one
        let fresh = acc(&f, &f.maker, 0, EXPIRY);
        let claim = |kp: &Keypair, name: &str| Scn {
            name: name.into(),
            inputs: vec![
                Inp { entry: UtxoEntry::new(v1 as u64, spk_of(&b1), 1_000, false, Some(BID_COV)), role: Role::CallSig { art: b1.clone(), entry: "cancel", kp: *kp } },
                tok_leader(&f.t, DC, &have, &[plain(4_000, &m)], Wit::CovId),
                p2pk_in(&f.maker, DC),
            ],
            outputs: vec![out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))), tok_out(&f.t, DC, &plain(4_000, &m), 1)],
        };
        ok(&f.t, &claim(&f.maker, "F claim: custody to the maker, the order continues with bought = 0"));
        bad(&f.t, &claim(&f.taker, "G16 claim signed by another key"), 0);
        ok(&f.t, &acc_first(&f, "F first fill of the claimed order", 3_000, custody(3_000, BID_COV), v1 + DC));
    }
}

// ================================================================ Part H: one template with a delivery mode (KobBidUni)

const UNI_BOUGHT_AT: usize = 314;

/// KobBidUni with the terms of `bid`: mode 0 new, 1 accumulate, 2 top-up (recvThreshold THR, topupTip TT).
fn uni(f: &Fx, maker: &Keypair, mode: i64, bought: i64) -> SilAbiArtifact {
    let mut a = today_args(f, maker, EXPIRY);
    let (thr, tip) = if mode == 2 { (THR, TT) } else { (0, 0) };
    a.extend([int(mode), int(thr), int(tip), int(bought)]);
    compile_contract(&common::contract_source("KobBidUni"), &a, CompileOptions::default()).expect("compile KobBidUni")
}
fn args4(n: i64, idx: usize, form: i64) -> Vec<ArtifactValue> {
    vec![bytes(&n.to_le_bytes()), int(idx as i64), int(0), int(form)]
}
/// A taker sells `n` into the bid `b` (value `v`): `lead` is the first token input (None: the seller's own token leads),
/// output 0 is `out0` with `out0_kas`, the continuation is `cont`.
#[allow(clippy::too_many_arguments)]
fn uni_sell(
    f: &Fx,
    name: &str,
    b: &SilAbiArtifact,
    fill: Vec<ArtifactValue>,
    v: i64,
    lead: Option<(St, Wit)>,
    out0: St,
    out0_kas: i64,
    cont: (&SilAbiArtifact, i64),
    n: i64,
) -> Scn {
    let tk = pk(&f.taker);
    let change = plain(10_000 - n, &tk);
    let mut inputs = vec![call(b, "fill", fill, "bid.fill", v, BID_COV)];
    match lead {
        Some((st, wit)) => {
            inputs.push(tok_leader(&f.t, DC, &st, &[out0, change], wit));
            inputs.push(tok_delegate(&f.t, CARRIER, &plain(10_000, &tk), Wit::P2pk(f.taker)));
        }
        None => inputs.push(tok_leader(&f.t, CARRIER, &plain(10_000, &tk), &[out0, change], Wit::P2pk(f.taker))),
    }
    inputs.push(p2pk_in(&f.taker, KAS));
    close(
        Scn {
            name: name.into(),
            inputs,
            outputs: vec![tok_out(&f.t, out0_kas, &out0, 1), out(cont.1, spk_of(cont.0), Some((0, BID_COV))), tok_out(&f.t, CARRIER, &change, 1)],
        },
        &f.taker,
    )
}

/// H1: script size and fee per fill of the one-template form against the separate templates, on the four programs.
#[test]
fn h_one_template_sizes_and_fees() {
    for t in [
        Token::reference(),
        Token::pinned(TemplateId::Kcc20PublicMint),
        Token::pinned(TemplateId::KForgeToken7214),
        Token::pinned(TemplateId::KaspaRocketToken11458),
    ] {
        let f = fx(t);
        let m = pk(&f.maker);
        let (u5, u0) = (bytecode(&uni(&f, &f.maker, 1, 5)), bytecode(&uni(&f, &f.maker, 1, 0)));
        assert_eq!(u5[UNI_BOUGHT_AT - 1], 0x08);
        assert_eq!(i64::from_le_bytes(u5[UNI_BOUGHT_AT..UNI_BOUGHT_AT + 8].try_into().unwrap()), 5);
        assert_eq!((&u0[..UNI_BOUGHT_AT], &u0[UNI_BOUGHT_AT + 8..]), (&u5[..UNI_BOUGHT_AT], &u5[UNI_BOUGHT_AT + 8..]));
        let v0 = bid_value();
        let n = 3_000;
        // mode 0: today's delivery
        let b = uni(&f, &f.maker, 0, 0);
        let m0 = ok(&f.t, &uni_sell(&f, "H mode 0: new plain delivery", &b, args4(n, 1, 0), v0, None, plain(n, &m), DC, (&b, v0 - spend(3) - DC), n));
        // mode 1: first fill and a later fill
        let (b0, b3) = (uni(&f, &f.maker, 1, 0), uni(&f, &f.maker, 1, 3_000));
        let m1a = ok(
            &f.t,
            &uni_sell(&f, "H mode 1: first fill creates the custody", &b0, args4(n, 1, 0), v0, None, custody(n, BID_COV), DC, (&b3, v0 - spend(3) - DC), n),
        );
        let (b4, b7) = (uni(&f, &f.maker, 1, 4_000), uni(&f, &f.maker, 1, 7_000));
        let v1 = v0 - spend(4) - DC;
        let m1b = ok(
            &f.t,
            &uni_sell(
                &f,
                "H mode 1: later fill continues the custody",
                &b4,
                args4(n, 1, 0),
                v1,
                Some((custody(4_000, BID_COV), Wit::CovId)),
                custody(7_000, BID_COV),
                DC,
                (&b7, v1 - spend(3)),
                n,
            ),
        );
        // mode 2: the three forms
        let b2 = uni(&f, &f.maker, 2, 0);
        let m2a = ok(&f.t, &uni_sell(&f, "H mode 2 form 0: new plain delivery", &b2, args4(n, 1, 0), v0, None, plain(n, &m), DC, (&b2, v0 - spend(3) - DC), n));
        let m2b = ok(
            &f.t,
            &uni_sell(&f, "H mode 2 form 1: new receive UTXO", &b2, args4(n, 1, 1), v0, None, recv(n, &m, THR), DC, (&b2, v0 - spend(3) - DC), n),
        );
        let r = recv(7_000, &m, THR);
        let m2c = ok(
            &f.t,
            &uni_sell(
                &f,
                "H mode 2 form 2: top-up, the filler keeps topupTip",
                &b2,
                args4(n, 1, 2),
                v0,
                Some((r, Wit::Borrow)),
                r.with(|s| s.amount += n),
                DC,
                (&b2, v0 - spend(3) - TT),
                n,
            ),
        );
        // the modes do not leak into each other
        bad_only(
            &f.t,
            &uni_sell(&f, "H mode 0 order, custody delivery", &b, args4(n, 1, 0), v0, None, custody(n, BID_COV), DC, (&b, v0 - spend(3) - DC), n),
            0,
        );
        bad_only(
            &f.t,
            &uni_sell(&f, "H mode 0 order, receive-state delivery (form 1)", &b, args4(n, 1, 1), v0, None, recv(n, &m, THR), DC, (&b, v0 - spend(3) - DC), n),
            0,
        );
        bad_only(
            &f.t,
            &uni_sell(&f, "H mode 1 order, plain delivery to the maker", &b0, args4(n, 1, 0), v0, None, plain(n, &m), DC, (&b3, v0 - spend(3) - DC), n),
            0,
        );
        bad_only(
            &f.t,
            &uni_sell(
                &f,
                "H mode 1 order, top-up form into a receive UTXO",
                &b0,
                args4(n, 1, 2),
                v0,
                Some((r, Wit::Borrow)),
                r.with(|s| s.amount += n),
                DC,
                (&b0, v0 - spend(3) - TT),
                n,
            ),
            0,
        );
        bad_only(
            &f.t,
            &uni_sell(&f, "H mode 2 order, custody delivery", &b2, args4(n, 1, 0), v0, None, custody(n, BID_COV), DC, (&b2, v0 - spend(3) - DC), n),
            0,
        );
        bad_only(
            &f.t,
            &uni_sell(
                &f,
                "H mode 2 top-up taking 1 sompi more than topupTip",
                &b2,
                args4(n, 1, 2),
                v0,
                Some((r, Wit::Borrow)),
                r.with(|s| s.amount += n),
                DC,
                (&b2, v0 - spend(3) - TT - 1),
                n,
            ),
            0,
        );
        // separate templates, the same fills
        let t0 = ok(&f.t, &today_taker(&f, 3));
        let (a0, a3) = (acc(&f, &f.maker, 0, EXPIRY), acc(&f, &f.maker, 3_000, EXPIRY));
        let _ = (&a0, &a3);
        let s1a = ok(&f.t, &acc_first(&f, "H separate KobBidAcc: first fill", n, custody(n, BID_COV), v0));
        let (a4, a7) = (acc(&f, &f.maker, 4_000, EXPIRY), acc(&f, &f.maker, 7_000, EXPIRY));
        let s1b = ok(
            &f.t,
            &acc_later(&f, "H separate KobBidAcc: later fill", &a4, n, v1, custody(4_000, BID_COV), custody(7_000, BID_COV), Some((&a7, v1 - spend(3))), 10_000, 7_000),
        );
        let s2c = ok(&f.t, &taker_topup(&f, 3));
        println!(
            "UNI @{}: sizes KobBid {} / KobBidAcc {} / KobBidTopup {} / KobBidUni {} B",
            f.t.name,
            bytecode(&today(&f, &f.maker)).len(),
            bytecode(&a0).len(),
            bytecode(&bid(&f, &f.maker, THR)).len(),
            u0.len()
        );
        println!(
            "UNI @{}: mode 0 fill  one template {} vs KobBid {} ({:+}) sompi",
            f.t.name,
            m0.fee,
            t0.fee,
            m0.fee as i64 - t0.fee as i64
        );
        println!(
            "UNI @{}: mode 1 first {} vs KobBidAcc {} ({:+}); later {} vs {} ({:+}) sompi",
            f.t.name,
            m1a.fee,
            s1a.fee,
            m1a.fee as i64 - s1a.fee as i64,
            m1b.fee,
            s1b.fee,
            m1b.fee as i64 - s1b.fee as i64
        );
        println!(
            "UNI @{}: mode 2 form 0 {} / form 1 {} / top-up {} vs KobBidTopup top-up {} ({:+}) sompi; top-up extra over a mode-2 new delivery {}",
            f.t.name,
            m2a.fee,
            m2b.fee,
            m2c.fee,
            s2c.fee,
            m2c.fee as i64 - s2c.fee as i64,
            m2c.fee as i64 - m2b.fee as i64
        );
        println!(
            "UNI @{}: compute mass mode 0 {} / mode 1 later {} / mode 2 top-up {}; transient {} / {} / {}",
            f.t.name, m0.compute, m1b.compute, m2c.compute, m0.transient, m1b.transient, m2c.transient
        );
    }
}

/// H2: a mixed batch. One transaction fills a mode-2 bid by top-up and a mode-1 bid into its custody, against one resting ask.
#[test]
fn h_mixed_batch_accumulate_and_topup() {
    for t in tokens() {
        let f = fx(t);
        let (m, m2) = (pk(&f.maker), pk(&f.maker2));
        let v0 = bid_value();
        let v1 = v0 - spend(4) - DC;
        // bid T: maker 1, mode 2, topped up by 2,000. bid A: maker 2, mode 1, holds 4,000, buys 3,000.
        let bt = uni(&f, &f.maker, 2, 0);
        let (ba, ba2) = (uni(&f, &f.maker2, 1, 4_000), uni(&f, &f.maker2, 1, 7_000));
        let r = recv(7_000, &m, THR);
        let succ = r.with(|s| s.amount += 2_000);
        let (a, a_next) = (ask(&f, 10), ask(&f, 5));
        let rest = custody(5_000, ASK_COV);
        let cust_a = custody(7_000, BID2_COV);
        let batch = |top_first: bool, name: &str| {
            // the top-up bid must be the token's lowest-index bid: its delivery is the leader's first output
            let (it, ia) = if top_first { (0, 1) } else { (1, 0) };
            let mut inputs = vec![
                call(&bt, "fill", args4(2_000, 3, 2), "bidT.fill", v0, BID_COV),
                call(&ba, "fill", args4(3_000, 4, 0), "bidA.fill", v1, BID2_COV),
            ];
            let mut outs = vec![tok_out(&f.t, CARRIER, &succ, 3), tok_out(&f.t, DC, &cust_a, 3)];
            let mut next = vec![succ, cust_a, rest];
            if !top_first {
                inputs.swap(0, 1);
                outs.swap(0, 1);
                next.swap(0, 1);
            }
            inputs.extend([
                call(&a, "settle", vec![bytes(&5_000i64.to_le_bytes()), int(5), int(6), int(0)], "ask.settle", CARRIER, ASK_COV),
                tok_leader(&f.t, CARRIER, &r, &next, Wit::Borrow),
                tok_delegate(&f.t, DC, &custody(4_000, BID2_COV), Wit::CovId),
                tok_delegate(&f.t, CARRIER, &custody(10_000, ASK_COV), Wit::CovId),
                p2pk_in(&f.matcher, KAS),
            ]);
            outs.extend([
                out(5 * (P240 - TIP), p2pk_spk(&pk(&f.asker)), None),
                out(v0 - spend(2) - TT, spk_of(&bt), Some((it, BID_COV))),
                out(v1 - spend(3), spk_of(&ba2), Some((ia, BID2_COV))),
                out(CARRIER, spk_of(&a_next), Some((2, ASK_COV))),
                tok_out(&f.t, CARRIER, &rest, 3),
            ]);
            close(Scn { name: name.into(), inputs, outputs: outs }, &f.matcher)
        };
        let c = ok(&f.t, &batch(true, "H mixed batch: mode-2 top-up + mode-1 custody x one resting ask (3 token in / 3 out)"));
        assert_eq!((c.inputs, c.outputs), (7, 8));
        // with the accumulating bid first, the top-up's delivery is the second token output: the token refuses, the orders accept
        bad_only(&f.t, &batch(false, "H mixed batch with the mode-1 bid at the lower index"), 3);
        let _ = m2;
    }
}

// ================================================================ Part I: claim, then sell or merge, in one transaction

fn ask_of(f: &Fx, maker: &Keypair, left: i64) -> SilAbiArtifact {
    let mut a = vec![bytes(&pk(maker))];
    a.extend(f.t.tok_args());
    a.extend([int(SCALE), int(SCALE), int(P240), int(TIP), int(0), int(0), int(EXPIRY), int(REFUND_TIP)]);
    a.extend([int(0), int(0), int(0), int(0), int(0), int(left), bytes(&EXT)]);
    compile_contract(&common::contract_source("KobAsk"), &a, CompileOptions::default()).expect("compile KobAsk")
}
/// The consensus genesis id of the one-output group `out` authorised by input `auth` (outpoints in build() are ([k+1; 32], k)).
fn genesis_id(auth: usize, out_idx: usize, o: &TransactionOutput) -> Hash {
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes([auth as u8 + 1; 32]), index: auth as u32 };
    kaspa_consensus_core::hashing::covenant_id::covenant_id(op, [(out_idx as u32, o)].into_iter())
}

/// I: the maker claims the custody of an accumulating bid and, in the same signed transaction, places a KobAsk with those
/// tokens (plus wallet UTXOs), or merges them with wallet UTXOs.
#[test]
fn i_claim_then_sell_or_merge_in_one_transaction() {
    for t in tokens() {
        let f = fx(t);
        let m = pk(&f.maker);
        let v0 = bid_value();
        let v1 = v0 - spend(4) - DC;
        let b1 = acc(&f, &f.maker, 4_000, EXPIRY);
        let fresh = acc(&f, &f.maker, 0, EXPIRY);
        let have = custody(4_000, BID_COV);
        let cancel_in = |kp: &Keypair| Inp {
            entry: UtxoEntry::new(v1 as u64, spk_of(&b1), 1_000, false, Some(BID_COV)),
            role: Role::CallSig { art: b1.clone(), entry: "cancel", kp: *kp },
        };
        // I1 the bid ends; custody 4,000 + wallet 1,000 + wallet 2,000 become the custody of a new KobAsk for 7,000
        //    token slots: 3 inputs, 1 output
        let ask7 = ask_of(&f, &f.maker, 7_000);
        let place_all = |kp: &Keypair, name: &str| {
            let mut ask_out = out(CARRIER, spk_of(&ask7), None);
            let id = genesis_id(4, 0, &ask_out);
            ask_out.covenant = Some(CovenantBinding { authorizing_input: 4, covenant_id: id });
            let cust = custody(7_000, id);
            let s = Scn {
                name: name.into(),
                inputs: vec![
                    cancel_in(kp),
                    tok_leader(&f.t, DC, &have, &[cust], Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    tok_delegate(&f.t, CARRIER, &plain(2_000, &m), Wit::P2pk(f.maker)),
                    p2pk_in(&f.maker, KAS),
                ],
                outputs: vec![ask_out, tok_out(&f.t, CARRIER, &cust, 1)],
            };
            (close(s, &f.maker), id)
        };
        let (s, ask_id) = place_all(&f.maker, "I1 claim (bid ends) + 2 wallet UTXOs -> one KobAsk for 7,000");
        ok(&f.t, &s);
        bad(&f.t, &place_all(&f.taker, "I1x the same, signed by another key").0, 0);
        // the ask placed that way is a normal ask: a taker buys all 7,000
        let tk = pk(&f.taker);
        let buy = close(
            Scn {
                name: "I1b a taker buys the 7,000 from the ask placed in I1".into(),
                inputs: vec![
                    call(&ask7, "settle", vec![bytes(&7_000i64.to_le_bytes()), int(1), int(1), int(0)], "ask.settle", CARRIER, ask_id),
                    tok_leader(&f.t, CARRIER, &custody(7_000, ask_id), &[plain(7_000, &tk)], Wit::CovId),
                    p2pk_in(&f.taker, 20 * KAS),
                ],
                outputs: vec![out(7 * (P240 - TIP) + 2 * CARRIER, p2pk_spk(&m), None), tok_out(&f.t, CARRIER, &plain(7_000, &tk), 1)],
            },
            &f.taker,
        );
        ok(&f.t, &buy);
        // I2 the bid continues (bought = 0, one more carrier from the maker); custody 4,000 + wallet 1,000 -> KobAsk for 4,500
        //    and 500 change to the maker. token slots: 2 inputs, 2 outputs
        let ask45 = ask_of(&f, &f.maker, 4_500);
        let mut ask_out = out(CARRIER, spk_of(&ask45), None);
        let id = genesis_id(3, 1, &ask_out);
        ask_out.covenant = Some(CovenantBinding { authorizing_input: 3, covenant_id: id });
        let cust = custody(4_500, id);
        let s = close(
            Scn {
                name: "I2 claim (bid continues) + 1 wallet UTXO -> KobAsk for 4,500 and 500 change".into(),
                inputs: vec![
                    cancel_in(&f.maker),
                    tok_leader(&f.t, DC, &have, &[cust, plain(500, &m)], Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    p2pk_in(&f.maker, 3 * KAS),
                ],
                outputs: vec![
                    out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))),
                    ask_out,
                    tok_out(&f.t, CARRIER, &cust, 1),
                    tok_out(&f.t, CARRIER, &plain(500, &m), 1),
                ],
            },
            &f.maker,
        );
        ok(&f.t, &s);
        // I3 claim + merge: custody 4,000 + two wallet UTXOs -> one plain UTXO of 7,000; the bid continues. 3 inputs, 1 output
        let s = close(
            Scn {
                name: "I3 claim (bid continues) + merge with 2 wallet UTXOs -> one plain UTXO of 7,000".into(),
                inputs: vec![
                    cancel_in(&f.maker),
                    tok_leader(&f.t, DC, &have, &[plain(7_000, &m)], Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    tok_delegate(&f.t, CARRIER, &plain(2_000, &m), Wit::P2pk(f.maker)),
                    p2pk_in(&f.maker, KAS),
                ],
                outputs: vec![out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))), tok_out(&f.t, 3 * CARRIER, &plain(7_000, &m), 1)],
            },
            &f.maker,
        );
        ok(&f.t, &s);
        // I4 a fourth token input (custody + 3 wallet UTXOs) does not fit a 3 / 3 program
        let s = close(
            Scn {
                name: "I4 claim + 3 wallet UTXOs (4 token inputs)".into(),
                inputs: vec![
                    cancel_in(&f.maker),
                    tok_leader(&f.t, DC, &have, &[plain(8_000, &m)], Wit::CovId),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    tok_delegate(&f.t, CARRIER, &plain(2_000, &m), Wit::P2pk(f.maker)),
                    tok_delegate(&f.t, CARRIER, &plain(1_000, &m), Wit::P2pk(f.maker)),
                    p2pk_in(&f.maker, KAS),
                ],
                outputs: vec![out(v1 + DC, spk_of(&fresh), Some((0, BID_COV))), tok_out(&f.t, 4 * CARRIER, &plain(8_000, &m), 1)],
            },
            &f.maker,
        );
        bad_only(&f.t, &s, 1);
    }
}

// ================================================================ Part J: option A for every borrow scheme

/// J: the owner path of a key-owned holding does not depend on its borrow scheme. A holder whose UTXOs carry borrow scheme
/// 0x01 (amount threshold), 0x02 (Schnorr signature guard) or 0x03 (hash chain) places a KobAsk from one (plain custody, the
/// change keeps scheme and guard) and merges two of the same settings, with the owner key's signature alone.
#[test]
fn j_option_a_owner_path_for_every_borrow_scheme() {
    for t in tokens() {
        let f = fx(t);
        let m = pk(&f.maker);
        let guard_key = pk(&f.matcher); // somebody else's key (scheme 2) / an arbitrary chain head (scheme 3)
        for (scheme, guard) in [(1u8, thr_guard(THR)), (2u8, guard_key), (3u8, [0x5a; 32])] {
            let held = |amount: i64| St { amount, owner: m, scheme: 0, borrow: scheme, guard };
            // sell: 5,000 held -> KobAsk custody of 3,000 (plain, covenant-owned) + 2,000 change with the same settings
            let ask3 = ask_of(&f, &f.maker, 3_000);
            let mut ask_out = out(CARRIER, spk_of(&ask3), None);
            let id = genesis_id(1, 0, &ask_out);
            ask_out.covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: id });
            let cust = custody(3_000, id);
            let place = |change: St, name: String| {
                close(
                    Scn {
                        name,
                        inputs: vec![tok_leader(&f.t, CARRIER, &held(5_000), &[cust, change], Wit::P2pk(f.maker)), p2pk_in(&f.maker, 3 * KAS)],
                        outputs: vec![ask_out.clone(), tok_out(&f.t, CARRIER, &cust, 0), tok_out(&f.t, CARRIER, &change, 0)],
                    },
                    &f.maker,
                )
            };
            ok(&f.t, &place(held(2_000), format!("J borrow scheme {scheme}: owner places a KobAsk, change keeps scheme and guard")));
            // the ask is an ordinary ask: a taker buys its custody
            let tk = pk(&f.taker);
            let buy = close(
                Scn {
                    name: format!("J borrow scheme {scheme}: a taker buys the 3,000 from that ask"),
                    inputs: vec![
                        call(&ask3, "settle", vec![bytes(&3_000i64.to_le_bytes()), int(1), int(1), int(0)], "ask.settle", CARRIER, id),
                        tok_leader(&f.t, CARRIER, &cust, &[plain(3_000, &tk)], Wit::CovId),
                        p2pk_in(&f.taker, 10 * KAS),
                    ],
                    outputs: vec![out(3 * (P240 - TIP) + 2 * CARRIER, p2pk_spk(&m), None), tok_out(&f.t, CARRIER, &plain(3_000, &tk), 1)],
                },
                &f.taker,
            );
            ok(&f.t, &buy);
            // merge: two holdings of the same settings into one of the same settings (leader and delegate, both owner-signed)
            let merge = |kp: &Keypair, name: String| Scn {
                name,
                inputs: vec![
                    tok_leader(&f.t, CARRIER, &held(5_000), &[held(6_500)], Wit::P2pk(*kp)),
                    tok_delegate(&f.t, CARRIER, &held(1_500), Wit::P2pk(*kp)),
                ],
                outputs: vec![tok_out(&f.t, 2 * CARRIER, &held(6_500), 0)],
            };
            ok(&f.t, &merge(&f.maker, format!("J borrow scheme {scheme}: owner merges two holdings of the same settings")));
            // not the owner: refused on the leader and on the delegate, whatever the scheme (the guard key does not own it)
            bad(&f.t, &merge(&f.matcher, format!("J borrow scheme {scheme}: the same merge signed by the guard's key")), 0);
            bad(&f.t, &merge(&f.matcher, format!("J borrow scheme {scheme}: the same merge signed by the guard's key (delegate)")), 1);
        }
    }
}
