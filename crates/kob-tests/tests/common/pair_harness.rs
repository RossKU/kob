//! Engine harness of the pair order suites (`KobPair`, `KobCondPair`, `KobIfdPair`; contracts/v2): the three pair
//! templates compiled from their (possibly mutated) sources, and a transaction editor over honest `kob-protocol` builds.
//!
//! Include it next to the `kob-protocol` fixtures in a test binary:
//! ```ignore
//! mod common;
//! #[path = "../../kob-protocol/tests/common/mod.rs"]
//! mod fx;
//! #[path = "common/pair_harness.rs"]
//! mod ph;
//! ```
//!
//! Templates under test ([`Subs`]): `KobPair`, then `KobCondPair` (its build constants PAIR_TPL / PRE / SUF re-pointed
//! at the compiled KobPair), then `KobIfdPair` (COND_TPL / PRE / SUF and PAIR_TPL / PRE / SUF re-pointed);
//! [`Subs::compile_asks`]: the KCC-20 ask family and its readers instead (`KobAsk`, then `KobCondAsk` and `KobCondBid`
//! against it, `KobIfdAsk` against KobCondBid and KobAsk, `KobIfdBid` against KobCondAsk). An ablation
//! run (`KOB_ABLATION_SRC` = a directory with mutated sources, `KOB_ABLATION=1`) therefore executes the weakened covenant
//! everywhere it appears: as the order filled, as trigger evidence of a conditional, as the exit an entry creates.
//! Without an ablation the compiled templates must equal the pinned ones. A mutation must not move the state span.
//!
//! [`Ed::finish`] rewrites every pair-template script of the transaction to the build under test: the redeem scripts of
//! the spent pair UTXOs, every output whose script public key the library derived from a pair template (looked up in
//! `kob_protocol::artifacts::spk_trace`), the KobCondPair prefix / suffix arguments of a KobIfdPair fill, and the
//! covenant id of a genesis whose outputs changed (an IFD exit: binding and the custody owned by it). A genesis id that
//! is not the honest id of the pinned outputs (a forged exit id) is left as it is.
#![allow(dead_code)]

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, ScriptPublicKey, Transaction, TransactionInput, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kob_protocol::artifacts::spk_trace::{self, Origin};
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::{build_batch_unchecked, build_with, Action, Batch};
use kob_protocol::script::{entry_sigscript, p2pk_spk, p2sh_spk, push_data};
use kob_protocol::state::*;
use kob_protocol::tx::{sign_digest, Arg, BuiltTx, SigPlan, TokenUtxo, Utxo, Witness};
use silverscript_abi::ArtifactValue;
use silverscript_lang::compiler::CompileOptions;

use crate::common;
use crate::fx::{keys, pk, utxo, KAS, MATCHER};

pub const PAIR_KINDS: [TemplateId; 3] = [TemplateId::KobPair, TemplateId::KobCondPair, TemplateId::KobIfdPair];

pub fn ablating() -> bool {
    std::env::var_os("KOB_ABLATION").is_some()
}

pub fn budgets(role: &str) -> kob_protocol::Result<u16> {
    Ok(kob_protocol::budget::lookup(role).unwrap_or(80))
}

pub fn is_pair(id: TemplateId) -> bool {
    PAIR_KINDS.contains(&id)
}

// ---------------------------------------------------------------- the pair templates under test

/// Prefix and suffix of a pair template compiled from its (possibly mutated) source.
#[derive(Clone)]
pub struct Sub {
    pub id: TemplateId,
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub hash: [u8; 32],
}

impl Sub {
    pub fn redeem(&self, state: &[u8]) -> Vec<u8> {
        [self.prefix.as_slice(), state, self.suffix.as_slice()].concat()
    }
    pub fn spk(&self, state: &[u8]) -> ScriptPublicKey {
        p2sh_spk(&self.redeem(state))
    }
}

/// The templates under test (the three pair templates, or the KCC-20 ask family: [`Subs::compile_asks`]).
pub struct Subs {
    pub subs: BTreeMap<TemplateId, Sub>,
}

/// The KCC-20 order templates that hold a custody of their one token (`KobAsk`, `KobCondAsk`, `KobIfdAsk`) and the ones
/// that read or write such a custody's state (`KobCondBid` reads the ask evidence, `KobIfdBid` writes its exit).
pub const ASK_KINDS: [TemplateId; 5] =
    [TemplateId::KobAsk, TemplateId::KobCondAsk, TemplateId::KobCondBid, TemplateId::KobIfdAsk, TemplateId::KobIfdBid];

/// (if-done entry, its exit template): the entry's fill takes the exit's prefix and suffix as arguments.
const EXIT_OF: [(TemplateId, TemplateId); 3] = [
    (TemplateId::KobIfdPair, TemplateId::KobCondPair),
    (TemplateId::KobIfdBid, TemplateId::KobCondAsk),
    (TemplateId::KobIfdAsk, TemplateId::KobCondBid),
];

/// Re-points the build constants (hash, prefix length, suffix length) of `dep` in a constructor argument list: the one
/// place where the pinned triple appears.
fn repoint(args: &mut [ArtifactValue], dep: TemplateId, sub: &Sub) {
    let pinned = template(dep);
    let ph = kob_protocol::artifacts::pinned_hash(dep).to_vec();
    let (pre, suf) = (pinned.prefix.len() as i64, pinned.suffix.len() as i64);
    let hits: Vec<usize> = (0..args.len().saturating_sub(2))
        .filter(|&i| {
            matches!(&args[i], ArtifactValue::Bytes(b) if *b == ph)
                && matches!(args[i + 1], ArtifactValue::Int(v) if v == pre)
                && matches!(args[i + 2], ArtifactValue::Int(v) if v == suf)
        })
        .collect();
    assert_eq!(hits.len(), 1, "constructor constants of {} found {} times", dep.name(), hits.len());
    let i = hits[0];
    args[i] = ArtifactValue::Bytes(sub.hash.to_vec());
    args[i + 1] = ArtifactValue::Int(sub.prefix.len() as i64);
    args[i + 2] = ArtifactValue::Int(sub.suffix.len() as i64);
}

impl Subs {
    /// The three pair templates.
    pub fn compile() -> Subs {
        Subs::compile_deps(&[
            (TemplateId::KobPair, &[]),
            (TemplateId::KobCondPair, &[TemplateId::KobPair]),
            (TemplateId::KobIfdPair, &[TemplateId::KobCondPair, TemplateId::KobPair]),
        ])
    }
    /// The KCC-20 ask family ([`ASK_KINDS`]), each against the compiled templates it embeds.
    pub fn compile_asks() -> Subs {
        Subs::compile_deps(&[
            (TemplateId::KobAsk, &[]),
            (TemplateId::KobCondAsk, &[TemplateId::KobAsk]),
            (TemplateId::KobCondBid, &[TemplateId::KobAsk]),
            (TemplateId::KobIfdAsk, &[TemplateId::KobCondBid, TemplateId::KobAsk]),
            (TemplateId::KobIfdBid, &[TemplateId::KobCondAsk]),
        ])
    }
    /// `deps`: (template, the templates under test whose build constants it embeds), in dependency order.
    pub fn compile_deps(deps: &[(TemplateId, &[TemplateId])]) -> Subs {
        let mut subs = BTreeMap::new();
        for &(id, ds) in deps {
            let ctor = format!("contracts/v2/{}.ctor.json", id.name());
            let mut args: Vec<ArtifactValue> =
                serde_json::from_str(&std::fs::read_to_string(common::repo_root().join(&ctor)).unwrap()).expect("ctor json");
            for d in ds {
                repoint(&mut args, *d, &subs[d]);
            }
            let art = common::compile_contract(&common::contract_source(id.name()), &args, CompileOptions::default())
                .unwrap_or_else(|e| panic!("compile {}: {e:?}", id.name()));
            let (prefix, suffix, hash) = common::compiled_template_parts_and_hash(&art);
            let pinned = template(id);
            if !ablating() {
                assert_eq!(
                    (prefix.as_slice(), suffix.as_slice()),
                    (pinned.prefix.as_slice(), pinned.suffix.as_slice()),
                    "{} source != pinned artifact",
                    id.name()
                );
                assert_eq!(hash.as_slice(), kob_protocol::artifacts::pinned_hash(id).as_slice(), "{} hash", id.name());
            }
            assert_eq!(prefix, pinned.prefix, "{}: a mutation must not move the state span", id.name());
            if ablating() {
                let same = hash.as_slice() == kob_protocol::artifacts::pinned_hash(id).as_slice();
                println!("TEMPLATE {} {} ({})", id.name(), hex8(&hash), if same { "pinned" } else { "rebuilt" });
            }
            subs.insert(id, Sub { id, prefix, suffix, hash: hash.try_into().expect("32-byte hash") });
        }
        Subs { subs }
    }
    pub fn get(&self, id: TemplateId) -> &Sub {
        &self.subs[&id]
    }
    /// Whether `id` is one of the templates under test.
    pub fn has(&self, id: TemplateId) -> bool {
        self.subs.contains_key(&id)
    }
    /// The script public key under test of a script public key the library derived from a template under test, if it is one.
    pub fn rewrite(&self, spk: &ScriptPublicKey) -> Option<ScriptPublicKey> {
        match spk_trace::lookup(spk) {
            Some((Origin::Template(id), state)) if self.has(id) => Some(self.get(id).spk(&state)),
            _ => None,
        }
    }
}

/// Template and state span of a script public key the library derived (any template), if recorded.
pub fn traced(spk: &ScriptPublicKey) -> Option<(TemplateId, Vec<u8>)> {
    match spk_trace::lookup(spk) {
        Some((Origin::Template(id), state)) => Some((id, state)),
        _ => None,
    }
}

// ---------------------------------------------------------------- builds

pub fn built(a: Action) -> BuiltTx {
    build_with(&a, &budgets).unwrap_or_else(|e| panic!("honest build: {e}"))
}

pub fn unchecked(b: Batch) -> BuiltTx {
    build_batch_unchecked(&b, &budgets).unwrap_or_else(|e| panic!("unchecked build: {e}"))
}

/// The fill amount of an entry plan (its first argument, the fixed 8-byte push), if it is one.
pub fn fill_n(p: &SigPlan) -> Option<i64> {
    match p {
        SigPlan::Entry { args, .. } => match args.first() {
            Some(Arg::Bytes(b)) if b.len() == 8 => Some(i64::from_le_bytes(b.as_slice().try_into().expect("8 bytes"))),
            _ => None,
        },
        _ => None,
    }
}

pub fn nb(n: i64) -> Arg {
    Arg::Bytes(n.to_le_bytes().to_vec())
}

pub fn cov_of(e: &UtxoEntry) -> Option<[u8; 32]> {
    e.covenant_id.map(|h| h.as_bytes())
}

// ---------------------------------------------------------------- transaction editor

/// A built transaction opened for editing: inputs with their plans, outputs, and the token state of every token output.
#[derive(Clone)]
pub struct Ed {
    pub name: String,
    pub tx: Transaction,
    pub entries: Vec<UtxoEntry>,
    pub plans: Vec<SigPlan>,
    /// Token outputs: index -> (token covenant id, state).
    pub tok_out: BTreeMap<usize, ([u8; 32], TokenState)>,
    /// Token programs by covenant id.
    pub programs: BTreeMap<[u8; 32], TemplateId>,
    /// Signature hash type per input (default SIGHASH_ALL).
    pub sighash_types: BTreeMap<usize, u8>,
    /// Signing key override per input (default: the plan's signer).
    pub signers: BTreeMap<usize, u8>,
    /// Raw signature-script override per input (applied after assembly; the attack writes the bytes itself).
    pub raw_sigscripts: BTreeMap<usize, Vec<u8>>,
}

impl Ed {
    pub fn new(name: &str, built: &BuiltTx) -> Ed {
        let (tx, entries) = built.tx.to_tx().expect("tx");
        let plans = built.plans.clone();
        let mut programs = BTreeMap::new();
        for (i, p) in plans.iter().enumerate() {
            if let SigPlan::TokenLeader { template, .. }
            | SigPlan::TokenDelegator { template, .. }
            | SigPlan::KronToken { template, .. } = p
            {
                programs.insert(cov_of(&entries[i]).expect("token input covenant"), *template);
            }
        }
        let mut tok_out = BTreeMap::new();
        for cov in programs.keys() {
            let next: Vec<TokenState> = plans
                .iter()
                .enumerate()
                .find_map(|(i, p)| match p {
                    SigPlan::TokenLeader { next_states, .. } if cov_of(&entries[i]) == Some(*cov) => {
                        Some(next_states.iter().cloned().map(TokenState::from).collect())
                    }
                    SigPlan::KronToken { next_states, .. } if cov_of(&entries[i]) == Some(*cov) => {
                        Some(next_states.iter().cloned().map(TokenState::from).collect())
                    }
                    _ => None,
                })
                .expect("token leader");
            let idx: Vec<usize> = tx
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, o)| o.covenant.map(|c| c.covenant_id.as_bytes()) == Some(*cov))
                .map(|(k, _)| k)
                .collect();
            assert_eq!(idx.len(), next.len(), "{name}: token outputs vs leader next states");
            for (k, st) in idx.into_iter().zip(next) {
                tok_out.insert(k, (*cov, st));
            }
        }
        Ed {
            name: name.into(),
            tx,
            entries,
            plans,
            tok_out,
            programs,
            sighash_types: BTreeMap::new(),
            signers: BTreeMap::new(),
            raw_sigscripts: BTreeMap::new(),
        }
    }

    pub fn named(mut self, name: &str) -> Ed {
        self.name = name.into();
        self
    }

    // -------- inputs

    pub fn entry_state(&self, i: usize) -> (TemplateId, Vec<u8>) {
        match &self.plans[i] {
            SigPlan::Entry { template, state, .. } => (*template, state.clone()),
            p => panic!("{}: input {i} is not an entry call: {p:?}", self.name),
        }
    }
    /// Input indexes of the entry calls of template `id`, in input order.
    pub fn inputs_of(&self, id: TemplateId) -> Vec<usize> {
        (0..self.plans.len()).filter(|i| matches!(&self.plans[*i], SigPlan::Entry { template, .. } if *template == id)).collect()
    }
    /// The input spending covenant `cov` with an entry call.
    pub fn input_of_cov(&self, cov: [u8; 32]) -> usize {
        (0..self.plans.len())
            .find(|i| matches!(self.plans[*i], SigPlan::Entry { .. }) && cov_of(&self.entries[*i]) == Some(cov))
            .unwrap_or_else(|| panic!("{}: no order input of covenant {:02x?}", self.name, &cov[..4]))
    }
    /// Replaces the state span of entry input i (the redeem script and the UTXO script follow at assembly). Outputs are
    /// not touched: use [`Ed::set_out_spk_state`] for a continuation.
    pub fn set_entry_state(&mut self, i: usize, new: Vec<u8>) {
        match &mut self.plans[i] {
            SigPlan::Entry { state, .. } => *state = new,
            p => panic!("{}: input {i} is not an entry call: {p:?}", self.name),
        }
    }
    pub fn set_entry(&mut self, i: usize, name: &str, args: Vec<Arg>) {
        match &mut self.plans[i] {
            SigPlan::Entry { entry, args: a, .. } => {
                *entry = name.into();
                *a = args;
            }
            p => panic!("{}: input {i} is not an entry call: {p:?}", self.name),
        }
    }
    pub fn args(&self, i: usize) -> Vec<Arg> {
        match &self.plans[i] {
            SigPlan::Entry { args, .. } => args.clone(),
            p => panic!("{}: input {i} is not an entry call: {p:?}", self.name),
        }
    }
    pub fn arg_int(&self, i: usize, pos: usize) -> i64 {
        match &self.args(i)[pos] {
            Arg::Int(v) => *v,
            a => panic!("{}: input {i} arg {pos} is not an int: {a:?}", self.name),
        }
    }
    pub fn set_arg(&mut self, i: usize, pos: usize, v: Arg) {
        match &mut self.plans[i] {
            SigPlan::Entry { args, .. } => args[pos] = v,
            p => panic!("{}: input {i} is not an entry call: {p:?}", self.name),
        }
    }
    /// Turns input i into a plain P2PK input of `key` (same outpoint and amount).
    pub fn make_p2pk(&mut self, i: usize, key: u8) {
        let e = &self.entries[i];
        self.entries[i] = UtxoEntry::new(e.amount, p2pk_spk(&pk(key)), e.block_daa_score, false, None);
        self.plans[i] = SigPlan::P2pk { pubkey: pk(key) };
    }
    /// Value (sompi) and DAA score of input i's UTXO.
    pub fn set_input_value(&mut self, i: usize, v: u64) {
        let e = &self.entries[i];
        self.entries[i] = UtxoEntry::new(v, e.script_public_key.clone(), e.block_daa_score, e.is_coinbase, e.covenant_id);
    }
    pub fn set_input_daa(&mut self, i: usize, daa: u64) {
        let e = &self.entries[i];
        self.entries[i] = UtxoEntry::new(e.amount, e.script_public_key.clone(), daa, e.is_coinbase, e.covenant_id);
    }
    /// Appends a token input (a stray or a key-held UTXO) of a token of the transaction (`u.covenant_id` = the token).
    pub fn add_token_input(&mut self, u: Utxo, st: TokenState, witness: Witness) -> usize {
        let cov = u.covenant_id.expect("token covenant");
        let tpl = self.programs[&cov];
        let plan = match &st {
            TokenState::Kcc20(k) => SigPlan::TokenDelegator { template: tpl, state: k.clone(), witness },
            TokenState::Kron(k) => SigPlan::KronToken { template: tpl, state: k.clone(), next_states: vec![], witnesses: vec![] },
        };
        self.tx.inputs.push(TransactionInput::new_with_compute_budget(u.outpoint(), vec![], 0, 80));
        self.entries.push(UtxoEntry::new(
            u.amount,
            st.spk_with(token_template(tpl)),
            u.block_daa_score,
            false,
            Some(Hash::from_bytes(cov)),
        ));
        self.plans.push(plan);
        self.tx.inputs.len() - 1
    }
    /// [`Ed::add_token_input`] from a fixture token UTXO.
    pub fn add_token_utxo(&mut self, t: &TokenUtxo, witness: Witness) -> usize {
        self.add_token_input(t.utxo.clone(), t.state.clone(), witness)
    }
    pub fn sign_as(&mut self, i: usize, key: u8) {
        self.signers.insert(i, key);
    }

    // -------- outputs

    pub fn bound_to(&self, cov: [u8; 32]) -> Vec<usize> {
        self.tx
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.covenant.map(|c| c.covenant_id.as_bytes()) == Some(cov))
            .map(|(k, _)| k)
            .collect()
    }
    /// Template and state span of output k when it is an order output the library derived.
    pub fn out_order(&self, k: usize) -> Option<(TemplateId, Vec<u8>)> {
        traced(&self.tx.outputs[k].script_public_key)
    }
    /// Sets output k's script to template `id` with `state` (pinned; rewritten to the build under test at assembly).
    pub fn set_out_spk_state(&mut self, k: usize, id: TemplateId, state: &[u8]) {
        self.tx.outputs[k].script_public_key = template(id).spk(state);
    }
    pub fn is_tok_out(&self, k: usize) -> bool {
        self.tok_out.contains_key(&k)
    }
    pub fn out_state(&self, k: usize) -> TokenState {
        self.tok_out.get(&k).unwrap_or_else(|| panic!("{}: output {k} is not a token output", self.name)).1.clone()
    }
    pub fn out_token(&self, k: usize) -> [u8; 32] {
        self.tok_out.get(&k).unwrap_or_else(|| panic!("{}: output {k} is not a token output", self.name)).0
    }
    pub fn set_out_state(&mut self, k: usize, st: TokenState) {
        let cov = self.tok_out[&k].0;
        self.tok_out.insert(k, (cov, st));
    }
    /// Token outputs of token `cov`, in output order.
    pub fn tok_outs_of(&self, cov: [u8; 32]) -> Vec<usize> {
        self.tok_out.iter().filter(|(_, (c, _))| *c == cov).map(|(k, _)| *k).collect()
    }
    pub fn add_token_output(&mut self, cov: [u8; 32], st: TokenState, value: u64) -> usize {
        self.tx.outputs.push(TransactionOutput { value, script_public_key: ScriptPublicKey::default(), covenant: None });
        let k = self.tx.outputs.len() - 1;
        self.tok_out.insert(k, (cov, st));
        k
    }
    pub fn add_plain_output(&mut self, value: u64, key: u8) -> usize {
        self.tx.outputs.push(TransactionOutput { value, script_public_key: p2pk_spk(&pk(key)), covenant: None });
        self.tx.outputs.len() - 1
    }
    /// Makes output k a plain P2PK output of `key` (no covenant binding, no token).
    pub fn unbind(&mut self, k: usize, key: u8) {
        self.tok_out.remove(&k);
        self.tx.outputs[k].covenant = None;
        self.tx.outputs[k].script_public_key = p2pk_spk(&pk(key));
    }
    pub fn set_value(&mut self, k: usize, v: u64) {
        self.tx.outputs[k].value = v;
    }
    pub fn value(&self, k: usize) -> u64 {
        self.tx.outputs[k].value
    }
    pub fn swap_outputs(&mut self, a: usize, b: usize) {
        self.tx.outputs.swap(a, b);
        let (x, y) = (self.tok_out.remove(&a), self.tok_out.remove(&b));
        if let Some(v) = x {
            self.tok_out.insert(b, v);
        }
        if let Some(v) = y {
            self.tok_out.insert(a, v);
        }
    }
    /// Appends a plain P2PK input of `key` (fresh outpoint `tag`).
    pub fn add_p2pk_input(&mut self, key: u8, tag: u8) -> usize {
        let u = utxo(tag, KAS, 1_000, None);
        self.tx.inputs.push(TransactionInput::new_with_compute_budget(u.outpoint(), vec![], 0, 0));
        self.entries.push(UtxoEntry::new(u.amount, p2pk_spk(&pk(key)), u.block_daa_score, false, None));
        self.plans.push(SigPlan::P2pk { pubkey: pk(key) });
        self.tx.inputs.len() - 1
    }
    /// Pads plain inputs (the matcher's) and outputs until both counts are equal, so the next input and the next
    /// output share an index.
    pub fn align(&mut self) {
        let mut tag = 200u8;
        while self.tx.inputs.len() < self.tx.outputs.len() {
            self.add_p2pk_input(MATCHER, tag);
            tag += 1;
        }
        while self.tx.outputs.len() < self.tx.inputs.len() {
            self.add_plain_output(KAS, MATCHER);
        }
    }

    // -------- assembly

    pub fn first_token_input(&self, cov: [u8; 32]) -> usize {
        (0..self.plans.len())
            .find(|i| {
                cov_of(&self.entries[*i]) == Some(cov) && !matches!(self.plans[*i], SigPlan::Entry { .. } | SigPlan::P2pk { .. })
            })
            .expect("token input")
    }

    fn kron_witness(&self, st: &KronState) -> u8 {
        let i = match st.id_type {
            2 => (0..self.plans.len())
                .find(|i| matches!(self.plans[*i], SigPlan::Entry { .. }) && cov_of(&self.entries[*i]) == Some(st.owner)),
            3 => (0..self.plans.len()).find(|i| matches!(&self.plans[*i], SigPlan::P2pk { pubkey } if *pubkey == st.owner)),
            _ => None,
        };
        i.unwrap_or(127) as u8
    }

    /// Rewrites the scripts of the outputs of the templates under test to the build under test, and re-derives the covenant id of
    /// every genesis whose outputs changed (only where the binding holds the honest id of the pinned outputs).
    fn rewrite_outputs(&self, subs: &Subs, tx: &mut Transaction, tok_out: &mut BTreeMap<usize, ([u8; 32], TokenState)>) {
        let pinned: Vec<TransactionOutput> = tx.outputs.clone();
        for o in tx.outputs.iter_mut() {
            if let Some(spk) = subs.rewrite(&o.script_public_key) {
                o.script_public_key = spk;
            }
        }
        // genesis groups: bindings whose id is not the authorising input's own covenant id
        let mut groups: BTreeMap<([u8; 32], u16), Vec<usize>> = BTreeMap::new();
        for (k, o) in tx.outputs.iter().enumerate() {
            if let Some(b) = o.covenant {
                let own = self.entries.get(b.authorizing_input as usize).and_then(cov_of);
                if own != Some(b.covenant_id.as_bytes()) {
                    groups.entry((b.covenant_id.as_bytes(), b.authorizing_input)).or_default().push(k);
                }
            }
        }
        for ((old, auth), ks) in groups {
            if ks.iter().all(|k| pinned[*k].script_public_key == tx.outputs[*k].script_public_key) {
                continue;
            }
            let op = tx.inputs[auth as usize].previous_outpoint;
            let strip = |o: &TransactionOutput| TransactionOutput {
                value: o.value,
                script_public_key: o.script_public_key.clone(),
                covenant: None,
            };
            let honest_pinned =
                covenant_id(op, ks.iter().map(|k| (*k as u32, strip(&pinned[*k]))).collect::<Vec<_>>().iter().map(|(k, o)| (*k, o)))
                    .as_bytes();
            if honest_pinned != old {
                continue;
            }
            let new = covenant_id(
                op,
                ks.iter().map(|k| (*k as u32, strip(&tx.outputs[*k]))).collect::<Vec<_>>().iter().map(|(k, o)| (*k, o)),
            )
            .as_bytes();
            for k in &ks {
                tx.outputs[*k].covenant = Some(CovenantBinding { authorizing_input: auth, covenant_id: Hash::from_bytes(new) });
            }
            for (_, (_, st)) in tok_out.iter_mut() {
                if st.is_covenant_owned() && st.owner() == old {
                    crate::fx::set_owner(st, new);
                }
            }
        }
    }

    /// The signed transaction and its UTXO entries, with the templates under test.
    pub fn finish(&self, subs: &Subs) -> (Transaction, Vec<UtxoEntry>) {
        let mut tx = self.tx.clone();
        let mut entries = self.entries.clone();
        let mut plans = self.plans.clone();
        let mut tok_out = self.tok_out.clone();
        self.rewrite_outputs(subs, &mut tx, &mut tok_out);
        // token outputs: script and binding to the token's first input
        for (k, (cov, st)) in &tok_out {
            tx.outputs[*k].script_public_key = st.spk_with(token_template(self.programs[cov]));
            let leader = self.first_token_input(*cov);
            tx.outputs[*k].covenant = Some(CovenantBinding { authorizing_input: leader as u16, covenant_id: Hash::from_bytes(*cov) });
        }
        // leader next states (KCC-20), next-state and witness columns (KRON)
        for cov in self.programs.keys() {
            let next: Vec<TokenState> = tok_out.values().filter(|(c, _)| c == cov).map(|(_, s)| s.clone()).collect();
            let leader = self.first_token_input(*cov);
            let witnesses: Vec<u8> = (0..plans.len())
                .filter(|i| cov_of(&entries[*i]) == Some(*cov))
                .filter_map(|i| match &plans[i] {
                    SigPlan::KronToken { state, .. } => Some(self.kron_witness(state)),
                    _ => None,
                })
                .collect();
            for (i, p) in plans.iter_mut().enumerate() {
                if cov_of(&entries[i]) != Some(*cov) {
                    continue;
                }
                match p {
                    SigPlan::TokenLeader { next_states, .. } if i == leader => {
                        *next_states = next.iter().map(|s| s.as_kcc20().expect("kcc20").clone()).collect();
                    }
                    SigPlan::TokenDelegator { template, state, witness } if i == leader => {
                        let ns = next.iter().map(|s| s.as_kcc20().expect("kcc20").clone()).collect();
                        *p = SigPlan::TokenLeader {
                            template: *template,
                            state: state.clone(),
                            next_states: ns,
                            witness: witness.clone(),
                        };
                    }
                    SigPlan::KronToken { next_states, witnesses: w, .. } => {
                        *next_states = next
                            .iter()
                            .map(|s| match s {
                                TokenState::Kron(k) => k.clone(),
                                _ => panic!("KRON token with a KCC-20 state"),
                            })
                            .collect();
                        *w = witnesses.clone();
                    }
                    _ => {}
                }
            }
        }
        // the exit prefix / suffix arguments of an if-done entry (KobIfdPair: KobCondPair, KobIfdBid: KobCondAsk,
        // KobIfdAsk: KobCondBid)
        for (entry_t, exit_t) in EXIT_OF.into_iter().filter(|(_, x)| subs.has(*x)) {
            let (cp, cs) = (&template(exit_t).prefix, &template(exit_t).suffix);
            let csub = subs.get(exit_t);
            for p in plans.iter_mut() {
                if let SigPlan::Entry { template, args, .. } = p {
                    if *template != entry_t {
                        continue;
                    }
                    for a in args.iter_mut() {
                        if let Arg::Bytes(b) = a {
                            if b == cp {
                                *b = csub.prefix.clone();
                            } else if b == cs {
                                *b = csub.suffix.clone();
                            }
                        }
                    }
                }
            }
        }
        // script public keys of the spent UTXOs; orders under the templates being tested
        for (i, p) in plans.iter().enumerate() {
            let spk = match p {
                SigPlan::P2pk { pubkey } => p2pk_spk(pubkey),
                SigPlan::Entry { template: t, state, .. } if subs.has(*t) => subs.get(*t).spk(state),
                other => other.spk(),
            };
            let e = &entries[i];
            entries[i] = UtxoEntry::new(e.amount, spk, e.block_daa_score, e.is_coinbase, e.covenant_id);
        }
        // signatures
        for inp in tx.inputs.iter_mut() {
            inp.signature_script.clear();
        }
        let keys = keys();
        let mut sigs: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for (i, p) in plans.iter().enumerate() {
            let Some(signer) = p.signer() else { continue };
            let key = self.signers.get(&i).map(|k| pk(*k)).unwrap_or(signer);
            let ht = self.sighash_types.get(&i).copied().unwrap_or(1);
            let mt = MutableTransaction::with_entries(tx.clone(), entries.clone());
            let reused = SigHashReusedValuesUnsync::new();
            let digest = calc_schnorr_signature_hash(&mt.as_verifiable(), i, SigHashType::from_u8(ht).expect("sighash type"), &reused)
                .as_bytes();
            let mut sig = sign_digest(&keys[&key], &digest).expect("sign");
            sig[64] = ht;
            sigs.insert(i, sig);
        }
        for (i, p) in plans.iter().enumerate() {
            let sig = sigs.get(&i).map(|s| s.as_slice());
            tx.inputs[i].signature_script = match p {
                SigPlan::Entry { template: t, state, entry, args } if subs.has(*t) => {
                    let vals: Vec<ArtifactValue> = args
                        .iter()
                        .map(|a| match a {
                            Arg::Int(v) => ArtifactValue::Int(*v),
                            Arg::Bytes(b) => ArtifactValue::Bytes(b.clone()),
                            Arg::Sig(_) => ArtifactValue::Bytes(sig.expect("signature").to_vec()),
                        })
                        .collect();
                    entry_sigscript(template(*t), &subs.get(*t).redeem(state), entry, &vals)
                        .unwrap_or_else(|e| panic!("{}: input {i} sigscript: {e}", self.name))
                }
                SigPlan::P2pk { .. } => push_data(sig.expect("p2pk signature")),
                other => other.sigscript(sig).unwrap_or_else(|e| panic!("{}: input {i} sigscript: {e}", self.name)),
            };
        }
        for (i, raw) in &self.raw_sigscripts {
            tx.inputs[*i].signature_script = raw.clone();
        }
        (tx, entries)
    }
}

// ---------------------------------------------------------------- scenario runner

/// Runs scenarios of one program pair against the templates under test. Positives (`ok`) must pass on every input;
/// attacks (`bad`) must be rejected by the named input. Output lines: `POSITIVE <name> <pair>  [PASS]`, `NEGATIVE
/// <name> <pair>  [REJECTED at in[i]: ...]`; under `KOB_ABLATION`, `ABLATION-PASS <name> <pair> all_inputs_ok=<bool>` for
/// an accepted attack and `ABLATION-POS-FAIL <name> <pair> (...)` for a rejected positive (tools/ablation).
pub struct Run<'a> {
    pub subs: &'a Subs,
    pub pair: String,
}

impl Run<'_> {
    pub fn exec(&self, ed: &Ed) -> Vec<Result<u64, String>> {
        let (tx, entries) = ed.finish(self.subs);
        kob_protocol::verify::execute(&tx, &entries, false)
            .unwrap_or_else(|e| panic!("{} {}: tx-level failure {e}", ed.name, self.pair))
    }
    pub fn ok(&self, ed: &Ed) {
        let res = self.exec(ed);
        let failed: Vec<String> =
            res.iter().enumerate().filter_map(|(i, r)| r.as_ref().err().map(|e| format!("in[{i}]: {e}"))).collect();
        if failed.is_empty() {
            println!("POSITIVE {} {}  [PASS]", ed.name, self.pair);
        } else if ablating() {
            println!("ABLATION-POS-FAIL {} {} ({})", ed.name, self.pair, failed.join("; "));
        } else {
            panic!("{} {}: positive scenario rejected: {}", ed.name, self.pair, failed.join("; "));
        }
    }
    /// An honest build of `a`, run as a positive.
    pub fn ok_action(&self, name: &str, a: Action) {
        self.ok(&Ed::new(name, &built(a)));
    }
    pub fn bad(&self, ed: &Ed, expect: usize) {
        let res = self.exec(ed);
        let r = &res[expect];
        if r.is_ok() && ablating() {
            println!("ABLATION-PASS {} {} all_inputs_ok={}", ed.name, self.pair, res.iter().all(|r| r.is_ok()));
            return;
        }
        assert!(r.is_err(), "{} {}: expected input {expect} to REJECT but it passed", ed.name, self.pair);
        let others: Vec<String> =
            res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
        println!(
            "NEGATIVE {} {}  [REJECTED at in[{expect}]: {}]{}",
            ed.name,
            self.pair,
            r.as_ref().unwrap_err(),
            if others.is_empty() { String::new() } else { format!(" (also failing: {})", others.join(",")) }
        );
    }
}

/// The four family mixes of a pair A/B: (A program, B program).
pub fn family_mixes() -> [(TemplateId, TemplateId); 4] {
    use TemplateId::{Kcc20Ref8x8, KronToken2433, KronToken2732};
    [(Kcc20Ref8x8, Kcc20Ref8x8), (Kcc20Ref8x8, KronToken2433), (KronToken2433, Kcc20Ref8x8), (KronToken2732, KronToken2433)]
}

/// The first four bytes of a hash in hex.
pub fn hex8(h: &[u8]) -> String {
    h.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

impl Ed {
    /// Recompute the genesis covenant id of the single authorising input `auth` over the CURRENT (pinned-template)
    /// outputs and rebind its group (and the token outputs it owns) to it. Call after editing the committed state of an
    /// IFD exit (`set_out_spk_state`) so the transaction stays a valid genesis: `finish` then recomputes the id again for
    /// the build under test, and the contract's own exit checks (the exit script equality, the recomputed id) see the
    /// edited state. A no-op when `auth` authorises no genesis group.
    pub fn rebind_genesis(&mut self, auth: u16) {
        let own = self.entries.get(auth as usize).and_then(cov_of);
        let ks: Vec<usize> = self
            .tx
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o.covenant, Some(b) if b.authorizing_input == auth && Some(b.covenant_id.as_bytes()) != own))
            .map(|(k, _)| k)
            .collect();
        if ks.is_empty() {
            return;
        }
        let old = self.tx.outputs[ks[0]].covenant.expect("binding").covenant_id.as_bytes();
        let op = self.tx.inputs[auth as usize].previous_outpoint;
        let strip = |o: &TransactionOutput| TransactionOutput {
            value: o.value,
            script_public_key: o.script_public_key.clone(),
            covenant: None,
        };
        let new = covenant_id(
            op,
            ks.iter().map(|k| (*k as u32, strip(&self.tx.outputs[*k]))).collect::<Vec<_>>().iter().map(|(k, o)| (*k, o)),
        )
        .as_bytes();
        for k in &ks {
            self.tx.outputs[*k].covenant = Some(CovenantBinding { authorizing_input: auth, covenant_id: Hash::from_bytes(new) });
        }
        for (_, (_, st)) in self.tok_out.iter_mut() {
            if st.is_covenant_owned() && st.owner() == old {
                crate::fx::set_owner(st, new);
            }
        }
    }
}

// ---------------------------------------------------------------- generic attack helpers (appended; used by the suites)

use kob_protocol::state::{Kcc20State, KronState};

/// `id` ("NNnn text") with `suffix` appended to its first whitespace token.
pub fn with_suffix(id: &str, suffix: &str) -> String {
    match id.split_once(' ') {
        Some((h, t)) => format!("{h}{suffix} {t}"),
        None => format!("{id}{suffix}"),
    }
}

impl Ed {
    /// The order input of covenant `cov` (an `Entry` call).
    pub fn order_in(&self, cov: [u8; 32]) -> usize {
        self.input_of_cov(cov)
    }
    /// The continuation output of covenant `c` (an order output the library derived bound to it), if any.
    pub fn cont_out(&self, c: [u8; 32]) -> Option<usize> {
        self.bound_to(c).into_iter().find(|k| self.out_order(*k).is_some())
    }
    /// The token state of token input i.
    pub fn in_tok(&self, i: usize) -> TokenState {
        match &self.plans[i] {
            SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. } => TokenState::Kcc20(state.clone()),
            SigPlan::KronToken { state, .. } => TokenState::Kron(state.clone()),
            p => panic!("{}: input {i} is not a token input: {p:?}", self.name),
        }
    }
    pub fn set_in_tok(&mut self, i: usize, st: TokenState) {
        match (&mut self.plans[i], st) {
            (SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. }, TokenState::Kcc20(k)) => *state = k,
            (SigPlan::KronToken { state, .. }, TokenState::Kron(k)) => *state = k,
            (p, _) => panic!("input {i}: {p:?} with a state of the other family"),
        }
    }
    /// The one token output of `tok` owned by `owner`.
    pub fn tok_out_of(&self, tok: [u8; 32], owner: [u8; 32]) -> usize {
        let v: Vec<usize> = self.tok_outs_of(tok).into_iter().filter(|k| self.out_state(*k).owner() == owner).collect();
        assert_eq!(v.len(), 1, "{}: token outputs of {:02x} owned by {:02x}: {v:?}", self.name, tok[0], owner[0]);
        v[0]
    }
    pub fn add_out_amount(&mut self, k: usize, d: i64) {
        let st = self.out_state(k);
        self.set_out_state(k, st.with_amount(st.amount() + d));
    }
    /// The matcher's or keeper's largest plain KAS output.
    pub fn change_out(&self) -> usize {
        (0..self.tx.outputs.len())
            .filter(|k| {
                self.tx.outputs[*k].covenant.is_none()
                    && [MATCHER, 6u8].iter().any(|m| self.tx.outputs[*k].script_public_key == p2pk_spk(&pk(*m)))
            })
            .max_by_key(|k| self.tx.outputs[*k].value)
            .expect("the matcher's change")
    }
    /// Moves `d` sompi from the change to output k.
    pub fn fund_out(&mut self, k: usize, d: i64) {
        let c = self.change_out();
        let (vc, vk) = (self.value(c) as i64, self.value(k) as i64);
        self.set_value(c, (vc - d) as u64);
        self.set_value(k, (vk + d) as u64);
    }
    /// Adds a stray of `tok` (program `p`, `amount`) owned by covenant `owner`, routed to the matcher.
    pub fn add_stray(&mut self, tok: [u8; 32], p: TemplateId, owner: [u8; 32], amount: i64, tag: u8) -> usize {
        self.programs.entry(tok).or_insert(p);
        let st = TokenState::custody(p.family(), amount, owner, crate::fx::ext_for(p));
        let i = self.add_token_input(utxo(tag, KAS, 1_000, Some(tok)), st, Witness::CovenantId);
        self.add_token_output(tok, TokenState::user(p.family(), amount, pk(MATCHER), crate::fx::ext_for(p)), KAS);
        i
    }
    /// An output bound to covenant `c` (authorised by input `auth`) paying `v` sompi to the matcher (a forged UTXO of `c`).
    pub fn forge_bound(&mut self, c: [u8; 32], auth: usize, v: u64) -> usize {
        let k = self.add_plain_output(0, MATCHER);
        self.fund_out(k, v as i64);
        self.tx.outputs[k].covenant = Some(CovenantBinding { authorizing_input: auth as u16, covenant_id: Hash::from_bytes(c) });
        k
    }
}

/// `OP_DROP OP_TRUE` (not P2SH).
pub fn drop_true_spk() -> ScriptPublicKey {
    use kaspa_txscript::opcodes::codes::{OpDrop, OpTrue};
    ScriptPublicKey::new(0, vec![OpDrop, OpTrue].into())
}

/// Push-only filler of exactly `len` bytes.
pub fn filler_bytes(mut len: usize) -> Vec<u8> {
    let mut v = vec![];
    while len > 0 {
        let take = len.min(300);
        let rest = len - take;
        let (take, rest) = if rest > 0 && rest < 2 { (take - 2, rest + 2) } else { (take, rest) };
        if take <= 76 {
            let m = take - 1;
            v.push(m as u8);
            v.extend(std::iter::repeat_n(0xaau8, m));
        } else if take <= 257 {
            let m = take - 2;
            v.extend([0x4c, m as u8]);
            v.extend(std::iter::repeat_n(0xaau8, m));
        } else {
            let m = take - 3;
            v.push(0x4d);
            v.extend_from_slice(&(m as u16).to_le_bytes());
            v.extend(std::iter::repeat_n(0xaau8, m));
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
    assert_eq!(i, b.len());
    n
}

/// A look-alike redeem script (`pre` bytes of pushes, the state, `suf` bytes that drop every push and return true).
pub fn fake_rs(pre: usize, state: &[u8], suf: usize) -> Vec<u8> {
    use kaspa_txscript::opcodes::codes::{OpDrop, OpEndIf, OpFalse, OpIf, OpNop, OpTrue};
    let mut v = filler_bytes(pre);
    v.extend_from_slice(state);
    let drops = push_count(&v);
    assert!(suf > drops, "suffix too short for the look-alike");
    v.extend(std::iter::repeat_n(OpDrop, drops));
    let pad = suf - drops - 1;
    match pad {
        0 => {}
        1 | 2 => v.extend(std::iter::repeat_n(OpNop, pad)),
        _ => {
            v.extend([OpFalse, OpIf]);
            v.extend(filler_bytes(pad - 3));
            v.push(OpEndIf);
        }
    }
    v.push(OpTrue);
    assert_eq!(v.len(), pre + state.len() + suf);
    v
}

/// Input i becomes a planted UTXO of covenant `cov`: its sigscript pushes `rs`; P2SH(rs) when `p2sh`, else `OP_DROP OP_TRUE`.
pub fn plant_input(tx: &mut Transaction, en: &mut [UtxoEntry], i: usize, cov: [u8; 32], rs: &[u8], p2sh: bool) {
    let spk = if p2sh { p2sh_spk(rs) } else { drop_true_spk() };
    let e = &en[i];
    en[i] = UtxoEntry::new(e.amount, spk, e.block_daa_score, false, Some(Hash::from_bytes(cov)));
    tx.inputs[i].signature_script = push_data(rs);
}

/// A patch applied to a finished transaction before execution.
pub type Patch<'a> = &'a dyn Fn(&mut Transaction, &mut Vec<UtxoEntry>);

fn redo_sig(tx: &Transaction, en: &[UtxoEntry], i: usize, key: [u8; 32], ht: u8) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), en.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let digest = calc_schnorr_signature_hash(&mt.as_verifiable(), i, SigHashType::from_u8(ht).unwrap(), &reused).as_bytes();
    let mut s = sign_digest(&keys()[&key], &digest).expect("sign");
    s[64] = ht;
    s
}

impl Run<'_> {
    /// The finished transaction with `patch` applied, every signature redone over the patched transaction.
    pub fn finish_patched(&self, e: &Ed, patch: Patch) -> (Transaction, Vec<UtxoEntry>) {
        let (tx0, en0) = e.finish(self.subs);
        let (mut tx, mut en) = (tx0.clone(), en0.clone());
        patch(&mut tx, &mut en);
        for (i, p) in e.plans.iter().enumerate() {
            let Some(signer) = p.signer() else { continue };
            let key = e.signers.get(&i).map(|k| pk(*k)).unwrap_or(signer);
            let ht = e.sighash_types.get(&i).copied().unwrap_or(1);
            let (old, new) = (redo_sig(&tx0, &en0, i, key, ht), redo_sig(&tx, &en, i, key, ht));
            let ss = &mut tx.inputs[i].signature_script;
            if let Some(at) = ss.windows(65).position(|w| w == old.as_slice()) {
                ss[at..at + 65].copy_from_slice(&new);
            }
        }
        (tx, en)
    }
    pub fn exec_patched(&self, e: &Ed, patch: Patch) -> Vec<Result<u64, String>> {
        let (tx, en) = self.finish_patched(e, patch);
        kob_protocol::verify::execute(&tx, &en, false).unwrap_or_else(|x| panic!("{} {}: tx-level failure {x}", e.name, self.pair))
    }
    pub fn ok_patched(&self, e: &Ed, patch: Patch) {
        let res = self.exec_patched(e, patch);
        let failed: Vec<String> =
            res.iter().enumerate().filter_map(|(i, x)| x.as_ref().err().map(|m| format!("in[{i}]: {m}"))).collect();
        if failed.is_empty() {
            println!("POSITIVE {} {}  [PASS]", e.name, self.pair);
        } else if ablating() {
            println!("ABLATION-POS-FAIL {} {} ({})", e.name, self.pair, failed.join("; "));
        } else {
            panic!("{} {}: positive scenario rejected: {}", e.name, self.pair, failed.join("; "));
        }
    }
    pub fn bad_patched(&self, e: &Ed, expect: usize, patch: Patch) {
        let res = self.exec_patched(e, patch);
        let x = &res[expect];
        if x.is_ok() && ablating() {
            println!("ABLATION-PASS {} {} all_inputs_ok={}", e.name, self.pair, res.iter().all(|r| r.is_ok()));
            return;
        }
        assert!(x.is_err(), "{} {}: expected input {expect} to REJECT but it passed", e.name, self.pair);
        let others: Vec<String> =
            res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
        println!(
            "NEGATIVE {} {}  [REJECTED at in[{expect}]: {}]{}",
            e.name,
            self.pair,
            x.as_ref().unwrap_err(),
            if others.is_empty() { String::new() } else { format!(" (also failing: {})", others.join(",")) }
        );
    }
}

/// The mixed family pairs (every family is S and T of some side on each).
pub fn mixed_pairs() -> [(TemplateId, TemplateId); 2] {
    let m = family_mixes();
    [m[1], m[2]]
}

/// `"k"` for a KCC-20 program, `"r"` for KRON.
pub fn fam_tag(p: TemplateId) -> &'static str {
    if p.family() == kob_protocol::Family::Kcc20 {
        "k"
    } else {
        "r"
    }
}

/// Edits a `Kcc20State` or `KronState` through closures (whichever it is).
pub fn map_tok(st: TokenState, kcc: impl FnOnce(Kcc20State) -> Kcc20State, kron: impl FnOnce(KronState) -> KronState) -> TokenState {
    match st {
        TokenState::Kcc20(k) => TokenState::Kcc20(kcc(k)),
        TokenState::Kron(k) => TokenState::Kron(kron(k)),
    }
}

// ---------------------------------------------------------------- stray at a given scan slot (appended)

/// Places a stray of the KCC-20 token `tok` owned by covenant `owner` at scan slot `k` of that token (the k-th input
/// carrying `tok`, 0-based: what `OpCovInputIdx(tok, k)` of a `noStrays` scan reads), every unit going to the matcher's
/// one output of `tok`. The transaction must carry exactly one input of `tok`, a key-held KCC-20 UTXO of the matcher.
/// Slot 0 turns that input into the stray (owned by `owner`, authorised by its covenant id); slot k >= 1 appends k - 1
/// key-held inputs of one unit of the matcher, then the stray (3 units). With `stray` false the last input is one more
/// key-held unit of the matcher (the honest twin of the same shape). Returns the input index at slot k.
pub fn stray_at_slot(e: &mut Ed, tok: [u8; 32], owner: [u8; 32], k: usize, stray: bool) -> usize {
    let ins: Vec<usize> = (0..e.plans.len())
        .filter(|i| cov_of(&e.entries[*i]) == Some(tok) && !matches!(e.plans[*i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }))
        .collect();
    assert_eq!(ins.len(), 1, "{}: one input of the token expected, found {ins:?}", e.name);
    let p = e.programs[&tok];
    assert_eq!(p.family(), kob_protocol::Family::Kcc20, "{}: the slot battery runs on a KCC-20 token", e.name);
    let ext = crate::fx::ext_for(p);
    let m = e.tok_out_of(tok, pk(MATCHER));
    if k == 0 {
        let i = ins[0];
        if stray {
            match &mut e.plans[i] {
                SigPlan::TokenLeader { state, witness, .. } | SigPlan::TokenDelegator { state, witness, .. } => {
                    assert_eq!(state.owner, pk(MATCHER), "{}: the matcher's input", e.name);
                    *state = Kcc20State::custody(state.amount, owner, ext);
                    *witness = Witness::CovenantId;
                }
                q => panic!("{}: input {i} is no KCC-20 input: {q:?}", e.name),
            }
        }
        return i;
    }
    for j in 0..k - 1 {
        e.add_token_input(
            utxo(0xc0 + j as u8, KAS, 1_000, Some(tok)),
            TokenState::user(p.family(), 1, pk(MATCHER), ext),
            Witness::P2pk(pk(MATCHER)),
        );
        e.add_out_amount(m, 1);
    }
    let i = if stray {
        e.add_token_input(utxo(0xcf, KAS, 1_000, Some(tok)), TokenState::custody(p.family(), 3, owner, ext), Witness::CovenantId)
    } else {
        e.add_token_input(
            utxo(0xcf, KAS, 1_000, Some(tok)),
            TokenState::user(p.family(), 3, pk(MATCHER), ext),
            Witness::P2pk(pk(MATCHER)),
        )
    };
    e.add_out_amount(m, 3);
    i
}
