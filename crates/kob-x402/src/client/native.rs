//! Payer / merchant SDK for KAS `standard-native` payments (the elldeeone binding).
//!
//! Merchant side: [`native_requirements`] builds an offer (with the independently derived
//! `payToScriptPublicKey`), [`payment_required`] wraps offers into a `402` body that requires the
//! `payment-identifier` extension, and the `*_header` helpers encode / decode the three HTTP headers.
//!
//! Payer side: [`pay_native`] validates the offer against the payer's own limits, builds a version-0
//! transaction (one merchant output, optional change output back to the payer, no payload, fee at the
//! relay floor computed from the exact masses), signs every P2PK input with SIGHASH_ALL, signs the
//! request authorization digest with the funding key and returns the `PAYMENT-SIGNATURE` object.
//! [`preflight_native`] runs the verifier's own checks against a chain view before the payload is
//! disclosed, and [`revoke_native`] builds the self-spend that makes a disclosed but unsettled payment
//! unconfirmable (the payer-side kill switch for an ambiguous settlement).
//!
//! Nothing here broadcasts: the caller (or the facilitator) submits.

use std::collections::BTreeSet;

use kaspa_addresses::Address;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutput, UtxoEntry};
use kaspa_txscript::pay_to_address_script;
use kob_protocol::script::{p2pk_spk, push_data};
use kob_protocol::tx::{masses, pubkey_of, sighash, sign_digest, target_fee, FeeMode, KeyUtxo, MIN_FEE_RATE};
use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::canonical::requirements_hash;
use crate::chain::Outpoint;
use crate::common::{address_of, iso_from_ms, signed_auth_digest, SignedAuth};
use crate::error::{Result as X402Result, X402Error};
use crate::exact::{ADDITIVE_EXTRA_KEYS, P2PK_SIGSCRIPT_LEN};
use crate::safe_tx::{spk_to_hex, SafeTx};
use crate::verify::{Verified, VerifyCtx};
use crate::wire::{
    header_decode, header_encode, hex, parse_hash32, parse_u64_canonical, Authorization, ExactPayload, Finality, Network,
    PaymentPayload, PaymentRequired, PaymentRequirements, Profile, Resource, SettlementResponse, ASSET_KAS, AUTH_VERSION_SIGNED,
    BINDING_EXACT, PAYLOAD_EXACT_TX, SCHEME_EXACT, TX_ENCODING, X402_VERSION,
};

/// Extension key of the idempotency identifier.
pub const PAYMENT_IDENTIFIER_KEY: &str = "payment-identifier";

/// `schemas/payment-identifier.schema.json` of the binding (the extension carries its schema).
pub fn payment_identifier_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://kaspa-x402.org/schemas/payment-identifier.schema.json",
        "title": "Kaspa x402 Payment Identifier Extension Info",
        "type": "object",
        "required": ["required"],
        "properties": {
            "required": { "type": "boolean" },
            "id": { "type": "string", "minLength": 16, "maxLength": 128, "pattern": "^[A-Za-z0-9_-]+$" }
        },
        "additionalProperties": true
    })
}

/// Client-side failures (before anything is disclosed).
#[derive(Debug, Error)]
pub enum NativeError {
    #[error("invalid offer: {0}")]
    Offer(String),
    #[error("amount {amount} sompi is above the payer limit {max}")]
    AmountAboveLimit { amount: u64, max: u64 },
    #[error("insufficient funds: need {need} sompi, have {have}")]
    InsufficientFunds { need: u64, have: u64 },
    #[error("fee {fee} sompi is above the payer limit {max}")]
    FeeAboveLimit { fee: u64, max: u64 },
    #[error("cannot build the payment: {0}")]
    Build(String),
    #[error(transparent)]
    Verify(#[from] X402Error),
}

// ------------------------------------------------------------------------------------------ merchant

/// Builds a standard-native offer. `pay_to` is the merchant's address on `network`; the script
/// public key is derived from it here and re-derived independently by every verifier.
pub fn native_requirements(
    network: Network,
    amount: u64,
    pay_to: &str,
    max_timeout_seconds: u64,
    finality: Finality,
) -> Result<PaymentRequirements, NativeError> {
    if amount == 0 {
        return Err(NativeError::Offer("amount must be positive".into()));
    }
    if max_timeout_seconds == 0 || max_timeout_seconds > u32::MAX as u64 {
        return Err(NativeError::Offer("maxTimeoutSeconds must be a positive uint32".into()));
    }
    if finality < Finality::Accepted {
        return Err(NativeError::Offer("finality must be accepted or confirmed".into()));
    }
    let addr = Address::try_from(pay_to).map_err(|e| NativeError::Offer(format!("payTo: {e}")))?;
    if addr.prefix != network.prefix() {
        return Err(NativeError::Offer("payTo prefix does not match the network".into()));
    }
    let mut extra = Map::new();
    extra.insert("binding".into(), json!(BINDING_EXACT));
    extra.insert("profile".into(), json!(Profile::StandardNative.as_str()));
    extra.insert("finality".into(), json!(finality.as_str()));
    extra.insert("transactionEncoding".into(), json!(TX_ENCODING));
    extra.insert("payToScriptPublicKey".into(), json!(spk_to_hex(&pay_to_address_script(&addr))));
    Ok(PaymentRequirements {
        scheme: SCHEME_EXACT.into(),
        network: network.as_str().into(),
        amount: amount.to_string(),
        asset: ASSET_KAS.into(),
        pay_to: pay_to.into(),
        max_timeout_seconds,
        extra,
        other: Map::new(),
    })
}

/// The `402` body: the offers plus the `payment-identifier` extension marked required (the binding
/// requires it for exact).
pub fn payment_required(resource: Resource, accepts: Vec<PaymentRequirements>, error: Option<String>) -> PaymentRequired {
    PaymentRequired {
        x402_version: X402_VERSION,
        resource,
        accepts,
        error,
        extensions: Some(json!({
            PAYMENT_IDENTIFIER_KEY: { "info": { "required": true }, "schema": payment_identifier_schema() }
        })),
    }
}

/// `PAYMENT-REQUIRED` header value.
pub fn payment_required_header(pr: &PaymentRequired) -> X402Result<String> {
    header_encode(pr)
}
/// Parses a `PAYMENT-REQUIRED` header value.
pub fn parse_payment_required(header: &str) -> X402Result<PaymentRequired> {
    header_decode(header)
}
/// `PAYMENT-SIGNATURE` header value.
pub fn payment_signature_header(p: &PaymentPayload) -> X402Result<String> {
    header_encode(p)
}
/// Parses a `PAYMENT-SIGNATURE` header value.
pub fn parse_payment_signature(header: &str) -> X402Result<PaymentPayload> {
    header_decode(header)
}
/// `PAYMENT-RESPONSE` header value.
pub fn payment_response_header(r: &SettlementResponse) -> X402Result<String> {
    header_encode(r)
}
/// Parses a `PAYMENT-RESPONSE` header value.
pub fn parse_payment_response(header: &str) -> X402Result<SettlementResponse> {
    header_decode(header)
}

/// The success `SettlementResponse` of a verified (and, by the caller, observed) native payment:
/// the recomputed transaction id, network, payer, amount and `extensions.kaspa`.
pub fn settlement_response(v: &Verified, network: Network) -> SettlementResponse {
    SettlementResponse {
        success: true,
        error_reason: None,
        transaction: hex(&v.txid),
        network: Some(network.as_str().into()),
        payer: v.payer_address.clone(),
        amount: Some(v.amount.to_string()),
        extensions: Some(json!({ "kaspa": Value::Object(v.response_extension.clone()) })),
    }
}

// ---------------------------------------------------------------------------------------------- payer

/// Payer-side policy for one payment.
#[derive(Clone, Debug)]
pub struct PayOptions {
    /// The most the payer is willing to pay the merchant (pin this before signing).
    pub max_amount_sompi: u64,
    /// Fee rate in sompi per gram (at least the relay floor [`MIN_FEE_RATE`]).
    pub fee_rate: u64,
    /// [`FeeMode::Relay`] (default: the node's relay floor) or [`FeeMode::Priority`] (storage-inclusive).
    pub fee_mode: FeeMode,
    /// The most the payer is willing to pay in fee.
    pub max_fee_sompi: u64,
    /// Change below this is folded into the fee (a tiny change output inflates the storage mass).
    pub min_change_sompi: u64,
    /// Most inputs the transaction may spend.
    pub max_inputs: usize,
    /// Explicit `payment-identifier`; by default a fresh random one ([`random_payment_id`]). A retry of the same
    /// payment re-sends the stored payload (same id, same transaction); a new transaction takes a new id.
    pub payment_id: Option<String>,
    /// Echoed in `paymentPayload.resource`.
    pub resource: Option<Resource>,
    /// Authorization lifetime in seconds (default: the offer's `maxTimeoutSeconds`, which a merchant may set to ~136 years:
    /// payer SDKs pass a short one). Longer than `maxTimeoutSeconds` is refused by the verifier.
    pub ttl_seconds: Option<u64>,
}

impl PayOptions {
    /// Defaults: relay-floor fee rate, 0.5 KAS fee cap, 0.01 KAS minimum change, 32 inputs.
    pub fn new(max_amount_sompi: u64) -> Self {
        PayOptions {
            max_amount_sompi,
            fee_rate: MIN_FEE_RATE,
            fee_mode: FeeMode::Relay,
            max_fee_sompi: 50_000_000,
            min_change_sompi: 1_000_000,
            max_inputs: 32,
            payment_id: None,
            resource: None,
            ttl_seconds: None,
        }
    }
}

/// What a payer needs from an offer, after the payer's own checks.
struct Offer {
    network: Network,
    amount: u64,
    pay_to_spk: kaspa_consensus_core::tx::ScriptPublicKey,
}

fn check_offer(offer: &PaymentRequirements, max_amount: u64) -> Result<Offer, NativeError> {
    let bad = |m: &str| Err(NativeError::Offer(m.to_string()));
    if offer.scheme != SCHEME_EXACT {
        return bad("scheme must be exact");
    }
    if offer.asset != ASSET_KAS {
        return bad("asset must be KAS");
    }
    let network = Network::parse(&offer.network).ok_or_else(|| NativeError::Offer("unknown network".into()))?;
    let amount = match parse_u64_canonical(&offer.amount) {
        Some(a) if a > 0 => a,
        _ => return bad("amount is not a canonical positive uint64"),
    };
    if amount > max_amount {
        return Err(NativeError::AmountAboveLimit { amount, max: max_amount });
    }
    if offer.max_timeout_seconds == 0 || offer.max_timeout_seconds > u32::MAX as u64 {
        return bad("maxTimeoutSeconds must be a positive uint32");
    }
    if offer.extra_str("binding") != Some(BINDING_EXACT) {
        return bad("extra.binding must be kaspa-exact-v2");
    }
    if offer.profile().ok() != Some(Profile::StandardNative) {
        return bad("only the standard-native profile is supported");
    }
    if offer.extra_str("transactionEncoding") != Some(TX_ENCODING) {
        return bad("unsupported transactionEncoding");
    }
    if offer.has_route() || ADDITIVE_EXTRA_KEYS.iter().any(|k| offer.extra.contains_key(*k)) {
        return bad("route and additive fields are not part of standard-native");
    }
    offer.finality().map_err(|e| NativeError::Offer(e.message))?;
    let addr = Address::try_from(offer.pay_to.as_str()).map_err(|e| NativeError::Offer(format!("payTo: {e}")))?;
    if addr.prefix != network.prefix() {
        return bad("payTo prefix does not match the network");
    }
    let pay_to_spk = pay_to_address_script(&addr);
    if offer.extra_str("payToScriptPublicKey").map(str::to_ascii_lowercase) != Some(spk_to_hex(&pay_to_spk)) {
        return bad("extra.payToScriptPublicKey does not match payTo");
    }
    Ok(Offer { network, amount, pay_to_spk })
}

/// A fresh `payment-identifier` id: 24 random bytes (`pay_` + 48 hex). Nobody else can name it before the payer
/// discloses the payment, and every new transaction gets its own.
pub fn random_payment_id() -> String {
    let bytes: [u8; 24] = secp256k1::rand::random();
    format!("pay_{}", hex(&bytes))
}

/// The `paymentPayload.extensions` carrying the payment identifier.
pub fn payment_identifier_extension(id: &str) -> Value {
    json!({ PAYMENT_IDENTIFIER_KEY: { "info": { "required": true, "id": id }, "schema": payment_identifier_schema() } })
}

fn placeholder_sigscript() -> Vec<u8> {
    let mut s = vec![0u8; P2PK_SIGSCRIPT_LEN];
    s[0] = 0x41;
    s[P2PK_SIGSCRIPT_LEN - 1] = 0x01;
    s
}

/// A version-0 P2PK transaction with placeholder signature scripts and the exact committed storage mass.
fn skeleton(inputs: &[&KeyUtxo], outputs: Vec<TransactionOutput>) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> =
        inputs.iter().map(|k| UtxoEntry::new(k.utxo.amount, p2pk_spk(&k.pubkey), k.utxo.block_daa_score, false, None)).collect();
    let ins = inputs.iter().map(|k| TransactionInput::new(k.utxo.outpoint(), placeholder_sigscript(), u64::MAX, 1)).collect();
    let tx = Transaction::new(0, ins, outputs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    let m = masses(&tx, &entries);
    tx.set_storage_mass(m.storage);
    (tx, entries)
}

fn floor_fee(tx: &Transaction, entries: &[UtxoEntry], rate: u64, mode: FeeMode) -> u64 {
    target_fee(&masses(tx, entries), rate, mode)
}

/// Fixed point of `fee = floor(tx with `free = total - fixed - fee`)`: the smallest fee that covers the
/// relay floor of the transaction it leaves behind. `build(free)` returns the candidate transaction.
fn solve_fee(total: u64, fixed: u64, rate: u64, mode: FeeMode, build: &dyn Fn(u64) -> (Transaction, Vec<UtxoEntry>)) -> Option<u64> {
    let mut fee = 0u64;
    for _ in 0..64 {
        let free = total.checked_sub(fixed)?.checked_sub(fee)?;
        if free == 0 {
            return None;
        }
        let (tx, entries) = build(free);
        let f = floor_fee(&tx, &entries, rate, mode);
        if f <= fee {
            // lowering the fee only grows the free output, which cannot raise the floor
            let free = total.checked_sub(fixed)?.checked_sub(f)?;
            let (tx, entries) = build(free);
            return (floor_fee(&tx, &entries, rate, mode) <= f).then_some(f);
        }
        fee = f;
    }
    None
}

struct Built {
    tx: Transaction,
    entries: Vec<UtxoEntry>,
    fee: u64,
}

fn plan(inputs: &[&KeyUtxo], merchant: &Offer, opts: &PayOptions) -> Result<Built, NativeError> {
    let total: u64 = inputs
        .iter()
        .try_fold(0u64, |a, k| a.checked_add(k.utxo.amount))
        .ok_or_else(|| NativeError::Build("input total overflows".into()))?;
    let payer_spk = p2pk_spk(&inputs[0].pubkey);
    let pay = TransactionOutput::new(merchant.amount, merchant.pay_to_spk.clone());
    let with_change = |change: u64| skeleton(inputs, vec![pay.clone(), TransactionOutput::new(change, payer_spk.clone())]);
    // Variant A: change back to the payer.
    if let Some(fee) = solve_fee(total, merchant.amount, opts.fee_rate, opts.fee_mode, &with_change) {
        let change = total - merchant.amount - fee;
        if change >= opts.min_change_sompi.max(1) && fee <= opts.max_fee_sompi {
            let (tx, entries) = with_change(change);
            // the relay floor does not price storage mass: a change output so small that the
            // transaction exceeds the block storage limit is folded into the fee instead
            if masses(&tx, &entries).within_block_limits() {
                return Ok(Built { tx, entries, fee });
            }
        }
    }
    // Variant B: no change, the remainder is the fee.
    let (tx, entries) = skeleton(inputs, vec![pay]);
    let need = merchant.amount.saturating_add(floor_fee(&tx, &entries, opts.fee_rate, opts.fee_mode));
    if total < need {
        return Err(NativeError::InsufficientFunds { need, have: total });
    }
    let fee = total - merchant.amount;
    if fee > opts.max_fee_sompi {
        return Err(NativeError::FeeAboveLimit { fee, max: opts.max_fee_sompi });
    }
    if !masses(&tx, &entries).within_block_limits() {
        return Err(NativeError::Build(
            "the payment output is so small that the transaction exceeds the block storage mass limit".into(),
        ));
    }
    Ok(Built { tx, entries, fee })
}

/// Signs every input with SIGHASH_ALL and returns the signed transaction.
fn sign_inputs(mut tx: Transaction, entries: &[UtxoEntry], secret: &[u8; 32]) -> Result<Transaction, NativeError> {
    for i in 0..tx.inputs.len() {
        let digest = sighash(&tx, entries, i);
        let sig = sign_digest(secret, &digest).map_err(|e| NativeError::Build(e.to_string()))?;
        tx.inputs[i].signature_script = push_data(&sig);
    }
    Ok(tx)
}

/// Builds and signs the `PAYMENT-SIGNATURE` object for a standard-native offer.
///
/// * `request_hash` is the resource server's request fingerprint (32-byte hex) the authorization commits to;
/// * `utxos` are the payer's P2PK coins (all locked to the key of `payer_secret`);
/// * the authorization expires at `now_ms + opts.ttl_seconds` (default `maxTimeoutSeconds`).
///
/// The result is not broadcast. Run [`preflight_native`] before disclosing it.
pub fn pay_native(
    offer: &PaymentRequirements,
    request_hash: &str,
    payer_secret: &[u8; 32],
    utxos: &[KeyUtxo],
    now_ms: u64,
    opts: &PayOptions,
) -> Result<PaymentPayload, NativeError> {
    let o = check_offer(offer, opts.max_amount_sompi)?;
    if opts.fee_rate < MIN_FEE_RATE {
        return Err(NativeError::Build(format!("fee rate {} is below the relay floor {MIN_FEE_RATE}", opts.fee_rate)));
    }
    let request_hash_bytes = parse_hash32(request_hash).ok_or_else(|| NativeError::Build("requestHash is not 32-byte hex".into()))?;
    let payer_key = pubkey_of(payer_secret).map_err(|e| NativeError::Build(e.to_string()))?;
    if utxos.iter().any(|k| k.pubkey != payer_key) {
        return Err(NativeError::Build("every utxo must be locked to the payer key".into()));
    }
    let mut sorted: Vec<&KeyUtxo> = utxos.iter().collect();
    sorted.sort_by(|a, b| {
        b.utxo.amount.cmp(&a.utxo.amount).then((a.utxo.transaction_id, a.utxo.index).cmp(&(b.utxo.transaction_id, b.utxo.index)))
    });
    let mut seen = BTreeSet::new();
    sorted.retain(|k| seen.insert((k.utxo.transaction_id, k.utxo.index)));
    let have: u64 = sorted.iter().map(|k| k.utxo.amount).sum();

    let mut built = None;
    let mut last_err = NativeError::InsufficientFunds { need: o.amount, have };
    for k in 1..=sorted.len().min(opts.max_inputs) {
        match plan(&sorted[..k], &o, opts) {
            Ok(b) => {
                built = Some(b);
                break;
            }
            Err(e @ NativeError::InsufficientFunds { .. }) => last_err = e,
            Err(e) => return Err(e),
        }
    }
    let Built { tx, entries, fee } = built.ok_or(last_err)?;
    if fee > opts.max_fee_sompi {
        return Err(NativeError::FeeAboveLimit { fee, max: opts.max_fee_sompi });
    }
    let tx = sign_inputs(tx, &entries, payer_secret)?;
    kob_protocol::verify::validate(&tx, &entries).map_err(|e| NativeError::Build(format!("self-check failed: {e}")))?;

    let txid = tx.id().as_bytes();
    let ttl = opts.ttl_seconds.unwrap_or(offer.max_timeout_seconds);
    if ttl == 0 {
        return Err(NativeError::Build("ttlSeconds must be positive".into()));
    }
    let expires_at = iso_from_ms(now_ms.saturating_add(ttl.saturating_mul(1000)));
    let req_hash = requirements_hash(offer)?;
    let input_index = 0u32;
    let digest = signed_auth_digest(&SignedAuth {
        network: o.network,
        profile: Profile::StandardNative,
        transaction_id: &txid,
        payment_output_index: 0,
        amount: &offer.amount,
        pay_to: &offer.pay_to,
        pay_to_spk_hex: &spk_to_hex(&o.pay_to_spk),
        requirements_hash: &req_hash,
        request_hash: &request_hash_bytes,
        challenge_id: None,
        input_index,
        expires_at: &expires_at,
    })?;
    let mut sig = sign_digest(payer_secret, &digest).map_err(|e| NativeError::Build(e.to_string()))?;
    sig.truncate(64);
    let id = opts.payment_id.clone().unwrap_or_else(random_payment_id);
    if !(16..=128).contains(&id.len()) || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
        return Err(NativeError::Build("payment id must match ^[A-Za-z0-9_-]{16,128}$".into()));
    }
    Ok(PaymentPayload {
        x402_version: X402_VERSION,
        accepted: offer.clone(),
        payload: ExactPayload {
            kind: PAYLOAD_EXACT_TX.into(),
            profile: Profile::StandardNative.as_str().into(),
            payer_address: address_of(&entries[0].script_public_key, o.network),
            transaction: SafeTx::from_consensus(&tx, &entries).to_text(),
            transaction_encoding: TX_ENCODING.into(),
            payment_output_index: 0,
            request_hash: request_hash.to_ascii_lowercase(),
            challenge_id: None,
            authorization: Authorization {
                version: AUTH_VERSION_SIGNED.into(),
                input_index: Some(input_index),
                expires_at,
                digest: hex(&digest),
                signature: Some(hex(&sig)),
            },
            route: None,
        },
        resource: opts.resource.clone(),
        extensions: Some(payment_identifier_extension(&id)),
    })
}

/// Payer-side check before disclosure: the verifier's own logic against a chain view (trusted UTXOs,
/// fee and mass, scripts, authorization, expiry). A payload that fails here would fail at the
/// facilitator too.
pub fn preflight_native(
    ctx: &VerifyCtx,
    offer: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
) -> X402Result<Verified> {
    crate::exact::verify_native(ctx, offer, payload, request_hash)
}

/// A signed self-spend that invalidates a disclosed payment.
#[derive(Clone, Debug)]
pub struct Revocation {
    pub tx: Transaction,
    pub entries: Vec<UtxoEntry>,
    /// The outpoint of the payment that the revocation spends (whichever confirms first wins).
    pub spent: Outpoint,
    pub fee: u64,
}

/// Builds the payer-side revocation of a signed payment: spends its largest input back to the payer's
/// own key at the relay-floor fee. If it confirms, the payment can never confirm (the inputs are
/// shared); if the payment confirms first, the revocation is void and nothing is lost. The caller
/// submits it (for example through [`crate::chain::ChainView::submit`]).
pub fn revoke_native(
    payload: &PaymentPayload,
    payer_secret: &[u8; 32],
    fee_rate: u64,
    max_fee_sompi: u64,
) -> Result<Revocation, NativeError> {
    if fee_rate < MIN_FEE_RATE {
        return Err(NativeError::Build(format!("fee rate {fee_rate} is below the relay floor {MIN_FEE_RATE}")));
    }
    let safe = SafeTx::parse(&payload.payload.transaction, 1 << 20)?;
    let parsed = safe.to_consensus()?;
    let payer_key = pubkey_of(payer_secret).map_err(|e| NativeError::Build(e.to_string()))?;
    let payer_spk = p2pk_spk(&payer_key);
    // The inputs come from the payer's own artifact; the hints carry amount and script.
    let mut best: Option<(usize, u64)> = None;
    for (i, h) in parsed.hints.iter().enumerate() {
        let h = h.as_ref().ok_or_else(|| NativeError::Build("the payment carries no utxo hints".into()))?;
        if h.script_public_key == payer_spk && best.is_none_or(|(_, a)| h.amount > a) {
            best = Some((i, h.amount));
        }
    }
    let (idx, amount) = best.ok_or_else(|| NativeError::Build("no input of the payment is locked to the payer key".into()))?;
    let input = &parsed.tx.inputs[idx];
    let entry = UtxoEntry::new(amount, payer_spk.clone(), 0, false, None);
    let mk = |value: u64| {
        let tx = Transaction::new(
            0,
            vec![TransactionInput::new(input.previous_outpoint, placeholder_sigscript(), u64::MAX, 1)],
            vec![TransactionOutput::new(value, payer_spk.clone())],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let entries = vec![entry.clone()];
        tx.set_storage_mass(masses(&tx, &entries).storage);
        (tx, entries)
    };
    let fee =
        solve_fee(amount, 0, fee_rate, FeeMode::Relay, &mk).ok_or(NativeError::InsufficientFunds { need: fee_rate, have: amount })?;
    if fee > max_fee_sompi {
        return Err(NativeError::FeeAboveLimit { fee, max: max_fee_sompi });
    }
    let (tx, entries) = mk(amount - fee);
    let tx = sign_inputs(tx, &entries, payer_secret)?;
    kob_protocol::verify::validate(&tx, &entries).map_err(|e| NativeError::Build(format!("self-check failed: {e}")))?;
    Ok(Revocation {
        tx,
        entries,
        spent: Outpoint::new(input.previous_outpoint.transaction_id.as_bytes(), input.previous_outpoint.index),
        fee,
    })
}

/// The first offer of a `402` the payer can and will pay: standard-native, KAS, on `network`, within
/// `max_amount_sompi`, with a re-derivable recipient script.
pub fn select_native_offer(pr: &PaymentRequired, network: Network, max_amount_sompi: u64) -> Option<&PaymentRequirements> {
    pr.accepts.iter().find(|o| o.network == network.as_str() && check_offer(o, max_amount_sompi).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{p2pk_spk as tk_p2pk, pubkey, secret};
    use kob_protocol::tx::{min_fee, priority_fee};

    fn merchant_address(n: u8) -> String {
        Address::new(Network::Testnet10.prefix(), kaspa_addresses::Version::PubKey, &pubkey(n)).to_string()
    }

    fn utxo(op: u8, amount: u64, key: u8) -> KeyUtxo {
        KeyUtxo {
            utxo: kob_protocol::tx::Utxo { transaction_id: [op; 32], index: 0, amount, block_daa_score: 0, covenant_id: None },
            pubkey: pubkey(key),
        }
    }

    #[test]
    fn requirements_carry_the_derived_script() {
        let r = native_requirements(Network::Testnet10, 2_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        assert_eq!(r.extra_str("payToScriptPublicKey"), Some(spk_to_hex(&tk_p2pk(&pubkey(8))).as_str()));
        assert_eq!(r.extra_str("profile"), Some("standard-native"));
        assert!(check_offer(&r, u64::MAX).is_ok());
        assert!(native_requirements(Network::Mainnet, 1, &merchant_address(8), 60, Finality::Accepted).is_err());
        assert!(native_requirements(Network::Testnet10, 0, &merchant_address(8), 60, Finality::Accepted).is_err());
        assert!(native_requirements(Network::Testnet10, 1, &merchant_address(8), 0, Finality::Accepted).is_err());
        assert!(native_requirements(Network::Testnet10, 1, &merchant_address(8), 60, Finality::Mempool).is_err());
    }

    #[test]
    fn payer_refuses_bad_or_expensive_offers() {
        let good = native_requirements(Network::Testnet10, 2_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        assert!(matches!(check_offer(&good, 1_999_999), Err(NativeError::AmountAboveLimit { .. })));
        let mut bad = good.clone();
        bad.extra.insert("payToScriptPublicKey".into(), json!(spk_to_hex(&tk_p2pk(&pubkey(9)))));
        assert!(check_offer(&bad, u64::MAX).is_err());
        let mut bad = good.clone();
        bad.asset = "BTC".into();
        assert!(check_offer(&bad, u64::MAX).is_err());
        let mut bad = good.clone();
        bad.extra.insert("headId".into(), json!("00"));
        assert!(check_offer(&bad, u64::MAX).is_err());
        let mut bad = good.clone();
        bad.amount = "02000000".into();
        assert!(check_offer(&bad, u64::MAX).is_err());
        let mut bad = good;
        bad.network = "testnet-10".into();
        assert!(check_offer(&bad, u64::MAX).is_err());
    }

    #[test]
    fn default_payment_ids_are_random_and_well_formed() {
        let a = random_payment_id();
        assert_ne!(a, random_payment_id());
        assert!((16..=128).contains(&a.len()));
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'));
    }

    #[test]
    fn fee_is_the_relay_floor_with_change() {
        let offer = native_requirements(Network::Testnet10, 20_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        let o = check_offer(&offer, u64::MAX).unwrap();
        let k = utxo(0x70, 500_000_000, 7);
        let b = plan(&[&k], &o, &PayOptions::new(u64::MAX)).unwrap();
        assert_eq!(b.tx.outputs.len(), 2);
        let floor = min_fee(&masses(&b.tx, &b.entries), MIN_FEE_RATE);
        assert_eq!(b.fee, floor, "the default fee is exactly the node's relay floor");
        // and minimal: a hundred sompi less would fall below the floor of the resulting transaction
        let (t2, e2) = skeleton(
            &[&k],
            vec![
                TransactionOutput::new(20_000_000, o.pay_to_spk.clone()),
                TransactionOutput::new(500_000_000 - 20_000_000 - (b.fee - 100), tk_p2pk(&pubkey(7))),
            ],
        );
        assert!(b.fee - 100 < min_fee(&masses(&t2, &e2), MIN_FEE_RATE));
        assert_eq!(b.tx.outputs[0].value + b.tx.outputs[1].value + b.fee, 500_000_000);

        // the storage-inclusive fee is an explicit option: a 0.2 KAS output makes the storage mass
        // dominate, so it is far above the relay floor
        let mut opts = PayOptions::new(u64::MAX);
        opts.fee_mode = FeeMode::Priority;
        let p = plan(&[&k], &o, &opts).unwrap();
        let m = masses(&p.tx, &p.entries);
        assert_eq!(p.fee, priority_fee(&m, MIN_FEE_RATE));
        assert!(m.storage > m.fee_mass && p.fee > 10 * min_fee(&m, MIN_FEE_RATE), "{m:?}");
    }

    #[test]
    fn tiny_change_is_folded_into_the_fee_within_the_cap() {
        let offer = native_requirements(Network::Testnet10, 20_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        let o = check_offer(&offer, u64::MAX).unwrap();
        // the change (1.2 KAS - the 203600 floor) is below the 0.01 KAS minimum change
        let k = utxo(0x70, 20_000_000 + 1_200_000, 7);
        let mut opts = PayOptions::new(u64::MAX);
        opts.max_fee_sompi = 10_000_000;
        let b = plan(&[&k], &o, &opts).unwrap();
        assert_eq!(b.tx.outputs.len(), 1);
        assert_eq!(b.fee, 1_200_000);
        opts.max_fee_sompi = 1_000_000;
        assert!(matches!(plan(&[&k], &o, &opts), Err(NativeError::FeeAboveLimit { .. })));
        // a change above the minimum but so small that the storage mass exceeds the block limit
        // (the relay floor does not price it) is folded too
        let k = utxo(0x70, 20_000_000 + 1_700_000, 7);
        opts.max_fee_sompi = 10_000_000;
        let (with_change, entries) = skeleton(
            &[&k],
            vec![TransactionOutput::new(20_000_000, o.pay_to_spk.clone()), TransactionOutput::new(1_496_400, tk_p2pk(&pubkey(7)))],
        );
        assert!(!masses(&with_change, &entries).within_block_limits());
        let b = plan(&[&k], &o, &opts).unwrap();
        assert_eq!((b.tx.outputs.len(), b.fee), (1, 1_700_000));
        assert!(masses(&b.tx, &b.entries).within_block_limits());
    }

    #[test]
    fn insufficient_funds_and_foreign_keys_are_refused() {
        let offer = native_requirements(Network::Testnet10, 20_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        let rh = hex(&[9u8; 32]);
        let opts = PayOptions::new(u64::MAX);
        let small = utxo(0x70, 10_000_000, 7);
        assert!(matches!(pay_native(&offer, &rh, &secret(7), &[small], 1_000, &opts), Err(NativeError::InsufficientFunds { .. })));
        let foreign = utxo(0x70, 500_000_000, 6);
        assert!(matches!(pay_native(&offer, &rh, &secret(7), &[foreign], 1_000, &opts), Err(NativeError::Build(_))));
        assert!(matches!(pay_native(&offer, "zz", &secret(7), &[], 1_000, &opts), Err(NativeError::Build(_))));
        let mut low = PayOptions::new(u64::MAX);
        low.fee_rate = 1;
        assert!(matches!(pay_native(&offer, &rh, &secret(7), &[utxo(0x70, 500_000_000, 7)], 1_000, &low), Err(NativeError::Build(_))));
    }

    #[test]
    fn several_small_coins_are_combined() {
        let offer = native_requirements(Network::Testnet10, 20_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        let rh = hex(&[9u8; 32]);
        let coins = [utxo(0x70, 15_000_000, 7), utxo(0x71, 15_000_000, 7), utxo(0x72, 15_000_000, 7)];
        let p = pay_native(&offer, &rh, &secret(7), &coins, 1_000, &PayOptions::new(u64::MAX)).unwrap();
        let safe = SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap();
        assert!(safe.inputs.len() >= 2);
    }

    #[test]
    fn headers_round_trip() {
        let offer = native_requirements(Network::Testnet10, 20_000_000, &merchant_address(8), 60, Finality::Accepted).unwrap();
        let pr = payment_required(
            Resource { url: "https://api.example.test/x".into(), description: None, mime_type: None, other: Map::new() },
            vec![offer.clone()],
            None,
        );
        let h = payment_required_header(&pr).unwrap();
        assert_eq!(parse_payment_required(&h).unwrap(), pr);
        assert_eq!(payment_required_header(&parse_payment_required(&h).unwrap()).unwrap(), h);
        assert!(select_native_offer(&pr, Network::Testnet10, 20_000_000).is_some());
        assert!(select_native_offer(&pr, Network::Testnet10, 19_999_999).is_none());
        assert!(select_native_offer(&pr, Network::Mainnet, u64::MAX).is_none());
    }

    #[test]
    fn identifier_schema_matches_the_binding_file() {
        let file = include_str!("../../vectors/kaspa-x402-rc1/schemas/payment-identifier.schema.json");
        assert_eq!(payment_identifier_schema(), serde_json::from_str::<Value>(file).unwrap());
    }
}
