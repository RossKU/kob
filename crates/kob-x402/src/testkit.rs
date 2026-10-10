//! Test support: an in-memory chain that runs submitted transactions through the script engine.
//!
//! [`MockChain`] mimics what the node gives the facilitator: an address-indexed UTXO set, a mempool,
//! acceptance into "chain state" (`mine`), and consensus-style validation on `submit` (scripts with
//! enforced compute budgets, exact storage-mass commitment, fee floor: `kob_protocol::verify`). It is
//! deterministic and needs no network.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};

use crate::chain::{ChainError, ChainUtxo, ChainView, Outpoint, OutputStatus, SubmitError, Txid};

/// Fixture secret key `[n; 32]` (n in 1..=255).
pub fn secret(n: u8) -> [u8; 32] {
    [n; 32]
}

/// x-only public key of fixture key `n`.
pub fn pubkey(n: u8) -> [u8; 32] {
    kob_protocol::tx::pubkey_of(&secret(n)).expect("valid fixture key")
}

/// P2PK (Schnorr) script public key of an x-only key.
pub fn p2pk_spk(pubkey: &[u8; 32]) -> ScriptPublicKey {
    kob_protocol::script::p2pk_spk(pubkey)
}

/// Tooling of the SIGHASH_ALL conformance tests: finds every payer signature of a payment transaction and re-signs
/// it under another hash type, so that a test can present a verifier with a transaction that the script engine
/// ACCEPTS but that is not signed with SIGHASH_ALL (the rule is `crate::sighash`).
pub mod hashtype {
    use std::collections::BTreeMap;

    use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
    use kob_protocol::tx::{sighash, sighash_typed, sign_digest_typed, verify_signature};

    use crate::common::is_p2pk;
    use crate::safe_tx::SafeTx;
    use crate::sighash::{is_leader, p2pk_hash_type, token_owner_proof};
    use crate::wire::PaymentPayload;

    /// Every hash type the script engine accepts besides SIGHASH_ALL: NONE, SINGLE and the three ANYONECANPAY forms.
    pub const NON_ALL: [u8; 5] = [0x02, 0x04, 0x81, 0x82, 0x84];
    /// Trailing bytes that are no Kaspa hash type at all (the engine refuses them; the verifier must refuse them first).
    pub const BOGUS: [u8; 4] = [0x00, 0x03, 0x05, 0xff];

    /// A payer signature of a transaction: the input, the key that made it and the offset of its 65 bytes inside the
    /// input's signature script.
    #[derive(Clone, Copy, Debug)]
    pub struct Signer {
        pub input: usize,
        pub secret: [u8; 32],
        pub at: usize,
    }

    /// The payer signatures of `tx`: P2PK funding inputs and KCC-20 owner witnesses (leader or delegator) whose 65
    /// bytes verify under one of `keys` (public key -> secret key). Order inputs and covenant-owned custody inputs are
    /// skipped.
    pub fn signers(tx: &Transaction, entries: &[UtxoEntry], keys: &BTreeMap<[u8; 32], [u8; 32]>) -> Vec<Signer> {
        let mut out = vec![];
        for (i, entry) in entries.iter().enumerate() {
            let script = &tx.inputs[i].signature_script;
            let at = if is_p2pk(&entry.script_public_key) && entry.covenant_id.is_none() {
                p2pk_hash_type(script).map(|_| 1)
            } else {
                token_owner_proof(script, is_leader(entries, i)).filter(|p| p.len() >= 65).map(|p| {
                    let start = p.as_ptr() as usize - script.as_ptr() as usize;
                    start + p.len() - 65
                })
            };
            let Some(at) = at else { continue };
            let digest = sighash(tx, entries, i);
            let sig = &script[at..at + 65];
            if let Some((_, sk)) = keys.iter().find(|(pk, _)| verify_signature(i, sig, &digest, pk).is_ok()) {
                out.push(Signer { input: i, secret: *sk, at });
            }
        }
        out
    }

    /// The transaction with the signature of `s` made under `hash_type` (a real signature over the digest of that hash
    /// type, so the engine accepts it when the type is valid).
    pub fn resigned(tx: &Transaction, entries: &[UtxoEntry], s: &Signer, hash_type: u8) -> Transaction {
        let digest = sighash_typed(tx, entries, s.input, hash_type).expect("a Kaspa hash type");
        let sig = sign_digest_typed(&s.secret, &digest, hash_type).expect("sign");
        let mut t = tx.clone();
        t.inputs[s.input].signature_script[s.at..s.at + 65].copy_from_slice(&sig);
        t
    }

    /// The transaction with only the trailing hash type byte of the signature of `s` replaced (the signature itself is
    /// left alone: the engine would refuse it, so the verifier must refuse it before the engine runs).
    pub fn tampered_byte(tx: &Transaction, s: &Signer, hash_type: u8) -> Transaction {
        let mut t = tx.clone();
        t.inputs[s.input].signature_script[s.at + 64] = hash_type;
        t
    }

    /// `p` with its transaction replaced (id and hints recomputed).
    pub fn with_tx(p: &PaymentPayload, tx: &Transaction, entries: &[UtxoEntry]) -> PaymentPayload {
        let mut t = tx.clone();
        t.finalize(); // a mutated transaction caches its old id until finalized
        let mut q = p.clone();
        q.payload.transaction = SafeTx::from_consensus(&t, entries).to_text();
        q
    }
}

#[derive(Default)]
struct State {
    utxos: BTreeMap<Outpoint, ChainUtxo>,
    /// Mempool transactions in arrival order.
    mempool: Vec<(Txid, Transaction)>,
    /// Outpoints spent by mempool transactions.
    mempool_spent: BTreeSet<Outpoint>,
    /// `replace` calls.
    replaces: u64,
    /// Accepted transaction ids.
    accepted: BTreeSet<Txid>,
    daa: u64,
    counter: u64,
    submits: u64,
    /// When set, `submit` fails with this error (simulates a node outage or rejection).
    fail_submit: Option<SubmitError>,
    /// When set, every lookup fails (simulates an unreachable node).
    unavailable: bool,
}

/// An in-memory chain implementing [`ChainView`].
pub struct MockChain {
    state: Mutex<State>,
}

impl Default for MockChain {
    fn default() -> Self {
        Self::new()
    }
}

impl MockChain {
    /// Empty chain at DAA score 1_000.
    pub fn new() -> Self {
        MockChain { state: Mutex::new(State { daa: 1_000, ..State::default() }) }
    }

    /// Adds an unspent output under a synthetic transaction id and returns its outpoint.
    pub fn add_utxo(&self, amount: u64, script_public_key: ScriptPublicKey, covenant_id: Option<[u8; 32]>) -> Outpoint {
        let mut s = self.state.lock().unwrap();
        s.counter += 1;
        let mut txid = [0u8; 32];
        txid[..8].copy_from_slice(&s.counter.to_be_bytes());
        txid[31] = 0xfe;
        let op = Outpoint::new(txid, 0);
        let daa = s.daa;
        s.utxos.insert(op, ChainUtxo { amount, script_public_key, block_daa_score: daa, is_coinbase: false, covenant_id });
        op
    }

    /// Adds a P2PK UTXO of `pubkey`.
    pub fn add_p2pk(&self, pubkey: &[u8; 32], amount: u64) -> Outpoint {
        self.add_utxo(amount, p2pk_spk(pubkey), None)
    }

    /// Adds a UTXO at an explicit outpoint (for fixtures that need a chosen id).
    pub fn insert_utxo(&self, op: Outpoint, u: ChainUtxo) {
        self.state.lock().unwrap().utxos.insert(op, u);
    }

    /// The UTXO at an outpoint, if unspent.
    pub fn utxo(&self, op: &Outpoint) -> Option<ChainUtxo> {
        self.state.lock().unwrap().utxos.get(op).cloned()
    }

    /// True if `op` is in the UTXO set and no mempool transaction spends it (what a node reports as spendable).
    pub fn is_spendable(&self, op: &Outpoint) -> bool {
        let s = self.state.lock().unwrap();
        s.utxos.contains_key(op) && !s.mempool_spent.contains(op)
    }

    /// All unspent outputs locked by `spk`.
    pub fn unspent_of(&self, spk: &ScriptPublicKey) -> Vec<(Outpoint, ChainUtxo)> {
        self.state.lock().unwrap().utxos.iter().filter(|(_, u)| &u.script_public_key == spk).map(|(o, u)| (*o, u.clone())).collect()
    }

    /// Simulates a competing accepted spend of `op`: the output disappears.
    pub fn spend_externally(&self, op: &Outpoint) {
        self.state.lock().unwrap().utxos.remove(op);
    }

    /// Advances the virtual DAA score.
    pub fn advance_daa(&self, n: u64) {
        self.state.lock().unwrap().daa += n;
    }

    /// Current DAA score.
    pub fn daa(&self) -> u64 {
        self.state.lock().unwrap().daa
    }

    /// Accepts every mempool transaction into chain state (spends inputs, creates outputs at the
    /// current DAA score) and then advances the DAA score by `advance`.
    pub fn mine(&self, advance: u64) {
        let mut s = self.state.lock().unwrap();
        let txs = std::mem::take(&mut s.mempool);
        s.mempool_spent.clear();
        s.daa += 1;
        let daa = s.daa;
        for (txid, tx) in txs {
            for i in &tx.inputs {
                s.utxos.remove(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index));
            }
            for (idx, o) in tx.outputs.iter().enumerate() {
                s.utxos.insert(
                    Outpoint::new(txid, idx as u32),
                    ChainUtxo {
                        amount: o.value,
                        script_public_key: o.script_public_key.clone(),
                        block_daa_score: daa,
                        is_coinbase: false,
                        covenant_id: o.covenant.map(|c| c.covenant_id.as_bytes()),
                    },
                );
            }
            s.accepted.insert(txid);
        }
        s.daa += advance;
    }

    /// Drops every mempool transaction (a node restart / eviction).
    pub fn evict_mempool(&self) {
        let mut s = self.state.lock().unwrap();
        s.mempool.clear();
        s.mempool_spent.clear();
    }

    /// Number of `submit` calls seen (including failed ones).
    /// Number of `replace` calls (accepted or not).
    pub fn replace_count(&self) -> u64 {
        self.state.lock().unwrap().replaces
    }

    pub fn submit_count(&self) -> u64 {
        self.state.lock().unwrap().submits
    }

    /// Makes `submit` fail with `e` (or work again with `None`).
    pub fn set_submit_failure(&self, e: Option<SubmitError>) {
        self.state.lock().unwrap().fail_submit = e;
    }

    /// Makes every lookup fail (a node outage).
    pub fn set_unavailable(&self, v: bool) {
        self.state.lock().unwrap().unavailable = v;
    }

    /// True if `txid` was accepted into chain state.
    pub fn is_accepted(&self, txid: &Txid) -> bool {
        self.state.lock().unwrap().accepted.contains(txid)
    }
}

fn check_up(s: &State) -> Result<(), ChainError> {
    if s.unavailable {
        Err(ChainError("mock node unavailable".into()))
    } else {
        Ok(())
    }
}

impl ChainView for MockChain {
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        let s = self.state.lock().unwrap();
        check_up(&s)?;
        Ok(wanted.iter().map(|(op, spk)| s.utxos.get(op).filter(|u| &u.script_public_key == spk).cloned()).collect())
    }

    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        check_up(&self.state.lock().unwrap())?;
        Ok(self.unspent_of(spk))
    }

    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        let s = self.state.lock().unwrap();
        check_up(&s)?;
        Ok(s.daa)
    }

    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        let s = self.state.lock().unwrap();
        check_up(&s)?;
        if let Some(u) = s.utxos.get(out).filter(|u| &u.script_public_key == spk) {
            return Ok(OutputStatus::Accepted { block_daa_score: u.block_daa_score });
        }
        if s.mempool.iter().any(|(t, _)| *t == out.txid) {
            return Ok(OutputStatus::Mempool);
        }
        Ok(OutputStatus::Unknown)
    }

    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError> {
        let s = self.state.lock().unwrap();
        check_up(&s)?;
        Ok(s.mempool.iter().any(|(t, _)| t == txid))
    }

    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let mut s = self.state.lock().unwrap();
        s.submits += 1;
        if let Some(e) = s.fail_submit.clone() {
            return Err(e);
        }
        let txid = tx.id().as_bytes();
        if s.accepted.contains(&txid) || s.mempool.iter().any(|(t, _)| *t == txid) {
            return Err(SubmitError::AlreadyKnown);
        }
        let mut entries = Vec::with_capacity(tx.inputs.len());
        for i in &tx.inputs {
            let op = Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index);
            if s.mempool_spent.contains(&op) {
                return Err(SubmitError::Conflict(format!("input {op} is already spent by a mempool transaction")));
            }
            match s.utxos.get(&op) {
                Some(u) => entries.push(u.to_entry()),
                None => return Err(SubmitError::Conflict(format!("input {op} is not an unspent output"))),
            }
        }
        kob_protocol::verify::validate(tx, &entries).map_err(|e| SubmitError::Rejected(e.to_string()))?;
        for i in &tx.inputs {
            s.mempool_spent.insert(Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index));
        }
        s.mempool.push((txid, tx.clone()));
        Ok(txid)
    }

    /// Replace-by-fee as the node does it: `tx` must spend an input of a mempool transaction and pay a higher fee than
    /// every transaction it replaces; those leave the mempool (their inputs are free again) and `tx` is submitted.
    fn replace(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let fee_of = |s: &State, t: &Transaction| -> Option<u64> {
            let ins: Option<u64> = t
                .inputs
                .iter()
                .map(|i| {
                    s.utxos
                        .get(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index))
                        .map(|u| u.amount)
                })
                .sum();
            Some(ins?.saturating_sub(t.outputs.iter().map(|o| o.value).sum()))
        };
        {
            let mut s = self.state.lock().unwrap();
            s.replaces += 1;
            let ops: BTreeSet<Outpoint> = tx
                .inputs
                .iter()
                .map(|i| Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index))
                .collect();
            let spends = |t: &Transaction| {
                t.inputs
                    .iter()
                    .any(|i| ops.contains(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index)))
            };
            let old: Vec<(Txid, Transaction)> = s.mempool.iter().filter(|(_, t)| spends(t)).cloned().collect();
            if old.is_empty() {
                return Err(SubmitError::Rejected("replacement: no mempool transaction spends its inputs".into()));
            }
            let new_fee =
                fee_of(&s, tx).ok_or_else(|| SubmitError::Conflict("replacement: an input is not an unspent output".into()))?;
            if old.iter().any(|(_, t)| fee_of(&s, t).is_none_or(|f| f >= new_fee)) {
                return Err(SubmitError::Rejected("replacement: the fee is not higher than the replaced transaction's".into()));
            }
            for (id, t) in &old {
                s.mempool.retain(|(x, _)| x != id);
                for i in &t.inputs {
                    s.mempool_spent.remove(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index));
                }
            }
        }
        self.submit(tx)
    }
}
