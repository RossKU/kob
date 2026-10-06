//! Invoices (`kob-invoice-v1`, proposal `docs/spec/x402-swap-and-pay.md`, "Invoices"): the x402 payment
//! requirements of one sale plus the merchant's reference and an expiry, served at a URL.
//!
//! ```json
//! {
//!   "x402Version": 2,
//!   "invoiceVersion": "kob-invoice-v1",
//!   "network": "kaspa:testnet-10",
//!   "reference": "order-1234",
//!   "expiresAt": "2026-10-01T12:00:00.000Z",
//!   "memo": "2 coffees",
//!   "accepts": [ { "scheme": "exact", "...": "PaymentRequirements, routes included" } ]
//! }
//! ```
//!
//! * The **id** is the SHA-256 of the invoice's canonical JSON (the binding's canonical JSON, as for the
//!   requirements hash): the invoice is content-addressed, so a URL that carries the id is
//!   self-verifying; a payer recomputes the id of what it fetched and refuses a mismatch. The facilitator
//!   serves a registered invoice at `GET /invoices/<id>`; that URL is what a QR code carries.
//! * The **request hash** of an invoice payment is the id: the payment's authorization commits to the
//!   invoice (and so to its reference and expiry) and cannot be presented for another invoice or offer.
//! * One invoice is paid once: the facilitator settles at most one payment per invoice; a second payment
//!   (duplicate) or one submitted after `expiresAt` (late) is refused before broadcast and kept as
//!   evidence, and if it reaches the chain anyway it is reported, so a refund can be made.
//!
//! This module holds the types, the id, validation and the fallback `kaspa:` URI; the store and the
//! endpoints live in `kob-executor` (`x402::invoices`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canonical::canonical_hash;
use crate::common::{iso_from_ms, parse_iso_ms};
use crate::error::{Diag, Result, X402Error};
use crate::wire::{hex, parse_hash32, Network, PaymentRequirements, ASSET_KAS, SCHEME_EXACT, X402_VERSION};

/// `invoiceVersion` of this revision.
pub const INVOICE_VERSION: &str = "kob-invoice-v1";
/// Most `accepts` entries of an invoice.
pub const MAX_ACCEPTS: usize = 16;
/// Longest `reference` (printable ASCII).
pub const MAX_REFERENCE: usize = 128;
/// Longest `memo` (characters).
pub const MAX_MEMO: usize = 280;

/// An invoice (see the module docs). Unknown fields are refused: everything is inside the id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Invoice {
    pub x402_version: u64,
    pub invoice_version: String,
    pub network: String,
    /// The merchant's own identifier of the sale (order number, ...).
    pub reference: String,
    /// ISO-8601 UTC with milliseconds; no payment is settled at or after it.
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    /// Ways to pay: exact requirements of any profile, payer-signed (`kob-swap-v1`) and intent
    /// (`kob-intent-v1`) routes included. Any one of them pays the invoice.
    pub accepts: Vec<PaymentRequirements>,
}

fn bad(msg: impl Into<String>) -> X402Error {
    X402Error::requirements(Diag::InvalidInvoice, msg)
}

impl Invoice {
    /// A new invoice on `network`.
    pub fn new(
        network: Network,
        reference: impl Into<String>,
        expires_at_ms: u64,
        memo: Option<String>,
        accepts: Vec<PaymentRequirements>,
    ) -> Invoice {
        Invoice {
            x402_version: X402_VERSION,
            invoice_version: INVOICE_VERSION.into(),
            network: network.as_str().into(),
            reference: reference.into(),
            expires_at: iso_from_ms(expires_at_ms),
            memo,
            accepts,
        }
    }

    /// The invoice id: SHA-256 of its canonical JSON.
    pub fn id(&self) -> Result<[u8; 32]> {
        let v = serde_json::to_value(self).map_err(|e| bad(e.to_string()))?;
        canonical_hash(&v)
    }

    /// The id as lowercase hex.
    pub fn id_hex(&self) -> Result<String> {
        Ok(hex(&self.id()?))
    }

    /// `expiresAt` in unix milliseconds.
    pub fn expires_ms(&self) -> Result<u64> {
        parse_iso_ms(&self.expires_at).ok_or_else(|| bad("expiresAt is not a millisecond ISO-8601 UTC timestamp"))
    }

    /// Structural validation: version, network, reference, memo, expiry within `(now, now + horizon]`,
    /// 1..=16 exact requirements on the same network. Whether the facilitator can settle each entry is
    /// checked by the facilitator (allowlists, routes) when the invoice is registered.
    pub fn validate(&self, network: Network, now_ms: u64, max_horizon_ms: u64) -> Result<u64> {
        if self.x402_version != X402_VERSION {
            return Err(bad("x402Version must be 2"));
        }
        if self.invoice_version != INVOICE_VERSION {
            return Err(bad("invoiceVersion must be kob-invoice-v1"));
        }
        if self.network != network.as_str() {
            return Err(bad(format!("this facilitator serves {network}")));
        }
        if self.reference.is_empty()
            || self.reference.len() > MAX_REFERENCE
            || !self.reference.bytes().all(|b| (0x20..0x7f).contains(&b))
        {
            return Err(bad(format!("reference must be 1..={MAX_REFERENCE} printable ASCII characters")));
        }
        if self.memo.as_ref().is_some_and(|m| m.chars().count() > MAX_MEMO || m.chars().any(char::is_control)) {
            return Err(bad(format!("memo must be at most {MAX_MEMO} characters without control characters")));
        }
        let exp = self.expires_ms()?;
        if exp <= now_ms {
            return Err(X402Error::state(Diag::InvoiceExpired, "the invoice has expired"));
        }
        if exp > now_ms.saturating_add(max_horizon_ms) {
            return Err(bad("expiresAt is further away than this facilitator accepts"));
        }
        if self.accepts.is_empty() || self.accepts.len() > MAX_ACCEPTS {
            return Err(bad(format!("accepts must list 1..={MAX_ACCEPTS} requirements")));
        }
        for (i, r) in self.accepts.iter().enumerate() {
            if r.scheme != SCHEME_EXACT || r.network != self.network {
                return Err(bad(format!("accepts[{i}] must be an exact requirement on {}", self.network)));
            }
            r.amount_u64().map_err(|e| bad(format!("accepts[{i}]: {}", e.message)))?;
            r.profile().map_err(|e| bad(format!("accepts[{i}]: {}", e.message)))?;
        }
        for (i, a) in self.accepts.iter().enumerate() {
            for b in &self.accepts[i + 1..] {
                if crate::canonical::requirements_hash(a)? == crate::canonical::requirements_hash(b)? {
                    return Err(bad("accepts lists the same requirement twice"));
                }
            }
        }
        Ok(exp)
    }

    /// The request hash of a payment of this invoice: the invoice id.
    pub fn request_hash(&self) -> Result<String> {
        self.id_hex()
    }

    /// The index of `accepted` in `accepts` (canonical JSON equality).
    pub fn find(&self, accepted: &PaymentRequirements) -> Result<Option<usize>> {
        let h = crate::canonical::requirements_hash(accepted)?;
        for (i, r) in self.accepts.iter().enumerate() {
            if crate::canonical::requirements_hash(r)? == h {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// The `kaspa:` payment URI of a KAS invoice (the first `standard-native` entry without a route):
    /// `<payTo>?amount=<KAS>` with the amount in KAS (8 decimals, trailing zeros trimmed). A wallet that
    /// pays it sends a plain KAS transaction the facilitator does not see: the merchant watches its
    /// address (no invoice status). `None` when no entry is a plain KAS payment.
    pub fn kaspa_uri(&self) -> Option<String> {
        let r = self
            .accepts
            .iter()
            .find(|r| r.asset == ASSET_KAS && !r.has_route() && r.extra_str("profile") == Some("standard-native"))?;
        let sompi = r.amount_u64().ok()?;
        Some(format!("{}?amount={}", r.pay_to, kas_decimal(sompi)))
    }
}

/// Sompi as a KAS decimal string (`150000000` -> `1.5`).
pub fn kas_decimal(sompi: u64) -> String {
    let whole = sompi / 100_000_000;
    let frac = sompi % 100_000_000;
    if frac == 0 {
        return whole.to_string();
    }
    let f = format!("{frac:08}");
    format!("{whole}.{}", f.trim_end_matches('0'))
}

/// The URL a registered invoice is served at (`<base>/invoices/<id>`): what a QR code carries.
pub fn invoice_url(base: &str, id_hex: &str) -> String {
    format!("{}/invoices/{}", base.trim_end_matches('/'), id_hex)
}

/// Checks that a fetched invoice is the one an URL (or id) names.
pub fn check_id(inv: &Invoice, id_hex: &str) -> Result<()> {
    let want = parse_hash32(id_hex).ok_or_else(|| bad("the invoice id is not 32-byte hex"))?;
    if inv.id()? != want {
        return Err(bad("the invoice does not hash to its id"));
    }
    Ok(())
}

/// Invoice-level settlement state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvoiceState {
    /// No payment was submitted (or every attempt was refused before broadcast) and it has not expired.
    Unpaid,
    /// A payment is being settled (broadcast, or an intent being executed); retry the status later.
    Pending,
    /// A payment reached the required finality.
    Paid,
    /// Expired without a payment.
    Expired,
    /// The last attempt failed definitively; the invoice can still be paid until it expires.
    Failed,
}

/// The payment that settled an invoice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoicePayment {
    /// The transaction that pays the merchant (an intent's execution, not its creation).
    pub transaction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_daa_score: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// Index of the paid entry in `accepts`.
    pub accepted_index: usize,
    /// The settlement response the facilitator returned (cached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

/// One attempt to pay an invoice (what the facilitator's ledger says of it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceAttempt {
    /// The payment transaction (an intent: its creation).
    pub transaction: String,
    /// Ledger state: pending, broadcast, accepted, failed, ambiguous.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A payment the facilitator refused (duplicate or late) or did not settle, that it keeps watching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtraPayment {
    /// `duplicate` (the invoice was paid or being paid) or `late` (submitted at or after `expiresAt`).
    pub kind: String,
    pub transaction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// `refused` (never broadcast by the facilitator), `accepted` (it reached the chain anyway: refund it),
    /// `spent` (an intent of it was spent outside the facilitator: executed or cancelled).
    pub observed: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_daa_score: Option<String>,
    pub at: String,
}

/// `GET /invoices/<id>/status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceStatus {
    pub id: String,
    pub reference: String,
    pub status: InvoiceState,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment: Option<InvoicePayment>,
    #[serde(default)]
    pub attempts: Vec<InvoiceAttempt>,
    /// Duplicate and late payments (refund candidates once `observed` is `accepted`).
    #[serde(default)]
    pub extra_payments: Vec<ExtraPayment>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::native::native_requirements;

    #[test]
    fn id_uri_and_validation() {
        let addr = kaspa_addresses::Address::new(
            kaspa_addresses::Prefix::Testnet,
            kaspa_addresses::Version::PubKey,
            &crate::testkit::pubkey(2),
        );
        let pay_to = addr.to_string();
        let r = native_requirements(Network::Testnet10, 150_000_000, &pay_to, 60, crate::wire::Finality::Accepted).unwrap();
        let now = 1_800_000_000_000;
        let inv = Invoice::new(Network::Testnet10, "order-1", now + 600_000, Some("two coffees".into()), vec![r.clone()]);
        let id = inv.id_hex().unwrap();
        check_id(&inv, &id).unwrap();
        let mut other = inv.clone();
        other.reference = "order-2".into();
        assert!(check_id(&other, &id).is_err(), "the reference is inside the id");
        assert_eq!(inv.validate(Network::Testnet10, now, 86_400_000).unwrap(), now + 600_000);
        assert_eq!(inv.validate(Network::Testnet10, now + 600_000, 86_400_000).unwrap_err().diag, Diag::InvoiceExpired);
        assert!(inv.validate(Network::Testnet10, now, 60_000).is_err(), "beyond the horizon");
        assert!(inv.validate(Network::Mainnet, now, 86_400_000).is_err());
        assert_eq!(inv.kaspa_uri().unwrap(), format!("{pay_to}?amount=1.5"));
        assert_eq!(inv.find(&r).unwrap(), Some(0));
        let twice = Invoice { accepts: vec![r.clone(), r], ..inv.clone() };
        assert!(twice.validate(Network::Testnet10, now, 86_400_000).is_err());
        assert_eq!(kas_decimal(100_000_000), "1");
        assert_eq!(kas_decimal(1), "0.00000001");
        assert_eq!(invoice_url("https://f.example/", &id), format!("https://f.example/invoices/{id}"));
        // unknown fields are refused (everything is inside the id)
        let mut v = serde_json::to_value(&inv).unwrap();
        v["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Invoice>(v).is_err());
    }
}
