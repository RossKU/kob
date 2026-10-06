//! The signature hash type of payer signatures: every payer signature of an x402 payment uses SIGHASH_ALL.
//!
//! A signature with another hash type (NONE, SINGLE, either with ANYONECANPAY) passes the script engine but
//! does not commit to every output (or input) of the transaction, so a third party could alter the verified
//! transaction before it confirms. The engine cannot tell the verifier which hash type was used, so each
//! verifier that accepts payer-signed inputs reads the hash type byte from the signature script itself, per
//! owner scheme, BEFORE it runs the engine (`docs/spec/x402-kcc20-profile.md` step 8 and section 5.1,
//! `docs/spec/x402-swap-and-pay.md` sections 8.1a and 17.5; conformance cases `kcc20-neg-hashtype`, `swap-neg-hashtype` and
//! `intent-neg-hashtype`).
//!
//! Where a payer signature sits:
//!
//! | Input | Signature script | Hash type byte |
//! |---|---|---|
//! | Schnorr P2PK funding input | `push(sig64 ‖ type)` | last byte of the single 65-byte push |
//! | KCC-20 leader (`transfer`), owner scheme `0x00` | `next_states…, push(0x00 ‖ sig64 ‖ type), tag, redeem` | last byte of the witness |
//! | KCC-20 delegator (`transfer_delegator`), `0x00` | `push(sig64 ‖ type), tag, redeem` | last byte of the witness |
//! | owner scheme `0x01` (hashed Schnorr key) | witness `pubkey32 ‖ sig64 ‖ type` | last byte of the witness |
//! | owner scheme `0x02` (hashed ECDSA key) | witness `pubkey33 ‖ sig64 ‖ type` | last byte of the witness |
//! | owner scheme `0x03` (P2SH authority) | witness: the index of the authority input | decided by the authority script |
//! | owner scheme `0x04` (covenant id) | empty witness | none |
//!
//! The reference verifiers accept owner scheme `0x00` only (the other schemes are `token_owner_scheme`
//! refusals before this check is reached); [`owner_proof`] still parses all five so that a verifier which
//! accepts more schemes later keeps the invariant. KRON tokens carry no signature of their own: their owner
//! authorizes them with a Schnorr P2PK input, which [`check_p2pk_input`] covers.

use kaspa_consensus_core::tx::{Transaction, UtxoEntry};

use crate::error::{Diag, Result, X402Error};
pub use crate::exact::SIGHASH_ALL;
use crate::token::{parse_pushes, Push};

/// `SIGHASH_NONE`.
pub const SIGHASH_NONE: u8 = 0x02;
/// `SIGHASH_SINGLE`.
pub const SIGHASH_SINGLE: u8 = 0x04;
/// The `ANYONECANPAY` flag, or-ed into one of the three base types.
pub const SIGHASH_ANYONE_CAN_PAY: u8 = 0x80;

/// KCC-20 owner schemes (the `owner_scheme` byte of the token state): the canonical KCC-2 scheme bytes.
pub const OWNER_P2PK_SCHNORR: u8 = kob_protocol::kcc2::P2PK_SCHNORR;
pub const OWNER_P2PKH_SCHNORR: u8 = kob_protocol::kcc2::P2PKH_SCHNORR;
pub const OWNER_P2PKH_ECDSA: u8 = kob_protocol::kcc2::P2PKH_ECDSA;
pub const OWNER_P2SH: u8 = kob_protocol::kcc2::P2SH;
pub const OWNER_COVENANT_ID: u8 = kob_protocol::kcc2::COVENANT_ID;

/// KCC-20 leader witness path byte: the owner path (`PATH_NORMAL`).
const PATH_NORMAL: u8 = 0x00;

/// A readable name of a hash type byte, for diagnostics.
pub fn hash_type_name(t: u8) -> String {
    let base = match t & !SIGHASH_ANYONE_CAN_PAY {
        0x01 => "SIGHASH_ALL",
        0x02 => "SIGHASH_NONE",
        0x04 => "SIGHASH_SINGLE",
        _ => return format!("the unknown hash type {t:#04x}"),
    };
    if t & SIGHASH_ANYONE_CAN_PAY != 0 {
        format!("{base}|ANYONECANPAY")
    } else {
        base.to_string()
    }
}

/// Refuses every hash type but SIGHASH_ALL (`token_owner_scheme`).
pub fn require_all(input: usize, hash_type: u8) -> Result<()> {
    if hash_type == SIGHASH_ALL {
        return Ok(());
    }
    Err(X402Error::payload(
        Diag::TokenOwnerScheme,
        format!("input {input}: the payer signature uses {}, every payer signature must use SIGHASH_ALL", hash_type_name(hash_type)),
    ))
}

fn malformed(input: usize, what: &str) -> X402Error {
    X402Error::payload(Diag::InvalidKaspaExactSignature, format!("input {input}: {what}"))
}

/// What the owner proof of a KCC-20 witness is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerProof {
    /// A key signature (owner schemes `0x00`, `0x01`, `0x02`) with this trailing hash type byte.
    Signature { hash_type: u8 },
    /// Owner scheme `0x03`: the index of the authority input, whose script decides the hash type.
    Authority { input: u8 },
    /// Owner scheme `0x04`: no signature.
    Covenant,
}

/// Parses the owner proof of a witness (the bytes after the path byte on the leader path, the whole witness on
/// a delegator) by owner scheme, with the exact lengths the token program enforces. `None` is a malformed
/// proof (the program would reject it).
pub fn owner_proof(owner_scheme: u8, proof: &[u8]) -> Option<OwnerProof> {
    match (owner_scheme, proof.len()) {
        (OWNER_P2PK_SCHNORR, 65) | (OWNER_P2PKH_SCHNORR, 97) | (OWNER_P2PKH_ECDSA, 98) => {
            Some(OwnerProof::Signature { hash_type: *proof.last().expect("non-empty") })
        }
        (OWNER_P2SH, 1) => Some(OwnerProof::Authority { input: proof[0] }),
        (OWNER_COVENANT_ID, 0) => Some(OwnerProof::Covenant),
        _ => None,
    }
}

/// The witness of a KCC-20 token input as it sits in the signature script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenWitness<'a> {
    /// A data push: for a leader `PATH_NORMAL ‖ proof`, for a delegator the proof alone.
    Bytes(&'a [u8]),
    /// A small-number opcode (`OP_1NEGATE`, `OP_1`..`OP_16`): the minimal encoding of a one-byte witness, which is
    /// what a scheme `0x03` authority index of 1 to 16 looks like on a delegator.
    SmallNumber,
}

/// The witness of a KCC-20 token input inside its signature script. The layout is
/// `[next-state arrays…,] witness, dispatch tag, redeem script`: the witness is the third push from the end.
/// A leader (`transfer`) carries the arrays; a delegator (`transfer_delegator`) has exactly the three pushes.
/// `None` when the layout differs.
pub fn token_witness(signature_script: &[u8], leader: bool) -> Option<TokenWitness<'_>> {
    let pushes = parse_pushes(signature_script)?;
    let n = pushes.len();
    if n < 3 || (leader && n < 4) || (!leader && n != 3) {
        return None;
    }
    Some(match pushes[n - 3] {
        Push::Data(d) => TokenWitness::Bytes(d),
        Push::Number => TokenWitness::SmallNumber,
    })
}

/// The owner proof bytes of a KCC-20 token input: the witness of a delegator, the witness after the path byte of a
/// leader (which must be `PATH_NORMAL`, the owner path). `None` when the layout differs, the leader takes another
/// path, or the witness is a small-number opcode ([`token_witness`]).
pub fn token_owner_proof(signature_script: &[u8], leader: bool) -> Option<&[u8]> {
    let TokenWitness::Bytes(w) = token_witness(signature_script, leader)? else { return None };
    if leader {
        let (path, proof) = w.split_first()?;
        (*path == PATH_NORMAL).then_some(proof)
    } else {
        Some(w)
    }
}

/// The hash type byte of a Schnorr P2PK signature script (`push(sig64 ‖ type)`), `None` when the script is not
/// exactly one 65-byte push.
pub fn p2pk_hash_type(signature_script: &[u8]) -> Option<u8> {
    match parse_pushes(signature_script)?.as_slice() {
        [Push::Data(d)] if d.len() == 65 => Some(d[64]),
        _ => None,
    }
}

/// Checks that the Schnorr P2PK input `input` signs with SIGHASH_ALL.
pub fn check_p2pk_input(input: usize, signature_script: &[u8]) -> Result<()> {
    let t =
        p2pk_hash_type(signature_script).ok_or_else(|| malformed(input, "the signature script is not one 65-byte signature push"))?;
    require_all(input, t)
}

/// Checks that the owner signature of the token input `input` (owner scheme `owner_scheme`, `leader` = it is the
/// first input of its token covenant) uses SIGHASH_ALL.
///
/// Owner scheme `0x03` is refused: its signature lives in another input whose script decides the hash type, and
/// a verifier cannot establish SIGHASH_ALL from here (the profile accepts it only for a pinned, reviewed
/// authority script). Owner scheme `0x04` carries no signature, so there is nothing to check.
pub fn check_token_input(input: usize, signature_script: &[u8], owner_scheme: u8, leader: bool) -> Result<()> {
    let no_witness = || malformed(input, "the token input's signature script has no canonical owner witness");
    let authority = |idx: Option<u8>| {
        let by = idx.map_or("another input".to_string(), |i| format!("input {i}"));
        X402Error::payload(
            Diag::TokenOwnerScheme,
            format!("input {input}: owner scheme 0x03 is authorized by {by}; its signature hash type cannot be established"),
        )
    };
    if token_witness(signature_script, leader).ok_or_else(no_witness)? == TokenWitness::SmallNumber {
        // a one-byte witness in its minimal (opcode) encoding: only an authority index looks like this
        return Err(if owner_scheme == OWNER_P2SH && !leader { authority(None) } else { no_witness() });
    }
    let proof = token_owner_proof(signature_script, leader).ok_or_else(no_witness)?;
    match owner_proof(owner_scheme, proof).ok_or_else(|| malformed(input, "the owner witness does not match the owner scheme"))? {
        OwnerProof::Signature { hash_type } => require_all(input, hash_type),
        OwnerProof::Authority { input: idx } => Err(authority(Some(idx))),
        OwnerProof::Covenant => Ok(()),
    }
}

/// True when input `i` is the leader of its token covenant: the first input that carries its covenant id.
pub fn is_leader(entries: &[UtxoEntry], i: usize) -> bool {
    let Some(cov) = entries[i].covenant_id else { return false };
    entries.iter().position(|e| e.covenant_id == Some(cov)) == Some(i)
}

/// Checks every payer-signed input of `tx`: `payer_p2pk` and `payer_token` are the input classes the verifier
/// established from the trusted entries (Schnorr P2PK funding inputs; KCC-20 token inputs with their owner
/// scheme). Fails with `token_owner_scheme` on the first hash type other than SIGHASH_ALL.
pub fn check_payer_inputs(
    tx: &Transaction,
    entries: &[UtxoEntry],
    payer_p2pk: impl IntoIterator<Item = usize>,
    payer_token: impl IntoIterator<Item = (usize, u8)>,
) -> Result<()> {
    let mut checks: Vec<(usize, Option<u8>)> = payer_p2pk.into_iter().map(|i| (i, None)).collect();
    checks.extend(payer_token.into_iter().map(|(i, s)| (i, Some(s))));
    checks.sort_by_key(|(i, _)| *i);
    for (i, scheme) in checks {
        let script = &tx.inputs[i].signature_script;
        match scheme {
            None => check_p2pk_input(i, script)?,
            Some(s) => check_token_input(i, script, s, is_leader(entries, i))?,
        }
    }
    Ok(())
}
