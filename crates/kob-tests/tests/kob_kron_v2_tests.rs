//! KOB protocol v3 harness, KRON family, SELL side: KobAskKron / KobBidKron / KobCondAskKron / KobIfdBidKron (stops armed
//! by a plain fill in the same transaction, touch) executed against the REAL KRON token program bytes (the two pinned
//! mainnet templates, 2,433 B and 2,732 B) in rusty-kaspa v2.1.0's TxScriptEngine with KIP-20 covenant context and
//! script-unit metering. Port of kob_v2_tests.rs (same scenario ids and structure; every scenario of the KCC-20 suite
//! exists here) plus the KRON-specific custody / token-program attacks of the v1 adapter suite (kob_kron_adapter_tests.rs)
//! applied to the v2 orders.
//!
//! Amounts are base units of the token (SCALE = 1000 base units per whole token, a 3-decimal token), prices and tips
//! sompi per whole token; every quote is the covenants' quoteOf, rounded in the maker's favour (ceil where the maker
//! receives, floor where it pays).
//!
//! KRON vs KCC-20: no leader/delegator (every token input runs the full check on the same next-state
//! columns), custody = id_type 2 owned by the order covenant id with is_minter 0, maker/taker held tokens
//! and deliveries = id_type 3 (address presence: the owner's P2PK input must be in the tx, so takers that
//! hand in tokens add a P2PK input), at most 4 token inputs and 5 token outputs per tx.
//! Every test loops over both pinned templates (2433 primary, 2732).
//! Run: cargo test -p kob-tests --test kob_kron_v2_tests -- --nocapture --test-threads=1

// Harness style (same as the other suites): explicit `&` on byte-slice arguments, index loops over
// parallel vectors and written-out identity arithmetic keep the scenario tables readable.
#![allow(
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_range_loop,
    clippy::identity_op,
    clippy::self_assignment
)]

mod common;

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
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::{ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use common::kron::{kron_ss, ks, Kron, KS, TPL_2433, TPL_2732, T_ADDR, T_COVID, T_PUBKEY};
use common::{
    bytecode, compile_contract, compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script, state_layout,
};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const OTHER_COV: Hash = Hash::from_bytes([0x71; 32]);
const FAKE_COV: Hash = Hash::from_bytes([0x66; 32]);
/// Maker / taker held tokens and deliveries: KRON id_type 3 (address presence). Custody: id_type 2.
const SCHEME_P2PK: u8 = T_ADDR;
const SCHEME_COVID: u8 = T_COVID;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;

const CARRIER: i64 = 10 * KAS;
const DC: i64 = 10 * KAS; // bid delivery carrier
/// Base units per whole token of the test token (scale = 10^3): prices and tips are sompi per SCALE base units.
const SCALE: i64 = 1_000;
/// Default minimum fill of every order of the fixtures (one whole token).
const MIN_FILL: i64 = 1_000;
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
/// Repeat merge argument -(k * MERGE_K + m) (k = exit input index, m < MERGE_K).
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

/// The covenants' quoteOf: the quote value of n base units at rate r (quote units per `scale` base units), with the
/// split formula q * r + m * (r / scale) + (m * (r % scale) + c) / scale (q = n / scale, m = n % scale), c = scale - 1
/// (ceil, `up`) or 0 (floor). Evaluated left to right with checked i64 arithmetic like the engine: None where the
/// script would fail on an overflow.
fn quote_of(n: i64, r: i64, scale: i64, up: bool) -> Option<i64> {
    let c = if up { scale - 1 } else { 0 };
    let (q, m) = (n / scale, n % scale);
    let a = q.checked_mul(r)?;
    let b = m.checked_mul(r / scale)?;
    let d = m.checked_mul(r % scale)?.checked_add(c)? / scale;
    a.checked_add(b)?.checked_add(d)
}
/// quoteOf at the fixtures' scale, rounded up (what a maker receives / a budget a bid consumes).
fn q_up(n: i64, r: i64) -> i64 {
    quote_of(n, r, SCALE, true).expect("quote overflow")
}
/// quoteOf at the fixtures' scale, rounded down (what a maker pays).
fn q_down(n: i64, r: i64) -> i64 {
    quote_of(n, r, SCALE, false).expect("quote overflow")
}
/// All-in minimum an ask (quote `p`, tip `tip`) receives for n base units (ceil).
fn ask_all_in(n: i64, p: i64, tip: i64) -> i64 {
    q_up(n, p - tip)
}
/// All-in maximum a bid (quote `p`, tip `tip`) pays for n base units (floor).
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
            amount: 10_000,
            min_fill: MIN_FILL,
            token: TOKEN_COV,
            scale: SCALE,
        }
    }
    /// What the maker receives for n base units at the quote `eff` (ceil; 0 when the quote is below the tip).
    fn pay(&self, n: i64, eff: i64) -> i64 {
        quote_of(n, eff - self.tip, self.scale, true).expect("ask quote overflow").max(0)
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
    /// The quote budgets are taken at: the price, or the cap of a rising bid.
    fn pmax(&self) -> i64 {
        if self.slope != 0 && self.price_end > self.price {
            self.price_end
        } else {
            self.price
        }
    }
    /// Budget n base units consume from the escrow (all-in at pMax, ceil).
    fn used(&self, n: i64) -> i64 {
        quote_of(n, self.pmax() + self.tip, self.scale, true).expect("bid budget overflow")
    }
    /// What the maker pays for n base units at the quote `eff` (floor).
    fn spend(&self, n: i64, eff: i64) -> i64 {
        quote_of(n, eff + self.tip, self.scale, false).expect("bid spend overflow")
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
/// Trigger evidence (touch): a plain resting KobAsk (ask) or KobBid of maker C, of which the matcher fills all of
/// `amount` in the same transaction. Exposed at its quote since max(daa + interval, activeFrom, custody DAA (ask)).
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
            amount: 5_000,
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
    min_touch: i64,
    min_rest: i64,
    armed: i64,
    band_daa: i64,
    keeper_tip: i64,
    amount: i64,
    min_fill: i64,
    /// repeat IFD: the entry this exit re-arms (0 = none), its budget rate and deadline
    parent: [u8; 32],
    rpt_price: i64,
    rpt_until: i64,
}
impl CondP {
    /// OCO: take-profit at 3.00, stop 2.00 with the default 3% band (stop leg 1.94), evidence fills of
    /// >= 1 base unit exposed >= 600 DAA (minRestDaa R = 600 in this harness; the wallet default is 50).
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
            min_touch: 1,
            min_rest: 600,
            armed: 0,
            band_daa: 0,
            keeper_tip: 0,
            amount: 10_000,
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
    amount: i64,
    price: i64,
    tip: i64,
    exit: CondP,
    min_fill: i64,
    entry_stop: i64,
    band_daa: i64,
    keeper_tip: i64,
    armed: i64,
    /// repeat: 0 = off, else 1 + base units of re-arms left
    rpt: i64,
    /// stop entry: smallest evidence fill (base units)
    min_touch: i64,
}
impl IfdP {
    /// A limit IFO entry (no stop trigger), fills of at least one whole token.
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
            min_touch: 1,
        }
    }
    /// The all-in budget rate (price + tip, sompi per whole token): the booked exit's rptPrice.
    fn rate(&self) -> i64 {
        self.price + self.tip
    }
}
/// Escrow of an if-done entry for `amount` at the all-in rate `rate`: the budget (ceil) plus a delivery and an exit
/// carrier per fill (at most ceil(amount / min_fill) fills).
fn ifd_value(amount: i64, rate: i64, min_fill: i64) -> i64 {
    let fills = if amount == 0 { 0 } else { (amount + min_fill - 1) / min_fill };
    q_up(amount, rate) + fills * (DC + EC)
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
        vec![bytes(&token.as_bytes()), bytes(&self.t.k.hash), int(0), int(self.t.k.suffix.len() as i64)]
    }
    /// The evidence templates a conditional order / stop entry inlines (KobAskKron, then KobBidKron:
    /// hash, prefix and suffix length), as in contracts/adapters/kron/v2/KobCondAskKron.ctor.json.
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
        ]);
        compile_contract(&src("KobAskKron"), &a, CompileOptions::default()).expect("compile KobAskKron")
    }
    fn bid(&self, p: &BidP) -> SilAbiArtifact {
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
            int(0),
            int(DC),
            int(p.interval),
            int(p.max_fill),
            int(p.slope),
            int(p.price_end),
            int(p.decay_step),
        ]);
        compile_contract(&src("KobBidKron"), &a, CompileOptions::default()).expect("compile KobBidKron")
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
        ]);
        compile_contract(&src("KobCondAskKron"), &a, CompileOptions::default()).expect("compile KobCondAskKron")
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
        compile_contract(&src("KobIfdBidKron"), &a, CompileOptions::default()).expect("compile KobIfdBidKron")
    }
}

// ---------------------------------------------------------------- token (real KRON program)

/// A pinned KRON token program (2,433 B or 2,732 B), see common/kron.rs.
struct Token {
    k: Kron,
}
impl Token {
    fn load(file: &str) -> Self {
        Token { k: Kron::load(file) }
    }
    fn redeem(&self, amount: i64, owner: &[u8; 32], typ: u8) -> Vec<u8> {
        self.k.redeem(&ks(owner, typ, amount))
    }
    fn spk(&self, amount: i64, owner: &[u8; 32], typ: u8) -> ScriptPublicKey {
        self.k.spk(&ks(owner, typ, amount))
    }
}
/// Next state of a token output (KRON id_type in place of the KCC-20 owner scheme).
fn tok_state(amount: i64, owner: &[u8; 32], typ: u8) -> KS {
    ks(owner, typ, amount)
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
    /// First argument only: an int pushed as a non-minimal 8-byte push (0x08 || 8-byte LE), which the engine
    /// accepts as the same number.
    Wide(i64),
    /// First argument only: a byte argument pushed with OP_PUSHDATA1 (0x4c || len || data) instead of a direct push.
    Pd1(Vec<u8>),
}
fn nb(n: i64) -> Arg {
    Arg::V(bytes(&n.to_le_bytes()))
}
fn iv(i: i64) -> Arg {
    Arg::V(int(i))
}
/// Witness byte of a KRON token input (index of the input that authorises it). `CovId` / `P2pk` are
/// resolved by `build` from the scenario's inputs (id_type 2: the input carrying the owner covenant id,
/// id_type 3: the input whose script public key is P2PK(owner)); `Idx` forces an index (attacks).
#[derive(Clone)]
#[allow(dead_code)]
enum Wit {
    CovId,
    P2pk(Keypair),
    Idx(u8),
}
#[derive(Clone)]
enum Role {
    Call {
        art: SilAbiArtifact,
        entry: &'static str,
        args: Vec<Arg>,
        name: &'static str,
    },
    /// A KRON token input. `next` (the output states of the whole covenant group) is carried by ONE input of
    /// the scenario (the "leader" of the KCC-20 suite) and shared by all token inputs at build time.
    Tok {
        redeem: Vec<u8>,
        next: Option<Vec<KS>>,
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
    leader: Option<Vec<KS>>,
    wit: Wit,
    daa: u64,
) -> Inp {
    let redeem = t.redeem(amount, owner, scheme);
    let entry = utxo(value, pay_to_script_hash_script(&redeem), c, daa);
    Inp { entry, role: Role::Tok { redeem, next: leader, wit }, seq: 0 }
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

/// Witness index for a token input with state `redeem[..46]`: the first input that authorises its owner.
fn auto_witness(s: &Scn, redeem: &[u8]) -> u8 {
    let owner: [u8; 32] = redeem[1..33].try_into().unwrap();
    let found = match redeem[34] {
        T_COVID => {
            s.inputs.iter().position(|i| i.entry.covenant_id == Some(Hash::from_bytes(owner)) && !matches!(i.role, Role::Tok { .. }))
        }
        T_ADDR => s.inputs.iter().position(|i| i.entry.script_public_key == p2pk_spk(&owner)),
        _ => None,
    };
    let at = found.expect("no authorising input for token owner");
    assert!(at < 128, "a KRON witness is one byte read as a signed script number: the authorising input must sit below index 128");
    at as u8
}

fn build(_net: &Net, s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
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
    // KRON token columns, per token covenant (group): one shared next-state column set, one witness byte per
    // token input of the group.
    let next_cols = |c: Option<Hash>| -> Vec<KS> {
        s.inputs
            .iter()
            .filter(|i| i.entry.covenant_id == c)
            .find_map(|i| match &i.role {
                Role::Tok { next: Some(n), .. } => Some(n.clone()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let wit_col = |c: Option<Hash>| -> Vec<u8> {
        s.inputs
            .iter()
            .filter(|i| i.entry.covenant_id == c)
            .filter_map(|i| match &i.role {
                Role::Tok { redeem, wit, .. } => Some(match wit {
                    Wit::Idx(b) => *b,
                    _ => auto_witness(s, redeem),
                }),
                _ => None,
            })
            .collect()
    };
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
                        Arg::Wide(v) => int(*v),
                        Arg::Pd1(b) => bytes(b),
                    })
                    .collect();
                let ss = entry_ss(art, entry, &vals);
                match args.first() {
                    Some(Arg::Wide(v)) => {
                        // the minimal push of v is as long as the encoding grows over OP_0's single byte
                        let mut z = vals.clone();
                        z[0] = int(0);
                        let l = ss.len() + 1 - entry_ss(art, entry, &z).len();
                        let mut w = vec![0x08];
                        w.extend(v.to_le_bytes());
                        w.extend_from_slice(&ss[l..]);
                        w
                    }
                    Some(Arg::Pd1(b)) => {
                        assert_eq!(ss[0] as usize, b.len(), "a direct push of the first argument");
                        [vec![0x4c], ss].concat()
                    }
                    _ => ss,
                }
            }
            Role::Tok { redeem, .. } => kron_ss(redeem, &next_cols(inp.entry.covenant_id), &[], &wit_col(inp.entry.covenant_id)),
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
        Role::Tok { .. } => "kron.token",
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
    // (the storage mass divides by every amount: not computed for scenarios with zero-value inputs or outputs)
    let zero = tx.outputs.iter().any(|o| o.value == 0) || entries.iter().any(|e| e.amount == 0);
    let storage = if zero { u64::MAX } else { mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX) };
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
fn set_leader_next(s: &mut Scn, idx: usize, new_next: Vec<KS>) {
    if let Role::Tok { next, .. } = &mut s.inputs[idx].role {
        *next = Some(new_next);
    }
}

// ---------------------------------------------------------------- fixtures

struct Fx {
    net: Net,
    /// The other pinned KRON template (foreign-template attacks).
    other: Kron,
    maker_a: Keypair,
    maker_b: Keypair,
    maker_c: Keypair,
    taker: Keypair,
    matcher: Keypair,
}
/// Fixture for a pinned KRON template file (primary), with the other template as the foreign one.
fn fx_with(tpl: &str) -> Fx {
    let other = if tpl == TPL_2433 { TPL_2732 } else { TPL_2433 };
    Fx {
        net: Net::with_token(Token::load(tpl)),
        other: Kron::load(other),
        maker_a: keypair(),
        maker_b: keypair(),
        maker_c: keypair(),
        taker: keypair(),
        matcher: keypair(),
    }
}
/// Both pinned templates: 2,433 B (primary, common) and 2,732 B (newer).
const TEMPLATES: [&str; 2] = [TPL_2433, TPL_2732];

const P250: i64 = 250_000_000; // 2.50 KAS per whole token
const P255: i64 = 255_000_000;
const P260: i64 = 260_000_000;
const P245: i64 = 245_000_000;

/// S1: a taker (who is also the matcher of its own tx) buys n of the 10,000 base units of one ask and pays
/// exactly its all-in price (so it keeps the ask's tip).
fn s1(f: &Fx) -> Scn {
    s1_with(f, &AskP::new(pk(&f.maker_a), P250), 4_000)
}
fn s1_with(f: &Fx, p: &AskP, n: i64) -> Scn {
    s1_at(f, p, n, p.price, 0)
}
/// As s1_with, with the effective quote `eff` (decay) and the decay time argument `t`.
fn s1_at(f: &Fx, p: &AskP, n: i64, eff: i64, t: i64) -> Scn {
    let t_ = &f.net.t;
    let a = cov(0xa1);
    let ask = f.net.ask(p);
    let taker = pk(&f.taker);
    let pay = p.pay(n, eff);
    let rest = p.amount - n;
    Scn {
        name: format!("S1 taker buys {n} of {} from 1 ask quoting {}", p.amount, p.price),
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

/// S2: a matcher with no capital crosses 1 bid (quote 2.60, 8 whole tokens) with 2 asks (A 2.50 full 5,
/// C 2.55 partial 3 of 10), every order at its own all-in price. The matcher keeps the crossing spread plus the tips.
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
    ap.amount = 5_000;
    let mut cp = AskP::new(pk(&f.maker_c), P255);
    cp.tip = tip;
    let bid = f.net.bid(&bp);
    let ask_a = f.net.ask(&ap);
    let ask_c = f.net.ask(&cp);
    let bid_value = bp.used(8_000) + DC;
    let spend = bp.spend(8_000, P260);
    let (pay_a, pay_c) = (ap.pay(5_000, P250), cp.pay(3_000, P255));
    Scn {
        name: format!("S2 matcher crosses 1 bid x 2 asks, each at its own all-in price (tips {tip})"),
        inputs: vec![
            call(&bid, "fill", vec![nb(8_000), iv(3), iv(0)], "bid.fill", bid_value, b, 1_000),
            call(&ask_a, "settle", vec![nb(5_000), iv(3), iv(0), iv(0)], "ask.settle", CARRIER, ca, 1_000),
            call(&ask_c, "settle", vec![nb(3_000), iv(4), iv(4), iv(0)], "ask.settle", CARRIER, cc, 1_000),
            tok_in(
                t,
                CARRIER,
                5_000,
                &ca.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(8_000, &pk(&f.maker_b), SCHEME_P2PK), tok_state(7_000, &cc.as_bytes(), SCHEME_COVID)]),
                Wit::CovId,
                1_000,
            ),
            tok_in(t, CARRIER, 10_000, &cc.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000),
        ],
        outputs: vec![
            out(bid_value - spend, t.spk(8_000, &pk(&f.maker_b), SCHEME_P2PK), Some((3, TOKEN_COV))),
            out(pay_a + 2 * CARRIER, p2pk_spk(&pk(&f.maker_a)), None),
            out(pay_c, p2pk_spk(&pk(&f.maker_c)), None),
            out(CARRIER, spk_of(&f.net.ask(&AskP { amount: 7_000, ..cp.clone() })), Some((2, cc))),
            out(CARRIER, t.spk(7_000, &cc.as_bytes(), SCHEME_COVID), Some((3, TOKEN_COV))),
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
    let (v1, v2) = (bp1.used(10_000) + 3 * DC, bp2.used(2_000) + DC);
    let (sp1, sp2) = (bp1.spend(4_000, P245), bp2.spend(2_000, P250));
    Scn {
        name: "S3 taker sells into 2 bids (1 partial + 1 exhausted)".into(),
        inputs: vec![
            call(&bid1, "fill", vec![nb(4_000), iv(2), iv(0)], "bid.fill", v1, b1, 1_000),
            call(&bid2, "fill", vec![nb(2_000), iv(2), iv(0)], "bid.fill", v2, b2, 1_000),
            tok_in(
                t,
                CARRIER,
                6_000,
                &taker,
                SCHEME_P2PK,
                TOKEN_COV,
                Some(vec![tok_state(4_000, &pk(&f.maker_b), SCHEME_P2PK), tok_state(2_000, &pk(&f.maker_c), SCHEME_P2PK)]),
                Wit::P2pk(f.taker),
                1_000,
            ),
            // KRON id_type 3 (address presence): the taker's P2PK input authorises the tokens it holds
            p2pk_in(&f.taker, KAS),
        ],
        outputs: vec![
            out(DC + bp1.used(4_000) - sp1, t.spk(4_000, &pk(&f.maker_b), SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(v2 - sp2, t.spk(2_000, &pk(&f.maker_c), SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(v1 - bp1.used(4_000) - DC, spk_of(&bid1), Some((0, b1))),
            out(sp1 + sp2 + CARRIER + KAS - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S4: IOC ask, 4,000 of 10,000 filled; the other 6,000 go straight back to the maker.
fn s4(f: &Fx) -> Scn {
    let t = &f.net.t;
    let a = cov(0xa1);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    let ask = f.net.ask(&p);
    let taker = pk(&f.taker);
    let pay = p.pay(4_000, P250);
    Scn {
        name: "S4 IOC (market with slippage bound) ask: 4/10 filled, remainder returned".into(),
        inputs: vec![
            call(&ask, "settle", vec![nb(4_000), iv(1), iv(1), iv(0)], "ask.settle", CARRIER, a, 1_000),
            tok_in(
                t,
                CARRIER,
                10_000,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(6_000, &p.maker, SCHEME_P2PK), tok_state(4_000, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay + CARRIER, p2pk_spk(&p.maker), None),
            out(CARRIER, t.spk(6_000, &p.maker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(CARRIER, t.spk(4_000, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - pay - 2 * CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S5: FOK ask filled completely (10/10).
fn s5(f: &Fx) -> Scn {
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 2;
    with_name(sell_out(f, &p, P250), "S5 FOK ask filled 10/10")
}
/// A taker buys everything an ask holds (p.amount) at the quote `eff`: maker paid the all-in price plus both carriers.
fn sell_out(f: &Fx, p: &AskP, eff: i64) -> Scn {
    sell_out_paying(f, p, p.pay(p.amount, eff) as u64)
}
/// As sell_out, the maker paid `pay` (u64: the overflow scenarios pay more than i64::MAX) plus both carriers.
fn sell_out_paying(f: &Fx, p: &AskP, pay: u64) -> Scn {
    let t = &f.net.t;
    let a = cov(0xa1);
    let ask = f.net.ask(p);
    let taker = pk(&f.taker);
    let n = p.amount;
    let mut fund = p2pk_in(&f.taker, 0);
    fund.entry = UtxoEntry::new(pay + (1000 * KAS) as u64, p2pk_spk(&taker), 0, false, None);
    let mut maker = out(0, p2pk_spk(&p.maker), None);
    maker.value = pay + (2 * CARRIER) as u64;
    Scn {
        name: "sell out".into(),
        inputs: vec![
            call(&ask, "settle", vec![nb(n), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000),
            tok_in(
                t,
                CARRIER,
                n,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(n, &taker, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
            fund,
        ],
        outputs: vec![
            maker,
            out(CARRIER, t.spk(n, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}

/// S6: bid variants: a taker sells n base units into one bid funded for `funded` base units.
/// tif 1 IOC / 2 FOK terminate; tif 0 with more budget continues (DCA and rising bids too).
fn s6_with(f: &Fx, bp: &BidP, n: i64, funded: i64, eff: i64, t: i64) -> Scn {
    let mut s = s6_v(f, bp, n, bp.used(funded) + DC, eff, t);
    s.name = format!("S6 taker sells {n} into a bid (tif {}, funded {funded})", bp.tif);
    s
}
/// As s6_with, with the bid's escrow value `v`. The outputs follow the contract: the bid continues (GTC) while
/// left - deliveryCarrier >= the budget of one minimum fill (ceil), else it terminates.
fn s6_v(f: &Fx, bp: &BidP, n: i64, v: i64, eff: i64, t: i64) -> Scn {
    let cont = bp.tif == 0 && v - bp.used(n) - DC >= bp.used(bp.min_fill);
    s6_shape(f, bp, n, v, eff, t, cont)
}
/// As s6_v with the shape forced: `cont` = the bid continues (delivery DC + used - spend, continuation
/// left - DC), else it terminates (delivery v - spend).
fn s6_shape(f: &Fx, bp: &BidP, n: i64, v: i64, eff: i64, t: i64, cont: bool) -> Scn {
    let tk = &f.net.t;
    let b = cov(0xb1);
    let bid = f.net.bid(bp);
    let taker = pk(&f.taker);
    let spend = bp.spend(n, eff);
    let used = bp.used(n);
    let left = v - used;
    let mut outputs = vec![];
    if cont {
        outputs.push(out(DC + used - spend, tk.spk(n, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))));
        outputs.push(out(left - DC, spk_of(&bid), Some((0, b))));
    } else {
        outputs.push(out(v - spend, tk.spk(n, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    }
    outputs.push(out(spend + CARRIER + KAS - NET_FEE, p2pk_spk(&taker), None));
    Scn {
        name: format!("S6 taker sells {n} into a bid (tif {}, value {v})", bp.tif),
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
            p2pk_in(&f.taker, KAS),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
    }
}
fn s6(f: &Fx, tif: i64) -> Scn {
    let mut bp = BidP::new(pk(&f.maker_b), P245);
    bp.tif = tif;
    // FOK: funded for exactly the 4,000 base units
    s6_with(f, &bp, 4_000, if tif == 2 { 4_000 } else { 10_000 }, P245, 0)
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
                10_000,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(10_000, &m, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
        ],
        outputs: vec![out(CARRIER, t.spk(10_000, &m, SCHEME_P2PK), Some((1, TOKEN_COV))), out(CARRIER - NET_FEE, p2pk_spk(&m), None)],
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
                10_000,
                &a.as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(10_000, &m, SCHEME_P2PK)]),
                Wit::CovId,
                1_000,
            ),
        ],
        outputs: vec![out(2 * CARRIER - REFUND_TIP, t.spk(10_000, &m, SCHEME_P2PK), Some((1, TOKEN_COV)))],
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

// ---------------------------------------------------------------- trigger evidence (touch)

/// Appends a KRON token input of covenant `token`. The group's next-state columns are carried by one of its
/// token inputs (build() hands every input of the group the same columns): if `s` already has a token input
/// of that covenant, `new_states` (the states of token outputs the caller appends after every existing
/// output) join those columns; else the new input carries `new_states`. Returns the index of the input
/// carrying the columns (the authorising input of the group's outputs).
#[allow(clippy::too_many_arguments)]
fn push_tok(
    f: &Fx,
    s: &mut Scn,
    value: i64,
    amount: i64,
    owner: &[u8; 32],
    typ: u8,
    token: Hash,
    wit: Wit,
    daa: u64,
    new_states: Vec<KS>,
) -> usize {
    let holder = s.inputs.iter().position(|i| i.entry.covenant_id == Some(token) && matches!(i.role, Role::Tok { next: Some(_), .. }));
    match holder {
        Some(h) => {
            if let Role::Tok { next: Some(n), .. } = &mut s.inputs[h].role {
                n.extend(new_states);
            }
            s.inputs.push(tok_in(&f.net.t, value, amount, owner, typ, token, None, wit, daa));
            h
        }
        None => {
            s.inputs.push(tok_in(&f.net.t, value, amount, owner, typ, token, Some(new_states), wit, daa));
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
/// the fill); a bid buys the matcher's tokens (input ei + 1, id_type 3: the matcher's P2PK input follows). The
/// matcher takes the change, so the evidence part balances by itself. Returns (ev, tk) as the conditional orders
/// take them (tk = -1 for a bid).
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
            let pay = if e.mode == EvMode::Fill { ap.pay(n, e.price) + 2 * CARRIER } else { CARRIER };
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
        let v = bp.used(n) + DC;
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
            let st = vec![tok_state(n, &mc, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n, &matcher, SCHEME_P2PK, e.token, Wit::P2pk(f.matcher), 1_000, st);
            // KRON: the matcher's tokens (id_type 3) need its P2PK input in the transaction
            s.inputs.push(p2pk_in(&f.matcher, KAS));
            s.outputs.push(out(v - bp.spend(n, e.price), t.spk(n, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
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
/// As cond_fill, with the auction time argument `t` and the leg price the fill pays.
#[allow(clippy::too_many_arguments)]
fn cond_fill_at(f: &Fx, cp: &CondP, n: i64, leg: i64, next_cp: &CondP, ev: Option<&Ev>, t_arg: i64, leg_price: i64) -> Scn {
    let t = &f.net.t;
    let c = cov(0xc1);
    let cond = f.net.cond(cp);
    let rest = cp.amount - n;
    let next = f.net.cond(&CondP { amount: next_cp.amount - n, ..next_cp.clone() });
    let taker = pk(&f.taker);
    let pay = q_up(n, leg_price - cp.tip).max(0);
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
/// Up evidence: a resting bid quoting 2.20 filled with 5 whole tokens (trails a 2.00 stop by two 0.05 steps with
/// the 0.10 gap).
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
    /// the entry's limit and tip (default 2.60 and TIP)
    price: i64,
    tip: i64,
    exit: Option<CondP>,
    to_maker: bool,
    double_exit: bool,
    self_id_exit: bool,
    foreign_exit: bool,
    cont_amount: Option<i64>,
    cont_delta: i64,
    exit_delta: i64,
    ask_template: bool,
    /// KRON custody attacks on the exit's token state: id_type and is_minter of the delivered tokens.
    deliver_type: Option<u8>,
    deliver_minter: u8,
    /// stop entry: trigger price, auction length, armed state, minimum fill
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
}
impl IfdKnobs {
    fn new(amount_left: i64, n: i64) -> Self {
        IfdKnobs {
            amount_left,
            n,
            deliver: n,
            price: P260,
            tip: TIP,
            exit: None,
            to_maker: false,
            double_exit: false,
            self_id_exit: false,
            foreign_exit: false,
            cont_amount: None,
            cont_delta: 0,
            exit_delta: 0,
            ask_template: false,
            deliver_type: None,
            deliver_minter: 0,
            entry_stop: 0,
            band_daa: 0,
            armed: 0,
            min_fill: MIN_FILL,
            ev: None,
            min_touch: 1,
            t: 0,
            eff: None,
            next_armed: None,
            rpt: 0,
            next_rpt: None,
            exit_rpt: None,
            terminate: false,
        }
    }
}
/// A taker sells n base units into an if-done bid (quote 2.60, cov 0xd1) with amountLeft left. Outputs:
/// [0] tokens into the exit's custody, [1] entry continuation (if any remains), then the exit (a
/// fresh genesis covenant authorised by the entry), then the taker's KAS.
fn ifd_fill(f: &Fx, k: &IfdKnobs) -> Scn {
    let t = &f.net.t;
    let i = cov(0xd1);
    let foreign = cov(0x99);
    let m = pk(&f.maker_a);
    let mut ip = IfdP::limit(m, k.amount_left, k.price, ifd_exit(m));
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
    let v = ifd_value(k.amount_left, ip.rate(), k.min_fill) + if k.rpt > 0 { EC } else { 0 };
    let spend = q_down(k.n, k.eff.unwrap_or(k.price) + k.tip);
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
            p2pk_in(&f.taker, KAS),
        ],
        outputs: vec![out(DC, p2pk_spk(&taker), Some((1, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
    };
    let mut taker_kas = spend + CARRIER + KAS - NET_FEE;
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
    s.outputs.push(out(taker_kas.max(0), p2pk_spk(&taker), None));
    if taker_kas < 0 {
        // the taker tops the transaction up (large continuations of the attack scenarios)
        s.inputs.push(p2pk_in(&f.taker, -taker_kas));
    }
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
    let scheme = k.deliver_type.unwrap_or(scheme);
    let mut st = tok_state(k.deliver, &owner, scheme);
    st.minter = k.deliver_minter;
    s.outputs[0].script_public_key = t.k.spk(&st);
    set_leader_next(&mut s, 1, vec![st]);
    if let Some(ev) = &k.ev {
        let (ei, _) = add_ev(f, &mut s, ev);
        set_arg(&mut s, 0, 5, iv(ei));
    }
    s
}
fn s14(f: &Fx) -> Scn {
    with_name(
        ifd_fill(f, &IfdKnobs::new(10_000, 4_000)),
        "S14 IFO entry partial 4/10: a fresh OCO exit holds 4,000, the entry continues with 6,000",
    )
}

/// B1 batch (largest legal KRON matcher batch: 4 token inputs, 5 token outputs): 4 bids (3 whole tokens each,
/// all exhausted) x 4 asks (3 fully sold, the 4th partial 3 of 10), every order at its own all-in price,
/// plus a stop (stop 2.50) armed by update reading the 2.50 ask's fill as its evidence (touch: no
/// receipt; the arm costs one input and one output). The matcher adds its own funding input so its income
/// does not sit on a tiny UTXO. Returns (scenario, matcher gross income in sompi).
fn b1(f: &Fx, tip: i64) -> (Scn, i64) {
    let t = &f.net.t;
    let makers = [keypair(), keypair(), keypair(), keypair()];
    let bid_ps = [P260, 258_000_000, 256_000_000, 255_000_000];
    // (price, amount held, amount sold)
    let ask_ps = [(P250, 3_000i64, 3_000i64), (252_000_000, 3_000, 3_000), (253_000_000, 3_000, 3_000), (P255, 10_000, 3_000)];
    let bid_makers = [pk(&f.maker_a), pk(&f.maker_b), pk(&f.maker_c), pk(&keypair())];
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut spend_total = 0;
    let mut pay_total = 0;
    // inputs: bids 0..4, asks 4..8, token inputs 8..12, the stop's update 12, matcher funding 13
    for (i, price) in bid_ps.iter().enumerate() {
        let mut bp = BidP::new(bid_makers[i], *price);
        bp.tip = tip;
        let bid = f.net.bid(&bp);
        let v = bp.used(3_000) + DC;
        let c = bp.spend(3_000, *price);
        spend_total += c;
        inputs.push(call(&bid, "fill", vec![nb(3_000), iv(8), iv(0)], "bid.fill", v, cov(0xb1 + i as u8), 1_000));
        outputs.push(out(v - c, t.spk(3_000, &bid_makers[i], SCHEME_P2PK), Some((8, TOKEN_COV))));
    }
    let mut toks = vec![];
    let mut ask4 = None;
    for (i, (price, amount, sold)) in ask_ps.iter().enumerate() {
        let c = cov(0xa1 + i as u8);
        let mut ap = AskP::new(pk(&makers[i]), *price);
        ap.tip = tip;
        ap.amount = *amount;
        let ask = f.net.ask(&ap);
        let pay = ap.pay(*sold, *price);
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
            ask4 = Some(f.net.ask(&AskP { amount: amount - sold, ..ap.clone() }));
        }
    }
    let a4 = cov(0xa4);
    outputs.push(out(CARRIER, spk_of(&ask4.unwrap()), Some((7, a4))));
    outputs.push(out(CARRIER, t.spk(7_000, &a4.as_bytes(), SCHEME_COVID), Some((8, TOKEN_COV))));
    let mut next: Vec<KS> = (0..4).map(|i| tok_state(3_000, &bid_makers[i], SCHEME_P2PK)).collect();
    next.push(tok_state(7_000, &a4.as_bytes(), SCHEME_COVID));
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
    // the ask at input 4 (2.50, sold out) with its custody at input 8 arms a stop at 2.50 (input 12)
    let stop = CondP { stop: P250, ..CondP::oco(pk(&f.maker_a)) };
    let c = cov(0xc1);
    inputs.push(call(&f.net.cond(&stop), "update", vec![iv(4), iv(8)], "cond.update", CARRIER, c, 2_000));
    outputs.push(out(CARRIER, spk_of(&f.net.cond(&armed(&stop))), Some((12, c))));
    inputs.push(p2pk_in(&f.matcher, 10 * KAS));
    let income = spend_total - pay_total;
    outputs.push(out(10 * KAS + income - NET_FEE, p2pk_spk(&owner), None));
    (
        Scn {
            name: format!(
                "B1 batch: 4 bids x 4 asks at own prices + a stop armed by one of the fills (KRON limit 4 in / 5 out, tips {tip})"
            ),
            inputs,
            outputs,
            lock_time: NOW,
            payload: vec![],
        },
        income,
    )
}

/// L1: a taker sweeps `cnt` asks (each holding 2 whole tokens, all sold out) into ONE token output. cnt token
/// inputs: 4 is the KRON maximum.
fn sweep(f: &Fx, cnt: usize) -> Scn {
    let t = &f.net.t;
    let taker = pk(&f.taker);
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut pays = 0;
    let mut covs = vec![];
    for i in 0..cnt {
        let c = cov(0xa1 + i as u8);
        covs.push(c);
        let maker = pk(&f.maker_a);
        let mut ap = AskP::new(maker, P250);
        ap.amount = 2_000;
        let ask = f.net.ask(&ap);
        let pay = ap.pay(2_000, P250);
        pays += pay;
        inputs.push(call(&ask, "settle", vec![nb(2_000), iv((cnt + i) as i64), iv(0), iv(0)], "ask.settle", CARRIER, c, 1_000));
        outputs.push(out(pay + 2 * CARRIER, p2pk_spk(&maker), None));
    }
    for (i, c) in covs.iter().enumerate() {
        let leader = if i == 0 { Some(vec![tok_state(2_000 * cnt as i64, &taker, SCHEME_P2PK)]) } else { None };
        inputs.push(tok_in(t, CARRIER, 2_000, &c.as_bytes(), SCHEME_COVID, TOKEN_COV, leader, Wit::CovId, 1_000));
    }
    inputs.push(p2pk_in(&f.taker, 1000 * KAS));
    outputs.push(out(CARRIER, t.spk(2_000 * cnt as i64, &taker, SCHEME_P2PK), Some((cnt as u16, TOKEN_COV))));
    outputs.push(out(1000 * KAS - pays - CARRIER - NET_FEE, p2pk_spk(&taker), None));
    Scn {
        name: format!("L1 taker sweeps {cnt} asks (sold out) with {cnt} token inputs"),
        inputs,
        outputs,
        lock_time: NOW,
        payload: vec![],
    }
}
/// L2: a taker sells one whole token each into `cnt` bids: one token input, `cnt` token outputs (KRON max 5).
fn fan_out(f: &Fx, cnt: usize) -> Scn {
    let t = &f.net.t;
    let taker = pk(&f.taker);
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut next = vec![];
    let mut spend_total = 0;
    for i in 0..cnt {
        let maker = pk(&keypair());
        let bp = BidP::new(maker, P245);
        let bid = f.net.bid(&bp);
        let v = bp.used(1_000) + DC;
        let spend = bp.spend(1_000, P245);
        spend_total += spend;
        inputs.push(call(&bid, "fill", vec![nb(1_000), iv(cnt as i64), iv(0)], "bid.fill", v, cov(0xb1 + i as u8), 1_000));
        outputs.push(out(v - spend, t.spk(1_000, &maker, SCHEME_P2PK), Some((cnt as u16, TOKEN_COV))));
        next.push(tok_state(1_000, &maker, SCHEME_P2PK));
    }
    inputs.push(tok_in(t, CARRIER, cnt as i64 * 1_000, &taker, SCHEME_P2PK, TOKEN_COV, Some(next), Wit::P2pk(f.taker), 1_000));
    inputs.push(p2pk_in(&f.taker, KAS));
    outputs.push(out(spend_total + CARRIER + KAS - NET_FEE, p2pk_spk(&taker), None));
    Scn { name: format!("L2 taker sells into {cnt} bids with {cnt} token outputs"), inputs, outputs, lock_time: NOW, payload: vec![] }
}

/// TWAP ask: at most 2,000 base units per fill, fills at least 600 DAA apart (CSV on the order UTXO).
fn twap_ask(f: &Fx) -> AskP {
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.interval = 600;
    p.max_fill = 2_000;
    p
}
/// DCA bid: at most 1,000 base units per fill, fills at least 600 DAA apart.
fn dca_bid(f: &Fx) -> BidP {
    let mut p = BidP::new(pk(&f.maker_b), P245);
    p.interval = 600;
    p.max_fill = 1_000;
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

// ---------------------------------------------------------------- v2.3 lifecycle fixtures

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
/// IOC ask (tif 1) sells n of its 10,000 at quote `eff` with decay time t; the rest goes back to
/// the maker (S4 shape).
fn ioc_fill(f: &Fx, p: &AskP, n: i64, eff: i64, t: i64) -> Scn {
    let tk = &f.net.t;
    let a = cov(0xa1);
    let ask = f.net.ask(p);
    let taker = pk(&f.taker);
    let pay = p.pay(n, eff);
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
    let v = ifd_value(ip.amount, ip.rate(), ip.min_fill);
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_sizes_layouts_sigops_body(&fx_with(tpl));
    }
}
fn v2_sizes_layouts_sigops_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let arts = [
        ("KobAskKron", n.ask(&AskP::new(m, P250)), 243),
        ("KobBidKron", n.bid(&BidP::new(m, P250)), 252),
        ("KobCondAskKron", n.cond(&CondP::oco(m)), 330),
        ("KobIfdBidKron", n.ifd(&IfdP::limit(m, 10_000, P260, ifd_exit(m))), 552),
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
    ap.min_fill = 1_234;
    let ask = bytecode(&n.ask(&ap));
    let s = &ask[1..244];
    assert_eq!(&s[34..66], &TOKEN_COV.as_bytes(), "ask tokenCovId offset (touch)");
    assert_eq!(&s[67..99], &n.t.k.hash[..], "ask tokenTplHash offset");
    assert_eq!(le(s, 100, 108), 0, "ask tplPrefixLen offset (KRON: state at template offset 0)");
    assert_eq!(le(s, 109, 117), n.t.k.suffix.len() as i64, "ask tplSuffixLen offset");
    assert_eq!(le(s, 118, 126), SCALE, "ask scale offset");
    assert_eq!(le(s, 127, 135), 1_234, "ask minFill offset");
    assert_eq!(le(s, 136, 144), ap.price, "ask price offset");
    assert_eq!(le(s, 163, 171), ap.active_from, "ask activeFrom offset");
    assert_eq!(le(s, 190, 198), 77, "ask interval offset");
    assert_eq!(le(s, 208, 216), ap.slope, "ask slope offset");
    let mut bp = rising_bid(&f);
    bp.interval = 88;
    bp.min_fill = 4_321;
    let bid = bytecode(&n.bid(&bp));
    // no extension commitment in the KRON bid: everything after tplSuffixLen sits 33 bytes earlier
    let s = &bid[1..253];
    assert_eq!(le(s, 118, 126), SCALE, "bid scale offset");
    assert_eq!(le(s, 127, 135), 4_321, "bid minFill offset");
    assert_eq!(le(s, 136, 144), bp.price, "bid price offset");
    assert_eq!(le(s, 163, 171), bp.active_from, "bid activeFrom offset");
    assert_eq!(le(s, 208, 216), 88, "bid interval offset");
    assert_eq!(le(s, 226, 234), bp.slope, "bid slope offset");
    assert_eq!(le(s, 244, 252), bp.decay_step, "bid decayStep offset (the last state field)");
    let mut cp = CondP::oco(m);
    cp.armed = 1;
    let c = bytecode(&n.cond(&cp));
    assert_eq!(le(&c, 182, 190), cp.stop, "cond stopPrice splice window");
    assert_eq!(le(&c, 245, 253), 1, "cond armed splice window");
    let mut ap = AskP::new(m, P250);
    ap.decay_step = 55;
    ap.amount = 66_000;
    let ab = bytecode(&n.ask(&ap));
    assert_eq!(le(&ab[1..244], 226, 234), 55, "ask decayStep offset");
    assert_eq!((ab[235], le(&ab, 236, 244)), (0x08, 66_000), "ask amountLeft splice window (the last state field)");
    let mut cl = CondP::oco(m);
    cl.amount = 77_000;
    let cb = bytecode(&n.cond(&cl));
    assert_eq!((cb[271], le(&cb, 272, 280)), (0x08, 77_000), "cond amountLeft splice window");
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
    // KobIfdBidKron splices amountLeft [128..136), armed [254..262) and rptAmount [263..271) (bytecode
    // coordinates; 33 bytes earlier than KCC-20: no extension commitment).
    let mut ip = IfdP::limit(m, 7_000, P260, ifd_exit(m));
    ip.armed = 1_234;
    ip.entry_stop = 220_000_000;
    let ib = bytecode(&n.ifd(&ip));
    assert_eq!((ib[127], le(&ib, 128, 136)), (0x08, 7_000), "ifd amountLeft splice window");
    assert_eq!((ib[253], le(&ib, 254, 262)), (0x08, 1_234), "ifd armed splice window");
    let mut ipr = ip.clone();
    ipr.rpt = 5_555;
    let irb = bytecode(&n.ifd(&ipr));
    assert_eq!((irb[262], le(&irb, 263, 271)), (0x08, 5_555), "ifd rptAmount splice window");
    assert_eq!(n.cond_state(&CondP::oco(m)).len(), 279, "KobCondAsk state length (KobIfdBid exitState)");
    // The touch windows (KobCondAskKron / KobIfdBidKron touchAsk, touchBid, touch): the KRON bid's scale ..
    // activeFrom sit where the ask's do, its interval and slope 18 bytes later.
    assert_eq!(&bytecode(&n.bid(&bp))[1..253][34..66], &TOKEN_COV.as_bytes()[..], "bid tokenCovId offset (touch)");
    // The evidence templates this harness inlines are the ones the committed ctor files carry.
    let ctor = |name: &str| -> Vec<ArtifactValue> {
        serde_json::from_slice(
            &std::fs::read(common::repo_root().join(format!("contracts/adapters/kron/v2/{name}.ctor.json"))).expect("ctor"),
        )
        .expect("ctor json")
    };
    assert_eq!(ctor("KobCondAskKron")[..6], n.ev_consts()[..], "KobCondAskKron ctor: ASK_TPL/PRE/SUF, BID_TPL/PRE/SUF");
    let mut ifd_consts = n.tpl_consts(&n.cond_tpl);
    ifd_consts.extend(n.tpl_consts(&n.bid_tpl));
    assert_eq!(ctor("KobIfdBidKron")[..6], ifd_consts[..], "KobIfdBidKron ctor: COND_TPL/PRE/SUF, BID_TPL/PRE/SUF");
    // (A layout/offset check of the KRON token state itself: 46 B, owner [1..33), id_type [34], amount
    // [36..44), is_minter [45], against the codec and the real program's state prefix.)
    let st = ks(&[7u8; 32], T_COVID, 1234).bytes();
    assert_eq!(&st[1..33], &[7u8; 32]);
    assert_eq!(&st[33..36], &[0x01, T_COVID, 0x08]);
    assert_eq!(&st[36..44], &1234i64.to_le_bytes());
    assert_eq!(&st[44..46], &[0x01, 0x00]);
    assert_eq!(&n.t.k.redeem(&ks(&[7u8; 32], T_COVID, 1234))[..46], &st[..]);
    // The tested templates are the committed, reproducible artifacts (network constants resolved): the templates do
    // not depend on the token program, but the constructor placeholders of the committed artifacts pin its suffix length.
    let ifd_tpl = tpl_of(&n.ifd(&IfdP::limit(m, 10_000, P260, ifd_exit(m))));
    for (name, tpl) in
        [("KobAskKron", &n.ask_tpl), ("KobBidKron", &n.bid_tpl), ("KobCondAskKron", &n.cond_tpl), ("KobIfdBidKron", &ifd_tpl)]
    {
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_positive_limit_orders_body(&fx_with(tpl));
    }
}
fn v2_positive_limit_orders_body(f: &Fx) {
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
    run_ok(n, &with_name(s1_with(&f, &p, 4_000), "S1a timed activation: fill at lockTime == activeFrom"));
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tip = 0;
    run_ok(n, &with_name(s1_with(&f, &p, 4_000), "S1z zero-fee ask filled at exactly its quote"));
    // Linearity at whole tokens: two chained 2,000 fills pay the maker exactly what one 4,000 fill pays.
    assert_eq!(2 * ask_all_in(2_000, P250, TIP), ask_all_in(4_000, P250, TIP));
    run_ok(n, &with_name(s1_with(&f, &AskP::new(pk(&f.maker_a), P250), 2_000), "S1c partial fill of 2,000 (chain step)"));
}

#[test]
fn v2_positive_time_variants() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_positive_time_variants_body(&fx_with(tpl));
    }
}
fn v2_positive_time_variants_body(f: &Fx) {
    let n = &f.net;
    run_ok(n, &with_name(with_seq(s1_with(&f, &twap_ask(&f), 2_000), 0, 600), "T1 TWAP ask: 2,000 after 600 DAA"));
    run_ok(
        n,
        &with_name(with_seq(s6_with(&f, &dca_bid(&f), 1_000, 10_000, P245, 0), 0, 600), "T2 DCA bid: 1,000 after 600 DAA, continues"),
    );
    let d = dutch_ask(&f);
    run_ok(
        n,
        &with_name(
            s1_at(&f, &d, 4_000, decay_down(d.price, d.price_end, d.slope, 1_000, 51_000), 51_000),
            "T3 Dutch ask at t=51000: 3.00 -> 2.50",
        ),
    );
    run_ok(n, &with_name(s1_at(&f, &d, 4_000, d.price_end, 300_000), "T3b Dutch ask floored at 2.00"));
    let r = rising_bid(&f);
    let eff = rise_up(r.price, r.price_end, r.slope, 1_000, 51_000);
    run_ok(
        n,
        &with_name(s6_with(&f, &r, 4_000, 4_000, eff, 51_000), "T4 rising bid at t=51000: 2.00 -> 2.50 (budget at the 2.60 cap)"),
    );
    run_ok(n, &with_name(s6_with(&f, &r, 4_000, 10_000, eff, 51_000), "T4b rising bid partial: cap difference rides on the delivery"));
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_positive_touch_conditionals_ifd_body(&fx_with(tpl));
    }
}
fn v2_positive_touch_conditionals_ifd_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    // S10: a taker buys 4,000 of 10,000 from a resting ask at 2.50 (S1); next to it a stop at 2.50 arms reading that
    // partial fill (the ask at input 0 continues with 6,000; its custody is input 1).
    let mut s = with_name(s1(&f), "S10 a partial fill of a resting ask (S1, 4/10 at 2.50) arms a 2.50 stop next to it (update)");
    let st = CondP { stop: P250, ..CondP::oco(m) };
    let c = cov(0xc1);
    s.inputs.push(call(&n.cond(&st), "update", vec![iv(0), iv(1)], "cond.update", CARRIER, c, 2_000));
    s.outputs.push(out(CARRIER, spk_of(&n.cond(&armed(&st))), Some((3, c))));
    run_ok(n, &s);
    let oco = CondP::oco(m);
    run_ok(n, &with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "S11 OCO take-profit leg sells 4/10 at 3.00 all-in"));
    let r = down_ev();
    run_ok(
        n,
        &with_name(
            cond_fill(&f, &oco, 4_000, 1, &armed(&oco), Some(&r)),
            "S12 OCO stop leg armed in its own fill: an ask at 1.98 (<= stop) sold out in the same tx, sells 4/10 in the default 3% band (1.94), continues armed",
        ),
    );
    run_ok(
        n,
        &with_name(
            cond_update(&f, &oco, &armed(&oco), &r, 0),
            "S13 permissionless arm next to the evidence fill (no fill of the order)",
        ),
    );
    run_ok(n, &with_name(cond_fill(&f, &armed(&oco), 4_000, 1, &armed(&oco), None), "S13b armed stop leg fills without evidence"));
    let mut sl = oco.clone();
    sl.tp = 0;
    run_ok(n, &with_name(cond_fill(&f, &sl, 4_000, 1, &armed(&sl), Some(&r)), "S13c stop-limit (no TP leg) triggered fill"));
    let mut band = oco.clone();
    band.slip_bps = 100;
    run_ok(n, &with_name(cond_fill(&f, &band, 4_000, 1, &armed(&band), Some(&r)), "S13d stop-market with a user band of 1% (1.98)"));
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
            ifd_fill(&f, &IfdKnobs::new(6_000, 6_000)),
            "S14c IFO entry final 6/6: a second, independent exit with 6,000; entry terminates",
        ),
    );
    run_ok(n, &with_name(ifd_fill(&f, &IfdKnobs::new(10_000, 1_000)), "S14d IFO entry partial 1/10 (smallest exit)"));
    let exit = ifd_exit(m);
    let mut s = with_name(cond_fill(&f, &exit, 10_000, 0, &exit, None), "S14b IFO exit take-profit sells 10/10 at 3.00");
    s.outputs.remove(1);
    s.outputs.remove(1);
    s.outputs[0].value = (ask_all_in(10_000, 300_000_000, TIP) + 2 * CARRIER) as u64;
    set_arg(&mut s, 0, 2, iv(0));
    set_leader_next(&mut s, 1, vec![tok_state(10_000, &pk(&f.taker), SCHEME_P2PK)]);
    let last = s.outputs.len() - 1;
    s.outputs[last].value = (1000 * KAS - ask_all_in(10_000, 300_000_000, TIP) - CARRIER - NET_FEE) as u64;
    run_ok(n, &s);
}

#[test]
fn v2_batch_kron_limits() {
    for tpl in TEMPLATES {
        let f = fx_with(tpl);
        println!("=== KRON template {tpl}");
        batch_kron_limits(&f);
    }
}
fn batch_kron_limits(f: &Fx) {
    for tip in [TIP, 0] {
        let (s, income) = b1(f, tip);
        let fee = run_ok(&f.net, &s);
        println!(
            "  matcher gross income {income} sompi ({:.5} KAS), network fee floor {fee} sompi, net {:.5} KAS",
            income as f64 / 1e8,
            (income - fee as i64) as f64 / 1e8
        );
    }
    // the KRON limits themselves: 4 token inputs / 5 token outputs pass, 5 inputs / 6 outputs are rejected
    // by the token program (every token input runs the full check)
    run_ok(&f.net, &sweep(f, 4));
    run_ok(&f.net, &fan_out(f, 5));
    let s = with_name(sweep(f, 5), "NL1 5 token inputs (KRON allows 4)");
    for k in 5..10 {
        run_bad(&f.net, &s, k);
    }
    run_bad(&f.net, &with_name(fan_out(f, 6), "NL2 6 token outputs (KRON allows 5)"), 6);
}

#[test]
fn v2_negative_limits_and_tips() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_negative_limits_and_tips_body(&fx_with(tpl));
    }
}
fn v2_negative_limits_and_tips_body(f: &Fx) {
    let n = &f.net;
    let taker = pk(&f.taker);

    let mut s = with_name(s1(&f), "NF1 matcher pays the ask 1 sompi below its all-in limit");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);

    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tip = 0;
    let mut s = with_name(s1_with(&f, &p, 4_000), "NF2 zero-fee ask paid 1 sompi below its quote");
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
    run_bad(n, &with_name(s1_with(&f, &p, 4_000), "NF7 negative tip makes the order unfillable"), 0);

    let mut s = with_name(s1(&f), "NF8 malleated n=4,001 (outputs for 4,000)");
    set_arg(&mut s, 0, 0, nb(4_001));
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_negative_time_variants_body(&fx_with(tpl));
    }
}
fn v2_negative_time_variants_body(f: &Fx) {
    let n = &f.net;
    run_bad(n, &with_name(with_seq(s1_with(&f, &twap_ask(&f), 2_000), 0, 599), "NV1 TWAP ask filled 1 DAA too early (CSV)"), 0);
    run_bad(n, &with_name(with_seq(s1_with(&f, &twap_ask(&f), 2_001), 0, 600), "NV2 TWAP ask: 2,001 > maxFill 2,000"), 0);
    run_bad(
        n,
        &with_name(with_seq(s6_with(&f, &dca_bid(&f), 1_000, 10_000, P245, 0), 0, 599), "NV3 DCA bid filled 1 DAA too early (CSV)"),
        0,
    );
    run_bad(
        n,
        &with_name(with_seq(s6_with(&f, &dca_bid(&f), 1_001, 10_000, P245, 0), 0, 600), "NV4 DCA bid: 1,001 > maxFill 1,000"),
        0,
    );
    let d = dutch_ask(&f);
    let t = NOW as i64 + 1;
    run_bad(
        n,
        &with_name(
            s1_at(&f, &d, 4_000, decay_down(d.price, d.price_end, d.slope, 1_000, t), t),
            "NV5 Dutch ask: decay time above the tx lockTime (CLTV)",
        ),
        0,
    );
    let later = decay_down(d.price, d.price_end, d.slope, 1_000, 52_000);
    run_bad(n, &with_name(s1_at(&f, &d, 4_000, later, 51_000), "NV6 Dutch ask paid at a later (lower) price than the proven time"), 0);
    let mut up = dutch_ask(&f);
    up.slope = -1_000_000;
    run_bad(
        n,
        &with_name(s1_at(&f, &up, 4_000, 350_000_000, 51_000), "NV7 ask with a negative slope (would reward understated time)"),
        0,
    );
    let r = rising_bid(&f);
    let later = rise_up(r.price, r.price_end, r.slope, 1_000, 52_000);
    run_bad(
        n,
        &with_name(s6_with(&f, &r, 4_000, 4_000, later, 51_000), "NV8 rising bid charged a later (higher) price than the proven time"),
        0,
    );
    run_bad(
        n,
        &with_name(
            s6_with(&f, &r, 4_000, 4_000, rise_up(r.price, r.price_end, r.slope, 1_000, NOW as i64 + 1), NOW as i64 + 1),
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_negative_tif_cancel_expiry_body(&fx_with(tpl));
    }
}
fn v2_negative_tif_cancel_expiry_body(f: &Fx) {
    let n = &f.net;
    let taker = pk(&f.taker);

    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 2;
    run_bad(n, &with_name(s1_with(&f, &p, 4_000), "NT-FOK ask filled 4/10"), 0);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    run_bad(n, &with_name(s1_with(&f, &p, 4_000), "NT-IOC ask remainder kept resting"), 0);
    let mut s = with_name(s4(&f), "NT-IOC ask remainder sent to the taker");
    s.outputs[1].script_public_key = n.t.spk(6_000, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6_000, &taker, SCHEME_P2PK), tok_state(4_000, &taker, SCHEME_P2PK)]);
    run_bad(n, &s, 0);
    let mut bp = BidP::new(pk(&f.maker_b), P245);
    bp.tif = 2;
    run_bad(n, &with_name(s6_with(&f, &bp, 4_000, 10_000, P245, 0), "NT-FOK bid filled 4,000 of 10,000"), 0);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.active_from = NOW as i64 + 1;
    run_bad(n, &with_name(s1_with(&f, &p, 4_000), "NT-ACT fill one DAA before activeFrom"), 0);

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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_negative_trigger_manipulation_body(&fx_with(tpl));
    }
}
fn v2_negative_trigger_manipulation_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let oco = CondP::oco(m);
    let good = down_ev();
    let fill = |e: &Ev, name: &str| with_name(cond_fill(&f, &oco, 4_000, 1, &armed(&oco), Some(e)), name);

    let mut s = with_name(
        cond_fill(&f, &oco, 4_000, 1, &armed(&oco), None),
        "NT1 stop leg fill without evidence (ev = the taker's P2PK input)",
    );
    set_arg(&mut s, 0, 4, iv(2));
    set_arg(&mut s, 0, 5, iv(1));
    run_bad(n, &s, 0);

    run_bad(n, &fill(&good.with(|e| e.daa = NOW - 500), "NT2 wash print: the evidence ask was exposed only 500 DAA (< 600)"), 0);
    run_bad(n, &fill(&Ev::ask(200_000_001), "NT3 evidence quote above the stop"), 0);
    run_bad(n, &fill(&Ev::bid(198_000_000), "NT4 down trigger from a resting BID fill (not a seller's quote)"), 0);
    let mut o2 = oco.clone();
    o2.min_touch = 5_001;
    run_bad(n, &with_name(cond_fill(&f, &o2, 4_000, 1, &armed(&o2), Some(&good)), "NT5 evidence fill of 5,000 < required 5,001"), 0);
    run_bad(
        n,
        &fill(&good.with(|e| e.mode = EvMode::Forged), "NT6 look-alike evidence: a real ask's state under another template"),
        0,
    );
    run_bad(n, &fill(&good.with(|e| e.tok_daa = NOW - 599), "NT7 evidence ask's custody moved R - 1 DAA before (a fresh top-up)"), 0);
    run_bad(n, &fill(&good.with(|e| e.token = OTHER_COV), "NT8 evidence of another token"), 0);

    let mut s = with_name(cond_fill(&f, &armed(&oco), 4_000, 1, &armed(&oco), None), "NT9 armed stop leg paid 1 sompi below its band");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    run_bad(
        n,
        &with_name(cond_fill(&f, &oco, 4_000, 0, &armed(&oco), None), "NT10 TP-leg fill sneaks armed=1 into the continuation"),
        0,
    );
    let mut s = with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "NT11 TP leg paid 1 sompi below its all-in price");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    let mut wide = oco.clone();
    wide.slip_bps = 10_001;
    run_bad(n, &with_name(cond_fill(&f, &armed(&wide), 4_000, 1, &armed(&wide), None), "NT12 stop band above 100%"), 0);

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
        10_000,
        &c.as_bytes(),
        SCHEME_COVID,
        TOKEN_COV,
        Wit::CovId,
        2_000,
        vec![tok_state(10_000, &thief, SCHEME_P2PK)],
    );
    s.outputs.push(out(CARRIER, t.spk(10_000, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
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

#[test]
fn v2_negative_oco_and_ifd() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_negative_oco_and_ifd_body(&fx_with(tpl));
    }
}
fn v2_negative_oco_and_ifd_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let oco = CondP::oco(m);
    let taker = pk(&f.taker);
    let c = cov(0xc1);

    let mut s = with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "NO1 two UTXOs share the OCO covenant id (double fill attempt)");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(n, &s, 0);

    let mut s =
        with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "NO2 OCO fill under-reports the remainder (sells 5,000, pays 4,000)");
    s.outputs[2].script_public_key = n.t.spk(5_000, &c.as_bytes(), SCHEME_COVID);
    let last_tok = s.outputs.len() - 2;
    s.outputs[last_tok].script_public_key = n.t.spk(5_000, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(5_000, &c.as_bytes(), SCHEME_COVID), tok_state(5_000, &taker, SCHEME_P2PK)]);
    run_bad(n, &s, 0);

    let mut s = with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "NO3 OCO continuation drops the stop leg");
    let mut nostop = oco.clone();
    nostop.stop = 0;
    s.outputs[1].script_public_key = spk_of(&n.cond(&nostop));
    run_bad(n, &s, 0);

    let mut s = with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "NO4 OCO terminated while tokens remain");
    s.outputs[1].covenant = None;
    s.outputs[1].script_public_key = p2pk_spk(&taker);
    run_bad(n, &s, 0);

    let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
    let base = || IfdKnobs::new(10_000, 4_000);
    bad(IfdKnobs { deliver: 4_001, ..base() }, "NI1 over-delivery: 4,001 into the exit for a 4,000 fill");
    let mut e = ifd_exit(m);
    e.stop = 0;
    bad(IfdKnobs { exit: Some(e), ..base() }, "NI2 exit order tampered (stop leg removed)");
    let mut e = ifd_exit(m);
    e.slip_bps = 5_000;
    bad(IfdKnobs { exit: Some(e), ..base() }, "NI2b exit order tampered (stop band widened to 50%)");
    bad(IfdKnobs { to_maker: true, ..base() }, "NI3 tokens delivered to the maker instead of the exit custody");
    bad(IfdKnobs { ask_template: true, ..base() }, "NI4 exit built from another template (KobAsk)");
    bad(
        IfdKnobs { exit_delta: -1, ..IfdKnobs::new(6_000, 6_000) },
        "NI5 final fill charged 1 sompi above the all-in price (exit short)",
    );
    bad(IfdKnobs { cont_delta: -1, ..base() }, "NI5b partial fill: entry continuation short by 1 sompi");
    bad(IfdKnobs { exit_delta: -1, ..base() }, "NI5c partial fill: exit carrier short by 1 sompi");
    bad(IfdKnobs { double_exit: true, ..base() }, "NI6 double exit: a second exit output authorised by the entry");
    bad(IfdKnobs { self_id_exit: true, ..base() }, "NI7 exit reuses the entry's covenant id");
    bad(IfdKnobs { foreign_exit: true, ..base() }, "NI8 exit is an attacker covenant continuation (not a fresh genesis)");
    bad(IfdKnobs { cont_amount: Some(10_000), ..base() }, "NI9 entry continuation keeps amountLeft = 10,000 (not decremented)");
    bad(
        IfdKnobs { cont_amount: Some(5_999), ..base() },
        "NI9b entry continuation drops a base unit (amountLeft 5,999 instead of 6,000)",
    );
    bad(IfdKnobs::new(6_000, 6_001), "NI10 fill of 6,001 with only 6,000 left");
    let mut s = with_name(s14(&f), "NI12 exit index malleated to the taker output");
    let last = s.outputs.len() - 1;
    set_arg(&mut s, 0, 2, iv(last as i64));
    run_bad(n, &s, 0);
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
/// The stop entry of the IfdBid triggers: stop 2.20, limit 2.60, 10,000 base units.
fn stop_ifd(m: [u8; 32], min_touch: i64) -> IfdP {
    IfdP { entry_stop: 220_000_000, min_touch, ..IfdP::limit(m, 10_000, P260, ifd_exit(m)) }
}
/// The stop order of `kind` (input 0) triggered by the evidence `ev`; `min_touch`: its minTouch (base units).
fn trig(f: &Fx, kind: Kind, ev: &Ev, min_touch: i64) -> Scn {
    let m = pk(&f.maker_a);
    match kind {
        Kind::Settle => {
            let cp = CondP { min_touch, ..CondP::oco(m) };
            cond_fill(f, &cp, 4_000, 1, &armed(&cp), Some(ev))
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
        Kind::IfdFill => {
            ifd_fill(f, &IfdKnobs { ev: Some(ev.clone()), next_armed: Some(1), min_touch, ..stop_entry(IfdKnobs::new(10_000, 4_000)) })
        }
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
            let ip = stop_ifd(m, 1);
            let v = ifd_value(ip.amount, ip.rate(), ip.min_fill);
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
/// accepted, one DAA short rejected), slope, side, token, scale, size threshold (minTouch), an evidence that does not
/// fill, wrong ev / tk indices, look-alike templates, the price rule at its edge, one evidence shared by
/// two orders, and the update-only rules (own tokens, other fill kinds). Input 0 is the order throughout.
fn touch_battery(f: &Fx, kind: Kind) {
    let n = &f.net;
    let tag = kind.tag();
    let good = kind.ev();
    let matcher = pk(&f.matcher);
    let (pe, pt) = kind.ev_pos();
    let name = |neg: bool, id: &str, what: &str| format!("{}{tag}{id} {what}", if neg { "N" } else { "" });
    let ok = |e: &Ev, mt: i64, id: &str, what: &str| {
        run_ok(n, &with_name(trig(f, kind, e, mt), &name(false, id, what)));
    };
    let bad = |e: &Ev, mt: i64, id: &str, what: &str| run_bad(n, &with_name(trig(f, kind, e, mt), &name(true, id, what)), 0);
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
        let st = vec![tok_state(5_000, &matcher, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 5_000, &good.cov.as_bytes(), SCHEME_COVID, OTHER_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(5_000, &matcher, SCHEME_P2PK), Some((l as u16, OTHER_COV))));
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
        let st = vec![tok_state(5_000, &matcher, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 5_000, &good.cov.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(5_000, &matcher, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        set_arg(&mut s, 0, pt, iv(at));
        bad_s(s, "07b", "evidence of another token; tk names a UTXO of this token owned by that ask", 0);
    }
    bad(&good.with(|e| e.scale = 10 * SCALE), 1, "08", "evidence quoting per another scale (10^4 base units)");
    ok(&good, 5_000, "09", "evidence of exactly minTouch (5,000 base units, threshold 5,000)");
    bad(&good.with(|e| e.amount = 4_999), 5_000, "09", "evidence below minTouch (4,999 base units, threshold 5,000)");
    bad(&good.with(|e| e.mode = EvMode::Cancel), 1, "10", "cancelled, not filled (a signature push, ground to read as n > 0)");
    bad(&good.with(|e| e.mode = EvMode::Refund), 0, "11", "refunded, not filled (ask settle n = 0 / bid refund; minTouch 0)");
    if !good.ask {
        bad(&good.with(|e| e.mode = EvMode::ZeroFill), 0, "11b", "a bid fill with n = 0 (the bid refuses it too; minTouch 0)");
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
        let st = vec![tok_state(3_000, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3_000, &own.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 2_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(3_000, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        bad_s(s, "23", "update next to the evidence also spends a token UTXO owned by the order (to the attacker)", 0);
        let mut s = base.clone();
        let st = vec![tok_state(3_000, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3_000, &thief, SCHEME_P2PK, TOKEN_COV, Wit::P2pk(f.taker), 1_000, st);
        s.outputs.push(out(CARRIER, n.t.spk(3_000, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        // the taker's own P2PK input (KRON id_type 3 authorises by its presence)
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(out(KAS, p2pk_spk(&thief), None));
        run_ok(
            n,
            &with_name(s, &name(false, "23", "update next to the evidence and another foreign token input (the taker's own tokens)")),
        );
    }
    if kind == Kind::IfdArm {
        // update arms only an entry that has something to fill and its stop on the limit's side (else the arm only
        // takes keeperTip)
        let ip = stop_ifd(pk(&f.maker_a), 1);
        let v = ifd_value(ip.amount, ip.rate(), ip.min_fill);
        let empty = IfdP { amount: 0, rpt: 9, ..ip.clone() };
        let mut s = ifd_update(f, &empty, &IfdP { armed: 1, ..empty.clone() }, &good, 0);
        s.inputs[0].entry.amount = v as u64;
        s.outputs[0].value = v as u64;
        bad_s(s, "24", "update arms a repeating entry with nothing left (all of it is in its exits)", 0);
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
        (cond_fill(f, &CondP::oco(m), 4_000, 0, &CondP::oco(m), None), "a KobCondAsk take-profit fill (its custody as tk) as evidence")
    } else {
        (ifd_fill(f, &IfdKnobs::new(10_000, 4_000)), "a KobIfdBid limit-entry fill as evidence")
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
/// The unarmed stop (OCO, stop 2.00) sells 4,000 straight into a resting bid quoting 1.98 (its
/// counterparty, input 2) and names that bid as its evidence (ev 2, tk = its custody).
fn cond_into_bid(f: &Fx) -> Scn {
    let t = &f.net.t;
    let c = cov(0xc1);
    let cp = CondP::oco(pk(&f.maker_a));
    let bp = BidP::new(pk(&f.maker_b), 198_000_000);
    let b = cov(0xb1);
    let v = bp.used(4_000) + DC;
    let spend = bp.spend(4_000, bp.price);
    let pay = ask_all_in(4_000, stop_leg(cp.stop, cp.slip_bps), TIP);
    let next = vec![tok_state(4_000, &bp.maker, SCHEME_P2PK), tok_state(6_000, &c.as_bytes(), SCHEME_COVID)];
    Scn {
        name: "cond into bid".into(),
        inputs: vec![
            call(
                &f.net.cond(&cp),
                "settle",
                vec![nb(4_000), iv(1), iv(3), iv(1), iv(2), iv(1), iv(0)],
                "cond.settle",
                CARRIER,
                c,
                2_000,
            ),
            tok_in(t, CARRIER, 10_000, &c.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 2_000),
            call(&f.net.bid(&bp), "fill", vec![nb(4_000), iv(1), iv(0)], "bid.fill", v, b, 1_000),
        ],
        outputs: vec![
            out(pay, p2pk_spk(&cp.maker), None),
            out(CARRIER, spk_of(&f.net.cond(&armed(&CondP { amount: 6_000, ..cp.clone() }))), Some((0, c))),
            out(v - spend, t.spk(4_000, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(CARRIER, t.spk(6_000, &c.as_bytes(), SCHEME_COVID), Some((1, TOKEN_COV))),
            out(spend - pay - NET_FEE, p2pk_spk(&pk(&f.matcher)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    }
}
/// The unarmed buy-stop entry (stop 2.20, limit 2.60) buys 4,000 from a resting ask quoting 2.40 (its
/// counterparty: the ask's custody is the token input, the ask itself input 4) and names that ask as its
/// evidence.
fn ifd_from_ask(f: &Fx) -> Scn {
    let a = cov(0x9a);
    let ap = AskP { amount: 4_000, ..AskP::new(pk(&f.maker_c), 240_000_000) };
    let mut s = ifd_fill(f, &IfdKnobs { next_armed: Some(1), ..stop_entry(IfdKnobs::new(10_000, 4_000)) });
    // the entry's tokens come from the ask's custody instead of the taker
    let next = match &s.inputs[1].role {
        Role::Tok { next: Some(n), .. } => n.clone(),
        _ => panic!("the taker's token input carries the columns"),
    };
    s.inputs[1] = tok_in(&f.net.t, CARRIER, 4_000, &a.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000);
    let pay = ap.pay(4_000, ap.price);
    s.inputs.push(p2pk_in(&f.taker, pay + CARRIER));
    while s.inputs.len() < s.outputs.len() {
        s.inputs.push(p2pk_in(&f.taker, 0));
    }
    let at = s.inputs.len();
    s.inputs.push(call(&f.net.ask(&ap), "settle", vec![nb(4_000), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000));
    s.outputs.push(out(pay + 2 * CARRIER, p2pk_spk(&ap.maker), None));
    set_arg(&mut s, 0, 5, iv(at as i64));
    s
}

/// Runs `battery` on both pinned KRON templates.
fn both(kind: Kind) {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        touch_battery(&fx_with(tpl), kind);
    }
}
/// Touch trigger, KobCondAskKron stop leg armed inside its own fill (settle, leg 1, ask evidence).
#[test]
fn v2_touch_cond_settle() {
    both(Kind::Settle);
}
/// Touch trigger, KobCondAskKron stop leg armed by update next to an ask fill.
#[test]
fn v2_touch_cond_arm() {
    both(Kind::Arm);
}
/// Touch trigger, KobCondAskKron trailing ratchet (update next to a bid fill).
#[test]
fn v2_touch_cond_trail() {
    both(Kind::Trail);
}
/// Touch trigger, KobIfdBidKron buy-stop entry armed inside its own fill (bid evidence).
#[test]
fn v2_touch_ifd_fill() {
    both(Kind::IfdFill);
}
/// Touch trigger, KobIfdBidKron buy-stop entry armed by update next to a bid fill.
#[test]
fn v2_touch_ifd_arm() {
    both(Kind::IfdArm);
}

/// Merge-value safety of update: a repeat merge requires its partner's sigscript to start with 0x08 (the 8-byte push
/// of the fill argument) and reads bytes [1..9) as a fixed 8-byte script number. The real encoder never starts an
/// update sigscript with 0x08 (small index pushes). Without that first-byte check an entry's update could read as an
/// exit's merge value -(i * 2^53 + n): with i < 1024 every negative 8-byte number is one, and KobIfdBid.update(17)
/// reads as -3,048,096,669,922,821,137 (i = 338). An exit's update never reads as an entry's claim m (1 <= m < 2^53).
/// Checked over every small index and the push-size boundaries (NRP22 and NRP28 are the engine-level attacks; V3CA05
/// the non-minimal push).
#[test]
fn v2_update_sigscript_is_never_a_merge_value() {
    let f = fx_with(TPL_2433);
    let n = &f.net;
    let m = pk(&f.maker_a);
    let cond = n.cond(&CondP::oco(m));
    let ifd = n.ifd(&stop_ifd(m, 1));
    let head = |art: &SilAbiArtifact, args: &[ArtifactValue]| {
        let mut ss = encode_entry_sig_script(art, "update", args).expect("encode update");
        ss.extend_from_slice(&push(&bytecode(art))[..3]);
        ss
    };
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
    let idx: Vec<i64> = (-1..=260).chain([1_000, 32_767, 32_768, 65_535, 65_536, 8_388_607, 8_388_608]).collect();
    let mut checked = 0;
    let mut negative = 0;
    for &ev in idx.iter().filter(|&&i| i >= 0) {
        let ss = head(&ifd, &[int(ev)]);
        assert_ne!(ss[0], 0x08, "KobIfdBid.update({ev}) starts with an 8-byte push");
        if num(&ss) < 0 {
            negative += 1;
        }
        for &tk in &idx {
            let ss = head(&cond, &[int(ev), int(tk)]);
            assert_ne!(ss[0], 0x08, "KobCondAsk.update({ev}, {tk}) starts with an 8-byte push");
            let v = num(&ss);
            assert!(!(1..MERGE_K).contains(&v), "KobCondAsk.update({ev}, {tk}) reads as a merge claim {v}");
            checked += 1;
        }
    }
    assert_eq!(num(&head(&ifd, &[int(17)])), -814_311_254_747_055_121, "the bytes the 0x08 check keeps from being a merge value");
    println!(
        "MERGE-VALUE {checked} update sigscripts checked: none starts with 0x08 or reads as a merge claim; {negative} entry updates \
         would read as a merge value without the 0x08 check"
    );
}

// ---------------------------------------------------------------- KRON-specific attacks (v1 adapter suite A/B classes on the v2 orders)

/// Re-point every token input and token output of a scenario at another covenant id (`cov`) and/or
/// another token program (`kron`; the other pinned template). Outputs are re-encoded from the shared
/// next-state column set, in output order (the KRON program pairs covenant outputs with `next` in order).
fn retoken(s: &mut Scn, kron: Option<&Kron>, cov: Option<Hash>) {
    let next: Vec<KS> = s
        .inputs
        .iter()
        .find_map(|i| match &i.role {
            Role::Tok { next: Some(n), .. } => Some(n.clone()),
            _ => None,
        })
        .expect("scenario has token inputs");
    for inp in s.inputs.iter_mut() {
        if let Role::Tok { redeem, .. } = &mut inp.role {
            if let Some(k) = kron {
                *redeem = [redeem[..46].to_vec(), k.suffix.clone()].concat();
            }
            let c = cov.or(inp.entry.covenant_id);
            inp.entry = UtxoEntry::new(inp.entry.amount, pay_to_script_hash_script(redeem), inp.entry.block_daa_score, false, c);
        }
    }
    let mut j = 0;
    for o in s.outputs.iter_mut() {
        if o.covenant.as_ref().is_some_and(|b| b.covenant_id == TOKEN_COV) {
            if let Some(k) = kron {
                o.script_public_key = k.spk(&next[j]);
            }
            if let Some(c) = cov {
                o.covenant.as_mut().unwrap().covenant_id = c;
            }
            j += 1;
        }
    }
}
/// Replace token output `o` (and entry `ni` of the shared next-state columns carried by input `leader`).
fn set_tok_out(s: &mut Scn, t: &Token, o: usize, leader: usize, ni: usize, st: KS) {
    s.outputs[o].script_public_key = t.k.spk(&st);
    if let Role::Tok { next: Some(n), .. } = &mut s.inputs[leader].role {
        n[ni] = st;
    }
}
/// Make the token input `idx` a minter (is_minter = 1) without changing anything else.
fn make_minter(s: &mut Scn, idx: usize) {
    let e = s.inputs[idx].entry.clone();
    if let Role::Tok { redeem, .. } = &mut s.inputs[idx].role {
        redeem[45] = 1;
        s.inputs[idx].entry = UtxoEntry::new(e.amount, pay_to_script_hash_script(redeem), e.block_daa_score, false, e.covenant_id);
    }
}
/// Force the state (owner, id_type) of token input `idx` (custody attacks) and its witness index `w`.
fn set_tok_in(s: &mut Scn, idx: usize, owner: &[u8; 32], typ: u8, w: u8) {
    let e = s.inputs[idx].entry.clone();
    if let Role::Tok { redeem, wit, .. } = &mut s.inputs[idx].role {
        redeem[1..33].copy_from_slice(owner);
        redeem[34] = typ;
        *wit = Wit::Idx(w);
        s.inputs[idx].entry = UtxoEntry::new(e.amount, pay_to_script_hash_script(redeem), e.block_daa_score, false, e.covenant_id);
    }
}
fn set_wit(s: &mut Scn, idx: usize, w: u8) {
    if let Role::Tok { wit, .. } = &mut s.inputs[idx].role {
        *wit = Wit::Idx(w);
    }
}

#[test]
fn v2_kron_attacks_ask_side() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        let f = fx_with(tpl);
        let n = &f.net;
        let t = &n.t;
        let a = cov(0xa1);
        let taker = pk(&f.taker);
        let s1s = || s1(&f);
        // outputs of S1: [0] maker pay, [1] ask continuation, [2] token remainder (6,000, custody),
        // [3] taker tokens, [4] change; token input [1] carries the next columns [remainder, taker tokens]
        let run = |name: &str, expect: usize, mutate: &dyn Fn(&mut Scn)| {
            let mut s = s1s();
            mutate(&mut s);
            run_bad(n, &with_name(s, name), expect);
        };
        // A1 change owned by the taker
        run("KA1 token change owned by the taker", 0, &|s| set_tok_out(s, t, 2, 1, 0, tok_state(6_000, &taker, T_ADDR)));
        // A2 change with id_type 0 (pubkey) instead of covenant custody
        run("KA2 token change with id_type 0", 0, &|s| set_tok_out(s, t, 2, 1, 0, tok_state(6_000, &a.as_bytes(), T_PUBKEY)));
        run("KA2b token change with id_type 3 (owner = the order id)", 0, &|s| {
            set_tok_out(s, t, 2, 1, 0, tok_state(6_000, &a.as_bytes(), T_ADDR))
        });
        // A3 change with is_minter = 1
        run("KA3 token change with is_minter=1", 0, &|s| {
            let mut st = tok_state(6_000, &a.as_bytes(), T_COVID);
            st.minter = 1;
            set_tok_out(s, t, 2, 1, 0, st);
        });
        // A4 the order spends a token UTXO owned by the taker (id_type 3), not by its own covenant id
        run("KA4 token input owned by the taker (not the order)", 0, &|s| set_tok_in(s, 1, &taker, T_ADDR, 2));
        run("KA4b token input owned by the order id but with id_type 3", 0, &|s| set_tok_in(s, 1, &a.as_bytes(), T_ADDR, 0));
        run("KA4c token input that is a minter", 0, &|s| make_minter(s, 1));
        // A5 fake covenant id, same template
        run("KA5 fake token covenant id, same template", 0, &|s| retoken(s, None, Some(FAKE_COV)));
        // A6 right family, other pinned template (template-hash pin)
        run("KA6 other pinned KRON template (template-hash pin)", 0, &|s| retoken(s, Some(&f.other), None));
        // A7 token-change carrier skim by 1 sompi
        run("KA7 token change carrier skimmed by 1 sompi", 0, &|s| {
            s.outputs[2].value -= 1;
            s.outputs[4].value += 1;
        });
        // A10 malleated n
        run("KA10 malleated n=0 (refund path before expiry)", 0, &|s| set_arg(s, 0, 0, nb(0)));
        run("KA10b malleated n=-1", 0, &|s| set_arg(s, 0, 0, nb(-1)));
        run("KA10c n one base unit above the token balance (10,001 of 10,000)", 0, &|s| set_arg(s, 0, 0, nb(10_001)));
        // A12 duplicate covenant-id input (two UTXOs with the order's id)
        run("KA12 second UTXO with the order's covenant id", 0, &|s| {
            let dup = s.inputs[0].clone();
            s.inputs.push(dup);
        });
        // token program (real KRON bytes) guards
        run("KT1 minted +1 base unit: token program conservation", 1, &|s| {
            set_tok_out(s, t, 3, 1, 1, tok_state(4_000 + 1, &taker, T_ADDR));
        });
        run("KT2 witness points at a non-order input", 1, &|s| set_wit(s, 1, 2));
        run("KT3 witness points at the token input itself", 1, &|s| set_wit(s, 1, 1));
        // IOC ask: remainder returned with is_minter=1
        let mut p = AskP::new(pk(&f.maker_a), P250);
        p.tif = 1;
        let mut s = with_name(s4(&f), "KA-IOC remainder returned with is_minter=1");
        let mut st = tok_state(6_000, &p.maker, T_ADDR);
        st.minter = 1;
        set_tok_out(&mut s, t, 1, 1, 0, st);
        run_bad(n, &s, 0);
        // refund: tokens to a foreign owner / id_type 0 / id_type 2 / minter
        let m = pk(&f.maker_a);
        let refund = |st: KS, name: &str| {
            let mut s = fresh(s8(&f, EXPIRY, EXPIRY as u64));
            set_tok_out(&mut s, t, 0, 1, 0, st);
            run_bad(n, &with_name(s, name), 0);
        };
        refund(tok_state(10_000, &taker, T_ADDR), "KA13 refund tokens delivered to the taker");
        refund(tok_state(10_000, &m, T_PUBKEY), "KA13b refund tokens delivered with id_type 0");
        refund(tok_state(10_000, &m, T_COVID), "KA13c refund tokens delivered with id_type 2");
        let mut st = tok_state(10_000, &m, T_ADDR);
        st.minter = 1;
        refund(st, "KA13d refund tokens delivered with is_minter=1");
        let mut s = fresh(s8(&f, EXPIRY, EXPIRY as u64));
        retoken(&mut s, Some(&f.other), None);
        run_bad(n, &with_name(s, "KA14 refund with the other pinned template"), 0);
    }
}

#[test]
fn v2_kron_attacks_bid_side() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        let f = fx_with(tpl);
        let n = &f.net;
        let t = &n.t;
        let taker = pk(&f.taker);
        let maker = pk(&f.maker_b);
        // S6 (tif 0, 4,000 of 10,000): outputs [0] delivery to the maker (4,000, id_type 3), [1] continuation,
        // [2] taker KAS; token input [1] (the taker's 4,000, id_type 3, authorised by P2PK input [2])
        let base = || s6_with(&f, &BidP::new(maker, P245), 4_000, 10_000, P245, 0);
        run_ok(n, &base());
        let run = |name: &str, mutate: &dyn Fn(&mut Scn)| {
            let mut s = base();
            mutate(&mut s);
            run_bad(n, &with_name(s, name), 0);
        };
        run("KB1 delivery owned by the taker", &|s| set_tok_out(s, t, 0, 1, 0, tok_state(4_000, &taker, T_ADDR)));
        run("KB2 delivery with id_type 0", &|s| set_tok_out(s, t, 0, 1, 0, tok_state(4_000, &maker, T_PUBKEY)));
        run("KB2b delivery with id_type 2 (owner = a pubkey)", &|s| set_tok_out(s, t, 0, 1, 0, tok_state(4_000, &maker, T_COVID)));
        run("KB3 delivery with is_minter=1", &|s| {
            let mut st = tok_state(4_000, &maker, T_ADDR);
            st.minter = 1;
            set_tok_out(s, t, 0, 1, 0, st);
        });
        run("KB4 under-delivery: 3,999 of the 4,000 bought", &|s| {
            // the taker only puts 3,999 in; the bid is filled for 4,000
            s.inputs[1] = tok_in(
                t,
                CARRIER,
                3_999,
                &taker,
                T_ADDR,
                TOKEN_COV,
                Some(vec![tok_state(3_999, &maker, T_ADDR)]),
                Wit::P2pk(f.taker),
                1_000,
            );
            s.outputs[0].script_public_key = t.spk(3_999, &maker, T_ADDR);
        });
        run("KB5 foreign token template (other pinned program)", &|s| retoken(s, Some(&f.other), None));
        run("KB6 fake token covenant id, same template", &|s| retoken(s, None, Some(FAKE_COV)));
        run("KB7 second UTXO with the bid's covenant id", &|s| {
            let dup = s.inputs[0].clone();
            s.inputs.push(dup);
        });
        run("KB8 template source input is not a token (P2PK input as tokenTplIn)", &|s| set_arg(s, 0, 1, iv(2)));
        // token program: the taker's tokens must be authorised by its P2PK input
        let mut s = base();
        set_wit(&mut s, 1, 0);
        run_bad(n, &with_name(s, "KT4 taker tokens authorised by a non-P2PK input"), 1);
        // minted +1 in the delivery
        let mut s = base();
        set_tok_out(&mut s, t, 0, 1, 0, tok_state(4_000 + 1, &maker, T_ADDR));
        run_bad(n, &with_name(s, "KT5 minted +1 base unit in the delivery"), 1);
    }
}

#[test]
fn v2_kron_attacks_conditional_and_ifd() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        let f = fx_with(tpl);
        let n = &f.net;
        let t = &n.t;
        let taker = pk(&f.taker);
        let m = pk(&f.maker_a);
        let c = cov(0xc1);
        let oco = CondP::oco(m);
        // cond_fill: [0] maker pay, [1] continuation, [2] token remainder (custody), [3] taker tokens, [4] change
        let base = || cond_fill(&f, &oco, 4_000, 0, &oco, None);
        run_ok(n, &base());
        let run = |name: &str, mutate: &dyn Fn(&mut Scn)| {
            let mut s = base();
            mutate(&mut s);
            run_bad(n, &with_name(s, name), 0);
        };
        run("KC1 conditional sell: token change owned by the taker", &|s| {
            set_tok_out(s, t, 2, 1, 0, tok_state(6_000, &taker, T_ADDR))
        });
        run("KC2 conditional sell: token change with id_type 0", &|s| {
            set_tok_out(s, t, 2, 1, 0, tok_state(6_000, &c.as_bytes(), T_PUBKEY))
        });
        run("KC3 conditional sell: token change with is_minter=1", &|s| {
            let mut st = tok_state(6_000, &c.as_bytes(), T_COVID);
            st.minter = 1;
            set_tok_out(s, t, 2, 1, 0, st);
        });
        run("KC4 conditional sell: token input that is a minter", &|s| make_minter(s, 1));
        run("KC5 conditional sell: token input owned by the taker", &|s| set_tok_in(s, 1, &taker, T_ADDR, 2));
        run("KC6 conditional sell: other pinned template", &|s| retoken(s, Some(&f.other), None));
        run("KC7 conditional sell: fake covenant id", &|s| retoken(s, None, Some(FAKE_COV)));
        // if-done entry: the exit's custody state
        run_ok(n, &ifd_fill(&f, &IfdKnobs::new(10_000, 4_000)));
        let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
        bad(
            IfdKnobs { deliver_type: Some(T_ADDR), ..IfdKnobs::new(10_000, 4_000) },
            "KI1 exit custody with id_type 3 (owner = exit id)",
        );
        bad(IfdKnobs { deliver_type: Some(T_PUBKEY), ..IfdKnobs::new(10_000, 4_000) }, "KI2 exit custody with id_type 0");
        bad(IfdKnobs { deliver_minter: 1, ..IfdKnobs::new(10_000, 4_000) }, "KI3 exit custody with is_minter=1");
        let mut s = with_name(ifd_fill(&f, &IfdKnobs::new(10_000, 4_000)), "KI4 if-done: other pinned template");
        retoken(&mut s, Some(&f.other), None);
        run_bad(n, &s, 0);
        let mut s = with_name(ifd_fill(&f, &IfdKnobs::new(10_000, 4_000)), "KI5 if-done: fake token covenant id");
        retoken(&mut s, None, Some(FAKE_COV));
        run_bad(n, &s, 0);
    }
}

#[test]
fn v2_kron_attacks_touch() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        let f = fx_with(tpl);
        let n = &f.net;
        let t = &n.t;
        let taker = pk(&f.taker);
        let oco = CondP::oco(pk(&f.maker_a));
        // a stop armed by update (input 0) next to an ask fill: evidence ask at input 2, its custody at 3
        let base = cond_update(&f, &oco, &armed(&oco), &down_ev(), 0);
        // KT0: an unrelated taker token input (id_type 3, the taker's P2PK input present) rides along
        let mut s = base.clone();
        s.inputs.push(p2pk_in(&f.taker, KAS));
        let ti = s.inputs.len();
        let st = vec![tok_state(1_000, &taker, T_ADDR)];
        let l = push_tok(&f, &mut s, CARRIER, 1_000, &taker, T_ADDR, TOKEN_COV, Wit::P2pk(f.taker), 1_000, st);
        s.outputs.push(out(CARRIER, t.spk(1_000, &taker, T_ADDR), Some((l as u16, TOKEN_COV))));
        s.outputs.push(out(KAS, p2pk_spk(&taker), None));
        run_ok(n, &with_name(s.clone(), "KT0 control: an unrelated taker token input next to the evidence fill is fine"));
        set_arg(&mut s, 0, 1, iv(ti as i64));
        run_bad(n, &with_name(s, "NKT1 tk names a token input owned by the taker (id_type 3), not by the evidence ask"), 0);
        // NKT2: tk names a UTXO of another KRON token covenant owned by the evidence ask (id_type 2)
        let mut s = base.clone();
        let ti = s.inputs.len();
        let st = vec![tok_state(1_000, &taker, T_ADDR)];
        let l = push_tok(&f, &mut s, CARRIER, 1_000, &cov(0x9a).as_bytes(), T_COVID, OTHER_COV, Wit::CovId, 1_000, st);
        s.outputs.push(out(CARRIER, t.spk(1_000, &taker, T_ADDR), Some((l as u16, OTHER_COV))));
        set_arg(&mut s, 0, 1, iv(ti as i64));
        run_bad(n, &with_name(s, "NKT2 tk names a UTXO of another token covenant owned by the evidence ask"), 0);
        // NKT3: an evidence ask whose custody is a minter refuses its own fill (the tx is invalid)
        let mut s = base.clone();
        make_minter(&mut s, 3);
        run_bad(n, &with_name(s, "NKT3 evidence ask with a minter custody: the ask refuses its own fill"), 2);
    }
}

#[test]
fn v2_kron_vs_kcc20_fees() {
    // KCC-20 reference (3/3, 3,090 B program) figures from kob_v2_tests.rs v2_positive_limit_orders at
    // the time of writing (protocol v2.6): (scenario, tx_size B, min_fee sompi).
    let kcc = [("S1", 5_235u64, 1_047_000u64), ("S2", 10_789, 2_157_800), ("S3", 6_214, 1_242_800)];
    for tpl in TEMPLATES {
        let f = fx_with(tpl);
        println!("=== KRON template {tpl}: fee floor vs the KCC-20 reference");
        for (i, s) in [s1(&f), s2(&f), s3(&f)].iter().enumerate() {
            let fee = run_ok(&f.net, s);
            let (name, size, kfee) = kcc[i];
            println!(
                "COMPARE {name}: KRON fee {fee} sompi vs KCC-20 {kfee} sompi (KCC-20 tx {size} B) => {:+.1}%",
                (fee as f64 / kfee as f64 - 1.0) * 100.0
            );
        }
    }
}

// ---------------------------------------------------------------- v2.3 lifecycle

/// Market orders as IOC auctions, TWAP slice auctions, and the IOC/FOK kill (KobAsk / KobBid).
#[test]
fn v2_lifecycle_auctions_and_kill() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_lifecycle_auctions_and_kill_body(&fx_with(tpl));
    }
}
fn v2_lifecycle_auctions_and_kill_body(f: &Fx) {
    let n = &f.net;
    let t = NOW as i64;

    // Market sell: IOC auction from the touch. At t = activeFrom + 100 the quote is 2.4625, and
    // that is what the maker must receive (not the 2.425 bound).
    let m = market_ask(&f);
    let eff = decay_at(m.price, m.price_end, m.slope, 1, m.active_from, t);
    assert_eq!(eff, 246_250_000);
    run_ok(
        n,
        &with_name(ioc_fill(&f, &m, 4_000, eff, t), "M1 market sell = IOC auction: 4/10 filled 100 DAA in at 2.4625, rest returned"),
    );
    let mut over = with_name(ioc_fill(&f, &m, 4_000, m.price_end, t + 500), "M1b market sell auction over: the 3% bound 2.425");
    over.lock_time = NOW + 500;
    run_ok(n, &over);
    run_bad(
        n,
        &with_name(ioc_fill(&f, &m, 4_000, m.price_end, t), "NM1 market sell paid its bound while the auction is at 2.4625"),
        0,
    );
    let mut z = m.clone();
    z.decay_step = 0;
    run_bad(n, &with_name(ioc_fill(&f, &z, 4_000, m.price_end, t), "NM2 decaying order with decayStep 0 is unfillable"), 0);
    let late = decay_at(m.price, m.price_end, m.slope, 1, m.active_from, t + 1);
    run_bad(n, &with_name(ioc_fill(&f, &m, 4_000, late, t + 1), "NM3 auction time above the tx lockTime (CLTV)"), 0);

    // Market buy: IOC rising auction; the delivery carries the unspent budget.
    let b = market_bid(&f);
    let eff = rise_at(b.price, b.price_end, b.slope, 1, b.active_from, t);
    assert_eq!(eff, 253_750_000);
    run_ok(n, &with_name(s6_with(&f, &b, 4_000, 10_000, eff, t), "M2 market buy = IOC rising auction: 4,000 100 DAA in at 2.5375"));
    run_bad(
        n,
        &with_name(s6_with(&f, &b, 4_000, 10_000, b.price_end, t), "NM4 market buy charged its bound while the auction is at 2.5375"),
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
            with_seq(s1_at(&f, &tw, 2_000, eff, 1_750), 0, 600),
            "T6 TWAP slice auction: slice opens at 1600, 2,000 at t=1750 (2.55)",
        ),
    );
    let from_start = decay_at(tw.price, tw.price_end, tw.slope, 1, 0, 1_750);
    run_bad(
        n,
        &with_name(
            with_seq(s1_at(&f, &tw, 2_000, from_start, 1_750), 0, 600),
            "NV10 TWAP slice priced from activeFrom instead of the slice opening",
        ),
        0,
    );
    run_bad(
        n,
        &with_name(with_seq(s1_at(&f, &tw, 2_000, tw.price, 1_599), 0, 600), "NV11 TWAP slice auction time before the slice opens"),
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_lifecycle_stop_auction_keeper_trailing_body(&fx_with(tpl));
    }
}
fn v2_lifecycle_stop_auction_keeper_trailing_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let a = auction_oco(m);
    let r = down_ev();
    let stop = a.stop;
    let fill = |cp: &CondP, leg: i64, next: &CondP, ev: Option<&Ev>, t: i64, price: i64, name: &str| {
        with_name(cond_fill_at(&f, cp, 4_000, leg, next, ev, t, price), name)
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
    run_ok(n, &with_name(cond_fill(&f, &a1, 4_000, 0, &a2000, None), "S17e TP-leg fill of an armed auction order carries the origin"));
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_lifecycle_ifd_stop_entry_body(&fx_with(tpl));
    }
}
fn v2_lifecycle_ifd_stop_entry_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let up = up_ev(); // a resting bid quoting 2.20 is filled in the same transaction
    let armed_in = |k: IfdKnobs| IfdKnobs { ev: Some(up.clone()), next_armed: Some(1), ..k };
    let auction = |k: IfdKnobs| stop_entry(IfdKnobs { band_daa: 1_000, ..k });

    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &armed_in(stop_entry(IfdKnobs::new(10_000, 4_000)))),
            "S18 buy-stop IFO entry (stop 2.20, limit 2.60) armed inside the fill: 4/10 at the limit, continues armed",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(
                &f,
                &IfdKnobs {
                    armed: 1,
                    t: 1_500,
                    eff: Some(240_000_000),
                    next_armed: Some(1_000),
                    ..auction(IfdKnobs::new(10_000, 4_000))
                },
            ),
            "S18b armed stop entry auction 500/1000 DAA: buys at 2.40, remainder keeps origin 1000",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { eff: Some(220_000_000), ..armed_in(auction(IfdKnobs::new(10_000, 4_000))) }),
            "S18c arm + fill in one tx (auction starts at the trigger): pays the stop 2.20",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { armed: 1_000, t: 1_500, eff: Some(240_000_000), ..auction(IfdKnobs::new(6_000, 6_000)) }),
            "S18d final fill of an auction entry (stored origin)",
        ),
    );
    let mut ip = IfdP::limit(m, 10_000, P260, ifd_exit(m));
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
            ifd_fill(&f, &IfdKnobs { min_fill: 3_000, ..IfdKnobs::new(10_000, 3_000) }),
            "S18f minFill 3,000: a 3,000 partial fill",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { min_fill: 3_000, ..IfdKnobs::new(2_000, 2_000) }),
            "S18g minFill 3,000: the final 2,000 may fill",
        ),
    );

    let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
    bad(IfdKnobs { next_armed: Some(1), ..stop_entry(IfdKnobs::new(10_000, 4_000)) }, "NI13 stop entry filled without evidence");
    bad(
        armed_in(auction(IfdKnobs::new(10_000, 4_000))),
        "NI16b arm + fill in one tx (auction) charged the limit 2.60 instead of the stop",
    );
    bad(
        IfdKnobs { ev: Some(side1_of()), next_armed: Some(1), ..stop_entry(IfdKnobs::new(10_000, 4_000)) },
        "NI14 buy-stop entry armed by a resting-ASK fill (wrong side)",
    );
    let low = up.with(|e| e.price = 219_999_999);
    bad(
        IfdKnobs { ev: Some(low), next_armed: Some(1), ..stop_entry(IfdKnobs::new(10_000, 4_000)) },
        "NI15 evidence bid quoting below the entry stop",
    );
    let young = up.with(|e| e.daa = NOW - 599);
    bad(
        IfdKnobs { ev: Some(young), next_armed: Some(1), ..stop_entry(IfdKnobs::new(10_000, 4_000)) },
        "NI15b evidence bid exposed R - 1 DAA (a fresh print)",
    );
    bad(
        IfdKnobs { armed: 1, t: 1_500, next_armed: Some(1_000), ..auction(IfdKnobs::new(10_000, 4_000)) },
        "NI16 auction entry charged its limit 2.60 at 500/1000 DAA",
    );
    bad(
        IfdKnobs { armed: 1, t: 1_500, eff: Some(240_000_000), next_armed: Some(1), ..auction(IfdKnobs::new(10_000, 4_000)) },
        "NI17 continuation restarts the entry auction (armed = 1)",
    );
    bad(
        IfdKnobs { next_armed: Some(0), ..armed_in(stop_entry(IfdKnobs::new(10_000, 4_000))) },
        "NI17b continuation drops the armed state",
    );
    bad(IfdKnobs { min_fill: 3_000, ..IfdKnobs::new(10_000, 2_000) }, "NI18 partial fill below minFill");
    let high = up.with(|e| e.price = 275_000_000);
    bad(
        IfdKnobs { entry_stop: 270_000_000, ev: Some(high), next_armed: Some(1), ..IfdKnobs::new(10_000, 4_000) },
        "NI19 stop entry with its stop above its limit is unfillable",
    );
    let mut lim = IfdP::limit(m, 10_000, P260, ifd_exit(m));
    lim.keeper_tip = 1_000_000;
    let mut lim_armed = lim.clone();
    lim_armed.armed = 1;
    run_bad(n, &with_name(ifd_update(&f, &lim, &lim_armed, &up, 0), "NI20 arm update on a limit entry"), 0);
    run_bad(n, &with_name(ifd_update(&f, &ip, &ip_armed, &up, 1_000_001), "NI21 arming keeper takes keeperTip + 1"), 0);
    run_bad(n, &with_name(ifd_update(&f, &ip, &ip_armed, &side1_of(), 0), "NI22 arm update from a wrong-side (ask) fill"), 0);
    let mut ip_amount = ip_armed.clone();
    ip_amount.amount = 9_000;
    run_bad(n, &with_name(ifd_update(&f, &ip, &ip_amount, &up, 0), "NI23 arm update also changes amountLeft"), 0);
}

/// A token UTXO of `amount` base units owned by covenant id `owner` (id_type 2), carrying the group's next-state
/// columns when `next` is given.
fn owned_tok(f: &Fx, amount: i64, owner: Hash, next: Option<Vec<KS>>) -> Inp {
    tok_in(&f.net.t, CARRIER, amount, &owner.as_bytes(), SCHEME_COVID, TOKEN_COV, next, Wit::CovId, 1_500)
}

/// Stray custody UTXOs (tokens sent to an order's covenant id outside the protocol): they can
/// neither stand in for the custody nor be co-spent by anyone but the maker.
#[test]
fn v2_lifecycle_stray_custody() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_lifecycle_stray_custody_body(&fx_with(tpl));
    }
}
fn v2_lifecycle_stray_custody_body(f: &Fx) {
    let n = &f.net;
    let a = cov(0xa1);
    let m = pk(&f.maker_a);
    let taker = pk(&f.taker);
    let tk = &n.t;
    let ask = n.ask(&AskP::new(m, P250));

    // NS1: 1,000 base units of dust sent to the ask's id stand in as tokenIn; the real 10,000 custody is
    // co-spent (authorised through the ask input) and routed to the filler.
    let steal = |with_custody: bool, name: &str| {
        let got = if with_custody { 11_000 } else { 1_000 };
        let mut inputs = vec![
            call(&ask, "settle", vec![nb(1_000), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000),
            owned_tok(&f, 1_000, a, Some(vec![tok_state(got, &taker, SCHEME_P2PK)])),
        ];
        if with_custody {
            inputs.push(owned_tok(&f, 10_000, a, None));
        }
        inputs.push(p2pk_in(&f.taker, 1000 * KAS));
        let pay = ask_all_in(1_000, P250, TIP);
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
    run_bad(n, &steal(true, "NS1 1,000 dust stands in for the custody; the 10,000 custody is routed to the filler"), 0);
    run_bad(n, &steal(false, "NS2 1,000 dust sold out to terminate the ask and orphan its 10,000 custody"), 0);

    // NS3: the real custody as tokenIn, plus a 3,000 stray co-spent to the filler.
    let mut s = with_name(s1(&f), "NS3 fill co-spends a 3,000 stray owned by the ask (to the filler)");
    s.inputs.insert(2, owned_tok(&f, 3_000, a, None));
    s.outputs[3].script_public_key = tk.spk(7_000, &taker, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(6_000, &a.as_bytes(), SCHEME_COVID), tok_state(7_000, &taker, SCHEME_P2PK)]);
    let l = s.outputs.len() - 1;
    s.outputs[l].value -= CARRIER as u64;
    s.outputs.push(out(2 * CARRIER, p2pk_spk(&taker), None));
    run_bad(n, &s, 0);

    // NS4: a refund keeper (anyone) co-spends a stray.
    let keeper = keypair();
    let mut s = with_name(s8(&f, NO_EXPIRY, (1_000 + MAX_IDLE) as u64), "NS4 refund keeper co-spends a 3,000 stray (to itself)");
    s.inputs.push(owned_tok(&f, 3_000, a, None));
    set_leader_next(&mut s, 1, vec![tok_state(10_000, &m, SCHEME_P2PK), tok_state(3_000, &pk(&keeper), SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(3_000, &pk(&keeper), SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);

    // S19: only the maker's cancel (SIGHASH_ALL: the maker routes every output) moves strays.
    let mut s = with_name(s7(&f, 0x01, f.maker_a), "S19 maker cancel sweeps the custody and a 3,000 stray back to the maker");
    s.inputs.push(owned_tok(&f, 3_000, a, None));
    s.outputs[0].script_public_key = tk.spk(13_000, &m, SCHEME_P2PK);
    set_leader_next(&mut s, 1, vec![tok_state(13_000, &m, SCHEME_P2PK)]);
    s.outputs[1].value += CARRIER as u64;
    run_ok(n, &s);

    // NS5 / NS6: strays owned by a bid's id.
    let b = cov(0xb1);
    let mut s = with_name(s6(&f, 1), "NS5 bid fill co-spends a 3,000 stray owned by the bid (to the taker)");
    s.inputs.push(owned_tok(&f, 3_000, b, None));
    let mb = pk(&f.maker_b);
    set_leader_next(&mut s, 1, vec![tok_state(4_000, &mb, SCHEME_P2PK), tok_state(3_000, &taker, SCHEME_P2PK)]);
    s.outputs.push(out(CARRIER, tk.spk(3_000, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);
    let mut s =
        with_name(s9(&f, "refund", NO_EXPIRY, (1_000 + MAX_IDLE) as u64), "NS6 bid refund co-spends a 3,000 stray owned by the bid");
    s.inputs.push(owned_tok(&f, 3_000, b, Some(vec![tok_state(3_000, &pk(&keeper), SCHEME_P2PK)])));
    s.outputs.push(out(CARRIER, tk.spk(3_000, &pk(&keeper), SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);

    // NS7: conditional sell / IFD exit (KobCondAsk, cov 0xc1): stray co-spent on a TP fill, and
    // dust standing in for the custody.
    let c = cov(0xc1);
    let oco = CondP::oco(m);
    let mut s = with_name(cond_fill(&f, &oco, 4_000, 0, &oco, None), "NS7 conditional (IFD exit) TP fill co-spends a 2,000 stray");
    s.inputs.push(owned_tok(&f, 2_000, c, None));
    set_leader_next(&mut s, 1, vec![tok_state(6_000, &c.as_bytes(), SCHEME_COVID), tok_state(6_000, &taker, SCHEME_P2PK)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = tk.spk(6_000, &taker, SCHEME_P2PK);
    s.outputs.push(out(CARRIER, p2pk_spk(&taker), None));
    run_bad(n, &s, 0);
    let cond = n.cond(&oco);
    let pay = ask_all_in(1_000, oco.tp, TIP);
    let s = Scn {
        name: "NS7b 1,000 dust sold out on the TP leg to terminate a 10,000 conditional (orphaning its custody)".into(),
        inputs: vec![
            call(&cond, "settle", vec![nb(1_000), iv(1), iv(0), iv(0), iv(0), iv(0), iv(0)], "cond.settle", CARRIER, c, 2_000),
            owned_tok(&f, 1_000, c, Some(vec![tok_state(1_000, &taker, SCHEME_P2PK)])),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(pay + 2 * CARRIER, p2pk_spk(&m), None),
            out(CARRIER, tk.spk(1_000, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))),
            out(1000 * KAS - pay - 2 * CARRIER - NET_FEE, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    run_bad(n, &s, 0);

    // NS8: if-done entry (cov 0xd1) fill co-spends a stray owned by the entry.
    let mut s = with_name(s14(&f), "NS8 if-done entry fill co-spends a 3,000 stray owned by the entry");
    s.inputs.push(owned_tok(&f, 3_000, cov(0xd1), None));
    let exit_owner = s.outputs[0].script_public_key.clone();
    let exit_id = s
        .outputs
        .iter()
        .find_map(|o| o.covenant.as_ref().filter(|b| b.covenant_id != TOKEN_COV && b.covenant_id != cov(0xd1)).map(|b| b.covenant_id))
        .unwrap();
    set_leader_next(&mut s, 1, vec![tok_state(4_000, &exit_id.as_bytes(), SCHEME_COVID), tok_state(3_000, &taker, SCHEME_P2PK)]);
    assert_eq!(exit_owner, tk.spk(4_000, &exit_id.as_bytes(), SCHEME_COVID));
    s.outputs.push(out(CARRIER, tk.spk(3_000, &taker, SCHEME_P2PK), Some((1, TOKEN_COV))));
    run_bad(n, &s, 0);
}

// ---------------------------------------------------------------- v2.4 repeat IFD, buy-first

/// Covenant id of the repeating if-done bid in the fixtures (as in ifd_fill).
const RPT_ENTRY: u8 = 0xd1;
/// The budget rate a re-armed amount needs again (the entry's all-in price at its limit 2.60, sompi per whole token):
/// the booked exit's rptPrice.
fn rpt_rate() -> i64 {
    P260 + TIP
}
/// The exit a fill of n base units books from the entry at UTXO DAA 1000 with t = 0.
fn booked_exit(maker: [u8; 32], n: i64) -> CondP {
    CondP {
        amount: n,
        parent: cov(RPT_ENTRY).as_bytes(),
        rpt_price: rpt_rate(),
        rpt_until: EXPIRY.min(1_000 + MAX_IDLE),
        ..ifd_exit(maker)
    }
}
/// A repeating limit entry (quote 2.60) with `amount` left and rptAmount `rpt`.
fn rpt_entry(maker: [u8; 32], amount: i64, rpt: i64) -> IfdP {
    IfdP { rpt, ..IfdP::limit(maker, amount, P260, ifd_exit(maker)) }
}
/// Merge argument -(k * 2^53 + m) as an 8-byte script number (sign-magnitude, little endian).
fn merge_arg(k: i64, m: i64) -> [u8; 8] {
    let mut b = (k * MERGE_K + m).to_le_bytes();
    b[7] |= 0x80;
    b
}
fn nb_merge(k: i64, m: i64) -> Arg {
    Arg::V(bytes(&merge_arg(k, m)))
}
/// Escrow of a repeating entry with `amount` left (its fills' budgets and carriers plus its own carrier).
fn rpt_value(amount: i64) -> i64 {
    ifd_value(amount, rpt_rate(), MIN_FILL) + EC
}
/// Sets output `idx` to whatever balances the transaction after NET_FEE (the taker's change).
fn balance(s: &mut Scn, idx: usize) {
    let ins: i64 = s.inputs.iter().map(|i| i.entry.amount as i64).sum();
    let outs: i64 = s.outputs.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, o)| o.value as i64).sum();
    s.outputs[idx].value = (ins - outs - NET_FEE) as u64;
}

/// Knobs of a take-profit fill of a booked exit that re-arms its entry (single-lever mutations).
#[derive(Clone)]
struct MergeKnobs {
    /// entry parameters before the merge (amountLeft, armed, rptAmount) and its UTXO value
    entry: IfdP,
    entry_value: i64,
    /// the exit (default: booked_exit of `exit_amount`), its amountLeft, the amount it sells, its leg
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
        MergeKnobs {
            entry: rpt_entry(maker, entry_amount, 21_000),
            entry_value: rpt_value(entry_amount),
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
    let xp = k.exit.clone().unwrap_or_else(|| booked_exit(m, k.exit_amount));
    let xp = CondP { amount: k.exit_amount, ..xp };
    let leg_price = if k.leg == 0 { xp.tp } else { stop_leg(xp.stop, xp.slip_bps) };
    let all_in = q_up(k.n, leg_price - xp.tip);
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
        // armed: reset when the entry had nothing left; an entry armed by update (1) with a band records its origin, the
        // entry UTXO's DAA (1_000), as a fill does
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
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}

/// A crafted P2SH input that imitates a booked exit of `parent` (not a KobCondAsk): a redeem
/// script of the KobCondAsk length whose would-be state holds `parent` at the parent offset,
/// executing as "drop, drop, true"; its first sigscript push is 0x08 || m.
fn fake_exit(f: &Fx, parent: Hash, m: i64, value: i64) -> Inp {
    let size = f.net.cond_tpl.pre.len() + 330 + f.net.cond_tpl.suf.len();
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
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_repeat_ifd_positive_body(&fx_with(tpl));
    }
}
fn v2_repeat_ifd_positive_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);

    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 21_000, ..IfdKnobs::new(10_000, 4_000) }),
            "RP1 repeating IFO entry fills 4/10: the exit is booked (parent = entry, rptPrice, rptUntil), rptAmount 21,000 -> 17,000",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 17_000, ..IfdKnobs::new(6_000, 6_000) }),
            "RP2 repeating entry sold out 6/6: it stays (amountLeft 0, rptAmount 11,000) and waits for its exits",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 1, ..IfdKnobs::new(10_000, 4_000) }),
            "RP3 last cycle: the re-arms are used up (rptAmount 1), the exit is a plain exit (parent 0)",
        ),
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(&f, &IfdKnobs { rpt: 21_000, t: 5_000, ..IfdKnobs::new(10_000, 4_000) }),
            "RP3b the fill's time argument t does not date the cycle: rptUntil = entry UTXO DAA + 90 days",
        ),
    );
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs::new(m, 6_000, 4_000, 3_000)),
            "RP4 booked exit sells 3/4 on its take-profit: the entry re-arms 3,000 (6,000 -> 9,000) with their budget, the maker keeps the profit",
        ),
    );
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs::new(m, 0, 4_000, 4_000)),
            "RP5 booked exit sells out 4/4: the empty entry re-arms 4,000, both exit carriers come back",
        ),
    );
    let mut stop_entry_p = rpt_entry(m, 0, 21_000);
    stop_entry_p.entry_stop = 220_000_000;
    stop_entry_p.armed = 1;
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { entry: stop_entry_p.clone(), ..MergeKnobs::new(m, 0, 4_000, 4_000) }),
            "RP5b an armed stop entry with nothing left re-arms unarmed (a new cycle waits for a new trigger)",
        ),
    );
    let mut stop_part = rpt_entry(m, 3_000, 21_000);
    stop_part.entry_stop = 220_000_000;
    stop_part.armed = 1_000;
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { entry: stop_part, entry_value: rpt_value(3_000), ..MergeKnobs::new(m, 3_000, 4_000, 2_000) }),
            "RP5c re-armed amount joins a running stop-entry auction (amount left: armed origin kept)",
        ),
    );
    let until = EXPIRY.min(1_000 + MAX_IDLE);
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { with_entry: false, lock_time: until as u64, ..MergeKnobs::new(m, 0, 4_000, 4_000) }),
            "RP6 from rptUntil on a booked exit may take profit without its entry (entry expired or idle)",
        ),
    );
    let mut sl = booked_exit(m, 4_000);
    sl.armed = 1;
    run_ok(
        n,
        &with_name(
            rpt_merge(&f, &MergeKnobs { exit: Some(sl), leg: 1, with_entry: false, ..MergeKnobs::new(m, 0, 4_000, 4_000) }),
            "RP7 stop-loss of a booked exit: plain, proceeds and carriers to the maker, no re-arm",
        ),
    );
    // A re-armed stop entry is armed again by a keeper (update keeps rptAmount).
    let mut ip = rpt_entry(m, 10_000, 21_000);
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

    // Three cycles of a 4,000 position (rptAmount 9,000 = 2 re-arm cycles), each fill and take-profit at
    // the limits, values carried from one transaction to the next: the entry is restored exactly
    // after every cycle and the maker earns exactly the spread per cycle.
    let v0 = rpt_value(4_000);
    let profit_token = ask_all_in(SCALE, ifd_exit(m).tp, TIP) - q_up(SCALE, rpt_rate());
    let mut v = v0;
    let mut rpt = 9_000;
    let mut maker_total = 0;
    for cycle in 1..=3 {
        let booked = rpt > 4_000;
        let mut fk = IfdKnobs { rpt, ..IfdKnobs::new(4_000, 4_000) };
        if !booked {
            fk.exit_rpt = Some(([0; 32], 0, 0));
        }
        let mut s = ifd_fill(&f, &fk);
        s.inputs[0].entry = UtxoEntry::new(v as u64, s.inputs[0].entry.script_public_key.clone(), 1_000, false, Some(cov(RPT_ENTRY)));
        let cont = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPT_ENTRY))).unwrap();
        s.outputs[cont].value = (v - bid_all_in(4_000, P260, TIP) - DC - EC) as u64;
        let name = format!("RP10 ledger cycle {cycle}: entry buys 4/4 (value {v}, rptAmount {rpt}, booked {booked})");
        run_ok(n, &with_name(s.clone(), &name));
        v = s.outputs[cont].value as i64;
        assert_eq!(v, v0 - q_up(4_000, rpt_rate()) - DC - EC, "entry after its fill");
        if booked {
            rpt -= 4_000;
            let mut mk = MergeKnobs::new(m, 0, 4_000, 4_000);
            mk.entry = rpt_entry(m, 0, rpt);
            mk.entry_value = v;
            let s = rpt_merge(&f, &mk);
            let name = format!("RP10 ledger cycle {cycle}: take-profit 4/4 re-arms the entry");
            run_ok(n, &with_name(s.clone(), &name));
            let back = s.outputs.iter().find(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPT_ENTRY))).unwrap();
            v = back.value as i64;
            assert_eq!(v, v0, "entry value restored exactly after cycle {cycle}");
            assert_eq!(s.outputs[0].value as i64, 4 * profit_token, "maker profit of cycle {cycle}");
            maker_total += s.outputs[0].value as i64;
        } else {
            let mut mk = MergeKnobs::new(m, 0, 4_000, 4_000);
            mk.exit = Some(CondP { amount: 4_000, ..ifd_exit(m) });
            mk.with_entry = false;
            let s = rpt_merge(&f, &mk);
            run_ok(n, &with_name(s.clone(), &format!("RP10 ledger cycle {cycle}: plain take-profit, no re-arm left")));
            maker_total += s.outputs[0].value as i64;
        }
    }
    assert_eq!(v, v0 - q_up(4_000, rpt_rate()) - DC - EC, "after the last cycle the entry keeps its unused carriers");
    assert_eq!(
        maker_total,
        2 * 4 * profit_token + ask_all_in(4_000, ifd_exit(m).tp, TIP) + 2 * CARRIER,
        "maker receipts over 3 cycles"
    );
    println!("LEDGER v0={v0} profit/token={profit_token} maker_total={maker_total}");
}

/// The exit's arming update (cov 0xc1, booked by entry 0xd1) next to an entry merge that names it as the exit at
/// input 0 selling `claim`: the exit arms next to a plain ask fill (touch). `wide`: the update's ev is pushed as a
/// non-minimal 8-byte push and the merge claims m = ev (bytes [1..9) of the exit's sigscript); else ev is a minimal
/// push and the merge claims 1.
fn exit_update_beside_merge(f: &Fx, wide: bool) -> Scn {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);
    let x = booked_exit(m, 4_000);
    let v6 = rpt_value(6_000);
    let mut s = Scn {
        name: "exit update beside merge".into(),
        inputs: vec![
            call(&n.cond(&x), "update", vec![iv(0), iv(0)], "cond.update", CARRIER, cov(0xc1), 2_000),
            call(
                &n.ifd(&rpt_entry(m, 6_000, 21_000)),
                "fill",
                vec![nb_merge(0, 1), iv(0), iv(0), Arg::V(bytes(&[])), Arg::V(bytes(&[])), iv(0), iv(0)],
                "ifd.merge",
                v6,
                e,
                1_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(CARRIER, spk_of(&n.cond(&CondP { armed: 1, ..x.clone() })), Some((0, cov(0xc1)))),
            out(0, p2pk_spk(&pk(&f.taker)), None),
            out(0, p2pk_spk(&pk(&f.taker)), None),
        ],
        lock_time: NOW,
        payload: vec![],
    };
    // the exit arms next to a plain ask fill (touch); its update sigscript is [ev, tk, tag, redeem]
    let (ei, ti) = add_ev(&f, &mut s, &down_ev());
    let claim = if wide { ei } else { 1 };
    set_arg(&mut s, 0, 0, if wide { Arg::Wide(ei) } else { iv(ei) });
    set_arg(&mut s, 0, 1, iv(ti));
    set_arg(&mut s, 1, 0, nb_merge(0, claim));
    set_arg(&mut s, 1, 1, iv(ti));
    s.outputs[1] = out(v6 + q_up(claim, rpt_rate()), spk_of(&n.ifd(&rpt_entry(m, 6_000 + claim, 21_000))), Some((1, e)));
    balance(&mut s, 2);
    s
}

/// Single-lever attacks on repeat IFD, buy-first. Each is rejected by exactly one check; the
/// ablation runs (KOB_ABLATION=1 with that check removed) accept the same transaction.
#[test]
fn v2_repeat_ifd_attacks() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_repeat_ifd_attacks_body(&fx_with(tpl));
    }
}
fn v2_repeat_ifd_attacks_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);
    let taker = pk(&f.taker);
    let tk = &n.t;

    // ---- booking (entry fill)
    let bad = |k: IfdKnobs, name: &str| run_bad(n, &with_name(ifd_fill(&f, &k), name), 0);
    let base = || IfdKnobs { rpt: 21_000, ..IfdKnobs::new(10_000, 4_000) };
    bad(IfdKnobs { exit_rpt: Some(([0; 32], 0, 0)), ..base() }, "NRP1 booked exit written as a plain exit (re-arm skipped)");
    bad(
        IfdKnobs {
            exit_rpt: Some((e.as_bytes(), rpt_rate(), EXPIRY.min(1_000 + MAX_IDLE))),
            rpt: 4_000,
            next_rpt: Some(0),
            ..IfdKnobs::new(10_000, 4_000)
        },
        "NRP2 exit booked although the re-arms are exhausted (rptAmount 4,000, fill 4,000; continuation rptAmount 0)",
    );
    bad(IfdKnobs { next_rpt: Some(21_000), ..base() }, "NRP3 continuation keeps rptAmount (cycle count not decremented)");
    bad(IfdKnobs { next_rpt: Some(0), ..base() }, "NRP3b continuation drops the repeat (rptAmount 0)");
    bad(
        IfdKnobs { exit_rpt: Some((e.as_bytes(), rpt_rate() - 1, EXPIRY.min(1_000 + MAX_IDLE))), ..base() },
        "NRP4 exit's re-arm rate rptPrice lowered by 1 sompi (skims every cycle)",
    );
    bad(
        IfdKnobs { exit_rpt: Some((e.as_bytes(), rpt_rate(), EXPIRY.min(1_000 + MAX_IDLE) - 1)), ..base() },
        "NRP5 exit's rptUntil shortened (re-arm skippable early)",
    );
    bad(
        IfdKnobs { t: 5_000, exit_rpt: Some((e.as_bytes(), rpt_rate(), EXPIRY.min(1_000 + MAX_IDLE) + 4_000)), ..base() },
        "NRP6 exit's rptUntil dated by the filler's t instead of the entry UTXO's DAA",
    );
    bad(
        IfdKnobs { rpt: 4_000, ..IfdKnobs::new(10_000, 4_000) },
        "NRP6b a fill of 4,000 with 3,999 re-arms left (at least a minimum fill): a plain exit would leave them unused",
    );
    bad(
        IfdKnobs { rpt: 17_000, terminate: true, ..IfdKnobs::new(6_000, 6_000) },
        "NRP7 repeating entry terminated at its final fill (everything to the exit)",
    );
    bad(
        IfdKnobs { rpt: 17_000, cont_delta: -1, ..IfdKnobs::new(6_000, 6_000) },
        "NRP8 repeating final fill: entry continuation short by 1 sompi",
    );

    // ---- take-profit / merge (exit side)
    let mb = |k: MergeKnobs, name: &str, at: usize| run_bad(n, &with_name(rpt_merge(&f, &k), name), at);
    mb(
        MergeKnobs { with_entry: false, ..MergeKnobs::new(m, 0, 4_000, 4_000) },
        "NRP10 booked exit takes profit without re-arming its entry (before rptUntil)",
        0,
    );
    mb(
        MergeKnobs { maker_delta: -1, ..MergeKnobs::new(m, 6_000, 4_000, 3_000) },
        "NRP11 maker's profit short by 1 sompi on a re-arming take-profit",
        0,
    );
    mb(
        MergeKnobs { cont_delta: -1, ..MergeKnobs::new(m, 0, 4_000, 4_000) },
        "NRP12 sell-out: the exit's carriers skimmed by 1 sompi (entry gets the budget only)",
        0,
    );
    let mut sl = booked_exit(m, 4_000);
    sl.armed = 1;
    mb(
        MergeKnobs { exit: Some(sl), leg: 1, ..MergeKnobs::new(m, 0, 4_000, 4_000) },
        "NRP13 stop-loss fill re-arms its entry (attacker-funded re-arm of a stopped-out position)",
        0,
    );
    // Two booked exits of the same entry in one transaction; the entry names only the first, the
    // second pays its maker the profit only and its budget goes to the filler.
    let mut s = rpt_merge(&f, &MergeKnobs::new(m, 0, 4_000, 4_000));
    let c2 = cov(0xc2);
    let x2 = booked_exit(m, 4_000);
    let last = s.outputs.len() - 1;
    s.outputs.truncate(last); // re-balanced below
    let b_in = s.inputs.len();
    s.inputs.push(call(
        &n.cond(&x2),
        "settle",
        vec![nb(4_000), iv(b_in as i64 + 1), iv(0), iv(0), iv(0), iv(0), iv(0)],
        "cond.settle",
        CARRIER,
        c2,
        2_000,
    ));
    s.inputs.push(tok_in(tk, CARRIER, 4_000, &c2.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 2_000));
    // one token transfer: exit A's custody carries the columns, both custodies go to the taker (8,000)
    set_leader_next(&mut s, 1, vec![tok_state(8_000, &taker, SCHEME_P2PK)]);
    s.outputs[2].script_public_key = tk.spk(8_000, &taker, SCHEME_P2PK);
    while s.outputs.len() < b_in {
        s.outputs.push(out(0, p2pk_spk(&taker), None));
    }
    s.outputs.push(out(ask_all_in(4_000, x2.tp, TIP) - q_up(4_000, rpt_rate()), p2pk_spk(&m), None));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRP14 double re-arm: a second booked exit rides on the first one's merge (its budget skimmed)"), b_in);

    // ---- merge (entry side)
    mb(
        MergeKnobs { cont_delta: -1, ..MergeKnobs::new(m, 6_000, 4_000, 3_000) },
        "NRP15 partial take-profit: entry re-armed 1 sompi short of the 3,000 budget",
        2,
    );
    let ip = rpt_entry(m, 6_000, 21_000);
    mb(
        MergeKnobs { cont: Some(IfdP { amount: 8_000, ..ip.clone() }), ..MergeKnobs::new(m, 6_000, 4_000, 3_000) },
        "NRP16 entry re-arms less than it got the budget for (6,000 + 2,000 instead of 6,000 + 3,000)",
        2,
    );
    mb(
        MergeKnobs { cont: Some(IfdP { amount: 9_000, price: P255, ..ip.clone() }), ..MergeKnobs::new(m, 6_000, 4_000, 3_000) },
        "NRP17 re-arm with altered parameters (limit 2.60 -> 2.55)",
        2,
    );
    mb(
        MergeKnobs { cont: Some(IfdP { amount: 9_000, rpt: 99_000, ..ip.clone() }), ..MergeKnobs::new(m, 6_000, 4_000, 3_000) },
        "NRP17b re-arm refills the cycle count (rptAmount 21,000 -> 99,000)",
        2,
    );
    mb(
        MergeKnobs {
            entry: stop_entry_armed(m),
            cont: Some(IfdP { amount: 4_000, ..stop_entry_armed(m) }),
            ..MergeKnobs::new(m, 0, 4_000, 4_000)
        },
        "NRP18 an empty stop entry re-arms still armed (skips the new trigger)",
        2,
    );
    mb(
        MergeKnobs { claim_m: Some(4_000), ..MergeKnobs::new(m, 6_000, 4_000, 3_000) },
        "NRP19 entry claims 4,000 re-armed for a 3,000 take-profit (attacker funds the extra budget)",
        0,
    );
    // A foreign exit: a plain KobCondAsk (parent 0) takes profit normally; the entry claims it.
    let s = rpt_merge(&f, &MergeKnobs { exit: Some(CondP { amount: 4_000, ..ifd_exit(m) }), ..MergeKnobs::new(m, 0, 4_000, 4_000) });
    run_bad(n, &with_name(s, "NRP20 entry re-arms from a plain exit of another order (not its own, attacker-funded)"), 2);
    // A crafted non-KobCondAsk input imitating a booked exit of this entry.
    let mut s = rpt_merge(&f, &MergeKnobs::new(m, 6_000, 4_000, 3_000));
    s.inputs[0] = fake_exit(&f, e, 3_000, CARRIER);
    s.inputs.remove(1);
    s.outputs.truncate(1);
    s.outputs[0].value = 0;
    s.outputs[0].script_public_key = p2pk_spk(&taker);
    let ip6 = rpt_entry(m, 6_000, 21_000);
    set_arg(&mut s, 1, 0, nb_merge(0, 3_000));
    s.outputs.push(out(0, spk_of(&n.ifd(&IfdP { amount: 9_000, ..ip6 })), Some((1, e))));
    let v6 = rpt_value(6_000);
    s.outputs[1].value = (v6 + q_up(3_000, rpt_rate())) as u64;
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &with_name(s, "NRP21 entry re-arms from a crafted input that imitates its booked exit (not a KobCondAsk)"), 1);
    // Beside the exit's arming update (nothing sold) the entry claims 1 base unit re-armed (the exit refuses an update
    // next to its entry; the update's minimal pushes also fail the entry's 0x08 check and its m).
    run_bad(
        n,
        &with_name(
            exit_update_beside_merge(&f, false),
            "NRP22 entry re-arms beside its exit's update (nothing sold; attacker-funded)",
        ),
        1,
    );
    // The other direction: a booked exit takes profit on 3,000 and names as its merge an entry (a repeating
    // stop entry) that is only armed by update next to a bid fill: the budget would stay with the filler.
    let x = booked_exit(m, 4_000);
    let stop_e = IfdP { entry_stop: 220_000_000, ..rpt_entry(m, 6_000, 21_000) };
    let mut s = rpt_merge(&f, &MergeKnobs { entry: stop_e.clone(), ..MergeKnobs::new(m, 6_000, 4_000, 3_000) });
    s.inputs[2] = call(&n.ifd(&stop_e), "update", vec![iv(0)], "ifd.update", v6, e, 1_000);
    let ci = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == e)).unwrap();
    s.outputs[ci] = out(v6, spk_of(&n.ifd(&IfdP { armed: 1, ..stop_e.clone() })), Some((2, e)));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    let (ei, _) = add_ev(&f, &mut s, &Ev::bid(225_000_000));
    set_arg(&mut s, 2, 0, iv(ei));
    run_bad(n, &with_name(s, "NRP28 re-arming take-profit names its entry's arming update as the merge (budget skimmed)"), 0);
    // A zero "merge" beside the exit's refund re-dates the entry (idle clock, trigger freshness).
    run_bad(n, &with_name(merge_beside_refund(&f, 0, false), "NRP23 zero merge beside an exit refund (re-dates the entry)"), 0);
    // A stray owned by the entry, co-spent in a merge.
    let mut s = with_name(
        rpt_merge(&f, &MergeKnobs::new(m, 6_000, 4_000, 3_000)),
        "NRP24 merge co-spends a 2,000 stray owned by the entry (to the taker)",
    );
    s.inputs.push(owned_tok(&f, 2_000, e, None));
    set_leader_next(&mut s, 1, vec![tok_state(1_000, &cov(0xc1).as_bytes(), SCHEME_COVID), tok_state(5_000, &taker, SCHEME_P2PK)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = tk.spk(5_000, &taker, SCHEME_P2PK);
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(n, &s, 2);
    // ---- the merge requires the exit to be exactly one this entry books (terms, parent, rptPrice)
    let atk = pk(&f.taker);
    let mut s = rpt_merge(&f, &MergeKnobs { exit: Some(booked_exit(atk, 4_000)), ..MergeKnobs::new(m, 0, 4_000, 4_000) });
    s.outputs[0].script_public_key = p2pk_spk(&atk);
    run_bad(n, &with_name(s, "NRP25 look-alike exit of another maker (parent = this entry) re-arms the entry"), 2);
    let x2 = CondP { rpt_price: rpt_rate() - 1, ..x.clone() };
    let s = rpt_merge(&f, &MergeKnobs { exit: Some(x2), ..MergeKnobs::new(m, 0, 4_000, 4_000) });
    run_bad(n, &with_name(s, "NRP26 look-alike exit with another rptPrice (rate - 1) re-arms the entry"), 2);
    let x3 = CondP { tp: x.tp + 1, ..x.clone() };
    let s = rpt_merge(&f, &MergeKnobs { exit: Some(x3), ..MergeKnobs::new(m, 0, 4_000, 4_000) });
    run_bad(n, &with_name(s, "NRP27 look-alike exit with other terms (take-profit + 1) re-arms the entry"), 2);
    // ---- a merge into a stop entry armed by update and not filled yet keeps its band origin (the armed UTXO's DAA)
    let ae = IfdP { entry_stop: 220_000_000, band_daa: 300, armed: 1, ..rpt_entry(m, 6_000, 21_000) };
    let k = MergeKnobs { entry: ae.clone(), ..MergeKnobs::new(m, 6_000, 4_000, 3_000) };
    run_ok(
        n,
        &with_name(rpt_merge(&f, &k), "RP11 merge into an armed, unfilled stop entry: the continuation records its band origin"),
    );
    let s = rpt_merge(&f, &MergeKnobs { cont: Some(IfdP { amount: 9_000, ..ae.clone() }), ..k.clone() });
    run_bad(n, &with_name(s, "NRP29 merge into an armed, unfilled stop entry keeps armed = 1 (its band auction restarts)"), 2);
}
/// An armed repeating stop entry (stop 2.20, limit 2.60) with nothing left.
fn stop_entry_armed(maker: [u8; 32]) -> IfdP {
    let mut p = rpt_entry(maker, 0, 21_000);
    p.entry_stop = 220_000_000;
    p.armed = 1;
    p
}
/// The entry (cov 0xd1, 6,000 left, input 0) merges `claim` naming its booked exit at input 1, which is refunded at
/// its expiry (n = 0, sold nothing; its custody at input 2 goes back to the maker). `pd1`: the exit's n is pushed with
/// OP_PUSHDATA1, so its sigscript starts 0x4c 0x08 and bytes [1..9) read 0x08 || 0 = 8. A taker input funds the claim.
fn merge_beside_refund(f: &Fx, claim: i64, pd1: bool) -> Scn {
    let n = &f.net;
    let tk = &n.t;
    let m = pk(&f.maker_a);
    let e = cov(RPT_ENTRY);
    let x = booked_exit(m, 4_000);
    let v6 = rpt_value(6_000);
    let mut s = Scn {
        name: "merge beside refund".into(),
        inputs: vec![
            call(
                &n.ifd(&rpt_entry(m, 6_000, 21_000)),
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
                vec![if pd1 { Arg::Pd1(0i64.to_le_bytes().to_vec()) } else { nb(0) }, iv(2), iv(0), iv(0), iv(0), iv(0), iv(0)],
                "cond.refund",
                CARRIER,
                cov(0xc1),
                (EXPIRY - 1_000) as u64,
            ),
            tok_in(
                tk,
                CARRIER,
                4_000,
                &cov(0xc1).as_bytes(),
                SCHEME_COVID,
                TOKEN_COV,
                Some(vec![tok_state(4_000, &m, SCHEME_P2PK)]),
                Wit::CovId,
                2_000,
            ),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(v6 + q_up(claim, rpt_rate()), spk_of(&n.ifd(&rpt_entry(m, 6_000 + claim, 21_000))), Some((0, e))),
            out(2 * CARRIER - REFUND_TIP, tk.spk(4_000, &m, SCHEME_P2PK), Some((2, TOKEN_COV))),
            out(0, p2pk_spk(&pk(&f.taker)), None),
        ],
        lock_time: EXPIRY as u64,
        payload: vec![],
    };
    balance(&mut s, 2);
    s
}

// ---------------------------------------------------------------- KRON extras (v2.3 witnesses added in the port)

/// Single-lever witnesses added while porting to the KRON 4-token-input limit: the stray guard's last
/// slot, the continuation's amountLeft, decayStep / slice origin of the bid, and the token-input refusals
/// of the if-done entry's update / refund (the KCC-20 suite covers these only through the shared paths).
#[test]
fn v2_kron_v23_extra_attacks() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_kron_v23_extra_attacks_body(&fx_with(tpl));
    }
}
fn v2_kron_v23_extra_attacks_body(f: &Fx) {
    let n = &f.net;
    let a = cov(0xa1);
    let m = pk(&f.maker_a);
    let taker = pk(&f.taker);
    let tk = &n.t;

    // KS1: the custody plus two taker-held token inputs plus a 3,000 stray owned by the ask fill the 4 token
    // slots KRON allows; the stray sits in the LAST slot. Without the stray the same shape is valid.
    let four_inputs = |with_stray: bool, name: &str| {
        let mut s = with_name(s1(f), name);
        for _ in 0..2 {
            s.inputs.insert(2, tok_in(tk, CARRIER, 1_000, &taker, SCHEME_P2PK, TOKEN_COV, None, Wit::P2pk(f.taker), 1_500));
        }
        let mut got = 6_000;
        if with_stray {
            s.inputs.insert(4, owned_tok(f, 3_000, a, None));
            got = 9_000;
        }
        set_leader_next(&mut s, 1, vec![tok_state(6_000, &a.as_bytes(), SCHEME_COVID), tok_state(got, &taker, SCHEME_P2PK)]);
        s.outputs[3].script_public_key = tk.spk(got, &taker, SCHEME_P2PK);
        let ins: i64 = s.inputs.iter().map(|i| i.entry.amount as i64).sum();
        let outs: i64 = s.outputs[..s.outputs.len() - 1].iter().map(|o| o.value as i64).sum();
        let last = s.outputs.len() - 1;
        s.outputs[last].value = (ins - outs - NET_FEE) as u64;
        s
    };
    run_ok(n, &four_inputs(false, "KS1a ask fill with 3 token inputs (custody + 2 taker-held) is valid"));
    run_bad(n, &four_inputs(true, "KS1 stray owned by the ask in the 4th (last) token slot"), 0);

    // KS2: the ask's continuation must carry amountLeft - n (splice window), not the old amountLeft.
    let mut s = with_name(s1(f), "KS2 ask continuation keeps amountLeft 10,000 instead of 6,000");
    s.outputs[1].script_public_key = spk_of(&n.ask(&AskP::new(m, P250)));
    run_bad(n, &s, 0);

    // KS10: a TWAP slice auction time before the slice opens, priced consistently (start price + one step: the
    // taker overpays), is refused by t >= origin alone.
    let mut tw = twap_ask(f);
    tw.price = P260;
    tw.price_end = P250;
    tw.slope = (P260 - P250) / 300;
    tw.decay_step = 1;
    let eff = decay_at(tw.price, tw.price_end, tw.slope, 1, 1_600, 1_599);
    assert_eq!(eff, tw.price + tw.slope);
    run_bad(
        n,
        &with_name(
            with_seq(s1_at(f, &tw, 2_000, eff, 1_599), 0, 600),
            "KS10 TWAP slice auction time before the slice opens (priced by the formula)",
        ),
        0,
    );

    // KS9: a decaying ask needs decayStep > 0 (a negative step would reprice upwards).
    let mut z = market_ask(f);
    z.decay_step = -1;
    let t = NOW as i64;
    let eff = decay_at(z.price, z.price_end, z.slope, -1, z.active_from, t);
    run_bad(n, &with_name(ioc_fill(f, &z, 4_000, eff, t), "KS9 decaying ask with a negative decayStep"), 0);

    // KS5: the same for a rising bid.
    let mut rb = rising_bid(f);
    rb.decay_step = -1;
    let eff = rise_at(rb.price, rb.price_end, rb.slope, -1, 1_000, 1_100);
    assert!(eff > 0);
    run_bad(n, &with_name(s6_with(f, &rb, 4_000, 10_000, eff, 1_100), "KS5 rising bid with a negative decayStep"), 0);

    // KS6: a DCA rising bid's slice is its own auction opening at UTXO DAA + interval (1000 + 600).
    let mut db = rising_bid(f);
    db.slope = 100_000;
    db.decay_step = 1;
    db.interval = 600;
    let good = rise_at(db.price, db.price_end, db.slope, 1, 1_600, 1_750);
    assert_eq!(good, 215_000_000);
    run_ok(
        n,
        &with_name(
            with_seq(s6_with(f, &db, 4_000, 10_000, good, 1_750), 0, 600),
            "KS6a DCA rising bid: slice opens at 1600, 4,000 at t=1750 (2.15)",
        ),
    );
    let early = rise_at(db.price, db.price_end, db.slope, 1, 1_000, 1_599);
    run_bad(
        n,
        &with_name(
            with_seq(s6_with(f, &db, 4_000, 10_000, early, 1_599), 0, 600),
            "KS6 DCA rising bid auction time before the slice opens",
        ),
        0,
    );
    let from_start = rise_at(db.price, db.price_end, db.slope, 1, 1_000, 1_750);
    run_bad(
        n,
        &with_name(
            with_seq(s6_with(f, &db, 4_000, 10_000, from_start, 1_750), 0, 600),
            "KS6b DCA rising bid priced from activeFrom, not the slice",
        ),
        0,
    );

    // KS3 / KS4 / KS8: the if-done entry's update and refund spend no token input; update's keeperTip >= 0.
    let mut ip = IfdP::limit(m, 10_000, P260, ifd_exit(m));
    ip.entry_stop = 220_000_000;
    ip.keeper_tip = 1_000_000;
    let ip_armed = IfdP { armed: 1, ..ip.clone() };
    let up = up_ev();
    let keeper = pk(&keypair());
    let mut s = with_name(ifd_update(f, &ip, &ip_armed, &up, 0), "KS3 if-done update co-spends a 2,000 stray owned by the entry");
    let st = vec![tok_state(2_000, &keeper, SCHEME_P2PK)];
    let l = push_tok(f, &mut s, CARRIER, 2_000, &cov(0xd1).as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_500, st);
    s.outputs.push(out(CARRIER, tk.spk(2_000, &keeper, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
    run_bad(n, &s, 0);
    let e = rpt_entry(m, 0, 1);
    let mut s = Scn {
        name: "KS4 if-done refund co-spends a 2,000 stray owned by the entry".into(),
        inputs: vec![
            call(&n.ifd(&e), "refund", vec![], "ifd.refund", EC, cov(RPT_ENTRY), (EXPIRY - 1_000) as u64),
            owned_tok(f, 2_000, cov(RPT_ENTRY), Some(vec![tok_state(2_000, &keeper, SCHEME_P2PK)])),
        ],
        outputs: vec![
            out(EC - REFUND_TIP, p2pk_spk(&m), None),
            out(CARRIER, tk.spk(2_000, &keeper, SCHEME_P2PK), Some((1, TOKEN_COV))),
        ],
        lock_time: EXPIRY as u64,
        payload: vec![],
    };
    s.inputs[0].seq = 0;
    run_bad(n, &s, 0);
    let mut neg = ip.clone();
    neg.keeper_tip = -1;
    let neg_armed = IfdP { armed: 1, ..neg.clone() };
    run_bad(n, &with_name(ifd_update(f, &neg, &neg_armed, &up, -1), "KS8 negative keeperTip on an if-done arm update"), 0);
}

// ================================================================ regressions (sell side)

/// Two IOC asks (10,000 each) of ONE maker, n sold from each, remainder r = 10,000 - n.
/// `shared`: both point tokOut at ONE output (the attack: the maker gets r back instead of 2r);
/// else each returns at its own custody index (tokOut == tokenIn = 2 and 3).
fn fx_ioc_pair(f: &Fx, n: i64, shared: bool) -> Scn {
    let t = &f.net.t;
    let (a1, a2) = (cov(0xa1), cov(0xa2));
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    let ask = f.net.ask(&p);
    let taker = pk(&f.taker);
    let r = 10_000 - n;
    let pay = p.pay(n, P250);
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
            "FXE1 two IOC asks of one maker share ONE remainder output (maker gets 9,000 back instead of 18,000)".into()
        } else {
            "FXE1+ two IOC asks of one maker, each remainder at its own custody index".into()
        },
        inputs: vec![
            call(&ask, "settle", vec![nb(n), iv(2), iv(tok_out[0]), iv(0)], "ask1.settle", CARRIER, a1, 1_000),
            call(&ask, "settle", vec![nb(n), iv(3), iv(tok_out[1]), iv(0)], "ask2.settle", CARRIER, a2, 1_000),
            tok_in(t, CARRIER, 10_000, &a1.as_bytes(), SCHEME_COVID, TOKEN_COV, Some(next), Wit::CovId, 1_000),
            tok_in(t, CARRIER, 10_000, &a2.as_bytes(), SCHEME_COVID, TOKEN_COV, None, Wit::CovId, 1_000),
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

/// An IOC ask and a bid of the SAME maker: the ask returns r = 6,000 at output 1, which is
/// also the bid's positional delivery of 6,000 (output 1 = the bid's input), so the bid "buys" the
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
    let (n_ask, r) = (4_000, 6_000);
    let v = bp.used(10_000) + 3 * DC;
    let pay = ap.pay(n_ask, P250);
    let cont = v - bp.used(r) - DC;
    let mut s = Scn {
        name: "FXE2 IOC ask + bid of one maker: the ask's return (tokOut 1) is also the bid's delivery".into(),
        inputs: vec![
            call(&ask, "settle", vec![nb(n_ask), iv(2), iv(1), iv(0)], "ask.settle", CARRIER, a1, 1_000),
            call(&bid, "fill", vec![nb(r), iv(2), iv(0)], "bid.fill", v, b1, 1_000),
            tok_in(
                t,
                CARRIER,
                10_000,
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
    // the foreign token: 5,000 owned (id_type 2) by the ask's covenant id, carrying its own columns
    let at = s.inputs.len();
    s.inputs.push(tok_in(
        &f.net.t,
        CARRIER,
        5_000,
        &a.as_bytes(),
        SCHEME_COVID,
        OTHER_COV,
        Some(vec![tok_state(5_000, &taker, SCHEME_P2PK)]),
        Wit::CovId,
        1_000,
    ));
    let change = s.outputs.pop().expect("change");
    s.outputs.push(out(CARRIER, f.net.t.spk(5_000, &taker, SCHEME_P2PK), Some((at as u16, OTHER_COV))));
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
    let mut s = cond_fill_at(f, &cp, 4_000, 1, &cp, None, 0, floor + delta);
    s.name = format!("FXL4 low stop {stop}: stop-market fill at {} (band floor {floor})", floor + delta);
    s
}

/// Regressions (sell side, KRON, both templates): positional IOC returns, foreign strays, the multiply-first stop
/// band. The repeat-IFD merge checks are in `v2_repeat_ifd_attacks` (NRP25..NRP27, both templates).
#[test]
fn v2_kron_fix_pass_regressions() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v2_kron_fix_pass_regressions_body(&fx_with(tpl));
    }
}
fn v2_kron_fix_pass_regressions_body(f: &Fx) {
    let n = &f.net;
    // each IOC return is positional (tokOut == tokenIn)
    run_ok(n, &fx_ioc_pair(f, 1_000, false));
    run_bad(n, &fx_ioc_pair(f, 1_000, true), 1);
    let mut p = AskP::new(pk(&f.maker_a), P250);
    p.tif = 1;
    run_ok(n, &with_name(ioc_fill(f, &p, 4_000, P250, 0), "FXE1+ single IOC ask returns at its custody index"));
    // the return cannot double as a bid's positional delivery
    run_bad(n, &fx_ioc_ask_plus_bid(f), 0);
    // a foreign-program stray can never be the custody; one riding along does not weaken the ask
    run_bad(n, &fx_foreign_stray(f, true), 0);
    // (the positive with a second token riding along is in the KCC-20 suite: this harness carries one token per transaction)
    // stop 9999 sompi per whole token with the default 3% band: the floor is 9700, not 9999
    run_ok(n, &fx_low_stop(f, 9_999, 0));
    run_bad(n, &fx_low_stop(f, 9_999, -1), 0);
    run_ok(n, &fx_low_stop(f, 100, 0));
}

// ================================================================ protocol v3: base units, minFill, rounding, tips, overflow, merges

/// Added to a price or tip so that the rate is not a multiple of SCALE: quotes of amounts that are not a multiple of
/// the scale then round (floor and ceil differ by one sompi).
const ODD: i64 = 7;

/// The covenants' quoteOf (mirrored here) against kob_protocol::state::quote_of and the exact i128 quotient.
#[test]
fn v3_quote_mirror() {
    use kob_protocol::state::{quote_of as proto_quote, Round};
    let ns = [0i64, 1, 7, 999, 1_000, 1_001, 4_321, 10_000, 123_456_789, 1 << 40];
    let rs = [0i64, 1, 7, 999, 1_000, 1_001, 245_100_007, 300_000_000, 9_000_000_000];
    let scales = [1i64, 10, 1_000, 1_000_000, 1_000_000_000];
    let mut checked = 0;
    for &s in &scales {
        for &n in &ns {
            for &r in &rs {
                for up in [false, true] {
                    let mine = quote_of(n, r, s, up);
                    let exact = (n as i128 * r as i128 + if up { s as i128 - 1 } else { 0 }) / s as i128;
                    let proto = proto_quote(n, r, s, if up { Round::Up } else { Round::Down });
                    if let Some(q) = mine {
                        assert_eq!(q as i128, exact, "quoteOf({n}, {r}, {s}, {up}) split formula");
                        assert_eq!(proto, Some(q), "kob_protocol quote_of({n}, {r}, {s}, {up})");
                    } else {
                        assert!(exact > i64::MAX as i128, "quoteOf({n}, {r}, {s}, {up}) fails although {exact} fits");
                    }
                    checked += 1;
                }
            }
        }
    }
    println!("QUOTE-MIRROR {checked} quotes checked against kob_protocol and i128");
}

/// minFill: a fill of at least minFill base units unless it takes everything left (asks, conditional, if-done) or, for
/// a bid, unless the bid terminates because less than one minimum fill of buying power is left; KobBid needs
/// minFill > 0.
#[test]
fn v3_min_fill() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v3_min_fill_body(&fx_with(tpl));
    }
}
fn v3_min_fill_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    // KobAskKron
    let p = AskP { min_fill: 2_500, ..AskP::new(m, P250) };
    run_ok(n, &with_name(s1_with(f, &p, 2_500), "V3A01+ ask fill of exactly minFill (2,500 of 10,000)"));
    run_bad(n, &with_name(s1_with(f, &p, 2_499), "V3A01 ask fill one base unit below minFill (2,499 of 10,000, not the rest)"), 0);
    let small = AskP { amount: 1_234, ..p.clone() };
    run_ok(n, &with_name(sell_out(f, &small, P250), "V3A01r ask fill below minFill that takes the rest (1,234 left, minFill 2,500)"));
    // KobBidKron
    let bp = BidP { min_fill: 1_500, ..BidP::new(pk(&f.maker_b), P245) };
    run_ok(n, &with_name(s6_with(f, &bp, 1_500, 10_000, P245, 0), "V3B01+ bid fill of exactly minFill (1,500), the bid continues"));
    run_bad(
        n,
        &with_name(s6_with(f, &bp, 1_499, 10_000, P245, 0), "V3B01 bid fill below minFill (1,499) that does not end the bid"),
        0,
    );
    let zero = BidP { min_fill: 0, ..BidP::new(pk(&f.maker_b), P245) };
    run_bad(n, &with_name(s6_with(f, &zero, 4_000, 10_000, P245, 0), "V3B02 bid with minFill 0 is unfillable"), 0);
    // less than one minimum fill of buying power left after the fill: a fill below minFill may end the bid
    let v = bp.used(1_000) + DC + bp.used(1_500) - 1;
    run_ok(
        n,
        &with_name(
            s6_v(f, &bp, 1_000, v, P245, 0),
            "V3B03+ bid fill below minFill (1,000) that ends the bid: less than one minimum fill of buying power left",
        ),
    );
    // canContinue compares with the ceil budget of one minimum fill: at a rate with a remainder one sompi less is not enough
    let odd = BidP { min_fill: 1_500, price: P245 + ODD, ..BidP::new(pk(&f.maker_b), P245) };
    assert_eq!(odd.used(1_500), quote_of(1_500, odd.price + odd.tip, SCALE, false).unwrap() + 1);
    let v = odd.used(4_000) + DC + odd.used(1_500) - 1;
    run_ok(
        n,
        &with_name(
            s6_v(f, &odd, 4_000, v, odd.price, 0),
            "V3B04+ bid left one sompi short of a minimum fill's ceil budget terminates",
        ),
    );
    run_bad(
        n,
        &with_name(
            s6_shape(f, &odd, 4_000, v, odd.price, 0, true),
            "V3B04 bid continues with one sompi less than the ceil budget of one minimum fill",
        ),
        0,
    );
    // KobCondAskKron
    let cp = CondP { min_fill: 2_500, ..CondP::oco(m) };
    run_ok(n, &with_name(cond_fill(f, &cp, 2_500, 0, &cp, None), "V3CA01+ conditional TP fill of exactly minFill (2,500 of 10,000)"));
    run_bad(
        n,
        &with_name(cond_fill(f, &cp, 2_499, 0, &cp, None), "V3CA01 conditional TP fill below minFill (2,499 of 10,000, not the rest)"),
        0,
    );
    // KobIfdBidKron
    run_ok(
        n,
        &with_name(
            ifd_fill(f, &IfdKnobs { min_fill: 2_500, ..IfdKnobs::new(10_000, 2_500) }),
            "V3IB01+ if-done fill of exactly minFill (2,500)",
        ),
    );
    run_bad(
        n,
        &with_name(
            ifd_fill(f, &IfdKnobs { min_fill: 2_500, ..IfdKnobs::new(10_000, 2_499) }),
            "V3IB01 if-done fill below minFill (2,499, not the rest)",
        ),
        0,
    );
    run_ok(
        n,
        &with_name(
            ifd_fill(f, &IfdKnobs { min_fill: 2_500, ..IfdKnobs::new(1_234, 1_234) }),
            "V3IB01r if-done fill below minFill that takes the rest (1,234)",
        ),
    );
}

/// Rounding in the maker's favour at amounts that are not a multiple of the scale: the exact boundary validates, one
/// sompi worse is refused (ask proceeds ceil, bid spend floor and budget ceil, conditional proceeds and re-arm budget
/// ceil, if-done spend floor and merge budget ceil).
#[test]
fn v3_rounding() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v3_rounding_body(&fx_with(tpl));
    }
}
fn v3_rounding_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let rounds = |amount: i64, r: i64| assert_eq!(q_up(amount, r), q_down(amount, r) + 1, "{amount} at {r} rounds");
    // KobAskKron: proceeds = quoteOf(n, p - tip, ceil)
    let p = AskP { price: P250 + ODD, ..AskP::new(m, P250) };
    rounds(4_321, p.price - p.tip);
    run_ok(n, &with_name(s1_with(f, &p, 4_321), "V3A02+ ask fill of 4,321 paid exactly the ceil of its proceeds"));
    let mut s = with_name(s1_with(f, &p, 4_321), "V3A02 ask paid one sompi short of the ceil (4,321 at a rate with a remainder)");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    // KobBidKron: the maker pays quoteOf(n, p + tip, floor); the escrow is consumed at quoteOf(n, pMax + tip, ceil)
    let bp = BidP { price: P245 + ODD, ..BidP::new(pk(&f.maker_b), P245) };
    rounds(4_321, bp.price + bp.tip);
    run_ok(
        n,
        &with_name(
            s6_with(f, &bp, 4_321, 10_000, bp.price, 0),
            "V3B05+ bid fill of 4,321: spend floor, budget ceil, the sompi rides on the delivery",
        ),
    );
    let mut s = with_name(
        s6_with(f, &bp, 4_321, 10_000, bp.price, 0),
        "V3B05 bid charged one sompi above the floor (the delivery one sompi short)",
    );
    s.outputs[0].value -= 1;
    s.outputs[2].value += 1;
    run_bad(n, &s, 0);
    let mut s = with_name(
        s6_with(f, &bp, 4_321, 10_000, bp.price, 0),
        "V3B06 bid continuation consumes one sompi less than the ceil budget (the delivery one sompi short)",
    );
    s.outputs[1].value += 1;
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    // KobCondAskKron: proceeds = quoteOf(n, legPrice - tip, ceil)
    let cp = CondP { tp: 300_000_000 + ODD, ..CondP::oco(m) };
    rounds(4_321, cp.tp - cp.tip);
    run_ok(n, &with_name(cond_fill(f, &cp, 4_321, 0, &cp, None), "V3CA02+ conditional TP fill of 4,321 paid exactly the ceil"));
    let mut s = with_name(cond_fill(f, &cp, 4_321, 0, &cp, None), "V3CA02 conditional TP fill paid one sompi short of the ceil");
    s.outputs[0].value -= 1;
    run_bad(n, &s, 0);
    // KobCondAskKron as a booked exit: the entry gets rptBudget = quoteOf(n, rptPrice, ceil) back on a sell-out
    let entry = IfdP { tip: TIP + ODD, ..rpt_entry(m, 0, 21_000) };
    rounds(4_321, entry.rate());
    let exit = CondP { rpt_price: entry.rate(), ..booked_exit(m, 4_321) };
    let k = MergeKnobs { entry: entry.clone(), exit: Some(exit.clone()), ..MergeKnobs::new(m, 0, 4_321, 4_321) };
    run_ok(n, &with_name(rpt_merge(f, &k), "V3CA03+ re-arming sell-out of 4,321: the entry gets exactly the ceil budget"));
    run_bad(
        n,
        &with_name(
            rpt_merge(f, &MergeKnobs { cont_delta: -1, maker_delta: 1, ..k.clone() }),
            "V3CA03 re-arming sell-out: entry continuation one sompi short of the ceil budget (the maker gets that sompi)",
        ),
        0,
    );
    // KobIfdBidKron: spend = quoteOf(n, p + tip, floor)
    let ik = IfdKnobs { tip: TIP + ODD, ..IfdKnobs::new(10_000, 4_321) };
    rounds(4_321, P260 + TIP + ODD);
    run_ok(n, &with_name(ifd_fill(f, &ik), "V3IB02+ if-done fill of 4,321 charged exactly the floor"));
    run_bad(
        n,
        &with_name(
            ifd_fill(f, &IfdKnobs { cont_delta: -1, ..ik.clone() }),
            "V3IB02 if-done fill charged one sompi above the floor (continuation short)",
        ),
        0,
    );
    // KobIfdBidKron merge: floor = in + quoteOf(m, price + tip, ceil) (partial take-profit: the exit does not check it)
    let entry = IfdP { tip: TIP + ODD, ..rpt_entry(m, 6_000, 21_000) };
    rounds(3_001, entry.rate());
    let k = MergeKnobs {
        entry: entry.clone(),
        exit: Some(CondP { rpt_price: entry.rate(), ..booked_exit(m, 4_000) }),
        ..MergeKnobs::new(m, 6_000, 4_000, 3_001)
    };
    run_ok(n, &with_name(rpt_merge(f, &k), "V3IB03+ partial take-profit of 3,001 re-arms the entry with exactly the ceil budget"));
    run_bad(
        n,
        &with_name(
            rpt_merge(f, &MergeKnobs { cont_delta: -1, ..k.clone() }),
            "V3IB03 merge: entry re-armed one sompi short of the ceil budget",
        ),
        2,
    );
}

/// The tip never exceeds the price: KobAsk p >= tip (also a decayed price), KobCondAsk legPrice >= tip (either leg).
#[test]
fn v3_tip() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v3_tip_body(&fx_with(tpl));
    }
}
fn v3_tip_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let p = AskP { price: TIP, ..AskP::new(m, P250) };
    run_ok(n, &with_name(s1_with(f, &p, 4_000), "V3A03+ ask quoting exactly its tip (proceeds 0)"));
    let p = AskP { price: TIP / 2, ..AskP::new(m, P250) };
    run_bad(n, &with_name(s1_with(f, &p, 4_000), "V3A03 ask quoting below its tip"), 0);
    let d = AskP { price: 300_000, slope: 100_000, price_end: TIP / 2, active_from: 1_000, decay_step: 1_000, ..AskP::new(m, P250) };
    let at = |t: i64| decay_at(d.price, d.price_end, d.slope, d.decay_step, d.active_from, t);
    assert_eq!(at(3_000), TIP);
    run_ok(n, &with_name(s1_at(f, &d, 4_000, at(3_000), 3_000), "V3A04+ Dutch ask decayed exactly to its tip"));
    assert!(at(4_000) < TIP);
    run_bad(n, &with_name(s1_at(f, &d, 4_000, at(4_000), 4_000), "V3A04 Dutch ask decayed below its tip"), 0);
    let cp = CondP { tp: TIP, ..CondP::oco(m) };
    run_ok(n, &with_name(cond_fill(f, &cp, 4_000, 0, &cp, None), "V3CA04+ conditional TP leg at exactly its tip"));
    let cp = CondP { tp: TIP / 2, ..CondP::oco(m) };
    run_bad(n, &with_name(cond_fill(f, &cp, 4_000, 0, &cp, None), "V3CA04 conditional TP leg below its tip"), 0);
    let low = armed(&CondP { stop: 99_000, ..CondP::oco(m) });
    assert!(stop_leg(low.stop, low.slip_bps) < TIP);
    run_bad(n, &with_name(cond_fill(f, &low, 4_000, 1, &low, None), "V3CA04b armed stop leg whose band price is below its tip"), 0);
}

/// Overflow at the limits: every product is checked by the engine, so a quote that does not fit i64 fails the script
/// (never wraps). The largest fitting amount validates; one more base unit fails although the counterparty pays the
/// exact value. (No contract check to ablate: the failure is the engine's checked arithmetic.)
#[test]
fn v3_overflow() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v3_overflow_body(&fx_with(tpl));
    }
}
fn v3_overflow_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    const PX: i64 = 9_000_000_000_000_000_000; // 9 * 10^18 sompi per whole token
    let fits = |a: i64| quote_of(a, PX, SCALE, true).is_some();
    let max = (1..=2_000).rev().find(|&a| fits(a)).unwrap();
    assert_eq!(max, 1_024);
    // KobAskKron, tip 0: the proceeds of the whole amount
    let p = AskP { price: PX, tip: 0, amount: max, ..AskP::new(m, PX) };
    run_ok(n, &with_name(sell_out(f, &p, PX), "V3A05+ ask sold out at the largest amount whose proceeds fit i64 (1,024 at 9e18)"));
    let p1 = AskP { amount: max + 1, ..p.clone() };
    let exact = ((max + 1) as i128 * PX as i128 + SCALE as i128 - 1) / SCALE as i128;
    assert!(exact > i64::MAX as i128 && exact < u64::MAX as i128);
    run_bad(
        n,
        &with_name(
            sell_out_paying(f, &p1, exact as u64),
            "V3A05 ask sold out one base unit above it, paid the exact value: the quote overflows",
        ),
        0,
    );
    // KobBidKron, tip 0, minFill = the amount (one fill): the budget of the whole amount
    let bp = BidP { price: PX, tip: 0, min_fill: max, ..BidP::new(pk(&f.maker_b), PX) };
    let v = bp.used(max) + DC;
    run_ok(n, &with_name(s6_v(f, &bp, max, v, PX, 0), "V3B07+ bid buys the largest amount whose budget fits i64 (1,024 at 9e18)"));
    let mut s = with_name(s6_v(f, &bp, max, v, PX, 0), "V3B07 bid fill one base unit above it: the budget overflows");
    let taker = pk(&f.taker);
    set_arg(&mut s, 0, 0, nb(max + 1));
    s.inputs[1] = tok_in(
        &n.t,
        CARRIER,
        max + 1,
        &taker,
        SCHEME_P2PK,
        TOKEN_COV,
        Some(vec![tok_state(max + 1, &bp.maker, SCHEME_P2PK)]),
        Wit::P2pk(f.taker),
        1_000,
    );
    s.outputs[0].script_public_key = n.t.spk(max + 1, &bp.maker, SCHEME_P2PK);
    run_bad(n, &s, 0);
}

/// The entry's fill with its n pushed by OP_PUSHDATA1 (0x4c 0x08 n): bytes [1..9) of its sigscript read 0x08 || n[0..7).
/// With n = 2^55 + k * 2^45 + 3 they are the merge value -(k * 2^53 + 776), what a booked exit at input k selling 776
/// requires of its entry's merge argument. That booked exit (cov 0xc2, parent = this entry) sells out on its take-profit
/// next to the fill (input 3, its custody input 4) and names the fill as its merge. (A KRON token UTXO holds at most
/// 10^9 base units: the token program refuses the delivery of n >= 2^55, so only the exit's input proves the check.)
fn entry_fill_pd1(f: &Fx) -> Scn {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let taker = pk(&f.taker);
    let k2 = 3i64;
    let fill = (1i64 << 55) + k2 * (1i64 << 45) + 3;
    let sold = 8 + 256 * 3;
    let mut w = [0u8; 8];
    w[0] = 0x08;
    w[1..8].copy_from_slice(&fill.to_le_bytes()[..7]);
    assert_eq!(w, merge_arg(k2, sold), "the shifted fill argument reads as the merge value");
    let mut s = ifd_fill(f, &IfdKnobs { price: 1, tip: 0, min_fill: fill, ..IfdKnobs::new(fill + 1_000, fill) });
    set_arg(&mut s, 0, 0, Arg::Pd1(fill.to_le_bytes().to_vec()));
    assert_eq!(s.inputs.len() as i64, k2);
    let x = CondP { amount: sold, parent: cov(RPT_ENTRY).as_bytes(), rpt_price: 1, rpt_until: EXPIRY, ..ifd_exit(m) };
    s.inputs.push(call(
        &n.cond(&x),
        "settle",
        vec![nb(sold), iv(k2 + 1), iv(0), iv(0), iv(0), iv(0), iv(0)],
        "cond.settle",
        CARRIER,
        cov(0xc2),
        2_000,
    ));
    let l = push_tok(
        f,
        &mut s,
        CARRIER,
        sold,
        &cov(0xc2).as_bytes(),
        SCHEME_COVID,
        TOKEN_COV,
        Wit::CovId,
        2_000,
        vec![tok_state(sold, &taker, SCHEME_P2PK)],
    );
    // output 3 (the exit's index) is its maker's payout; the entry's continuation holds what the exit requires
    let budget = q_up(sold, 1);
    s.outputs[3] = out(q_up(sold, x.tp - x.tip) - budget, p2pk_spk(&m), None);
    s.outputs[1].value = s.inputs[0].entry.amount + (budget + 2 * CARRIER) as u64;
    s.outputs.push(out(CARRIER, n.t.spk(sold, &taker, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
    s.inputs.push(p2pk_in(&f.taker, 1000 * KAS));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}

/// Repeat merges: the merge argument -(k * 2^53 + m), the 0x08 first-byte check on both sides (an argument pushed
/// any other way never reads as a merge value), the exit update never next to its entry, the merge claim equal to
/// what the exit sold, and booking only amounts below 2^53.
#[test]
fn v3_merge_push() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v3_merge_push_body(&fx_with(tpl));
    }
}
fn v3_merge_push_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    // exit side, update: an 8-byte ev push is a valid update on its own; next to its entry it is refused
    let oco = CondP::oco(m);
    let mut s = cond_update(f, &oco, &armed(&oco), &down_ev(), 0);
    let ei = ev_args(&s, Kind::Arm).0;
    set_arg(&mut s, 0, 0, Arg::Wide(ei));
    run_ok(n, &with_name(s, "V3CA05+ conditional update with its ev pushed as 8 bytes (no entry beside it)"));
    run_bad(
        n,
        &with_name(exit_update_beside_merge(f, true), "V3CA05 exit update next to its entry: its 8-byte ev reads as the merge amount"),
        0,
    );
    // exit side, settle: the entry's sigscript must start with 0x08
    let mut s = ifd_fill(f, &IfdKnobs::new(10_000, 4_000));
    set_arg(&mut s, 0, 0, Arg::Pd1(4_000i64.to_le_bytes().to_vec()));
    run_ok(n, &with_name(s, "V3CA06+ if-done fill with its n pushed by OP_PUSHDATA1 (no exit beside it)"));
    run_bad(
        n,
        &with_name(entry_fill_pd1(f), "V3CA06 booked exit sells out next to its entry's fill pushed by OP_PUSHDATA1 (not a merge)"),
        3,
    );
    // entry side: the exit's sigscript must start with 0x08
    let mut s = merge_beside_refund(f, 0, true);
    s.inputs.remove(0);
    s.outputs.remove(0);
    set_arg(&mut s, 0, 1, iv(1));
    s.outputs[0].covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: TOKEN_COV });
    balance(&mut s, 1);
    run_ok(n, &with_name(s, "V3IB04+ exit refund with its n pushed by OP_PUSHDATA1 (no entry beside it)"));
    run_bad(
        n,
        &with_name(
            merge_beside_refund(f, 8, true),
            "V3IB04 merge of 8 beside an exit refund pushed by OP_PUSHDATA1 (bytes [1..9) read 8)",
        ),
        0,
    );
    run_bad(n, &with_name(merge_beside_refund(f, 5, false), "V3IB05 merge of 5 beside an exit refund (the exit sold nothing)"), 0);
}

/// The most a KRON token UTXO holds: the pinned token programs refuse an amount above 10^9 base units.
const KRON_MAX_AMOUNT: i64 = 1_000_000_000;

/// Merge limits: a booked exit's amount is below 2^53 (the merge argument holds it), and a merge at the largest
/// buildable exit index validates. KRON bounds both: a token UTXO holds at most 10^9 base units (so neither a booking of
/// 2^53 nor a merge of m = 2^53 - 1 can carry real KRON tokens: the booking attack is proven at the entry's input, the
/// merge argument of m = 2^53 - 1 and k = 999 by its encoding) and an exit's custody is authorised by a one-byte
/// witness read as a signed script number (the exit sits at input 127 at most).
#[test]
fn v3_merge_limits() {
    for tpl in TEMPLATES {
        println!("=== KRON template {tpl}");
        v3_merge_limits_body(&fx_with(tpl));
    }
}
fn v3_merge_limits_body(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let taker = pk(&f.taker);
    // booking (price 1 sompi per whole token, tip 0, one fill: the spend of 2^53 base units fits)
    let book = |a: i64| IfdKnobs { price: 1, tip: 0, min_fill: a, rpt: a + 1, ..IfdKnobs::new(a, a) };
    run_ok(
        n,
        &with_name(ifd_fill(f, &book(KRON_MAX_AMOUNT)), "V3IB06+ booked exit of 10^9 base units (the most a KRON token UTXO holds)"),
    );
    run_bad(n, &with_name(ifd_fill(f, &book(MERGE_K)), "V3IB06 booked exit of 2^53 base units (does not fit the merge argument)"), 0);
    // the merge argument of k = 999 (the contracts' bound) and m = 2^53 - 1 decodes as the covenants decode it
    let b = merge_arg(999, MERGE_K - 1);
    let mut mag = b;
    mag[7] &= 0x7f;
    let v = i64::from_le_bytes(mag);
    assert!(b[7] & 0x80 != 0);
    assert_eq!((v / MERGE_K, v % MERGE_K), (999, MERGE_K - 1));
    // merge at exit index k = 127: a KRON exit's custody is authorised by a one-byte witness (the exit's input index) that the
    // token program reads as a signed script number (0xff reads as -127), so 127 is the largest index an exit can sit at in a KRON
    // transaction; m = 10^9, the most its custody can hold
    let k = 127usize;
    let sold = KRON_MAX_AMOUNT;
    let terms = CondP { tip: 0, tp: 2, stop: 1, ..ifd_exit(m) };
    let entry = IfdP { price: 1, tip: 0, exit: terms.clone(), rpt: 1, ..IfdP::limit(m, 0, 1, terms.clone()) };
    let x = CondP { amount: sold, parent: cov(RPT_ENTRY).as_bytes(), rpt_price: entry.rate(), rpt_until: EXPIRY, ..terms.clone() };
    let budget = q_up(sold, entry.rate());
    let ev = EC;
    let filler = || Inp {
        entry: UtxoEntry::new(0, ScriptPublicKey::new(0, vec![kaspa_txscript::opcodes::codes::OpTrue].into()), 0, false, None),
        role: Role::Raw { ss: vec![], name: "filler" },
        seq: 0,
    };
    let mut inputs = vec![call(
        &n.ifd(&entry),
        "fill",
        vec![nb_merge(k as i64, sold), iv(0), iv(0), Arg::V(bytes(&[])), Arg::V(bytes(&[])), iv(0), iv(0)],
        "ifd.merge",
        ev,
        cov(RPT_ENTRY),
        1_000,
    )];
    while inputs.len() < k {
        inputs.push(filler());
    }
    inputs.push(call(
        &n.cond(&x),
        "settle",
        vec![nb(sold), iv(k as i64 + 1), iv(0), iv(0), iv(0), iv(0), iv(0)],
        "cond.settle",
        CARRIER,
        cov(0xc1),
        2_000,
    ));
    inputs.push(tok_in(
        &n.t,
        CARRIER,
        sold,
        &cov(0xc1).as_bytes(),
        SCHEME_COVID,
        TOKEN_COV,
        Some(vec![tok_state(sold, &taker, SCHEME_P2PK)]),
        Wit::CovId,
        2_000,
    ));
    inputs.push(p2pk_in(&f.taker, 1_000_000 * KAS));
    let mut outputs = vec![
        out(ev + budget + 2 * CARRIER, spk_of(&n.ifd(&IfdP { amount: sold, ..entry.clone() })), Some((0, cov(RPT_ENTRY)))),
        out(CARRIER, n.t.spk(sold, &taker, SCHEME_P2PK), Some(((k + 1) as u16, TOKEN_COV))),
        out(0, p2pk_spk(&taker), None),
    ];
    while outputs.len() < k {
        outputs.push(out(0, p2pk_spk(&taker), None));
    }
    outputs.push(out(q_up(sold, x.tp - x.tip) - budget, p2pk_spk(&m), None));
    let mut s = Scn {
        name: "V3IB07+ merge at exit index 127 with m = 10^9 (the largest index and amount a KRON exit can carry)".into(),
        inputs,
        outputs,
        lock_time: NOW,
        payload: vec![],
    };
    balance(&mut s, 2);
    run_ok(n, &s);
}
