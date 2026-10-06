//! x402 v2 wire types for the Kaspa binding.
//!
//! Field names follow the elldeeone/kaspa-x402 v1.0.0-rc.1 schemas (`schemas/*.json`,
//! `spec/kaspa-exact-v2.md`). Requirements and payloads keep unknown fields (`#[serde(flatten)]`)
//! because the requirements hash covers the *complete* selected object.
//!
//! KOB additions, both proposals to the binding (`docs/spec/x402-kcc20-profile.md`,
//! `docs/spec/x402-swap-and-pay.md`):
//!
//! * profile `kcc20` next to `standard-native` / `additive`, with `asset` = KCC-20 covenant id and
//!   `extra.token`;
//! * `extra.route` (binding `kob-swap-v1`) on a requirement, and `payload.route`;
//! * authorization version [`AUTH_VERSION_PAYLOAD`] (a commitment inside the transaction payload)
//!   next to the binding's Schnorr-signed [`AUTH_VERSION_SIGNED`].

use std::fmt;

use kaspa_addresses::Prefix;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Diag, Reason, Result, X402Error};

pub const X402_VERSION: u64 = 2;
pub const SCHEME_EXACT: &str = "exact";
/// `extra.binding` of every exact requirement.
pub const BINDING_EXACT: &str = "kaspa-exact-v2";
/// The only transaction interchange encoding of the binding.
pub const TX_ENCODING: &str = "kaspa-sdk-safe-json-v2.0.0";
pub const ASSET_KAS: &str = "KAS";
/// Payload `type` of an exact payment.
pub const PAYLOAD_EXACT_TX: &str = "exact-transaction";
/// Authorization: Schnorr signature over the request digest by a standard P2PK input (the binding).
pub const AUTH_VERSION_SIGNED: &str = "kaspa-x402-exact-request-authorization-v1";
/// Authorization: the digest is committed in the transaction payload, so every input authorizer's
/// SIGHASH_ALL covers it (token profile, swap-and-pay).
pub const AUTH_VERSION_PAYLOAD: &str = "kob-x402-payload-commitment-v1";
/// `extra.route.binding` of swap-and-pay.
pub const BINDING_SWAP: &str = "kob-swap-v1";
/// `extra.route.binding` of intent-based swap-and-pay: the payer signs a KOB router intent once and the facilitator
/// executes it (`docs/spec/x402-swap-and-pay.md`, intent mode).
pub const BINDING_INTENT: &str = "kob-intent-v1";

/// A Kaspa network identifier (`kaspa:mainnet`, `kaspa:testnet-10`). Non-colon aliases are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Network {
    Mainnet,
    Testnet10,
}

impl Network {
    pub fn as_str(self) -> &'static str {
        match self {
            Network::Mainnet => "kaspa:mainnet",
            Network::Testnet10 => "kaspa:testnet-10",
        }
    }
    pub fn parse(s: &str) -> Option<Network> {
        match s {
            "kaspa:mainnet" => Some(Network::Mainnet),
            "kaspa:testnet-10" => Some(Network::Testnet10),
            _ => None,
        }
    }
    /// Address prefix (`kaspa` / `kaspatest`).
    pub fn prefix(self) -> Prefix {
        match self {
            Network::Mainnet => Prefix::Mainnet,
            Network::Testnet10 => Prefix::Testnet,
        }
    }
    /// Name used by the KOB registry (`mainnet`, `testnet-10`).
    pub fn registry_name(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet10 => "testnet-10",
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Exact profile. `Kcc20` is the KOB token profile proposed to the binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    StandardNative,
    Additive,
    Kcc20,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::StandardNative => "standard-native",
            Profile::Additive => "additive",
            Profile::Kcc20 => "kcc20",
        }
    }
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "standard-native" => Some(Profile::StandardNative),
            "additive" => Some(Profile::Additive),
            "kcc20" => Some(Profile::Kcc20),
            _ => None,
        }
    }
}

/// Finality ordering of the binding: `mempool < accepted < confirmed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Finality {
    Mempool,
    Accepted,
    Confirmed,
}

impl Finality {
    pub fn as_str(self) -> &'static str {
        match self {
            Finality::Mempool => "mempool",
            Finality::Accepted => "accepted",
            Finality::Confirmed => "confirmed",
        }
    }
    pub fn parse(s: &str) -> Option<Finality> {
        match s {
            "mempool" => Some(Finality::Mempool),
            "accepted" => Some(Finality::Accepted),
            "confirmed" => Some(Finality::Confirmed),
            _ => None,
        }
    }
    /// True if `self` (what was observed) satisfies `required`.
    pub fn satisfies(self, required: Finality) -> bool {
        self >= required
    }
}

/// `resource` of a `PaymentRequired`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// One offer of `accepts` (x402 v2 `PaymentRequirements`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    pub scheme: String,
    pub network: String,
    /// Decimal string: sompi for KAS, token base units for `kcc20`.
    pub amount: String,
    /// `"KAS"` or a KCC-20 covenant id (64 lowercase hex).
    pub asset: String,
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    #[serde(default)]
    pub extra: Map<String, Value>,
    /// Unknown top-level fields: kept because the requirements hash covers them.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// The `402` body (and `PAYMENT-REQUIRED` header) object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    pub x402_version: u64,
    pub resource: Resource,
    pub accepts: Vec<PaymentRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
}

/// `payload.authorization` (both authorization versions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Authorization {
    /// [`AUTH_VERSION_SIGNED`] or [`AUTH_VERSION_PAYLOAD`].
    pub version: String,
    /// Signed version only: the funding input whose P2PK key signed the digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_index: Option<u32>,
    /// ISO-8601 UTC, millisecond precision (`2099-01-01T00:00:00.000Z`).
    pub expires_at: String,
    /// 32-byte hex digest.
    pub digest: String,
    /// Signed version only: 64-byte Schnorr signature over the digest (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// `payload.route` of a swap-and-pay payment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePayload {
    /// [`BINDING_SWAP`].
    pub binding: String,
    /// Covenant id (64 hex) of the token the payer pays with.
    pub pay_asset: String,
    /// Order outpoints the payer names (hints; the verifier derives the orders from the transaction).
    #[serde(default)]
    pub orders: Vec<OutpointJson>,
    /// Intent mode only ([`BINDING_INTENT`]): the router actor and the payer's terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<IntentTerms>,
}

/// `payload.route.intent`: what the facilitator needs, besides the offer, to recompute the intent's script. Amounts are
/// canonical decimal strings, keys 64 lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntentTerms {
    /// Router actor (`KasToToken_buy`, `TokenToKas_sell2`, ...): the fill shape the keeper uses.
    pub actor: String,
    /// The payer's x-only key: KAS / token change, `cancel`.
    pub payer: String,
    /// KasToToken: most sompi the asks may demand at their quotes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pay: Option<String>,
    /// KasToToken: most sompi carriers, fillers and the network fee may take besides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_extra: Option<String>,
    /// TokenToKas / TokenSwap: most units of the pay token sold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sell: Option<String>,
    /// TokenToKas / TokenSwap: index of the locked token output in the creation transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_output_index: Option<u32>,
    /// TokenToKas / TokenSwap: units locked in the intent (at least `maxSell`; default `maxSell`). The rest returns to
    /// the payer at execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_amount: Option<String>,
}

/// `{ "txid": <64 hex>, "index": n }` as used by the binding's outpoint fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutpointJson {
    pub txid: String,
    pub index: u32,
}

/// The `exact-transaction` payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactPayload {
    /// [`PAYLOAD_EXACT_TX`].
    #[serde(rename = "type")]
    pub kind: String,
    pub profile: String,
    /// Receipt metadata only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_address: Option<String>,
    /// The signed transaction as `kaspa-sdk-safe-json-v2.0.0` (a JSON text).
    pub transaction: String,
    pub transaction_encoding: String,
    pub payment_output_index: u32,
    pub request_hash: String,
    /// Additive profile only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_id: Option<String>,
    pub authorization: Authorization,
    /// Swap-and-pay only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RoutePayload>,
}

/// The `PAYMENT-SIGNATURE` object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u64,
    /// The selected offer, as the payer received it (compared canonically with the server's offer).
    pub accepted: PaymentRequirements,
    pub payload: ExactPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
}

/// Body of `POST /verify` and `POST /settle` (x402 v2 facilitator shape plus the binding's
/// mandatory `requestHash`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest {
    pub x402_version: u64,
    pub payment_payload: PaymentPayload,
    pub payment_requirements: PaymentRequirements,
    /// The resource server's independently computed request fingerprint (32-byte hex). Mandatory.
    #[serde(default)]
    pub request_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Value>,
}

/// `POST /verify` response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub is_valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// KOB: local diagnostic, retryability and details of a failure (`extensions.kaspa`-shaped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
}

/// `PAYMENT-RESPONSE` / `POST /settle` response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementResponse {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// Recomputed transaction id, or `""` on failure.
    pub transaction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
}

impl SettlementResponse {
    /// A failed response for an error (`extensions.kaspa` carries the diagnostic, retryability, details).
    pub fn failure(network: Network, payer: Option<String>, err: &X402Error) -> SettlementResponse {
        SettlementResponse {
            success: false,
            error_reason: Some(err.reason.as_str().to_string()),
            transaction: String::new(),
            network: Some(network.as_str().to_string()),
            payer,
            amount: None,
            extensions: Some(failure_extension(err)),
        }
    }
}

/// `{ "kaspa": { "diagnostic", "retryable", "message", "details"? } }` for failed responses.
pub fn failure_extension(err: &X402Error) -> Value {
    let mut k = Map::new();
    k.insert("diagnostic".into(), Value::String(err.diag.as_str().into()));
    k.insert("retryable".into(), Value::Bool(err.retryable));
    k.insert("message".into(), Value::String(err.message.clone()));
    if let Some(d) = &err.details {
        k.insert("details".into(), d.clone());
    }
    let mut o = Map::new();
    o.insert("kaspa".into(), Value::Object(k));
    Value::Object(o)
}

// ---------------------------------------------------------------------------- helpers

/// Canonical unsigned 64-bit decimal: `"0"` or a non-zero digit followed by digits; no sign, no
/// leading zeroes, no whitespace, at most `u64::MAX`.
pub fn parse_u64_canonical(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) || (b.len() > 1 && b[0] == b'0') {
        return None;
    }
    s.parse().ok()
}

/// Exactly 32 bytes of hex (either case).
pub fn parse_hash32(s: &str) -> Option<[u8; 32]> {
    kob_protocol::json::hex32(s).ok()
}

/// Lowercase hex.
pub fn hex(b: &[u8]) -> String {
    kob_protocol::json::to_hex(b)
}

impl PaymentRequirements {
    /// Validated network of the requirement.
    pub fn network(&self) -> Result<Network> {
        Network::parse(&self.network).ok_or_else(|| {
            X402Error::new(
                Reason::InvalidNetwork,
                Diag::InvalidKaspaX402Binding,
                format!("network {:?} is not a Kaspa network identifier", self.network),
            )
        })
    }
    /// `amount` as an integer (canonical decimal, positive).
    pub fn amount_u64(&self) -> Result<u64> {
        match parse_u64_canonical(&self.amount) {
            Some(a) if a > 0 => Ok(a),
            _ => Err(X402Error::requirements(
                Diag::InvalidKaspaX402Amount,
                format!("amount {:?} is not a canonical positive uint64", self.amount),
            )),
        }
    }
    /// `extra.<key>` as a string.
    pub fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }
    /// The declared profile (`extra.profile`).
    pub fn profile(&self) -> Result<Profile> {
        Profile::parse(self.extra_str("profile").unwrap_or("")).ok_or_else(|| {
            X402Error::requirements(
                Diag::UnsupportedKaspaExactProfile,
                format!("unsupported exact profile {:?}", self.extra_str("profile")),
            )
        })
    }
    /// The declared finality (`extra.finality`), the binding allows `accepted` and `confirmed`.
    pub fn finality(&self) -> Result<Finality> {
        match Finality::parse(self.extra_str("finality").unwrap_or("")) {
            Some(f) if f >= Finality::Accepted => Ok(f),
            _ => Err(X402Error::requirements(Diag::InvalidKaspaExactFinality, "extra.finality must be accepted or confirmed")),
        }
    }
    /// True if the offer carries `extra.route` (swap-and-pay).
    pub fn has_route(&self) -> bool {
        self.extra.contains_key("route")
    }
    /// `extra.route.binding`, when the offer carries a route.
    pub fn route_binding(&self) -> Option<&str> {
        self.extra.get("route").and_then(|r| r.get("binding")).and_then(Value::as_str)
    }
    /// True if the offer is an intent-based swap-and-pay entry (`extra.route.binding` = [`BINDING_INTENT`]).
    pub fn is_intent(&self) -> bool {
        self.route_binding() == Some(BINDING_INTENT)
    }
}

/// Base64 (standard alphabet, padded) of the compact JSON with lexicographically sorted keys: the
/// header encoding of the binding's HTTP profile (`PAYMENT-REQUIRED`, `PAYMENT-SIGNATURE`,
/// `PAYMENT-RESPONSE`).
pub fn header_encode<T: Serialize>(value: &T) -> Result<String> {
    use base64::Engine;
    let v = serde_json::to_value(value).map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, e.to_string()))?;
    let text = crate::canonical::canonical_json(&v)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(text.as_bytes()))
}

/// Decodes a header value into `T`.
pub fn header_decode<T: for<'de> Deserialize<'de>>(header: &str) -> Result<T> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(header.trim())
        .map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, format!("header is not base64: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, format!("header is not the expected JSON: {e}")))
}
