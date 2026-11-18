//! Red-team harness: honest engine-built transactions, one mutation per attack, judged by the rusty-kaspa v2.1.0 script
//! engine (every input script with an unlimited meter, the covenant context, amounts, the storage-mass commitment and
//! the block mass limits). Mempool policy (fee floor, standardness) is not applied: consensus only.
//!
//! The attack patterns follow the kaspa-redteam 0.17.2 library (MIT, Kaspero Labs; the ids are theirs, this code is
//! KOB's own): `OUT-01..04`, `FEE-01`, `IN-01..02`, `SIG-01..02`, `FAM-10`, `BOR-01`, plus KOB's token-amount and
//! delivery attacks (`TOK-*`, `DLV-*`).
//!
//! Seats: the attacker builds the transaction and holds the keys of the taker, matcher, keeper and keeper (intents)
//! seats and its own key; every signature of those keys is made again after each change. Any other signature (a
//! maker's) is kept as it was, so a change it commits to breaks it.
//!
//! An accepted attempt is a note only by an explicit rule of [`classify`] (the attack's own note rule, or the
//! allow-list [`ALLOW`]); anything else accepted is a finding, written as a replay file.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::router::{Actor, IntentState};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use serde_json::{json, Value};

/// The attacker's own key (a fixture key no scenario uses).
pub const ATTACKER: u8 = 37;
/// Seats attacked from: taker, matcher, keeper, keeper (intents), and the attacker.
pub const SEATS: [u8; 5] = [4, 5, 6, 12, ATTACKER];
/// Units of a stray token UTXO (FAM-10).
pub const STRAY_UNITS: i64 = 1_000_000;

pub fn sk(n: u8) -> [u8; 32] {
    [n; 32]
}
pub fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&sk(n)).unwrap()
}
pub fn attacker() -> [u8; 32] {
    pk(ATTACKER)
}
pub fn seat_pks() -> BTreeSet<[u8; 32]> {
    SEATS.iter().map(|n| pk(*n)).collect()
}
fn secret_of(p: &[u8; 32]) -> Option<[u8; 32]> {
    (1..=40u8).find(|n| pk(*n) == *p).map(sk)
}

// ---------------------------------------------------------------- the mutable transaction

/// Where an input's signature comes from when the attempt is judged.
#[derive(Clone, Debug)]
pub enum SigSrc {
    None,
    /// Made again by a seat key over the attempt.
    Fresh,
    /// The honest signature (a key the attacker does not hold).
    Stale(Vec<u8>),
    /// 64 zero bytes + SIGHASH_ALL (SIG-01).
    Garbage,
}

/// A transaction under attack: the unsigned transaction with its UTXO entries, each input's plan and signature source.
#[derive(Clone, Debug)]
pub struct Mtx {
    pub tx: TxJson,
    pub plans: Vec<SigPlan>,
    pub sigs: Vec<SigSrc>,
}

impl Mtx {
    /// The honest transaction: seat signatures fresh, every other signature as the honest one.
    pub fn from_built(b: &BuiltTx) -> Mtx {
        let seats = seat_pks();
        let (tx, entries) = b.tx.to_tx().expect("honest tx");
        let sigs = b
            .plans
            .iter()
            .enumerate()
            .map(|(i, p)| match p.signer() {
                None => SigSrc::None,
                Some(k) if seats.contains(&k) => SigSrc::Fresh,
                Some(k) => {
                    let s = secret_of(&k).expect("a fixture key");
                    SigSrc::Stale(sign_digest(&s, &sighash(&tx, &entries, i)).unwrap())
                }
            })
            .collect();
        Mtx { tx: b.tx.clone(), plans: b.plans.clone(), sigs }
    }

    pub fn input_total(&self) -> u64 {
        self.tx.inputs.iter().map(|i| i.utxo.amount).sum()
    }
    pub fn output_total(&self) -> u64 {
        self.tx.outputs.iter().map(|o| o.value).sum()
    }
    pub fn cov_in(&self, i: usize) -> Option<[u8; 32]> {
        self.tx.inputs[i].utxo.covenant_id
    }

    /// Inserts an output at `j`, shifting nothing but later outputs (the KCC-20 leader's next states follow).
    pub fn insert_output(&mut self, j: usize, o: TxOutputJson, token: Option<TokenState>) {
        // the token output's place among its token's outputs
        let pos_info = o.covenant.as_ref().map(|c| (c.covenant_id, c.authorizing_input));
        self.tx.outputs.insert(j, o);
        if let (Some((cid, a)), Some(st)) = (pos_info, token) {
            let pos = self.tx.outputs[..j]
                .iter()
                .filter(|x| x.covenant.as_ref().is_some_and(|c| c.covenant_id == cid && self.group_match(c, a)))
                .count();
            for (i, p) in self.plans.iter_mut().enumerate() {
                match p {
                    SigPlan::TokenLeader { next_states, .. } if i == a as usize => {
                        if let TokenState::Kcc20(k) = &st {
                            next_states.insert(pos, k.clone());
                        }
                    }
                    SigPlan::KronToken { next_states, .. } if self.tx.inputs[i].utxo.covenant_id == Some(cid) => {
                        if let TokenState::Kron(k) = &st {
                            next_states.insert(pos, k.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn group_match(&self, c: &CovenantJson, a: u16) -> bool {
        // KCC-20: the leader's outputs; KRON: every output of the token
        match self.plans.get(a as usize) {
            Some(SigPlan::KronToken { .. }) => true,
            _ => c.authorizing_input == a,
        }
    }

    /// Inserts an input at `i` (covenant bindings, and nothing else, follow).
    pub fn insert_input(&mut self, i: usize, inp: TxInputJson, plan: SigPlan, sig: SigSrc) {
        self.tx.inputs.insert(i, inp);
        self.plans.insert(i, plan);
        self.sigs.insert(i, sig);
        for o in self.tx.outputs.iter_mut() {
            if let Some(c) = o.covenant.as_mut() {
                if c.authorizing_input as usize >= i {
                    c.authorizing_input += 1;
                }
            }
        }
    }

    /// Appends an input.
    pub fn push_input(&mut self, inp: TxInputJson, plan: SigPlan, sig: SigSrc) {
        let i = self.tx.inputs.len();
        self.insert_input(i, inp, plan, sig);
    }

    /// Removes input `i` (bindings of later inputs shift down; outputs it authorised are left unbound to it).
    pub fn remove_input(&mut self, i: usize) {
        self.tx.inputs.remove(i);
        self.plans.remove(i);
        self.sigs.remove(i);
        for o in self.tx.outputs.iter_mut() {
            if let Some(c) = o.covenant.as_mut() {
                if c.authorizing_input as usize > i {
                    c.authorizing_input -= 1;
                }
            }
        }
    }
}

/// Adds an attacker P2PK input covering what the outputs exceed the inputs by (plus 0.01 KAS of fee).
pub fn fund_deficit(m: &mut Mtx) {
    let (i, o) = (m.input_total(), m.output_total());
    if o + 1_000_000 > i {
        let (inp, plan) = attacker_input(0xc8, o + 1_000_000 - i);
        m.push_input(inp, plan, SigSrc::Fresh);
    }
}

/// An attacker P2PK input of `value` sompi (outpoint tag `tag`).
pub fn attacker_input(tag: u8, value: u64) -> (TxInputJson, SigPlan) {
    let k = attacker();
    (
        TxInputJson {
            transaction_id: [tag; 32],
            index: 7_000 + tag as u32,
            sequence: 0,
            sig_op_count: 1,
            compute_budget: 0,
            signature_script: vec![],
            utxo: UtxoJson {
                address: None,
                amount: value,
                script_public_key: spk_to_string(&p2pk_spk(&k)),
                block_daa_score: 500,
                is_coinbase: false,
                covenant_id: None,
            },
        },
        SigPlan::P2pk { pubkey: k },
    )
}

pub fn p2pk_out(key: &[u8; 32], value: u64) -> TxOutputJson {
    TxOutputJson { value, script_public_key: spk_to_string(&p2pk_spk(key)), covenant: None }
}

// ---------------------------------------------------------------- judging

/// The engine's verdict on an attempt.
#[derive(Clone, Debug)]
pub enum Verdict {
    Accepted,
    /// Refused: the failing inputs (index, error), or a transaction-level rule.
    Refused {
        inputs: Vec<(usize, String)>,
        rule: Option<String>,
    },
    /// The attempt could not be assembled (a plan refused its own arguments): not a result.
    NotBuilt(String),
}

impl Verdict {
    pub fn accepted(&self) -> bool {
        matches!(self, Verdict::Accepted)
    }
    pub fn short(&self) -> String {
        match self {
            Verdict::Accepted => "accepted".into(),
            Verdict::Refused { inputs, rule: Some(r) } if inputs.is_empty() => format!("refused: {r}"),
            Verdict::Refused { inputs, .. } => {
                let (i, e) = &inputs[0];
                let e: String = e.chars().take(160).collect();
                format!("refused at input {i} ({} failing): {e}", inputs.len())
            }
            Verdict::NotBuilt(e) => format!("not built: {e}"),
        }
    }
}

/// Assembles the attempt (fresh seat signatures, stale or garbage others) into a consensus transaction.
pub fn assemble_attempt(m: &Mtx) -> Result<(kaspa_consensus_core::tx::Transaction, Vec<kaspa_consensus_core::tx::UtxoEntry>), String> {
    let (mut tx, entries) = m.tx.to_tx().map_err(|e| e.to_string())?;
    for i in 0..tx.inputs.len() {
        tx.inputs[i].signature_script = vec![];
    }
    let mut sigs: Vec<Option<Vec<u8>>> = vec![None; tx.inputs.len()];
    for (i, s) in m.sigs.iter().enumerate() {
        sigs[i] = match s {
            SigSrc::None => None,
            SigSrc::Stale(v) => Some(v.clone()),
            SigSrc::Garbage => Some([vec![0u8; 64], vec![0x01u8]].concat()),
            SigSrc::Fresh => {
                let k = m.plans[i].signer().ok_or("fresh signature on an input that takes none")?;
                let s = secret_of(&k).ok_or("no fixture key")?;
                Some(sign_digest(&s, &sighash(&tx, &entries, i)).map_err(|e| e.to_string())?)
            }
        };
    }
    for (i, p) in m.plans.iter().enumerate() {
        tx.inputs[i].signature_script = p.sigscript(sigs[i].as_deref()).map_err(|e| format!("input {i}: {e}"))?;
    }
    let mass = masses(&tx, &entries);
    tx.set_storage_mass(if mass.storage == u64::MAX { 0 } else { mass.storage });
    Ok((tx, entries))
}

/// Judges an attempt.
pub fn judge(m: &Mtx) -> Verdict {
    let (tx, entries) = match assemble_attempt(m) {
        Ok(x) => x,
        Err(e) => return Verdict::NotBuilt(e),
    };
    let refused = |r: String| Verdict::Refused { inputs: vec![], rule: Some(r) };
    if tx.outputs.iter().any(|o| o.value == 0) {
        return refused("zero-value output".into());
    }
    let total_in: u128 = entries.iter().map(|e| e.amount as u128).sum();
    let total_out: u128 = tx.outputs.iter().map(|o| o.value as u128).sum();
    if total_out > total_in {
        return refused(format!("outputs {total_out} exceed inputs {total_in}"));
    }
    let mass = masses(&tx, &entries);
    if mass.storage == u64::MAX || !mass.within_block_limits() {
        return refused(format!("mass beyond the block limits ({mass:?})"));
    }
    match kob_protocol::verify::execute(&tx, &entries, false) {
        Err(e) => refused(e.to_string()),
        Ok(res) => {
            let bad: Vec<(usize, String)> = res.into_iter().enumerate().filter_map(|(i, r)| r.err().map(|e| (i, e))).collect();
            if bad.is_empty() {
                Verdict::Accepted
            } else {
                Verdict::Refused { inputs: bad, rule: None }
            }
        }
    }
}

// ---------------------------------------------------------------- reading a transaction

/// What an input is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    /// A KOB order (template, decoded state).
    Order(TemplateId),
    /// A KOB router intent (actor name).
    Router(String),
    /// A retired template's cancel.
    Retired,
    /// A token input (program, token covenant id).
    Token(TemplateId),
    /// A P2PK coin.
    Plain([u8; 32]),
}

pub fn role(m: &Mtx, i: usize) -> Role {
    match &m.plans[i] {
        SigPlan::Entry { template: t, .. } => Role::Order(*t),
        SigPlan::Router { actor, .. } => Role::Router(actor.clone()),
        SigPlan::RetiredCancel { .. } => Role::Retired,
        SigPlan::TokenLeader { template: t, .. }
        | SigPlan::TokenDelegator { template: t, .. }
        | SigPlan::KronToken { template: t, .. } => Role::Token(*t),
        SigPlan::P2pk { pubkey } | SigPlan::CreatorLock { key: pubkey, .. } => Role::Plain(*pubkey),
    }
}

/// The covenant inputs under test: orders, router intents, retired cancels.
pub fn targets(m: &Mtx) -> Vec<usize> {
    (0..m.plans.len()).filter(|i| matches!(role(m, *i), Role::Order(_) | Role::Router(_) | Role::Retired)).collect()
}

/// A token input's state.
pub fn token_in_state(m: &Mtx, i: usize) -> Option<(TemplateId, TokenState)> {
    match &m.plans[i] {
        SigPlan::TokenLeader { template: t, state, .. } | SigPlan::TokenDelegator { template: t, state, .. } => {
            Some((*t, TokenState::Kcc20(state.clone())))
        }
        SigPlan::KronToken { template: t, state, .. } => Some((*t, TokenState::Kron(state.clone()))),
        _ => None,
    }
}

/// Sets a token input's state (plan and the UTXO's script).
pub fn set_token_in_state(m: &mut Mtx, i: usize, st: TokenState) {
    let t = match &mut m.plans[i] {
        SigPlan::TokenLeader { template: t, state, .. } | SigPlan::TokenDelegator { template: t, state, .. } => {
            *state = st.as_kcc20().expect("a KCC-20 state").clone();
            *t
        }
        SigPlan::KronToken { template: t, state, .. } => {
            let TokenState::Kron(k) = &st else { panic!("a KRON state") };
            *state = k.clone();
            *t
        }
        _ => panic!("input {i} is not a token input"),
    };
    m.tx.inputs[i].utxo.script_public_key = spk_to_string(&st.spk_with(token_template(t)));
    new_outpoint(m, i);
}

/// Gives input `i` an outpoint of its own: a coin with another script is another coin (signatures over the old
/// outpoint no longer cover it).
pub fn new_outpoint(m: &mut Mtx, i: usize) {
    let spk = m.tx.inputs[i].utxo.script_public_key.clone();
    let mut id = [0u8; 32];
    for (k, b) in spk.bytes().enumerate() {
        id[k % 32] = id[k % 32].wrapping_mul(31).wrapping_add(b);
    }
    m.tx.inputs[i].transaction_id = id;
    m.tx.inputs[i].index = 4_000 + i as u32;
}

/// A token output: index, program, token covenant id, the input that authorises it, its place among the outputs of that
/// leader (KRON: of the token), and its state.
#[derive(Clone, Debug)]
pub struct TokOut {
    pub j: usize,
    pub program: TemplateId,
    pub token: [u8; 32],
    pub leader: usize,
    pub pos: usize,
    pub state: TokenState,
}

pub fn token_outputs(m: &Mtx) -> Vec<TokOut> {
    let mut out = vec![];
    let mut seen_kron: BTreeSet<[u8; 32]> = BTreeSet::new();
    for (a, p) in m.plans.iter().enumerate() {
        let Some(cid) = m.tx.inputs[a].utxo.covenant_id else { continue };
        match p {
            SigPlan::TokenLeader { template: t, next_states, .. } => {
                let js: Vec<usize> = (0..m.tx.outputs.len())
                    .filter(|j| {
                        m.tx.outputs[*j].covenant.as_ref().is_some_and(|c| c.covenant_id == cid && c.authorizing_input as usize == a)
                    })
                    .collect();
                for (pos, j) in js.into_iter().enumerate() {
                    if let Some(s) = next_states.get(pos) {
                        out.push(TokOut { j, program: *t, token: cid, leader: a, pos, state: TokenState::Kcc20(s.clone()) });
                    }
                }
            }
            SigPlan::KronToken { template: t, next_states, .. } if seen_kron.insert(cid) => {
                let js: Vec<usize> = (0..m.tx.outputs.len())
                    .filter(|j| m.tx.outputs[*j].covenant.as_ref().is_some_and(|c| c.covenant_id == cid))
                    .collect();
                for (pos, j) in js.into_iter().enumerate() {
                    if let Some(s) = next_states.get(pos) {
                        out.push(TokOut { j, program: *t, token: cid, leader: a, pos, state: TokenState::Kron(s.clone()) });
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|t| t.j);
    out
}

/// Rewrites a token output's state (its script, and the next states that authorise it).
pub fn set_token_out(m: &mut Mtx, t: &TokOut, st: TokenState) {
    m.tx.outputs[t.j].script_public_key = spk_to_string(&st.spk_with(token_template(t.program)));
    for (i, p) in m.plans.iter_mut().enumerate() {
        match (p, &st) {
            (SigPlan::TokenLeader { next_states, .. }, TokenState::Kcc20(k)) if i == t.leader => next_states[t.pos] = k.clone(),
            (SigPlan::KronToken { next_states, .. }, TokenState::Kron(k)) if m.tx.inputs[i].utxo.covenant_id == Some(t.token) => {
                next_states[t.pos] = k.clone()
            }
            _ => {}
        }
    }
}

/// Appends a token output of the token led by input `leader` at output index `j`.
pub fn insert_token_out(m: &mut Mtx, leader: usize, program: TemplateId, j: usize, st: TokenState, value: u64) {
    let token = m.tx.inputs[leader].utxo.covenant_id.expect("token input");
    let o = TxOutputJson {
        value,
        script_public_key: spk_to_string(&st.spk_with(token_template(program))),
        covenant: Some(CovenantJson { authorizing_input: leader as u16, covenant_id: token }),
    };
    m.insert_output(j, o, Some(st));
}

/// The leader (KRON: any input) of token `token`.
pub fn token_leader(m: &Mtx, token: [u8; 32]) -> Option<usize> {
    (0..m.plans.len()).find(|i| {
        m.tx.inputs[*i].utxo.covenant_id == Some(token)
            && matches!(m.plans[*i], SigPlan::TokenLeader { .. } | SigPlan::KronToken { .. })
    })
}

/// The tokens of the transaction: (token covenant id, program).
pub fn tokens(m: &Mtx) -> Vec<([u8; 32], TemplateId)> {
    let mut v: Vec<([u8; 32], TemplateId)> = vec![];
    for i in 0..m.plans.len() {
        if let (Some((t, _)), Some(c)) = (token_in_state(m, i), m.cov_in(i)) {
            if !v.iter().any(|x| x.0 == c) {
                v.push((c, t));
            }
        }
    }
    v
}

/// The order state of input `i` (decoded).
pub fn order_state(m: &Mtx, i: usize) -> Option<(TemplateId, AnyState)> {
    match &m.plans[i] {
        SigPlan::Entry { template: t, state, .. } => AnyState::decode(*t, state).ok().map(|s| (*t, s)),
        _ => None,
    }
}

/// An entry argument by its ABI name.
pub fn arg_index(m: &Mtx, i: usize, name: &str) -> Option<usize> {
    let SigPlan::Entry { template: t, entry, .. } = &m.plans[i] else { return None };
    template(*t).contract().entries.get(entry)?.params.iter().position(|p| p.name == name)
}
pub fn get_int_arg(m: &Mtx, i: usize, name: &str) -> Option<i64> {
    let k = arg_index(m, i, name)?;
    match &m.plans[i] {
        SigPlan::Entry { args, .. } => match args.get(k)? {
            Arg::Int(v) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}
pub fn set_int_arg(m: &mut Mtx, i: usize, name: &str, v: i64) -> bool {
    let Some(k) = arg_index(m, i, name) else { return false };
    if let SigPlan::Entry { args, .. } = &mut m.plans[i] {
        args[k] = Arg::Int(v);
        return true;
    }
    false
}
pub fn entry_name(m: &Mtx, i: usize) -> String {
    match &m.plans[i] {
        SigPlan::Entry { entry, .. } | SigPlan::Router { entry, .. } => entry.clone(),
        _ => String::new(),
    }
}

/// The 32-byte keys a covenant input's state names (makers, payers, merchants).
pub fn named_keys(m: &Mtx, i: usize) -> Vec<[u8; 32]> {
    let state: Vec<u8> = match &m.plans[i] {
        SigPlan::Entry { state, .. } | SigPlan::Router { state, .. } | SigPlan::RetiredCancel { state, .. } => state.clone(),
        _ => return vec![],
    };
    let mut v = vec![];
    for n in 1..=40u8 {
        let k = pk(n);
        if state.windows(32).any(|w| w == k) {
            v.push(k);
        }
    }
    v
}

/// The numbers a covenant input's state names as a tip, fee or budget: (field, value).
pub fn designed_fields(m: &Mtx, i: usize) -> Vec<(String, i64)> {
    let v: Option<Value> = match &m.plans[i] {
        SigPlan::Entry { template: t, state, .. } => AnyState::decode(*t, state).ok().and_then(|s| serde_json::to_value(s).ok()),
        SigPlan::Router { actor, state, .. } => {
            Actor::by_name(actor).and_then(|a| IntentState::decode(a, state).ok()).and_then(|s| serde_json::to_value(s).ok())
        }
        _ => None,
    };
    let mut out = vec![];
    fn walk(v: &Value, out: &mut Vec<(String, i64)>) {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    let n = k.to_ascii_lowercase();
                    let num = x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse::<i64>().ok()));
                    if let Some(num) = num {
                        if n.contains("tip") || n.contains("fee") || n.contains("extra") || n.contains("budget") {
                            out.push((k.clone(), num));
                        }
                    } else {
                        walk(x, out);
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    if let Some(v) = v {
        walk(&v, &mut out);
    }
    out
}

/// The router rule of [`ALLOW`] (`router-carrier`): an execution's KAS-only cut of the payer's or merchant's token cell carrier, or
/// of a plain output to the payer, within the KAS the payer handed the executor (the intent's and the lock's KAS).
pub fn router_carrier_note(h: &Mtx, a: &Attempt) -> Option<String> {
    if a.token_change || a.reduced.is_empty() || a.gain == 0 {
        return None;
    }
    let r = targets(h).into_iter().find(|i| matches!(role(h, *i), Role::Router(_)))?;
    let SigPlan::Router { actor, state, .. } = &h.plans[r] else { return None };
    let st = IntentState::decode(Actor::by_name(actor)?, state).ok()?;
    let (payer, merchant) = (st.payer(), st.merchant());
    let id = h.cov_in(r)?;
    let lock: u64 = (0..h.plans.len())
        .filter(|k| token_in_state(h, *k).is_some_and(|(_, s)| s.is_covenant_owned() && s.owner() == id))
        .map(|k| h.tx.inputs[k].utxo.amount)
        .sum();
    let budget = h.tx.inputs[r].utxo.amount + lock;
    let touts = token_outputs(h);
    let ok = a.reduced.iter().all(|j| match touts.iter().find(|t| t.j == *j) {
        Some(t) => t.state.is_user() && (t.state.owner() == payer || t.state.owner() == merchant),
        None => p2pk_key_of(&h.tx.outputs[*j]) == Some(payer),
    });
    (ok && a.gain <= budget).then(|| format!("router-carrier: {}", ALLOW[3].1))
}

/// The router `max_extra` of the attempt's intent input, if any.
pub fn router_max_extra(m: &Mtx) -> Option<i64> {
    targets(m).into_iter().find_map(|i| {
        let SigPlan::Router { actor, state, .. } = &m.plans[i] else { return None };
        let s = IntentState::decode(Actor::by_name(actor)?, state).ok()?;
        let v = serde_json::to_value(s).ok()?;
        fn find(v: &Value) -> Option<i64> {
            match v {
                Value::Object(o) => o.iter().find_map(|(k, x)| {
                    if k == "max_extra" || k == "maxExtra" {
                        x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                    } else {
                        find(x)
                    }
                }),
                _ => None,
            }
        }
        find(&v)
    })
}

/// Output `j` pays a seat (a P2PK output of a seat key, or a token output owned by a seat key).
pub fn seat_output(m: &Mtx, j: usize) -> bool {
    let seats = seat_pks();
    let o = &m.tx.outputs[j];
    if seats.iter().any(|k| o.script_public_key == spk_to_string(&p2pk_spk(k))) {
        return true;
    }
    token_outputs(m).iter().any(|t| t.j == j && t.state.is_user() && seats.contains(&t.state.owner()))
}

/// The key a plain P2PK output pays, if it is one.
pub fn p2pk_key_of(o: &TxOutputJson) -> Option<[u8; 32]> {
    let b = from_hex(&o.script_public_key).ok()?;
    (b.len() == 36 && b[0] == 0 && b[1] == 0 && b[2] == 0x20 && b[35] == 0xac).then(|| b[3..35].try_into().unwrap())
}
fn from_hex(s: &str) -> Result<Vec<u8>, ()> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ())).collect()
}
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// KAS the seat keys' own coins paid in.
pub fn seat_input_total(m: &Mtx) -> u64 {
    let seats = seat_pks();
    (0..m.plans.len())
        .filter(|i| match &m.plans[*i] {
            SigPlan::P2pk { pubkey } => seats.contains(pubkey),
            p => p.signer().is_some_and(|k| seats.contains(&k)) && !targets(m).contains(i),
        })
        .map(|i| m.tx.inputs[i].utxo.amount)
        .sum()
}

/// Output `j` is the signer's own payment (kaspa-redteam's rule): a plain output to a key that no covenant input's
/// state names, and the seats paid in at least its value.
pub fn signers_own_payment(m: &Mtx, j: usize) -> bool {
    let o = &m.tx.outputs[j];
    let named: BTreeSet<[u8; 32]> = targets(m).into_iter().flat_map(|i| named_keys(m, i)).collect();
    // a key-owned token output whose owner no covenant input names: the tokens the seat bought, sent where it chose
    let k = if o.covenant.is_some() {
        match token_outputs(m).into_iter().find(|t| t.j == j) {
            Some(t) if t.state.is_user() => t.state.owner(),
            _ => return false,
        }
    } else {
        match p2pk_key_of(o) {
            Some(k) => k,
            None => return false,
        }
    };
    !named.contains(&k) && seat_input_total(m) >= o.value
}

/// Token `token` is named by covenant input `i`'s state (its own token); otherwise a stray of it is foreign.
pub fn names_token(m: &Mtx, i: usize, token: &[u8; 32]) -> bool {
    match &m.plans[i] {
        SigPlan::Entry { state, .. } | SigPlan::Router { state, .. } | SigPlan::RetiredCancel { state, .. } => {
            state.windows(32).any(|w| w == token)
        }
        _ => false,
    }
}

/// Where a new attacker output goes: before the trailing outputs that pay a key a target names (terminal refunds sit at
/// `M - 1 - i`, so an output appended after them would move them).
pub fn insert_pos(m: &Mtx) -> usize {
    let named: BTreeSet<[u8; 32]> = targets(m).into_iter().flat_map(|i| named_keys(m, i)).collect();
    let mut j = m.tx.outputs.len();
    while j > 0 && p2pk_key_of(&m.tx.outputs[j - 1]).is_some_and(|k| named.contains(&k)) && m.tx.outputs[j - 1].covenant.is_none() {
        j -= 1;
    }
    j
}

/// Gives `v` sompi to the attacker: onto an existing plain seat output, or a new attacker output.
pub fn give_attacker_kas(m: &mut Mtx, v: u64) {
    if v == 0 {
        return;
    }
    let seats = seat_pks();
    if let Some(j) = (0..m.tx.outputs.len())
        .find(|j| m.tx.outputs[*j].covenant.is_none() && p2pk_key_of(&m.tx.outputs[*j]).is_some_and(|k| seats.contains(&k)))
    {
        m.tx.outputs[j].value += v;
        return;
    }
    let j = insert_pos(m);
    m.insert_output(j, p2pk_out(&attacker(), v), None);
}

/// Gives `n` units of token `token` to the attacker: onto a seat's token output of that token, or a new attacker
/// token output (the attacker's key, the family's holder owner, no borrow) carrying `carrier` sompi taken from `from`.
pub fn give_attacker_tokens(m: &mut Mtx, token: [u8; 32], n: i64, like: &TokenState) -> bool {
    let seats = seat_pks();
    let outs = token_outputs(m);
    if let Some(t) = outs.iter().find(|t| t.token == token && t.state.is_user() && seats.contains(&t.state.owner())) {
        let st = t.state.with_amount(t.state.amount() + n);
        set_token_out(m, &t.clone(), st);
        return true;
    }
    let Some(leader) = token_leader(m, token) else { return false };
    let program = token_in_state(m, leader).unwrap().0;
    let st = like.with_amount(n).with_holder_owner(program, attacker()).with_borrow(NO_BORROW);
    let j = insert_pos(m);
    insert_token_out(m, leader, program, j, st, 100_000_000);
    // the new cell's carrier is the attacker's own KAS
    let (inp, plan) = attacker_input(0xc7, 100_000_000);
    m.push_input(inp, plan, SigSrc::Fresh);
    true
}

// ---------------------------------------------------------------- attempts and classification

/// One hostile transaction.
pub struct Attempt {
    pub id: &'static str,
    pub label: String,
    pub m: Mtx,
    /// The attack's own note rule matched (kaspa-redteam's `benign`), with its reason.
    pub benign: Option<String>,
    /// KAS the attacker gains (for the allow-list).
    pub gain: u64,
    /// The attempt changes a token amount or owner (no KAS-only allow-list entry applies).
    pub token_change: bool,
    /// A variant the token side alone refuses is dropped (FAM / BOR rule: built wrong, proves nothing).
    pub must_reach_target: bool,
    /// Outputs whose KAS the attempt cut or redirected (for the router carrier rule).
    pub reduced: Vec<usize>,
}

impl Attempt {
    pub fn new(id: &'static str, label: impl Into<String>, m: Mtx) -> Attempt {
        Attempt { id, label: label.into(), m, benign: None, gain: 0, token_change: false, must_reach_target: false, reduced: vec![] }
    }
}

/// The allow-list: accepted outcomes KOB designed, each with its reason.
pub const ALLOW: [(&str, &str); 4] = [
    (
        "router-max-extra",
        "whoever executes a router intent may keep up to `max_extra` of the payer's KAS (docs/spec/x402-swap-and-pay.md section 17.8; \
         REDTEAM-2026-11-kaspero.md section 3: the filler output and the delivered cell carrier are not pinned on purpose)",
    ),
    ("signed-by-maker", "the maker's own SIGHASH_ALL signature authorises any outputs on `cancel` (claims, amends): the signer's own money"),
    (
        "foreign-stray",
        "a token of another covenant id than the order's own, owned by the order's id, is not protected by design: whoever spends \
         the order may move it (docs/spec/matcher.md section 1.2)",
    ),
    (
        "router-carrier",
        "a router execution pins the token amount, owner and extension of the cells it delivers, not their KAS carrier, nor the \n         payer's filler output; the executor may keep that KAS within what the payer handed it (REDTEAM-2026-11-kaspero.md section 3, \n         maintainer's answers: by design)",
    ),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Class {
    Refused,
    Note(String),
    Finding(String),
    Dropped(String),
}

/// Classifies an attempt's verdict.
pub fn classify(a: &Attempt, v: &Verdict, h: &Mtx) -> Class {
    match v {
        Verdict::NotBuilt(e) => Class::Dropped(format!("not built: {e}")),
        // an attempt whose outputs exceed its inputs was built wrong (the attacker would fund it): not a result
        Verdict::Refused { inputs, rule: Some(r) } if inputs.is_empty() && r.starts_with("outputs ") => {
            Class::Dropped(format!("unbalanced: {r}"))
        }
        Verdict::Refused { inputs, .. } => {
            if a.must_reach_target && !inputs.is_empty() {
                let t = targets(&a.m);
                if !inputs.iter().any(|(i, _)| t.contains(i)) {
                    return Class::Dropped(format!("only the token side refused it: {}", v.short()));
                }
            }
            Class::Refused
        }
        Verdict::Accepted => {
            if let Some(b) = &a.benign {
                return Class::Note(b.clone());
            }
            if !a.token_change {
                if let Some(r) = router_carrier_note(h, a) {
                    return Class::Note(r);
                }
                if let Some(x) = router_max_extra(&a.m) {
                    if a.gain > 0 && a.gain <= x as u64 {
                        return Class::Note(format!("within `max_extra` ({} sompi): {}", x, ALLOW[0].1));
                    }
                }
            }
            Class::Finding(format!("{}: {}", a.id, a.label))
        }
    }
}

// ---------------------------------------------------------------- the library attacks

/// The kaspa-redteam covenant attacks that apply to a real transaction, on the honest `h`.
pub fn library_attempts(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    let t = targets(h);
    if t.is_empty() {
        return v;
    }
    let seats = seat_pks();
    let signed: Vec<usize> = t.iter().copied().filter(|i| h.plans[*i].signer().is_some_and(|k| !seats.contains(&k))).collect();
    let fee = h.input_total().saturating_sub(h.output_total()).max(1);
    let all_to_attacker = |m: &Mtx| {
        let mut p = m.clone();
        let total = p.input_total();
        p.tx.outputs = vec![p2pk_out(&attacker(), total - fee)];
        for pl in p.plans.iter_mut() {
            match pl {
                SigPlan::TokenLeader { next_states, .. } => next_states.clear(),
                SigPlan::KronToken { next_states, .. } => next_states.clear(),
                _ => {}
            }
        }
        p
    };
    if !signed.is_empty() {
        // SIG-01 / SIG-02 on the signed covenant inputs
        let mut g = h.clone();
        for i in &signed {
            g.sigs[*i] = SigSrc::Garbage;
        }
        v.push(Attempt::new("SIG-01", "invalid signature, all value to attacker", all_to_attacker(&g)));
        v.push(Attempt::new("SIG-01", "invalid signature, honest outputs", g));
        let mut w = h.clone();
        for i in &signed {
            if let SigPlan::Entry { args, .. } | SigPlan::Router { args, .. } = &mut w.plans[*i] {
                for a in args.iter_mut() {
                    if let Arg::Sig(_) = a {
                        *a = Arg::Sig(attacker());
                    }
                }
            }
            if let SigPlan::RetiredCancel { maker, .. } = &mut w.plans[*i] {
                *maker = attacker();
            }
            w.sigs[*i] = SigSrc::Fresh;
        }
        v.push(Attempt::new("SIG-02", "attacker's signature, all value to attacker", all_to_attacker(&w)));
        v.push(Attempt::new("SIG-02", "attacker's signature, honest outputs", w));
        return v;
    }

    // OUT-01: everything to the attacker; then each output redirected
    let mut a = Attempt::new("OUT-01", "every output replaced by one attacker output", all_to_attacker(h));
    a.token_change = true;
    v.push(a);
    let touts = token_outputs(h);
    for j in 0..h.tx.outputs.len() {
        if seat_output(h, j) {
            continue;
        }
        let mut p = h.clone();
        let value = h.tx.outputs[j].value;
        let mut a = if let Some(tk) = touts.iter().find(|x| x.j == j) {
            // a token output: the attacker becomes its owner
            let st = tk.state.with_holder_owner(tk.program, attacker()).with_borrow(NO_BORROW);
            set_token_out(&mut p, tk, st);
            let mut a = Attempt::new("OUT-01", format!("token output {j} owned by the attacker"), p);
            a.token_change = true;
            if signers_own_payment(h, j) {
                a.benign =
                    Some(format!("paid from the signer's own coins: token output {j} is owned by a key no covenant input names"));
            }
            a
        } else {
            p.tx.outputs[j] = p2pk_out(&attacker(), value);
            let mut a = Attempt::new("OUT-01", format!("output {j} redirected to attacker"), p);
            a.gain = value;
            a.reduced = vec![j];
            if signers_own_payment(h, j) {
                a.benign = Some(format!("paid from the signer's own coins: output {j} pays a key no covenant input names"));
            }
            a
        };
        a.label.push_str(&format!(" ({} sompi)", value));
        v.push(a);
    }
    // OUT-02: output 0 cut to 10%
    if !seat_output(h, 0) {
        let honest = h.tx.outputs[0].value;
        let reduced = (honest / 10).max(10_000);
        if reduced < honest {
            let mut p = h.clone();
            p.tx.outputs[0].value = reduced;
            give_attacker_kas(&mut p, honest - reduced);
            let mut a = Attempt::new("OUT-02", "output 0 cut to 10%, difference to attacker", p);
            a.gain = honest - reduced;
            a.reduced = vec![0];
            if signers_own_payment(h, 0) {
                a.benign = Some("paid from the signer's own coins: output 0".into());
            }
            v.push(a);
        }
    }
    // OUT-03: each later output halved
    for j in 1..h.tx.outputs.len() {
        if seat_output(h, j) {
            continue;
        }
        let honest = h.tx.outputs[j].value;
        let half = honest / 2;
        if half == 0 {
            continue;
        }
        let mut p = h.clone();
        p.tx.outputs[j].value = honest - half;
        give_attacker_kas(&mut p, half);
        let mut a = Attempt::new("OUT-03", format!("output {j} halved, half to attacker"), p);
        a.gain = half;
        a.reduced = vec![j];
        if signers_own_payment(h, j) {
            a.benign = Some(format!("paid from the signer's own coins: output {j}"));
        }
        v.push(a);
    }
    // OUT-04: keep the payouts, take the leftover
    let left = h.input_total().saturating_sub(h.output_total());
    if left > 100_000 {
        let designed =
            t.iter().flat_map(|i| designed_fields(h, *i)).filter(|(_, x)| *x > 0 && *x as u64 >= left).min_by_key(|(_, x)| *x);
        if designed.is_some() || left > seat_input_total(h) {
            let take = left - 20_000;
            let mut p = h.clone();
            give_attacker_kas(&mut p, take);
            let mut a = Attempt::new("OUT-04", format!("honest outputs kept, the {take} sompi left over to attacker"), p);
            a.gain = take;
            // what the spend already pays the seats (kaspa-redteam: the fee comes out of the signer's own share)
            let share: u64 = (0..h.tx.outputs.len())
                .filter(|j| h.tx.outputs[*j].covenant.is_none() && seat_output(h, *j))
                .map(|j| h.tx.outputs[j].value)
                .sum();
            if let Some((name, x)) = designed {
                a.benign = Some(format!("the leftover ({left}) is within `{name}` ({x}), a tip or budget the covenant names"));
            } else if share > 0 && left <= share {
                a.benign = Some(format!("the leftover ({left}) is no more than what the spend already pays the seat ({share}): the fee out of the signer's own share"));
            }
            v.push(a);
        }
    }
    // FEE-01: every output not the attacker's halved
    {
        let mut p = h.clone();
        let mut all_own = true;
        for j in 0..p.tx.outputs.len() {
            if !seat_output(h, j) {
                p.tx.outputs[j].value /= 2;
                all_own &= signers_own_payment(h, j);
            }
        }
        let mut a = Attempt::new("FEE-01", "every output halved, difference left as fee", p);
        if all_own {
            a.benign = Some("paid from the signer's own coins: every output halved".into());
        }
        v.push(a);
    }
    // IN-01: a second copy of the first covenant input
    {
        let i0 = t[0];
        let mut dup = h.tx.inputs[i0].clone();
        dup.transaction_id[0] ^= 0xd1;
        dup.index += 101;
        let extra = dup.utxo.amount;
        let mut p = h.clone();
        p.push_input(dup.clone(), h.plans[i0].clone(), h.sigs[i0].clone());
        let mut q = p.clone();
        give_attacker_kas(&mut p, extra);
        let mut a = Attempt::new("IN-01", "second covenant coin added, its value to attacker", p);
        a.gain = extra;
        v.push(a);
        v.push(Attempt::new("IN-01", "second covenant coin added, its value left as fee", {
            q.sigs = q.sigs.clone();
            q
        }));
    }
    // IN-02: an attacker coin at input 0, outputs sized to it
    {
        let i0 = t[0];
        let cov_value = h.tx.inputs[i0].utxo.amount;
        let bait = 50_000_000u64;
        if cov_value > bait {
            let mut p = h.clone();
            let (inp, plan) = attacker_input(0xa0, bait);
            p.insert_input(0, inp, plan, SigSrc::Fresh);
            for j in 0..p.tx.outputs.len() {
                if !seat_output(&p, j) {
                    p.tx.outputs[j].value = (p.tx.outputs[j].value as u128 * bait as u128 / cov_value as u128) as u64;
                }
            }
            let taken = p.input_total().saturating_sub(p.output_total()).saturating_sub(fee);
            if taken > 0 {
                give_attacker_kas(&mut p, taken);
                let mut a = Attempt::new("IN-02", "attacker coin at input 0, outputs sized to it, rest to attacker", p);
                a.gain = taken;
                v.push(a);
            }
        }
    }
    v.extend(fam10_attempts(h));
    v.extend(bor01_attempts(h));
    v.extend(tok_amount_attempts(h));
    v
}

/// A token UTXO input for token `token` of `program` with state `st`, spent by `plan`.
pub fn token_input(tag: u8, program: TemplateId, token: [u8; 32], st: &TokenState, value: u64) -> TxInputJson {
    TxInputJson {
        transaction_id: [tag; 32],
        index: 9_000 + tag as u32,
        sequence: 0,
        sig_op_count: 0,
        compute_budget: 0,
        signature_script: vec![],
        utxo: UtxoJson {
            address: None,
            amount: value,
            script_public_key: spk_to_string(&st.spk_with(token_template(program))),
            block_daa_score: 1_000,
            is_coinbase: false,
            covenant_id: Some(token),
        },
    }
}

/// Adds a token input of `st` to the transaction: a KCC-20 delegate (or the leader when the token has no input yet,
/// with no outputs), a KRON input carrying the group's next states. The witness: covenant id for a covenant-owned state,
/// the attacker's key otherwise. Returns its index.
pub fn add_token_input(m: &mut Mtx, tag: u8, program: TemplateId, token: [u8; 32], st: TokenState, value: u64) -> usize {
    let inp = token_input(tag, program, token, &st, value);
    let witness = if st.is_covenant_owned() { Witness::CovenantId } else { Witness::P2pk(st.owner()) };
    let sig = if st.is_covenant_owned() { SigSrc::None } else { SigSrc::Fresh };
    match st {
        TokenState::Kcc20(k) => {
            let plan = if token_leader(m, token).is_some() {
                SigPlan::TokenDelegator { template: program, state: k, witness }
            } else {
                SigPlan::TokenLeader { template: program, state: k, next_states: vec![], witness }
            };
            m.push_input(inp, plan, sig);
        }
        TokenState::Kron(k) => {
            // every KRON input of the token carries the whole group's next states and one witness byte per token input
            let group: Vec<usize> = (0..m.plans.len()).filter(|i| m.cov_in(*i) == Some(token)).collect();
            let next: Vec<KronState> = group
                .first()
                .and_then(|g| match &m.plans[*g] {
                    SigPlan::KronToken { next_states, .. } => Some(next_states.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let mut wit: Vec<u8> = group
                .first()
                .and_then(|g| match &m.plans[*g] {
                    SigPlan::KronToken { witnesses, .. } => Some(witnesses.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let me = m.tx.inputs.len();
            // a covenant-owned input is authorised by its owner's input
            let auth = (0..m.plans.len()).find(|i| m.cov_in(*i) == Some(k.owner)).unwrap_or(me) as u8;
            wit.push(if k.id_type == kob_protocol::family::KRON_TYPE_COVID { auth } else { me as u8 });
            for g in &group {
                if let SigPlan::KronToken { witnesses, .. } = &mut m.plans[*g] {
                    *witnesses = wit.clone();
                }
            }
            let signer = (k.id_type == kob_protocol::family::KRON_TYPE_PUBKEY).then_some(k.owner);
            let plan = SigPlan::KronToken { template: program, state: k, next_states: next, witnesses: wit, signer };
            m.push_input(inp, plan, if signer.is_some() { SigSrc::Fresh } else { SigSrc::None });
        }
    }
    m.tx.inputs.len() - 1
}

/// FAM-10, on every token build: a stray UTXO of each token of the transaction, owned by a covenant input's id, spent
/// next to it, its units to the attacker.
pub fn fam10_attempts(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for t in targets(h) {
        let Some(owner) = h.cov_in(t) else { continue };
        for (token, program) in tokens(h) {
            let like = (0..h.plans.len())
                .find_map(|i| token_in_state(h, i).filter(|_| h.cov_in(i) == Some(token)).map(|x| x.1))
                .expect("a token input");
            let stray = TokenState::custody(like.family(), STRAY_UNITS, owner, like.extension());
            let mut p = h.clone();
            add_token_input(&mut p, 0xf1, program, token, stray.clone(), 100_000_000);
            if !give_attacker_tokens(&mut p, token, STRAY_UNITS, &like) {
                continue;
            }
            let mut a = Attempt::new(
                "FAM-10",
                format!("a stray {} UTXO owned by input {t} ({}) swept to the attacker", program.name(), role_name(h, t)),
                p,
            );
            a.token_change = true;
            a.must_reach_target = true;
            if !names_token(h, t, &token) {
                a.benign = Some(format!("foreign stray: {}", ALLOW[2].1));
            }
            v.push(a);
        }
    }
    v
}

/// BOR-01, on every KCC-20 build: each token input owned by a covenant input's id (a custody), and the outputs of the
/// same owner, with borrowing on (scheme 1, zero guard).
pub fn bor01_attempts(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    let owners: BTreeSet<[u8; 32]> = targets(h).into_iter().filter_map(|t| h.cov_in(t)).collect();
    for i in 0..h.plans.len() {
        let Some((program, st)) = token_in_state(h, i) else { continue };
        if st.family() != kob_protocol::Family::Kcc20
            || !st.is_covenant_owned()
            || !owners.contains(&st.owner())
            || st.borrow() != NO_BORROW
        {
            continue;
        }
        let mut p = h.clone();
        set_token_in_state(&mut p, i, st.clone().with_borrow((1, [0; 32])));
        for to in token_outputs(h) {
            if to.token == h.cov_in(i).unwrap() && to.state.owner() == st.owner() && to.state.is_covenant_owned() {
                set_token_out(&mut p, &to, to.state.clone().with_borrow((1, [0; 32])));
            }
        }
        let mut a = Attempt::new("BOR-01", format!("the {} custody at input {i} with borrowing on", program.name()), p);
        a.token_change = true;
        a.must_reach_target = true;
        v.push(a);
    }
    v
}

/// TOK-01: half of each token output not the attacker's moved to the attacker (token amounts pinned).
pub fn tok_amount_attempts(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for to in token_outputs(h) {
        if seat_output(h, to.j) || to.state.amount() < 2 {
            continue;
        }
        let half = to.state.amount() / 2;
        let mut p = h.clone();
        set_token_out(&mut p, &to, to.state.with_amount(to.state.amount() - half));
        if !give_attacker_tokens(&mut p, to.token, half, &to.state) {
            continue;
        }
        let mut a = Attempt::new("TOK-01", format!("token output {} cut by half, the half to the attacker", to.j), p);
        a.token_change = true;
        if signers_own_payment(h, to.j) {
            a.benign =
                Some(format!("paid from the signer's own coins: token output {} is owned by a key no covenant input names", to.j));
        }
        v.push(a);
    }
    v
}

pub fn role_name(m: &Mtx, i: usize) -> String {
    match role(m, i) {
        Role::Order(t) => t.name().into(),
        Role::Router(a) => a,
        Role::Retired => "retired".into(),
        Role::Token(t) => t.name().into(),
        Role::Plain(_) => "p2pk".into(),
    }
}

// ---------------------------------------------------------------- runs, the matrix and replay files

/// One cell of the coverage matrix.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cell {
    pub judged: u64,
    pub refused: u64,
    pub notes: u64,
    pub findings: u64,
    pub dropped: u64,
    /// Refusals where a covenant input under test failed (the rest: only a partner input, or a transaction rule).
    pub by_target: u64,
}

/// The run's counts: (attack, template, program) -> cell; notes by reason; findings.
#[derive(Default)]
pub struct Run {
    pub cells: BTreeMap<(String, String, String), Cell>,
    pub notes: BTreeMap<String, u64>,
    pub findings: Vec<String>,
    pub shapes: u64,
    pub shapes_attacked: u64,
    pub not_attacked: Vec<String>,
}

static FINDINGS_LOCK: Mutex<()> = Mutex::new(());

/// The templates (target kinds) and programs (token programs) of a shape, for the matrix.
pub fn shape_keys(h: &Mtx) -> (String, String) {
    let mut tpl: Vec<String> = targets(h).into_iter().map(|i| role_name(h, i)).collect();
    tpl.sort();
    tpl.dedup();
    let mut prog: Vec<String> = tokens(h).into_iter().map(|(_, p)| p.name().to_string()).collect();
    for i in targets(h) {
        if let Some((_, s)) = order_state(h, i) {
            if let Some(p) = token_program_of(&s) {
                prog.push(p.name().into());
            }
        }
    }
    prog.sort();
    prog.dedup();
    (tpl.join("+"), if prog.is_empty() { "-".into() } else { prog.join("+") })
}

fn token_program_of(s: &AnyState) -> Option<TemplateId> {
    let h = s.token_tpl_hash()?;
    TemplateId::TOKEN_PROGRAMS.into_iter().find(|p| token_template(*p).hash == h)
}

impl Run {
    /// Judges every attempt of one honest shape and records it.
    pub fn attack(&mut self, name: &str, h: &Mtx, attempts: Vec<Attempt>) {
        self.shapes += 1;
        if attempts.is_empty() {
            self.not_attacked.push(name.into());
            return;
        }
        self.shapes_attacked += 1;
        let (tpl, prog) = shape_keys(h);
        for a in attempts {
            let v = judge(&a.m);
            let c = classify(&a, &v, h);
            let cell = self.cells.entry((a.id.to_string(), tpl.clone(), prog.clone())).or_default();
            match &c {
                Class::Dropped(_) => {
                    cell.dropped += 1;
                    continue;
                }
                _ => cell.judged += 1,
            }
            match c {
                Class::Refused => {
                    cell.refused += 1;
                    if let Verdict::Refused { inputs, .. } = &v {
                        let t = targets(&a.m);
                        if inputs.iter().any(|(i, _)| t.contains(i)) {
                            cell.by_target += 1;
                        }
                    }
                }
                Class::Note(r) => {
                    cell.notes += 1;
                    let key: String = r.split(':').next().unwrap_or(&r).chars().take(90).collect();
                    *self.notes.entry(format!("{}: {key}", a.id)).or_default() += 1;
                }
                Class::Finding(f) => {
                    cell.findings += 1;
                    let file = write_replay(name, &a, &v);
                    self.findings.push(format!("{name}: {f} -> {file}"));
                }
                Class::Dropped(_) => unreachable!(),
            }
        }
    }

    /// Prints the matrix (`RT|attack|template|program|judged|refused|notes|findings|dropped|refused by a covenant input`) and the totals.
    pub fn report(&self, title: &str) {
        let mut tot = Cell::default();
        for ((a, t, p), c) in &self.cells {
            println!("RT|{a}|{t}|{p}|{}|{}|{}|{}|{}|{}", c.judged, c.refused, c.notes, c.findings, c.dropped, c.by_target);
            tot.judged += c.judged;
            tot.refused += c.refused;
            tot.notes += c.notes;
            tot.findings += c.findings;
            tot.dropped += c.dropped;
            tot.by_target += c.by_target;
        }
        for (r, n) in &self.notes {
            println!("RTNOTE|{r}|{n}");
        }
        for n in &self.not_attacked {
            println!("RTSKIP|{n}");
        }
        println!(
            "RTTOTAL|{title}|shapes {}|attacked {}|judged {}|refused {} (by a covenant input {})|notes {}|findings {}|dropped {}",
            self.shapes, self.shapes_attacked, tot.judged, tot.refused, tot.by_target, tot.notes, tot.findings, tot.dropped
        );
        for f in &self.findings {
            println!("RTFINDING|{f}");
        }
        if let Ok(dir) = std::env::var("KOB_RT_OUT") {
            let _ = std::fs::create_dir_all(&dir);
            let rows: Vec<Value> = self
                .cells
                .iter()
                .map(|((a, t, p), c)| {
                    json!({"attack": a, "template": t, "program": p, "judged": c.judged, "refused": c.refused, "notes": c.notes, "findings": c.findings, "dropped": c.dropped, "by_target": c.by_target})
                })
                .collect();
            let doc = json!({"title": title, "shapes": self.shapes, "attacked": self.shapes_attacked, "cells": rows, "notes": self.notes, "findings": self.findings, "not_attacked": self.not_attacked});
            let f = format!("{dir}/{}.json", title.replace(['/', ' ', ':'], "_"));
            std::fs::write(f, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        }
    }

    pub fn assert_clean(&self) {
        assert!(self.findings.is_empty(), "{} finding(s):\n{}", self.findings.len(), self.findings.join("\n"));
    }
}

/// Writes a replay file for an accepted attempt: the kaspa-redteam baseline format (`kaspa-redteam --baseline <file>`
/// judges it again) with KOB's attempt fields. Returns the path.
pub fn write_replay(name: &str, a: &Attempt, v: &Verdict) -> String {
    let _g = FINDINGS_LOCK.lock();
    let dir = std::env::var("KOB_RT_FINDINGS").unwrap_or_else(|_| std::env::temp_dir().join("kob-rt-findings").display().to_string());
    let _ = std::fs::create_dir_all(&dir);
    let (mut tx, entries) = assemble_attempt(&a.m).expect("assembled");
    // commit the budgets the attempt needs (the judge meters without a limit; consensus enforces the committed one)
    let _ = kob_protocol::tx::tighten_budgets(&mut tx, &entries);
    let tx_v = serde_json::to_value(&tx).unwrap();
    let doc = json!({
        "format": "kaspa-redteam/baseline/1",
        "daaScore": "10000000",
        "pastMedianTime": "4000000000000",
        "transactions": [{ "path": format!("{} {}", a.id, a.label), "targets": targets(&a.m), "tx": tx_v, "utxos": entries }],
        "kob": { "shape": name, "attack": a.id, "label": a.label, "verdict": v.short(), "signedTx": TxJson::from_tx(&tx, &entries) },
    });
    let safe: String = format!("{}-{}-{}", a.id, name, a.label)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .take(150)
        .collect();
    let path = format!("{dir}/{safe}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    path
}

/// Builds, signs with every fixture key and checks the honest transaction in the engine (it must be accepted as given).
pub fn honest(name: &str, built: &BuiltTx) -> Mtx {
    let m = Mtx::from_built(built);
    let v = judge(&m);
    assert!(v.accepted(), "{name}: the honest transaction is refused: {}", v.short());
    m
}

/// Describes a transaction (inputs and outputs), for debugging attacks.
pub fn describe(m: &Mtx) -> String {
    let mut s = String::new();
    let touts = token_outputs(m);
    for (i, inp) in m.tx.inputs.iter().enumerate() {
        let extra = match &m.plans[i] {
            SigPlan::Entry { entry, args, .. } => format!("{entry}({:?})", args),
            SigPlan::Router { entry, .. } => entry.clone(),
            SigPlan::TokenLeader { state, witness, .. } => format!("leader {state:?} {witness:?}"),
            SigPlan::TokenDelegator { state, witness, .. } => format!("delegate {state:?} {witness:?}"),
            SigPlan::KronToken { state, witnesses, .. } => format!("kron {state:?} w{witnesses:?}"),
            SigPlan::P2pk { pubkey } => format!("p2pk {}", &hex(pubkey)[..8]),
            SigPlan::RetiredCancel { .. } => "retired cancel".into(),
            SigPlan::CreatorLock { until, .. } => format!("creator lock until {until}"),
        };
        s.push_str(&format!(
            "  in {i}: {} {} cov {:?} {}\n",
            role_name(m, i),
            inp.utxo.amount,
            inp.utxo.covenant_id.map(|c| hex(&c[..2])),
            extra
        ));
    }
    for (j, o) in m.tx.outputs.iter().enumerate() {
        let what = if let Some(t) = touts.iter().find(|t| t.j == j) {
            format!("token {:?}", t.state)
        } else if let Some(k) = p2pk_key_of(o) {
            format!("p2pk {}", &hex(&k)[..8])
        } else {
            "p2sh".into()
        };
        s.push_str(&format!(
            "  out {j}: {} {:?} {what}\n",
            o.value,
            o.covenant.as_ref().map(|c| (c.authorizing_input, hex(&c.covenant_id[..2])))
        ));
    }
    s
}
