//! KOB protocol v3 harness (amounts in base units): KobAsk / KobBid / KobCondAsk / KobIfdBid with the reference
//! KCC-20 (argent kcc20-reference PR#1), executed in rusty-kaspa v2.1.0's TxScriptEngine with KIP-20
//! covenant context and script-unit metering.
//!
//! Amounts are token base units (any amount); prices and tips are sompi per whole token (`SCALE` base
//! units). Every order executes at its own all-in limit or better, rounded in the maker's favour
//! (quoteOf: up for what a maker receives, down for what a maker pays); the crossing spread and the
//! tips are the matcher's. Contract sources are read from `contracts/v2/`.
//! Run: cargo test --release -p kob-tests --test kob_v2_tests -- --nocapture --test-threads=1

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
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32};
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
const EXT: [u8; 32] = [0xee; 32];
const SCHEME_P2PK: u8 = 0x00;
const SCHEME_COVID: u8 = 0x04;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;

const CARRIER: i64 = 10 * KAS;
const DC: i64 = 10 * KAS; // bid delivery carrier
/// Base units per whole token (a 3-decimal token): the price denominator (`scale`) of the fixture orders.
const SCALE: i64 = 1_000;
/// One whole token in base units.
const TOK: i64 = SCALE;
/// Default minFill of the fixture orders: one whole token.
const MIN_FILL: i64 = TOK;
const TIP: i64 = 100_000; // default priority tip, sompi per whole token (0.001 KAS)
const EXPIRY: i64 = 400_000_000;
const NO_EXPIRY: i64 = 499_999_999_999;
const REFUND_TIP: i64 = 3_000_000;
const MAX_IDLE: i64 = 77_760_000;
const NOW: u64 = 1_000_000; // lockTime of fills (DAA type)
const NET_FEE: i64 = KAS / 10;
const EC: i64 = 10 * KAS; // exit-order carrier of if-done entries
/// Length of the KobCondAsk state an if-done entry commits to (through amountLeft).
const COND_COMMIT_LEN: usize = 279;
/// Repeat merge argument -(k * MERGE_K + m) (MERGE_K = 2^53 in the covenants).
const MERGE_K: i64 = 1 << 53;

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

/// The covenants' quoteOf: the quote value of n base units at rate r (quote units per `scale` base units),
/// rounded up (c = scale - 1) or down (c = 0), with the same split product
/// q * r + m * (r / scale) + (m * (r % scale) + c) / scale (q = n / scale, m = n % scale). None where a
/// covenant fails (a checked product or sum overflows).
fn quote_at(n: i64, r: i64, scale: i64, up: bool) -> Option<i64> {
    let c = if up { scale - 1 } else { 0 };
    let (q, m) = (n / scale, n % scale);
    q.checked_mul(r)?.checked_add(m.checked_mul(r / scale)?)?.checked_add(m.checked_mul(r % scale)?.checked_add(c)? / scale)
}
/// quoteOf at the fixture scale, rounded up (what a maker receives, what a bid's escrow consumes).
fn q_up(n: i64, r: i64) -> i64 {
    quote_at(n, r, SCALE, true).expect("quoteOf overflow")
}
/// quoteOf at the fixture scale, rounded down (what a maker pays).
fn q_down(n: i64, r: i64) -> i64 {
    quote_at(n, r, SCALE, false).expect("quoteOf overflow")
}
/// All-in minimum an ask (quote `p`, tip `tip`) receives for n base units: ceil(n * (p - tip) / scale).
fn ask_all_in(n: i64, p: i64, tip: i64) -> i64 {
    q_up(n, p - tip)
}
/// All-in maximum a bid (quote `p`, tip `tip`) pays for n base units: floor(n * (p + tip) / scale).
fn bid_all_in(n: i64, p: i64, tip: i64) -> i64 {
    q_down(n, p + tip)
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
    /// amountLeft (base units)
    amount: i64,
    min_fill: i64,
    /// token covenant id and scale (the evidence tests quote another token or scale)
    token: Hash,
    scale: i64,
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
            amount: 10 * TOK,
            min_fill: MIN_FILL,
            token: TOKEN_COV,
            scale: SCALE,
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
    min_fill: i64,
    token: Hash,
    scale: i64,
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
            min_fill: MIN_FILL,
            token: TOKEN_COV,
            scale: SCALE,
        }
    }
    /// Budget rate per whole token: the all-in quote, or the cap of a rising bid (pMax + tip).
    fn rate_max(&self) -> i64 {
        let pmax = if self.slope != 0 && self.price_end > self.price { self.price_end } else { self.price };
        pmax + self.tip
    }
    /// Budget a fill of n base units consumes from the escrow: used(n) = ceil(n * (pMax + tip) / scale).
    fn used(&self, n: i64) -> i64 {
        quote_at(n, self.rate_max(), self.scale, true).expect("used overflow")
    }
    /// Escrow of a bid funded for exactly `amount` base units (plus its delivery carrier).
    fn funded(&self, amount: i64) -> i64 {
        self.used(amount) + DC
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
/// Trigger evidence (touch): a plain resting KobAsk (ask) or KobBid of maker C, of which the matcher fills
/// all `amount` base units in the same transaction. Exposed at its quote since max(daa + interval,
/// activeFrom, custody DAA (ask)).
#[derive(Clone)]
struct Ev {
    ask: bool,
    price: i64,
    amount: i64,
    daa: u64,
    tok_daa: u64,
    active_from: i64,
    interval: i64,
    slope: i64,
    scale: i64,
    token: Hash,
    cov: Hash,
    mode: EvMode,
}
impl Ev {
    /// A resting ask (cov 0x9a) of 5 whole tokens quoting `price`, placed at DAA 1000 (exposed long before NOW).
    fn ask(price: i64) -> Self {
        Ev {
            ask: true,
            price,
            amount: 5 * TOK,
            daa: 1_000,
            tok_daa: 1_000,
            active_from: 0,
            interval: 0,
            slope: 0,
            scale: SCALE,
            token: TOKEN_COV,
            cov: cov(0x9a),
            mode: EvMode::Fill,
        }
    }
    /// A resting bid (cov 0x9b) funded for exactly 5 whole tokens quoting `price` (the fill exhausts it).
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
    /// minTouch: smallest evidence fill (base units)
    min_touch: i64,
    min_rest: i64,
    armed: i64,
    band_daa: i64,
    keeper_tip: i64,
    /// amountLeft (base units) and minFill
    amount: i64,
    min_fill: i64,
    /// repeat IFD: the entry this exit re-arms (0 = none), its budget rate and deadline
    parent: [u8; 32],
    rpt_price: i64,
    rpt_until: i64,
}
impl CondP {
    /// OCO: take-profit at 3.00, stop 2.00 with the default 3% band (stop leg 1.94), evidence fills of
    /// >= 1 whole token exposed >= 600 DAA (minRestDaa R = 600 in this harness; the wallet default is 50).
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
            min_touch: TOK,
            min_rest: 600,
            armed: 0,
            band_daa: 0,
            keeper_tip: 0,
            amount: 10 * TOK,
            min_fill: MIN_FILL,
            parent: [0; 32],
            rpt_price: 0,
            rpt_until: 0,
        }
    }
}
#[derive(Clone)]
struct IfdP {
    maker: [u8; 32],
    /// amountLeft (base units)
    amount: i64,
    price: i64,
    tip: i64,
    exit: CondP,
    min_fill: i64,
    entry_stop: i64,
    band_daa: i64,
    keeper_tip: i64,
    armed: i64,
    /// repeat: 0 = off, else 1 + base units of re-arms left (rptAmount)
    rpt: i64,
    /// stop entry: smallest evidence fill (base units)
    min_touch: i64,
}
impl IfdP {
    /// A limit IFO entry (no stop trigger), minFill one whole token.
    fn limit(maker: [u8; 32], amount: i64, price: i64, exit: CondP) -> Self {
        IfdP {
            maker,
            amount,
            price,
            tip: TIP,
            exit,
            min_fill: MIN_FILL,
            entry_stop: 0,
            band_daa: 0,
            keeper_tip: 0,
            armed: 0,
            rpt: 0,
            min_touch: TOK,
        }
    }
    /// Budget rate per whole token (price + tip): what a fill may cost at most, what a booked exit's rptPrice is.
    fn rate(&self) -> i64 {
        self.price + self.tip
    }
}
/// Escrow of an if-done entry for `amount` base units: the budget at its limit (rounded up) plus a delivery
/// and an exit carrier for every whole token (the fills of the fixtures are whole tokens or fewer).
fn ifd_value(ip: &IfdP) -> i64 {
    q_up(ip.amount, ip.rate()) + ((ip.amount + TOK - 1) / TOK) * (DC + EC)
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
    /// prefix and suffix length), as in contracts/v2/KobCondAsk.ctor.json.
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
            int(p.scale),
            int(p.min_fill),
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
            int(p.amount),
            bytes(&EXT),
        ]);
        compile_contract(&src("KobAsk"), &a, CompileOptions::default()).expect("compile KobAsk")
    }
    fn bid(&self, p: &BidP) -> SilAbiArtifact {
        let mut a = vec![bytes(&p.maker)];
        a.extend(self.tok_args_of(p.token));
        a.extend([
            bytes(&EXT),
            int(p.scale),
            int(p.min_fill),
            int(p.price),
            int(p.tip),
            int(p.tif),
            int(p.active_from),
            int(p.expiry),
            int(REFUND_TIP),
            int(0),
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
            int(SCALE),
            int(p.min_fill),
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
            int(p.min_touch),
            int(p.min_rest),
            int(p.armed),
            int(p.band_daa),
            int(p.keeper_tip),
            int(p.amount),
            bytes(&p.parent),
            int(p.rpt_price),
            int(p.rpt_until),
            bytes(&EXT),
        ]);
        compile_contract(&src("KobCondAsk"), &a, CompileOptions::default()).expect("compile KobCondAsk")
    }
    /// Encoded KobCondAsk state of an exit order up to its amountLeft (what KobIfdBid commits to;
    /// the entry appends the repeat fields itself).
    fn cond_state(&self, p: &CondP) -> Vec<u8> {
        let a = self.cond(p);
        let l = state_layout(&a);
        bytecode(&a)[l.start..l.start + COND_COMMIT_LEN].to_vec()
    }
    fn ifd(&self, p: &IfdP) -> SilAbiArtifact {
        let mut a = self.tpl_consts(&self.cond_tpl);
        a.extend(self.tpl_consts(&self.bid_tpl));
        a.push(bytes(&p.maker));
        a.extend(self.tok_args());
        a.extend([
            bytes(&EXT),
            int(SCALE),
            int(p.amount),
            int(p.price),
            int(p.tip),
            int(0),
            int(EXPIRY),
            int(REFUND_TIP),
            int(DC),
            int(EC),
            int(p.min_fill),
            int(p.entry_stop),
            int(p.band_daa),
            int(p.min_touch),
            int(600),
            int(p.keeper_tip),
            int(p.armed),
            int(p.rpt),
            bytes(&self.cond_state(&p.exit)),
        ]);
        compile_contract(&src("KobIfdBid"), &a, CompileOptions::default()).expect("compile KobIfdBid")
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
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([(k as u8).wrapping_add(1); 32]), index: k as u32 },
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
    // (storage mass divides by every output value: undefined for the zero-value outputs of some fixtures)
    let storage = if tx.outputs.iter().any(|o| o.value == 0) {
        u64::MAX
    } else {
        mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX)
    };
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

const P250: i64 = 250_000_000; // 2.50 KAS per whole token
const P255: i64 = 255_000_000;
const P260: i64 = 260_000_000;
const P245: i64 = 245_000_000;

/// S1: a taker (who is also the matcher of its own tx) buys n of the ask's 10 whole tokens and pays
/// exactly its all-in price (so it keeps the ask's tip).
fn s1(f: &Fx) -> Scn {
    s1_with(f, &AskP::new(pk(&f.maker_a), P250), 4 * TOK)
}
fn s1_with(f: &Fx, p: &AskP, n: i64) -> Scn {
    s1_at(f, p, n, p.price, 0)
}
/// As s1_with, with the effective quote `eff` (decay) and the decay time argument `t`. A GTC ask whose
/// whole amount is taken sells out (no continuation; both carriers go to the maker).
fn s1_at(f: &Fx, p: &AskP, n: i64, eff: i64, t: i64) -> Scn {
    let t_ = &f.net.t;
    let a = cov(0xa1);
    let ask = f.net.ask(p);
    let taker = pk(&f.taker);
    let pay = if eff >= p.tip { ask_all_in(n, eff, p.tip) } else { 0 };
    let rest = p.amount - n;
    let name = format!("S1 taker buys {n} of {} base units from 1 ask quoting {}", p.amount, p.price);
    if rest <= 0 {
        return Scn {
            name,
            inputs: vec![
                call(&ask, "settle", vec![nb(n), iv(1), iv(0), iv(t)], "ask.settle", CARRIER, a, 1_000),
                tok_in(
                    t_,
                    CARRIER,
                    p.amount,
                    &a.as_bytes(),
                    SCHEME_COVID,
                    TOKEN_COV,
                    Some(vec![tok_state(n, &taker, SCHEME_P2PK)]),
                    Wit::CovId,
                    1_000,
                ),
                p2pk_in(&f.taker, 1000 * KAS),
            ],
            outputs: vec![
                out(pay + 2 * CARRIER, p2pk_spk(&p.maker), None),
                out(CARRIER, t_.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
                out(1000 * KAS - pay - CARRIER - NET_FEE, p2pk_spk(&taker), None),
            ],
            lock_time: NOW,
            payload: vec![],
        };
    }
    Scn {
        name,
        inputs: vec![
            call(&ask, "settle", vec![nb(n), iv(1), iv(2), iv(t)], "ask.settle", CARRIER, a, 1_000),
            tok_in(
                t_,
                CARRIER,
                p.amount,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(rest, &a.as_bytes(), SCHEME_COVID), tok_state(n, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay, p2pk_spk(&p.maker), None),
            out(CARRIER, spk_of(&f.net.ask(&AskP { amount: rest, ..p.clone() })), Some((0, a))),
            out(CARRIER, t_.spk(rest, &a.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))),
            out(CARRIER, t_.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - pay - CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S2: a matcher with no capital crosses 1 bid (quote 2.60, 8 whole tokens) with 2 asks (A 2.50 full 5
/// tokens, C 2.55 partial 3 of 10), every order at its own all-in price. The matcher keeps the
/// crossing spread plus the tips.
fn s2(f: &Fx) -> Scn {
    s2_tips(f, TIP)
}
fn s2_tips(f: &Fx, tip: i64) -> Scn {
    let t = &f.net.t;
    let (b, ca, cc) = (cov(0xb1), cov(0xa1), cov(0xa3));
    let mut bp = BidP::new(pk(&f.maker_b), P260);
    bp.tip = tip;
    let mut ap = AskP::new(pk(&f.maker_a), P250);
    ap.tip = tip;
    ap.amount = 5 * TOK;
    let mut cp = AskP::new(pk(&f.maker_c), P255);
    cp.tip = tip;
    let bid = f.net.bid(&bp);
    let ask_a = f.net.ask(&ap);
    let ask_c = f.net.ask(&cp);
    let bid_value = bp.funded(8 * TOK);
    let spend = bid_all_in(8 * TOK, P260, tip);
    let (pay_a, pay_c) = (ask_all_in(5 * TOK, P250, tip), ask_all_in(3 * TOK, P255, tip));
    Scn {
        name: format!("S2 matcher crosses 1 bid x 2 asks, each at its own all-in price (tips {tip})"),
        inputs: vec![
            call(&bid, "fill", vec![nb(8 * TOK), iv(3), iv(0)], "bid.fill", bid_value, b, 1_000),
            call(&ask_a, "settle", vec![nb(5 * TOK), iv(3), iv(0), iv(0)], "ask.settle", CARRIER, ca, 1_000),
            call(&ask_c, "settle", vec![nb(3 * TOK), iv(4), iv(4), iv(0)], "ask.settle", CARRIER, cc, 1_000),
            tok_in(
                t,
                CARRIER,
                5 * TOK,
                &ca.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(8 * TOK, &pk(&f.maker_b), SCHEME_P2PK), tok_state(7 * TOK, &cc.as_bytes(), SCHEME_COVID)]),
                Wit::CovId,
                1_000,
            ),
            tok_in(t, CARRIER, 10 * TOK, &cc.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000),
        ],
        outputs: vec![
            out(bid_value - spend, t.spk(8 * TOK, &pk(&f.maker_b), SCHEME_P2PK), Some((3, TOKEN_COV))),
            out(pay_a + 2 * CARRIER, p2pk_spk(&pk(&f.maker_a)), None),
            out(pay_c, p2pk_spk(&pk(&f.maker_c)), None),
            out(CARRIER, spk_of(&f.net.ask(&AskP { amount: 7 * TOK, ..cp.clone() })), Some((2, cc))),
            out(CARRIER, t.spk(7 * TOK, &cc.as_bytes(), SCHEME_COVID), Some((3, TOKEN_COV))),
            out(spend - pay_a - pay_c - NET_FEE, p2pk_spk(&pk(&f.matcher)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S3: a taker sells 6 whole tokens into 2 bids: B1 (2.45) partial and continuing with exactly its
/// unused budget, B2 (2.50) exhausted.
fn s3(f: &Fx) -> Scn {
    let t = &f.net.t;
    let (b1, b2) = (cov(0xb1), cov(0xb2));
    let bp1 = BidP::new(pk(&f.maker_b), P245);
    let bp2 = BidP::new(pk(&f.maker_c), P250);
    let bid1 = f.net.bid(&bp1);
    let bid2 = f.net.bid(&bp2);
    let taker = pk(&f.taker);
    let (v1, v2) = (bp1.used(10 * TOK) + 3 * DC, bp2.funded(2 * TOK));
    let (u1, u2) = (bp1.used(4 * TOK), bp2.used(2 * TOK));
    Scn {
        name: "S3 taker sells into 2 bids (1 partial + 1 exhausted)".into(),
        inputs: vec![
            call(&bid1, "fill", vec![nb(4 * TOK), iv(2), iv(0)], "bid.fill", v1, b1, 1_000),
            call(&bid2, "fill", vec![nb(2 * TOK), iv(2), iv(0)], "bid.fill", v2, b2, 1_000),
            tok_in(
                t,
                CARRIER,
                6 * TOK,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(4 * TOK, &pk(&f.maker_b), SCHEME_P2PK), tok_state(2 * TOK, &pk(&f.maker_c), SCHEME_P2PK)]),
                Wit::P2pk(f.taker),
                1_000,
            ),
        ],
        outputs: vec![
            out(DC, t.spk(4 * TOK, &pk(&f.maker_b), SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(v2 - u2, t.spk(2 * TOK, &pk(&f.maker_c), SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(v1 - u1 - DC, spk_of(&bid1), Some((0, b1))),
            out(u1 + u2 + CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S4: IOC ask, 4 of 10 whole tokens filled; the other 6 go straight back to the maker.
fn s4(f: &Fx) -> Scn {
    let t = &f.net.t;
    let a = cov(0xa1);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    let ask = f.net.ask(&p);
    let taker = pk(&f.taker);
    let pay = ask_all_in(4 * TOK, P250, TIP);
    Scn {
        name: "S4 IOC (market with slippage bound) ask: 4 of 10 tokens filled, remainder returned".into(),
        inputs: vec![
            call(&ask, "settle", vec![nb(4 * TOK), iv(1), iv(1), iv(0)], "ask.settle", CARRIER, a, 1_000),
            tok_in(
                t,
                CARRIER,
                10 * TOK,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(6 * TOK, &p.maker, SCHEME_P2PK), tok_state(4 * TOK, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay + CARRIER, p2pk_spk(&p.maker), None),
            out(CARRIER, t.spk(6 * TOK, &p.maker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(CARRIER, t.spk(4 * TOK, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - pay - 2 * CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S5: FOK ask filled completely (10 of 10 whole tokens).
fn s5(f: &Fx) -> Scn {
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 2;
    with_name(s1_with(f, &p, 10 * TOK), "S5 FOK ask filled 10/10 tokens")
}

/// S6: bid variants: a taker sells n base units into one bid funded for `funded` base units.
/// tif 1 IOC / 2 FOK terminate; tif 0 with buying power for one more minimum fill continues (DCA and rising
/// bids too). `eff`: the quote the fill pays (rising bid), `t`: its time argument.
fn s6_with(f: &Fx, bp: &BidP, n: i64, funded: i64, eff: i64, t: i64) -> Scn {
    s6_value(f, bp, n, bp.funded(funded), eff, t)
}
/// As s6_with, with the bid's escrow value `v` itself.
fn s6_value(f: &Fx, bp: &BidP, n: i64, v: i64, eff: i64, t: i64) -> Scn {
    let tk = &f.net.t;
    let b = cov(0xb1);
    let bid = f.net.bid(bp);
    let taker = pk(&f.taker);
    let spend = quote_at(n, eff + bp.tip, bp.scale, false).expect("spend");
    let used = bp.used(n);
    let left = v - used;
    let cont = bp.tif == 0 && left - DC >= bp.used(bp.min_fill);
    let mut outputs = vec![];
    if cont {
        outputs.push(out(DC + used - spend, tk.spk(n, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))));
        outputs.push(out(left - DC, spk_of(&bid), Some((0, b))));
    } else {
        outputs.push(out(v - spend, tk.spk(n, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    }
    outputs.push(out(spend + CARRIER - NET_FEE, p2pk_spk(&taker), None));
    Scn {
        name: format!("S6 taker sells {n} base units into a bid (tif {}, escrow {v})", bp.tif),
        inputs: vec![
            call(&bid, "fill", vec![nb(n), iv(1), iv(t)], "bid.fill", v, b, 1_000),
            tok_in(
                tk,
                CARRIER,
                n,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(n, &bp.maker, SCHEME_P2PK)]),
                Wit::P2pk(f.taker),
                1_000,
            ),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
    }
}
fn s6(f: &Fx, tif: i64) -> Scn {
    let mut bp = BidP::new(pk(&f.maker_b), P245);
    bp.tif = tif;
    // FOK: funded for exactly the 4 whole tokens
    s6_with(f, &bp, 4 * TOK, if tif == 2 { 4 * TOK } else { 10 * TOK }, P245, 0)
}

/// S7: maker cancels an ask (SIGHASH_ALL), tokens back to the maker.
fn s7(f: &Fx, sighash: u8, signer: Keypair) -> Scn {
    let t = &f.net.t;
    let a = cov(0xa1);
    let m = pk(&f.maker_a);
    let ask = f.net.ask(&AskP::new(m, P250));
    Scn {
        name: format!("S7 maker cancels ask (sighash {sighash:#04x})"),
        inputs: vec![
            call(&ask, "cancel", vec![Arg::Sig(signer, sighash)], "ask.cancel", CARRIER, a, 1_000),
            tok_in(
                t,
                CARRIER,
                10 * TOK,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(10 * TOK, &m, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
        ],
        outputs: vec![
            out(CARRIER, t.spk(10 * TOK, &m, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(CARRIER - NET_FEE, p2pk_spk(&m), None),
        ],
        lock_time: 0,
        payload: vec![],
    }
}
/// S8: anyone refunds an ask at its soft expiry (or, with expiry disabled, after 90 days idle).
fn s8(f: &Fx, expiry: i64, lock_time: u64) -> Scn {
    let t = &f.net.t;
    let a = cov(0xa1);
    let m = pk(&f.maker_a);
    let mut p = AskP::new(m, P250);
    p.expiry = expiry;
    let ask = f.net.ask(&p);
    Scn {
        name: format!("S8 keeper refunds ask (expiry {expiry}, lockTime {lock_time})"),
        inputs: vec![
            call(&ask, "settle", vec![nb(0), iv(1), iv(0), iv(0)], "ask.refund", CARRIER, a, 1_000),
            tok_in(
                t,
                CARRIER,
                10 * TOK,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(10 * TOK, &m, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
        ],
        outputs: vec![out(2 * CARRIER - REFUND_TIP, t.spk(10 * TOK, &m, SCHEME_P2PK), Some((1, TOKEN_COV)))],
        lock_time,
        payload: vec![],
    }
}
/// S9: bid cancel (entry "cancel") or refund (entry "refund").
fn s9(f: &Fx, entry: &'static str, expiry: i64, lock_time: u64) -> Scn {
    let b = cov(0xb1);
    let m = pk(&f.maker_b);
    let mut bp = BidP::new(m, P245);
    bp.expiry = expiry;
    let bid = f.net.bid(&bp);
    let args = if entry == "cancel" { vec![Arg::Sig(f.maker_b, 0x01)] } else { vec![] };
    Scn {
        name: format!("S9 bid {entry} (expiry {expiry}, lockTime {lock_time})"),
        inputs: vec![call(&bid, entry, args, if entry == "cancel" { "bid.cancel" } else { "bid.refund" }, 25 * KAS, b, 1_000)],
        outputs: vec![out(25 * KAS - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time,
        payload: vec![],
    }
}

/// The sigscript of `entry(args)` of `art` with its FIRST argument push replaced by `first` (raw bytes):
/// hand-assembled non-minimal pushes (covenant sigscripts need not push minimally).
fn ss_with_first_push(art: &SilAbiArtifact, entry: &str, args: &[ArtifactValue], first: &[u8]) -> Vec<u8> {
    let ss = entry_ss(art, entry, args);
    let len = match ss[0] {
        0x00 | 0x4f | 0x51..=0x60 => 1,
        b @ 0x01..=0x4b => 1 + b as usize,
        0x4c => 2 + ss[1] as usize,
        b => panic!("unexpected first opcode {b:#04x}"),
    };
    [first.to_vec(), ss[len..].to_vec()].concat()
}
/// An 8-byte push (0x08 || v as an 8-byte script number, sign-magnitude little endian).
fn push8(v: i64) -> Vec<u8> {
    let mut b = v.unsigned_abs().to_le_bytes();
    if v < 0 {
        b[7] |= 0x80;
    }
    [vec![0x08], b.to_vec()].concat()
}
/// Turns input `idx` (a Call role without signatures) into a Raw input whose first argument push is `first`.
fn raw_first_push(s: &mut Scn, idx: usize, first: &[u8]) {
    let (ss, name) = match &s.inputs[idx].role {
        Role::Call { art, entry, args, name } => {
            let vals: Vec<ArtifactValue> = args
                .iter()
                .map(|a| match a {
                    Arg::V(v) => v.clone(),
                    _ => panic!("raw_first_push: signature arguments are not supported"),
                })
                .collect();
            (ss_with_first_push(art, entry, &vals, first), *name)
        }
        _ => panic!("raw_first_push: input {idx} is not a call"),
    };
    s.inputs[idx].role = Role::Raw { ss, name };
}
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
        role: Role::Raw { ss, name: "ev.forged" },
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
        role: Role::Raw { ss, name: "ev.not_p2sh" },
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
    let n = e.amount;
    let decay = e.slope != 0;
    let origin = if e.interval > 0 { e.active_from.max(e.daa as i64 + e.interval) } else { e.active_from };
    let t_arg = if decay { origin } else { 0 };
    let expiry = if matches!(e.mode, EvMode::Refund | EvMode::ZeroFill) { NOW as i64 } else { EXPIRY };
    let look = matches!(e.mode, EvMode::Forged | EvMode::NotP2sh);
    let owner = if e.mode == EvMode::Cancel { mc } else { matcher };
    if e.ask {
        let ap = AskP {
            amount: n,
            active_from: e.active_from,
            expiry,
            interval: e.interval,
            slope: e.slope,
            price_end: if decay { e.price / 2 } else { 0 },
            decay_step: if decay { 1 } else { 0 },
            token: e.token,
            scale: e.scale,
            ..AskP::new(mc, e.price)
        };
        let art = f.net.ask(&ap);
        let tk = ei as i64 + 1;
        let mut inp = match e.mode {
            EvMode::Fill => call(&art, "settle", vec![nb(n), iv(tk), iv(0), iv(t_arg)], "ev.ask.settle", CARRIER, e.cov, e.daa),
            EvMode::Refund | EvMode::ZeroFill => {
                call(&art, "settle", vec![nb(0), iv(tk), iv(0), iv(0)], "ev.ask.refund", CARRIER, e.cov, e.daa)
            }
            EvMode::Cancel => call(&art, "cancel", vec![Arg::SigPos(f.maker_c, 0x01)], "ev.ask.cancel", CARRIER, e.cov, e.daa),
            EvMode::Forged => forged_ev(&bytecode(&art), n, CARRIER, e.cov, e.daa),
            EvMode::NotP2sh => not_p2sh_ev(&bytecode(&art), n, CARRIER, e.cov, e.daa),
        };
        inp.seq = e.interval as u64;
        s.inputs.push(inp);
        if matches!(e.mode, EvMode::Refund | EvMode::ZeroFill) {
            // the unsold tokens go back to maker C at the ask's own index
            let st = vec![tok_state(n, &mc, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n, &e.cov.as_bytes(), SCHEME_COVID, e.token, Wit::CovId, e.tok_daa, st);
            s.outputs.push(out(2 * CARRIER, t.spk(n, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
        } else {
            let all_in = quote_at(n, e.price - TIP, e.scale, true).expect("evidence ask proceeds");
            let pay = if e.mode == EvMode::Fill { all_in + 2 * CARRIER } else { CARRIER };
            s.outputs.push(out(pay, p2pk_spk(if look { &matcher } else { &mc }), None));
            let st = vec![tok_state(n, &owner, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n, &e.cov.as_bytes(), SCHEME_COVID, e.token, Wit::CovId, e.tok_daa, st);
            s.outputs.push(out(CARRIER, t.spk(n, &owner, SCHEME_P2PK), Some((l as u16, e.token))));
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
            scale: e.scale,
            ..BidP::new(mc, e.price)
        };
        let art = f.net.bid(&bp);
        let v = bp.funded(n);
        let mut inp = match e.mode {
            EvMode::Fill => call(&art, "fill", vec![nb(n), iv(ei as i64 + 1), iv(t_arg)], "ev.bid.fill", v, e.cov, e.daa),
            EvMode::Refund => call(&art, "refund", vec![], "ev.bid.refund", v, e.cov, e.daa),
            EvMode::Cancel => call(&art, "cancel", vec![Arg::SigPos(f.maker_c, 0x01)], "ev.bid.cancel", v, e.cov, e.daa),
            EvMode::ZeroFill => call(&art, "fill", vec![nb(0), iv(ei as i64 + 1), iv(t_arg)], "ev.bid.fill0", v, e.cov, e.daa),
            EvMode::Forged => forged_ev(&bytecode(&art), n, v, e.cov, e.daa),
            EvMode::NotP2sh => not_p2sh_ev(&bytecode(&art), n, v, e.cov, e.daa),
        };
        inp.seq = e.interval as u64;
        s.inputs.push(inp);
        if e.mode == EvMode::Fill {
            // the bid is exhausted: everything but the all-in spend rides on its positional delivery
            let spend = quote_at(n, e.price + TIP, e.scale, false).expect("evidence bid spend");
            let st = vec![tok_state(n, &mc, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n, &matcher, SCHEME_P2PK, e.token, Wit::P2pk(f.matcher), 1_000, st);
            s.outputs.push(out(v - spend, t.spk(n, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
        } else {
            s.outputs.push(out(v, p2pk_spk(if look { &matcher } else { &mc }), None));
        }
    }
    let ins: i64 = s.inputs[in0..].iter().map(|i| i.entry.amount as i64).sum();
    let outs: i64 = s.outputs[out0..].iter().map(|o| o.value as i64).sum();
    s.outputs.push(out(ins - outs, p2pk_spk(&matcher), None));
    (ei as i64, if e.ask { ei as i64 + 1 } else { -1 })
}

/// Conditional-ask fill: cond at input 0 (cov 0xc1, UTXO DAA 2000) sells n of its amount on `leg` to a taker.
/// With `ev`, the evidence fill (see add_ev) follows every other input and output (inputs 3.., outputs 5..)
/// and the settle reads it (ev, tk).
fn cond_fill(f: &Fx, cp: &CondP, n: i64, leg: i64, next_cp: &CondP, ev: Option<&Ev>) -> Scn {
    let leg_price = if leg == 0 { cp.tp } else { stop_leg(cp.stop, cp.slip_bps) };
    cond_fill_at(f, cp, n, leg, next_cp, ev, 0, leg_price)
}
/// As cond_fill, with the auction time argument `t` and the leg price the fill pays (a leg price below the
/// tip pays nothing).
#[allow(clippy::too_many_arguments)]
fn cond_fill_at(f: &Fx, cp: &CondP, n: i64, leg: i64, next_cp: &CondP, ev: Option<&Ev>, t_arg: i64, leg_price: i64) -> Scn {
    let t = &f.net.t;
    let c = cov(0xc1);
    let cond = f.net.cond(cp);
    let next = f.net.cond(&CondP { amount: next_cp.amount - n, ..next_cp.clone() });
    let taker = pk(&f.taker);
    let pay = if leg_price >= cp.tip { ask_all_in(n, leg_price, cp.tip) } else { 0 };
    let rest = cp.amount - n;
    let mut s = Scn {
        name: "cond fill".into(),
        inputs: vec![
            call(&cond, "settle", vec![nb(n), iv(1), iv(2), iv(leg), iv(0), iv(0), iv(t_arg)], "cond.settle", CARRIER, c, 2_000),
            tok_in(
                t,
                CARRIER,
                cp.amount,
                &c.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(rest, &c.as_bytes(), SCHEME_COVID), tok_state(n, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                2_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay, p2pk_spk(&cp.maker), None),
            out(CARRIER, spk_of(&next), Some((0, c))),
            out(CARRIER, t.spk(rest, &c.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    s.outputs.push(out(CARRIER, t.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    s.outputs.push(out(1000 * KAS - pay - CARRIER - NET_FEE, p2pk_spk(&taker), None));
    if let Some(e) = ev {
        let (ei, tk) = add_ev(f, &mut s, e);
        set_arg(&mut s, 0, 4, iv(ei));
        set_arg(&mut s, 0, 5, iv(tk));
    }
    s
}
/// Down evidence: a resting ask quoting 1.98 (<= stop 2.00) sold out (5 whole tokens), exposed since DAA 1000.
fn down_ev() -> Ev {
    Ev::ask(198_000_000)
}
/// Up evidence: a resting bid quoting 2.20 filled with 5 whole tokens (trails a 2.00 stop by two 0.05 steps
/// with the 0.10 gap).
fn up_ev() -> Ev {
    Ev::bid(220_000_000)
}
fn armed(cp: &CondP) -> CondP {
    let mut c = cp.clone();
    c.armed = 1;
    c
}
/// Permissionless update (arm or trail) of cond (cov 0xc1, UTXO DAA 2000) next to the evidence fill `ev`
/// (inputs 2.., see add_ev); a keeper (input 1) runs it.
fn cond_update(f: &Fx, cp: &CondP, next_cp: &CondP, ev: &Ev, seq: u64) -> Scn {
    cond_update_tip(f, cp, next_cp, ev, seq, 0)
}
/// As cond_update; the keeper takes `take` sompi from the order's carrier.
fn cond_update_tip(f: &Fx, cp: &CondP, next_cp: &CondP, ev: &Ev, seq: u64, take: i64) -> Scn {
    let c = cov(0xc1);
    let keeper = keypair();
    let mut cin = call(&f.net.cond(cp), "update", vec![iv(0), iv(0)], "cond.update", CARRIER, c, 2_000);
    cin.seq = seq;
    let mut s = Scn {
        name: "cond update".into(),
        inputs: vec![cin, p2pk_in(&keeper, 10 * KAS)],
        outputs: vec![
            out(CARRIER - take, spk_of(&f.net.cond(next_cp)), Some((0, c))),
            out(10 * KAS + take - NET_FEE, p2pk_spk(&pk(&keeper)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    let (ei, tk) = add_ev(f, &mut s, ev);
    set_arg(&mut s, 0, 0, iv(ei));
    set_arg(&mut s, 0, 1, iv(tk));
    s
}
fn trailing(maker: [u8; 32]) -> CondP {
    let mut c = CondP::oco(maker);
    c.step = 5_000_000;
    c.gap = 10_000_000;
    c.wait = 600;
    c
}

/// The committed exit of the if-done fixtures: OCO with TP 3.00 / stop 2.20 (3% band).
fn ifd_exit(maker: [u8; 32]) -> CondP {
    let mut c = CondP::oco(maker);
    c.stop = 220_000_000;
    c
}
fn outpoint_of(k: usize) -> TransactionOutpoint {
    TransactionOutpoint { transaction_id: TransactionId::from_bytes([(k as u8).wrapping_add(1); 32]), index: k as u32 }
}
/// Genesis covenant id of outputs `outs` authorised by input k, exactly as consensus derives it.
fn genesis_id(s: &Scn, k: usize, outs: &[usize]) -> Hash {
    covenant_id(outpoint_of(k), outs.iter().map(|&i| (i as u32, &s.outputs[i])))
}
/// Knobs of one if-done entry fill (single-lever mutations for the attack tests).
#[derive(Clone)]
struct IfdKnobs {
    amount_left: i64,
    n: i64,
    deliver: i64,
    exit: Option<CondP>,
    to_maker: bool,
    double_exit: bool,
    self_id_exit: bool,
    foreign_exit: bool,
    cont_amount: Option<i64>,
    cont_delta: i64,
    exit_delta: i64,
    ask_template: bool,
    /// stop entry: trigger price, auction length, armed state, minimum fill (base units)
    entry_stop: i64,
    band_daa: i64,
    armed: i64,
    min_fill: i64,
    /// evidence read by an unarmed stop entry (added after every other input and output) and the
    /// entry's minTouch
    ev: Option<Ev>,
    min_touch: i64,
    /// auction time argument and the quote the fill pays (default: the limit)
    t: i64,
    eff: Option<i64>,
    /// armed state written into the continuation (default: unchanged)
    next_armed: Option<i64>,
    /// repeat: the entry's rptAmount, the continuation's (default: booked = rpt - n), the exit's
    /// repeat fields (default: as the entry must write them), and a forced termination
    rpt: i64,
    next_rpt: Option<i64>,
    exit_rpt: Option<([u8; 32], i64, i64)>,
    terminate: bool,
    /// the entry's limit, tip and committed exit (default: 2.60, TIP, ifd_exit), and its escrow (default: ifd_value)
    price: i64,
    tip: i64,
    exit_terms: Option<CondP>,
    value: Option<i64>,
}
impl IfdKnobs {
    fn new(amount_left: i64, n: i64) -> Self {
        IfdKnobs {
            amount_left,
            n,
            deliver: n,
            exit: None,
            to_maker: false,
            double_exit: false,
            self_id_exit: false,
            foreign_exit: false,
            cont_amount: None,
            cont_delta: 0,
            exit_delta: 0,
            ask_template: false,
            entry_stop: 0,
            band_daa: 0,
            armed: 0,
            min_fill: MIN_FILL,
            ev: None,
            min_touch: TOK,
            t: 0,
            eff: None,
            next_armed: None,
            rpt: 0,
            next_rpt: None,
            exit_rpt: None,
            terminate: false,
            price: P260,
            tip: TIP,
            exit_terms: None,
            value: None,
        }
    }
}
/// A taker sells n base units into an if-done bid (quote 2.60, cov 0xd1) with amountLeft base units. Outputs:
/// [0] tokens into the exit's custody, [1] entry continuation (if an amount remains), then the exit (a
/// fresh genesis covenant authorised by the entry), then the taker's KAS.
fn ifd_fill(f: &Fx, k: &IfdKnobs) -> Scn {
    let t = &f.net.t;
    let i = cov(0xd1);
    let foreign = cov(0x99);
    let m = pk(&f.maker_a);
    let mut ip = IfdP::limit(m, k.amount_left, k.price, k.exit_terms.clone().unwrap_or(ifd_exit(m)));
    ip.tip = k.tip;
    ip.entry_stop = k.entry_stop;
    ip.band_daa = k.band_daa;
    ip.armed = k.armed;
    ip.min_fill = k.min_fill;
    ip.rpt = k.rpt;
    ip.min_touch = k.min_touch;
    let ifd = f.net.ifd(&ip);
    let booked = k.rpt > k.n;
    let mut exit_cp = CondP { amount: k.n, ..k.exit.clone().unwrap_or(ip.exit.clone()) };
    if booked {
        exit_cp.parent = i.as_bytes();
        exit_cp.rpt_price = ip.rate();
        exit_cp.rpt_until = EXPIRY.min(1_000 + MAX_IDLE);
    }
    if let Some((p, l, u)) = k.exit_rpt {
        exit_cp.parent = p;
        exit_cp.rpt_price = l;
        exit_cp.rpt_until = u;
    }
    let exit_art = f.net.cond(&exit_cp);
    let taker = pk(&f.taker);
    // a repeating entry also holds its own carrier (it outlives its last fill)
    let v = k.value.unwrap_or_else(|| ifd_value(&ip) + if k.rpt > 0 { EC } else { 0 });
    let spend = bid_all_in(k.n, k.eff.unwrap_or(k.price), k.tip);
    let rest = k.amount_left - k.n;
    let (pre, suf) =
        if k.ask_template { (&f.net.ask_tpl.pre, &f.net.ask_tpl.suf) } else { (&f.net.cond_tpl.pre, &f.net.cond_tpl.suf) };
    let mut s = Scn {
        name: "ifd fill".into(),
        inputs: vec![
            call(
                &ifd,
                "fill",
                vec![nb(k.n), iv(1), iv(0), Arg::V(bytes(pre)), Arg::V(bytes(suf)), iv(0), iv(k.t)],
                "ifd.fill",
                v,
                i,
                1_000,
            ),
            tok_in(t, CARRIER, k.deliver, &taker, SCHEME_P2PK, TOKEN_COV, Some(vec![]), Wit::P2pk(f.taker), 1_000),
        ],
        outputs: vec![out(DC, p2pk_spk(&taker), Some((1, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
    };
    let mut taker_kas = spend + CARRIER - NET_FEE;
    let cont_exists = (rest > 0 || k.rpt > 0) && !k.terminate;
    let exit_value = if cont_exists {
        let next_rpt = k.next_rpt.unwrap_or(if booked { k.rpt - k.n } else { k.rpt });
        let cont = f.net.ifd(&IfdP {
            amount: k.cont_amount.unwrap_or(rest),
            armed: k.next_armed.unwrap_or(ip.armed),
            rpt: next_rpt,
            ..ip.clone()
        });
        s.outputs.push(out(v - spend - DC - EC + k.cont_delta, spk_of(&cont), Some((0, i))));
        taker_kas -= k.cont_delta;
        EC + k.exit_delta
    } else {
        v - spend - DC + k.exit_delta
    };
    taker_kas -= k.exit_delta;
    let exit_idx = s.outputs.len();
    s.outputs.push(out(exit_value, spk_of(&exit_art), None));
    set_arg(&mut s, 0, 2, iv(exit_idx as i64));
    let mut group = vec![exit_idx];
    if k.double_exit {
        s.outputs.push(out(EC, spk_of(&exit_art), None));
        group.push(exit_idx + 1);
        s.inputs.push(p2pk_in(&f.taker, EC));
    }
    s.outputs.push(out(taker_kas, p2pk_spk(&taker), None));
    let exit_id = if k.self_id_exit {
        s.outputs[exit_idx].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: i });
        i
    } else if k.foreign_exit {
        // the attacker spends its own covenant UTXO (id 0x99, OpTrue script) and continues it as
        // the "exit": a covenant id it keeps controlling through its other UTXOs
        s.inputs.push(Inp {
            entry: utxo(EC, ScriptPublicKey::new(0, vec![kaspa_txscript::opcodes::codes::OpTrue].into()), foreign, 1_000),
            role: Role::Raw { ss: vec![], name: "attacker.cov" },
            seq: 0,
        });
        let a = (s.inputs.len() - 1) as u16;
        s.outputs[exit_idx].covenant = Some(CovenantBinding { authorizing_input: a, covenant_id: foreign });
        foreign
    } else {
        let id = genesis_id(&s, 0, &group);
        for &g in &group {
            s.outputs[g].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: id });
        }
        id
    };
    let (owner, scheme) = if k.to_maker { (m, SCHEME_P2PK) } else { (exit_id.as_bytes(), SCHEME_COVID) };
    s.outputs[0].script_public_key = t.spk(k.deliver, &owner, scheme);
    set_leader_next(&mut s, 1, vec![tok_state(k.deliver, &owner, scheme)]);
    if let Some(ev) = &k.ev {
        let (ei, _) = add_ev(f, &mut s, ev);
        set_arg(&mut s, 0, 5, iv(ei));
    }
    s
}
fn s14(f: &Fx) -> Scn {
    with_name(
        ifd_fill(f, &IfdKnobs::new(10 * TOK, 4 * TOK)),
        "S14 IFO entry partial 4/10 tokens: a fresh OCO exit holds 4 tokens, the entry continues with 6",
    )
}

/// B1 batch: one matcher batch on a token issued with the 8/8 slot limit: 3 bids (12 whole tokens, all
/// exhausted) x 5 asks (4 full, 1 partial 4 of 10), every order at its own all-in price, plus a stop
/// (stop 2.50) armed by update reading the 2.50 ask's fill as its evidence (touch: the arm costs one input
/// and one output, no receipt). The matcher adds its own funding input so its income does not sit on a tiny
/// UTXO. Returns (scenario, matcher gross income in sompi).
fn b1(f: &Fx, tip: i64) -> (Scn, i64) {
    let t = &f.net.t;
    let makers = [keypair(), keypair(), keypair(), keypair(), keypair()];
    let bid_ps = [(P260, 4 * TOK), (258_000_000, 3 * TOK), (256_000_000, 5 * TOK)];
    let ask_ps = [
        (P250, 2 * TOK, 2 * TOK),
        (252_000_000, 2 * TOK, 2 * TOK),
        (253_000_000, 2 * TOK, 2 * TOK),
        (P255, 2 * TOK, 2 * TOK),
        (256_000_000, 10 * TOK, 4 * TOK),
    ];
    let bid_makers = [pk(&f.maker_a), pk(&f.maker_b), pk(&f.maker_c)];
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut spend_total = 0;
    let mut pay_total = 0;
    for (i, (price, amount)) in bid_ps.iter().enumerate() {
        let mut bp = BidP::new(bid_makers[i], *price);
        bp.tip = tip;
        let bid = f.net.bid(&bp);
        let v = bp.funded(*amount);
        let c = bid_all_in(*amount, *price, tip);
        spend_total += c;
        inputs.push(call(&bid, "fill", vec![nb(*amount), iv(8), iv(0)], "bid.fill", v, cov(0xb1 + i as u8), 1_000));
        outputs.push(out(v - c, t.spk(*amount, &bid_makers[i], SCHEME_P2PK), Some((8, TOKEN_COV))));
    }
    let mut toks = vec![];
    let mut ask5 = None;
    for (i, (price, amount, sold)) in ask_ps.iter().enumerate() {
        let c = cov(0xa1 + i as u8);
        let mut ap = AskP::new(pk(&makers[i]), *price);
        ap.tip = tip;
        ap.amount = *amount;
        let ask = f.net.ask(&ap);
        let pay = ask_all_in(*sold, *price, tip);
        pay_total += pay;
        let partial = sold < amount;
        inputs.push(call(
            &ask,
            "settle",
            vec![nb(*sold), iv(8 + i as i64), iv(if partial { 9 } else { 0 }), iv(0)],
            "ask.settle",
            CARRIER,
            c,
            1_000 + i as u64,
        ));
        outputs.push(out(if partial { pay } else { pay + 2 * CARRIER }, p2pk_spk(&pk(&makers[i])), None));
        toks.push((c, *amount));
        if partial {
            ask5 = Some(f.net.ask(&AskP { amount: amount - sold, ..ap.clone() }));
        }
    }
    let a5 = cov(0xa5);
    outputs.push(out(CARRIER, spk_of(&ask5.unwrap()), Some((7, a5))));
    outputs.push(out(CARRIER, t.spk(6 * TOK, &a5.as_bytes(), SCHEME_COVID), Some((8, TOKEN_COV))));
    let next = vec![
        tok_state(4 * TOK, &bid_makers[0], SCHEME_P2PK),
        tok_state(3 * TOK, &bid_makers[1], SCHEME_P2PK),
        tok_state(5 * TOK, &bid_makers[2], SCHEME_P2PK),
        tok_state(6 * TOK, &a5.as_bytes(), SCHEME_COVID),
    ];
    for (i, (c, amount)) in toks.iter().enumerate() {
        inputs.push(tok_in(
            t,
            CARRIER,
            *amount,
            &c.as_bytes(),
            SCHEME_COVID,
            TOKEN_COV,
            if i == 0 { Some(next.clone()) } else { None },
            Wit::CovId,
            1_000 + i as u64,
        ));
    }
    let owner = pk(&f.matcher);
    // the ask at input 3 (2.50, sold out) with its custody at input 8 arms a stop at 2.50 (input 13)
    let stop = CondP { stop: P250, ..CondP::oco(pk(&f.maker_a)) };
    let c = cov(0xc1);
    inputs.push(call(&f.net.cond(&stop), "update", vec![iv(3), iv(8)], "cond.update", CARRIER, c, 2_000));
    outputs.push(out(CARRIER, spk_of(&f.net.cond(&armed(&stop))), Some((13, c))));
    inputs.push(p2pk_in(&f.matcher, 10 * KAS));
    let income = spend_total - pay_total;
    outputs.push(out(10 * KAS + income - NET_FEE, p2pk_spk(&owner), None));
    (
        Scn {
            name: format!("B1 batch: 3 bids x 5 asks at own prices + a stop armed by one of the fills (8/8 token, tips {tip})"),
            inputs,
            outputs,
            lock_time: NOW,
            payload: vec![],
        },
        income,
    )
}

/// TWAP ask: at most 2 whole tokens per fill, fills at least 600 DAA apart (CSV on the order UTXO).
fn twap_ask(f: &Fx) -> AskP {
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.interval = 600;
    p.max_fill = 2 * TOK;
    p
}
/// DCA bid: at most 1 whole token per fill, fills at least 600 DAA apart.
fn dca_bid(f: &Fx) -> BidP {
    let mut p = BidP::new(pk(&f.maker_b), P245);
    p.interval = 600;
    p.max_fill = TOK;
    p
}
/// Dutch ask: 3.00 falling 0.01 per 1000 DAA from DAA 1000, floor 2.00.
fn dutch_ask(f: &Fx) -> AskP {
    let mut p = AskP::new(pk(&f.maker_a), 300_000_000);
    p.slope = 1_000_000;
    p.price_end = 200_000_000;
    p.active_from = 1_000;
    p.decay_step = 1_000;
    p
}
/// Rising bid: 2.00 rising 0.01 per 1000 DAA from DAA 1000, cap 2.60.
fn rising_bid(f: &Fx) -> BidP {
    let mut p = BidP::new(pk(&f.maker_b), 200_000_000);
    p.slope = 1_000_000;
    p.price_end = P260;
    p.active_from = 1_000;
    p.decay_step = 1_000;
    p
}
fn with_seq(mut s: Scn, idx: usize, seq: u64) -> Scn {
    s.inputs[idx].seq = seq;
    s
}

// ---------------------------------------------------------------- lifecycle fixtures

/// Linear decay with a step of `step` DAA from `origin` (mirrors KobAsk / KobBid).
fn decay_at(start: i64, end: i64, slope: i64, step: i64, origin: i64, t: i64) -> i64 {
    (start - slope * ((t - origin) / step)).max(end)
}
fn rise_at(start: i64, end: i64, slope: i64, step: i64, origin: i64, t: i64) -> i64 {
    (start + slope * ((t - origin) / step)).min(end)
}
/// Stop-leg price of a conditional sell after `e` DAA of a `band` DAA auction.
fn stop_auction(stop: i64, slip_bps: i64, band: i64, e: i64) -> i64 {
    let bps = if e < band { slip_bps * e / band } else { slip_bps };
    stop - stop * bps / 10_000
}
/// Market sell = IOC auction: from the touch 2.50 down to its 3% slippage bound 2.425 over 200
/// DAA (one step per DAA), placed at NOW - 100.
fn market_ask(f: &Fx) -> AskP {
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    p.price_end = 242_500_000;
    p.slope = (P250 - p.price_end) / 200;
    p.decay_step = 1;
    p.active_from = NOW as i64 - 100;
    p
}
/// Market buy = IOC rising auction from the touch 2.50 up to its 3% bound 2.575 over 200 DAA.
fn market_bid(f: &Fx) -> BidP {
    let mut p = BidP::new(pk(&f.maker_b), P250);
    p.tif = 1;
    p.price_end = 257_500_000;
    p.slope = (p.price_end - P250) / 200;
    p.decay_step = 1;
    p.active_from = NOW as i64 - 100;
    p
}
/// IOC ask (tif 1) sells n of its amount at quote `eff` with decay time t; the rest goes back to
/// the maker (S4 shape).
fn ioc_fill(f: &Fx, p: &AskP, n: i64, eff: i64, t: i64) -> Scn {
    let tk = &f.net.t;
    let a = cov(0xa1);
    let ask = f.net.ask(p);
    let taker = pk(&f.taker);
    let pay = ask_all_in(n, eff, p.tip);
    let rest = p.amount - n;
    Scn {
        name: "ioc fill".into(),
        inputs: vec![
            call(&ask, "settle", vec![nb(n), iv(1), iv(1), iv(t)], "ask.settle", CARRIER, a, 1_000),
            tok_in(
                tk,
                CARRIER,
                p.amount,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(rest, &p.maker, SCHEME_P2PK), tok_state(n, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay + CARRIER, p2pk_spk(&p.maker), None),
            out(CARRIER, tk.spk(rest, &p.maker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(CARRIER, tk.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - pay - 2 * CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}
/// Refund of an ask with the given tif / activeFrom (order UTXO DAA 1000, expiry far).
fn ask_refund(f: &Fx, tif: i64, active_from: i64, lock_time: u64) -> Scn {
    let mut s = s8(f, NO_EXPIRY, lock_time);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.expiry = NO_EXPIRY;
    p.tif = tif;
    p.active_from = active_from;
    let ask = f.net.ask(&p);
    s.inputs[0] = call(&ask, "settle", vec![nb(0), iv(1), iv(0), iv(0)], "ask.refund", CARRIER, cov(0xa1), 1_000);
    s
}
/// Refund of a bid with the given tif (UTXO DAA 1000, expiry far).
fn bid_refund(f: &Fx, tif: i64, lock_time: u64) -> Scn {
    let mut s = s9(f, "refund", NO_EXPIRY, lock_time);
    let mut bp = BidP::new(pk(&f.maker_b), P245);
    bp.expiry = NO_EXPIRY;
    bp.tif = tif;
    s.inputs[0] = call(&f.net.bid(&bp), "refund", vec![], "bid.refund", 25 * KAS, cov(0xb1), 1_000);
    s
}
/// OCO with a 300-DAA stop auction (stop 2.00, band 3%).
fn auction_oco(maker: [u8; 32]) -> CondP {
    let mut c = CondP::oco(maker);
    c.band_daa = 300;
    c
}
fn with_armed(cp: &CondP, armed: i64) -> CondP {
    let mut c = cp.clone();
    c.armed = armed;
    c
}
/// Permissionless arm (update) of an if-done entry (cov 0xd1, UTXO DAA 1000) next to the evidence
/// fill `ev` (inputs 2.., see add_ev); the keeper takes `take` sompi from the escrow.
fn ifd_update(f: &Fx, ip: &IfdP, next: &IfdP, ev: &Ev, take: i64) -> Scn {
    let i = cov(0xd1);
    let v = ifd_value(ip);
    let keeper = keypair();
    let mut s = Scn {
        name: "ifd update".into(),
        inputs: vec![call(&f.net.ifd(ip), "update", vec![iv(0)], "ifd.update", v, i, 1_000), p2pk_in(&keeper, 10 * KAS)],
        outputs: vec![
            out(v - take, spk_of(&f.net.ifd(next)), Some((0, i))),
            out(10 * KAS + take - NET_FEE, p2pk_spk(&pk(&keeper)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    let (ei, _) = add_ev(f, &mut s, ev);
    set_arg(&mut s, 0, 0, iv(ei));
    s
}
/// A buy-stop IFO entry: stop 2.20 (limit 2.60), armed by the fill of a resting bid quoting >= 2.20.
fn stop_entry(k: IfdKnobs) -> IfdKnobs {
    IfdKnobs { entry_stop: 220_000_000, ..k }
}

// ---------------------------------------------------------------- tests

#[test]
fn v2_sizes_layouts_sigops() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let arts = [
        ("KobAsk", n.ask(&AskP::new(m, P250)), 276),
        ("KobBid", n.bid(&BidP::new(m, P250)), 285),
        ("KobCondAsk", n.cond(&CondP::oco(m)), 363),
        ("KobIfdBid", n.ifd(&IfdP::limit(m, 10 * TOK, P260, ifd_exit(m))), 585),
    ];
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
    // Hand-coded byte offsets used inside the contracts must match the compiler's state encoding.
    let le = |s: &[u8], a: usize, b: usize| i64::from_le_bytes(s[a..b].try_into().unwrap());
    let mut ap = dutch_ask(&f);
    ap.interval = 77;
    ap.min_fill = 4_321;
    let ask = bytecode(&n.ask(&ap));
    let s = &ask[1..244];
    assert_eq!(&s[34..66], &TOKEN_COV.as_bytes(), "ask tokenCovId offset (touch)");
    assert_eq!(&s[67..99], &n.t.hash[..], "ask tokenTplHash offset");
    assert_eq!(le(s, 118, 126), SCALE, "ask scale offset");
    assert_eq!(le(s, 127, 135), 4_321, "ask minFill offset");
    assert_eq!(le(s, 136, 144), ap.price, "ask price offset");
    assert_eq!(le(s, 163, 171), ap.active_from, "ask activeFrom offset");
    assert_eq!(le(s, 190, 198), 77, "ask interval offset");
    assert_eq!(le(s, 208, 216), ap.slope, "ask slope offset");
    let mut bp = rising_bid(&f);
    bp.interval = 88;
    bp.min_fill = 1_234;
    let bid = bytecode(&n.bid(&bp));
    let s = &bid[1..286];
    assert_eq!(le(s, 151, 159), SCALE, "bid scale offset");
    assert_eq!(le(s, 160, 168), 1_234, "bid minFill offset");
    assert_eq!(le(s, 169, 177), bp.price, "bid price offset");
    assert_eq!(le(s, 196, 204), bp.active_from, "bid activeFrom offset");
    assert_eq!(le(s, 241, 249), 88, "bid interval offset");
    assert_eq!(le(s, 259, 267), bp.slope, "bid slope offset");
    let mut cp = CondP::oco(m);
    cp.armed = 1;
    let c = bytecode(&n.cond(&cp));
    assert_eq!(le(&c, 182, 190), cp.stop, "cond stopPrice splice window");
    assert_eq!(le(&c, 245, 253), 1, "cond armed splice window");
    let mut ap = AskP::new(m, P250);
    ap.decay_step = 55;
    ap.amount = 66_666;
    let ab = bytecode(&n.ask(&ap));
    assert_eq!(le(&ab[1..244], 226, 234), 55, "ask decayStep offset");
    assert_eq!((ab[235], le(&ab, 236, 244)), (0x08, 66_666), "ask amountLeft splice window (the last state field)");
    let mut cl = CondP::oco(m);
    cl.amount = 77_777;
    let cb = bytecode(&n.cond(&cl));
    assert_eq!((cb[271], le(&cb, 272, 280)), (0x08, 77_777), "cond amountLeft splice window");
    // Repeat fields of KobCondAsk (immutable, after amountLeft): parent [281..313), rptPrice [314..322),
    // rptUntil [323..331) in bytecode coordinates; KobIfdBid reads parent at state [280..312).
    let mut cr = CondP::oco(m);
    cr.parent = [0xab; 32];
    cr.rpt_price = 4_321;
    cr.rpt_until = 8_765;
    let crb = bytecode(&n.cond(&cr));
    assert_eq!((crb[280], &crb[281..313]), (0x20, &[0xab; 32][..]), "cond parent field");
    assert_eq!((crb[313], le(&crb, 314, 322)), (0x08, 4_321), "cond rptPrice field");
    assert_eq!((crb[322], le(&crb, 323, 331)), (0x08, 8_765), "cond rptUntil field");
    assert_eq!(&crb[1..331][280..312], &[0xab; 32][..], "cond parent at state [280..312) (KobIfdBid merge)");
    // KobIfdBid splices amountLeft [161..169), armed [287..295) and rptAmount [296..304) (bytecode
    // coordinates).
    let mut ip = IfdP::limit(m, 7_777, P260, ifd_exit(m));
    ip.armed = 1_234;
    ip.entry_stop = 220_000_000;
    let ib = bytecode(&n.ifd(&ip));
    assert_eq!((ib[160], le(&ib, 161, 169)), (0x08, 7_777), "ifd amountLeft splice window");
    assert_eq!((ib[286], le(&ib, 287, 295)), (0x08, 1_234), "ifd armed splice window");
    let mut ipr = ip.clone();
    ipr.rpt = 5_555;
    let irb = bytecode(&n.ifd(&ipr));
    assert_eq!((irb[295], le(&irb, 296, 304)), (0x08, 5_555), "ifd rptAmount splice window");
    assert_eq!(n.cond_state(&CondP::oco(m)).len(), 279, "KobCondAsk state length (KobIfdBid exitState)");
    // The touch windows (KobCondAsk / KobIfdBid touchAsk, touchBid, touch): a bid's fields sit 33 bytes
    // after an ask's (interval and slope: 51).
    let (ask_s, bid_s) = (bytecode(&n.ask(&ap))[1..244].to_vec(), bytecode(&n.bid(&bp))[1..286].to_vec());
    assert_eq!((&ask_s[34..66], &bid_s[34..66]), (&TOKEN_COV.as_bytes()[..], &TOKEN_COV.as_bytes()[..]), "touch tokenCovId window");
    for (w, a, b) in [("scale", 118, 151), ("minFill", 127, 160), ("price", 136, 169), ("activeFrom", 163, 196)] {
        assert_eq!(b - a, 33, "{w}");
        assert_eq!((ask_s[a - 1], bid_s[b - 1]), (0x08, 0x08), "{w} push prefix");
    }
    for (w, a, b) in [("interval", 190, 241), ("slope", 208, 259)] {
        assert_eq!(b - a, 51, "{w}");
        assert_eq!((ask_s[a - 1], bid_s[b - 1]), (0x08, 0x08), "{w} push prefix");
    }
    // The evidence templates this harness inlines are the ones the committed ctor files carry.
    let ctor = |name: &str| -> Vec<ArtifactValue> {
        serde_json::from_slice(&std::fs::read(common::repo_root().join(format!("contracts/v2/{name}.ctor.json"))).expect("ctor"))
            .expect("ctor json")
    };
    assert_eq!(ctor("KobCondAsk")[..6], n.ev_consts()[..], "KobCondAsk ctor: ASK_TPL/PRE/SUF, BID_TPL/PRE/SUF");
    let mut ifd_consts = n.tpl_consts(&n.cond_tpl);
    ifd_consts.extend(n.tpl_consts(&n.bid_tpl));
    assert_eq!(ctor("KobIfdBid")[..6], ifd_consts[..], "KobIfdBid ctor: COND_TPL/PRE/SUF, BID_TPL/PRE/SUF");
    // The tested templates are the committed, reproducible artifacts (network constants resolved).
    let ifd_tpl = tpl_of(&n.ifd(&IfdP::limit(m, 10 * TOK, P260, ifd_exit(m))));
    for (name, tpl) in [("KobAsk", &n.ask_tpl), ("KobBid", &n.bid_tpl), ("KobCondAsk", &n.cond_tpl), ("KobIfdBid", &ifd_tpl)] {
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(common::repo_root().join(format!("contracts/artifacts/{name}.json"))).expect("artifact"),
        )
        .unwrap();
        let c = json["contracts"].as_object().unwrap().values().next().unwrap()["compiled"]["template_hash"].clone();
        let committed: Vec<u8> = match c {
            serde_json::Value::String(h) => (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect(),
            v => serde_json::from_value(v).unwrap(),
        };
        assert_eq!(committed, tpl.hash, "{name}: tested template differs from the committed artifact");
    }
}

#[test]
fn v2_positive_limit_orders() {
    let f = fx();
    let n = &f.net;
    run_ok(n, &s1(&f));
    run_ok(n, &s2(&f));
    run_ok(n, &s2_tips(&f, 0));
    run_ok(n, &s3(&f));
    run_ok(n, &s4(&f));
    run_ok(n, &s5(&f));
    run_ok(n, &s6(&f, 1));
    run_ok(n, &s6(&f, 2));
    run_ok(n, &s7(&f, 0x01, f.maker_a));
    run_ok(n, &fresh(s8(&f, EXPIRY, EXPIRY as u64)));
    run_ok(n, &s8(&f, NO_EXPIRY, (1_000 + MAX_IDLE) as u64));
    run_ok(n, &s9(&f, "cancel", EXPIRY, 0));
    run_ok(n, &fresh(s9(&f, "refund", EXPIRY, EXPIRY as u64)));
    run_ok(n, &s9(&f, "refund", NO_EXPIRY, (1_000 + MAX_IDLE) as u64));
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.active_from = NOW as i64;
    run_ok(n, &with_name(s1_with(&f, &p, 4 * TOK), "S1a timed activation: fill at lockTime == activeFrom"));
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tip = 0;
    run_ok(n, &with_name(s1_with(&f, &p, 4 * TOK), "S1z zero-fee ask filled at exactly its quote"));
    // Linearity at whole tokens: two chained 2-token fills pay the maker exactly what one 4-token fill pays.
    assert_eq!(2 * ask_all_in(2 * TOK, P250, TIP), ask_all_in(4 * TOK, P250, TIP));
    run_ok(n, &with_name(s1_with(&f, &AskP::new(pk(&f.maker_a), P250), 2 * TOK), "S1c partial fill of 2 tokens (chain step)"));
    // Any amount: a fill of 4321 base units (not a multiple of the scale), paid the exact ceil.
    run_ok(n, &with_name(s1_with(&f, &AskP::new(pk(&f.maker_a), P250), 4_321), "S1d partial fill of 4321 base units"));
}

#[test]
fn v2_positive_time_variants() {
    let f = fx();
    let n = &f.net;
    run_ok(n, &with_name(with_seq(s1_with(&f, &twap_ask(&f), 2 * TOK), 0, 600), "T1 TWAP ask: 2 tokens after 600 DAA"));
    run_ok(
        n,
        &with_name(
            with_seq(s6_with(&f, &dca_bid(&f), TOK, 10 * TOK, P245, 0), 0, 600),
            "T2 DCA bid: 1 token after 600 DAA, continues",
        ),
    );
    let d = dutch_ask(&f);
    run_ok(
        n,
        &with_name(
            s1_at(&f, &d, 4 * TOK, decay_down(d.price, d.price_end, d.slope, 1_000, 51_000), 51_000),
            "T3 Dutch ask at t=51000: 3.00 -> 2.50",
        ),
    );
    run_ok(n, &with_name(s1_at(&f, &d, 4 * TOK, d.price_end, 300_000), "T3b Dutch ask floored at 2.00"));
    let r = rising_bid(&f);
    let eff = rise_up(r.price, r.price_end, r.slope, 1_000, 51_000);
    run_ok(
        n,
        &with_name(s6_with(&f, &r, 4 * TOK, 4 * TOK, eff, 51_000), "T4 rising bid at t=51000: 2.00 -> 2.50 (budget at the 2.60 cap)"),
    );
    run_ok(
        n,
        &with_name(s6_with(&f, &r, 4 * TOK, 10 * TOK, eff, 51_000), "T4b rising bid partial: cap difference rides on the delivery"),
    );
    // A TWAP ask's fill is evidence exposed from its UTXO DAA + interval (its slice opening).
    let twap = down_ev().with(|e| {
        e.interval = 600;
        e.daa = NOW - 1_200;
    });
    let oco = CondP::oco(pk(&f.maker_a));
    run_ok(
        n,
        &with_name(
            cond_update(&f, &oco, &armed(&oco), &twap, 0),
            "T5 a TWAP ask's fill arms a stop: exposed from UTXO DAA + interval, exactly R = 600 DAA before",
        ),
    );
}

#[test]
fn v2_positive_touch_conditionals_ifd() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    // S10: a taker buys 4 of 10 tokens from a resting ask at 2.50 (S1); next to it a stop at 2.50 arms reading
    // that partial fill (the ask at input 0 continues with 6 tokens; its custody is input 1).
    let mut s =
        with_name(s1(&f), "S10 a partial fill of a resting ask (S1, 4 of 10 tokens at 2.50) arms a 2.50 stop next to it (update)");
    let st = CondP { stop: P250, ..CondP::oco(m) };
    let c = cov(0xc1);
    s.inputs.push(call(&n.cond(&st), "update", vec![iv(0), iv(1)], "cond.update", CARRIER, c, 2_000));
    s.outputs.push(out(CARRIER, spk_of(&n.cond(&armed(&st))), Some((3, c))));
    run_ok(n, &s);
    let oco = CondP::oco(m);
    run_ok(n, &with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "S11 OCO take-profit leg sells 4 of 10 tokens at 3.00 all-in"));
    let r = down_ev();
    run_ok(
        n,
        &with_name(
            cond_fill(&f, &oco, 4 * TOK, 1, &armed(&oco), Some(&r)),
            "S12 OCO stop leg armed in its own fill: an ask at 1.98 (<= stop) sold out in the same tx, sells 4 of 10 tokens in the default 3% band (1.94), continues armed",
        ),
    );
    run_ok(
        n,
        &with_name(
            cond_update(&f, &oco, &armed(&oco), &r, 0),
            "S13 permissionless arm next to the evidence fill (no fill of the order)",
        ),
    );
    run_ok(n, &with_name(cond_fill(&f, &armed(&oco), 4 * TOK, 1, &armed(&oco), None), "S13b armed stop leg fills without evidence"));
    let mut sl = oco.clone();
    sl.tp = 0;
    run_ok(n, &with_name(cond_fill(&f, &sl, 4 * TOK, 1, &armed(&sl), Some(&r)), "S13c stop-limit (no TP leg) triggered fill"));
    let mut band = oco.clone();
    band.slip_bps = 100;
    run_ok(n, &with_name(cond_fill(&f, &band, 4 * TOK, 1, &armed(&band), Some(&r)), "S13d stop-market with a user band of 1% (1.98)"));
    let tr = trailing(m);
    let mut tr2 = tr.clone();
    tr2.stop += 2 * tr.step;
    run_ok(
        n,
        &with_name(
            cond_update(&f, &tr, &tr2, &up_ev(), tr.wait as u64),
            "S15 trailing stop ratchets 2.00 -> 2.10: one bid fill at 2.20 justifies two steps (gap 0.10; band moves along)",
        ),
    );
    run_ok(n, &s14(&f));
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs::new(6 * TOK, 6 * TOK)),
            "S14c IFO entry final 6 of 6 tokens: a second, independent exit with 6 tokens; entry terminates",
        ),
    );
    run_ok(n, &with_name(ifd_fill(&f, &IfdKnobs::new(10 * TOK, TOK)), "S14d IFO entry partial 1 of 10 tokens (smallest exit)"));
    run_ok(
        n,
        &with_name(ifd_fill(&f, &IfdKnobs::new(10 * TOK, 4_321)), "S14e IFO entry partial 4321 base units (spend rounded down)"),
    );
    let exit = ifd_exit(m);
    let mut s = with_name(cond_fill(&f, &exit, 10 * TOK, 0, &exit, None), "S14b IFO exit take-profit sells 10 of 10 tokens at 3.00");
    s.outputs.remove(1);
    s.outputs.remove(1);
    s.outputs[0].value = (ask_all_in(10 * TOK, 300_000_000, TIP) + 2 * CARRIER) as u64;
    set_arg(&mut s, 0, 2, iv(0));
    set_leader_next(&mut s, 1, vec![tok_state(10 * TOK, &pk(&f.taker), SCHEME_P2PK)]);
    let last = s.outputs.len() - 1;
    s.outputs[last].value = (1000 * KAS - ask_all_in(10 * TOK, 300_000_000, TIP) - CARRIER - NET_FEE) as u64;
    run_ok(n, &s);
}

#[test]
fn v2_batch_8x8() {
    let f = Fx {
        net: Net::with_token(Token::named("KCC20Ref_8x8")),
        maker_a: keypair(),
        maker_b: keypair(),
        maker_c: keypair(),
        taker: keypair(),
        matcher: keypair(),
    };
    for tip in [TIP, 0] {
        let (s, income) = b1(&f, tip);
        let fee = run_ok(&f.net, &s);
        println!(
            "  matcher gross income {income} sompi ({:.5} KAS), network fee floor {fee} sompi, net {:.5} KAS",
            income as f64 / 1e8,
            (income - fee as i64) as f64 / 1e8
        );
    }
}

#[test]
fn v2_negative_limits_and_tips() {
    let f = fx();
    let n = &f.net;
    let taker = pk(&f.taker);

    let mut s = with_name(s1(&f), "NF1 matcher pays the ask 1 sompi below its all-in limit");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tip = 0;
    let mut s = with_name(s1_with(&f, &p, 4 * TOK), "NF2 zero-fee ask paid 1 sompi below its quote");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    let mut s = with_name(s2(&f), "NF3 matcher charges the bid 1 sompi above its all-in limit");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    let mut s = with_name(s3(&f), "NF4 partial bid fill: delivery carrier skimmed by 1 sompi (v9 class)");
    s.outputs[0].value -= 1;
    s.outputs[3].value += 1;
    run_bad(n, &s, 0);

    let mut s = with_name(s3(&f), "NF5 partial bid fill: continuation short by 1 sompi (v9 class)");
    s.outputs[2].value -= 1;
    s.outputs[3].value += 1;
    run_bad(n, &s, 0);

    let mut s = with_name(s3(&f), "NF6 partial bid fill: continuation 1 sompi over budget (over-buying)");
    s.outputs[2].value += 1;
    s.outputs[3].value -= 1;
    run_bad(n, &s, 0);

    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tip = -1;
    run_bad(n, &with_name(s1_with(&f, &p, 4 * TOK), "NF7 negative tip makes the order unfillable"), 0);

    let mut s = with_name(s1(&f), "NF8 malleated n = 4001 base units (outputs for 4000)");
    set_arg(&mut s, 0, 0, nb(4 * TOK + 1));
    run_bad(n, &s, 0);

    let mut s = with_name(s2(&f), "NF9 payout aliasing: ask C's payout slot redirected");
    s.outputs[2].script_public_key = p2pk_spk(&taker);
    run_bad(n, &s, 2);

    let mut s = with_name(s1(&f), "NF10 rest continuation carrier skimmed by 1 sompi");
    s.outputs[1].value -= 1;
    run_bad(n, &s, 0);
}

#[test]
fn v2_negative_time_variants() {
    let f = fx();
    let n = &f.net;
    run_bad(n, &with_name(with_seq(s1_with(&f, &twap_ask(&f), 2 * TOK), 0, 599), "NV1 TWAP ask filled 1 DAA too early (CSV)"), 0);
    run_bad(
        n,
        &with_name(with_seq(s1_with(&f, &twap_ask(&f), 2 * TOK + 1), 0, 600), "NV2 TWAP ask: 2001 base units > maxFill 2000"),
        0,
    );
    run_bad(
        n,
        &with_name(with_seq(s6_with(&f, &dca_bid(&f), TOK, 10 * TOK, P245, 0), 0, 599), "NV3 DCA bid filled 1 DAA too early (CSV)"),
        0,
    );
    run_bad(
        n,
        &with_name(
            with_seq(s6_with(&f, &dca_bid(&f), TOK + 1, 10 * TOK, P245, 0), 0, 600),
            "NV4 DCA bid: 1001 base units > maxFill 1000",
        ),
        0,
    );
    let d = dutch_ask(&f);
    let t = NOW as i64 + 1;
    run_bad(
        n,
        &with_name(
            s1_at(&f, &d, 4 * TOK, decay_down(d.price, d.price_end, d.slope, 1_000, t), t),
            "NV5 Dutch ask: decay time above the tx lockTime (CLTV)",
        ),
        0,
    );
    let later = decay_down(d.price, d.price_end, d.slope, 1_000, 52_000);
    run_bad(
        n,
        &with_name(s1_at(&f, &d, 4 * TOK, later, 51_000), "NV6 Dutch ask paid at a later (lower) price than the proven time"),
        0,
    );
    let mut up = dutch_ask(&f);
    up.slope = -1_000_000;
    run_bad(
        n,
        &with_name(s1_at(&f, &up, 4 * TOK, 350_000_000, 51_000), "NV7 ask with a negative slope (would reward understated time)"),
        0,
    );
    let r = rising_bid(&f);
    let later = rise_up(r.price, r.price_end, r.slope, 1_000, 52_000);
    run_bad(
        n,
        &with_name(
            s6_with(&f, &r, 4 * TOK, 4 * TOK, later, 51_000),
            "NV8 rising bid charged a later (higher) price than the proven time",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(
            s6_with(&f, &r, 4 * TOK, 4 * TOK, rise_up(r.price, r.price_end, r.slope, 1_000, NOW as i64 + 1), NOW as i64 + 1),
            "NV9 rising bid: time above the tx lockTime (CLTV)",
        ),
        0,
    );
    // A TWAP ask's fill is exposed from its UTXO DAA + interval: one DAA short of R after the slice opened.
    let oco = CondP::oco(pk(&f.maker_a));
    let twap = down_ev().with(|e| {
        e.interval = 600;
        e.daa = NOW - 1_199;
    });
    run_bad(
        n,
        &with_name(
            cond_update(&f, &oco, &armed(&oco), &twap, 0),
            "NR14 TWAP ask evidence exposed R - 1 DAA after its slice opened (the interval counts)",
        ),
        0,
    );
    // Decaying orders are not evidence: a Dutch ask quoting 1.98 at its start.
    let dutch = down_ev().with(|e| e.slope = 1);
    run_bad(n, &with_name(cond_update(&f, &oco, &armed(&oco), &dutch, 0), "NR15 a Dutch ask's fill (moving price) as evidence"), 0);
}

#[test]
fn v2_negative_tif_cancel_expiry() {
    let f = fx();
    let n = &f.net;
    let taker = pk(&f.taker);

    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 2;
    run_bad(n, &with_name(s1_with(&f, &p, 4 * TOK), "NT-FOK ask filled 4 of 10 tokens"), 0);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    run_bad(n, &with_name(s1_with(&f, &p, 4 * TOK), "NT-IOC ask remainder kept resting"), 0);
    let mut s = with_name(s4(&f), "NT-IOC ask remainder sent to the taker");
    s.outputs[1].script_public_key = n.t.spk(6 * TOK, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6 * TOK, &taker, SCHEME_P2PK), tok_state(4 * TOK, &taker, SCHEME_P2PK)]);
    run_bad(n, &s, 0);
    let mut bp = BidP::new(pk(&f.maker_b), P245);
    bp.tif = 2;
    run_bad(n, &with_name(s6_with(&f, &bp, 4 * TOK, 10 * TOK, P245, 0), "NT-FOK bid filled 4 of 10 tokens"), 0);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.active_from = NOW as i64 + 1;
    run_bad(n, &with_name(s1_with(&f, &p, 4 * TOK), "NT-ACT fill one DAA before activeFrom"), 0);

    run_bad(n, &with_name(s7(&f, 0x81, f.maker_a), "NC1 cancel with SIGHASH_ALL|ANYONECANPAY"), 0);
    run_bad(n, &with_name(s7(&f, 0x01, f.taker), "NC2 cancel with the wrong key"), 0);
    run_bad(n, &with_name(fresh(s8(&f, EXPIRY, EXPIRY as u64 - 1)), "NC3 ask refund one DAA before expiry"), 0);
    run_bad(n, &with_name(fresh(s9(&f, "refund", EXPIRY, EXPIRY as u64 - 1)), "NC3b bid refund one DAA before expiry"), 0);
    run_bad(n, &with_name(s8(&f, NO_EXPIRY, (1_000 + MAX_IDLE - 1) as u64), "NC4 ask refund one DAA before the 90-day idle bound"), 0);
    run_bad(n, &with_name(s9(&f, "refund", NO_EXPIRY, (1_000 + MAX_IDLE - 1) as u64), "NC5 bid refund before the idle bound"), 0);
    let mut s = with_name(fresh(s8(&f, EXPIRY, EXPIRY as u64)), "NC6 refund keeper takes more than refundTip");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    let mut s = with_name(fresh(s9(&f, "refund", EXPIRY, EXPIRY as u64)), "NC7 bid refund paid to a non-maker key");
    s.outputs[0].script_public_key = p2pk_spk(&taker);
    run_bad(n, &s, 0);
}

#[test]
fn v2_negative_trigger_manipulation() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let oco = CondP::oco(m);
    let good = down_ev();
    let fill = |e: &Ev, name: &str| with_name(cond_fill(&f, &oco, 4 * TOK, 1, &armed(&oco), Some(e)), name);

    let mut s = with_name(
        cond_fill(&f, &oco, 4 * TOK, 1, &armed(&oco), None),
        "NT1 stop leg fill without evidence (ev = the taker's P2PK input)",
    );
    set_arg(&mut s, 0, 4, iv(2));
    set_arg(&mut s, 0, 5, iv(1));
    run_bad(n, &s, 0);

    run_bad(n, &fill(&good.with(|e| e.daa = NOW - 500), "NT2 wash print: the evidence ask was exposed only 500 DAA (< 600)"), 0);
    run_bad(n, &fill(&Ev::ask(200_000_001), "NT3 evidence quote above the stop"), 0);
    run_bad(n, &fill(&Ev::bid(198_000_000), "NT4 down trigger from a resting BID fill (not a seller's quote)"), 0);
    let mut o2 = oco.clone();
    o2.min_touch = 6 * TOK;
    run_bad(
        n,
        &with_name(cond_fill(&f, &o2, 4 * TOK, 1, &armed(&o2), Some(&good)), "NT5 evidence fill of 5000 base units < required 6000"),
        0,
    );
    run_bad(
        n,
        &fill(&good.with(|e| e.mode = EvMode::Forged), "NT6 look-alike evidence: a real ask's state under another template"),
        0,
    );
    run_bad(n, &fill(&good.with(|e| e.tok_daa = NOW - 599), "NT7 evidence ask's custody moved R - 1 DAA before (a fresh top-up)"), 0);
    run_bad(n, &fill(&good.with(|e| e.token = OTHER_COV), "NT8 evidence of another token"), 0);

    let mut s =
        with_name(cond_fill(&f, &armed(&oco), 4 * TOK, 1, &armed(&oco), None), "NT9 armed stop leg paid 1 sompi below its band");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    run_bad(
        n,
        &with_name(cond_fill(&f, &oco, 4 * TOK, 0, &armed(&oco), None), "NT10 TP-leg fill sneaks armed=1 into the continuation"),
        0,
    );
    let mut s = with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "NT11 TP leg paid 1 sompi below its all-in price");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    let mut wide = oco.clone();
    wide.slip_bps = 10_001;
    run_bad(n, &with_name(cond_fill(&f, &armed(&wide), 4 * TOK, 1, &armed(&wide), None), "NT12 stop band above 100%"), 0);

    // The order's own token UTXO may not be spent next to an update (it pins no token output); the
    // evidence ask's custody (a foreign token input) is fine.
    let mut s = with_name(cond_update(&f, &oco, &armed(&oco), &good, 0), "NT13 arm tx also spends the order's tokens to the attacker");
    let t = &n.t;
    let c = cov(0xc1);
    let thief = pk(&f.taker);
    let l = push_tok(
        &f,
        &mut s,
        CARRIER,
        10 * TOK,
        &c.as_bytes(),
        SCHEME_COVID,
        TOKEN_COV,
        Wit::CovId,
        2_000,
        vec![tok_state(10 * TOK, &thief, SCHEME_P2PK)],
    );
    s.outputs.push(out(CARRIER, t.spk(10 * TOK, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
    run_bad(n, &s, 0);
    run_bad(n, &with_name(cond_update(&f, &armed(&oco), &armed(&oco), &good, 0), "NT14 update on an already armed order"), 0);

    let tr = trailing(m);
    let mut tr2 = tr.clone();
    tr2.stop += 2 * tr.step; // the bid fill at 2.20 justifies two steps (gap 0.10)
    run_bad(n, &with_name(cond_update(&f, &tr, &tr2, &up_ev(), tr.wait as u64 - 1), "NT15 ratchet sooner than trailWait (CSV)"), 0);
    run_bad(
        n,
        &with_name(cond_update(&f, &tr, &tr2, &Ev::bid(214_999_999), tr.wait as u64), "NT16 ratchet evidence below stop+step+gap"),
        0,
    );
    let mut tr1 = tr.clone();
    tr1.stop += tr.step;
    run_bad(
        n,
        &with_name(cond_update(&f, &tr, &tr1, &up_ev(), tr.wait as u64), "NT17 ratchet by one step when the evidence justifies two"),
        0,
    );
    let mut tr3 = tr.clone();
    tr3.stop += 3 * tr.step;
    run_bad(
        n,
        &with_name(cond_update(&f, &tr, &tr3, &up_ev(), tr.wait as u64), "NT17b ratchet by three steps (one more than justified)"),
        0,
    );
    let mut high = tr.clone();
    high.stop = 295_000_000;
    let mut high2 = high.clone();
    high2.stop = 300_000_000;
    run_bad(
        n,
        &with_name(
            cond_update(&f, &high, &high2, &Ev::bid(320_000_000), tr.wait as u64),
            "NT18 ratchet up to the take-profit (travel cap)",
        ),
        0,
    );
    let mut nt = oco.clone();
    nt.stop += 5_000_000;
    run_bad(n, &with_name(cond_update(&f, &oco, &nt, &up_ev(), 600), "NT19 ratchet on a non-trailing order"), 0);
}
// ---------------------------------------------------------------- touch trigger battery

/// minRestDaa of every order in the fixtures.
const R: u64 = 600;

/// A trigger of this family's sell-side templates: the stop leg of a KobCondAsk armed inside its fill
/// (settle) or by update, its trailing ratchet (update), and the buy-stop entry of a KobIfdBid armed inside
/// its fill or by update.
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
            Kind::Settle => "TA",
            Kind::Arm => "TU",
            Kind::Trail => "TT",
            Kind::IfdFill => "TI",
            Kind::IfdArm => "TJ",
        }
    }
    fn is_update(self) -> bool {
        matches!(self, Kind::Arm | Kind::Trail | Kind::IfdArm)
    }
    /// Default evidence: an ask at 1.98 for the 2.00 stop, a bid at 2.20 for the trail (2 steps), a bid at 2.25
    /// for the 2.20 buy-stop entry.
    fn ev(self) -> Ev {
        match self {
            Kind::Settle | Kind::Arm => Ev::ask(198_000_000),
            Kind::Trail => Ev::bid(220_000_000),
            Kind::IfdFill | Kind::IfdArm => Ev::bid(225_000_000),
        }
    }
    /// The evidence quote exactly at the trigger boundary, and one sompi on the wrong side of it.
    fn edge(self) -> (i64, i64) {
        match self {
            Kind::Settle | Kind::Arm => (200_000_000, 200_000_001),
            Kind::Trail => (215_000_000, 214_999_999),
            Kind::IfdFill | Kind::IfdArm => (220_000_000, 219_999_999),
        }
    }
    /// Trail only: evidence far on the wrong side of the boundary (a bid at 1.50: k = -12 would move the sell stop down to 1.40).
    fn far(self) -> i64 {
        150_000_000
    }
    /// Argument positions of ev and tk in the order's sigscript.
    fn ev_pos(self) -> (usize, Option<usize>) {
        match self {
            Kind::Settle => (4, Some(5)),
            Kind::Arm | Kind::Trail => (0, Some(1)),
            Kind::IfdFill => (5, None),
            Kind::IfdArm => (0, None),
        }
    }
}
/// The stop entry of the IfdBid triggers: stop 2.20, limit 2.60, 10 whole tokens.
fn stop_ifd(m: [u8; 32], min_touch: i64) -> IfdP {
    IfdP { entry_stop: 220_000_000, min_touch, ..IfdP::limit(m, 10 * TOK, P260, ifd_exit(m)) }
}
/// The stop order of `kind` (input 0) triggered by the evidence `ev`; `min_touch`: its minTouch (base units).
fn trig(f: &Fx, kind: Kind, ev: &Ev, min_touch: i64) -> Scn {
    let m = pk(&f.maker_a);
    match kind {
        Kind::Settle => {
            let cp = CondP { min_touch, ..CondP::oco(m) };
            cond_fill(f, &cp, 4 * TOK, 1, &armed(&cp), Some(ev))
        }
        Kind::Arm => {
            let cp = CondP { min_touch, ..CondP::oco(m) };
            cond_update(f, &cp, &armed(&cp), ev, 0)
        }
        Kind::Trail => {
            let tr = CondP { min_touch, ..trailing(m) };
            let k = ((ev.price - tr.gap - tr.stop) / tr.step).min((tr.tp - 1 - tr.stop) / tr.step);
            cond_update(f, &tr, &CondP { stop: tr.stop + k * tr.step, ..tr.clone() }, ev, tr.wait as u64)
        }
        Kind::IfdFill => ifd_fill(
            f,
            &IfdKnobs { ev: Some(ev.clone()), next_armed: Some(1), min_touch, ..stop_entry(IfdKnobs::new(10 * TOK, 4 * TOK)) },
        ),
        Kind::IfdArm => {
            let ip = stop_ifd(m, min_touch);
            ifd_update(f, &ip, &IfdP { armed: 1, ..ip.clone() }, ev, 0)
        }
    }
}
/// Appends a second order (cov 0xc2 / 0xd2, input returned) that arms or trails by update next to the evidence
/// at (ev, tk), with its continuation output: the shared-evidence scenarios.
fn add_second_update(f: &Fx, s: &mut Scn, kind: Kind, ev: i64, tk: i64) -> usize {
    let m = pk(&f.maker_b);
    let at = s.inputs.len();
    match kind {
        Kind::Settle | Kind::Arm => {
            let cp = CondP::oco(m);
            s.inputs.push(call(&f.net.cond(&cp), "update", vec![iv(ev), iv(tk)], "cond2.update", CARRIER, cov(0xc2), 2_000));
            s.outputs.push(out(CARRIER, spk_of(&f.net.cond(&armed(&cp))), Some((at as u16, cov(0xc2)))));
        }
        Kind::Trail => {
            let tr = trailing(m);
            let mut i = call(&f.net.cond(&tr), "update", vec![iv(ev), iv(tk)], "cond2.update", CARRIER, cov(0xc2), 2_000);
            i.seq = tr.wait as u64;
            s.inputs.push(i);
            let next = CondP { stop: tr.stop + 2 * tr.step, ..tr.clone() };
            s.outputs.push(out(CARRIER, spk_of(&f.net.cond(&next)), Some((at as u16, cov(0xc2)))));
        }
        Kind::IfdFill | Kind::IfdArm => {
            let ip = stop_ifd(m, TOK);
            let v = ifd_value(&ip);
            s.inputs.push(call(&f.net.ifd(&ip), "update", vec![iv(ev)], "ifd2.update", v, cov(0xd2), 1_000));
            s.outputs.push(out(v, spk_of(&f.net.ifd(&IfdP { armed: 1, ..ip.clone() })), Some((at as u16, cov(0xd2)))));
        }
    }
    at
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
/// accepted, one DAA short rejected), slope, side, token, scale, size threshold, an evidence that does not
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

    ok(&good, TOK, "00", "triggered by a plain fill in the same transaction");
    // exposure = max(UTXO DAA + interval, activeFrom, custody DAA) + R <= lock time
    ok(&good.with(|e| e.daa = NOW - R), TOK, "01", "evidence UTXO exposed exactly R = minRestDaa DAA");
    bad(&good.with(|e| e.daa = NOW - R + 1), TOK, "01", "evidence UTXO exposed R - 1 DAA (too young)");
    if let (true, Some(pt)) = (good.ask, pt) {
        ok(&good.with(|e| e.tok_daa = NOW - R), TOK, "02", "evidence ask's custody moved exactly R DAA before");
        bad(&good.with(|e| e.tok_daa = NOW - R + 1), TOK, "02", "evidence ask's custody moved R - 1 DAA before (a fresh top-up)");
        // the fresh custody hidden behind an old token UTXO of ANOTHER token owned by the evidence ask
        let mut s = trig(f, kind, &good.with(|e| e.tok_daa = NOW - R + 1), TOK);
        let at = s.inputs.len() as i64;
        let st = vec![tok_state(5 * TOK, &matcher, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 5 * TOK, &good.cov.as_bytes(), SCHEME_COVID, OTHER_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(5 * TOK, &matcher, SCHEME_P2PK), Some((l as u16, OTHER_COV))));
        set_arg(&mut s, 0, pt, iv(at));
        bad_s(s, "02b", "fresh custody; tk names an old UTXO of another token owned by the evidence ask", 0);
    }
    ok(&good.with(|e| e.active_from = (NOW - R) as i64), TOK, "03", "evidence active exactly R DAA (activeFrom)");
    bad(&good.with(|e| e.active_from = (NOW - R) as i64 + 1), TOK, "03", "evidence active only R - 1 DAA (activeFrom)");
    ok(
        &good.with(|e| {
            e.interval = 100;
            e.daa = NOW - R - 100;
        }),
        TOK,
        "04",
        "TWAP/DCA evidence: its slice opened (UTXO DAA + interval) exactly R DAA before",
    );
    bad(
        &good.with(|e| {
            e.interval = 100;
            e.daa = NOW - R - 99;
        }),
        TOK,
        "04",
        "TWAP/DCA evidence: its slice opened R - 1 DAA before (the interval counts)",
    );
    bad(&good.with(|e| e.slope = 1), TOK, "05", "decaying evidence (Dutch ask / rising bid): a moving price is not a quote");
    bad(&good.with(|e| e.ask = !e.ask), TOK, "06", "evidence of the wrong side (a bid for a sell stop, an ask for a buy stop)");
    bad(&good.with(|e| e.token = OTHER_COV), TOK, "07", "evidence of another token");
    if let (true, Some(pt)) = (good.ask, pt) {
        // an ask of another token whose covenant also owns a UTXO of this token, named as its custody
        let mut s = trig(f, kind, &good.with(|e| e.token = OTHER_COV), TOK);
        let at = s.inputs.len() as i64;
        let st = vec![tok_state(5 * TOK, &matcher, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 5 * TOK, &good.cov.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(5 * TOK, &matcher, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        set_arg(&mut s, 0, pt, iv(at));
        bad_s(s, "07b", "evidence of another token; tk names a UTXO of this token owned by that ask", 0);
    }
    bad(&good.with(|e| e.scale = 10 * SCALE), TOK, "08", "evidence quoting per another scale (10^4: its prices are not comparable)");
    ok(&good, 5 * TOK, "09", "evidence of exactly minTouch (5000 base units, threshold 5000)");
    bad(
        &good.with(|e| e.amount = 5 * TOK - 1),
        5 * TOK,
        "09",
        "evidence one base unit below minTouch (4999 base units, threshold 5000)",
    );
    bad(&good.with(|e| e.mode = EvMode::Cancel), TOK, "10", "cancelled, not filled (a signature push, ground to read as n > 0)");
    bad(&good.with(|e| e.mode = EvMode::Refund), 0, "11", "refunded, not filled (ask settle n = 0 / bid refund; minTouch 0)");
    if !good.ask {
        bad(&good.with(|e| e.mode = EvMode::ZeroFill), 0, "11b", "a bid fill with n = 0 (the bid refuses it too; minTouch 0)");
    }
    // wrong indices
    let base = trig(f, kind, &good, TOK);
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
        let fresh = trig(f, kind, &good.with(|e| e.tok_daa = NOW - R + 1), TOK);
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
    bad(&good.with(|e| e.mode = EvMode::Forged), TOK, "17", "look-alike: a real order's state under another template (P2SH of it)");
    bad(&good.with(|e| e.mode = EvMode::NotP2sh), TOK, "18", "the real redeem script pushed by an input that is not its P2SH");
    let (edge, beyond) = kind.edge();
    ok(&good.with(|e| e.price = edge), TOK, "19", "evidence quoting exactly at the trigger boundary");
    bad(&good.with(|e| e.price = beyond), TOK, "19", "evidence quoting one sompi beyond the trigger boundary");
    if kind == Kind::Trail {
        bad(
            &good.with(|e| e.price = kind.far()),
            TOK,
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
        let st = vec![tok_state(3 * TOK, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3 * TOK, &own.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 2_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(3 * TOK, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        bad_s(s, "23", "update next to the evidence also spends a token UTXO owned by the order (to the attacker)", 0);
        let mut s = base.clone();
        let st = vec![tok_state(3 * TOK, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3 * TOK, &thief, SCHEME_P2PK, TOKEN_COV, Wit::P2pk(f.taker), 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(3 * TOK, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        // the taker's own P2PK input (KRON id_type 3 authorises by its presence)
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(out(KAS, p2pk_spk(&thief), None));
        run_ok(
            n,
            &with_name(s, &name(false, "23", "update next to the evidence and another foreign token input (the taker's own tokens)")),
        );
    }
    if kind == Kind::IfdArm {
        // update arms only an entry that has an amount to fill and its stop on the limit's side (else the arm only takes keeperTip)
        let ip = stop_ifd(pk(&f.maker_a), TOK);
        let v = ifd_value(&ip);
        let empty = IfdP { amount: 0, rpt: 9 * TOK, ..ip.clone() };
        let mut s = ifd_update(f, &empty, &IfdP { armed: 1, ..empty.clone() }, &good, 0);
        s.inputs[0].entry.amount = v as u64;
        s.outputs[0].value = v as u64;
        bad_s(s, "24", "update arms a repeating entry with nothing left to buy (all of it is in its exits)", 0);
        let inv = IfdP { price: ip.entry_stop - 1, ..ip.clone() };
        let s = ifd_update(f, &inv, &IfdP { armed: 1, ..inv.clone() }, &good, 0);
        bad_s(s, "25", "update arms an entry whose stop is above its limit (no fill can follow)", 0);
    }
}

/// The order's own counterparty named as its evidence (fill kinds): the resting order it trades with.
fn counterparty(f: &Fx, kind: Kind) -> Option<(Scn, &'static str)> {
    match kind {
        Kind::Settle => Some((cond_into_bid(f), "the stop's own counterparty (the resting bid it sells into) as its evidence")),
        Kind::IfdFill => Some((ifd_from_ask(f), "the entry's own counterparty (the resting ask it buys from) as its evidence")),
        _ => None,
    }
}
/// A fill (input 0) of a template that is never evidence: a conditional take-profit fill (ask side, its
/// custody at input 1), an if-done entry fill (bid side).
fn other_fill(f: &Fx, ask: bool) -> (Scn, &'static str) {
    let m = pk(&f.maker_a);
    if ask {
        (
            cond_fill(f, &CondP::oco(m), 4 * TOK, 0, &CondP::oco(m), None),
            "a KobCondAsk take-profit fill (its custody as tk) as evidence",
        )
    } else {
        (ifd_fill(f, &IfdKnobs::new(10 * TOK, 4 * TOK)), "a KobIfdBid limit-entry fill as evidence")
    }
}
/// The covenant id of the order input 0 of a trigger scenario.
fn own_cov(kind: Kind) -> Hash {
    if kind == Kind::IfdArm || kind == Kind::IfdFill {
        cov(0xd1)
    } else {
        cov(0xc1)
    }
}
/// The unarmed stop (OCO, stop 2.00) sells 4 whole tokens straight into a resting bid quoting 1.98 (its
/// counterparty, input 2) and names that bid as its evidence (ev 2, tk = its custody).
fn cond_into_bid(f: &Fx) -> Scn {
    let t = &f.net.t;
    let c = cov(0xc1);
    let cp = CondP::oco(pk(&f.maker_a));
    let bp = BidP::new(pk(&f.maker_b), 198_000_000);
    let b = cov(0xb1);
    let v = bp.funded(4 * TOK);
    let spend = bid_all_in(4 * TOK, bp.price, TIP);
    let pay = ask_all_in(4 * TOK, stop_leg(cp.stop, cp.slip_bps), TIP);
    let next = vec![tok_state(4 * TOK, &bp.maker, SCHEME_P2PK), tok_state(6 * TOK, &c.as_bytes(), SCHEME_COVID)];
    Scn {
        name: "cond into bid".into(),
        inputs: vec![
            call(
                &f.net.cond(&cp),
                "settle",
                vec![nb(4 * TOK), iv(1), iv(3), iv(1), iv(2), iv(1), iv(0)],
                "cond.settle",
                CARRIER,
                c,
                2_000,
            ),
            tok_in(t, CARRIER, 10 * TOK, &c.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 2_000),
            call(&f.net.bid(&bp), "fill", vec![nb(4 * TOK), iv(1), iv(0)], "bid.fill", v, b, 1_000),
        ],
        outputs: vec![
            out(pay, p2pk_spk(&cp.maker), None),
            out(CARRIER, spk_of(&f.net.cond(&armed(&CondP { amount: 6 * TOK, ..cp.clone() }))), Some((0, c))),
            out(v - spend, t.spk(4 * TOK, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(CARRIER, t.spk(6 * TOK, &c.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))),
            out(spend - pay - NET_FEE, p2pk_spk(&pk(&f.matcher)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}
/// The unarmed buy-stop entry (stop 2.20, limit 2.60) buys 4 whole tokens from a resting ask quoting 2.40 (its
/// counterparty: the ask's custody is the token input, the ask itself input 4) and names that ask as its
/// evidence.
fn ifd_from_ask(f: &Fx) -> Scn {
    let a = cov(0x9a);
    let ap = AskP { amount: 4 * TOK, ..AskP::new(pk(&f.maker_c), 240_000_000) };
    let mut s = ifd_fill(f, &IfdKnobs { next_armed: Some(1), ..stop_entry(IfdKnobs::new(10 * TOK, 4 * TOK)) });
    // the entry's tokens come from the ask's custody instead of the taker
    let next = match &s.inputs[1].role {
        Role::TokLeader { next, .. } => next.clone(),
        _ => panic!("leader"),
    };
    s.inputs[1] = tok_in(&f.net.t, CARRIER, 4 * TOK, &a.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000);
    let pay = ask_all_in(4 * TOK, ap.price, TIP);
    s.inputs.push(p2pk_in(&f.taker, pay + CARRIER));
    while s.inputs.len() < s.outputs.len() {
        s.inputs.push(p2pk_in(&f.taker, 0));
    }
    let at = s.inputs.len();
    s.inputs.push(call(&f.net.ask(&ap), "settle", vec![nb(4 * TOK), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000));
    s.outputs.push(out(pay + 2 * CARRIER, p2pk_spk(&ap.maker), None));
    set_arg(&mut s, 0, 5, iv(at as i64));
    s
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

/// Touch trigger, KobCondAsk stop leg armed inside its own fill (settle, leg 1, ask evidence).
#[test]
fn v2_touch_cond_settle() {
    touch_battery(&fx8(), Kind::Settle);
}
/// Touch trigger, KobCondAsk stop leg armed by update next to an ask fill.
#[test]
fn v2_touch_cond_arm() {
    touch_battery(&fx8(), Kind::Arm);
}
/// Touch trigger, KobCondAsk trailing ratchet (update next to a bid fill).
#[test]
fn v2_touch_cond_trail() {
    touch_battery(&fx8(), Kind::Trail);
}
/// Touch trigger, KobIfdBid buy-stop entry armed inside its own fill (bid evidence).
#[test]
fn v2_touch_ifd_fill() {
    touch_battery(&fx8(), Kind::IfdFill);
}
/// Touch trigger, KobIfdBid buy-stop entry armed by update next to a bid fill.
#[test]
fn v2_touch_ifd_arm() {
    touch_battery(&fx8(), Kind::IfdArm);
}

/// Merge-value safety of update: a repeat merge requires its partner's sigscript to START with 0x08 (the 8-byte
/// push of a fill argument) and reads bytes [1..9) as a fixed 8-byte script number. The real encoder pushes an
/// update's index arguments minimally, so an exit's update (KobCondAsk.update(ev, tk)) or an entry's update
/// (KobIfdBid.update(ev)) never starts with 0x08 for any index (checked over every small index and the push-size
/// boundaries). Hand-assembled non-minimal 8-byte pushes are the engine-level attacks of `v3_merge_push`.
#[test]
fn v2_update_sigscript_is_never_a_merge_value() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let cond = n.cond(&CondP::oco(m));
    let ifd = n.ifd(&stop_ifd(m, TOK));
    let head = |art: &SilAbiArtifact, args: &[ArtifactValue]| {
        let mut ss = encode_entry_sig_script(art, "update", args).expect("encode update");
        ss.extend_from_slice(&push(&bytecode(art))[..3]);
        ss
    };
    let idx: Vec<i64> = (-1..=260).chain([1_000, 32_767, 32_768, 65_535, 65_536, 8_388_607, 8_388_608]).collect();
    let mut checked = 0;
    for &ev in idx.iter().filter(|&&i| i >= 0) {
        let ss = head(&ifd, &[int(ev)]);
        assert_ne!(ss[0], 0x08, "KobIfdBid.update({ev}) starts with an 8-byte push");
        for &tk in &idx {
            let ss = head(&cond, &[int(ev), int(tk)]);
            assert_ne!(ss[0], 0x08, "KobCondAsk.update({ev}, {tk}) starts with an 8-byte push");
            checked += 1;
        }
    }
    println!("MERGE-VALUE {checked} update sigscripts checked, none starts with the 8-byte push a merge requires");
}

#[test]
fn v2_negative_oco_and_ifd() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let oco = CondP::oco(m);
    let taker = pk(&f.taker);
    let c = cov(0xc1);

    let mut s =
        with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "NO1 two UTXOs share the OCO covenant id (double fill attempt)");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(n, &s, 0);

    let mut s =
        with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "NO2 OCO fill under-reports the remainder (sells 5000, pays 4000)");
    s.outputs[2].script_public_key = n.t.spk(5 * TOK, &c.as_bytes(), SCHEME_COVID);
    let last_tok = s.outputs.len() - 2;
    s.outputs[last_tok].script_public_key = n.t.spk(5 * TOK, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(5 * TOK, &c.as_bytes(), SCHEME_COVID), tok_state(5 * TOK, &taker, SCHEME_P2PK)]);
    run_bad(n, &s, 0);

    let mut s = with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "NO3 OCO continuation drops the stop leg");
    let mut nostop = oco.clone();
    nostop.stop = 0;
    s.outputs[1].script_public_key = spk_of(&n.cond(&nostop));
    run_bad(n, &s, 0);

    let mut s = with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "NO4 OCO terminated while tokens remain");
    s.outputs[1].covenant = None;
    s.outputs[1].script_public_key = p2pk_spk(&taker);
    run_bad(n, &s, 0);

    let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
    let base = || IfdKnobs::new(10 * TOK, 4 * TOK);
    bad(IfdKnobs { deliver: 4 * TOK + 1, ..base() }, "NI1 over-delivery: 4001 base units into the exit for a 4000 fill");
    let mut e = ifd_exit(m);
    e.stop = 0;
    bad(IfdKnobs { exit: Some(e), ..base() }, "NI2 exit order tampered (stop leg removed)");
    let mut e = ifd_exit(m);
    e.slip_bps = 5_000;
    bad(IfdKnobs { exit: Some(e), ..base() }, "NI2b exit order tampered (stop band widened to 50%)");
    bad(IfdKnobs { to_maker: true, ..base() }, "NI3 tokens delivered to the maker instead of the exit custody");
    bad(IfdKnobs { ask_template: true, ..base() }, "NI4 exit built from another template (KobAsk)");
    bad(
        IfdKnobs { exit_delta: -1, ..IfdKnobs::new(6 * TOK, 6 * TOK) },
        "NI5 final fill charged 1 sompi above the all-in price (exit short)",
    );
    bad(IfdKnobs { cont_delta: -1, ..base() }, "NI5b partial fill: entry continuation short by 1 sompi");
    bad(IfdKnobs { exit_delta: -1, ..base() }, "NI5c partial fill: exit carrier short by 1 sompi");
    bad(IfdKnobs { double_exit: true, ..base() }, "NI6 double exit: a second exit output authorised by the entry");
    bad(IfdKnobs { self_id_exit: true, ..base() }, "NI7 exit reuses the entry's covenant id");
    bad(IfdKnobs { foreign_exit: true, ..base() }, "NI8 exit is an attacker covenant continuation (not a fresh genesis)");
    bad(IfdKnobs { cont_amount: Some(10 * TOK), ..base() }, "NI9 entry continuation keeps amountLeft = 10000 (not decremented)");
    bad(
        IfdKnobs { cont_amount: Some(6 * TOK - 1), ..base() },
        "NI9b entry continuation drops a base unit (amountLeft 5999 instead of 6000)",
    );
    bad(IfdKnobs::new(6 * TOK, 6 * TOK + 1), "NI10 fill of 6001 base units with only 6000 left");
    let mut s = with_name(s14(&f), "NI12 exit index malleated to the taker output");
    let last = s.outputs.len() - 1;
    set_arg(&mut s, 0, 2, iv(last as i64));
    run_bad(n, &s, 0);
}

// ---------------------------------------------------------------- lifecycle

/// Market orders as IOC auctions, TWAP slice auctions, and the IOC/FOK kill (KobAsk / KobBid).
#[test]
fn v2_lifecycle_auctions_and_kill() {
    let f = fx();
    let n = &f.net;
    let t = NOW as i64;

    // Market sell: IOC auction from the touch. At t = activeFrom + 100 the quote is 2.4625, and
    // that is what the maker must receive (not the 2.425 bound).
    let m = market_ask(&f);
    let eff = decay_at(m.price, m.price_end, m.slope, 1, m.active_from, t);
    assert_eq!(eff, 246_250_000);
    run_ok(
        n,
        &with_name(
            ioc_fill(&f, &m, 4 * TOK, eff, t),
            "M1 market sell = IOC auction: 4 of 10 tokens filled 100 DAA in at 2.4625, rest returned",
        ),
    );
    let mut over = with_name(ioc_fill(&f, &m, 4 * TOK, m.price_end, t + 500), "M1b market sell auction over: the 3% bound 2.425");
    over.lock_time = NOW + 500;
    run_ok(n, &over);
    run_bad(
        n,
        &with_name(ioc_fill(&f, &m, 4 * TOK, m.price_end, t), "NM1 market sell paid its bound while the auction is at 2.4625"),
        0,
    );
    let mut z = m.clone();
    z.decay_step = 0;
    run_bad(n, &with_name(ioc_fill(&f, &z, 4 * TOK, m.price_end, t), "NM2 decaying order with decayStep 0 is unfillable"), 0);
    let late = decay_at(m.price, m.price_end, m.slope, 1, m.active_from, t + 1);
    run_bad(n, &with_name(ioc_fill(&f, &m, 4 * TOK, late, t + 1), "NM3 auction time above the tx lockTime (CLTV)"), 0);

    // Market buy: IOC rising auction; the delivery carries the unspent budget.
    let b = market_bid(&f);
    let eff = rise_at(b.price, b.price_end, b.slope, 1, b.active_from, t);
    assert_eq!(eff, 253_750_000);
    run_ok(
        n,
        &with_name(s6_with(&f, &b, 4 * TOK, 10 * TOK, eff, t), "M2 market buy = IOC rising auction: 4 tokens 100 DAA in at 2.5375"),
    );
    run_bad(
        n,
        &with_name(
            s6_with(&f, &b, 4 * TOK, 10 * TOK, b.price_end, t),
            "NM4 market buy charged its bound while the auction is at 2.5375",
        ),
        0,
    );

    // TWAP slice auction: every slice opens at UTXO DAA + interval (1000 + 600) at 2.60 and
    // falls 0.10 over 300 DAA to 2.50.
    let mut tw = twap_ask(&f);
    tw.price = P260;
    tw.price_end = P250;
    tw.slope = (P260 - P250) / 300;
    tw.decay_step = 1;
    let eff = decay_at(tw.price, tw.price_end, tw.slope, 1, 1_600, 1_750);
    run_ok(
        n,
        &with_name(
            with_seq(s1_at(&f, &tw, 2 * TOK, eff, 1_750), 0, 600),
            "T6 TWAP slice auction: slice opens at 1600, 2 tokens at t=1750 (2.55)",
        ),
    );
    let from_start = decay_at(tw.price, tw.price_end, tw.slope, 1, 0, 1_750);
    run_bad(
        n,
        &with_name(
            with_seq(s1_at(&f, &tw, 2 * TOK, from_start, 1_750), 0, 600),
            "NV10 TWAP slice priced from activeFrom instead of the slice opening",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(with_seq(s1_at(&f, &tw, 2 * TOK, tw.price, 1_599), 0, 600), "NV11 TWAP slice auction time before the slice opens"),
        0,
    );

    // IOC / FOK kill: refundable by anyone IOC_LIFE (600 DAA) after max(UTXO DAA, activeFrom).
    run_ok(n, &with_name(ask_refund(&f, 1, 0, 1_600), "S8i IOC ask killed by anyone 600 DAA after placement"));
    run_bad(n, &with_name(ask_refund(&f, 1, 0, 1_599), "NC8 IOC ask kill one DAA early"), 0);
    run_bad(n, &with_name(ask_refund(&f, 0, 0, 1_600), "NC9 GTC ask is not killable at the IOC deadline"), 0);
    run_ok(n, &with_name(ask_refund(&f, 2, 5_000, 5_600), "S8k timed FOK ask killed 600 DAA after activeFrom 5000"));
    run_bad(n, &with_name(ask_refund(&f, 2, 5_000, 5_599), "NC8b timed FOK ask kill one DAA before activeFrom + 600"), 0);
    run_ok(n, &with_name(bid_refund(&f, 1, 1_600), "S9i IOC bid killed by anyone 600 DAA after placement"));
    run_ok(n, &with_name(bid_refund(&f, 2, 1_600), "S9k FOK bid killed by anyone 600 DAA after placement"));
    run_bad(n, &with_name(bid_refund(&f, 1, 1_599), "NC10 IOC bid kill one DAA early"), 0);
    run_bad(n, &with_name(bid_refund(&f, 0, 1_600), "NC10b GTC bid is not killable at the IOC deadline"), 0);
}

/// Stop-leg auction, keeper tip and multi-step trailing (KobCondAsk).
#[test]
fn v2_lifecycle_stop_auction_keeper_trailing() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let a = auction_oco(m);
    let r = down_ev();
    let stop = a.stop;
    let fill = |cp: &CondP, leg: i64, next: &CondP, ev: Option<&Ev>, t: i64, price: i64, name: &str| {
        with_name(cond_fill_at(&f, cp, 4 * TOK, leg, next, ev, t, price), name)
    };

    // Armed by `update` in a transaction at DAA 2000 (armed = 1 on the UTXO of DAA 2000).
    let half = stop_auction(stop, 300, 300, 150);
    assert_eq!(half, 197_000_000);
    let a1 = with_armed(&a, 1);
    let a2000 = with_armed(&a, 2_000);
    run_ok(
        n,
        &fill(&a1, 1, &a2000, None, 2_150, half, "S17 stop auction 150/300 DAA after arming: 1.97, remainder keeps origin 2000"),
    );
    let a1900 = with_armed(&a, 1_900);
    let p250 = stop_auction(stop, 300, 300, 250);
    run_ok(n, &fill(&a1900, 1, &a1900, None, 2_150, p250, "S17b stored origin 1900 (after a partial fill): 250/300 DAA -> 1.95"));
    run_ok(n, &fill(&a1, 1, &a2000, None, 2_400, stop_leg(stop, 300), "S17c auction over: the band floor 1.94"));
    run_ok(n, &fill(&a, 1, &a1, Some(&r), 0, stop, "S17d arm + fill in one tx (auction starts at the trigger): pays the stop 2.00"));
    run_ok(
        n,
        &with_name(cond_fill(&f, &a1, 4 * TOK, 0, &a2000, None), "S17e TP-leg fill of an armed auction order carries the origin"),
    );
    run_bad(n, &fill(&a1, 1, &a2000, None, 2_150, stop_leg(stop, 300), "NA1 stop auction paid the band floor at 150/300 DAA"), 0);
    run_bad(n, &fill(&a1, 1, &a2000, None, NOW as i64 + 1, stop_leg(stop, 300), "NA2 auction time above the tx lockTime (CLTV)"), 0);
    // priced consistently at t = origin - 1 (bps -1: above the stop), so only the time check rejects
    run_bad(n, &fill(&a1, 1, &a2000, None, 1_999, stop + stop / 10_000, "NA3 auction time before the arming origin"), 0);
    run_bad(n, &fill(&a1, 1, &a1, None, 2_150, half, "NA4 continuation restarts the auction (armed = 1, not origin 2000)"), 0);
    run_bad(n, &fill(&a, 1, &a1, Some(&r), 0, stop_leg(stop, 300), "NA5 arm + fill in one tx (auction) paid the floor"), 0);
    let a2100 = with_armed(&a, 2_100);
    run_bad(n, &fill(&a1900, 1, &a2100, None, 2_150, p250, "NA6 continuation moves the stored origin (1900 -> 2100)"), 0);

    // Keeper tip on update (arm or trail), paid from the order's carrier.
    let mut kt = CondP::oco(m);
    kt.keeper_tip = 1_000_000;
    run_ok(n, &with_name(cond_update_tip(&f, &kt, &armed(&kt), &r, 0, 1_000_000), "S13k arming keeper paid keeperTip by the order"));
    run_bad(n, &with_name(cond_update_tip(&f, &kt, &armed(&kt), &r, 0, 1_000_001), "NK1 arming keeper takes keeperTip + 1"), 0);
    let mut neg = CondP::oco(m);
    neg.keeper_tip = -1;
    run_bad(
        n,
        &with_name(
            cond_update_tip(&f, &neg, &armed(&neg), &r, 0, -1),
            "NK2 negative keeperTip refused (even when the keeper tops the order up)",
        ),
        0,
    );
    let mut tr = trailing(m);
    tr.keeper_tip = 1_000_000;
    let mut tr2 = tr.clone();
    tr2.stop += 2 * tr.step;
    run_ok(n, &with_name(cond_update_tip(&f, &tr, &tr2, &up_ev(), tr.wait as u64, 1_000_000), "S15k trailing keeper paid keeperTip"));

    // Multi-step trailing, capped below the take-profit.
    let tr = trailing(m);
    let up = Ev::bid(350_000_000);
    let mut capped = tr.clone();
    capped.stop = 295_000_000;
    run_ok(
        n,
        &with_name(
            cond_update(&f, &tr, &capped, &up, tr.wait as u64),
            "S15c trail jump 2.00 -> 2.95 (28 steps justified, capped below TP)",
        ),
    );
    let mut over = tr.clone();
    over.stop = 340_000_000;
    run_bad(n, &with_name(cond_update(&f, &tr, &over, &up, tr.wait as u64), "NT18b trail jump not capped (stop 3.40 >= TP)"), 0);
}

/// Wrong-side evidence for the buy-stop entry: a resting ASK quoting 2.30 (above the 2.20 stop) filled.
fn side1_of() -> Ev {
    Ev::ask(230_000_000)
}

/// Stop IFD / IFO entries and minFill (KobIfdBid).
#[test]
fn v2_lifecycle_ifd_stop_entry() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let up = up_ev(); // a resting bid quoting 2.20 is filled in the same transaction
    let armed_in = |k: IfdKnobs| IfdKnobs { ev: Some(up.clone()), next_armed: Some(1), ..k };
    let auction = |k: IfdKnobs| stop_entry(IfdKnobs { band_daa: 1_000, ..k });
    let base = || IfdKnobs::new(10 * TOK, 4 * TOK);

    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &armed_in(stop_entry(base()))),
            "S18 buy-stop IFO entry (stop 2.20, limit 2.60) armed inside the fill: 4 of 10 tokens at the limit, continues armed",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { armed: 1, t: 1_500, eff: Some(240_000_000), next_armed: Some(1_000), ..auction(base()) }),
            "S18b armed stop entry auction 500/1000 DAA: buys at 2.40, remainder keeps origin 1000",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { eff: Some(220_000_000), ..armed_in(auction(base())) }),
            "S18c arm + fill in one tx (auction starts at the trigger): pays the stop 2.20",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { armed: 1_000, t: 1_500, eff: Some(240_000_000), ..auction(IfdKnobs::new(6 * TOK, 6 * TOK)) }),
            "S18d final fill of an auction entry (stored origin)",
        ),
    );
    let mut ip = IfdP::limit(m, 10 * TOK, P260, ifd_exit(m));
    ip.entry_stop = 220_000_000;
    ip.keeper_tip = 1_000_000;
    let mut ip_armed = ip.clone();
    ip_armed.armed = 1;
    run_ok(
        n,
        &with_name(ifd_update(&f, &ip, &ip_armed, &up, 1_000_000), "S18e permissionless arm of a stop entry, keeper paid keeperTip"),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { min_fill: 3 * TOK, ..IfdKnobs::new(10 * TOK, 3 * TOK) }),
            "S18f minFill 3000: a 3000 partial fill",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { min_fill: 3 * TOK, ..IfdKnobs::new(2 * TOK, 2 * TOK) }),
            "S18g minFill 3000: the final 2000 may fill",
        ),
    );

    let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
    bad(IfdKnobs { next_armed: Some(1), ..stop_entry(base()) }, "NI13 stop entry filled without evidence");
    bad(armed_in(auction(base())), "NI16b arm + fill in one tx (auction) charged the limit 2.60 instead of the stop");
    bad(
        IfdKnobs { ev: Some(side1_of()), next_armed: Some(1), ..stop_entry(base()) },
        "NI14 buy-stop entry armed by a resting-ASK fill (wrong side)",
    );
    let low = up.with(|e| e.price = 219_999_999);
    bad(IfdKnobs { ev: Some(low), next_armed: Some(1), ..stop_entry(base()) }, "NI15 evidence bid quoting below the entry stop");
    let young = up.with(|e| e.daa = NOW - 599);
    bad(
        IfdKnobs { ev: Some(young), next_armed: Some(1), ..stop_entry(base()) },
        "NI15b evidence bid exposed R - 1 DAA (a fresh print)",
    );
    bad(
        IfdKnobs { armed: 1, t: 1_500, next_armed: Some(1_000), ..auction(base()) },
        "NI16 auction entry charged its limit 2.60 at 500/1000 DAA",
    );
    bad(
        IfdKnobs { armed: 1, t: 1_500, eff: Some(240_000_000), next_armed: Some(1), ..auction(base()) },
        "NI17 continuation restarts the entry auction (armed = 1)",
    );
    bad(IfdKnobs { next_armed: Some(0), ..armed_in(stop_entry(base())) }, "NI17b continuation drops the armed state");
    bad(IfdKnobs { min_fill: 3 * TOK, ..IfdKnobs::new(10 * TOK, 2 * TOK) }, "NI18 partial fill below minFill");
    let high = up.with(|e| e.price = 275_000_000);
    bad(
        IfdKnobs { entry_stop: 270_000_000, ev: Some(high), next_armed: Some(1), ..base() },
        "NI19 stop entry with its stop above its limit is unfillable",
    );
    let mut lim = IfdP::limit(m, 10 * TOK, P260, ifd_exit(m));
    lim.keeper_tip = 1_000_000;
    let mut lim_armed = lim.clone();
    lim_armed.armed = 1;
    run_bad(n, &with_name(ifd_update(&f, &lim, &lim_armed, &up, 0), "NI20 arm update on a limit entry"), 0);
    run_bad(n, &with_name(ifd_update(&f, &ip, &ip_armed, &up, 1_000_001), "NI21 arming keeper takes keeperTip + 1"), 0);
    run_bad(n, &with_name(ifd_update(&f, &ip, &ip_armed, &side1_of(), 0), "NI22 arm update from a wrong-side (ask) fill"), 0);
    let mut ip_amount = ip_armed.clone();
    ip_amount.amount = 9 * TOK;
    run_bad(n, &with_name(ifd_update(&f, &ip, &ip_amount, &up, 0), "NI23 arm update also changes amountLeft"), 0);
}

/// A token UTXO of `amount` base units owned by covenant id `owner` (scheme 0x04), as a KCC-20 delegator
/// (or leader when `next` is given).
fn owned_tok(f: &Fx, amount: i64, owner: Hash, next: Option<Vec<ArtifactValue>>) -> Inp {
    tok_in(&f.net.t, CARRIER, amount, &owner.as_bytes(), SCHEME_COVID, TOKEN_COV, next, Wit::CovId, 1_500)
}

/// Stray custody UTXOs (tokens sent to an order's covenant id outside the protocol): they can
/// neither stand in for the custody nor be co-spent by anyone but the maker.
#[test]
fn v2_lifecycle_stray_custody() {
    let f = fx();
    let n = &f.net;
    let a = cov(0xa1);
    let m = pk(&f.maker_a);
    let taker = pk(&f.taker);
    let tk = &n.t;
    let ask = n.ask(&AskP::new(m, P250));

    // NS1: a 1000-base-unit dust UTXO sent to the ask's id stands in as tokenIn; the real 10000 custody is
    // co-spent (KCC-20 authorises it through the ask input) and routed to the filler.
    let steal = |with_custody: bool, name: &str| {
        let got = if with_custody { 11 * TOK } else { TOK };
        let mut inputs = vec![
            call(&ask, "settle", vec![nb(TOK), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000),
            owned_tok(&f, TOK, a, Some(vec![tok_state(got, &taker, SCHEME_P2PK)])),
        ];
        if with_custody {
            inputs.push(owned_tok(&f, 10 * TOK, a, None));
        }
        inputs.push(p2pk_in(&f.taker, 1000 * KAS));
        let pay = ask_all_in(TOK, P250, TIP);
        Scn {
            name: name.into(),
            inputs,
            outputs: vec![
                out(pay + 2 * CARRIER, p2pk_spk(&m), None),
                out(CARRIER, tk.spk(got, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
                out(1000 * KAS - pay - 2 * CARRIER - NET_FEE + if with_custody { CARRIER } else { 0 }, p2pk_spk(&taker), None),
            ],
            lock_time: NOW,
            payload: vec![],
        }
    };
    run_bad(n, &steal(true, "NS1 1000-base-unit dust stands in for the custody; the 10000 custody is routed to the filler"), 0);
    run_bad(n, &steal(false, "NS2 1000-base-unit dust sold out to terminate the ask and orphan its 10000 custody"), 0);

    // NS3: the real custody as tokenIn, plus a 3000 stray co-spent to the filler.
    let mut s = with_name(s1(&f), "NS3 fill co-spends a 3000-base-unit stray owned by the ask (to the filler)");
    s.inputs.insert(2, owned_tok(&f, 3 * TOK, a, None));
    s.outputs[3].script_public_key = tk.spk(7 * TOK, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6 * TOK, &a.as_bytes(), SCHEME_COVID), tok_state(7 * TOK, &taker, SCHEME_P2PK)]);
    let l = s.outputs.len() - 1;
    s.outputs[l].value -= CARRIER as u64;
    s.outputs.push(out(2 * CARRIER, p2pk_spk(&taker), None));
    run_bad(n, &s, 0);

    // NS4: a refund keeper (anyone) co-spends a stray.
    let keeper = keypair();
    let mut s =
        with_name(s8(&f, NO_EXPIRY, (1_000 + MAX_IDLE) as u64), "NS4 refund keeper co-spends a 3000-base-unit stray (to itself)");
    s.inputs.push(owned_tok(&f, 3 * TOK, a, None));
    set_leader_next(&mut s, 1, vec![tok_state(10 * TOK, &m, SCHEME_P2PK), tok_state(3 * TOK, &pk(&keeper), SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(3 * TOK, &pk(&keeper), SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);

    // S19: only the maker's cancel (SIGHASH_ALL: the maker routes every output) moves strays.
    let mut s = with_name(s7(&f, 0x01, f.maker_a), "S19 maker cancel sweeps the custody and a 3000-base-unit stray back to the maker");
    s.inputs.push(owned_tok(&f, 3 * TOK, a, None));
    s.outputs[0].script_public_key = tk.spk(13 * TOK, &m, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(13 * TOK, &m, SCHEME_P2PK)]);
    s.outputs[1].value += CARRIER as u64;
    run_ok(n, &s);

    // NS5 / NS6: strays owned by a bid's id.
    let b = cov(0xb1);
    let mut s = with_name(s6(&f, 1), "NS5 bid fill co-spends a 3000-base-unit stray owned by the bid (to the taker)");
    s.inputs.push(owned_tok(&f, 3 * TOK, b, None));
    let mb = pk(&f.maker_b);
    set_leader_next(&mut s, 1, vec![tok_state(4 * TOK, &mb, SCHEME_P2PK), tok_state(3 * TOK, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(3 * TOK, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);
    let mut s = with_name(
        s9(&f, "refund", NO_EXPIRY, (1_000 + MAX_IDLE) as u64),
        "NS6 bid refund co-spends a 3000-base-unit stray owned by the bid",
    );
    s.inputs.push(owned_tok(&f, 3 * TOK, b, Some(vec![tok_state(3 * TOK, &pk(&keeper), SCHEME_P2PK)])));
    s.outputs.push(out(CARRIER, tk.spk(3 * TOK, &pk(&keeper), SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);

    // NS7: conditional sell / IFD exit (KobCondAsk, cov 0xc1): stray co-spent on a TP fill, and
    // dust standing in for the custody.
    let c = cov(0xc1);
    let oco = CondP::oco(m);
    let mut s =
        with_name(cond_fill(&f, &oco, 4 * TOK, 0, &oco, None), "NS7 conditional (IFD exit) TP fill co-spends a 2000-base-unit stray");
    s.inputs.push(owned_tok(&f, 2 * TOK, c, None));
    set_leader_next(&mut s, 1, vec![tok_state(6 * TOK, &c.as_bytes(), SCHEME_COVID), tok_state(6 * TOK, &taker, SCHEME_P2PK)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = tk.spk(6 * TOK, &taker, SCHEME_P2PK);
    s.outputs.push(out(CARRIER, p2pk_spk(&taker), None));
    run_bad(n, &s, 0);
    let cond = n.cond(&oco);
    let pay = ask_all_in(TOK, oco.tp, TIP);
    let s = Scn {
        name: "NS7b 1000-base-unit dust sold out on the TP leg to terminate a 10000 conditional (orphaning its custody)".into(),
        inputs: vec![
            call(&cond, "settle", vec![nb(TOK), iv(1), iv(0), iv(0), iv(0), iv(0), iv(0)], "cond.settle", CARRIER, c, 2_000),
            owned_tok(&f, TOK, c, Some(vec![tok_state(TOK, &taker, SCHEME_P2PK)])),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay + 2 * CARRIER, p2pk_spk(&m), None),
            out(CARRIER, tk.spk(TOK, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - pay - 2 * CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    run_bad(n, &s, 0);

    // NS8: if-done entry (cov 0xd1) fill co-spends a stray owned by the entry.
    let mut s = with_name(s14(&f), "NS8 if-done entry fill co-spends a 3000-base-unit stray owned by the entry");
    s.inputs.push(owned_tok(&f, 3 * TOK, cov(0xd1), None));
    let exit_owner = s.outputs[0].script_public_key.clone();
    let exit_id = s
        .outputs
        .iter()
        .find_map(|o| o.covenant.as_ref().filter(|b| b.covenant_id != TOKEN_COV && b.covenant_id != cov(0xd1)).map(|b| b.covenant_id))
        .unwrap();
    set_leader_next(&mut s, 1, vec![tok_state(4 * TOK, &exit_id.as_bytes(), SCHEME_COVID), tok_state(3 * TOK, &taker, SCHEME_P2PK)]);
    assert_eq!(exit_owner, tk.spk(4 * TOK, &exit_id.as_bytes(), SCHEME_COVID));
    s.outputs.push(out(CARRIER, tk.spk(3 * TOK, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);
}

// ---------------------------------------------------------------- repeat IFD, buy-first

/// Covenant id of the repeating if-done bid in the fixtures (as in ifd_fill).
const RPT_ENTRY: u8 = 0xd1;
/// The budget rate a re-armed amount needs again (the entry's all-in price per whole token at its limit 2.60).
fn rpt_rate() -> i64 {
    P260 + TIP
}
/// The exit a fill of `amount` books from the entry `ip` at UTXO DAA 1000 with t = 0.
fn booked_from(ip: &IfdP, amount: i64) -> CondP {
    CondP {
        amount,
        parent: cov(RPT_ENTRY).as_bytes(),
        rpt_price: ip.rate(),
        rpt_until: EXPIRY.min(1_000 + MAX_IDLE),
        ..ip.exit.clone()
    }
}
/// The exit a fill of `amount` books from the fixture entry (limit 2.60, tip TIP, exit ifd_exit).
fn booked_exit(maker: [u8; 32], amount: i64) -> CondP {
    booked_from(&rpt_entry(maker, 0, 0), amount)
}
/// A repeating limit entry (quote 2.60) with `amount` base units left and rptAmount `rpt`.
fn rpt_entry(maker: [u8; 32], amount: i64, rpt: i64) -> IfdP {
    IfdP { rpt, ..IfdP::limit(maker, amount, P260, ifd_exit(maker)) }
}
/// Merge argument -(k * 2^53 + m) as an 8-byte script number (sign-magnitude, little endian).
fn nb_merge(k: i64, m: i64) -> Arg {
    let mut b = (k * MERGE_K + m).to_le_bytes();
    b[7] |= 0x80;
    Arg::V(bytes(&b))
}
/// Sets output `idx` to whatever balances the transaction after NET_FEE (the taker's change).
fn balance(s: &mut Scn, idx: usize) {
    let ins: i64 = s.inputs.iter().map(|i| i.entry.amount as i64).sum();
    let outs: i64 = s.outputs.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, o)| o.value as i64).sum();
    s.outputs[idx].value = (ins - outs - NET_FEE) as u64;
}
/// Tops up input `funder` so that output `idx` (the funder's change) balances with at least `keep` sompi.
fn fund_and_balance(s: &mut Scn, funder: usize, idx: usize, keep: i64) {
    let ins: i64 = s.inputs.iter().enumerate().filter(|(i, _)| *i != funder).map(|(_, x)| x.entry.amount as i64).sum();
    let outs: i64 = s.outputs.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, o)| o.value as i64).sum();
    let need = (outs + NET_FEE + keep - ins).max(keep);
    s.inputs[funder].entry.amount = need as u64;
    balance(s, idx);
}

/// Knobs of a take-profit fill of a booked exit that re-arms its entry (single-lever mutations).
#[derive(Clone)]
struct MergeKnobs {
    /// entry parameters before the merge (amountLeft, armed, rptAmount) and its UTXO value
    entry: IfdP,
    entry_value: i64,
    /// the exit (default: booked_from the entry), its amountLeft, the amount it sells, its leg
    exit: Option<CondP>,
    exit_amount: i64,
    n: i64,
    leg: i64,
    /// the entry input is present (merge) or absent
    with_entry: bool,
    /// what the entry's merge argument claims: exit index and amount (default 0, n)
    claim_k: i64,
    claim_m: Option<i64>,
    /// continuation of the entry (default: amountLeft + m, armed reset when it had none)
    cont: Option<IfdP>,
    cont_delta: i64,
    maker_delta: i64,
    lock_time: u64,
}
impl MergeKnobs {
    fn new(maker: [u8; 32], entry_amount: i64, exit_amount: i64, n: i64) -> Self {
        Self::of(rpt_entry(maker, entry_amount, 21 * TOK), exit_amount, n)
    }
    /// A merge into the entry `entry` (escrow: ifd_value + its own carrier).
    fn of(entry: IfdP, exit_amount: i64, n: i64) -> Self {
        MergeKnobs {
            entry_value: ifd_value(&entry) + EC,
            entry,
            exit: None,
            exit_amount,
            n,
            leg: 0,
            with_entry: true,
            claim_k: 0,
            claim_m: None,
            cont: None,
            cont_delta: 0,
            maker_delta: 0,
            lock_time: NOW,
        }
    }
}
/// A taker buys n base units from a booked exit (KobCondAsk, cov 0xc1, UTXO DAA 2000) on `leg` at its
/// all-in price and the entry (KobIfdBid, cov 0xd1) re-arms them in the same transaction.
/// Inputs: [0] exit, [1] exit custody, [2] entry (merge), [3] taker. Outputs: [0] maker profit,
/// then (exit amount left) exit continuation and token remainder, the entry continuation, the
/// taker's tokens and the taker's change.
fn rpt_merge(f: &Fx, k: &MergeKnobs) -> Scn {
    let tk = &f.net.t;
    let m = pk(&f.maker_a);
    let c = cov(0xc1);
    let e = cov(RPT_ENTRY);
    let taker = pk(&f.taker);
    let xp = k.exit.clone().unwrap_or_else(|| booked_from(&k.entry, k.exit_amount));
    let xp = CondP { amount: k.exit_amount, ..xp };
    let leg_price = if k.leg == 0 { xp.tp } else { stop_leg(xp.stop, xp.slip_bps) };
    let all_in = ask_all_in(k.n, leg_price, xp.tip);
    let rest = k.exit_amount - k.n;
    let claim = k.claim_m.unwrap_or(k.n);
    let rearm = k.with_entry && k.leg == 0 && xp.parent != [0; 32];
    let maker_pay = if rearm { all_in - q_up(k.n, xp.rpt_price) } else { all_in + if rest == 0 { 2 * CARRIER } else { 0 } };
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest, &c.as_bytes(), SCHEME_COVID));
    }
    next.push(tok_state(k.n, &taker, SCHEME_P2PK));
    let mut s = Scn {
        name: "rpt merge".into(),
        inputs: vec![
            call(
                &f.net.cond(&xp),
                "settle",
                vec![nb(k.n), iv(1), iv(2), iv(k.leg), iv(0), iv(0), iv(0)],
                "cond.settle",
                CARRIER,
                c,
                2_000,
            ),
            tok_in(tk, CARRIER, k.exit_amount, &c.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 2_000),
        ],
        outputs: vec![out(maker_pay + k.maker_delta, p2pk_spk(&m), None)],
        lock_time: k.lock_time,
        payload: vec![],
    };
    if rest > 0 {
        s.outputs.push(out(CARRIER, spk_of(&f.net.cond(&CondP { amount: rest, ..xp.clone() })), Some((0, c))));
        s.outputs.push(out(CARRIER, tk.spk(rest, &c.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))));
    }
    if k.with_entry {
        let ip = &k.entry;
        s.inputs.push(call(
            &f.net.ifd(ip),
            "fill",
            vec![nb_merge(k.claim_k, claim), iv(1), iv(0), Arg::V(bytes(&[])), Arg::V(bytes(&[])), iv(0), iv(0)],
            "ifd.merge",
            k.entry_value,
            e,
            1_000,
        ));
        // armed: reset when the entry had nothing left; an entry armed by update (1) with a band records its origin,
        // the entry UTXO's DAA (1_000), as a fill does
        let armed = if ip.amount == 0 {
            0
        } else if ip.armed == 1 && ip.band_daa > 0 {
            1_000
        } else {
            ip.armed
        };
        let cont = k.cont.clone().unwrap_or(IfdP { amount: ip.amount + claim, armed, ..ip.clone() });
        let back = q_up(claim, ip.rate()) + if rearm && rest == 0 { 2 * CARRIER } else { 0 };
        s.outputs.push(out(k.entry_value + back + k.cont_delta, spk_of(&f.net.ifd(&cont)), Some((2, e))));
    }
    s.inputs.push(p2pk_in(&f.taker, 1000 * KAS));
    s.outputs.push(out(CARRIER, tk.spk(k.n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let (ti, last) = (s.inputs.len() - 1, s.outputs.len() - 1);
    fund_and_balance(&mut s, ti, last, 1000 * KAS);
    s
}

/// A crafted P2SH input that imitates a booked exit of `parent` (not a KobCondAsk): a redeem
/// script of the KobCondAsk length whose would-be state holds `parent` at the parent offset,
/// executing as "drop, drop, true"; its first sigscript push is 0x08 || m.
fn fake_exit(f: &Fx, parent: Hash, m: i64, value: i64) -> Inp {
    let size = f.net.cond_tpl.pre.len() + 363 + f.net.cond_tpl.suf.len();
    let data_len = size - 6;
    let mut data = vec![0u8; data_len];
    // state span = redeem[1..331); parent at state [280..312) = redeem [281..313) = data [278..310)
    data[278..310].copy_from_slice(&parent.as_bytes());
    let mut redeem = vec![0x4d, (data_len & 0xff) as u8, (data_len >> 8) as u8];
    redeem.extend(data);
    redeem.extend([0x75, 0x75, 0x51]);
    assert_eq!(redeem.len(), size);
    let mut ss = vec![0x08];
    ss.extend(m.to_le_bytes());
    ss.extend(push(&redeem));
    Inp {
        entry: UtxoEntry::new(value as u64, pay_to_script_hash_script(&redeem), 2_000, false, None),
        role: Role::Raw { ss, name: "fake.exit" },
        seq: 0,
    }
}

/// Repeat IFD, buy-first (KobIfdBid entry, KobCondAsk exits): booking, re-arming (merge) on the
/// take-profit, sell-out carrier return, the rptUntil fallback, stop-loss exits, exhausted
/// cycles, stop entries, and a three-cycle ledger with exact accounting.
#[test]
fn v2_repeat_ifd_positive() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);

    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 21 * TOK, ..IfdKnobs::new(10 * TOK, 4 * TOK) }),
            "RP1 repeating IFO entry fills 4 of 10 tokens: the exit is booked (parent = entry, rptPrice, rptUntil), rptAmount 21000 -> 17000",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 17 * TOK, ..IfdKnobs::new(6 * TOK, 6 * TOK) }),
            "RP2 repeating entry sold out 6 of 6 tokens: it stays (amountLeft 0, rptAmount 11000) and waits for its exits",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 1, ..IfdKnobs::new(10 * TOK, 4 * TOK) }),
            "RP3 last cycle: the re-arms are used up (rptAmount 1), the exit is a plain exit (parent 0)",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 21 * TOK, t: 5_000, ..IfdKnobs::new(10 * TOK, 4 * TOK) }),
            "RP3b the fill's time argument t does not date the cycle: rptUntil = entry UTXO DAA + 90 days",
        ),
    );
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK)),
            "RP4 booked exit sells 3 of 4 tokens on its take-profit: the entry re-arms 3000 (6000 -> 9000) with their budget, the maker keeps the profit",
        ),
    );
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK)),
            "RP5 booked exit sells out 4 of 4 tokens: the empty entry re-arms 4000, both exit carriers come back",
        ),
    );
    let mut stop_entry_p = rpt_entry(m, 0, 21 * TOK);
    stop_entry_p.entry_stop = 220_000_000;
    stop_entry_p.armed = 1;
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { entry: stop_entry_p.clone(), ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) }),
            "RP5b an armed stop entry with nothing left re-arms unarmed (a new cycle waits for a new trigger)",
        ),
    );
    let mut stop_part = rpt_entry(m, 3 * TOK, 21 * TOK);
    stop_part.entry_stop = 220_000_000;
    stop_part.armed = 1_000;
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs::of(stop_part, 4 * TOK, 2 * TOK)),
            "RP5c re-armed amount joins a running stop-entry auction (amount left: armed origin kept)",
        ),
    );
    let until = EXPIRY.min(1_000 + MAX_IDLE);
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { with_entry: false, lock_time: until as u64, ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) }),
            "RP6 from rptUntil on a booked exit may take profit without its entry (entry expired or idle)",
        ),
    );
    let mut sl = booked_exit(m, 4 * TOK);
    sl.armed = 1;
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { exit: Some(sl), leg: 1, with_entry: false, ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) }),
            "RP7 stop-loss of a booked exit: plain, proceeds and carriers to the maker, no re-arm",
        ),
    );
    // A re-armed stop entry is armed again by a keeper (update keeps rptAmount).
    let mut ip = rpt_entry(m, 10 * TOK, 21 * TOK);
    ip.entry_stop = 220_000_000;
    ip.keeper_tip = 1_000_000;
    let armed_ip = IfdP { armed: 1, ..ip.clone() };
    run_ok(
        n,
        &with_name(
            ifd_update(&f, &ip, &armed_ip, &up_ev(), 1_000_000),
            "RP8 a re-armed stop entry is armed again by a paid keeper (rptAmount kept)",
        ),
    );
    // The entry's refund after its expiry (amountLeft 0: only its carrier is left).
    let e = rpt_entry(m, 0, 1);
    let s = Scn {
        name: "RP9 a repeating entry with nothing left is refunded at its soft expiry".into(),
        inputs: vec![call(&n.ifd(&e), "refund", vec![], "ifd.refund", EC, cov(RPT_ENTRY), (EXPIRY - 1_000) as u64)],
        outputs: vec![out(EC - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time: EXPIRY as u64,
        payload: vec![],
    };
    run_ok(n, &s);

    // Three cycles of a 4-token position (rptAmount 9000 = 2 re-arm cycles of 4000), each fill and take-profit
    // at the limits, values carried from one transaction to the next: the entry is restored exactly after
    // every cycle and the maker earns exactly the spread per cycle.
    let v0 = ifd_value(&rpt_entry(m, 4 * TOK, 9 * TOK)) + EC;
    let profit = ask_all_in(4 * TOK, ifd_exit(m).tp, TIP) - q_up(4 * TOK, rpt_rate());
    let mut v = v0;
    let mut rpt = 9 * TOK;
    let mut maker_total = 0;
    for cycle in 1..=3 {
        let booked = rpt > 4 * TOK;
        let mut fk = IfdKnobs { rpt, ..IfdKnobs::new(4 * TOK, 4 * TOK) };
        if !booked {
            fk.exit_rpt = Some(([0; 32], 0, 0));
        }
        let mut s = ifd_fill(&f, &fk);
        s.inputs[0].entry = UtxoEntry::new(v as u64, s.inputs[0].entry.script_public_key.clone(), 1_000, false, Some(cov(RPT_ENTRY)));
        let cont = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPT_ENTRY))).unwrap();
        s.outputs[cont].value = (v - bid_all_in(4 * TOK, P260, TIP) - DC - EC) as u64;
        let name = format!("RP10 ledger cycle {cycle}: entry buys 4 of 4 tokens (value {v}, rptAmount {rpt}, booked {booked})");
        run_ok(n, &with_name(s.clone(), &name));
        v = s.outputs[cont].value as i64;
        assert_eq!(v, v0 - q_up(4 * TOK, rpt_rate()) - DC - EC, "entry after its fill");
        if booked {
            rpt -= 4 * TOK;
            let mut mk = MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK);
            mk.entry = rpt_entry(m, 0, rpt);
            mk.entry_value = v;
            let s = rpt_merge(&f, &mk);
            let name = format!("RP10 ledger cycle {cycle}: take-profit 4 of 4 tokens re-arms the entry");
            run_ok(n, &with_name(s.clone(), &name));
            let back = s.outputs.iter().find(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPT_ENTRY))).unwrap();
            v = back.value as i64;
            assert_eq!(v, v0, "entry value restored exactly after cycle {cycle}");
            assert_eq!(s.outputs[0].value as i64, profit, "maker profit of cycle {cycle}");
            maker_total += s.outputs[0].value as i64;
        } else {
            let mut mk = MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK);
            mk.exit = Some(CondP { amount: 4 * TOK, ..ifd_exit(m) });
            mk.with_entry = false;
            let s = rpt_merge(&f, &mk);
            run_ok(n, &with_name(s.clone(), &format!("RP10 ledger cycle {cycle}: plain take-profit, no re-arm left")));
            maker_total += s.outputs[0].value as i64;
        }
    }
    assert_eq!(v, v0 - q_up(4 * TOK, rpt_rate()) - DC - EC, "after the last cycle the entry keeps its unused carriers");
    assert_eq!(maker_total, 2 * profit + ask_all_in(4 * TOK, ifd_exit(m).tp, TIP) + 2 * CARRIER, "maker receipts over 3 cycles");
    println!("LEDGER v0={v0} profit/cycle={profit} maker_total={maker_total}");
}

/// Single-lever attacks on repeat IFD, buy-first. Each is rejected by exactly one check; the
/// ablation runs (KOB_ABLATION=1 with that check removed) accept the same transaction.
#[test]
fn v2_repeat_ifd_attacks() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);
    let taker = pk(&f.taker);
    let tk = &n.t;
    let until = EXPIRY.min(1_000 + MAX_IDLE);

    // ---- booking (entry fill)
    let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
    let base = || IfdKnobs { rpt: 21 * TOK, ..IfdKnobs::new(10 * TOK, 4 * TOK) };
    bad(IfdKnobs { exit_rpt: Some(([0; 32], 0, 0)), ..base() }, "NRP1 booked exit written as a plain exit (re-arm skipped)");
    bad(
        IfdKnobs { exit_rpt: Some((e.as_bytes(), rpt_rate(), until)), rpt: 4 * TOK, ..IfdKnobs::new(10 * TOK, 4 * TOK) },
        "NRP2 exit booked although the re-arms are exhausted (rptAmount 4000, fill 4000)",
    );
    bad(IfdKnobs { next_rpt: Some(21 * TOK), ..base() }, "NRP3 continuation keeps rptAmount (cycle count not decremented)");
    bad(IfdKnobs { next_rpt: Some(0), ..base() }, "NRP3b continuation drops the repeat (rptAmount 0)");
    bad(
        IfdKnobs { exit_rpt: Some((e.as_bytes(), rpt_rate() - 1, until)), ..base() },
        "NRP4 exit's re-arm budget rate rptPrice lowered by 1 sompi (skims every cycle)",
    );
    bad(
        IfdKnobs { exit_rpt: Some((e.as_bytes(), rpt_rate(), until - 1)), ..base() },
        "NRP5 exit's rptUntil shortened (re-arm skippable early)",
    );
    bad(
        IfdKnobs { t: 5_000, exit_rpt: Some((e.as_bytes(), rpt_rate(), until + 4_000)), ..base() },
        "NRP6 exit's rptUntil dated by the filler's t instead of the entry UTXO's DAA",
    );
    bad(
        IfdKnobs { rpt: 4 * TOK, ..IfdKnobs::new(10 * TOK, 4 * TOK) },
        "NRP6b a fill of 4000 with 3999 re-arms left (at least a minimum fill): a plain exit would leave them unused",
    );
    bad(
        IfdKnobs { rpt: 17 * TOK, terminate: true, ..IfdKnobs::new(6 * TOK, 6 * TOK) },
        "NRP7 repeating entry terminated at its final fill (everything to the exit)",
    );
    bad(
        IfdKnobs { rpt: 17 * TOK, cont_delta: -1, ..IfdKnobs::new(6 * TOK, 6 * TOK) },
        "NRP8 repeating final fill: entry continuation short by 1 sompi",
    );

    // ---- take-profit / merge (exit side)
    let mb = |k: MergeKnobs, name: &str, at: usize| run_bad(n, &with_name(rpt_merge(&f, &k), name), at);
    mb(
        MergeKnobs { with_entry: false, ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) },
        "NRP10 booked exit takes profit without re-arming its entry (before rptUntil)",
        0,
    );
    mb(
        MergeKnobs { maker_delta: -1, ..MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRP11 maker's profit short by 1 sompi on a re-arming take-profit",
        0,
    );
    mb(
        MergeKnobs { cont_delta: -1, ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) },
        "NRP12 sell-out: the exit's carriers skimmed by 1 sompi (entry gets the budget only)",
        0,
    );
    let mut sl = booked_exit(m, 4 * TOK);
    sl.armed = 1;
    mb(
        MergeKnobs { exit: Some(sl), leg: 1, ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) },
        "NRP13 stop-loss fill re-arms its entry (attacker-funded re-arm of stopped-out tokens)",
        0,
    );
    // Two booked exits of the same entry in one transaction; the entry names only the first, the
    // second pays its maker the profit only and its budget goes to the filler.
    let mut s = rpt_merge(&f, &MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK));
    let c2 = cov(0xc2);
    let x2 = booked_exit(m, 4 * TOK);
    let last = s.outputs.len() - 1;
    s.outputs.truncate(last); // re-balanced below
    let b_in = s.inputs.len();
    s.inputs.push(call(
        &n.cond(&x2),
        "settle",
        vec![nb(4 * TOK), iv(b_in as i64 + 1), iv(0), iv(0), iv(0), iv(0), iv(0)],
        "cond.settle",
        CARRIER,
        c2,
        2_000,
    ));
    s.inputs.push(tok_in(tk, CARRIER, 4 * TOK, &c2.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 2_000));
    // one KCC-20 transfer: exit A's custody leads, both custodies go to the taker (8 tokens)
    set_leader_next(&mut s, 1, vec![tok_state(8 * TOK, &taker, SCHEME_P2PK)]);
    s.outputs[2].script_public_key = tk.spk(8 * TOK, &taker, SCHEME_P2PK);
    while s.outputs.len() < b_in {
        s.outputs.push(out(0, p2pk_spk(&taker), None));
    }
    s.outputs.push(out(ask_all_in(4 * TOK, x2.tp, TIP) - q_up(4 * TOK, rpt_rate()), p2pk_spk(&m), None));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRP14 double re-arm: a second booked exit rides on the first one's merge (its budget skimmed)"), b_in);

    // ---- merge (entry side)
    mb(
        MergeKnobs { cont_delta: -1, ..MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRP15 partial take-profit: entry re-armed 1 sompi short of the budget of 3000",
        2,
    );
    let ip = rpt_entry(m, 6 * TOK, 21 * TOK);
    mb(
        MergeKnobs { cont: Some(IfdP { amount: 8 * TOK, ..ip.clone() }), ..MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRP16 entry re-arms less than it got the budget for (6000 + 2000 instead of 6000 + 3000)",
        2,
    );
    mb(
        MergeKnobs {
            cont: Some(IfdP { amount: 9 * TOK, price: P255, ..ip.clone() }),
            ..MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK)
        },
        "NRP17 re-arm with altered parameters (limit 2.60 -> 2.55)",
        2,
    );
    mb(
        MergeKnobs {
            cont: Some(IfdP { amount: 9 * TOK, rpt: 99 * TOK, ..ip.clone() }),
            ..MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK)
        },
        "NRP17b re-arm refills the cycle count (rptAmount 21000 -> 99000)",
        2,
    );
    mb(
        MergeKnobs {
            entry: stop_entry_armed(m),
            cont: Some(IfdP { amount: 4 * TOK, ..stop_entry_armed(m) }),
            ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK)
        },
        "NRP18 an empty stop entry re-arms still armed (skips the new trigger)",
        2,
    );
    mb(
        MergeKnobs { claim_m: Some(4 * TOK), ..MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRP19 entry claims 4000 re-armed for a 3000 take-profit (attacker funds the extra budget)",
        0,
    );
    // A foreign exit: a plain KobCondAsk (parent 0) takes profit normally; the entry claims it.
    let s =
        rpt_merge(&f, &MergeKnobs { exit: Some(CondP { amount: 4 * TOK, ..ifd_exit(m) }), ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) });
    run_bad(n, &with_name(s, "NRP20 entry re-arms from a plain exit of another order (not its own, attacker-funded)"), 2);
    // A crafted non-KobCondAsk input imitating a booked exit of this entry.
    let mut s = rpt_merge(&f, &MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK));
    s.inputs[0] = fake_exit(&f, e, 3 * TOK, CARRIER);
    s.inputs.remove(1);
    s.outputs.truncate(1);
    s.outputs[0].value = 0;
    s.outputs[0].script_public_key = p2pk_spk(&taker);
    let ip6 = rpt_entry(m, 6 * TOK, 21 * TOK);
    set_arg(&mut s, 1, 0, nb_merge(0, 3 * TOK));
    s.outputs.push(out(0, spk_of(&n.ifd(&IfdP { amount: 9 * TOK, ..ip6.clone() })), Some((1, e))));
    let v6 = ifd_value(&ip6) + EC;
    s.outputs[1].value = (v6 + q_up(3 * TOK, rpt_rate())) as u64;
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRP21 entry re-arms from a crafted input that imitates its booked exit (not a KobCondAsk)"), 1);
    // Beside the exit's arming update (nothing sold) the entry claims 1000 re-armed base units: the exit refuses an
    // update next to its entry, and the entry refuses an exit sigscript that does not start with an 8-byte push.
    let s = exit_update_beside_merge(&f, None);
    run_bad(n, &with_name(s, "NRP22 entry re-arms beside its exit's update (nothing sold; attacker-funded)"), 1);
    // The other direction: a booked exit takes profit on 3000 and names as its merge an entry (a repeating
    // stop entry) that is only armed by update next to a bid fill: the budget would stay with the filler.
    let stop_e = IfdP { entry_stop: 220_000_000, ..rpt_entry(m, 6 * TOK, 21 * TOK) };
    let mut s = rpt_merge(&f, &MergeKnobs::of(stop_e.clone(), 4 * TOK, 3 * TOK));
    let v6s = ifd_value(&stop_e) + EC;
    s.inputs[2] = call(&n.ifd(&stop_e), "update", vec![iv(0)], "ifd.update", v6s, e, 1_000);
    let ci = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == e)).unwrap();
    s.outputs[ci] = out(v6s, spk_of(&n.ifd(&IfdP { armed: 1, ..stop_e.clone() })), Some((2, e)));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    let (ei, _) = add_ev(&f, &mut s, &Ev::bid(225_000_000));
    set_arg(&mut s, 2, 0, iv(ei));
    run_bad(n, &with_name(s, "NRP28 re-arming take-profit names its entry's arming update as the merge (budget skimmed)"), 0);
    // A zero "merge" beside the exit's refund re-dates the entry (idle clock, trigger freshness).
    let s = merge_beside_refund(&f, 0, false);
    run_bad(n, &with_name(s, "NRP23 zero merge beside an exit refund (re-dates the entry)"), 0);
    // A stray owned by the entry, co-spent in a merge.
    let mut s = with_name(
        rpt_merge(&f, &MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK)),
        "NRP24 merge co-spends a 2000-base-unit stray owned by the entry (to the taker)",
    );
    s.inputs.push(owned_tok(&f, 2 * TOK, e, None));
    set_leader_next(&mut s, 1, vec![tok_state(TOK, &cov(0xc1).as_bytes(), SCHEME_COVID), tok_state(5 * TOK, &taker, SCHEME_P2PK)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = tk.spk(5 * TOK, &taker, SCHEME_P2PK);
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &s, 2);
    // ---- the merge requires the exit to be exactly one this entry books (terms, parent, rptPrice)
    let atk = pk(&f.taker);
    let mut s = rpt_merge(&f, &MergeKnobs { exit: Some(booked_exit(atk, 4 * TOK)), ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) });
    s.outputs[0].script_public_key = p2pk_spk(&atk);
    run_bad(n, &with_name(s, "NRP25 look-alike exit of another maker (parent = this entry) re-arms the entry"), 2);
    let x = CondP { rpt_price: rpt_rate() - 1, ..booked_exit(m, 4 * TOK) };
    let s = rpt_merge(&f, &MergeKnobs { exit: Some(x), ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) });
    run_bad(n, &with_name(s, "NRP26 look-alike exit with another rptPrice (rate - 1) re-arms the entry"), 2);
    let x = CondP { tp: booked_exit(m, 4 * TOK).tp + 1, ..booked_exit(m, 4 * TOK) };
    let s = rpt_merge(&f, &MergeKnobs { exit: Some(x), ..MergeKnobs::new(m, 0, 4 * TOK, 4 * TOK) });
    run_bad(n, &with_name(s, "NRP27 look-alike exit with other terms (take-profit + 1) re-arms the entry"), 2);
    // ---- a merge into a stop entry armed by update and not filled yet keeps its band origin (the armed UTXO's DAA)
    let ae = IfdP { entry_stop: 220_000_000, band_daa: 300, armed: 1, ..rpt_entry(m, 6 * TOK, 21 * TOK) };
    let k = MergeKnobs::of(ae.clone(), 4 * TOK, 3 * TOK);
    run_ok(
        n,
        &with_name(rpt_merge(&f, &k), "RP11 merge into an armed, unfilled stop entry: the continuation records its band origin"),
    );
    let s = rpt_merge(&f, &MergeKnobs { cont: Some(IfdP { amount: 9 * TOK, ..ae.clone() }), ..k.clone() });
    run_bad(n, &with_name(s, "NRP29 merge into an armed, unfilled stop entry keeps armed = 1 (its band auction restarts)"), 2);
}
/// An armed repeating stop entry (stop 2.20, limit 2.60) with nothing left.
fn stop_entry_armed(maker: [u8; 32]) -> IfdP {
    let mut p = rpt_entry(maker, 0, 21 * TOK);
    p.entry_stop = 220_000_000;
    p.armed = 1;
    p
}
/// The booked exit (cov 0xc1, 4 tokens) is spent by its update (arming next to a plain ask fill, evidence at the
/// end) while its entry (input 1, 6 tokens left) merges `claim` (default 1000) beside it. `ev8`: the exit's ev
/// argument is pushed as a non-minimal 8-byte number and the entry claims exactly that value (the alias).
fn exit_update_beside_merge(f: &Fx, ev8: Option<()>) -> Scn {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);
    let x = booked_exit(m, 4 * TOK);
    let ip6 = rpt_entry(m, 6 * TOK, 21 * TOK);
    let v6 = ifd_value(&ip6) + EC;
    let mut s = Scn {
        name: "exit update beside merge".into(),
        inputs: vec![
            call(&n.cond(&x), "update", vec![iv(0), iv(0)], "cond.update", CARRIER, cov(0xc1), 2_000),
            call(
                &n.ifd(&ip6),
                "fill",
                vec![nb_merge(0, TOK), iv(0), iv(0), Arg::V(bytes(&[])), Arg::V(bytes(&[])), iv(0), iv(0)],
                "ifd.merge",
                v6,
                e,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(CARRIER, spk_of(&n.cond(&CondP { armed: 1, ..x.clone() })), Some((0, cov(0xc1)))),
            out(v6 + q_up(TOK, rpt_rate()), spk_of(&n.ifd(&IfdP { amount: 7 * TOK, ..ip6.clone() })), Some((1, e))),
            out(0, p2pk_spk(&pk(&f.taker)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    balance(&mut s, 2);
    // the exit arms next to a plain ask fill (touch); its update sigscript is [ev, tk, tag, redeem]
    let (ei, ti) = add_ev(f, &mut s, &down_ev());
    set_arg(&mut s, 0, 0, iv(ei));
    set_arg(&mut s, 0, 1, iv(ti));
    set_arg(&mut s, 1, 1, iv(ti));
    if ev8.is_some() {
        // the entry claims m = ev: the exit's first push, read as the 8-byte merge value, is exactly ev
        let ip7 = IfdP { amount: 6 * TOK + ei, ..ip6.clone() };
        set_arg(&mut s, 1, 0, nb_merge(0, ei));
        s.outputs[1] = out(v6 + q_up(ei, rpt_rate()), spk_of(&n.ifd(&ip7)), Some((1, e)));
        raw_first_push(&mut s, 0, &push8(ei));
        balance(&mut s, 2);
    }
    s
}
/// The entry (input 0, 6 tokens left) merges `claim` beside its booked exit's refund (input 1, custody input 2,
/// at its soft expiry). `push1`: the exit's refund argument nb = 0 is pushed as OP_PUSHDATA1 0x08 || 0^8, so its
/// sigscript does not start with 0x08 but bytes [1..9) read 8 (the entry claims exactly that).
fn merge_beside_refund(f: &Fx, claim: i64, push1: bool) -> Scn {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);
    let tk = &n.t;
    let x = booked_exit(m, 4 * TOK);
    let ip6 = rpt_entry(m, 6 * TOK, 21 * TOK);
    let v6 = ifd_value(&ip6) + EC;
    let claim = if push1 { 8 } else { claim };
    let mut s = Scn {
        name: "merge beside refund".into(),
        inputs: vec![
            call(
                &n.ifd(&ip6),
                "fill",
                vec![nb_merge(1, claim), iv(2), iv(0), Arg::V(bytes(&[])), Arg::V(bytes(&[])), iv(0), iv(0)],
                "ifd.merge",
                v6,
                e,
                1_000,
            ),
            call(
                &n.cond(&x),
                "settle",
                vec![nb(0), iv(2), iv(0), iv(0), iv(0), iv(0), iv(0)],
                "cond.refund",
                CARRIER,
                cov(0xc1),
                (EXPIRY - 1_000) as u64,
            ),
            tok_in(
                tk,
                CARRIER,
                4 * TOK,
                &cov(0xc1).as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(4 * TOK, &m, SCHEME_P2PK)]),
                Wit::CovId,
                2_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(
                v6 + if claim > 0 { q_up(claim, rpt_rate()) } else { 0 },
                spk_of(&n.ifd(&IfdP { amount: 6 * TOK + claim, ..ip6.clone() })),
                Some((0, e)),
            ),
            out(2 * CARRIER - REFUND_TIP, tk.spk(4 * TOK, &m, SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(0, p2pk_spk(&pk(&f.taker)), None),
        ],
        lock_time: EXPIRY as u64,
        payload: vec![],
    };
    balance(&mut s, 2);
    if push1 {
        let mut p = vec![0x4c, 0x08];
        p.extend(0i64.to_le_bytes());
        raw_first_push(&mut s, 1, &p);
    }
    s
}
// ================================================================ regressions (sell side)

/// Two IOC asks (10 whole tokens each) of ONE maker, n base units sold from each, remainder r = 10000 - n.
/// `shared`: both point tokOut at ONE output (the attack: the maker gets r back instead of 2r);
/// else each returns at its own custody index (tokOut == tokenIn = 2 and 3).
fn fx_ioc_pair(f: &Fx, n: i64, shared: bool) -> Scn {
    let t = &f.net.t;
    let (a1, a2) = (cov(0xa1), cov(0xa2));
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    let ask = f.net.ask(&p);
    let taker = pk(&f.taker);
    let r = 10 * TOK - n;
    let pay = ask_all_in(n, P250, TIP);
    let tok_out: [i64; 2] = if shared { [2, 2] } else { [2, 3] };
    let (next, taker_amount) = if shared {
        (vec![tok_state(r, &p.maker, SCHEME_P2PK), tok_state(2 * n + r, &taker, SCHEME_P2PK)], 2 * n + r)
    } else {
        (vec![tok_state(r, &p.maker, SCHEME_P2PK), tok_state(r, &p.maker, SCHEME_P2PK), tok_state(2 * n, &taker, SCHEME_P2PK)], 2 * n)
    };
    let mut outputs = vec![
        out(pay + CARRIER, p2pk_spk(&p.maker), None),
        out(pay + CARRIER, p2pk_spk(&p.maker), None),
        out(CARRIER, t.spk(r, &p.maker, SCHEME_P2PK), Some((2, TOKEN_COV))),
    ];
    if !shared {
        outputs.push(out(CARRIER, t.spk(r, &p.maker, SCHEME_P2PK), Some((2, TOKEN_COV))));
    }
    outputs.push(out(CARRIER, t.spk(taker_amount, &taker, SCHEME_P2PK), Some((2, TOKEN_COV))));
    outputs.push(out(0, p2pk_spk(&taker), None));
    let mut s = Scn {
        name: if shared {
            "FXE1 two IOC asks of one maker share ONE remainder output (maker gets 9000 base units back instead of 18000)".into()
        } else {
            "FXE1+ two IOC asks of one maker, each remainder at its own custody index".into()
        },
        inputs: vec![
            call(&ask, "settle", vec![nb(n), iv(2), iv(tok_out[0]), iv(0)], "ask1.settle", CARRIER, a1, 1_000),
            call(&ask, "settle", vec![nb(n), iv(3), iv(tok_out[1]), iv(0)], "ask2.settle", CARRIER, a2, 1_000),
            tok_in(t, CARRIER, 10 * TOK, &a1.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000),
            tok_in(t, CARRIER, 10 * TOK, &a2.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
    };
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}

/// An IOC ask and a bid of the SAME maker: the ask returns r = 6000 base units at output 1, which is
/// also the bid's positional delivery of 6000 (output 1 = the bid's input), so the bid "buys" the
/// maker's own returned tokens and the matcher takes the bid's KAS.
fn fx_ioc_ask_plus_bid(f: &Fx) -> Scn {
    let t = &f.net.t;
    let (a1, b1) = (cov(0xa1), cov(0xb1));
    let maker = pk(&f.maker_a);
    let mut ap = AskP::new(maker, P250);
    ap.tif = 1;
    let ask = f.net.ask(&ap);
    let bp = BidP::new(maker, P245);
    let bid = f.net.bid(&bp);
    let taker = pk(&f.taker);
    let (n_ask, r) = (4 * TOK, 6 * TOK);
    let v = bp.used(10 * TOK) + 3 * DC;
    let pay = ask_all_in(n_ask, P250, TIP);
    let cont = v - bp.used(r) - DC;
    let mut s = Scn {
        name: "FXE2 IOC ask + bid of one maker: the ask's return (tokOut 1) is also the bid's delivery".into(),
        inputs: vec![
            call(&ask, "settle", vec![nb(n_ask), iv(2), iv(1), iv(0)], "ask.settle", CARRIER, a1, 1_000),
            call(&bid, "fill", vec![nb(r), iv(2), iv(0)], "bid.fill", v, b1, 1_000),
            tok_in(
                t,
                CARRIER,
                10 * TOK,
                &a1.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(r, &maker, SCHEME_P2PK), tok_state(n_ask, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay + CARRIER, p2pk_spk(&maker), None),
            out(DC.max(CARRIER), t.spk(r, &maker, SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(CARRIER, t.spk(n_ask, &taker, SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(cont, spk_of(&bid), Some((1, b1))),
            out(0, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    balance(&mut s, 4);
    s
}

/// A foreign-token stray (same program, another covenant id: OTHER_COV) owned by the ask's covenant id
/// rides along S1. `as_custody`: the stray is named as the ask's tokenIn.
fn fx_foreign_stray(f: &Fx, as_custody: bool) -> Scn {
    let mut s = s1(f);
    let taker = pk(&f.taker);
    let a = cov(0xa1);
    // the foreign token: 5 whole tokens owned (scheme 4) by the ask's covenant id, its own leader
    let at = s.inputs.len();
    s.inputs.push(tok_in(
        &f.net.t,
        CARRIER,
        5 * TOK,
        &a.as_bytes(),
        SCHEME_COVID,
        OTHER_COV,
        Some(vec![tok_state(5 * TOK, &taker, SCHEME_P2PK)]),
        Wit::CovId,
        1_000,
    ));
    let change = s.outputs.pop().expect("change");
    s.outputs.push(out(CARRIER, f.net.t.spk(5 * TOK, &taker, SCHEME_P2PK), Some((at as u16, OTHER_COV))));
    s.outputs.push(change);
    if as_custody {
        set_arg(&mut s, 0, 1, iv(at as i64));
    }
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s.name = if as_custody {
        "FXL2 a foreign-token stray owned by the ask is named as its custody".into()
    } else {
        "FXL2+ a foreign-token stray owned by the ask moves with its fill (unprotected by design); the maker's pins hold".into()
    };
    s
}

/// A stop sell at a LOW stop price (below 10000 sompi per whole token), armed, whole band at once:
/// the 3% band must hold (stop 9999 -> floor 9700). `delta`: sompi per whole token paid above the floor.
fn fx_low_stop(f: &Fx, stop: i64, delta: i64) -> Scn {
    let cp = CondP { tip: 0, stop, tp: 0, armed: 1, ..CondP::oco(pk(&f.maker_a)) };
    let floor = stop - stop.saturating_mul(cp.slip_bps) / 10_000;
    let mut s = cond_fill_at(f, &cp, 4 * TOK, 1, &cp, None, 0, floor + delta);
    s.name = format!("FXL4 low stop {stop}: stop-market fill at {} (band floor {floor})", floor + delta);
    s
}

/// Regressions (sell side, KCC-20): positional IOC returns, foreign strays, the multiply-first stop band. The repeat-IFD merge checks are in
/// `v2_repeat_ifd_attacks` (NRP25..NRP27).
#[test]
fn v2_fix_pass_regressions() {
    let f = fx();
    let n = &f.net;
    // each IOC return is positional (tokOut == tokenIn)
    run_ok(n, &fx_ioc_pair(&f, TOK, false));
    run_bad(n, &fx_ioc_pair(&f, TOK, true), 1);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    run_ok(n, &with_name(ioc_fill(&f, &p, 4 * TOK, P250, 0), "FXE1+ single IOC ask returns at its custody index"));
    // the return cannot double as a bid's positional delivery
    run_bad(n, &fx_ioc_ask_plus_bid(&f), 0);
    // a foreign-program stray can never be the custody; one riding along does not weaken the ask
    run_bad(n, &fx_foreign_stray(&f, true), 0);
    run_ok(n, &fx_foreign_stray(&f, false));
    // stop 9999 sompi per whole token with the default 3% band: the floor is 9700, not 9999
    run_ok(n, &fx_low_stop(&f, 9_999, 0));
    run_bad(n, &fx_low_stop(&f, 9_999, -1), 0);
    run_ok(n, &fx_low_stop(&f, 100, 0));
}

// ================================================================ protocol v3: minFill, rounding, tips, overflow, merges

/// A take-profit fill of the conditional `cp` that takes everything it holds (sold out: no continuation, both
/// carriers to the maker), paying `pay_delta` sompi above the all-in proceeds.
fn cond_sell_out(f: &Fx, cp: &CondP, pay_delta: i64) -> Scn {
    let n = cp.amount;
    let mut s = cond_fill(f, cp, n, 0, cp, None);
    s.outputs.remove(1);
    s.outputs.remove(1);
    let pay = ask_all_in(n, cp.tp, cp.tip);
    s.outputs[0].value = (pay + 2 * CARRIER + pay_delta) as u64;
    set_arg(&mut s, 0, 2, iv(0));
    set_leader_next(&mut s, 1, vec![tok_state(n, &pk(&f.taker), SCHEME_P2PK)]);
    let last = s.outputs.len() - 1;
    s.outputs[last].value = (1000 * KAS - pay - pay_delta - CARRIER - NET_FEE) as u64;
    s
}

/// minFill in every kind: a fill below minFill that does not take the rest (KobBid: that does not end the bid)
/// is refused; exactly minFill, and a smaller fill that takes everything left, are accepted.
#[test]
fn v3_min_fill() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    // ---- KobAsk (minFill 1000, 10000 left)
    let ap = AskP::new(m, P250);
    run_ok(n, &with_name(s1_with(&f, &ap, MIN_FILL), "V3A01+ ask: a fill of exactly minFill (1000 of 10000)"));
    run_ok(
        n,
        &with_name(
            s1_with(&f, &AskP { amount: 500, ..ap.clone() }, 500),
            "V3A01+ ask: a 500 fill below minFill 1000 that takes the rest",
        ),
    );
    run_bad(n, &with_name(s1_with(&f, &ap, MIN_FILL - 1), "V3A01 ask: a 999 fill below minFill 1000 that leaves 9001"), 0);
    // ---- KobBid (minFill 1000)
    let bp = BidP::new(pk(&f.maker_b), P245);
    run_ok(n, &with_name(s6_with(&f, &bp, MIN_FILL, 10 * TOK, P245, 0), "V3B01+ bid: a fill of exactly minFill, the bid continues"));
    // 1500 funded, 600 bought: 900 of buying power is left, less than one minimum fill, so the bid ends
    let v = bp.funded(1_500);
    assert!(v - bp.used(600) - DC < bp.used(bp.min_fill), "fixture: less than one minimum fill left");
    run_ok(
        n,
        &with_name(
            s6_value(&f, &bp, 600, v, P245, 0),
            "V3B01+ bid: a 600 fill below minFill ends a bid left with less than one minimum fill",
        ),
    );
    run_bad(
        n,
        &with_name(s6_with(&f, &bp, MIN_FILL - 1, 10 * TOK, P245, 0), "V3B01 bid: a 999 fill below minFill that does not end the bid"),
        0,
    );
    let b0 = BidP { min_fill: 0, ..bp.clone() };
    run_bad(n, &with_name(s6_with(&f, &b0, 4 * TOK, 10 * TOK, P245, 0), "V3B02 bid with minFill = 0 is unfillable"), 0);
    // ---- KobCondAsk (minFill 1000, 10000 left)
    let oco = CondP::oco(m);
    run_ok(n, &with_name(cond_fill(&f, &oco, MIN_FILL, 0, &oco, None), "V3CA01+ conditional: a TP fill of exactly minFill"));
    run_ok(
        n,
        &with_name(
            cond_sell_out(&f, &CondP { amount: 500, ..oco.clone() }, 0),
            "V3CA01+ conditional: a 500 TP fill below minFill that takes the rest",
        ),
    );
    run_bad(n, &with_name(cond_fill(&f, &oco, MIN_FILL - 1, 0, &oco, None), "V3CA01 conditional: a 999 TP fill below minFill"), 0);
    // ---- KobIfdBid (minFill 1500)
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { min_fill: 1_500, ..IfdKnobs::new(10 * TOK, 1_500) }),
            "V3IB01+ if-done entry: a fill of exactly minFill 1500",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { min_fill: 1_500, ..IfdKnobs::new(1_200, 1_200) }),
            "V3IB01+ if-done entry: a 1200 fill below minFill 1500 that takes the rest",
        ),
    );
    run_bad(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { min_fill: 1_500, ..IfdKnobs::new(10 * TOK, 1_499) }),
            "V3IB01 if-done entry: a 1499 fill below minFill 1500",
        ),
        0,
    );
}

/// quoteOf rounding in the maker's favour, at amounts that are not a multiple of the scale and rates with a
/// sompi remainder: every exact boundary is accepted, the attack one sompi worse is refused.
#[test]
fn v3_rounding() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    // the local mirror is the protocol library's quote rule
    for (a, r) in [(4_321, 249_900_007), (1, 1), (999, 7), (10_000, 260_100_000), (3_217, 260_100_003), (12_345_678, 999_999_999)] {
        for (up, round) in [(true, Round::Up), (false, Round::Down)] {
            assert_eq!(quote_at(a, r, SCALE, up), quote_of(a, r, SCALE, round), "quoteOf({a}, {r}, {round:?})");
        }
    }
    assert_eq!(quote_at(i64::MAX / 1000 + 1, 1000, SCALE, true), quote_of(i64::MAX / 1000 + 1, 1000, SCALE, Round::Up));
    let frac = |a: i64, r: i64| assert_eq!(q_up(a, r), q_down(a, r) + 1, "fixture: {a} x {r} / {SCALE} has a remainder");

    // ---- KobAsk: proceeds ceil(4321 * 249.900007 KAS / 1000)
    let ap = AskP::new(m, 250_000_007);
    frac(4_321, ap.price - TIP);
    run_ok(n, &with_name(s1_with(&f, &ap, 4_321), "V3A02+ ask: 4321 at 2.50000007 paid exactly the ceil of its proceeds"));
    let mut s =
        with_name(s1_with(&f, &ap, 4_321), "V3A02 ask: proceeds one sompi short of the ceil (4321, not a multiple of the scale)");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    // ---- KobBid: spend floor(4321 * rate / 1000), budget used ceil(4321 * rate / 1000)
    let bp = BidP::new(pk(&f.maker_b), 245_000_003);
    frac(4_321, bp.rate_max());
    run_ok(
        n,
        &with_name(s6_with(&f, &bp, 4_321, 10 * TOK, bp.price, 0), "V3B03+ bid: 4321 bought, spend rounded down, budget rounded up"),
    );
    let mut s = with_name(
        s6_with(&f, &bp, 4_321, 10 * TOK, bp.price, 0),
        "V3B03 bid (continuing): spend one sompi above the floor (the delivery one sompi short)",
    );
    s.outputs[0].value -= 1;
    s.outputs[2].value += 1;
    run_bad(n, &s, 0);
    let ioc = BidP { tif: 1, ..bp.clone() };
    run_ok(
        n,
        &with_name(s6_with(&f, &ioc, 4_321, 10 * TOK, ioc.price, 0), "V3B03b+ IOC bid: 4321 bought, the rest rides on the delivery"),
    );
    let mut s = with_name(
        s6_with(&f, &ioc, 4_321, 10 * TOK, ioc.price, 0),
        "V3B03b IOC bid (terminating): spend one sompi above the floor (the delivery one sompi short)",
    );
    s.outputs[0].value -= 1;
    s.outputs[1].value += 1;
    run_bad(n, &s, 0);
    let mut s = with_name(
        s6_with(&f, &bp, 4_321, 10 * TOK, bp.price, 0),
        "V3B04 bid: the continuation consumes one sompi less than the ceil budget (keeps one sompi more)",
    );
    s.outputs[1].value += 1;
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    // ---- KobCondAsk: TP proceeds ceil(4321 * 2.99900007 KAS / 1000)
    let cp = CondP { tp: 300_000_007, ..CondP::oco(m) };
    frac(4_321, cp.tp - TIP);
    run_ok(n, &with_name(cond_fill(&f, &cp, 4_321, 0, &cp, None), "V3CA02+ conditional: 4321 on the TP leg paid exactly the ceil"));
    let mut s =
        with_name(cond_fill(&f, &cp, 4_321, 0, &cp, None), "V3CA02 conditional: TP proceeds one sompi short of the ceil (4321)");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    // ---- KobIfdBid: spend floor(4321 * 2.60100003 KAS / 1000)
    let k = IfdKnobs { price: 260_000_003, ..IfdKnobs::new(10 * TOK, 4_321) };
    frac(4_321, k.price + k.tip);
    run_ok(n, &with_name(ifd_fill(&f, &k), "V3IB02+ if-done entry: 4321 bought, spend rounded down"));
    run_bad(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { cont_delta: -1, ..k.clone() }),
            "V3IB02 if-done entry: spend one sompi above the floor (continuation short)",
        ),
        0,
    );
    let kf = IfdKnobs { price: 260_000_003, ..IfdKnobs::new(4_321, 4_321) };
    run_ok(n, &with_name(ifd_fill(&f, &kf), "V3IB02b+ if-done entry: final 4321, the rest rides on the exit"));
    run_bad(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { exit_delta: -1, ..kf.clone() }),
            "V3IB02b if-done entry: final fill spends one sompi above the floor (exit short)",
        ),
        0,
    );

    // ---- repeat merge: budget(3217) = ceil(3217 * 2.60100003 KAS / 1000) on both sides
    let ip = IfdP { price: 260_000_003, ..rpt_entry(m, 6 * TOK, 21 * TOK) };
    frac(3_217, ip.rate());
    let k = MergeKnobs::of(ip.clone(), 4 * TOK, 3_217);
    run_ok(n, &with_name(rpt_merge(&f, &k), "V3IB05+ partial take-profit of 3217: the entry gets exactly the ceil budget back"));
    run_bad(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { cont_delta: -1, ..k.clone() }),
            "V3IB05 partial take-profit: the entry re-armed one sompi short of the ceil budget",
        ),
        2,
    );
    let k = MergeKnobs::of(IfdP { amount: 0, ..ip.clone() }, 3_217, 3_217);
    run_ok(n, &with_name(rpt_merge(&f, &k), "V3CA04+ sell-out take-profit of 3217: the entry gets the ceil budget and both carriers"));
    run_bad(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { cont_delta: -1, maker_delta: 1, ..k.clone() }),
            "V3CA04 sell-out: the entry gets one sompi less than the ceil budget (the maker the sompi)",
        ),
        0,
    );
}

/// The price must cover the tip: KobAsk p(t) >= tip (also a decayed price), KobCondAsk legPrice >= tip (TP leg,
/// stop leg, stop auction). A price exactly at the tip fills at all-in 0.
#[test]
fn v3_tip() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let ap = AskP { price: TIP, ..AskP::new(m, TIP) };
    run_ok(n, &with_name(s1_with(&f, &ap, 4 * TOK), "V3A03+ ask quoting exactly its tip fills (all-in 0)"));
    run_bad(n, &with_name(s1_with(&f, &AskP { price: TIP / 2, ..ap.clone() }, 4 * TOK), "V3A03 ask whose tip exceeds its price"), 0);
    // Dutch ask 0.003 -> 0.0005 (1000 sompi per 1000 DAA from DAA 1000), tip 0.001
    let d = AskP { price: 300_000, slope: 1_000, price_end: TIP / 2, active_from: 1_000, decay_step: 1_000, ..AskP::new(m, 300_000) };
    let p51 = decay_down(d.price, d.price_end, d.slope, 1_000, 51_000);
    assert!(p51 >= TIP);
    run_ok(n, &with_name(s1_at(&f, &d, 4 * TOK, p51, 51_000), "V3A04+ Dutch ask decayed to 0.0025, above its tip"));
    let late = decay_down(d.price, d.price_end, d.slope, 1_000, 300_000);
    assert!(late < TIP);
    run_bad(
        n,
        &with_name(s1_at(&f, &d, 4 * TOK, late, 300_000), "V3A04 Dutch ask decayed below its tip (price 0.0005 < tip 0.001)"),
        0,
    );

    // KobCondAsk
    let low_tp = CondP { tp: TIP / 2, ..CondP::oco(m) };
    run_bad(n, &with_name(cond_fill(&f, &low_tp, 4 * TOK, 0, &low_tp, None), "V3CA03a conditional TP leg priced below its tip"), 0);
    let low_stop = armed(&CondP { stop: TIP, ..CondP::oco(m) });
    assert!(stop_leg(TIP, 300) < TIP);
    run_bad(
        n,
        &with_name(
            cond_fill(&f, &low_stop, 4 * TOK, 1, &low_stop, None),
            "V3CA03b conditional stop leg (band floor 0.00097) below its tip",
        ),
        0,
    );
    let au = CondP { stop: 102_000, band_daa: 300, ..CondP::oco(m) };
    let (a1, a2000) = (with_armed(&au, 1), with_armed(&au, 2_000));
    let half = stop_auction(au.stop, 300, 300, 150);
    assert!(half >= TIP);
    run_ok(
        n,
        &with_name(
            cond_fill_at(&f, &a1, 4 * TOK, 1, &a2000, None, 2_150, half),
            "V3CA03c+ stop auction at 150/300 DAA, still above the tip",
        ),
    );
    let end = stop_auction(au.stop, 300, 300, 400);
    assert!(end < TIP);
    let mut s = with_name(
        cond_fill_at(&f, &a1, 4 * TOK, 1, &a2000, None, 2_400, end),
        "V3CA03c stop auction descended below the tip (band floor 0.0009894 < tip 0.001)",
    );
    s.lock_time = NOW;
    run_bad(n, &s, 0);
}

/// The largest fitting amount fills; one base unit more fails closed (every covenant product is checked by the
/// engine), whatever the filler pays. Rate R = i64::MAX / 9001 + 1 sompi per whole token, tip 0: a fill of
/// 9000999 base units is worth just under 2^63 sompi, 9001000 overflow the product q * R.
fn overflow_rate() -> (i64, i64) {
    let r = i64::MAX / 9_001 + 1;
    let fit = 9_000 * TOK + 999;
    let v = quote_at(fit, r, SCALE, true).expect("the largest amount fits");
    assert!(v.checked_add(2 * CARRIER + DC).is_some(), "fixture: proceeds plus carriers fit");
    assert_eq!(quote_at(fit + 1, r, SCALE, true), None, "fixture: one more base unit overflows");
    assert_eq!(quote_of(fit + 1, r, SCALE, Round::Up), None);
    (r, fit)
}
/// An ask (tip 0, price r) holding `amount` sold out to a taker; the maker gets `pay` plus both carriers.
fn ask_full(f: &Fx, r: i64, amount: i64, pay: i64) -> Scn {
    let t = &f.net.t;
    let a = cov(0xa1);
    let p = AskP { tip: 0, amount, ..AskP::new(pk(&f.maker_a), r) };
    let taker = pk(&f.taker);
    Scn {
        name: "ask full fill".into(),
        inputs: vec![
            call(&f.net.ask(&p), "settle", vec![nb(amount), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000),
            tok_in(
                t,
                CARRIER,
                amount,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(amount, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, pay.saturating_add(KAS)),
        ],
        outputs: vec![
            out(pay.saturating_add(2 * CARRIER), p2pk_spk(&p.maker), None),
            out(CARRIER, t.spk(amount, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(0, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}
/// A bid (tip 0, price r) with escrow `v` buying `amount`; the delivery carries `delivery`.
fn bid_full(f: &Fx, r: i64, amount: i64, v: i64, delivery: i64) -> Scn {
    let t = &f.net.t;
    let b = cov(0xb1);
    let bp = BidP { tip: 0, ..BidP::new(pk(&f.maker_b), r) };
    let taker = pk(&f.taker);
    Scn {
        name: "bid full fill".into(),
        inputs: vec![
            call(&f.net.bid(&bp), "fill", vec![nb(amount), iv(1), iv(0)], "bid.fill", v, b, 1_000),
            tok_in(
                t,
                CARRIER,
                amount,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(amount, &bp.maker, SCHEME_P2PK)]),
                Wit::P2pk(f.taker),
                1_000,
            ),
        ],
        outputs: vec![out(delivery, t.spk(amount, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))), out(0, p2pk_spk(&taker), None)],
        lock_time: NOW,
        payload: vec![],
    }
}

#[test]
fn v3_overflow() {
    let f = fx();
    let n = &f.net;
    let (r, fit) = overflow_rate();
    let pay = q_up(fit, r);
    run_ok(
        n,
        &with_name(ask_full(&f, r, fit, pay), "V3A05+ ask: a full fill of the largest fitting amount (9000999 at 1.02e15 per token)"),
    );
    run_bad(
        n,
        &with_name(
            ask_full(&f, r, fit + 1, i64::MAX - 2 * CARRIER),
            "V3A05 ask: one base unit more overflows the product and fails, although the maker is paid 2^63 - 1",
        ),
        0,
    );
    let v = quote_at(fit, r, SCALE, true).unwrap() + DC;
    run_ok(n, &with_name(bid_full(&f, r, fit, v, v - q_down(fit, r)), "V3B07+ bid: a fill of the largest fitting amount"));
    run_bad(
        n,
        &with_name(
            bid_full(&f, r, fit + 1, i64::MAX, i64::MAX),
            "V3B07 bid: one base unit more overflows the product and fails, although the whole escrow is delivered",
        ),
        0,
    );
}

/// FOK bid: it must leave less buying power than one minimum fill.
#[test]
fn v3_fok() {
    let f = fx();
    let n = &f.net;
    let bp = BidP { tif: 2, ..BidP::new(pk(&f.maker_b), 245_000_003) };
    // funded for 4321 + 999: after the fill less than one minimum fill (1000) is left
    let v = bp.funded(4_321 + 999);
    assert!(v - bp.used(4_321) - DC < bp.used(bp.min_fill), "fixture: FOK leaves less than one minimum fill");
    run_ok(n, &with_name(s6_value(&f, &bp, 4_321, v, bp.price, 0), "V3B06+ FOK bid fills 4321 leaving less than one minimum fill"));
    let v = bp.funded(4_321 + MIN_FILL);
    assert!(v - bp.used(4_321) - DC >= bp.used(bp.min_fill));
    run_bad(
        n,
        &with_name(
            s6_value(&f, &bp, 4_321, v, bp.price, 0),
            "V3B06 FOK bid filled 4321 leaving buying power for one more minimum fill",
        ),
        0,
    );
}

/// Repeat merges: nb = -(k * 2^53 + m), the 0x08 first-byte checks on both sides, the exit's update never next
/// to its entry, booked amounts below 2^53, and the largest merge (m = 2^53 - 1 at input k = 999).
#[test]
fn v3_merge_push() {
    let f = fx();
    let n = &f.net;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);

    // the exit's update pushes ev as a non-minimal 8-byte number and the entry claims m = ev: the exit refuses an
    // update next to its entry (OpCovInputCount(parent) == 0)
    run_bad(
        n,
        &with_name(
            exit_update_beside_merge(&f, Some(())),
            "V3CA05 exit's update (ev pushed as 8 bytes) beside its entry, which merges m = ev",
        ),
        0,
    );
    // the exit refunds with nb = 0 pushed as OP_PUSHDATA1 0x08 || 0^8: bytes [1..9) read 8; the entry claims m = 8
    run_bad(
        n,
        &with_name(
            merge_beside_refund(&f, 0, true),
            "V3IB06 entry merges m = 8 beside its exit's refund pushed as PUSHDATA1 (sigscript does not start with 0x08)",
        ),
        0,
    );
    // the exit refunds (nothing sold) and the entry claims 3000
    run_bad(
        n,
        &with_name(merge_beside_refund(&f, 3 * TOK, false), "V3IB07 entry merges m = 3000 beside its exit's refund (nothing sold)"),
        0,
    );
    // the entry's update pushes ev as the 8-byte merge value -(0 * 2^53 + 3000): the exit reads a merge, the
    // entry's own update refuses a negative input index
    let stop_e = IfdP { entry_stop: 220_000_000, ..rpt_entry(m, 6 * TOK, 21 * TOK) };
    let mut s = rpt_merge(&f, &MergeKnobs::of(stop_e.clone(), 4 * TOK, 3 * TOK));
    let v6s = ifd_value(&stop_e) + EC;
    s.inputs[2] = call(&n.ifd(&stop_e), "update", vec![iv(0)], "ifd.update", v6s, e, 1_000);
    let ci = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == e)).unwrap();
    s.outputs[ci] = out(v6s, spk_of(&n.ifd(&IfdP { armed: 1, ..stop_e.clone() })), Some((2, e)));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    add_ev(&f, &mut s, &Ev::bid(225_000_000));
    raw_first_push(&mut s, 2, &push8(-3 * TOK));
    run_bad(n, &with_name(s, "V3CA07 entry's update pushes ev = -(0 * 2^53 + 3000) (8 bytes) beside a re-arming take-profit"), 2);

    // the entry's merge argument pushed as a 9-byte item (0x09 || -(0 * 2^53 + 3000) || 0x00): bytes [1..9) are the
    // exact merge value the exit expects, only the first byte is not 0x08
    let mut s = rpt_merge(&f, &MergeKnobs::new(m, 6 * TOK, 4 * TOK, 3 * TOK));
    let mut p9 = vec![0x09];
    p9.extend_from_slice(&push8(-3 * TOK)[1..]);
    p9.push(0x00);
    raw_first_push(&mut s, 2, &p9);
    run_bad(
        n,
        &with_name(s, "V3CA08 re-arming take-profit beside an entry whose merge argument is a 9-byte push (first byte 0x09)"),
        0,
    );

    // booking: a booked exit's amount must be below 2^53 (entry 0.0006 per whole token, tip 0)
    let low = |amount: i64, fill: i64| IfdKnobs {
        price: 600,
        tip: 0,
        rpt: MERGE_K + 1,
        value: Some(q_up(amount, 600) + 2 * (DC + EC)),
        ..IfdKnobs::new(amount, fill)
    };
    run_ok(n, &with_name(ifd_fill(&f, &low(MERGE_K + TOK, MERGE_K - 1)), "V3IB03+ booked exit of 2^53 - 1 base units"));
    run_bad(
        n,
        &with_name(ifd_fill(&f, &low(MERGE_K + TOK, MERGE_K)), "V3IB03 booked exit of 2^53 base units (the merge cannot name it)"),
        0,
    );

    // the largest merge: m = 2^53 - 1 sold by the exit at input k = 999
    run_ok(n, &with_name(merge_far(&f, 999, MERGE_K - 1), "V3IB04+ merge of m = 2^53 - 1 from the exit at input 999"));
}

/// A booked exit at input `k` (k >= 3) sells out `amount` on its take-profit (0.001 per whole token, tip 0) and its
/// entry (input 0, nothing left, 0.0006 per whole token, tip 0) merges it: nb = -(k * 2^53 + amount). Inputs 3..k
/// and outputs 3..k are padding (anyone-can-spend inputs, zero outputs); the exit's payout is output k.
fn merge_far(f: &Fx, k: usize, amount: i64) -> Scn {
    let tk = &f.net.t;
    let m = pk(&f.maker_a);
    let c = cov(0xc1);
    let e = cov(RPT_ENTRY);
    let taker = pk(&f.taker);
    let exit_terms = CondP { tp: 1_000, tip: 0, ..ifd_exit(m) };
    let ip = IfdP { price: 600, tip: 0, rpt: 1, ..IfdP::limit(m, 0, 600, exit_terms) };
    let xp = booked_from(&ip, amount);
    let entry_value = EC;
    let all_in = ask_all_in(amount, xp.tp, xp.tip);
    let budget = q_up(amount, ip.rate());
    let pad = || Inp {
        entry: UtxoEntry::new(0, ScriptPublicKey::new(0, vec![kaspa_txscript::opcodes::codes::OpTrue].into()), 1_000, false, None),
        role: Role::Raw { ss: vec![], name: "pad" },
        seq: 0,
    };
    let mut inputs = vec![
        call(
            &f.net.ifd(&ip),
            "fill",
            vec![nb_merge(k as i64, amount), iv(1), iv(0), Arg::V(bytes(&[])), Arg::V(bytes(&[])), iv(0), iv(0)],
            "ifd.merge",
            entry_value,
            e,
            1_000,
        ),
        tok_in(
            tk,
            CARRIER,
            amount,
            &c.as_bytes(),
            SCHEME_COVID,
            TOKEN_COV,
            Some(vec![tok_state(amount, &taker, SCHEME_P2PK)]),
            Wit::CovId,
            2_000,
        ),
        p2pk_in(&f.taker, 0),
    ];
    let mut outputs = vec![
        out(entry_value + budget + 2 * CARRIER, spk_of(&f.net.ifd(&IfdP { amount, armed: 0, ..ip.clone() })), Some((0, e))),
        out(CARRIER, tk.spk(amount, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
        out(0, p2pk_spk(&taker), None),
    ];
    while inputs.len() < k {
        inputs.push(pad());
        outputs.push(out(0, p2pk_spk(&taker), None));
    }
    inputs.push(call(
        &f.net.cond(&xp),
        "settle",
        vec![nb(amount), iv(1), iv(0), iv(0), iv(0), iv(0), iv(0)],
        "cond.settle",
        CARRIER,
        c,
        2_000,
    ));
    outputs.push(out(all_in - budget, p2pk_spk(&m), None));
    let mut s = Scn { name: "merge far".into(), inputs, outputs, lock_time: NOW, payload: vec![] };
    fund_and_balance(&mut s, 2, 2, KAS);
    s
}
