//! The verification entry point: `verify_payment` runs the complete pre-broadcast verification of an
//! `exact-transaction` payment for every profile this crate knows and returns the facts the
//! facilitator needs to settle it ([`Verified`]).
//!
//! Dispatch:
//!
//! * `route.binding` `kob-intent-v1` (payload or offer) -> intent-based swap-and-pay ([`crate::intent`]);
//! * `payload.route` / `accepted.extra.route` present otherwise -> swap-and-pay ([`crate::swap`]);
//! * profile `standard-native` -> [`crate::exact`] (the binding, KAS);
//! * profile `kcc20` -> [`crate::token`] (KOB token profile);
//! * profile `additive` -> not supported here (`unsupported_kaspa_exact_profile`; KOB advertises only
//!   the profiles it implements in `/supported`).
//!
//! Every verifier follows the binding's step list ("Verification", steps 1 to 11): shape checks,
//! `payToScriptPublicKey` re-derivation, size bounds, canonical decoding and id recomputation,
//! trusted UTXO resolution, hint comparison, signature / witness execution through the script
//! engine, exact merchant gain, fee and mass recomputation, request authorization.

use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use serde_json::{Map, Value};

use crate::chain::{ChainView, Clock, Outpoint, Txid};
use crate::error::{Diag, Reason, Result, X402Error};
use crate::policy::{Custody, Policy};
use crate::wire::{Finality, PaymentPayload, PaymentRequirements, Profile};
use kaspa_consensus_core::tx::ScriptPublicKey;

/// What kind of payment was verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaymentKind {
    /// KAS `standard-native` (the binding).
    Native,
    /// KCC-20 transfer (`kcc20` profile).
    Kcc20,
    /// Swap-and-pay, merchant receives KAS.
    SwapToKas,
    /// Swap-and-pay, merchant receives a KCC-20 token.
    SwapToToken,
    /// Intent-based swap-and-pay, merchant receives KAS (the verified transaction is the intent creation).
    IntentToKas,
    /// Intent-based swap-and-pay, merchant receives a KCC-20 token.
    IntentToToken,
}

/// The merchant output the facilitator observes for acceptance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedOutput {
    pub outpoint: Outpoint,
    pub script_public_key: ScriptPublicKey,
    pub amount: u64,
}

/// A fully verified payment, ready to be consumed, broadcast and observed.
#[derive(Clone, Debug)]
pub struct Verified {
    pub kind: PaymentKind,
    pub profile: Profile,
    /// Recomputed transaction id.
    pub txid: Txid,
    pub tx: Transaction,
    /// Trusted UTXO entries of every input.
    pub entries: Vec<UtxoEntry>,
    /// Payer address (from the authoritative funding input) for receipts.
    pub payer_address: Option<String>,
    /// Advertised amount (sompi for KAS, token base units otherwise).
    pub amount: u64,
    pub payment_output_index: u32,
    pub merchant_output: WatchedOutput,
    /// Every outpoint the transaction spends (the replay ledger reserves them).
    pub consumed: Vec<Outpoint>,
    /// Swap-and-pay: the KOB order outpoints the transaction spends (conflict detection).
    pub order_inputs: Vec<Outpoint>,
    pub fee: u64,
    /// Required finality: the stronger of the offer and the policy.
    pub finality: Finality,
    /// Token payments: custody class of the token.
    pub custody: Option<Custody>,
    /// Authorization expiry (unix ms) that was valid at verification time.
    pub authorization_expires_at_ms: u64,
    pub request_hash: [u8; 32],
    pub requirements_hash: [u8; 32],
    /// The `payment-identifier` extension id of the payload (validated; `None` when the policy does not require it and none was sent).
    pub payment_identifier: Option<String>,
    /// Fields for `SettlementResponse.extensions.kaspa` (binding, profile, paymentOutputIndex,
    /// finality, transactionEncoding, and profile specifics).
    pub response_extension: Map<String, Value>,
}

/// Everything verification reads besides the request.
pub struct VerifyCtx<'a> {
    pub chain: &'a dyn ChainView,
    pub clock: &'a dyn Clock,
    pub policy: &'a Policy,
}

/// Verifies `payload` against the server-offered requirements `offered`.
///
/// `request_hash` is the resource server's independently computed 32-byte fingerprint; it must
/// equal the payload's and the one the authorization commits to.
pub fn verify_payment(
    ctx: &VerifyCtx,
    offered: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
) -> Result<Verified> {
    if payload.x402_version != crate::wire::X402_VERSION {
        return Err(X402Error::new(Reason::InvalidX402Version, Diag::InvalidKaspaX402Payload, "unsupported x402 version"));
    }
    let intent = payload.payload.route.as_ref().is_some_and(|r| r.binding == crate::wire::BINDING_INTENT) || offered.is_intent();
    if intent {
        return crate::intent::verify_intent(ctx, offered, payload, request_hash).map(|(v, _)| v);
    }
    let routed = payload.payload.route.is_some() || offered.has_route();
    if routed {
        return crate::swap::verify_swap(ctx, offered, payload, request_hash);
    }
    match Profile::parse(&payload.payload.profile) {
        Some(Profile::StandardNative) => crate::exact::verify_native(ctx, offered, payload, request_hash),
        Some(Profile::Kcc20) => crate::token::verify_kcc20(ctx, offered, payload, request_hash),
        Some(Profile::Additive) | None => Err(X402Error::new(
            Reason::UnsupportedScheme,
            Diag::UnsupportedKaspaExactProfile,
            format!("exact profile {:?} is not supported", payload.payload.profile),
        )),
    }
}
