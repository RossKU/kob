//! KOB protocol v2.2 harness, BUY side: KobCondBid (buy-stop / buy-stop-limit / trailing buy / OCO)
//! and KobIfdAsk (sell-first IFD/IFO entry with partial fills, each creating a fresh KobCondBid exit), triggered by a
//! plain KobAsk / KobBid fill in the same transaction (touch, v2.6), with the reference KCC-20, executed in rusty-kaspa
//! v2.1.0's TxScriptEngine.
//! Every order executes at its own all-in limit or better. Helpers are copied from kob_v2_tests.rs.
//! Run: cargo test --release -p kob-tests --test kob_v2_buy_tests -- --nocapture --test-threads=1
#![allow(dead_code)]

mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
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
use kob_protocol::state::{quote_of, Round};
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
const FAKE_COV: Hash = Hash::from_bytes([0x66; 32]);
const EXT: [u8; 32] = [0xee; 32];
const SCHEME_P2PK: u8 = 0x00;
const SCHEME_COVID: u8 = 0x04;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;

const CARRIER: i64 = 10 * KAS;
const DC: i64 = 10 * KAS; // bid delivery carrier
const SCALE: i64 = 1_000; // base units of a whole token (10^3, a 3-decimal token): the price denominator
const UNIT: i64 = SCALE; // alias: base units in one whole token (the evidence `unit` field is a per-order scale)
const WHOLE: i64 = SCALE; // base units in one whole token
const DEFAULT_MIN_FILL: i64 = WHOLE; // the default smallest fill: one whole token
const TIP: i64 = 100_000; // default priority tip, sompi per whole token (0.001 KAS)
const EXPIRY: i64 = 400_000_000;
const NO_EXPIRY: i64 = 499_999_999_999;
const REFUND_TIP: i64 = 3_000_000;
const MAX_IDLE: i64 = 77_760_000;
const NOW: u64 = 1_000_000; // lockTime of fills (DAA type)
const NET_FEE: i64 = KAS / 10;

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
fn cov(b: u8) -> Hash {
    Hash::from_bytes([b; 32])
}
fn bytes(v: &[u8]) -> ArtifactValue {
    v.to_vec().into()
}
fn int(v: i64) -> ArtifactValue {
    ArtifactValue::Int(v)
}

// ---------------------------------------------------------------- economics (mirrors the contracts)

/// Quote value (base units `n`, rate per whole token) rounded UP (maker receives), mirroring every covenant's quoteOf
/// and cross-checked against kob_protocol::state::quote_of.
fn q_up(n: i64, rate: i64) -> i64 {
    let v = quote_up(n, rate);
    assert_eq!(Some(v), quote_of(n, rate, SCALE, Round::Up), "q_up mismatch kob_protocol");
    v
}
/// Quote value rounded DOWN (maker pays).
fn q_dn(n: i64, rate: i64) -> i64 {
    let v = quote_dn(n, rate);
    assert_eq!(Some(v), quote_of(n, rate, SCALE, Round::Down), "q_dn mismatch kob_protocol");
    v
}
/// The covenant split formula, c = scale - 1 (ceil).
fn quote_up(n: i64, rate: i64) -> i64 {
    let (q, m) = (n / SCALE, n % SCALE);
    q * rate + m * (rate / SCALE) + (m * (rate % SCALE) + SCALE - 1) / SCALE
}
/// The covenant split formula, c = 0 (floor).
fn quote_dn(n: i64, rate: i64) -> i64 {
    let (q, m) = (n / SCALE, n % SCALE);
    q * rate + m * (rate / SCALE) + (m * (rate % SCALE)) / SCALE
}
/// All-in minimum an ask (quote `p`, tip `tip`) receives for `n` WHOLE tokens (= ceil of n*WHOLE base units).
fn ask_all_in(n: i64, p: i64, tip: i64) -> i64 {
    n * (p - tip)
}
/// All-in maximum a bid (quote `p`, tip `tip`) pays for `n` WHOLE tokens (= floor of n*WHOLE base units).
fn bid_all_in(n: i64, p: i64, tip: i64) -> i64 {
    n * (p + tip)
}
/// Decayed ask / rising bid quote at time t (slope per 1000 DAA from activeFrom).
fn decay_down(start: i64, end: i64, slope: i64, from: i64, t: i64) -> i64 {
    (start - slope * ((t - from) / 1000)).max(end)
}
fn rise_up(start: i64, end: i64, slope: i64, from: i64, t: i64) -> i64 {
    (start + slope * ((t - from) / 1000)).min(end)
}
/// Stop-leg price of a conditional sell (band below the stop).
fn stop_leg(stop: i64, slip_bps: i64) -> i64 {
    stop - stop * slip_bps / 10_000
}

// ---------------------------------------------------------------- order parameters

#[derive(Clone)]
struct AskP {
    maker: [u8; 32],
    price: i64,
    tip: i64,
    tif: i64,
    active_from: i64,
    expiry: i64,
    interval: i64,
    max_fill: i64,
    slope: i64,
    price_end: i64,
    decay_step: i64,
    qty: i64,
    /// token covenant id and unit (the evidence tests quote another token or unit)
    token: Hash,
    unit: i64,
}
impl AskP {
    fn new(maker: [u8; 32], price: i64) -> Self {
        AskP {
            maker,
            price,
            tip: TIP,
            tif: 0,
            active_from: 0,
            expiry: EXPIRY,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 0,
            qty: 10,
            token: TOKEN_COV,
            unit: UNIT,
        }
    }
}
#[derive(Clone)]
struct BidP {
    maker: [u8; 32],
    price: i64,
    tip: i64,
    tif: i64,
    active_from: i64,
    expiry: i64,
    interval: i64,
    max_fill: i64,
    slope: i64,
    price_end: i64,
    decay_step: i64,
    token: Hash,
    unit: i64,
}
impl BidP {
    fn new(maker: [u8; 32], price: i64) -> Self {
        BidP {
            maker,
            price,
            tip: TIP,
            tif: 0,
            active_from: 0,
            expiry: EXPIRY,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 0,
            token: TOKEN_COV,
            unit: UNIT,
        }
    }
    /// Budget one whole consumes (all-in at the quote, or at the cap of a rising bid).
    fn token_max(&self) -> i64 {
        let pmax = if self.slope != 0 && self.price_end > self.price { self.price_end } else { self.price };
        pmax + self.tip
    }
}
/// How the evidence order is spent in the transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EvMode {
    /// filled in full (its fill entry, n > 0): genuine evidence
    Fill,
    /// cancelled by its maker (the sigscript starts with a signature push, not n)
    Cancel,
    /// refunded (KobAsk settle with n = 0, KobBid refund)
    Refund,
    /// a KobBid fill entry with n = 0 (refused by the bid itself: the attacked order must refuse it too)
    ZeroFill,
    /// a look-alike: a P2SH script carrying a real order's state under another template
    Forged,
    /// the real redeem script pushed by an input whose script public key is not its P2SH
    NotP2sh,
}
/// Trigger evidence (touch, v2.6): a plain resting KobAsk (ask) or KobBid of maker C, of which the
/// matcher fills all `qty` in the same transaction. Exposed at its quote since max(daa + interval,
/// activeFrom, custody DAA (ask)).
#[derive(Clone)]
struct Ev {
    ask: bool,
    price: i64,
    qty: i64,
    daa: u64,
    tok_daa: u64,
    active_from: i64,
    interval: i64,
    slope: i64,
    unit: i64,
    token: Hash,
    cov: Hash,
    mode: EvMode,
}
impl Ev {
    /// A resting ask (cov 0x9a) of 5 qty quoting `price`, placed at DAA 1000 (exposed long before NOW).
    fn ask(price: i64) -> Self {
        Ev {
            ask: true,
            price,
            qty: 5,
            daa: 1_000,
            tok_daa: 1_000,
            active_from: 0,
            interval: 0,
            slope: 0,
            unit: UNIT,
            token: TOKEN_COV,
            cov: cov(0x9a),
            mode: EvMode::Fill,
        }
    }
    /// A resting bid (cov 0x9b) funded for exactly 5 qty quoting `price` (the fill exhausts it).
    fn bid(price: i64) -> Self {
        Ev { ask: false, cov: cov(0x9b), ..Ev::ask(price) }
    }
    fn with(&self, f: impl FnOnce(&mut Ev)) -> Ev {
        let mut e = self.clone();
        f(&mut e);
        e
    }
}
#[derive(Clone)]
struct CondP {
    maker: [u8; 32],
    tip: i64,
    active_from: i64,
    expiry: i64,
    tp: i64,
    stop: i64,
    slip_bps: i64,
    step: i64,
    gap: i64,
    wait: i64,
    min_units: i64,
    min_rest: i64,
    armed: i64,
    band_daa: i64,
    keeper_tip: i64,
    qty: i64,
}
impl CondP {
    /// OCO: take-profit at 3.00, stop 2.00 with the default 3% band (stop leg 1.94), evidence fills of
    /// >= 1 unit exposed >= 600 DAA.
    fn oco(maker: [u8; 32]) -> Self {
        CondP {
            maker,
            tip: TIP,
            active_from: 0,
            expiry: EXPIRY,
            tp: 300_000_000,
            stop: 200_000_000,
            slip_bps: 300,
            step: 0,
            gap: 0,
            wait: 0,
            min_units: 1,
            min_rest: 600,
            armed: 0,
            band_daa: 0,
            keeper_tip: 0,
            qty: 10,
        }
    }
}
#[derive(Clone)]
struct Tpl {
    pre: Vec<u8>,
    suf: Vec<u8>,
    hash: Vec<u8>,
}
fn tpl_of(a: &SilAbiArtifact) -> Tpl {
    let (pre, suf, hash) = compiled_template_parts_and_hash(a);
    Tpl { pre, suf, hash }
}

/// A network deployment: the token plus every KOB template (network constants resolved).
struct Net {
    t: Token,
    ask_tpl: Tpl,
    bid_tpl: Tpl,
    cond_tpl: Tpl,
}
impl Net {
    fn new() -> Self {
        Self::with_token(Token::new())
    }
    fn with_token(t: Token) -> Self {
        let empty = || Tpl { pre: vec![], suf: vec![], hash: vec![] };
        let mut n = Net { t, ask_tpl: empty(), bid_tpl: empty(), cond_tpl: empty() };
        n.ask_tpl = tpl_of(&n.ask(&AskP::new([1; 32], 1)));
        n.bid_tpl = tpl_of(&n.bid(&BidP::new([1; 32], 1)));
        n.cond_tpl = tpl_of(&n.cond(&CondP::oco([1; 32])));
        n
    }
    fn tok_args(&self) -> Vec<ArtifactValue> {
        self.tok_args_of(TOKEN_COV)
    }
    fn tok_args_of(&self, token: Hash) -> Vec<ArtifactValue> {
        vec![bytes(&token.as_bytes()), bytes(&self.t.hash), int(self.t.prefix.len() as i64), int(self.t.suffix.len() as i64)]
    }
    /// The evidence templates a conditional order / stop entry inlines (KobAsk, then KobBid: hash,
    /// prefix and suffix length), as in contracts/v2/KobCondBid.ctor.json.
    fn ev_consts(&self) -> Vec<ArtifactValue> {
        let mut a = self.tpl_consts(&self.ask_tpl);
        a.extend(self.tpl_consts(&self.bid_tpl));
        a
    }
    fn tpl_consts(&self, t: &Tpl) -> Vec<ArtifactValue> {
        vec![bytes(&t.hash), int(t.pre.len() as i64), int(t.suf.len() as i64)]
    }
    fn ask(&self, p: &AskP) -> SilAbiArtifact {
        let mut a = vec![bytes(&p.maker)];
        a.extend(self.tok_args_of(p.token));
        a.extend([
            int(p.unit), // scale (base units per whole token)
            int(p.unit), // minFill: one whole token (evidence asks fill in full, so it is never binding)
            int(p.price),
            int(p.tip),
            int(p.tif),
            int(p.active_from),
            int(p.expiry),
            int(REFUND_TIP),
            int(p.interval),
            int(p.max_fill),
            int(p.slope),
            int(p.price_end),
            int(p.decay_step),
            int(p.qty * p.unit), // amountLeft in base units (= custody)
        ]);
        compile_contract(&src("KobAsk"), &a, CompileOptions::default()).expect("compile KobAsk")
    }
    fn bid(&self, p: &BidP) -> SilAbiArtifact {
        let mut a = vec![bytes(&p.maker)];
        a.extend(self.tok_args_of(p.token));
        a.extend([
            bytes(&EXT),
            int(p.unit), // scale
            int(p.unit), // minFill: one whole token (> 0 as KobBid requires)
            int(p.price),
            int(p.tip),
            int(p.tif),
            int(p.active_from),
            int(p.expiry),
            int(REFUND_TIP),
            int(0), // reserve
            int(DC),
            int(p.interval),
            int(p.max_fill),
            int(p.slope),
            int(p.price_end),
            int(p.decay_step),
        ]);
        compile_contract(&src("KobBid"), &a, CompileOptions::default()).expect("compile KobBid")
    }
    fn cond(&self, p: &CondP) -> SilAbiArtifact {
        let mut a = self.ev_consts();
        a.push(bytes(&p.maker));
        a.extend(self.tok_args());
        a.extend([
            int(UNIT),             // scale
            int(DEFAULT_MIN_FILL), // minFill
            int(p.tip),
            int(p.active_from),
            int(p.expiry),
            int(REFUND_TIP),
            int(p.tp),
            int(p.stop),
            int(p.slip_bps),
            int(p.step),
            int(p.gap),
            int(p.wait),
            int(p.min_units * UNIT), // minTouch in base units
            int(p.min_rest),
            int(p.armed),
            int(p.band_daa),
            int(p.keeper_tip),
            int(p.qty * UNIT), // amountLeft in base units
            bytes(&[0; 32]),
            int(0),
            int(0),
        ]);
        compile_contract(&src("KobCondAsk"), &a, CompileOptions::default()).expect("compile KobCondAsk")
    }
    /// Encoded KobCondAsk state span of an exit order (what KobIfdBid commits to).
    fn cond_state(&self, p: &CondP) -> Vec<u8> {
        let a = self.cond(p);
        let l = state_layout(&a);
        bytecode(&a)[l.start..l.start + l.len].to_vec()
    }
}

// ---------------------------------------------------------------- token (reference KCC-20, 3/3)

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
        Self::named("KCC20Ref")
    }
    /// A KCC-20 program by source name (e.g. the 8/8 slot-limit variant "KCC20Ref_8x8").
    fn named(name: &str) -> Self {
        let art = compile_contract(
            &src(name),
            &[int(1000), bytes(&[3u8; 32]), ArtifactValue::Byte(SCHEME_COVID), ArtifactValue::Byte(0), bytes(&[0u8; 32]), bytes(&EXT)],
            CompileOptions::default(),
        )
        .expect("compile KCC20Ref");
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        let t = Token { art, prefix, suffix, hash };
        assert_eq!(t.redeem(1000, &[3u8; 32], SCHEME_COVID, 0), bytecode(&t.art));
        t
    }
    fn redeem(&self, amount: i64, owner: &[u8; 32], scheme: u8, borrow: u8) -> Vec<u8> {
        [self.prefix.clone(), tok_state_bytes(amount, owner, scheme, borrow), self.suffix.clone()].concat()
    }
    fn spk(&self, amount: i64, owner: &[u8; 32], scheme: u8) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(amount, owner, scheme, 0))
    }
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

fn spk_of(a: &SilAbiArtifact) -> ScriptPublicKey {
    pay_to_script_hash_script(&bytecode(a))
}

// ---------------------------------------------------------------- scenario model

#[derive(Clone)]
enum Arg {
    V(ArtifactValue),
    Sig(Keypair, u8),
    /// A signature ground (fresh aux randomness per try) until its first 8 bytes read as a positive script
    /// number: a cancel whose sigscript bytes [1..9) look like a fill's n > 0.
    SigPos(Keypair, u8),
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
    /// A covenant UTXO with an OpTrue script (an attacker-controlled covenant).
    Raw,
    /// Arbitrary signature script (a crafted P2SH input).
    RawSs {
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
                        Arg::SigPos(kp, ht) => (0..256)
                            .map(|_| sig_of(kp, *ht))
                            .find(|s| s[7] & 0x80 == 0 && s[..8].iter().any(|&b| b != 0))
                            .expect("a signature reading as n > 0")
                            .into(),
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
            Role::Raw => vec![],
            Role::RawSs { ss, .. } => ss.clone(),
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
        Role::Raw => "attacker.cov",
        Role::RawSs { name, .. } => name,
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
            Err(e) => {
                if std::env::var_os("KOB_ABLATION").is_some() {
                    // mutation run: a weakened contract may reject a positive scenario; report and go on
                    println!("ABLATION-POS-FAIL {} (input {i}: {e:?})", s.name);
                    return 0;
                }
                panic!("{}: input {i} ({}) failed: {e:?}", s.name, role_name(&s.inputs[i].role))
            }
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
            if r.is_ok() && std::env::var_os("KOB_ABLATION").is_some() {
                // mutation run (a check removed from a contract): report instead of failing
                println!("ABLATION-PASS {} all_inputs_ok={}", s.name, res.iter().all(|r| r.is_ok()));
                return;
            }
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

// ---------------------------------------------------------------- fixtures

struct Fx {
    net: Net,
    maker_a: Keypair,
    maker_b: Keypair,
    maker_c: Keypair,
    taker: Keypair,
    matcher: Keypair,
}
fn fx() -> Fx {
    Fx { net: Net::new(), maker_a: keypair(), maker_b: keypair(), maker_c: keypair(), taker: keypair(), matcher: keypair() }
}

const P250: i64 = 250_000_000; // 2.50 KAS per unit
const P255: i64 = 255_000_000;
const P260: i64 = 260_000_000;
const P245: i64 = 245_000_000;

// ---------------------------------------------------------------- trigger evidence (touch, v2.6)

/// Appends a token input of covenant `token` (the reference program). If `s` already has a token input of
/// that covenant (its first one leads the group), the new input delegates and `new_states` (the states of
/// token outputs the caller appends after every existing output) join the leader's next states; else the new
/// input leads with `new_states`. Returns the leader's index.
#[allow(clippy::too_many_arguments)]
fn push_tok(
    f: &Fx,
    s: &mut Scn,
    value: i64,
    amount: i64,
    owner: &[u8; 32],
    scheme: u8,
    token: Hash,
    wit: Wit,
    daa: u64,
    new_states: Vec<ArtifactValue>,
) -> usize {
    let leader = s.inputs.iter().position(|i| i.entry.covenant_id == Some(token));
    let mut inp = tok_in(&f.net.t, value, amount, owner, scheme, token, None, wit, daa);
    match leader {
        Some(l) => {
            match &mut s.inputs[l].role {
                Role::TokLeader { next, .. } => next.extend(new_states),
                _ => panic!("the first token input of the group does not lead"),
            }
            s.inputs.push(inp);
            l
        }
        None => {
            if let Role::TokDelegate { redeem, wit } = inp.role {
                inp.role = Role::TokLeader { redeem, next: new_states, wit };
            }
            s.inputs.push(inp);
            s.inputs.len() - 1
        }
    }
}

/// A look-alike evidence order: a P2SH script of the real redeem script's length whose state span holds the
/// real order's state (the push header of its data overwrites the first two state bytes, which no touch check
/// reads) under another template ("PUSHDATA2 data, drop, drop, true"). Its sigscript starts with 0x08 || n.
fn forged_ev(real: &[u8], n: i64, value: i64, c: Hash, daa: u64) -> Inp {
    let data_len = real.len() - 6;
    let mut redeem = vec![0x4d, (data_len & 0xff) as u8, (data_len >> 8) as u8];
    redeem.extend_from_slice(&real[3..3 + data_len]);
    redeem.extend([0x75, 0x75, 0x51]);
    assert_eq!(redeem.len(), real.len());
    let mut ss = vec![0x08];
    ss.extend(n.to_le_bytes());
    ss.extend(push(&redeem));
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&redeem), daa, false, Some(c)),
        role: Role::RawSs { ss, name: "ev.forged" },
        seq: 0,
    }
}
/// The real redeem script of an evidence order pushed (after 0x08 || n) by an input whose script public key
/// is not its P2SH ("2DROP TRUE"): the state is genuine, the UTXO is not that order.
fn not_p2sh_ev(real: &[u8], n: i64, value: i64, c: Hash, daa: u64) -> Inp {
    let mut ss = vec![0x08];
    ss.extend(n.to_le_bytes());
    ss.extend(push(real));
    Inp {
        entry: UtxoEntry::new(value as u64, ScriptPublicKey::new(0, vec![0x6d, 0x51].into()), daa, false, Some(c)),
        role: Role::RawSs { ss, name: "ev.not_p2sh" },
        seq: 0,
    }
}

/// Adds the evidence `e` after every existing input and output of `s`, so no index of the scenario moves:
/// input ei is the evidence order and output ei its positional payout (ask: maker C's KAS; bid: maker C's
/// tokens), ei = max(inputs, outputs) (missing inputs are padded with matcher P2PK inputs, missing outputs
/// with zero-value matcher outputs). An ask's custody is input ei + 1 (its tokens go to the matcher, who funds
/// the fill); a bid buys the matcher's tokens (input ei + 1). The matcher takes the change, so the evidence
/// part balances by itself. Returns (ev, tk) as the conditional orders take them (tk = -1 for a bid).
fn add_ev(f: &Fx, s: &mut Scn, e: &Ev) -> (i64, i64) {
    let t = &f.net.t;
    let mc = pk(&f.maker_c);
    let matcher = pk(&f.matcher);
    let (in0, out0) = (s.inputs.len(), s.outputs.len());
    while s.inputs.len() < s.outputs.len() {
        s.inputs.push(p2pk_in(&f.matcher, KAS));
    }
    while s.outputs.len() < s.inputs.len() {
        s.outputs.push(out(0, p2pk_spk(&matcher), None));
    }
    let ei = s.inputs.len();
    let whole = e.unit;
    let n = e.qty;
    let decay = e.slope != 0;
    let origin = if e.interval > 0 { e.active_from.max(e.daa as i64 + e.interval) } else { e.active_from };
    let t_arg = if decay { origin } else { 0 };
    let expiry = if matches!(e.mode, EvMode::Refund | EvMode::ZeroFill) { NOW as i64 } else { EXPIRY };
    let look = matches!(e.mode, EvMode::Forged | EvMode::NotP2sh);
    let owner = if e.mode == EvMode::Cancel { mc } else { matcher };
    if e.ask {
        let ap = AskP {
            qty: n,
            active_from: e.active_from,
            expiry,
            interval: e.interval,
            slope: e.slope,
            price_end: if decay { e.price / 2 } else { 0 },
            decay_step: if decay { 1 } else { 0 },
            token: e.token,
            unit: e.unit,
            ..AskP::new(mc, e.price)
        };
        let art = f.net.ask(&ap);
        let tk = ei as i64 + 1;
        let mut inp = match e.mode {
            EvMode::Fill => {
                call(&art, "settle", vec![nb(n * whole), iv(tk), iv(0), iv(t_arg)], "ev.ask.settle", CARRIER, e.cov, e.daa)
            }
            EvMode::Refund | EvMode::ZeroFill => {
                call(&art, "settle", vec![nb(0), iv(tk), iv(0), iv(0)], "ev.ask.refund", CARRIER, e.cov, e.daa)
            }
            EvMode::Cancel => call(&art, "cancel", vec![Arg::SigPos(f.maker_c, 0x01)], "ev.ask.cancel", CARRIER, e.cov, e.daa),
            EvMode::Forged => forged_ev(&bytecode(&art), n * whole, CARRIER, e.cov, e.daa),
            EvMode::NotP2sh => not_p2sh_ev(&bytecode(&art), n * whole, CARRIER, e.cov, e.daa),
        };
        inp.seq = e.interval as u64;
        s.inputs.push(inp);
        if matches!(e.mode, EvMode::Refund | EvMode::ZeroFill) {
            // the unsold tokens go back to maker C at the ask's own index
            let st = vec![tok_state(n * whole, &mc, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n * whole, &e.cov.as_bytes(), SCHEME_COVID, e.token, Wit::CovId, e.tok_daa, st);
            s.outputs.push(out(2 * CARRIER, t.spk(n * whole, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
        } else {
            let pay = if e.mode == EvMode::Fill { ask_all_in(n, e.price, TIP) + 2 * CARRIER } else { CARRIER };
            s.outputs.push(out(pay, p2pk_spk(if look { &matcher } else { &mc }), None));
            let st = vec![tok_state(n * whole, &owner, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n * whole, &e.cov.as_bytes(), SCHEME_COVID, e.token, Wit::CovId, e.tok_daa, st);
            s.outputs.push(out(CARRIER, t.spk(n * whole, &owner, SCHEME_P2PK), Some((l as u16, e.token))));
            s.inputs.push(p2pk_in(&f.matcher, pay + KAS));
        }
    } else {
        let bp = BidP {
            active_from: e.active_from,
            expiry,
            interval: e.interval,
            slope: e.slope,
            price_end: if decay { e.price + e.price / 10 } else { 0 },
            decay_step: if decay { 1 } else { 0 },
            token: e.token,
            unit: e.unit,
            ..BidP::new(mc, e.price)
        };
        let art = f.net.bid(&bp);
        let v = n * bp.token_max() + DC;
        let mut inp = match e.mode {
            EvMode::Fill => call(&art, "fill", vec![nb(n * whole), iv(ei as i64 + 1), iv(t_arg)], "ev.bid.fill", v, e.cov, e.daa),
            EvMode::Refund => call(&art, "refund", vec![], "ev.bid.refund", v, e.cov, e.daa),
            EvMode::Cancel => call(&art, "cancel", vec![Arg::SigPos(f.maker_c, 0x01)], "ev.bid.cancel", v, e.cov, e.daa),
            EvMode::ZeroFill => call(&art, "fill", vec![nb(0), iv(ei as i64 + 1), iv(t_arg)], "ev.bid.fill0", v, e.cov, e.daa),
            EvMode::Forged => forged_ev(&bytecode(&art), n * whole, v, e.cov, e.daa),
            EvMode::NotP2sh => not_p2sh_ev(&bytecode(&art), n * whole, v, e.cov, e.daa),
        };
        inp.seq = e.interval as u64;
        s.inputs.push(inp);
        if e.mode == EvMode::Fill {
            // the bid is exhausted: everything but the all-in spend rides on its positional delivery
            let st = vec![tok_state(n * whole, &mc, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n * whole, &matcher, SCHEME_P2PK, e.token, Wit::P2pk(f.matcher), 1_000, st);
            s.outputs.push(out(v - bid_all_in(n, e.price, TIP), t.spk(n * whole, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
        } else {
            s.outputs.push(out(v, p2pk_spk(if look { &matcher } else { &mc }), None));
        }
    }
    let ins: i64 = s.inputs[in0..].iter().map(|i| i.entry.amount as i64).sum();
    let outs: i64 = s.outputs[out0..].iter().map(|o| o.value as i64).sum();
    s.outputs.push(out(ins - outs, p2pk_spk(&matcher), None));
    (ei as i64, if e.ask { ei as i64 + 1 } else { -1 })
}

// ---------------------------------------------------------------- buy-side parameters

#[derive(Clone)]
struct CondBP {
    maker: [u8; 32],
    tip: i64,
    active_from: i64,
    expiry: i64,
    tp: i64,
    stop: i64,
    slip_bps: i64,
    step: i64,
    gap: i64,
    wait: i64,
    min_units: i64,
    min_rest: i64,
    qty: i64,
    /// optional raw base-unit amountLeft (new v3 base-unit scenarios); overrides `qty * UNIT`
    qty_base: Option<i64>,
    /// smallest fill in BASE units (contract field minFill); default DEFAULT_MIN_FILL
    min_fill: i64,
    armed: i64,
    band_daa: i64,
    keeper_tip: i64,
    /// repeat IFD: the entry this exit re-arms (0 = none), its proceeds and prefund rates (sompi per whole token), deadline
    parent: [u8; 32],
    rpt_price: i64,
    rpt_pre: i64,
    rpt_until: i64,
}
impl CondBP {
    /// Buy OCO: limit leg at 2.00 (below the market), buy-stop 3.00 with the default 3% band
    /// (stop leg 3.09), 10 qty, evidence fills of >= 1 unit exposed >= 600 DAA.
    fn oco(maker: [u8; 32]) -> Self {
        CondBP {
            maker,
            tip: TIP,
            active_from: 0,
            expiry: EXPIRY,
            tp: 200_000_000,
            stop: 300_000_000,
            slip_bps: 300,
            step: 0,
            gap: 0,
            wait: 0,
            min_units: 1,
            min_rest: 600,
            qty: 10,
            qty_base: None,
            min_fill: DEFAULT_MIN_FILL,
            armed: 0,
            band_daa: 0,
            keeper_tip: 0,
            parent: [0; 32],
            rpt_price: 0,
            rpt_pre: 0,
            rpt_until: 0,
        }
    }
    fn with_qty(&self, qty: i64) -> Self {
        let mut c = self.clone();
        c.qty = qty;
        c
    }
    fn armed(&self) -> Self {
        let mut c = self.clone();
        c.armed = 1;
        c
    }
    /// Stop-leg price (band above the stop).
    fn stop_leg(&self) -> i64 {
        self.stop + self.stop * self.slip_bps / 10_000
    }
    fn leg_price(&self, leg: i64) -> i64 {
        if leg == 0 {
            self.tp
        } else {
            self.stop_leg()
        }
    }
    /// Worst quote either leg can pay (the budget price).
    fn worst(&self) -> i64 {
        self.tp.max(self.stop_leg())
    }
}
#[derive(Clone)]
struct IfdAP {
    maker: [u8; 32],
    price: i64,
    tip: i64,
    prefund: i64,
    exit_carrier: i64,
    exit: CondBP,
    min_fill: i64,
    entry_stop: i64,
    band_daa: i64,
    keeper_tip: i64,
    armed: i64,
    qty: i64,
    /// optional raw base-unit amountLeft / custody (new v3 base-unit scenarios); overrides `qty * UNIT`
    qty_base: Option<i64>,
    /// repeat: 0 = off, else re-arm budget in WHOLE tokens (multiplied by UNIT into rptAmount base units)
    rpt: i64,
    /// optional raw base-unit rptAmount (new v3 merge-limit scenarios); overrides `rpt * UNIT`
    rpt_base: Option<i64>,
    /// stop entry: smallest evidence fill (whole tokens; multiplied by UNIT into base units)
    min_touch: i64,
}
impl IfdAP {
    /// rptAmount in base units: the raw override, or the whole-token budget scaled.
    fn rpt_amount(&self) -> i64 {
        self.rpt_base.unwrap_or(self.rpt * UNIT)
    }
}

fn condb(n: &Net, p: &CondBP) -> SilAbiArtifact {
    let mut a = n.ev_consts();
    a.push(bytes(&p.maker));
    a.extend(n.tok_args());
    a.extend([
        bytes(&EXT),
        int(UNIT),       // scale
        int(p.min_fill), // minFill in base units
        int(p.tip),
        int(p.active_from),
        int(p.expiry),
        int(REFUND_TIP),
        int(DC),
        int(p.tp),
        int(p.stop),
        int(p.slip_bps),
        int(p.step),
        int(p.gap),
        int(p.wait),
        int(p.min_units * UNIT), // minTouch in base units
        int(p.min_rest),
        int(p.qty_base.unwrap_or(p.qty * UNIT)), // amountLeft in base units
        int(p.armed),
        int(p.band_daa),
        int(p.keeper_tip),
        bytes(&p.parent),
        int(p.rpt_price),
        int(p.rpt_pre),
        int(p.rpt_until),
    ]);
    compile_contract(&src("KobCondBid"), &a, CompileOptions::default()).expect("compile KobCondBid")
}
fn condb_tpl(n: &Net) -> Tpl {
    tpl_of(&condb(n, &CondBP::oco([1; 32])))
}
fn condb_state(n: &Net, p: &CondBP) -> Vec<u8> {
    let a = condb(n, p);
    let l = state_layout(&a);
    bytecode(&a)[l.start..l.start + CONDB_COMMIT_LEN].to_vec()
}
fn ifda(n: &Net, p: &IfdAP) -> SilAbiArtifact {
    let tpl = condb_tpl(n);
    let mut a = n.tpl_consts(&tpl);
    a.extend(n.tpl_consts(&n.ask_tpl));
    a.push(bytes(&p.maker));
    a.extend(n.tok_args());
    a.extend([
        int(UNIT), // scale
        int(p.price),
        int(p.tip),
        int(0), // activeFrom
        int(EXPIRY),
        int(REFUND_TIP),
        int(p.prefund),
        int(p.exit_carrier),
        int(p.min_fill), // minFill in base units
        int(p.entry_stop),
        int(p.band_daa),
        int(p.min_touch * UNIT), // minTouch in base units
        int(600),                // minRestDaa
        int(p.keeper_tip),
        int(p.armed),
        int(p.qty_base.unwrap_or(p.qty * UNIT)), // amountLeft in base units (= custody)
        int(p.rpt_amount()),                     // rptAmount in base units
        bytes(&condb_state(n, &p.exit)),
    ]);
    compile_contract(&src("KobIfdAsk"), &a, CompileOptions::default()).expect("compile KobIfdAsk")
}

// ---------------------------------------------------------------- buy-side fixtures

/// Value a conditional buy escrows: every whole at the worst leg all-in price plus one delivery
/// carrier.
fn condb_value(cp: &CondBP) -> i64 {
    cp.qty * (cp.worst() + cp.tip) + DC
}

/// Conditional-buy fill: condb at input 0 (cov 0xe1, UTXO DAA 2000) buys n qty on `leg` at its
/// all-in price from a taker's P2PK-owned tokens (input 1); the taker (its own matcher) receives
/// the whole all-in spend. With `ev`, the evidence fill (see add_ev) follows every other input and
/// output and the settle reads it. The order continues (amountLeft - n > 0) as `next_cp`, else terminates.
fn condb_fill(f: &Fx, cp: &CondBP, n: i64, leg: i64, next_cp: &CondBP, ev: Option<&Ev>) -> Scn {
    condb_fill_at(f, cp, n, leg, next_cp, ev, 0, cp.leg_price(leg))
}
/// As condb_fill, with the auction time argument `t_arg` and the leg quote the fill pays.
#[allow(clippy::too_many_arguments)]
fn condb_fill_at(f: &Fx, cp: &CondBP, n: i64, leg: i64, next_cp: &CondBP, ev: Option<&Ev>, t_arg: i64, leg_price: i64) -> Scn {
    let t = &f.net.t;
    let c = cov(0xe1);
    let order = condb(&f.net, cp);
    let taker = pk(&f.taker);
    let v = condb_value(cp);
    let spend = bid_all_in(n, leg_price, cp.tip).max(0);
    let cont = n < cp.qty;
    let mut s = Scn {
        name: "condb fill".into(),
        inputs: vec![
            call(&order, "settle", vec![nb(n * WHOLE), iv(1), iv(leg), iv(0), iv(t_arg)], "condb.settle", v, c, 2_000),
            tok_in(
                t,
                CARRIER,
                n * WHOLE,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(n * WHOLE, &cp.maker, SCHEME_P2PK)]),
                Wit::P2pk(f.taker),
                1_000,
            ),
        ],
        outputs: vec![out(if cont { DC } else { v - spend }, t.spk(n * WHOLE, &cp.maker, SCHEME_P2PK), Some((1, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
    };
    if cont {
        s.outputs.push(out(v - spend - DC, spk_of(&condb(&f.net, next_cp)), Some((0, c))));
    }
    s.outputs.push(out(spend + CARRIER - NET_FEE, p2pk_spk(&taker), None));
    if let Some(e) = ev {
        let (ei, _) = add_ev(f, &mut s, e);
        set_arg(&mut s, 0, 3, iv(ei));
    }
    s
}
/// Arming evidence for the buy-stop: a resting bid quoting 3.02 (>= stop 3.00) filled with 5 qty.
fn arm_ev() -> Ev {
    Ev::bid(302_000_000)
}
/// Trailing evidence: a resting ask quoting 2.80 (<= 3.00 - 0.05 - 0.10) sold out (5 qty).
fn trail_ev() -> Ev {
    Ev::ask(280_000_000)
}
/// Permissionless update (arm or trail) of condb (cov 0xe1, UTXO DAA 2000) next to the evidence fill
/// `ev` (inputs 2.., see add_ev); a keeper (input 1) runs it.
fn condb_update(f: &Fx, cp: &CondBP, next_cp: &CondBP, ev: &Ev, seq: u64) -> Scn {
    let c = cov(0xe1);
    let v = condb_value(cp);
    let keeper = keypair();
    let mut cin = call(&condb(&f.net, cp), "update", vec![iv(0), iv(0)], "condb.update", v, c, 2_000);
    cin.seq = seq;
    let mut s = Scn {
        name: "condb update".into(),
        inputs: vec![cin, p2pk_in(&keeper, 10 * KAS)],
        outputs: vec![out(v, spk_of(&condb(&f.net, next_cp)), Some((0, c))), out(10 * KAS - NET_FEE, p2pk_spk(&pk(&keeper)), None)],
        lock_time: NOW,
        payload: vec![],
    };
    let (ei, tk) = add_ev(f, &mut s, ev);
    set_arg(&mut s, 0, 0, iv(ei));
    set_arg(&mut s, 0, 1, iv(tk));
    s
}
fn trailing_b(maker: [u8; 32]) -> CondBP {
    let mut c = CondBP::oco(maker);
    c.step = 5_000_000;
    c.gap = 10_000_000;
    c.wait = 600;
    c
}
/// Exit of the sell-first bracket: limit leg 2.40 (take profit on the short), buy-stop 2.80 with
/// the default 3% band (stop leg 2.884). The committed amountLeft is irrelevant: every fill writes n.
fn ifda_exit(maker: [u8; 32]) -> CondBP {
    let mut c = CondBP::oco(maker);
    c.tp = 240_000_000;
    c.stop = 280_000_000;
    c
}
/// Pre-funded buy-back budget per whole beyond the proceeds, and KAS on each exit UTXO.
const IFDA_PREFUND: i64 = KAS / 2;
const IFDA_EXIT_CARRIER: i64 = 10 * KAS;
/// Length of the KobCondBid state an if-done ask commits to (the entry appends the repeat fields).
const CONDB_COMMIT_LEN: usize = 321;
/// Entry UTXO value: carrier + 10 qty of prefund + two exits' carriers (two partial fills).
const IFDA_VALUE: i64 = CARRIER + 10 * IFDA_PREFUND + 2 * IFDA_EXIT_CARRIER;
fn ifda_p(f: &Fx) -> IfdAP {
    let m = pk(&f.maker_a);
    IfdAP {
        maker: m,
        price: P250,
        tip: TIP,
        prefund: IFDA_PREFUND,
        exit_carrier: IFDA_EXIT_CARRIER,
        exit: ifda_exit(m),
        min_fill: DEFAULT_MIN_FILL,
        entry_stop: 0,
        band_daa: 0,
        keeper_tip: 0,
        armed: 0,
        qty: 10,
        qty_base: None,
        rpt: 0,
        rpt_base: None,
        min_touch: 1,
    }
}
/// Recomputes the genesis covenant id of output `out` (authorised by input `auth`) exactly as
/// consensus does (outpoints in build() are ([k+1; 32], k)) and rebinds the output to it.
fn regen(s: &mut Scn, out: usize, auth: usize) -> Hash {
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes([auth as u8 + 1; 32]), index: auth as u32 };
    let id = covenant_id(op, [(out as u32, &s.outputs[out])].into_iter());
    s.outputs[out].covenant = Some(CovenantBinding { authorizing_input: auth as u16, covenant_id: id });
    id
}
/// Sell-first IFO entry fill: the if-done ask (cov 0xf1, value `value`, holding `have` qty)
/// sells n qty at its all-in price to a taker. Output 0 is the fresh KobCondBid exit (genesis,
/// authorised by input 0) with amountLeft = n. If qty remain, output 1 is the entry continuation
/// and output 2 the token remainder (tokOut = 2).
fn ifda_fill(f: &Fx, ip: &IfdAP, have: i64, n: i64, value: i64) -> Scn {
    let t = &f.net.t;
    let i = cov(0xf1);
    let entry = ifda(&f.net, &IfdAP { qty: have, ..ip.clone() });
    let exit = condb(&f.net, &ip.exit.with_qty(n));
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let proceeds = ask_all_in(n, ip.price, ip.tip).max(0);
    let rest = have - n;
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest * WHOLE, &i.as_bytes(), SCHEME_COVID));
    }
    next.push(tok_state(n * WHOLE, &taker, SCHEME_P2PK));
    let mut outputs = vec![];
    if rest > 0 {
        outputs.push(out(proceeds + n * ip.prefund + ip.exit_carrier, spk_of(&exit), None));
        let cont = ifda(&f.net, &IfdAP { qty: rest, ..ip.clone() });
        outputs.push(out(value - n * ip.prefund - ip.exit_carrier, spk_of(&cont), Some((0, i))));
        outputs.push(out(CARRIER, t.spk(rest * WHOLE, &i.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))));
    } else {
        outputs.push(out(proceeds + value + CARRIER, spk_of(&exit), None));
    }
    outputs.push(out(CARRIER, t.spk(n * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    outputs.push(out(1000 * KAS - proceeds - CARRIER - NET_FEE, p2pk_spk(&taker), None));
    let mut s = Scn {
        name: format!("B7 sell-first IFO entry sells {n}/{have} qty, fresh exit with amountLeft = {n}"),
        inputs: vec![
            call(
                &entry,
                "settle",
                vec![
                    nb(n * WHOLE),
                    iv(1),
                    iv(if rest > 0 { 2 } else { 0 }),
                    iv(0),
                    Arg::V(bytes(&tpl.pre)),
                    Arg::V(bytes(&tpl.suf)),
                    iv(0),
                    iv(0),
                    iv(0),
                ],
                "ifda.settle",
                value,
                i,
                1_000,
            ),
            tok_in(t, CARRIER, have * WHOLE, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
    };
    regen(&mut s, 0, 0);
    s
}
fn b7(f: &Fx) -> Scn {
    ifda_fill(f, &ifda_p(f), 10, 4, IFDA_VALUE)
}
/// Second partial fill: the continuation of b7 (6 qty, value after one exit) sells out.
fn b7b(f: &Fx) -> Scn {
    let ip = ifda_p(f);
    ifda_fill(f, &ip, 6, 6, IFDA_VALUE - 4 * ip.prefund - ip.exit_carrier)
}

// ---------------------------------------------------------------- buy-side tests

#[test]
fn v2_buy_sizes_layouts_sigops() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let arts = [("KobCondBid", condb(n, &CondBP::oco(m)), 381), ("KobIfdAsk", ifda(n, &ifda_p(&f)), 594)];
    for (name, a, st) in arts.iter() {
        let bc = bytecode(a);
        let l = state_layout(a);
        assert_eq!((l.start, l.len), (1, *st), "{name}: state span");
        let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
            &push(&bc),
            &pay_to_script_hash_script(&bc),
        );
        println!(
            "SIZE {name}: redeem script {} B (state {} B, template {} B), static sig-op upper bound {so}",
            bc.len(),
            st,
            bc.len() - st
        );
        assert!(so <= 15, "{name}: sig-op cap");
    }
    // The contSpk splice windows of KobCondBid must match the compiler's state encoding.
    let mut cp = CondBP::oco(m);
    cp.qty = 7;
    cp.armed = 1;
    let c = bytecode(&condb(n, &cp));
    assert_eq!(c[223], 0x08, "stopPrice push prefix");
    assert_eq!(i64::from_le_bytes(c[224..232].try_into().unwrap()), cp.stop, "stopPrice window");
    assert_eq!(c[286], 0x08, "amountLeft push prefix");
    assert_eq!(i64::from_le_bytes(c[287..295].try_into().unwrap()), 7 * UNIT, "amountLeft window (base units)");
    assert_eq!(c[295], 0x08, "armed push prefix");
    assert_eq!(i64::from_le_bytes(c[296..304].try_into().unwrap()), 1, "armed window");
    // KobIfdAsk splices the exit's amountLeft at KobCondBid STATE payload [286..294) (bytecode
    // window [287..295) minus the 1-byte state offset).
    let st = condb_state(n, &cp);
    assert_eq!(st[285], 0x08, "amountLeft push prefix (state coordinates)");
    assert_eq!(i64::from_le_bytes(st[286..294].try_into().unwrap()), 7 * UNIT, "amountLeft window (state coordinates)");
    assert_eq!(st.len(), 321, "KobCondBid state length (KobIfdAsk exitState)");
    // KobIfdAsk (594-byte state) splices armed [245..253), amountLeft [254..262) and rptAmount [263..271)
    // (bytecode / redeem-script coordinates, state starts at byte 1).
    let mut ip = ifda_p(&f);
    ip.armed = 1_234;
    ip.qty = 9;
    let ib = bytecode(&ifda(n, &ip));
    let le = |s: &[u8], a: usize, b: usize| i64::from_le_bytes(s[a..b].try_into().unwrap());
    assert_eq!((ib[244], le(&ib, 245, 253)), (0x08, 1_234), "ifda armed splice window");
    assert_eq!((ib[253], le(&ib, 254, 262)), (0x08, 9 * UNIT), "ifda amountLeft splice window (base units)");
    let mut ipr = ifda_p(&f);
    ipr.rpt_base = Some(4_444);
    let irb = bytecode(&ifda(n, &ipr));
    assert_eq!((irb[262], le(&irb, 263, 271)), (0x08, 4_444), "ifda rptAmount splice window");
    // Repeat fields of KobCondBid (after keeperTip): parent [323..355), rptPrice [356..364), rptPre
    // [365..373), rptUntil [374..382) (bytecode); KobIfdAsk reads parent at state [322..354), the
    // exit amountLeft at [286..294) and its extensionCommitment at [118..150).
    let mut cr = CondBP::oco(m);
    cr.parent = [0xab; 32];
    cr.rpt_price = 11;
    cr.rpt_pre = 22;
    cr.rpt_until = 33;
    let crb = bytecode(&condb(n, &cr));
    assert_eq!((crb[322], &crb[323..355]), (0x20, &[0xab; 32][..]), "condb parent field");
    assert_eq!((crb[355], le(&crb, 356, 364)), (0x08, 11), "condb rptPrice field");
    assert_eq!((crb[364], le(&crb, 365, 373)), (0x08, 22), "condb rptPre field");
    assert_eq!((crb[373], le(&crb, 374, 382)), (0x08, 33), "condb rptUntil field");
    let cst = &crb[1..382];
    assert_eq!(&cst[322..354], &[0xab; 32][..], "condb parent at state [322..354)");
    assert_eq!(&cst[118..150], &EXT[..], "condb extensionCommitment at state [118..150)");
    let t = condb_tpl(n);
    println!("TEMPLATES condbid {}+{} B", t.pre.len(), t.suf.len());
}

#[test]
fn v2_buy_positive() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_b);
    let oco = CondBP::oco(m);
    run_ok(n, &with_name(condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None), "B1 buy OCO limit leg buys 4/10 at 2.00 all-in"));
    let r = arm_ev();
    run_ok(
        n,
        &with_name(
            condb_fill(&f, &oco, 4, 1, &oco.with_qty(6).armed(), Some(&r)),
            "B2 buy-stop leg armed in its own fill: a bid at 3.02 (>= stop) filled in the same tx, buys 4/10 in the default 3% band (3.09), continues armed with 6 qty",
        ),
    );
    run_ok(n, &with_name(condb_update(&f, &oco, &oco.armed(), &r, 0), "B3 permissionless arm next to a resting-bid fill"));
    run_ok(
        n,
        &with_name(condb_fill(&f, &oco.armed(), 4, 1, &oco.with_qty(6).armed(), None), "B4 armed buy-stop leg buys without evidence"),
    );
    let tr = trailing_b(m);
    let mut tr2 = tr.clone();
    tr2.stop -= 2 * tr.step;
    run_ok(
        n,
        &with_name(
            condb_update(&f, &tr, &tr2, &trail_ev(), tr.wait as u64),
            "B5 trailing buy stop ratchets 3.00 -> 2.90: one ask fill at 2.80 justifies two steps (gap 0.10)",
        ),
    );
    let small = oco.with_qty(4);
    run_ok(
        n,
        &with_name(condb_fill(&f, &small, 4, 0, &small, None), "B6 last 4 qty bought: order terminates, delivery carries the rest"),
    );
    let mut sl = oco.clone();
    sl.tp = 0;
    run_ok(
        n,
        &with_name(condb_fill(&f, &sl, 4, 1, &sl.with_qty(6).armed(), Some(&r)), "B6b buy-stop-limit (no limit leg) triggered fill"),
    );
    let s7 = b7(&f);
    let exit_id = s7.outputs[0].covenant.as_ref().unwrap().covenant_id;
    run_ok(n, &s7);
    run_ok(n, &with_name(b7b(&f), "B7b second partial fill (6/6) sells out: a second, independent exit with amountLeft = 6"));
    // The exit of the first fill (genesis id from B7) works as a normal conditional buy for its
    // 4 qty: its stop leg (2.80, band 2.884) arms on a resting bid quoting 2.85 filled in the same
    // transaction and buys 2 of them.
    let exit = ifda_exit(pk(&f.maker_a)).with_qty(4);
    let up = Ev::bid(285_000_000);
    let mut s = with_name(
        condb_fill(&f, &exit, 2, 1, &exit.with_qty(2).armed(), Some(&up)),
        "B8 exit of B7 (genesis id) buy-stop fills 2 of its 4 qty",
    );
    s.inputs[0].entry = utxo(s.inputs[0].entry.amount as i64, s.inputs[0].entry.script_public_key.clone(), exit_id, 2_000);
    s.outputs[1].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: exit_id });
    run_ok(n, &s);
    let mut zp = ifda_p(&f);
    zp.tip = 0;
    run_ok(n, &with_name(ifda_fill(&f, &zp, 10, 4, IFDA_VALUE), "B11 zero-tip if-done ask, partial 4/10"));
    let mut z = oco.clone();
    z.tip = 0;
    run_ok(
        n,
        &with_name(
            condb_fill(&f, &z, 4, 0, &z.with_qty(6), None),
            "B9 zero-tip conditional buy filled at exactly its limit-leg quote",
        ),
    );
    let mut band = oco.clone();
    band.slip_bps = 100;
    run_ok(
        n,
        &with_name(
            condb_fill(&f, &band, 4, 1, &band.with_qty(6).armed(), Some(&r)),
            "B10 buy-stop-market with a user band of 1% (3.03)",
        ),
    );
}

#[test]
fn v2_buy_negative() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_b);
    let oco = CondBP::oco(m);
    let good = arm_ev();
    let stop_fill = |cp: &CondBP, e: &Ev, name: &str| with_name(condb_fill(&f, cp, 4, 1, &cp.with_qty(6).armed(), Some(e)), name);

    let mut s = with_name(
        condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None),
        "NB1 limit leg charged 1 sompi above its all-in price (continuation short)",
    );
    s.outputs[1].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(n, &s, 0);

    let mut s = with_name(
        condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None),
        "NB2 limit leg charged 1 sompi above its all-in price (delivery carrier short)",
    );
    s.outputs[0].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(n, &s, 0);

    let mut s = with_name(condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None), "NB3 malleated n=3 (outputs for 4)");
    set_arg(&mut s, 0, 0, nb(3 * WHOLE));
    run_bad(n, &s, 0);

    let mut s = with_name(
        condb_fill(&f, &oco.armed(), 4, 1, &oco.with_qty(6).armed(), None),
        "NB4 armed stop leg charged 1 sompi above its band",
    );
    s.outputs[1].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(n, &s, 0);

    let mut s =
        with_name(condb_fill(&f, &oco, 4, 1, &oco.with_qty(6).armed(), None), "NB5 stop leg without evidence (ev = the token input)");
    set_arg(&mut s, 0, 3, iv(1));
    run_bad(n, &s, 0);

    let mut stop_only = oco.armed();
    stop_only.tp = 0;
    run_bad(
        n,
        &with_name(condb_fill(&f, &stop_only, 4, 0, &stop_only.with_qty(6), None), "NB6 limit leg requested on a stop-only order"),
        0,
    );

    run_bad(n, &stop_fill(&oco, &Ev::ask(302_000_000), "NB7 buy-stop armed by a resting ASK fill (wrong side)"), 0);
    run_bad(
        n,
        &stop_fill(&oco, &good.with(|e| e.daa = NOW - 500), "NB8 wash print: the evidence bid was exposed only 500 DAA (< 600)"),
        0,
    );
    run_bad(n, &stop_fill(&oco, &good.with(|e| e.active_from = NOW as i64 - 599), "NB9 evidence bid active only R - 1 DAA"), 0);
    run_bad(n, &stop_fill(&oco, &good.with(|e| e.token = OTHER_COV), "NB10 evidence of another token"), 0);
    run_bad(
        n,
        &stop_fill(
            &oco,
            &good.with(|e| e.mode = EvMode::Forged),
            "NB11 look-alike evidence: a real bid's state under another template",
        ),
        0,
    );
    let mut o6 = oco.clone();
    o6.min_units = 6;
    run_bad(n, &stop_fill(&o6, &good, "NB12 evidence fill of 5 units < required 6"), 0);
    run_bad(n, &stop_fill(&oco, &Ev::bid(299_999_999), "NB13 evidence quote below the buy-stop"), 0);

    let small = oco.with_qty(4);
    run_bad(n, &with_name(condb_fill(&f, &small, 5, 0, &small, None), "NB14 n=5 > amountLeft=4"), 0);

    run_bad(n, &with_name(condb_fill(&f, &oco, 4, 0, &oco, None), "NB15 amountLeft not decremented (over-buying)"), 0);

    run_bad(
        n,
        &with_name(
            condb_fill(&f, &oco, 4, 0, &oco.with_qty(6).armed(), None),
            "NB16 limit-leg fill sneaks armed=1 into the continuation",
        ),
        0,
    );

    let mut s =
        with_name(condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None), "NB17 two UTXOs share the covenant id (double fill attempt)");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(n, &s, 0);

    let mut s = with_name(condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None), "NB18 delivery to a non-maker key");
    let thief = pk(&f.taker);
    s.outputs[0].script_public_key = n.t.spk(4 * WHOLE, &thief, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(4 * WHOLE, &thief, SCHEME_P2PK)]);
    run_bad(n, &s, 0);

    run_bad(n, &with_name(condb_update(&f, &oco.armed(), &oco.armed(), &good, 0), "NB19 update on an already armed order"), 0);

    let tr = trailing_b(m);
    let mut tr2 = tr.clone();
    tr2.stop -= 2 * tr.step; // the ask fill at 2.80 justifies two steps
    run_bad(n, &with_name(condb_update(&f, &tr, &tr2, &trail_ev(), tr.wait as u64 - 1), "NB20 trail sooner than trailWait (CSV)"), 0);
    let mut tr1 = tr.clone();
    tr1.stop -= tr.step;
    run_bad(
        n,
        &with_name(condb_update(&f, &tr, &tr1, &trail_ev(), tr.wait as u64), "NB21 trail by one step when the evidence justifies two"),
        0,
    );
    let mut tr3 = tr.clone();
    tr3.stop -= 3 * tr.step;
    run_bad(
        n,
        &with_name(condb_update(&f, &tr, &tr3, &trail_ev(), tr.wait as u64), "NB21b trail by three steps (one more than justified)"),
        0,
    );
    let mut low = tr.clone();
    low.stop = 205_000_000;
    let mut low2 = low.clone();
    low2.stop = 200_000_000;
    let dn = Ev::ask(150_000_000);
    run_bad(n, &with_name(condb_update(&f, &low, &low2, &dn, tr.wait as u64), "NB22 trail down to the limit leg (travel cap)"), 0);
    let dn = Ev::ask(285_000_001);
    run_bad(n, &with_name(condb_update(&f, &tr, &tr2, &dn, tr.wait as u64), "NB23 trail evidence above stop-step-gap"), 0);
    let mut o2 = oco.clone();
    o2.stop -= 5_000_000;
    run_bad(n, &with_name(condb_update(&f, &oco, &o2, &trail_ev(), 600), "NB24 trail a non-trailing order"), 0);
    let mut s = with_name(condb_update(&f, &oco, &oco.armed(), &good, 0), "NB25 arm update skims the escrow by 1 sompi");
    s.outputs[0].value -= 1;
    s.outputs[1].value += 1;
    run_bad(n, &s, 0);

    let mut wide = oco.armed();
    wide.slip_bps = 10_001;
    run_bad(n, &with_name(condb_fill(&f, &wide, 4, 1, &wide.with_qty(6), None), "NB26 stop band above 100%"), 0);

    // sell-first IFO entry with partial fills (each fill = one fresh exit)
    let ip = ifda_p(&f);
    let taker = pk(&f.taker);
    let last = |s: &Scn| s.outputs.len() - 1;

    let mut s = with_name(b7(&f), "NI1 over-delivery: exit amountLeft 5 for a 4-whole fill");
    s.outputs[0].script_public_key = spk_of(&condb(n, &ip.exit.with_qty(5)));
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI2 exit state tampered (buy-stop removed)");
    let mut e = ip.exit.with_qty(4);
    e.stop = 0;
    s.outputs[0].script_public_key = spk_of(&condb(n, &e));
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI2b exit state tampered (band 3% -> 10%)");
    let mut e = ip.exit.with_qty(4);
    e.slip_bps = 1_000;
    s.outputs[0].script_public_key = spk_of(&condb(n, &e));
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);

    // Double exit: a second exit output in the SAME genesis group (one id for both). Consensus
    // accepts the two-output group; the entry's recomputed one-output id no longer matches.
    let mut s = with_name(b7(&f), "NI3 double exit: two outputs share the exit's genesis id");
    let dup = s.outputs[0].clone();
    s.outputs.push(dup);
    let l = last(&s);
    s.outputs[l - 1].value -= s.outputs[l].value as u64;
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes([1; 32]), index: 0 };
    let id = covenant_id(op, [(0u32, &s.outputs[0]), (l as u32, &s.outputs[l])].into_iter());
    for k in [0, l] {
        s.outputs[k].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: id });
    }
    run_bad(n, &s, 0);

    // Aliasing: the "exit" is a genesis authorised by ANOTHER input (the taker's), as another
    // entry's exit would be. The entry's recomputed id (its own outpoint) does not match.
    let mut s = with_name(b7(&f), "NI3b exit genesis authorised by another input (claimed by a second entry)");
    regen(&mut s, 0, 2);
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI4 exit reuses the entry's covenant id");
    s.outputs[0].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: cov(0xf1) });
    run_bad(n, &s, 0);

    // The attacker spends its own covenant Y (OpTrue script) and creates the "exit" as a
    // continuation of Y: tokens bought back later would be owned by an id the attacker can
    // co-spend. The entry authorises only its continuation, so it rejects.
    let mut s = with_name(b7(&f), "NI5 exit authorised by an attacker covenant (continuation, not genesis)");
    let y = cov(0x99);
    let ev = s.outputs[0].value as i64;
    s.inputs.push(Inp { entry: utxo(ev, ScriptPublicKey::new(0, vec![OpTrue].into()), y, 500), role: Role::Raw, seq: 0 });
    s.outputs[0].covenant = Some(CovenantBinding { authorizing_input: 3, covenant_id: y });
    let l = last(&s);
    s.outputs[l].value += ev as u64;
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI6 prefund skimmed: exit short by 1 sompi");
    s.outputs[0].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);

    let mut s = with_name(b7b(&f), "NI7 sold out below the all-in price: last exit short by 1 sompi");
    s.outputs[0].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI8 entry continuation short by 1 sompi");
    s.outputs[1].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI9 token remainder mis-owned (sent to the taker)");
    s.outputs[2].script_public_key = n.t.spk(6 * WHOLE, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6 * WHOLE, &taker, SCHEME_P2PK), tok_state(4 * WHOLE, &taker, SCHEME_P2PK)]);
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI10 exit built from another template (KobCondAsk)");
    set_arg(&mut s, 0, 4, Arg::V(bytes(&n.cond_tpl.pre)));
    set_arg(&mut s, 0, 5, Arg::V(bytes(&n.cond_tpl.suf)));
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI11 two UTXOs share the if-done covenant id");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(n, &s, 0);

    let mut bad = ifda_p(&f);
    bad.tip = -1;
    run_bad(n, &with_name(ifda_fill(&f, &bad, 10, 4, IFDA_VALUE), "NI12 if-done ask with a negative tip is unfillable"), 0);

    let mut s = with_name(b7(&f), "NI13 malleated n=5 (outputs for 4)");
    set_arg(&mut s, 0, 0, nb(5 * WHOLE));
    run_bad(n, &s, 0);

    let mut s = with_name(b7(&f), "NI14 continuation dropped while tokens remain (exit only)");
    s.outputs.remove(1);
    for o in s.outputs.iter_mut() {
        if let Some(c) = o.covenant.as_mut() {
            if c.covenant_id == TOKEN_COV {
                c.authorizing_input = 1;
            }
        }
    }
    set_arg(&mut s, 0, 2, iv(1));
    let l = last(&s);
    s.outputs[l].value += (IFDA_VALUE - 4 * ip.prefund - ip.exit_carrier) as u64;
    run_bad(n, &s, 0);
}

// ---------------------------------------------------------------- v2.3 buy-side lifecycle

/// Sell-first if-done fill with every lever: the quote the fill pays (`eff`), an optional trigger
/// evidence (added after every other input and output, see add_ev), the auction time `t` and the
/// armed state of the continuation.
#[allow(clippy::too_many_arguments)]
fn ifda_fill_x(f: &Fx, ip: &IfdAP, have: i64, n: i64, value: i64, eff: i64, ev: Option<&Ev>, t_arg: i64, next_armed: i64) -> Scn {
    let t = &f.net.t;
    let i = cov(0xf1);
    let entry = ifda(&f.net, &IfdAP { qty: have, ..ip.clone() });
    let exit = condb(&f.net, &ip.exit.with_qty(n));
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let proceeds = ask_all_in(n, eff, ip.tip).max(0);
    let rest = have - n;
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest * WHOLE, &i.as_bytes(), SCHEME_COVID));
    }
    next.push(tok_state(n * WHOLE, &taker, SCHEME_P2PK));
    let mut outputs = vec![];
    if rest > 0 {
        outputs.push(out(proceeds + n * ip.prefund + ip.exit_carrier, spk_of(&exit), None));
        let cont = ifda(&f.net, &IfdAP { qty: rest, armed: next_armed, ..ip.clone() });
        outputs.push(out(value - n * ip.prefund - ip.exit_carrier, spk_of(&cont), Some((0, i))));
        outputs.push(out(CARRIER, t.spk(rest * WHOLE, &i.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))));
    } else {
        outputs.push(out(proceeds + value + CARRIER, spk_of(&exit), None));
    }
    outputs.push(out(CARRIER, t.spk(n * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    outputs.push(out(1000 * KAS - proceeds - CARRIER - NET_FEE, p2pk_spk(&taker), None));
    let inputs = vec![
        call(
            &entry,
            "settle",
            vec![
                nb(n * WHOLE),
                iv(1),
                iv(if rest > 0 { 2 } else { 0 }),
                iv(0),
                Arg::V(bytes(&tpl.pre)),
                Arg::V(bytes(&tpl.suf)),
                iv(0),
                iv(0),
                iv(t_arg),
            ],
            "ifda.settle",
            value,
            i,
            1_000,
        ),
        tok_in(t, CARRIER, have * WHOLE, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000),
        p2pk_in(&f.taker, 1000 * KAS),
    ];
    let mut s = Scn { name: "ifda fill".into(), inputs, outputs, lock_time: NOW, payload: vec![] };
    regen(&mut s, 0, 0);
    if let Some(e) = ev {
        let (ei, tk) = add_ev(f, &mut s, e);
        set_arg(&mut s, 0, 6, iv(ei));
        set_arg(&mut s, 0, 7, iv(tk));
    }
    s
}
/// Permissionless arm of a sell-stop if-done entry (cov 0xf1, 10-whole custody not spent); the
/// keeper takes `take` from the entry's carrier. `with_token`: the attacker also spends the
/// entry's token UTXO.
fn ifda_update(f: &Fx, ip: &IfdAP, next: &IfdAP, ev: &Ev, take: i64, with_token: bool) -> Scn {
    let i = cov(0xf1);
    let keeper = keypair();
    let mut s = Scn {
        name: "ifda update".into(),
        inputs: vec![
            call(&ifda(&f.net, ip), "update", vec![iv(0), iv(0)], "ifda.update", IFDA_VALUE, i, 1_000),
            p2pk_in(&keeper, 10 * KAS),
        ],
        outputs: vec![
            out(IFDA_VALUE - take, spk_of(&ifda(&f.net, next)), Some((0, i))),
            out(10 * KAS + take - NET_FEE, p2pk_spk(&pk(&keeper)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    let (ei, tk) = add_ev(f, &mut s, ev);
    set_arg(&mut s, 0, 0, iv(ei));
    set_arg(&mut s, 0, 1, iv(tk));
    if with_token {
        // the entry's own 10-whole custody, co-spent to the keeper
        let thief = pk(&keeper);
        let st = vec![tok_state(10 * WHOLE, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 10 * WHOLE, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, f.net.t.spk(10 * WHOLE, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
    }
    s
}
/// As condb_update; the keeper takes `take` sompi from the escrow.
fn condb_update_tip(f: &Fx, cp: &CondBP, next_cp: &CondBP, ev: &Ev, seq: u64, take: i64) -> Scn {
    let mut s = condb_update(f, cp, next_cp, ev, seq);
    s.outputs[0].value -= take as u64;
    s.outputs[1].value += take as u64;
    s
}
/// Anyone refunds a conditional buy (cov 0xe1) at its soft expiry.
fn condb_refund(f: &Fx, cp: &CondBP) -> Scn {
    let v = condb_value(cp);
    Scn {
        name: "condb refund".into(),
        inputs: vec![call(&condb(&f.net, cp), "refund", vec![], "condb.refund", v, cov(0xe1), (EXPIRY - 1_000) as u64)],
        outputs: vec![out(v - REFUND_TIP, p2pk_spk(&cp.maker), None)],
        lock_time: EXPIRY as u64,
        payload: vec![],
    }
}
/// A token UTXO of `qty` qty owned by covenant id `owner` (scheme 0x04).
fn owned_tok(f: &Fx, qty: i64, owner: Hash, next: Option<Vec<ArtifactValue>>) -> Inp {
    tok_in(&f.net.t, CARRIER, qty * WHOLE, &owner.as_bytes(), SCHEME_COVID, TOKEN_COV, next, Wit::CovId, 1_500)
}

#[test]
fn v2_buy_lifecycle() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_b);
    let taker = pk(&f.taker);

    // Buy-stop auction (stop 3.00, 3% band over 300 DAA): the price opens UP from the stop.
    let mut a = CondBP::oco(m);
    a.band_daa = 300;
    let stop = a.stop;
    let at = |e: i64| stop + stop * (if e < 300 { 300 * e / 300 } else { 300 }) / 10_000;
    let armed_as = |c: &CondBP, v: i64| CondBP { armed: v, ..c.clone() };
    let a1 = armed_as(&a, 1);
    let next2000 = armed_as(&a.with_qty(6), 2_000);
    let fill = |cp: &CondBP, next: &CondBP, ev: Option<&Ev>, t: i64, q: i64, name: &str| {
        with_name(condb_fill_at(&f, cp, 4, 1, next, ev, t, q), name)
    };
    assert_eq!(at(150), 304_500_000);
    run_ok(
        n,
        &fill(
            &a1,
            &next2000,
            None,
            2_150,
            at(150),
            "B12 buy-stop auction 150/300 DAA after arming: 3.045, remainder keeps origin 2000",
        ),
    );
    let a1900 = armed_as(&a, 1_900);
    run_ok(n, &fill(&a1900, &armed_as(&a.with_qty(6), 1_900), None, 2_150, at(250), "B12b stored origin 1900: 3.075"));
    let r = arm_ev();
    run_ok(
        n,
        &fill(
            &a,
            &armed_as(&a.with_qty(6), 1),
            Some(&r),
            0,
            stop,
            "B12c arm + fill in one tx (auction starts at the trigger): pays the stop 3.00",
        ),
    );
    run_bad(n, &fill(&a1, &next2000, None, 2_150, a.stop_leg(), "NB27 buy-stop auction charged the band ceiling at 150/300 DAA"), 0);
    run_bad(n, &fill(&a1, &armed_as(&a.with_qty(6), 1), None, 2_150, at(150), "NB28 continuation restarts the buy auction"), 0);
    run_bad(
        n,
        &fill(&a1, &next2000, None, 1_999, stop - stop / 10_000, "NB29 buy auction time before the arming origin (priced at t)"),
        0,
    );

    // Keeper tip on arm / trail.
    let mut kt = CondBP::oco(m);
    kt.keeper_tip = 1_000_000;
    run_ok(
        n,
        &with_name(condb_update_tip(&f, &kt, &kt.armed(), &r, 0, 1_000_000), "B13 arming keeper paid keeperTip from the escrow"),
    );
    run_bad(n, &with_name(condb_update_tip(&f, &kt, &kt.armed(), &r, 0, 1_000_001), "NB30 arming keeper takes keeperTip + 1"), 0);

    // Multi-step trail down, capped above the limit leg (2.00).
    let tr = trailing_b(m);
    let dn = Ev::ask(150_000_000);
    let mut capped = tr.clone();
    capped.stop = 205_000_000;
    run_ok(
        n,
        &with_name(
            condb_update(&f, &tr, &capped, &dn, tr.wait as u64),
            "B5c trail jump 3.00 -> 2.05 (28 steps justified, capped above the 2.00 limit leg)",
        ),
    );
    let mut over = tr.clone();
    over.stop = 160_000_000;
    run_bad(
        n,
        &with_name(condb_update(&f, &tr, &over, &dn, tr.wait as u64), "NB22b trail jump not capped (stop 1.60 <= limit leg)"),
        0,
    );

    // Sell-stop if-done entry: stop 2.60, limit 2.50, armed by the fill of a resting ask quoting <= 2.60.
    let mut ip = ifda_p(&f);
    ip.entry_stop = 260_000_000;
    let down = Ev::ask(255_000_000);
    run_ok(
        n,
        &with_name(
            ifda_fill_x(&f, &ip, 10, 4, IFDA_VALUE, P250, Some(&down), 0, 1),
            "B14 sell-stop IFO entry (stop 2.60, limit 2.50) armed inside the fill: 4/10 at the limit, continues armed",
        ),
    );
    let mut ipa = ip.clone();
    ipa.band_daa = 1_000;
    ipa.armed = 1;
    run_ok(
        n,
        &with_name(
            ifda_fill_x(&f, &ipa, 10, 4, IFDA_VALUE, 255_000_000, None, 1_500, 1_000),
            "B14b armed sell-stop entry auction 500/1000 DAA: sells at 2.55",
        ),
    );
    // arm + fill in one transaction with an auction: the auction opens at the trigger (the entry stop 2.60)
    let ipu = IfdAP { band_daa: 1_000, ..ip.clone() };
    run_ok(
        n,
        &with_name(
            ifda_fill_x(&f, &ipu, 10, 4, IFDA_VALUE, 260_000_000, Some(&down), 0, 1),
            "B14e arm + fill in one tx (auction starts at the trigger): sells at the stop 2.60",
        ),
    );
    run_bad(
        n,
        &with_name(
            ifda_fill_x(&f, &ipu, 10, 4, IFDA_VALUE, P250, Some(&down), 0, 1),
            "NI18b arm + fill in one tx (auction) paid the limit 2.50 instead of the stop",
        ),
        0,
    );
    let mut ipk = ip.clone();
    ipk.keeper_tip = 1_000_000;
    run_ok(
        n,
        &with_name(
            ifda_update(&f, &ipk, &IfdAP { armed: 1, ..ipk.clone() }, &down, 1_000_000, false),
            "B14c permissionless arm of a sell-stop entry, keeper paid",
        ),
    );
    let mut ml = ifda_p(&f);
    ml.min_fill = 3 * WHOLE;
    run_ok(n, &with_name(ifda_fill_x(&f, &ml, 10, 3, IFDA_VALUE, P250, None, 0, 0), "B14d minFill 3: a 3-whole partial fill"));

    run_bad(
        n,
        &with_name(ifda_fill_x(&f, &ip, 10, 4, IFDA_VALUE, P250, None, 0, 1), "NI15 sell-stop entry filled without evidence"),
        0,
    );
    let side2 = Ev::bid(255_000_000);
    run_bad(
        n,
        &with_name(
            ifda_fill_x(&f, &ip, 10, 4, IFDA_VALUE, P250, Some(&side2), 0, 1),
            "NI16 sell-stop entry armed by a resting-BID fill (wrong side)",
        ),
        0,
    );
    let high = Ev::ask(260_000_001);
    run_bad(
        n,
        &with_name(ifda_fill_x(&f, &ip, 10, 4, IFDA_VALUE, P250, Some(&high), 0, 1), "NI17 evidence ask quoting above the entry stop"),
        0,
    );
    run_bad(
        n,
        &with_name(
            ifda_fill_x(&f, &ipa, 10, 4, IFDA_VALUE, P250, None, 1_500, 1_000),
            "NI18 auction entry paid its limit 2.50 at 500/1000 DAA (exit short)",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(
            ifda_fill_x(&f, &ipa, 10, 4, IFDA_VALUE, 255_000_000, None, 1_500, 1),
            "NI19 continuation restarts the entry auction",
        ),
        0,
    );
    run_bad(n, &with_name(ifda_fill_x(&f, &ml, 10, 2, IFDA_VALUE, P250, None, 0, 0), "NI20 partial fill below minFill"), 0);
    let mut low = ip.clone();
    low.entry_stop = 240_000_000;
    let dlow = Ev::ask(230_000_000);
    run_bad(
        n,
        &with_name(
            ifda_fill_x(&f, &low, 10, 4, IFDA_VALUE, P250, Some(&dlow), 0, 1),
            "NI21 sell-stop entry with its stop below its limit is unfillable",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(
            ifda_update(&f, &ipk, &IfdAP { armed: 1, ..ipk.clone() }, &down, 0, true),
            "NI22 arm update also spends the entry's token custody",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(
            ifda_update(&f, &ipk, &IfdAP { armed: 1, ..ipk.clone() }, &down, 1_000_001, false),
            "NI23 arming keeper takes keeperTip + 1",
        ),
        0,
    );

    // Strays.
    let e = cov(0xe1);
    let oco = CondBP::oco(m);
    let mut s = with_name(
        condb_fill(&f, &oco, 4, 0, &oco.with_qty(6), None),
        "NS10 conditional-buy fill co-spends a 3-whole stray owned by it",
    );
    s.inputs.push(owned_tok(&f, 3, e, None));
    set_leader_next(&mut s, 1, vec![tok_state(4 * WHOLE, &m, SCHEME_P2PK), tok_state(3 * WHOLE, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, n.t.spk(3 * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);
    run_ok(n, &with_name(condb_refund(&f, &oco), "B15 anyone refunds a conditional buy at its soft expiry"));
    let mut s = with_name(condb_refund(&f, &oco), "NS11 conditional-buy refund co-spends a stray owned by it");
    s.inputs.push(owned_tok(&f, 3, e, Some(vec![tok_state(3 * WHOLE, &taker, SCHEME_P2PK)])));
    s.outputs.push(out(CARRIER, n.t.spk(3 * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);
    let mut s = with_name(condb_update(&f, &oco, &oco.armed(), &r, 0), "NS12 conditional-buy arm co-spends a stray owned by it");
    let st = vec![tok_state(3 * WHOLE, &taker, SCHEME_P2PK)];
    let l = push_tok(&f, &mut s, CARRIER, 3 * WHOLE, &e.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_500, st);
    s.outputs.push(out(CARRIER, n.t.spk(3 * WHOLE, &taker, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
    run_bad(n, &s, 0);

    // Sell-first if-done entry (cov 0xf1, 10-whole custody): dust stand-in and stray co-spend.
    let i = cov(0xf1);
    let mut s = with_name(b7(&f), "NS13 if-done ask fill co-spends a 3-whole stray owned by the entry");
    s.inputs.push(owned_tok(&f, 3, i, None));
    set_leader_next(&mut s, 1, vec![tok_state(6 * WHOLE, &i.as_bytes(), SCHEME_COVID), tok_state(7 * WHOLE, &taker, SCHEME_P2PK)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = n.t.spk(7 * WHOLE, &taker, SCHEME_P2PK);
    s.outputs.push(out(CARRIER, p2pk_spk(&taker), None));
    run_bad(n, &s, 0);
    let mut s = with_name(b7(&f), "NS14 if-done ask fill co-spends a second 10-whole UTXO owned by the entry (a top-up)");
    s.inputs.push(owned_tok(&f, 10, i, None));
    set_leader_next(&mut s, 1, vec![tok_state(6 * WHOLE, &i.as_bytes(), SCHEME_COVID), tok_state(14 * WHOLE, &taker, SCHEME_P2PK)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = n.t.spk(14 * WHOLE, &taker, SCHEME_P2PK);
    s.outputs.push(out(CARRIER, p2pk_spk(&taker), None));
    run_bad(n, &s, 0);
}

#[test]
fn v2_buy_lifecycle_dust_custody() {
    let f = fx();
    let n = &f.net;
    // The entry holds 10 qty (amountLeft 10); the attacker presents a 1-whole dust UTXO owned by the
    // entry as its custody and sells it out, terminating the entry and orphaning the 10 qty.
    let real = ifda(n, &IfdAP { qty: 10, ..ifda_p(&f) });
    let mut s = with_name(
        ifda_fill_x(&f, &ifda_p(&f), 1, 1, IFDA_VALUE, P250, None, 0, 0),
        "NS15 if-done ask: 1-whole dust sold out to terminate a 10-whole entry (orphaning its custody)",
    );
    if let Role::Call { art, .. } = &mut s.inputs[0].role {
        *art = real.clone();
    }
    s.inputs[0].entry = utxo(IFDA_VALUE, spk_of(&real), cov(0xf1), 1_000);
    run_bad(n, &s, 0);
}

// ---------------------------------------------------------------- touch trigger battery (v2.6)

/// minRestDaa of every order in the fixtures.
const R: u64 = 600;

/// A trigger of this family's buy-side templates: the buy-stop leg of a KobCondBid armed inside its fill
/// (settle) or by update, its trailing ratchet (update, down), and the sell-stop entry of a KobIfdAsk armed
/// inside its fill or by update.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Settle,
    Arm,
    Trail,
    IfdFill,
    IfdArm,
}
impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Settle => "TB",
            Kind::Arm => "TV",
            Kind::Trail => "TW",
            Kind::IfdFill => "TK",
            Kind::IfdArm => "TL",
        }
    }
    fn is_update(self) -> bool {
        matches!(self, Kind::Arm | Kind::Trail | Kind::IfdArm)
    }
    /// Default evidence: a bid at 3.02 for the 3.00 buy stop, an ask at 2.80 for the trail (2 steps down), an
    /// ask at 2.55 for the 2.60 sell-stop entry.
    fn ev(self) -> Ev {
        match self {
            Kind::Settle | Kind::Arm => Ev::bid(302_000_000),
            Kind::Trail => Ev::ask(280_000_000),
            Kind::IfdFill | Kind::IfdArm => Ev::ask(255_000_000),
        }
    }
    /// The evidence quote exactly at the trigger boundary, and one sompi on the wrong side of it.
    fn edge(self) -> (i64, i64) {
        match self {
            Kind::Settle | Kind::Arm => (300_000_000, 299_999_999),
            Kind::Trail => (285_000_000, 285_000_001),
            Kind::IfdFill | Kind::IfdArm => (260_000_000, 260_000_001),
        }
    }
    /// Trail only: evidence far on the wrong side of the boundary (an ask at 3.50: k = -12 would move the buy stop up to 3.60).
    fn far(self) -> i64 {
        350_000_000
    }
    /// Argument positions of ev and tk in the order's sigscript.
    fn ev_pos(self) -> (usize, Option<usize>) {
        match self {
            Kind::Settle => (3, None),
            Kind::Arm | Kind::Trail | Kind::IfdArm => (0, Some(1)),
            Kind::IfdFill => (6, Some(7)),
        }
    }
}
/// The sell-stop entry of the IfdAsk triggers: stop 2.60, limit 2.50, 10 qty.
fn stop_ifda(f: &Fx, min_touch: i64) -> IfdAP {
    IfdAP { entry_stop: 260_000_000, min_touch, ..ifda_p(f) }
}
/// The stop order of `kind` (input 0) triggered by the evidence `ev`; `min_units`: its minTouchUnits.
fn trig(f: &Fx, kind: Kind, ev: &Ev, min_units: i64) -> Scn {
    let m = pk(&f.maker_b);
    match kind {
        Kind::Settle => {
            let cp = CondBP { min_units, ..CondBP::oco(m) };
            condb_fill(f, &cp, 4, 1, &cp.with_qty(6).armed(), Some(ev))
        }
        Kind::Arm => {
            let cp = CondBP { min_units, ..CondBP::oco(m) };
            condb_update(f, &cp, &cp.armed(), ev, 0)
        }
        Kind::Trail => {
            let tr = CondBP { min_units, ..trailing_b(m) };
            let k = ((tr.stop - tr.gap - ev.price) / tr.step).min((tr.stop - tr.tp - 1) / tr.step);
            condb_update(f, &tr, &CondBP { stop: tr.stop - k * tr.step, ..tr.clone() }, ev, tr.wait as u64)
        }
        Kind::IfdFill => ifda_fill_x(f, &stop_ifda(f, min_units), 10, 4, IFDA_VALUE, P250, Some(ev), 0, 1),
        Kind::IfdArm => {
            let ip = stop_ifda(f, min_units);
            ifda_update(f, &ip, &IfdAP { armed: 1, ..ip.clone() }, ev, 0, false)
        }
    }
}
/// Appends a second order (cov 0xe2 / 0xf2, input returned) that arms or trails by update next to the evidence
/// at (ev, tk), with its continuation output: the shared-evidence scenarios.
fn add_second_update(f: &Fx, s: &mut Scn, kind: Kind, ev: i64, tk: i64) -> usize {
    let m = pk(&f.maker_a);
    let at = s.inputs.len();
    match kind {
        Kind::Settle | Kind::Arm => {
            let cp = CondBP::oco(m);
            let v = condb_value(&cp);
            s.inputs.push(call(&condb(&f.net, &cp), "update", vec![iv(ev), iv(tk)], "condb2.update", v, cov(0xe2), 2_000));
            s.outputs.push(out(v, spk_of(&condb(&f.net, &cp.armed())), Some((at as u16, cov(0xe2)))));
        }
        Kind::Trail => {
            let tr = trailing_b(m);
            let v = condb_value(&tr);
            let mut i = call(&condb(&f.net, &tr), "update", vec![iv(ev), iv(tk)], "condb2.update", v, cov(0xe2), 2_000);
            i.seq = tr.wait as u64;
            s.inputs.push(i);
            let next = CondBP { stop: tr.stop - 2 * tr.step, ..tr.clone() };
            s.outputs.push(out(v, spk_of(&condb(&f.net, &next)), Some((at as u16, cov(0xe2)))));
        }
        Kind::IfdFill | Kind::IfdArm => {
            let ip = IfdAP { maker: m, ..stop_ifda(f, 1) };
            s.inputs.push(call(&ifda(&f.net, &ip), "update", vec![iv(ev), iv(tk)], "ifda2.update", IFDA_VALUE, cov(0xf2), 1_000));
            s.outputs.push(out(IFDA_VALUE, spk_of(&ifda(&f.net, &IfdAP { armed: 1, ..ip.clone() })), Some((at as u16, cov(0xf2)))));
        }
    }
    at
}
/// The order's own counterparty named as its evidence (fill kinds): the resting order it trades with.
fn counterparty(f: &Fx, kind: Kind) -> Option<(Scn, &'static str)> {
    match kind {
        Kind::Settle => Some((condb_from_ask(f), "the buy stop's own counterparty (the resting ask it buys from) as its evidence")),
        Kind::IfdFill => {
            Some((ifda_into_bid(f), "the sell-stop entry's own counterparty (the resting bid it sells into) as its evidence"))
        }
        _ => None,
    }
}
/// A fill (input 0) of a template that is never evidence: a sell-first if-done entry fill (ask side, its
/// custody at input 1), a conditional buy limit-leg fill (bid side).
fn other_fill(f: &Fx, ask: bool) -> (Scn, &'static str) {
    if ask {
        (b7(f), "a KobIfdAsk entry fill (its custody as tk) as evidence")
    } else {
        let cp = CondBP::oco(pk(&f.maker_b));
        (condb_fill(f, &cp, 4, 0, &cp.with_qty(6), None), "a KobCondBid limit-leg fill as evidence")
    }
}
/// The covenant id of the order input 0 of a trigger scenario.
fn own_cov(kind: Kind) -> Hash {
    if kind == Kind::IfdArm || kind == Kind::IfdFill {
        cov(0xf1)
    } else {
        cov(0xe1)
    }
}
/// The unarmed buy stop (OCO, stop 3.00) buys 4 qty from a resting ask quoting 3.00 (its counterparty:
/// the ask's custody is the token input, the ask itself a later input) and names that ask as its evidence.
fn condb_from_ask(f: &Fx) -> Scn {
    let a = cov(0x9a);
    let ap = AskP { qty: 4, ..AskP::new(pk(&f.maker_c), 300_000_000) };
    let cp = CondBP::oco(pk(&f.maker_b));
    let mut s = condb_fill(f, &cp, 4, 1, &cp.with_qty(6).armed(), None);
    // the order's tokens come from the ask's custody instead of the taker
    let next = match &s.inputs[1].role {
        Role::TokLeader { next, .. } => next.clone(),
        _ => panic!("leader"),
    };
    s.inputs[1] = tok_in(&f.net.t, CARRIER, 4 * WHOLE, &a.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000);
    let pay = ask_all_in(4, ap.price, TIP);
    s.inputs.push(p2pk_in(&f.taker, pay + CARRIER));
    while s.inputs.len() < s.outputs.len() {
        s.inputs.push(p2pk_in(&f.taker, 0));
    }
    let at = s.inputs.len();
    s.inputs.push(call(&f.net.ask(&ap), "settle", vec![nb(4 * WHOLE), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000));
    s.outputs.push(out(pay + 2 * CARRIER, p2pk_spk(&ap.maker), None));
    set_arg(&mut s, 0, 3, iv(at as i64));
    s
}
/// The unarmed sell-stop entry (stop 2.60, limit 2.50) sells 4 qty straight into a resting bid quoting 2.55
/// (its counterparty, input 3, whose positional delivery is output 3) and names that bid as its evidence.
fn ifda_into_bid(f: &Fx) -> Scn {
    let t = &f.net.t;
    let bp = BidP::new(pk(&f.maker_c), 255_000_000);
    let v = 4 * bp.token_max() + DC;
    let mut s = ifda_fill_x(f, &stop_ifda(f, 1), 10, 4, IFDA_VALUE, P250, None, 0, 1);
    // outputs: [0] exit, [1] entry continuation, [2] token remainder, [3] the buyer's tokens, [4] taker KAS
    s.inputs.push(call(&f.net.bid(&bp), "fill", vec![nb(4 * WHOLE), iv(1), iv(0)], "bid.fill", v, cov(0x9b), 1_000));
    s.outputs[3] = out(v - bid_all_in(4, bp.price, TIP), t.spk(4 * WHOLE, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV)));
    if let Role::TokLeader { next, .. } = &mut s.inputs[1].role {
        next[1] = tok_state(4 * WHOLE, &bp.maker, SCHEME_P2PK);
    }
    balance(&mut s, 4);
    set_arg(&mut s, 0, 6, iv(3));
    set_arg(&mut s, 0, 7, iv(1));
    s
}
/// The evidence input (ev) and custody (tk) a scenario of `kind` passes to its order.
fn ev_args(s: &Scn, kind: Kind) -> (i64, i64) {
    let (pe, pt) = kind.ev_pos();
    let get = |p: usize| match &s.inputs[0].role {
        Role::Call { args, .. } => match &args[p] {
            Arg::V(ArtifactValue::Int(i)) => *i,
            _ => panic!("not an int argument"),
        },
        _ => panic!("input 0 is not the order"),
    };
    (get(pe), pt.map(get).unwrap_or(-1))
}

/// Every evidence rule of one trigger: exposure (UTXO, custody, activeFrom, TWAP/DCA interval; exactly R
/// accepted, one DAA short rejected), slope, side, token, unit, size threshold, an evidence that does not
/// fill, wrong ev / tk indices, look-alike templates, the price rule at its edge, one evidence shared by
/// two orders, and the update-only rules (own tokens, other fill kinds). Input 0 is the order throughout.
fn touch_battery(f: &Fx, kind: Kind) {
    let n = &f.net;
    let tag = kind.tag();
    let good = kind.ev();
    let matcher = pk(&f.matcher);
    let (pe, pt) = kind.ev_pos();
    let name = |neg: bool, id: &str, what: &str| format!("{}{tag}{id} {what}", if neg { "N" } else { "" });
    let ok = |e: &Ev, mu: i64, id: &str, what: &str| {
        run_ok(n, &with_name(trig(f, kind, e, mu), &name(false, id, what)));
    };
    let bad = |e: &Ev, mu: i64, id: &str, what: &str| run_bad(n, &with_name(trig(f, kind, e, mu), &name(true, id, what)), 0);
    let bad_s = |s: Scn, id: &str, what: &str, at: usize| run_bad(n, &with_name(s, &name(true, id, what)), at);

    ok(&good, 1, "00", "triggered by a plain fill in the same transaction");
    // exposure = max(UTXO DAA + interval, activeFrom, custody DAA) + R <= lock time
    ok(&good.with(|e| e.daa = NOW - R), 1, "01", "evidence UTXO exposed exactly R = minRestDaa DAA");
    bad(&good.with(|e| e.daa = NOW - R + 1), 1, "01", "evidence UTXO exposed R - 1 DAA (too young)");
    if let (true, Some(pt)) = (good.ask, pt) {
        ok(&good.with(|e| e.tok_daa = NOW - R), 1, "02", "evidence ask's custody moved exactly R DAA before");
        bad(&good.with(|e| e.tok_daa = NOW - R + 1), 1, "02", "evidence ask's custody moved R - 1 DAA before (a fresh top-up)");
        // the fresh custody hidden behind an old token UTXO of ANOTHER token owned by the evidence ask
        let mut s = trig(f, kind, &good.with(|e| e.tok_daa = NOW - R + 1), 1);
        let at = s.inputs.len() as i64;
        let st = vec![tok_state(5 * WHOLE, &matcher, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 5 * WHOLE, &good.cov.as_bytes(), SCHEME_COVID, OTHER_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(5 * WHOLE, &matcher, SCHEME_P2PK), Some((l as u16, OTHER_COV))));
        set_arg(&mut s, 0, pt, iv(at));
        bad_s(s, "02b", "fresh custody; tk names an old UTXO of another token owned by the evidence ask", 0);
    }
    ok(&good.with(|e| e.active_from = (NOW - R) as i64), 1, "03", "evidence active exactly R DAA (activeFrom)");
    bad(&good.with(|e| e.active_from = (NOW - R) as i64 + 1), 1, "03", "evidence active only R - 1 DAA (activeFrom)");
    ok(
        &good.with(|e| {
            e.interval = 100;
            e.daa = NOW - R - 100;
        }),
        1,
        "04",
        "TWAP/DCA evidence: its slice opened (UTXO DAA + interval) exactly R DAA before",
    );
    bad(
        &good.with(|e| {
            e.interval = 100;
            e.daa = NOW - R - 99;
        }),
        1,
        "04",
        "TWAP/DCA evidence: its slice opened R - 1 DAA before (the interval counts)",
    );
    bad(&good.with(|e| e.slope = 1), 1, "05", "decaying evidence (Dutch ask / rising bid): a moving price is not a quote");
    bad(&good.with(|e| e.ask = !e.ask), 1, "06", "evidence of the wrong side (a bid for a sell stop, an ask for a buy stop)");
    bad(&good.with(|e| e.token = OTHER_COV), 1, "07", "evidence of another token");
    if let (true, Some(pt)) = (good.ask, pt) {
        // an ask of another token whose covenant also owns a UTXO of this token, named as its custody
        let mut s = trig(f, kind, &good.with(|e| e.token = OTHER_COV), 1);
        let at = s.inputs.len() as i64;
        let st = vec![tok_state(5 * WHOLE, &matcher, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 5 * WHOLE, &good.cov.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(5 * WHOLE, &matcher, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        set_arg(&mut s, 0, pt, iv(at));
        bad_s(s, "07b", "evidence of another token; tk names a UTXO of this token owned by that ask", 0);
    }
    bad(&good.with(|e| e.unit = 2 * UNIT), 1, "08", "evidence quoting another unit");
    ok(&good, 5, "09", "evidence of exactly minTouchUnits (5 qty x 1 unit, threshold 5)");
    bad(&good.with(|e| e.qty = 4), 5, "09", "evidence below minTouchUnits (4 units, threshold 5)");
    bad(&good.with(|e| e.mode = EvMode::Cancel), 1, "10", "cancelled, not filled (a signature push, ground to read as n > 0)");
    bad(&good.with(|e| e.mode = EvMode::Refund), 0, "11", "refunded, not filled (ask settle n = 0 / bid refund; minTouchUnits 0)");
    if !good.ask {
        bad(&good.with(|e| e.mode = EvMode::ZeroFill), 0, "11b", "a bid fill with n = 0 (the bid refuses it too; minTouchUnits 0)");
    }
    // wrong indices
    let base = trig(f, kind, &good, 1);
    let (ei, tk) = ev_args(&base, kind);
    let mut s = base.clone();
    set_arg(&mut s, 0, pe, iv(0));
    bad_s(s, "12", "ev points at the order itself", 0);
    let tok = base.inputs.iter().position(|i| i.entry.covenant_id == Some(TOKEN_COV)).unwrap();
    let mut s = base.clone();
    set_arg(&mut s, 0, pe, iv(tok as i64));
    bad_s(s, "13", "ev points at a token input", 0);
    let p2 = base.inputs.iter().position(|i| matches!(i.role, Role::P2pk { .. })).unwrap();
    let mut s = base.clone();
    set_arg(&mut s, 0, pe, iv(p2 as i64));
    bad_s(s, "14", "ev points at a P2PK input", 0);
    if let (true, Some(pt)) = (good.ask, pt) {
        // the evidence ask's custody is fresh (moved R - 1 DAA before); tk names an old token input instead
        let fresh = trig(f, kind, &good.with(|e| e.tok_daa = NOW - R + 1), 1);
        // tk: a token input the evidence ask does not own (another covenant's custody / a key's tokens)
        let mut s = fresh.clone();
        let other = s
            .inputs
            .iter()
            .enumerate()
            .find(|(i, x)| x.entry.covenant_id == Some(TOKEN_COV) && *i as i64 != tk)
            .map(|(i, _)| i as i64);
        let other = match other {
            Some(o) => o,
            // no other token input: the matcher also sells its own tokens into a bid
            None => add_ev(f, &mut s, &Ev::bid(P250)).0 + 1,
        };
        set_arg(&mut s, 0, pt, iv(other));
        bad_s(s, "15", "fresh custody; tk names an old token input the evidence ask does not own", 0);
        // tk: the custody of another ask filled in the same transaction
        let mut s = fresh.clone();
        let (_, tk2) = add_ev(f, &mut s, &good.with(|e| e.cov = cov(0x9c)));
        set_arg(&mut s, 0, pt, iv(tk2));
        bad_s(s, "16", "fresh custody; tk names the old custody of another ask filled in the same transaction", 0);
    }
    bad(&good.with(|e| e.mode = EvMode::Forged), 1, "17", "look-alike: a real order's state under another template (P2SH of it)");
    bad(&good.with(|e| e.mode = EvMode::NotP2sh), 1, "18", "the real redeem script pushed by an input that is not its P2SH");
    let (edge, beyond) = kind.edge();
    ok(&good.with(|e| e.price = edge), 1, "19", "evidence quoting exactly at the trigger boundary");
    bad(&good.with(|e| e.price = beyond), 1, "19", "evidence quoting one sompi beyond the trigger boundary");
    if kind == Kind::Trail {
        bad(
            &good.with(|e| e.price = kind.far()),
            1,
            "19b",
            "trail evidence far beyond the boundary (k < 0: the stop would move back)",
        );
    }
    // one evidence, two orders
    let mut s = base.clone();
    add_second_update(f, &mut s, kind, ei, tk);
    run_ok(n, &with_name(s, &name(false, "20", "two orders arm (trail) from ONE evidence fill in the same transaction")));
    // the order's own counterparty is on the other side of the book: never evidence
    if let Some((s, what)) = counterparty(f, kind) {
        bad_s(s, "21", what, 0);
    }
    if kind.is_update() {
        // other fills (conditional, if-done) are never evidence: input 0 fills, a second order reads it
        let (mut s, what) = other_fill(f, good.ask);
        let at = add_second_update(f, &mut s, kind, 0, if good.ask { 1 } else { -1 });
        bad_s(s, "22", what, at);
        // update spends no token input of its own covenant id; foreign token inputs are fine
        let own = own_cov(kind);
        let thief = pk(&f.taker);
        let mut s = base.clone();
        let st = vec![tok_state(3 * WHOLE, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3 * WHOLE, &own.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 2_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(3 * WHOLE, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        bad_s(s, "23", "update next to the evidence also spends a token UTXO owned by the order (to the attacker)", 0);
        let mut s = base.clone();
        let st = vec![tok_state(3 * WHOLE, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3 * WHOLE, &thief, SCHEME_P2PK, TOKEN_COV, Wit::P2pk(f.taker), 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(3 * WHOLE, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        // the taker's own P2PK input (KRON id_type 3 authorises by its presence)
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(out(KAS, p2pk_spk(&thief), None));
        run_ok(
            n,
            &with_name(s, &name(false, "23", "update next to the evidence and another foreign token input (the taker's own tokens)")),
        );
    }
    if kind == Kind::IfdArm {
        // update arms only an entry that has qty to fill and its stop on the limit's side (else the arm only takes keeperTip)
        let ip = stop_ifda(f, 1);
        let empty = IfdAP { qty: 0, rpt: 9, ..ip.clone() };
        let s = ifda_update(f, &empty, &IfdAP { armed: 1, ..empty.clone() }, &good, 0, false);
        bad_s(s, "24", "update arms a repeating entry with no qty left (they are all in its exits)", 0);
        let inv = IfdAP { price: ip.entry_stop + 1, ..ip.clone() };
        let s = ifda_update(f, &inv, &IfdAP { armed: 1, ..inv.clone() }, &good, 0, false);
        bad_s(s, "25", "update arms an entry whose stop is below its limit (no fill can follow)", 0);
    }
}

/// The touch batteries run on the 8/8 token (evidence fills add token inputs and outputs next to the
/// order's own).
fn fx8() -> Fx {
    Fx {
        net: Net::with_token(Token::named("KCC20Ref_8x8")),
        maker_a: keypair(),
        maker_b: keypair(),
        maker_c: keypair(),
        taker: keypair(),
        matcher: keypair(),
    }
}

/// Touch trigger (v2.6), KobCondBid buy-stop leg armed inside its own fill (settle, leg 1, bid evidence).
#[test]
fn v2_buy_touch_cond_settle() {
    touch_battery(&fx8(), Kind::Settle);
}
/// Touch trigger (v2.6), KobCondBid buy-stop leg armed by update next to a bid fill.
#[test]
fn v2_buy_touch_cond_arm() {
    touch_battery(&fx8(), Kind::Arm);
}
/// Touch trigger (v2.6), KobCondBid trailing ratchet down (update next to an ask fill).
#[test]
fn v2_buy_touch_cond_trail() {
    touch_battery(&fx8(), Kind::Trail);
}
/// Touch trigger (v2.6), KobIfdAsk sell-stop entry armed inside its own fill (ask evidence).
#[test]
fn v2_buy_touch_ifd_fill() {
    touch_battery(&fx8(), Kind::IfdFill);
}
/// Touch trigger (v2.6), KobIfdAsk sell-stop entry armed by update next to an ask fill.
#[test]
fn v2_buy_touch_ifd_arm() {
    touch_battery(&fx8(), Kind::IfdArm);
}

/// Merge-value safety of update (protocol v3), buy family. Both merges require the OTHER input's sigscript to
/// START with 0x08 (the 8-byte push of the fill / merge argument), so neither can read an update as a merge: an
/// update's first push is its `ev` input index, which for every buildable index (0..= a few thousand) is a 1-,
/// 2-, 3- or 4-byte canonical push, never the 8-byte push that begins 0x08. Checked on the real encoder across
/// the small indices and the push-size boundaries; the control shows that only an 8-byte-pushed first argument
/// (a value >= 2^31, which no input index ever is) would begin with 0x08 and be read at all.
#[test]
fn v2_buy_update_sigscript_is_never_a_merge_value() {
    let f = fx();
    let n = &f.net;
    let condb_art = condb(n, &CondBP::oco(pk(&f.maker_b)));
    let ifda_art = ifda(n, &stop_ifda(&f, 1));
    let head = |art: &SilAbiArtifact, args: &[ArtifactValue]| encode_entry_sig_script(art, "update", args).expect("encode update");
    // decode bytes[1..9) as a sign-magnitude script number, as a merge would if the first byte were 0x08
    let num = |ss: &[u8]| -> i64 {
        let mut b: [u8; 8] = ss[1..9].try_into().unwrap();
        let neg = b[7] & 0x80 != 0;
        b[7] &= 0x7f;
        let v = i64::from_le_bytes(b);
        if neg {
            -v
        } else {
            v
        }
    };
    let idx: Vec<i64> = (-1..=260).chain([1_000, 32_767, 32_768, 65_535, 65_536, 8_388_607, 2_147_483_646]).collect();
    let mut checked = 0;
    for &ev in idx.iter().filter(|&&i| i >= 0) {
        for &tk in &idx {
            // the exit's merge (KobIfdAsk) reads the entry's update; the entry's merge (KobCondBid) reads the
            // exit's update; neither can, because the first byte is not the 8-byte push opcode 0x08
            assert_ne!(head(&ifda_art, &[int(ev), int(tk)])[0], 0x08, "KobIfdAsk.update({ev}, {tk}) starts with 0x08");
            assert_ne!(head(&condb_art, &[int(ev), int(tk)])[0], 0x08, "KobCondBid.update({ev}, {tk}) starts with 0x08");
            checked += 2;
        }
    }
    // control: a first argument whose minimal encoding needs 8 bytes (>= 2^56; never an input index) IS pushed
    // with 0x08, and then its bytes[1..9) read as a plain script number (positive here: not even a merge value)
    let ss = head(&ifda_art, &[int(1i64 << 56), int(0)]);
    assert_eq!(ss[0], 0x08, "control: an 8-byte first argument begins with the 0x08 push");
    assert_eq!(num(&ss), 1i64 << 56, "control: bytes[1..9) read as the pushed number");
    println!("MERGE-VALUE {checked} update sigscripts checked, none begins with the 0x08 merge push (control needs a >= 2^31 first argument)");
}

// ---------------------------------------------------------------- v2.4 repeat IFD, sell-first

/// Covenant ids of the repeating if-done ask (as in ifda_fill) and of its exits.
const RPTA_ENTRY: u8 = 0xf1;
const RPTA_EXIT: u8 = 0xe1;
/// Proceeds rate (sompi per whole token) the maker is owed by a sell-first cycle (the entry's all-in at its limit).
fn sell_proceeds(ip: &IfdAP) -> i64 {
    ip.price - ip.tip
}
fn rpta_p(f: &Fx, qty: i64, rpt: i64) -> IfdAP {
    IfdAP { qty, rpt, ..ifda_p(f) }
}
/// The exit a fill of n qty books from the entry at UTXO DAA 1000, cycle time t.
fn booked_exit_b(ip: &IfdAP, n: i64, t: i64) -> CondBP {
    CondBP {
        qty: n,
        parent: cov(RPTA_ENTRY).as_bytes(),
        rpt_price: sell_proceeds(ip),
        rpt_pre: ip.prefund,
        rpt_until: EXPIRY.min(t.max(1_000) + MAX_IDLE),
        ..ip.exit.clone()
    }
}
/// Merge argument -(k * 2^53 + m) as an 8-byte script number (sign-magnitude, little endian); m in BASE units.
fn nb_merge(k: i64, m_base: i64) -> Arg {
    let mut b = (k * MERGE_K + m_base).to_le_bytes();
    b[7] |= 0x80;
    Arg::V(bytes(&b))
}
/// The merge argument constant (2^53): the base-unit ceiling of a booked exit's amount.
const MERGE_K: i64 = 9_007_199_254_740_992;
/// Sets output `idx` to whatever balances the transaction after NET_FEE.
fn balance(s: &mut Scn, idx: usize) {
    let ins: i64 = s.inputs.iter().map(|i| i.entry.amount as i64).sum();
    let outs: i64 = s.outputs.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, o)| o.value as i64).sum();
    s.outputs[idx].value = (ins - outs - NET_FEE) as u64;
}

/// Knobs of a repeating sell-first entry fill.
#[derive(Clone)]
struct RaKnobs {
    have: i64,
    n: i64,
    value: i64,
    rpt: i64,
    t: i64,
    exit_rpt: Option<([u8; 32], i64, i64, i64)>,
    next_rpt: Option<i64>,
    terminate: bool,
    cont_delta: i64,
}
impl RaKnobs {
    fn new(have: i64, n: i64, rpt: i64) -> Self {
        RaKnobs {
            have,
            n,
            value: CARRIER + have * IFDA_PREFUND + have * IFDA_EXIT_CARRIER,
            rpt,
            t: 0,
            exit_rpt: None,
            next_rpt: None,
            terminate: false,
            cont_delta: 0,
        }
    }
}
/// A repeating if-done ask (cov 0xf1, `have` qty in custody) sells n qty at its limit to a
/// taker. Outputs: [0] the fresh KobCondBid exit, [1] the entry continuation (qty left or
/// repeating), [2] the token remainder (qty left), the taker's tokens, the taker's change.
fn rpta_fill(f: &Fx, k: &RaKnobs) -> Scn {
    let t = &f.net.t;
    let i = cov(RPTA_ENTRY);
    let ip = rpta_p(f, k.have, k.rpt);
    let booked = k.rpt > k.n;
    let mut xp = if booked { booked_exit_b(&ip, k.n, k.t) } else { ip.exit.with_qty(k.n) };
    if let Some((p, l, pr, u)) = k.exit_rpt {
        xp.parent = p;
        xp.rpt_price = l;
        xp.rpt_pre = pr;
        xp.rpt_until = u;
    }
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let proceeds = ask_all_in(k.n, ip.price, ip.tip);
    let rest = k.have - k.n;
    let cont_exists = (rest > 0 || k.rpt > 0) && !k.terminate;
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest * WHOLE, &i.as_bytes(), SCHEME_COVID));
    }
    next.push(tok_state(k.n * WHOLE, &taker, SCHEME_P2PK));
    let mut outputs = vec![];
    if cont_exists {
        outputs.push(out(proceeds + k.n * ip.prefund + ip.exit_carrier, spk_of(&condb(&f.net, &xp)), None));
        let next_rpt = k.next_rpt.unwrap_or(if booked { k.rpt - k.n } else { k.rpt });
        let cont = ifda(&f.net, &IfdAP { qty: rest, rpt: next_rpt, ..ip.clone() });
        let absorbed = if rest == 0 { CARRIER } else { 0 };
        outputs.push(out(k.value - k.n * ip.prefund - ip.exit_carrier + absorbed + k.cont_delta, spk_of(&cont), Some((0, i))));
        if rest > 0 {
            outputs.push(out(CARRIER, t.spk(rest * WHOLE, &i.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))));
        }
    } else {
        outputs.push(out(proceeds + k.value + CARRIER, spk_of(&condb(&f.net, &xp)), None));
    }
    outputs.push(out(CARRIER, t.spk(k.n * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    outputs.push(out(0, p2pk_spk(&taker), None));
    let mut s = Scn {
        name: "rpta fill".into(),
        inputs: vec![
            call(
                &ifda(&f.net, &ip),
                "settle",
                vec![
                    nb(k.n * WHOLE),
                    iv(1),
                    iv(if rest > 0 { 2 } else { 0 }),
                    iv(0),
                    Arg::V(bytes(&tpl.pre)),
                    Arg::V(bytes(&tpl.suf)),
                    iv(0),
                    iv(0),
                    iv(k.t),
                ],
                "ifda.settle",
                k.value,
                i,
                1_000,
            ),
            tok_in(t, CARRIER, k.have * WHOLE, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
    };
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    regen(&mut s, 0, 0);
    s
}

/// Knobs of a take-profit buy-back by a booked sell-first exit that re-arms its entry.
#[derive(Clone)]
struct MergeBKnobs {
    entry: IfdAP,
    entry_value: i64,
    exit: Option<CondBP>,
    exit_qty: i64,
    n: i64,
    leg: i64,
    with_entry: bool,
    claim_k: i64,
    claim_m: Option<i64>,
    cont: Option<IfdAP>,
    cont_delta: i64,
    maker_delta: i64,
    exit_delta: i64,
    custody_delta: i64,
    custody_qty: Option<i64>,
    custody_owner: Option<[u8; 32]>,
    lock_time: u64,
}
impl MergeBKnobs {
    fn new(f: &Fx, entry_qty: i64, exit_qty: i64, n: i64) -> Self {
        MergeBKnobs {
            entry: rpta_p(f, entry_qty, 21),
            entry_value: CARRIER + entry_qty * (IFDA_PREFUND + IFDA_EXIT_CARRIER),
            exit: None,
            exit_qty,
            n,
            leg: 0,
            with_entry: true,
            claim_k: 0,
            claim_m: None,
            cont: None,
            cont_delta: 0,
            maker_delta: 0,
            exit_delta: 0,
            custody_delta: 0,
            custody_qty: None,
            custody_owner: None,
            lock_time: NOW,
        }
    }
}
/// A booked exit (KobCondBid, cov 0xe1, UTXO DAA 2000, funded at the entry's limit) buys back n
/// qty from a seller on `leg`; the entry (KobIfdAsk, cov 0xf1) takes them into its custody.
/// Inputs: [0] exit, [1] the seller's tokens (KCC-20 leader), [2] entry (merge), [3] the entry's
/// custody (when it has qty). Outputs: [0] maker profit (re-arm) or the delivery (plain), [1]
/// exit continuation (qty left), then the entry's custody, the entry continuation and the
/// seller's KAS.
fn rpta_merge(f: &Fx, k: &MergeBKnobs) -> Scn {
    let tk = &f.net.t;
    let m = pk(&f.maker_a);
    let c = cov(RPTA_EXIT);
    let i = cov(RPTA_ENTRY);
    let taker = pk(&f.taker);
    let ip = &k.entry;
    let xp = k.exit.clone().unwrap_or_else(|| booked_exit_b(ip, k.exit_qty, 0));
    let xp = CondBP { qty: k.exit_qty, ..xp };
    let x_value = k.exit_qty * (sell_proceeds(ip) + ip.prefund) + ip.exit_carrier;
    let spend = bid_all_in(k.n, xp.leg_price(k.leg), xp.tip);
    let rest = k.exit_qty - k.n;
    let rearm = k.with_entry && k.leg == 0 && xp.parent != [0; 32];
    let claim = k.claim_m.unwrap_or(k.n);
    let seller_qty = if rearm { k.n } else { k.n + if k.with_entry { claim } else { 0 } };
    let custody_qty = k.custody_qty.unwrap_or(ip.qty + claim);
    let custody_owner = k.custody_owner.unwrap_or(i.as_bytes());
    let custody_scheme = if k.custody_owner.is_some() { SCHEME_P2PK } else { SCHEME_COVID };
    let mut next = vec![];
    if !rearm {
        next.push(tok_state(k.n * WHOLE, &m, SCHEME_P2PK));
    }
    if k.with_entry {
        next.push(tok_state(custody_qty * WHOLE, &custody_owner, custody_scheme));
    }
    let mut s = Scn {
        name: "rpta merge".into(),
        inputs: vec![
            call(
                &condb(&f.net, &xp),
                "settle",
                vec![nb(k.n * WHOLE), iv(1), iv(k.leg), iv(0), iv(0)],
                "condb.settle",
                x_value,
                c,
                2_000,
            ),
            tok_in(tk, CARRIER, seller_qty * WHOLE, &taker, SCHEME_P2PK, TOKEN_COV, Some(next), Wit::P2pk(f.taker), 1_500),
        ],
        outputs: vec![],
        lock_time: k.lock_time,
        payload: vec![],
    };
    if rearm {
        s.outputs.push(out(k.n * xp.rpt_price - spend + k.maker_delta, p2pk_spk(&m), None));
    } else {
        let v = if rest > 0 { DC } else { x_value - spend };
        s.outputs.push(out(v + k.maker_delta, tk.spk(k.n * WHOLE, &m, SCHEME_P2PK), Some((1, TOKEN_COV))));
    }
    if rest > 0 {
        let keep = if rearm { x_value - k.n * (xp.rpt_price + xp.rpt_pre) } else { x_value - spend - DC };
        s.outputs.push(out(keep + k.exit_delta, spk_of(&condb(&f.net, &CondBP { qty: rest, ..xp.clone() })), Some((0, c))));
    }
    if k.with_entry {
        let entry_in = s.inputs.len();
        let custody_in = entry_in + 1;
        let custody_out = s.outputs.len();
        s.inputs.push(call(
            &ifda(&f.net, ip),
            "settle",
            vec![
                nb_merge(k.claim_k, claim * WHOLE),
                iv(if ip.qty > 0 { custody_in as i64 } else { 1 }),
                iv(custody_out as i64),
                iv(0),
                Arg::V(bytes(&[])),
                Arg::V(bytes(&[])),
                iv(0),
                iv(0),
                iv(0),
            ],
            "ifda.merge",
            k.entry_value,
            i,
            1_000,
        ));
        if ip.qty > 0 {
            s.inputs.push(tok_in(tk, CARRIER, ip.qty * WHOLE, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000));
        }
        let new_custody = ip.qty == 0;
        let custody_value = if new_custody { ip.exit_carrier } else { CARRIER };
        s.outputs.push(out(
            custody_value + k.custody_delta,
            tk.spk(custody_qty * WHOLE, &custody_owner, custody_scheme),
            Some((1, TOKEN_COV)),
        ));
        // what the entry requires back: everything the exit held beyond the maker's proceeds when
        // its qty run out, else the prefund of the re-armed qty (funded by the filler if the exit
        // does not pay it)
        let back = if claim == k.exit_qty { x_value - claim * sell_proceeds(ip) } else { claim * ip.prefund };
        let back = back - if new_custody { ip.exit_carrier } else { 0 };
        // armed: reset with a new custody (no qty were left); an entry armed by update (1) with a band records its origin,
        // the entry UTXO's DAA (1_000), as a fill does
        let armed = if new_custody {
            0
        } else if ip.armed == 1 && ip.band_daa > 0 {
            1_000
        } else {
            ip.armed
        };
        let cont = k.cont.clone().unwrap_or(IfdAP { qty: ip.qty + claim, armed, ..ip.clone() });
        s.outputs.push(out(k.entry_value + back + k.cont_delta, spk_of(&ifda(&f.net, &cont)), Some((entry_in as u16, i))));
    }
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}
/// A crafted P2SH input imitating a booked KobCondBid exit of `parent` (see the sell-side twin).
fn fake_exit_b(f: &Fx, parent: Hash, m: i64, value: i64) -> Inp {
    let tpl = condb_tpl(&f.net);
    let size = tpl.pre.len() + 381 + tpl.suf.len();
    let data_len = size - 6;
    let mut data = vec![0u8; data_len];
    // state span = redeem[1..382); parent at state [322..354) = redeem [323..355) = data [320..352)
    // the would-be extensionCommitment at state [118..150) = data [116..148) (a new custody takes it)
    data[116..148].copy_from_slice(&EXT);
    data[320..352].copy_from_slice(&parent.as_bytes());
    let mut redeem = vec![0x4d, (data_len & 0xff) as u8, (data_len >> 8) as u8];
    redeem.extend(data);
    redeem.extend([0x75, 0x75, 0x51]);
    assert_eq!(redeem.len(), size);
    let mut ss = vec![0x08];
    ss.extend(m.to_le_bytes());
    ss.extend(push(&redeem));
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&redeem), 2_000, false, None),
        role: Role::RawSs { ss, name: "fake.exit" },
        seq: 0,
    }
}

/// Repeat IFD, sell-first (KobIfdAsk entry, KobCondBid exits): booking, the sold-out entry that
/// waits without custody, re-arming into an existing or a new custody, sell-out leftovers, the
/// rptUntil fallback, stop-loss exits, close, and a three-cycle ledger with exact accounting.
#[test]
fn v2_buy_repeat_ifd_positive() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);

    run_ok(
        n,
        &with_name(
            rpta_fill(&f, &RaKnobs::new(10, 4, 21)),
            "RPB1 repeating sell-first IFO sells 4/10: booked exit (rptPrice, rptPre), rptAmount 21 -> 17",
        ),
    );
    run_ok(
        n,
        &with_name(
            rpta_fill(&f, &RaKnobs::new(6, 6, 17)),
            "RPB2 repeating entry sold out 6/6: it stays with amountLeft 0 and no custody (keeps the custody carrier)",
        ),
    );
    run_ok(n, &with_name(rpta_fill(&f, &RaKnobs::new(10, 4, 4)), "RPB3 last cycle: rptAmount 4 cannot re-arm 4 qty, plain exit"));
    run_ok(n, &with_name(rpta_fill(&f, &RaKnobs { t: 7_000, ..RaKnobs::new(10, 4, 21) }), "RPB3b the cycle is dated by t (CLTV)"));
    run_ok(
        n,
        &with_name(
            rpta_merge(&f, &MergeBKnobs::new(&f, 6, 4, 3)),
            "RPB4 booked exit buys back 3/4 on its take-profit: 3 qty join the entry's custody (6 -> 9) with 3 prefunds, maker keeps the profit",
        ),
    );
    run_ok(
        n,
        &with_name(
            rpta_merge(&f, &MergeBKnobs::new(&f, 0, 4, 4)),
            "RPB5 booked exit buys back 4/4: a new 4-whole custody for the empty entry, the exit's leftovers come back",
        ),
    );
    let mut stop_ip = rpta_p(&f, 0, 21);
    stop_ip.entry_stop = 260_000_000;
    stop_ip.armed = 1;
    run_ok(
        n,
        &with_name(
            rpta_merge(&f, &MergeBKnobs { entry: stop_ip.clone(), ..MergeBKnobs::new(&f, 0, 4, 4) }),
            "RPB5b an armed sell-stop entry with no qty left re-arms unarmed",
        ),
    );
    let until = EXPIRY.min(1_000 + MAX_IDLE);
    run_ok(
        n,
        &with_name(
            rpta_merge(&f, &MergeBKnobs { with_entry: false, lock_time: until as u64, ..MergeBKnobs::new(&f, 0, 4, 4) }),
            "RPB6 from rptUntil a booked exit may take profit without its entry (tokens to the maker)",
        ),
    );
    let mut sl = booked_exit_b(&rpta_p(&f, 0, 21), 4, 0);
    sl.armed = 1;
    run_ok(
        n,
        &with_name(
            rpta_merge(&f, &MergeBKnobs { exit: Some(sl), leg: 1, with_entry: false, ..MergeBKnobs::new(&f, 0, 4, 4) }),
            "RPB7 stop-loss of a booked exit: plain delivery to the maker, no re-arm",
        ),
    );
    // close: an empty repeating entry is refunded at its soft expiry.
    let e = rpta_p(&f, 0, 1);
    let s = Scn {
        name: "RPB8 close: an empty repeating entry (no custody) is refunded at its soft expiry".into(),
        inputs: vec![call(&ifda(n, &e), "close", vec![], "ifda.close", 2 * CARRIER, cov(RPTA_ENTRY), (EXPIRY - 1_000) as u64)],
        outputs: vec![out(2 * CARRIER - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time: EXPIRY as u64,
        payload: vec![],
    };
    run_ok(n, &s);

    // Three cycles of a 4-whole position (rptAmount 9), fills and take-profits at the limits, values
    // carried forward: the entry and its custody are restored exactly after every cycle.
    let ip0 = rpta_p(&f, 4, 9);
    let v0 = CARRIER + 4 * (ip0.prefund + ip0.exit_carrier);
    let spend_token = bid_all_in(1, ip0.exit.tp, ip0.exit.tip);
    let profit_token = sell_proceeds(&ip0) - spend_token;
    let mut v = v0;
    let mut rpt = 9;
    let mut maker_total = 0;
    for cycle in 1..=3 {
        let booked = rpt > 4;
        let s = rpta_fill(&f, &RaKnobs { value: v, ..RaKnobs::new(4, 4, rpt) });
        run_ok(
            n,
            &with_name(
                s.clone(),
                &format!("RPB9 ledger cycle {cycle}: entry sells 4/4 (value {v}, rptAmount {rpt}, booked {booked})"),
            ),
        );
        v = s.outputs[1].value as i64;
        assert_eq!(v, v0 - 4 * ip0.prefund - ip0.exit_carrier + CARRIER, "entry after its fill keeps the custody carrier");
        let x_value = s.outputs[0].value as i64;
        assert_eq!(
            x_value,
            4 * (sell_proceeds(&ip0) + ip0.prefund) + ip0.exit_carrier,
            "exit funded with proceeds + prefund + carrier"
        );
        if booked {
            rpt -= 4;
            let mut mk = MergeBKnobs::new(&f, 0, 4, 4);
            mk.entry = rpta_p(&f, 0, rpt);
            mk.entry_value = v;
            let s = rpta_merge(&f, &mk);
            run_ok(n, &with_name(s.clone(), &format!("RPB9 ledger cycle {cycle}: take-profit buys back 4/4 and re-arms the entry")));
            let cont = s.outputs.iter().find(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPTA_ENTRY))).unwrap();
            let custody = s
                .outputs
                .iter()
                .find(|o| o.script_public_key == n.t.spk(4 * WHOLE, &cov(RPTA_ENTRY).as_bytes(), SCHEME_COVID))
                .unwrap();
            v = cont.value as i64;
            assert_eq!(v + custody.value as i64, v0 + CARRIER, "entry value + custody carrier restored after cycle {cycle}");
            assert_eq!(v, v0, "entry value restored exactly after cycle {cycle}");
            assert_eq!(s.outputs[0].value as i64, 4 * profit_token, "maker profit of cycle {cycle}");
            maker_total += s.outputs[0].value as i64;
        } else {
            let mut mk = MergeBKnobs::new(&f, 0, 4, 4);
            mk.exit = Some(ip0.exit.with_qty(4));
            mk.with_entry = false;
            let s = rpta_merge(&f, &mk);
            run_ok(n, &with_name(s.clone(), &format!("RPB9 ledger cycle {cycle}: plain take-profit, tokens back to the maker")));
            maker_total += s.outputs[0].value as i64;
        }
    }
    assert_eq!(
        maker_total,
        2 * 4 * profit_token + 4 * (sell_proceeds(&ip0) + ip0.prefund) + ip0.exit_carrier - 4 * spend_token,
        "maker receipts over 3 cycles"
    );
    println!("LEDGER-B v0={v0} profit/whole={profit_token} maker_total={maker_total}");
}

/// Single-lever attacks on repeat IFD, sell-first (see v2_repeat_ifd_attacks).
#[test]
fn v2_buy_repeat_ifd_attacks() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let i = cov(RPTA_ENTRY);
    let taker = pk(&f.taker);
    let tk = &n.t;
    let ip21 = rpta_p(&f, 10, 21);

    // ---- booking (entry fill)
    let bad = |k: RaKnobs, name: &str| run_bad(n, &with_name(rpta_fill(&f, &k), name), 0);
    let base = || RaKnobs::new(10, 4, 21);
    let until = EXPIRY.min(1_000 + MAX_IDLE);
    bad(RaKnobs { exit_rpt: Some(([0; 32], 0, 0, 0)), ..base() }, "NRPB1 booked exit written as a plain exit (re-arm skipped)");
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_proceeds(&ip21), ip21.prefund, until)), ..RaKnobs::new(10, 4, 4) },
        "NRPB2 exit booked although the re-arms are exhausted",
    );
    bad(RaKnobs { next_rpt: Some(21), ..base() }, "NRPB3 continuation keeps rptAmount (cycle count not decremented)");
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_proceeds(&ip21) + 1, ip21.prefund, until)), ..base() },
        "NRPB4 exit's rptPrice raised by 1 sompi (maker overpaid from the prefund)",
    );
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_proceeds(&ip21), ip21.prefund - 1, until)), ..base() },
        "NRPB4b exit's rptPre lowered by 1 sompi (prefund skimmed every cycle)",
    );
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_proceeds(&ip21), ip21.prefund, until - 1)), ..base() },
        "NRPB5 exit's rptUntil shortened",
    );
    bad(RaKnobs { t: NOW as i64 + 1, ..base() }, "NRPB6 cycle dated after the lockTime (t > tx DAA)");
    bad(
        RaKnobs { terminate: true, ..RaKnobs::new(6, 6, 17) },
        "NRPB7 repeating entry terminated when sold out (everything to the exit)",
    );
    bad(RaKnobs { cont_delta: -1, ..RaKnobs::new(6, 6, 17) }, "NRPB8 sold-out repeating entry: custody carrier skimmed by 1 sompi");

    // ---- take-profit / merge (exit side)
    let mb = |k: MergeBKnobs, name: &str, at: usize| run_bad(n, &with_name(rpta_merge(&f, &k), name), at);
    mb(
        MergeBKnobs { with_entry: false, ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB10 booked exit takes profit without re-arming its entry (before rptUntil)",
        0,
    );
    mb(
        MergeBKnobs { maker_delta: -1, ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB11 maker's profit short by 1 sompi on a re-arming buy-back",
        0,
    );
    mb(
        MergeBKnobs { exit_delta: -1, ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB12 exit keeps 1 sompi less than its remaining qty' proceeds + prefund",
        0,
    );
    let mut sl = booked_exit_b(&rpta_p(&f, 0, 21), 4, 0);
    sl.armed = 1;
    mb(
        MergeBKnobs { exit: Some(sl), leg: 1, ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB13 stop-loss buy-back re-arms its entry (attacker-funded qty for stopped-out cycle)",
        0,
    );
    // Two booked exits; the entry names only the first. The second pays the maker the profit, the
    // seller keeps both its tokens and the buy-back money, and its leftovers go to the filler.
    let mut s = rpta_merge(&f, &MergeBKnobs::new(&f, 0, 4, 4));
    let last = s.outputs.len() - 1;
    s.outputs.truncate(last);
    let b_in = s.inputs.len();
    let x2 = booked_exit_b(&rpta_p(&f, 0, 21), 4, 0);
    let x2_value = 4 * (sell_proceeds(&ip21) + ip21.prefund) + ip21.exit_carrier;
    s.inputs.push(call(
        &condb(n, &x2),
        "settle",
        vec![nb(4 * WHOLE), iv(1), iv(0), iv(0), iv(0)],
        "condb.settle",
        x2_value,
        cov(0xe2),
        2_000,
    ));
    while s.outputs.len() < b_in {
        s.outputs.push(out(0, p2pk_spk(&taker), None));
    }
    s.outputs.push(out(4 * x2.rpt_price - bid_all_in(4, x2.tp, x2.tip), p2pk_spk(&m), None));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(
        n,
        &with_name(
            s,
            "NRPB14 double re-arm: a second booked exit rides on the first one's merge (buy-back money and leftovers skimmed)",
        ),
        b_in,
    );

    // ---- merge (entry side)
    mb(
        MergeBKnobs { cont_delta: -1, ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB15 partial buy-back: entry re-armed 1 sompi short of 3 prefunds",
        2,
    );
    mb(
        MergeBKnobs { cont_delta: -1, ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB16 sell-out buy-back: the exit's leftovers skimmed by 1 sompi",
        2,
    );
    mb(
        MergeBKnobs { custody_delta: -1, ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB16b new custody carries 1 sompi less than exitCarrier",
        2,
    );
    mb(MergeBKnobs { custody_delta: -1, ..MergeBKnobs::new(&f, 6, 4, 3) }, "NRPB16c existing custody's carrier skimmed by 1 sompi", 2);
    // the custody after the merge holds 8 qty, the ninth goes to the filler
    let mut s = with_name(
        rpta_merge(&f, &MergeBKnobs { custody_qty: Some(8), ..MergeBKnobs::new(&f, 6, 4, 3) }),
        "NRPB17 custody after the merge one whole short (8 instead of 9, one whole to the filler)",
    );
    set_leader_next(&mut s, 1, vec![tok_state(8 * WHOLE, &i.as_bytes(), SCHEME_COVID), tok_state(WHOLE, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    s.inputs.push(p2pk_in(&f.taker, 100 * KAS));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(n, &s, 2);
    mb(
        MergeBKnobs { custody_owner: Some(taker), ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB18 bought-back qty routed to the filler instead of the entry's custody",
        2,
    );
    let ip6 = rpta_p(&f, 6, 21);
    mb(
        MergeBKnobs { cont: Some(IfdAP { qty: 9, price: P255, ..ip6.clone() }), ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB19 re-arm with altered parameters (limit 2.50 -> 2.55)",
        2,
    );
    mb(
        MergeBKnobs { cont: Some(IfdAP { qty: 9, rpt: 99, ..ip6.clone() }), ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB19b re-arm refills the cycle count",
        2,
    );
    mb(
        MergeBKnobs { cont: Some(IfdAP { qty: 8, ..ip6.clone() }), ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB19c entry records fewer qty than its custody holds (6 + 2 instead of 6 + 3)",
        2,
    );
    mb(
        MergeBKnobs { entry: stop_ip_armed(&f), cont: Some(IfdAP { qty: 4, ..stop_ip_armed(&f) }), ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB19d an empty sell-stop entry re-arms still armed",
        2,
    );
    mb(
        MergeBKnobs { exit: Some(rpta_p(&f, 0, 21).exit.with_qty(4)), ..MergeBKnobs::new(&f, 0, 4, 4) },
        "NRPB20 entry re-arms from a plain exit of another order (attacker-funded tokens)",
        2,
    );
    // A crafted non-KobCondBid input imitating a booked exit of this entry (empty entry).
    let mut s = rpta_merge(&f, &MergeBKnobs::new(&f, 0, 4, 4));
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0] = fake_exit_b(&f, i, 4 * WHOLE, x_value);
    s.outputs[0].value = 0;
    s.outputs[0].script_public_key = p2pk_spk(&taker);
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRPB21 entry re-arms from a crafted input imitating its booked exit (not a KobCondBid)"), 2);
    // Strays and custody stand-ins.
    let mut s = with_name(
        rpta_merge(&f, &MergeBKnobs::new(&f, 0, 4, 4)),
        "NRPB24 merge into an empty entry co-spends a 2-whole stray owned by it",
    );
    s.inputs.push(owned_tok(&f, 2, i, None));
    set_leader_next(&mut s, 1, vec![tok_state(4 * WHOLE, &i.as_bytes(), SCHEME_COVID), tok_state(2 * WHOLE, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(2 * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(n, &s, 2);
    let mut s = with_name(
        rpta_merge(&f, &MergeBKnobs::new(&f, 0, 4, 4)),
        "NRPB24b merge into an empty entry uses a 2-whole stray owned by it as the template source",
    );
    s.inputs.push(owned_tok(&f, 2, i, None));
    set_leader_next(&mut s, 1, vec![tok_state(4 * WHOLE, &i.as_bytes(), SCHEME_COVID), tok_state(2 * WHOLE, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(2 * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    let stray_in = s.inputs.len() - 1;
    set_arg(&mut s, 2, 1, iv(stray_in as i64));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(n, &s, 2);
    let mut s = with_name(
        rpta_merge(&f, &MergeBKnobs::new(&f, 6, 4, 3)),
        "NRPB25 a 7-whole stray owned by the entry stands in for its 6-whole custody (the extra whole to the filler)",
    );
    s.inputs[3] = owned_tok(&f, 7, i, None);
    set_leader_next(&mut s, 1, vec![tok_state(9 * WHOLE, &i.as_bytes(), SCHEME_COVID), tok_state(WHOLE, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    s.inputs.push(p2pk_in(&f.taker, 100 * KAS));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(n, &s, 2);

    // ---- the merge requires the exit to be exactly one this entry books (terms, parent,
    // rptPrice, rptPre), run by its settle with n = m, and never costs the entry
    let atk = pk(&f.taker);
    let x = CondBP { maker: atk, ..booked_exit_b(&rpta_p(&f, 6, 21), 4, 0) };
    let mut s = rpta_merge(&f, &MergeBKnobs { exit: Some(x), ..MergeBKnobs::new(&f, 6, 4, 3) });
    s.outputs[0].script_public_key = p2pk_spk(&atk);
    run_bad(n, &with_name(s, "NRPB29 look-alike exit of another maker (parent = this entry) re-arms the entry"), 2);
    let x = CondBP { rpt_pre: ip6.prefund - 1, ..booked_exit_b(&rpta_p(&f, 6, 21), 4, 0) };
    mb(
        MergeBKnobs { exit: Some(x), ..MergeBKnobs::new(&f, 6, 4, 3) },
        "NRPB30 look-alike exit with another rptPre (prefund - 1) re-arms the entry",
        2,
    );
    // an underfunded look-alike (1000 sompi) sells out: the old floor (entry + exit - proceeds) fell below the entry's own value
    let mut s = rpta_merge(&f, &MergeBKnobs::new(&f, 6, 4, 4));
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0].entry.amount = 1_000;
    let e_out = s.outputs.len() - 2;
    let old_floor = s.outputs[e_out].value as i64 - x_value + 1_000;
    s.outputs[e_out].value = old_floor as u64;
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRPB31 underfunded look-alike exit sells out: the entry keeps less than its own value"), 2);
    // the exit is cancelled by its maker beside a merge that claims 3 qty: only its settle may name the merge
    let mut s = rpta_merge(&f, &MergeBKnobs::new(&f, 6, 4, 3));
    let xp = booked_exit_b(&rpta_p(&f, 6, 21), 4, 0);
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0] = call(&condb(n, &xp), "cancel", vec![Arg::Sig(f.maker_a, 0x01)], "condb.cancel", x_value, cov(RPTA_EXIT), 2_000);
    s.outputs[0] = out(0, p2pk_spk(&m), None);
    s.outputs[1] = out(0, p2pk_spk(&m), None);
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRPB32 exit cancelled beside a merge of 3 qty (the merge is not its settle)"), 2);
    // ... or armed by update next to a bid fill (touch, v2.6): an update sigscript [ev, tk, tag, redeem] is no settle
    let mut s = rpta_merge(&f, &MergeBKnobs::new(&f, 6, 4, 3));
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0] = call(&condb(n, &xp), "update", vec![iv(0), iv(-1)], "condb.update", x_value, cov(RPTA_EXIT), 2_000);
    s.outputs[0] = out(0, p2pk_spk(&m), None);
    s.outputs[1] = out(x_value, spk_of(&condb(n, &xp.armed())), Some((0, cov(RPTA_EXIT))));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    let (ei, _) = add_ev(&f, &mut s, &Ev::bid(285_000_000));
    set_arg(&mut s, 0, 0, iv(ei));
    run_bad(n, &with_name(s, "NRPB33 exit armed by update beside a merge of 3 qty (an update is not its settle)"), 2);
    // The other direction: a re-arming take-profit names, as its merge, its sell-stop entry's arming update next to an
    // ask fill: the exit's whole budgets would leak to the filler. (The merge is laid out for an empty entry; the entry that
    // is armed instead still holds qty, since update arms only an entry with qty to fill.)
    let stop_e = IfdAP { entry_stop: 260_000_000, ..rpta_p(&f, 0, 21) };
    let mut s = rpta_merge(&f, &MergeBKnobs { entry: stop_e.clone(), ..MergeBKnobs::new(&f, 0, 4, 3) });
    let stop_e = IfdAP { qty: 6, ..stop_e };
    let ev_in = 2;
    let e_value = s.inputs[ev_in].entry.amount as i64;
    s.inputs[ev_in] = call(&ifda(n, &stop_e), "update", vec![iv(0), iv(0)], "ifda.update", e_value, cov(RPTA_ENTRY), 1_000);
    let ci = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPTA_ENTRY))).unwrap();
    s.outputs[ci] = out(e_value, spk_of(&ifda(n, &IfdAP { armed: 1, ..stop_e.clone() })), Some((ev_in as u16, cov(RPTA_ENTRY))));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    let (ei, ti) = add_ev(&f, &mut s, &Ev::ask(255_000_000));
    set_arg(&mut s, ev_in, 0, iv(ei));
    set_arg(&mut s, ev_in, 1, iv(ti));
    run_bad(n, &with_name(s, "NRPB34 re-arming take-profit names its entry's arming update as the merge (budgets leak)"), 0);

    // ---- close
    let e0 = rpta_p(&f, 0, 1);
    let close = |e: &IfdAP, lock: u64, name: &str| Scn {
        name: name.into(),
        inputs: vec![call(&ifda(n, e), "close", vec![], "ifda.close", 2 * CARRIER, i, (EXPIRY - 1_000) as u64)],
        outputs: vec![out(2 * CARRIER - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time: lock,
        payload: vec![],
    };
    run_bad(
        n,
        &close(&rpta_p(&f, 3, 1), EXPIRY as u64, "NRPB26 close of an entry that still has qty (its custody would be orphaned)"),
        0,
    );
    run_bad(n, &close(&e0, EXPIRY as u64 - 1, "NRPB27 close before the soft expiry"), 0);
    let mut s = close(&e0, EXPIRY as u64, "NRPB28 close co-spends a stray owned by the entry");
    s.inputs.push(owned_tok(&f, 2, i, Some(vec![tok_state(2 * WHOLE, &taker, SCHEME_P2PK)])));
    s.outputs.push(out(CARRIER, tk.spk(2 * WHOLE, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);

    // ---- a merge into a stop entry armed by update and not filled yet keeps its band origin (the armed UTXO's DAA)
    let ae = IfdAP { entry_stop: 260_000_000, band_daa: 300, armed: 1, ..rpta_p(&f, 6, 21) };
    let k = MergeBKnobs { entry: ae.clone(), ..MergeBKnobs::new(&f, 6, 4, 3) };
    run_ok(
        n,
        &with_name(rpta_merge(&f, &k), "RPB10 merge into an armed, unfilled stop entry: the continuation records its band origin"),
    );
    let s = rpta_merge(&f, &MergeBKnobs { cont: Some(IfdAP { qty: 9, ..ae.clone() }), ..k.clone() });
    run_bad(n, &with_name(s, "NRPB35 merge into an armed, unfilled stop entry keeps armed = 1 (its band auction restarts)"), 2);
}
/// An armed repeating sell-stop entry (stop 2.60, limit 2.50) with no qty left.
fn stop_ip_armed(f: &Fx) -> IfdAP {
    let mut p = rpta_p(f, 0, 21);
    p.entry_stop = 260_000_000;
    p.armed = 1;
    p
}

// ================================================================ protocol v3 (base units, rounding, minFill, tip, merge)

/// A plain KobCondBid LIMIT-leg fill in BASE units (no stop, no evidence): amountLeft `al`, fills `n`
/// base units at the limit price, with minFill `mf`. The order terminates when `n == al`, else continues.
fn condb_fill_base(f: &Fx, al: i64, n: i64, mf: i64) -> Scn {
    let c = cov(0xe1);
    // tp chosen so (tp + tip) % scale == 1: the spend floor differs from the ceil at a non-multiple fill
    let cp = CondBP { qty_base: Some(al), min_fill: mf, tp: 200_000_001, stop: 0, slip_bps: 0, ..CondBP::oco(pk(&f.maker_b)) };
    let order = condb(&f.net, &cp);
    let taker = pk(&f.taker);
    let rate = cp.tp + cp.tip; // limit leg (leg 0)
    let v = quote_up(al, rate) + DC; // escrow: enough for the whole amount plus the delivery carrier
    let spend = q_dn(n, rate);
    let cont = n < al;
    let mut s = Scn {
        name: "condb base fill".into(),
        inputs: vec![
            call(&order, "settle", vec![nb(n), iv(1), iv(0), iv(0), iv(0)], "condb.settle", v, c, 2_000),
            tok_in(
                &f.net.t,
                CARRIER,
                n,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(n, &cp.maker, SCHEME_P2PK)]),
                Wit::P2pk(f.taker),
                1_000,
            ),
        ],
        outputs: vec![out(if cont { DC } else { v - spend }, f.net.t.spk(n, &cp.maker, SCHEME_P2PK), Some((1, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
    };
    if cont {
        let next = CondBP { qty_base: Some(al - n), ..cp.clone() };
        s.outputs.push(out(v - spend - DC, spk_of(&condb(&f.net, &next)), Some((0, c))));
    }
    s.outputs.push(out(spend + CARRIER - NET_FEE, p2pk_spk(&taker), None));
    s
}

/// A plain KobIfdAsk entry fill in BASE units: custody `have`, sells `n` base units (exit amountLeft = n),
/// minFill `mf`. The entry continues when `n < have`, else sells out.
fn ifda_fill_base(f: &Fx, have: i64, n: i64, mf: i64) -> Scn {
    let t = &f.net.t;
    let i = cov(0xf1);
    // price - tip and prefund both % scale == 1: ceil differs from floor at a non-multiple fill
    let ip = IfdAP { price: P250 + 1, prefund: IFDA_PREFUND + 1, qty_base: Some(have), min_fill: mf, ..ifda_p(f) };
    let value = CARRIER + quote_up(have, ip.prefund) + 2 * ip.exit_carrier;
    let entry = ifda(&f.net, &ip);
    let exit = condb(&f.net, &CondBP { qty_base: Some(n), ..ip.exit.clone() });
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let proceeds = q_up(n, ip.price - ip.tip);
    let pre = q_up(n, ip.prefund);
    let rest = have - n;
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest, &i.as_bytes(), SCHEME_COVID));
    }
    next.push(tok_state(n, &taker, SCHEME_P2PK));
    let mut outputs = vec![];
    if rest > 0 {
        outputs.push(out(proceeds + pre + ip.exit_carrier, spk_of(&exit), None));
        let cont = ifda(&f.net, &IfdAP { qty_base: Some(rest), ..ip.clone() });
        outputs.push(out(value - pre - ip.exit_carrier, spk_of(&cont), Some((0, i))));
        outputs.push(out(CARRIER, t.spk(rest, &i.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))));
    } else {
        outputs.push(out(proceeds + value + CARRIER, spk_of(&exit), None));
    }
    outputs.push(out(CARRIER, t.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    outputs.push(out(1000 * KAS - proceeds - CARRIER - NET_FEE, p2pk_spk(&taker), None));
    let mut s = Scn {
        name: "ifda base fill".into(),
        inputs: vec![
            call(
                &entry,
                "settle",
                vec![
                    nb(n),
                    iv(1),
                    iv(if rest > 0 { 2 } else { 0 }),
                    iv(0),
                    Arg::V(bytes(&tpl.pre)),
                    Arg::V(bytes(&tpl.suf)),
                    iv(0),
                    iv(0),
                    iv(0),
                ],
                "ifda.settle",
                value,
                i,
                1_000,
            ),
            tok_in(t, CARRIER, have, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
    };
    regen(&mut s, 0, 0);
    s
}

/// A take-profit MERGE of a booked KobCondBid exit (leg 0, limit) by a KobIfdAsk entry that already holds a
/// custody, in BASE units. The exit holds `x` base units and the take-profit buys back `m` (m <= x). Output
/// deltas let an attack skim one sompi from the maker profit, the exit continuation, the entry continuation or
/// the new custody. Inputs: [0] exit, [1] seller tokens, [2] entry (merge), [3] entry custody.
#[allow(clippy::too_many_arguments)]
fn v3_merge_base(f: &Fx, x: i64, m: i64, maker_d: i64, exit_d: i64, cont_d: i64, custody_d: i64) -> Scn {
    let t = &f.net.t;
    let i = cov(RPTA_ENTRY);
    let c = cov(RPTA_EXIT);
    let mk = pk(&f.maker_a);
    let taker = pk(&f.taker);
    let entry_whole = 6;
    // price - tip and prefund both % scale == 1, so ceil differs from floor at a non-multiple merge amount
    let ip = IfdAP { price: P250 + 1, prefund: IFDA_PREFUND + 1, ..rpta_p(f, entry_whole, 21) };
    let rate = sell_proceeds(&ip); // price - tip
    let xp = CondBP {
        qty_base: Some(x),
        parent: i.as_bytes(),
        rpt_price: rate,
        rpt_pre: ip.prefund,
        rpt_until: EXPIRY.min(1_000 + MAX_IDLE),
        ..ip.exit.clone()
    };
    let x_value = q_up(x, rate) + q_up(x, ip.prefund) + ip.exit_carrier;
    let entry_in = CARRIER + entry_whole * (ip.prefund + IFDA_EXIT_CARRIER);
    let spend = q_dn(m, xp.tp + xp.tip);
    let proceeds = q_up(m, rate);
    let rest = x - m;
    let custody_base = entry_whole * WHOLE + m;
    let next = vec![tok_state(custody_base, &i.as_bytes(), SCHEME_COVID)];
    let mut s = Scn {
        name: "v3 merge base".into(),
        inputs: vec![
            call(&condb(&f.net, &xp), "settle", vec![nb(m), iv(1), iv(0), iv(0), iv(0)], "condb.settle", x_value, c, 2_000),
            tok_in(t, CARRIER, m, &taker, SCHEME_P2PK, TOKEN_COV, Some(next), Wit::P2pk(f.taker), 1_500),
        ],
        outputs: vec![out(proceeds - spend + maker_d, p2pk_spk(&mk), None)],
        lock_time: NOW,
        payload: vec![],
    };
    if rest > 0 {
        let keep = x_value - proceeds - q_up(m, ip.prefund);
        s.outputs.push(out(keep + exit_d, spk_of(&condb(&f.net, &CondBP { qty_base: Some(rest), ..xp.clone() })), Some((0, c))));
    }
    let entry_idx = s.inputs.len();
    let custody_out = s.outputs.len();
    s.inputs.push(call(
        &ifda(&f.net, &ip),
        "settle",
        vec![
            nb_merge(0, m),
            iv(entry_idx as i64 + 1),
            iv(custody_out as i64),
            iv(0),
            Arg::V(bytes(&[])),
            Arg::V(bytes(&[])),
            iv(0),
            iv(0),
            iv(0),
        ],
        "ifda.merge",
        entry_in,
        i,
        1_000,
    ));
    s.inputs.push(tok_in(t, CARRIER, entry_whole * WHOLE, &i.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000));
    s.outputs.push(out(CARRIER + custody_d, t.spk(custody_base, &i.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))));
    let floor = {
        let base = entry_in + q_up(m, ip.prefund);
        if rest == 0 {
            base.max(entry_in + x_value - q_up(m, rate))
        } else {
            base
        }
    };
    let cont = IfdAP { qty_base: Some(custody_base), ..ip.clone() };
    s.outputs.push(out(floor + cont_d, spk_of(&ifda(&f.net, &cont)), Some((entry_idx as u16, i))));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}

/// minFill in base units (KobCondBid and KobIfdAsk): exactly minFill is accepted, one below it that does not
/// take the rest is refused, and a below-minFill fill that DOES take the rest is accepted.
#[test]
fn v3_buy_min_fill() {
    let f = fx();
    let n = &f.net;
    // KobCondBid: amountLeft 5000, minFill 1500 (non-multiple)
    run_ok(n, &with_name(condb_fill_base(&f, 5_000, 1_500, 1_500), "V3CB01 KobCondBid fill of exactly minFill (1500), rest remains"));
    run_bad(
        n,
        &with_name(condb_fill_base(&f, 5_000, 1_499, 1_500), "NV3CB01 KobCondBid fill below minFill that does not take the rest"),
        0,
    );
    run_ok(n, &with_name(condb_fill_base(&f, 1_200, 1_200, 1_500), "V3CB02 KobCondBid below-minFill fill that takes the rest"));
    // KobIfdAsk: custody 5000, minFill 1500
    run_ok(n, &with_name(ifda_fill_base(&f, 5_000, 1_500, 1_500), "V3IA01 KobIfdAsk fill of exactly minFill (1500), custody remains"));
    run_bad(n, &with_name(ifda_fill_base(&f, 5_000, 1_499, 1_500), "NV3IA01 KobIfdAsk fill below minFill that does not sell out"), 0);
    run_ok(n, &with_name(ifda_fill_base(&f, 1_200, 1_200, 1_500), "V3IA02 KobIfdAsk below-minFill fill that sells out"));
}

/// Maker-favour rounding at the boundary (base units, amounts not a multiple of the scale): the bid spend is
/// the floor and the ask-side proceeds / prefund are the ceil; one sompi on the maker-adverse side is refused.
#[test]
fn v3_buy_rounding() {
    let f = fx();
    let n = &f.net;
    // KobCondBid spend = floor(n * (legPrice + tip) / scale): n = 1500 base (1.5 whole), not a multiple
    run_ok(
        n,
        &with_name(condb_fill_base(&f, 1_500, 1_500, DEFAULT_MIN_FILL), "V3CB10 KobCondBid spend at exactly the floor (terminating)"),
    );
    let mut s = with_name(
        condb_fill_base(&f, 1_500, 1_500, DEFAULT_MIN_FILL),
        "NV3CB10 KobCondBid spend one sompi above the floor (delivery short)",
    );
    s.outputs[0].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(n, &s, 0);
    let mut s = with_name(
        condb_fill_base(&f, 5_000, 1_500, DEFAULT_MIN_FILL),
        "NV3CB11 KobCondBid spend one sompi above the floor (continuation short)",
    );
    s.outputs[1].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(n, &s, 0);
    // KobIfdAsk proceeds = ceil(n * (price - tip) / scale), pre = ceil(n * prefund / scale)
    run_ok(
        n,
        &with_name(ifda_fill_base(&f, 5_000, 1_500, DEFAULT_MIN_FILL), "V3IA10 KobIfdAsk proceeds + prefund at exactly the ceil"),
    );
    // proceeds ceil: the exit is one sompi short, the sompi goes to the taker
    let mut s = with_name(
        ifda_fill_base(&f, 5_000, 1_500, DEFAULT_MIN_FILL),
        "NV3IA10 KobIfdAsk exit one sompi short of ceil proceeds (sompi to the taker)",
    );
    s.outputs[0].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);
    // prefund ceil: pre also sets the entry's continuation floor (in - pre - exitCarrier), so the attack moves
    // the sompi from the exit into the entry continuation (outputs: [0] exit, [1] entry continuation)
    let mut s = with_name(
        ifda_fill_base(&f, 5_000, 1_500, DEFAULT_MIN_FILL),
        "NV3IA11 KobIfdAsk exit one sompi short of ceil prefund (sompi kept by the entry)",
    );
    s.outputs[0].value -= 1;
    s.outputs[1].value += 1;
    regen(&mut s, 0, 0);
    run_bad(n, &s, 0);
}

/// The tip never exceeds the price (KobIfdAsk price >= tip), including a sell-stop entry whose auction price is
/// above `price` but still below `tip`.
#[test]
fn v3_buy_tip() {
    let f = fx();
    let n = &f.net;
    // price == tip is the boundary (accepted); price < tip is refused.
    let edge = IfdAP { price: TIP, tip: TIP, prefund: 0, exit: CondBP { tp: TIP, ..ifda_exit(pk(&f.maker_a)) }, ..ifda_p(&f) };
    run_ok(n, &with_name(ifda_fill_at_tip(&f, &edge, 10, 4, TIP), "V3IA20 KobIfdAsk with price == tip (boundary) fills"));
    let below = IfdAP { price: TIP - 1, ..edge.clone() };
    run_bad(
        n,
        &with_name(ifda_fill_at_tip(&f, &below, 10, 4, TIP - 1), "NV3IA20 KobIfdAsk limit entry with price < tip is unfillable"),
        0,
    );
    // sell-stop entry: auction price p = entryStop (> price) but price < tip, so the booked exit's rptPrice would be negative
    let stop = IfdAP { price: TIP - 1, entry_stop: TIP + 1, ..edge.clone() };
    let down = Ev::ask(TIP);
    run_bad(
        n,
        &with_name(
            ifda_fill_x(&f, &stop, 10, 4, IFDA_VALUE, TIP + 1, Some(&down), 0, 1),
            "NV3IA21 sell-stop entry: auction price above price, but price < tip",
        ),
        0,
    );
}

/// A KobIfdAsk entry fill at an auction/limit price `eff` with a price-tip edge order (uses ifda_fill_x shape).
fn ifda_fill_at_tip(f: &Fx, ip: &IfdAP, have: i64, n: i64, eff: i64) -> Scn {
    ifda_fill_x(f, ip, have, n, IFDA_VALUE, eff, None, 0, 0)
}

/// Overflow fails closed (no contract check: the engine fails the script on the overflowing product). A full
/// fill at the largest fitting amount validates; one more base unit, where the product overflows i64, fails
/// without the maker being paid less.
#[test]
fn v3_buy_overflow() {
    let f = fx();
    let n = &f.net;
    // rate = 1e12 sompi per whole token: products jump by 1e12 >> the carriers, so the largest fitting fill
    // leaves room for the carriers and one scale more overflows the covenant product q * rate (q = n / scale).
    let rate = 1_000_000_000_000i64;
    let q_ok = i64::MAX / rate; // largest q with q * rate <= i64::MAX
    let n_ok = q_ok * SCALE;
    // i128 spend/value, saturated to i64 (the overflow case never reads these: the covenant fails first)
    let spend_sat = |nn: i64| -> i64 { ((nn as i128 * rate as i128) / SCALE as i128).min(i64::MAX as i128) as i64 };

    // ---- KobCondBid: spend = floor(n * (tp + tip) / scale), tp + tip = rate
    let cp_rate = CondBP { tp: rate - TIP, tip: TIP, stop: 0, slip_bps: 0, ..CondBP::oco(pk(&f.maker_b)) };
    let condb_of = |al: i64, nn: i64| {
        let c = cov(0xe1);
        let order = condb(n, &CondBP { qty_base: Some(al), ..cp_rate.clone() });
        let taker = pk(&f.taker);
        let spend = spend_sat(nn);
        let v = spend.saturating_add(DC);
        let mut s = Scn {
            name: "condb overflow".into(),
            inputs: vec![
                call(&order, "settle", vec![nb(nn), iv(1), iv(0), iv(0), iv(0)], "condb.settle", v, c, 2_000),
                tok_in(
                    &f.net.t,
                    CARRIER,
                    nn,
                    &taker,
                    SCHEME_P2PK,
                    TOKEN_COV,
                    Some(vec![tok_state(nn, &cp_rate.maker, SCHEME_P2PK)]),
                    Wit::P2pk(f.taker),
                    1_000,
                ),
            ],
            outputs: vec![out(v.saturating_sub(spend), f.net.t.spk(nn, &cp_rate.maker, SCHEME_P2PK), Some((1, TOKEN_COV)))],
            lock_time: NOW,
            payload: vec![],
        };
        s.outputs.push(out(spend.saturating_add(CARRIER - NET_FEE), p2pk_spk(&taker), None));
        s
    };
    run_ok(n, &with_name(condb_of(n_ok, n_ok), "V3CB30 KobCondBid full fill at the largest fitting amount validates"));
    run_bad(
        n,
        &with_name(condb_of(n_ok + SCALE, n_ok + SCALE), "NV3CB30 KobCondBid one scale more overflows the quote and fails closed"),
        0,
    );

    // ---- KobIfdAsk: proceeds = ceil(n * (price - tip) / scale), price - tip = rate, prefund 0
    let ip = IfdAP { price: rate + TIP, tip: TIP, prefund: 0, ..ifda_p(&f) };
    let ifda_of = |have: i64, nn: i64| {
        let t = &f.net.t;
        let i = cov(0xf1);
        // the overflowing case is rejected by the covenant before any output is read, so modest placeholder
        // values keep the test's own i64 sums (balance / mass) from overflowing
        let proceeds = if nn > n_ok { 1000 * KAS } else { spend_sat(nn) };
        let value = CARRIER + ip.exit_carrier;
        let entry = ifda(&f.net, &IfdAP { qty_base: Some(have), ..ip.clone() });
        let exit = condb(&f.net, &CondBP { qty_base: Some(nn), ..ip.exit.clone() });
        let tpl = condb_tpl(&f.net);
        let taker = pk(&f.taker);
        let mut s = Scn {
            name: "ifda overflow".into(),
            inputs: vec![
                call(
                    &entry,
                    "settle",
                    vec![nb(nn), iv(1), iv(0), iv(0), Arg::V(bytes(&tpl.pre)), Arg::V(bytes(&tpl.suf)), iv(0), iv(0), iv(0)],
                    "ifda.settle",
                    value,
                    i,
                    1_000,
                ),
                tok_in(
                    t,
                    CARRIER,
                    have,
                    &i.as_bytes(),
                    SCHEME_COVID,
                    TOKEN_COV,
                    Some(vec![tok_state(nn, &taker, SCHEME_P2PK)]),
                    Wit::CovId,
                    1_000,
                ),
                p2pk_in(&f.taker, proceeds.saturating_add(20 * KAS)),
            ],
            outputs: vec![out(proceeds.saturating_add(value).saturating_add(CARRIER), spk_of(&exit), None)],
            lock_time: NOW,
            payload: vec![],
        };
        s.outputs.push(out(CARRIER, t.spk(nn, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
        s.outputs.push(out(0, p2pk_spk(&taker), None));
        let last = s.outputs.len() - 1;
        balance(&mut s, last);
        regen(&mut s, 0, 0);
        s
    };
    run_ok(n, &with_name(ifda_of(n_ok, n_ok), "V3IA30 KobIfdAsk sell-out at the largest fitting amount validates"));
    run_bad(
        n,
        &with_name(ifda_of(n_ok + SCALE, n_ok + SCALE), "NV3IA30 KobIfdAsk one scale more overflows the proceeds and fails closed"),
        0,
    );
}

/// Repeat-merge rounding and framing (protocol v3): the maker-favour ceils of the rearm (KobCondBid proceeds /
/// prefund) and of the entry's merge floor (KobIfdAsk prefund / sell-out), each one sompi short, are refused.
#[test]
fn v3_buy_merge_base() {
    let f = fx();
    let n = &f.net;
    // non-multiple amounts: x = 4500 base, partial m = 2500, sell-out m = 4500
    run_ok(n, &with_name(v3_merge_base(&f, 4_500, 2_500, 0, 0, 0, 0), "V3CB20 repeat merge (partial 2500/4500) at exact rounding"));
    // sell-out (no exit continuation: proceeds only sets the maker's floor there)
    run_bad(
        n,
        &with_name(
            v3_merge_base(&f, 4_500, 4_500, -1, 0, 0, 0),
            "NV3CB12 rearm (sell-out) maker profit one sompi short of ceil proceeds",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(v3_merge_base(&f, 4_500, 2_500, 0, -1, 0, 0), "NV3CB13 rearm exit continuation one sompi short (ceil prefund)"),
        0,
    );
    run_bad(
        n,
        &with_name(
            v3_merge_base(&f, 4_500, 2_500, 0, 0, -1, 0),
            "NV3IA12 merge: entry continuation one sompi short of in + ceil(m*prefund)",
        ),
        2,
    );
    run_ok(n, &with_name(v3_merge_base(&f, 4_500, 4_500, 0, 0, 0, 0), "V3IA14 repeat merge (sell-out 4500) at exact rounding"));
    run_bad(
        n,
        &with_name(v3_merge_base(&f, 4_500, 4_500, 0, 0, -1, 0), "NV3IA13 merge sell-out: entry continuation one sompi short"),
        2,
    );
}

/// A repeating sell-first KobIfdAsk fill that BOOKS its exit (rptAmount > n), with price == tip (so proceeds
/// and prefund are zero and n can be driven to the MERGE_K boundary). The entry sells out (custody = n) and
/// continues empty-but-repeating; the exit is booked (parent = entry, rptPrice = rptPre = 0).
fn booking_fill(f: &Fx, n: i64, rpt: i64) -> Scn {
    let t = &f.net.t;
    let i = cov(0xf1);
    let mk = pk(&f.maker_a);
    let exit_params = CondBP { tp: TIP, stop: 0, slip_bps: 0, ..ifda_exit(mk) };
    let ip = IfdAP {
        price: TIP,
        tip: TIP,
        prefund: 0,
        exit_carrier: IFDA_EXIT_CARRIER,
        exit: exit_params.clone(),
        qty_base: Some(n),
        rpt_base: Some(rpt),
        ..ifda_p(f)
    };
    let value = CARRIER + IFDA_EXIT_CARRIER;
    let entry = ifda(&f.net, &ip);
    let xp = CondBP {
        qty_base: Some(n),
        parent: i.as_bytes(),
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: EXPIRY.min(1_000 + MAX_IDLE),
        ..exit_params
    };
    let exit = condb(&f.net, &xp);
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let entry_cont = IfdAP { qty_base: Some(0), rpt_base: Some(rpt - n), ..ip.clone() };
    let mut s = Scn {
        name: "booking fill".into(),
        inputs: vec![
            call(
                &entry,
                "settle",
                vec![nb(n), iv(1), iv(0), iv(0), Arg::V(bytes(&tpl.pre)), Arg::V(bytes(&tpl.suf)), iv(0), iv(0), iv(0)],
                "ifda.settle",
                value,
                i,
                1_000,
            ),
            tok_in(
                t,
                CARRIER,
                n,
                &i.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(n, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(ip.exit_carrier, spk_of(&exit), None),
            out(value - ip.exit_carrier + CARRIER, spk_of(&ifda(&f.net, &entry_cont)), Some((0, i))),
            out(CARRIER, t.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(0, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    regen(&mut s, 0, 0);
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}

/// Rewrites a first push of exactly 8 bytes (`0x08 ...`) into a NON-minimal 9-byte push (`0x09 ... 0x00`): the
/// pushed value's low 8 bytes are unchanged, so a merge that reads bytes [1..9) would still see the same value,
/// but the first byte is no longer the 0x08 that both merges require.
fn craft_non_minimal(ss: &[u8]) -> Vec<u8> {
    assert_eq!(ss[0], 0x08, "first push must be an 8-byte push");
    let mut out = vec![0x09];
    out.extend_from_slice(&ss[1..9]); // the 8 value bytes
    out.push(0x00); // one extra (high) byte so the push is 9 bytes but the value is unchanged
    out.extend_from_slice(&ss[9..]);
    out
}

/// A sell-out repeat merge (as v3_merge_base with m == x) where one counterparty's fill argument is pushed
/// non-minimally: `craft_exit` crafts the exit's settle (read by the entry), else the entry's merge (read by
/// the exit). Returns (scenario, the input index whose 0x08 check must reject it).
fn v3_merge_craft(f: &Fx, m: i64, craft_exit: bool) -> (Scn, usize) {
    let mut s = v3_merge_base(f, m, m, 0, 0, 0, 0);
    let ip = IfdAP { price: P250 + 1, prefund: IFDA_PREFUND + 1, ..rpta_p(f, 6, 21) };
    let xp = CondBP {
        qty_base: Some(m),
        parent: cov(RPTA_ENTRY).as_bytes(),
        rpt_price: sell_proceeds(&ip),
        rpt_pre: ip.prefund,
        rpt_until: EXPIRY.min(1_000 + MAX_IDLE),
        ..ip.exit.clone()
    };
    if craft_exit {
        let real = entry_ss(&condb(&f.net, &xp), "settle", &[bytes(&m.to_le_bytes()), int(1), int(0), int(0), int(0)]);
        s.inputs[0].role = Role::RawSs { ss: craft_non_minimal(&real), name: "exit.nonmin" };
        (s, 2) // the entry (input 2) reads the exit and must reject on its 0x08 check
    } else {
        let mut mb = m.to_le_bytes();
        mb[7] |= 0x80; // the merge value -(0 * MERGE_K + m) = -m
        let real = entry_ss(
            &ifda(&f.net, &ip),
            "settle",
            &[bytes(&mb), int(3), int(1), int(0), bytes(&[]), bytes(&[]), int(0), int(0), int(0)],
        );
        s.inputs[2].role = Role::RawSs { ss: craft_non_minimal(&real), name: "entry.nonmin" };
        (s, 0) // the exit (input 0) reads the entry pin and must reject on its 0x08 check
    }
}

/// Repeat-merge framing (protocol v3): the booking amount stays below MERGE_K (2^53), the largest merge
/// argument round-trips, an exit's update may not run next to its repeat entry (the parent guard), and each
/// merge requires the other input's fill argument to start with the 8-byte push 0x08 (both directions).
#[test]
fn v3_buy_merge_push() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);

    // V3IA32: the largest merge argument -(k * 2^53 + m), k = 999, m = 2^53 - 1, round-trips to (k, m).
    // (k = 999 as a live input index would need 1000 inputs; here the argument encoding is verified.)
    let magnitude = 999i64 * MERGE_K + (MERGE_K - 1);
    assert!(magnitude > 0 && magnitude < i64::MAX, "merge magnitude fits a signed 8-byte script number");
    assert_eq!(magnitude / MERGE_K, 999, "V3IA32 decodes k = 999");
    assert_eq!(magnitude % MERGE_K, MERGE_K - 1, "V3IA32 decodes m = 2^53 - 1");
    println!("MERGE-ARG V3IA32 -(999 * 2^53 + (2^53 - 1)) = -{magnitude} round-trips to (k=999, m=2^53-1)");

    run_ok(
        n,
        &with_name(
            booking_fill(&f, MERGE_K - 1, MERGE_K),
            "V3IA31 booking at n = 2^53 - 1 (largest that fits the merge argument) accepted",
        ),
    );
    run_bad(
        n,
        &with_name(
            booking_fill(&f, MERGE_K, MERGE_K + 1),
            "NV3IA31 booking at n = 2^53 refused (merge argument would overflow into k)",
        ),
        0,
    );

    // NV3CB41: an exit's update (arm) in a transaction that also spends its repeat entry (parent present) is
    // refused by the parent guard OpCovInputCount(parent) == 0.
    let entry_ip = rpta_p(&f, 6, 21);
    let xp = booked_exit_b(&entry_ip, 4, 0);
    let mut s = condb_update(&f, &xp, &xp.armed(), &Ev::bid(285_000_000), 0);
    s.inputs.push(call(
        &ifda(n, &entry_ip),
        "cancel",
        vec![Arg::Sig(f.maker_a, 0x01)],
        "ifda.cancel",
        2 * CARRIER,
        cov(RPTA_ENTRY),
        1_000,
    ));
    s.outputs.push(out(2 * CARRIER - NET_FEE, p2pk_spk(&m), None));
    run_bad(n, &with_name(s, "NV3CB41 exit update (arm) next to its repeat entry is refused (parent guard)"), 0);

    // 0x08 first-byte checks (both directions): a non-minimally pushed fill argument whose low 8 bytes alias the
    // merge value is still refused, because its first byte is 0x09, not the 0x08 both merges require.
    let (s, at) = v3_merge_craft(&f, 4_500, true);
    run_bad(n, &with_name(s, "NV3IA40 entry refuses an exit whose settle argument is pushed non-minimally (first byte 0x09)"), at);
    let (s, at) = v3_merge_craft(&f, 4_500, false);
    run_bad(n, &with_name(s, "NV3CB40 exit refuses an entry whose merge argument is pushed non-minimally (first byte 0x09)"), at);
}

/// A buy-stop at a LOW stop price (below 10000 sompi per unit), armed, whole band at once:
/// the 3% band must hold (stop 9999 -> ceiling 10298). `delta`: sompi per unit paid above the ceiling.
fn fx_low_stop_b(f: &Fx, stop: i64, delta: i64) -> Scn {
    let cp = CondBP { tip: 0, stop, tp: 0, armed: 1, ..CondBP::oco(pk(&f.maker_a)) };
    let ceiling = stop.saturating_add(stop.saturating_mul(cp.slip_bps) / 10_000);
    let mut s = condb_fill_at(f, &cp, 4, 1, &cp.with_qty(6), None, 0, ceiling.saturating_add(delta));
    s.name = format!("FXL4b low buy-stop {stop}: stop-market fill at {} (band ceiling {ceiling})", ceiling.saturating_add(delta));
    s
}

/// Regressions (buy side, KCC-20): the multiply-first stop band. The repeat-IFD merge
/// checks are in `v2_buy_repeat_ifd_attacks` (NRPB29..NRPB32).
#[test]
fn v2_buy_fix_pass_regressions() {
    let f = fx();
    let n = &f.net;
    run_ok(n, &fx_low_stop_b(&f, 9_999, 0));
    run_bad(n, &fx_low_stop_b(&f, 9_999, 1), 0);
    run_ok(n, &fx_low_stop_b(&f, 100, 0));
}
