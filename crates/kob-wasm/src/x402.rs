//! x402 bindings: JSON in / JSON out over the `kob-x402` payer and merchant SDK (native KAS, KCC-20,
//! swap-and-pay), its preflight verifier, revocation and the hashes / digests of the binding.
//!
//! Conventions of `kob-protocol` apply: 64-bit integers are decimal strings (numbers are accepted on
//! input), bytes are lowercase hex, transactions are kaspa-wasm safe JSON. Nothing here reads a clock
//! or does I/O: `nowMs` and the chain snapshot are arguments, so the module runs on
//! `wasm32-unknown-unknown` (the SDK's `SystemClock` is never touched).
//!
//! Errors are `String`s holding a JSON object `{ "reason", "diagnostic", "message", "retryable",
//! "details"? }` (the `extensions.kaspa` shape) so the JS side can branch on the diagnostic.
//!
//! Wallet flow: `x402BuildKcc20Unsigned` / `x402FinishKcc20` and `x402PrepareSwap` / `x402FinishSwap`
//! return the unsigned transaction with its signing requests (`built.sign`), the wallet signs, and
//! only signatures come back. Native KAS payments sign the request authorization digest with the
//! funding key (a raw Schnorr signature over a digest, which browser wallets do not offer), so
//! `x402PayNative` takes a local key; the KCC-20 and swap profiles commit the digest in the transaction
//! payload instead and work with any wallet that signs the transaction.

use std::collections::BTreeMap;

use kob_protocol::artifacts::token_template_by_hash;
use kob_protocol::json::{hex32, to_hex};
use kob_protocol::registry::{Family, Registry};
use kob_protocol::tx::{pubkey_of, BuiltTx, InputSignature, KeyUtxo, TokenUtxo, TxJson, Utxo};
use kob_x402::canonical::{canonical_hash, canonical_json, http_request_hash, sha256};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::client::native::{self, PayOptions};
use kob_x402::client::swap::{self, MerchantGain, PayAssetSpec, PayBound, PayerFunds, PreparedSwap, Quote, SwapOptions, SwapPayment};
use kob_x402::client::token::{self, Kcc20Options, PayloadTemplate};
use kob_x402::common::{self, parse_iso_ms, PayloadCommit, SignedAuth};
use kob_x402::error::X402Error;
use kob_x402::policy::{AllowedToken, Custody, Limits, Policy};
use kob_x402::safe_tx::{spk_from_hex, spk_to_hex, SafeTx};
use kob_x402::verify::{verify_payment, VerifyCtx};
use kob_x402::wire::{
    hex, parse_u64_canonical, Finality, Network, OutpointJson, PaymentPayload, PaymentRequirements, Profile, Resource,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

type R<T> = Result<T, String>;

const MAX_TX_JSON: usize = 4 << 20;

// ------------------------------------------------------------------------------------------------ plumbing

fn parse<T: serde::de::DeserializeOwned>(what: &str, json: &str) -> R<T> {
    serde_json::from_str(json).map_err(|e| perr(format!("{what}: {e}")))
}

fn out<T: Serialize>(v: &T) -> R<String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

/// A non-verifier failure (bad request shape, bad key): reported with the `invalid_payload` reason.
fn perr(message: impl Into<String>) -> String {
    json!({ "reason": "invalid_payload", "diagnostic": "invalid_kaspa_x402_payload", "message": message.into(), "retryable": false })
        .to_string()
}

fn xerr(e: X402Error) -> String {
    let mut o = Map::new();
    o.insert("reason".into(), json!(e.reason.as_str()));
    o.insert("diagnostic".into(), json!(e.diag.as_str()));
    o.insert("message".into(), json!(e.message));
    o.insert("retryable".into(), json!(e.retryable));
    if let Some(d) = e.details {
        o.insert("details".into(), d);
    }
    Value::Object(o).to_string()
}

fn nerr(e: native::NativeError) -> String {
    match e {
        native::NativeError::Verify(x) => xerr(x),
        other => json!({
            "reason": "invalid_payment_requirements",
            "diagnostic": match &other {
                native::NativeError::Offer(_) => "invalid_kaspa_x402_binding",
                native::NativeError::AmountAboveLimit { .. } => "overpayment",
                native::NativeError::InsufficientFunds { .. } => "invalid_kaspa_exact_utxo",
                native::NativeError::FeeAboveLimit { .. } => "invalid_kaspa_exact_fee",
                native::NativeError::SpendAboveLimit { .. } => "overpayment",
                _ => "invalid_kaspa_exact_transaction",
            },
            "message": other.to_string(),
            "retryable": false,
        })
        .to_string(),
    }
}

fn h32(what: &str, s: &str) -> R<[u8; 32]> {
    hex32(s).map_err(|e| perr(format!("{what}: {e}")))
}

fn net(s: &str) -> R<Network> {
    Network::parse(s).ok_or_else(|| perr(format!("unknown network {s:?}")))
}

fn fin(s: &str) -> R<Finality> {
    Finality::parse(s).ok_or_else(|| perr(format!("unknown finality {s:?}")))
}

fn u64s(what: &str, s: &str) -> R<u64> {
    parse_u64_canonical(s).ok_or_else(|| perr(format!("{what} {s:?} is not a canonical uint64")))
}

/// A `u64` given as a decimal string or a JSON number.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Num {
    S(String),
    N(u64),
}

impl Num {
    fn get(self, what: &str) -> R<u64> {
        match self {
            Num::N(n) => Ok(n),
            Num::S(s) => s.parse().map_err(|e| perr(format!("{what}: {e}"))),
        }
    }
}

fn secrets_of(keys: &[String]) -> R<BTreeMap<[u8; 32], [u8; 32]>> {
    let mut m = BTreeMap::new();
    for k in keys {
        let sk = h32("secret key", k)?;
        let pk = pubkey_of(&sk).map_err(|e| perr(e.to_string()))?;
        m.insert(pk, sk);
    }
    if m.is_empty() {
        return Err(perr("at least one secret key is required"));
    }
    Ok(m)
}

// ------------------------------------------------------------------------------------------------ wire structs

/// A KAS UTXO as the SDK's chain context reports it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayerUtxo {
    txid: String,
    index: u32,
    amount: Value,
    #[serde(default)]
    script_public_key: Option<String>,
    #[serde(default)]
    block_daa_score: Option<Value>,
    #[serde(default)]
    covenant_id: Option<String>,
}

fn value_u64(what: &str, v: &Value) -> R<u64> {
    match v {
        Value::String(s) => s.parse().map_err(|e| perr(format!("{what}: {e}"))),
        Value::Number(n) => n.as_u64().ok_or_else(|| perr(format!("{what}: out of range"))),
        _ => Err(perr(format!("{what}: expected an integer"))),
    }
}

fn key_utxos(list: &[PayerUtxo], pubkey: &[u8; 32]) -> R<Vec<KeyUtxo>> {
    let want = spk_to_hex(&kob_protocol::script::p2pk_spk(pubkey));
    let mut out = Vec::new();
    for u in list {
        if u.covenant_id.is_some() {
            continue;
        }
        if let Some(spk) = &u.script_public_key {
            if spk.to_ascii_lowercase() != want {
                continue; // not a P2PK coin of this key
            }
        }
        out.push(KeyUtxo {
            utxo: Utxo {
                transaction_id: h32("utxo txid", &u.txid)?,
                index: u.index,
                amount: value_u64("utxo amount", &u.amount)?,
                block_daa_score: match &u.block_daa_score {
                    Some(v) => value_u64("blockDaaScore", v)?,
                    None => 0,
                },
                covenant_id: None,
            },
            pubkey: *pubkey,
        });
    }
    Ok(out)
}

/// Token identity as the caller configures it (either family: the family is the program's).
/// `templateHash` / `extensionCommitment` default to the embedded registry entry of the covenant id
/// (mainnet registry only); a KRON token has no extension commitment (omit it).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenSpec {
    covenant_id: String,
    #[serde(default)]
    template_hash: Option<String>,
    #[serde(default)]
    extension_commitment: Option<String>,
    #[serde(default)]
    custody: Option<String>,
    #[serde(default)]
    ticker: Option<String>,
    #[serde(default)]
    decimals: Option<u8>,
}

/// Where a token description comes from: decides whether its `custody` label is believed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trust {
    /// The caller's own configuration (the payer's pinned allowlist, the merchant's offer configuration): a custody
    /// label may only TIGHTEN what the shipped registry derives from the program's capabilities.
    Configured,
    /// Derived from an offer a merchant sent: nothing in it is believed. A registry token takes its program,
    /// extension commitment and custody from the shipped registry; any other program is classified
    /// `issuer-controlled` (fail closed: pin it in `tokens` to accept it).
    Offer,
}

/// The custody class of a token program from the SHIPPED registry's template capabilities (`None`: the program is
/// not in the registry, its capabilities are unknown). `unconditional` only when the registry lists no capability
/// (no admin, freeze, seize, mint or burn path), exactly like `TokenAllowlist::from_registry`.
fn program_custody(template_hash: &[u8; 32]) -> Option<Custody> {
    let reg = Registry::default_registry();
    reg.templates.iter().find(|t| kob_protocol::registry::parse_hex32(&t.template_hash) == Some(*template_hash)).map(|t| {
        if t.capabilities.is_empty() {
            Custody::Unconditional
        } else {
            Custody::IssuerControlled
        }
    })
}

fn allowed(network: Network, s: &TokenSpec, trust: Trust) -> R<AllowedToken> {
    let covenant_id = h32("covenantId", &s.covenant_id)?;
    let reg = Registry::default_registry();
    let reg_token = if reg.network == network.registry_name() {
        reg.tokens.iter().find(|t| kob_protocol::registry::parse_hex32(&t.covenant_id) == Some(covenant_id))
    } else {
        None
    };
    // an offer cannot override the registry's program / extension commitment of a registry token
    let (spec_template, spec_extension) = match (trust, reg_token) {
        (Trust::Offer, Some(_)) => (&None, &None),
        _ => (&s.template_hash, &s.extension_commitment),
    };
    let template_hash = match (spec_template, reg_token) {
        (Some(h), _) => h32("templateHash", h)?,
        (None, Some(t)) => reg
            .template(&t.template_id)
            .and_then(|tpl| kob_protocol::registry::parse_hex32(&tpl.template_hash))
            .ok_or_else(|| perr("registry template is missing"))?,
        (None, None) => {
            return Err(perr(format!("token {} is not in the registry: give templateHash and extensionCommitment", s.covenant_id)))
        }
    };
    let tpl = token_template_by_hash(&template_hash).ok_or_else(|| perr("templateHash is not a pinned token program"))?;
    let (program, family) = (tpl.id, tpl.family);
    let extension_commitment = match (spec_extension, reg_token, family) {
        (Some(h), _, _) => h32("extensionCommitment", h)?,
        // a KRON token has no extension commitment
        (None, _, Family::Kron) => [0; 32],
        (None, Some(t), _) => t
            .extension_commitment
            .as_deref()
            .and_then(kob_protocol::registry::parse_hex32)
            .ok_or_else(|| perr("registry token has no extension commitment"))?,
        (None, None, _) => return Err(perr("extensionCommitment is required for a token outside the registry")),
    };
    let requested = match (trust, s.custody.as_deref()) {
        (Trust::Offer, _) | (_, None) => None,
        (_, Some("unconditional")) => Some(Custody::Unconditional),
        (_, Some("issuer-controlled")) => Some(Custody::IssuerControlled),
        (_, Some(o)) => return Err(perr(format!("unknown custody {o:?}"))),
    };
    // the class is the PROGRAM's (registry capabilities); a label can only tighten it
    let custody = match (program_custody(&template_hash), requested) {
        (Some(Custody::IssuerControlled), Some(Custody::Unconditional)) => {
            return Err(perr(format!(
                "token {}: the program has issuer capabilities in the registry (mint, burn, freeze, seize or blacklist); it cannot be labelled unconditional",
                s.covenant_id
            )))
        }
        (Some(Custody::IssuerControlled), _) => Custody::IssuerControlled,
        (Some(Custody::Unconditional), r) => r.unwrap_or(Custody::Unconditional),
        // not in the registry: the caller's own label, else fail closed
        (None, Some(r)) => r,
        (None, None) => Custody::IssuerControlled,
    };
    let allowed = AllowedToken {
        covenant_id,
        program,
        family,
        extension_commitment,
        custody,
        ticker: s.ticker.clone().or_else(|| reg_token.map(|t| t.ticker.clone())).unwrap_or_default(),
        decimals: s.decimals.or_else(|| reg_token.map(|t| t.decimals)).unwrap_or(0),
    };
    // a KRON token has no extension commitment field: the allowlist entry is all zero
    if family == Family::Kron && extension_commitment != [0; 32] {
        return Err(perr("a KRON token has no extension commitment (omit it or give all zeros)"));
    }
    Ok(allowed)
}

/// The tokens a payment may involve, from the offer itself (merchant token, route pay assets) unless
/// the caller pins its own allowlist.
fn token_specs_of_offer(offer: &PaymentRequirements) -> Vec<TokenSpec> {
    let mut v = Vec::new();
    if let Some(t) = offer.extra.get("token").and_then(Value::as_object) {
        v.push(TokenSpec {
            covenant_id: offer.asset.clone(),
            template_hash: t.get("templateHash").and_then(Value::as_str).map(str::to_string),
            extension_commitment: t.get("extensionCommitment").and_then(Value::as_str).map(str::to_string),
            custody: t.get("custody").and_then(Value::as_str).map(str::to_string),
            ticker: t.get("ticker").and_then(Value::as_str).map(str::to_string),
            decimals: t.get("decimals").and_then(Value::as_u64).and_then(|d| u8::try_from(d).ok()),
        });
    }
    if let Some(list) = offer.extra.get("route").and_then(|r| r.get("payAssets")).and_then(Value::as_array) {
        for p in list {
            let Some(asset) = p.get("asset").and_then(Value::as_str) else { continue };
            if asset == kob_x402::swap::PAY_ASSET_KAS {
                continue;
            }
            v.push(TokenSpec {
                covenant_id: asset.to_string(),
                template_hash: p.get("templateHash").and_then(Value::as_str).map(str::to_string),
                extension_commitment: p.get("extensionCommitment").and_then(Value::as_str).map(str::to_string),
                ..TokenSpec::default()
            });
        }
    }
    v
}

fn policy_for(
    network: Network,
    offer: &PaymentRequirements,
    pinned: &Option<Vec<TokenSpec>>,
    allow_issuer: bool,
    max_carrier: &Option<String>,
) -> R<Policy> {
    let mut p = Policy::new(network);
    p.allow_issuer_controlled = allow_issuer;
    if let Some(c) = max_carrier {
        p.limits.max_carrier_sompi = u64s("maxCarrierSompi", c)?;
    }
    let (specs, trust) = match pinned {
        Some(s) => (s.clone(), Trust::Configured),
        None => (token_specs_of_offer(offer), Trust::Offer),
    };
    for s in &specs {
        // a duplicate covenant id (merchant asset listed twice) is harmless
        let _ = p.tokens.insert(allowed(network, s, trust)?);
    }
    Ok(p)
}

fn requirements_of(v: Value) -> R<PaymentRequirements> {
    serde_json::from_value(v).map_err(|e| perr(format!("offer: {e}")))
}

// ------------------------------------------------------------------------------------------------ results

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PayOut {
    payment_payload: PaymentPayload,
    transaction_id: String,
    consumed: Vec<OutpointJson>,
    fee_sompi: String,
    expires_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    payer_spent: Option<String>,
    /// Swap-and-pay: sompi taken from the payer's KAS coins (fee, carriers, and the cost when paying KAS).
    #[serde(skip_serializing_if = "Option::is_none")]
    kas_spent: Option<String>,
    warnings: Vec<String>,
}

fn pay_out(payload: PaymentPayload, payer_spent: Option<u64>, warnings: Vec<String>) -> R<String> {
    pay_out_with(payload, payer_spent, None, warnings)
}

fn pay_out_with(payload: PaymentPayload, payer_spent: Option<u64>, kas_spent: Option<u64>, warnings: Vec<String>) -> R<String> {
    let safe = SafeTx::parse(&payload.payload.transaction, MAX_TX_JSON).map_err(xerr)?;
    let parsed = safe.to_consensus().map_err(xerr)?;
    let consumed: Vec<OutpointJson> = parsed.tx.inputs.iter().map(|i| common::outpoint_of(i).to_json()).collect();
    let total_in: u64 = parsed.hints.iter().flatten().map(|h| h.amount).sum();
    let total_out: u64 = parsed.tx.outputs.iter().map(|o| o.value).sum();
    let expires =
        parse_iso_ms(&payload.payload.authorization.expires_at).ok_or_else(|| perr("authorization.expiresAt does not parse"))?;
    out(&PayOut {
        transaction_id: hex(&parsed.tx.id().as_bytes()),
        consumed,
        fee_sompi: total_in.saturating_sub(total_out).to_string(),
        expires_at_ms: expires,
        payer_spent: payer_spent.map(|n| n.to_string()),
        kas_spent: kas_spent.map(|n| n.to_string()),
        warnings,
        payment_payload: payload,
    })
}

// ------------------------------------------------------------------------------------------------ hashes and digests

pub fn canonical(json: &str) -> R<String> {
    let v: Value = parse("value", json)?;
    canonical_json(&v).map_err(xerr)
}

pub fn sha256_hex(text: &str) -> String {
    to_hex(&sha256(text.as_bytes()))
}

/// SHA-256 of the canonical JSON of the requirements.
pub fn requirements_hash(req: &str) -> R<String> {
    let v: Value = parse("requirements", req)?;
    Ok(to_hex(&canonical_hash(&v).map_err(xerr)?))
}

/// The reference SDK's request fingerprint (`body` is JSON text; `null` for none).
pub fn request_hash(method: &str, url: &str, body: &str, requirements_hash_hex: &str) -> R<String> {
    let b: Value = parse("body", body)?;
    let body = if b.is_null() { None } else { Some(&b) };
    Ok(to_hex(&http_request_hash(method, url, body, requirements_hash_hex).map_err(xerr)?))
}

pub fn address_to_spk(address: &str) -> R<String> {
    let a = kaspa_addresses::Address::try_from(address).map_err(|e| perr(format!("address: {e}")))?;
    Ok(spk_to_hex(&kaspa_txscript::pay_to_address_script(&a)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignedAuthIn {
    network: String,
    profile: String,
    transaction_id: String,
    payment_output_index: u32,
    amount: String,
    pay_to: String,
    pay_to_script_public_key: String,
    payment_requirements_hash: String,
    request_hash: String,
    #[serde(default)]
    challenge_id: Option<String>,
    input_index: u32,
    expires_at: String,
}

fn profile(s: &str) -> R<Profile> {
    Profile::parse(s).ok_or_else(|| perr(format!("unknown profile {s:?}")))
}

/// The binding's Schnorr-signed authorization digest (`kaspa-x402-exact-request-authorization-v1`).
pub fn signed_auth_digest(json: &str) -> R<String> {
    let a: SignedAuthIn = parse("auth", json)?;
    let (txid, rh, qh) =
        (h32("transactionId", &a.transaction_id)?, h32("requestHash", &a.request_hash)?, h32("hash", &a.payment_requirements_hash)?);
    let d = common::signed_auth_digest(&SignedAuth {
        network: net(&a.network)?,
        profile: profile(&a.profile)?,
        transaction_id: &txid,
        payment_output_index: a.payment_output_index,
        amount: &a.amount,
        pay_to: &a.pay_to,
        pay_to_spk_hex: &a.pay_to_script_public_key,
        requirements_hash: &qh,
        request_hash: &rh,
        challenge_id: a.challenge_id.as_deref(),
        input_index: a.input_index,
        expires_at: &a.expires_at,
    })
    .map_err(xerr)?;
    Ok(to_hex(&d))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitIn {
    network: String,
    profile: String,
    #[serde(default)]
    route_pay_asset: Option<String>,
    asset: String,
    amount: String,
    pay_to: String,
    pay_to_script_public_key: String,
    payment_output_index: u32,
    payment_requirements_hash: String,
    request_hash: String,
    expires_at: String,
}

/// The KOB payload-commitment digest (`kob-x402-payload-commitment-v1`).
pub fn payload_commit_digest(json: &str) -> R<String> {
    let a: CommitIn = parse("commit", json)?;
    let (rh, qh) = (h32("requestHash", &a.request_hash)?, h32("hash", &a.payment_requirements_hash)?);
    let d = common::payload_commit_digest(&PayloadCommit {
        network: net(&a.network)?,
        profile: profile(&a.profile)?,
        route_pay_asset: a.route_pay_asset.as_deref(),
        asset: &a.asset,
        amount: &a.amount,
        pay_to: &a.pay_to,
        pay_to_spk_hex: &a.pay_to_script_public_key,
        payment_output_index: a.payment_output_index,
        requirements_hash: &qh,
        request_hash: &rh,
        expires_at: &a.expires_at,
    })
    .map_err(xerr)?;
    Ok(to_hex(&d))
}

// ------------------------------------------------------------------------------------------------ merchant offers

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeOfferIn {
    network: String,
    amount: String,
    pay_to: String,
    max_timeout_seconds: u64,
    finality: String,
}

pub fn native_requirements(json: &str) -> R<String> {
    let p: NativeOfferIn = parse("offer", json)?;
    let r =
        native::native_requirements(net(&p.network)?, u64s("amount", &p.amount)?, &p.pay_to, p.max_timeout_seconds, fin(&p.finality)?)
            .map_err(nerr)?;
    out(&r)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenOfferIn {
    network: String,
    /// The KCC-20 covenant id.
    asset: String,
    pay_to: String,
    amount: String,
    #[serde(default)]
    carrier: Option<String>,
    #[serde(default = "default_timeout")]
    max_timeout_seconds: u64,
    #[serde(default = "default_finality")]
    finality: String,
    #[serde(default)]
    custody: Option<String>,
    #[serde(default)]
    template_hash: Option<String>,
    #[serde(default)]
    extension_commitment: Option<String>,
    #[serde(default)]
    ticker: Option<String>,
    #[serde(default)]
    decimals: Option<u8>,
}

fn default_timeout() -> u64 {
    60
}
fn default_finality() -> String {
    "accepted".into()
}

fn token_offer_req(p: &TokenOfferIn) -> R<PaymentRequirements> {
    let network = net(&p.network)?;
    let spec = TokenSpec {
        covenant_id: p.asset.clone(),
        template_hash: p.template_hash.clone(),
        extension_commitment: p.extension_commitment.clone(),
        custody: p.custody.clone(),
        ticker: p.ticker.clone(),
        decimals: p.decimals,
    };
    let carrier = match &p.carrier {
        Some(c) => u64s("carrier", c)?,
        None => Limits::default().min_carrier_sompi,
    };
    token::kcc20_requirements(
        network,
        &allowed(network, &spec, Trust::Configured)?,
        u64s("amount", &p.amount)?,
        &p.pay_to,
        carrier,
        p.max_timeout_seconds,
        fin(&p.finality)?,
    )
    .map_err(xerr)
}

/// The `accepts` entry for `amount` units of a token.
pub fn kcc20_requirements(json: &str) -> R<String> {
    out(&token_offer_req(&parse("offer", json)?)?)
}

/// Just `extra.token` of that entry.
pub fn token_offer(json: &str) -> R<String> {
    let r = token_offer_req(&parse("offer", json)?)?;
    out(r.extra.get("token").ok_or_else(|| perr("no extra.token"))?)
}

/// Pinned program hash and extension commitment of a registry token.
pub fn resolve_token(network: &str, asset: &str) -> R<String> {
    let network = net(network)?;
    let a = allowed(network, &TokenSpec { covenant_id: asset.into(), ..TokenSpec::default() }, Trust::Configured)?;
    out(&json!({
        "family": a.family.as_str(),
        "templateHash": to_hex(&a.template_hash()),
        "extensionCommitment": to_hex(&a.extension_commitment),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwapOfferIn {
    network: String,
    amount: String,
    pay_to: String,
    #[serde(default = "default_timeout")]
    max_timeout_seconds: u64,
    #[serde(default = "default_finality")]
    finality: String,
    /// `"kas"` or `"kcc20"`.
    receive: String,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    carrier: Option<String>,
    #[serde(default)]
    token: Option<TokenSpec>,
    pay_assets: Vec<TokenSpec>,
}

pub fn swap_requirements(json: &str) -> R<String> {
    route_requirements(json, false)
}

/// An intent-based swap-and-pay offer (`extra.route.binding` = `kob-intent-v1`): the same input as
/// [`swap_requirements`]; every token must be on the router's program.
pub fn intent_requirements(json: &str) -> R<String> {
    route_requirements(json, true)
}

fn route_requirements(json: &str, intent: bool) -> R<String> {
    let p: SwapOfferIn = parse("offer", json)?;
    let network = net(&p.network)?;
    let merchant = match (p.receive.as_str(), &p.token) {
        ("kas", _) => None,
        ("kcc20", Some(t)) => {
            let mut t = t.clone();
            if t.covenant_id.is_empty() {
                t.covenant_id = p.asset.clone().ok_or_else(|| perr("asset is required when the merchant receives a token"))?;
            }
            Some(allowed(network, &t, Trust::Configured)?)
        }
        ("kcc20", None) => return Err(perr("token metadata is required when the merchant receives a token")),
        (o, _) => return Err(perr(format!("unknown receive {o:?}"))),
    };
    // `covenantId: "KAS"` is the KAS pay asset (only when the merchant receives a token)
    let pay_tokens: Vec<Option<AllowedToken>> =
        p.pay_assets
            .iter()
            .map(|s| {
                if s.covenant_id == kob_x402::swap::PAY_ASSET_KAS {
                    Ok(None)
                } else {
                    allowed(network, s, Trust::Configured).map(Some)
                }
            })
            .collect::<R<_>>()?;
    let carrier = match &p.carrier {
        Some(c) => u64s("carrier", c)?,
        None => Limits::default().min_carrier_sompi,
    };
    let params = swap::SwapOfferParams {
        network,
        amount: u64s("amount", &p.amount)?,
        pay_to: &p.pay_to,
        max_timeout_seconds: p.max_timeout_seconds,
        finality: fin(&p.finality)?,
        gain: match &merchant {
            None => MerchantGain::Kas,
            Some(t) => MerchantGain::Token { token: t, carrier },
        },
        pay_assets: pay_tokens.iter().map(|t| t.as_ref().map_or(PayAssetSpec::Kas, PayAssetSpec::Token)).collect(),
    };
    if intent {
        return out(&kob_x402::client::intent::intent_requirements(&params).map_err(xerr)?);
    }
    out(&swap::swap_requirements(&params).map_err(xerr)?)
}

// ------------------------------------------------------------------------------------------------ payer: native

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct NativeOptions {
    #[serde(default)]
    max_amount_sompi: Option<String>,
    #[serde(default)]
    fee_rate: Option<u64>,
    #[serde(default)]
    max_fee_sompi: Option<String>,
    #[serde(default)]
    min_change_sompi: Option<String>,
    #[serde(default)]
    max_inputs: Option<usize>,
    #[serde(default)]
    payment_id: Option<String>,
    #[serde(default)]
    resource: Option<Resource>,
    /// Authorization lifetime in seconds (default: the offer's `maxTimeoutSeconds`).
    #[serde(default)]
    ttl_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayNativeIn {
    offer: Value,
    request_hash: String,
    secret_key: String,
    utxos: Vec<PayerUtxo>,
    now_ms: Num,
    #[serde(default)]
    options: NativeOptions,
}

/// Builds and signs a standard-native payment with a local key.
pub fn pay_native(json: &str) -> R<String> {
    let p: PayNativeIn = parse("request", json)?;
    let offer = requirements_of(p.offer)?;
    let secret = h32("secretKey", &p.secret_key)?;
    let pubkey = pubkey_of(&secret).map_err(|e| perr(e.to_string()))?;
    let utxos = key_utxos(&p.utxos, &pubkey)?;
    let max_amount = match &p.options.max_amount_sompi {
        Some(m) => u64s("maxAmountSompi", m)?,
        None => offer.amount_u64().map_err(xerr)?, // pin the offered amount
    };
    let mut o = PayOptions::new(max_amount);
    if let Some(r) = p.options.fee_rate {
        o.fee_rate = r;
    }
    if let Some(m) = &p.options.max_fee_sompi {
        o.max_fee_sompi = u64s("maxFeeSompi", m)?;
    }
    if let Some(m) = &p.options.min_change_sompi {
        o.min_change_sompi = u64s("minChangeSompi", m)?;
    }
    if let Some(m) = p.options.max_inputs {
        o.max_inputs = m;
    }
    o.payment_id = p.options.payment_id.clone();
    o.resource = p.options.resource.clone();
    o.ttl_seconds = p.options.ttl_seconds;
    let payload = native::pay_native(&offer, &p.request_hash, &secret, &utxos, p.now_ms.get("nowMs")?, &o).map_err(nerr)?;
    pay_out(payload, None, vec![])
}

// ------------------------------------------------------------------------------------------------ payer: kcc20

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TokenOptions {
    #[serde(default)]
    fee_rate: Option<u64>,
    #[serde(default)]
    token_change_carrier: Option<String>,
    #[serde(default)]
    ttl_seconds: Option<u64>,
    #[serde(default)]
    payment_id: Option<String>,
    #[serde(default)]
    max_fee_sompi: Option<String>,
    /// Payer bound on the merchant carrier and on the token change carrier (default 2 KAS).
    #[serde(default)]
    max_carrier_sompi: Option<String>,
}

impl TokenOptions {
    fn build(&self) -> R<Kcc20Options> {
        let mut o = Kcc20Options { fee_rate: self.fee_rate, ..Kcc20Options::default() };
        if let Some(c) = &self.token_change_carrier {
            o.token_change_carrier = Some(u64s("tokenChangeCarrier", c)?);
        }
        o.ttl_seconds = self.ttl_seconds;
        o.payment_id = self.payment_id.clone();
        if let Some(m) = &self.max_fee_sompi {
            o.max_fee_sompi = u64s("maxFeeSompi", m)?;
        }
        if let Some(m) = &self.max_carrier_sompi {
            o.max_carrier_sompi = u64s("maxCarrierSompi", m)?;
        }
        Ok(o)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kcc20In {
    offer: Value,
    request_hash: String,
    /// Local keys (all-in-one builders).
    #[serde(default)]
    secret_keys: Vec<String>,
    /// The wallet's x-only key (unsigned builders; funding UTXOs are locked to it).
    #[serde(default)]
    payer_public_key: Option<String>,
    /// kob-protocol `TokenUtxo` JSON.
    token_utxos: Vec<TokenUtxo>,
    #[serde(default)]
    funding: Vec<PayerUtxo>,
    now_ms: Num,
    #[serde(default)]
    options: TokenOptions,
    /// Payer-pinned token allowlist (default: the tokens the offer names, classified from the shipped registry).
    #[serde(default)]
    tokens: Option<Vec<TokenSpec>>,
    #[serde(default)]
    allow_issuer_controlled: bool,
}

impl Kcc20In {
    /// The payer key for funding UTXOs.
    fn funding_key(&self) -> R<Option<[u8; 32]>> {
        if let Some(pk) = &self.payer_public_key {
            return Ok(Some(h32("payerPublicKey", pk)?));
        }
        // with local keys, funding coins are P2PK coins of any held key: filtered per key below
        Ok(None)
    }
}

fn funding_for(list: &[PayerUtxo], keys: &[[u8; 32]]) -> R<Vec<KeyUtxo>> {
    let mut all = Vec::new();
    for k in keys {
        all.extend(key_utxos(list, k)?);
    }
    Ok(all)
}

/// The payer-side policy gate of a KCC-20 payment, run BEFORE anything is built or signed (the wallet is never asked to
/// sign an offer the payer's own policy refuses): allowlist (the payer's pin, else the shipped registry; custody is the
/// PROGRAM's, never the offer's), custody opt-in and the carrier ceiling.
fn gate_kcc20(p: &Kcc20In, offer: &PaymentRequirements) -> R<()> {
    let network = offer.network().map_err(xerr)?;
    let policy = policy_for(network, offer, &p.tokens, p.allow_issuer_controlled, &p.options.max_carrier_sompi)?;
    let facts = kob_x402::token::parse_offer(offer).map_err(xerr)?;
    kob_x402::token::check_offer_policy(&policy, &facts).map_err(xerr)?;
    Ok(())
}

/// Builds, signs locally and finalizes a KCC-20 payment.
pub fn pay_kcc20(json: &str) -> R<String> {
    let p: Kcc20In = parse("request", json)?;
    let offer = requirements_of(p.offer.clone())?;
    gate_kcc20(&p, &offer)?;
    let secrets = secrets_of(&p.secret_keys)?;
    let keys: Vec<[u8; 32]> = secrets.keys().copied().collect();
    let funding = funding_for(&p.funding, &keys)?;
    let payload =
        token::pay_kcc20_with(&offer, &p.request_hash, &secrets, p.token_utxos, funding, p.now_ms.get("nowMs")?, &p.options.build()?)
            .map_err(xerr)?;
    pay_out(payload, None, vec![])
}

/// Step 1 of the wallet flow: the unsigned payment, its signing requests and the payload template.
pub fn build_kcc20_unsigned(json: &str) -> R<String> {
    let p: Kcc20In = parse("request", json)?;
    let offer = requirements_of(p.offer.clone())?;
    gate_kcc20(&p, &offer)?;
    let keys: Vec<[u8; 32]> = match p.funding_key()? {
        Some(k) => vec![k],
        None => secrets_of(&p.secret_keys)?.keys().copied().collect(),
    };
    let funding = funding_for(&p.funding, &keys)?;
    let (built, template) =
        token::build_kcc20_unsigned(&offer, &p.request_hash, p.token_utxos, funding, p.now_ms.get("nowMs")?, &p.options.build()?)
            .map_err(xerr)?;
    out(&json!({ "built": built, "template": template }))
}

fn signatures(json: &str) -> R<Vec<InputSignature>> {
    parse("signatures", json)
}

/// Step 2 of the wallet flow: checks the signatures, tightens the budgets, assembles the payload.
pub fn finish_kcc20(built: &str, template: &str, sigs: &str) -> R<String> {
    let b: BuiltTx = parse("built", built)?;
    let t: PayloadTemplate = parse("template", template)?;
    let payload = token::finish_kcc20(&b, &t, &signatures(sigs)?).map_err(xerr)?;
    pay_out(payload, None, vec![])
}

// ------------------------------------------------------------------------------------------------ payer: swap

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SwapOptionsIn {
    /// The payer's bound on the swap's cost, counted in `maxPayAsset`.
    #[serde(default)]
    max_pay: Option<String>,
    /// `"KAS"` (default: an unqualified bound counts sompi only) or the pay token's covenant id. A route that pays with
    /// another asset is refused.
    #[serde(default)]
    max_pay_asset: Option<String>,
    #[serde(default)]
    expires_in_ms: Option<u64>,
    #[serde(default)]
    fee_rate: Option<u64>,
    #[serde(default)]
    change_carrier: Option<String>,
    #[serde(default)]
    min_change_sompi: Option<String>,
    #[serde(default)]
    max_fee_sompi: Option<String>,
    #[serde(default)]
    payment_identifier: Option<String>,
    /// Payer bound on the merchant carrier and on the change carrier (default 2 KAS).
    #[serde(default)]
    max_carrier_sompi: Option<String>,
    /// The most sompi the payment may take from the payer's KAS coins (fee, carriers, the cost when paying KAS).
    #[serde(default)]
    max_kas_sompi: Option<String>,
}

impl SwapOptionsIn {
    fn build(&self) -> R<SwapOptions> {
        let mut o = SwapOptions::default();
        if let Some(m) = &self.max_pay {
            o.max_pay = Some(pay_bound(self.max_pay_asset.as_deref(), u64s("maxPay", m)?));
        }
        if let Some(e) = self.expires_in_ms {
            o.expires_in_ms = e;
        }
        o.fee_rate = self.fee_rate;
        if let Some(c) = &self.change_carrier {
            o.change_carrier = Some(u64s("changeCarrier", c)?);
        }
        if let Some(m) = &self.min_change_sompi {
            o.min_change_sompi = u64s("minChangeSompi", m)?;
        }
        if let Some(m) = &self.max_fee_sompi {
            o.max_fee_sompi = Some(u64s("maxFeeSompi", m)?);
        }
        o.payment_identifier = self.payment_identifier.clone();
        if let Some(m) = &self.max_kas_sompi {
            o.max_kas_sompi = Some(u64s("maxKasSompi", m)?);
        }
        Ok(o)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwapIn {
    offer: Value,
    /// `{ lockTime, orders: [OrderRef] }` (kob-x402 `Quote` JSON).
    quote: Quote,
    request_hash: String,
    #[serde(default)]
    secret_keys: Vec<String>,
    #[serde(default)]
    payer_public_key: Option<String>,
    #[serde(default)]
    token_utxos: Vec<TokenUtxo>,
    #[serde(default)]
    funding: Vec<PayerUtxo>,
    now_ms: Num,
    #[serde(default)]
    options: SwapOptionsIn,
    /// Payer-pinned token allowlist (default: the tokens the offer names).
    #[serde(default)]
    tokens: Option<Vec<TokenSpec>>,
    #[serde(default)]
    allow_issuer_controlled: bool,
}

struct SwapCtx {
    policy: Policy,
    offer: PaymentRequirements,
    funds: PayerFunds,
    keys: Vec<[u8; 32]>,
}

fn swap_ctx(p: &SwapIn, need_secrets: bool) -> R<SwapCtx> {
    let offer = requirements_of(p.offer.clone())?;
    let network = offer.network().map_err(xerr)?;
    let keys: Vec<[u8; 32]> = if need_secrets || !p.secret_keys.is_empty() {
        secrets_of(&p.secret_keys)?.keys().copied().collect()
    } else {
        vec![h32("payerPublicKey", p.payer_public_key.as_deref().ok_or_else(|| perr("payerPublicKey or secretKeys required"))?)?]
    };
    let change = match &p.payer_public_key {
        Some(k) => h32("payerPublicKey", k)?,
        None => keys[0],
    };
    let funds = PayerFunds { tokens: p.token_utxos.clone(), funding: funding_for(&p.funding, &keys)?, change };
    Ok(SwapCtx {
        policy: policy_for(network, &offer, &p.tokens, p.allow_issuer_controlled, &p.options.max_carrier_sompi)?,
        offer,
        funds,
        keys,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreparedJson {
    built: BuiltTx,
    offer: PaymentRequirements,
    request_hash: String,
    pay_asset: String,
    orders: Vec<OutpointJson>,
    payment_output_index: u32,
    digest: String,
    expires_at: String,
    payer_spent: String,
    #[serde(default)]
    kas_spent: Option<String>,
    payer_address: Option<String>,
    warnings: Vec<String>,
    payment_identifier: Option<String>,
}

fn outpoint_of_json(o: &OutpointJson) -> R<Outpoint> {
    Ok(Outpoint::new(h32("order txid", &o.txid)?, o.index))
}

impl PreparedJson {
    fn from(p: &PreparedSwap) -> Self {
        PreparedJson {
            built: p.built.clone(),
            offer: p.offer.clone(),
            request_hash: p.request_hash.clone(),
            pay_asset: p.pay_asset.clone(),
            orders: p.orders.iter().map(Outpoint::to_json).collect(),
            payment_output_index: p.payment_output_index,
            digest: hex(&p.digest),
            expires_at: p.expires_at.clone(),
            payer_spent: p.payer_spent.to_string(),
            kas_spent: Some(p.kas_spent.to_string()),
            payer_address: p.payer_address.clone(),
            warnings: p.warnings.clone(),
            payment_identifier: p.payment_identifier.clone(),
        }
    }
    fn into_prepared(self) -> R<PreparedSwap> {
        Ok(PreparedSwap {
            built: self.built,
            offer: self.offer,
            request_hash: self.request_hash,
            pay_asset: self.pay_asset,
            orders: self.orders.iter().map(outpoint_of_json).collect::<R<_>>()?,
            payment_output_index: self.payment_output_index,
            digest: h32("digest", &self.digest)?,
            expires_at: self.expires_at,
            payer_spent: u64s("payerSpent", &self.payer_spent)?,
            kas_spent: match &self.kas_spent {
                Some(s) => u64s("kasSpent", s)?,
                None => 0,
            },
            payer_address: self.payer_address,
            warnings: self.warnings,
            payment_identifier: self.payment_identifier,
        })
    }
}

/// A payer bound counted in `asset` (`None`: KAS, so a bare number never changes unit with the route's pay asset).
fn pay_bound(asset: Option<&str>, amount: u64) -> PayBound {
    match asset {
        Some(a) if !a.eq_ignore_ascii_case("KAS") => PayBound { asset: a.to_ascii_lowercase(), amount },
        _ => PayBound::kas(amount),
    }
}

fn swap_out(pay: SwapPayment) -> R<String> {
    pay_out_with(pay.payload, Some(pay.payer_spent), Some(pay.kas_spent), pay.warnings)
}

/// Step 1 of the wallet flow: builds the unsigned swap payment (`built.sign` = what the wallet signs).
pub fn prepare_swap(json: &str) -> R<String> {
    let p: SwapIn = parse("request", json)?;
    let c = swap_ctx(&p, false)?;
    let prepared =
        swap::prepare_swap(&c.policy, &c.offer, &p.quote, &p.request_hash, &c.funds, p.now_ms.get("nowMs")?, &p.options.build()?)
            .map_err(xerr)?;
    out(&PreparedJson::from(&prepared))
}

/// Step 2 of the wallet flow.
pub fn finish_swap(prepared: &str, sigs: &str) -> R<String> {
    let p: PreparedJson = parse("prepared", prepared)?;
    swap_out(p.into_prepared()?.complete(&signatures(sigs)?).map_err(xerr)?)
}

/// Builds, signs locally and assembles a swap-and-pay payment.
pub fn pay_swap(json: &str) -> R<String> {
    let p: SwapIn = parse("request", json)?;
    let c = swap_ctx(&p, true)?;
    let secrets = secrets_of(&p.secret_keys)?;
    let pay = swap::pay_swap(
        &c.policy,
        &c.offer,
        &p.quote,
        &p.request_hash,
        &secrets,
        &c.funds,
        p.now_ms.get("nowMs")?,
        &p.options.build()?,
    )
    .map_err(xerr)?;
    let _ = c.keys;
    swap_out(pay)
}

// ------------------------------------------------------------------------------------------------ preflight

/// A chain view over a fixed set of UTXOs: the ones the payment's own transaction carries as hints.
///
/// Preflight is the payer running the facilitator's verification (scripts through the engine with
/// enforced budgets, signatures, mass, fee floor, exact merchant gain, authorization, expiry,
/// allowlist) over the artifact it is about to disclose. The existence of the spent outputs is the
/// caller's own chain data: the SDK cross-checks the consumed outpoints against the UTXOs it loaded
/// before it calls this.
struct Snapshot {
    utxos: BTreeMap<Outpoint, ChainUtxo>,
    daa: u64,
}

impl ChainView for Snapshot {
    fn utxos(&self, wanted: &[(Outpoint, kaspa_consensus_core::tx::ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        Ok(wanted.iter().map(|(op, spk)| self.utxos.get(op).filter(|u| &u.script_public_key == spk).cloned()).collect())
    }
    fn utxos_of(&self, spk: &kaspa_consensus_core::tx::ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        Ok(self.utxos.iter().filter(|(_, u)| &u.script_public_key == spk).map(|(o, u)| (*o, u.clone())).collect())
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        Ok(self.daa)
    }
    fn output_status(&self, _: &Outpoint, _: &kaspa_consensus_core::tx::ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        Ok(OutputStatus::Unknown)
    }
    fn in_mempool(&self, _: &Txid) -> Result<bool, ChainError> {
        Ok(false)
    }
    fn submit(&self, _: &kaspa_consensus_core::tx::Transaction) -> Result<Txid, SubmitError> {
        Err(SubmitError::Unavailable("preflight never submits".into()))
    }
}

fn snapshot_of(payload: &PaymentPayload, daa: Option<u64>) -> R<Snapshot> {
    let safe = SafeTx::parse(&payload.payload.transaction, MAX_TX_JSON).map_err(xerr)?;
    let parsed = safe.to_consensus().map_err(xerr)?;
    let mut utxos = BTreeMap::new();
    for (i, input) in parsed.tx.inputs.iter().enumerate() {
        let Some(h) = safe.inputs.get(i).and_then(|x| x.utxo.as_ref()) else { continue };
        let block_daa = match &h.block_daa_score {
            Some(s) => u64s("blockDaaScore", s)?,
            None => 0,
        };
        utxos.insert(
            common::outpoint_of(input),
            ChainUtxo {
                amount: u64s("amount", &h.amount)?,
                script_public_key: spk_from_hex(&h.script_public_key).map_err(xerr)?,
                block_daa_score: block_daa,
                is_coinbase: h.is_coinbase.unwrap_or(false),
                covenant_id: match &h.covenant_id {
                    Some(c) => Some(h32("covenantId", c)?),
                    None => None,
                },
            },
        );
    }
    let lock = parsed.tx.lock_time;
    Ok(Snapshot { utxos, daa: daa.unwrap_or(lock).max(lock) })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreflightIn {
    offer: Value,
    payload: PaymentPayload,
    request_hash: String,
    now_ms: Num,
    #[serde(default)]
    virtual_daa_score: Option<Num>,
    #[serde(default)]
    tokens: Option<Vec<TokenSpec>>,
    #[serde(default)]
    allow_issuer_controlled: bool,
    /// Swap-and-pay: the payer's bound on what the payment costs, in units of `maxPayAsset`.
    #[serde(default)]
    max_pay: Option<String>,
    /// The asset `maxPay` counts (`"KAS"` by default, or a token covenant id).
    #[serde(default)]
    max_pay_asset: Option<String>,
    /// The payer's ceiling on the merchant carrier (default 2 KAS).
    #[serde(default)]
    max_carrier_sompi: Option<String>,
}

/// Runs the facilitator's verification over a built payment. Returns `{ok: true, ...}` or
/// `{ok: false, diagnostic, reason, message, retryable, details?}` (it does not throw on a failed check).
pub fn preflight(json: &str) -> R<String> {
    let p: PreflightIn = parse("request", json)?;
    let offer = requirements_of(p.offer)?;
    let network = offer.network().map_err(xerr)?;
    let policy = policy_for(network, &offer, &p.tokens, p.allow_issuer_controlled, &p.max_carrier_sompi)?;
    let daa = match p.virtual_daa_score {
        Some(n) => Some(n.get("virtualDaaScore")?),
        None => None,
    };
    let chain = snapshot_of(&p.payload, daa)?;
    let clock = FixedClock::new(p.now_ms.get("nowMs")?);
    let ctx = VerifyCtx { chain: &chain, clock: &clock, policy: &policy };
    let is_intent = kob_x402::client::intent::is_intent_payload(&p.payload) || offer.is_intent();
    let result = if is_intent {
        let max_pay = match &p.max_pay {
            Some(m) => Some(u64s("maxPay", m)?),
            None => None,
        };
        kob_x402::intent::verify_intent(&ctx, &offer, &p.payload, &p.request_hash).and_then(|(v, f)| {
            // the payer's worst case: the intent's KAS (KasToToken) or the units it may sell
            let worst = match f.state.max_sell() {
                Some(units) => units as u64,
                None => f.intent.amount,
            };
            let pays_kas = f.state.max_sell().is_none();
            if max_pay.is_some() && pay_bound(p.max_pay_asset.as_deref(), 0).asset.eq_ignore_ascii_case("KAS") != pays_kas {
                return Err(X402Error::payload(
                    kob_x402::error::Diag::PayAssetNotAccepted,
                    "the payer's bound counts another asset than the intent pays with",
                ));
            }
            if max_pay.is_some_and(|m| worst > m) {
                return Err(X402Error::payload(kob_x402::error::Diag::Overpayment, "the intent may cost more than the payer's bound"));
            }
            Ok((v, Some(worst)))
        })
    } else if p.payload.payload.route.is_some() || offer.has_route() {
        let max_pay = match &p.max_pay {
            Some(m) => Some(pay_bound(p.max_pay_asset.as_deref(), u64s("maxPay", m)?)),
            None => None,
        };
        swap::preflight_swap(&ctx, &offer, &p.payload, &p.request_hash, max_pay.as_ref()).map(|(v, f)| (v, Some(f.payer_spent)))
    } else {
        verify_payment(&ctx, &offer, &p.payload, &p.request_hash).map(|v| (v, None))
    };
    match result {
        Ok((v, spent)) => out(&json!({
            "ok": true,
            "transactionId": hex(&v.txid),
            "feeSompi": v.fee.to_string(),
            "amount": v.amount.to_string(),
            "payerSpent": spent.map(|s| s.to_string()),
            "custody": v.custody.map(|c| c.as_str()),
        })),
        Err(e) => {
            let mut o: Map<String, Value> = serde_json::from_str(&xerr(e)).expect("xerr is an object");
            o.insert("ok".into(), Value::Bool(false));
            out(&Value::Object(o))
        }
    }
}

// ------------------------------------------------------------------------------------------------ revoke

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeIn {
    payload: PaymentPayload,
    #[serde(default)]
    secret_keys: Vec<String>,
    /// Wallet mode (no `secretKeys`): the payer's x-only key; the answer is `{ built, spent }` for the wallet to sign.
    #[serde(default)]
    payer_public_key: Option<String>,
    #[serde(default)]
    fee_rate: Option<u64>,
    #[serde(default)]
    max_fee_sompi: Option<String>,
    #[serde(default)]
    tokens: Option<Vec<TokenSpec>>,
}

/// The self-spend that makes a signed but unsettled payment unconfirmable: one of its payer-owned
/// inputs spent back to the payer (a P2PK KAS input first, else a P2PK-owned token input). Returns
/// `{ transaction (safe JSON text), transactionId, spent: {txid, index} }`; in wallet mode (`payerPublicKey` instead of
/// `secretKeys`, kcc20 and swap-and-pay payments) `{ built, spent }`: the wallet signs `built.sign`, then
/// `x402FinishRevoke` (C5 X-10). A native payment's revoke needs the local key (it is a plain self-spend of the key the
/// native payment itself signs a digest with).
pub fn revoke(json: &str) -> R<String> {
    let p: RevokeIn = parse("request", json)?;
    let offer = p.payload.accepted.clone();
    let network = offer.network().map_err(xerr)?;
    let is_native = offer.profile().map_err(xerr)? == Profile::StandardNative && !offer.has_route();
    let wallet = if p.secret_keys.is_empty() {
        let k = p.payer_public_key.as_deref().ok_or_else(|| perr("secretKeys or payerPublicKey is required"))?;
        Some(kob_protocol::registry::parse_hex32(k).ok_or_else(|| perr("payerPublicKey must be 64 hex"))?)
    } else {
        None
    };
    if is_native && wallet.is_some() {
        return Err(perr("a native payment's revoke signs with the local key (wallet mode cannot revoke it)"));
    }
    let secrets = if wallet.is_some() { Default::default() } else { secrets_of(&p.secret_keys)? };
    if is_native {
        let secret = secrets.values().next().expect("non-empty");
        let max_fee = match &p.max_fee_sompi {
            Some(m) => u64s("maxFeeSompi", m)?,
            None => 50_000_000,
        };
        let rev = native::revoke_native(&p.payload, secret, p.fee_rate.unwrap_or(kob_protocol::tx::MIN_FEE_RATE * 2), max_fee)
            .map_err(nerr)?;
        return out(&json!({
            "transaction": serde_json::to_string(&TxJson::from_tx(&rev.tx, &rev.entries)).map_err(|e| e.to_string())?,
            "transactionId": hex(&rev.tx.id().as_bytes()),
            "spent": rev.spent.to_json(),
        }));
    }
    // kcc20 and swap-and-pay: rebuild the signed payment from the artifact and use the swap revoker
    let policy = policy_for(network, &offer, &p.tokens, true, &None)?;
    let safe = SafeTx::parse(&p.payload.payload.transaction, MAX_TX_JSON).map_err(xerr)?;
    let parsed = safe.to_consensus().map_err(xerr)?;
    let snap = snapshot_of(&p.payload, None)?;
    let entries: Vec<_> = parsed
        .tx
        .inputs
        .iter()
        .map(|i| {
            snap.utxos
                .get(&common::outpoint_of(i))
                .map(ChainUtxo::to_entry)
                .ok_or_else(|| perr("the artifact carries no utxo hint for an input"))
        })
        .collect::<R<_>>()?;
    let payment = SwapPayment {
        payload: p.payload.clone(),
        txid: parsed.tx.id().as_bytes(),
        tx: parsed.tx,
        entries,
        fee: 0,
        payer_spent: 0,
        kas_spent: 0,
        warnings: vec![],
    };
    if let Some(key) = wallet {
        let keys = std::collections::BTreeSet::from([key]);
        let (built, spent) = swap::prepare_revoke_swap(&policy, &payment, &keys, p.fee_rate).map_err(xerr)?;
        return out(&json!({ "built": built, "spent": spent.to_json() }));
    }
    let (rev, spent) = swap::revoke_swap(&policy, &payment, &secrets, p.fee_rate).map_err(xerr)?;
    let idx = payment
        .tx
        .inputs
        .iter()
        .position(|i| common::outpoint_of(i) == spent)
        .ok_or_else(|| perr("revoked input is not part of the payment"))?;
    let rev_entries = vec![payment.entries[idx].clone()];
    out(&json!({
        "transaction": serde_json::to_string(&TxJson::from_tx(&rev, &rev_entries)).map_err(|e| e.to_string())?,
        "transactionId": hex(&rev.id().as_bytes()),
        "spent": spent.to_json(),
    }))
}

/// Step 2 of the wallet revoke: the signed self-spend from `{ built, spent }` and the wallet's signatures. Returns
/// `{ transaction (safe JSON text), transactionId, spent }`.
pub fn finish_revoke(prepared: &str, sigs: &str) -> R<String> {
    #[derive(Deserialize)]
    struct P {
        built: BuiltTx,
        spent: Value,
    }
    let p: P = parse("prepared", prepared)?;
    let signed = kob_protocol::tx::finalize(&p.built, &signatures(sigs)?, kob_protocol::tx::FinalizeOptions { tighten_budgets: true })
        .map_err(|e| perr(e.to_string()))?;
    let (tx, entries) = signed.tx.to_tx().map_err(|e| perr(e.to_string()))?;
    kob_protocol::verify::validate(&tx, &entries).map_err(|e| perr(format!("the signed revoke does not validate: {e}")))?;
    out(&json!({
        "transaction": serde_json::to_string(&signed.tx).map_err(|e| e.to_string())?,
        "transactionId": hex(&tx.id().as_bytes()),
        "spent": p.spent,
    }))
}

// ------------------------------------------------------------------------------------------------ payer: intents

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IntentOptionsIn {
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    max_pay: Option<String>,
    #[serde(default)]
    max_extra: Option<String>,
    #[serde(default)]
    max_sell: Option<String>,
    #[serde(default)]
    lock_amount: Option<String>,
    #[serde(default)]
    keeper_value: Option<String>,
    #[serde(default)]
    lock_carrier: Option<String>,
    #[serde(default)]
    expires_in_ms: Option<u64>,
    #[serde(default)]
    fee_rate: Option<u64>,
    #[serde(default)]
    payment_identifier: Option<String>,
    #[serde(default)]
    max_carrier_sompi: Option<String>,
}

impl IntentOptionsIn {
    fn build(&self) -> R<kob_x402::client::intent::IntentOptions> {
        let mut o = kob_x402::client::intent::IntentOptions { actor: self.actor.clone(), ..Default::default() };
        let i64s = |what: &str, s: &str| -> R<i64> { i64::try_from(u64s(what, s)?).map_err(|_| perr(format!("{what} is too large"))) };
        if let Some(v) = &self.max_pay {
            o.max_pay = Some(u64s("maxPay", v)?);
        }
        if let Some(v) = &self.max_extra {
            o.max_extra = Some(u64s("maxExtra", v)?);
        }
        if let Some(v) = &self.max_sell {
            o.max_sell = Some(i64s("maxSell", v)?);
        }
        if let Some(v) = &self.lock_amount {
            o.lock_amount = Some(i64s("lockAmount", v)?);
        }
        if let Some(v) = &self.keeper_value {
            o.keeper_value = u64s("keeperValue", v)?;
        }
        if let Some(v) = &self.lock_carrier {
            o.lock_carrier = Some(u64s("lockCarrier", v)?);
        }
        if let Some(e) = self.expires_in_ms {
            o.expires_in_ms = Some(e);
        }
        o.fee_rate = self.fee_rate;
        o.payment_identifier = self.payment_identifier.clone();
        Ok(o)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntentIn {
    offer: Value,
    /// `"KAS"` or the pay token's covenant id (one of the offer's `payAssets`).
    pay_asset: String,
    request_hash: String,
    #[serde(default)]
    secret_keys: Vec<String>,
    #[serde(default)]
    payer_public_key: Option<String>,
    #[serde(default)]
    token_utxos: Vec<TokenUtxo>,
    #[serde(default)]
    funding: Vec<PayerUtxo>,
    now_ms: Num,
    #[serde(default)]
    options: IntentOptionsIn,
    #[serde(default)]
    tokens: Option<Vec<TokenSpec>>,
    #[serde(default)]
    allow_issuer_controlled: bool,
}

fn intent_ctx(p: &IntentIn, need_secrets: bool) -> R<SwapCtx> {
    let offer = requirements_of(p.offer.clone())?;
    let network = offer.network().map_err(xerr)?;
    let keys: Vec<[u8; 32]> = if need_secrets || !p.secret_keys.is_empty() {
        secrets_of(&p.secret_keys)?.keys().copied().collect()
    } else {
        vec![h32("payerPublicKey", p.payer_public_key.as_deref().ok_or_else(|| perr("payerPublicKey or secretKeys required"))?)?]
    };
    let change = match &p.payer_public_key {
        Some(k) => h32("payerPublicKey", k)?,
        None => keys[0],
    };
    let funds = PayerFunds { tokens: p.token_utxos.clone(), funding: funding_for(&p.funding, &keys)?, change };
    Ok(SwapCtx {
        policy: policy_for(network, &offer, &p.tokens, p.allow_issuer_controlled, &p.options.max_carrier_sompi)?,
        offer,
        funds,
        keys,
    })
}

/// What a payer keeps of an intent payment to cancel it later.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntentHandle {
    actor: String,
    state: kob_protocol::router::IntentState,
    intent: Utxo,
    #[serde(default)]
    lock: Option<TokenUtxo>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreparedIntentJson {
    built: BuiltTx,
    offer: PaymentRequirements,
    request_hash: String,
    pay_asset: String,
    actor: String,
    state: kob_protocol::router::IntentState,
    terms: kob_x402::wire::IntentTerms,
    digest: String,
    expires_at: String,
    payer_address: Option<String>,
    worst_kas: String,
    worst_tokens: String,
    payment_identifier: Option<String>,
}

impl PreparedIntentJson {
    fn from(p: &kob_x402::client::intent::PreparedIntent) -> Self {
        PreparedIntentJson {
            built: p.built.clone(),
            offer: p.offer.clone(),
            request_hash: p.request_hash.clone(),
            pay_asset: p.pay_asset.clone(),
            actor: p.actor.clone(),
            state: p.state.clone(),
            terms: p.terms.clone(),
            digest: hex(&p.digest),
            expires_at: p.expires_at.clone(),
            payer_address: p.payer_address.clone(),
            worst_kas: p.worst_kas.to_string(),
            worst_tokens: p.worst_tokens.to_string(),
            payment_identifier: p.payment_identifier.clone(),
        }
    }
    fn into_prepared(self) -> R<kob_x402::client::intent::PreparedIntent> {
        Ok(kob_x402::client::intent::PreparedIntent {
            built: self.built,
            offer: self.offer,
            request_hash: self.request_hash,
            pay_asset: self.pay_asset,
            actor: self.actor,
            state: self.state,
            terms: self.terms,
            digest: h32("digest", &self.digest)?,
            expires_at: self.expires_at,
            payer_address: self.payer_address,
            worst_kas: u64s("worstKas", &self.worst_kas)?,
            worst_tokens: self.worst_tokens.parse().map_err(|_| perr("worstTokens"))?,
            payment_identifier: self.payment_identifier,
        })
    }
}

/// The payer's worst case in units of the pay asset: the units the intent may sell, or its KAS.
fn worst_of(p: &kob_x402::client::intent::PreparedIntent) -> u64 {
    if p.worst_tokens > 0 {
        p.worst_tokens as u64
    } else {
        p.worst_kas
    }
}

fn intent_out(p: kob_x402::client::intent::IntentPayment, worst: u64) -> R<String> {
    let handle = IntentHandle { actor: p.actor.clone(), state: p.state.clone(), intent: p.intent.clone(), lock: p.lock.clone() };
    let base: Value = serde_json::from_str(&pay_out(p.payload, Some(worst), vec![])?).map_err(|e| e.to_string())?;
    let Value::Object(mut o) = base else { return Err(perr("unexpected pay output")) };
    o.insert("intent".into(), serde_json::to_value(&handle).map_err(|e| e.to_string())?);
    out(&Value::Object(o))
}

/// Step 1 of the wallet flow: builds the unsigned intent creation (`built.sign` = what the wallet signs, once).
pub fn prepare_intent(json: &str) -> R<String> {
    let p: IntentIn = parse("request", json)?;
    let c = intent_ctx(&p, false)?;
    let prepared = kob_x402::client::intent::prepare_intent(
        &c.policy,
        &c.offer,
        &p.pay_asset,
        &p.request_hash,
        &c.funds,
        p.now_ms.clone().get("nowMs")?,
        &p.options.build()?,
    )
    .map_err(xerr)?;
    out(&PreparedIntentJson::from(&prepared))
}

/// Step 2 of the wallet flow: the payment payload (`paymentPayload`) and the intent handle (`intent`) the payer keeps
/// to cancel the intent.
pub fn finish_intent(prepared: &str, sigs: &str) -> R<String> {
    let p: PreparedIntentJson = parse("prepared", prepared)?;
    let prepared = p.into_prepared()?;
    let worst = worst_of(&prepared);
    intent_out(prepared.complete(&signatures(sigs)?).map_err(xerr)?, worst)
}

/// Builds, signs locally and assembles an intent payment.
pub fn pay_intent(json: &str) -> R<String> {
    let p: IntentIn = parse("request", json)?;
    let c = intent_ctx(&p, true)?;
    let secrets = secrets_of(&p.secret_keys)?;
    let prepared = kob_x402::client::intent::prepare_intent(
        &c.policy,
        &c.offer,
        &p.pay_asset,
        &p.request_hash,
        &c.funds,
        p.now_ms.clone().get("nowMs")?,
        &p.options.build()?,
    )
    .map_err(xerr)?;
    let worst = worst_of(&prepared);
    let sigs = prepared.sign_with(&secrets).map_err(xerr)?;
    let _ = c.keys;
    intent_out(prepared.complete(&sigs).map_err(xerr)?, worst)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelIn {
    intent: IntentHandle,
    #[serde(default)]
    secret_keys: Vec<String>,
    #[serde(default)]
    fee_rate: Option<u64>,
}

/// The payer's cancel of an intent: `{ built }` (unsigned, the wallet signs the intent input) or, with `secretKeys`,
/// `{ transaction (safe JSON text), transactionId }`. The intent's KAS and its locked tokens return to the payer.
pub fn cancel_intent(json: &str) -> R<String> {
    let p: CancelIn = parse("request", json)?;
    let req = kob_protocol::build::CancelIntent {
        actor: p.intent.actor,
        state: p.intent.state,
        intent: p.intent.intent,
        lock: p.intent.lock,
        to: None,
        fee: kob_protocol::tx::FeeOptions { fee_rate: p.fee_rate, ..Default::default() },
    };
    let built =
        kob_protocol::build::build_cancel_intent(&req, &kob_protocol::build::intent_budgets).map_err(|e| perr(e.to_string()))?;
    if p.secret_keys.is_empty() {
        return out(&json!({ "built": built }));
    }
    let sigs = kob_protocol::tx::sign_locally(&built, &secrets_of(&p.secret_keys)?).map_err(|e| perr(e.to_string()))?;
    finish_cancel_built(&built, &sigs)
}

fn finish_cancel_built(built: &BuiltTx, sigs: &[InputSignature]) -> R<String> {
    let signed = kob_protocol::tx::finalize(built, sigs, kob_protocol::tx::FinalizeOptions { tighten_budgets: true })
        .map_err(|e| perr(e.to_string()))?;
    let (tx, _) = signed.tx.to_tx().map_err(|e| perr(e.to_string()))?;
    out(&json!({
        "transaction": serde_json::to_string(&signed.tx).map_err(|e| e.to_string())?,
        "transactionId": hex(&tx.id().as_bytes()),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpireIn {
    intent: IntentHandle,
    #[serde(default)]
    fee_rate: Option<u64>,
}

/// The expiry of an intent (C5 X-10: anyone may submit it once the node's past median time reached the intent's deadline, so
/// a payer or a keeper need not wait for the facilitator that took the payment): `{ transaction (safe JSON text),
/// transactionId, lockTime }`. No signature: the intent's KAS (less at most `EXPIRE_MAX_FEE`) and its locked tokens return
/// to the payer's key.
pub fn expire_intent(json: &str) -> R<String> {
    let p: ExpireIn = parse("request", json)?;
    let req = kob_protocol::build::ExpireIntent {
        actor: p.intent.actor,
        state: p.intent.state,
        intent: p.intent.intent,
        lock: p.intent.lock,
        fee: kob_protocol::tx::FeeOptions { fee_rate: p.fee_rate, ..Default::default() },
    };
    let built =
        kob_protocol::build::build_expire_intent(&req, &kob_protocol::build::intent_budgets).map_err(|e| perr(e.to_string()))?;
    let signed = kob_protocol::tx::finalize(&built, &[], kob_protocol::tx::FinalizeOptions { tighten_budgets: true })
        .map_err(|e| perr(e.to_string()))?;
    let (tx, _) = signed.tx.to_tx().map_err(|e| perr(e.to_string()))?;
    out(&json!({
        "transaction": serde_json::to_string(&signed.tx).map_err(|e| e.to_string())?,
        "transactionId": hex(&tx.id().as_bytes()),
        "lockTime": tx.lock_time.to_string(),
    }))
}

/// Step 2 of the wallet cancel: the signed cancel from `{ built }` and the wallet's signatures.
pub fn finish_cancel(built: &str, sigs: &str) -> R<String> {
    #[derive(Deserialize)]
    struct B {
        built: BuiltTx,
    }
    let b: B = parse("built", built)?;
    finish_cancel_built(&b.built, &signatures(sigs)?)
}

// ------------------------------------------------------------------------------------------------ invoices

fn invoice_of(json: &str) -> R<kob_x402::invoice::Invoice> {
    parse("invoice", json)
}

/// The id of an invoice (SHA-256 of its canonical JSON, hex).
pub fn invoice_id(json: &str) -> R<String> {
    invoice_of(json)?.id_hex().map_err(xerr)
}

/// Checks that a fetched invoice hashes to `id` and is structurally valid at `nowMs` (version, network, reference,
/// expiry, accepts); returns `{ id, expiresAtMs, kaspaUri }` (`kaspaUri`: the `kaspa:` fallback of a KAS invoice, or null).
pub fn check_invoice(json: &str, id: &str, now_ms: u64) -> R<String> {
    let inv = invoice_of(json)?;
    kob_x402::invoice::check_id(&inv, id).map_err(xerr)?;
    let network = net(&inv.network)?;
    let exp = inv.validate(network, now_ms, u64::MAX / 2).map_err(xerr)?;
    out(&json!({ "id": id.to_ascii_lowercase(), "expiresAtMs": exp, "kaspaUri": inv.kaspa_uri() }))
}
