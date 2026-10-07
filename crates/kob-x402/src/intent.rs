//! Intent-based swap-and-pay (`extra.route.binding = "kob-intent-v1"`, proposal
//! `docs/spec/x402-swap-and-pay.md`, "Intent mode").
//!
//! The payer signs ONE transaction, the **creation**: it locks the payer's funds in a KOB router intent
//! (`kob_protocol::router`, `contracts/argent/kob_router.ag`) whose state binds the merchant and the
//! payer's worst case. The facilitator verifies the creation against the offer, broadcasts it and then
//! executes the intent itself, as a keeper, against the book of the moment; when an order it named is
//! taken first it plans again and re-executes, without the payer (the intent needs no payer signature at
//! execution). Settlement is the execution transaction accepted. The payer can always cancel an intent
//! that was not executed (`cancel`, SIGHASH_ALL), and SHOULD once the authorization expired.
//!
//! | Pay asset | Merchant receives | Intent |
//! |---|---|---|
//! | KAS | token B (`kcc20`) | `KasToToken_<shape>` |
//! | token A | KAS (`standard-native`) | `TokenToKas_<shape>` |
//! | token A | token B (`kcc20`) | `TokenSwap_<shape>` |
//! | KRON token A | KAS / token B | `TokenToKasKron_<shape>` / `TokenSwapKron_<shape>` |
//!
//! The router reads every token under the program the intent's state names (open ICC handles), so an intent
//! trades any allowlisted token on a program the router accepts (`kob_protocol::router::intent_program`: the
//! KCC-20 programs of the 112-byte state and the KRON programs; KaspaCom's program is pending review and paid
//! through the payer-signed route `kob-swap-v1`). The verifier recomputes the programs from the allowlist, like
//! every other term. A KRON token is a pay asset only (the merchant receives KAS or a KCC-20 token), its intent
//! locks more than it may sell (a KRON output holds at least one unit), and a creation that spends key-held KRON
//! tokens carries a P2PK input of their owner (KRON address presence).
//!
//! **Binding.** The payload commitment (`kob-x402-payload-commitment-v1`) is embedded in the creation
//! transaction's payload, with `route = { binding: "kob-intent-v1", payAsset, actor }` and
//! `paymentOutputIndex` = the intent output. Every payer signature (SIGHASH_ALL) covers it, so the intent
//! cannot be presented for another offer, request or expiry; the verifier recomputes the intent's script
//! from the OFFER (merchant key, merchant asset, amount) and the payer's terms, so an intent for another
//! merchant or amount does not match. The intent outpoint, like every input of the creation, is consumed
//! once in the replay ledger.
//!
//! This module verifies a creation ([`verify_intent`]) and plans and builds executions
//! ([`plan_executions`], [`build_execution`]); it does no I/O. The facilitator's settlement loop lives in
//! `kob-executor` (`x402::facilitator`).

use std::collections::BTreeSet;

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kob_protocol::build::{execute_intent, ExecuteIntent, ExecutionFacts, IntentAsk, IntentBid};
use kob_protocol::family::Family;
use kob_protocol::router::{intent_program, Actor, IntentKind, IntentState, MIN_INTENT_VALUE, ROUTER_ARTIFACT_ID};
use kob_protocol::script::{p2pk_spk, p2sh_spk};
use kob_protocol::state::{quote_of, AskState, BidState, Round, TokenState, TIF_FOK, TIF_GTC, TIF_IOC};
use kob_protocol::tx::{FeeOptions, OrderUtxo, TokenUtxo, Utxo};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::canonical::canonical_hash;
use crate::chain::{Outpoint, Txid};
use crate::common::{
    address_of, check_economics_and_scripts, check_envelope, check_expiry, check_payload_commitment, effective_finality, is_p2pk,
    outpoint_of, p2pk_key, parse_tx, payload_commit_object, payment_identifier, resolve_entries, PayloadCommit,
};
use crate::error::{Diag, Reason, Result, X402Error};
use crate::policy::{AllowedToken, Custody};
use crate::safe_tx::spk_to_hex;
use crate::swap::{merchant_token, redeem_of, MerchantToken, PayAssetOffer, MAX_PAY_ASSETS, PAY_ASSET_KAS};
use crate::verify::{PaymentKind, Verified, VerifyCtx, WatchedOutput};
use crate::wire::{
    hex, parse_hash32, parse_u64_canonical, IntentTerms, PaymentPayload, PaymentRequirements, Profile, ASSET_KAS,
    AUTH_VERSION_PAYLOAD, BINDING_INTENT, TX_ENCODING,
};
use kob_protocol::artifacts::TemplateId;

fn req(msg: impl Into<String>) -> X402Error {
    X402Error::requirements(Diag::InvalidKaspaX402Binding, msg)
}

fn unsupported(msg: impl Into<String>) -> X402Error {
    X402Error::payload(Diag::RouteUnsupported, msg)
}

fn lower_hex32(s: &str) -> Option<[u8; 32]> {
    (s == s.to_ascii_lowercase()).then(|| parse_hash32(s)).flatten()
}

// ------------------------------------------------------------------------------------------ the offer

/// Parses and validates `offered.extra.route` of an intent offer: binding, critical flag, the router
/// artifact the facilitator executes (it must be the one this build pins) and the pay assets.
pub fn parse_intent_offer(offered: &PaymentRequirements) -> Result<Vec<PayAssetOffer>> {
    let route = offered.extra.get("route").and_then(Value::as_object).ok_or_else(|| req("extra.route must be an object"))?;
    if route.get("binding").and_then(Value::as_str) != Some(BINDING_INTENT) {
        return Err(req("extra.route.binding must be kob-intent-v1"));
    }
    if route.get("critical") != Some(&Value::Bool(true)) {
        return Err(req("extra.route.critical must be true"));
    }
    if route.get("router").and_then(Value::as_str) != Some(ROUTER_ARTIFACT_ID) {
        return Err(X402Error::requirements(
            Diag::RouteUnsupported,
            "extra.route.router names another router artifact than the one this facilitator executes",
        ));
    }
    let list = route.get("payAssets").and_then(Value::as_array).ok_or_else(|| req("extra.route.payAssets must be a list"))?;
    if list.is_empty() || list.len() > MAX_PAY_ASSETS {
        return Err(req(format!("extra.route.payAssets must list 1..={MAX_PAY_ASSETS} assets")));
    }
    let mut out = Vec::with_capacity(list.len());
    for e in list {
        let o = e.as_object().ok_or_else(|| req("a payAssets entry must be an object"))?;
        let asset = o.get("asset").and_then(Value::as_str).ok_or_else(|| req("payAssets entry without asset"))?;
        if asset == PAY_ASSET_KAS {
            out.push(PayAssetOffer { asset: asset.to_string(), template_hash: None, extension_commitment: None });
            continue;
        }
        lower_hex32(asset).ok_or_else(|| req("payAssets asset must be KAS or a 64-hex covenant id"))?;
        let th = o.get("templateHash").and_then(Value::as_str).and_then(lower_hex32);
        let ec = o.get("extensionCommitment").and_then(Value::as_str).and_then(lower_hex32);
        if th.is_none() || ec.is_none() {
            return Err(req("a token payAssets entry needs templateHash and extensionCommitment (64 lowercase hex)"));
        }
        out.push(PayAssetOffer { asset: asset.to_string(), template_hash: th, extension_commitment: ec });
    }
    // one entry per asset: a second one with other pins is never the one that applies (the first match is)
    let mut seen = std::collections::HashSet::new();
    if !out.iter().all(|e| seen.insert(e.asset.as_str())) {
        return Err(req("extra.route.payAssets lists an asset twice"));
    }
    Ok(out)
}

/// A token the router can sell for the payer: on a program an intent may name (KCC-20 of the 112-byte state or
/// KRON; not KaspaCom's program while it is pending review).
pub fn router_capable(t: &AllowedToken) -> bool {
    intent_program(t.program).is_ok()
}

/// A token the router can deliver to a merchant: [`router_capable`] and KCC-20 (a KRON token is paid with, never
/// received).
pub fn router_deliverable(t: &AllowedToken) -> bool {
    router_capable(t) && t.program.family() == Family::Kcc20
}

// ------------------------------------------------------------------------------------------ commitment

/// The commitment object of an intent creation: the payload commitment of the companion proposal with
/// `route = { binding: "kob-intent-v1", payAsset, actor }`.
pub fn intent_commit_object(c: &PayloadCommit, actor: &str) -> Value {
    let mut v = payload_commit_object(c);
    v["route"] = json!({
        "binding": BINDING_INTENT,
        "payAsset": match c.route_pay_asset.unwrap_or(ASSET_KAS) {
            ASSET_KAS => ASSET_KAS.to_string(),
            a => a.to_ascii_lowercase(),
        },
        "actor": actor,
    });
    v
}

/// SHA-256 digest of [`intent_commit_object`].
pub fn intent_commit_digest(c: &PayloadCommit, actor: &str) -> Result<[u8; 32]> {
    canonical_hash(&intent_commit_object(c, actor))
}

// ------------------------------------------------------------------------------------------ facts

/// What the facilitator needs to execute a verified intent (serialized into its ledger).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentFacts {
    pub actor: String,
    pub state: IntentState,
    /// `route.payAsset` (`"KAS"` or the token covenant id).
    pub pay_asset: String,
    /// The intent UTXO the creation makes (covenant id = the intent id).
    pub intent: Utxo,
    /// Token intents: the locked token UTXO.
    #[serde(default)]
    pub lock: Option<TokenUtxo>,
    /// Version-prefixed script public key hex of the intent output.
    pub intent_spk: String,
    /// Script public key hex of the merchant's output in an execution.
    pub merchant_spk: String,
    /// KAS value of the merchant's output: the carrier (token) or the amount (KAS).
    #[serde(with = "kob_protocol::json::field")]
    pub merchant_value: u64,
    /// Authorization expiry (unix ms): the facilitator does not execute after it.
    #[serde(with = "kob_protocol::json::field")]
    pub deadline_ms: u64,
}

impl IntentFacts {
    pub fn actor(&self) -> Result<&'static Actor> {
        Actor::by_name(&self.actor).ok_or_else(|| unsupported(format!("{} is not a router actor", self.actor)))
    }
    /// The intent outpoint.
    pub fn outpoint(&self) -> Outpoint {
        Outpoint::new(self.intent.transaction_id, self.intent.index)
    }
    /// The merchant receives a token (KasToToken, TokenSwap).
    pub fn merchant_gets_token(&self) -> bool {
        self.state.kind().merchant_gets_token()
    }
}

fn amount_field(v: &Option<String>, what: &str) -> Result<i64> {
    let s =
        v.as_deref().ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, format!("route.intent.{what} is required")))?;
    parse_u64_canonical(s).filter(|x| *x > 0 && *x <= i64::MAX as u64).map(|x| x as i64).ok_or_else(|| {
        X402Error::payload(Diag::InvalidKaspaX402Payload, format!("route.intent.{what} must be a canonical positive amount"))
    })
}

/// The lock's units the payer's terms name (`lockAmount`, default `maxSell`): the state's lock pin.
fn lock_amount(terms: &IntentTerms, max_sell: i64) -> Result<i64> {
    match &terms.lock_amount {
        Some(_) => amount_field(&terms.lock_amount, "lockAmount"),
        None => Ok(max_sell),
    }
}

/// The intent state the offer and the payer's terms call for.
fn expected_state(
    kind: IntentKind,
    terms: &IntentTerms,
    merchant_key: [u8; 32],
    amount: u64,
    pay_token: Option<&AllowedToken>,
    merchant: Option<&MerchantToken>,
    deadline: i64,
) -> Result<IntentState> {
    let payer = lower_hex32(&terms.payer)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, "route.intent.payer must be a 64-hex x-only key"))?;
    let amount = i64::try_from(amount).map_err(|_| req("amount exceeds the token range"))?;
    let none = |v: &Option<String>, what: &str| -> Result<()> {
        if v.is_some() {
            return Err(X402Error::payload(
                Diag::InvalidKaspaX402Payload,
                format!("route.intent.{what} does not apply to this intent"),
            ));
        }
        Ok(())
    };
    let s = match kind {
        IntentKind::KasToToken => {
            none(&terms.max_sell, "maxSell")?;
            IntentState::KasToToken {
                payer,
                merchant: merchant_key,
                token: merchant.expect("token gain").id,
                program: merchant.expect("token gain").allowed.program,
                amount,
                max_pay: amount_field(&terms.max_pay, "maxPay")?,
                max_extra: amount_field(&terms.max_extra, "maxExtra")?,
                b_extension: merchant.expect("token gain").allowed.extension_commitment,
                deadline,
            }
        }
        IntentKind::TokenToKas => {
            none(&terms.max_pay, "maxPay")?;
            none(&terms.max_extra, "maxExtra")?;
            let max_sell = amount_field(&terms.max_sell, "maxSell")?;
            IntentState::TokenToKas {
                payer,
                merchant: merchant_key,
                token: pay_token.expect("token pay asset").covenant_id,
                program: pay_token.expect("token pay asset").program,
                merchant_kas: amount,
                max_sell,
                lock_amount: lock_amount(terms, max_sell)?,
                lock_extension: pay_token.expect("token pay asset").extension_commitment,
                deadline,
            }
        }
        IntentKind::TokenSwap => {
            none(&terms.max_pay, "maxPay")?;
            none(&terms.max_extra, "maxExtra")?;
            let max_sell_a = amount_field(&terms.max_sell, "maxSell")?;
            IntentState::TokenSwap {
                payer,
                merchant: merchant_key,
                token_a: pay_token.expect("token pay asset").covenant_id,
                program_a: pay_token.expect("token pay asset").program,
                token_b: merchant.expect("token gain").id,
                program_b: merchant.expect("token gain").allowed.program,
                max_sell_a,
                amount_b: amount,
                lock_amount: lock_amount(terms, max_sell_a)?,
                lock_extension: pay_token.expect("token pay asset").extension_commitment,
                b_extension: merchant.expect("token gain").allowed.extension_commitment,
                deadline,
            }
        }
    };
    s.check().map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, format!("route.intent: {e}")))?;
    Ok(s)
}

// ------------------------------------------------------------------------------------------ verification

/// Verifies an intent creation (see the module docs). Returns the facilitator's [`Verified`] (the payment
/// transaction is the creation; its watched output is the intent) and the [`IntentFacts`] to execute it.
///
/// Checks, fail closed, everything recomputed from the offer and the trusted chain view:
///
/// 1. the envelope (accepted == offered, network, amount, `payTo`, request hash) and the intent offer
///    (binding, critical, pinned router, pay assets);
/// 2. the pay asset (`KAS` only when the merchant receives a token; a token must be allowlisted, on a program
///    the router accepts, issuer-controlled only when the policy allows it) and the intent kind it implies,
///    the actor (a router actor of that kind and of the pay token's family, whose shape the programs can run);
/// 3. the intent state recomputed from the OFFER (merchant key from `payTo`, merchant asset, exact
///    amount) and the payer's terms, and its script;
/// 4. the creation transaction: bounded parse, trusted UTXOs, every input a payer P2PK input or a payer
///    key-owned token of the pay asset (borrow disabled / not a minter, pinned program and extension; a KRON
///    token with a P2PK input of its owner), engine validation, fee and mass;
/// 5. output `paymentOutputIndex` is the intent: script = the recomputed actor instance, a single-output
///    genesis authorised by a P2PK input; for a token intent the locked token output is owned by the
///    intent id and holds at least `maxSell`; no other output carries the intent's covenant id;
/// 6. the commitment digest (route = `{ kob-intent-v1, payAsset, actor }`) in the creation's payload,
///    the expiry and the payment identifier.
pub fn verify_intent(
    ctx: &VerifyCtx,
    offered: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
) -> Result<(Verified, IntentFacts)> {
    let pol = ctx.policy;
    if !pol.swap_enabled {
        return Err(X402Error::new(Reason::UnsupportedScheme, Diag::RouteUnsupported, "swap-and-pay is not enabled on this verifier"));
    }
    // 1. envelope and offer
    let env = check_envelope(ctx, offered, payload, request_hash)?;
    let pay_offers = parse_intent_offer(offered)?;
    let route = payload.payload.route.as_ref().ok_or_else(|| {
        X402Error::payload(Diag::InvalidKaspaX402Payload, "the offer carries extra.route but the payload has no route")
    })?;
    if route.binding != BINDING_INTENT {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "payload.route.binding must be kob-intent-v1"));
    }
    if !route.orders.is_empty() {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "an intent names no orders (the keeper chooses them)"));
    }
    let terms =
        route.intent.as_ref().ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, "payload.route.intent is required"))?;
    let merchant_key = p2pk_key(&env.pay_to_spk).ok_or_else(|| {
        X402Error::requirements(Diag::TokenOwnerScheme, "payTo must be a Schnorr P2PK address for an intent payment")
    })?;
    let merchant: Option<MerchantToken> = match env.profile {
        Profile::StandardNative => {
            if offered.asset != ASSET_KAS || offered.extra.contains_key("token") {
                return Err(req("a standard-native intent offer pays KAS: asset must be KAS and extra.token absent"));
            }
            if pol.limits.min_amount_sompi > 0 && env.amount < pol.limits.min_amount_sompi {
                return Err(X402Error::requirements(Diag::InvalidKaspaX402Amount, "amount is below the policy minimum"));
            }
            None
        }
        Profile::Kcc20 => {
            let m = merchant_token(pol, offered, &env)?;
            if !router_deliverable(&m.allowed) {
                return Err(X402Error::requirements(
                    Diag::RouteUnsupported,
                    format!("the router does not deliver {} tokens ({} program)", m.allowed.ticker, m.allowed.program.name()),
                ));
            }
            Some(m)
        }
        Profile::Additive => {
            return Err(X402Error::new(Reason::UnsupportedScheme, Diag::UnsupportedKaspaExactProfile, "additive is not supported"))
        }
    };
    // 2. pay asset and kind
    let offer_entry = pay_offers
        .iter()
        .find(|e| e.asset == route.pay_asset)
        .ok_or_else(|| X402Error::payload(Diag::PayAssetNotAccepted, "route.payAsset is not listed in the offer's payAssets"))?;
    let (kind, pay_token): (IntentKind, Option<AllowedToken>) = if route.pay_asset == PAY_ASSET_KAS {
        if merchant.is_none() {
            return Err(X402Error::payload(Diag::PayAssetNotAccepted, "payAsset equals the merchant asset (KAS)"));
        }
        (IntentKind::KasToToken, None)
    } else {
        let id = lower_hex32(&route.pay_asset)
            .ok_or_else(|| X402Error::payload(Diag::PayAssetNotAccepted, "route.payAsset is not a covenant id"))?;
        if merchant.as_ref().is_some_and(|m| m.id == id) {
            return Err(X402Error::payload(Diag::PayAssetNotAccepted, "payAsset equals the merchant asset"));
        }
        let allowed = pol
            .tokens
            .find(&id)
            .ok_or_else(|| X402Error::payload(Diag::PayAssetNotAccepted, "route.payAsset is not an accepted token"))?;
        if offer_entry.template_hash != Some(allowed.template_hash())
            || offer_entry.extension_commitment != Some(allowed.extension_commitment)
        {
            return Err(X402Error::payload(
                Diag::PayAssetNotAccepted,
                "the offered payAsset template / extension differs from the allowlist entry",
            ));
        }
        if allowed.custody == Custody::IssuerControlled && !pol.allow_issuer_controlled {
            return Err(X402Error::payload(Diag::TokenCustodyPolicy, "issuer-controlled tokens are not accepted by this verifier"));
        }
        if !router_capable(allowed) {
            return Err(unsupported(format!(
                "the router does not sell tokens of the {} program; pay {} through the payer-signed route (kob-swap-v1)",
                allowed.program.name(),
                allowed.ticker
            )));
        }
        (if merchant.is_some() { IntentKind::TokenSwap } else { IntentKind::TokenToKas }, Some(allowed.clone()))
    };
    let actor = Actor::by_name(&terms.actor).ok_or_else(|| unsupported(format!("{} is not a router actor", terms.actor)))?;
    if actor.shape.kind != kind {
        return Err(unsupported(format!("{} is not a {} intent", actor.name, kind.as_str())));
    }
    let auth = &payload.payload.authorization;
    if auth.version != AUTH_VERSION_PAYLOAD || auth.signature.is_some() || auth.input_index.is_some() {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "an intent is authorized by the payload commitment only"));
    }
    // 3. the intent the offer calls for
    // the intent's deadline is the authorization's expiry (unix ms): from it on anyone may expire the intent
    let deadline = crate::common::parse_iso_ms(&auth.expires_at).and_then(|ms| i64::try_from(ms).ok()).ok_or_else(|| {
        X402Error::payload(Diag::InvalidAuthorization, "authorization.expiresAt is not a millisecond ISO-8601 UTC timestamp")
    })?;
    let state = expected_state(kind, terms, merchant_key, env.amount, pay_token.as_ref(), merchant.as_ref(), deadline)?;
    // the actor's family (token A) and the programs' slots: a shape the programs cannot run is refused
    state.check_actor(actor).map_err(|e| unsupported(e.to_string()))?;
    let intent_spk = state.spk(actor).map_err(|e| unsupported(e.to_string()))?;
    let pi = payload.payload.payment_output_index;
    let spk_hex = spk_to_hex(&env.pay_to_spk);
    let digest = intent_commit_digest(
        &PayloadCommit {
            network: env.network,
            profile: env.profile,
            route_pay_asset: Some(&route.pay_asset),
            asset: &offered.asset,
            amount: &offered.amount,
            pay_to: &offered.pay_to,
            pay_to_spk_hex: &spk_hex,
            payment_output_index: pi,
            requirements_hash: &env.requirements_hash,
            request_hash: &env.request_hash,
            expires_at: &auth.expires_at,
        },
        actor.name,
    )?;
    if parse_hash32(&auth.digest) != Some(digest) {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "authorization.digest differs from the recomputed commitment"));
    }
    let expires_ms = check_expiry(ctx.clock.now_ms(), env.max_timeout_seconds, &auth.expires_at)?;
    let payment_identifier = payment_identifier(ctx, payload)?;

    // 4. the creation transaction
    let parsed = parse_tx(ctx, payload)?;
    let tx = &parsed.tx;
    if tx.version != 1 || tx.gas != 0 {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "an intent creation is version 1 with gas 0"));
    }
    check_payload_commitment(tx, &digest)?;
    let entries = resolve_entries(ctx, &parsed)?;
    let mut payer_keys: Vec<[u8; 32]> = vec![];
    // payer token inputs: (input, owner key, KCC-20 owner scheme; None for KRON, which signs nothing itself)
    let mut token_inputs: Vec<(usize, [u8; 32], Option<u8>)> = vec![];
    for (i, e) in entries.iter().enumerate() {
        if is_p2pk(&e.script_public_key) && e.covenant_id.is_none() {
            payer_keys.push(p2pk_key(&e.script_public_key).expect("p2pk"));
            continue;
        }
        let Some(pt) = pay_token.as_ref() else {
            return Err(X402Error::payload(
                Diag::InvalidKaspaExactUtxo,
                format!("input {i}: a KasToToken creation spends P2PK inputs only"),
            ));
        };
        if e.covenant_id.map(|h| h.as_bytes()) != Some(pt.covenant_id) {
            return Err(X402Error::payload(Diag::PayAssetNotAccepted, format!("input {i} is neither a P2PK input nor the pay token")));
        }
        let redeem = redeem_of(&tx.inputs[i].signature_script)
            .ok_or_else(|| X402Error::payload(Diag::TokenTemplateMismatch, format!("input {i}: no redeem script")))?;
        if p2sh_spk(redeem) != e.script_public_key {
            return Err(X402Error::payload(
                Diag::TokenTemplateMismatch,
                format!("input {i}: redeem script does not hash to the chain script"),
            ));
        }
        let tpl = kob_protocol::artifacts::token_template(pt.program);
        let st = TokenState::from_redeem_with(tpl, redeem)
            .map_err(|_| X402Error::payload(Diag::TokenTemplateMismatch, format!("input {i}: not the pay token's pinned program")))?;
        match &st {
            TokenState::Kcc20(k) => {
                if k.borrow_scheme != 0 || k.borrow_guard != [0; 32] {
                    return Err(X402Error::payload(
                        Diag::TokenBorrowEnabled,
                        format!("input {i}: the payer token has borrowing enabled"),
                    ));
                }
                if k.owner_scheme != kob_protocol::state::SCHEME_P2PK {
                    return Err(X402Error::payload(Diag::TokenOwnerScheme, format!("input {i}: the payer token is not key-owned")));
                }
                token_inputs.push((i, k.owner, Some(k.owner_scheme)));
            }
            TokenState::Kron(k) => {
                if k.is_minter != 0 {
                    return Err(X402Error::payload(
                        Diag::TokenBorrowEnabled,
                        format!("input {i}: the payer token is a KRON minter UTXO"),
                    ));
                }
                if k.id_type != kob_protocol::family::KRON_TYPE_ADDR {
                    return Err(X402Error::payload(
                        Diag::TokenOwnerScheme,
                        format!("input {i}: the payer KRON token is not held by address presence (id_type 3)"),
                    ));
                }
                token_inputs.push((i, k.owner, None));
            }
        }
        if st.extension() != pt.extension_commitment {
            return Err(X402Error::payload(Diag::TokenNotAllowlisted, format!("input {i}: another extension commitment")));
        }
        payer_keys.push(st.owner());
    }
    // KRON address presence: a key-held KRON token is authorised by a P2PK input of its owner (it signs nothing itself)
    let p2pk_keys: Vec<[u8; 32]> = entries
        .iter()
        .filter(|e| is_p2pk(&e.script_public_key) && e.covenant_id.is_none())
        .filter_map(|e| p2pk_key(&e.script_public_key))
        .collect();
    for (i, key, scheme) in &token_inputs {
        if scheme.is_none() && !p2pk_keys.contains(key) {
            return Err(X402Error::payload(
                Diag::TokenOwnerScheme,
                format!("input {i}: a KRON token held by a key needs a P2PK input of that key in the creation"),
            ));
        }
    }

    // every payer signature is SIGHASH_ALL (P2PK inputs and the owner witnesses of the pay-token inputs), checked on
    // the signature scripts before the engine, which accepts the other hash types
    crate::sighash::check_payer_inputs(
        tx,
        &entries,
        entries.iter().enumerate().filter(|(_, e)| is_p2pk(&e.script_public_key) && e.covenant_id.is_none()).map(|(i, _)| i),
        token_inputs.iter().filter_map(|(i, _, scheme)| scheme.map(|s| (*i, s))),
    )?;

    // 5. the intent output and the locked tokens
    let outs = &tx.outputs;
    let io = outs
        .get(pi as usize)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "paymentOutputIndex is outside the creation"))?;
    if io.script_public_key != intent_spk {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactPaymentOutput,
            "the intent output is not the router intent the offer calls for (merchant, asset, amount, terms or actor differ)",
        ));
    }
    let binding =
        io.covenant.ok_or_else(|| X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "the intent output has no covenant"))?;
    let a = binding.authorizing_input as usize;
    if a >= tx.inputs.len() || !is_p2pk(&entries[a].script_public_key) || entries[a].covenant_id.is_some() {
        return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "the intent genesis is not authorised by a P2PK input"));
    }
    let expect_id = covenant_id(tx.inputs[a].previous_outpoint, std::iter::once((pi, io)));
    if binding.covenant_id != expect_id {
        return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "the intent output is not a single-output genesis"));
    }
    let intent_id = expect_id.as_bytes();
    if outs.iter().enumerate().any(|(k, o)| k != pi as usize && o.covenant.is_some_and(|c| c.covenant_id == expect_id)) {
        return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "another output carries the intent's covenant id"));
    }
    let creation_txid = tx.id().as_bytes();
    let intent = Utxo { transaction_id: creation_txid, index: pi, amount: io.value, block_daa_score: 0, covenant_id: Some(intent_id) };
    if io.value < MIN_INTENT_VALUE {
        return Err(X402Error::payload(
            Diag::InvalidKaspaX402Amount,
            format!("the intent holds less than {MIN_INTENT_VALUE} sompi: its cancel and its expiry could not pay their fee"),
        ));
    }
    let lock = match &pay_token {
        None => {
            if terms.lock_output_index.is_some() || terms.lock_amount.is_some() {
                return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "a KasToToken intent locks no tokens"));
            }
            None
        }
        Some(pt) => {
            let li = terms
                .lock_output_index
                .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, "route.intent.lockOutputIndex is required"))?;
            let lo = outs.get(li as usize).filter(|_| li != pi).ok_or_else(|| {
                X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "lockOutputIndex is not an output of the creation")
            })?;
            if lo.covenant.map(|c| c.covenant_id.as_bytes()) != Some(pt.covenant_id) {
                return Err(X402Error::payload(
                    Diag::InvalidKaspaExactPaymentOutput,
                    "the locked output is not a token output of the pay asset",
                ));
            }
            let max_sell = state.max_sell().expect("token intent");
            let amount = lock_amount(terms, max_sell)?;
            let lock_state = check_lock(lo, pt, intent_id, amount, max_sell)?;
            Some(TokenUtxo {
                utxo: Utxo {
                    transaction_id: creation_txid,
                    index: li,
                    amount: lo.value,
                    block_daa_score: 0,
                    covenant_id: Some(pt.covenant_id),
                },
                state: lock_state,
            })
        }
    };
    let fee = check_economics_and_scripts(ctx, tx, &entries)?;

    // 6. results
    let (merchant_spk, merchant_value) = match &merchant {
        Some(m) => (m.spk.clone(), m.carrier),
        None => (env.pay_to_spk.clone(), env.amount),
    };
    let mut consumed: Vec<Outpoint> = tx.inputs.iter().map(outpoint_of).collect();
    consumed.push(Outpoint::new(creation_txid, pi));
    if let Some(l) = &lock {
        consumed.push(Outpoint::new(creation_txid, l.utxo.index));
    }
    let mut ext = Map::new();
    ext.insert("binding".into(), json!(crate::wire::BINDING_EXACT));
    ext.insert("profile".into(), json!(env.profile.as_str()));
    ext.insert("finality".into(), json!(effective_finality(env.finality, pol).as_str()));
    ext.insert("transactionEncoding".into(), json!(TX_ENCODING));
    if let Some(m) = &merchant {
        ext.insert("custody".into(), json!(m.allowed.custody.as_str()));
    }
    ext.insert(
        "route".into(),
        json!({
            "binding": BINDING_INTENT,
            "payAsset": route.pay_asset,
            "actor": actor.name,
            "intent": Outpoint::new(creation_txid, pi).to_json(),
            "creation": hex(&creation_txid),
        }),
    );
    let payer = payer_keys.first().copied().expect("a P2PK input exists");
    let verified = Verified {
        kind: if merchant.is_some() { PaymentKind::IntentToToken } else { PaymentKind::IntentToKas },
        profile: env.profile,
        txid: creation_txid,
        tx: tx.clone(),
        entries,
        payer_address: address_of(&p2pk_spk(&payer), env.network),
        amount: env.amount,
        payment_output_index: pi,
        merchant_output: WatchedOutput {
            outpoint: Outpoint::new(creation_txid, pi),
            script_public_key: intent_spk.clone(),
            amount: io.value,
        },
        consumed,
        order_inputs: vec![],
        fee,
        finality: effective_finality(env.finality, pol),
        custody: merchant.as_ref().map(|m| m.allowed.custody),
        authorization_expires_at_ms: expires_ms,
        request_hash: env.request_hash,
        requirements_hash: env.requirements_hash,
        payment_identifier,
        response_extension: ext,
    };
    let facts = IntentFacts {
        actor: actor.name.to_string(),
        state,
        pay_asset: route.pay_asset.clone(),
        intent,
        lock,
        intent_spk: spk_to_hex(&intent_spk),
        merchant_spk: spk_to_hex(&merchant_spk),
        merchant_value,
        deadline_ms: expires_ms,
    };
    Ok((verified, facts))
}

/// Checks the locked token output: exactly `amount` units (the payer's `lockAmount`, at least `maxSell`; more
/// than `maxSell` for KRON, whose outputs hold at least one unit) owned by the intent's covenant id (KCC-20 owner
/// scheme 0x04 with borrow disabled and the pinned extension, KRON id_type 2, not a minter), on the pay token's
/// program. A P2SH output hides its state, so the payer states the amount and the verifier recomputes the script.
/// Returns the lock's state.
fn check_lock(
    lo: &kaspa_consensus_core::tx::TransactionOutput,
    pt: &AllowedToken,
    intent_id: [u8; 32],
    amount: i64,
    max_sell: i64,
) -> Result<TokenState> {
    if amount < max_sell {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "route.intent.lockAmount is below maxSell"));
    }
    let fam = pt.program.family();
    if fam == Family::Kron && amount <= max_sell {
        return Err(X402Error::payload(
            Diag::InvalidKaspaX402Payload,
            "route.intent.lockAmount of a KRON intent must exceed maxSell (the payer's change holds at least one unit)",
        ));
    }
    let st = TokenState::custody(fam, amount, intent_id, pt.extension_commitment);
    if lo.script_public_key != st.spk_with(kob_protocol::artifacts::token_template(pt.program)) {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactPaymentOutput,
            "the locked token output must hold lockAmount units owned by the intent (borrow disabled / not a minter, pinned program and extension)",
        ));
    }
    Ok(st)
}

// ------------------------------------------------------------------------------------------ execution

/// The book an execution is planned against: plain resting orders of the intent's tokens.
#[derive(Clone, Debug, Default)]
pub struct BookView {
    /// Asks with their exact custody.
    pub asks: Vec<(OrderUtxo<AskState>, TokenUtxo)>,
    pub bids: Vec<OrderUtxo<BidState>>,
}

/// The keeper's parameters.
#[derive(Clone, Debug)]
pub struct KeeperParams {
    /// Receives the residue (token intents) and the fillers of token intents.
    pub keeper: [u8; 32],
    /// Value of a filler output.
    pub filler: u64,
    pub fee: FeeOptions,
    /// Orders considered per side (best price first).
    pub max_candidates: usize,
    /// Executions tried per planning pass.
    pub max_builds: usize,
}

impl Default for KeeperParams {
    fn default() -> Self {
        KeeperParams { keeper: [0; 32], filler: 20_000_000, fee: FeeOptions::default(), max_candidates: 8, max_builds: 24 }
    }
}

/// A built execution.
#[derive(Clone, Debug)]
pub struct Execution {
    pub tx: Transaction,
    pub entries: Vec<UtxoEntry>,
    pub facts: ExecutionFacts,
    /// The order outpoints it spends.
    pub orders: Vec<Outpoint>,
    pub merchant_output: Outpoint,
    pub merchant_spk: ScriptPublicKey,
}

fn ask_ok(o: &OrderUtxo<AskState>, c: &TokenUtxo, token: &[u8; 32], program: TemplateId, lock: u64) -> bool {
    let s = &o.state;
    let p = kob_protocol::artifacts::token_template(program);
    s.token_cov_id == *token
        && s.token_tpl_hash == p.hash
        && s.tpl_prefix_len == p.prefix.len() as i64
        && s.tpl_suffix_len == p.suffix.len() as i64
        && s.slope == 0
        && s.interval == 0
        && s.max_fill == 0
        && s.amount_left > 0
        && (s.active_from as u64) <= lock
        && (s.expiry_daa <= 0 || s.expiry_daa as u64 > lock)
        && c.state.amount() == s.custody_amount()
        && c.state.owner() == o.utxo.covenant_id.unwrap_or_default()
        && c.state.is_covenant_owned()
        && c.state.is_plain()
}

fn bid_ok(o: &OrderUtxo<BidState>, token: &[u8; 32], program: TemplateId, lock: u64) -> bool {
    let s = &o.state;
    let p = kob_protocol::artifacts::token_template(program);
    s.token_cov_id == *token
        && s.token_tpl_hash == p.hash
        && s.tpl_prefix_len == p.prefix.len() as i64
        && s.tpl_suffix_len == p.suffix.len() as i64
        && s.slope == 0
        && s.interval == 0
        && s.max_fill == 0
        && (s.active_from as u64) <= lock
        && (s.expiry_daa <= 0 || s.expiry_daa as u64 > lock)
        && s.min_fill > 0
        && s.budget_rate().is_some_and(|r| r > 0)
}

/// Base units a bid can take at most (its buying power: the escrow less the delivery carrier and reserve, at the budget
/// rate).
fn bid_affordable(o: &OrderUtxo<BidState>) -> i64 {
    o.state.buying_power(o.utxo.amount as i64)
}

/// What the router lets the asks demand for `n` base units: the ask's quote, rounded up (`ask_leg` of the router).
fn ask_quote(s: &AskState, n: i64) -> Option<i64> {
    quote_of(n, s.price, s.scale, Round::Up)
}

/// Ask sweeps of `shape` that deliver exactly `amount` units, cheapest quote first.
fn ask_sweeps(
    asks: &[&(OrderUtxo<AskState>, TokenUtxo)],
    k: usize,
    last_rests: bool,
    amount: i64,
    max_pay: Option<i64>,
) -> Vec<Vec<IntentAsk>> {
    let mut out: Vec<(i64, Vec<IntentAsk>)> = vec![];
    let n = asks.len();
    // choose the k-1 sold-out asks as an ordered subset (in price order), then the last ask
    let mut idx: Vec<usize> = Vec::with_capacity(k);
    fn rec(
        asks: &[&(OrderUtxo<AskState>, TokenUtxo)],
        k: usize,
        start: usize,
        idx: &mut Vec<usize>,
        last_rests: bool,
        amount: i64,
        max_pay: Option<i64>,
        out: &mut Vec<(i64, Vec<IntentAsk>)>,
    ) {
        if idx.len() + 1 == k {
            let full: i64 = idx.iter().map(|&i| asks[i].0.state.custody_amount()).sum();
            let rest = amount - full;
            if rest <= 0 {
                return;
            }
            for (j, a) in asks.iter().enumerate() {
                if idx.contains(&j) {
                    continue;
                }
                let s = &a.0.state;
                let fits = if last_rests { rest < s.amount_left && s.tif == TIF_GTC } else { rest == s.amount_left };
                if !fits || !s.fill_ok(rest) {
                    continue;
                }
                let mut legs: Vec<IntentAsk> = idx
                    .iter()
                    .map(|&i| IntentAsk { order: asks[i].0.clone(), custody: asks[i].1.clone(), amount: asks[i].0.state.amount_left })
                    .collect();
                legs.push(IntentAsk { order: a.0.clone(), custody: a.1.clone(), amount: rest });
                let Some(quote) = legs.iter().try_fold(0i64, |acc, l| acc.checked_add(ask_quote(&l.order.state, l.amount)?)) else {
                    continue;
                };
                if max_pay.is_some_and(|m| quote > m) {
                    continue;
                }
                out.push((quote, legs));
            }
            return;
        }
        for i in start..asks.len() {
            idx.push(i);
            rec(asks, k, i + 1, idx, last_rests, amount, max_pay, out);
            idx.pop();
        }
    }
    let _ = n;
    rec(asks, k, 0, &mut idx, last_rests, amount, max_pay, &mut out);
    out.sort_by_key(|(q, _)| *q);
    out.into_iter().map(|(_, l)| l).collect()
}

/// Base units an ENDING bid of a sweep takes when it is not the last one: everything it can afford (a GTC bid is then
/// exhausted, a FOK bid filled completely, an IOC bid killed after the fill). `None`: the bid cannot end here.
fn bid_prefix_amount(o: &OrderUtxo<BidState>) -> Option<i64> {
    let aff = bid_affordable(o);
    (aff > 0 && matches!(o.state.tif, TIF_GTC | TIF_FOK | TIF_IOC) && bid_ends(o, aff) && o.state.fill_ok(aff, o.utxo.amount as i64))
        .then_some(aff)
}

/// A fill of `n` leaves the bid less than one minimum fill of buying power (a GTC bid then ends).
fn bid_ends(o: &OrderUtxo<BidState>, n: i64) -> bool {
    match o.state.used(n) {
        Some(u) => !o.state.can_continue(o.utxo.amount as i64 - u),
        None => false,
    }
}

/// The KAS a bid releases for `n` base units (its all-in spend at its quote, rounded down).
fn bid_spend(o: &OrderUtxo<BidState>, n: i64) -> Option<i64> {
    o.state.spend(n, 0, o.utxo.block_daa_score as i64)
}

/// The fewest base units the LAST bid of a sweep takes so that the sweep releases at least `missing` sompi, within the
/// amounts its position allows (resting: a GTC bid filled at least its minimum fill and keeping at least one minimum
/// fill of buying power; ending: an exhausted GTC or FOK bid takes everything it can afford, an IOC bid any amount
/// from its minimum fill). `None` when no amount fits.
fn bid_last_amount(o: &OrderUtxo<BidState>, rests: bool, missing: i64) -> Option<i64> {
    let aff = bid_affordable(o);
    if aff <= 0 {
        return None;
    }
    let s = &o.state;
    let rate = s.price.checked_add(s.tip).filter(|r| *r > 0)?;
    // fewest n with floor(n * rate / scale) >= missing: n = ceil(missing * scale / rate)
    let want = if missing <= 0 {
        1
    } else {
        let w = (missing as i128 * s.scale as i128 + rate as i128 - 1) / rate as i128;
        if w > aff as i128 {
            return None;
        }
        w as i64
    };
    let n = match (rests, s.tif) {
        (true, TIF_GTC) => {
            let n = want.max(s.min_fill);
            (n <= aff && !bid_ends(o, n)).then_some(n)?
        }
        (false, TIF_IOC) => want.max(s.min_fill).min(aff),
        (false, TIF_GTC | TIF_FOK) => bid_ends(o, aff).then_some(aff)?,
        _ => return None,
    };
    (s.fill_ok(n, o.utxo.amount as i64) && bid_spend(o, n)? >= missing).then_some(n)
}

/// Bid sweeps of `k` bids (the first `k - 1` end) selling at most `max_sell`, releasing at least `need`
/// sompi (all-in), fewest units sold first.
fn bid_sweeps(bids: &[&OrderUtxo<BidState>], k: usize, last_rests: bool, max_sell: i64, need: i64) -> Vec<Vec<IntentBid>> {
    let mut out: Vec<(i64, Vec<IntentBid>)> = vec![];
    fn rec(
        bids: &[&OrderUtxo<BidState>],
        k: usize,
        start: usize,
        chosen: &mut Vec<IntentBid>,
        last_rests: bool,
        max_sell: i64,
        need: i64,
        out: &mut Vec<(i64, Vec<IntentBid>)>,
    ) {
        let sold: i64 = chosen.iter().map(|b| b.amount).sum();
        let got: i64 = chosen.iter().map(|b| bid_spend(&b.order, b.amount).unwrap_or(0)).sum();
        if chosen.len() + 1 == k {
            for b in bids {
                if chosen.iter().any(|c| c.order.utxo == b.utxo) {
                    continue;
                }
                let Some(n) = bid_last_amount(b, last_rests, need - got) else { continue };
                let s2 = sold + n;
                if s2 <= max_sell {
                    let mut legs = chosen.clone();
                    legs.push(IntentBid { order: (*b).clone(), amount: n });
                    out.push((s2, legs));
                }
            }
            return;
        }
        for i in start..bids.len() {
            let Some(n) = bid_prefix_amount(bids[i]) else { continue };
            if sold + n > max_sell {
                continue;
            }
            chosen.push(IntentBid { order: bids[i].clone(), amount: n });
            rec(bids, k, i + 1, chosen, last_rests, max_sell, need, out);
            chosen.pop();
        }
    }
    let mut chosen = vec![];
    rec(bids, k, 0, &mut chosen, last_rests, max_sell, need, &mut out);
    out.sort_by_key(|(s, _)| *s);
    out.into_iter().map(|(_, l)| l).collect()
}

/// Units of the lock an execution may sell: all of them, but one for a KRON lock (its change output holds at least one
/// unit).
fn sellable(f: &IntentFacts) -> i64 {
    let amount = f.lock.as_ref().map(|l| l.state.amount()).unwrap_or(0);
    if f.state.a_family() == Family::Kron {
        amount - 1
    } else {
        amount
    }
}

/// Candidate executions of a verified intent against `book`, best first (cheapest quote for the payer's
/// KAS, fewest units sold of the payer's token). Orders in `exclude` (lost to a conflict) are skipped.
/// The fee is not known before building: a token intent's candidates must release the merchant's KAS
/// plus `fee_reserve` (beyond the intent's own value); [`build_execution`] builds them in order.
pub fn plan_executions(
    f: &IntentFacts,
    book: &BookView,
    exclude: &BTreeSet<Outpoint>,
    lock_time: u64,
    kp: &KeeperParams,
    fee_reserve: i64,
) -> Result<Vec<ExecuteIntent>> {
    let actor = f.actor()?;
    let shape = actor.shape;
    let op = |u: &Utxo| Outpoint::new(u.transaction_id, u.index);
    let mut asks: Vec<&(OrderUtxo<AskState>, TokenUtxo)> = match f.state.merchant_token().zip(f.state.merchant_program()) {
        Some((t, p)) => book.asks.iter().filter(|(o, c)| !exclude.contains(&op(&o.utxo)) && ask_ok(o, c, &t, p, lock_time)).collect(),
        None => vec![],
    };
    asks.sort_by(|a, b| a.0.state.price.cmp(&b.0.state.price).then(a.0.utxo.block_daa_score.cmp(&b.0.utxo.block_daa_score)));
    asks.truncate(kp.max_candidates);
    let mut bids: Vec<&OrderUtxo<BidState>> = match f.state.locked_token().zip(f.state.locked_program()) {
        Some((t, p)) => book.bids.iter().filter(|o| !exclude.contains(&op(&o.utxo)) && bid_ok(o, &t, p, lock_time)).collect(),
        None => vec![],
    };
    bids.sort_by(|a, b| b.state.price.cmp(&a.state.price).then(a.utxo.block_daa_score.cmp(&b.utxo.block_daa_score)));
    bids.truncate(kp.max_candidates);
    let base = |asks: Vec<IntentAsk>, bids: Vec<IntentBid>, merchant_kas: u64| ExecuteIntent {
        actor: f.actor.clone(),
        state: f.state.clone(),
        intent: f.intent.clone(),
        lock: f.lock.clone(),
        asks,
        bids,
        merchant_carrier: if f.merchant_gets_token() { f.merchant_value } else { 0 },
        merchant_kas,
        payer_token_carrier: None,
        filler: kp.filler,
        keeper: kp.keeper,
        lock_time,
        records: vec![],
        fee: kp.fee.clone(),
    };
    let fillers = |n: usize| (n as i64) * kp.filler as i64;
    let mut out = vec![];
    match &f.state {
        IntentState::KasToToken { amount, max_pay, .. } => {
            for legs in ask_sweeps(&asks, shape.asks, shape.last_ask_rests, *amount, Some(*max_pay)) {
                out.push(base(legs, vec![], 0));
            }
        }
        IntentState::TokenToKas { merchant_kas, max_sell, .. } => {
            let lock_amt = sellable(f);
            // fillers: one when the last bid ends
            let need = *merchant_kas + fee_reserve + if shape.last_bid_rests { 0 } else { fillers(1) } - f.intent.amount as i64;
            for legs in bid_sweeps(&bids, shape.bids, shape.last_bid_rests, (*max_sell).min(lock_amt), need.max(1)) {
                out.push(base(vec![], legs, *merchant_kas as u64));
            }
        }
        IntentState::TokenSwap { max_sell_a, amount_b, .. } => {
            let lock_amt = sellable(f);
            for ask_legs in ask_sweeps(&asks, shape.asks, shape.last_ask_rests, *amount_b, None).into_iter().take(4) {
                let Some(pay) = ask_legs.iter().try_fold(0i64, |acc, a| {
                    acc.checked_add(a.order.state.proceeds(a.amount, 0, a.order.utxo.block_daa_score as i64)?)
                }) else {
                    continue;
                };
                // the queue holds ka + 1 slots: custody rest of B, bid continuation, ask continuation
                let queued = usize::from(shape.last_ask_rests) * 2 + usize::from(shape.last_bid_rests);
                let need = pay + f.merchant_value as i64 + fee_reserve + fillers((shape.asks + 1).saturating_sub(queued))
                    - f.intent.amount as i64;
                for bid_legs in
                    bid_sweeps(&bids, shape.bids, shape.last_bid_rests, (*max_sell_a).min(lock_amt), need.max(1)).into_iter().take(4)
                {
                    out.push(base(ask_legs.clone(), bid_legs, 0));
                }
            }
        }
    }
    Ok(out)
}

/// Builds (and engine-validates) the first candidate of [`plan_executions`] that funds itself, raising the
/// fee reserve of token intents until the fee is covered. `None`: the book cannot execute the intent now.
pub fn build_execution(
    f: &IntentFacts,
    book: &BookView,
    exclude: &BTreeSet<Outpoint>,
    lock_time: u64,
    kp: &KeeperParams,
) -> Result<Option<Execution>> {
    let mut tried = 0usize;
    let mut last_err = None;
    for reserve in [5_000_000i64, 20_000_000, 60_000_000] {
        for r in plan_executions(f, book, exclude, lock_time, kp, reserve)? {
            if tried >= kp.max_builds {
                break;
            }
            tried += 1;
            match execute_intent(&r) {
                Ok((signed, facts)) => {
                    let (tx, entries) = signed.tx.to_tx().map_err(X402Error::from)?;
                    kob_protocol::verify::validate(&tx, &entries).map_err(X402Error::from)?;
                    let txid = tx.id().as_bytes();
                    let orders = r
                        .asks
                        .iter()
                        .map(|a| Outpoint::new(a.order.utxo.transaction_id, a.order.utxo.index))
                        .chain(r.bids.iter().map(|b| Outpoint::new(b.order.utxo.transaction_id, b.order.utxo.index)))
                        .collect();
                    let mo = facts.merchant_output;
                    let merchant_spk = tx.outputs[mo as usize].script_public_key.clone();
                    if spk_to_hex(&merchant_spk) != f.merchant_spk {
                        return Err(X402Error::new(
                            Reason::UnexpectedSettleError,
                            Diag::Internal,
                            "an execution pays another merchant script",
                        ));
                    }
                    return Ok(Some(Execution { tx, entries, facts, orders, merchant_output: Outpoint::new(txid, mo), merchant_spk }));
                }
                Err(e) => last_err = Some(e),
            }
        }
        if !f.state.kind().locks_tokens() {
            break; // KasToToken funds itself from the intent (max_extra); a larger reserve changes nothing
        }
    }
    if let Some(e) = last_err {
        eprintln!("x402 intent: no candidate execution builds ({tried} tried): {e}");
    }
    Ok(None)
}

/// The expiry of a verified intent (anyone may submit it once the chain's past median time reached the intent's
/// deadline): the intent's KAS back to the payer, less the fee, and the locked tokens to the payer's key. Needs no
/// signature. `intent` and `lock` carry the UTXOs as the chain reports them.
pub fn build_expiry(
    f: &IntentFacts,
    intent: &Utxo,
    lock: Option<&TokenUtxo>,
    fee: &FeeOptions,
) -> Result<(Transaction, Vec<UtxoEntry>)> {
    let r = kob_protocol::build::ExpireIntent {
        actor: f.actor.clone(),
        state: f.state.clone(),
        intent: intent.clone(),
        lock: lock.cloned(),
        fee: fee.clone(),
    };
    let built = kob_protocol::build::build_expire_intent(&r, &kob_protocol::build::intent_budgets).map_err(X402Error::from)?;
    let signed = kob_protocol::tx::finalize(&built, &[], kob_protocol::tx::FinalizeOptions { tighten_budgets: false })
        .map_err(X402Error::from)?;
    signed.tx.to_tx().map_err(X402Error::from)
}

/// The facts of an execution transaction of `f` already built (for the ledger): the merchant output and
/// its value, so acceptance can be observed.
pub fn merchant_watch(f: &IntentFacts, ex: &Execution) -> (Outpoint, ScriptPublicKey, u64) {
    (ex.merchant_output, ex.merchant_spk.clone(), f.merchant_value)
}

/// Txid of a creation's payload (helper for the facilitator's lock before verification).
pub fn creation_txid(payload: &PaymentPayload, ctx: &VerifyCtx) -> Option<Txid> {
    parse_tx(ctx, payload).ok().map(|p| p.tx.id().as_bytes())
}

/// Hex of the intent's script (for SDKs).
pub fn intent_script_hex(actor: &Actor, state: &IntentState) -> Result<String> {
    Ok(spk_to_hex(&state.spk(actor).map_err(|e| unsupported(e.to_string()))?))
}

/// True if this pay asset entry is KAS.
pub fn is_kas(asset: &str) -> bool {
    asset == ASSET_KAS
}
