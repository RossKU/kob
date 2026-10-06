//! Script helpers and signature-script encoding.
//!
//! A KOB covenant input's signature script is `args… ‖ push(dispatch tag) ‖ push(redeem)`: the
//! entry arguments in ABI order (encoded by `silverscript-abi`, the same encoder the compiler
//! tests use and the one Kaspire's wallet reproduced byte-for-byte on TN10), the 4-byte dispatch
//! tag, then the redeem script. KCC-20 inputs use the token program's `transfer` (leader) or
//! `transfer_delegator` entry with an owner witness; KRON inputs use the single-entry column layout
//! of [`kron_token_sigscript`]. A P2PK input is `push(sig65)`.

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32};
use kaspa_txscript::script_builder::ScriptBuilder;
use kaspa_txscript::{pay_to_script_hash_script, EngineFlags};
use silverscript_abi::{encode_contract_entry_sig_script, ArtifactValue};

use crate::artifacts::Template;
use crate::state::{FieldMap, Kcc20State, KronState};

/// Signature hash type used by every KOB signature (SIGHASH_ALL).
pub const SIGHASH_ALL: u8 = 0x01;

/// Canonical data push.
pub fn push_data(data: &[u8]) -> Vec<u8> {
    ScriptBuilder::with_flags(EngineFlags::default()).add_data(data).expect("push data").drain()
}

/// P2PK (Schnorr, x-only key) script public key.
pub fn p2pk_spk(pubkey: &[u8; 32]) -> ScriptPublicKey {
    let mut s = Vec::with_capacity(34);
    s.push(OpData32);
    s.extend_from_slice(pubkey);
    s.push(OpCheckSig);
    ScriptPublicKey::new(0, s.into())
}

/// P2SH script public key of a redeem script.
pub fn p2sh_spk(redeem: &[u8]) -> ScriptPublicKey {
    pay_to_script_hash_script(redeem)
}

/// Signature script of a template entry: ABI-encoded arguments, dispatch tag, redeem push.
pub fn entry_sigscript(tpl: &Template, redeem: &[u8], entry: &str, args: &[ArtifactValue]) -> Result<Vec<u8>, String> {
    let mut s = encode_contract_entry_sig_script(&tpl.artifact, &tpl.contract_name, entry, args)
        .map_err(|e| format!("{}.{entry}: {e}", tpl.id.name()))?;
    s.extend_from_slice(&push_data(redeem));
    Ok(s)
}

/// ABI value of a KCC-20 state (the `State` struct argument of `transfer`).
pub fn kcc20_state_value(s: &Kcc20State) -> ArtifactValue {
    ArtifactValue::Object(s.to_values())
}

/// KCC-20 owner witness for the leader path: `PATH_NORMAL (0x00) ‖ owner proof`.
pub fn leader_witness(sig: Option<&[u8]>) -> Vec<u8> {
    let mut w = vec![0x00];
    if let Some(s) = sig {
        w.extend_from_slice(s);
    }
    w
}

/// Signature script of a KRON token input (KRON SDK `transferSigScript` layout): the next-state
/// columns of the whole covenant group (`owners`, `id_types`, `amounts`, `is_minters`, each one
/// concatenated push), the signature column (`sigs`, only `id_type` 0 uses it; empty here), the
/// witness column (one byte per token input of the token, in input order: the index of the input
/// that authorises it) and the redeem script. There is no dispatch tag: the program has one entry.
pub fn kron_token_sigscript(redeem: &[u8], next: &[KronState], sigs: &[u8], witnesses: &[u8]) -> Vec<u8> {
    let mut s = vec![];
    s.extend(push_data(&next.iter().flat_map(|x| x.owner.to_vec()).collect::<Vec<u8>>()));
    s.extend(push_data(&next.iter().map(|x| x.id_type).collect::<Vec<u8>>()));
    s.extend(push_data(&next.iter().flat_map(|x| x.amount.to_le_bytes().to_vec()).collect::<Vec<u8>>()));
    s.extend(push_data(&next.iter().map(|x| x.is_minter).collect::<Vec<u8>>()));
    s.extend(push_data(sigs));
    s.extend(push_data(witnesses));
    s.extend(push_data(redeem));
    s
}
