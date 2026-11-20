//! Creator lock: a cliff time lock on a share of a token's genesis (`docs/spec/token-metadata.md`, *Creator lock*).
//!
//! No contract changes. The KCC-20 programs already authorise a token UTXO whose owner scheme is
//! `0x03` (`p2sh/v1`) when the transaction spends an input whose script public key is the P2SH of
//! that owner; the owner witness is the index of that input. A creator lock is such an owner with
//! the redeem script
//!
//! ```text
//! OP_DATA_8 <until: u64 LE> OP_CHECKLOCKTIMEVERIFY OP_DATA_32 <x-only key> OP_CHECKSIG
//! ```
//!
//! (44 bytes; Kaspa's `OP_CHECKLOCKTIMEVERIFY` pops its operand, so there is no `OP_DROP`).
//! `until` is a unix time in milliseconds, at or above the consensus `LOCK_TIME_THRESHOLD`
//! (5 × 10^11): the node compares it with the past median time, so the lock opens at a calendar
//! time, not a DAA score. An input of the script can only be spent by a transaction whose
//! `lock_time` is at least `until` (consensus accepts it once the past median time passes its lock
//! time), with a non-final sequence on that input and a signature of `key`. So nobody, the key
//! included, can move the locked tokens before `until`.
//!
//! The lock is published in the token's KCC-23 declaration as `"lock": "<script hex>"`
//! ([`crate::kcc23::TokenMetadata::lock`]). A reader takes it only when the script is exactly this
//! template ([`CreatorLock::parse`]); [`verify_genesis_lock`] then finds the genesis outputs that the
//! P2SH hash of the script owns, from the genesis's own named states.
//!
//! The unlock ([`build_unlock`]) spends the token UTXO(s) and a KAS UTXO at the lock's P2SH address
//! (the *lock coin*) in one transaction, and pays the tokens to a key. The lock coin is sent there
//! by the unlocker first ([`build_lock_coin`]): a plain KAS output, any amount that pays the unlock's
//! fee (default [`LOCK_COIN`]); the unlock returns the rest as change.

use kaspa_consensus_core::hashing::covenant_id::covenant_id as covenant_id_of;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionOutput};
use kaspa_consensus_core::Hash;
use serde::{Deserialize, Serialize};

use crate::artifacts::{token_template, TemplateId};
use crate::build::{BudgetFn, TokenRef};
use crate::error::{Error, Result};
use crate::json::to_hex;
use crate::script::push_data;
use crate::state::{Kcc20State, TokenState};
use crate::tx::{check_key, BuiltTx, Draft, FeeOptions, KeyUtxo, SigPlan, TokenUtxo, Utxo, Witness, SEQUENCE_NONFINAL};

/// Consensus `LOCK_TIME_THRESHOLD`: a lock time at or above it is a unix time in milliseconds,
/// below it a DAA score. A creator lock is always a time.
pub const LOCK_TIME_THRESHOLD: u64 = 500_000_000_000;
/// The latest `until` taken (2^53 − 1 ms, year 287396): a JavaScript number holds it exactly.
pub const MAX_UNTIL: u64 = (1 << 53) - 1;
/// Length of the redeem script.
pub const SCRIPT_LEN: usize = 44;
/// Default KAS sent to the lock's address to authorise an unlock: 1 KAS (the unlock's fee is paid
/// from it, the rest returns as change).
pub const LOCK_COIN: u64 = 100_000_000;
/// The budget role of the lock coin's input.
pub const LOCK_ROLE: &str = "creator-lock";
/// Compute budget of the lock coin's input (one Schnorr check and a small script), measured in
/// the script engine by `crates/kob-tests/tests/kcc20/creator_lock_tests.rs`.
pub const LOCK_BUDGET: u16 = 10;

const OP_DATA_8: u8 = 0x08;
const OP_DATA_32: u8 = 0x20;
const OP_CHECKLOCKTIMEVERIFY: u8 = 0xb0;
const OP_CHECKSIG: u8 = 0xac;

/// A creator lock: the tokens it holds open at `until` (unix ms) to a signature of `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatorLock {
    /// Unix time in milliseconds, [`LOCK_TIME_THRESHOLD`]..=[`MAX_UNTIL`].
    #[serde(with = "crate::json::field")]
    pub until: u64,
    /// The x-only public key that signs the unlock.
    #[serde(with = "crate::json::field")]
    pub key: [u8; 32],
}

impl CreatorLock {
    /// A lock until `until` (unix ms) for `key`; refuses a DAA-score lock time, an `until` past
    /// [`MAX_UNTIL`] and a key that is not a curve point.
    pub fn new(until: u64, key: [u8; 32]) -> Result<Self> {
        if !(LOCK_TIME_THRESHOLD..=MAX_UNTIL).contains(&until) {
            return Err(Error::Invalid(format!(
                "creator lock: until must be a unix time in milliseconds from {LOCK_TIME_THRESHOLD} to {MAX_UNTIL} (got {until})"
            )));
        }
        check_key(&key, "creator lock key")?;
        Ok(CreatorLock { until, key })
    }

    /// The redeem script (44 bytes).
    pub fn script(&self) -> Vec<u8> {
        let mut s = Vec::with_capacity(SCRIPT_LEN);
        s.push(OP_DATA_8);
        s.extend_from_slice(&self.until.to_le_bytes());
        s.push(OP_CHECKLOCKTIMEVERIFY);
        s.push(OP_DATA_32);
        s.extend_from_slice(&self.key);
        s.push(OP_CHECKSIG);
        s
    }

    /// The redeem script as lowercase hex (what the declaration's `lock` holds).
    pub fn script_hex(&self) -> String {
        to_hex(&self.script())
    }

    /// The lock of a redeem script that is exactly the template, else `None`: any other script
    /// (another opcode, a shorter push, a DAA-score lock time, an `until` past [`MAX_UNTIL`], a key
    /// off the curve) is not a creator lock.
    pub fn parse(script: &[u8]) -> Option<Self> {
        if script.len() != SCRIPT_LEN
            || script[0] != OP_DATA_8
            || script[9] != OP_CHECKLOCKTIMEVERIFY
            || script[10] != OP_DATA_32
            || script[43] != OP_CHECKSIG
        {
            return None;
        }
        let until = u64::from_le_bytes(script[1..9].try_into().ok()?);
        let key: [u8; 32] = script[11..43].try_into().ok()?;
        CreatorLock::new(until, key).ok()
    }

    /// [`CreatorLock::parse`] of a hex string (lowercase or not).
    pub fn parse_hex(s: &str) -> Option<Self> {
        let b = crate::json::from_hex(s).ok()?;
        CreatorLock::parse(&b)
    }

    /// The `p2sh/v1` owner of the locked token states: `Blake2b(script)`.
    pub fn authority(&self) -> [u8; 32] {
        crate::kcc2::p2sh_authority(&self.script())
    }

    /// The script public key of the lock's address (the P2SH of the script): the lock coin's.
    pub fn spk(&self) -> ScriptPublicKey {
        crate::kcc2::p2sh_envelope(&self.authority())
    }

    /// A locked KCC-20 state of `amount`: owner scheme 0x03, borrow disabled.
    pub fn state(&self, amount: i64, extension_commitment: [u8; 32]) -> Kcc20State {
        Kcc20State {
            amount,
            owner: self.authority(),
            owner_scheme: crate::kcc2::P2SH,
            borrow_scheme: 0,
            borrow_guard: [0; 32],
            extension_commitment,
        }
    }

    /// Whether a KCC-20 state is held by this lock (owner scheme 0x03, the lock's hash, borrow
    /// disabled: a borrow setting would let others change the UTXO).
    pub fn holds(&self, s: &Kcc20State) -> bool {
        s.owner_scheme == crate::kcc2::P2SH && s.owner == self.authority() && s.borrow_scheme == 0 && s.borrow_guard == [0; 32]
    }

    /// The signature script of the lock coin's input: `push(sig65) push(script)`.
    pub fn sig_script(&self, sig65: &[u8]) -> Vec<u8> {
        let mut s = push_data(sig65);
        s.extend(push_data(&self.script()));
        s
    }
}

// ------------------------------------------------------------------------------------------------
// verification from the genesis

/// A creator lock verified against its token's genesis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedLock {
    /// Unix ms.
    #[serde(with = "crate::json::field")]
    pub until: u64,
    #[serde(with = "crate::json::field")]
    pub key: [u8; 32],
    /// The redeem script, as declared.
    #[serde(with = "crate::json::field")]
    pub script: Vec<u8>,
    /// The `p2sh/v1` owner of the locked states (`Blake2b(script)`).
    #[serde(with = "crate::json::field")]
    pub authority: [u8; 32],
    /// Base units the genesis locked.
    #[serde(with = "crate::json::field")]
    pub amount: u64,
    /// The genesis outputs holding them.
    pub outputs: Vec<LockedOutput>,
    /// The token program the genesis states are proven under.
    pub program: TemplateId,
    /// The extension commitment of the locked states.
    #[serde(with = "crate::json::field")]
    pub extension_commitment: [u8; 32],
    /// The token's supply (the sum of its genesis outputs), when every token output's state is
    /// proven.
    #[serde(with = "crate::json::field")]
    pub supply: Option<u64>,
}

/// A genesis output a creator lock holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedOutput {
    pub index: u32,
    #[serde(with = "crate::json::field")]
    pub amount: u64,
}

impl VerifiedLock {
    /// The lock.
    pub fn lock(&self) -> CreatorLock {
        CreatorLock { until: self.until, key: self.key }
    }
    /// The locked token states, with their genesis output indices.
    pub fn states(&self) -> Vec<(u32, Kcc20State)> {
        let l = self.lock();
        self.outputs.iter().map(|o| (o.index, l.state(o.amount as i64, self.extension_commitment))).collect()
    }
}

/// The state spans a genesis names: its `KOBG` payload, or the `GENESIS` record of a KOB1 payload
/// (a curve created in the genesis).
pub fn genesis_spans(payload: &[u8]) -> Option<Vec<Vec<u8>>> {
    if let Some(s) = crate::genesis_states::decode(payload) {
        return Some(s);
    }
    if !payload.starts_with(crate::payload::MAGIC) {
        return None;
    }
    crate::payload::decode(payload).ok()??.records.into_iter().find_map(|r| match r {
        crate::payload::Record::Unknown { record_type: crate::payload::REC_GENESIS, value } => crate::genesis_states::decode(&value),
        _ => None,
    })
}

/// The genesis outputs that `covenant_id` commits to: the group of the input whose outpoint and
/// group hash to it (KCC-23 section 3.3, as [`crate::kcc23::is_bound`] checks it). Any other output
/// that names the id, under another authorizing input, is not the token's: the id does not bind it,
/// so whoever serves the genesis could have added it. Empty when no input's group hashes to the id.
fn bound_outputs(genesis: &Transaction, covenant_id: Hash) -> Vec<usize> {
    for (i, input) in genesis.inputs.iter().enumerate() {
        let Ok(authorizing) = u16::try_from(i) else { break };
        let group: Vec<(u32, &TransactionOutput)> = genesis
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.covenant.is_some_and(|b| b.authorizing_input == authorizing && b.covenant_id == covenant_id))
            .map(|(k, o)| (k as u32, o))
            .collect();
        if !group.is_empty() && covenant_id_of(input.previous_outpoint, group.iter().copied()) == covenant_id {
            return group.into_iter().map(|(k, _)| k as usize).collect();
        }
    }
    Vec::new()
}

/// The genesis outputs of `covenant_id` that `lock` holds, proven from the states the genesis names
/// (a state counts only where its P2SH under a pinned token program is the output's script). Only
/// the outputs the id commits to count, in the locked amount and in the supply.
///
/// An error says why the lock holds nothing in this genesis.
pub fn verify_genesis_lock(genesis: &Transaction, covenant_id: Hash, lock: &CreatorLock) -> std::result::Result<VerifiedLock, String> {
    let token_outputs = bound_outputs(genesis, covenant_id);
    if token_outputs.is_empty() {
        return Err("the genesis has no output of the token".into());
    }
    let spans = genesis_spans(&genesis.payload).ok_or("the genesis names no states")?;
    if spans.len() > token_outputs.len() {
        return Err("the genesis names more states than it has outputs".into());
    }
    for tid in TemplateId::TOKEN_PROGRAMS {
        let tpl = token_template(tid);
        let Some(states) = spans.iter().map(|b| TokenState::decode_with(tpl, b).ok()).collect::<Option<Vec<_>>>() else { continue };
        let scripts: Vec<ScriptPublicKey> = states.iter().map(|s| s.spk_with(tpl)).collect();
        let mut proven = 0usize;
        let mut supply: u128 = 0;
        let mut amount: u128 = 0;
        let mut outputs = vec![];
        let mut ext = [0u8; 32];
        for &k in &token_outputs {
            let Some(at) = scripts.iter().position(|s| *s == genesis.outputs[k].script_public_key) else { continue };
            proven += 1;
            let a = states[at].amount().max(0) as u128;
            supply += a;
            if let TokenState::Kcc20(s) = &states[at] {
                if lock.holds(s) && s.amount > 0 {
                    amount += a;
                    ext = s.extension_commitment;
                    outputs.push(LockedOutput { index: k as u32, amount: s.amount as u64 });
                }
            }
        }
        if proven == 0 {
            continue;
        }
        if outputs.is_empty() {
            return Err("no genesis output is held by the lock".into());
        }
        return Ok(VerifiedLock {
            until: lock.until,
            key: lock.key,
            script: lock.script(),
            authority: lock.authority(),
            amount: u64::try_from(amount).map_err(|_| "the locked amount overflows")?,
            outputs,
            program: tid,
            extension_commitment: ext,
            supply: (proven == token_outputs.len()).then(|| u64::try_from(supply).ok()).flatten(),
        });
    }
    Err("no state the genesis names is proven under a token program".into())
}

// ------------------------------------------------------------------------------------------------
// the unlock

/// An unlock: the locked token UTXO(s) of one lock and the lock coin, paid to `to` in one
/// transaction (the request of kob-wasm `buildUnlock` and `kob token unlock`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockCreatorLock {
    pub token: TokenRef,
    pub lock: CreatorLock,
    /// The token UTXOs the lock holds (1 up to the program's token inputs).
    pub tokens: Vec<TokenUtxo>,
    /// The lock coin: a plain KAS UTXO at the lock's address ([`CreatorLock::spk`]).
    pub coin: Utxo,
    /// The receiving key of the tokens (default: the lock's key).
    #[serde(with = "crate::json::field", default)]
    pub to: Option<[u8; 32]>,
    /// KAS per token output (default: the token inputs' KAS).
    #[serde(with = "crate::json::field", default)]
    pub carrier: Option<u64>,
    /// More KAS when the coin does not pay the fee.
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// The KAS change's key (default: the lock's key).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// A lock coin: `amount` sompi (default [`LOCK_COIN`]) to the lock's address, funded by
/// `funding`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockCoin {
    pub lock: CreatorLock,
    pub funding: Vec<KeyUtxo>,
    #[serde(with = "crate::json::field", default)]
    pub amount: Option<u64>,
    /// The change's key (default: the first funding key).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// The budgets of an unlock's inputs: the lock coin's [`LOCK_BUDGET`], and a token input whose
/// owner proof is the coin's index one unit above the covenant-id proof of the same shape (both
/// compare an input's script or covenant id; the P2SH envelope is built first). The table answers
/// every other role.
pub fn unlock_budgets<'a>(table: BudgetFn<'a>) -> impl Fn(&str) -> Result<u16> + 'a {
    move |role: &str| {
        if role == LOCK_ROLE {
            return Ok(LOCK_BUDGET);
        }
        if role.contains(".p2sh.") {
            return table(&role.replace(".p2sh.", ".covid.")).map(|b| b.saturating_add(1));
        }
        table(role)
    }
}

/// Builds the KAS output that authorises an unlock (a plain send to the lock's address).
pub fn build_lock_coin(r: &LockCoin, budgets: BudgetFn) -> Result<BuiltTx> {
    if r.funding.is_empty() {
        return Err(Error::Invalid("lock coin: no funding".into()));
    }
    CreatorLock::new(r.lock.until, r.lock.key)?;
    let change = r.change.or(r.funding.first().map(|f| f.pubkey));
    let mut d = Draft::new(0, &r.fee, change)?;
    for f in &r.funding {
        d.add_p2pk(f);
    }
    d.add_output(r.amount.unwrap_or(LOCK_COIN), r.lock.spk(), None);
    d.seal(budgets)
}

/// Builds an unlock (see the module docs). Refused before nothing: the node accepts it only once
/// its past median time is past `lock.until`, which is the transaction's lock time.
pub fn build_unlock(r: &UnlockCreatorLock, budgets: BudgetFn) -> Result<BuiltTx> {
    let lock = CreatorLock::new(r.lock.until, r.lock.key)?;
    let token = (r.token.covenant_id, r.token.program);
    if !token.1.is_token() {
        return Err(Error::Invalid(format!("{} is not a token program", token.1.name())));
    }
    if r.tokens.is_empty() {
        return Err(Error::Invalid("unlock: no locked token UTXO".into()));
    }
    if r.coin.covenant_id.is_some() {
        return Err(Error::Invalid("unlock: the lock coin is a plain KAS UTXO (this one carries a covenant id)".into()));
    }
    let to = r.to.unwrap_or(lock.key);
    check_key(&to, "unlock recipient")?;
    let mut ext = None;
    let mut total: i64 = 0;
    let mut kas: u64 = 0;
    for t in &r.tokens {
        let TokenState::Kcc20(s) = &t.state else { return Err(Error::Invalid("unlock: a creator lock holds KCC-20 states".into())) };
        if !lock.holds(s) {
            return Err(Error::Invalid("unlock: a token UTXO is not held by this lock".into()));
        }
        if *ext.get_or_insert(s.extension_commitment) != s.extension_commitment {
            return Err(Error::Invalid("unlock: the token UTXOs mix extension commitments".into()));
        }
        total = total.checked_add(s.amount).ok_or_else(|| Error::Invalid("unlock: amounts overflow".into()))?;
        kas = kas.saturating_add(t.utxo.amount);
    }
    if total <= 0 {
        return Err(Error::Invalid("unlock: nothing to unlock".into()));
    }
    let mut d = Draft::new(lock.until, &r.fee, Some(r.change.unwrap_or(lock.key)))?;
    // the coin first: its index (0) is every token input's owner witness
    let at = d.add_input(&r.coin, SigPlan::CreatorLock { until: lock.until, key: lock.key }, LOCK_ROLE, SEQUENCE_NONFINAL);
    let witness = Witness::P2sh(u8::try_from(at).expect("index 0"));
    for t in &r.tokens {
        d.add_token_input(t, token, witness.clone())?;
    }
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let state = TokenState::Kcc20(Kcc20State::p2pk(total, to, ext.expect("one token input")));
    d.add_token_output(token, state, r.carrier.unwrap_or(kas))?;
    d.seal(&unlock_budgets(budgets))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        crate::tx::pubkey_of(&[0x11; 32]).unwrap()
    }

    #[test]
    fn the_script_is_the_template_and_parses_back() {
        let l = CreatorLock::new(1_800_000_000_000, key()).unwrap();
        let s = l.script();
        assert_eq!(s.len(), SCRIPT_LEN);
        assert_eq!((s[0], s[9], s[10], s[43]), (0x08, 0xb0, 0x20, 0xac));
        assert_eq!(CreatorLock::parse(&s), Some(l));
        assert_eq!(CreatorLock::parse_hex(&l.script_hex()), Some(l));
        assert_eq!(l.spk().script()[2..34], l.authority());
    }

    #[test]
    fn look_alikes_are_not_locks() {
        let l = CreatorLock::new(1_800_000_000_000, key()).unwrap();
        let s = l.script();
        // a DAA-score lock time
        let mut daa = s.clone();
        daa[1..9].copy_from_slice(&1_000_000u64.to_le_bytes());
        assert_eq!(CreatorLock::parse(&daa), None);
        // CHECKSIGVERIFY, CHECKSEQUENCEVERIFY, a trailing byte, one byte short, a key off the curve
        for (at, b) in [(43usize, 0xadu8), (9, 0xb1), (0, 0x07), (10, 0x21)] {
            let mut x = s.clone();
            x[at] = b;
            assert_eq!(CreatorLock::parse(&x), None, "byte {at} = {b:#x}");
        }
        let mut longer = s.clone();
        longer.push(0x75);
        assert_eq!(CreatorLock::parse(&longer), None);
        assert_eq!(CreatorLock::parse(&s[..43]), None);
        let mut off = s.clone();
        off[11..43].copy_from_slice(&[0xff; 32]);
        assert_eq!(CreatorLock::parse(&off), None);
        assert!(CreatorLock::new(MAX_UNTIL + 1, key()).is_err());
        assert!(CreatorLock::new(LOCK_TIME_THRESHOLD - 1, key()).is_err());
    }
}
