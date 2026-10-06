//! C6 "unknown unknowns": a structural mutation fuzzer over builder-produced KOB transactions with a
//! value-conservation oracle (see `c6_fuzz.rs` for the entry points and `research/.../c6_unknowns.md` for the
//! report).
//!
//! * [`MTx`] is a transaction kept at the level of the library's signing plans: every input is its UTXO plus its
//!   [`SigPlan`] (template, state, entry, arguments, KCC-20 next states, KRON witnesses), so a mutant is re-signed
//!   with the fixture keys and re-assembled by the production code ([`SigPlan::sigscript`]) after every mutation.
//!   A mutation that breaks a signature therefore never hides behind the signature: the mutant is what its signers
//!   would sign.
//! * [`accept`] is consensus as far as it concerns a single transaction: duplicate inputs, zero / overflowing
//!   outputs, the KIP-9 masses within the block limits, the covenant context (bindings, genesis ids) and every
//!   input script in the rusty-kaspa v2.1.0 engine. The fee floor and the compute budgets are not checked (an
//!   attacker pays any fee and commits any budget).
//! * [`oracle`] values every party of an accepted mutant: per token covenant id the supply, per maker the change of
//!   its KAS and tokens valued at the order's own worst all-in price for fills in base units with the covenants'
//!   exact rounding (at least `ceil(n * rate / scale)` for what a maker receives, at most `floor(n * rate / scale)` for
//!   what it pays; refund and keeper tips allowed), per pair order maker both tokens (an ask receives at least the ceil
//!   of the quote token B, a bid pays at most the floor of it; custodies of both tokens are the maker's) and the KAS tip
//!   (rounded down), per intent the payer's bounds and the merchant's amount, and per
//!   order covenant the continuation (one output, same template, only the mutable windows changed, monotone
//!   transitions, arming only next to a fill).

#![allow(dead_code)]
// index loops over parallel per-token vectors read clearer than zipped iterators; `What` is a short-lived per-UTXO
// classification (its size is irrelevant)
#![allow(clippy::needless_range_loop, clippy::large_enum_variant)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kob_protocol::artifacts::spk_trace::{self, Origin};
use kob_protocol::artifacts::{template, try_token_template, TemplateId};
use kob_protocol::router::{Actor, IntentState, EXPIRE_MAX_FEE};
use kob_protocol::state::*;
use kob_protocol::tx::{masses, sighash, sign_digest, Arg, BuiltTx, SigPlan};
use kob_protocol::verify::execute;

// ------------------------------------------------------------------------------------------------ model

/// One input: its outpoint, UTXO entry, signing plan, sequence and committed compute budget.
#[derive(Clone, Debug)]
pub struct MIn {
    pub op: TransactionOutpoint,
    pub entry: UtxoEntry,
    pub plan: SigPlan,
    pub seq: u64,
    pub budget: u16,
}

/// A transaction at the level of its signing plans.
#[derive(Clone, Debug)]
pub struct MTx {
    pub ins: Vec<MIn>,
    pub outs: Vec<TransactionOutput>,
    pub lock_time: u64,
    pub payload: Vec<u8>,
}

impl MTx {
    pub fn from_built(b: &BuiltTx) -> MTx {
        let (tx, entries) = b.tx.to_tx().expect("built tx");
        let ins = tx
            .inputs
            .iter()
            .zip(entries)
            .zip(&b.plans)
            .map(|((i, e), p)| MIn {
                op: i.previous_outpoint,
                entry: e,
                plan: p.clone(),
                seq: i.sequence,
                budget: i.compute_commit.compute_budget().unwrap_or(0),
            })
            .collect();
        MTx { ins, outs: tx.outputs.clone(), lock_time: tx.lock_time, payload: tx.payload.clone() }
    }
}

/// The attacker: a fixture key no fixture order or wallet uses.
pub const ATTACKER: u8 = 33;

pub type Keys = BTreeMap<[u8; 32], [u8; 32]>;

/// Re-signs and assembles a mutant (production sigscript assembly, fixture keys, honest storage-mass commitment).
pub fn materialize(m: &MTx, keys: &Keys) -> Result<(Transaction, Vec<UtxoEntry>), String> {
    if m.ins.is_empty() || m.outs.is_empty() {
        return Err("no inputs or no outputs".into());
    }
    let inputs: Vec<TransactionInput> =
        m.ins.iter().map(|i| TransactionInput::new_with_compute_budget(i.op, vec![], i.seq, i.budget)).collect();
    let entries: Vec<UtxoEntry> = m.ins.iter().map(|i| i.entry.clone()).collect();
    let mut tx = Transaction::new(1, inputs, m.outs.clone(), m.lock_time, SUBNETWORK_ID_NATIVE, 0, m.payload.clone());
    let storage = masses(&tx, &entries).storage;
    tx.set_storage_mass(storage);
    let mut scripts = Vec::with_capacity(m.ins.len());
    for (i, inp) in m.ins.iter().enumerate() {
        inp.plan.check().map_err(|e| format!("plan {i}: {e}"))?;
        let sig = match inp.plan.signer() {
            Some(k) => {
                let sk = keys.get(&k).ok_or_else(|| format!("input {i}: no key"))?;
                Some(sign_digest(sk, &sighash(&tx, &entries, i)).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        scripts.push(inp.plan.sigscript(sig.as_deref()).map_err(|e| format!("sigscript {i}: {e}"))?);
    }
    for (i, s) in scripts.into_iter().enumerate() {
        tx.inputs[i].signature_script = s;
    }
    tx.finalize();
    Ok((tx, entries))
}

/// Why a mutant is not a valid transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reject {
    Assemble(String),
    Consensus(String),
    Script(usize, String),
}

impl Reject {
    pub fn class(&self) -> String {
        match self {
            Reject::Assemble(_) => "assemble".into(),
            Reject::Consensus(s) => format!("consensus:{}", s.split(':').next().unwrap_or("")),
            Reject::Script(_, e) => format!("script:{}", e.split(['(', ' ', '{']).next().unwrap_or("")),
        }
    }
}

const MAX_SOMPI: u64 = 29_000_000_000 * 100_000_000;

/// Single-transaction consensus validity (see the module docs).
pub fn accept(tx: &Transaction, entries: &[UtxoEntry]) -> Result<(), Reject> {
    accept_with(tx, entries, &[])
}

/// [`accept`] with the script results of the inputs flagged in `skip` ignored (ablation: a canary that the fuzzer and
/// the oracle find the theft a missing covenant check would allow).
pub fn accept_with(tx: &Transaction, entries: &[UtxoEntry], skip: &[bool]) -> Result<(), Reject> {
    let c = |s: String| Err(Reject::Consensus(s));
    if tx.inputs.is_empty() || tx.outputs.is_empty() {
        return c("empty: no inputs or outputs".into());
    }
    let mut seen = BTreeSet::new();
    for i in &tx.inputs {
        if !seen.insert((i.previous_outpoint.transaction_id, i.previous_outpoint.index)) {
            return c("duplicate: input outpoint".into());
        }
    }
    let mut total_out: u64 = 0;
    for (i, o) in tx.outputs.iter().enumerate() {
        if o.value == 0 {
            return c(format!("zero: output {i}"));
        }
        total_out = match total_out.checked_add(o.value) {
            Some(t) if t <= MAX_SOMPI => t,
            _ => return c("overflow: outputs".into()),
        };
    }
    let total_in = entries.iter().try_fold(0u64, |a, e| a.checked_add(e.amount)).unwrap_or(u64::MAX);
    if total_out > total_in {
        return c(format!("value: outputs {total_out} > inputs {total_in}"));
    }
    let mass = masses(tx, entries);
    if !mass.within_block_limits() {
        return c(format!("mass: {mass:?}"));
    }
    let res = execute(tx, entries, false).map_err(|e| Reject::Consensus(format!("covenant: {e}")))?;
    for (i, r) in res.into_iter().enumerate() {
        if skip.get(i).copied().unwrap_or(false) {
            continue;
        }
        if let Err(e) = r {
            return Err(Reject::Script(i, e));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------ decoding

/// Who holds a UTXO.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Holder {
    Key([u8; 32]),
    Cov([u8; 32]),
    Unknown,
}

/// What a UTXO is.
#[derive(Clone, Debug)]
pub enum What {
    Kas,
    /// A token UTXO: token covenant id (None: unbound output), program, state.
    Token(Option<[u8; 32]>, TemplateId, TokenState),
    Order(TemplateId, AnyState),
    Intent(&'static Actor, IntentState),
    Other(String),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub value: u64,
    /// Covenant id of the UTXO (input entry) or of the output's binding.
    pub cov: Option<[u8; 32]>,
    pub holder: Holder,
    pub what: What,
}

pub fn p2pk_of(spk: &ScriptPublicKey) -> Option<[u8; 32]> {
    let s = spk.script();
    (spk.version() == 0 && s.len() == 34 && s[0] == 0x20 && s[33] == 0xac).then(|| s[1..33].try_into().unwrap())
}

pub fn token_holder(s: &TokenState) -> Holder {
    if s.is_covenant_owned() {
        Holder::Cov(s.owner())
    } else if s.is_user() {
        Holder::Key(s.owner())
    } else {
        Holder::Unknown
    }
}

fn input_item(i: &MIn) -> Item {
    let cov = i.entry.covenant_id.map(|h| h.as_bytes());
    let value = i.entry.amount;
    let (holder, what) = match &i.plan {
        SigPlan::P2pk { pubkey } => (Holder::Key(*pubkey), What::Kas),
        SigPlan::Entry { template: t, state, .. } => match AnyState::decode(*t, state) {
            Ok(s) => (cov.map(Holder::Cov).unwrap_or(Holder::Unknown), What::Order(*t, s)),
            Err(e) => (Holder::Unknown, What::Other(format!("undecodable {}: {e}", t.name()))),
        },
        SigPlan::TokenLeader { template: t, state, .. } | SigPlan::TokenDelegator { template: t, state, .. } => {
            let s = TokenState::Kcc20(state.clone());
            (token_holder(&s), What::Token(cov, *t, s))
        }
        SigPlan::KronToken { template: t, state, .. } => {
            let s = TokenState::Kron(state.clone());
            (token_holder(&s), What::Token(cov, *t, s))
        }
        SigPlan::Router { actor, state, .. } => {
            match Actor::by_name(actor).and_then(|a| IntentState::decode(a, state).ok().map(|s| (a, s))) {
                Some((a, s)) => (cov.map(Holder::Cov).unwrap_or(Holder::Unknown), What::Intent(a, s)),
                None => (Holder::Unknown, What::Other(format!("router {actor}"))),
            }
        }
        SigPlan::Retired { .. } => (cov.map(Holder::Cov).unwrap_or(Holder::Unknown), What::Other("retired".into())),
    };
    Item { value, cov, holder, what }
}

/// Decodes an output from the script public keys the library derived on this thread (and, for order
/// continuations, from the mutable windows of the order spent under the same covenant id).
fn output_item(o: &TransactionOutput, conts: &HashMap<[u8; 32], (TemplateId, AnyState, u64)>, lock: u64) -> Item {
    let cov = o.covenant.map(|c| c.covenant_id.as_bytes());
    if let Some(k) = p2pk_of(&o.script_public_key) {
        return Item { value: o.value, cov, holder: Holder::Key(k), what: What::Kas };
    }
    let found = spk_trace::lookup(&o.script_public_key).or_else(|| {
        let (t, s, daa) = conts.get(&cov?)?;
        brute_continuation(*t, s, *daa, lock, &o.script_public_key)
    });
    let (holder, what) = match found {
        Some((Origin::Template(t), state)) if t.is_token() => {
            match try_token_template(t).map(|tt| TokenState::decode_with(tt, &state)) {
                Some(Ok(s)) => (token_holder(&s), What::Token(cov, t, s)),
                _ => (Holder::Unknown, What::Other(format!("undecodable token {}", t.name()))),
            }
        }
        Some((Origin::Template(t), state)) => match AnyState::decode(t, &state) {
            Ok(s) => (cov.map(Holder::Cov).unwrap_or(Holder::Unknown), What::Order(t, s)),
            Err(e) => (Holder::Unknown, What::Other(format!("undecodable {}: {e}", t.name()))),
        },
        Some((Origin::Router(name), state)) => {
            match Actor::by_name(name).and_then(|a| IntentState::decode(a, &state).ok().map(|s| (a, s))) {
                Some((a, s)) => (cov.map(Holder::Cov).unwrap_or(Holder::Unknown), What::Intent(a, s)),
                None => (Holder::Unknown, What::Other(format!("router {name}"))),
            }
        }
        None => (Holder::Unknown, What::Other("unknown script".into())),
    };
    Item { value: o.value, cov, holder, what }
}

/// Field access over every order kind (the mutable windows and the economics the oracle needs): base units still open.
pub fn amount_left(s: &AnyState) -> Option<i64> {
    s.amount_left()
}

fn set_mut(s: &AnyState, amount: Option<i64>, armed: Option<i64>, stop: Option<i64>, rpt: Option<i64>) -> AnyState {
    let mut s = s.clone();
    macro_rules! upd {
        ($x:expr, amount) => {
            if let Some(v) = amount {
                $x.amount_left = v;
            }
        };
        ($x:expr, armed) => {
            if let Some(v) = armed {
                $x.armed = v;
            }
        };
        ($x:expr, stop) => {
            if let Some(v) = stop {
                $x.stop_price = v;
            }
        };
        ($x:expr, rpt) => {
            if let Some(v) = rpt {
                $x.rpt_amount = v;
            }
        };
    }
    match &mut s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => upd!(x, amount),
        AnyState::KobPair(x) => upd!(x, amount),
        AnyState::KobCondPair(x) => {
            upd!(x, amount);
            upd!(x, armed);
            upd!(x, stop);
        }
        AnyState::KobIfdPair(x) => {
            upd!(x, amount);
            upd!(x, armed);
            upd!(x, rpt);
        }
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => {
            upd!(x, amount);
            upd!(x, armed);
            upd!(x, stop);
        }
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => {
            upd!(x, amount);
            upd!(x, armed);
            upd!(x, stop);
        }
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => {
            upd!(x, amount);
            upd!(x, armed);
            upd!(x, rpt);
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => {
            upd!(x, amount);
            upd!(x, armed);
            upd!(x, rpt);
        }
        AnyState::KobBid(_) | AnyState::KobBidKron(_) => {}
    }
    s
}

fn armed_of(s: &AnyState) -> Option<i64> {
    match s {
        AnyState::KobCondPair(x) => Some(x.armed),
        AnyState::KobIfdPair(x) => Some(x.armed),
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => Some(x.armed),
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => Some(x.armed),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => Some(x.armed),
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => Some(x.armed),
        _ => None,
    }
}

fn stop_of(s: &AnyState) -> Option<(i64, i64)> {
    match s {
        AnyState::KobCondPair(x) => Some((x.stop_price, x.trail_step)),
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => Some((x.stop_price, x.trail_step)),
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => Some((x.stop_price, x.trail_step)),
        _ => None,
    }
}

fn rpt_of(s: &AnyState) -> Option<i64> {
    match s {
        AnyState::KobIfdPair(x) => Some(x.rpt_amount),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => Some(x.rpt_amount),
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => Some(x.rpt_amount),
        _ => None,
    }
}

/// Candidate changes of a base-unit amount `a` for the continuation search: small amounts, every eighth of `a`, and
/// steps of 250 base units (a quarter of a whole fixture token) up to 20,000.
fn amount_deltas(a: i64) -> Vec<i64> {
    let mut d: BTreeSet<i64> = (0..=64).collect();
    d.extend((1..=8).map(|k| a.max(0) / 8 * k));
    d.extend((1..=80).map(|k| 250 * k));
    d.into_iter().collect()
}

/// The exact custody of a pair order (the last mutable field), if `s` is one.
fn pair_custody(s: &AnyState) -> Option<i64> {
    match s {
        AnyState::KobPair(x) => Some(x.custody),
        AnyState::KobCondPair(x) => Some(x.custody),
        AnyState::KobIfdPair(x) => Some(x.custody),
        _ => None,
    }
}

fn set_custody(s: &AnyState, c: i64) -> AnyState {
    let mut s = s.clone();
    match &mut s {
        AnyState::KobPair(x) => x.custody = c,
        AnyState::KobCondPair(x) => x.custody = c,
        AnyState::KobIfdPair(x) => x.custody = c,
        _ => {}
    }
    s
}

/// Candidate custodies of a pair order's continuation with `l` base units of A left (the order had `left0` and
/// custody `c0`): unchanged, equal to the amount (an ask), or moved by the quote of the amount traded at any rate
/// field of the order rounded either way (a bid's release, a merge's budget / prefund).
fn pair_custody_cands(s: &AnyState, left0: i64, l: i64) -> Vec<i64> {
    let Some(c0) = pair_custody(s) else { return vec![] };
    let sc = s.scale().max(1) as i128;
    let rates: Vec<i64> = match s {
        AnyState::KobPair(x) => vec![x.price, x.price_end],
        AnyState::KobCondPair(x) => vec![x.tp_price, x.stop_price, x.rpt_price, x.rpt_pre],
        AnyState::KobIfdPair(x) => vec![x.price, x.entry_stop, x.prefund],
        _ => vec![],
    };
    let d = (left0 as i128 - l as i128).abs();
    let mut v: BTreeSet<i64> = [c0, l].into_iter().collect();
    for r in rates {
        for q in [floor_div(d * r as i128, sc), ceil_div(d * r as i128, sc)] {
            for c in [c0 as i128 - q, c0 as i128 + q] {
                if (0..=i64::MAX as i128).contains(&c) {
                    v.insert(c as i64);
                }
            }
        }
    }
    v.into_iter().collect()
}

/// Searches the continuations of `s` (template `t`, UTXO DAA `daa`) that differ in the mutable windows for one
/// with script public key `spk` (bounded: the amount moved by a candidate fill or merge, armed, stop steps, the repeat
/// amount moved with the amount or alone).
fn brute_continuation(t: TemplateId, s: &AnyState, daa: u64, lock: u64, spk: &ScriptPublicKey) -> Option<(Origin, Vec<u8>)> {
    let left0 = amount_left(s).unwrap_or(0);
    let deltas = amount_deltas(left0);
    let mut amounts: BTreeSet<i64> = BTreeSet::new();
    for &d in &deltas {
        amounts.insert(left0.saturating_sub(d).max(0));
        amounts.insert(left0.saturating_add(d));
    }
    let mut armed: Vec<i64> = vec![0, 1, daa as i64, lock as i64];
    if let Some(a) = armed_of(s) {
        armed.push(a);
    }
    let stops: Vec<i64> = match stop_of(s) {
        Some((p, step)) if step > 0 => (-60..=60i64).filter_map(|k| p.checked_add(k.checked_mul(step)?)).collect(),
        Some((p, _)) => vec![p],
        None => vec![],
    };
    let hit = |c: AnyState| (c.spk() == *spk).then(|| (Origin::Template(t), c.encode()));
    for &l in &amounts {
        if let Some(r) = hit(set_mut(s, Some(l), None, None, None)) {
            return Some(r);
        }
        for c in pair_custody_cands(s, left0, l) {
            let sc = set_custody(s, c);
            if let Some(r) = hit(set_mut(&sc, Some(l), None, None, None)) {
                return Some(r);
            }
            if armed_of(s).is_some() {
                for &a in &armed {
                    if let Some(r) = hit(set_mut(&sc, Some(l), Some(a), None, None)) {
                        return Some(r);
                    }
                }
            }
        }
        if armed_of(s).is_some() {
            for &a in &armed {
                if let Some(r) = hit(set_mut(s, Some(l), Some(a), None, None)) {
                    return Some(r);
                }
            }
        }
    }
    // a repeating entry's fill moves amountLeft and rptAmount together; a merge moves amountLeft alone
    if let Some(r0) = rpt_of(s) {
        for &d in &deltas {
            for (l, r1) in
                [(left0.saturating_sub(d), r0.saturating_sub(d)), (left0, r0.saturating_sub(d)), (left0, r0.saturating_add(d))]
            {
                for &a in &armed {
                    if let Some(r) = hit(set_mut(s, Some(l.max(0)), Some(a), None, Some(r1))) {
                        return Some(r);
                    }
                }
            }
        }
    }
    for &p in &stops {
        for &a in &armed {
            if let Some(r) = hit(set_mut(s, None, Some(a), Some(p), None)) {
                return Some(r);
            }
        }
    }
    None
}

// ------------------------------------------------------------------------------------------------ oracle

/// `floor(a / b)` for `b > 0`.
fn floor_div(a: i128, b: i128) -> i128 {
    a.div_euclid(b)
}

/// `ceil(a / b)` for `b > 0`.
fn ceil_div(a: i128, b: i128) -> i128 {
    -(-a).div_euclid(b)
}

/// How a fill of n base units of one order moves one token of its maker (the maker may end with more, never fewer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// The maker gives n base units (a sell).
    Give,
    /// The maker receives n base units (a buy).
    Take,
    /// The maker receives at least `ceil(n * rate / scale)` base units (a pair ask's quote token B, rounded UP: what a
    /// maker receives).
    TakeQuote { rate: i128, scale: i128 },
    /// The maker gives at most `floor(n * rate / scale)` base units (a pair bid's quote token B, rounded DOWN: what a
    /// maker pays).
    GiveQuote { rate: i128, scale: i128 },
}

impl Flow {
    pub fn at(&self, n: i128) -> i128 {
        match *self {
            Flow::Give => -n,
            Flow::Take => n,
            Flow::TakeQuote { rate, scale } => ceil_div(n * rate.max(0), scale.max(1)),
            Flow::GiveQuote { rate, scale } => -floor_div(n * rate.max(0), scale.max(1)),
        }
    }
    /// Non-decreasing in n (else non-increasing).
    fn rising(&self) -> bool {
        !matches!(self, Flow::Give | Flow::GiveQuote { .. })
    }
}

/// The least KAS a maker must gain for a fill of n base units (negative: the most it may pay), by the exact covenant
/// rounding in the maker's favour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owed {
    /// At least `ceil(n * rate / scale)` received (asks, conditional asks, sell-first entries).
    Receive { rate: i128, scale: i128 },
    /// At most `floor(n * rate / scale)` paid (bids, conditional bids, buy-first entries, the cross limit's KAS tip).
    Pay { rate: i128, scale: i128 },
    /// No fill is possible on these terms (the covenant refuses every fill: a price below its tip).
    Nothing,
}

/// What a fill on terms [`Owed::Nothing`] owes: more than any transaction can pay.
const NO_FILL: i128 = i128::MAX / 8;

impl Owed {
    pub fn at(&self, n: i128) -> i128 {
        match *self {
            Owed::Receive { rate, scale } => ceil_div(n * rate, scale.max(1)),
            Owed::Pay { rate, scale } => -floor_div(n * rate, scale.max(1)),
            Owed::Nothing => {
                if n > 0 {
                    NO_FILL
                } else {
                    0
                }
            }
        }
    }
}

/// What a fill of one order means for its maker: the fill is `lo..=hi` base units (exact when the order continues with
/// a smaller `amountLeft`), its token flows for the maker, and the least KAS the maker must gain, at the order's worst
/// all-in price for a fill at lock time `lock` (prices decay / rise / open their band toward the maker's worst as t
/// grows, and t <= lock time by CLTV), rounded as the covenant rounds: UP for what the maker receives, DOWN for what it
/// pays.
#[derive(Clone, Debug)]
pub struct Terms {
    pub lo: i64,
    pub hi: i64,
    pub flows: Vec<([u8; 32], Flow)>,
    pub owed: Owed,
    /// The fill amount the entry names (its first argument; 0 when it does not fill): one candidate of `lo..=hi`.
    pub hint: i64,
}

/// Rate field arithmetic in i128 (the states of a mutant carry arbitrary numbers).
type W = i128;

pub fn fill_terms(s: &AnyState, plan: &SigPlan, value: u64, daa: i64, lock: i64, cont: Option<i64>) -> Terms {
    let tok = s.token_cov_id();
    let left = amount_left(s).unwrap_or(0).max(0);
    // only a fill entry with n > 0 trades (refund, close, update, cancel and a repeat merge do not)
    let (entry, args) = match plan {
        SigPlan::Entry { entry, args, .. } => (entry.as_str(), args.as_slice()),
        _ => ("", &[][..]),
    };
    let n = match args.first() {
        Some(Arg::Bytes(b)) if b.len() == 8 => snum8(b),
        _ => 0,
    };
    let fills = (entry == "settle" || entry == "fill") && n > 0;
    let int_arg = |k: usize| match args.get(k) {
        Some(Arg::Int(v)) => Some(*v),
        _ => None,
    };
    let (lo, hi) = match cont {
        _ if !fills => (0, 0),
        Some(l2) if (0..=left).contains(&l2) => (left - l2, left - l2),
        _ => (0, left),
    };
    let scale = s.scale() as W;
    let (lock, daa) = (lock as W, daa as W);
    // decayed (down) / risen (up) quote at time t; a decay with a non-positive step cannot fill (the covenant requires
    // decayStep > 0), so its quote stays
    let origin = |af: i64, interval: i64| if interval > 0 { (af as W).max(daa + interval as W) } else { af as W };
    let moved = |price: i64, end: i64, slope: i64, step: i64, o: W, t: W, up: bool| -> W {
        if slope == 0 || step <= 0 {
            return price as W;
        }
        let k = (t - o).div_euclid(step as W) * slope as W;
        if up {
            (price as W + k).min(end as W)
        } else {
            (price as W - k).max(end as W)
        }
    };
    let band = |stop: i64, bps: i64| (stop as W * bps as W).div_euclid(10_000);
    // a seller's proceeds: the covenant requires the price of the fill (between `best` and `worst`) to be at least the
    // tip, so the least the maker receives is at max(worst, tip) - tip; no fill is possible when even `best` is below it
    let receive = |best: W, worst: W, tip: i64| -> Owed {
        let tip = tip as W;
        if best < tip {
            Owed::Nothing
        } else {
            Owed::Receive { rate: worst.max(tip) - tip, scale }
        }
    };
    let pay = |rate: W| Owed::Pay { rate, scale };
    let hint = if fills { n } else { 0 };
    // a pair order: both tokens move (rates per whole A, the scale of A), the KAS tip is paid (rounded down). Every pair
    // covenant requires the quote of a fill to be positive (p > 0, legPrice > 0): a seller's worst quote is at least 1
    // (an auction toward a non-positive limit stops paying at 1, never below), a buyer whose highest quote is not
    // positive cannot fill at all
    let pair_terms = |ask: bool, a: [u8; 32], b: [u8; 32], rate: W, tip: i64| -> Terms {
        let rate = if ask { rate.max(1) } else { rate };
        let flows = if ask {
            vec![(a, Flow::Give), (b, Flow::TakeQuote { rate, scale })]
        } else {
            vec![(a, Flow::Take), (b, Flow::GiveQuote { rate, scale })]
        };
        let owed = if rate <= 0 { Owed::Nothing } else { Owed::Pay { rate: tip as W, scale } };
        Terms { lo, hi, flows, owed, hint }
    };
    let sell = |owed: Owed| Terms { lo, hi, flows: vec![(tok, Flow::Give)], owed, hint };
    let buy = |lo: i64, hi: i64, owed: Owed| Terms { lo, hi, flows: vec![(tok, Flow::Take)], owed, hint };
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => {
            let o = origin(x.active_from, x.interval);
            let (p0, p1) = (
                moved(x.price, x.price_end, x.slope, x.decay_step, o, o, false),
                moved(x.price, x.price_end, x.slope, x.decay_step, o, lock, false),
            );
            sell(receive(p0.max(p1), p0.min(p1), x.tip))
        }
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => {
            // the leg the fill names (settle argument 3): take-profit, or the stop between its price and its band floor
            let (best, worst) = match int_arg(3) {
                Some(0) => (x.tp_price as W, x.tp_price as W),
                _ => {
                    let floor = x.stop_price as W - band(x.stop_price, x.slip_bps);
                    ((x.stop_price as W).max(floor), (x.stop_price as W).min(floor))
                }
            };
            sell(receive(best, worst, x.tip))
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => {
            // the entry quote runs between entryStop and price; the covenant requires price >= tip
            let (best, worst) = if x.entry_stop > 0 {
                (x.price.max(x.entry_stop) as W, x.price.min(x.entry_stop) as W)
            } else {
                (x.price as W, x.price as W)
            };
            sell(if x.price < x.tip { Owed::Nothing } else { receive(best, worst, x.tip) })
        }
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => {
            let p =
                moved(x.price, x.price_end, x.slope, x.decay_step, origin(x.active_from, x.interval), lock, true).max(x.price as W);
            let pmax = if x.slope != 0 && x.price_end > x.price { x.price_end } else { x.price } as W;
            // buying power: the largest n whose budget ceil(n * (pMax + tip) / scale) fits in the escrow
            let rate = pmax + x.tip as W;
            let hi = if !fills {
                0
            } else if rate > 0 {
                floor_div(value as W * scale.max(1), rate).clamp(0, i64::MAX as W / 4) as i64
            } else {
                i64::MAX / 4
            };
            buy(0, hi, pay(p + x.tip as W))
        }
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => {
            // settle argument 2: the limit leg, or the stop at its band ceiling
            let leg = match int_arg(2) {
                Some(0) => x.tp_price as W,
                _ => x.stop_price as W + band(x.stop_price, x.slip_bps),
            };
            buy(lo, hi, pay(leg + x.tip as W))
        }
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => {
            let q = if x.entry_stop > 0 { x.price.max(x.entry_stop) } else { x.price };
            buy(lo, hi, pay(q as W + x.tip as W))
        }
        AnyState::KobPair(x) => {
            // ASK: gives n of A, receives at least ceil(n * p / sA) of B at the decayed (lowest) price; BID: receives n of
            // A, pays at most floor(n * p / sA) of B at the risen (highest) price; both pay the KAS tip floor(n * tip / sA)
            let o = origin(x.active_from, x.interval);
            let t = x.tokens();
            let up = x.side == SIDE_BID;
            let (p0, p1) = (
                moved(x.price, x.price_end, x.slope, x.decay_step, o, o, up),
                moved(x.price, x.price_end, x.slope, x.decay_step, o, lock, up),
            );
            pair_terms(!up, t.a.cov_id, t.b.cov_id, if up { p0.max(p1) } else { p0.min(p1) }, x.tip)
        }
        AnyState::KobCondPair(x) => {
            // the leg the fill names (settle argument 6): take-profit / limit, or the stop at its band's worst
            let t = x.tokens();
            let ask = x.side == SIDE_ASK;
            let leg = match int_arg(6) {
                Some(0) => x.tp_price as W,
                _ if ask => (x.stop_price as W).min(x.stop_price as W - band(x.stop_price, x.slip_bps)),
                _ => (x.stop_price as W).max(x.stop_price as W + band(x.stop_price, x.slip_bps)),
            };
            pair_terms(ask, t.a.cov_id, t.b.cov_id, leg, x.tip)
        }
        AnyState::KobIfdPair(x) => {
            // sell-first (ASK) receives at least the proceeds at the lower of entryStop and price; buy-first (BID) pays at
            // most the higher of them; the A bought lands in the exit's custody, the B prefund moves into the exit's
            // custody (both the maker's)
            let ask = x.side == SIDE_ASK;
            let q = if x.entry_stop > 0 {
                if ask {
                    x.price.min(x.entry_stop)
                } else {
                    x.price.max(x.entry_stop)
                }
            } else {
                x.price
            } as W;
            pair_terms(ask, x.a_cov_id, x.b_cov_id, q, x.tip)
        }
    }
}

/// Outcome of [`least_need`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Least {
    /// The least KAS the maker must gain.
    Need(i128),
    /// No combination of fills explains the maker's token changes (tokens left the maker without fills).
    Unexplained,
    /// Too many free fill amounts to search exactly (no verdict; counted in [`UNRESOLVED`]).
    Unresolved,
}

thread_local! {
    /// Number of makers [`least_need`] could not resolve on this thread (no verdict was given for them).
    pub static UNRESOLVED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Number of makers left unvalued on this thread because one of their spent orders is a pair order with a scale
    /// that is not positive (terms only the maker can create, refused by the builders and the indexer; KobPair has no
    /// `scale > 0` check, reported as finding NP64: such an order trades on meaningless terms of its own maker).
    pub static INVALID_TERMS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Most assignments of the free fill amounts (all but the largest range, which is solved in closed form) searched
/// first, and when neither the named fill amounts nor the lower bound decide.
const SEARCH: i128 = 1 << 16;
const SEARCH_SLOW: i128 = 1 << 22;

/// The feasible sub-interval of `[lo, hi]` where the monotone `f(n) <= cap` (None: empty).
fn feasible(f: &dyn Fn(i128) -> i128, rising: bool, lo: i128, hi: i128, cap: i128) -> Option<(i128, i128)> {
    if lo > hi {
        return None;
    }
    if rising {
        if f(lo) > cap {
            return None;
        }
        let (mut a, mut b) = (lo, hi); // f(a) <= cap
        while a < b {
            let mid = a + (b - a + 1) / 2;
            if f(mid) <= cap {
                a = mid;
            } else {
                b = mid - 1;
            }
        }
        Some((lo, a))
    } else {
        if f(hi) > cap {
            return None;
        }
        let (mut a, mut b) = (lo, hi); // f(b) <= cap
        while a < b {
            let mid = a + (b - a) / 2;
            if f(mid) <= cap {
                b = mid;
            } else {
                a = mid + 1;
            }
        }
        Some((b, hi))
    }
}

/// The fills of one maker's orders with their bounds tightened ([`prepare`]).
struct Prep<'a> {
    terms: &'a [Terms],
    tokens: &'a [[u8; 32]],
    have: &'a [i128],
    /// Per term: its flow on each token.
    per: Vec<Vec<Option<Flow>>>,
    /// A term with at most one flow per token (monotone on each).
    simple: Vec<bool>,
    lo: Vec<i128>,
    hi: Vec<i128>,
}

impl Prep<'_> {
    fn flow_at(&self, i: usize, j: usize, n: i128) -> i128 {
        if self.simple[i] {
            self.per[i][j].map(|f| f.at(n)).unwrap_or(0)
        } else {
            self.terms[i].flows.iter().filter(|(t, _)| self.ix(t) == j).map(|(_, f)| f.at(n)).sum()
        }
    }
    fn ix(&self, t: &[u8; 32]) -> usize {
        self.tokens.iter().position(|x| x == t).unwrap()
    }
}

/// Bounds of every fill, tightened per token: each simple term's flow on a token is at most `have` minus the least the
/// others can flow there (only when every term is simple: the least of a non-monotone flow is not at an end of its
/// range). None when the maker's token changes cannot be explained at all.
fn prepare<'a>(terms: &'a [Terms], tokens: &'a [[u8; 32]], have: &'a [i128]) -> Option<Prep<'a>> {
    let k = tokens.len();
    let ix = |t: &[u8; 32]| tokens.iter().position(|x| x == t).unwrap();
    let mut per: Vec<Vec<Option<Flow>>> = vec![vec![None; k]; terms.len()];
    let mut simple = vec![true; terms.len()];
    for (i, t) in terms.iter().enumerate() {
        for (tok, f) in &t.flows {
            let j = ix(tok);
            if per[i][j].is_some() {
                simple[i] = false;
            }
            per[i][j] = Some(*f);
        }
    }
    let lo: Vec<i128> = terms.iter().map(|t| t.lo.max(0) as i128).collect();
    let hi: Vec<i128> = terms.iter().map(|t| (t.hi as i128).max(t.lo.max(0) as i128)).collect();
    let mut p = Prep { terms, tokens, have, per, simple, lo, hi };
    let rounds = if p.simple.iter().all(|x| *x) { 16 } else { 0 };
    for _ in 0..rounds {
        let mut changed = false;
        for j in 0..k {
            let mins: Vec<i128> = (0..terms.len()).map(|i| p.flow_at(i, j, p.lo[i]).min(p.flow_at(i, j, p.hi[i]))).collect();
            let total: i128 = mins.iter().sum();
            for i in 0..terms.len() {
                let Some(f) = p.per[i][j] else { continue };
                if p.lo[i] == p.hi[i] {
                    continue;
                }
                let cap = have[j] - (total - mins[i]);
                let (a, b) = feasible(&|n| f.at(n), f.rising(), p.lo[i], p.hi[i], cap)?;
                if (a, b) != (p.lo[i], p.hi[i]) {
                    (p.lo[i], p.hi[i]) = (a, b);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    Some(p)
}

/// Exact least: every assignment of the free amounts but one (the largest simple range, solved in closed form: each
/// flow and each owed amount is monotone in its fill). None when more than `budget` assignments would be searched.
fn search(p: &Prep, budget: i128) -> Option<Least> {
    let (terms, have) = (p.terms, p.have);
    let k = have.len();
    let free: Vec<usize> = (0..terms.len()).filter(|&i| p.lo[i] < p.hi[i]).collect();
    let last = free.iter().copied().filter(|&i| p.simple[i]).max_by_key(|&i| p.hi[i] - p.lo[i]);
    let searched: Vec<usize> = free.iter().copied().filter(|&i| Some(i) != last).collect();
    let mut work: i128 = 1;
    for &i in &searched {
        work = work.saturating_mul(p.hi[i] - p.lo[i] + 1);
    }
    if work > budget {
        return None;
    }
    let mut base_flow = vec![0i128; k];
    let mut base_owed: i128 = 0;
    for i in 0..terms.len() {
        if p.lo[i] == p.hi[i] {
            for (j, bf) in base_flow.iter_mut().enumerate() {
                *bf += p.flow_at(i, j, p.lo[i]);
            }
            base_owed += terms[i].owed.at(p.lo[i]);
        }
    }
    let mut best: Option<i128> = None;
    let mut cur: Vec<i128> = searched.iter().map(|&i| p.lo[i]).collect();
    loop {
        let mut flow = base_flow.clone();
        let mut owed = base_owed;
        for (q, &i) in searched.iter().enumerate() {
            for (j, fl) in flow.iter_mut().enumerate() {
                *fl += p.flow_at(i, j, cur[q]);
            }
            owed += terms[i].owed.at(cur[q]);
        }
        let cand = match last {
            None => flow.iter().zip(have).all(|(a, b)| a <= b).then_some(owed),
            Some(l) => {
                // the closed-form term: its feasible interval under every token, then the cheaper end
                let mut iv = Some((p.lo[l], p.hi[l]));
                for j in 0..k {
                    let Some((a, b)) = iv else { break };
                    match p.per[l][j] {
                        Some(f) => iv = feasible(&|n| f.at(n), f.rising(), a, b, have[j] - flow[j]),
                        None if flow[j] > have[j] => iv = None,
                        None => {}
                    }
                }
                iv.map(|(a, b)| owed + terms[l].owed.at(a).min(terms[l].owed.at(b)))
            }
        };
        if let Some(c) = cand {
            best = Some(best.map_or(c, |b| b.min(c)));
        }
        let mut q = 0;
        loop {
            if q == searched.len() {
                return Some(best.map_or(Least::Unexplained, Least::Need));
            }
            let i = searched[q];
            if cur[q] < p.hi[i] {
                cur[q] += 1;
                break;
            }
            cur[q] = p.lo[i];
            q += 1;
        }
    }
}

/// What the maker is owed at the fill amounts its entries name (each clamped into its bounds), if those fills explain
/// its token changes: an upper bound of the least.
fn at_hints(p: &Prep) -> Option<i128> {
    let k = p.have.len();
    let mut flow = vec![0i128; k];
    let mut owed = 0i128;
    for (i, t) in p.terms.iter().enumerate() {
        let n = (t.hint as i128).clamp(p.lo[i], p.hi[i]);
        for (j, fl) in flow.iter_mut().enumerate() {
            *fl += p.flow_at(i, j, n);
        }
        owed += t.owed.at(n);
    }
    flow.iter().zip(p.have).all(|(a, b)| a <= b).then_some(owed)
}

/// A lower bound of the least (Lagrangian relaxation of the token constraints with every cost and flow replaced by its
/// exact linear value, which the covenant rounding only moves in the maker's favour: ceil >= x for what the maker
/// receives, -floor >= -x for what it pays). The multiplier of each token is tried at 0 and at every breakpoint of
/// the terms that flow on it (single token: the LP optimum). In f64 with a safety margin.
fn lower_bound(p: &Prep) -> i128 {
    let (terms, have) = (p.terms, p.have);
    let k = have.len();
    let unit = |o: &Owed| -> f64 {
        match *o {
            Owed::Receive { rate, scale } => rate as f64 / scale.max(1) as f64,
            Owed::Pay { rate, scale } => -(rate as f64) / scale.max(1) as f64,
            Owed::Nothing => NO_FILL as f64,
        }
    };
    let coef = |f: &Flow| -> f64 {
        match *f {
            Flow::Give => -1.0,
            Flow::Take => 1.0,
            Flow::TakeQuote { rate, scale } => rate.max(0) as f64 / scale.max(1) as f64,
            Flow::GiveQuote { rate, scale } => -(rate.max(0) as f64) / scale.max(1) as f64,
        }
    };
    // candidate multipliers per token
    let mut cands: Vec<Vec<f64>> = vec![vec![0.0]; k];
    for t in terms {
        for (tok, f) in &t.flows {
            let j = p.ix(tok);
            let a = coef(f);
            if a != 0.0 {
                let l = -unit(&t.owed) / a;
                if l > 0.0 && l.is_finite() && cands[j].len() < 64 {
                    cands[j].push(l);
                }
            }
        }
    }
    let mut best = f64::NEG_INFINITY;
    let mut idx = vec![0usize; k];
    let mut magnitude = 0f64;
    loop {
        let lam: Vec<f64> = (0..k).map(|j| cands[j][idx[j]]).collect();
        let mut g = 0f64;
        for (i, t) in terms.iter().enumerate() {
            let mut c = unit(&t.owed);
            for (tok, f) in &t.flows {
                c += lam[p.ix(tok)] * coef(f);
            }
            let (a, b) = (p.lo[i] as f64, p.hi[i] as f64);
            let v = (c * a).min(c * b);
            g += v;
            magnitude += v.abs();
        }
        for j in 0..k {
            g -= lam[j] * have[j] as f64;
            magnitude += (lam[j] * have[j] as f64).abs();
        }
        best = best.max(g);
        // next combination
        let mut q = 0;
        loop {
            if q == k {
                let margin = 2.0 + magnitude * 1e-9;
                return if best.is_finite() { (best - margin).floor().clamp(-1e36, 1e36) as i128 } else { i128::MIN / 4 };
            }
            if idx[q] + 1 < cands[q].len() {
                idx[q] += 1;
                break;
            }
            idx[q] = 0;
            q += 1;
        }
    }
}

/// What a maker must gain, decided against `got` (what the maker gained, allowances included).
///
/// * At the fill amounts its entries NAME (their first argument), when those fills explain the maker's token changes
///   (`flows <= have` per token: the maker may end with more tokens, never fewer): each order's covenant guarantees its
///   maker at least its terms at its own fill amount, so the maker must gain at least their sum.
/// * Otherwise, the least over EVERY combination of fills of its orders (each `lo..=hi` base units) whose flows the token
///   changes cover: exact whenever the free amounts can be searched; else short when `got` is below a lower bound of the
///   least, the search retried with a larger budget in between, and no verdict when that is too large too
///   ([`Least::Unresolved`]).
pub fn least_need(terms: &[Terms], tokens: &[[u8; 32]], have: &[i128], got: i128) -> Least {
    let Some(p) = prepare(terms, tokens, have) else { return Least::Unexplained };
    if let Some(u) = at_hints(&p) {
        return Least::Need(u);
    }
    if let Some(l) = search(&p, SEARCH) {
        return l;
    }
    let l = lower_bound(&p);
    if got < l {
        return Least::Need(l);
    }
    if let Some(l) = search(&p, SEARCH_SLOW) {
        return l;
    }
    UNRESOLVED.with(|u| u.set(u.get() + 1));
    Least::Unresolved
}

/// What one entry of an order input may cost its maker besides its fills (refund and keeper tips).
fn allowance(s: &AnyState, plan: &SigPlan) -> i128 {
    let SigPlan::Entry { entry, args, .. } = plan else { return 0 };
    let n = match args.first() {
        Some(Arg::Bytes(b)) if b.len() == 8 => Some(snum8(b)),
        _ => None,
    };
    let refund_tip = s.expiry().map(|_| ()).and_then(|_| refund_tip_of(s)).unwrap_or(0) as i128;
    // pair conditionals and entries: n == 0 refunds (upd 0) or updates (upd 1: the keeper tip); upd is KobCondPair
    // settle argument 11, KobIfdPair fill argument 17
    let upd = match (s, args.get(11), args.get(17)) {
        (AnyState::KobCondPair(_), Some(Arg::Int(u)), _) => *u,
        (AnyState::KobIfdPair(_), _, Some(Arg::Int(u))) => *u,
        _ => 0,
    };
    if upd != 0 && n == Some(0) {
        return s.keeper_tip().unwrap_or(0) as i128;
    }
    match entry.as_str() {
        "refund" | "close" | "expire" => refund_tip,
        "settle" | "fill" if n == Some(0) => refund_tip,
        "update" => s.keeper_tip().unwrap_or(0) as i128,
        _ => 0,
    }
}

fn refund_tip_of(s: &AnyState) -> Option<i64> {
    Some(match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.refund_tip,
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.refund_tip,
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.refund_tip,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.refund_tip,
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => x.refund_tip,
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => x.refund_tip,
        AnyState::KobPair(x) => x.refund_tip,
        AnyState::KobCondPair(x) => x.refund_tip,
        AnyState::KobIfdPair(x) => x.refund_tip,
    })
}

/// One oracle violation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// Stable class (deduplication, minimisation target).
    pub kind: String,
    pub detail: String,
}

#[derive(Default, Clone, Debug)]
struct Book {
    kas: i128,
    tok: BTreeMap<[u8; 32], i128>,
}

fn short(b: &[u8; 32]) -> String {
    b[..4].iter().map(|x| format!("{x:02x}")).collect()
}

/// Every violation of the value / state invariants by an accepted mutant.
pub fn oracle(m: &MTx) -> Vec<Finding> {
    let mut out = vec![];
    let mut f = |kind: String, detail: String| out.push(Finding { kind, detail });
    let lock = m.lock_time as i64;
    let ins: Vec<Item> = m.ins.iter().map(input_item).collect();
    // order covenants spent here (for the continuation search)
    let mut conts: HashMap<[u8; 32], (TemplateId, AnyState, u64)> = HashMap::new();
    for (k, it) in ins.iter().enumerate() {
        if let (Some(c), What::Order(t, s)) = (it.cov, &it.what) {
            conts.insert(c, (*t, s.clone(), m.ins[k].entry.block_daa_score));
        }
    }
    let outs: Vec<Item> = m.outs.iter().map(|o| output_item(o, &conts, m.lock_time)).collect();

    // signers consent to everything (SIGHASH_ALL)
    let signers: BTreeSet<[u8; 32]> = m.ins.iter().filter_map(|i| i.plan.signer()).collect();

    // maker of each covenant id (orders) and payer of each intent
    let mut cov_owner: HashMap<[u8; 32], [u8; 32]> = HashMap::new();
    // the tokens each covenant id trades (its own tokens): an order's token, a cross limit's A and B, an intent's tokens
    let mut cov_tokens: HashMap<[u8; 32], BTreeSet<[u8; 32]>> = HashMap::new();
    for it in ins.iter().chain(outs.iter()) {
        match (&it.cov, &it.what) {
            (Some(c), What::Order(_, s)) => {
                cov_owner.entry(*c).or_insert(s.maker());
                let e = cov_tokens.entry(*c).or_default();
                e.insert(s.token_cov_id());
                if let Some(p) = s.pair_tokens() {
                    e.insert(p.a.cov_id);
                    e.insert(p.b.cov_id);
                }
            }
            (Some(c), What::Intent(_, s)) => {
                cov_owner.entry(*c).or_insert(s.payer());
                let e = cov_tokens.entry(*c).or_default();
                e.extend(s.locked_token());
                e.extend(s.merchant_token());
            }
            _ => {}
        }
    }
    // A token UTXO of ANOTHER token owned by an order (a foreign-token stray) is documented as unprotected
    // (`docs/spec/order-types.md` "Custody": every spend of the order authorises it, whoever builds the transaction may
    // move it): it is nobody's in the books.
    let foreign_stray = |it: &Item| match (&it.holder, &it.what) {
        (Holder::Cov(c), What::Token(Some(t), _, _)) => cov_tokens.get(c).is_some_and(|ts| !ts.contains(t)),
        _ => false,
    };
    let party = |h: Holder| -> Option<[u8; 32]> {
        match h {
            Holder::Key(k) => Some(k),
            Holder::Cov(c) => cov_owner.get(&c).copied(),
            Holder::Unknown => None,
        }
    };

    // ---- token supply per token covenant id; unknown outputs bound to a token
    let mut supply: BTreeMap<[u8; 32], (i128, i128)> = BTreeMap::new();
    for it in &ins {
        if let What::Token(Some(t), _, s) = &it.what {
            supply.entry(*t).or_default().0 += s.amount() as i128;
        }
    }
    for (j, it) in outs.iter().enumerate() {
        match &it.what {
            // a token script bound to a covenant id no token input carries is not a token (no program authorises it)
            What::Token(Some(t), _, s) if supply.contains_key(t) => supply.entry(*t).or_default().1 += s.amount() as i128,
            What::Other(why) if it.cov.is_some() => {
                let c = it.cov.unwrap();
                if supply.contains_key(&c) || ins.iter().any(|i| matches!(&i.what, What::Token(Some(t), _, _) if *t == c)) {
                    f("token.unknownOutput".into(), format!("output {j} bound to token {}: {why}", short(&c)));
                } else if conts.contains_key(&c) {
                    f("order.unknownContinuation".into(), format!("output {j} bound to order {}: {why}", short(&c)));
                }
            }
            _ => {}
        }
    }
    for (t, (i, o)) in &supply {
        if i != o {
            f("token.supply".into(), format!("token {}: inputs {i} != outputs {o}", short(t)));
        }
    }

    // ---- party books
    let mut books: BTreeMap<[u8; 32], Book> = BTreeMap::new();
    for (sign, items) in [(-1i128, &ins), (1i128, &outs)] {
        for it in items.iter() {
            if foreign_stray(it) {
                continue;
            }
            let Some(p) = party(it.holder) else { continue };
            let b = books.entry(p).or_default();
            b.kas += sign * it.value as i128;
            if let What::Token(Some(t), _, s) = &it.what {
                if !supply.contains_key(t) {
                    continue;
                }
                *b.tok.entry(*t).or_default() += sign * s.amount() as i128;
            }
        }
    }

    // ---- makers: orders spent without the maker's signature
    let mut maker_orders: BTreeMap<[u8; 32], Vec<usize>> = BTreeMap::new();
    for (k, it) in ins.iter().enumerate() {
        if let What::Order(_, s) = &it.what {
            maker_orders.entry(s.maker()).or_default().push(k);
        }
    }
    for (mk, idx) in &maker_orders {
        if signers.contains(mk) {
            continue;
        }
        let invalid = idx.iter().any(|&k| match &ins[k].what {
            What::Order(_, s) => s.pair_tokens().is_some_and(|p| p.a.scale <= 0 || p.b.scale <= 0),
            _ => false,
        });
        if invalid {
            INVALID_TERMS.with(|u| u.set(u.get() + 1));
            continue;
        }
        let b = books.get(mk).cloned().unwrap_or_default();
        let allow: i128 =
            idx.iter().map(|&k| if let What::Order(_, s) = &ins[k].what { allowance(s, &m.ins[k].plan) } else { 0 }).sum();
        // every order's fill: base units in [lo, hi], the maker's token flows and the least KAS it must gain
        let mut terms: Vec<Terms> = vec![];
        for &k in idx {
            let (Some(c), What::Order(_, s)) = (ins[k].cov, &ins[k].what) else { continue };
            let cont = (0..outs.len()).find(|&j| outs[j].cov == Some(c)).and_then(|j| match &outs[j].what {
                What::Order(_, s2) => amount_left(s2),
                _ => None,
            });
            terms.push(fill_terms(s, &m.ins[k].plan, m.ins[k].entry.amount, m.ins[k].entry.block_daa_score as i64, lock, cont));
        }
        let tokens: Vec<[u8; 32]> = {
            let mut t: BTreeSet<[u8; 32]> = BTreeSet::new();
            for x in &terms {
                t.extend(x.flows.iter().map(|f| f.0));
            }
            t.into_iter().collect()
        };
        let have: Vec<i128> = tokens.iter().map(|t| b.tok.get(t).copied().unwrap_or(0)).collect();
        match least_need(&terms, &tokens, &have, b.kas + allow) {
            Least::Unexplained => f(
                "maker.tokens".into(),
                format!("maker {}: token changes {:?} not explained by fills of its {} orders", short(mk), have, terms.len()),
            ),
            Least::Need(need) if b.kas + allow < need => f(
                "maker.value".into(),
                format!("maker {}: KAS {} + allowance {allow} < least owed {need} (tokens {:?})", short(mk), b.kas, have),
            ),
            _ => {}
        }
    }

    // ---- intents executed / expired without the payer's signature
    for (k, it) in ins.iter().enumerate() {
        let What::Intent(_, s) = &it.what else { continue };
        if signers.contains(&s.payer()) {
            continue;
        }
        let entry = match &m.ins[k].plan {
            SigPlan::Router { entry, .. } => entry.clone(),
            _ => String::new(),
        };
        let pb = books.get(&s.payer()).cloned().unwrap_or_default();
        let mb = books.get(&s.merchant()).cloned().unwrap_or_default();
        if entry == "expire" {
            if pb.kas + (EXPIRE_MAX_FEE as i128) < 0 || pb.tok.values().any(|d| *d < 0) {
                f("intent.expire".into(), format!("payer {}: KAS {} tokens {:?}", short(&s.payer()), pb.kas, pb.tok));
            }
            continue;
        }
        match s {
            IntentState::KasToToken { token, amount, max_pay, max_extra, .. } => {
                if mb.tok.get(token).copied().unwrap_or(0) < *amount as i128 {
                    f("intent.merchant".into(), format!("merchant got {:?} of {amount}", mb.tok.get(token)));
                }
                if pb.kas + (*max_pay as i128) + (*max_extra as i128) < 0 {
                    f("intent.payer".into(), format!("payer KAS {} beyond {max_pay} + {max_extra}", pb.kas));
                }
            }
            IntentState::TokenToKas { token, merchant_kas, max_sell, .. } => {
                if mb.kas < *merchant_kas as i128 {
                    f("intent.merchant".into(), format!("merchant got {} KAS of {merchant_kas}", mb.kas));
                }
                if pb.tok.get(token).copied().unwrap_or(0) + (*max_sell as i128) < 0 {
                    f("intent.payer".into(), format!("payer sold {:?} beyond {max_sell}", pb.tok.get(token)));
                }
            }
            IntentState::TokenSwap { token_a, token_b, max_sell_a, amount_b, .. } => {
                if mb.tok.get(token_b).copied().unwrap_or(0) < *amount_b as i128 {
                    f("intent.merchant".into(), format!("merchant got {:?} B of {amount_b}", mb.tok.get(token_b)));
                }
                if pb.tok.get(token_a).copied().unwrap_or(0) + (*max_sell_a as i128) < 0 {
                    f("intent.payer".into(), format!("payer sold {:?} beyond {max_sell_a}", pb.tok.get(token_a)));
                }
            }
        }
    }

    // ---- order covenant continuations
    for (k, it) in ins.iter().enumerate() {
        let (Some(c), What::Order(t, s)) = (it.cov, &it.what) else { continue };
        if signers.contains(&s.maker()) {
            continue;
        }
        let bound: Vec<usize> = (0..outs.len()).filter(|&j| outs[j].cov == Some(c)).collect();
        if bound.len() > 1 {
            f("order.split".into(), format!("order {} ({}) continues in {} outputs", short(&c), t.name(), bound.len()));
            continue;
        }
        let Some(&j) = bound.first() else { continue };
        let What::Order(t2, s2) = &outs[j].what else {
            if !matches!(outs[j].what, What::Other(_)) {
                f("order.continuationKind".into(), format!("order {} continues as {:?}", short(&c), outs[j].holder));
            }
            continue;
        };
        if t2 != t {
            f("order.template".into(), format!("order {} {} continues as {}", short(&c), t.name(), t2.name()));
            continue;
        }
        // immutable bytes
        let (r1, r2) = (template(*t).redeem(&s.encode()), template(*t).redeem(&s2.encode()));
        let mut masked = vec![false; r1.len()];
        for &(_, a, b) in mutable_windows(*t) {
            for x in masked.iter_mut().take(b).skip(a) {
                *x = true;
            }
        }
        if let Some(i) = (0..r1.len()).find(|&i| !masked[i] && r1[i] != r2[i]) {
            f("order.immutable".into(), format!("order {} {}: redeem byte {i} changed", short(&c), t.name()));
        }
        // monotone transitions
        let merged = m.ins.iter().zip(&ins).any(|(_, x)| match &x.what {
            What::Order(_, AnyState::KobCondAsk(e) | AnyState::KobCondAskKron(e)) => e.parent == c,
            What::Order(_, AnyState::KobCondBid(e) | AnyState::KobCondBidKron(e)) => e.parent == c,
            What::Order(_, AnyState::KobCondPair(e)) => e.parent == c,
            _ => false,
        });
        if let (Some(l1), Some(l2)) = (amount_left(s), amount_left(s2)) {
            if l2 > l1 && !merged {
                f("order.amountUp".into(), format!("order {} {}: amountLeft {l1} -> {l2}", short(&c), t.name()));
            }
        }
        match (s, s2) {
            (AnyState::KobCondAsk(a) | AnyState::KobCondAskKron(a), AnyState::KobCondAsk(b) | AnyState::KobCondAskKron(b)) => {
                if b.stop_price < a.stop_price {
                    f("order.stopDown".into(), format!("cond ask {} stop {} -> {}", short(&c), a.stop_price, b.stop_price));
                }
            }
            (AnyState::KobCondBid(a) | AnyState::KobCondBidKron(a), AnyState::KobCondBid(b) | AnyState::KobCondBidKron(b)) => {
                if b.stop_price > a.stop_price {
                    f("order.stopUp".into(), format!("cond bid {} stop {} -> {}", short(&c), a.stop_price, b.stop_price));
                }
            }
            (AnyState::KobCondPair(a), AnyState::KobCondPair(b)) => {
                // a sell stop (ASK) only ratchets up, a buy stop (BID) only down
                if (a.side == SIDE_ASK && b.stop_price < a.stop_price) || (a.side == SIDE_BID && b.stop_price > a.stop_price) {
                    f(
                        "order.stopWrongWay".into(),
                        format!("cond pair {} side {} stop {} -> {}", short(&c), a.side, a.stop_price, b.stop_price),
                    );
                }
            }
            _ => {}
        }
        if let (Some(0), Some(a2)) = (armed_of(s), armed_of(s2)) {
            if a2 != 0 {
                // arming needs a fill of a plain order of the same token next to it (a pair order: fills of plain KAS-book
                // orders of both its tokens, or a fill of a KobPair of the same pair)
                let tok = s.token_cov_id();
                let filled = |mi: &MIn| {
                    matches!(&mi.plan, SigPlan::Entry { entry, args, .. }
                        if (entry == "settle" || entry == "fill")
                            && matches!(args.first(), Some(Arg::Bytes(b)) if b.len() == 8 && snum8(b) > 0))
                };
                let kas_book = |t: &[u8; 32]| {
                    m.ins.iter().zip(&ins).any(|(mi, x)| {
                        filled(mi)
                            && matches!(&x.what, What::Order(_, e @ (AnyState::KobAsk(_) | AnyState::KobAskKron(_) | AnyState::KobBid(_) | AnyState::KobBidKron(_))) if e.token_cov_id() == *t)
                    })
                };
                let pair_evidence = s.pair_tokens().map(|p| {
                    (kas_book(&p.a.cov_id) && kas_book(&p.b.cov_id))
                        || m.ins.iter().zip(&ins).any(|(mi, x)| {
                            filled(mi)
                                && matches!(&x.what, What::Order(_, e @ AnyState::KobPair(_))
                                    if e.pair_tokens().is_some_and(|q| q.a.cov_id == p.a.cov_id && q.b.cov_id == p.b.cov_id))
                        })
                });
                let evidence = pair_evidence.unwrap_or(false)
                    || pair_evidence.is_none()
                        && m.ins.iter().zip(&ins).any(|(mi, x)| match (&mi.plan, &x.what) {
                            (
                                SigPlan::Entry { entry, args, .. },
                                What::Order(
                                    _,
                                    e
                                    @ (AnyState::KobAsk(_) | AnyState::KobAskKron(_) | AnyState::KobBid(_) | AnyState::KobBidKron(_)),
                                ),
                            ) => {
                                (entry == "settle" || entry == "fill")
                                    && e.token_cov_id() == tok
                                    && matches!(args.first(), Some(Arg::Bytes(b)) if b.len() == 8 && snum8(b) > 0)
                            }
                            _ => false,
                        });
                if !evidence {
                    f(
                        "order.armedWithoutTouch".into(),
                        format!("order {} {} armed {a2} without a fill of a plain order", short(&c), t.name()),
                    );
                }
            }
        }
        let _ = k;
    }
    out
}

/// An 8-byte script number (little endian, sign-magnitude: the top bit of the last byte is the sign), as the
/// covenants read a fixed 8-byte push (`int(byte[8])`).
pub fn snum8(b: &[u8]) -> i64 {
    let mut x: [u8; 8] = b[..8].try_into().unwrap();
    let neg = x[7] & 0x80 != 0;
    x[7] &= 0x7f;
    let v = i64::from_le_bytes(x);
    if neg {
        -v
    } else {
        v
    }
}

/// Inverse of [`snum8`] (`i64::MIN` has no 8-byte sign-magnitude encoding and saturates).
pub fn enc8(v: i64) -> Vec<u8> {
    let mut x = v.unsigned_abs().min(i64::MAX as u64).to_le_bytes();
    if v < 0 {
        x[7] |= 0x80;
    }
    x.to_vec()
}

thread_local! {
    /// Ablation canary: the order template whose input scripts [`fuzz::evaluate`] ignores (None in every real run).
    pub static ABLATE: std::cell::Cell<Option<TemplateId>> = const { std::cell::Cell::new(None) };
}
