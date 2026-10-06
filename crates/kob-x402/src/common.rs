//! Checks shared by every profile: envelope, transaction bounds, trusted input resolution, fee / mass /
//! script validation, authorization expiry and the two request-authorization digests.
//!
//! The order of checks follows the binding's "Verification" list (`kaspa-exact-v2.md`):
//! 1 shape, 2 `payToScriptPublicKey` re-derivation, 3 size bounds, 4 canonical decode and id,
//! 5 trusted UTXO resolution, 6 hint comparison, 7 signatures / witnesses (script engine), 9 fee and mass,
//! 10 consensus-style validation, 11 request authorization.

use std::collections::BTreeSet;

use kaspa_addresses::Address;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kaspa_txscript::pay_to_address_script;
use serde_json::{json, Value};

use crate::canonical::{canonical_hash, canonical_json, requirements_hash, sha256};
use crate::chain::Outpoint;
use crate::error::{Diag, Reason, Result, X402Error};
use crate::safe_tx::{spk_to_hex, ParsedTx, SafeTx};
use crate::verify::VerifyCtx;
use crate::wire::{
    hex, parse_hash32, Finality, Network, PaymentPayload, PaymentRequirements, Profile, AUTH_VERSION_PAYLOAD, AUTH_VERSION_SIGNED,
    BINDING_EXACT, BINDING_SWAP, PAYLOAD_EXACT_TX, SCHEME_EXACT, TX_ENCODING, X402_VERSION,
};

/// Facts every profile needs from the validated envelope.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub network: Network,
    /// `amount` as an integer (sompi or token base units).
    pub amount: u64,
    pub profile: Profile,
    /// Finality the offer asks for (`accepted` or `confirmed`).
    pub finality: Finality,
    pub pay_to: Address,
    /// Serialized script public key of `payTo`, re-derived from the address.
    pub pay_to_spk: ScriptPublicKey,
    pub requirements_hash: [u8; 32],
    pub request_hash: [u8; 32],
    pub max_timeout_seconds: u64,
}

fn same_json(a: &impl serde::Serialize, b: &impl serde::Serialize) -> Result<bool> {
    let a = serde_json::to_value(a).map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, e.to_string()))?;
    let b = serde_json::to_value(b).map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, e.to_string()))?;
    Ok(canonical_json(&a)? == canonical_json(&b)?)
}

/// Validates the envelope (binding steps 1 and 2 and the request-hash rule) and returns its facts.
///
/// * `offered` is the requirement the resource server holds (from its own `accepts`); `payload.accepted`
///   must equal it (canonical JSON), so a payer cannot change amount, recipient, asset or profile;
/// * `request_hash_hex` is the resource server's independently computed fingerprint; it must equal
///   `payload.requestHash` (the facilitator never infers it from the payload).
pub fn check_envelope(
    ctx: &VerifyCtx,
    offered: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash_hex: &str,
) -> Result<Envelope> {
    if payload.x402_version != X402_VERSION {
        return Err(X402Error::new(Reason::InvalidX402Version, Diag::InvalidKaspaX402Payload, "unsupported x402 version"));
    }
    if !same_json(&payload.accepted, offered)? {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Accepted, "accepted requirements differ from the server offer"));
    }
    if offered.scheme != SCHEME_EXACT {
        return Err(X402Error::new(Reason::InvalidScheme, Diag::InvalidKaspaX402Binding, "scheme must be exact"));
    }
    let network = offered.network()?;
    if network != ctx.policy.network {
        return Err(X402Error::new(
            Reason::InvalidNetwork,
            Diag::InvalidKaspaX402Binding,
            format!("this verifier serves {} only", ctx.policy.network),
        ));
    }
    let amount = offered.amount_u64()?;
    if offered.max_timeout_seconds == 0 || offered.max_timeout_seconds > u32::MAX as u64 {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "maxTimeoutSeconds must be a positive uint32"));
    }
    if offered.extra_str("binding") != Some(BINDING_EXACT) {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "extra.binding must be kaspa-exact-v2"));
    }
    let profile = offered.profile()?;
    if payload.payload.profile != profile.as_str() {
        return Err(X402Error::payload(Diag::UnsupportedKaspaExactProfile, "payload.profile differs from the offered profile"));
    }
    let finality = offered.finality()?;
    if offered.extra_str("transactionEncoding") != Some(TX_ENCODING) || payload.payload.transaction_encoding != TX_ENCODING {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "transactionEncoding must be kaspa-sdk-safe-json-v2.0.0"));
    }
    if payload.payload.kind != PAYLOAD_EXACT_TX {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "payload.type must be exact-transaction"));
    }
    // payTo -> script public key, independently.
    let pay_to = Address::try_from(offered.pay_to.as_str())
        .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo is not a Kaspa address: {e}")))?;
    if pay_to.prefix != network.prefix() {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "payTo prefix does not match the network"));
    }
    let pay_to_spk = pay_to_address_script(&pay_to);
    let claimed = offered.extra_str("payToScriptPublicKey").unwrap_or("").to_ascii_lowercase();
    if claimed != spk_to_hex(&pay_to_spk) {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "extra.payToScriptPublicKey does not match payTo"));
    }
    // requestHash: the caller's independently computed value must equal the payload's.
    let request_hash = parse_hash32(request_hash_hex)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, "requestHash is not 32-byte hex"))?;
    let embedded = parse_hash32(&payload.payload.request_hash)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, "payload.requestHash is not 32-byte hex"))?;
    if embedded != request_hash {
        return Err(X402Error::payload(
            Diag::InvalidKaspaX402Payload,
            "payload.requestHash differs from the resource server's requestHash",
        ));
    }
    Ok(Envelope {
        network,
        amount,
        profile,
        finality,
        pay_to,
        pay_to_spk,
        requirements_hash: requirements_hash(offered)?,
        request_hash,
        max_timeout_seconds: offered.max_timeout_seconds,
    })
}

/// Parses the transaction artifact under the resource bounds (binding steps 3 and 4). The transaction
/// id is recomputed by the consensus library; a stated `id` must match.
pub fn parse_tx(ctx: &VerifyCtx, payload: &PaymentPayload) -> Result<ParsedTx> {
    let lim = &ctx.policy.limits;
    let safe = SafeTx::parse(&payload.payload.transaction, lim.max_tx_json_bytes)?;
    if safe.inputs.is_empty() || safe.inputs.len() > lim.max_inputs {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, format!("input count must be 1..={}", lim.max_inputs)));
    }
    if safe.outputs.is_empty() || safe.outputs.len() > lim.max_outputs {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, format!("output count must be 1..={}", lim.max_outputs)));
    }
    if safe.payload.len() / 2 > lim.max_payload_bytes {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "transaction payload exceeds the bound"));
    }
    if safe.inputs.iter().any(|i| i.signature_script.len() / 2 > lim.max_signature_script_bytes) {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "signature script exceeds the bound"));
    }
    let parsed = safe.to_consensus()?;
    let mut seen = BTreeSet::new();
    for i in &parsed.tx.inputs {
        if !seen.insert(outpoint_of(i)) {
            return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "an outpoint is spent twice"));
        }
    }
    Ok(parsed)
}

/// Outpoint an input spends.
pub fn outpoint_of(i: &kaspa_consensus_core::tx::TransactionInput) -> Outpoint {
    Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index)
}

/// Resolves every input from the trusted chain view and compares the embedded hints (binding steps 5
/// and 6). An input that is not in the UTXO set is spent or unknown: `invalid_transaction_state`.
pub fn resolve_entries(ctx: &VerifyCtx, parsed: &ParsedTx) -> Result<Vec<UtxoEntry>> {
    let mut wanted = Vec::with_capacity(parsed.tx.inputs.len());
    for (i, (inp, hint)) in parsed.tx.inputs.iter().zip(&parsed.hints).enumerate() {
        let hint = hint.as_ref().ok_or_else(|| {
            X402Error::payload(
                Diag::InvalidKaspaExactUtxo,
                format!("input {i} carries no embedded utxo (needed to locate it on chain)"),
            )
        })?;
        wanted.push((outpoint_of(inp), hint.script_public_key.clone()));
    }
    let found = ctx
        .chain
        .utxos(&wanted)
        .map_err(|e| X402Error::new(Reason::UnexpectedSettleError, Diag::NodeUnavailable, e.to_string()).retryable())?;
    let mut entries = Vec::with_capacity(found.len());
    for (i, ((op, _), u)) in wanted.iter().zip(found).enumerate() {
        let u = u.ok_or_else(|| {
            X402Error::state(Diag::InvalidKaspaExactUtxo, format!("input {i} ({op}) is not an unspent output of the claimed script"))
                .with_details(json!({ "outpoint": op.to_json(), "input": i }))
        })?;
        let hint = parsed.hints[i].as_ref().expect("checked above");
        if hint.amount != u.amount || hint.covenant_id != u.covenant_id {
            return Err(X402Error::payload(Diag::InvalidKaspaExactUtxo, format!("input {i}: embedded utxo differs from the chain")));
        }
        entries.push(u.to_entry());
    }
    Ok(entries)
}

/// Fee, mass and script validation (binding steps 7, 9 and 10). Returns the fee.
pub fn check_economics_and_scripts(ctx: &VerifyCtx, tx: &Transaction, entries: &[UtxoEntry]) -> Result<u64> {
    let total_in: u128 = entries.iter().map(|e| e.amount as u128).sum();
    let total_out: u128 = tx.outputs.iter().map(|o| o.value as u128).sum();
    if total_out > total_in {
        return Err(X402Error::payload(Diag::InvalidKaspaExactFee, "outputs exceed inputs"));
    }
    let fee = (total_in - total_out) as u64;
    if fee > ctx.policy.limits.max_fee_sompi {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactFee,
            format!("fee {fee} exceeds the policy bound {}", ctx.policy.limits.max_fee_sompi),
        ));
    }
    let mass = kob_protocol::tx::masses(tx, entries);
    if tx.storage_mass() != mass.storage {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactMass,
            format!("committed storage mass {} differs from the computed {}", tx.storage_mass(), mass.storage),
        ));
    }
    if !mass.within_block_limits() {
        return Err(X402Error::payload(Diag::InvalidKaspaExactMass, "transaction mass exceeds the block limits"));
    }
    let min = kob_protocol::tx::min_fee(&mass, kob_protocol::tx::MIN_FEE_RATE);
    if fee < min {
        return Err(X402Error::payload(Diag::InvalidKaspaExactFee, format!("fee {fee} is below the relay floor {min}")));
    }
    kob_protocol::verify::validate(tx, entries).map_err(|e| {
        let msg = e.to_string();
        // "engine: input N: <script error>": a failing plain P2PK input is a signature failure.
        let diag = match msg.split("input ").nth(1).and_then(|r| r.split(':').next()).and_then(|n| n.trim().parse::<usize>().ok()) {
            Some(n) if entries.get(n).is_some_and(|en| is_p2pk(&en.script_public_key)) => Diag::InvalidKaspaExactSignature,
            _ => Diag::InvalidKaspaExactTransaction,
        };
        X402Error::payload(diag, msg)
    })?;
    Ok(fee)
}

/// True for a Schnorr P2PK script (`OP_DATA_32 <x-only key> OP_CHECKSIG`).
pub fn is_p2pk(spk: &ScriptPublicKey) -> bool {
    spk.version() == 0 && spk.script().len() == 34 && spk.script()[0] == 0x20 && spk.script()[33] == 0xac
}

/// The x-only key of a P2PK script.
pub fn p2pk_key(spk: &ScriptPublicKey) -> Option<[u8; 32]> {
    is_p2pk(spk).then(|| spk.script()[1..33].try_into().expect("32 bytes"))
}

// ------------------------------------------------------------------------------------------ expiry

/// Parses `YYYY-MM-DDTHH:MM:SS(.mmm)?Z` (UTC) into unix milliseconds.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u64> {
        let part = b.get(r)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    if b.len() != 20 && b.len() != 24 {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || *b.last()? != b'Z' {
        return None;
    }
    let (y, mo, d, h, mi, sec) = (digits(0..4)?, digits(5..7)?, digits(8..10)?, digits(11..13)?, digits(14..16)?, digits(17..19)?);
    let ms = if b.len() == 24 {
        if b[19] != b'.' {
            return None;
        }
        digits(20..23)?
    } else {
        0
    };
    if !(1..=12).contains(&mo) || d == 0 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(mo - 1) as usize];
    if d > dim {
        return None;
    }
    // days from civil (Howard Hinnant)
    let (yy, m) = if mo <= 2 { (y as i64 - 1, mo as i64 + 9) } else { (y as i64, mo as i64 - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * m + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    Some(((days as u64 * 24 + h) * 60 + mi) * 60_000 + sec * 1_000 + ms)
}

/// Authorization expiry rule (`Expiry`): strictly after `now` and no later than `now + maxTimeoutSeconds`.
/// Returns the expiry in unix ms.
pub fn check_expiry(now_ms: u64, max_timeout_seconds: u64, expires_at: &str) -> Result<u64> {
    let exp = parse_iso_ms(expires_at).ok_or_else(|| {
        X402Error::payload(Diag::InvalidAuthorization, "authorization.expiresAt is not a millisecond ISO-8601 UTC timestamp")
    })?;
    if exp <= now_ms {
        return Err(X402Error::state(Diag::ExpiredAuthorization, "the payment authorization has expired"));
    }
    if exp > now_ms.saturating_add(max_timeout_seconds.saturating_mul(1_000)) {
        return Err(X402Error::payload(
            Diag::AuthorizationExceedsMaxTimeout,
            "authorization expires later than maxTimeoutSeconds allows",
        ));
    }
    Ok(exp)
}

/// Formats unix milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso_from_ms(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let rem = ms % 86_400_000;
    let (h, mi, s, milli) = (rem / 3_600_000, rem / 60_000 % 60, rem / 1_000 % 60, rem % 1_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

// -------------------------------------------------------------------------------- authorization digests

/// Inputs of the binding's Schnorr-signed request authorization digest (`kaspa-x402-exact-request-authorization-v1`).
pub struct SignedAuth<'a> {
    pub network: Network,
    pub profile: Profile,
    pub transaction_id: &'a [u8; 32],
    pub payment_output_index: u32,
    pub amount: &'a str,
    pub pay_to: &'a str,
    pub pay_to_spk_hex: &'a str,
    pub requirements_hash: &'a [u8; 32],
    pub request_hash: &'a [u8; 32],
    pub challenge_id: Option<&'a str>,
    pub input_index: u32,
    pub expires_at: &'a str,
}

/// The canonical JSON object whose SHA-256 the payer signs.
pub fn signed_auth_object(a: &SignedAuth) -> Value {
    json!({
        "scope": AUTH_VERSION_SIGNED,
        "network": a.network.as_str(),
        "profile": a.profile.as_str(),
        "transactionId": hex(a.transaction_id),
        "paymentOutputIndex": a.payment_output_index,
        "amount": a.amount,
        "payTo": a.pay_to,
        "payToScriptPublicKey": a.pay_to_spk_hex.to_ascii_lowercase(),
        "paymentRequirementsHash": hex(a.requirements_hash),
        "requestHash": hex(a.request_hash),
        "challengeId": a.challenge_id.map(|c| c.to_ascii_lowercase()),
        "inputIndex": a.input_index,
        "expiresAt": a.expires_at,
    })
}

/// SHA-256 digest of [`signed_auth_object`].
pub fn signed_auth_digest(a: &SignedAuth) -> Result<[u8; 32]> {
    canonical_hash(&signed_auth_object(a))
}

/// Inputs of the KOB payload-commitment digest (`kob-x402-payload-commitment-v1`), used where the
/// paying authority is not a single P2PK signer (KCC-20 owner witnesses, swap-and-pay). It commits to
/// everything the signed digest does except the transaction id and the input index (the digest is
/// embedded in the transaction payload, so it cannot contain the id), and every input authorizer's
/// SIGHASH_ALL covers the payload.
pub struct PayloadCommit<'a> {
    pub network: Network,
    pub profile: Profile,
    /// Swap-and-pay: the covenant id the payer pays with; `None` for a direct token payment.
    pub route_pay_asset: Option<&'a str>,
    pub asset: &'a str,
    pub amount: &'a str,
    pub pay_to: &'a str,
    pub pay_to_spk_hex: &'a str,
    pub payment_output_index: u32,
    pub requirements_hash: &'a [u8; 32],
    pub request_hash: &'a [u8; 32],
    pub expires_at: &'a str,
}

/// The canonical JSON object behind the payload commitment.
pub fn payload_commit_object(c: &PayloadCommit) -> Value {
    json!({
        "scope": AUTH_VERSION_PAYLOAD,
        "network": c.network.as_str(),
        "profile": c.profile.as_str(),
        "route": c.route_pay_asset.map(|a| json!({ "binding": BINDING_SWAP, "payAsset": a.to_ascii_lowercase() })),
        "asset": c.asset,
        "amount": c.amount,
        "payTo": c.pay_to,
        "payToScriptPublicKey": c.pay_to_spk_hex.to_ascii_lowercase(),
        "paymentOutputIndex": c.payment_output_index,
        "paymentRequirementsHash": hex(c.requirements_hash),
        "requestHash": hex(c.request_hash),
        "expiresAt": c.expires_at,
    })
}

/// SHA-256 digest of [`payload_commit_object`].
pub fn payload_commit_digest(c: &PayloadCommit) -> Result<[u8; 32]> {
    canonical_hash(&payload_commit_object(c))
}

/// The transaction payload that carries a commitment: a `KOB1` payload with one `X402` record holding
/// the 32-byte digest.
pub fn commitment_payload(digest: &[u8; 32]) -> Vec<u8> {
    kob_protocol::payload::encode(&[kob_protocol::payload::Record::X402 { reference: digest.to_vec() }])
        .expect("32-byte reference encodes")
}

/// Checks that the transaction payload commits to `digest`: a non-legacy `KOB1` payload with exactly
/// one `X402` record equal to the digest, and no other record except notes.
pub fn check_payload_commitment(tx: &Transaction, digest: &[u8; 32]) -> Result<()> {
    use kob_protocol::payload::{decode, Record};
    let p = decode(&tx.payload)
        .map_err(|e| X402Error::payload(Diag::InvalidAuthorization, format!("transaction payload: {e}")))?
        .ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "transaction payload is not a KOB1 payload"))?;
    if p.legacy {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "the legacy X402 text payload cannot carry a commitment"));
    }
    let mut found = false;
    for r in &p.records {
        match r {
            Record::X402 { reference } => {
                if found || reference.as_slice() != digest {
                    return Err(X402Error::payload(
                        Diag::InvalidAuthorization,
                        "the payment reference does not match the authorization digest",
                    ));
                }
                found = true;
            }
            Record::Note { .. } => {}
            _ => return Err(X402Error::payload(Diag::InvalidAuthorization, "a payment transaction carries no order records")),
        }
    }
    if !found {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "the transaction payload does not commit to the authorization"));
    }
    Ok(())
}

/// SHA-256 of the requirements' canonical JSON as lowercase hex (convenience for SDKs).
pub fn requirements_hash_hex(req: &PaymentRequirements) -> Result<String> {
    Ok(hex(&requirements_hash(req)?))
}

/// SHA-256 of raw bytes as hex.
pub fn sha256_hex(b: &[u8]) -> String {
    hex(&sha256(b))
}

/// Payer receipt address: the address of a P2PK / P2SH script public key on `network`.
pub fn address_of(spk: &ScriptPublicKey, network: Network) -> Option<String> {
    kaspa_txscript::extract_script_pub_key_address(spk, network.prefix()).ok().map(|a| a.to_string())
}

/// Effective required finality: the offer's, raised to the policy minimum.
pub fn effective_finality(offered: Finality, policy: &crate::policy::Policy) -> Finality {
    offered.max(policy.min_finality)
}

/// The `payment-identifier` extension of a payload (`paymentPayload.extensions["payment-identifier"].info`):
/// `{ "required": bool, "id": "^[A-Za-z0-9_-]{16,128}$" }`. When the policy requires the extension
/// (the binding requires it for exact) a missing or malformed id fails; otherwise a malformed id is
/// still an error but a missing one is `None`.
pub fn payment_identifier(ctx: &VerifyCtx, payload: &PaymentPayload) -> Result<Option<String>> {
    let id =
        payload.extensions.as_ref().and_then(|e| e.get("payment-identifier")).and_then(|e| e.get("info")).and_then(|i| i.get("id"));
    match id {
        None if ctx.policy.require_payment_identifier => Err(X402Error::payload(
            Diag::MissingKaspaPaymentIdentifier,
            "the payment-identifier extension with an id is required for exact",
        )),
        None => Ok(None),
        Some(Value::String(s))
            if (16..=128).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') =>
        {
            Ok(Some(s.clone()))
        }
        Some(_) => {
            Err(X402Error::payload(Diag::InvalidKaspaPaymentIdentifier, "payment-identifier id must match ^[A-Za-z0-9_-]{16,128}$"))
        }
    }
}
