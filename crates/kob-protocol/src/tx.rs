//! Transaction model, the fee pass and the wallet-signing split.
//!
//! Every builder produces a [`BuiltTx`]:
//!
//! 1. an **unsigned** version-1 transaction (kaspa-wasm "safe JSON" layout, every input carrying
//!    its UTXO entry, signature scripts empty, compute budgets from the budget table, exact
//!    storage-mass commitment, final fee and change);
//! 2. one [`SigPlan`] per input describing how its signature script is assembled;
//! 3. the [`SignRequest`]s: which inputs need a Schnorr signature by which key, with the SIGHASH_ALL
//!    digest each wallet must sign.
//!
//! Wallets (KasWare, Kaspire, Kastle) sign the unsigned transaction and return **signatures only**.
//! [`finalize`] checks each signature against its digest and key, assembles every signature script
//! from its plan and returns the signed transaction, optionally re-measuring the exact compute
//! budget of every input in the script engine (a v1 sighash does not commit to the budget or to the
//! storage mass, so neither invalidates the signatures).

use std::collections::{BTreeMap, BTreeSet};

use kaspa_consensus_core::config::params::{Params, MAINNET_PARAMS};
use kaspa_consensus_core::constants::MAX_SOMPI;
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::mass::{transaction_estimated_serialized_size, BlockMassLimits, MassCalculator};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use serde::{Deserialize, Serialize};
use silverscript_abi::ArtifactValue;

use crate::artifacts::{template, token_template, try_template, try_token_template, TemplateId};
use crate::error::{invalid, Error, Result};
use crate::family::Family;
use crate::json::{from_hex, to_hex};
use crate::registry::KRON_MAX_OUTPUT_AMOUNT;
use crate::script::{entry_sigscript, kcc20_state_value, kron_token_sigscript, leader_witness, p2pk_spk, push_data, SIGHASH_ALL};
use crate::state::{Kcc20State, KronState, TokenState};

/// Minimum relay fee rate, sompi per gram (rusty-kaspa v2.1.0 `DEFAULT_MINIMUM_RELAY_TRANSACTION_FEE`,
/// 100,000 sompi per kilogram).
pub const MIN_FEE_RATE: u64 = 100;
/// Mainnet mass parameters (rusty-kaspa v2.1.0).
pub const MASS_PER_TX_BYTE: u64 = 1;
pub const MASS_PER_SPK_BYTE: u64 = 10;
pub const STORAGE_MASS_PARAMETER: u64 = 1_000_000_000_000;
/// Block mass limits (compute, storage, transient).
pub const BLOCK_COMPUTE_LIMIT: u64 = 500_000;
pub const BLOCK_STORAGE_LIMIT: u64 = 500_000;
pub const BLOCK_TRANSIENT_LIMIT: u64 = 1_000_000;
/// Smallest output value (sompi, 0.02 KAS) whose KIP-9 storage mass alone (`storage_mass_parameter / value`) fits the
/// block storage limit. A payout or carrier below it makes the transaction unminable unless the inputs' own term offsets
/// it (rarely: only tiny inputs do), so order terms and builders refuse it as dust (the mempool has no separate dust
/// threshold).
pub const DUST_OUTPUT_MIN: u64 = 2_000_000;
/// Input sequence that keeps lock time enforced (non-final) with no relative lock.
pub const SEQUENCE_NONFINAL: u64 = 0;

// ---------------------------------------------------------------- request-side types

/// An unspent output, as supplied by the indexer or wallet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Utxo {
    #[serde(with = "crate::json::field")]
    pub transaction_id: [u8; 32],
    pub index: u32,
    #[serde(with = "crate::json::field")]
    pub amount: u64,
    #[serde(with = "crate::json::field", default)]
    pub block_daa_score: u64,
    #[serde(with = "crate::json::field", default)]
    pub covenant_id: Option<[u8; 32]>,
}

impl Utxo {
    pub fn outpoint(&self) -> TransactionOutpoint {
        TransactionOutpoint { transaction_id: TransactionId::from_bytes(self.transaction_id), index: self.index }
    }
    pub(crate) fn entry(&self, spk: ScriptPublicKey) -> UtxoEntry {
        UtxoEntry::new(self.amount, spk, self.block_daa_score, false, self.covenant_id.map(Hash::from_bytes))
    }
}

/// A P2PK-owned KAS UTXO and its (x-only) key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyUtxo {
    #[serde(flatten)]
    pub utxo: Utxo,
    #[serde(with = "crate::json::field")]
    pub pubkey: [u8; 32],
}

/// A token UTXO (either family) with its decoded state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUtxo {
    #[serde(flatten)]
    pub utxo: Utxo,
    pub state: TokenState,
}

/// An order or receipt UTXO with its decoded state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderUtxo<S> {
    #[serde(flatten)]
    pub utxo: Utxo,
    pub state: S,
}

/// Fee options shared by every request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeOptions {
    /// Sompi per gram (at least [`MIN_FEE_RATE`]; default 100).
    #[serde(with = "crate::json::field", default)]
    pub fee_rate: Option<u64>,
    /// [`FeeMode::Relay`] (default: the node's relay floor) or [`FeeMode::Priority`] (storage-inclusive).
    #[serde(default)]
    pub fee_mode: FeeMode,
}

impl FeeOptions {
    /// `rate` sompi per gram in the default [`FeeMode::Relay`].
    pub fn rate(rate: u64) -> Self {
        FeeOptions { fee_rate: Some(rate), fee_mode: FeeMode::Relay }
    }
}

// ---------------------------------------------------------------- signature plans

/// An entry argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Arg {
    /// `int` argument.
    Int(#[serde(with = "crate::json::field")] i64),
    /// `byte[N]`, `byte[]` or `pubkey` argument.
    Bytes(#[serde(with = "crate::json::field")] Vec<u8>),
    /// `sig` argument: a SIGHASH_ALL Schnorr signature by this key.
    Sig(#[serde(with = "crate::json::field")] [u8; 32]),
}

/// How a token input is authorised by its owner.
///
/// KCC-20: `CovenantId` (owner scheme 0x04) needs the owning covenant spent in the same transaction;
/// `P2pk` (scheme 0x00) carries a SIGHASH_ALL Schnorr signature by the key inside the token input.
/// KRON has no signature in the token input: `CovenantId` (`id_type` 2) and `P2pk` (`id_type` 3, address
/// presence) both name another input of the transaction, resolved when the transaction is sealed: the
/// input carrying the owner covenant id, respectively a P2PK input of the owner key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Witness {
    /// The owning covenant is spent in the same transaction.
    CovenantId,
    /// The owner key: a SIGHASH_ALL Schnorr signature (KCC-20) or a P2PK input of the key (KRON).
    P2pk(#[serde(with = "crate::json::field")] [u8; 32]),
}

/// How one input's signature script is assembled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SigPlan {
    /// P2PK: `push(sig65)`.
    P2pk {
        #[serde(with = "crate::json::field")]
        pubkey: [u8; 32],
    },
    /// Template entry call.
    #[serde(rename_all = "camelCase")]
    Entry {
        template: TemplateId,
        #[serde(with = "crate::json::field")]
        state: Vec<u8>,
        entry: String,
        args: Vec<Arg>,
    },
    /// KCC-20 leader (`transfer`): authorises every token output of the transaction.
    #[serde(rename_all = "camelCase")]
    TokenLeader { template: TemplateId, state: Kcc20State, next_states: Vec<Kcc20State>, witness: Witness },
    /// KCC-20 delegator (`transfer_delegator`).
    #[serde(rename_all = "camelCase")]
    TokenDelegator { template: TemplateId, state: Kcc20State, witness: Witness },
    /// KRON token input: no leader. Every token input of the token carries the next states of the
    /// whole covenant group (`next_states`, in output order) and one witness byte per token input
    /// of the token (`witnesses`, in input order), each the index of the input that authorises it.
    /// The token needs no signature of its own: address presence is a P2PK input of the owner
    /// elsewhere in the transaction.
    #[serde(rename_all = "camelCase")]
    KronToken {
        template: TemplateId,
        state: KronState,
        next_states: Vec<KronState>,
        #[serde(with = "crate::json::field")]
        witnesses: Vec<u8>,
    },
    /// Entry call of a KOB router intent (`crate::router`): the actor by name and its state span.
    #[serde(rename_all = "camelCase")]
    Router {
        actor: String,
        #[serde(with = "crate::json::field")]
        state: Vec<u8>,
        entry: String,
        args: Vec<Arg>,
    },
    /// Entry call of a RETIRED order template (`crate::retired`, spend-only: the maker's cancel of an order placed under an
    /// older template): the retired template by hash and its state span.
    #[serde(rename_all = "camelCase")]
    Retired {
        #[serde(with = "crate::json::field")]
        template_hash: [u8; 32],
        #[serde(with = "crate::json::field")]
        state: Vec<u8>,
        entry: String,
        args: Vec<Arg>,
    },
}

fn router_actor(name: &str) -> Result<&'static crate::router::Actor> {
    crate::router::Actor::by_name(name).ok_or_else(|| Error::Invalid(format!("{name} is not a router actor")))
}

impl SigPlan {
    /// Key that must sign this input, if any.
    pub fn signer(&self) -> Option<[u8; 32]> {
        match self {
            SigPlan::P2pk { pubkey } => Some(*pubkey),
            SigPlan::Entry { args, .. } | SigPlan::Router { args, .. } | SigPlan::Retired { args, .. } => {
                args.iter().find_map(|a| if let Arg::Sig(k) = a { Some(*k) } else { None })
            }
            SigPlan::TokenLeader { witness, .. } | SigPlan::TokenDelegator { witness, .. } => match witness {
                Witness::P2pk(k) => Some(*k),
                Witness::CovenantId => None,
            },
            SigPlan::KronToken { .. } => None,
        }
    }

    /// Redeem script of a P2SH input (None for P2PK).
    pub fn redeem(&self) -> Option<Vec<u8>> {
        match self {
            SigPlan::P2pk { .. } => None,
            SigPlan::Entry { template: t, state, .. } => Some(template(*t).redeem(state)),
            SigPlan::TokenLeader { template: t, state, .. } | SigPlan::TokenDelegator { template: t, state, .. } => {
                Some(state.redeem_with(template(*t)))
            }
            SigPlan::KronToken { template: t, state, .. } => Some(state.redeem_with(token_template(*t))),
            SigPlan::Router { actor, state, .. } => router_actor(actor).ok()?.template().redeem(state).ok(),
            SigPlan::Retired { template_hash, state, .. } => {
                let r = crate::retired::by_hash(template_hash)?;
                (state.len() == r.template.state_len).then(|| r.template.redeem(state))
            }
        }
    }

    /// The plan names embedded templates of the right kind (a hostile `BuiltTx` could name a raw token program as an
    /// artifact template, which the accessors below would panic on).
    pub fn check(&self) -> Result<()> {
        let artifact = |t: TemplateId| {
            try_template(t).map(|_| ()).ok_or_else(|| Error::Invalid(format!("{} is not an artifact-backed template", t.name())))
        };
        match self {
            SigPlan::P2pk { .. } => Ok(()),
            SigPlan::Entry { template: t, .. }
            | SigPlan::TokenLeader { template: t, .. }
            | SigPlan::TokenDelegator { template: t, .. } => artifact(*t),
            SigPlan::KronToken { template: t, .. } => {
                try_token_template(*t).map(|_| ()).ok_or_else(|| Error::Invalid(format!("{} is not a token program", t.name())))
            }
            SigPlan::Router { actor, state, entry, .. } => {
                let a = router_actor(actor)?;
                if entry != a.entry && entry != "cancel" && entry != "expire" {
                    return invalid(format!("{actor} has no entry {entry}"));
                }
                a.template().redeem(state).map(|_| ())
            }
            SigPlan::Retired { template_hash, state, entry, .. } => {
                let r = crate::retired::by_hash(template_hash).ok_or_else(|| {
                    Error::Invalid(format!("{} is not a retired template this build can spend", to_hex(template_hash)))
                })?;
                if entry != "cancel" {
                    return invalid("a retired template is spend-only: the maker's cancel");
                }
                if state.len() != r.template.state_len {
                    return invalid("the state span does not fit the retired template");
                }
                Ok(())
            }
        }
    }

    /// Script public key of the UTXO this plan spends.
    pub fn spk(&self) -> ScriptPublicKey {
        match self {
            SigPlan::P2pk { pubkey } => p2pk_spk(pubkey),
            SigPlan::Entry { template: t, state, .. } => template(*t).spk(state),
            SigPlan::TokenLeader { template: t, state, .. } | SigPlan::TokenDelegator { template: t, state, .. } => {
                state.spk_with(template(*t))
            }
            SigPlan::KronToken { template: t, state, .. } => state.spk_with(token_template(*t)),
            // an unknown actor or a malformed state gives an unspendable empty script (`check` refuses such a plan)
            SigPlan::Router { .. } | SigPlan::Retired { .. } => self
                .redeem()
                .map(|r| kaspa_txscript::pay_to_script_hash_script(&r))
                .unwrap_or_else(|| ScriptPublicKey::new(0, Default::default())),
        }
    }

    /// The signature script, given the input's signature (65 bytes incl. sighash type) if it
    /// needs one.
    pub fn sigscript(&self, sig: Option<&[u8]>) -> Result<Vec<u8>> {
        let need = |what: &str| -> Result<&[u8]> { sig.ok_or_else(|| Error::Invalid(format!("{what}: signature missing"))) };
        match self {
            SigPlan::P2pk { .. } => Ok(push_data(need("p2pk")?)),
            SigPlan::Entry { template: t, state, entry, args } => {
                let tpl = template(*t);
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(match a {
                        Arg::Int(i) => ArtifactValue::Int(*i),
                        Arg::Bytes(b) => ArtifactValue::Bytes(b.clone()),
                        Arg::Sig(_) => ArtifactValue::Bytes(need(entry)?.to_vec()),
                    });
                }
                entry_sigscript(tpl, &tpl.redeem(state), entry, &vals).map_err(Error::Invalid)
            }
            SigPlan::TokenLeader { template: t, state, next_states, witness } => {
                let tpl = template(*t);
                let w = match witness {
                    Witness::CovenantId => leader_witness(None),
                    Witness::P2pk(_) => leader_witness(Some(need("kcc20 leader")?)),
                };
                let next = ArtifactValue::Array(next_states.iter().map(kcc20_state_value).collect());
                entry_sigscript(tpl, &state.redeem_with(tpl), "transfer", &[next, ArtifactValue::Bytes(w)]).map_err(Error::Invalid)
            }
            SigPlan::TokenDelegator { template: t, state, witness } => {
                let tpl = template(*t);
                let w = match witness {
                    Witness::CovenantId => vec![],
                    Witness::P2pk(_) => need("kcc20 delegator")?.to_vec(),
                };
                entry_sigscript(tpl, &state.redeem_with(tpl), "transfer_delegator", &[ArtifactValue::Bytes(w)]).map_err(Error::Invalid)
            }
            SigPlan::KronToken { template: t, state, next_states, witnesses } => {
                Ok(kron_token_sigscript(&state.redeem_with(token_template(*t)), next_states, &[], witnesses))
            }
            SigPlan::Router { actor, state, entry, args } => {
                let a = router_actor(actor)?;
                let mut vals = Vec::with_capacity(args.len());
                for x in args {
                    vals.push(match x {
                        Arg::Int(i) => ArtifactValue::Int(*i),
                        Arg::Bytes(b) => ArtifactValue::Bytes(b.clone()),
                        Arg::Sig(_) => ArtifactValue::Bytes(need(entry)?.to_vec()),
                    });
                }
                crate::router::entry_sigscript(a, state, entry, &vals)
            }
            SigPlan::Retired { template_hash, state, entry, args } => {
                let r = crate::retired::by_hash(template_hash)
                    .ok_or_else(|| Error::Invalid(format!("{} is not a retired template", to_hex(template_hash))))?;
                if state.len() != r.template.state_len {
                    return invalid("the state span does not fit the retired template");
                }
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(match a {
                        Arg::Int(i) => ArtifactValue::Int(*i),
                        Arg::Bytes(b) => ArtifactValue::Bytes(b.clone()),
                        Arg::Sig(_) => ArtifactValue::Bytes(need(entry)?.to_vec()),
                    });
                }
                entry_sigscript(&r.template, &r.template.redeem(state), entry, &vals).map_err(Error::Invalid)
            }
        }
    }
}

/// One signature a wallet must produce.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignRequest {
    pub input_index: usize,
    #[serde(with = "crate::json::field")]
    pub pubkey: [u8; 32],
    pub sighash_type: u8,
    /// The Schnorr SIGHASH_ALL digest of this input.
    #[serde(with = "crate::json::field")]
    pub sighash: [u8; 32],
    /// Redeem script of a P2SH input (what Kastle needs in its `scripts` option; wallets that
    /// display scripts show it). None for P2PK inputs.
    #[serde(with = "crate::json::field")]
    pub redeem_script: Option<Vec<u8>>,
}

/// A signature returned by a wallet: 64 bytes, 65 bytes (with the 0x01 type byte), or the
/// 66-byte `push(sig65)` script KasWare returns for P2SH inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSignature {
    pub input_index: usize,
    #[serde(with = "crate::json::field")]
    pub signature: Vec<u8>,
}

// ---------------------------------------------------------------- kaspa-wasm safe JSON

/// UTXO entry of an input (kaspa-wasm `ISerializableUtxoEntry`, safe JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtxoJson {
    pub address: Option<String>,
    #[serde(with = "crate::json::field")]
    pub amount: u64,
    pub script_public_key: String,
    #[serde(with = "crate::json::field")]
    pub block_daa_score: u64,
    pub is_coinbase: bool,
    #[serde(with = "crate::json::field")]
    pub covenant_id: Option<[u8; 32]>,
}

/// Transaction input (kaspa-wasm safe JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxInputJson {
    #[serde(with = "crate::json::field")]
    pub transaction_id: [u8; 32],
    pub index: u32,
    #[serde(with = "crate::json::field")]
    pub sequence: u64,
    pub sig_op_count: u8,
    pub compute_budget: u16,
    #[serde(with = "crate::json::field")]
    pub signature_script: Vec<u8>,
    pub utxo: UtxoJson,
}

/// Covenant binding of an output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CovenantJson {
    pub authorizing_input: u16,
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
}

/// Transaction output (kaspa-wasm safe JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxOutputJson {
    #[serde(with = "crate::json::field")]
    pub value: u64,
    pub script_public_key: String,
    pub covenant: Option<CovenantJson>,
}

/// A transaction in kaspa-wasm's "safe JSON" layout (`Transaction.deserializeFromSafeJSON`):
/// 64-bit values as strings, bytes as hex, every input carrying its UTXO entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxJson {
    #[serde(with = "crate::json::field")]
    pub id: [u8; 32],
    pub version: u16,
    pub inputs: Vec<TxInputJson>,
    pub outputs: Vec<TxOutputJson>,
    #[serde(with = "crate::json::field")]
    pub subnetwork_id: Vec<u8>,
    #[serde(with = "crate::json::field")]
    pub lock_time: u64,
    #[serde(with = "crate::json::field")]
    pub gas: u64,
    #[serde(with = "crate::json::field")]
    pub storage_mass: u64,
    #[serde(with = "crate::json::field")]
    pub payload: Vec<u8>,
}

/// Script public key as kaspa's human-readable form: `version (u16 BE hex) ‖ script hex`.
pub fn spk_to_string(spk: &ScriptPublicKey) -> String {
    format!("{}{}", to_hex(&spk.version().to_be_bytes()), to_hex(spk.script()))
}

/// Inverse of [`spk_to_string`].
pub fn spk_from_string(s: &str) -> Result<ScriptPublicKey> {
    let b = from_hex(s).map_err(Error::Invalid)?;
    if b.len() < 2 {
        return invalid("script public key shorter than its version");
    }
    Ok(ScriptPublicKey::new(u16::from_be_bytes([b[0], b[1]]), b[2..].to_vec().into()))
}

impl TxJson {
    /// Serialises a consensus transaction with its UTXO entries.
    pub fn from_tx(tx: &Transaction, entries: &[UtxoEntry]) -> TxJson {
        TxJson {
            id: tx.id().as_bytes(),
            version: tx.version,
            inputs: tx
                .inputs
                .iter()
                .zip(entries)
                .map(|(i, e)| TxInputJson {
                    transaction_id: i.previous_outpoint.transaction_id.as_bytes(),
                    index: i.previous_outpoint.index,
                    sequence: i.sequence,
                    sig_op_count: i.compute_commit.sig_op_count().unwrap_or(0),
                    compute_budget: i.compute_commit.compute_budget().unwrap_or(0),
                    signature_script: i.signature_script.clone(),
                    utxo: UtxoJson {
                        address: None,
                        amount: e.amount,
                        script_public_key: spk_to_string(&e.script_public_key),
                        block_daa_score: e.block_daa_score,
                        is_coinbase: e.is_coinbase,
                        covenant_id: e.covenant_id.map(|h| h.as_bytes()),
                    },
                })
                .collect(),
            outputs: tx
                .outputs
                .iter()
                .map(|o| TxOutputJson {
                    value: o.value,
                    script_public_key: spk_to_string(&o.script_public_key),
                    covenant: o
                        .covenant
                        .map(|c| CovenantJson { authorizing_input: c.authorizing_input, covenant_id: c.covenant_id.as_bytes() }),
                })
                .collect(),
            subnetwork_id: AsRef::<[u8]>::as_ref(&tx.subnetwork_id).to_vec(),
            lock_time: tx.lock_time,
            gas: tx.gas,
            storage_mass: tx.storage_mass(),
            payload: tx.payload.clone(),
        }
    }

    /// Rebuilds the consensus transaction and its UTXO entries.
    pub fn to_tx(&self) -> Result<(Transaction, Vec<UtxoEntry>)> {
        if self.version != 1 {
            return invalid(format!("only version-1 transactions are supported (got {})", self.version));
        }
        let mut inputs = Vec::with_capacity(self.inputs.len());
        let mut entries = Vec::with_capacity(self.inputs.len());
        for i in &self.inputs {
            inputs.push(TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes(i.transaction_id), index: i.index },
                i.signature_script.clone(),
                i.sequence,
                i.compute_budget,
            ));
            entries.push(UtxoEntry::new(
                i.utxo.amount,
                spk_from_string(&i.utxo.script_public_key)?,
                i.utxo.block_daa_score,
                i.utxo.is_coinbase,
                i.utxo.covenant_id.map(Hash::from_bytes),
            ));
        }
        let mut outputs = Vec::with_capacity(self.outputs.len());
        for o in &self.outputs {
            outputs.push(TransactionOutput {
                value: o.value,
                script_public_key: spk_from_string(&o.script_public_key)?,
                covenant: o
                    .covenant
                    .as_ref()
                    .map(|c| CovenantBinding { authorizing_input: c.authorizing_input, covenant_id: Hash::from_bytes(c.covenant_id) }),
            });
        }
        if self.subnetwork_id.as_slice() != AsRef::<[u8]>::as_ref(&SUBNETWORK_ID_NATIVE) {
            return invalid("only native-subnetwork transactions are supported");
        }
        let tx = Transaction::new(1, inputs, outputs, self.lock_time, SUBNETWORK_ID_NATIVE, self.gas, self.payload.clone());
        tx.set_storage_mass(self.storage_mass);
        Ok((tx, entries))
    }
}

// ---------------------------------------------------------------- mass and fee

/// How a builder sizes the fee.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeeMode {
    /// The node's relay floor: `rate × max(compute, normalized transient)` ([`MassReport::fee_mass`]).
    /// A rusty-kaspa v2.1.0 node accepts and relays the transaction at exactly this fee
    /// (`mining/src/mempool/check_transaction_standard.rs`): storage mass needs no relay fee.
    #[default]
    Relay,
    /// Storage-inclusive: `rate × max(compute, normalized transient, storage)`
    /// ([`MassReport::priority_mass`]). Block templates rank transactions by fee over their normalized
    /// mass including storage, so under contention this fee keeps the requested feerate over every
    /// mass dimension. Never below the relay floor.
    Priority,
}

/// Masses of a (signed or placeholder-signed) transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MassReport {
    /// Estimated serialized size in bytes.
    pub size: u64,
    /// Compute mass (size + 10 × script-public-key bytes + 100 × Σ compute budget).
    pub compute: u64,
    /// Transient mass (4 × size) and its value normalized to the compute scale with the node's mempool
    /// mass cofactors (`ceil(transient × compute limit / transient limit)` = 2 × size).
    pub transient: u64,
    pub transient_normalized: u64,
    /// Persistent storage mass (KIP-9), committed in the transaction.
    pub storage: u64,
    /// max(compute, normalized transient): the mass the node's relay floor is charged on (storage mass
    /// is not part of it).
    pub fee_mass: u64,
    /// max(compute, normalized transient, storage): the mass of the storage-inclusive
    /// [`FeeMode::Priority`] fee.
    #[serde(default)]
    pub priority_mass: u64,
}

impl MassReport {
    pub fn within_block_limits(&self) -> bool {
        self.compute <= BLOCK_COMPUTE_LIMIT && self.storage <= BLOCK_STORAGE_LIMIT && self.transient <= BLOCK_TRANSIENT_LIMIT
    }
}

/// The consensus parameters masses are computed with: rusty-kaspa v2.1.0 mainnet (testnet-10 has the
/// same mass parameters and block mass limits; `tests/fee_floor.rs` checks both).
pub fn mass_params() -> &'static Params {
    &MAINNET_PARAMS
}

/// The block mass limits the constants above state (also the node's mempool limits).
pub const BLOCK_MASS_LIMITS: BlockMassLimits =
    BlockMassLimits { compute: BLOCK_COMPUTE_LIMIT, storage: BLOCK_STORAGE_LIMIT, transient: BLOCK_TRANSIENT_LIMIT };

/// Computes every mass dimension of a transaction with its entries, with rusty-kaspa's own mass
/// calculator and the node's mempool mass cofactors.
pub fn masses(tx: &Transaction, entries: &[UtxoEntry]) -> MassReport {
    let params = mass_params();
    let mc = MassCalculator::new_with_consensus_params(params);
    let nc = mc.calc_non_contextual_masses(tx);
    // KIP-9 storage mass exists only for a transaction consensus can take: rusty-kaspa's calculator divides by every
    // output's value (and by the input count), and `PopulatedTransaction` asserts one entry per input. Consensus refuses a
    // transaction without inputs or with a zero-value output before computing masses; a hostile one handed to this
    // library (kob-wasm `masses` / `validate`, an x402 payload, a builder fed hostile order values) must be refused too,
    // not panic: its storage mass is beyond every limit.
    let storage = if tx.inputs.is_empty() || entries.len() != tx.inputs.len() || tx.outputs.iter().any(|o| o.value == 0) {
        u64::MAX
    } else {
        let populated = PopulatedTransaction::new(tx, entries.to_vec());
        mc.calc_contextual_masses(&populated).map(|c| c.storage_mass).unwrap_or(u64::MAX)
    };
    let size = transaction_estimated_serialized_size(tx);
    // mining/src/mempool/check_transaction_standard.rs:71-75: the mempool normalizes the transient mass
    // with the cofactors of its block mass limits (the consensus block mass limits).
    let transient_normalized = nc.normalized_transient(&params.block_mass_cofactors());
    let fee_mass = nc.compute_mass.max(transient_normalized);
    MassReport {
        size,
        compute: nc.compute_mass,
        transient: nc.transient_mass,
        transient_normalized,
        storage,
        fee_mass,
        priority_mass: fee_mass.max(storage),
    }
}

/// The node's minimum relay fee for `mass` grams at `rate` sompi per gram, computed as rusty-kaspa
/// v2.1.0 does it (`Mempool::minimum_required_transaction_relay_fee`, whose rate is in sompi per kilogram).
pub fn relay_fee_for_mass(mass: u64, rate: u64) -> u64 {
    let per_kg = rate.saturating_mul(1000);
    let mut fee = mass.saturating_mul(per_kg) / 1000;
    if fee == 0 {
        fee = per_kg;
    }
    fee.min(MAX_SOMPI)
}

/// Minimum fee, the node's relay floor: `rate × max(compute, normalized transient)`. At
/// [`MIN_FEE_RATE`] a rusty-kaspa v2.1.0 node relays a transaction paying at least this and rejects one
/// paying less (`RejectInsufficientComputeFee` / `RejectInsufficientTransientFee`).
pub fn min_fee(report: &MassReport, rate: u64) -> u64 {
    relay_fee_for_mass(report.fee_mass, rate)
}

/// The storage-inclusive fee `rate × max(compute, normalized transient, storage)` ([`FeeMode::Priority`]).
pub fn priority_fee(report: &MassReport, rate: u64) -> u64 {
    relay_fee_for_mass(report.priority_mass, rate)
}

/// The fee a builder targets in `mode` (never below the relay floor).
pub fn target_fee(report: &MassReport, rate: u64, mode: FeeMode) -> u64 {
    match mode {
        FeeMode::Relay => min_fee(report, rate),
        FeeMode::Priority => priority_fee(report, rate).max(min_fee(report, rate)),
    }
}

/// Fee summary of a built transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeReport {
    /// Fee paid (Σ inputs − Σ outputs).
    #[serde(with = "crate::json::field")]
    pub fee: u64,
    /// Fee the builder targeted at the requested rate and mode (in [`FeeMode::Relay`], the node's relay floor).
    #[serde(with = "crate::json::field")]
    pub min_fee: u64,
    #[serde(with = "crate::json::field")]
    pub fee_rate: u64,
    #[serde(default)]
    pub fee_mode: FeeMode,
    pub mass: MassReport,
    /// Index and value of the change output, if one was added.
    pub change_output: Option<u32>,
}

// ---------------------------------------------------------------- built and signed transactions

/// A covenant created by the transaction (genesis).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCovenant {
    pub outputs: Vec<u32>,
    pub authorizing_input: u16,
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    pub template: Option<TemplateId>,
}

/// An unsigned transaction with its signing plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltTx {
    /// Unsigned transaction (signature scripts empty).
    pub tx: TxJson,
    /// How to assemble each input's signature script.
    pub plans: Vec<SigPlan>,
    /// Compute-budget role of each input (key into the budget table).
    pub roles: Vec<String>,
    /// Signatures the wallet(s) must produce.
    pub sign: Vec<SignRequest>,
    pub fee: FeeReport,
    /// Covenants created by this transaction.
    pub covenants: Vec<NewCovenant>,
}

/// Finalisation options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeOptions {
    /// Re-run every input in the script engine and commit its exact minimal compute budget.
    #[serde(default)]
    pub tighten_budgets: bool,
}

/// A signed transaction, ready for `submitTransaction` over node RPC (never REST: REST drops the
/// compute budget).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedTx {
    pub tx: TxJson,
    pub fee: FeeReport,
}

/// Normalises a wallet signature to 65 bytes (64-byte Schnorr + SIGHASH_ALL).
pub fn normalize_signature(input: usize, sig: &[u8]) -> Result<Vec<u8>> {
    let bad = |reason: String| Err(Error::Signature { input, reason });
    match sig.len() {
        64 => Ok([sig, &[SIGHASH_ALL]].concat()),
        65 if sig[64] == SIGHASH_ALL => Ok(sig.to_vec()),
        65 => bad(format!("sighash type {:#04x} is not SIGHASH_ALL", sig[64])),
        66 if sig[0] == 0x41 && sig[65] == SIGHASH_ALL => Ok(sig[1..].to_vec()),
        n => bad(format!("unexpected signature length {n}")),
    }
}

/// Schnorr SIGHASH_ALL digest of input `idx`.
pub fn sighash(tx: &Transaction, entries: &[UtxoEntry], idx: usize) -> [u8; 32] {
    sighash_typed(tx, entries, idx, SIGHASH_ALL).expect("sighash all")
}

/// Schnorr digest of input `idx` under an explicit hash type byte (`None` when the byte is not a Kaspa hash
/// type). Every payer builder signs SIGHASH_ALL ([`sighash`]); this exists for the verifiers' negative tests
/// and for tools that must forge a deliberately non-ALL signature.
pub fn sighash_typed(tx: &Transaction, entries: &[UtxoEntry], idx: usize, hash_type: u8) -> Option<[u8; 32]> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let ht = SigHashType::from_u8(hash_type).ok()?;
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, ht, &reused);
    Some(h.as_bytes())
}

/// Verifies a 65-byte signature against a digest and x-only key.
pub fn verify_signature(input: usize, sig65: &[u8], digest: &[u8; 32], pubkey: &[u8; 32]) -> Result<()> {
    let bad = |reason: String| Error::Signature { input, reason };
    let secp = secp256k1::Secp256k1::verification_only();
    let sig = secp256k1::schnorr::Signature::from_slice(&sig65[..64]).map_err(|e| bad(e.to_string()))?;
    let key = secp256k1::XOnlyPublicKey::from_slice(pubkey).map_err(|e| bad(e.to_string()))?;
    let msg = secp256k1::Message::from_digest_slice(digest).map_err(|e| bad(e.to_string()))?;
    secp.verify_schnorr(&sig, &msg, &key).map_err(|_| bad("signature does not verify for this input's digest and key".into()))
}

/// Deterministic local signing (no auxiliary randomness) for executors, the CLI and tests.
pub fn sign_digest(secret_key: &[u8; 32], digest: &[u8; 32]) -> Result<Vec<u8>> {
    sign_digest_typed(secret_key, digest, SIGHASH_ALL)
}

/// [`sign_digest`] with an explicit trailing hash type byte (the digest must be the matching [`sighash_typed`]).
pub fn sign_digest_typed(secret_key: &[u8; 32], digest: &[u8; 32], hash_type: u8) -> Result<Vec<u8>> {
    let secp = secp256k1::Secp256k1::new();
    let kp = secp256k1::Keypair::from_seckey_slice(&secp, secret_key).map_err(|e| Error::Invalid(format!("secret key: {e}")))?;
    let msg = secp256k1::Message::from_digest_slice(digest).expect("32-byte digest");
    let mut s = secp.sign_schnorr_no_aux_rand(&msg, &kp).as_ref().to_vec();
    s.push(hash_type);
    Ok(s)
}

/// x-only public key of a secret key.
pub fn pubkey_of(secret_key: &[u8; 32]) -> Result<[u8; 32]> {
    let secp = secp256k1::Secp256k1::new();
    let kp = secp256k1::Keypair::from_seckey_slice(&secp, secret_key).map_err(|e| Error::Invalid(format!("secret key: {e}")))?;
    Ok(kp.x_only_public_key().0.serialize())
}

/// Signs every request of a built transaction with local keys (x-only pubkey -> secret key).
pub fn sign_locally(built: &BuiltTx, keys: &BTreeMap<[u8; 32], [u8; 32]>) -> Result<Vec<InputSignature>> {
    built
        .sign
        .iter()
        .map(|r| {
            let sk = keys.get(&r.pubkey).ok_or_else(|| Error::Invalid(format!("no local key for {}", to_hex(&r.pubkey))))?;
            Ok(InputSignature { input_index: r.input_index, signature: sign_digest(sk, &r.sighash)? })
        })
        .collect()
}

/// Checks the wallet signatures and assembles every signature script (no fee or budget pass).
pub fn assemble(built: &BuiltTx, signatures: &[InputSignature]) -> Result<(Transaction, Vec<UtxoEntry>)> {
    let (mut tx, entries) = built.tx.to_tx()?;
    if built.plans.len() != tx.inputs.len() {
        return invalid("plan count differs from input count");
    }
    let mut sigs: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    for s in signatures {
        let sig = normalize_signature(s.input_index, &s.signature)?;
        if sigs.insert(s.input_index, sig).is_some() {
            return Err(Error::Signature { input: s.input_index, reason: "duplicate signature".into() });
        }
    }
    for p in &built.plans {
        p.check()?;
    }
    for r in &built.sign {
        if r.input_index >= tx.inputs.len() {
            return invalid(format!("sign request for input {} but the transaction has {} inputs", r.input_index, tx.inputs.len()));
        }
        let digest = sighash(&tx, &entries, r.input_index);
        if digest != r.sighash {
            return invalid(format!("input {}: sign request digest does not match the transaction", r.input_index));
        }
        let sig = sigs.get(&r.input_index).ok_or(Error::Signature { input: r.input_index, reason: "missing".into() })?;
        verify_signature(r.input_index, sig, &digest, &r.pubkey)?;
    }
    for (i, plan) in built.plans.iter().enumerate() {
        if plan.signer().is_none() && sigs.contains_key(&i) {
            return Err(Error::Signature { input: i, reason: "input takes no signature".into() });
        }
        tx.inputs[i].signature_script = plan.sigscript(sigs.get(&i).map(|v| v.as_slice()))?;
    }
    Ok((tx, entries))
}

/// Sets every input's compute budget to the exact minimum measured by the script engine.
#[cfg(feature = "engine")]
pub fn tighten_budgets(tx: &mut Transaction, entries: &[UtxoEntry]) -> Result<()> {
    let units = crate::verify::measure_units(tx, entries)?;
    for (i, u) in units.iter().enumerate() {
        let b = crate::budget::budget_for_units(*u);
        tx.inputs[i].compute_commit = kaspa_consensus_core::mass::ComputeBudget::from(b).into();
    }
    Ok(())
}

/// Without the script engine, budgets stay at the table values.
#[cfg(not(feature = "engine"))]
pub fn tighten_budgets(_tx: &mut Transaction, _entries: &[UtxoEntry]) -> Result<()> {
    Err(Error::Engine("tightenBudgets needs the script engine (feature `engine`)".into()))
}

/// Assembles the signed transaction from a [`BuiltTx`] and the wallet signatures.
pub fn finalize(built: &BuiltTx, signatures: &[InputSignature], opts: FinalizeOptions) -> Result<SignedTx> {
    let (mut tx, entries) = assemble(built, signatures)?;
    let total_in = entries.iter().try_fold(0u64, |a, e| a.checked_add(e.amount));
    let total_out = tx.outputs.iter().try_fold(0u64, |a, o| a.checked_add(o.value));
    let fee = match (total_in, total_out) {
        (Some(i), Some(o)) if o <= i => i - o,
        _ => return invalid("the outputs exceed the inputs (or the sums overflow)"),
    };
    if opts.tighten_budgets {
        tighten_budgets(&mut tx, &entries)?;
    }
    let (rate, mode) = (built.fee.fee_rate, built.fee.fee_mode);
    let mut report = masses(&tx, &entries);
    tx.set_storage_mass(report.storage);
    report = masses(&tx, &entries);
    let min = target_fee(&report, rate, mode);
    if fee < min {
        return Err(Error::InsufficientFunds { need: min, have: fee });
    }
    let fee_report =
        FeeReport { fee, min_fee: min, fee_rate: rate, fee_mode: mode, mass: report, change_output: built.fee.change_output };
    Ok(SignedTx { tx: TxJson::from_tx(&tx, &entries), fee: fee_report })
}

// ---------------------------------------------------------------- the draft (builders' workspace)

pub(crate) struct DInput {
    pub utxo: Utxo,
    pub plan: DPlan,
    pub sequence: u64,
    pub role: String,
}

/// A plan whose KCC-20 leader `next_states` are filled in at seal time.
pub(crate) enum DPlan {
    Plain(SigPlan),
    Token { cov: [u8; 32], template: TemplateId, state: TokenState, witness: Witness },
}

pub(crate) struct DOutput {
    pub value: u64,
    pub spk: ScriptPublicKey,
    pub cov: Option<(u16, [u8; 32])>,
    /// Token covenant id and state (token outputs are bound to their token's leader at seal time).
    pub token: Option<([u8; 32], TokenState)>,
}

/// Builders' mutable transaction under construction.
pub(crate) struct Draft {
    pub inputs: Vec<DInput>,
    pub outputs: Vec<DOutput>,
    pub lock_time: u64,
    pub payload: Vec<u8>,
    pub change: Option<[u8; 32]>,
    pub fee_rate: u64,
    pub fee_mode: FeeMode,
    pub covenants: Vec<NewCovenant>,
    /// Tokens of the transaction (covenant id, program), in first-use order. Each token has its
    /// own KCC-20 leader (its first token input) and slot limits.
    pub tokens: Vec<([u8; 32], TemplateId)>,
    /// Output indices reserved for a positional output filled later (an IOC ask's return sits at
    /// its custody input's index): appended outputs skip them.
    pub pinned: BTreeSet<usize>,
    /// The order output that takes the change instead of a change output ([`Absorber`]).
    pub absorber: Option<Absorber>,
}

/// An order output of the transaction that takes what would otherwise be a change output (the change rule of
/// `docs/spec/kob1-payload.md` "Change"):
/// * `pays` (a maker's in-place amend without funding): the output pays the fee itself, its value becomes
///   `value − fee` and never falls below `pays`;
/// * otherwise only a TINY change is taken: one whose own storage mass (KIP-9: 10^12 / value) would exceed the
///   transaction's fee mass, i.e. a change below 10^12 / feeMass sompi (0.55 KAS on an 8/8 cancel-replace, 2.2 KAS on
///   an in-place amend). It costs no relay fee but makes storage the transaction's largest mass (a storage-inclusive fee
///   pays for it, a block holds few such transactions); the maker gets it back with the order instead. At most
///   `max_add` sompi (a bid takes no more than keeps its buying power, the amount its escrow funds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Absorber {
    pub output: usize,
    pub max_add: u64,
    pub pays: Option<u64>,
}

/// A 32-byte x-only key written into an output (maker, recipient, holder, change) must be a curve point, otherwise the
/// output is unspendable and the funds are burnt.
pub fn check_key(key: &[u8; 32], what: &str) -> Result<()> {
    secp256k1::XOnlyPublicKey::from_slice(key).map(|_| ()).map_err(|_| {
        Error::Invalid(format!("{what} {} is not a valid x-only public key: the output would be unspendable", to_hex(key)))
    })
}

impl Draft {
    pub fn new(lock_time: u64, fee: &FeeOptions, change: Option<[u8; 32]>) -> Result<Draft> {
        if let Some(c) = &change {
            check_key(c, "change key")?;
        }
        let fee_rate = fee.fee_rate.unwrap_or(MIN_FEE_RATE);
        if fee_rate < MIN_FEE_RATE {
            return invalid(format!("fee rate {fee_rate} is below the relay minimum {MIN_FEE_RATE}"));
        }
        Ok(Draft {
            inputs: vec![],
            outputs: vec![],
            lock_time,
            payload: vec![],
            change,
            fee_rate,
            fee_mode: fee.fee_mode,
            covenants: vec![],
            tokens: vec![],
            pinned: BTreeSet::new(),
            absorber: None,
        })
    }

    pub fn add_input(&mut self, utxo: &Utxo, plan: SigPlan, role: impl Into<String>, sequence: u64) -> usize {
        self.inputs.push(DInput { utxo: utxo.clone(), plan: DPlan::Plain(plan), sequence, role: role.into() });
        self.inputs.len() - 1
    }

    pub fn add_p2pk(&mut self, k: &KeyUtxo) -> usize {
        self.add_input(&k.utxo, SigPlan::P2pk { pubkey: k.pubkey }, "p2pk", SEQUENCE_NONFINAL)
    }

    /// Adds a token input; the first token input of each token is its KCC-20 leader.
    pub fn add_token_input(&mut self, tok: &TokenUtxo, token: ([u8; 32], TemplateId), witness: Witness) -> Result<usize> {
        self.set_token(token)?;
        if tok.utxo.covenant_id != Some(token.0) {
            return invalid("token UTXO does not carry the token's covenant id");
        }
        if tok.state.family() != token.1.family() {
            return invalid(format!("a {:?} token state under the {} program", tok.state.family(), token.1.name()));
        }
        if self.inputs.iter().any(|i| i.utxo.outpoint() == tok.utxo.outpoint()) {
            return invalid("a UTXO is spent twice");
        }
        self.inputs.push(DInput {
            utxo: tok.utxo.clone(),
            plan: DPlan::Token { cov: token.0, template: token.1, state: tok.state.clone(), witness },
            sequence: SEQUENCE_NONFINAL,
            role: String::new(),
        });
        Ok(self.inputs.len() - 1)
    }

    fn set_token(&mut self, token: ([u8; 32], TemplateId)) -> Result<()> {
        if !token.1.is_token() {
            return invalid(format!("{} is not a token program", token.1.name()));
        }
        match self.tokens.iter().find(|t| t.0 == token.0) {
            None => self.tokens.push(token),
            Some(t) if *t == token => {}
            Some(_) => return invalid("one token covenant id with two different programs"),
        }
        Ok(())
    }

    /// Reserves output slot `idx` for an output filled later: outputs appended from now on skip it.
    pub fn pin_output(&mut self, idx: usize) {
        self.pinned.insert(idx);
    }

    /// Appends empty reserved slots until the next appended output would not land on a pinned index.
    fn skip_pinned(&mut self) {
        while self.pinned.contains(&self.outputs.len()) {
            self.outputs.push(DOutput { value: 0, spk: ScriptPublicKey::new(0, Default::default()), cov: None, token: None });
        }
    }

    /// Makes slot `idx` exist (reserving empty slots up to it); it must then be filled.
    pub fn ensure_output(&mut self, idx: usize) {
        while self.outputs.len() <= idx {
            self.outputs.push(DOutput { value: 0, spk: ScriptPublicKey::new(0, Default::default()), cov: None, token: None });
        }
    }

    pub fn add_output(&mut self, value: u64, spk: ScriptPublicKey, cov: Option<(u16, [u8; 32])>) -> usize {
        self.skip_pinned();
        self.outputs.push(DOutput { value, spk, cov, token: None });
        self.outputs.len() - 1
    }

    /// Reserves an output slot (positional outputs are filled after the extras are laid out).
    pub fn reserve_output(&mut self) -> usize {
        self.add_output(0, ScriptPublicKey::new(0, Default::default()), None)
    }

    /// Fills a reserved slot with a plain output.
    pub fn fill_output(&mut self, idx: usize, value: u64, spk: ScriptPublicKey, cov: Option<(u16, [u8; 32])>) {
        self.outputs[idx] = DOutput { value, spk, cov, token: None };
    }

    /// Checks a token output state against its program (family, KRON output amount range).
    fn check_token_output(token: ([u8; 32], TemplateId), state: &TokenState) -> Result<()> {
        if state.family() != token.1.family() {
            return invalid(format!("a {:?} token state under the {} program", state.family(), token.1.name()));
        }
        if state.is_covenant_owned() && state.owner() == token.0 {
            // A token UTXO owned by the token's own covenant id is unlocked by any input of the token, i.e. by
            // anyone never deliver there.
            return invalid("a token output owned by the token's own covenant id is spendable by anyone: refused");
        }
        if state.family() == Family::Kron && !(1..=KRON_MAX_OUTPUT_AMOUNT).contains(&state.amount()) {
            return invalid(format!(
                "KRON token outputs hold 1..={KRON_MAX_OUTPUT_AMOUNT} units (this output would hold {}): the token program rejects it",
                state.amount()
            ));
        }
        Ok(())
    }

    /// Fills a reserved slot with a token output.
    pub fn fill_token_output(&mut self, idx: usize, token: ([u8; 32], TemplateId), state: TokenState, value: u64) -> Result<()> {
        self.set_token(token)?;
        Self::check_token_output(token, &state)?;
        let spk = state.spk_with(token_template(token.1));
        self.outputs[idx] = DOutput { value, spk, cov: None, token: Some((token.0, state)) };
        Ok(())
    }

    /// Fills a reserved slot with a single-output genesis authorised by input `auth`.
    pub fn fill_genesis(
        &mut self,
        idx: usize,
        auth: usize,
        value: u64,
        spk: ScriptPublicKey,
        template: Option<TemplateId>,
    ) -> Result<[u8; 32]> {
        let op = self.inputs.get(auth).ok_or_else(|| Error::Invalid("genesis authorising input missing".into()))?.utxo.outpoint();
        let o = TransactionOutput { value, script_public_key: spk.clone(), covenant: None };
        let id = covenant_id(op, std::iter::once((idx as u32, &o))).as_bytes();
        self.outputs[idx] = DOutput { value, spk, cov: Some((auth as u16, id)), token: None };
        self.covenants.push(NewCovenant { outputs: vec![idx as u32], authorizing_input: auth as u16, covenant_id: id, template });
        Ok(id)
    }

    /// Changes the value of the single-output genesis at `idx` and re-derives what hangs on its covenant id (the id is
    /// keyed by the output's value): the binding, the covenant record, and every token output owned by the old id (the
    /// new order's custody), whose state names the new id.
    fn set_genesis_value(&mut self, idx: usize, value: u64) -> Result<()> {
        let Some((auth, old)) = self.outputs[idx].cov else { return invalid("not a genesis output") };
        let op = self.inputs[auth as usize].utxo.outpoint();
        let o = TransactionOutput { value, script_public_key: self.outputs[idx].spk.clone(), covenant: None };
        let id = covenant_id(op, std::iter::once((idx as u32, &o))).as_bytes();
        self.outputs[idx].value = value;
        self.outputs[idx].cov = Some((auth, id));
        for c in self.covenants.iter_mut().filter(|c| c.covenant_id == old) {
            c.covenant_id = id;
        }
        let tokens = self.tokens.clone();
        for out in self.outputs.iter_mut() {
            if let Some((tok, st)) = &mut out.token {
                if st.is_covenant_owned() && st.owner() == old {
                    *st = TokenState::custody(st.family(), st.amount(), id, st.extension());
                    let program = tokens.iter().find(|t| t.0 == *tok).expect("token of an output").1;
                    out.spk = st.spk_with(token_template(program));
                }
            }
        }
        Ok(())
    }

    /// Adds a token output; it is bound to its token's leader at seal time.
    pub fn add_token_output(&mut self, token: ([u8; 32], TemplateId), state: TokenState, value: u64) -> Result<usize> {
        self.set_token(token)?;
        Self::check_token_output(token, &state)?;
        self.skip_pinned();
        let spk = state.spk_with(token_template(token.1));
        self.outputs.push(DOutput { value, spk, cov: None, token: Some((token.0, state)) });
        Ok(self.outputs.len() - 1)
    }

    /// Adds genesis outputs authorised by input `auth` as one group; returns the new covenant id.
    pub fn add_genesis(
        &mut self,
        auth: usize,
        outs: Vec<(u64, ScriptPublicKey)>,
        template: Option<TemplateId>,
    ) -> Result<(Vec<usize>, [u8; 32])> {
        self.skip_pinned();
        let first = self.outputs.len();
        let tx_outs: Vec<TransactionOutput> =
            outs.iter().map(|(v, s)| TransactionOutput { value: *v, script_public_key: s.clone(), covenant: None }).collect();
        let op = self.inputs.get(auth).ok_or_else(|| Error::Invalid("genesis authorising input missing".into()))?.utxo.outpoint();
        let id = covenant_id(op, tx_outs.iter().enumerate().map(|(k, o)| ((first + k) as u32, o))).as_bytes();
        let mut idx = vec![];
        for (v, s) in outs {
            idx.push(self.add_output(v, s, Some((auth as u16, id))));
        }
        self.covenants.push(NewCovenant {
            outputs: idx.iter().map(|i| *i as u32).collect(),
            authorizing_input: auth as u16,
            covenant_id: id,
            template,
        });
        Ok((idx, id))
    }

    /// Leader (first token input) of a token.
    fn token_leader(&self, cov: [u8; 32]) -> Option<usize> {
        self.inputs.iter().position(|i| matches!(i.plan, DPlan::Token { cov: c, .. } if c == cov))
    }

    /// Index of the input that authorises a KRON token input with owner witness `w` (the input
    /// carrying the owner covenant id, or a P2PK input of the owner key).
    fn kron_witness_index(&self, state: &KronState, w: &Witness) -> Result<u8> {
        let found = match w {
            Witness::CovenantId => {
                self.inputs.iter().position(|i| matches!(i.plan, DPlan::Plain(_)) && i.utxo.covenant_id == Some(state.owner))
            }
            Witness::P2pk(k) => {
                self.inputs.iter().position(|i| matches!(&i.plan, DPlan::Plain(SigPlan::P2pk { pubkey }) if pubkey == k))
            }
        };
        let idx = found.ok_or_else(|| match w {
            Witness::CovenantId => Error::Invalid(format!(
                "KRON custody token owned by {}: the owning order input is not in the transaction",
                to_hex(&state.owner)
            )),
            Witness::P2pk(k) => Error::Invalid(format!(
                "KRON tokens of {} are held by address presence: the transaction needs a P2PK input of that key (add a funding input)",
                to_hex(k)
            )),
        })?;
        // The witness is one script-number byte: indices above 127 read as negative and can never authorise.
        u8::try_from(idx).ok().filter(|i| *i <= 127).ok_or_else(|| Error::Invalid(format!("KRON witness index {idx} exceeds 127")))
    }

    fn plans(&self) -> Result<Vec<SigPlan>> {
        let mut out = Vec::with_capacity(self.inputs.len());
        for (i, inp) in self.inputs.iter().enumerate() {
            out.push(match &inp.plan {
                DPlan::Plain(p) => p.clone(),
                DPlan::Token { cov, template, state: TokenState::Kcc20(state), witness } => {
                    if Some(i) == self.token_leader(*cov) {
                        let next: Vec<Kcc20State> = self
                            .outputs
                            .iter()
                            .filter_map(|o| match o.token.as_ref() {
                                Some((c, TokenState::Kcc20(st))) if c == cov => Some(st.clone()),
                                _ => None,
                            })
                            .collect();
                        SigPlan::TokenLeader { template: *template, state: state.clone(), next_states: next, witness: witness.clone() }
                    } else {
                        SigPlan::TokenDelegator { template: *template, state: state.clone(), witness: witness.clone() }
                    }
                }
                DPlan::Token { cov, template, state: TokenState::Kron(state), .. } => {
                    let next: Vec<KronState> = self
                        .outputs
                        .iter()
                        .filter_map(|o| match o.token.as_ref() {
                            Some((c, TokenState::Kron(st))) if c == cov => Some(st.clone()),
                            _ => None,
                        })
                        .collect();
                    // One witness byte per token input of this token, in input order.
                    let mut witnesses = vec![];
                    for j in self.inputs.iter() {
                        if let DPlan::Token { cov: c, state: TokenState::Kron(st), witness, .. } = &j.plan {
                            if c == cov {
                                witnesses.push(self.kron_witness_index(st, witness)?);
                            }
                        }
                    }
                    SigPlan::KronToken { template: *template, state: state.clone(), next_states: next, witnesses }
                }
            });
        }
        Ok(out)
    }

    /// Token input / output counts of one token (leader included).
    fn token_counts(&self, cov: [u8; 32]) -> (usize, usize) {
        (
            self.inputs.iter().filter(|i| matches!(i.plan, DPlan::Token { cov: c, .. } if c == cov)).count(),
            self.outputs.iter().filter(|o| o.token.as_ref().is_some_and(|t| t.0 == cov)).count(),
        )
    }

    /// Runs the fee pass and produces the unsigned transaction with its signing plan.
    pub fn seal(self, budgets: &dyn Fn(&str) -> Result<u16>) -> Result<BuiltTx> {
        if self.inputs.is_empty() {
            return invalid("a transaction needs at least one input");
        }
        // An empty reserved slot (a positional slot below an IOC return, say) takes the change output when there is one; any
        // other empty slot is a builder error.
        let empty = |o: &DOutput| o.token.is_none() && o.cov.is_none() && o.value == 0 && o.spk.script().is_empty();
        let gaps: Vec<usize> = self.outputs.iter().enumerate().filter(|(_, o)| empty(o)).map(|(k, _)| k).collect();
        if gaps.len() > 1 || (gaps.len() == 1 && self.change.is_none()) {
            return invalid(format!("output slot {} was reserved but never filled", gaps[0]));
        }
        let gap = gaps.first().copied();
        let mut seen = std::collections::BTreeSet::new();
        for i in &self.inputs {
            if !seen.insert((i.utxo.transaction_id, i.utxo.index)) {
                return invalid("a UTXO is spent twice");
            }
        }
        for (cov, tpl) in &self.tokens {
            let (tin, tout) = self.token_counts(*cov);
            let (max_in, max_out) = tpl.token_slots().expect("token program");
            if tin > max_in || tout > max_out {
                return invalid(format!(
                    "{} allows {max_in} token inputs / {max_out} token outputs per transaction; this needs {tin} / {tout}",
                    tpl.name()
                ));
            }
            if tin == 0 && tout > 0 {
                return invalid("token outputs without a token input");
            }
            if tin > 0 && tout == 0 {
                return invalid("KCC-20 transfers need at least one token output");
            }
        }
        // Every token output carries at least what its token program accepts (KaspaCom 0.2.5: 0.5 KAS).
        for (k, o) in self.outputs.iter().enumerate() {
            let Some((cov, _)) = &o.token else { continue };
            let Some(tpl) = self.tokens.iter().find(|(c, _)| c == cov).map(|(_, t)| *t) else { continue };
            let floor = tpl.min_token_output().unwrap_or(0);
            if o.value < floor {
                return invalid(format!(
                    "token output {k} carries {} sompi; the {} token program refuses a token output below {floor} sompi",
                    o.value,
                    tpl.name()
                ));
            }
        }
        let plans = self.plans()?;

        // Roles and budgets.
        let mut roles = Vec::with_capacity(self.inputs.len());
        for (i, inp) in self.inputs.iter().enumerate() {
            let counts = match &inp.plan {
                DPlan::Token { cov, .. } => self.token_counts(*cov),
                DPlan::Plain(_) => (0, 0),
            };
            roles.push(match &plans[i] {
                SigPlan::TokenLeader { template, witness, next_states, .. } => {
                    format!("{}.leader.{}.i{}.o{}", template.name(), witness_tag(witness), counts.0, next_states.len())
                }
                SigPlan::TokenDelegator { template, witness, .. } => {
                    format!("{}.delegator.{}.i{}.o{}", template.name(), witness_tag(witness), counts.0, counts.1)
                }
                SigPlan::KronToken { template, .. } => {
                    let DPlan::Token { witness, .. } = &inp.plan else { unreachable!("KRON plans come from token inputs") };
                    format!(
                        "{}.token.{}.i{}.o{}",
                        template.name(),
                        match witness {
                            Witness::CovenantId => "covid",
                            Witness::P2pk(_) => "addr",
                        },
                        counts.0,
                        counts.1
                    )
                }
                _ => inp.role.clone(),
            });
        }
        let mut budget_values = Vec::with_capacity(roles.len());
        for r in &roles {
            budget_values.push(budgets(r)?);
        }

        // Transaction with placeholder signatures (identical sizes).
        let entries: Vec<UtxoEntry> = self.inputs.iter().zip(&plans).map(|(i, p)| i.utxo.entry(p.spk())).collect();
        let token_binding = |o: &DOutput| -> Option<CovenantBinding> {
            if let Some((cov, _)) = &o.token {
                let leader = self.token_leader(*cov).expect("checked: every token output has a token input");
                Some(CovenantBinding { authorizing_input: leader as u16, covenant_id: Hash::from_bytes(*cov) })
            } else {
                o.cov.map(|(a, c)| CovenantBinding { authorizing_input: a, covenant_id: Hash::from_bytes(c) })
            }
        };
        let mut outputs: Vec<TransactionOutput> = self
            .outputs
            .iter()
            .filter(|o| !empty(o))
            .map(|o| TransactionOutput { value: o.value, script_public_key: o.spk.clone(), covenant: token_binding(o) })
            .collect();
        // the change output: in the empty slot if there is one, else last
        let with_change = |outs: &mut Vec<TransactionOutput>, o: TransactionOutput| -> usize {
            match gap {
                Some(k) => {
                    outs.insert(k, o);
                    k
                }
                None => {
                    outs.push(o);
                    outs.len() - 1
                }
            }
        };
        let dummy_sig = [[0u8; 64].as_slice(), &[SIGHASH_ALL]].concat();
        let mut inputs = Vec::with_capacity(self.inputs.len());
        for ((inp, plan), b) in self.inputs.iter().zip(&plans).zip(&budget_values) {
            let ss = plan.sigscript(plan.signer().map(|_| dummy_sig.as_slice()))?;
            inputs.push(TransactionInput::new_with_compute_budget(inp.utxo.outpoint(), ss, inp.sequence, *b));
        }
        let total_in: u64 = entries.iter().map(|e| e.amount).sum();
        let fixed_out: u64 = outputs.iter().map(|o| o.value).sum();
        if fixed_out > total_in {
            return Err(Error::InsufficientFunds { need: fixed_out, have: total_in });
        }
        let slack = total_in - fixed_out;
        let mk = |outs: Vec<TransactionOutput>| {
            Transaction::new(1, inputs.clone(), outs, self.lock_time, SUBNETWORK_ID_NATIVE, 0, self.payload.clone())
        };
        let fee_of = |tx: &Transaction| target_fee(&masses(tx, &entries), self.fee_rate, self.fee_mode);
        let fee_without = fee_of(&mk(outputs.clone()));
        let mut change_output = None;
        // The absorber's position among the final outputs (empty reserved slots are not outputs). A transaction with a
        // reserved gap needs its change output there: no absorber then.
        let absorber = match (self.absorber, gap) {
            (Some(a), None) => Some((a, a.output - self.outputs[..a.output].iter().filter(|o| empty(o)).count())),
            _ => None,
        };
        // Fixed point of the absorber's value: `base + slack − fee` (storage mass moves with the value), from the upper bound.
        let absorbed = |outs: &[TransactionOutput], at: usize, slack: u64, floor: u64| -> Option<Vec<TransactionOutput>> {
            let base = outs[at].value;
            let mut v = (base + slack).checked_sub(fee_of(&mk(outs.to_vec())))?;
            for _ in 0..32 {
                let mut o = outs.to_vec();
                o[at].value = v;
                let next = (base + slack).checked_sub(fee_of(&mk(o.clone())))?;
                if next >= v {
                    return (v >= floor).then_some(o);
                }
                v = next;
            }
            None
        };
        if let Some((a, at)) = absorber.filter(|(a, _)| a.pays.is_some()) {
            // the order pays the fee out of its own value: no funding, no change
            let floor = a.pays.expect("pays");
            match absorbed(&outputs, at, slack, floor) {
                Some(o) => outputs = o,
                None => {
                    let need = fixed_out + fee_without - outputs[at].value + floor;
                    return Err(Error::InsufficientFunds { need, have: total_in });
                }
            }
        } else if let Some(pk) = self.change {
            // Fixed point c = slack − fee(c); fee(c) falls as c grows (storage mass ∝ 1/c), so
            // iterating from the upper bound converges from above or goes negative.
            let spk = p2pk_spk(&pk);
            let mut c = slack.saturating_sub(fee_without);
            let mut ok = false;
            for _ in 0..32 {
                if c == 0 {
                    break;
                }
                let mut outs = outputs.clone();
                with_change(&mut outs, TransactionOutput { value: c, script_public_key: spk.clone(), covenant: None });
                let fee = fee_of(&mk(outs));
                let next = slack.saturating_sub(fee);
                if next == c {
                    ok = true;
                    break;
                }
                if next > c {
                    // cannot happen with a monotone fee; keep the smaller (safe) value
                    ok = true;
                    break;
                }
                c = next;
            }
            if ok && c > 0 {
                // The relay floor does not price storage mass, so nothing else keeps a small change
                // output (storage mass ~ 10^12 / value) from pushing the transaction over the block
                // storage limit: such a change is folded into the fee instead.
                let mut outs = outputs.clone();
                with_change(&mut outs, TransactionOutput { value: c, script_public_key: spk.clone(), covenant: None });
                let m = masses(&mk(outs), &entries);
                // a tiny change (its own storage mass, KIP-9 C / value, would exceed the fee mass) goes into the absorber
                // instead, if it fits
                let tiny = mass_params().storage_mass_parameter / c > m.fee_mass;
                let folded = absorber
                    .filter(|_| tiny)
                    .and_then(|(a, at)| absorbed(&outputs, at, slack, 0).filter(|o| o[at].value - outputs[at].value <= a.max_add));
                if let Some(o) = folded {
                    let (a, at) = absorber.expect("folded");
                    if self.covenants.iter().any(|c| c.outputs == [a.output as u32]) {
                        // A genesis output: its value is part of its covenant id (and the id owns the order's custody).
                        // Re-derive both with the new value and seal again without a change output; the fixed point
                        // left nothing over (any rounding goes to the fee).
                        let value = o[at].value;
                        let mut d = self;
                        d.set_genesis_value(a.output, value)?;
                        d.absorber = None;
                        d.change = None;
                        return d.seal(budgets);
                    }
                    outputs = o;
                } else if m.within_block_limits() {
                    let at = with_change(&mut outputs, TransactionOutput { value: c, script_public_key: spk, covenant: None });
                    change_output = Some(at as u32);
                }
            }
        }
        if let (Some(k), None) = (gap, change_output) {
            return invalid(format!("output slot {k} was reserved but never filled (no change output to put there)"));
        }
        let paid_by_absorber = absorber.is_some_and(|(a, _)| a.pays.is_some());
        if change_output.is_none() && !paid_by_absorber && slack < fee_without {
            return Err(Error::InsufficientFunds { need: fixed_out + fee_without, have: total_in });
        }
        let tx = mk(outputs);
        let report = masses(&tx, &entries);
        tx.set_storage_mass(report.storage);
        let report = masses(&tx, &entries);
        if report.storage > BLOCK_STORAGE_LIMIT {
            // KIP-9: a small output's storage mass is ~ storage_mass_parameter / value; name the dust payouts
            let dust: Vec<String> = tx
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, o)| o.value < DUST_OUTPUT_MIN)
                .map(|(k, o)| format!("output {k} ({} sompi)", o.value))
                .collect();
            return invalid(format!(
                "dust payout: {} below {DUST_OUTPUT_MIN} sompi; the transaction's storage mass {} exceeds the block limit {BLOCK_STORAGE_LIMIT} (KIP-9), so it could never be mined",
                if dust.is_empty() { "small outputs".to_string() } else { dust.join(", ") },
                report.storage
            ));
        }
        let fee = total_in - tx.outputs.iter().map(|o| o.value).sum::<u64>();
        let min = target_fee(&report, self.fee_rate, self.fee_mode);
        if fee < min {
            return Err(Error::InsufficientFunds { need: total_in - fee + min, have: total_in });
        }

        // Unsigned transaction and sign requests.
        let mut unsigned = tx.clone();
        for i in unsigned.inputs.iter_mut() {
            i.signature_script.clear();
        }
        let mut sign = vec![];
        for (i, p) in plans.iter().enumerate() {
            if let Some(pk) = p.signer() {
                sign.push(SignRequest {
                    input_index: i,
                    pubkey: pk,
                    sighash_type: SIGHASH_ALL,
                    sighash: sighash(&unsigned, &entries, i),
                    redeem_script: p.redeem(),
                });
            }
        }
        Ok(BuiltTx {
            tx: TxJson::from_tx(&unsigned, &entries),
            plans,
            roles,
            sign,
            fee: FeeReport { fee, min_fee: min, fee_rate: self.fee_rate, fee_mode: self.fee_mode, mass: report, change_output },
            covenants: self.covenants,
        })
    }
}

fn witness_tag(w: &Witness) -> &'static str {
    match w {
        Witness::CovenantId => "covid",
        Witness::P2pk(_) => "p2pk",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TokenState;

    /// A token UTXO owned (owner_scheme 4 / KRON id_type 2) by the token's OWN covenant id is unlocked
    /// by any input of the token, i.e. by anyone. No builder may create one.
    #[test]
    fn no_token_output_owned_by_its_own_covenant_id() {
        let fee = FeeOptions::default();
        for (tpl, fam) in [(TemplateId::Kcc20Ref8x8, Family::Kcc20), (TemplateId::KronToken2433, Family::Kron)] {
            let tok = [0x70; 32];
            let mut d = Draft::new(0, &fee, None).unwrap();
            let own = TokenState::custody(fam, 5, tok, [0; 32]);
            let e = d.add_token_output((tok, tpl), own.clone(), 1_000).unwrap_err();
            assert!(e.to_string().contains("own covenant id"), "{e}");
            let at = d.reserve_output();
            assert!(d.fill_token_output(at, (tok, tpl), own, 1_000).is_err());
            // any other covenant owner (an order's custody) and a key owner are fine
            assert!(d.add_token_output((tok, tpl), TokenState::custody(fam, 5, [0x71; 32], [0; 32]), 1_000).is_ok());
            assert!(d.add_token_output((tok, tpl), TokenState::user(fam, 5, [0x72; 32], [0; 32]), 1_000).is_ok());
        }
    }
}
