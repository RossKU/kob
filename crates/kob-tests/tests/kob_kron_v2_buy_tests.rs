//! KOB protocol v3 harness, BUY side, KRON family: KobCondBidKron (buy-stop / buy-stop-limit / trailing buy /
//! limit / OCO, buy auctions, keeper tips, repeat exits) and KobIfdAskKron (sell-first IFD/IFO entry with partial
//! fills, stop entries, minFill and repeat IFD, each fill creating a fresh KobCondBidKron exit), triggered by a plain
//! KobAskKron / KobBidKron fill in the same transaction (touch), executed in rusty-kaspa v2.1.0's TxScriptEngine against
//! the REAL KRON token program bytes (both pinned templates, 2,433 B and 2,732 B).
//!
//! Amounts are base units of the token (no lots); prices and tips are sompi per WHOLE token (`scale` base units);
//! quote values follow the covenants' quoteOf (exact split multiplication, rounded in the maker's favour: up for what a
//! maker receives, down for what a maker pays). The fixtures use scale 1000 (a 3-decimal token), so the protocol v2
//! numbers carry over: a price per old lot is a price per whole token, and an old fill of k lots is k * 1000 base units.
//!
//! Port of kob_v2_buy_tests.rs (every scenario id of the KCC-20 buy suite is kept, KCC-20 token semantics replaced by
//! KRON's), plus the KRON token attack set of the v1 adapter suite applied to the buy-side contracts, plus the protocol
//! v3 checks (minimum fill, rounding, tip, overflow, the repeat merge argument -(k * 2^53 + m) and its 0x08 checks, the
//! update parent guard). KRON allows 4 token inputs and 5 token outputs per transaction (the orders' stray scan bound is
//! 4). Ablation runs: KOB_ABLATION=1 KOB_ABLATION_SRC=<dir with a mutated <Name>.sil> reports ABLATION-PASS lines instead
//! of failing when an attack transaction is accepted because the check under test was removed. KRON token semantics: one
//! entry, no leader/delegator; every token input carries the same next-state columns; one witness byte per token input
//! points at the authorising input (id_type 2: the input carrying the owner covenant id; id_type 3: a P2PK input
//! of the owner); custody = id_type 2 owner = order covenant id, is_minter 0; maker/taker deliveries id_type 3.
//! Run: cargo test -p kob-tests --test kob_kron_v2_buy_tests -- --nocapture --test-threads=1
#![allow(dead_code)]
#![allow(clippy::needless_borrows_for_generic_args, clippy::needless_range_loop)]

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
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32, OpTrue};
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use kob_protocol::state::{quote_of, Round};
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
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const MIN_FEE_PER_GRAM: u64 = 100;

const CARRIER: i64 = 10 * KAS;
const DC: i64 = 10 * KAS; // bid delivery carrier
/// Base units per whole token (10^decimals of a 3-decimal token): the price denominator of every order here.
const SCALE: i64 = 1_000;
/// One whole token in base units.
const TOK: i64 = SCALE;
/// Default minimum fill of the fixtures (one whole token).
const MIN_FILL: i64 = TOK;
const TIP: i64 = 100_000; // default priority tip, sompi per whole token (0.001 KAS)
const EXPIRY: i64 = 400_000_000;
const NO_EXPIRY: i64 = 499_999_999_999;
const REFUND_TIP: i64 = 3_000_000;
const MAX_IDLE: i64 = 77_760_000;
const NOW: u64 = 1_000_000; // lockTime of fills (DAA type)
const NET_FEE: i64 = KAS / 10;
/// KAS on the taker's P2PK input that authorises its id_type 3 (address presence) token inputs.
const TK: i64 = KAS;
/// Repeat merge argument -(k * MERGE_K + m): k = input index of the exit, m < MERGE_K the amount it bought back.
const MERGE_K: i64 = 1 << 53;

/// KRON id_types under the names the KCC-20 suites use (owner schemes).
const SCHEME_P2PK: u8 = T_ADDR;
const SCHEME_COVID: u8 = T_COVID;

/// Both pinned KRON token programs; the first is the primary one.
const TPLS: [&str; 2] = [TPL_2433, TPL_2732];

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

/// The covenants' quoteOf(n, r, c) at `scale`, step by step with checked arithmetic (the engine fails the script on an
/// overflow): q = n / scale, m = n % scale, q * r + m * (r / scale) + (m * (r % scale) + c) / scale, c = scale - 1 (up)
/// or 0 (down). None where the covenant fails.
fn quote_at(n: i64, r: i64, scale: i64, up: bool) -> Option<i64> {
    let c = if up { scale - 1 } else { 0 };
    let (q, m) = (n / scale, n % scale);
    let a = q.checked_mul(r)?;
    let b = m.checked_mul(r / scale)?;
    let d = m.checked_mul(r % scale)?.checked_add(c)? / scale;
    let v = a.checked_add(b)?.checked_add(d)?;
    if n >= 0 && r >= 0 {
        // cross-check against the protocol library (the builders, indexer and planner use it)
        assert_eq!(Some(v), quote_of(n, r, scale, if up { Round::Up } else { Round::Down }), "quote_of({n}, {r}, {scale})");
        // and against the exact value: ceil / floor of n * r / scale
        let exact = n as i128 * r as i128;
        let want = if up { (exact + scale as i128 - 1) / scale as i128 } else { exact / scale as i128 };
        assert_eq!(v as i128, want, "split quoteOf({n}, {r}) is not the exact rounded value");
    }
    Some(v)
}
/// quoteOf at the fixtures' scale (panics where the covenant would fail).
fn quote(n: i64, r: i64, up: bool) -> i64 {
    quote_at(n, r, SCALE, up).unwrap_or_else(|| panic!("quoteOf({n}, {r}) overflows"))
}
const UP: bool = true;
const DOWN: bool = false;
/// All-in minimum an ask (quote `p`, tip `tip`) receives for n base units (rounded up).
fn ask_all_in(n: i64, p: i64, tip: i64) -> i64 {
    quote(n, p - tip, UP)
}
/// All-in maximum a bid (quote `p`, tip `tip`) pays for n base units (rounded down).
fn bid_all_in(n: i64, p: i64, tip: i64) -> i64 {
    quote(n, p + tip, DOWN)
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
    /// The all-in rate the bid's escrow is consumed at (the quote, or the cap of a rising bid, plus the tip).
    fn rate_max(&self) -> i64 {
        let pmax = if self.slope != 0 && self.price_end > self.price { self.price_end } else { self.price };
        pmax + self.tip
    }
    /// Escrow n base units consume (rounded up).
    fn budget(&self, n: i64) -> i64 {
        quote_at(n, self.rate_max(), self.scale, UP).expect("bid budget")
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
/// Trigger evidence (touch): a plain resting KobAskKron (ask) or KobBidKron of maker C, of which the
/// matcher fills all `n` base units in the same transaction. Exposed at its quote since max(daa + interval,
/// activeFrom, custody DAA (ask)).
#[derive(Clone)]
struct Ev {
    ask: bool,
    price: i64,
    n: i64,
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
            n: 5 * TOK,
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
struct Tpl {
    pre: Vec<u8>,
    suf: Vec<u8>,
    hash: Vec<u8>,
}
fn tpl_of(a: &SilAbiArtifact) -> Tpl {
    let (pre, suf, hash) = compiled_template_parts_and_hash(a);
    Tpl { pre, suf, hash }
}

/// A network deployment: the KRON token program (primary and the other pinned one) plus the KOB templates the
/// buy side depends on (network constants resolved).
struct Net {
    k: Kron,
    other: Kron,
    ask_tpl: Tpl,
    bid_tpl: Tpl,
}
impl Net {
    fn new(tpl: &str) -> Self {
        let other = if tpl == TPL_2433 { TPL_2732 } else { TPL_2433 };
        let empty = || Tpl { pre: vec![], suf: vec![], hash: vec![] };
        let mut n = Net { k: Kron::load(tpl), other: Kron::load(other), ask_tpl: empty(), bid_tpl: empty() };
        n.ask_tpl = tpl_of(&n.ask(&AskP::new([1; 32], 1)));
        n.bid_tpl = tpl_of(&n.bid(&BidP::new([1; 32], 1)));
        n
    }
    /// (tokenCovId, tokenTplHash, tplPrefixLen = 0, tplSuffixLen) of the primary token program.
    fn tok_args(&self) -> Vec<ArtifactValue> {
        self.tok_args_of(TOKEN_COV)
    }
    fn tok_args_of(&self, token: Hash) -> Vec<ArtifactValue> {
        vec![bytes(&token.as_bytes()), bytes(&self.k.hash), int(0), int(self.k.suffix.len() as i64)]
    }
    /// The evidence templates a conditional order / stop entry inlines (KobAskKron, then KobBidKron:
    /// hash, prefix and suffix length), as in contracts/adapters/kron/v2/KobCondBidKron.ctor.json.
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
}

/// KRON token state helper (amount, owner, id_type), non-minter.
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
    /// An int argument pushed NON-minimally as an 8-byte push (0x08 || 8-byte script number); first argument only.
    /// Signature-script pushes need not be minimal under covenants, and the engine decodes numbers of up to 8 bytes.
    Int8(i64),
}
fn nb(n: i64) -> Arg {
    Arg::V(bytes(&n.to_le_bytes()))
}
fn iv(i: i64) -> Arg {
    Arg::V(int(i))
}
/// An 8-byte script number (sign-magnitude, little endian).
fn num8(v: i64) -> [u8; 8] {
    let mut b = v.unsigned_abs().to_le_bytes();
    assert!(b[7] & 0x80 == 0, "magnitude does not fit 63 bits");
    if v < 0 {
        b[7] |= 0x80;
    }
    b
}
/// Length of the first push of a script.
fn first_push_len(s: &[u8]) -> usize {
    match s[0] {
        0x01..=0x4b => 1 + s[0] as usize,
        0x4c => 2 + s[1] as usize,
        0x4d => 3 + u16::from_le_bytes([s[1], s[2]]) as usize,
        _ => 1,
    }
}
/// How a token input's witness byte is chosen.
#[derive(Clone, Copy)]
enum Wit {
    /// The first input that authorises it (id_type 2: covenant id == owner; id_type 3: P2PK(owner)).
    Auto,
    /// As Auto (the names the KCC-20 suites use: by covenant id / by the owner's P2PK input).
    CovId,
    P2pk(Keypair),
    /// Force the witness byte (attacks).
    At(u8),
}
#[derive(Clone)]
enum Role {
    Call {
        art: SilAbiArtifact,
        entry: &'static str,
        args: Vec<Arg>,
        name: &'static str,
    },
    /// A KRON token input in state `st` (program bytes `redeem`).
    Tok {
        redeem: Vec<u8>,
        st: KS,
        wit: Wit,
        /// next states of this input's covenant group when it is not the token group (`Scn::next`)
        cols: Option<Vec<KS>>,
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
    /// Next states of the token covenant group (shared by every token input, in token output order).
    next: Vec<KS>,
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
/// A KRON token input (`amount` base units owned by `owner` with id_type `typ`, not a minter).
#[allow(clippy::too_many_arguments)]
fn tok_in(k: &Kron, value: i64, amount: i64, owner: &[u8; 32], typ: u8, c: Hash, wit: Wit, daa: u64) -> Inp {
    let st = ks(owner, typ, amount);
    let redeem = k.redeem(&st);
    Inp { entry: utxo(value, pay_to_script_hash_script(&redeem), c, daa), role: Role::Tok { redeem, st, wit, cols: None }, seq: 0 }
}
fn p2pk_in(kp: &Keypair, value: i64) -> Inp {
    Inp { entry: UtxoEntry::new(value as u64, p2pk_spk(&pk(kp)), 0, false, None), role: Role::P2pk { kp: *kp }, seq: 0 }
}
/// A plain OpTrue input without covenant (padding: moves later inputs to higher indices).
fn pad_in() -> Inp {
    Inp {
        entry: UtxoEntry::new(0, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, None),
        role: Role::RawSs { ss: vec![], name: "pad" },
        seq: 0,
    }
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

/// The witness column of covenant group `c`: one byte per token input of the group, in input order.
fn witness_column(s: &Scn, c: Option<Hash>) -> Vec<u8> {
    let mut w = vec![];
    for inp in s.inputs.iter().filter(|i| i.entry.covenant_id == c) {
        if let Role::Tok { st, wit, .. } = &inp.role {
            w.push(match wit {
                Wit::At(b) => *b,
                Wit::Auto | Wit::CovId | Wit::P2pk(_) => {
                    let j = match st.typ {
                        T_COVID => s.inputs.iter().position(|o| o.entry.covenant_id == Some(Hash::from_bytes(st.owner))),
                        T_ADDR => s.inputs.iter().position(|o| o.entry.script_public_key == p2pk_spk(&st.owner)),
                        _ => None,
                    };
                    j.unwrap_or(0xff) as u8
                }
            });
        }
    }
    w
}

fn build(s: &Scn, budgets: &[u16]) -> (Transaction, Vec<UtxoEntry>) {
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
    // next states of a group: the token group's are s.next, another covenant's ride on its first token input
    let cols_of = |c: Option<Hash>| -> Vec<KS> {
        s.inputs
            .iter()
            .filter(|i| i.entry.covenant_id == c)
            .find_map(|i| match &i.role {
                Role::Tok { cols: Some(n), .. } => Some(n.clone()),
                _ => None,
            })
            .unwrap_or_else(|| s.next.clone())
    };
    for (k, inp) in s.inputs.iter().enumerate() {
        let sig_of = |kp: &Keypair, ht: u8| sign(&unsigned, &entries, k, kp, ht);
        let ss = match &inp.role {
            Role::Call { art, entry, args, .. } => {
                let vals: Vec<ArtifactValue> = args
                    .iter()
                    .map(|a| match a {
                        Arg::V(v) => v.clone(),
                        Arg::Int8(v) => int(*v),
                        Arg::Sig(kp, ht) => sig_of(kp, *ht).into(),
                        Arg::SigPos(kp, ht) => (0..256)
                            .map(|_| sig_of(kp, *ht))
                            .find(|s| s[7] & 0x80 == 0 && s[..8].iter().any(|&b| b != 0))
                            .expect("a signature reading as n > 0")
                            .into(),
                    })
                    .collect();
                let mut ss = entry_ss(art, entry, &vals);
                if let Some(Arg::Int8(v)) = args.first() {
                    // the first argument re-pushed as 0x08 || 8-byte script number
                    let mut b = vec![0x08];
                    b.extend(num8(*v));
                    b.extend_from_slice(&ss[first_push_len(&ss)..]);
                    ss = b;
                }
                ss
            }
            Role::Tok { redeem, .. } => {
                kron_ss(redeem, &cols_of(inp.entry.covenant_id), &[], &witness_column(s, inp.entry.covenant_id))
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
        Role::Tok { .. } => "kron.token",
        Role::P2pk { .. } => "p2pk",
        Role::Raw => "attacker.cov",
        Role::RawSs { name, .. } => name,
    }
}

/// Positive scenario: every input must pass. Prints per-input units and tx size / mass / fee.
fn run_ok(s: &Scn) -> u64 {
    let (tx, entries) = build(s, &[]);
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
    let (tx, entries) = build(s, &budgets);
    let res = execute(&tx, &entries).expect("ctx");
    assert!(res.iter().all(|r| r.is_ok()), "{}: second pass failed", s.name);
    let mc = MassCalculator::new(1, 10, 1_000_000_000_000);
    let nc = mc.calc_non_contextual_masses(&tx);
    let populated = PopulatedTransaction::new(&tx, entries.clone());
    let storage = mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX);
    let size = transaction_estimated_serialized_size(&tx);
    let fee_mass = nc.compute_mass.max(2 * size);
    println!("SCENARIO {}  [PASS]", s.name);
    if tx.inputs.len() <= 16 {
        for (i, r) in res.iter().enumerate() {
            println!(
                "  in[{i}] {:<16} sigscript={:>5} B  script_units={:>7}  compute_budget={}",
                role_name(&s.inputs[i].role),
                tx.inputs[i].signature_script.len(),
                r.as_ref().unwrap(),
                budgets[i]
            );
        }
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
fn run_bad(s: &Scn, expect: usize) {
    let (tx, entries) = build(s, &[]);
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
/// Sets the value of input idx.
fn set_value(s: &mut Scn, idx: usize, value: i64) {
    let e = &s.inputs[idx].entry;
    s.inputs[idx].entry = UtxoEntry::new(value as u64, e.script_public_key.clone(), e.block_daa_score, false, e.covenant_id);
}
/// Expiry scenarios: order UTXO recent enough that the 90-day idle bound is later than expiry.
fn fresh(mut s: Scn) -> Scn {
    set_daa(&mut s, 0, (EXPIRY - 1_000) as u64);
    s
}
/// Replaces the shared next states of the token group (idx kept for parity with the KCC-20 suite).
fn set_leader_next(s: &mut Scn, _idx: usize, next: Vec<KS>) {
    s.next = next;
}
/// Rebuilds token input `idx` (and every token output, from `next`) on another KRON program (template pin).
fn retemplate(s: &mut Scn, k: &Kron) {
    for inp in s.inputs.iter_mut() {
        if let Role::Tok { redeem, st, .. } = &mut inp.role {
            *redeem = k.redeem(st);
            let e = &inp.entry;
            inp.entry = UtxoEntry::new(e.amount, pay_to_script_hash_script(redeem), e.block_daa_score, false, e.covenant_id);
        }
    }
    let mut j = 0;
    for o in s.outputs.iter_mut() {
        if o.covenant.as_ref().is_some_and(|c| c.covenant_id == TOKEN_COV) {
            o.script_public_key = k.spk(&s.next[j]);
            j += 1;
        }
    }
    assert_eq!(j, s.next.len(), "token outputs vs next states");
}
/// Moves every token input and token output to covenant id `c` (a lookalike token with the right template).
fn retag(s: &mut Scn, c: Hash) {
    for inp in s.inputs.iter_mut() {
        if matches!(inp.role, Role::Tok { .. }) {
            let e = &inp.entry;
            inp.entry = UtxoEntry::new(e.amount, e.script_public_key.clone(), e.block_daa_score, false, Some(c));
        }
    }
    for o in s.outputs.iter_mut() {
        if let Some(b) = o.covenant.as_mut() {
            if b.covenant_id == TOKEN_COV {
                b.covenant_id = c;
            }
        }
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
fn fx(tpl: &str) -> Fx {
    Fx { net: Net::new(tpl), maker_a: keypair(), maker_b: keypair(), maker_c: keypair(), taker: keypair(), matcher: keypair() }
}

const P250: i64 = 250_000_000; // 2.50 KAS per whole token
const P255: i64 = 255_000_000;
const P260: i64 = 260_000_000;
const P245: i64 = 245_000_000;
/// Script public key of a KRON token UTXO (`amount` base units, `owner`, id_type `typ`, not a minter).
fn tspk(k: &Kron, amount: i64, owner: &[u8; 32], typ: u8) -> ScriptPublicKey {
    k.spk(&tok_state(amount, owner, typ))
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
    min_touch: i64,
    min_rest: i64,
    amount: i64,
    armed: i64,
    band_daa: i64,
    keeper_tip: i64,
    min_fill: i64,
    /// repeat IFD: the entry this exit re-arms (0 = none), its proceeds and prefund rates, deadline
    parent: [u8; 32],
    rpt_price: i64,
    rpt_pre: i64,
    rpt_until: i64,
}
impl CondBP {
    /// Buy OCO: limit leg at 2.00 (below the market), buy-stop 3.00 with the default 3% band
    /// (stop leg 3.09), 10 whole tokens, evidence fills of >= 1 whole token exposed >= 600 DAA.
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
            min_touch: TOK,
            min_rest: 600,
            amount: 10 * TOK,
            armed: 0,
            band_daa: 0,
            keeper_tip: 0,
            min_fill: MIN_FILL,
            parent: [0; 32],
            rpt_price: 0,
            rpt_pre: 0,
            rpt_until: 0,
        }
    }
    fn with_amount(&self, amount: i64) -> Self {
        let mut c = self.clone();
        c.amount = amount;
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
    expiry: i64,
    exit: CondBP,
    min_fill: i64,
    entry_stop: i64,
    band_daa: i64,
    keeper_tip: i64,
    armed: i64,
    amount: i64,
    /// repeat: 0 = off, else 1 + base units of re-arms left
    rpt: i64,
    /// stop entry: smallest evidence fill (base units)
    min_touch: i64,
}

/// Length of the KobCondBidKron state an if-done ask commits to (the entry appends the repeat fields).
const CONDB_COMMIT_LEN: usize = 288;
/// Full KobCondBidKron state (commit + parent, rptPrice, rptPre, rptUntil).
const CONDB_STATE_LEN: usize = 348;
/// KobIfdAskKron state.
const IFDA_STATE_LEN: usize = 561;

fn condb(n: &Net, p: &CondBP) -> SilAbiArtifact {
    let mut a = n.ev_consts();
    a.push(bytes(&p.maker));
    a.extend(n.tok_args());
    a.extend([
        int(SCALE),
        int(p.min_fill),
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
        int(p.min_touch),
        int(p.min_rest),
        int(p.amount),
        int(p.armed),
        int(p.band_daa),
        int(p.keeper_tip),
        bytes(&p.parent),
        int(p.rpt_price),
        int(p.rpt_pre),
        int(p.rpt_until),
    ]);
    compile_contract(&src("KobCondBidKron"), &a, CompileOptions::default()).expect("compile KobCondBidKron")
}
fn condb_tpl(n: &Net) -> Tpl {
    tpl_of(&condb(n, &CondBP::oco([1; 32])))
}
/// The committed part of the exit state (what KobIfdAskKron stores as `exitState`).
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
        int(SCALE),
        int(p.price),
        int(p.tip),
        int(0),
        int(p.expiry),
        int(REFUND_TIP),
        int(p.prefund),
        int(p.exit_carrier),
        int(p.min_fill),
        int(p.entry_stop),
        int(p.band_daa),
        int(p.min_touch),
        int(600),
        int(p.keeper_tip),
        int(p.armed),
        int(p.amount),
        int(p.rpt),
        bytes(&condb_state(n, &p.exit)),
    ]);
    compile_contract(&src("KobIfdAskKron"), &a, CompileOptions::default()).expect("compile KobIfdAskKron")
}

// ---------------------------------------------------------------- trigger evidence (touch)

/// Appends a KRON token input of covenant `token`. The token group's next states are `s.next`; another
/// covenant's are carried by its first token input (`cols`). `new_states` (the states of token outputs the
/// caller appends after every existing output) join that group's columns. Returns the group's authorising
/// input (its first token input) for the new outputs.
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
    let first = s.inputs.iter().position(|i| i.entry.covenant_id == Some(token) && matches!(i.role, Role::Tok { .. }));
    let mut inp = tok_in(&f.net.k, value, amount, owner, typ, token, wit, daa);
    if token == TOKEN_COV {
        s.next.extend(new_states);
    } else {
        match first {
            Some(h) => {
                if let Role::Tok { cols: Some(c), .. } = &mut s.inputs[h].role {
                    c.extend(new_states);
                }
            }
            None => {
                if let Role::Tok { cols, .. } = &mut inp.role {
                    *cols = Some(new_states);
                }
            }
        }
    }
    s.inputs.push(inp);
    first.unwrap_or(s.inputs.len() - 1)
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
/// the fill); a bid buys the matcher's tokens (input ei + 1, id_type 3: the matcher's P2PK input follows). The matcher
/// takes the change, so the evidence part balances by itself. Returns (ev, tk) as the conditional orders take them
/// (tk = -1 for a bid).
fn add_ev(f: &Fx, s: &mut Scn, e: &Ev) -> (i64, i64) {
    let k = &f.net.k;
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
    let n = e.n;
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
            s.outputs.push(out(2 * CARRIER, tspk(k, n, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
        } else {
            let pay = if e.mode == EvMode::Fill {
                quote_at(n, e.price - TIP, e.scale, UP).expect("evidence ask proceeds") + 2 * CARRIER
            } else {
                CARRIER
            };
            s.outputs.push(out(pay, p2pk_spk(if look { &matcher } else { &mc }), None));
            let st = vec![tok_state(n, &owner, SCHEME_P2PK)];
            let l = push_tok(f, s, CARRIER, n, &e.cov.as_bytes(), SCHEME_COVID, e.token, Wit::CovId, e.tok_daa, st);
            s.outputs.push(out(CARRIER, tspk(k, n, &owner, SCHEME_P2PK), Some((l as u16, e.token))));
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
        let v = bp.budget(n) + DC;
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
            let spend = quote_at(n, e.price + TIP, e.scale, DOWN).expect("evidence bid spend");
            s.outputs.push(out(v - spend, tspk(k, n, &mc, SCHEME_P2PK), Some((l as u16, e.token))));
        } else {
            s.outputs.push(out(v, p2pk_spk(if look { &matcher } else { &mc }), None));
        }
    }
    let ins: i64 = s.inputs[in0..].iter().map(|i| i.entry.amount as i64).sum();
    let outs: i64 = s.outputs[out0..].iter().map(|o| o.value as i64).sum();
    s.outputs.push(out(ins - outs, p2pk_spk(&matcher), None));
    (ei as i64, if e.ask { ei as i64 + 1 } else { -1 })
}

// ---------------------------------------------------------------- buy-side fixtures

/// Value a conditional buy escrows: its whole amount at the worst leg all-in price (rounded up) plus one delivery
/// carrier.
fn condb_value(cp: &CondBP) -> i64 {
    quote(cp.amount, cp.worst() + cp.tip, UP) + DC
}

/// Conditional-buy fill: condb at input 0 (cov 0xe1, UTXO DAA 2000) buys n base units on `leg` at its
/// all-in price from a taker's tokens (input 1, id_type 3 owned by the taker, authorised by the taker's P2PK
/// input, which follows the order's outputs); the taker (its own matcher) receives the whole all-in spend (rounded
/// down). With `ev`, the evidence fill (see add_ev) follows every other input and output and the settle reads it. The
/// order continues (amountLeft - n > 0) as `next_cp`, else terminates.
fn condb_fill(f: &Fx, cp: &CondBP, n: i64, leg: i64, next_cp: &CondBP, ev: Option<&Ev>) -> Scn {
    condb_fill_at(f, cp, n, leg, next_cp, ev, 0, cp.leg_price(leg))
}
/// As condb_fill, with the auction time argument `t_arg` and the leg quote the fill pays.
#[allow(clippy::too_many_arguments)]
fn condb_fill_at(f: &Fx, cp: &CondBP, n: i64, leg: i64, next_cp: &CondBP, ev: Option<&Ev>, t_arg: i64, leg_price: i64) -> Scn {
    let k = &f.net.k;
    let c = cov(0xe1);
    let order = condb(&f.net, cp);
    let taker = pk(&f.taker);
    let v = condb_value(cp);
    let spend = bid_all_in(n, leg_price, cp.tip);
    let cont = n < cp.amount;
    let mut s = Scn {
        name: "condb fill".into(),
        inputs: vec![
            call(&order, "settle", vec![nb(n), iv(1), iv(leg), iv(0), iv(t_arg)], "condb.settle", v, c, 2_000),
            tok_in(k, CARRIER, n, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000),
        ],
        outputs: vec![out(if cont { DC } else { v - spend }, tspk(k, n, &cp.maker, T_ADDR), Some((1, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
        next: vec![tok_state(n, &cp.maker, T_ADDR)],
    };
    if cont {
        s.outputs.push(out(v - spend - DC, spk_of(&condb(&f.net, next_cp)), Some((0, c))));
    }
    s.inputs.push(p2pk_in(&f.taker, TK));
    s.outputs.push(out(spend + CARRIER + TK - NET_FEE, p2pk_spk(&taker), None));
    if let Some(e) = ev {
        let (ei, _) = add_ev(f, &mut s, e);
        set_arg(&mut s, 0, 3, iv(ei));
    }
    s
}
/// Arming evidence for the buy-stop: a resting bid quoting 3.02 (>= stop 3.00) filled with 5 whole tokens.
fn arm_ev() -> Ev {
    Ev::bid(302_000_000)
}
/// Trailing evidence: a resting ask quoting 2.80 (<= 3.00 - 0.05 - 0.10) sold out (5 whole tokens).
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
        next: vec![],
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
/// Pre-funded buy-back budget beyond the proceeds (sompi per whole token), and KAS on each exit UTXO.
const IFDA_PREFUND: i64 = KAS / 2;
const IFDA_EXIT_CARRIER: i64 = 10 * KAS;
/// Entry UTXO value: carrier + the prefund of 10 whole tokens + two exits' carriers (two partial fills).
const IFDA_VALUE: i64 = CARRIER + 10 * IFDA_PREFUND + 2 * IFDA_EXIT_CARRIER;
fn ifda_p(f: &Fx) -> IfdAP {
    let m = pk(&f.maker_a);
    IfdAP {
        maker: m,
        price: P250,
        tip: TIP,
        prefund: IFDA_PREFUND,
        exit_carrier: IFDA_EXIT_CARRIER,
        expiry: EXPIRY,
        exit: ifda_exit(m),
        min_fill: MIN_FILL,
        entry_stop: 0,
        band_daa: 0,
        keeper_tip: 0,
        armed: 0,
        amount: 10 * TOK,
        rpt: 0,
        min_touch: TOK,
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
/// Sell-first IFO entry fill: the if-done ask (cov 0xf1, value `value`, holding `have` base units)
/// sells n base units at its all-in price to a taker. Output 0 is the fresh KobCondBidKron exit (genesis,
/// authorised by input 0) with amountLeft = n. If tokens remain, output 1 is the entry continuation
/// and output 2 the token remainder (tokOut = 2). The taker's tokens (id_type 3) are authorised by input 2.
fn ifda_fill(f: &Fx, ip: &IfdAP, have: i64, n: i64, value: i64) -> Scn {
    ifda_fill_x(f, ip, have, n, value, ip.price, None, 0, 0)
}
/// Sell-first if-done fill with every lever: the quote the fill pays (`eff`), an optional trigger
/// evidence (added after every other input and output, see add_ev), the auction time `t` and the
/// armed state of the continuation. The exit holds the proceeds and the prefund of n, both rounded up.
#[allow(clippy::too_many_arguments)]
fn ifda_fill_x(f: &Fx, ip: &IfdAP, have: i64, n: i64, value: i64, eff: i64, ev: Option<&Ev>, t_arg: i64, next_armed: i64) -> Scn {
    let k = &f.net.k;
    let i = cov(0xf1);
    let entry = ifda(&f.net, &IfdAP { amount: have, ..ip.clone() });
    let exit = condb(&f.net, &ip.exit.with_amount(n));
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let proceeds = quote(n, eff - ip.tip, UP);
    let pre = quote(n, ip.prefund, UP);
    let rest = have - n;
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest, &i.as_bytes(), T_COVID));
    }
    next.push(tok_state(n, &taker, T_ADDR));
    let mut outputs = vec![];
    if rest > 0 {
        outputs.push(out(proceeds + pre + ip.exit_carrier, spk_of(&exit), None));
        let cont = ifda(&f.net, &IfdAP { amount: rest, armed: next_armed, ..ip.clone() });
        outputs.push(out(value - pre - ip.exit_carrier, spk_of(&cont), Some((0, i))));
        outputs.push(out(CARRIER, tspk(k, rest, &i.as_bytes(), T_COVID), Some((1, TOKEN_COV))));
    } else {
        outputs.push(out(proceeds + value + CARRIER, spk_of(&exit), None));
    }
    outputs.push(out(CARRIER, tspk(k, n, &taker, T_ADDR), Some((1, TOKEN_COV))));
    outputs.push(out(1000 * KAS - proceeds - CARRIER - NET_FEE, p2pk_spk(&taker), None));
    let inputs = vec![
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
                iv(t_arg),
            ],
            "ifda.settle",
            value,
            i,
            1_000,
        ),
        tok_in(k, CARRIER, have, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000),
        p2pk_in(&f.taker, 1000 * KAS),
    ];
    let mut s = Scn {
        name: format!("B7 sell-first IFO entry sells {n}/{have} base units, fresh exit with amountLeft = {n}"),
        inputs,
        outputs,
        lock_time: NOW,
        payload: vec![],
        next,
    };
    regen(&mut s, 0, 0);
    if let Some(e) = ev {
        let (ei, tk) = add_ev(f, &mut s, e);
        set_arg(&mut s, 0, 6, iv(ei));
        set_arg(&mut s, 0, 7, iv(tk));
    }
    s
}
fn b7(f: &Fx) -> Scn {
    ifda_fill(f, &ifda_p(f), 10 * TOK, 4 * TOK, IFDA_VALUE)
}
/// Second partial fill: the continuation of b7 (6 whole tokens, value after one exit) sells out.
fn b7b(f: &Fx) -> Scn {
    let ip = ifda_p(f);
    ifda_fill(f, &ip, 6 * TOK, 6 * TOK, IFDA_VALUE - quote(4 * TOK, ip.prefund, UP) - ip.exit_carrier)
}
/// Permissionless arm of a sell-stop if-done entry (cov 0xf1, its 10-token custody not spent); the
/// keeper takes `take` from the entry's carrier. `with_token`: the attacker also spends the
/// entry's token UTXO.
fn ifda_update(f: &Fx, ip: &IfdAP, next: &IfdAP, ev: &Ev, take: i64, with_token: bool) -> Scn {
    let i = cov(0xf1);
    let k = &f.net.k;
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
        next: vec![],
    };
    let (ei, tk) = add_ev(f, &mut s, ev);
    set_arg(&mut s, 0, 0, iv(ei));
    set_arg(&mut s, 0, 1, iv(tk));
    if with_token {
        // the entry's own 10-token custody, co-spent to the keeper
        let thief = pk(&keeper);
        let st = vec![tok_state(10 * TOK, &thief, T_ADDR)];
        let l = push_tok(f, &mut s, CARRIER, 10 * TOK, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000, st);
        s.outputs.push(out(CARRIER, tspk(k, 10 * TOK, &thief, T_ADDR), Some((l as u16, TOKEN_COV))));
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
        next: vec![],
    }
}
/// A token UTXO of `amount` base units owned by covenant id `owner` (id_type 2, authorised by the input carrying that id).
fn owned_tok(f: &Fx, amount: i64, owner: Hash) -> Inp {
    tok_in(&f.net.k, CARRIER, amount, &owner.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_500)
}

// ---------------------------------------------------------------- KRON-specific fixtures

/// Splits token input 1 of a condb fill into `1 + extra` inputs (the first keeps the rest, every extra carries
/// one whole token, taker-owned id_type 3): a covenant group with `1 + extra` token inputs.
fn split_token_inputs(f: &Fx, mut s: Scn, extra: usize) -> Scn {
    let k = &f.net.k;
    let taker = pk(&f.taker);
    let whole = match &s.inputs[1].role {
        Role::Tok { st, .. } => st.amount,
        _ => panic!("input 1 is not a token"),
    };
    let keep = whole - extra as i64 * TOK;
    assert!(keep >= TOK);
    let c = s.inputs[1].entry.amount as i64;
    s.inputs[1] = tok_in(k, c, keep, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000);
    for _ in 0..extra {
        s.inputs.push(tok_in(k, c, TOK, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000));
    }
    // the extra inputs bring their carriers: hand them to the taker so KAS stays balanced
    let l = s.outputs.len() - 1;
    s.outputs[l].value += (extra as i64 * c) as u64;
    s
}
/// Adds `extra` one-token taker-owned token inputs to an if-done entry fill; they come out in the taker's token
/// output (output `taker_out`, the last token output).
fn add_taker_tokens(f: &Fx, mut s: Scn, extra: usize, taker_out: usize) -> Scn {
    let k = &f.net.k;
    let taker = pk(&f.taker);
    for _ in 0..extra {
        s.inputs.push(tok_in(k, CARRIER, TOK, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000));
    }
    let last = s.next.len() - 1;
    s.next[last].amount += extra as i64 * TOK;
    s.outputs[taker_out].script_public_key = k.spk(&s.next[last]);
    s.outputs[taker_out].value += (extra as i64 * CARRIER) as u64;
    s
}
/// If-done entry refund (settle n = 0): tokens back to the maker at output 0 (the order's own index), once
/// expired, or after the 90-day idle bound.
fn ifda_refund(f: &Fx, expiry: i64, lock_time: u64) -> Scn {
    let k = &f.net.k;
    let i = cov(0xf1);
    let mut ip = ifda_p(f);
    ip.expiry = expiry;
    let entry = ifda(&f.net, &ip);
    let tpl = condb_tpl(&f.net);
    let m = ip.maker;
    Scn {
        name: format!("R1 keeper refunds if-done ask (expiry {expiry}, lockTime {lock_time})"),
        inputs: vec![
            call(
                &entry,
                "settle",
                vec![nb(0), iv(1), iv(0), iv(0), Arg::V(bytes(&tpl.pre)), Arg::V(bytes(&tpl.suf)), iv(0), iv(0), iv(0)],
                "ifda.refund",
                IFDA_VALUE,
                i,
                1_000,
            ),
            tok_in(k, CARRIER, 10 * TOK, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000),
        ],
        outputs: vec![out(IFDA_VALUE + CARRIER - REFUND_TIP, tspk(k, 10 * TOK, &m, T_ADDR), Some((1, TOKEN_COV)))],
        lock_time,
        payload: vec![],
        next: vec![tok_state(10 * TOK, &m, T_ADDR)],
    }
}
/// If-done entry cancel by the maker (SIGHASH_ALL only): tokens back to the maker.
fn ifda_cancel(f: &Fx, sighash: u8, signer: Keypair) -> Scn {
    let k = &f.net.k;
    let i = cov(0xf1);
    let ip = ifda_p(f);
    let entry = ifda(&f.net, &ip);
    let m = ip.maker;
    Scn {
        name: format!("C1 maker cancels if-done ask (sighash {sighash:#04x})"),
        inputs: vec![
            call(&entry, "cancel", vec![Arg::Sig(signer, sighash)], "ifda.cancel", IFDA_VALUE, i, 1_000),
            tok_in(k, CARRIER, 10 * TOK, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000),
        ],
        outputs: vec![
            out(CARRIER, tspk(k, 10 * TOK, &m, T_ADDR), Some((1, TOKEN_COV))),
            out(IFDA_VALUE - NET_FEE, p2pk_spk(&m), None),
        ],
        lock_time: 0,
        payload: vec![],
        next: vec![tok_state(10 * TOK, &m, T_ADDR)],
    }
}
/// Conditional buy cancel / refund (KAS only): `entry` = "cancel" (SIGHASH_ALL sig) or "refund".
fn condb_exit(f: &Fx, entry: &'static str, expiry: i64, lock_time: u64, sighash: u8, signer: Keypair) -> Scn {
    let m = pk(&f.maker_b);
    let mut cp = CondBP::oco(m);
    cp.expiry = expiry;
    let order = condb(&f.net, &cp);
    let v = condb_value(&cp);
    let args = if entry == "cancel" { vec![Arg::Sig(signer, sighash)] } else { vec![] };
    Scn {
        name: format!("X1 conditional buy {entry} (expiry {expiry}, lockTime {lock_time}, sighash {sighash:#04x})"),
        inputs: vec![call(&order, entry, args, if entry == "cancel" { "condb.cancel" } else { "condb.refund" }, v, cov(0xe1), 1_000)],
        outputs: vec![out(v - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time,
        payload: vec![],
        next: vec![],
    }
}

// ---------------------------------------------------------------- tests

fn committed_template_hash(name: &str) -> Vec<u8> {
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(common::repo_root().join(format!("contracts/artifacts/{name}.json"))).expect("artifact"),
    )
    .unwrap();
    let c = json["contracts"].as_object().unwrap().values().next().unwrap()["compiled"]["template_hash"].clone();
    match c {
        serde_json::Value::String(h) => (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect(),
        v => serde_json::from_value(v).unwrap(),
    }
}

#[test]
fn kron_v2_buy_sizes_layouts_sigops() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let n = &f.net;
        let m = pk(&f.maker_a);
        let arts =
            [("KobCondBidKron", condb(n, &CondBP::oco(m)), CONDB_STATE_LEN), ("KobIfdAskKron", ifda(n, &ifda_p(&f)), IFDA_STATE_LEN)];
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
        let le = |s: &[u8], a: usize, b: usize| i64::from_le_bytes(s[a..b].try_into().unwrap());
        // The contSpk splice windows of KobCondBidKron must match the compiler's state encoding (no extension
        // commitment: every window sits 33 bytes before the KCC-20 variant's).
        let mut cp = CondBP::oco(m);
        cp.amount = 7_777;
        cp.armed = 1;
        let c = bytecode(&condb(n, &cp));
        assert_eq!((c[190], le(&c, 191, 199)), (0x08, cp.stop), "stopPrice window");
        assert_eq!((c[253], le(&c, 254, 262)), (0x08, 7_777), "amountLeft window");
        assert_eq!((c[262], le(&c, 263, 271)), (0x08, 1), "armed window");
        // KobIfdAskKron splices the exit's amountLeft at KobCondBidKron STATE payload [253..261) and commits the
        // first 288 bytes of the exit state (the entry appends the repeat fields).
        let st = condb_state(n, &cp);
        assert_eq!(st[252], 0x08, "amountLeft push prefix (state coordinates)");
        assert_eq!(le(&st, 253, 261), 7_777, "amountLeft window (state coordinates)");
        assert_eq!(st.len(), CONDB_COMMIT_LEN, "KobCondBidKron committed state length (KobIfdAskKron exitState)");
        // KobIfdAskKron splices armed [245..253), amountLeft [254..262) and rptAmount [263..271) (no token-codec field
        // precedes them: the same offsets as KobIfdAsk) (bytecode coordinates).
        let mut ip = ifda_p(&f);
        ip.armed = 1_234;
        ip.amount = 9_999;
        let ib = bytecode(&ifda(n, &ip));
        assert_eq!((ib[244], le(&ib, 245, 253)), (0x08, 1_234), "ifda armed splice window");
        assert_eq!((ib[253], le(&ib, 254, 262)), (0x08, 9_999), "ifda amountLeft splice window");
        let mut ipr = ifda_p(&f);
        ipr.rpt = 4_444;
        let irb = bytecode(&ifda(n, &ipr));
        assert_eq!((irb[262], le(&irb, 263, 271)), (0x08, 4_444), "ifda rptAmount splice window");
        // Repeat fields of KobCondBidKron (after keeperTip): parent [290..322), rptPrice [323..331), rptPre
        // [332..340), rptUntil [341..349) (bytecode); KobIfdAskKron reads parent at state [289..321) and the exit
        // amountLeft at [253..261).
        let mut cr = CondBP::oco(m);
        cr.parent = [0xab; 32];
        cr.rpt_price = 11;
        cr.rpt_pre = 22;
        cr.rpt_until = 33;
        let crb = bytecode(&condb(n, &cr));
        assert_eq!((crb[289], &crb[290..322]), (0x20, &[0xab; 32][..]), "condb parent field");
        assert_eq!((crb[322], le(&crb, 323, 331)), (0x08, 11), "condb rptPrice field");
        assert_eq!((crb[331], le(&crb, 332, 340)), (0x08, 22), "condb rptPre field");
        assert_eq!((crb[340], le(&crb, 341, 349)), (0x08, 33), "condb rptUntil field");
        let cst = &crb[1..349];
        assert_eq!(cst.len(), CONDB_STATE_LEN);
        assert_eq!(&cst[289..321], &[0xab; 32][..], "condb parent at state [289..321)");
        // The touch windows (touchBid / touch) of the resting KobBidKron's state (252 B, no extension commitment).
        let mut bp = BidP::new(m, P250);
        bp.interval = 88;
        bp.slope = 5;
        bp.active_from = 77;
        let bid = bytecode(&n.bid(&bp));
        let s = &bid[1..253];
        assert_eq!(&s[34..66], &TOKEN_COV.as_bytes(), "bid tokenCovId offset (touch)");
        assert_eq!(le(s, 118, 126), SCALE, "bid scale offset");
        assert_eq!(le(s, 127, 135), MIN_FILL, "bid minFill offset");
        assert_eq!(le(s, 136, 144), bp.price, "bid price offset");
        assert_eq!(le(s, 163, 171), 77, "bid activeFrom offset");
        assert_eq!(le(s, 208, 216), 88, "bid interval offset");
        assert_eq!(le(s, 226, 234), 5, "bid slope offset");
        // ... and of the resting KobAskKron's (243 B): scale [118..126), price [136..144), interval [190..198), slope [208..216)
        let mut ap = AskP::new(m, P255);
        ap.interval = 66;
        ap.slope = 4;
        let ask = bytecode(&n.ask(&ap));
        let s = &ask[1..244];
        assert_eq!(le(s, 118, 126), SCALE, "ask scale offset");
        assert_eq!(le(s, 136, 144), P255, "ask price offset");
        assert_eq!(le(s, 190, 198), 66, "ask interval offset");
        assert_eq!(le(s, 208, 216), 4, "ask slope offset");
        // The evidence templates this harness inlines are the ones the committed ctor files carry.
        let ctor = |name: &str| -> Vec<ArtifactValue> {
            serde_json::from_slice(
                &std::fs::read(common::repo_root().join(format!("contracts/adapters/kron/v2/{name}.ctor.json"))).expect("ctor"),
            )
            .expect("ctor json")
        };
        assert_eq!(ctor("KobCondBidKron")[..6], n.ev_consts()[..], "KobCondBidKron ctor: ASK_TPL/PRE/SUF, BID_TPL/PRE/SUF");
        let mut ifda_consts = n.tpl_consts(&condb_tpl(n));
        ifda_consts.extend(n.tpl_consts(&n.ask_tpl));
        assert_eq!(ctor("KobIfdAskKron")[..6], ifda_consts[..], "KobIfdAskKron ctor: COND_BID_TPL/PRE/SUF, ASK_TPL/PRE/SUF");
        // The tested templates are the committed, reproducible artifacts (network constants resolved).
        let ct = condb_tpl(n);
        let it = tpl_of(&ifda(n, &ifda_p(&f)));
        for (name, tpl) in [("KobAskKron", &n.ask_tpl), ("KobBidKron", &n.bid_tpl), ("KobCondBidKron", &ct), ("KobIfdAskKron", &it)] {
            assert_eq!(committed_template_hash(name), tpl.hash, "{name}: tested template differs from the committed artifact");
        }
        println!("TEMPLATES condbid {}+{} B", ct.pre.len(), ct.suf.len());
    }
}

#[test]
fn kron_v2_buy_positive() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        buy_positive(&fx(tpl));
    }
}
fn buy_positive(f: &Fx) {
    let m = pk(&f.maker_b);
    let oco = CondBP::oco(m);
    run_ok(&with_name(
        condb_fill(f, &oco, 4 * TOK, 0, &oco.with_amount(6 * TOK), None),
        "B1 buy OCO limit leg buys 4/10 whole tokens at 2.00 all-in",
    ));
    let r = arm_ev();
    run_ok(&with_name(
        condb_fill(f, &oco, 4 * TOK, 1, &oco.with_amount(6 * TOK).armed(), Some(&r)),
        "B2 buy-stop leg armed in its own fill: a bid at 3.02 (>= stop) filled in the same tx, buys 4/10 in the default 3% band (3.09), continues armed with 6 tokens",
    ));
    run_ok(&with_name(condb_update(f, &oco, &oco.armed(), &r, 0), "B3 permissionless arm next to a resting-bid fill"));
    run_ok(&with_name(
        condb_fill(f, &oco.armed(), 4 * TOK, 1, &oco.with_amount(6 * TOK).armed(), None),
        "B4 armed buy-stop leg buys without evidence",
    ));
    let tr = trailing_b(m);
    let mut tr2 = tr.clone();
    tr2.stop -= 2 * tr.step;
    run_ok(&with_name(
        condb_update(f, &tr, &tr2, &trail_ev(), tr.wait as u64),
        "B5 trailing buy stop ratchets 3.00 -> 2.90: one ask fill at 2.80 justifies two steps (gap 0.10)",
    ));
    let small = oco.with_amount(4 * TOK);
    run_ok(&with_name(
        condb_fill(f, &small, 4 * TOK, 0, &small, None),
        "B6 last 4 whole tokens bought: order terminates, delivery carries the rest",
    ));
    let mut sl = oco.clone();
    sl.tp = 0;
    run_ok(&with_name(
        condb_fill(f, &sl, 4 * TOK, 1, &sl.with_amount(6 * TOK).armed(), Some(&r)),
        "B6b buy-stop-limit (no limit leg) triggered fill",
    ));
    let s7 = b7(f);
    let exit_id = s7.outputs[0].covenant.as_ref().unwrap().covenant_id;
    run_ok(&s7);
    run_ok(&with_name(b7b(f), "B7b second partial fill (6/6) sells out: a second, independent exit with amountLeft = 6000"));
    // The exit of the first fill (genesis id from B7) works as a normal conditional buy for its
    // 4 tokens: its stop leg (2.80, band 2.884) arms on a resting bid quoting 2.85 filled in the same
    // transaction and buys 2 of them.
    let exit = ifda_exit(pk(&f.maker_a)).with_amount(4 * TOK);
    let up = Ev::bid(285_000_000);
    let mut s = with_name(
        condb_fill(f, &exit, 2 * TOK, 1, &exit.with_amount(2 * TOK).armed(), Some(&up)),
        "B8 exit of B7 (genesis id) buy-stop fills 2 of its 4 whole tokens",
    );
    s.inputs[0].entry = utxo(s.inputs[0].entry.amount as i64, s.inputs[0].entry.script_public_key.clone(), exit_id, 2_000);
    s.outputs[1].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: exit_id });
    run_ok(&s);
    let mut zp = ifda_p(f);
    zp.tip = 0;
    run_ok(&with_name(ifda_fill(f, &zp, 10 * TOK, 4 * TOK, IFDA_VALUE), "B11 zero-tip if-done ask, partial 4/10"));
    let mut z = oco.clone();
    z.tip = 0;
    run_ok(&with_name(
        condb_fill(f, &z, 4 * TOK, 0, &z.with_amount(6 * TOK), None),
        "B9 zero-tip conditional buy filled at exactly its limit-leg quote",
    ));
    let mut band = oco.clone();
    band.slip_bps = 100;
    run_ok(&with_name(
        condb_fill(f, &band, 4 * TOK, 1, &band.with_amount(6 * TOK).armed(), Some(&r)),
        "B10 buy-stop-market with a user band of 1% (3.03)",
    ));

    // --- KRON-specific positives (not in the KCC-20 suite)
    // KRON allows 4 token inputs: a fill whose 4 whole tokens come from 4 taker token UTXOs.
    run_ok(&with_name(
        split_token_inputs(f, condb_fill(f, &oco, 4 * TOK, 0, &oco.with_amount(6 * TOK), None), 3),
        "KB1 buy fill from 4 taker token inputs (KRON max), delivery to the maker as id_type 3",
    ));
    // if-done entry taking 4 token inputs into the group (custody + 3 taker UTXOs)
    run_ok(&with_name(add_taker_tokens(f, b7(f), 3, 3), "KB2 IFO entry partial in a 4-token-input group (custody + 3 taker UTXOs)"));
    // refund / cancel paths of both contracts
    run_ok(&fresh(ifda_refund(f, EXPIRY, EXPIRY as u64)));
    run_ok(&ifda_refund(f, NO_EXPIRY, (1_000 + MAX_IDLE) as u64));
    run_ok(&ifda_cancel(f, 0x01, f.maker_a));
    run_ok(&condb_exit(f, "cancel", EXPIRY, 0, 0x01, f.maker_b));
    run_ok(&fresh(condb_exit(f, "refund", EXPIRY, EXPIRY as u64, 0x01, f.maker_b)));
    run_ok(&condb_exit(f, "refund", NO_EXPIRY, (1_000 + MAX_IDLE) as u64, 0x01, f.maker_b));
}

#[test]
fn kron_v2_buy_negative() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        buy_negative(&fx(tpl));
    }
}
fn buy_negative(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_b);
    let oco = CondBP::oco(m);
    let good = arm_ev();
    let stop_fill =
        |cp: &CondBP, e: &Ev, name: &str| with_name(condb_fill(f, cp, 4 * TOK, 1, &cp.with_amount(6 * TOK).armed(), Some(e)), name);
    let base = || condb_fill(f, &oco, 4 * TOK, 0, &oco.with_amount(6 * TOK), None);

    let mut s = with_name(base(), "NB1 limit leg charged 1 sompi above its all-in price (continuation short)");
    s.outputs[1].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(&s, 0);

    let mut s = with_name(base(), "NB2 limit leg charged 1 sompi above its all-in price (delivery carrier short)");
    s.outputs[0].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(&s, 0);

    let mut s = with_name(base(), "NB3 malleated n = 3000 (outputs for 4000)");
    set_arg(&mut s, 0, 0, nb(3 * TOK));
    run_bad(&s, 0);

    let mut s = with_name(
        condb_fill(f, &oco.armed(), 4 * TOK, 1, &oco.with_amount(6 * TOK).armed(), None),
        "NB4 armed stop leg charged 1 sompi above its band",
    );
    s.outputs[1].value -= 1;
    let l = s.outputs.len() - 1;
    s.outputs[l].value += 1;
    run_bad(&s, 0);

    let mut s = with_name(
        condb_fill(f, &oco, 4 * TOK, 1, &oco.with_amount(6 * TOK).armed(), None),
        "NB5 stop leg without evidence (ev = the token input)",
    );
    set_arg(&mut s, 0, 3, iv(1));
    run_bad(&s, 0);

    let mut stop_only = oco.armed();
    stop_only.tp = 0;
    run_bad(
        &with_name(
            condb_fill(f, &stop_only, 4 * TOK, 0, &stop_only.with_amount(6 * TOK), None),
            "NB6 limit leg requested on a stop-only order",
        ),
        0,
    );

    run_bad(&stop_fill(&oco, &Ev::ask(302_000_000), "NB7 buy-stop armed by a resting ASK fill (wrong side)"), 0);
    run_bad(
        &stop_fill(&oco, &good.with(|e| e.daa = NOW - 500), "NB8 wash print: the evidence bid was exposed only 500 DAA (< 600)"),
        0,
    );
    run_bad(&stop_fill(&oco, &good.with(|e| e.active_from = NOW as i64 - 599), "NB9 evidence bid active only R - 1 DAA"), 0);
    run_bad(&stop_fill(&oco, &good.with(|e| e.token = OTHER_COV), "NB10 evidence of another token"), 0);
    run_bad(
        &stop_fill(
            &oco,
            &good.with(|e| e.mode = EvMode::Forged),
            "NB11 look-alike evidence: a real bid's state under another template",
        ),
        0,
    );
    let mut o6 = oco.clone();
    o6.min_touch = 6 * TOK;
    run_bad(&stop_fill(&o6, &good, "NB12 evidence fill of 5000 base units < minTouch 6000"), 0);
    run_bad(&stop_fill(&oco, &Ev::bid(299_999_999), "NB13 evidence quote below the buy-stop"), 0);

    let small = oco.with_amount(4 * TOK);
    run_bad(&with_name(condb_fill(f, &small, 4 * TOK + 1, 0, &small, None), "NB14 n = 4001 > amountLeft = 4000"), 0);

    run_bad(&with_name(condb_fill(f, &oco, 4 * TOK, 0, &oco, None), "NB15 amountLeft not decremented (over-buying)"), 0);

    run_bad(
        &with_name(
            condb_fill(f, &oco, 4 * TOK, 0, &oco.with_amount(6 * TOK).armed(), None),
            "NB16 limit-leg fill sneaks armed=1 into the continuation",
        ),
        0,
    );

    let mut s = with_name(base(), "NB17 two UTXOs share the covenant id (double fill attempt)");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(&s, 0);

    let thief = pk(&f.taker);
    let mut s = with_name(base(), "NB18 delivery to a non-maker key (id_type 3)");
    s.outputs[0].script_public_key = tspk(&n.k, 4 * TOK, &thief, T_ADDR);
    set_leader_next(&mut s, 1, vec![tok_state(4 * TOK, &thief, T_ADDR)]);
    run_bad(&s, 0);

    run_bad(&with_name(condb_update(f, &oco.armed(), &oco.armed(), &good, 0), "NB19 update on an already armed order"), 0);

    let tr = trailing_b(m);
    let mut tr2 = tr.clone();
    tr2.stop -= 2 * tr.step; // the ask fill at 2.80 justifies two steps
    run_bad(&with_name(condb_update(f, &tr, &tr2, &trail_ev(), tr.wait as u64 - 1), "NB20 trail sooner than trailWait (CSV)"), 0);
    let mut tr1 = tr.clone();
    tr1.stop -= tr.step;
    run_bad(
        &with_name(condb_update(f, &tr, &tr1, &trail_ev(), tr.wait as u64), "NB21 trail by one step when the evidence justifies two"),
        0,
    );
    let mut tr3 = tr.clone();
    tr3.stop -= 3 * tr.step;
    run_bad(
        &with_name(condb_update(f, &tr, &tr3, &trail_ev(), tr.wait as u64), "NB21b trail by three steps (one more than justified)"),
        0,
    );
    let mut low = tr.clone();
    low.stop = 205_000_000;
    let mut low2 = low.clone();
    low2.stop = 200_000_000;
    let dn = Ev::ask(150_000_000);
    run_bad(&with_name(condb_update(f, &low, &low2, &dn, tr.wait as u64), "NB22 trail down to the limit leg (travel cap)"), 0);
    let dn = Ev::ask(285_000_001);
    run_bad(&with_name(condb_update(f, &tr, &tr2, &dn, tr.wait as u64), "NB23 trail evidence above stop-step-gap"), 0);
    let mut o2 = oco.clone();
    o2.stop -= 5_000_000;
    run_bad(&with_name(condb_update(f, &oco, &o2, &trail_ev(), 600), "NB24 trail a non-trailing order"), 0);
    let mut s = with_name(condb_update(f, &oco, &oco.armed(), &good, 0), "NB25 arm update skims the escrow by 1 sompi");
    s.outputs[0].value -= 1;
    s.outputs[1].value += 1;
    run_bad(&s, 0);

    let mut wide = oco.armed();
    wide.slip_bps = 10_001;
    run_bad(&with_name(condb_fill(f, &wide, 4 * TOK, 1, &wide.with_amount(6 * TOK), None), "NB26 stop band above 100%"), 0);

    // sell-first IFO entry with partial fills (each fill = one fresh exit)
    let ip = ifda_p(f);
    let taker = pk(&f.taker);
    let last = |s: &Scn| s.outputs.len() - 1;

    let mut s = with_name(b7(f), "NI1 over-delivery: exit amountLeft 5000 for a 4000 fill");
    s.outputs[0].script_public_key = spk_of(&condb(n, &ip.exit.with_amount(5 * TOK)));
    regen(&mut s, 0, 0);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI2 exit state tampered (buy-stop removed)");
    let mut e = ip.exit.with_amount(4 * TOK);
    e.stop = 0;
    s.outputs[0].script_public_key = spk_of(&condb(n, &e));
    regen(&mut s, 0, 0);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI2b exit state tampered (band 3% -> 10%)");
    let mut e = ip.exit.with_amount(4 * TOK);
    e.slip_bps = 1_000;
    s.outputs[0].script_public_key = spk_of(&condb(n, &e));
    regen(&mut s, 0, 0);
    run_bad(&s, 0);

    // Double exit: a second exit output in the SAME genesis group (one id for both). Consensus
    // accepts the two-output group; the entry's recomputed one-output id no longer matches.
    let mut s = with_name(b7(f), "NI3 double exit: two outputs share the exit's genesis id");
    let dup = s.outputs[0].clone();
    s.outputs.push(dup);
    let l = last(&s);
    s.outputs[l - 1].value -= s.outputs[l].value;
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes([1; 32]), index: 0 };
    let id = covenant_id(op, [(0u32, &s.outputs[0]), (l as u32, &s.outputs[l])].into_iter());
    for k in [0, l] {
        s.outputs[k].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: id });
    }
    run_bad(&s, 0);

    // Aliasing: the "exit" is a genesis authorised by ANOTHER input (the taker's), as another
    // entry's exit would be. The entry's recomputed id (its own outpoint) does not match.
    let mut s = with_name(b7(f), "NI3b exit genesis authorised by another input (claimed by a second entry)");
    regen(&mut s, 0, 2);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI4 exit reuses the entry's covenant id");
    s.outputs[0].covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: cov(0xf1) });
    run_bad(&s, 0);

    // The attacker spends its own covenant Y (OpTrue script) and creates the "exit" as a
    // continuation of Y: tokens bought back later would be owned by an id the attacker can
    // co-spend. The entry authorises only its continuation, so it rejects.
    let mut s = with_name(b7(f), "NI5 exit authorised by an attacker covenant (continuation, not genesis)");
    let y = cov(0x99);
    let ev = s.outputs[0].value as i64;
    s.inputs.push(Inp { entry: utxo(ev, ScriptPublicKey::new(0, vec![OpTrue].into()), y, 500), role: Role::Raw, seq: 0 });
    s.outputs[0].covenant = Some(CovenantBinding { authorizing_input: 3, covenant_id: y });
    let l = last(&s);
    s.outputs[l].value += ev as u64;
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI6 prefund skimmed: exit short by 1 sompi");
    s.outputs[0].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    regen(&mut s, 0, 0);
    run_bad(&s, 0);

    let mut s = with_name(b7b(f), "NI7 sold out below the all-in price: last exit short by 1 sompi");
    s.outputs[0].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    regen(&mut s, 0, 0);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI8 entry continuation short by 1 sompi");
    s.outputs[1].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI9 token remainder mis-owned (sent to the taker)");
    s.outputs[2].script_public_key = tspk(&n.k, 6 * TOK, &taker, T_ADDR);
    set_leader_next(&mut s, 1, vec![tok_state(6 * TOK, &taker, T_ADDR), tok_state(4 * TOK, &taker, T_ADDR)]);
    run_bad(&s, 0);

    // (the KCC-20 suite uses the KobCondAsk template as the foreign one; the KRON port uses KobAskKron)
    let mut s = with_name(b7(f), "NI10 exit built from another template (KobAskKron)");
    set_arg(&mut s, 0, 4, Arg::V(bytes(&n.ask_tpl.pre)));
    set_arg(&mut s, 0, 5, Arg::V(bytes(&n.ask_tpl.suf)));
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI11 two UTXOs share the if-done covenant id");
    let dup = s.inputs[0].clone();
    s.inputs.push(dup);
    run_bad(&s, 0);

    let mut bad = ifda_p(f);
    bad.tip = -1;
    run_bad(&with_name(ifda_fill(f, &bad, 10 * TOK, 4 * TOK, IFDA_VALUE), "NI12 if-done ask with a negative tip is unfillable"), 0);

    let mut s = with_name(b7(f), "NI13 malleated n = 5000 (outputs for 4000)");
    set_arg(&mut s, 0, 0, nb(5 * TOK));
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "NI14 continuation dropped while tokens remain (exit only)");
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
    s.outputs[l].value += (IFDA_VALUE - quote(4 * TOK, ip.prefund, UP) - ip.exit_carrier) as u64;
    run_bad(&s, 0);

    // ------------------------------------------------------------ KRON token attacks (v1 adapter suite A/B/C
    // classes applied to the buy-side contracts). Guard: the order input (0) unless the token program (1) is named.

    // -- KobCondBidKron delivery (output 0, positional): only a genuine non-minter id_type 3 UTXO of n base units owned by the maker
    let deliver = |name: &str, st: KS| {
        let mut s = with_name(base(), name);
        s.outputs[0].script_public_key = n.k.spk(&st);
        set_leader_next(&mut s, 1, vec![st]);
        s
    };
    run_bad(&deliver("KB-A1 delivery with id_type 0 (pubkey)", KS { typ: T_PUBKEY, ..tok_state(4 * TOK, &m, T_ADDR) }), 0);
    run_bad(&deliver("KB-A2 delivery with is_minter = 1", KS { minter: 1, ..tok_state(4 * TOK, &m, T_ADDR) }), 0);
    run_bad(&deliver("KB-A3 delivery with id_type 2 (covenant id) owned by the maker key", tok_state(4 * TOK, &m, T_COVID)), 0);
    run_bad(&deliver("KB-A4 delivery id_type 1 (script hash)", KS { typ: 1, ..tok_state(4 * TOK, &m, T_ADDR) }), 0);
    run_bad(&deliver("KB-A5 delivery 1 base unit short", tok_state(4 * TOK - 1, &m, T_ADDR)), 0);
    // minted +1 in the delivery: the order pins the amount; the token program also rejects (conservation)
    run_bad(&deliver("KB-A6 delivery minted +1 base unit", tok_state(4 * TOK + 1, &m, T_ADDR)), 0);
    // the token program itself: mint in an output the order does not police
    let mut s = with_name(base(), "KB-P1 token program: extra token output minting +1 base unit is rejected by the token input");
    let extra = tok_state(1, &pk(&f.taker), T_ADDR);
    s.outputs.insert(1, out(CARRIER, n.k.spk(&extra), Some((1, TOKEN_COV))));
    s.next.push(extra);
    let l = last(&s);
    s.outputs[l].value -= CARRIER as u64;
    run_bad(&s, 1);
    // witness of the taker's id_type 3 token pointing at a non-P2PK input (the order)
    let mut s = with_name(base(), "KB-P2 token program: witness points at a non-owner input");
    if let Role::Tok { wit, .. } = &mut s.inputs[1].role {
        *wit = Wit::At(0);
    }
    run_bad(&s, 1);
    // 5 token inputs (KRON max is 4): the token program refuses (input 1) and so does the order's stray scan (input 0)
    let s5 = with_name(
        split_token_inputs(f, condb_fill(f, &oco, 5 * TOK, 0, &oco.with_amount(5 * TOK), None), 4),
        "KB-P3 token program: 5 token inputs rejected (KRON max 4)",
    );
    run_bad(&s5, 1);
    run_bad(&with_name(s5, "KB-P3b 5 token inputs: the order's stray scan bound (MAX_TOK_IN = 4) rejects too"), 0);
    // lookalike token: right template, wrong covenant id (order pins tokenCovId)
    let mut s = with_name(base(), "KB-A7 fake covenant id: taker's tokens of a lookalike token with the right template");
    retag(&mut s, FAKE_COV);
    run_bad(&s, 0);
    // right covenant id, the OTHER pinned template (template-hash pin)
    let mut s = with_name(base(), "KB-A8 right covenant id under the other pinned KRON template (template-hash pin)");
    retemplate(&mut s, &n.other);
    run_bad(&s, 0);
    // delivery slot aliasing: two orders (two covenant ids) at inputs 0 and 1, but only ONE delivery output
    // (index 0) for both: positional rule rejects the second
    let other = condb(n, &CondBP::oco(pk(&f.maker_c)));
    let mut s = with_name(base(), "KB-A9 delivery aliasing: two orders served by the single delivery output");
    s.inputs.insert(
        1,
        call(&other, "settle", vec![nb(4 * TOK), iv(2), iv(0), iv(0), iv(0)], "condb.settle", condb_value(&oco), cov(0xe2), 2_000),
    );
    // token input moved to index 2; witness auto-resolves; the second order expects its delivery at output 1
    set_arg(&mut s, 0, 1, iv(2));
    for o in s.outputs.iter_mut() {
        if let Some(b) = o.covenant.as_mut() {
            if b.covenant_id == TOKEN_COV {
                b.authorizing_input = 2;
            } else if b.authorizing_input >= 1 {
                b.authorizing_input += 1;
            }
        }
    }
    run_bad(&s, 1);
    // cancel / refund guards of the conditional buy
    run_bad(
        &with_name(condb_exit(f, "cancel", EXPIRY, 0, 0x81, f.maker_b), "X-NC1 conditional buy cancel with SIGHASH_ALL|ANYONECANPAY"),
        0,
    );
    run_bad(&with_name(condb_exit(f, "cancel", EXPIRY, 0, 0x01, f.maker_c), "X-NC2 conditional buy cancel with the wrong key"), 0);
    run_bad(
        &with_name(
            fresh(condb_exit(f, "refund", EXPIRY, EXPIRY as u64 - 1, 0x01, f.maker_b)),
            "X-NC3 conditional buy refund one DAA before expiry",
        ),
        0,
    );
    let mut s = fresh(condb_exit(f, "refund", EXPIRY, EXPIRY as u64, 0x01, f.maker_b));
    s.outputs[0].value -= 1;
    run_bad(&with_name(s, "X-NC6 conditional buy refund keeper takes more than refundTip"), 0);
    let mut s = fresh(condb_exit(f, "refund", EXPIRY, EXPIRY as u64, 0x01, f.maker_b));
    s.outputs[0].script_public_key = p2pk_spk(&pk(&f.taker));
    run_bad(&with_name(s, "X-NC7 conditional buy refund redirected to a non-maker key"), 0);
    run_bad(
        &with_name(
            condb_exit(f, "refund", NO_EXPIRY, (1_000 + MAX_IDLE) as u64 - 1, 0x01, f.maker_b),
            "X-NC4 conditional buy refund one DAA before the 90-day idle bound",
        ),
        0,
    );

    // -- KobIfdAskKron custody / remainder / refund (output 2 = token remainder; taker token output 3)
    let ifd_rest = |name: &str, st: KS| {
        let mut s = with_name(b7(f), name);
        s.outputs[2].script_public_key = n.k.spk(&st);
        let mut next = s.next.clone();
        next[0] = st;
        set_leader_next(&mut s, 1, next);
        s
    };
    let i_id = cov(0xf1).as_bytes();
    run_bad(
        &ifd_rest(
            "KI-A1 remainder with id_type 0 (pubkey) owned by the order id",
            KS { typ: T_PUBKEY, ..tok_state(6 * TOK, &i_id, T_COVID) },
        ),
        0,
    );
    run_bad(&ifd_rest("KI-A2 remainder with is_minter = 1", KS { minter: 1, ..tok_state(6 * TOK, &i_id, T_COVID) }), 0);
    run_bad(
        &ifd_rest(
            "KI-A3 remainder in id_type 3 (presence) owned by the order id: custody type must be 2",
            tok_state(6 * TOK, &i_id, T_ADDR),
        ),
        0,
    );
    run_bad(&ifd_rest("KI-A4 remainder owned by another covenant id", tok_state(6 * TOK, &cov(0xf2).as_bytes(), T_COVID)), 0);
    run_bad(&ifd_rest("KI-A5 remainder 1 base unit short", tok_state(6 * TOK - 1, &i_id, T_COVID)), 0);

    let mut s = with_name(b7(f), "KI-A6 token input owned by the taker (not by the order)");
    s.inputs[1] = tok_in(&n.k, CARRIER, 10 * TOK, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-A7 token input owned by another covenant id");
    s.inputs[1] = tok_in(&n.k, CARRIER, 10 * TOK, &cov(0xf2).as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-A8 fake covenant id: custody token of a lookalike token with the right template");
    retag(&mut s, FAKE_COV);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-A9 right covenant id under the other pinned KRON template (template-hash pin)");
    retemplate(&mut s, &n.other);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-A10 token-change carrier skimmed by 1 sompi");
    s.outputs[2].value -= 1;
    let l = last(&s);
    s.outputs[l].value += 1;
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-A11 malleated n = 3000 (outputs for 4000)");
    set_arg(&mut s, 0, 0, nb(3 * TOK));
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-A12 remainder one base unit short, the unit moved to the taker (token conservation holds)");
    let mut next = s.next.clone();
    next[0].amount = 6 * TOK - 1;
    next[1].amount = 4 * TOK + 1;
    s.outputs[2].script_public_key = n.k.spk(&next[0]);
    s.outputs[3].script_public_key = n.k.spk(&next[1]);
    set_leader_next(&mut s, 1, next);
    run_bad(&s, 0);

    let mut s = with_name(b7(f), "KI-P1 token program: taker output minting +1 base unit is rejected by the custody token input");
    let mut next = s.next.clone();
    next[1].amount += 1;
    s.outputs[3].script_public_key = n.k.spk(&next[1]);
    set_leader_next(&mut s, 1, next);
    run_bad(&s, 1);

    let mut s = with_name(b7(f), "KI-P2 token program: witness points at the taker's P2PK input, not the order");
    if let Role::Tok { wit, .. } = &mut s.inputs[1].role {
        *wit = Wit::At(2);
    }
    run_bad(&s, 1);

    let s5 = with_name(add_taker_tokens(f, b7(f), 4, 3), "KI-P3 token program: 5 token inputs rejected (KRON max 4)");
    run_bad(&s5, 1);
    run_bad(&with_name(s5, "KI-P3b 5 token inputs: the order's stray scan bound (MAX_TOK_IN = 4) rejects too"), 0);

    // v2.2 KNOWN property "a SECOND token UTXO owned by the order id is authorised by the same order input and
    // spendable by the filler" (KI-INFO) is closed by the v2.3 custody rule: NS13 / NS14 below reject it.

    // refund guards: tokens go back to the maker as a genuine non-minter id_type 3 UTXO, only when expired
    let refund_to = |name: &str, st: KS| {
        let mut s = fresh(with_name(ifda_refund(f, EXPIRY, EXPIRY as u64), name));
        s.outputs[0].script_public_key = n.k.spk(&st);
        set_leader_next(&mut s, 1, vec![st]);
        s
    };
    let mk = ifda_p(f).maker;
    run_bad(&refund_to("KI-R1 refund tokens to the taker (id_type 3)", tok_state(10 * TOK, &taker, T_ADDR)), 0);
    run_bad(&refund_to("KI-R2 refund with id_type 0", KS { typ: T_PUBKEY, ..tok_state(10 * TOK, &mk, T_ADDR) }), 0);
    run_bad(&refund_to("KI-R3 refund with is_minter = 1", KS { minter: 1, ..tok_state(10 * TOK, &mk, T_ADDR) }), 0);
    run_bad(&refund_to("KI-R4 refund keeps the tokens in custody (id_type 2, order id)", tok_state(10 * TOK, &i_id, T_COVID)), 0);
    run_bad(&refund_to("KI-R5 refund 1 base unit short", tok_state(10 * TOK - 1, &mk, T_ADDR)), 0);
    run_bad(&fresh(with_name(ifda_refund(f, EXPIRY, EXPIRY as u64 - 1), "KI-R6 refund one DAA before expiry")), 0);
    run_bad(
        &with_name(ifda_refund(f, NO_EXPIRY, (1_000 + MAX_IDLE) as u64 - 1), "KI-R7 refund one DAA before the 90-day idle bound"),
        0,
    );
    let mut s = fresh(with_name(ifda_refund(f, EXPIRY, EXPIRY as u64), "KI-R8 refund keeper takes more than refundTip"));
    s.outputs[0].value -= 1;
    run_bad(&s, 0);
    // cancel guards
    run_bad(&with_name(ifda_cancel(f, 0x81, f.maker_a), "KI-C1 if-done cancel with SIGHASH_ALL|ANYONECANPAY"), 0);
    run_bad(&with_name(ifda_cancel(f, 0x01, f.maker_c), "KI-C2 if-done cancel with the wrong key"), 0);
}

// ---------------------------------------------------------------- buy-side lifecycle

#[test]
fn kron_v2_buy_lifecycle() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        buy_lifecycle(&fx(tpl));
    }
}
fn buy_lifecycle(f: &Fx) {
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
    let next2000 = armed_as(&a.with_amount(6 * TOK), 2_000);
    let fill = |cp: &CondBP, next: &CondBP, ev: Option<&Ev>, t: i64, q: i64, name: &str| {
        with_name(condb_fill_at(f, cp, 4 * TOK, 1, next, ev, t, q), name)
    };
    assert_eq!(at(150), 304_500_000);
    run_ok(&fill(
        &a1,
        &next2000,
        None,
        2_150,
        at(150),
        "B12 buy-stop auction 150/300 DAA after arming: 3.045, remainder keeps origin 2000",
    ));
    let a1900 = armed_as(&a, 1_900);
    run_ok(&fill(&a1900, &armed_as(&a.with_amount(6 * TOK), 1_900), None, 2_150, at(250), "B12b stored origin 1900: 3.075"));
    let r = arm_ev();
    run_ok(&fill(
        &a,
        &armed_as(&a.with_amount(6 * TOK), 1),
        Some(&r),
        0,
        stop,
        "B12c arm + fill in one tx (auction starts at the trigger): pays the stop 3.00",
    ));
    run_bad(&fill(&a1, &next2000, None, 2_150, a.stop_leg(), "NB27 buy-stop auction charged the band ceiling at 150/300 DAA"), 0);
    run_bad(&fill(&a1, &armed_as(&a.with_amount(6 * TOK), 1), None, 2_150, at(150), "NB28 continuation restarts the buy auction"), 0);
    run_bad(
        &fill(&a1, &next2000, None, 1_999, stop - stop / 10_000, "NB29 buy auction time before the arming origin (priced at t)"),
        0,
    );

    // Keeper tip on arm / trail.
    let mut kt = CondBP::oco(m);
    kt.keeper_tip = 1_000_000;
    run_ok(&with_name(condb_update_tip(f, &kt, &kt.armed(), &r, 0, 1_000_000), "B13 arming keeper paid keeperTip from the escrow"));
    run_bad(&with_name(condb_update_tip(f, &kt, &kt.armed(), &r, 0, 1_000_001), "NB30 arming keeper takes keeperTip + 1"), 0);

    // Multi-step trail down, capped above the limit leg (2.00).
    let tr = trailing_b(m);
    let dn = Ev::ask(150_000_000);
    let mut capped = tr.clone();
    capped.stop = 205_000_000;
    run_ok(&with_name(
        condb_update(f, &tr, &capped, &dn, tr.wait as u64),
        "B5c trail jump 3.00 -> 2.05 (28 steps justified, capped above the 2.00 limit leg)",
    ));
    // NB22b (the jump not capped above the limit leg) is kron_v2_buy_trail_cap_attack below, a test of its own: with the step
    // cap ablated B5c itself is no longer accepted, which would abort an ablation run before the attack.

    // Sell-stop if-done entry: stop 2.60, limit 2.50, armed by the fill of a resting ask quoting <= 2.60.
    let mut ip = ifda_p(f);
    ip.entry_stop = 260_000_000;
    let down = Ev::ask(255_000_000);
    run_ok(&with_name(
        ifda_fill_x(f, &ip, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, Some(&down), 0, 1),
        "B14 sell-stop IFO entry (stop 2.60, limit 2.50) armed inside the fill: 4/10 at the limit, continues armed",
    ));
    let mut ipa = ip.clone();
    ipa.band_daa = 1_000;
    ipa.armed = 1;
    run_ok(&with_name(
        ifda_fill_x(f, &ipa, 10 * TOK, 4 * TOK, IFDA_VALUE, 255_000_000, None, 1_500, 1_000),
        "B14b armed sell-stop entry auction 500/1000 DAA: sells at 2.55",
    ));
    // arm + fill in one transaction with an auction: the auction opens at the trigger (the entry stop 2.60)
    let ipu = IfdAP { band_daa: 1_000, ..ip.clone() };
    run_ok(&with_name(
        ifda_fill_x(f, &ipu, 10 * TOK, 4 * TOK, IFDA_VALUE, 260_000_000, Some(&down), 0, 1),
        "B14e arm + fill in one tx (auction starts at the trigger): sells at the stop 2.60",
    ));
    run_bad(
        &with_name(
            ifda_fill_x(f, &ipu, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, Some(&down), 0, 1),
            "NI18b arm + fill in one tx (auction) paid the limit 2.50 instead of the stop",
        ),
        0,
    );
    let mut ipk = ip.clone();
    ipk.keeper_tip = 1_000_000;
    run_ok(&with_name(
        ifda_update(f, &ipk, &IfdAP { armed: 1, ..ipk.clone() }, &down, 1_000_000, false),
        "B14c permissionless arm of a sell-stop entry, keeper paid",
    ));
    let mut ml = ifda_p(f);
    ml.min_fill = 3 * TOK;
    run_ok(&with_name(
        ifda_fill_x(f, &ml, 10 * TOK, 3 * TOK, IFDA_VALUE, P250, None, 0, 0),
        "B14d minFill 3000: a 3000 base unit partial fill",
    ));

    run_bad(
        &with_name(
            ifda_fill_x(f, &ip, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, None, 0, 1),
            "NI15 sell-stop entry filled without evidence",
        ),
        0,
    );
    let side2 = Ev::bid(255_000_000);
    run_bad(
        &with_name(
            ifda_fill_x(f, &ip, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, Some(&side2), 0, 1),
            "NI16 sell-stop entry armed by a resting-BID fill (wrong side)",
        ),
        0,
    );
    let high = Ev::ask(260_000_001);
    run_bad(
        &with_name(
            ifda_fill_x(f, &ip, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, Some(&high), 0, 1),
            "NI17 evidence ask quoting above the entry stop",
        ),
        0,
    );
    run_bad(
        &with_name(
            ifda_fill_x(f, &ipa, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, None, 1_500, 1_000),
            "NI18 auction entry paid its limit 2.50 at 500/1000 DAA (exit short)",
        ),
        0,
    );
    run_bad(
        &with_name(
            ifda_fill_x(f, &ipa, 10 * TOK, 4 * TOK, IFDA_VALUE, 255_000_000, None, 1_500, 1),
            "NI19 continuation restarts the entry auction",
        ),
        0,
    );
    run_bad(
        &with_name(
            ifda_fill_x(f, &ml, 10 * TOK, 3 * TOK - 1, IFDA_VALUE, P250, None, 0, 0),
            "NI20 partial fill one base unit below minFill",
        ),
        0,
    );
    let mut low = ip.clone();
    low.entry_stop = 240_000_000;
    let dlow = Ev::ask(230_000_000);
    run_bad(
        &with_name(
            ifda_fill_x(f, &low, 10 * TOK, 4 * TOK, IFDA_VALUE, P250, Some(&dlow), 0, 1),
            "NI21 sell-stop entry with its stop below its limit is unfillable",
        ),
        0,
    );
    run_bad(
        &with_name(
            ifda_update(f, &ipk, &IfdAP { armed: 1, ..ipk.clone() }, &down, 0, true),
            "NI22 arm update also spends the entry's token custody",
        ),
        0,
    );
    run_bad(
        &with_name(
            ifda_update(f, &ipk, &IfdAP { armed: 1, ..ipk.clone() }, &down, 1_000_001, false),
            "NI23 arming keeper takes keeperTip + 1",
        ),
        0,
    );

    // Strays.
    let k = &n.k;
    let e = cov(0xe1);
    let oco = CondBP::oco(m);
    let mut s = with_name(
        condb_fill(f, &oco, 4 * TOK, 0, &oco.with_amount(6 * TOK), None),
        "NS10 conditional-buy fill co-spends a 3-token stray owned by it",
    );
    s.inputs.push(owned_tok(f, 3 * TOK, e));
    set_leader_next(&mut s, 1, vec![tok_state(4 * TOK, &m, T_ADDR), tok_state(3 * TOK, &taker, T_ADDR)]);
    s.outputs.push(out(CARRIER, tspk(k, 3 * TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    run_bad(&s, 0);
    run_ok(&with_name(condb_refund(f, &oco), "B15 anyone refunds a conditional buy at its soft expiry"));
    let mut s = with_name(condb_refund(f, &oco), "NS11 conditional-buy refund co-spends a stray owned by it");
    s.inputs.push(owned_tok(f, 3 * TOK, e));
    s.next = vec![tok_state(3 * TOK, &taker, T_ADDR)];
    s.outputs.push(out(CARRIER, tspk(k, 3 * TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    run_bad(&s, 0);
    let mut s = with_name(condb_update(f, &oco, &oco.armed(), &r, 0), "NS12 conditional-buy arm co-spends a stray owned by it");
    let st = vec![tok_state(3 * TOK, &taker, T_ADDR)];
    let l = push_tok(f, &mut s, CARRIER, 3 * TOK, &e.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_500, st);
    s.outputs.push(out(CARRIER, tspk(k, 3 * TOK, &taker, T_ADDR), Some((l as u16, TOKEN_COV))));
    run_bad(&s, 0);

    // Sell-first if-done entry (cov 0xf1, 10-token custody): dust stand-in and stray co-spend.
    let i = cov(0xf1);
    let mut s = with_name(b7(f), "NS13 if-done ask fill co-spends a 3-token stray owned by the entry");
    s.inputs.push(owned_tok(f, 3 * TOK, i));
    set_leader_next(&mut s, 1, vec![tok_state(6 * TOK, &i.as_bytes(), T_COVID), tok_state(7 * TOK, &taker, T_ADDR)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = tspk(k, 7 * TOK, &taker, T_ADDR);
    s.outputs.push(out(CARRIER, p2pk_spk(&taker), None));
    run_bad(&s, 0);
    let mut s = with_name(b7(f), "NS14 if-done ask fill co-spends a second 10-token UTXO owned by the entry (a top-up)");
    s.inputs.push(owned_tok(f, 10 * TOK, i));
    set_leader_next(&mut s, 1, vec![tok_state(6 * TOK, &i.as_bytes(), T_COVID), tok_state(14 * TOK, &taker, T_ADDR)]);
    let ti = s.outputs.len() - 2;
    s.outputs[ti].script_public_key = tspk(k, 14 * TOK, &taker, T_ADDR);
    s.outputs.push(out(CARRIER, p2pk_spk(&taker), None));
    run_bad(&s, 0);
}

#[test]
fn kron_v2_buy_trail_cap_attack() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let tr = trailing_b(pk(&f.maker_b));
        let dn = Ev::ask(150_000_000);
        let mut over = tr.clone();
        over.stop = 160_000_000;
        run_bad(
            &with_name(condb_update(&f, &tr, &over, &dn, tr.wait as u64), "NB22b trail jump not capped (stop 1.60 <= limit leg)"),
            0,
        );
    }
}

#[test]
fn kron_v2_buy_lifecycle_dust_custody() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        buy_dust_custody(&fx(tpl));
    }
}
fn buy_dust_custody(f: &Fx) {
    let n = &f.net;
    // The entry holds 10 whole tokens (amountLeft 10000); the attacker presents a 1-token dust UTXO owned by the
    // entry as its custody and sells it out, terminating the entry and orphaning the 10 tokens.
    let real = ifda(n, &IfdAP { amount: 10 * TOK, ..ifda_p(f) });
    let mut s = with_name(
        ifda_fill_x(f, &ifda_p(f), TOK, TOK, IFDA_VALUE, P250, None, 0, 0),
        "NS15 if-done ask: 1-token dust sold out to terminate a 10-token entry (orphaning its custody)",
    );
    if let Role::Call { art, .. } = &mut s.inputs[0].role {
        *art = real.clone();
    }
    s.inputs[0].entry = utxo(IFDA_VALUE, spk_of(&real), cov(0xf1), 1_000);
    run_bad(&s, 0);

    // NS15b (KRON addition, single lever): the custody swap. The entry (10 tokens) sells 1; the attacker presents a
    // 1-token dust UTXO owned by the entry as its custody and supplies the other 9 tokens from its own, so token
    // conservation holds and the entry continues with a 9-token custody: the real 10-token custody is left behind as an
    // orphan. Only the exact-amount check (custody amount == amountLeft) rejects it (NS15 above is also rejected
    // by the missing continuation, so it does not isolate that check).
    let mut s = with_name(
        ifda_fill_x(f, &ifda_p(f), 10 * TOK, TOK, IFDA_VALUE, P250, None, 0, 0),
        "NS15b if-done ask: custody swap, a 1-token dust UTXO plus 9 attacker tokens replace the 10-token custody",
    );
    let taker = pk(&f.taker);
    s.inputs[1] = owned_tok(f, TOK, cov(0xf1));
    s.inputs.push(tok_in(&n.k, CARRIER, 9 * TOK, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000));
    let last = s.outputs.len() - 1;
    s.outputs[last].value += CARRIER as u64;
    run_bad(&s, 0);
}

// ---------------------------------------------------------------- touch trigger battery

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
/// The sell-stop entry of the IfdAsk triggers: stop 2.60, limit 2.50, 10 whole tokens.
fn stop_ifda(f: &Fx, min_touch: i64) -> IfdAP {
    IfdAP { entry_stop: 260_000_000, min_touch, ..ifda_p(f) }
}
/// The stop order of `kind` (input 0) triggered by the evidence `ev`; `min_touch`: its minTouch (base units).
fn trig(f: &Fx, kind: Kind, ev: &Ev, min_touch: i64) -> Scn {
    let m = pk(&f.maker_b);
    match kind {
        Kind::Settle => {
            let cp = CondBP { min_touch, ..CondBP::oco(m) };
            condb_fill(f, &cp, 4 * TOK, 1, &cp.with_amount(6 * TOK).armed(), Some(ev))
        }
        Kind::Arm => {
            let cp = CondBP { min_touch, ..CondBP::oco(m) };
            condb_update(f, &cp, &cp.armed(), ev, 0)
        }
        Kind::Trail => {
            let tr = CondBP { min_touch, ..trailing_b(m) };
            let k = ((tr.stop - tr.gap - ev.price) / tr.step).min((tr.stop - tr.tp - 1) / tr.step);
            condb_update(f, &tr, &CondBP { stop: tr.stop - k * tr.step, ..tr.clone() }, ev, tr.wait as u64)
        }
        Kind::IfdFill => ifda_fill_x(f, &stop_ifda(f, min_touch), 10 * TOK, 4 * TOK, IFDA_VALUE, P250, Some(ev), 0, 1),
        Kind::IfdArm => {
            let ip = stop_ifda(f, min_touch);
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
            let ip = IfdAP { maker: m, ..stop_ifda(f, TOK) };
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
        (condb_fill(f, &cp, 4 * TOK, 0, &cp.with_amount(6 * TOK), None), "a KobCondBid limit-leg fill as evidence")
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
/// The unarmed buy stop (OCO, stop 3.00) buys 4 whole tokens from a resting ask quoting 3.00 (its counterparty:
/// the ask's custody is the token input, the ask itself a later input) and names that ask as its evidence.
fn condb_from_ask(f: &Fx) -> Scn {
    let a = cov(0x9a);
    let ap = AskP { amount: 4 * TOK, ..AskP::new(pk(&f.maker_c), 300_000_000) };
    let cp = CondBP::oco(pk(&f.maker_b));
    let mut s = condb_fill(f, &cp, 4 * TOK, 1, &cp.with_amount(6 * TOK).armed(), None);
    // the order's tokens come from the ask's custody instead of the taker
    s.inputs[1] = tok_in(&f.net.k, CARRIER, 4 * TOK, &a.as_bytes(), SCHEME_COVID, TOKEN_COV, Wit::CovId, 1_000);
    let pay = ask_all_in(4 * TOK, ap.price, TIP);
    s.inputs.push(p2pk_in(&f.taker, pay + CARRIER));
    while s.inputs.len() < s.outputs.len() {
        s.inputs.push(p2pk_in(&f.taker, 0));
    }
    while s.outputs.len() < s.inputs.len() {
        s.outputs.push(out(0, p2pk_spk(&pk(&f.taker)), None));
    }
    let at = s.inputs.len();
    s.inputs.push(call(&f.net.ask(&ap), "settle", vec![nb(4 * TOK), iv(1), iv(0), iv(0)], "ask.settle", CARRIER, a, 1_000));
    s.outputs.push(out(pay + 2 * CARRIER, p2pk_spk(&ap.maker), None));
    set_arg(&mut s, 0, 3, iv(at as i64));
    s
}
/// The unarmed sell-stop entry (stop 2.60, limit 2.50) sells 4 whole tokens straight into a resting bid quoting 2.55
/// (its counterparty, input 3, whose positional delivery is output 3) and names that bid as its evidence.
fn ifda_into_bid(f: &Fx) -> Scn {
    let k = &f.net.k;
    let bp = BidP::new(pk(&f.maker_c), 255_000_000);
    let v = bp.budget(4 * TOK) + DC;
    let mut s = ifda_fill_x(f, &stop_ifda(f, TOK), 10 * TOK, 4 * TOK, IFDA_VALUE, P250, None, 0, 1);
    // outputs: [0] exit, [1] entry continuation, [2] token remainder, [3] the buyer's tokens, [4] taker KAS
    s.inputs.push(call(&f.net.bid(&bp), "fill", vec![nb(4 * TOK), iv(1), iv(0)], "bid.fill", v, cov(0x9b), 1_000));
    s.outputs[3] = out(v - bid_all_in(4 * TOK, bp.price, TIP), tspk(k, 4 * TOK, &bp.maker, SCHEME_P2PK), Some((1, TOKEN_COV)));
    s.next[1] = tok_state(4 * TOK, &bp.maker, SCHEME_P2PK);
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
/// accepted, one DAA short rejected), slope, side, token, scale, size threshold (minTouch base units), an evidence that
/// does not fill, wrong ev / tk indices, look-alike templates, the price rule at its edge, one evidence shared by
/// two orders, and the update-only rules (own tokens, other fill kinds). Input 0 is the order throughout.
fn touch_battery(f: &Fx, kind: Kind) {
    let n = &f.net;
    let tag = kind.tag();
    let good = kind.ev();
    let matcher = pk(&f.matcher);
    let (pe, pt) = kind.ev_pos();
    let name = |neg: bool, id: &str, what: &str| format!("{}{tag}{id} {what}", if neg { "N" } else { "" });
    let ok = |e: &Ev, mt: i64, id: &str, what: &str| {
        run_ok(&with_name(trig(f, kind, e, mt), &name(false, id, what)));
    };
    let bad = |e: &Ev, mt: i64, id: &str, what: &str| run_bad(&with_name(trig(f, kind, e, mt), &name(true, id, what)), 0);
    let bad_s = |s: Scn, id: &str, what: &str, at: usize| run_bad(&with_name(s, &name(true, id, what)), at);

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
        s.outputs.push(out(CARRIER, tspk(&n.k, 5 * TOK, &matcher, SCHEME_P2PK), Some((l as u16, OTHER_COV))));
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
        s.outputs.push(out(CARRIER, tspk(&n.k, 5 * TOK, &matcher, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        set_arg(&mut s, 0, pt, iv(at));
        bad_s(s, "07b", "evidence of another token; tk names a UTXO of this token owned by that ask", 0);
    }
    bad(
        &good.with(|e| e.scale = 10 * SCALE),
        TOK,
        "08",
        "evidence quoting per another scale (10^4 base units per whole token: prices not comparable)",
    );
    ok(&good, 5 * TOK, "09", "evidence of exactly minTouch (5000 base units, threshold 5000)");
    bad(&good.with(|e| e.n = 5 * TOK - 1), 5 * TOK, "09", "evidence one base unit below minTouch (4999, threshold 5000)");
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
    run_ok(&with_name(s, &name(false, "20", "two orders arm (trail) from ONE evidence fill in the same transaction")));
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
        s.outputs.push(out(CARRIER, tspk(&n.k, 3 * TOK, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        bad_s(s, "23", "update next to the evidence also spends a token UTXO owned by the order (to the attacker)", 0);
        let mut s = base.clone();
        let st = vec![tok_state(3 * TOK, &thief, SCHEME_P2PK)];
        let l = push_tok(f, &mut s, CARRIER, 3 * TOK, &thief, SCHEME_P2PK, TOKEN_COV, Wit::P2pk(f.taker), 1_000, st);
        s.outputs.push(out(CARRIER, tspk(&n.k, 3 * TOK, &thief, SCHEME_P2PK), Some((l as u16, TOKEN_COV))));
        // the taker's own P2PK input (KRON id_type 3 authorises by its presence)
        s.inputs.push(p2pk_in(&f.taker, KAS));
        s.outputs.push(out(KAS, p2pk_spk(&thief), None));
        run_ok(&with_name(
            s,
            &name(false, "23", "update next to the evidence and another foreign token input (the taker's own tokens)"),
        ));
    }
    if kind == Kind::IfdArm {
        // update arms only an entry that has tokens to fill and its stop on the limit's side (else the arm only takes keeperTip)
        let ip = stop_ifda(f, TOK);
        let empty = IfdAP { amount: 0, rpt: 9 * TOK, ..ip.clone() };
        let s = ifda_update(f, &empty, &IfdAP { armed: 1, ..empty.clone() }, &good, 0, false);
        bad_s(s, "24", "update arms a repeating entry with nothing left (it is all in its exits)", 0);
        let inv = IfdAP { price: ip.entry_stop + 1, ..ip.clone() };
        let s = ifda_update(f, &inv, &IfdAP { armed: 1, ..inv.clone() }, &good, 0, false);
        bad_s(s, "25", "update arms an entry whose stop is below its limit (no fill can follow)", 0);
    }
}

/// Runs the battery on both pinned KRON templates.
fn both(kind: Kind) {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        touch_battery(&fx(tpl), kind);
    }
}
/// Touch trigger, KobCondBidKron buy-stop leg armed inside its own fill (settle, leg 1, bid evidence).
#[test]
fn kron_v2_buy_touch_cond_settle() {
    both(Kind::Settle);
}
/// Touch trigger, KobCondBidKron buy-stop leg armed by update next to a bid fill.
#[test]
fn kron_v2_buy_touch_cond_arm() {
    both(Kind::Arm);
}
/// Touch trigger, KobCondBidKron trailing ratchet down (update next to an ask fill).
#[test]
fn kron_v2_buy_touch_cond_trail() {
    both(Kind::Trail);
}
/// Touch trigger, KobIfdAskKron sell-stop entry armed inside its own fill (ask evidence).
#[test]
fn kron_v2_buy_touch_ifd_fill() {
    both(Kind::IfdFill);
}
/// Touch trigger, KobIfdAskKron sell-stop entry armed by update next to an ask fill.
#[test]
fn kron_v2_buy_touch_ifd_arm() {
    both(Kind::IfdArm);
}

/// Merge-value safety of update, buy family (protocol v3). Both sides of a repeat merge require the other input's
/// sigscript to START with 0x08 (the 8-byte push of the fill / merge argument) before they read bytes [1..9). The
/// encoder pushes update's index arguments minimally (an index is at most a 3-byte push), so no update sigscript the
/// encoder writes starts with 0x08: checked over every small index and the push-size boundaries. The control shows
/// why the 0x08 checks alone are not enough: pushes need not be minimal under covenants, and the engine decodes a
/// non-minimal 8-byte number, so an update whose ev is pushed as 0x08 || ev reads as the amount ev; KobCondBid.update
/// therefore refuses to run next to its repeat entry (OpCovInputCount(parent) == 0, attack NV3CB41), and KobIfdAsk.update
/// cannot alias an exit's merge claim at all (a merge argument is negative, a usable ev is not).
#[test]
fn kron_v2_buy_update_sigscript_is_never_a_merge_value() {
    let f = fx(TPL_2433);
    let n = &f.net;
    let condb_art = condb(n, &CondBP::oco(pk(&f.maker_b)));
    let ifda_art = ifda(n, &stop_ifda(&f, TOK));
    let head = |art: &SilAbiArtifact, args: &[ArtifactValue]| encode_entry_sig_script(art, "update", args).expect("encode update");
    let idx: Vec<i64> = (-1..=260).chain([1_000, 32_767, 32_768, 65_535, 65_536, 8_388_607]).collect();
    let mut checked = 0;
    for &ev in idx.iter().filter(|&&i| i >= 0) {
        for &tk in &idx {
            for (what, art) in [("KobIfdAsk", &ifda_art), ("KobCondBid", &condb_art)] {
                let ss = head(art, &[int(ev), int(tk)]);
                assert_ne!(ss[0], 0x08, "{what}.update({ev}, {tk}) starts with an 8-byte push");
                checked += 1;
            }
        }
    }
    // control: the same update with ev pushed non-minimally (0x08 || 8-byte number) starts with 0x08 and its bytes
    // [1..9) read as ev, the shape of a merge claim m = ev
    let ss = head(&condb_art, &[int(5), int(-1)]);
    let mut nm = vec![0x08];
    nm.extend(num8(5));
    nm.extend_from_slice(&ss[first_push_len(&ss)..]);
    assert_eq!((nm[0], i64::from_le_bytes(nm[1..9].try_into().unwrap())), (0x08, 5), "control: non-minimal ev reads as m = 5");
    println!("MERGE-VALUE {checked} update sigscripts checked, none starts with 0x08 (control: a non-minimal ev push does)");
}

// ---------------------------------------------------------------- repeat IFD, sell-first

/// Covenant ids of the repeating if-done ask (as in ifda_fill) and of its exits.
const RPTA_ENTRY: u8 = 0xf1;
const RPTA_EXIT: u8 = 0xe1;
/// Proceeds the maker is owed per whole token of a sell-first cycle (the entry's all-in at its limit).
fn sell_price(ip: &IfdAP) -> i64 {
    ip.price - ip.tip
}
fn rpta_p(f: &Fx, amount: i64, rpt: i64) -> IfdAP {
    IfdAP { amount, rpt, ..ifda_p(f) }
}
/// The exit a fill of n base units books from the entry at UTXO DAA 1000, cycle time t.
fn booked_exit_b(ip: &IfdAP, n: i64, t: i64) -> CondBP {
    CondBP {
        amount: n,
        parent: cov(RPTA_ENTRY).as_bytes(),
        rpt_price: sell_price(ip),
        rpt_pre: ip.prefund,
        rpt_until: EXPIRY.min(t.max(1_000) + MAX_IDLE),
        ..ip.exit.clone()
    }
}
/// Merge argument -(k * MERGE_K + m) as an 8-byte script number (sign-magnitude, little endian).
fn nb_merge(k: i64, m: i64) -> Arg {
    let mut b = (k * MERGE_K + m).to_le_bytes();
    b[7] |= 0x80;
    Arg::V(bytes(&b))
}
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
            value: CARRIER + quote(have, IFDA_PREFUND, UP) + (have / TOK) * IFDA_EXIT_CARRIER,
            rpt,
            t: 0,
            exit_rpt: None,
            next_rpt: None,
            terminate: false,
            cont_delta: 0,
        }
    }
}
/// A repeating if-done ask (cov 0xf1, `have` base units in custody) sells n base units at its limit to a
/// taker. Outputs: [0] the fresh KobCondBidKron exit, [1] the entry continuation (tokens left or
/// repeating), [2] the token remainder (tokens left), the taker's tokens, the taker's change.
fn rpta_fill(f: &Fx, k: &RaKnobs) -> Scn {
    let tk = &f.net.k;
    let i = cov(RPTA_ENTRY);
    let ip = rpta_p(f, k.have, k.rpt);
    let booked = k.rpt > k.n;
    let mut xp = if booked { booked_exit_b(&ip, k.n, k.t) } else { ip.exit.with_amount(k.n) };
    if let Some((p, l, pr, u)) = k.exit_rpt {
        xp.parent = p;
        xp.rpt_price = l;
        xp.rpt_pre = pr;
        xp.rpt_until = u;
    }
    let taker = pk(&f.taker);
    let proceeds = quote(k.n, ip.price - ip.tip, UP);
    let pre = quote(k.n, ip.prefund, UP);
    let rest = k.have - k.n;
    let cont_exists = (rest > 0 || k.rpt > 0) && !k.terminate;
    let mut next = vec![];
    if rest > 0 {
        next.push(tok_state(rest, &i.as_bytes(), T_COVID));
    }
    next.push(tok_state(k.n, &taker, T_ADDR));
    let mut outputs = vec![];
    if cont_exists {
        outputs.push(out(proceeds + pre + ip.exit_carrier, spk_of(&condb(&f.net, &xp)), None));
        let next_rpt = k.next_rpt.unwrap_or(if booked { k.rpt - k.n } else { k.rpt });
        let cont = ifda(&f.net, &IfdAP { amount: rest, rpt: next_rpt, ..ip.clone() });
        let absorbed = if rest == 0 { CARRIER } else { 0 };
        outputs.push(out(k.value - pre - ip.exit_carrier + absorbed + k.cont_delta, spk_of(&cont), Some((0, i))));
        if rest > 0 {
            outputs.push(out(CARRIER, tspk(tk, rest, &i.as_bytes(), T_COVID), Some((1, TOKEN_COV))));
        }
    } else {
        outputs.push(out(proceeds + k.value + CARRIER, spk_of(&condb(&f.net, &xp)), None));
    }
    outputs.push(out(CARRIER, tspk(tk, k.n, &taker, T_ADDR), Some((1, TOKEN_COV))));
    outputs.push(out(0, p2pk_spk(&taker), None));
    let mut s = Scn {
        name: "rpta fill".into(),
        inputs: vec![
            call(
                &ifda(&f.net, &ip),
                "settle",
                vec![
                    nb(k.n),
                    iv(1),
                    iv(if rest > 0 { 2 } else { 0 }),
                    iv(0),
                    Arg::V(bytes(&condb_tpl(&f.net).pre)),
                    Arg::V(bytes(&condb_tpl(&f.net).suf)),
                    iv(0),
                    iv(0),
                    iv(k.t),
                ],
                "ifda.settle",
                k.value,
                i,
                1_000,
            ),
            tok_in(tk, CARRIER, k.have, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs,
        lock_time: NOW,
        payload: vec![],
        next,
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
    exit_amount: i64,
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
    custody_amount: Option<i64>,
    custody_owner: Option<[u8; 32]>,
    lock_time: u64,
}
impl MergeBKnobs {
    fn new(f: &Fx, entry_amount: i64, exit_amount: i64, n: i64) -> Self {
        MergeBKnobs {
            entry: rpta_p(f, entry_amount, 21 * TOK),
            entry_value: CARRIER + quote(entry_amount, IFDA_PREFUND, UP) + (entry_amount / TOK) * IFDA_EXIT_CARRIER,
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
            exit_delta: 0,
            custody_delta: 0,
            custody_amount: None,
            custody_owner: None,
            lock_time: NOW,
        }
    }
}
/// A booked exit (KobCondBidKron, cov 0xe1, UTXO DAA 2000, funded at the entry's limit) buys back n
/// base units from a seller on `leg`; the entry (KobIfdAskKron, cov 0xf1) takes them into its custody.
/// Inputs: [0] exit, [1] the seller's tokens (id_type 3), [2] entry (merge), [3] the entry's
/// custody (when it has tokens), then the seller's P2PK input that authorises the seller's tokens. Outputs: [0]
/// maker profit (re-arm) or the delivery (plain), [1] exit continuation (tokens left), then the entry's custody, the
/// entry continuation and the seller's KAS.
fn rpta_merge(f: &Fx, k: &MergeBKnobs) -> Scn {
    let tk = &f.net.k;
    let m = pk(&f.maker_a);
    let c = cov(RPTA_EXIT);
    let i = cov(RPTA_ENTRY);
    let taker = pk(&f.taker);
    let ip = &k.entry;
    let xp = k.exit.clone().unwrap_or_else(|| booked_exit_b(ip, k.exit_amount, 0));
    let xp = CondBP { amount: k.exit_amount, ..xp };
    let x_value = quote(k.exit_amount, sell_price(ip) + ip.prefund, UP) + ip.exit_carrier;
    let spend = bid_all_in(k.n, xp.leg_price(k.leg), xp.tip);
    let rest = k.exit_amount - k.n;
    let rearm = k.with_entry && k.leg == 0 && xp.parent != [0; 32];
    let claim = k.claim_m.unwrap_or(k.n);
    let seller_amount = if rearm { k.n } else { k.n + if k.with_entry { claim } else { 0 } };
    let custody_amount = k.custody_amount.unwrap_or(ip.amount + claim);
    let custody_owner = k.custody_owner.unwrap_or(i.as_bytes());
    let custody_type = if k.custody_owner.is_some() { T_ADDR } else { T_COVID };
    let mut next = vec![];
    if !rearm {
        next.push(tok_state(k.n, &m, T_ADDR));
    }
    if k.with_entry {
        next.push(tok_state(custody_amount, &custody_owner, custody_type));
    }
    let mut s = Scn {
        name: "rpta merge".into(),
        inputs: vec![
            call(&condb(&f.net, &xp), "settle", vec![nb(k.n), iv(1), iv(k.leg), iv(0), iv(0)], "condb.settle", x_value, c, 2_000),
            tok_in(tk, CARRIER, seller_amount, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_500),
        ],
        outputs: vec![],
        lock_time: k.lock_time,
        payload: vec![],
        next,
    };
    if rearm {
        // the maker keeps the profit: the exit's proceeds of n (its rptPrice, rounded up) minus the buy-back spend
        s.outputs.push(out(quote(k.n, xp.rpt_price, UP) - spend + k.maker_delta, p2pk_spk(&m), None));
    } else {
        let v = if rest > 0 { DC } else { x_value - spend };
        s.outputs.push(out(v + k.maker_delta, tspk(tk, k.n, &m, T_ADDR), Some((1, TOKEN_COV))));
    }
    if rest > 0 {
        let keep = if rearm { x_value - quote(k.n, xp.rpt_price, UP) - quote(k.n, xp.rpt_pre, UP) } else { x_value - spend - DC };
        s.outputs.push(out(keep + k.exit_delta, spk_of(&condb(&f.net, &CondBP { amount: rest, ..xp.clone() })), Some((0, c))));
    }
    if k.with_entry {
        let entry_in = s.inputs.len();
        let custody_in = entry_in + 1;
        let custody_out = s.outputs.len();
        s.inputs.push(call(
            &ifda(&f.net, ip),
            "settle",
            vec![
                nb_merge(k.claim_k, claim),
                iv(if ip.amount > 0 { custody_in as i64 } else { 1 }),
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
        if ip.amount > 0 {
            s.inputs.push(tok_in(tk, CARRIER, ip.amount, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000));
        }
        let new_custody = ip.amount == 0;
        let custody_value = if new_custody { ip.exit_carrier } else { CARRIER };
        s.outputs.push(out(
            custody_value + k.custody_delta,
            tspk(tk, custody_amount, &custody_owner, custody_type),
            Some((1, TOKEN_COV)),
        ));
        // what the entry requires back: everything the exit held beyond the maker's proceeds when
        // its tokens run out, else the prefund of the re-armed amount (funded by the filler if the exit
        // does not pay it), both rounded up
        let back = if claim == k.exit_amount { x_value - quote(claim, sell_price(ip), UP) } else { quote(claim, ip.prefund, UP) };
        let back = back - if new_custody { ip.exit_carrier } else { 0 };
        // armed: reset with a new custody (none was left); an entry armed by update (1) with a band records its origin,
        // the entry UTXO's DAA (1_000), as a fill does
        let armed = if new_custody {
            0
        } else if ip.armed == 1 && ip.band_daa > 0 {
            1_000
        } else {
            ip.armed
        };
        let cont = k.cont.clone().unwrap_or(IfdAP { amount: ip.amount + claim, armed, ..ip.clone() });
        s.outputs.push(out(k.entry_value + back + k.cont_delta, spk_of(&ifda(&f.net, &cont)), Some((entry_in as u16, i))));
    }
    // the seller's P2PK input authorises its id_type 3 tokens; its KAS change is the balancing output
    s.inputs.push(p2pk_in(&f.taker, TK));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    s
}
/// A crafted P2SH input imitating a booked KobCondBidKron exit of `parent` (see the sell-side twin).
fn fake_exit_b(f: &Fx, parent: Hash, m: i64, value: i64) -> Inp {
    let tpl = condb_tpl(&f.net);
    let size = tpl.pre.len() + CONDB_STATE_LEN + tpl.suf.len();
    let data_len = size - 6;
    let mut data = vec![0u8; data_len];
    // state span = redeem[1..349); parent at state [289..321) = redeem [290..322) = data [287..319)
    data[287..319].copy_from_slice(&parent.as_bytes());
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

/// Repeat IFD, sell-first (KobIfdAskKron entry, KobCondBidKron exits): booking, the sold-out entry that
/// waits without custody, re-arming into an existing or a new custody, sell-out leftovers, the
/// rptUntil fallback, stop-loss exits, close, and a three-cycle ledger with exact accounting.
#[test]
fn kron_v2_buy_repeat_ifd_positive() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        buy_repeat_positive(&fx(tpl));
    }
}
fn buy_repeat_positive(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);

    run_ok(&with_name(
        rpta_fill(f, &RaKnobs::new(10 * TOK, 4 * TOK, 21 * TOK)),
        "RPB1 repeating sell-first IFO sells 4/10: booked exit (rptPrice, rptPre), rptAmount 21000 -> 17000",
    ));
    run_ok(&with_name(
        rpta_fill(f, &RaKnobs::new(6 * TOK, 6 * TOK, 17 * TOK)),
        "RPB2 repeating entry sold out 6/6: it stays with amountLeft 0 and no custody (keeps the custody carrier)",
    ));
    run_ok(&with_name(
        rpta_fill(f, &RaKnobs::new(10 * TOK, 4 * TOK, 4 * TOK)),
        "RPB3 last cycle: rptAmount 4000 cannot re-arm 4000, plain exit",
    ));
    run_ok(&with_name(
        rpta_fill(f, &RaKnobs { t: 7_000, ..RaKnobs::new(10 * TOK, 4 * TOK, 21 * TOK) }),
        "RPB3b the cycle is dated by t (CLTV)",
    ));
    run_ok(&with_name(
        rpta_merge(f, &MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK)),
        "RPB4 booked exit buys back 3/4 on its take-profit: 3000 join the entry's custody (6000 -> 9000) with 3000-worth prefund, maker keeps the profit",
    ));
    run_ok(&with_name(
        rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK)),
        "RPB5 booked exit buys back 4/4: a new 4000 custody for the empty entry, the exit's leftovers come back",
    ));
    let mut stop_ip = rpta_p(f, 0, 21 * TOK);
    stop_ip.entry_stop = 260_000_000;
    stop_ip.armed = 1;
    run_ok(&with_name(
        rpta_merge(f, &MergeBKnobs { entry: stop_ip.clone(), ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) }),
        "RPB5b an armed sell-stop entry with nothing left re-arms unarmed",
    ));
    let until = EXPIRY.min(1_000 + MAX_IDLE);
    run_ok(&with_name(
        rpta_merge(f, &MergeBKnobs { with_entry: false, lock_time: until as u64, ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) }),
        "RPB6 from rptUntil a booked exit may take profit without its entry (tokens to the maker)",
    ));
    let mut sl = booked_exit_b(&rpta_p(f, 0, 21 * TOK), 4 * TOK, 0);
    sl.armed = 1;
    run_ok(&with_name(
        rpta_merge(f, &MergeBKnobs { exit: Some(sl), leg: 1, with_entry: false, ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) }),
        "RPB7 stop-loss of a booked exit: plain delivery to the maker, no re-arm",
    ));
    // close: an empty repeating entry is refunded at its soft expiry.
    let e = rpta_p(f, 0, TOK);
    let s = Scn {
        name: "RPB8 close: an empty repeating entry (no custody) is refunded at its soft expiry".into(),
        inputs: vec![call(&ifda(n, &e), "close", vec![], "ifda.close", 2 * CARRIER, cov(RPTA_ENTRY), (EXPIRY - 1_000) as u64)],
        outputs: vec![out(2 * CARRIER - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time: EXPIRY as u64,
        payload: vec![],
        next: vec![],
    };
    run_ok(&s);

    // Three cycles of a 4-token position (rptAmount 9000), fills and take-profits at the limits, values
    // carried forward: the entry and its custody are restored exactly after every cycle.
    let ip0 = rpta_p(f, 4 * TOK, 9 * TOK);
    let v0 = CARRIER + quote(4 * TOK, ip0.prefund, UP) + 4 * ip0.exit_carrier;
    let spend4 = bid_all_in(4 * TOK, ip0.exit.tp, ip0.exit.tip);
    let profit4 = quote(4 * TOK, sell_price(&ip0), UP) - spend4;
    let mut v = v0;
    let mut rpt = 9 * TOK;
    let mut maker_total = 0;
    for cycle in 1..=3 {
        let booked = rpt > 4 * TOK;
        let s = rpta_fill(f, &RaKnobs { value: v, ..RaKnobs::new(4 * TOK, 4 * TOK, rpt) });
        run_ok(&with_name(
            s.clone(),
            &format!("RPB9 ledger cycle {cycle}: entry sells 4/4 (value {v}, rptAmount {rpt}, booked {booked})"),
        ));
        v = s.outputs[1].value as i64;
        assert_eq!(
            v,
            v0 - quote(4 * TOK, ip0.prefund, UP) - ip0.exit_carrier + CARRIER,
            "entry after its fill keeps the custody carrier"
        );
        let x_value = s.outputs[0].value as i64;
        assert_eq!(
            x_value,
            quote(4 * TOK, sell_price(&ip0) + ip0.prefund, UP) + ip0.exit_carrier,
            "exit funded with proceeds + prefund + carrier"
        );
        if booked {
            rpt -= 4 * TOK;
            let mut mk = MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK);
            mk.entry = rpta_p(f, 0, rpt);
            mk.entry_value = v;
            let s = rpta_merge(f, &mk);
            run_ok(&with_name(s.clone(), &format!("RPB9 ledger cycle {cycle}: take-profit buys back 4/4 and re-arms the entry")));
            let cont = s.outputs.iter().find(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPTA_ENTRY))).unwrap();
            let custody =
                s.outputs.iter().find(|o| o.script_public_key == tspk(&n.k, 4 * TOK, &cov(RPTA_ENTRY).as_bytes(), T_COVID)).unwrap();
            v = cont.value as i64;
            assert_eq!(v + custody.value as i64, v0 + CARRIER, "entry value + custody carrier restored after cycle {cycle}");
            assert_eq!(v, v0, "entry value restored exactly after cycle {cycle}");
            assert_eq!(s.outputs[0].value as i64, profit4, "maker profit of cycle {cycle}");
            maker_total += s.outputs[0].value as i64;
        } else {
            let mut mk = MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK);
            mk.exit = Some(ip0.exit.with_amount(4 * TOK));
            mk.with_entry = false;
            let s = rpta_merge(f, &mk);
            run_ok(&with_name(s.clone(), &format!("RPB9 ledger cycle {cycle}: plain take-profit, tokens back to the maker")));
            maker_total += s.outputs[0].value as i64;
        }
    }
    assert_eq!(
        maker_total,
        2 * profit4 + quote(4 * TOK, sell_price(&ip0) + ip0.prefund, UP) + ip0.exit_carrier - spend4,
        "maker receipts over 3 cycles"
    );
    println!("LEDGER-B v0={v0} profit(4)={profit4} maker_total={maker_total}");
}

/// Single-lever attacks on repeat IFD, sell-first (see the sell-side twin).
#[test]
fn kron_v2_buy_repeat_ifd_attacks() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        buy_repeat_attacks(&fx(tpl));
    }
}
fn buy_repeat_attacks(f: &Fx) {
    let n = &f.net;
    let m = pk(&f.maker_a);
    let i = cov(RPTA_ENTRY);
    let taker = pk(&f.taker);
    let tk = &n.k;
    let ip21 = rpta_p(f, 10 * TOK, 21 * TOK);

    // ---- booking (entry fill)
    let bad = |k: RaKnobs, name: &str| run_bad(&with_name(rpta_fill(f, &k), name), 0);
    let base = || RaKnobs::new(10 * TOK, 4 * TOK, 21 * TOK);
    let until = EXPIRY.min(1_000 + MAX_IDLE);
    bad(RaKnobs { exit_rpt: Some(([0; 32], 0, 0, 0)), ..base() }, "NRPB1 booked exit written as a plain exit (re-arm skipped)");
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_price(&ip21), ip21.prefund, until)), ..RaKnobs::new(10 * TOK, 4 * TOK, 4 * TOK) },
        "NRPB2 exit booked although the re-arms are exhausted",
    );
    bad(RaKnobs { next_rpt: Some(21 * TOK), ..base() }, "NRPB3 continuation keeps rptAmount (cycle count not decremented)");
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_price(&ip21) + 1, ip21.prefund, until)), ..base() },
        "NRPB4 exit's rptPrice raised by 1 sompi (maker overpaid from the prefund)",
    );
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_price(&ip21), ip21.prefund - 1, until)), ..base() },
        "NRPB4b exit's rptPre lowered by 1 sompi (prefund skimmed every cycle)",
    );
    bad(
        RaKnobs { exit_rpt: Some((i.as_bytes(), sell_price(&ip21), ip21.prefund, until - 1)), ..base() },
        "NRPB5 exit's rptUntil shortened",
    );
    bad(RaKnobs { t: NOW as i64 + 1, ..base() }, "NRPB6 cycle dated after the lockTime (t > tx DAA)");
    bad(
        RaKnobs { terminate: true, ..RaKnobs::new(6 * TOK, 6 * TOK, 17 * TOK) },
        "NRPB7 repeating entry terminated when sold out (everything to the exit)",
    );
    bad(
        RaKnobs { cont_delta: -1, ..RaKnobs::new(6 * TOK, 6 * TOK, 17 * TOK) },
        "NRPB8 sold-out repeating entry: custody carrier skimmed by 1 sompi",
    );

    // ---- take-profit / merge (exit side)
    let mb = |k: MergeBKnobs, name: &str, at: usize| run_bad(&with_name(rpta_merge(f, &k), name), at);
    mb(
        MergeBKnobs { with_entry: false, ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) },
        "NRPB10 booked exit takes profit without re-arming its entry (before rptUntil)",
        0,
    );
    mb(
        MergeBKnobs { maker_delta: -1, ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRPB11 maker's profit short by 1 sompi on a re-arming buy-back",
        0,
    );
    mb(
        MergeBKnobs { exit_delta: -1, ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRPB12 exit keeps 1 sompi less than its remaining tokens' proceeds + prefund",
        0,
    );
    let mut sl = booked_exit_b(&rpta_p(f, 0, 21 * TOK), 4 * TOK, 0);
    sl.armed = 1;
    mb(
        MergeBKnobs { exit: Some(sl), leg: 1, ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) },
        "NRPB13 stop-loss buy-back re-arms its entry (attacker-funded tokens for a stopped-out cycle)",
        0,
    );
    // Two booked exits; the entry names only the first. The second pays the maker the profit, the
    // seller keeps both its tokens and the buy-back money, and its leftovers go to the filler.
    let mut s = rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK));
    let last = s.outputs.len() - 1;
    s.outputs.truncate(last);
    let b_in = s.inputs.len();
    let x2 = booked_exit_b(&rpta_p(f, 0, 21 * TOK), 4 * TOK, 0);
    let x2_value = quote(4 * TOK, sell_price(&ip21) + ip21.prefund, UP) + ip21.exit_carrier;
    s.inputs.push(call(
        &condb(n, &x2),
        "settle",
        vec![nb(4 * TOK), iv(1), iv(0), iv(0), iv(0)],
        "condb.settle",
        x2_value,
        cov(0xe2),
        2_000,
    ));
    while s.outputs.len() < b_in {
        s.outputs.push(out(0, p2pk_spk(&taker), None));
    }
    s.outputs.push(out(quote(4 * TOK, x2.rpt_price, UP) - bid_all_in(4 * TOK, x2.tp, x2.tip), p2pk_spk(&m), None));
    s.outputs.push(out(0, p2pk_spk(&taker), None));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(
        &with_name(
            s,
            "NRPB14 double re-arm: a second booked exit rides on the first one's merge (buy-back money and leftovers skimmed)",
        ),
        b_in,
    );

    // ---- merge (entry side)
    mb(
        MergeBKnobs { cont_delta: -1, ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRPB15 partial buy-back: entry re-armed 1 sompi short of the prefund of 3000",
        2,
    );
    mb(
        MergeBKnobs { cont_delta: -1, ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) },
        "NRPB16 sell-out buy-back: the exit's leftovers skimmed by 1 sompi",
        2,
    );
    mb(
        MergeBKnobs { custody_delta: -1, ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) },
        "NRPB16b new custody carries 1 sompi less than exitCarrier",
        2,
    );
    mb(
        MergeBKnobs { custody_delta: -1, ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRPB16c existing custody's carrier skimmed by 1 sompi",
        2,
    );
    // the custody after the merge holds 8000, the last 1000 goes to the filler
    let mut s = with_name(
        rpta_merge(f, &MergeBKnobs { custody_amount: Some(8 * TOK), ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) }),
        "NRPB17 custody after the merge 1000 base units short (8000 instead of 9000, one token to the filler)",
    );
    set_leader_next(&mut s, 1, vec![tok_state(8 * TOK, &i.as_bytes(), T_COVID), tok_state(TOK, &taker, T_ADDR)]);
    s.outputs.push(out(CARRIER, tspk(tk, TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    s.inputs.push(p2pk_in(&f.taker, 100 * KAS));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(&s, 2);
    mb(
        MergeBKnobs { custody_owner: Some(taker), ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) },
        "NRPB18 bought-back tokens routed to the filler instead of the entry's custody",
        2,
    );
    let ip6 = rpta_p(f, 6 * TOK, 21 * TOK);
    mb(
        MergeBKnobs {
            cont: Some(IfdAP { amount: 9 * TOK, price: P255, ..ip6.clone() }),
            ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK)
        },
        "NRPB19 re-arm with altered parameters (limit 2.50 -> 2.55)",
        2,
    );
    mb(
        MergeBKnobs {
            cont: Some(IfdAP { amount: 9 * TOK, rpt: 99 * TOK, ..ip6.clone() }),
            ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK)
        },
        "NRPB19b re-arm refills the cycle count",
        2,
    );
    mb(
        MergeBKnobs { cont: Some(IfdAP { amount: 8 * TOK, ..ip6.clone() }), ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRPB19c entry records fewer base units than its custody holds (6000 + 2000 instead of 6000 + 3000)",
        2,
    );
    mb(
        MergeBKnobs {
            entry: stop_ip_armed(f),
            cont: Some(IfdAP { amount: 4 * TOK, ..stop_ip_armed(f) }),
            ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK)
        },
        "NRPB19d an empty sell-stop entry re-arms still armed",
        2,
    );
    mb(
        MergeBKnobs { exit: Some(rpta_p(f, 0, 21 * TOK).exit.with_amount(4 * TOK)), ..MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK) },
        "NRPB20 entry re-arms from a plain exit of another order (attacker-funded tokens)",
        2,
    );
    // A crafted non-KobCondBidKron input imitating a booked exit of this entry (empty entry).
    let mut s = rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK));
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0] = fake_exit_b(f, i, 4 * TOK, x_value);
    s.outputs[0].value = 0;
    s.outputs[0].script_public_key = p2pk_spk(&taker);
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(&with_name(s, "NRPB21 entry re-arms from a crafted input imitating its booked exit (not a KobCondBidKron)"), 2);
    // Strays and custody stand-ins.
    let mut s = with_name(
        rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK)),
        "NRPB24 merge into an empty entry co-spends a 2-token stray owned by it",
    );
    s.inputs.push(owned_tok(f, 2 * TOK, i));
    set_leader_next(&mut s, 1, vec![tok_state(4 * TOK, &i.as_bytes(), T_COVID), tok_state(2 * TOK, &taker, T_ADDR)]);
    s.outputs.push(out(CARRIER, tspk(tk, 2 * TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(&s, 2);
    let mut s = with_name(
        rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK)),
        "NRPB24b merge into an empty entry uses a 2-token stray owned by it as the template source",
    );
    s.inputs.push(owned_tok(f, 2 * TOK, i));
    set_leader_next(&mut s, 1, vec![tok_state(4 * TOK, &i.as_bytes(), T_COVID), tok_state(2 * TOK, &taker, T_ADDR)]);
    s.outputs.push(out(CARRIER, tspk(tk, 2 * TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    let stray_in = s.inputs.len() - 1;
    set_arg(&mut s, 2, 1, iv(stray_in as i64));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(&s, 2);
    let mut s = with_name(
        rpta_merge(f, &MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK)),
        "NRPB25 a 7-token stray owned by the entry stands in for its 6-token custody (the extra token to the filler)",
    );
    s.inputs[3] = owned_tok(f, 7 * TOK, i);
    set_leader_next(&mut s, 1, vec![tok_state(9 * TOK, &i.as_bytes(), T_COVID), tok_state(TOK, &taker, T_ADDR)]);
    s.outputs.push(out(CARRIER, tspk(tk, TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    s.inputs.push(p2pk_in(&f.taker, 100 * KAS));
    let last = s.outputs.len() - 2;
    balance(&mut s, last);
    run_bad(&s, 2);

    // ---- the merge requires the exit to be exactly one this entry books (terms, parent,
    // rptPrice, rptPre), run by its settle with n = m, and never costs the entry
    let atk = pk(&f.taker);
    let x = CondBP { maker: atk, ..booked_exit_b(&rpta_p(f, 6 * TOK, 21 * TOK), 4 * TOK, 0) };
    let mut s = rpta_merge(f, &MergeBKnobs { exit: Some(x), ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) });
    s.outputs[0].script_public_key = p2pk_spk(&atk);
    run_bad(&with_name(s, "NRPB29 look-alike exit of another maker (parent = this entry) re-arms the entry"), 2);
    let x = CondBP { rpt_pre: ip6.prefund - 1, ..booked_exit_b(&rpta_p(f, 6 * TOK, 21 * TOK), 4 * TOK, 0) };
    mb(
        MergeBKnobs { exit: Some(x), ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) },
        "NRPB30 look-alike exit with another rptPre (prefund - 1) re-arms the entry",
        2,
    );
    // an underfunded look-alike (1000 sompi) sells out: the old floor (entry + exit - proceeds) fell below the entry's own value
    let mut s = rpta_merge(f, &MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 4 * TOK));
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0].entry.amount = 1_000;
    let e_out = s.outputs.len() - 2;
    let old_floor = s.outputs[e_out].value as i64 - x_value + 1_000;
    s.outputs[e_out].value = old_floor as u64;
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(&with_name(s, "NRPB31 underfunded look-alike exit sells out: the entry keeps less than its own value"), 2);
    // the exit is cancelled by its maker beside a merge that claims 3000: only its settle may name the merge
    let mut s = rpta_merge(f, &MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK));
    let xp = booked_exit_b(&rpta_p(f, 6 * TOK, 21 * TOK), 4 * TOK, 0);
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0] = call(&condb(&f.net, &xp), "cancel", vec![Arg::Sig(f.maker_a, 0x01)], "condb.cancel", x_value, cov(RPTA_EXIT), 2_000);
    s.outputs[0] = out(0, p2pk_spk(&m), None);
    s.outputs[1] = out(0, p2pk_spk(&m), None);
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    run_bad(&with_name(s, "NRPB32 exit cancelled beside a merge of 3000 (the merge is not its settle)"), 2);
    // ... or armed by update next to a bid fill (touch): an update sigscript [ev, tk, tag, redeem] is no settle. Its ev is
    // pushed non-minimally (0x08 || ev, v3): the sigscript starts with 0x08 like a settle, so only the value check
    // (bytes [1..9) == m) tells it apart on the entry side (the exit's own parent guard refuses it as well: inputOnly).
    let mut s = rpta_merge(f, &MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK));
    let x_value = s.inputs[0].entry.amount as i64;
    s.inputs[0] = call(&condb(&f.net, &xp), "update", vec![iv(0), iv(-1)], "condb.update", x_value, cov(RPTA_EXIT), 2_000);
    s.outputs[0] = out(0, p2pk_spk(&m), None);
    s.outputs[1] = out(x_value, spk_of(&condb(&f.net, &xp.armed())), Some((0, cov(RPTA_EXIT))));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    let (ei, _) = add_ev(f, &mut s, &Ev::bid(285_000_000));
    set_arg(&mut s, 0, 0, Arg::Int8(ei));
    run_bad(&with_name(s, "NRPB33 exit armed by update beside a merge of 3000 (an update is not its settle)"), 2);
    // The other direction: a re-arming take-profit names, as its merge, its sell-stop entry's arming update next to an
    // ask fill: the exit's budgets would leak to the filler. (The merge is laid out for an empty entry; the entry that
    // is armed instead still holds tokens, since update arms only an entry with tokens to fill.)
    let stop_e = IfdAP { entry_stop: 260_000_000, ..rpta_p(f, 0, 21 * TOK) };
    let mut s = rpta_merge(f, &MergeBKnobs { entry: stop_e.clone(), ..MergeBKnobs::new(f, 0, 4 * TOK, 3 * TOK) });
    let stop_e = IfdAP { amount: 6 * TOK, ..stop_e };
    let ev_in = 2;
    let e_value = s.inputs[ev_in].entry.amount as i64;
    s.inputs[ev_in] = call(&ifda(&f.net, &stop_e), "update", vec![iv(0), iv(0)], "ifda.update", e_value, cov(RPTA_ENTRY), 1_000);
    let ci = s.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|b| b.covenant_id == cov(RPTA_ENTRY))).unwrap();
    s.outputs[ci] = out(e_value, spk_of(&ifda(&f.net, &IfdAP { armed: 1, ..stop_e.clone() })), Some((ev_in as u16, cov(RPTA_ENTRY))));
    let last = s.outputs.len() - 1;
    balance(&mut s, last);
    let (ei, ti) = add_ev(f, &mut s, &Ev::ask(255_000_000));
    // ev pushed non-minimally (0x08 || ev): the entry's sigscript starts with 0x08 like a merge, so only the exit's
    // value check (bytes [1..9) == -(k * 2^53 + n)) refuses it
    set_arg(&mut s, ev_in, 0, Arg::Int8(ei));
    set_arg(&mut s, ev_in, 1, iv(ti));
    run_bad(&with_name(s, "NRPB34 re-arming take-profit names its entry's arming update as the merge (budgets leak)"), 0);

    // ---- close
    let e0 = rpta_p(f, 0, TOK);
    let close = |e: &IfdAP, lock: u64, name: &str| Scn {
        name: name.into(),
        inputs: vec![call(&ifda(n, e), "close", vec![], "ifda.close", 2 * CARRIER, i, (EXPIRY - 1_000) as u64)],
        outputs: vec![out(2 * CARRIER - REFUND_TIP, p2pk_spk(&m), None)],
        lock_time: lock,
        payload: vec![],
        next: vec![],
    };
    run_bad(
        &close(
            &rpta_p(f, 3 * TOK, TOK),
            EXPIRY as u64,
            "NRPB26 close of an entry that still has tokens (its custody would be orphaned)",
        ),
        0,
    );
    run_bad(&close(&e0, EXPIRY as u64 - 1, "NRPB27 close before the soft expiry"), 0);
    let mut s = close(&e0, EXPIRY as u64, "NRPB28 close co-spends a stray owned by the entry");
    s.inputs.push(owned_tok(f, 2 * TOK, i));
    s.next = vec![tok_state(2 * TOK, &taker, T_ADDR)];
    s.outputs.push(out(CARRIER, tspk(tk, 2 * TOK, &taker, T_ADDR), Some((1, TOKEN_COV))));
    run_bad(&s, 0);

    // ---- a merge into a stop entry armed by update and not filled yet keeps its band origin (the armed UTXO's DAA)
    let ae = IfdAP { entry_stop: 260_000_000, band_daa: 300, armed: 1, ..rpta_p(f, 6 * TOK, 21 * TOK) };
    let k = MergeBKnobs { entry: ae.clone(), ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, 3 * TOK) };
    run_ok(&with_name(rpta_merge(f, &k), "RPB10 merge into an armed, unfilled stop entry: the continuation records its band origin"));
    let s = rpta_merge(f, &MergeBKnobs { cont: Some(IfdAP { amount: 9 * TOK, ..ae.clone() }), ..k.clone() });
    run_bad(&with_name(s, "NRPB35 merge into an armed, unfilled stop entry keeps armed = 1 (its band auction restarts)"), 2);
}
/// An armed repeating sell-stop entry (stop 2.60, limit 2.50) with nothing left.
fn stop_ip_armed(f: &Fx) -> IfdAP {
    let mut p = rpta_p(f, 0, 21 * TOK);
    p.entry_stop = 260_000_000;
    p.armed = 1;
    p
}

// ================================================================ regressions (buy side)

/// A buy-stop at a LOW stop price (below 10000 sompi per whole token), armed, whole band at once:
/// the 3% band must hold (stop 9999 -> ceiling 10298). `delta`: sompi per whole token paid above the ceiling.
fn fx_low_stop_b(f: &Fx, stop: i64, delta: i64) -> Scn {
    let cp = CondBP { tip: 0, stop, tp: 0, armed: 1, ..CondBP::oco(pk(&f.maker_a)) };
    let ceiling = stop.saturating_add(stop.saturating_mul(cp.slip_bps) / 10_000);
    let mut s = condb_fill_at(f, &cp, 4 * TOK, 1, &cp.with_amount(6 * TOK), None, 0, ceiling.saturating_add(delta));
    s.name = format!("FXL4b low buy-stop {stop}: stop-market fill at {} (band ceiling {ceiling})", ceiling.saturating_add(delta));
    s
}

/// Regressions (buy side, KRON, both templates): the multiply-first stop band. The repeat-IFD merge
/// checks are in `kron_v2_buy_repeat_ifd_attacks` (NRPB29..NRPB32).
#[test]
fn kron_v2_buy_fix_pass_regressions() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let f = &f;
        run_ok(&fx_low_stop_b(f, 9_999, 0));
        run_bad(&fx_low_stop_b(f, 9_999, 1), 0);
        run_ok(&fx_low_stop_b(f, 100, 0));
    }
}

// ================================================================ protocol v3 new checks (buy side)
//
// Base units replace lots (SCALE = 1000 base units per whole token), so minFill, the quoteOf rounding
// (floor for what the maker pays, ceil for what it receives), the price >= tip rule, the overflow-fails-closed
// property and the repeat merge argument -(k * 2^53 + m) each get their own scenario. Every attack here is
// REJECTED by the committed contracts and is the flip of a catalog mutation of the check it targets (except the
// overflow pair, which the engine's checked arithmetic enforces with no removable check, and the 0x08 first-byte
// merge checks, which are defence in depth behind the value / parent-guard checks: see kron-buy.mjs).

/// A counterparty input crafted from a REAL order template (so tplState and the covenant-id trust pass) but spent
/// with a non-minimal 9-byte first push whose low 8 bytes read as `val`: its sigscript starts with 0x09, not the
/// 8-byte fill/merge push (0x08), and bytes [1..9) equal `val`. The merge's value check (== m / == the merge
/// argument) passes, only the 0x08 first-byte check rejects it. The crafted input fails its OWN execution (the real
/// template decodes its first parameter as a byte[8] and a 9-byte push is the wrong length), so the attack is proven
/// by the merge-side input alone (inputOnly).
fn crafted_push9(redeem: &[u8], val: i64, value: i64, c: Hash, daa: u64, name: &'static str) -> Inp {
    let mut ss = vec![0x09];
    ss.extend(num8(val)); // 8 bytes read by substr(_, 1, 9)
    ss.push(0x00); // the 9th byte of the push
    ss.extend(push(redeem));
    Inp { entry: utxo(value, pay_to_script_hash_script(redeem), c, daa), role: Role::RawSs { ss, name }, seq: 0 }
}

/// A KobCondBidKron limit fill built without condb_value (so the escrow can hold an arbitrary, possibly
/// i64-saturating, spend): the order (cov 0xe1) buys `n` of `amount` base units on the limit leg at `rate`
/// (tip 0) from a taker. The continuation keeps one delivery carrier; the spend rides to the taker.
fn condb_raw_fill(f: &Fx, amount: i64, n: i64, rate: i64, spend: i64) -> Scn {
    let k = &f.net.k;
    let c = cov(0xe1);
    let m = pk(&f.maker_b);
    let cp = CondBP { tip: 0, tp: rate, amount, ..CondBP::oco(m) };
    let order = condb(&f.net, &cp);
    let taker = pk(&f.taker);
    let v = spend + 2 * DC;
    let cont = n < amount;
    let mut s = Scn {
        name: "condb overflow".into(),
        inputs: vec![
            call(&order, "settle", vec![nb(n), iv(1), iv(0), iv(0), iv(0)], "condb.settle", v, c, 2_000),
            tok_in(k, CARRIER, n, &taker, T_ADDR, TOKEN_COV, Wit::Auto, 1_000),
        ],
        outputs: vec![out(if cont { DC } else { v - spend }, tspk(k, n, &m, T_ADDR), Some((1, TOKEN_COV)))],
        lock_time: NOW,
        payload: vec![],
        next: vec![tok_state(n, &m, T_ADDR)],
    };
    if cont {
        s.outputs.push(out(v - spend - DC, spk_of(&condb(&f.net, &cp.with_amount(amount - n))), Some((0, c))));
    }
    s.inputs.push(p2pk_in(&f.taker, TK));
    s.outputs.push(out(spend + CARRIER + TK, p2pk_spk(&taker), None));
    s
}

/// A non-repeating KobIfdAskKron partial fill (rest > 0) built without a balancing taker change (so the exit can
/// hold an i64-saturating proceeds): the entry (cov 0xf1, `have` base units) sells `n` at `price`, output 0 the
/// exit holding proceeds + prefund + carrier, output 1 the entry continuation, then the token remainder, the
/// taker's tokens and a zero taker change (KAS conservation is not checked by the engine).
fn ifda_raw_fill(f: &Fx, ip: &IfdAP, have: i64, n: i64, price: i64) -> Scn {
    let k = &f.net.k;
    let i = cov(0xf1);
    let entry = ifda(&f.net, &IfdAP { amount: have, ..ip.clone() });
    let exit = condb(&f.net, &ip.exit.with_amount(n));
    let tpl = condb_tpl(&f.net);
    let taker = pk(&f.taker);
    let proceeds = quote(n, price - ip.tip, UP);
    let pre = quote(n, ip.prefund, UP);
    let value = IFDA_VALUE;
    let rest = have - n;
    let mut s = Scn {
        name: "ifda overflow".into(),
        inputs: vec![
            call(
                &entry,
                "settle",
                vec![nb(n), iv(1), iv(2), iv(0), Arg::V(bytes(&tpl.pre)), Arg::V(bytes(&tpl.suf)), iv(0), iv(0), iv(0)],
                "ifda.settle",
                value,
                i,
                1_000,
            ),
            tok_in(k, CARRIER, have, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_000),
            p2pk_in(&f.taker, 1000 * KAS),
        ],
        outputs: vec![
            out(proceeds + pre + IFDA_EXIT_CARRIER, spk_of(&exit), None),
            out(value - pre - IFDA_EXIT_CARRIER, spk_of(&ifda(&f.net, &IfdAP { amount: rest, ..ip.clone() })), Some((0, i))),
            out(CARRIER, tspk(k, rest, &i.as_bytes(), T_COVID), Some((1, TOKEN_COV))),
            out(CARRIER, tspk(k, n, &taker, T_ADDR), Some((1, TOKEN_COV))),
            // a positive taker change (KAS conservation is not checked; mass calc divides by zero on a 0 output)
            out(CARRIER, p2pk_spk(&taker), None),
        ],
        lock_time: NOW,
        payload: vec![],
        next: vec![tok_state(rest, &i.as_bytes(), T_COVID), tok_state(n, &taker, T_ADDR)],
    };
    regen(&mut s, 0, 0);
    s
}

#[test]
fn kron_v2_buy_v3_min_fill() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let f = &f;
        // KobCondBidKron: n >= minFill unless n == amountLeft
        let m = pk(&f.maker_b);
        let cp = CondBP { min_fill: 2 * TOK, ..CondBP::oco(m) };
        run_ok(&with_name(
            condb_fill(f, &cp, 2 * TOK, 0, &cp.with_amount(8 * TOK), None),
            "V3CB01 conditional buy fill of exactly minFill (2000) with 8000 left",
        ));
        run_bad(
            &with_name(
                condb_fill(f, &cp, 2 * TOK - 1, 0, &cp.with_amount(8 * TOK + 1), None),
                "NV3CB01 conditional buy fill one base unit below minFill, rest remains",
            ),
            0,
        );
        let small = CondBP { min_fill: 2 * TOK, amount: 3 * TOK / 2, ..CondBP::oco(m) };
        run_ok(&with_name(
            condb_fill(f, &small, 3 * TOK / 2, 0, &small, None),
            "V3CB02 conditional buy fill below minFill that takes the rest (1500 == amountLeft < minFill 2000)",
        ));
        // KobIfdAskKron: n >= minFill unless outAmount == 0
        let mf = IfdAP { min_fill: 2 * TOK, ..ifda_p(f) };
        run_ok(&with_name(
            ifda_fill(f, &mf, 10 * TOK, 2 * TOK, IFDA_VALUE),
            "V3IA01 if-done entry fill of exactly minFill (2000) with 8000 left",
        ));
        run_bad(
            &with_name(
                ifda_fill(f, &mf, 10 * TOK, 2 * TOK - 1, IFDA_VALUE),
                "NV3IA01 if-done entry fill one base unit below minFill, rest remains",
            ),
            0,
        );
        run_ok(&with_name(
            ifda_fill(f, &mf, 3 * TOK / 2, 3 * TOK / 2, IFDA_VALUE),
            "V3IA02 if-done entry fill below minFill that sells out (1500 == amountLeft < minFill 2000)",
        ));
    }
}

#[test]
fn kron_v2_buy_v3_rounding() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let f = &f;
        let m = pk(&f.maker_b);
        // KobCondBidKron spend = quoteOf(n, legPrice + tip, FLOOR) (the maker pays at most the floor). At a
        // fractional n the floor is one less than the ceil: the exact floor continuation is accepted, charging
        // one sompi more (the ceil) is rejected (CB-spendround: the floor rounding).
        let cp = CondBP { tip: 0, tp: 200_000_123, ..CondBP::oco(m) };
        let n = 4 * TOK + 1;
        assert_eq!(quote(n, cp.tp, DOWN) + 1, quote(n, cp.tp, UP), "V3CB10 needs a rounding n");
        let rest = cp.amount - n;
        run_ok(&with_name(
            condb_fill(f, &cp, n, 0, &cp.with_amount(rest), None),
            "V3CB10 conditional buy at a fractional amount: continuation holds the exact floor budget",
        ));
        let mut s = with_name(
            condb_fill(f, &cp, n, 0, &cp.with_amount(rest), None),
            "NV3CB10 conditional buy spend charged the ceil (continuation one sompi short of the floor)",
        );
        s.outputs[1].value -= 1;
        let l = s.outputs.len() - 1;
        s.outputs[l].value += 1;
        run_bad(&s, 0);

        // KobIfdAskKron proceeds = quoteOf(n, p - tip, CEIL) (the exit must hold at least the ceil the maker
        // receives). prefund multiple of the scale (exact), so only the proceeds rounds: the exact ceil exit is
        // accepted, one sompi short is rejected (IA-procround).
        let pp = IfdAP { tip: 0, price: 250_000_123, prefund: 50_000_000, ..ifda_p(f) };
        assert_eq!(quote(n, pp.price, DOWN) + 1, quote(n, pp.price, UP), "V3IA10 needs a rounding proceeds");
        assert_eq!(quote(n, pp.prefund, DOWN), quote(n, pp.prefund, UP), "V3IA10 prefund must be exact");
        run_ok(&with_name(
            ifda_fill(f, &pp, 10 * TOK, n, IFDA_VALUE),
            "V3IA10 if-done fill at a fractional amount: exit holds the ceil proceeds",
        ));
        let mut s = with_name(ifda_fill(f, &pp, 10 * TOK, n, IFDA_VALUE), "NV3IA10 if-done exit one sompi short of the ceil proceeds");
        s.outputs[0].value -= 1;
        let l = s.outputs.len() - 1;
        s.outputs[l].value += 1;
        regen(&mut s, 0, 0);
        run_bad(&s, 0);

        // prefund = quoteOf(n, prefund, CEIL): proceeds exact (p - tip multiple of the scale), prefund rounds, so
        // only the prefund-ceil (IA-preround) binds the one-sompi-short exit.
        let qp = IfdAP { tip: 0, price: 250_000_000, prefund: 50_000_123, ..ifda_p(f) };
        assert_eq!(quote(n, qp.price, DOWN), quote(n, qp.price, UP), "V3IA11 proceeds must be exact");
        assert_eq!(quote(n, qp.prefund, DOWN) + 1, quote(n, qp.prefund, UP), "V3IA11 needs a rounding prefund");
        run_ok(&with_name(
            ifda_fill(f, &qp, 10 * TOK, n, IFDA_VALUE),
            "V3IA11 if-done fill at a fractional amount: exit holds the ceil prefund",
        ));
        // the sompi stays with the entry (its continuation keeps it): a floored prefund would lower the exit's bound
        // AND raise the entry's floor by the same sompi, so the entry is where the floor-rounded attack puts it
        let mut s = with_name(
            ifda_fill(f, &qp, 10 * TOK, n, IFDA_VALUE),
            "NV3IA11 if-done exit one sompi short of the ceil prefund (the sompi kept by the entry)",
        );
        s.outputs[0].value -= 1;
        s.outputs[1].value += 1;
        regen(&mut s, 0, 0);
        run_bad(&s, 0);

        // Merge floor = inValue + quoteOf(m, prefund, CEIL): the re-armed entry gets back at least the ceil of the
        // prefund of m. A fractional buy-back with a fractional prefund: the exact ceil continuation is accepted,
        // one sompi short is rejected (IA-mergeround).
        let claim = 3 * TOK + 1;
        let mut ei = rpta_p(f, 6 * TOK, 21 * TOK);
        ei.prefund = 50_000_123;
        assert_eq!(quote(claim, ei.prefund, DOWN) + 1, quote(claim, ei.prefund, UP), "V3IA12 needs a rounding prefund");
        run_ok(&with_name(
            rpta_merge(f, &MergeBKnobs { entry: ei.clone(), ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, claim) }),
            "V3IA12 re-arming buy-back of a fractional amount: entry gets back the exact ceil prefund",
        ));
        run_bad(
            &with_name(
                rpta_merge(f, &MergeBKnobs { entry: ei, cont_delta: -1, ..MergeBKnobs::new(f, 6 * TOK, 4 * TOK, claim) }),
                "NV3IA12 re-arming buy-back: entry continuation one sompi short of the ceil prefund",
            ),
            2,
        );
    }
}

#[test]
fn kron_v2_buy_v3_tip() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let f = &f;
        // KobIfdAskKron requires price >= tip (so p >= price >= tip and a booked rptPrice = price - tip is never
        // negative). A sell-stop entry whose auction price (the stop 2.60) is above its limit price but whose
        // limit price is below the tip is refused by require(price >= tip) even though the fill price p exceeds
        // the tip (IA-pricetip).
        let ip = IfdAP { price: 50_000, entry_stop: 260_000_000, band_daa: 1_000, ..ifda_p(f) };
        let down = Ev::ask(255_000_000);
        run_bad(
            &with_name(
                ifda_fill_x(f, &ip, 10 * TOK, 4 * TOK, IFDA_VALUE, 260_000_000, Some(&down), 0, 1),
                "NV3IA21 sell-stop entry price (0.0005) below the tip though its auction price (2.60) is above it",
            ),
            0,
        );
    }
}

#[test]
fn kron_v2_buy_v3_overflow() {
    // scale = 1000, rate 9e18: quoteOf(n, rate) = 9e15 * n, which fits in an i64 up to n = 1024 and overflows at
    // 1025 (the checked multiply fails the script). The covenant fails closed: the largest fitting fill validates,
    // one more base unit is rejected without the attacker paying less. No contract check is removed (the engine's
    // checked arithmetic enforces it); this is a property test.
    const RATE: i64 = 9_000_000_000_000_000_000;
    let n_max = (1..).map(|q| q as i64).take_while(|&n| quote_at(n, RATE, SCALE, DOWN).is_some()).last().unwrap();
    assert_eq!(n_max, 1024, "largest fitting fill at rate 9e18");
    assert!(quote_at(n_max + 1, RATE, SCALE, DOWN).is_none(), "one more base unit overflows");
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let f = &f;
        // KobCondBidKron spend = quoteOf(n, legPrice, floor)
        let spend = quote(n_max, RATE, DOWN);
        run_ok(&with_name(
            condb_raw_fill(f, n_max + 1, n_max, RATE, spend),
            "V3CB30 conditional buy at the largest fitting amount (n = 1024, rate 9e18): the spend product fits",
        ));
        let mut s = with_name(
            condb_raw_fill(f, n_max + 1, n_max, RATE, spend),
            "NV3CB30 conditional buy at n = 1025: the spend product overflows, the fill is refused (no cheaper fill)",
        );
        set_arg(&mut s, 0, 0, nb(n_max + 1));
        run_bad(&s, 0);

        // KobIfdAskKron proceeds = quoteOf(n, p - tip, ceil)
        let ip = IfdAP { price: RATE + TIP, ..ifda_p(f) };
        run_ok(&with_name(
            ifda_raw_fill(f, &ip, n_max + 1, n_max, ip.price),
            "V3IA30 if-done entry at the largest fitting amount (n = 1024): the proceeds product fits",
        ));
        let mut s = with_name(
            ifda_raw_fill(f, &ip, n_max + 1, n_max, ip.price),
            "NV3IA30 if-done entry at n = 1025: the proceeds product overflows, the fill is refused (no cheaper fill)",
        );
        set_arg(&mut s, 0, 0, nb(n_max + 1));
        run_bad(&s, 0);
    }
}

#[test]
fn kron_v2_buy_v3_merge_push() {
    // The repeat merge argument is -(k * 2^53 + m), k the exit's input index, m < 2^53 the bought-back amount.
    // k = 999, m = 2^53 - 1 encodes to |value| = 1000 * 2^53 - 1, which fits in a signed 8-byte script number
    // (below 2^63); building 999 transaction inputs is infeasible, so the merge is run at the exit's natural
    // input index while the k = 999 encoding is asserted here.
    let enc = 999 * MERGE_K + (MERGE_K - 1);
    assert!(enc > 0 && enc < i64::MAX, "k = 999, m = 2^53 - 1 fits an 8-byte script number");
    assert_eq!((enc / MERGE_K, enc % MERGE_K), (999, MERGE_K - 1), "the argument decodes to k = 999, m = 2^53 - 1");

    // KRON token outputs are capped at 1 <= amount <= 1e9 base units (contracts/adapters/kron/README.md), far below
    // 2^53, so a booked exit amount can never approach the merge-argument limit on KRON (the token program rejects
    // any UTXO of that size). The n < MERGE_K booking guard is therefore unreachable on KRON and is exercised on the
    // KCC-20 twin; here only the argument encoding is checked (above). Positive and attack merges at buildable
    // amounts are covered by the RPB* / NRPB* battery.

    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let f = &f;
        let i = cov(RPTA_ENTRY);

        // Parent guard of KobCondBidKron.update: an exit's update never runs in a transaction that also spends its
        // repeat entry (OpCovInputCount(parent) == 0). Non-minimal signature-script pushes could otherwise make the
        // exit's update argument alias a merge amount the entry would read. A booked exit (parent = 0xf1) armed by
        // update, with an input of covenant id 0xf1 present, is refused (CB-updateparent).
        let cp = CondBP { parent: i.as_bytes(), rpt_price: 1, rpt_pre: 1, rpt_until: EXPIRY, ..CondBP::oco(pk(&f.maker_b)) };
        let mut s = with_name(
            condb_update(f, &cp, &cp.armed(), &arm_ev(), 0),
            "NV3CB41 exit's arming update runs next to its repeat entry (parent present)",
        );
        // an input owned by the repeat entry's covenant id (an OpTrue covenant UTXO): OpCovInputCount(parent) > 0
        let at = s.inputs.len();
        s.inputs.push(Inp { entry: utxo(CARRIER, ScriptPublicKey::new(0, vec![OpTrue].into()), i, 500), role: Role::Raw, seq: 0 });
        s.outputs.push(out(CARRIER, p2pk_spk(&pk(&f.taker)), Some((at as u16, i))));
        run_bad(&s, 0);

        // 0x08 first-byte check of KobCondBidKron.settle rearm (the exit reads the entry pin): the entry is a real
        // KobIfdAskKron spent with a non-minimal 9-byte first push whose low 8 bytes equal the merge argument
        // -(self * 2^53 + n). The value check (CB11) passes; only require(pin[0] == 0x08) (CB-pin08) rejects it.
        let empty = rpta_p(f, 0, 21 * TOK);
        let mut s = rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK));
        let entry_in = s.inputs.iter().position(|x| x.entry.covenant_id == Some(i)).unwrap();
        let ev = s.inputs[entry_in].entry.amount as i64;
        // the exit is at input 0, so self = 0: the merge argument is -(0 * 2^53 + 4000) = -4000
        s.inputs[entry_in] = crafted_push9(&bytecode(&ifda(&f.net, &empty)), -(4 * TOK), ev, i, 1_000, "entry.push9");
        run_bad(
            &with_name(
                s,
                "NV3CB42 exit re-arms beside an entry whose 9-byte first push aliases the merge argument (not the 8-byte push)",
            ),
            0,
        );

        // 0x08 first-byte check of KobIfdAskKron.settle merge (the entry reads the exit at k): the exit is a real
        // KobCondBidKron spent with a non-minimal 9-byte first push whose low 8 bytes equal m. The value check
        // (FXL3e) and the exit-terms comparison pass; only require(k[0] == 0x08) (IA-k08) rejects it.
        let xp = booked_exit_b(&empty, 4 * TOK, 0);
        let mut s = rpta_merge(f, &MergeBKnobs::new(f, 0, 4 * TOK, 4 * TOK));
        let xv = s.inputs[0].entry.amount as i64;
        s.inputs[0] = crafted_push9(&bytecode(&condb(&f.net, &xp)), 4 * TOK, xv, cov(RPTA_EXIT), 2_000, "exit.push9");
        let entry_in = s.inputs.iter().position(|x| x.entry.covenant_id == Some(i)).unwrap();
        run_bad(
            &with_name(s, "NV3IA40 entry merges an exit whose 9-byte first push aliases m (not the 8-byte settle push)"),
            entry_in,
        );
    }
}

/// Security review 2026-10-06 (the KRON twin of `kob_v2_buy_tests::sec_empty_repeating_ifda_refund_drain_is_refused`): the
/// refund (settle nb = 0) of an EMPTY repeating KobIfdAskKron entry with a zero-amount token UTXO owned by the entry's id
/// standing in as its custody pinned no output, so its KAS could go anywhere. The refund now requires tokens held: the
/// entry (input 0) refuses it whatever the token program makes of the stand-in; the empty entry ends by `close()` (RPB8)
/// and an entry holding tokens is still refunded to the maker (R1).
#[test]
fn kron_sec_empty_repeating_ifda_refund_drain_is_refused() {
    for tpl in TPLS {
        println!("=== KRON template {tpl}");
        let f = fx(tpl);
        let n = &f.net;
        let k = &n.k;
        let i = cov(RPTA_ENTRY);
        let attacker = pk(&f.taker);
        let e = rpta_p(&f, 0, TOK);
        let tpl_b = condb_tpl(n);
        let s = Scn {
            name: "SR2 empty repeating KobIfdAskKron drained via settle(0) with a zero-amount custody".into(),
            inputs: vec![
                call(
                    &ifda(n, &e),
                    "settle",
                    vec![nb(0), iv(1), iv(0), iv(0), Arg::V(bytes(&tpl_b.pre)), Arg::V(bytes(&tpl_b.suf)), iv(0), iv(0), iv(0)],
                    "ifda.settle",
                    2 * CARRIER,
                    i,
                    (EXPIRY - 1_000) as u64,
                ),
                tok_in(k, CARRIER, 0, &i.as_bytes(), T_COVID, TOKEN_COV, Wit::Auto, 1_500),
                p2pk_in(&f.taker, 10 * KAS),
            ],
            outputs: vec![
                // output self: every carrier minus refundTip (what the old refund required), to the attacker
                out(3 * CARRIER - REFUND_TIP, p2pk_spk(&attacker), None),
                out(CARRIER / 4, tspk(k, 0, &attacker, T_ADDR), Some((1, TOKEN_COV))),
                out(10 * KAS - CARRIER / 4 - NET_FEE, p2pk_spk(&attacker), None),
            ],
            lock_time: EXPIRY as u64,
            payload: vec![],
            next: vec![tok_state(0, &attacker, T_ADDR)],
        };
        run_bad(&s, 0);
    }
}
