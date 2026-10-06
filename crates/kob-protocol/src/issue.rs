//! Fixed-supply KCC-20 issuance: program pinning, genesis transaction builder, verification and the
//! off-chain documents (supply, metadata, registry entry) that describe the issued token.
//!
//! What is issued: the reference KCC-20 program (argent kcc20-reference PR #1) compiled with the
//! KOB slot limits 8 token inputs / 8 token outputs per transfer (`contracts/kcc20/variants/
//! KCC20Ref_8x8.sil`). The program has no mint, burn or public-mint entry, so the supply is exactly
//! the sum of the genesis outputs, fixed forever. Proposal P2 is not used.
//!
//! The genesis is ONE transaction: a plain P2PK funding input (input 0) authorises a KIP-20 genesis
//! group of 1..N token outputs whose covenant id is derived exactly as consensus does
//! ([`kaspa_consensus_core::hashing::covenant_id::covenant_id`]: the authorising outpoint plus the
//! index, value and script public key of every output in the group). Every issued output is one
//! token UTXO in the reference program's 112-byte state:
//!
//! ```text
//! 0x08 amount(LE i64) | 0x20 owner | 0x01 owner_scheme | 0x01 borrow_scheme | 0x20 borrow_guard | 0x20 extension_commitment
//! ```
//!
//! Rules enforced here (see `docs` in the CLI): owner scheme 0x04 (covenant id) states are always
//! issued with `borrow_scheme = 0x00`, `borrow_guard = 0^32` (borrows churn the outpoint
//! and break outpoint-bound claims); every other output defaults to borrow disabled too, and a
//! non-zero borrow scheme needs the explicit `allow_borrow` flag. The compiled program is
//! self-tested for owner scheme 0x04 before anything is produced ([`verify_scheme4_enabled`]).

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32};
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use silverscript_abi::{encode_contract_entry_sig_script, ArtifactValue, SilAbiArtifact};

use crate::artifacts::parse_artifact;
use crate::kcc20::{ISSUE_MAX_TOKEN_INPUTS, ISSUE_MAX_TOKEN_OUTPUTS};

/// KCC-20 owner scheme ids: the canonical KCC-2 authority-scheme bytes ([`crate::kcc2`]).
pub const SCHEME_P2PK_SCHNORR: u8 = crate::kcc2::P2PK_SCHNORR;
/// Owner is `Hash(pubkey)` (unkeyed BLAKE3, KCC-2 `p2pkh-schnorr/v1`) of a 32-byte Schnorr public key.
pub const SCHEME_P2PKH_SCHNORR: u8 = crate::kcc2::P2PKH_SCHNORR;
/// Owner is `Hash(pubkey)` (unkeyed BLAKE3, KCC-2 `p2pkh-ecdsa/v1`) of a compressed 33-byte ECDSA public key.
pub const SCHEME_P2PKH_ECDSA: u8 = crate::kcc2::P2PKH_ECDSA;
/// Owner is the KCC-1 P2SH commitment `Blake2b(R)` of one exact redeem script (KCC-2 `p2sh/v1`).
pub const SCHEME_P2SH: u8 = crate::kcc2::P2SH;
/// Owner is a KIP-20 covenant id (custody by a covenant such as a KOB order).
pub const SCHEME_COVENANT_ID: u8 = crate::kcc2::COVENANT_ID;
/// Owner schemes the issued program accepts, as recorded in the registry: every assigned KCC-2 scheme, no custom one.
pub const OWNER_SCHEMES_ENABLED: [u8; 5] = crate::kcc2::ASSIGNED;

/// Largest supply the tool issues. Order arithmetic multiplies token amounts by prices
/// terms in 64-bit script integers, so KOB keeps token amounts far below `i64::MAX`.
pub const MAX_SUPPLY: u64 = 2_900_000_000_000_000_000;
/// Default KAS carried by every token UTXO of the genesis group: 10 KAS, the KOB order carrier. Storage mass
/// grows as 1 / value per output, so 1 KAS outputs would cap a genesis group at about 2 outputs.
pub const DEFAULT_CARRIER: u64 = 1_000_000_000;
/// Default fee rate, sompi per gram: the node's relay floor `rate * max(compute, 2 * size)`
/// ([`crate::tx::min_fee`]; storage mass needs no relay fee).
pub const DEFAULT_FEE_RATE: u64 = 100;
/// Default `extension_commitment` of a fixed-supply standard token: 32 zero bytes, the value the
/// KCC-20 conformance vectors use for a token without extended state.
pub const EXTENSION_FIXED_SUPPLY: [u8; 32] = [0u8; 32];
/// Hard cap on token outputs in one genesis group (a warning is raised above the 8-output slot limit).
pub const MAX_GENESIS_OUTPUTS: usize = 64;
/// Standard transaction mass limit (relay policy).
pub const MAX_STANDARD_MASS: u64 = 100_000;
/// Registry class of the extension commitment used by the tool.
pub const EXTENSION_CLASS: &str = "fixed-supply-standard";
/// Id of the pinned program in `registry/tokens.json` that KOB-issued tokens run (reference KCC-20, 8/8 slots).
pub const REGISTRY_TEMPLATE_ID: &str = "kcc20-ref-8x8";

const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const ARTIFACT_JSON: &str = include_str!("../../../contracts/artifacts/KCC20Ref_8x8.json");
const ARTIFACT_NAME: &str = "KCC20Ref_8x8";

/// Errors of issuance planning and verification.
#[derive(Debug, thiserror::Error)]
pub enum IssueError {
    /// A parameter is invalid.
    #[error("invalid parameter: {0}")]
    Invalid(String),
    /// The holder amounts do not add up to the declared supply.
    #[error("supply mismatch: holders sum to {sum}, declared supply is {declared}")]
    SupplyMismatch {
        /// Sum of the holder amounts.
        sum: u128,
        /// Declared supply.
        declared: u64,
    },
    /// A covenant-held (scheme 0x04) state, or any state without `--allow-borrow`, tried to enable borrowing.
    #[error("borrow rule violated: {0}")]
    Borrow(String),
    /// The funding inputs do not cover carriers plus fee.
    #[error("insufficient funds: need {need} sompi, have {have}")]
    InsufficientFunds {
        /// Sompi needed.
        need: u64,
        /// Sompi available.
        have: u64,
    },
    /// The explicit fee is below the relay minimum.
    #[error("fee {fee} is below the minimum {min} sompi")]
    FeeTooLow {
        /// Fee given.
        fee: u64,
        /// Minimum fee.
        min: u64,
    },
    /// The artifact does not have the expected shape.
    #[error("artifact error: {0}")]
    Artifact(String),
    /// Consensus-level verification failed.
    #[error("verification failed: {0}")]
    Verify(String),
}

type Result<T> = std::result::Result<T, IssueError>;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(IssueError::Invalid(msg.into()))
}

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    silverscript_abi::encode_hex(bytes)
}

/// Parse a hex string of exactly `N` bytes.
pub fn hex_array<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v =
        silverscript_abi::decode_hex(s.trim_start_matches("0x")).map_err(|e| IssueError::Invalid(format!("bad hex `{s}`: {e}")))?;
    v.try_into().map_err(|v: Vec<u8>| IssueError::Invalid(format!("expected {N} bytes of hex, got {}", v.len())))
}

// ------------------------------------------------------------------------------------------------
// Program

/// State of one KCC-20 token UTXO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenState {
    /// Token amount in base units (non-negative, at most `i64::MAX`).
    pub amount: u64,
    /// Owner (interpretation depends on `owner_scheme`).
    pub owner: [u8; 32],
    /// Owner scheme id.
    pub owner_scheme: u8,
    /// Borrow scheme id (0 = disabled).
    pub borrow_scheme: u8,
    /// Scheme-specific guard (zero when borrowing is disabled).
    pub borrow_guard: [u8; 32],
    /// Extension commitment (fixed per token).
    pub extension_commitment: [u8; 32],
}

impl TokenState {
    /// Encoded 112-byte state span.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(112);
        v.push(0x08);
        v.extend_from_slice(&(self.amount as i64).to_le_bytes());
        v.push(0x20);
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.owner_scheme, 0x01, self.borrow_scheme, 0x20]);
        v.extend_from_slice(&self.borrow_guard);
        v.push(0x20);
        v.extend_from_slice(&self.extension_commitment);
        debug_assert_eq!(v.len(), 112);
        v
    }

    /// The state as an ABI struct value (for `transfer` next states).
    pub fn to_abi(&self) -> ArtifactValue {
        BTreeMap::from([
            ("amount".to_string(), ArtifactValue::Int(self.amount as i64)),
            ("owner".to_string(), self.owner.to_vec().into()),
            ("owner_scheme".to_string(), ArtifactValue::Byte(self.owner_scheme)),
            ("borrow_scheme".to_string(), ArtifactValue::Byte(self.borrow_scheme)),
            ("borrow_guard".to_string(), self.borrow_guard.to_vec().into()),
            ("extension_commitment".to_string(), self.extension_commitment.to_vec().into()),
        ])
        .into()
    }
}

/// The pinned token program: template prefix and suffix around the 112-byte state.
#[derive(Debug, Clone)]
pub struct Program {
    /// Artifact name (`KCC20Ref_8x8`).
    pub name: &'static str,
    /// Bytes before the state span.
    pub prefix: Vec<u8>,
    /// Bytes after the state span.
    pub suffix: Vec<u8>,
    /// blake3 template hash over prefix and suffix (as `template_hash` of the compiler).
    pub template_hash: [u8; 32],
    /// sha256 of the committed artifact JSON (LF-normalised, as in `contracts/SHA256SUMS`).
    pub artifact_sha256: [u8; 32],
    /// Maximum token inputs per transfer of this program.
    pub max_token_inputs: usize,
    /// Maximum token outputs per transfer of this program.
    pub max_token_outputs: usize,
    artifact: SilAbiArtifact,
    contract: String,
}

impl Program {
    /// The reference KCC-20 program with 8/8 slots, loaded from the committed artifact.
    ///
    /// The splice codec of [`TokenState::encode`] is cross-checked against the artifact's own
    /// bytecode (whose constructor state is `1000 | 0x03^32 | scheme 4 | borrow 0 | 0^32 | 0xee^32`).
    pub fn kcc20_8x8() -> Result<Self> {
        let artifact = parse_artifact(ARTIFACT_JSON).map_err(|e| IssueError::Artifact(e.to_string()))?;
        let (contract_name, contract) = artifact
            .contracts
            .iter()
            .next()
            .filter(|_| artifact.contracts.len() == 1)
            .ok_or_else(|| IssueError::Artifact("expected exactly one contract".into()))?;
        let bc = &contract.compiled.bytecode;
        let (prefix, state, suffix) =
            contract.compiled.script_parts(bc).ok_or_else(|| IssueError::Artifact("state span outside bytecode".into()))?;
        let placeholder = TokenState {
            amount: 1000,
            owner: [3u8; 32],
            owner_scheme: SCHEME_COVENANT_ID,
            borrow_scheme: 0,
            borrow_guard: [0u8; 32],
            extension_commitment: [0xee; 32],
        };
        if state != placeholder.encode().as_slice() {
            return Err(IssueError::Artifact("state codec does not match the artifact's constructor state".into()));
        }
        let template_hash = silverscript_abi::template_hash(prefix, suffix);
        if template_hash != contract.compiled.template_hash {
            return Err(IssueError::Artifact("template hash does not match the artifact".into()));
        }
        let normalised: Vec<u8> = ARTIFACT_JSON.bytes().filter(|b| *b != b'\r').collect();
        let artifact_sha256: [u8; 32] = Sha256::digest(&normalised).into();
        Ok(Program {
            name: ARTIFACT_NAME,
            prefix: prefix.to_vec(),
            suffix: suffix.to_vec(),
            template_hash,
            artifact_sha256,
            max_token_inputs: ISSUE_MAX_TOKEN_INPUTS,
            max_token_outputs: ISSUE_MAX_TOKEN_OUTPUTS,
            contract: contract_name.clone(),
            artifact: artifact.clone(),
        })
    }

    /// Redeem script of a token UTXO in `state`.
    pub fn redeem(&self, state: &TokenState) -> Vec<u8> {
        [self.prefix.as_slice(), &state.encode(), self.suffix.as_slice()].concat()
    }

    /// Script public key (P2SH of the redeem script) of a token UTXO in `state`.
    pub fn spk(&self, state: &TokenState) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(state))
    }

    /// Signature script of a `transfer` (leader) call: next states and the owner witness, then the redeem script.
    pub fn leader_sig_script(&self, redeem: &[u8], next: &[TokenState], witness: Vec<u8>) -> Result<Vec<u8>> {
        let args = [ArtifactValue::Array(next.iter().map(TokenState::to_abi).collect()), witness.into()];
        let mut s = encode_contract_entry_sig_script(&self.artifact, &self.contract, "transfer", &args)
            .map_err(|e| IssueError::Artifact(e.to_string()))?;
        s.extend(push_data(redeem));
        Ok(s)
    }

    /// Signature script of a `transfer_delegator` call (all token inputs but the leader).
    pub fn delegator_sig_script(&self, redeem: &[u8], witness: Vec<u8>) -> Result<Vec<u8>> {
        let mut s = encode_contract_entry_sig_script(&self.artifact, &self.contract, "transfer_delegator", &[witness.into()])
            .map_err(|e| IssueError::Artifact(e.to_string()))?;
        s.extend(push_data(redeem));
        Ok(s)
    }
}

fn push_data(data: &[u8]) -> Vec<u8> {
    kaspa_txscript::script_builder::ScriptBuilder::new().add_data(data).expect("push data").drain()
}

// ------------------------------------------------------------------------------------------------
// Specification

/// One initial holder of the issued supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// Owner bytes (x-only public key for scheme 0, hash or covenant id otherwise).
    pub owner: [u8; 32],
    /// Owner scheme (0..=4).
    pub owner_scheme: u8,
    /// Amount in base units.
    pub amount: u64,
    /// Borrow scheme (0 = disabled, the default and the only value allowed for scheme 0x04).
    pub borrow_scheme: u8,
    /// Borrow guard (zero when disabled).
    pub borrow_guard: [u8; 32],
}

impl Holder {
    /// Holder with borrowing disabled.
    pub fn new(owner: [u8; 32], owner_scheme: u8, amount: u64) -> Self {
        Holder { owner, owner_scheme, amount, borrow_scheme: 0, borrow_guard: [0u8; 32] }
    }
}

/// A funding UTXO (plain P2PK Schnorr).
#[derive(Debug, Clone)]
pub struct FundingUtxo {
    /// Outpoint.
    pub outpoint: TransactionOutpoint,
    /// Amount in sompi.
    pub amount: u64,
    /// x-only public key that owns the UTXO (P2PK Schnorr).
    pub owner_pubkey: [u8; 32],
}

/// Everything needed to plan an issuance.
#[derive(Debug, Clone)]
pub struct IssueSpec {
    /// Display name.
    pub name: String,
    /// Ticker (uppercase ASCII letters and digits, 2..=12).
    pub ticker: String,
    /// Decimals (informational; amounts are base units).
    pub decimals: u8,
    /// Declared total supply in base units; must equal the sum of the holders.
    pub supply: u64,
    /// Initial holders, one token output each.
    pub holders: Vec<Holder>,
    /// Extension commitment shared by every issued state.
    pub extension_commitment: [u8; 32],
    /// KAS per token output.
    pub carrier: u64,
    /// Explicit fee (else computed from `fee_rate`).
    pub fee: Option<u64>,
    /// Fee rate in sompi per mass unit.
    pub fee_rate: u64,
    /// Permit non-zero borrow schemes on non-0x04 holders.
    pub allow_borrow: bool,
    /// Funding inputs (input 0 authorises the genesis group).
    pub funding: Vec<FundingUtxo>,
    /// Script public key that receives the change (default: the owner of funding[0]).
    pub change_spk: Option<ScriptPublicKey>,
    /// Free-form metadata.
    pub description: Option<String>,
    /// Optional icon URL.
    pub icon: Option<String>,
    /// Optional website URL.
    pub website: Option<String>,
    /// Network label recorded in supply.json (informational).
    pub network: String,
}

impl IssueSpec {
    /// Spec with the defaults (carrier 10 KAS, fee rate 100, zero extension commitment).
    pub fn new(name: &str, ticker: &str, decimals: u8, supply: u64, holders: Vec<Holder>, funding: Vec<FundingUtxo>) -> Self {
        IssueSpec {
            name: name.into(),
            ticker: ticker.into(),
            decimals,
            supply,
            holders,
            extension_commitment: EXTENSION_FIXED_SUPPLY,
            carrier: DEFAULT_CARRIER,
            fee: None,
            fee_rate: DEFAULT_FEE_RATE,
            allow_borrow: false,
            funding,
            change_spk: None,
            description: None,
            icon: None,
            website: None,
            network: "testnet-10".into(),
        }
    }

    /// Validate every rule that does not need the transaction. Returns warnings.
    pub fn validate(&self) -> Result<Vec<String>> {
        let mut warnings = vec![];
        if self.name.is_empty() || self.name.chars().count() > 64 || self.name.chars().any(char::is_control) {
            return invalid("name must be 1..=64 characters without control characters");
        }
        if self.ticker.is_empty()
            || self.ticker.len() < 2
            || self.ticker.len() > 12
            || !self.ticker.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return invalid("ticker must be 2..=12 uppercase ASCII letters or digits");
        }
        if self.decimals > 18 {
            return invalid("decimals must be at most 18");
        }
        if self.holders.is_empty() {
            return invalid("at least one holder is required");
        }
        if self.holders.len() > MAX_GENESIS_OUTPUTS {
            return invalid(format!("at most {MAX_GENESIS_OUTPUTS} genesis outputs (got {})", self.holders.len()));
        }
        if self.holders.len() > ISSUE_MAX_TOKEN_OUTPUTS {
            warnings.push(format!(
                "{} genesis outputs exceed the {}-output transfer limit: consolidating them takes {} transfers of at most {} inputs",
                self.holders.len(),
                ISSUE_MAX_TOKEN_OUTPUTS,
                self.holders.len().div_ceil(ISSUE_MAX_TOKEN_INPUTS),
                ISSUE_MAX_TOKEN_INPUTS
            ));
        }
        if self.supply == 0 || self.supply > MAX_SUPPLY {
            return invalid(format!("supply must be 1..={MAX_SUPPLY} base units"));
        }
        let mut sum: u128 = 0;
        for (i, h) in self.holders.iter().enumerate() {
            if !OWNER_SCHEMES_ENABLED.contains(&h.owner_scheme) {
                let why = match crate::kcc2::classify(h.owner_scheme) {
                    crate::kcc2::SchemeClass::Reserved => "unassigned (KCC-2 reserves 0x05-0x7f for future standard schemes)",
                    crate::kcc2::SchemeClass::Custom => "a custom KCC-2 scheme (0x80-0xff) the KCC-20 program does not define",
                    crate::kcc2::SchemeClass::Assigned => "not enabled in the issued program",
                };
                return invalid(format!("holder {i}: unsupported owner scheme {:#04x}: {why}", h.owner_scheme));
            }
            if h.amount == 0 {
                return invalid(format!("holder {i}: amount must be positive"));
            }
            sum += h.amount as u128;
            if h.borrow_scheme > 3 {
                return invalid(format!("holder {i}: unassigned borrow scheme {:#04x}", h.borrow_scheme));
            }
            if h.owner_scheme == SCHEME_COVENANT_ID && (h.borrow_scheme != 0 || h.borrow_guard != [0u8; 32]) {
                return Err(IssueError::Borrow(format!(
                    "holder {i}: covenant-held (owner scheme 0x04) states must have borrow_scheme 0x00 and borrow_guard 0^32"
                )));
            }
            if h.borrow_scheme != 0 && !self.allow_borrow {
                return Err(IssueError::Borrow(format!(
                    "holder {i}: borrow scheme {:#04x} needs the explicit allow-borrow flag",
                    h.borrow_scheme
                )));
            }
            if h.borrow_scheme == 0 && h.borrow_guard != [0u8; 32] {
                return Err(IssueError::Borrow(format!("holder {i}: borrow_guard must be 0^32 while borrowing is disabled")));
            }
        }
        if sum != self.supply as u128 {
            return Err(IssueError::SupplyMismatch { sum, declared: self.supply });
        }
        if self.funding.is_empty() {
            return invalid("at least one funding UTXO is required");
        }
        if self.carrier == 0 {
            return invalid("carrier must be positive");
        }
        Ok(warnings)
    }
}

// ------------------------------------------------------------------------------------------------
// Plan

/// A planned genesis transaction with everything needed to inspect, verify and publish it.
#[derive(Debug, Clone)]
pub struct GenesisPlan {
    /// The transaction (signature scripts empty until [`GenesisPlan::sign`]).
    pub tx: Transaction,
    /// Funding UTXO entries in input order.
    pub entries: Vec<UtxoEntry>,
    /// Covenant id of the issued token.
    pub covenant_id: Hash,
    /// Token states per output index (outputs `0..states.len()`).
    pub states: Vec<TokenState>,
    /// Change output index and value, when there is change.
    pub change: Option<(usize, u64)>,
    /// Fee in sompi.
    pub fee: u64,
    /// Compute mass.
    pub compute_mass: u64,
    /// Storage mass.
    pub storage_mass: u64,
    /// Estimated serialized size.
    pub size: u64,
    /// Compute budget set on each funding input.
    pub compute_budget: u16,
    /// Non-fatal findings.
    pub warnings: Vec<String>,
    /// Whether the funding inputs are signed.
    pub signed: bool,
    /// The pinned program.
    pub program: Program,
    /// The spec the plan was built from.
    pub spec: IssueSpec,
}

fn p2pk_spk(pk: &[u8; 32]) -> ScriptPublicKey {
    let mut s = vec![OpData32];
    s.extend_from_slice(pk);
    s.push(OpCheckSig);
    ScriptPublicKey::new(0, s.into())
}

/// P2PK (Schnorr) script public key of an x-only public key.
pub fn p2pk_script(pk: &[u8; 32]) -> ScriptPublicKey {
    p2pk_spk(pk)
}

fn schnorr_sighash(tx: &Transaction, entries: &[UtxoEntry], idx: usize) -> Message {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(0x01).expect("sighash all"), &reused);
    Message::from_digest_slice(h.as_bytes().as_slice()).expect("32-byte digest")
}

fn schnorr_sig_script(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mut sig = kp.sign_schnorr(schnorr_sighash(tx, entries, idx)).as_ref().to_vec();
    sig.push(0x01);
    push_data(&sig)
}

/// Sign-and-execute measurement of a P2PK Schnorr input: the compute budget it needs (constant).
fn p2pk_compute_budget() -> u16 {
    let secp = Secp256k1::new();
    let kp = Keypair::from_secret_key(&secp, &SecretKey::from_slice(&[0x11u8; 32]).expect("key"));
    let pk = kp.x_only_public_key().0.serialize();
    let entries = vec![UtxoEntry::new(1_000_000, p2pk_spk(&pk), 0, false, None)];
    let out = TransactionOutput { value: 900_000, script_public_key: p2pk_spk(&pk), covenant: None };
    let input = TransactionInput::new_with_compute_budget(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([1u8; 32]), index: 0 },
        vec![],
        0,
        0,
    );
    let mut tx = Transaction::new(1, vec![input], vec![out], 0, Default::default(), 0, vec![]);
    tx.inputs[0].signature_script = schnorr_sig_script(&tx, &entries, 0, &kp);
    let units = execute_input(&tx, &entries, 0).expect("measure p2pk input");
    units.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16
}

fn execute_input(tx: &Transaction, entries: &[UtxoEntry], idx: usize) -> std::result::Result<u64, String> {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(1_000);
    let input = tx.inputs[idx].clone();
    let mut vm = TxScriptEngine::from_transaction_input(
        &populated,
        &input,
        idx,
        &entries[idx],
        EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
        EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
    );
    vm.execute().map(|_| vm.used_script_units().0).map_err(|e| format!("input {idx}: {e:?}"))
}

fn contextual_storage_mass(tx: &Transaction, entries: &[UtxoEntry]) -> u64 {
    crate::tx::masses(tx, entries).storage
}

/// `(compute, storage, size, relay fee mass, the mass checked against MAX_STANDARD_MASS)`: the fee is
/// the node's relay floor on `max(compute, normalized transient)`; the issuance policy limit keeps
/// counting every dimension.
fn masses(tx: &Transaction, entries: &[UtxoEntry]) -> (u64, u64, u64, u64, u64) {
    let m = crate::tx::masses(tx, entries);
    (m.compute, m.storage, m.size, m.fee_mass, m.compute.max(m.transient).max(m.storage))
}

/// Plan the genesis transaction (unsigned; call [`GenesisPlan::sign`] to sign the funding inputs).
///
/// Token outputs come first (indices `0..holders.len()`), then the change output. All token
/// outputs form one genesis group authorised by input 0.
pub fn build_genesis(spec: &IssueSpec) -> Result<GenesisPlan> {
    let mut warnings = spec.validate()?;
    let program = Program::kcc20_8x8()?;
    let states: Vec<TokenState> = spec
        .holders
        .iter()
        .map(|h| TokenState {
            amount: h.amount,
            owner: h.owner,
            owner_scheme: h.owner_scheme,
            borrow_scheme: h.borrow_scheme,
            borrow_guard: h.borrow_guard,
            extension_commitment: spec.extension_commitment,
        })
        .collect();

    let total_in: u64 = spec.funding.iter().map(|f| f.amount).sum();
    let carriers = spec.carrier.checked_mul(states.len() as u64).ok_or_else(|| IssueError::Invalid("carrier overflow".into()))?;
    let budget = p2pk_compute_budget();
    let entries: Vec<UtxoEntry> =
        spec.funding.iter().map(|f| UtxoEntry::new(f.amount, p2pk_spk(&f.owner_pubkey), 0, false, None)).collect();
    let inputs: Vec<TransactionInput> =
        spec.funding.iter().map(|f| TransactionInput::new_with_compute_budget(f.outpoint, vec![], 0, budget)).collect();
    let change_spk = spec.change_spk.clone().unwrap_or_else(|| p2pk_spk(&spec.funding[0].owner_pubkey));

    // Token outputs and the genesis id (covers index, value and spk of every output of the group).
    let mut outputs: Vec<TransactionOutput> =
        states.iter().map(|s| TransactionOutput { value: spec.carrier, script_public_key: program.spk(s), covenant: None }).collect();
    let id = covenant_id(spec.funding[0].outpoint, outputs.iter().enumerate().map(|(i, o)| (i as u32, o)));
    for o in outputs.iter_mut() {
        o.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: id });
    }

    // Fee: iterate once so the change value (fixed-size field) is in the measured transaction.
    let placeholder_ss = push_data(&[0u8; 65]);
    let assemble = |change: Option<u64>| -> Transaction {
        let mut outs = outputs.clone();
        if let Some(v) = change {
            outs.push(TransactionOutput { value: v, script_public_key: change_spk.clone(), covenant: None });
        }
        let mut ins = inputs.clone();
        for i in ins.iter_mut() {
            i.signature_script = placeholder_ss.clone();
        }
        Transaction::new(1, ins, outs, 0, Default::default(), 0, vec![])
    };
    let need_base = carriers;
    if total_in < need_base {
        return Err(IssueError::InsufficientFunds { need: need_base, have: total_in });
    }
    let slack = total_in - need_base;
    let min_fee_with = |change: Option<u64>| {
        let (_, _, _, fee_mass, _) = masses(&assemble(change), &entries);
        crate::tx::relay_fee_for_mass(fee_mass, spec.fee_rate)
    };
    if slack == 0 {
        // nothing is left for the fee (and a zero-value change output has no storage mass): report the shortfall
        return Err(IssueError::InsufficientFunds { need: need_base.saturating_add(min_fee_with(None)), have: total_in });
    }
    // The relay floor rests on compute and transient mass only, which do not depend on the change value (a fixed-size
    // field), so one probe at the largest possible change settles the fee (storage mass no longer enters it).
    let min_fee = min_fee_with(Some(slack));
    let fee = match spec.fee {
        Some(f) if f < min_fee => return Err(IssueError::FeeTooLow { fee: f, min: min_fee }),
        Some(f) => f,
        None => min_fee,
    };
    if total_in < need_base + fee {
        return Err(IssueError::InsufficientFunds { need: need_base + fee, have: total_in });
    }
    let change_value = total_in - need_base - fee;
    let (change, tx) = if change_value >= spec.carrier.min(DEFAULT_CARRIER) / 10 {
        (Some((outputs.len(), change_value)), assemble(Some(change_value)))
    } else {
        warnings.push(format!("change of {change_value} sompi is below the dust bound and is added to the fee"));
        (None, assemble(None))
    };
    let fee = if change.is_some() { fee } else { total_in - need_base };
    let mut tx = tx;
    for i in tx.inputs.iter_mut() {
        i.signature_script = vec![];
    }
    // The transaction commits its storage mass (consensus rejects a wrong commitment).
    tx.set_storage_mass(contextual_storage_mass(&tx, &entries));
    tx.finalize();
    let (compute_mass, storage_mass, size, _, limit_mass) = {
        let mut measured = tx.clone();
        for i in measured.inputs.iter_mut() {
            i.signature_script = placeholder_ss.clone();
        }
        masses(&measured, &entries)
    };
    if limit_mass > MAX_STANDARD_MASS {
        return Err(IssueError::Verify(format!("transaction mass {limit_mass} exceeds the standard limit {MAX_STANDARD_MASS} (storage mass grows with more or smaller outputs: raise the carrier or issue fewer outputs)")));
    }
    Ok(GenesisPlan {
        tx,
        entries,
        covenant_id: id,
        states,
        change,
        fee,
        compute_mass,
        storage_mass,
        size,
        compute_budget: budget,
        warnings,
        signed: false,
        program,
        spec: spec.clone(),
    })
}

impl GenesisPlan {
    /// Sign every funding input with `secret` (Schnorr, SIGHASH_ALL). The key must own every funding UTXO.
    pub fn sign(&mut self, secret: &SecretKey) -> Result<()> {
        let secp = Secp256k1::new();
        let kp = Keypair::from_secret_key(&secp, secret);
        let pk = kp.x_only_public_key().0.serialize();
        for (i, f) in self.spec.funding.iter().enumerate() {
            if f.owner_pubkey != pk {
                return Err(IssueError::Invalid(format!("funding UTXO {i} is not owned by the signing key")));
            }
        }
        let unsigned = self.tx.clone();
        for i in 0..self.tx.inputs.len() {
            self.tx.inputs[i].signature_script = schnorr_sig_script(&unsigned, &self.entries, i, &kp);
        }
        self.signed = true;
        Ok(())
    }

    /// Transaction id (independent of the signature scripts, so valid before signing).
    pub fn txid(&self) -> TransactionId {
        self.tx.id()
    }

    /// Outpoints of the token outputs.
    pub fn token_outpoints(&self) -> Vec<TransactionOutpoint> {
        let id = self.txid();
        (0..self.states.len()).map(|i| TransactionOutpoint { transaction_id: id, index: i as u32 }).collect()
    }

    /// Consensus-level verification: covenant context (genesis group), covenant id recomputation,
    /// exact token scripts, supply, and (when signed) execution of every funding input in the script
    /// engine with the compute budget set on the transaction. Also self-tests owner scheme 0x04.
    pub fn verify(&self) -> Result<VerifyReport> {
        let fail = |m: String| Err(IssueError::Verify(m));
        verify_scheme4_enabled(&self.program)?;
        // token outputs: exact scripts and one shared covenant id
        let mut supply: u128 = 0;
        for (i, s) in self.states.iter().enumerate() {
            let o = &self.tx.outputs[i];
            if o.script_public_key != self.program.spk(s) {
                return fail(format!("output {i} script does not match its token state"));
            }
            if o.covenant != Some(CovenantBinding { authorizing_input: 0, covenant_id: self.covenant_id }) {
                return fail(format!("output {i} is not bound to the genesis covenant"));
            }
            if s.owner_scheme == SCHEME_COVENANT_ID && (s.borrow_scheme != 0 || s.borrow_guard != [0u8; 32]) {
                return fail(format!("output {i}: covenant-held state with borrowing enabled"));
            }
            supply += s.amount as u128;
        }
        if supply != self.spec.supply as u128 {
            return Err(IssueError::SupplyMismatch { sum: supply, declared: self.spec.supply });
        }
        let group = self.tx.outputs[..self.states.len()].iter().enumerate().map(|(i, o)| (i as u32, o));
        let recomputed = covenant_id(self.tx.inputs[0].previous_outpoint, group);
        if recomputed != self.covenant_id {
            return fail("covenant id does not match the genesis group".into());
        }
        if self.tx.storage_mass() != contextual_storage_mass(&self.tx, &self.entries) {
            return fail("the transaction does not commit its exact storage mass".into());
        }
        let populated = PopulatedTransaction::new(&self.tx, self.entries.clone());
        CovenantsContext::from_tx(&populated).map_err(|e| IssueError::Verify(format!("covenant context: {e:?}")))?;
        let mut units = vec![];
        if self.signed {
            for i in 0..self.tx.inputs.len() {
                let u = execute_input(&self.tx, &self.entries, i).map_err(IssueError::Verify)?;
                let budget = self.tx.inputs[i].compute_commit.compute_budget().unwrap_or(0);
                let allowed = FREE_UNITS + u64::from(budget) * UNITS_PER_BUDGET;
                if u > allowed {
                    return fail(format!("input {i} uses {u} script units, budget allows {allowed}"));
                }
                units.push(u);
            }
        }
        let mut measured = self.tx.clone();
        if !self.signed {
            for i in measured.inputs.iter_mut() {
                i.signature_script = push_data(&[0u8; 65]);
            }
        }
        let (compute, storage, size, fee_mass, limit_mass) = masses(&measured, &self.entries);
        if limit_mass > MAX_STANDARD_MASS {
            return fail(format!("transaction mass {limit_mass} exceeds the standard limit {MAX_STANDARD_MASS} (storage mass grows with more or smaller outputs: raise the carrier or issue fewer outputs)"));
        }
        let min_fee = crate::tx::relay_fee_for_mass(fee_mass, self.spec.fee_rate);
        if self.fee < min_fee.min(self.spec.fee.unwrap_or(u64::MAX)) {
            return fail(format!("fee {} below minimum {min_fee}", self.fee));
        }
        Ok(VerifyReport { scripts_executed: self.signed, script_units: units, compute_mass: compute, storage_mass: storage, size })
    }

    /// The transaction in the node's RPC JSON shape (`RpcTransaction`, camelCase, hex fields).
    pub fn rpc_transaction_json(&self) -> Value {
        rpc_transaction_json(&self.tx)
    }

    /// `supply.json`: what was issued and where.
    pub fn supply_json(&self) -> Value {
        let txid = self.txid();
        let allocations: Vec<Value> = self
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "outpoint": format!("{txid}:{i}"),
                    "index": i,
                    "amount": s.amount.to_string(),
                    "owner": hex(&s.owner),
                    "owner_scheme": s.owner_scheme,
                    "borrow_scheme": s.borrow_scheme,
                    "borrow_guard": hex(&s.borrow_guard),
                    "carrier_sompi": self.spec.carrier,
                })
            })
            .collect();
        json!({
            "format": "kob-token-supply/1",
            "name": self.spec.name,
            "ticker": self.spec.ticker,
            "decimals": self.spec.decimals,
            "network": self.spec.network,
            "fixed_supply": true,
            "total_supply": self.spec.supply.to_string(),
            "supply_rule": "supply = sum of the genesis outputs; the program has no mint or burn entry",
            "covenant_id": hex(self.covenant_id.as_bytes().as_slice()),
            "genesis_txid": txid.to_string(),
            "genesis_outpoints": self.token_outpoints().iter().map(|o| format!("{}:{}", o.transaction_id, o.index)).collect::<Vec<_>>(),
            "allocations": allocations,
            "program": {
                "artifact": self.program.name,
                "artifact_sha256": hex(&self.program.artifact_sha256),
                "template_hash": hex(&self.program.template_hash),
                "template_prefix_len": self.program.prefix.len(),
                "template_suffix_len": self.program.suffix.len(),
                "max_token_inputs": self.program.max_token_inputs,
                "max_token_outputs": self.program.max_token_outputs,
                "owner_schemes_enabled": OWNER_SCHEMES_ENABLED,
                "proposal_p2": false,
            },
            "borrow": if self.states.iter().any(|s| s.borrow_scheme != 0) { "enabled-on-some-issued-outputs" } else { "disabled" },
            "extension_commitment": hex(&self.spec.extension_commitment),
            "extension_class": EXTENSION_CLASS,
            "carrier_sompi": self.spec.carrier,
            "fee_sompi": self.fee,
            "compute_mass": self.compute_mass,
            "storage_mass": self.storage_mass,
            "tx_size": self.size,
            "signed": self.signed,
        })
    }

    /// Off-chain token metadata (KCC-23-style JSON object). The field names are KOB-defined and
    /// provisional (KCC-23 leaves them to ecosystem convention). When published on chain under KCC-23,
    /// the payload is `0x00` followed by these UTF-8 bytes; nothing in KOB depends on that.
    pub fn metadata_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("standard".into(), json!("KCC-20"));
        m.insert("name".into(), json!(self.spec.name));
        m.insert("symbol".into(), json!(self.spec.ticker));
        m.insert("decimals".into(), json!(self.spec.decimals));
        m.insert("totalSupply".into(), json!(self.spec.supply.to_string()));
        m.insert("fixedSupply".into(), json!(true));
        if let Some(d) = &self.spec.description {
            m.insert("description".into(), json!(d));
        }
        if let Some(i) = &self.spec.icon {
            m.insert("icon".into(), json!(i));
        }
        if let Some(w) = &self.spec.website {
            m.insert("website".into(), json!(w));
        }
        Value::Object(m)
    }

    /// KCC-23 payload bytes for [`GenesisPlan::metadata_json`]: codec prefix `0x00`, then the compact UTF-8 JSON.
    pub fn metadata_payload(&self) -> Vec<u8> {
        let mut p = vec![0x00];
        p.extend(serde_json::to_vec(&self.metadata_json()).expect("json"));
        p
    }

    /// Token registry entry in the shape of `registry/tokens.schema.json` (`crate::registry::Token`): status
    /// `pending-review`, `verified` false, program = the pinned `kcc20-ref-8x8` template. Supply and borrow facts
    /// live in `supply.json`.
    pub fn registry_entry(&self) -> Value {
        let mut display = serde_json::Map::new();
        if let Some(d) = &self.spec.description {
            display.insert("description".into(), json!(d));
        }
        if let Some(w) = &self.spec.website {
            display.insert("website".into(), json!(w));
        }
        if let Some(i) = &self.spec.icon {
            display.insert("icon".into(), json!(i));
        }
        display.insert("kcc23".into(), self.metadata_json());
        json!({
            "ticker": self.spec.ticker,
            "name": self.spec.name,
            "family": "kcc20",
            "covenant_id": hex(self.covenant_id.as_bytes().as_slice()),
            "template_id": REGISTRY_TEMPLATE_ID,
            "extension_commitment": hex(&self.spec.extension_commitment),
            "extension_class": EXTENSION_CLASS,
            "decimals": self.spec.decimals,
            "max_token_inputs": self.program.max_token_inputs,
            "max_token_outputs": self.program.max_token_outputs,
            "status": "pending-review",
            "verified": false,
            "display": Value::Object(display),
        })
    }
}

/// Result of [`GenesisPlan::verify`].
#[derive(Debug, Clone)]
pub struct VerifyReport {
    /// Whether the funding inputs were executed in the script engine (signed plans only).
    pub scripts_executed: bool,
    /// Script units used per funding input.
    pub script_units: Vec<u64>,
    /// Compute mass.
    pub compute_mass: u64,
    /// Storage mass.
    pub storage_mass: u64,
    /// Estimated serialized size.
    pub size: u64,
}

/// `RpcTransaction` JSON (camelCase) of a transaction, as accepted by `submitTransaction` over wRPC.
pub fn rpc_transaction_json(tx: &Transaction) -> Value {
    let inputs: Vec<Value> = tx
        .inputs
        .iter()
        .map(|i| {
            json!({
                "previousOutpoint": { "transactionId": i.previous_outpoint.transaction_id.to_string(), "index": i.previous_outpoint.index },
                "signatureScript": hex(&i.signature_script),
                "sequence": i.sequence,
                "sigOpCount": i.compute_commit.sig_op_count().unwrap_or(0),
                "computeBudget": i.compute_commit.compute_budget().unwrap_or(0),
                "verboseData": Value::Null,
            })
        })
        .collect();
    let outputs: Vec<Value> = tx
        .outputs
        .iter()
        .map(|o| {
            let spk = format!("{:04x}{}", o.script_public_key.version(), hex(o.script_public_key.script()));
            json!({
                "value": o.value,
                "scriptPublicKey": spk,
                "verboseData": Value::Null,
                "covenant": o.covenant.map(|c| json!({ "authorizingInput": c.authorizing_input, "covenantId": c.covenant_id.to_string() })),
            })
        })
        .collect();
    json!({
        "version": tx.version,
        "inputs": inputs,
        "outputs": outputs,
        "lockTime": tx.lock_time,
        "subnetworkId": hex(tx.subnetwork_id.as_ref()),
        "gas": tx.gas,
        "payload": hex(&tx.payload),
        "storageMass": tx.storage_mass(),
        "mass": tx.storage_mass(),
        "verboseData": Value::Null,
    })
}

// ------------------------------------------------------------------------------------------------
// Program self-test

/// Execute a real transfer in the script engine that moves tokens into a covenant-owned (scheme
/// 0x04) state with borrowing disabled. Fails if the program does not accept owner scheme 0x04.
pub fn verify_scheme4_enabled(program: &Program) -> Result<()> {
    let secp = Secp256k1::new();
    let kp = Keypair::from_secret_key(&secp, &SecretKey::from_slice(&[0x22u8; 32]).expect("key"));
    let pk = kp.x_only_public_key().0.serialize();
    let cov = Hash::from_bytes([0x44; 32]);
    let ext = EXTENSION_FIXED_SUPPLY;
    let held = TokenState {
        amount: 1_000,
        owner: pk,
        owner_scheme: SCHEME_P2PK_SCHNORR,
        borrow_scheme: 0,
        borrow_guard: [0u8; 32],
        extension_commitment: ext,
    };
    let next = TokenState { owner: [0xc4; 32], owner_scheme: SCHEME_COVENANT_ID, ..held.clone() };
    let redeem = program.redeem(&held);
    let entries = vec![UtxoEntry::new(1_000_000, pay_to_script_hash_script(&redeem), 0, false, Some(cov))];
    let out = TransactionOutput {
        value: 900_000,
        script_public_key: program.spk(&next),
        covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: cov }),
    };
    let input = TransactionInput::new_with_compute_budget(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([9u8; 32]), index: 0 },
        vec![],
        0,
        200,
    );
    let mut tx = Transaction::new(1, vec![input], vec![out], 0, Default::default(), 0, vec![]);
    let mut sig = kp.sign_schnorr(schnorr_sighash(&tx, &entries, 0)).as_ref().to_vec();
    sig.push(0x01);
    let witness = [vec![0x00], sig].concat();
    tx.inputs[0].signature_script = program.leader_sig_script(&redeem, &[next], witness)?;
    execute_input(&tx, &entries, 0)
        .map(|_| ())
        .map_err(|e| IssueError::Verify(format!("the token program did not accept a transfer into an owner-scheme-0x04 state: {e}")))
}

// ------------------------------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn funding(amount: u64, pk: [u8; 32]) -> FundingUtxo {
        FundingUtxo {
            outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([7u8; 32]), index: 1 },
            amount,
            owner_pubkey: pk,
        }
    }
    fn key() -> (SecretKey, [u8; 32]) {
        let sk = SecretKey::from_slice(&[0x33u8; 32]).unwrap();
        let pk = Keypair::from_secret_key(&Secp256k1::new(), &sk).x_only_public_key().0.serialize();
        (sk, pk)
    }
    fn spec(holders: Vec<Holder>, supply: u64) -> IssueSpec {
        let (_, pk) = key();
        IssueSpec::new("Test Token", "TEST", 8, supply, holders, vec![funding(500 * 100_000_000, pk)])
    }

    #[test]
    fn program_pins_template_and_state_codec() {
        let p = Program::kcc20_8x8().unwrap();
        assert_eq!((p.max_token_inputs, p.max_token_outputs), (8, 8));
        assert_eq!(p.template_hash, silverscript_abi::template_hash(&p.prefix, &p.suffix));
        assert_eq!(
            TokenState {
                amount: 1,
                owner: [1; 32],
                owner_scheme: 0,
                borrow_scheme: 0,
                borrow_guard: [0; 32],
                extension_commitment: [0; 32]
            }
            .encode()
            .len(),
            112
        );
    }

    #[test]
    fn scheme4_selftest_passes_for_the_8x8_program() {
        verify_scheme4_enabled(&Program::kcc20_8x8().unwrap()).unwrap();
    }

    #[test]
    fn genesis_builds_signs_and_verifies() {
        let (sk, pk) = key();
        let holders = vec![Holder::new(pk, 0, 600), Holder::new([0xc4; 32], 4, 400)];
        let mut plan = build_genesis(&spec(holders, 1000)).unwrap();
        assert_eq!(plan.states.len(), 2);
        assert_eq!(plan.tx.outputs.len(), 3, "two token outputs and change");
        let unsigned = plan.verify().unwrap();
        assert!(!unsigned.scripts_executed);
        let txid = plan.txid();
        plan.sign(&sk).unwrap();
        assert_eq!(plan.txid(), txid, "txid does not depend on signature scripts");
        let r = plan.verify().unwrap();
        assert!(r.scripts_executed && r.script_units[0] > 0);
        // covenant id derivation matches consensus
        let g = covenant_id(plan.tx.inputs[0].previous_outpoint, plan.tx.outputs[..2].iter().enumerate().map(|(i, o)| (i as u32, o)));
        assert_eq!(g, plan.covenant_id);
        assert_eq!(plan.tx.version, 1);
        assert!(plan.tx.inputs[0].compute_commit.compute_budget().unwrap() > 0);
    }

    #[test]
    fn supply_mismatch_is_rejected() {
        let (_, pk) = key();
        let e = build_genesis(&spec(vec![Holder::new(pk, 0, 999)], 1000)).unwrap_err();
        assert!(matches!(e, IssueError::SupplyMismatch { sum: 999, declared: 1000 }), "{e}");
    }

    #[test]
    fn covenant_held_states_cannot_borrow() {
        let mut h = Holder::new([0xc4; 32], 4, 1000);
        h.borrow_scheme = 1;
        h.borrow_guard = [1; 32];
        let mut s = spec(vec![h], 1000);
        s.allow_borrow = true;
        assert!(matches!(s.validate().unwrap_err(), IssueError::Borrow(_)));
    }

    #[test]
    fn borrow_needs_the_explicit_flag() {
        let (_, pk) = key();
        let mut h = Holder::new(pk, 0, 1000);
        h.borrow_scheme = 1;
        h.borrow_guard = [1; 32];
        let mut s = spec(vec![h], 1000);
        assert!(matches!(s.validate().unwrap_err(), IssueError::Borrow(_)));
        s.allow_borrow = true;
        s.validate().unwrap();
        let plan = build_genesis(&s).unwrap();
        assert_eq!(plan.supply_json()["borrow"], "enabled-on-some-issued-outputs");
    }

    #[test]
    fn more_than_eight_outputs_is_flagged() {
        let (_, pk) = key();
        let holders: Vec<Holder> = (0..9).map(|i| Holder::new([i as u8 + 1; 32], 0, 100)).collect();
        let _ = pk;
        let plan = build_genesis(&spec(holders, 900)).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("9 genesis outputs exceed the 8-output")), "{:?}", plan.warnings);
        let too_many: Vec<Holder> = (0..65).map(|i| Holder::new([i as u8 + 1; 32], 0, 1)).collect();
        assert!(matches!(spec(too_many, 65).validate().unwrap_err(), IssueError::Invalid(_)));
    }

    #[test]
    fn rules_on_names_amounts_and_funds() {
        let (_, pk) = key();
        let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
        s.ticker = "bad".into();
        assert!(s.validate().is_err());
        let mut s = spec(vec![Holder::new(pk, 0, 0)], 0);
        s.supply = 0;
        assert!(s.validate().is_err());
        let mut s = spec(vec![Holder::new(pk, 9, 10)], 10);
        s.ticker = "OK".into();
        assert!(s.validate().is_err(), "unknown owner scheme");
        let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
        s.funding[0].amount = 1_000;
        assert!(matches!(build_genesis(&s).unwrap_err(), IssueError::InsufficientFunds { .. }));
        let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
        s.fee = Some(1);
        assert!(matches!(build_genesis(&s).unwrap_err(), IssueError::FeeTooLow { .. }));
    }

    /// A wallet that holds only a little more than the carrier gets a plan that verifies (storage mass grows as the change
    /// shrinks; the relay-floor fee does not depend on it, and a plan whose funds cannot pay it is an InsufficientFunds error).
    #[test]
    fn fee_is_settled_at_the_final_change_value() {
        let (sk, pk) = key();
        let mut fixed_point_needed = 0;
        for k in 0..60u64 {
            let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
            s.funding = vec![funding(10 * 100_000_000 + k * 3_000_000, pk)];
            match build_genesis(&s) {
                Ok(mut plan) => {
                    plan.verify().unwrap_or_else(|e| panic!("unsigned, k={k}: {e}"));
                    plan.sign(&sk).unwrap();
                    plan.verify().unwrap_or_else(|e| panic!("signed, k={k}: {e}"));
                    fixed_point_needed += usize::from(plan.change.is_some() && plan.storage_mass > plan.compute_mass);
                }
                Err(IssueError::InsufficientFunds { need, have }) => assert!(need > have, "k={k}"),
                Err(e) => panic!("k={k}: {e}"),
            }
        }
        assert!(fixed_point_needed > 0, "the sweep must reach the storage-mass-dominated range");
        // two 6 KAS inputs for a 10 KAS carrier: storage mass decides the fee
        let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
        let second = FundingUtxo {
            outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([7u8; 32]), index: 2 },
            ..funding(600_000_000, pk)
        };
        s.funding = vec![funding(600_000_000, pk), second];
        let mut plan = build_genesis(&s).unwrap();
        plan.sign(&sk).unwrap();
        plan.verify().unwrap();
    }

    #[test]
    fn funding_equal_to_the_carriers_leaves_nothing_for_the_fee() {
        let (_, pk) = key();
        let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
        s.funding = vec![funding(10 * 100_000_000, pk)];
        match build_genesis(&s).unwrap_err() {
            IssueError::InsufficientFunds { need, have } => assert!(need > have && have == 10 * 100_000_000),
            e => panic!("{e}"),
        }
    }

    #[test]
    fn wrong_signing_key_is_refused() {
        let (_, pk) = key();
        let mut plan = build_genesis(&spec(vec![Holder::new(pk, 0, 10)], 10)).unwrap();
        let other = SecretKey::from_slice(&[0x55u8; 32]).unwrap();
        assert!(plan.sign(&other).is_err());
    }

    #[test]
    fn documents_have_the_expected_shape() {
        let (_, pk) = key();
        let mut s = spec(vec![Holder::new(pk, 0, 10)], 10);
        s.description = Some("d".into());
        let plan = build_genesis(&s).unwrap();
        let r = plan.registry_entry();
        for k in [
            "ticker",
            "name",
            "family",
            "covenant_id",
            "template_id",
            "max_token_inputs",
            "max_token_outputs",
            "extension_commitment",
            "extension_class",
            "decimals",
            "verified",
            "display",
            "status",
        ] {
            assert!(r.get(k).is_some(), "registry entry misses {k}");
        }
        assert_eq!(r["family"], "kcc20");
        assert_eq!(r["status"], "pending-review");
        assert_eq!(r["max_token_inputs"], 8);
        let sup = plan.supply_json();
        assert_eq!(sup["total_supply"], "10");
        assert_eq!(sup["genesis_outpoints"].as_array().unwrap().len(), 1);
        assert_eq!(plan.metadata_payload()[0], 0x00);
        let rpc = plan.rpc_transaction_json();
        assert_eq!(rpc["version"], 1);
        assert_eq!(rpc["outputs"][0]["covenant"]["covenantId"], hex(plan.covenant_id.as_bytes().as_slice()));
        assert_eq!(rpc["inputs"][0]["computeBudget"], plan.compute_budget);
        assert_eq!(rpc["storageMass"], plan.storage_mass);
        assert!(rpc["outputs"][0]["scriptPublicKey"].as_str().unwrap().starts_with("0000aa20"), "P2SH spk");
    }
}
