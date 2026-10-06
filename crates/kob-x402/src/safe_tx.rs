//! `kaspa-sdk-safe-json-v2.0.0`: the bounded transaction projection of the binding
//! (`kaspa-exact-v2.md`, "Transaction interchange and identifiers"), version 0 and version 1.
//!
//! * uint64 values are canonical decimal strings; byte data is hex (lowercased on output);
//! * `scriptPublicKey` is `version (u16 BE) || script`;
//! * every output carries `covenant`: `null` in the `standard-native` and `additive` profiles, an
//!   object `{ "authorizingInput": n, "covenantId": <64 hex> }` in the KOB token and swap payments
//!   (version 1 only);
//! * embedded `utxo` data is a hint, never evidence: the verifier resolves every input from a
//!   trusted chain view and rejects any disagreement;
//! * the transaction id is computed from the consensus fields, never taken from `id`.

use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    CovenantBinding, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use serde::{Deserialize, Serialize};

use crate::error::{Diag, Result, X402Error};
use crate::wire::{hex, parse_hash32, parse_u64_canonical};

/// Embedded UTXO hint of an input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeUtxo {
    pub amount: String,
    pub script_public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_daa_score: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_coinbase: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covenant_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeOutpoint {
    pub transaction_id: String,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeInput {
    pub previous_outpoint: SafeOutpoint,
    pub sequence: String,
    /// Version-0 sig-op commitment (`0` in version 1).
    #[serde(default)]
    pub sig_op_count: u8,
    /// Version-1 compute budget (absent in version 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_budget: Option<u16>,
    pub signature_script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utxo: Option<SafeUtxo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeCovenant {
    pub authorizing_input: u16,
    pub covenant_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeOutput {
    pub value: String,
    pub script_public_key: String,
    /// Always serialized (`null` when absent), as the binding requires.
    #[serde(default)]
    pub covenant: Option<SafeCovenant>,
}

/// The bounded transaction projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeTx {
    /// Convenience id: if present it must equal the recomputed id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub version: u16,
    pub inputs: Vec<SafeInput>,
    pub outputs: Vec<SafeOutput>,
    #[serde(default = "zero_str")]
    pub lock_time: String,
    #[serde(default = "zero_subnetwork")]
    pub subnetwork_id: String,
    #[serde(default = "zero_str")]
    pub gas: String,
    #[serde(default)]
    pub payload: String,
    /// Mandatory committed storage mass.
    pub storage_mass: String,
}

fn zero_str() -> String {
    "0".into()
}
fn zero_subnetwork() -> String {
    "0".repeat(40)
}

/// A transaction rebuilt from a projection with its input hints.
pub struct ParsedTx {
    pub tx: Transaction,
    /// Embedded UTXO hints per input (None when absent).
    pub hints: Vec<Option<HintUtxo>>,
}

/// An embedded UTXO hint, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintUtxo {
    pub amount: u64,
    pub script_public_key: ScriptPublicKey,
    pub covenant_id: Option<[u8; 32]>,
}

fn bad(msg: impl Into<String>) -> X402Error {
    X402Error::payload(Diag::InvalidKaspaExactTransaction, msg)
}

/// Parses a serialized script public key: `version (u16 BE) || script`, hex.
pub fn spk_from_hex(s: &str) -> Result<ScriptPublicKey> {
    kob_protocol::tx::spk_from_string(s).map_err(|e| bad(format!("scriptPublicKey: {e}")))
}

/// Serialized script public key (lowercase hex).
pub fn spk_to_hex(spk: &ScriptPublicKey) -> String {
    kob_protocol::tx::spk_to_string(spk)
}

fn u64_field(s: &str, what: &str) -> Result<u64> {
    parse_u64_canonical(s).ok_or_else(|| bad(format!("{what} {s:?} is not a canonical uint64")))
}

fn hex_field(s: &str, what: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(bad(format!("{what}: odd-length hex")));
    }
    kob_protocol::json::from_hex(s).map_err(|e| bad(format!("{what}: {e}")))
}

impl SafeTx {
    /// Parses the JSON text of `payload.transaction`, rejecting oversized input before parsing.
    pub fn parse(text: &str, max_bytes: usize) -> Result<SafeTx> {
        if text.len() > max_bytes {
            return Err(bad(format!("transaction artifact of {} bytes exceeds the {max_bytes} byte bound", text.len())));
        }
        serde_json::from_str(text).map_err(|e| bad(format!("transaction artifact is not the safe projection: {e}")))
    }

    /// Rebuilds the consensus transaction (id recomputed) and the decoded hints.
    pub fn to_consensus(&self) -> Result<ParsedTx> {
        if self.version > 1 {
            return Err(bad(format!("unsupported transaction version {}", self.version)));
        }
        let mut inputs = Vec::with_capacity(self.inputs.len());
        let mut hints = Vec::with_capacity(self.inputs.len());
        for (i, inp) in self.inputs.iter().enumerate() {
            let txid = parse_hash32(&inp.previous_outpoint.transaction_id)
                .ok_or_else(|| bad(format!("input {i}: outpoint transactionId is not 32-byte hex")))?;
            let outpoint = TransactionOutpoint::new(TransactionId::from_bytes(txid), inp.previous_outpoint.index);
            let sig = hex_field(&inp.signature_script, "signatureScript")?;
            let seq = u64_field(&inp.sequence, "sequence")?;
            inputs.push(match self.version {
                0 => {
                    if inp.compute_budget.is_some() {
                        return Err(bad(format!("input {i}: version-0 input carries a computeBudget")));
                    }
                    TransactionInput::new(outpoint, sig, seq, inp.sig_op_count)
                }
                _ => {
                    if inp.sig_op_count != 0 {
                        return Err(bad(format!("input {i}: version-1 input carries a sigOpCount")));
                    }
                    let b = inp.compute_budget.ok_or_else(|| bad(format!("input {i}: version-1 input needs computeBudget")))?;
                    TransactionInput::new_with_compute_budget(outpoint, sig, seq, b)
                }
            });
            hints.push(match &inp.utxo {
                None => None,
                Some(u) => Some(HintUtxo {
                    amount: u64_field(&u.amount, "utxo.amount")?,
                    script_public_key: spk_from_hex(&u.script_public_key)?,
                    covenant_id: match &u.covenant_id {
                        None => None,
                        Some(c) => Some(parse_hash32(c).ok_or_else(|| bad("utxo.covenantId is not 32-byte hex"))?),
                    },
                }),
            });
        }
        let mut outputs = Vec::with_capacity(self.outputs.len());
        for (i, o) in self.outputs.iter().enumerate() {
            let covenant = match &o.covenant {
                None => None,
                Some(c) => {
                    if self.version == 0 {
                        return Err(bad(format!("output {i}: version-0 transactions carry no covenant bindings")));
                    }
                    let id = parse_hash32(&c.covenant_id).ok_or_else(|| bad(format!("output {i}: covenantId is not 32-byte hex")))?;
                    Some(CovenantBinding { authorizing_input: c.authorizing_input, covenant_id: Hash::from_bytes(id) })
                }
            };
            outputs.push(TransactionOutput {
                value: u64_field(&o.value, "output value")?,
                script_public_key: spk_from_hex(&o.script_public_key)?,
                covenant,
            });
        }
        let sub = hex_field(&self.subnetwork_id, "subnetworkId")?;
        if sub.as_slice() != AsRef::<[u8]>::as_ref(&SUBNETWORK_ID_NATIVE) {
            return Err(bad("only the native subnetwork is accepted"));
        }
        let tx = Transaction::new(
            self.version,
            inputs,
            outputs,
            u64_field(&self.lock_time, "lockTime")?,
            SUBNETWORK_ID_NATIVE,
            u64_field(&self.gas, "gas")?,
            hex_field(&self.payload, "payload")?,
        );
        tx.set_storage_mass(u64_field(&self.storage_mass, "storageMass")?);
        if let Some(id) = &self.id {
            let claimed =
                parse_hash32(id).ok_or_else(|| X402Error::payload(Diag::InvalidKaspaExactTransactionId, "id is not 32-byte hex"))?;
            if claimed != tx.id().as_bytes() {
                return Err(X402Error::payload(
                    Diag::InvalidKaspaExactTransactionId,
                    "artifact id differs from the recomputed transaction id",
                ));
            }
        }
        Ok(ParsedTx { tx, hints })
    }

    /// Projection of a consensus transaction with its UTXO entries as hints (`amount` and
    /// `scriptPublicKey`, like the binding's vectors; the covenant id is added for covenant inputs).
    pub fn from_consensus(tx: &Transaction, entries: &[UtxoEntry]) -> SafeTx {
        SafeTx {
            id: Some(hex(&tx.id().as_bytes())),
            version: tx.version,
            inputs: tx
                .inputs
                .iter()
                .zip(entries)
                .map(|(i, e)| SafeInput {
                    previous_outpoint: SafeOutpoint {
                        transaction_id: hex(&i.previous_outpoint.transaction_id.as_bytes()),
                        index: i.previous_outpoint.index,
                    },
                    sequence: i.sequence.to_string(),
                    sig_op_count: i.compute_commit.sig_op_count().unwrap_or(0),
                    compute_budget: i.compute_commit.compute_budget(),
                    signature_script: hex(&i.signature_script),
                    utxo: Some(SafeUtxo {
                        amount: e.amount.to_string(),
                        script_public_key: spk_to_hex(&e.script_public_key),
                        block_daa_score: None,
                        is_coinbase: None,
                        covenant_id: e.covenant_id.map(|h| hex(&h.as_bytes())),
                    }),
                })
                .collect(),
            outputs: tx
                .outputs
                .iter()
                .map(|o| SafeOutput {
                    value: o.value.to_string(),
                    script_public_key: spk_to_hex(&o.script_public_key),
                    covenant: o
                        .covenant
                        .map(|c| SafeCovenant { authorizing_input: c.authorizing_input, covenant_id: hex(&c.covenant_id.as_bytes()) }),
                })
                .collect(),
            lock_time: tx.lock_time.to_string(),
            subnetwork_id: hex(AsRef::<[u8]>::as_ref(&tx.subnetwork_id)),
            gas: tx.gas.to_string(),
            payload: hex(&tx.payload),
            storage_mass: tx.storage_mass().to_string(),
        }
    }

    /// Compact JSON text (the value of `payload.transaction`).
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).expect("safe transaction serializes")
    }
}
