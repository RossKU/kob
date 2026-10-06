//! KAS `standard-native` exact: the elldeeone binding (`kaspa-exact-v2`, "Standard-native profile",
//! "Verification", "Expiry"), verified from the payer's signed version-0 transaction.
//!
//! [`verify_native`] fails closed and trusts nothing the payer supplies: the transaction id is
//! recomputed, every input is resolved from the trusted chain view (embedded UTXO hints are only a
//! lookup key that must match), the payer's signatures run through the script engine, the merchant
//! gain is checked exactly, and the request authorization digest is recomputed by the verifier.
//!
//! The module also carries independent implementations of the binding's transaction-id preimages
//! ([`spec_txid_v0`], [`spec_txid_v1`]) written from the specification text only, so the interop
//! vectors can prove that the pinned rusty-kaspa hashes exactly what the binding says.

use kaspa_consensus_core::tx::Transaction;
use serde_json::{json, Map};

use crate::chain::Outpoint;
use crate::common::{
    address_of, check_economics_and_scripts, check_envelope, check_expiry, effective_finality, is_p2pk, outpoint_of, p2pk_key,
    parse_tx, payment_identifier, resolve_entries, signed_auth_digest, SignedAuth,
};
use crate::error::{Diag, Result, X402Error};
use crate::safe_tx::spk_to_hex;
use crate::verify::{PaymentKind, Verified, VerifyCtx, WatchedOutput};
use crate::wire::{
    parse_hash32, PaymentPayload, PaymentRequirements, Profile, ASSET_KAS, AUTH_VERSION_SIGNED, BINDING_EXACT, TX_ENCODING,
};

/// `extra` fields of the additive profile; they MUST be absent from a standard-native offer.
pub const ADDITIVE_EXTRA_KEYS: [&str; 10] = [
    "templateId",
    "headId",
    "headVersion",
    "expectedHeadOutpoint",
    "headAmount",
    "headScriptPublicKey",
    "headRedeemScript",
    "additiveThresholdSompi",
    "challengeId",
    "challengeExpiresAt",
];

/// Length of a canonical P2PK signature script: `OP_DATA_65 <64-byte Schnorr signature> <SIGHASH_ALL>`.
pub const P2PK_SIGSCRIPT_LEN: usize = 66;
/// The only signature hash type accepted on payer inputs. Anything else (NONE, SINGLE, ANYONECANPAY)
/// would let a third party alter the outputs of the verified transaction before it confirms.
pub const SIGHASH_ALL: u8 = 0x01;

/// Verifies a `standard-native` payment (see `crate::verify`).
///
/// Order: cheap envelope, profile and authorization checks first, then the artifact bounds and shape,
/// then the trusted chain lookup, the authorization signature, and finally the script engine, fee
/// and mass validation. Expiry is evaluated again after the chain work, right before returning.
pub fn verify_native(
    ctx: &VerifyCtx,
    offered: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
) -> Result<Verified> {
    // Binding step 1: the binding settles native KAS only.
    if offered.asset != ASSET_KAS {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Asset, "the standard-native profile settles asset KAS only"));
    }
    let env = check_envelope(ctx, offered, payload, request_hash)?;
    if env.profile != Profile::StandardNative {
        return Err(X402Error::requirements(Diag::UnsupportedKaspaExactProfile, "not a standard-native offer"));
    }
    if payload.payload.route.is_some() || offered.has_route() {
        return Err(X402Error::requirements(Diag::RouteUnsupported, "standard-native carries no route"));
    }
    if let Some(k) = ADDITIVE_EXTRA_KEYS.iter().find(|k| offered.extra.contains_key(**k)) {
        return Err(X402Error::requirements(
            Diag::InvalidKaspaX402Binding,
            format!("additive field extra.{k} must be absent from a standard-native offer"),
        ));
    }
    if payload.payload.challenge_id.is_some() {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "challengeId must be absent for standard-native"));
    }
    if env.amount < ctx.policy.limits.min_amount_sompi {
        return Err(X402Error::requirements(
            Diag::InvalidKaspaX402Amount,
            format!("amount {} is below the policy minimum {}", env.amount, ctx.policy.limits.min_amount_sompi),
        ));
    }
    let payment_id = payment_identifier(ctx, payload)?;

    let auth = &payload.payload.authorization;
    if auth.version != AUTH_VERSION_SIGNED {
        return Err(X402Error::payload(
            Diag::InvalidAuthorization,
            format!("standard-native requires authorization version {AUTH_VERSION_SIGNED}"),
        ));
    }
    // Early expiry decision (cheap, before any node call); repeated after the chain work below.
    check_expiry(ctx.clock.now_ms(), env.max_timeout_seconds, &auth.expires_at)?;

    // Binding steps 3 and 4: bounds, canonical decode, recomputed id.
    let parsed = parse_tx(ctx, payload)?;
    let tx = &parsed.tx;
    check_context(tx)?;
    let pay_idx = check_merchant_output(tx, &env.pay_to_spk, env.amount, payload.payload.payment_output_index)?;

    // Binding steps 5 and 6: trusted input resolution.
    let entries = resolve_entries(ctx, &parsed)?;

    // Every input is a standard Schnorr P2PK of the payer with a canonical SIGHASH_ALL signature script.
    for (i, (inp, e)) in tx.inputs.iter().zip(&entries).enumerate() {
        if !is_p2pk(&e.script_public_key) {
            return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, format!("input {i} is not a standard Schnorr P2PK")));
        }
        if e.covenant_id.is_some() {
            return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, format!("input {i} is covenant-bound")));
        }
        if inp.compute_commit.sig_op_count() != Some(1) {
            return Err(X402Error::payload(
                Diag::InvalidKaspaExactTransaction,
                format!("input {i} must commit exactly one sigop (version-0 mass field variant)"),
            ));
        }
        let s = &inp.signature_script;
        if s.len() != P2PK_SIGSCRIPT_LEN || s[0] != 0x41 || s[P2PK_SIGSCRIPT_LEN - 1] != SIGHASH_ALL {
            return Err(X402Error::payload(
                Diag::InvalidKaspaExactSignature,
                format!("input {i}: signature script is not a canonical Schnorr SIGHASH_ALL push"),
            ));
        }
    }
    // The optional other output returns change to the script of a verified payer input.
    for (i, o) in tx.outputs.iter().enumerate() {
        if i == pay_idx {
            continue;
        }
        if o.value == 0 {
            return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "the change output has value 0"));
        }
        if !entries.iter().any(|e| e.script_public_key == o.script_public_key) {
            return Err(X402Error::payload(
                Diag::InvalidKaspaExactPaymentOutput,
                "the other output does not return to the script of a payer input",
            ));
        }
    }

    // Binding step 11: request authorization by an authoritative funding input.
    let input_index =
        auth.input_index.ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "authorization.inputIndex is required"))?;
    let signer_input = entries
        .get(input_index as usize)
        .ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "authorization.inputIndex is out of range"))?;
    let signer = p2pk_key(&signer_input.script_public_key).expect("every input is P2PK (checked above)");
    let txid = tx.id().as_bytes();
    let digest = signed_auth_digest(&SignedAuth {
        network: env.network,
        profile: Profile::StandardNative,
        transaction_id: &txid,
        payment_output_index: payload.payload.payment_output_index,
        amount: &offered.amount,
        pay_to: &offered.pay_to,
        pay_to_spk_hex: &spk_to_hex(&env.pay_to_spk),
        requirements_hash: &env.requirements_hash,
        request_hash: &env.request_hash,
        challenge_id: None,
        input_index,
        expires_at: &auth.expires_at,
    })?;
    let claimed = parse_hash32(&auth.digest)
        .ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "authorization.digest is not 32-byte hex"))?;
    if claimed != digest {
        return Err(X402Error::payload(
            Diag::InvalidAuthorization,
            "authorization.digest differs from the digest the verifier recomputed",
        ));
    }
    let sig_hex = auth.signature.as_deref().unwrap_or("");
    let sig = kob_protocol::json::from_hex(sig_hex)
        .ok()
        .filter(|s| s.len() == 64)
        .ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "authorization.signature is not 64-byte hex"))?;
    if !verify_schnorr_digest(&sig, &digest, &signer) {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactSignature,
            "authorization signature does not verify under the key of the funding input",
        ));
    }

    // Binding steps 7, 9 and 10: script engine, fee, mass.
    let fee = check_economics_and_scripts(ctx, tx, &entries)?;

    // Expiry again, after the awaited chain work (binding "Expiry").
    let expires_ms = check_expiry(ctx.clock.now_ms(), env.max_timeout_seconds, &auth.expires_at)?;

    let finality = effective_finality(env.finality, ctx.policy);
    let mut ext = Map::new();
    ext.insert("binding".into(), json!(BINDING_EXACT));
    ext.insert("profile".into(), json!(Profile::StandardNative.as_str()));
    ext.insert("paymentOutputIndex".into(), json!(pay_idx));
    ext.insert("finality".into(), json!(finality.as_str()));
    ext.insert("transactionEncoding".into(), json!(TX_ENCODING));
    let merchant = &tx.outputs[pay_idx];
    Ok(Verified {
        kind: PaymentKind::Native,
        profile: Profile::StandardNative,
        txid,
        tx: tx.clone(),
        payer_address: address_of(&signer_input.script_public_key, env.network),
        amount: env.amount,
        payment_output_index: pay_idx as u32,
        merchant_output: WatchedOutput {
            outpoint: Outpoint::new(txid, pay_idx as u32),
            script_public_key: merchant.script_public_key.clone(),
            amount: merchant.value,
        },
        consumed: tx.inputs.iter().map(outpoint_of).collect(),
        order_inputs: vec![],
        fee,
        finality,
        custody: None,
        authorization_expires_at_ms: expires_ms,
        request_hash: env.request_hash,
        requirements_hash: env.requirements_hash,
        payment_identifier: payment_id,
        entries,
        response_extension: ext,
    })
}

/// Transaction context of the profile: version 0, native subnetwork (enforced by the decoder), gas 0,
/// lock time 0, empty payload, no output covenants, one or two outputs.
fn check_context(tx: &Transaction) -> Result<()> {
    let bad = |m: &str| Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, m.to_string()));
    if tx.version != 0 {
        return bad("standard-native uses transaction version 0");
    }
    if tx.gas != 0 {
        return bad("gas must be 0");
    }
    if tx.lock_time != 0 {
        return bad("lockTime must be 0");
    }
    if !tx.payload.is_empty() {
        return bad("the payload must be empty");
    }
    if tx.outputs.iter().any(|o| o.covenant.is_some()) {
        return bad("outputs must not carry covenant bindings");
    }
    if tx.outputs.is_empty() || tx.outputs.len() > 2 {
        return bad("a standard-native transaction has one merchant output and at most one other output");
    }
    Ok(())
}

/// Exactly one output pays `pay_to_spk`, at `paymentOutputIndex`, with exactly `amount`. Returns its index.
fn check_merchant_output(
    tx: &Transaction,
    pay_to_spk: &kaspa_consensus_core::tx::ScriptPublicKey,
    amount: u64,
    index: u32,
) -> Result<usize> {
    let hits: Vec<usize> = tx.outputs.iter().enumerate().filter(|(_, o)| &o.script_public_key == pay_to_spk).map(|(i, _)| i).collect();
    let idx = match hits.as_slice() {
        [i] => *i,
        [] => return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "no output pays payToScriptPublicKey")),
        _ => return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "more than one output pays payToScriptPublicKey")),
    };
    if idx != index as usize {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactPaymentOutput,
            format!("paymentOutputIndex {index} is not the merchant output ({idx})"),
        ));
    }
    let v = tx.outputs[idx].value;
    if v < amount {
        return Err(X402Error::payload(Diag::Underpayment, format!("merchant output pays {v}, the offer is {amount}")));
    }
    if v > amount {
        return Err(X402Error::payload(Diag::Overpayment, format!("merchant output pays {v}, the offer is {amount}")));
    }
    Ok(idx)
}

/// BIP340 verification of a 64-byte Schnorr signature over a 32-byte digest by an x-only key.
pub fn verify_schnorr_digest(sig64: &[u8], digest: &[u8; 32], key: &[u8; 32]) -> bool {
    let secp = secp256k1::Secp256k1::verification_only();
    let (Ok(sig), Ok(key)) = (secp256k1::schnorr::Signature::from_slice(sig64), secp256k1::XOnlyPublicKey::from_slice(key)) else {
        return false;
    };
    let msg = secp256k1::Message::from_digest(*digest);
    secp.verify_schnorr(&sig, &msg, &key).is_ok()
}

// ---------------------------------------------------------------------------- spec txid preimages

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_varbytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u64(out, b.len() as u64);
    out.extend_from_slice(b);
}

/// The transaction serialization shared by both id preimages ("Integer and byte serialization"):
/// signature scripts are always written as empty byte strings; `payload` is written as given
/// (v0: the real payload, v1: empty, its digest is hashed separately); version-1 outputs append a
/// covenant-presence byte and the binding (`u16 authorizingInput || 32-byte covenantId`).
fn serialize_for_id(tx: &Transaction, payload: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&tx.version.to_le_bytes());
    put_u64(&mut o, tx.inputs.len() as u64);
    for i in &tx.inputs {
        o.extend_from_slice(&i.previous_outpoint.transaction_id.as_bytes());
        o.extend_from_slice(&i.previous_outpoint.index.to_le_bytes());
        put_varbytes(&mut o, &[]);
        put_u64(&mut o, i.sequence);
    }
    put_u64(&mut o, tx.outputs.len() as u64);
    for out in &tx.outputs {
        put_u64(&mut o, out.value);
        o.extend_from_slice(&out.script_public_key.version().to_le_bytes());
        put_varbytes(&mut o, out.script_public_key.script());
        if tx.version >= 1 {
            match &out.covenant {
                None => o.push(0),
                Some(c) => {
                    o.push(1);
                    o.extend_from_slice(&c.authorizing_input.to_le_bytes());
                    o.extend_from_slice(&c.covenant_id.as_bytes());
                }
            }
        }
    }
    put_u64(&mut o, tx.lock_time);
    o.extend_from_slice(AsRef::<[u8]>::as_ref(&tx.subnetwork_id));
    put_u64(&mut o, tx.gas);
    put_varbytes(&mut o, payload);
    o
}

/// Version-0 transaction-id preimage of the binding ("Version 0 transaction id").
pub fn spec_txid_v0_preimage(tx: &Transaction) -> Vec<u8> {
    serialize_for_id(tx, &tx.payload)
}

/// Version-0 transaction id: BLAKE2b-256 keyed with the UTF-8 bytes `TransactionID` over the preimage.
pub fn spec_txid_v0(tx: &Transaction) -> [u8; 32] {
    let h = blake2b_simd::Params::new().hash_length(32).key(b"TransactionID").hash(&spec_txid_v0_preimage(tx));
    h.as_bytes().try_into().expect("32 bytes")
}

/// Every intermediate value of the version-1 transaction id ("Version 1 transaction id").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecTxidV1 {
    pub payload_digest: [u8; 32],
    pub rest_preimage: Vec<u8>,
    pub rest_digest: [u8; 32],
    /// `payloadDigest || restDigest`.
    pub preimage: [u8; 64],
    pub id: [u8; 32],
}

/// Keyed BLAKE3-256 whose 32-byte key is the ASCII domain in a zero-filled array.
fn blake3_domain(domain: &str, data: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..domain.len()].copy_from_slice(domain.as_bytes());
    *blake3::Hasher::new_keyed(&key).update(data).finalize().as_bytes()
}

/// Version-1 transaction id derivation of the binding, computed from the specification text.
pub fn spec_txid_v1(tx: &Transaction) -> SpecTxidV1 {
    let payload_digest = blake3_domain("PayloadDigest", &tx.payload);
    let rest_preimage = serialize_for_id(tx, &[]);
    let rest_digest = blake3_domain("TransactionRest", &rest_preimage);
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&payload_digest);
    preimage[32..].copy_from_slice(&rest_digest);
    let id = blake3_domain("TransactionV1Id", &preimage);
    SpecTxidV1 { payload_digest, rest_preimage, rest_digest, preimage, id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
    use kaspa_consensus_core::tx::{
        CovenantBinding, ScriptPublicKey, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput,
    };
    use kaspa_consensus_core::Hash;

    fn spk(b: u8) -> ScriptPublicKey {
        crate::testkit::p2pk_spk(&[b; 32])
    }

    #[test]
    fn spec_v0_id_matches_consensus() {
        let mk = |payload: Vec<u8>, lock: u64| {
            Transaction::new(
                0,
                vec![
                    TransactionInput::new(TransactionOutpoint::new(TransactionId::from_bytes([1; 32]), 7), vec![1, 2, 3], u64::MAX, 1),
                    TransactionInput::new(TransactionOutpoint::new(TransactionId::from_bytes([2; 32]), 0), vec![], 5, 1),
                ],
                vec![TransactionOutput::new(123, spk(3)), TransactionOutput::new(456, spk(4))],
                lock,
                SUBNETWORK_ID_NATIVE,
                0,
                payload,
            )
        };
        for tx in [mk(vec![], 0), mk(vec![9, 9, 9], 77)] {
            assert_eq!(spec_txid_v0(&tx), tx.id().as_bytes());
            assert_eq!(spec_txid_v0_preimage(&tx), kaspa_consensus_core::hashing::tx::transaction_v0_id_preimage(&tx));
        }
    }

    #[test]
    fn spec_v1_id_matches_consensus_with_covenants_and_payload() {
        let mut out = TransactionOutput::new(1_000, spk(5));
        out.covenant = Some(CovenantBinding { authorizing_input: 1, covenant_id: Hash::from_bytes([0xcc; 32]) });
        let tx = Transaction::new(
            1,
            vec![
                TransactionInput::new_with_compute_budget(
                    TransactionOutpoint::new(TransactionId::from_bytes([1; 32]), 1),
                    vec![1],
                    0,
                    10,
                ),
                TransactionInput::new_with_compute_budget(
                    TransactionOutpoint::new(TransactionId::from_bytes([2; 32]), 0),
                    vec![2],
                    1,
                    3,
                ),
            ],
            vec![out, TransactionOutput::new(456, spk(4))],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![7; 40],
        );
        let s = spec_txid_v1(&tx);
        assert_eq!(s.id, tx.id().as_bytes());
        assert_eq!(s.rest_preimage, kaspa_consensus_core::hashing::tx::transaction_v1_rest_preimage(&tx));
        assert_eq!(s.rest_digest, kaspa_consensus_core::hashing::tx::v1_rest_digest(&tx).as_bytes());
    }

    #[test]
    fn schnorr_helper_rejects_garbage() {
        let d = [7u8; 32];
        let sk = crate::testkit::secret(3);
        let sig = kob_protocol::tx::sign_digest(&sk, &d).unwrap();
        let pk = crate::testkit::pubkey(3);
        assert!(verify_schnorr_digest(&sig[..64], &d, &pk));
        assert!(!verify_schnorr_digest(&sig[..64], &[8u8; 32], &pk));
        assert!(!verify_schnorr_digest(&sig[..63], &d, &pk));
        assert!(!verify_schnorr_digest(&sig[..64], &d, &crate::testkit::pubkey(4)));
        assert!(!verify_schnorr_digest(&sig[..64], &d, &[0u8; 32]));
    }
}
