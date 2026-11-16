//! Engine harness of the pair order suites (`KobPair`, `KobCondPair`, `KobIfdPair`; contracts/orders): the three pair
//! templates compiled from their (possibly mutated) sources, and a transaction editor over honest `kob-protocol` builds.
//!
//! Include it next to the `kob-protocol` fixtures in a test binary:
//! ```ignore
//! #[path = "../common/mod.rs"]
//! mod common;
//! #[path = "../../../kob-protocol/tests/common/mod.rs"]
//! mod fx;
//! #[path = "../common/pair_harness.rs"]
//! mod ph;
//! ```
//!
//! Templates under test ([`Subs`]): `KobPair`, then `KobCondPair` (its build constants PAIR_TPL / PRE / SUF re-pointed
//! at the compiled KobPair), then `KobIfdPair` (COND_TPL / PRE / SUF and PAIR_TPL / PRE / SUF re-pointed);
//! [`Subs::compile_asks`]: the KCC-20 ask family and its readers instead (`KobAsk`, then `KobCondAsk` and `KobCondBid`
//! against it, `KobIfdAsk` against KobCondBid and KobAsk, `KobIfdBid` against KobCondAsk). An ablation
//! run (`KOB_ABLATION_SRC` = a directory with mutated sources, `KOB_ABLATION=1`) therefore executes the weakened covenant
//! everywhere it appears: as the order filled, as trigger evidence of a conditional, as the exit an entry creates.
//! Without an ablation the compiled templates must equal the pinned ones, except the templates whose re-pin is pending
//! ([`REPIN_PENDING`]). A mutation must not move the state span.
//!
//! Bid delivery modes (docs/design/bid-delivery-spec.md): the pair templates carry a delivery block appended to their
//! state (`KobPair` / `KobCondPair`: mode, recvThreshold, topupTip, bought; `KobIfdPair`: the three exit fields).
//! Until the re-pin the library builds states of the pinned (shorter) length; [`full_state`] appends the default block
//! (mode 0, all zero) to such a state, so every honest build runs in mode 0. A scenario sets another block per input
//! ([`Ed::dlv_in`]) or per order output ([`Ed::dlv_out`], continuations and exits). An entry call with fewer arguments
//! than the compiled entry takes (the new trailing `form`) is padded with zeros, and its signature script is encoded with
//! the compiled artifact's ABI. [`Ed::borrow_leaders`] turns a KCC-20 leader to the borrow path (witness `0x01`).
//!
//! [`Ed::finish`] rewrites every pair-template script of the transaction to the build under test: the redeem scripts of
//! the spent pair UTXOs, every output whose script public key the library derived from a pair template (looked up in
//! `kob_protocol::artifacts::spk_trace`), the KobCondPair prefix / suffix arguments of a KobIfdPair fill, and the
//! covenant id of a genesis whose outputs changed (an IFD exit: binding and the custody owned by it). A genesis id that
//! is not the honest id of the pinned outputs (a forged exit id) is left as it is.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

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
use kob_protocol::script::{entry_sigscript, kcc20_state_value, p2pk_spk, p2sh_spk, push_data};
use kob_protocol::state::*;
use kob_protocol::tx::{sign_digest, Arg, BuiltTx, SigPlan, TokenUtxo, Utxo, Witness};
use silverscript_abi::{encode_contract_entry_sig_script, ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

use crate::common;
use crate::fx::{keys, pk, utxo, KAS, MAKER_A, MATCHER};

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

/// (template, its pinned state length before the bid delivery modes, the number of int fields of its delivery block):
/// the templates whose sources changed with the delivery modes while their pinned artifacts wait for the re-pin
/// (package P1). Their compiled build is not compared with the pinned artifact while the pinned state still has the
/// old length, and a state span of that length is extended by the default delivery block ([`full_state`]).
pub const REPIN_PENDING: [(TemplateId, usize, usize); 3] =
    [(TemplateId::KobPair, 447, 4), (TemplateId::KobCondPair, 543, 4), (TemplateId::KobIfdPair, 909, 3)];

/// The delivery block of int fields `v` (`0x08` + 8 bytes little-endian each).
pub fn dlv_block(v: &[i64]) -> Vec<u8> {
    v.iter().flat_map(|x| [vec![0x08u8], x.to_le_bytes().to_vec()].concat()).collect()
}

/// Delivery block of `KobPair` / `KobCondPair`: mode, recvThreshold, topupTip, bought.
pub fn dlv(mode: i64, thr: i64, tip: i64, bought: i64) -> Vec<u8> {
    dlv_block(&[mode, thr, tip, bought])
}

/// Delivery block of `KobIfdPair`: exitMode, exitRecvThreshold, exitTopupTip.
pub fn xdlv(mode: i64, thr: i64, tip: i64) -> Vec<u8> {
    dlv_block(&[mode, thr, tip])
}

/// The pre-delivery state length and the delivery block size (bytes) of `id`, if its re-pin is pending.
pub fn pending(id: TemplateId) -> Option<(usize, usize)> {
    REPIN_PENDING.iter().find(|(t, ..)| *t == id).map(|(_, old, nf)| (*old, nf * 9))
}

/// `state` of template `id` with `block` (or the default delivery block, all zero) appended when it has the pre-delivery
/// length.
pub fn full_state_with(id: TemplateId, state: &[u8], block: Option<&[u8]>) -> Vec<u8> {
    match pending(id) {
        Some((old, size)) if state.len() == old => {
            let d = block.map(|b| b.to_vec()).unwrap_or_else(|| dlv_block(&vec![0; size / 9]));
            assert_eq!(d.len(), size, "{}: delivery block of {size} bytes", id.name());
            [state, d.as_slice()].concat()
        }
        _ => {
            assert!(block.is_none(), "{}: a delivery block for a state of {} bytes", id.name(), state.len());
            state.to_vec()
        }
    }
}

/// [`full_state_with`] with the default block.
pub fn full_state(id: TemplateId, state: &[u8]) -> Vec<u8> {
    full_state_with(id, state, None)
}

type Abis = Mutex<BTreeMap<[u8; 32], (SilAbiArtifact, String)>>;

/// The compiled artifacts by template hash: the ABI the signature scripts of the templates under test are encoded with.
fn abis() -> &'static Abis {
    static ABIS: OnceLock<Abis> = OnceLock::new();
    ABIS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Signature script of entry `entry` of the template under test `sub` (redeem script `redeem`): encoded with its compiled
/// ABI when known (the arguments padded with zeros up to the entry's parameter count), else with the pinned one.
pub fn sub_entry_sigscript(sub: &Sub, redeem: &[u8], entry: &str, vals: &[ArtifactValue]) -> Result<Vec<u8>, String> {
    let known = abis().lock().unwrap().get(&sub.hash).cloned();
    match known {
        Some((art, name)) => {
            let want = art.contracts[&name].entries.get(entry).map(|e| e.params.len()).unwrap_or(vals.len());
            let mut v = vals.to_vec();
            while v.len() < want {
                v.push(ArtifactValue::Int(0));
            }
            let mut s =
                encode_contract_entry_sig_script(&art, &name, entry, &v).map_err(|e| format!("{}.{entry}: {e}", sub.id.name()))?;
            s.extend_from_slice(&push_data(redeem));
            Ok(s)
        }
        None => entry_sigscript(template(sub.id), redeem, entry, vals),
    }
}

/// Prefix and suffix of a pair template compiled from its (possibly mutated) source.
#[derive(Clone)]
pub struct Sub {
    pub id: TemplateId,
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub hash: [u8; 32],
}

impl Sub {
    /// The redeem script of `state` (a pre-delivery state gets the default delivery block, [`full_state`]).
    pub fn redeem(&self, state: &[u8]) -> Vec<u8> {
        [self.prefix.as_slice(), &full_state(self.id, state), self.suffix.as_slice()].concat()
    }
    /// The redeem script of `state` with the delivery block `block` (or the default one).
    pub fn redeem_with(&self, state: &[u8], block: Option<&[u8]>) -> Vec<u8> {
        [self.prefix.as_slice(), &full_state_with(self.id, state, block), self.suffix.as_slice()].concat()
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
            let ctor = format!("contracts/orders/{}.ctor.json", id.name());
            let mut args: Vec<ArtifactValue> =
                serde_json::from_str(&std::fs::read_to_string(common::repo_root().join(&ctor)).unwrap()).expect("ctor json");
            for d in ds {
                repoint(&mut args, *d, &subs[d]);
            }
            let art = common::compile_contract(&common::contract_source(id.name()), &args, CompileOptions::default())
                .unwrap_or_else(|e| panic!("compile {}: {e:?}", id.name()));
            let (prefix, suffix, hash) = common::compiled_template_parts_and_hash(&art);
            let pinned = template(id);
            let repin = pending(id).is_some_and(|(old, _)| pinned.state_len == old);
            if !ablating() && !repin {
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
            let hash: [u8; 32] = hash.try_into().expect("32-byte hash");
            let name = art.contracts.keys().next().expect("one contract").clone();
            abis().lock().unwrap().insert(hash, (art.clone(), name));
            subs.insert(id, Sub { id, prefix, suffix, hash });
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
    /// Delivery block of the order spent at input i (a template under test with a pending re-pin; default: all zero).
    pub dlv_in: BTreeMap<usize, Vec<u8>>,
    /// Delivery block of the order output k (a continuation or an exit the library derived; default: all zero).
    pub dlv_out: BTreeMap<usize, Vec<u8>>,
    /// KCC-20 leaders that spend on the borrow path (witness `0x01` alone).
    pub borrow_leaders: BTreeSet<usize>,
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
            dlv_in: BTreeMap::new(),
            dlv_out: BTreeMap::new(),
            borrow_leaders: BTreeSet::new(),
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
            TokenState::Kron(k) => {
                SigPlan::KronToken { template: tpl, state: k.clone(), next_states: vec![], witnesses: vec![], signer: None }
            }
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
        for (k, o) in tx.outputs.iter_mut().enumerate() {
            if let Some(d) = self.dlv_out.get(&k) {
                let (id, st) = match spk_trace::lookup(&o.script_public_key) {
                    Some((Origin::Template(id), st)) if subs.has(id) => (id, st),
                    _ => panic!("{}: dlv_out at output {k}, which is no order output of a template under test", self.name),
                };
                o.script_public_key = p2sh_spk(&subs.get(id).redeem_with(&st, Some(d)));
            } else if let Some(spk) = subs.rewrite(&o.script_public_key) {
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
                SigPlan::Entry { template: t, state, .. } if subs.has(*t) => {
                    p2sh_spk(&subs.get(*t).redeem_with(state, self.dlv_in.get(&i).map(|v| v.as_slice())))
                }
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
                    let sub = subs.get(*t);
                    sub_entry_sigscript(sub, &sub.redeem_with(state, self.dlv_in.get(&i).map(|v| v.as_slice())), entry, &vals)
                        .unwrap_or_else(|e| panic!("{}: input {i} sigscript: {e}", self.name))
                }
                SigPlan::TokenLeader { template: t, state, next_states, .. } if self.borrow_leaders.contains(&i) => {
                    let tpl = template(*t);
                    let next = ArtifactValue::Array(next_states.iter().map(kcc20_state_value).collect());
                    entry_sigscript(tpl, &state.redeem_with(tpl), "transfer", &[next, ArtifactValue::Bytes(vec![0x01])])
                        .unwrap_or_else(|e| panic!("{}: input {i} borrow leader: {e}", self.name))
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
        let (tx, en) = ed.finish(self.subs);
        let Some(res) = self.run_fitting(ed, &tx, &en) else { return };
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
        let (tx, en) = ed.finish(self.subs);
        let Some(res) = self.run_fitting(ed, &tx, &en) else { return };
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
        let (tx, en) = self.finish_patched(e, patch);
        let Some(res) = self.run_fitting(e, &tx, &en) else { return };
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
        let (tx, en) = self.finish_patched(e, patch);
        let Some(res) = self.run_fitting(e, &tx, &en) else { return };
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

// ---------------------------------------------------------------- stop-trigger custody stand-ins (appended)

/// The stop-trigger custody attacks of `KobCondPair` / `KobIfdPair` (`rd` and `evLeg`: the custody `tk` of an ask / pair
/// evidence must be a holder UTXO of the token's program, template hash + P2SH, owned by the evidence). The real custody
/// at input `tk` is made fresh (exposed less than minRestDaa, so the honest trigger fails, as in NC51), and an OLD input
/// carrying the custody token's covenant id, whose redeem bytes read the evidence order as owner with the covenant marker,
/// is appended (a P2PK placeholder that the returned patch turns into the stand-in). `look`: a look-alike holder under
/// another program (the P2SH of a redeem script of the same length that keeps the genuine state bytes, other prefix /
/// suffix: another template hash); else the GENUINE holder redeem pushed by a non-P2SH input. The evidence order's own
/// stray guard (and the token program) refuse the stand-in at their inputs; the attack is judged at the order's input.
/// Returns the stand-in's input index (name it as tk) and the patch (`Run::bad_patched`).
pub fn old_custody_standin(e: &mut Ed, tk: usize, look: bool) -> (usize, impl Fn(&mut Transaction, &mut Vec<UtxoEntry>)) {
    let tok = cov_of(&e.entries[tk]).expect("the custody carries its token");
    let tt = token_template(e.programs[&tok]);
    let st = e.in_tok(tk);
    let rs = if look { fake_rs(tt.prefix.len(), &st.encode(), tt.suffix.len()) } else { st.redeem_with(tt) };
    e.set_input_daa(tk, crate::fx::NOW - 10);
    let u = utxo(0xe7, KAS, 1_000, None);
    e.tx.inputs.push(TransactionInput::new_with_compute_budget(u.outpoint(), vec![], 0, 80));
    e.entries.push(UtxoEntry::new(u.amount, p2pk_spk(&pk(MATCHER)), u.block_daa_score, false, None));
    e.plans.push(SigPlan::P2pk { pubkey: pk(MATCHER) });
    let j = e.tx.inputs.len() - 1;
    (j, move |tx: &mut Transaction, en: &mut Vec<UtxoEntry>| plant_input(tx, en, j, tok, &rs, look))
}

// ---------------------------------------------------------------- the 3 / 3 KCC-20 program (appended)

/// The mixed pairs on the 3 / 3 reference KCC-20 program (KCC20Ref, the tradable KCC-20 program on mainnet) instead of
/// KCC20Ref_8x8. A scenario whose transaction exceeds that program's token slots is skipped on it ([`slot_excess`]).
pub fn mixed_pairs_33() -> [(TemplateId, TemplateId); 2] {
    use TemplateId::{Kcc20Ref, KronToken2433};
    [(Kcc20Ref, KronToken2433), (KronToken2433, Kcc20Ref)]
}

/// The mixed pairs on the published public-mint build (KCC20PublicMint, 3 / 3, tradable since 41b7fdb; audit AUD-PM), gated
/// like the 3 / 3 ones ([`slot_excess`]).
pub fn mixed_pairs_pm() -> [(TemplateId, TemplateId); 2] {
    use TemplateId::{Kcc20PublicMint, KronToken2433};
    [(Kcc20PublicMint, KronToken2433), (KronToken2433, Kcc20PublicMint)]
}

/// The mixed pairs on KaspaRocket's token program (KaspaRocketToken11458: the public-mint actor at 10 / 10 slots, tradable on
/// mainnet since its review of 2026-10-27). Its slots exceed the eight token inputs a KOB order transaction carries, so no scenario
/// is skipped on it: the order's own bound refuses a ninth input (`kob_ifd_pair_tests` NIS8 / NIS9).
pub fn mixed_pairs_kr() -> [(TemplateId, TemplateId); 2] {
    use TemplateId::{KaspaRocketToken11458, KronToken2433};
    [(KaspaRocketToken11458, KronToken2433), (KronToken2433, KaspaRocketToken11458)]
}

/// Opt-in of a test binary (the KobCondPair / KobIfdPair suites, which run their KCC-20 mixes on the 3 / 3 program too)
/// to skip the scenarios that exceed a 3 / 3 program's slots ([`slot_excess`]). Off by default: the other binaries of
/// this harness run every scenario as written.
pub static SLOT_GATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Some token of the transaction has more inputs or outputs than its program's slots, for a program with fewer slots than
/// its family's MAX_TOK_IN (the 3 / 3 KCC20Ref; an 8 / 8 program is never gated, so the attacks beyond the scan bound
/// stay). Such a transaction does not exist on that program (its token inputs refuse it whatever the orders do).
pub fn slot_excess(programs: &BTreeMap<[u8; 32], TemplateId>, tx: &Transaction, en: &[UtxoEntry]) -> Option<String> {
    for (cov, p) in programs {
        let tt = token_template(*p);
        if tt.slots.0 >= p.family().max_tok_in() {
            continue;
        }
        let ins = en.iter().filter(|e| cov_of(e) == Some(*cov)).count();
        let outs = tx.outputs.iter().filter(|o| o.covenant.map(|c| c.covenant_id.as_bytes()) == Some(*cov)).count();
        if ins > tt.slots.0 || outs > tt.slots.1 {
            return Some(format!("{} in {ins}/{} out {outs}/{}", p.name(), tt.slots.0, tt.slots.1));
        }
    }
    None
}

/// [`built`], or None when the builder refuses the shape for a program's token slots (a shape the 3 / 3 program cannot
/// carry; the caller skips it there).
pub fn try_built(a: Action) -> Option<BuiltTx> {
    match build_with(&a, &budgets) {
        Ok(b) => Some(b),
        Err(e) if e.to_string().contains("token inputs /") => None,
        Err(e) => panic!("honest build: {e}"),
    }
}

impl Run<'_> {
    /// Executes a finished transaction, or (with [`SLOT_GATE`] on) prints `SKIP-SLOTS <name> <pair> (...)` and returns
    /// None when it exceeds the token slots of a 3 / 3 program ([`slot_excess`]).
    pub fn run_fitting(&self, e: &Ed, tx: &Transaction, en: &[UtxoEntry]) -> Option<Vec<Result<u64, String>>> {
        if SLOT_GATE.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(why) = slot_excess(&e.programs, tx, en) {
                println!("SKIP-SLOTS {} {} ({why})", e.name, self.pair);
                return None;
            }
        }
        Some(kob_protocol::verify::execute(tx, en, false).unwrap_or_else(|x| panic!("{} {}: tx-level failure {x}", e.name, self.pair)))
    }
}

// ---------------------------------------------------------------- bid delivery modes of the pair orders (appended)
//
// docs/design/bid-delivery-spec.md, sections 1 to 6, for the T delivery of `KobPair` and `KobCondPair`: mode 0 new,
// mode 1 accumulate in the order's bought-custody (exactly `bought`, owned by the order's id, scheme 0x04, borrowing
// disabled, zero guard, the order's tExt), mode 2 top-up (form 0 plain, 1 a new receive UTXO, 2 the maker's receive UTXO
// grows and the order leaves topupTip). The scenarios edit honest mode-0 builds: [`ModeCase`] names the order and its T,
// the shapes below turn a build into the mode under test, [`mode_battery`] runs the positives and the attacks.

/// recvThreshold of the mode-2 scenarios (a receive UTXO of this threshold accepts any top-up above one unit).
pub const M_THR: i64 = 1;
/// topupTip of the mode-2 scenarios (0.02 KAS, below every fixture's deliveryCarrier).
pub const M_TIP: i64 = 2_000_000;
/// KAS on the bought-custody or receive UTXO a scenario spends.
pub const M_KAS: u64 = 3 * KAS;
/// The units a bought-custody holds before the fill under test.
pub const M_BOUGHT: i64 = 3_001;
/// The units of the maker's receive UTXO before a top-up.
pub const M_HAVE: i64 = 5_003;

/// The borrow guard of a receive UTXO: the threshold as 8 bytes little-endian, then 24 zero bytes.
pub fn thr_guard(t: i64) -> [u8; 32] {
    let mut g = [0u8; 32];
    g[..8].copy_from_slice(&t.to_le_bytes());
    g
}

/// The pair order under test and its bought token T.
#[derive(Clone, Copy)]
pub struct ModeCase {
    /// `KobPair` or `KobCondPair`.
    pub kind: TemplateId,
    /// The order's covenant id.
    pub oid: [u8; 32],
    /// T: covenant id and program.
    pub tok: [u8; 32],
    pub prog: TemplateId,
    /// The order's deliveryCarrier.
    pub dc: i64,
    /// Position of the `form` argument of settle.
    pub form_at: usize,
}

impl ModeCase {
    pub fn ext(&self) -> [u8; 32] {
        crate::fx::ext_for(self.prog)
    }
    /// The bought-custody state of `amount` (owned by the order).
    pub fn cust(&self, amount: i64) -> TokenState {
        TokenState::custody(self.prog.family(), amount, self.oid, self.ext())
    }
    /// A plain T state of `amount` for `key`.
    pub fn plain(&self, amount: i64, key: u8) -> TokenState {
        TokenState::user(self.prog.family(), amount, pk(key), self.ext())
    }
    /// A receive UTXO state of `amount` for `key` with threshold `thr`.
    pub fn recv(&self, amount: i64, key: u8, thr: i64) -> TokenState {
        TokenState::Kcc20(Kcc20State {
            amount,
            owner: pk(key),
            owner_scheme: 0,
            borrow_scheme: 1,
            borrow_guard: thr_guard(thr),
            extension_commitment: self.ext(),
        })
    }
    pub fn order(&self, e: &Ed) -> usize {
        e.input_of_cov(self.oid)
    }
    /// The T input settle names (argument 2).
    pub fn t_in(&self, e: &Ed, i: usize) -> usize {
        e.arg_int(i, 2) as usize
    }
    pub fn set_t_in(&self, e: &mut Ed, i: usize, j: usize) {
        e.set_arg(i, 2, Arg::Int(j as i64));
    }
    /// Sets settle's `form` argument (appending the trailing arguments the build left out).
    pub fn set_form(&self, e: &mut Ed, i: usize, form: i64) {
        let mut a = e.args(i);
        while a.len() <= self.form_at {
            a.push(Arg::Int(0));
        }
        a[self.form_at] = Arg::Int(form);
        e.set_entry(i, "settle", a);
    }
    /// Sets the order's delivery block at input i and its continuation's, if any (bought `cont_bought`).
    pub fn set_mode(&self, e: &mut Ed, i: usize, mode: i64, thr: i64, tip: i64, bought: i64, cont_bought: i64) {
        e.dlv_in.insert(i, dlv(mode, thr, tip, bought));
        if let Some(k) = e.cont_out(self.oid) {
            e.dlv_out.insert(k, dlv(mode, thr, tip, cont_bought));
        }
    }
    /// The matcher's T output of the transaction (the counterparty's change).
    pub fn matcher_t(&self, e: &Ed) -> usize {
        e.tok_out_of(self.tok, pk(MATCHER))
    }
    /// Appends the order's bought-custody of `b` units (KAS `M_KAS`) as an input of T and names it as settle's T input.
    pub fn add_bought(&self, e: &mut Ed, i: usize, b: i64) -> usize {
        e.programs.entry(self.tok).or_insert(self.prog);
        let j = e.add_token_input(utxo(0xb7, M_KAS, 1_000, Some(self.tok)), self.cust(b), Witness::CovenantId);
        self.set_t_in(e, i, j);
        j
    }
    /// The delivered amount of an honest mode-0 fill (output i).
    pub fn t_out(&self, e: &Ed, i: usize) -> i64 {
        e.out_state(i).amount()
    }
}

impl Ed {
    /// Swaps inputs a and b (with their plans and the per-input maps). A KCC-20 leader plan off the token's first input
    /// becomes a delegator (assembly makes the token's first input its leader). Entry arguments naming a or b are the
    /// caller's to fix.
    pub fn swap_inputs(&mut self, a: usize, b: usize) {
        self.tx.inputs.swap(a, b);
        self.entries.swap(a, b);
        self.plans.swap(a, b);
        for k in [a, b] {
            if let SigPlan::TokenLeader { template, state, witness, .. } = &self.plans[k] {
                self.plans[k] = SigPlan::TokenDelegator { template: *template, state: state.clone(), witness: witness.clone() };
            }
        }
        fn swap_key<V>(m: &mut BTreeMap<usize, V>, a: usize, b: usize) {
            let (x, y) = (m.remove(&a), m.remove(&b));
            if let Some(v) = x {
                m.insert(b, v);
            }
            if let Some(v) = y {
                m.insert(a, v);
            }
        }
        swap_key(&mut self.signers, a, b);
        swap_key(&mut self.sighash_types, a, b);
        swap_key(&mut self.raw_sigscripts, a, b);
        swap_key(&mut self.dlv_in, a, b);
        let bl: BTreeSet<usize> = self.borrow_leaders.iter().map(|&k| if k == a { b } else if k == b { a } else { k }).collect();
        self.borrow_leaders = bl;
        for o in self.tx.outputs.iter_mut() {
            if let Some(c) = o.covenant.as_mut() {
                if c.authorizing_input as usize == a {
                    c.authorizing_input = b as u16;
                } else if c.authorizing_input as usize == b {
                    c.authorizing_input = a as u16;
                }
            }
        }
    }
}

/// Mode 1, first fill (bought 0) of an honest partial fill: output i becomes the bought-custody of the delivered amount
/// and the continuation carries bought = that amount. Same KAS as mode 0 (the order puts its carrier on the custody).
pub fn m1_first(c: &ModeCase, mut e: Ed) -> Ed {
    let i = c.order(&e);
    let t = c.t_out(&e, i);
    e.set_out_state(i, c.cust(t));
    c.set_mode(&mut e, i, 1, 0, 0, 0, t);
    e
}

/// Mode 1, later fill of an honest partial fill: the bought-custody of `b` is the named T input, output i the custody of
/// `b + n` carrying the custody's KAS, the continuation bought `b + n` and the order's KAS less the tip only.
pub fn m1_later(c: &ModeCase, mut e: Ed, b: i64) -> Ed {
    let i = c.order(&e);
    let t = c.t_out(&e, i);
    c.add_bought(&mut e, i, b);
    e.set_out_state(i, c.cust(b + t));
    let k = e.cont_out(c.oid).expect("a partial fill continues");
    let (dv, kv) = (e.value(i) as i64, e.value(k) as i64);
    e.set_value(i, (M_KAS as i64 + dv - c.dc) as u64);
    e.set_value(k, (kv + c.dc) as u64);
    c.set_mode(&mut e, i, 1, 0, 0, b, b + t);
    e
}

/// Mode 1, the fill that ends the order: the bought-custody of `b` is the named T input and output i the maker's plain
/// UTXO of `b + n`, with the custody's KAS on top.
pub fn m1_end(c: &ModeCase, mut e: Ed, b: i64) -> Ed {
    let i = c.order(&e);
    assert!(e.cont_out(c.oid).is_none(), "{}: an ending fill", e.name);
    let t = c.t_out(&e, i);
    c.add_bought(&mut e, i, b);
    e.set_out_state(i, c.plain(b + t, MAKER_A));
    let dv = e.value(i);
    e.set_value(i, dv + M_KAS);
    c.set_mode(&mut e, i, 1, 0, 0, b, 0);
    e
}

/// Mode 1, the refund of an order holding a bought-custody of `b`: the custody is the only input of T and returns whole,
/// with its KAS, to the maker's key at its own input index.
pub fn m1_refund(c: &ModeCase, mut e: Ed, b: i64) -> (Ed, usize) {
    let i = c.order(&e);
    e.align();
    let j = c.add_bought(&mut e, i, b);
    let k = e.add_token_output(c.tok, c.plain(b, MAKER_A), M_KAS);
    assert_eq!(j, k, "{}: the release sits at the custody's index", e.name);
    c.set_mode(&mut e, i, 1, 0, 0, b, 0);
    (e, j)
}

/// Mode 2, form 1 of an honest fill: output i becomes a new receive UTXO of the delivered amount, threshold M_THR.
pub fn m2_form1(c: &ModeCase, mut e: Ed) -> Ed {
    let i = c.order(&e);
    let t = c.t_out(&e, i);
    e.set_out_state(i, c.recv(t, MAKER_A, M_THR));
    c.set_form(&mut e, i, 1);
    c.set_mode(&mut e, i, 2, M_THR, M_TIP, 0, 0);
    e
}

/// Mode 2, form 2 of an honest partial fill: the maker's receive UTXO of `M_HAVE` units (threshold `thr`, KAS `M_KAS`)
/// becomes T's first input (the leader, borrow path) and the named T input; output i is that UTXO grown by n with its KAS;
/// the order leaves `M_TIP` (to the matcher's change) instead of a carrier. Returns (ed, the receive UTXO's input).
pub fn m2_topup(c: &ModeCase, mut e: Ed, thr: i64) -> (Ed, usize) {
    let i = c.order(&e);
    let t = c.t_out(&e, i);
    let first = e.first_token_input(c.tok);
    let j = e.add_token_input(utxo(0xb8, M_KAS, 1_000, Some(c.tok)), c.recv(M_HAVE, MAKER_A, thr), Witness::P2pk(pk(MAKER_A)));
    e.swap_inputs(first, j);
    c.set_t_in(&mut e, i, first);
    e.borrow_leaders.insert(first);
    e.set_out_state(i, c.recv(M_HAVE + t, MAKER_A, thr));
    let dv = e.value(i) as i64;
    if let Some(k) = e.cont_out(c.oid) {
        // no carrier leaves the order: output i carries the receive UTXO's KAS
        e.set_value(i, (M_KAS as i64 + dv - c.dc) as u64);
        let kv = e.value(k) as i64;
        e.set_value(k, (kv + c.dc - M_TIP) as u64);
    } else {
        // an ending top-up: output i keeps the order's KAS and the receive UTXO's, less topupTip
        e.set_value(i, (dv + M_KAS as i64 - M_TIP) as u64);
    }
    let ch = e.change_out();
    let cv = e.value(ch);
    e.set_value(ch, cv + M_TIP as u64);
    c.set_form(&mut e, i, 2);
    c.set_mode(&mut e, i, 2, M_THR, M_TIP, 0, 0);
    (e, first)
}

/// The honest builds a [`mode_battery`] edits: a partial fill and the fill that ends the order (both of a KCC-20 T at
/// the order's input, with the matcher's own T as the counterparty) and a refund.
pub struct ModeShapes<'a> {
    pub part: &'a dyn Fn(&str) -> Ed,
    pub end: &'a dyn Fn(&str) -> Ed,
    pub refund: &'a dyn Fn(&str) -> Ed,
}

/// Every delivery-mode rule of a pair order with a KCC-20 T: the positives of each mode and form, then the attacks, each
/// refused by the order's own input (names `<pfx>M..`; `PM` positives, `NM` attacks).
pub fn mode_battery(r: &Run, c: &ModeCase, sh: &ModeShapes, pfx: &str) {
    let id = |s: &str| format!("{pfx}{s}");
    let ok = |e: &Ed| r.ok(e);
    let bad = |e: &Ed| r.bad(e, c.order(e));
    let b = M_BOUGHT;

    // ---- positives
    ok(&(sh.part)(&id("PM00 mode 0 (honest)")));
    ok(&m1_first(c, (sh.part)(&id("PM01 mode 1 first fill: output i is the bought-custody of n"))));
    let later = m1_later(c, (sh.part)(&id("PM02 mode 1 later fill: the custody grows to bought + n, no carrier")), b);
    ok(&later);
    let end = m1_end(c, (sh.end)(&id("PM03 mode 1 ending fill: bought + n to the maker's key")), b);
    ok(&end);
    let (refund, rj) = m1_refund(c, (sh.refund)(&id("PM04 mode 1 refund: the custody returns whole at its index")), b);
    ok(&refund);
    let mut e = (sh.part)(&id("PM05 mode 2 form 0: a new plain UTXO"));
    let i = c.order(&e);
    c.set_mode(&mut e, i, 2, M_THR, M_TIP, 0, 0);
    ok(&e);
    let form1 = m2_form1(c, (sh.part)(&id("PM06 mode 2 form 1: a new receive UTXO")));
    ok(&form1);
    let (topup, _) = m2_topup(c, (sh.part)(&id("PM07 mode 2 form 2: top-up of the maker's receive UTXO")), M_THR);
    ok(&topup);
    let (topend, _) = m2_topup(c, (sh.end)(&id("PM08 mode 2 form 2 ending fill")), M_THR);
    ok(&topend);
    let mut e = m1_later(c, (sh.part)(&id("PM09 mode 1 later fill, the custody before the T counterparty")), b);
    let (j, first) = (c.t_in(&e, c.order(&e)), e.first_token_input(c.tok));
    e.swap_inputs(first, j);
    let i = c.order(&e);
    c.set_t_in(&mut e, i, first);
    ok(&e);

    // ---- mode 1, later fill: the bought-custody is known by its exact state
    let i = c.order(&later);
    let j = c.t_in(&later, i);
    let mt = c.matcher_t(&later);
    let mut e = later.clone().named(&id("NM01 look-alike custody: one unit more than bought"));
    e.set_in_tok(j, c.cust(b + 1));
    e.add_out_amount(mt, 1);
    bad(&e);
    let mut e = later.clone().named(&id("NM02 look-alike custody: one unit less than bought"));
    e.set_in_tok(j, c.cust(b - 1));
    e.add_out_amount(mt, -1);
    bad(&e);
    let mut e = later.clone().named(&id("NM03 look-alike custody: borrow-enabled (a receive state owned by the order)"));
    e.set_in_tok(j, map_tok(c.cust(b), |k| Kcc20State { borrow_scheme: 1, borrow_guard: thr_guard(1), ..k }, |k| k));
    bad(&e);
    let mut e = later.clone().named(&id("NM04 look-alike custody: a non-zero guard"));
    e.set_in_tok(j, map_tok(c.cust(b), |k| Kcc20State { borrow_guard: thr_guard(1), ..k }, |k| k));
    bad(&e);
    let mut e = later.clone().named(&id("NM05 look-alike custody: another extension commitment"));
    e.set_in_tok(j, map_tok(c.cust(b), |x| Kcc20State { extension_commitment: [0xdd; 32], ..x }, |x| x));
    bad(&e);
    let mut e = later.clone().named(&id("NM06 look-alike custody: owned by another covenant id"));
    e.set_in_tok(j, TokenState::custody(c.prog.family(), b, [0x99; 32], c.ext()));
    bad(&e);
    let mut e = later.clone().named(&id("NM07 look-alike custody: a key-owned UTXO of the maker"));
    e.set_in_tok(j, c.plain(b, MAKER_A));
    e.set_input_value(j, M_KAS);
    if let SigPlan::TokenDelegator { witness, .. } = &mut e.plans[j] {
        *witness = Witness::P2pk(pk(MAKER_A));
    }
    bad(&e);
    // the custody is a non-P2SH UTXO of T's covenant id pushing the genuine custody redeem bytes
    let rs = c.cust(b).redeem_with(token_template(c.prog));
    let e = later.clone().named(&id("NM08 planted custody (not P2SH, genuine redeem bytes)"));
    r.bad_patched(&e, i, &|tx, en| plant_input(tx, en, j, c.tok, &rs, false));
    let mut e = later.clone().named(&id("NM09 the custody spent but not named (the T counterparty named)"));
    let first = e.first_token_input(c.tok);
    let other = if first == j { (0..e.plans.len()).find(|&k| k != j && cov_of(&e.entries[k]) == Some(c.tok)).unwrap() } else { first };
    c.set_t_in(&mut e, i, other);
    bad(&e);
    let mut e = later.clone().named(&id("NM10 a second UTXO of the order's id next to the custody (exactly bought)"));
    e.add_stray(c.tok, c.prog, c.oid, b, 0xb9);
    bad(&e);
    let mut e = later.clone().named(&id("NM11 cross-custody: the sell custody named as the T input"));
    let ci = e.arg_int(i, 1) as usize;
    c.set_t_in(&mut e, i, ci);
    bad(&e);
    let mut e = later.clone().named(&id("NM12 cross-custody: the bought-custody named as the sell custody"));
    e.set_arg(i, 1, Arg::Int(j as i64));
    bad(&e);

    // ---- mode 1, later fill: what it must deliver and keep
    let mut e = later.clone().named(&id("NM13 under-delivery: the custody grows by n - 1"));
    e.add_out_amount(i, -1);
    e.add_out_amount(mt, 1);
    bad(&e);
    let mut e = later.clone().named(&id("NM14 stale bought: the continuation keeps bought"));
    let k = e.cont_out(c.oid).unwrap();
    e.dlv_out.insert(k, dlv(1, 0, 0, b));
    bad(&e);
    let mut e = later.clone().named(&id("NM15 inflated bought: the continuation records one unit more"));
    e.dlv_out.insert(k, dlv(1, 0, 0, c.t_out(&later, i) + 1));
    bad(&e);
    let mut e = later.clone().named(&id("NM16 the continuing order pays bought + n out to the maker's key"));
    e.set_out_state(i, c.plain(c.t_out(&later, i), MAKER_A));
    bad(&e);
    let mut e = later.clone().named(&id("NM17 the custody's KAS skimmed off output i"));
    e.fund_out(i, -1);
    bad(&e);
    let mut e = later.clone().named(&id("NM18 the continuation releases a carrier although the custody exists"));
    e.fund_out(k, -c.dc);
    bad(&e);
    let mut e = later.clone().named(&id("NM19 the custody left behind: a second custody of n started, the first not spent"));
    e.dlv_out.insert(k, dlv(1, 0, 0, c.t_out(&later, i)));
    let jj = c.t_in(&e, i);
    e.set_in_tok(jj, c.plain(b, MATCHER));
    if let SigPlan::TokenDelegator { witness, .. } = &mut e.plans[jj] {
        *witness = Witness::P2pk(pk(MATCHER));
    }
    bad(&e);

    // ---- mode 1, first fill
    let first = m1_first(c, (sh.part)(&id("first")));
    let i = c.order(&first);
    let k = first.cont_out(c.oid).unwrap();
    let mut e = first.clone().named(&id("NM20 first fill: the continuing order delivers to the maker's key"));
    e.set_out_state(i, c.plain(c.t_out(&first, i), MAKER_A));
    bad(&e);
    let mut e = first.clone().named(&id("NM21 first fill: the custody is created but bought stays 0"));
    e.dlv_out.insert(k, dlv(1, 0, 0, 0));
    bad(&e);
    let mut e = first.clone().named(&id("NM22 first fill: the custody owned by another covenant id"));
    e.set_out_state(i, TokenState::custody(c.prog.family(), c.t_out(&first, i), [0x99; 32], c.ext()));
    bad(&e);
    let mut e = first.clone().named(&id("NM23 first fill: a borrow-enabled custody"));
    let t = c.t_out(&first, i);
    e.set_out_state(i, map_tok(c.cust(t), |x| Kcc20State { borrow_scheme: 1, borrow_guard: thr_guard(1), ..x }, |x| x));
    bad(&e);
    let mut e = first.clone().named(&id("NM24 first fill: the carrier skimmed off the new custody"));
    e.fund_out(i, -1);
    bad(&e);

    // ---- mode 1, the fill that ends the order: no bought-custody is left behind
    let i = c.order(&end);
    let j = c.t_in(&end, i);
    let t = c.t_out(&end, i) - b; // the delivered n (output i holds bought + n)
    let mt = c.matcher_t(&end);
    let mut e = end.clone().named(&id("NM30 ending fill: only n to the maker, the custody's units to the matcher"));
    e.set_out_state(i, c.plain(t, MAKER_A));
    e.add_out_amount(mt, b);
    bad(&e);
    let mut e = end.clone().named(&id("NM31 ending fill: the custody stays owned by the ended order"));
    e.set_out_state(i, c.cust(b + t));
    bad(&e);
    let mut e = end.clone().named(&id("NM32 ending fill: the custody's KAS skimmed off output i"));
    e.fund_out(i, -1);
    bad(&e);
    let mut e = end.clone().named(&id("NM33 ending fill: bought + n to the matcher"));
    e.set_out_state(i, c.plain(b + t, MATCHER));
    bad(&e);
    let mut e = end.clone().named(&id("NM34 ending fill: the custody spent but not named"));
    let other = (0..e.plans.len()).find(|&k| k != j && k != i && cov_of(&e.entries[k]) == Some(c.tok)).unwrap();
    c.set_t_in(&mut e, i, other);
    bad(&e);

    // ---- mode 1, refund: no bought-custody is left behind
    let _ = c.order(&refund);
    let mut e = (sh.refund)(&id("NM40 refund of an order holding a custody, the custody not spent"));
    let ii = c.order(&e);
    c.set_mode(&mut e, ii, 1, 0, 0, b, 0);
    bad(&e);
    let mut e = refund.clone().named(&id("NM41 refund: the custody released to the matcher"));
    e.set_out_state(rj, c.plain(b, MATCHER));
    bad(&e);
    let mut e = refund.clone().named(&id("NM42 refund: the release short of the custody's KAS"));
    let v = e.value(rj);
    e.set_value(rj, v - 1);
    bad(&e);
    let mut e = refund.clone().named(&id("NM43 refund: the custody stays owned by the ended order"));
    e.set_out_state(rj, c.cust(b));
    bad(&e);
    let mut e = refund.clone().named(&id("NM44 refund: a second input of T owned by the order"));
    e.add_stray(c.tok, c.prog, c.oid, 7, 0xba);
    bad(&e);
    let mut e = refund.clone().named(&id("NM45 refund: a look-alike custody of one unit less"));
    e.set_in_tok(rj, c.cust(b - 1));
    e.set_out_state(rj, c.plain(b - 1, MAKER_A));
    bad(&e);
    let mut e = refund.clone().named(&id("NM46 refund: the release at another index"));
    let p = e.add_plain_output(KAS, MATCHER);
    e.swap_outputs(rj, p);
    bad(&e);

    // ---- the mode is the maker's: a filler cannot move an order between modes
    let mut e = (sh.part)(&id("NM50 mode 3"));
    let i = c.order(&e);
    c.set_mode(&mut e, i, 3, 0, 0, 0, 0);
    bad(&e);
    let mut e = (sh.part)(&id("NM51 mode -1"));
    c.set_mode(&mut e, i, -1, 0, 0, 0, 0);
    bad(&e);
    let mut e = (sh.part)(&id("NM52 mode 0 with bought > 0"));
    c.set_mode(&mut e, i, 0, 0, 0, b, b);
    bad(&e);
    let mut e = (sh.part)(&id("NM53 mode 2 with bought > 0"));
    c.set_mode(&mut e, i, 2, M_THR, M_TIP, b, b);
    bad(&e);
    let mut e = m1_first(c, (sh.part)(&id("NM54 a mode-0 order delivering into a custody")));
    c.set_mode(&mut e, i, 0, 0, 0, 0, c.t_out(&first, i));
    bad(&e);
    let mut e = m2_form1(c, (sh.part)(&id("NM55 a mode-0 order delivering a receive UTXO (form 1)")));
    c.set_mode(&mut e, i, 0, 0, 0, 0, 0);
    bad(&e);
    let mut e = m2_form1(c, (sh.part)(&id("NM56 a mode-1 order delivering a receive UTXO (form 1)")));
    c.set_mode(&mut e, i, 1, 0, 0, 0, c.t_out(&first, i));
    bad(&e);
    let (mut e, _) = m2_topup(c, (sh.part)(&id("NM57 a mode-0 order topped up (form 2)")), M_THR);
    c.set_mode(&mut e, i, 0, 0, 0, 0, 0);
    bad(&e);
    let (mut e, _) = m2_topup(c, (sh.part)(&id("NM58 a mode-1 order topped up (form 2)")), M_THR);
    c.set_mode(&mut e, i, 1, 0, 0, 0, 0);
    bad(&e);
    let mut e = m1_first(c, (sh.part)(&id("NM59 a mode-2 order delivering into a custody")));
    c.set_mode(&mut e, i, 2, M_THR, M_TIP, 0, c.t_out(&first, i));
    bad(&e);

    // ---- mode 2, form 1
    let i = c.order(&form1);
    let t = c.t_out(&form1, i);
    let mut e = form1.clone().named(&id("NM60 form 1: the receive UTXO's threshold above recvThreshold"));
    e.set_out_state(i, c.recv(t, MAKER_A, M_THR + 1));
    bad(&e);
    let mut e = form1.clone().named(&id("NM61 form 1: a receive UTXO of the matcher"));
    e.set_out_state(i, c.recv(t, MATCHER, M_THR));
    bad(&e);
    let mut e = form1.clone().named(&id("NM62 form 1 with recvThreshold 0"));
    e.set_out_state(i, c.recv(t, MAKER_A, 0));
    c.set_mode(&mut e, i, 2, 0, M_TIP, 0, 0);
    bad(&e);
    let mut e = form1.clone().named(&id("NM63 form 3"));
    c.set_form(&mut e, i, 3);
    bad(&e);
    let mut e = form1.clone().named(&id("NM64 form 1 with topupTip above deliveryCarrier"));
    c.set_mode(&mut e, i, 2, M_THR, c.dc + 1, 0, 0);
    bad(&e);
    let mut e = form1.clone().named(&id("NM65 form 1 with a negative topupTip"));
    c.set_mode(&mut e, i, 2, M_THR, -1, 0, 0);
    bad(&e);
    let mut e = form1.clone().named(&id("NM66 form 1: a plain UTXO delivered"));
    e.set_out_state(i, c.plain(t, MAKER_A));
    bad(&e);

    // ---- mode 2, form 2 (top-up)
    let i = c.order(&topup);
    let li = c.t_in(&topup, i);
    let t = c.t_out(&topup, i) - M_HAVE;
    let k = topup.cont_out(c.oid).unwrap();
    let mut e = topup.clone().named(&id("NM70 top-up of a receive UTXO of another key"));
    e.set_in_tok(li, c.recv(M_HAVE, MATCHER, M_THR));
    e.set_out_state(i, c.recv(M_HAVE + t, MATCHER, M_THR));
    bad(&e);
    let (mut e, _) = m2_topup(c, (sh.part)(&id("NM71 top-up of a receive UTXO below recvThreshold")), 4);
    c.set_mode(&mut e, i, 2, 5, M_TIP, 0, 0);
    bad(&e);
    let mut e = topup.clone().named(&id("NM72 top-up of a receive UTXO with a non-canonical guard"));
    let mut g = thr_guard(M_THR);
    g[8] = 1;
    let gk = |k: Kcc20State| Kcc20State { borrow_guard: g, ..k };
    e.set_in_tok(li, map_tok(c.recv(M_HAVE, MAKER_A, M_THR), gk, |k| k));
    e.set_out_state(i, map_tok(c.recv(M_HAVE + t, MAKER_A, M_THR), gk, |k| k));
    bad(&e);
    let mut e = topup.clone().named(&id("NM73 top-up of a receive UTXO of another extension commitment"));
    let xk = |k: Kcc20State| Kcc20State { extension_commitment: [0xdd; 32], ..k };
    for q in 0..e.plans.len() {
        if cov_of(&e.entries[q]) == Some(c.tok) && !matches!(e.plans[q], SigPlan::Entry { .. } | SigPlan::P2pk { .. }) {
            let st = e.in_tok(q);
            e.set_in_tok(q, map_tok(st, xk, |k| k));
        }
    }
    for q in e.tok_outs_of(c.tok) {
        let st = e.out_state(q);
        e.set_out_state(q, map_tok(st, xk, |k| k));
    }
    bad(&e);
    let mut e = topup.clone().named(&id("NM74 top-up takes more than topupTip"));
    e.fund_out(k, -1);
    bad(&e);
    let mut e = topup.clone().named(&id("NM75 top-up: output i short of the receive UTXO's KAS"));
    e.fund_out(i, -1);
    bad(&e);
    let mut e = topup.clone().named(&id("NM76 top-up under-delivers: the receive UTXO grows by n - 1"));
    e.add_out_amount(i, -1);
    let mt = c.matcher_t(&e);
    e.add_out_amount(mt, 1);
    bad(&e);
    let mut e = topup.clone().named(&id("NM77 top-up with recvThreshold 0"));
    c.set_mode(&mut e, i, 2, 0, M_TIP, 0, 0);
    bad(&e);
    let mut e = topup.clone().named(&id("NM78 top-up named but form 1 claimed (the receive UTXO's units kept)"));
    c.set_form(&mut e, i, 1);
    bad(&e);
    let rs = c.recv(M_HAVE, MAKER_A, M_THR).redeem_with(token_template(c.prog));
    let e = topup.clone().named(&id("NM79 top-up of a planted receive UTXO (not P2SH, genuine redeem bytes)"));
    r.bad_patched(&e, i, &|tx, en| plant_input(tx, en, li, c.tok, &rs, false));
}

/// A KRON T is delivered new: modes 1 and 2 refuse every fill (names `<pfx>NM9.`).
pub fn kron_t_battery(r: &Run, c: &ModeCase, part: &dyn Fn(&str) -> Ed, pfx: &str) {
    for (n, (mode, form, what)) in [(1, 0, "mode 1"), (2, 0, "mode 2 form 0"), (2, 1, "mode 2 form 1")].into_iter().enumerate() {
        let mut e = part(&format!("{pfx}NM9{n} a KRON T in {what}"));
        let i = c.order(&e);
        c.set_form(&mut e, i, form);
        c.set_mode(&mut e, i, mode, M_THR, M_TIP, 0, 0);
        r.bad(&e, i);
    }
}
