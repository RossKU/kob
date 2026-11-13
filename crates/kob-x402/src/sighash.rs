//! Payer signature hash types, and the SIGHASH_ALL rule of KOB's own profiles.
//!
//! # Standard-native payments
//!
//! The binding (v1.0.1) honors the hash type encoded in each signature of a standard-native payer
//! input, which may be any type in [`CONSENSUS_HASH_TYPES`]. The signer chooses its scope. The
//! exact merchant output, the recipient script, the profile and the request authorization stay
//! mandatory (`crate::exact`). Reference signers and every KOB signer default to SIGHASH_ALL.
//!
//! # KOB's own profiles
//!
//! Every payer signature of a `kcc20`, swap-and-pay or intent payment uses SIGHASH_ALL. These
//! payments are authorized by the payload commitment, not by a signature over the transaction id.
//! The payer's input signatures are therefore its only consent to the whole transaction, which
//! includes the token change, the KAS change, the order fills and the intent's lock. A weaker
//! scope would let whoever holds the signed payment reshape what the signature leaves open. That
//! may be the merchant, the facilitator or a relay. No KOB signer produces another hash type.
//!
//! # Checking the hash type
//!
//! The script engine accepts a correct signature of any hash type and does not report which one
//! it saw. Each verifier of KOB's profiles therefore reads the hash type byte from the signature
//! script, per owner scheme, before it runs the engine. The rule is in
//! `docs/spec/x402-kcc20-profile.md` (step 8 and section 5.1) and in
//! `docs/spec/x402-swap-and-pay.md` (sections 8.1a and 17.5). The conformance cases are
//! `kcc20-neg-hashtype`, `swap-neg-hashtype` and `intent-neg-hashtype`.
//!
//! # Where a payer signature sits
//!
//! - A Schnorr P2PK funding input has the script `push(sig64 ‖ type)`. The hash type is the last
//!   byte of that 65-byte push.
//! - A KCC-20 leader (`transfer`) has the script `next_states…, push(witness), tag, redeem`. Its
//!   witness is `0x00 ‖ proof`, where `0x00` is the owner path.
//! - A KCC-20 delegator (`transfer_delegator`) has the script `push(witness), tag, redeem`. Its
//!   witness is the proof alone.
//!
//! The proof depends on the owner scheme:
//!
//! - `0x00`, Schnorr key: `sig64 ‖ type`.
//! - `0x01`, hashed Schnorr key: `pubkey32 ‖ sig64 ‖ type`.
//! - `0x02`, hashed ECDSA key: `pubkey33 ‖ sig64 ‖ type`.
//! - `0x03`, P2SH authority: the index of the authority input, whose script decides the hash type.
//! - `0x04`, covenant id: empty, with no signature.
//!
//! The reference verifiers accept only owner scheme `0x00` for a payer token input. They refuse
//! the other schemes with `token_owner_scheme` before this check. [`owner_proof`] still parses all
//! five, so a verifier that accepts more schemes keeps the rule. KRON tokens carry no signature of
//! their own. Their owner authorizes them with a Schnorr P2PK input, which [`check_p2pk_input`]
//! covers.

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
/// Every signature hash type Kaspa consensus supports.
///
/// They are ALL, NONE and SINGLE, each with and without ANYONECANPAY. A standard-native payer
/// signature may use any of them. Any other trailing byte is invalid.
pub const CONSENSUS_HASH_TYPES: [u8; 6] = [0x01, 0x02, 0x04, 0x81, 0x82, 0x84];

/// True for a consensus-supported hash type byte ([`CONSENSUS_HASH_TYPES`]).
pub fn is_consensus_hash_type(t: u8) -> bool {
    CONSENSUS_HASH_TYPES.contains(&t)
}

// KCC-20 owner schemes, the `owner_scheme` byte of the token state. They are the KCC-2 scheme
// bytes.

/// Owner scheme `0x00`: a Schnorr public key.
pub const OWNER_P2PK_SCHNORR: u8 = kob_protocol::kcc2::P2PK_SCHNORR;
/// Owner scheme `0x01`: the hash of a Schnorr public key.
pub const OWNER_P2PKH_SCHNORR: u8 = kob_protocol::kcc2::P2PKH_SCHNORR;
/// Owner scheme `0x02`: the hash of a compressed ECDSA public key.
pub const OWNER_P2PKH_ECDSA: u8 = kob_protocol::kcc2::P2PKH_ECDSA;
/// Owner scheme `0x03`: a P2SH authority, satisfied by another input.
pub const OWNER_P2SH: u8 = kob_protocol::kcc2::P2SH;
/// Owner scheme `0x04`: a covenant id.
pub const OWNER_COVENANT_ID: u8 = kob_protocol::kcc2::COVENANT_ID;

/// The leader witness path byte of the owner path.
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

/// Refuses every hash type but SIGHASH_ALL with `token_owner_scheme`.
///
/// This is the rule of KOB's own profiles, see the module docs.
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
    /// A key signature of owner scheme `0x00`, `0x01` or `0x02`, with its trailing hash type byte.
    Signature { hash_type: u8 },
    /// Owner scheme `0x03`: the index of the authority input, whose script decides the hash type.
    Authority { input: u8 },
    /// Owner scheme `0x04`: no signature.
    Covenant,
}

/// Parses the owner proof of a witness, with the lengths the token program enforces per scheme.
///
/// The proof is the witness after its path byte on a leader, and the whole witness on a delegator.
/// `None` is a malformed proof, which the program would reject.
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
    /// A data push. It is `PATH_NORMAL ‖ proof` for a leader and the proof alone for a delegator.
    Bytes(&'a [u8]),
    /// A small-number opcode, `OP_1NEGATE` or `OP_1`..`OP_16`.
    ///
    /// It is the minimal encoding of some one-byte witnesses. On a delegator, a scheme `0x03`
    /// authority index of 1 to 16 looks like this.
    SmallNumber,
}

/// The witness of a KCC-20 token input, the third push from the end of its signature script.
///
/// The layout is `[next-state arrays…,] witness, dispatch tag, redeem script`. A leader
/// (`transfer`) carries the arrays, so it has at least four pushes. A delegator
/// (`transfer_delegator`) has exactly three. `None` when the layout differs.
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

/// The owner proof bytes of a KCC-20 token input.
///
/// On a delegator it is the whole witness. On a leader it is the witness after the path byte,
/// which must be `PATH_NORMAL`. `None` when one of these holds:
/// - the layout differs ([`token_witness`]);
/// - the leader witness is empty or takes another path;
/// - the witness is a small-number opcode.
pub fn token_owner_proof(signature_script: &[u8], leader: bool) -> Option<&[u8]> {
    let TokenWitness::Bytes(w) = token_witness(signature_script, leader)? else { return None };
    if leader {
        let (path, proof) = w.split_first()?;
        (*path == PATH_NORMAL).then_some(proof)
    } else {
        Some(w)
    }
}

/// The hash type byte of a Schnorr P2PK signature script, `push(sig64 ‖ type)`.
///
/// `None` when the script is not exactly one 65-byte push.
pub fn p2pk_hash_type(signature_script: &[u8]) -> Option<u8> {
    match parse_pushes(signature_script)?.as_slice() {
        [Push::Data(d)] if d.len() == 65 => Some(d[64]),
        _ => None,
    }
}

/// Checks that a Schnorr P2PK input of a KOB-profile payment signs with SIGHASH_ALL.
///
/// The input is a funding input of a `kcc20`, swap or intent payment, or the owner input of a KRON
/// token. Standard-native inputs are not checked here, because they may use any
/// consensus-supported hash type ([`is_consensus_hash_type`]).
pub fn check_p2pk_input(input: usize, signature_script: &[u8]) -> Result<()> {
    let t =
        p2pk_hash_type(signature_script).ok_or_else(|| malformed(input, "the signature script is not one 65-byte signature push"))?;
    require_all(input, t)
}

/// Checks that the owner signature of a KCC-20 token input uses SIGHASH_ALL.
///
/// `leader` is true when the input is the first input of its token covenant.
///
/// Owner scheme `0x03` is refused. Its signature lives in another input whose script decides the
/// hash type, so this check cannot establish SIGHASH_ALL. The profile accepts the scheme only for a
/// pinned, reviewed authority script. Owner scheme `0x04` carries no signature, so there is
/// nothing to check.
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
        // A one-byte witness in its minimal opcode encoding. Only an authority index has this form.
        return Err(if owner_scheme == OWNER_P2SH && !leader { authority(None) } else { no_witness() });
    }
    let proof = token_owner_proof(signature_script, leader).ok_or_else(no_witness)?;
    match owner_proof(owner_scheme, proof).ok_or_else(|| malformed(input, "the owner witness does not match the owner scheme"))? {
        OwnerProof::Signature { hash_type } => require_all(input, hash_type),
        OwnerProof::Authority { input: idx } => Err(authority(Some(idx))),
        OwnerProof::Covenant => Ok(()),
    }
}

/// True when input `i` is the leader of its token covenant.
///
/// The leader is the first input that carries the covenant id.
pub fn is_leader(entries: &[UtxoEntry], i: usize) -> bool {
    let Some(cov) = entries[i].covenant_id else { return false };
    entries.iter().position(|e| e.covenant_id == Some(cov)) == Some(i)
}

/// Checks that every payer-signed input of `tx` uses SIGHASH_ALL.
///
/// The verifier classifies the inputs from the trusted entries. `payer_p2pk` lists the Schnorr
/// P2PK funding inputs, and `payer_token` the KCC-20 token inputs with their owner scheme. The
/// inputs are checked in index order. The first hash type other than SIGHASH_ALL fails with
/// `token_owner_scheme`, and a malformed signature script with `invalid_kaspa_exact_signature`.
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
