//! Automatic creator lock (`docs/spec/creator-lock-auto.md`): a creator lock that opens by itself.
//!
//! The manual lock ([`crate::creator_lock`]) is a 44-byte script that needs the creator's
//! signature after its time. This one is the pinned SilverScript template `KobLockAuto`
//! (`contracts/locks/KobLockAuto.sil`): after `until` it needs no signature at all, and anyone may
//! send the release, but the release can only pay the creator.
//!
//! The locked genesis output's owner is the `p2sh/v1` hash of the template's redeem script for
//! (`until`, the creator's key, the token program's prefix and suffix lengths). The genesis also
//! pays a plain KAS output, the *lock coin* ([`AUTO_COIN`]), to the script's P2SH address. The
//! release ([`build_release`]) spends exactly:
//!
//! - input 0, the coin (the script's `release()`, no argument);
//! - input 1, the locked token UTXO, from the same transaction as the coin;
//! - input 2, optional, a plain KAS input of whoever sends it (a keeper's tip comes back on it);
//!
//! and pays output 0, the tokens, to the creator's key (the same state, owner scheme `0x00`), and
//! output 1, the coin's KAS less at most [`MAX_TAKE`], to the creator's key. What the coin leaves
//! pays the fee and the sender's tip ([`RELEASE_TIP`]).
//!
//! The declaration names the lock by its terms, the 44-byte manual template of (`until`, key) in
//! `lock`, and `"lock_kind": "auto"` ([`crate::kcc23`]). The script is derived from the terms and
//! the token program the genesis is proven under.

use std::sync::OnceLock;

use kaspa_consensus_core::tx::ScriptPublicKey;
use serde::{Deserialize, Serialize};
use silverscript_abi::{encode_contract_entry_sig_script, SilAbiArtifact};

use crate::artifacts::{parse_artifact, token_template, TemplateId};
use crate::build::{BudgetFn, TokenRef};
use crate::creator_lock::{CreatorLock, LOCK_COIN};
use crate::error::{Error, Result};
use crate::json::to_hex;
use crate::script::{p2pk_spk, push_data};
use crate::state::{Kcc20State, TokenState};
use crate::tx::{check_key, BuiltTx, Draft, FeeOptions, KeyUtxo, SigPlan, TokenUtxo, Utxo, Witness, SEQUENCE_NONFINAL};

/// The committed artifact of `contracts/locks/KobLockAuto.sil`.
const ARTIFACT_JSON: &str = include_str!("../../../contracts/artifacts/KobLockAuto.json");
/// The pinned template hash of `KobLockAuto` (blake3 over the lengths and bytes of the prefix and
/// the suffix around its state span, as `silverscript_abi::template_hash`). A changed artifact is
/// a changed protocol.
pub const TEMPLATE_HASH_HEX: &str = "ed392654183dfc8e0d8ff49130b15ba142b27af33ecb3a854b98ada5c45158e4";
/// The entry of the release.
pub const ENTRY: &str = "release";
/// Length of the state span: `until` (temporal), the key, the two template lengths.
pub const STATE_LEN: usize = 9 + 33 + 9 + 9;
/// The most sompi of the coin a release keeps from the creator (the template's `MAX_TAKE`): a
/// tip of 0.01 KAS and up to 0.02 KAS of fee (a release of `KCC20PublicMint` pays about 0.01 KAS
/// at the relay floor).
pub const MAX_TAKE: u64 = 3_000_000;
/// The tip a keeper takes for a release, 0.01 KAS. The creator pays it and the fee out of the coin.
pub const RELEASE_TIP: u64 = 1_000_000;
/// The most fee a release with [`RELEASE_TIP`] may pay out of the coin.
pub const RELEASE_FEE_MAX: u64 = MAX_TAKE - RELEASE_TIP;
/// The KAS the genesis pays to the lock's address: 1 KAS, back to the creator at the release
/// less the fee and the tip.
pub const AUTO_COIN: u64 = LOCK_COIN;
/// The smallest lock coin a reader takes for an automatic lock: 0.2 KAS (a coin below it could
/// not pay the release and its own KAS output).
pub const AUTO_COIN_MIN: u64 = 20_000_000;
/// The budget role of the coin's input: `creator-lock-auto.<token program>` ([`auto_role`]).
pub const AUTO_ROLE: &str = "creator-lock-auto";

/// The budget role of the coin's input for the tokens of `program`.
pub fn auto_role(program: TemplateId) -> String {
    format!("{AUTO_ROLE}.{}", program.name())
}

/// Compute budget of the coin's input (10,000 script units each). The script hashes the token's
/// redeem script twice, so the cost grows with the program's size: measured in the script engine
/// for every issued program by `crates/kob-tests/tests/kcc20/lock_auto_tests.rs` (exact: one unit
/// less fails). Another KCC-20 program gets a bound from its redeem script's length.
pub fn auto_budget(program: TemplateId) -> u16 {
    match program {
        TemplateId::Kcc20Ref => AUTO_BUDGET_REF,
        TemplateId::Kcc20PublicMint => AUTO_BUDGET_PUBLIC_MINT,
        _ => {
            let t = token_template(program);
            let len = (t.prefix.len() + t.state_len + t.suffix.len()) as u64;
            // measured: about 13 units per byte of the redeem script and 1,750 more; the budget
            // allows 10,000 units each and 9,999 free
            u16::try_from((len * 14 + 2_000).saturating_sub(9_999).div_ceil(10_000)).unwrap_or(u16::MAX)
        }
    }
}
/// [`auto_budget`] of `KCC20Ref` (3 / 3).
pub const AUTO_BUDGET_REF: u16 = 3;
/// [`auto_budget`] of `KCC20PublicMint`, KOB's default issuance.
pub const AUTO_BUDGET_PUBLIC_MINT: u16 = 5;

struct Loaded {
    artifact: SilAbiArtifact,
    contract: String,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
}

fn load() -> std::result::Result<Loaded, String> {
    let artifact = parse_artifact(ARTIFACT_JSON).map_err(|e| e.to_string())?;
    if artifact.contracts.len() != 1 {
        return Err("KobLockAuto: expected one contract".into());
    }
    let (name, c) = artifact.contracts.iter().next().expect("one contract");
    let bc = &c.compiled.bytecode;
    let span = c.compiled.state_span;
    if span.len != STATE_LEN || span.offset.checked_add(span.len).is_none_or(|e| e > bc.len()) {
        return Err(format!("KobLockAuto: state span {}+{} is not the {STATE_LEN}-byte state", span.offset, span.len));
    }
    let prefix = bc[..span.offset].to_vec();
    let suffix = bc[span.offset + span.len..].to_vec();
    let hash = silverscript_abi::template_hash(&prefix, &suffix);
    if hash != c.compiled.template_hash {
        return Err("KobLockAuto: the recorded template hash does not match the bytecode".into());
    }
    if to_hex(&hash) != TEMPLATE_HASH_HEX {
        return Err(format!("KobLockAuto: template hash {} is not the pinned {TEMPLATE_HASH_HEX}", to_hex(&hash)));
    }
    if !c.entries.contains_key(ENTRY) || c.entries.len() != 1 {
        return Err("KobLockAuto: the one entry is release()".into());
    }
    Ok(Loaded { contract: name.clone(), prefix, suffix, artifact: artifact.clone() })
}

fn loaded() -> &'static Loaded {
    static L: OnceLock<Loaded> = OnceLock::new();
    L.get_or_init(|| load().unwrap_or_else(|e| panic!("{e}")))
}

/// Checks the embedded artifact: one contract, the state span, the recorded and the pinned hash.
pub fn check() -> std::result::Result<(), String> {
    load().map(|_| ())
}

/// The template's prefix and suffix (around the state span).
pub fn template_parts() -> (&'static [u8], &'static [u8]) {
    let l = loaded();
    (&l.prefix, &l.suffix)
}

/// An automatic creator lock: its terms and the template lengths of its token program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLock {
    /// Unix ms, `LOCK_TIME_THRESHOLD..=MAX_UNTIL` of [`crate::creator_lock`].
    #[serde(with = "crate::json::field")]
    pub until: u64,
    /// The creator's x-only key: the tokens and the coin's KAS go to it.
    #[serde(with = "crate::json::field")]
    pub key: [u8; 32],
    /// The token program's bytes before its 112-byte KCC-20 state.
    pub prefix_len: u32,
    /// ... and after it.
    pub suffix_len: u32,
}

impl AutoLock {
    /// The automatic lock of `terms` for the tokens of `program` (a KCC-20 token program).
    pub fn new(terms: CreatorLock, program: TemplateId) -> Result<Self> {
        let terms = CreatorLock::new(terms.until, terms.key)?;
        if !program.is_token() || program.family() != crate::Family::Kcc20 {
            return Err(Error::Invalid(format!("an automatic creator lock holds KCC-20 tokens, not {}", program.name())));
        }
        let t = token_template(program);
        if t.state_len != 112 {
            return Err(Error::Invalid(format!("{}: the state is not the 112-byte KCC-20 state", program.name())));
        }
        Ok(AutoLock {
            until: terms.until,
            key: terms.key,
            prefix_len: u32::try_from(t.prefix.len()).map_err(|_| Error::Invalid("prefix too long".into()))?,
            suffix_len: u32::try_from(t.suffix.len()).map_err(|_| Error::Invalid("suffix too long".into()))?,
        })
    }

    /// The terms (`until`, key).
    pub fn terms(&self) -> CreatorLock {
        CreatorLock { until: self.until, key: self.key }
    }

    /// The state span: the canonical pushes of `until`, the key and the two lengths.
    pub fn state(&self) -> Vec<u8> {
        let mut s = Vec::with_capacity(STATE_LEN);
        s.push(0x08);
        s.extend_from_slice(&(self.until as i64).to_le_bytes());
        s.push(0x20);
        s.extend_from_slice(&self.key);
        s.push(0x08);
        s.extend_from_slice(&i64::from(self.prefix_len).to_le_bytes());
        s.push(0x08);
        s.extend_from_slice(&i64::from(self.suffix_len).to_le_bytes());
        s
    }

    /// The redeem script: `prefix ‖ state ‖ suffix` of the pinned template.
    pub fn script(&self) -> Vec<u8> {
        let (p, x) = template_parts();
        [p, &self.state(), x].concat()
    }

    /// The lock of a redeem script that is exactly the template with a valid state, else `None`.
    pub fn parse(script: &[u8]) -> Option<Self> {
        let (p, x) = template_parts();
        if script.len() != p.len() + STATE_LEN + x.len() || !script.starts_with(p) || !script.ends_with(x) {
            return None;
        }
        let s = &script[p.len()..p.len() + STATE_LEN];
        if s[0] != 0x08 || s[9] != 0x20 || s[42] != 0x08 || s[51] != 0x08 {
            return None;
        }
        let int = |at: usize| i64::from_le_bytes(s[at..at + 8].try_into().expect("8 bytes"));
        let until = u64::try_from(int(1)).ok()?;
        let key: [u8; 32] = s[10..42].try_into().ok()?;
        let (pl, sl) = (u32::try_from(int(43)).ok()?, u32::try_from(int(52)).ok()?);
        let terms = CreatorLock::new(until, key).ok()?;
        Some(AutoLock { until: terms.until, key: terms.key, prefix_len: pl, suffix_len: sl })
    }

    /// The `p2sh/v1` owner of the locked state: `Blake2b(script)`.
    pub fn authority(&self) -> [u8; 32] {
        crate::kcc2::p2sh_authority(&self.script())
    }

    /// The script public key of the lock's address (the lock coin's).
    pub fn spk(&self) -> ScriptPublicKey {
        crate::kcc2::p2sh_envelope(&self.authority())
    }

    /// A locked KCC-20 state of `amount`: owner scheme 0x03, borrow disabled.
    pub fn locked_state(&self, amount: i64, extension_commitment: [u8; 32]) -> Kcc20State {
        Kcc20State {
            amount,
            owner: self.authority(),
            owner_scheme: crate::kcc2::P2SH,
            borrow_scheme: 0,
            borrow_guard: [0; 32],
            extension_commitment,
        }
    }

    /// Whether a KCC-20 state is held by this lock (scheme 0x03, its hash, borrow disabled).
    pub fn holds(&self, s: &Kcc20State) -> bool {
        s.owner_scheme == crate::kcc2::P2SH && s.owner == self.authority() && s.borrow_scheme == 0 && s.borrow_guard == [0; 32]
    }

    /// The signature script of the coin's input: `release()` and the redeem script.
    pub fn sig_script(&self) -> Result<Vec<u8>> {
        let l = loaded();
        let mut s = encode_contract_entry_sig_script(&l.artifact, &l.contract, ENTRY, &[])
            .map_err(|e| Error::Invalid(format!("KobLockAuto.{ENTRY}: {e}")))?;
        s.extend(push_data(&self.script()));
        Ok(s)
    }

    /// Whether the lengths are the ones of `program` (a script whose lengths are another program's
    /// can never be released).
    pub fn fits(&self, program: TemplateId) -> bool {
        AutoLock::new(self.terms(), program).is_ok_and(|a| a == *self)
    }
}

// ------------------------------------------------------------------------------------------------
// the release

/// A release of an automatic creator lock: the coin and the locked token UTXO of one genesis, the
/// tokens and the coin's KAS to the creator (the request of kob-wasm `buildRelease` and
/// `kob token unlock`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAutoLock {
    pub token: TokenRef,
    /// The terms (`until`, the creator's key).
    pub lock: CreatorLock,
    /// The locked token UTXO (a genesis output).
    pub tokens: TokenUtxo,
    /// The lock coin, a plain KAS output of the same genesis at the lock's address.
    pub coin: Utxo,
    /// A keeper's KAS input, which comes back on output 2 with the tip.
    #[serde(default)]
    pub keeper: Option<KeyUtxo>,
    /// The keeper's tip, sompi (needs `keeper`; at most [`MAX_TAKE`] with the fee).
    #[serde(with = "crate::json::field", default)]
    pub tip: u64,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// The budgets of a release: the coin's [`auto_budget`] and the token input's p2sh owner proof
/// ([`crate::creator_lock::unlock_budgets`]).
pub fn release_budgets<'a>(table: BudgetFn<'a>) -> impl Fn(&str) -> Result<u16> + 'a {
    let inner = crate::creator_lock::unlock_budgets(table);
    move |role: &str| match role.strip_prefix(AUTO_ROLE).and_then(|r| r.strip_prefix('.')) {
        Some(name) => TemplateId::TOKEN_PROGRAMS
            .into_iter()
            .find(|t| t.name() == name)
            .map(auto_budget)
            .ok_or_else(|| Error::Invalid(format!("no budget for role {role}"))),
        None => inner(role),
    }
}

/// Builds a release (see the module docs). It is refused here when the fee and the tip would keep
/// more than [`MAX_TAKE`] of the coin from the creator; the node takes it only once its past median
/// time is past `lock.until`, the transaction's lock time.
pub fn build_release(r: &ReleaseAutoLock, budgets: BudgetFn) -> Result<BuiltTx> {
    let lock = AutoLock::new(r.lock, r.token.program)?;
    let token = (r.token.covenant_id, r.token.program);
    let TokenState::Kcc20(s) = &r.tokens.state else {
        return Err(Error::Invalid("release: an automatic lock holds a KCC-20 state".into()));
    };
    if !lock.holds(s) || s.amount <= 0 {
        return Err(Error::Invalid("release: the token UTXO is not held by this lock".into()));
    }
    if r.coin.covenant_id.is_some() {
        return Err(Error::Invalid("release: the lock coin is a plain KAS UTXO (this one carries a covenant id)".into()));
    }
    if r.coin.transaction_id != r.tokens.utxo.transaction_id {
        return Err(Error::Invalid("release: the lock coin and the locked tokens must be outputs of the same genesis".into()));
    }
    if r.tip > 0 && r.keeper.is_none() {
        return Err(Error::Invalid("release: a tip comes back on a keeper input (give `keeper`)".into()));
    }
    if r.tip > MAX_TAKE {
        return Err(Error::Invalid(format!("release: the tip {} is above the lock's {MAX_TAKE} sompi", r.tip)));
    }
    check_key(&lock.key, "creator lock key")?;
    // Output 1 is the creator's KAS: the change, in the slot kept for it.
    let mut d = Draft::new(lock.until, &r.fee, Some(lock.key))?;
    d.add_input(
        &r.coin,
        SigPlan::CreatorLockAuto { until: lock.until, key: lock.key, program: r.token.program },
        auto_role(r.token.program),
        SEQUENCE_NONFINAL,
    );
    d.add_token_input(&r.tokens, token, Witness::P2sh(0))?;
    if let Some(k) = &r.keeper {
        if k.utxo.covenant_id.is_some() {
            return Err(Error::Invalid("release: the keeper input is a plain KAS UTXO".into()));
        }
        d.add_p2pk(k);
    }
    let state = TokenState::Kcc20(Kcc20State::p2pk(s.amount, lock.key, s.extension_commitment));
    d.add_token_output(token, state, r.tokens.utxo.amount)?;
    if let Some(k) = &r.keeper {
        d.ensure_output(1);
        d.pin_output(1);
        let back = k.utxo.amount.checked_add(r.tip).ok_or_else(|| Error::Invalid("release: amounts overflow".into()))?;
        d.add_output(back, p2pk_spk(&k.pubkey), None);
    }
    let built = d.seal(&release_budgets(budgets))?;
    let out1 = built.tx.outputs.get(1);
    if out1.is_none_or(|o| o.script_public_key != crate::tx::spk_to_string(&p2pk_spk(&lock.key))) {
        return Err(Error::Invalid("release: output 1 is not the creator's KAS (the coin does not pay the fee)".into()));
    }
    let back = out1.map_or(0, |o| o.value);
    let take = r.coin.amount.saturating_sub(back);
    if take > MAX_TAKE {
        return Err(Error::Invalid(format!(
            "release: the fee ({} sompi) and the tip ({}) keep {take} sompi of the coin, above the lock's {MAX_TAKE}: lower the fee rate or the tip",
            built.fee.fee, r.tip
        )));
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creator_lock::{LOCK_TIME_THRESHOLD, MAX_UNTIL};

    fn key() -> [u8; 32] {
        crate::tx::pubkey_of(&[0x11; 32]).unwrap()
    }

    #[test]
    fn the_artifact_is_pinned() {
        check().unwrap();
        assert_eq!(loaded().prefix.len(), 1);
    }

    #[test]
    fn the_script_holds_the_terms_and_parses_back() {
        for p in [TemplateId::Kcc20PublicMint, TemplateId::Kcc20Ref] {
            let a = AutoLock::new(CreatorLock::new(1_800_000_000_000, key()).unwrap(), p).unwrap();
            let s = a.script();
            assert_eq!(AutoLock::parse(&s), Some(a));
            assert!(a.fits(p));
            assert_eq!(a.spk().script()[2..34], a.authority());
            assert_eq!(a.state().len(), STATE_LEN);
            // not the manual lock's owner
            assert_ne!(a.authority(), a.terms().authority());
            let mut x = s.clone();
            x[1] = 0x07;
            assert_eq!(AutoLock::parse(&x), None);
            let mut daa = a;
            daa.until = LOCK_TIME_THRESHOLD - 1;
            assert_eq!(AutoLock::parse(&daa.script()), None);
            let mut far = a;
            far.until = MAX_UNTIL + 1;
            assert_eq!(AutoLock::parse(&far.script()), None);
        }
        let a = AutoLock::new(CreatorLock::new(1_800_000_000_000, key()).unwrap(), TemplateId::Kcc20Ref).unwrap();
        assert!(!a.fits(TemplateId::Kcc20PublicMint));
    }

    /// A lock lasts up to 10 years, so every lock template a build ever issued (the `locks` of both
    /// deployment records, which only grow: `scripts/lib/deploy.mjs`) must stay readable and
    /// releasable: the reader takes its genesis owner for the declared terms, and the release (the
    /// keeper's and the creator's) spends exactly its script. A re-pin that drops an old template
    /// fails here (docs/spec/template-retirement.md, "Lock templates").
    #[test]
    fn every_issued_lock_template_stays_readable_and_releasable() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for record in [
            include_str!("../../../contracts/deploy/mainnet/deployment.json"),
            include_str!("../../../contracts/deploy/testnet-10/deployment.json"),
        ] {
            let m: serde_json::Value = serde_json::from_str(record).unwrap();
            let locks = m["locks"].as_array().expect("the record lists its lock templates");
            assert!(locks.iter().any(|l| l["hash"] == TEMPLATE_HASH_HEX), "{}: the pinned lock is not in the record", m["network"]);
            for l in locks {
                let (name, hash) = (l["name"].as_str().unwrap(), l["hash"].as_str().unwrap());
                let text = std::fs::read_to_string(root.join(l["artifact"].as_str().unwrap())).unwrap();
                let art = parse_artifact(&text).unwrap();
                let c = &art.contracts[name];
                let (bc, span) = (&c.compiled.bytecode, c.compiled.state_span);
                let (prefix, suffix) = (&bc[..span.offset], &bc[span.offset + span.len..]);
                assert_eq!(to_hex(&silverscript_abi::template_hash(prefix, suffix)), hash, "{name}: the artifact is not the issued template");
                assert_eq!(span.len, STATE_LEN, "{name} {hash}");
                for p in TemplateId::TOKEN_PROGRAMS {
                    let Ok(lock) = AutoLock::new(CreatorLock::new(1_800_000_000_000, key()).unwrap(), p) else { continue };
                    let script = [prefix, &lock.state(), suffix].concat();
                    assert_eq!(AutoLock::parse(&script), Some(lock), "{hash} {}: not readable", p.name());
                    assert_eq!(lock.authority(), crate::kcc2::p2sh_authority(&script), "{hash} {}: another owner", p.name());
                    assert!(lock.sig_script().unwrap().ends_with(&push_data(&script)), "{hash} {}: not releasable", p.name());
                }
            }
        }
    }
}
