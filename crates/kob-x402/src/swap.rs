//! Swap-and-pay (`extra.route`, binding `kob-swap-v1`): the payer pays with token A (or KAS), the
//! merchant receives KAS or KCC-20 token B, settled through KOB orders in ONE transaction.
//!
//! Token A is any allowlisted token of either family (KCC-20 programs, KaspaCom's included, or a KRON
//! token); token B is a KCC-20 token. Every leg follows the family of its token: KCC-20 orders
//! (`KobAsk`, `KobBid`) trade KCC-20 tokens, KRON orders (`KobAskKron`, `KobBidKron`) trade KRON
//! tokens, and one transaction may mix both (A KRON -> KAS -> B KCC-20). Templates, states and slot
//! layouts come from `kob_protocol` (`TemplateId::ALL`, `token_template`, `TokenState`), never from a
//! fixed list here.
//!
//! The transaction is built by `kob_protocol::build::{Batch, SwapRoute}`: order inputs first (input `i`
//! is paid at output `i`, the anti-aliasing layout), then token inputs (custodies, payer tokens), then
//! payer KAS funding; outputs are the positional slots `0..k`, order continuations and custody
//! remainders, KAS payments, the net token outputs and the payer's KAS change.
//!
//! What this verifier establishes (fail closed, everything recomputed from the trusted chain view):
//!
//! * every input is classified from the trusted UTXO: a plain `KobAsk` / `KobBid` (template match on the
//!   redeem script AND `P2SH(redeem) == UTXO script`), an allowlisted token input (a payer's key-owned
//!   token or an ask's exact custody), or a payer P2PK KAS input; everything else is rejected;
//! * order inputs are the first `k` inputs; outputs `0..k` are the orders' positional slots, and every
//!   output bound to an order covenant id (continuations) or recomputed as a custody remainder is
//!   claimed by an order and never counts as merchant gain (G3: no aliasing);
//! * the merchant gains exactly `amount` (KAS: one unclaimed output to `payToScriptPublicKey`; token:
//!   one unclaimed token output whose script is the recomputed `tokenScriptPublicKey`, carrier exact);
//!   every other unclaimed output is payer-controlled (P2PK of a payer input key, or the payer's token
//!   change whose script is recomputed);
//! * borrow is disabled on every KCC-20 token input and every token state the merchant or payer receives
//!   (G9); a KRON token input is never a minter (`is_minter` 0, the same guarantee) and is owned by a key
//!   (address presence: the payer's P2PK input is in the transaction) or by the order that custodies it;
//! * authorization is the payload commitment (`kob-x402-payload-commitment-v1`, `route.payAsset` is
//!   committed), so every input authorizer's SIGHASH_ALL covers it;
//! * the script engine executes every order covenant and every token program
//!   ([`crate::common::check_economics_and_scripts`]).
//!
//! An order (or continuation) that is no longer spendable is not a fraud signal but a race: the error is
//! [`Diag::OrderConflict`], retryable, with the spent order outpoints in `details.orders`; the payer must
//! re-quote and RE-SIGN (SIGHASH_ALL covers everything, the facilitator can never rebuild: G2).

use std::collections::{BTreeMap, BTreeSet};

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kob_protocol::artifacts::{template, token_template, token_template_by_hash, TemplateId};
use kob_protocol::registry::Family;
use kob_protocol::script::{p2pk_spk, p2sh_spk};
use kob_protocol::state::{AnyState, Kcc20State, TokenState, TIF_GTC};
use serde_json::{json, Map, Value};

use crate::chain::Outpoint;
use crate::common::{
    address_of, check_economics_and_scripts, check_envelope, check_expiry, check_payload_commitment, effective_finality, is_p2pk,
    outpoint_of, p2pk_key, parse_tx, payload_commit_digest, payment_identifier, resolve_entries, Envelope, PayloadCommit,
};
use crate::error::{Diag, Reason, Result, X402Error};
use crate::policy::{AllowedToken, Custody, Policy};
use crate::safe_tx::spk_to_hex;
use crate::verify::{PaymentKind, Verified, VerifyCtx, WatchedOutput};
use crate::wire::{
    hex, parse_hash32, parse_u64_canonical, PaymentPayload, PaymentRequirements, Profile, ASSET_KAS, AUTH_VERSION_PAYLOAD,
    BINDING_SWAP, TX_ENCODING,
};

/// Most `payAssets` entries an offer may carry.
pub const MAX_PAY_ASSETS: usize = 16;

/// `route.payAsset` value of a payer who pays with KAS (only when the merchant receives a token).
pub const PAY_ASSET_KAS: &str = ASSET_KAS;

/// Every order template of every family (a script that is none of these is unknown).
fn order_templates() -> impl Iterator<Item = TemplateId> {
    TemplateId::ALL.into_iter().filter(|t| !t.is_token())
}

/// True if a redeem script has the shape of some pinned token program (either family).
fn is_token_shape(redeem: &[u8]) -> bool {
    TemplateId::ALL.into_iter().filter(|t| t.is_token()).any(|t| token_template(t).state_of(redeem).is_some())
}

// ------------------------------------------------------------------------------------------ script pushes

static SMALL: [[u8; 1]; 17] = [[0], [1], [2], [3], [4], [5], [6], [7], [8], [9], [10], [11], [12], [13], [14], [15], [16]];
static NEG1: [u8; 1] = [0x81];

/// Splits a signature script into its data pushes. `None` if the script contains anything but pushes
/// (a covenant / P2SH signature script is pushes only) or a push runs past the end. Small-number
/// opcodes (`OP_0`, `OP_1NEGATE`, `OP_1`..`OP_16`) count as one-byte pushes.
pub fn script_pushes(script: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out: Vec<&[u8]> = Vec::new();
    let mut i = 0usize;
    while i < script.len() {
        let op = script[i];
        i += 1;
        let len = match op {
            0x00 => {
                out.push(&[]);
                continue;
            }
            0x01..=0x4b => op as usize,
            0x4c => {
                let l = *script.get(i)? as usize;
                i += 1;
                l
            }
            0x4d => {
                let b = script.get(i..i.checked_add(2)?)?;
                i += 2;
                u16::from_le_bytes([b[0], b[1]]) as usize
            }
            0x4e => {
                let b = script.get(i..i.checked_add(4)?)?;
                i += 4;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
            }
            0x4f => {
                out.push(&NEG1);
                continue;
            }
            0x51..=0x60 => {
                out.push(&SMALL[(op - 0x50) as usize]);
                continue;
            }
            _ => return None,
        };
        let end = i.checked_add(len)?;
        out.push(script.get(i..end)?);
        i = end;
    }
    Some(out)
}

/// The redeem script of a P2SH signature script (its last push).
pub fn redeem_of(script: &[u8]) -> Option<&[u8]> {
    script_pushes(script)?.last().copied().filter(|r| !r.is_empty())
}

/// The order template a redeem script is an instance of, if any (structural match only).
pub fn order_template_of(redeem: &[u8]) -> Option<TemplateId> {
    order_templates().find(|id| template(*id).state_of(redeem).is_some())
}

/// The fill quantity of an order input: its first push, the 8-byte base-unit amount n.
fn fill_arg(pushes: &[&[u8]]) -> Option<i64> {
    let b = pushes.first()?;
    (b.len() == 8).then(|| i64::from_le_bytes(b[..8].try_into().expect("8 bytes")))
}

// ------------------------------------------------------------------------------------------ the offer

/// One `extra.route.payAssets` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayAssetOffer {
    /// Covenant id (64 lowercase hex) or `"KAS"`.
    pub asset: String,
    pub template_hash: Option<[u8; 32]>,
    pub extension_commitment: Option<[u8; 32]>,
}

fn req(msg: impl Into<String>) -> X402Error {
    X402Error::requirements(Diag::InvalidKaspaX402Binding, msg)
}

fn lower_hex32(s: &str) -> Option<[u8; 32]> {
    (s == s.to_ascii_lowercase()).then(|| parse_hash32(s)).flatten()
}

/// Parses and validates `offered.extra.route` (binding, critical, payAssets shape).
pub fn parse_route_offer(offered: &PaymentRequirements) -> Result<Vec<PayAssetOffer>> {
    let route = offered.extra.get("route").and_then(Value::as_object).ok_or_else(|| req("extra.route must be an object"))?;
    if route.get("binding").and_then(Value::as_str) != Some(BINDING_SWAP) {
        return Err(req("extra.route.binding must be kob-swap-v1"));
    }
    if route.get("critical") != Some(&Value::Bool(true)) {
        return Err(req("extra.route.critical must be true"));
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

/// The merchant token of a `kcc20` swap offer, recomputed from the allowlist and the offer.
#[derive(Clone, Debug)]
pub struct MerchantToken {
    pub id: [u8; 32],
    pub allowed: AllowedToken,
    /// x-only owner key (from `payTo`).
    pub owner: [u8; 32],
    pub carrier: u64,
    pub state: Kcc20State,
    pub spk: ScriptPublicKey,
}

/// A candidate state with borrow enabled (`borrow_scheme` 1..=255, zero guard) whose script equals `spk`.
fn borrow_variant_matches(spk: &ScriptPublicKey, tpl: &kob_protocol::artifacts::Template, base: &Kcc20State) -> bool {
    (1..=255u8).any(|b| &Kcc20State { borrow_scheme: b, ..base.clone() }.spk_with(tpl) == spk)
}

/// Recomputes the merchant token output of a `kcc20` swap offer (the same rules as the token profile;
/// this module does not depend on the token verifier).
pub fn merchant_token(pol: &Policy, offered: &PaymentRequirements, env: &Envelope) -> Result<MerchantToken> {
    let id = lower_hex32(&offered.asset).ok_or_else(|| req("asset must be a 64-hex covenant id for the kcc20 profile"))?;
    let allowed = pol
        .tokens
        .find(&id)
        .ok_or_else(|| X402Error::requirements(Diag::TokenNotAllowlisted, "the asset is not an accepted token"))?
        .clone();
    if !allowed.is_merchant_capable() {
        return Err(X402Error::requirements(
            Diag::TokenNotAllowlisted,
            "the asset is a KRON token: it is a pay asset, not a merchant asset of the kcc20 profile",
        ));
    }
    let t = offered.extra.get("token").and_then(Value::as_object).ok_or_else(|| req("extra.token must be an object"))?;
    let s = |k: &str| t.get(k).and_then(Value::as_str);
    if s("family") != Some("kcc20") {
        return Err(req("extra.token.family must be kcc20"));
    }
    let tpl = template(allowed.program);
    if s("templateHash").and_then(lower_hex32) != Some(allowed.template_hash()) {
        return Err(X402Error::requirements(
            Diag::TokenTemplateMismatch,
            "extra.token.templateHash is not the pinned program of this token",
        ));
    }
    if s("extensionCommitment").and_then(lower_hex32) != Some(allowed.extension_commitment) {
        return Err(X402Error::requirements(Diag::TokenNotAllowlisted, "extra.token.extensionCommitment differs from the allowlist"));
    }
    if s("custody") != Some(allowed.custody.as_str()) {
        return Err(X402Error::requirements(Diag::TokenCustodyPolicy, "extra.token.custody differs from the allowlist"));
    }
    if allowed.custody == Custody::IssuerControlled && !pol.allow_issuer_controlled {
        return Err(X402Error::requirements(Diag::TokenCustodyPolicy, "issuer-controlled tokens are not accepted by this verifier"));
    }
    let carrier = s("carrier")
        .and_then(parse_u64_canonical)
        .ok_or_else(|| X402Error::requirements(Diag::CarrierMismatch, "extra.token.carrier must be a canonical uint64"))?;
    if carrier < pol.limits.min_carrier_sompi {
        return Err(X402Error::requirements(
            Diag::CarrierMismatch,
            format!("extra.token.carrier {carrier} is below the policy minimum {}", pol.limits.min_carrier_sompi),
        ));
    }
    if carrier > pol.limits.max_carrier_sompi {
        return Err(X402Error::requirements(
            Diag::CarrierMismatch,
            format!("extra.token.carrier {carrier} is above the policy ceiling {}", pol.limits.max_carrier_sompi),
        ));
    }
    let owner = p2pk_key(&env.pay_to_spk)
        .ok_or_else(|| X402Error::requirements(Diag::TokenOwnerScheme, "payTo must be a Schnorr P2PK address for a token payment"))?;
    let amount = i64::try_from(env.amount).map_err(|_| req("amount exceeds the token amount range"))?;
    let state = Kcc20State::p2pk(amount, owner, allowed.extension_commitment);
    let spk = state.spk_with(tpl);
    let claimed = s("tokenScriptPublicKey").unwrap_or("").to_ascii_lowercase();
    if claimed != spk_to_hex(&spk) {
        let is_borrow = crate::safe_tx::spk_from_hex(&claimed).is_ok_and(|c| borrow_variant_matches(&c, tpl, &state));
        return Err(if is_borrow {
            X402Error::requirements(Diag::TokenBorrowEnabled, "extra.token.tokenScriptPublicKey enables borrowing")
        } else {
            req("extra.token.tokenScriptPublicKey does not match the recomputed script")
        });
    }
    Ok(MerchantToken { id, allowed, owner, carrier, state, spk })
}

// ------------------------------------------------------------------------------------------ facts

/// Kind of an order leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegKind {
    /// The payer sells token A into a bid (the bid's escrow pays the KAS).
    Bid,
    /// The payer buys token from an ask.
    Ask,
}

/// One verified order leg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegFact {
    pub outpoint: Outpoint,
    pub kind: LegKind,
    /// Token base units the leg moves (the order's fill argument n).
    pub units: u128,
    /// Token covenant id of the order.
    pub token: [u8; 32],
}

/// What the payer gives up and which orders carry the payment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapFacts {
    /// `route.payAsset` (`"KAS"` or the token covenant id).
    pub pay_asset: String,
    /// Order outpoints in input order.
    pub orders: Vec<Outpoint>,
    pub legs: Vec<LegFact>,
    /// Units of the pay asset the payer loses net of change (sompi for KAS, including the fee).
    pub payer_spent: u64,
    /// Keys of the payer's inputs (P2PK KAS inputs and token owners).
    pub payer_keys: Vec<[u8; 32]>,
    /// Output index of the merchant's gain.
    pub merchant_output: u32,
}

#[derive(Clone, Copy)]
enum PayAsset<'a> {
    Kas,
    Token(&'a AllowedToken),
}

struct Leg {
    input: usize,
    outpoint: Outpoint,
    /// Covenant id of the order UTXO.
    cov: [u8; 32],
    kind: LegKind,
    /// Base units filled.
    amount: i64,
    token: [u8; 32],
    /// An ask's custody (base units); 0 for a bid.
    amount_left: i64,
    tif: i64,
    maker: [u8; 32],
}

enum InKind {
    Order,
    /// Payer P2PK KAS input.
    PayerKas([u8; 32]),
    /// Payer key-owned token input (KCC-20 P2PK, KRON address presence).
    PayerToken([u8; 32], TokenState),
    /// Custody token of an ask order.
    Custody(TokenState),
}

#[derive(Default)]
struct Flow {
    supplied: i128,
    sold: i128,
    bought: i128,
}

fn unsupported(msg: impl Into<String>) -> X402Error {
    X402Error::payload(Diag::RouteUnsupported, msg)
}

fn token_input(pol: &Policy, entry: &UtxoEntry, redeem: &[u8]) -> Result<(AllowedToken, TokenState)> {
    let id = entry.covenant_id.map(|h| h.as_bytes()).ok_or_else(|| {
        X402Error::payload(Diag::UnknownOrderTemplate, "an input is neither a KOB order, an allowlisted token nor a P2PK output")
    })?;
    let Some(allowed) = pol.tokens.find(&id) else {
        return Err(if is_token_shape(redeem) {
            X402Error::payload(Diag::TokenNotAllowlisted, "a token input is not an accepted token")
        } else {
            X402Error::payload(Diag::UnknownOrderTemplate, "an input spends an unknown covenant")
        });
    };
    let tpl = token_template(allowed.program);
    let state = TokenState::from_redeem_with(tpl, redeem).map_err(|_| {
        X402Error::payload(Diag::TokenTemplateMismatch, "a token input is not an instance of the token's pinned program")
    })?;
    if p2sh_spk(redeem) != entry.script_public_key {
        return Err(X402Error::payload(
            Diag::TokenTemplateMismatch,
            "a token input's redeem script does not hash to the chain script",
        ));
    }
    match &state {
        TokenState::Kcc20(s) if s.borrow_scheme != 0 || s.borrow_guard != [0; 32] => {
            return Err(X402Error::payload(Diag::TokenBorrowEnabled, "a token input has borrowing enabled"));
        }
        TokenState::Kron(s) if s.is_minter != 0 => {
            return Err(X402Error::payload(Diag::TokenBorrowEnabled, "a KRON token input is a minter UTXO"));
        }
        _ => {}
    }
    if state.extension() != allowed.extension_commitment {
        return Err(X402Error::payload(Diag::TokenNotAllowlisted, "a token input carries another extension commitment"));
    }
    if state.amount() <= 0 {
        return Err(X402Error::payload(Diag::TokenConservation, "a token input holds no units"));
    }
    Ok((allowed.clone(), state))
}

/// Checks an order's token fields against the allowlist and returns the token. `fam` is the family of
/// the order kind: a KCC-20 order trades KCC-20 tokens and a KRON order KRON tokens.
fn order_token<'a>(
    pol: &'a Policy,
    fam: Family,
    id: &[u8; 32],
    hash: &[u8; 32],
    pl: i64,
    sl: i64,
    ext: Option<[u8; 32]>,
) -> Result<&'a AllowedToken> {
    let allowed = pol
        .tokens
        .find(id)
        .ok_or_else(|| X402Error::payload(Diag::TokenNotAllowlisted, "an order trades a token that is not accepted"))?;
    let tpl = token_template(allowed.program);
    let by_hash = token_template_by_hash(hash);
    if allowed.family != fam
        || by_hash.map(|t| t.id) != Some(allowed.program)
        || tpl.prefix.len() as i64 != pl
        || tpl.suffix.len() as i64 != sl
    {
        return Err(X402Error::payload(Diag::TokenTemplateMismatch, "an order names another program than the token's pinned one"));
    }
    if ext.is_some_and(|e| e != allowed.extension_commitment) {
        return Err(X402Error::payload(Diag::TokenNotAllowlisted, "an order pins another extension commitment"));
    }
    Ok(allowed)
}

fn check_scale(scale: i64) -> Result<()> {
    if scale <= 0 {
        return Err(unsupported("an order has a non-positive scale"));
    }
    Ok(())
}

fn spent_orders_error(orders: Vec<Outpoint>) -> X402Error {
    X402Error::state(Diag::OrderConflict, "a named order is no longer spendable; re-quote and sign again")
        .retryable()
        .with_details(json!({ "orders": orders.iter().map(Outpoint::to_json).collect::<Vec<_>>() }))
}

/// Verifies a swap-and-pay payment (see the module docs).
pub fn verify_swap(ctx: &VerifyCtx, offered: &PaymentRequirements, payload: &PaymentPayload, request_hash: &str) -> Result<Verified> {
    verify_swap_facts(ctx, offered, payload, request_hash).map(|(v, _)| v)
}

/// [`verify_swap`] plus the payer-side facts (what the payer spends, which orders, which legs).
pub fn verify_swap_facts(
    ctx: &VerifyCtx,
    offered: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
) -> Result<(Verified, SwapFacts)> {
    let pol = ctx.policy;
    if !pol.swap_enabled {
        return Err(X402Error::new(Reason::UnsupportedScheme, Diag::RouteUnsupported, "swap-and-pay is not enabled on this verifier"));
    }
    // 1. envelope, offer and payload route
    let env = check_envelope(ctx, offered, payload, request_hash)?;
    let pay_offers = parse_route_offer(offered)?;
    let route = payload.payload.route.as_ref().ok_or_else(|| {
        X402Error::payload(Diag::InvalidKaspaX402Payload, "the offer carries extra.route but the payload has no route")
    })?;
    if route.binding != BINDING_SWAP {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "payload.route.binding must be kob-swap-v1"));
    }
    if route.orders.len() > pol.limits.max_inputs {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "payload.route.orders is too long"));
    }
    // merchant gain kind
    let merchant: Option<MerchantToken> = match env.profile {
        Profile::StandardNative => {
            if offered.asset != ASSET_KAS || offered.extra.contains_key("token") {
                return Err(req("a standard-native swap offer pays KAS: asset must be KAS and extra.token absent"));
            }
            if pol.limits.min_amount_sompi > 0 && env.amount < pol.limits.min_amount_sompi {
                return Err(X402Error::requirements(Diag::InvalidKaspaX402Amount, "amount is below the policy minimum"));
            }
            None
        }
        Profile::Kcc20 => Some(merchant_token(pol, offered, &env)?),
        Profile::Additive => {
            return Err(X402Error::new(Reason::UnsupportedScheme, Diag::UnsupportedKaspaExactProfile, "additive is not supported"))
        }
    };
    // pay asset
    if route.pay_asset != route.pay_asset.trim() || route.pay_asset.is_empty() {
        return Err(X402Error::payload(Diag::PayAssetNotAccepted, "route.payAsset is malformed"));
    }
    let offer_entry = pay_offers
        .iter()
        .find(|e| e.asset == route.pay_asset)
        .ok_or_else(|| X402Error::payload(Diag::PayAssetNotAccepted, "route.payAsset is not listed in the offer's payAssets"))?;
    let pay_asset = if route.pay_asset == PAY_ASSET_KAS {
        if merchant.is_none() {
            return Err(X402Error::payload(Diag::PayAssetNotAccepted, "payAsset equals the merchant asset (KAS)"));
        }
        PayAsset::Kas
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
        PayAsset::Token(allowed)
    };
    let auth = &payload.payload.authorization;
    if auth.version != AUTH_VERSION_PAYLOAD || auth.signature.is_some() || auth.input_index.is_some() {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "a swap payment is authorized by the payload commitment only"));
    }

    // 2. authorization: the digest commits to the claimed payment output index (checked against the
    // transaction below), the pay asset, the offer and the expiry
    let spk_hex = spk_to_hex(&env.pay_to_spk);
    let digest = payload_commit_digest(&PayloadCommit {
        network: env.network,
        profile: env.profile,
        route_pay_asset: Some(&route.pay_asset),
        asset: &offered.asset,
        amount: &offered.amount,
        pay_to: &offered.pay_to,
        pay_to_spk_hex: &spk_hex,
        payment_output_index: payload.payload.payment_output_index,
        requirements_hash: &env.requirements_hash,
        request_hash: &env.request_hash,
        expires_at: &auth.expires_at,
    })?;
    if parse_hash32(&auth.digest) != Some(digest) {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "authorization.digest differs from the recomputed commitment"));
    }
    let expires_ms = check_expiry(ctx.clock.now_ms(), env.max_timeout_seconds, &auth.expires_at)?;
    let payment_identifier = payment_identifier(ctx, payload)?;

    // 3. transaction shape
    let parsed = parse_tx(ctx, payload)?;
    let tx = &parsed.tx;
    if tx.version != 1 || tx.gas != 0 {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "a swap transaction is version 1 with gas 0"));
    }
    let daa = ctx
        .chain
        .virtual_daa_score()
        .map_err(|e| X402Error::new(Reason::UnexpectedSettleError, Diag::NodeUnavailable, e.to_string()).retryable())?;
    if tx.lock_time > daa {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "the transaction lock time is in the future"));
    }
    // classify by signature script BEFORE resolution, to tell a spent order from a spent payer input
    let sig_pushes: Vec<Option<Vec<&[u8]>>> = tx.inputs.iter().map(|i| script_pushes(&i.signature_script)).collect();
    let order_hint: Vec<bool> = tx
        .inputs
        .iter()
        .zip(&parsed.hints)
        .map(|(i, h)| {
            redeem_of(&i.signature_script)
                .is_some_and(|r| order_template_of(r).is_some() && h.as_ref().is_some_and(|h| h.script_public_key == p2sh_spk(r)))
        })
        .collect();

    check_payload_commitment(tx, &digest)?;

    // 4. trusted resolution, with order conflicts told apart
    let entries = match resolve_entries(ctx, &parsed) {
        Ok(e) => e,
        Err(e) if e.diag == Diag::InvalidKaspaExactUtxo && e.reason == Reason::InvalidTransactionState => {
            let wanted: Vec<_> = tx
                .inputs
                .iter()
                .zip(&parsed.hints)
                .filter_map(|(i, h)| h.as_ref().map(|h| (outpoint_of(i), h.script_public_key.clone())))
                .collect();
            let found = ctx
                .chain
                .utxos(&wanted)
                .map_err(|e| X402Error::new(Reason::UnexpectedSettleError, Diag::NodeUnavailable, e.to_string()).retryable())?;
            let missing: Vec<usize> = found.iter().enumerate().filter(|(_, u)| u.is_none()).map(|(i, _)| i).collect();
            if !missing.is_empty() && missing.iter().all(|i| order_hint[*i]) {
                return Err(spent_orders_error(missing.iter().map(|i| outpoint_of(&tx.inputs[*i])).collect()));
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };

    // 5. classify every input from the trusted UTXO
    let mut kinds: Vec<InKind> = Vec::with_capacity(entries.len());
    let mut legs: Vec<Leg> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if is_p2pk(&entry.script_public_key) && entry.covenant_id.is_none() {
            kinds.push(InKind::PayerKas(p2pk_key(&entry.script_public_key).expect("p2pk")));
            continue;
        }
        let pushes = sig_pushes[i].as_deref().ok_or_else(|| {
            X402Error::payload(Diag::UnknownOrderTemplate, format!("input {i}: the signature script is not a P2SH push sequence"))
        })?;
        let redeem = pushes
            .last()
            .copied()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| X402Error::payload(Diag::UnknownOrderTemplate, format!("input {i}: no redeem script")))?;
        if let Some(tpl_id) = order_template_of(redeem) {
            if p2sh_spk(redeem) != entry.script_public_key {
                return Err(X402Error::payload(
                    Diag::UnknownOrderTemplate,
                    format!("input {i}: redeem script does not hash to the chain script"),
                ));
            }
            let cov = entry.covenant_id.map(|h| h.as_bytes()).ok_or_else(|| {
                X402Error::payload(Diag::UnknownOrderTemplate, format!("input {i}: an order UTXO has no covenant id"))
            })?;
            let state = AnyState::decode(tpl_id, template(tpl_id).state_of(redeem).expect("matched"))
                .map_err(|e| X402Error::payload(Diag::UnknownOrderTemplate, format!("input {i}: order state: {e}")))?;
            let amount = fill_arg(pushes)
                .filter(|n| *n > 0)
                .ok_or_else(|| unsupported(format!("input {i}: the fill quantity is not a positive 8-byte push")))?;
            let outpoint = outpoint_of(&tx.inputs[i]);
            let leg = match &state {
                AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
                    let t = order_token(
                        pol,
                        state.family(),
                        &s.token_cov_id,
                        &s.token_tpl_hash,
                        s.tpl_prefix_len,
                        s.tpl_suffix_len,
                        None,
                    )?;
                    check_scale(s.scale)?;
                    if amount > s.amount_left || s.amount_left <= 0 {
                        return Err(unsupported(format!("input {i}: fills more than the ask holds")));
                    }
                    Leg {
                        input: i,
                        outpoint,
                        cov,
                        kind: LegKind::Ask,
                        amount,
                        token: t.covenant_id,
                        amount_left: s.amount_left,
                        tif: s.tif,
                        maker: s.maker,
                    }
                }
                AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
                    let t = order_token(
                        pol,
                        state.family(),
                        &s.token_cov_id,
                        &s.token_tpl_hash,
                        s.tpl_prefix_len,
                        s.tpl_suffix_len,
                        Some(s.extension_commitment),
                    )?;
                    check_scale(s.scale)?;
                    Leg {
                        input: i,
                        outpoint,
                        cov,
                        kind: LegKind::Bid,
                        amount,
                        token: t.covenant_id,
                        amount_left: 0,
                        tif: s.tif,
                        maker: s.maker,
                    }
                }
                other => {
                    return Err(unsupported(format!(
                        "input {i}: {} legs are not supported by swap-and-pay (plain {} / {} only)",
                        other.template_id().name(),
                        other.family().kind_name("KobAsk"),
                        other.family().kind_name("KobBid")
                    )))
                }
            };
            legs.push(leg);
            kinds.push(InKind::Order);
            continue;
        }
        let (_, state) = token_input(pol, entry, redeem)?;
        if state.is_user() {
            kinds.push(InKind::PayerToken(state.owner(), state));
        } else if state.is_covenant_owned() {
            kinds.push(InKind::Custody(state));
        } else {
            return Err(X402Error::payload(Diag::TokenOwnerScheme, format!("input {i}: unsupported token owner scheme")));
        }
    }
    let k = legs.len();
    if k == 0 {
        return Err(unsupported("a swap-and-pay transaction spends at least one KOB order"));
    }
    if legs.iter().enumerate().any(|(idx, l)| l.input != idx) {
        return Err(X402Error::payload(
            Diag::RouteAliasing,
            "order inputs must be the first k inputs (positional outputs 0..k belong to them)",
        ));
    }
    if legs.iter().map(|l| l.cov).collect::<BTreeSet<_>>().len() != k {
        return Err(unsupported("an order covenant id appears twice"));
    }
    // custody: exactly one exact custody per ask, no strays
    let mut custody_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, kind) in kinds.iter().enumerate() {
        if let InKind::Custody(st) = kind {
            let li = legs.iter().position(|l| l.kind == LegKind::Ask && l.cov == st.owner()).ok_or_else(|| {
                X402Error::payload(
                    Diag::TokenOwnerScheme,
                    format!("input {i}: token owned by an order this transaction does not fill"),
                )
            })?;
            let l = &legs[li];
            if entries[i].covenant_id.map(|h| h.as_bytes()) != Some(l.token) {
                return Err(X402Error::payload(Diag::TokenOwnerScheme, format!("input {i}: custody of another token than its order")));
            }
            if st.amount() != l.amount_left {
                return Err(unsupported(format!("input {i}: a token owned by an order that is not its exact custody (a stray)")));
            }
            if custody_of.insert(li, i).is_some() {
                return Err(unsupported("an ask has two custody inputs"));
            }
        }
    }
    if legs.iter().enumerate().any(|(li, l)| l.kind == LegKind::Ask && !custody_of.contains_key(&li)) {
        return Err(unsupported("an ask order without its custody input"));
    }
    // leg roles against the pay asset and the merchant
    for l in &legs {
        match (l.kind, pay_asset) {
            (LegKind::Bid, PayAsset::Kas) => return Err(unsupported("a payer who pays KAS sells no token into bids")),
            (LegKind::Bid, PayAsset::Token(a)) if a.covenant_id != l.token => {
                return Err(X402Error::payload(Diag::PayAssetNotAccepted, "a bid buys a token other than the pay asset"))
            }
            _ => {}
        }
    }
    // payer inputs and keys
    let mut payer_keys: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut flows: BTreeMap<[u8; 32], Flow> = BTreeMap::new();
    let mut payer_kas_in: u128 = 0;
    for (i, kind) in kinds.iter().enumerate() {
        match kind {
            InKind::PayerKas(key) => {
                payer_keys.insert(*key);
                payer_kas_in += entries[i].amount as u128;
            }
            InKind::PayerToken(key, st) => {
                let tok = entries[i].covenant_id.expect("token").as_bytes();
                match pay_asset {
                    PayAsset::Token(a) if a.covenant_id == tok => {}
                    _ => {
                        return Err(X402Error::payload(
                            Diag::PayAssetNotAccepted,
                            format!("input {i}: the payer spends a token other than the pay asset"),
                        ))
                    }
                }
                payer_keys.insert(*key);
                flows.entry(tok).or_default().supplied += st.amount() as i128;
            }
            _ => {}
        }
    }
    if payer_keys.is_empty() {
        return Err(unsupported("the transaction has no payer input"));
    }
    // every payer signature is SIGHASH_ALL, checked on the signature scripts before the engine (which accepts the
    // other hash types): P2PK funding inputs and the owner witnesses of the payer's KCC-20 token inputs. Order
    // inputs and custody inputs carry no payer signature; a KRON token has none of its own (its owner's P2PK input
    // is covered above).
    crate::sighash::check_payer_inputs(
        tx,
        &entries,
        kinds.iter().enumerate().filter(|(_, k)| matches!(k, InKind::PayerKas(_))).map(|(i, _)| i),
        kinds.iter().enumerate().filter_map(|(i, k)| match k {
            InKind::PayerToken(_, TokenState::Kcc20(s)) => Some((i, s.owner_scheme)),
            _ => None,
        }),
    )?;
    // KRON address presence: the owner of a key-held KRON token authorises it with a P2PK input of its own
    for (i, kind) in kinds.iter().enumerate() {
        if let InKind::PayerToken(key, TokenState::Kron(_)) = kind {
            if !kinds.iter().any(|k| matches!(k, InKind::PayerKas(kk) if kk == key)) {
                return Err(X402Error::payload(
                    Diag::TokenOwnerScheme,
                    format!("input {i}: a KRON token held by a key needs a P2PK input of that key in the transaction"),
                ));
            }
        }
    }
    for l in &legs {
        let f = flows.entry(l.token).or_default();
        let units = l.amount as i128;
        match l.kind {
            LegKind::Bid => f.bought += units,
            LegKind::Ask => f.sold += units,
        }
    }

    // 6. outputs: claimed / merchant / payer-controlled
    let outs = &tx.outputs;
    if outs.len() < k {
        return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "fewer outputs than order inputs"));
    }
    // expected custody remainders of the asks (claimed by their orders)
    struct Rest {
        token: [u8; 32],
        spk: ScriptPublicKey,
        used: bool,
    }
    let mut rests: Vec<Rest> = Vec::new();
    for (li, l) in legs.iter().enumerate().filter(|(_, l)| l.kind == LegKind::Ask) {
        let rest = l.amount_left - l.amount;
        if rest > 0 {
            let cust = match &kinds[custody_of[&li]] {
                InKind::Custody(st) => st,
                _ => unreachable!("custody index"),
            };
            let st = if l.tif == TIF_GTC { cust.with_amount(rest) } else { cust.with_amount(rest).with_user_owner(l.maker) };
            let tpl = token_template(pol.tokens.find(&l.token).expect("allowlisted").program);
            rests.push(Rest { token: l.token, spk: st.spk_with(tpl), used: false });
        }
    }
    // net token amounts (conservation): what the payer / merchant side receives
    let merchant_id = merchant.as_ref().map(|m| m.id);
    let mut change_expect: BTreeMap<[u8; 32], i128> = BTreeMap::new();
    let mut net_of: BTreeMap<[u8; 32], i128> = BTreeMap::new();
    for (tok, f) in &flows {
        let net = f.supplied + f.sold - f.bought;
        if net < 0 {
            return Err(X402Error::payload(Diag::TokenConservation, "the orders take more tokens than the payer supplies"));
        }
        net_of.insert(*tok, net);
        let mut rest = net;
        if Some(*tok) == merchant_id {
            let want = env.amount as i128;
            if net < want {
                return Err(X402Error::payload(Diag::Underpayment, "the transaction delivers fewer tokens than the offered amount"));
            }
            rest = net - want;
        }
        if rest > 0 {
            change_expect.insert(*tok, rest);
        }
    }
    let pay_spk = &env.pay_to_spk;
    let mut merchant_at: Option<usize> = None;
    let mut merchant_dup = false;
    let mut kas_change: u128 = 0;
    let mut strays: Vec<usize> = Vec::new();
    let mut token_change_spent: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut claimed_spks: Vec<(usize, ScriptPublicKey)> = Vec::new();
    for (j, o) in outs.iter().enumerate() {
        if j < k {
            claimed_spks.push((j, o.script_public_key.clone()));
            continue;
        }
        match o.covenant {
            Some(b) => {
                let id = b.covenant_id.as_bytes();
                if let Some(li) = legs.iter().position(|l| l.cov == id) {
                    if b.authorizing_input as usize != legs[li].input {
                        return Err(X402Error::payload(
                            Diag::RouteUnsupported,
                            format!("output {j}: an order continuation is authorized by another input"),
                        ));
                    }
                    claimed_spks.push((j, o.script_public_key.clone()));
                    continue;
                }
                if !flows.contains_key(&id) {
                    return Err(unsupported(format!("output {j}: bound to a covenant this route does not use")));
                }
                let allowed_tok = pol.tokens.find(&id).expect("allowlisted");
                let tpl = token_template(allowed_tok.program);
                // custody remainder of an ask
                if let Some(r) = rests.iter_mut().find(|r| !r.used && r.token == id && r.spk == o.script_public_key) {
                    r.used = true;
                    claimed_spks.push((j, o.script_public_key.clone()));
                    continue;
                }
                // merchant token
                if let Some(m) = merchant.as_ref().filter(|m| m.id == id && m.spk == o.script_public_key) {
                    if merchant_at.is_some() {
                        merchant_dup = true;
                    } else {
                        if o.value != m.carrier {
                            return Err(X402Error::payload(
                                Diag::CarrierMismatch,
                                "the merchant token output does not carry exactly the offered carrier",
                            ));
                        }
                        merchant_at = Some(j);
                    }
                    continue;
                }
                // payer change of this token
                if let Some(want) = change_expect.get(&id).filter(|_| !token_change_spent.contains(&id)) {
                    let ok = payer_keys.iter().any(|key| {
                        TokenState::user(allowed_tok.family, *want as i64, *key, allowed_tok.extension_commitment).spk_with(tpl)
                            == o.script_public_key
                    });
                    if ok {
                        token_change_spent.insert(id);
                        continue;
                    }
                }
                strays.push(j);
            }
            None => {
                if merchant.is_none() && &o.script_public_key == pay_spk {
                    if merchant_at.is_some() {
                        merchant_dup = true;
                    } else {
                        merchant_at = Some(j);
                    }
                } else if p2pk_key(&o.script_public_key).is_some_and(|key| payer_keys.contains(&key)) {
                    kas_change += o.value as u128;
                } else {
                    strays.push(j);
                }
            }
        }
    }
    // merchant gain
    let Some(mj) = merchant_at else {
        // aliasing: the only output that pays the merchant is claimed by an order
        let alias = match &merchant {
            None => claimed_spks.iter().any(|(_, s)| s == pay_spk),
            Some(m) => claimed_spks.iter().any(|(_, s)| *s == m.spk),
        };
        if alias {
            return Err(X402Error::payload(
                Diag::RouteAliasing,
                "the only output that pays the merchant is an order's own payout (positional slot or continuation)",
            ));
        }
        if let Some(m) = &merchant {
            let tpl = template(m.allowed.program);
            for &j in &strays {
                let o = &outs[j];
                if o.covenant.map(|b| b.covenant_id.as_bytes()) != Some(m.id) {
                    continue;
                }
                if borrow_variant_matches(&o.script_public_key, tpl, &m.state) {
                    return Err(X402Error::payload(Diag::TokenBorrowEnabled, "the merchant token output has borrowing enabled"));
                }
                let net = net_of.get(&m.id).copied().unwrap_or(0);
                if net > 0 && net != env.amount as i128 {
                    let st = Kcc20State { amount: net as i64, ..m.state.clone() };
                    if st.spk_with(tpl) == o.script_public_key {
                        return Err(X402Error::payload(
                            if net > env.amount as i128 { Diag::Overpayment } else { Diag::Underpayment },
                            "the merchant token output does not carry exactly the offered amount",
                        ));
                    }
                }
            }
        }
        return Err(X402Error::payload(Diag::Underpayment, "no unclaimed output pays the merchant"));
    };
    if merchant_dup {
        return Err(X402Error::payload(Diag::Overpayment, "more than one output pays the merchant"));
    }
    if merchant.is_none() {
        let v = outs[mj].value;
        if v != env.amount {
            return Err(X402Error::payload(
                if v > env.amount { Diag::Overpayment } else { Diag::Underpayment },
                "the merchant output does not carry exactly the offered amount",
            ));
        }
    }
    if let Some(&j) = strays.first() {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactPaymentOutput,
            format!("output {j} is neither claimed by an order, the merchant's gain nor payer-controlled"),
        ));
    }
    if rests.iter().any(|r| !r.used) {
        return Err(unsupported("an ask's custody remainder is missing"));
    }
    if change_expect.keys().any(|t| !token_change_spent.contains(t)) {
        return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "the payer's token change is missing or misdirected"));
    }
    if payload.payload.payment_output_index as usize != mj {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactPaymentOutput,
            format!("paymentOutputIndex must be {mj}, the merchant's output"),
        ));
    }
    // hints must agree with the transaction
    let derived: Vec<crate::wire::OutpointJson> = legs.iter().map(|l| l.outpoint.to_json()).collect();
    if route.orders != derived {
        return Err(X402Error::payload(
            Diag::InvalidKaspaX402Payload,
            "payload.route.orders differs from the orders the transaction spends",
        ));
    }
    // payer spend
    let payer_spent: u128 = match pay_asset {
        PayAsset::Token(a) => {
            let f = flows
                .get(&a.covenant_id)
                .ok_or_else(|| X402Error::payload(Diag::PayAssetNotAccepted, "the payer spends none of the pay asset"))?;
            (f.bought - f.sold).max(0) as u128
        }
        PayAsset::Kas => payer_kas_in.saturating_sub(kas_change),
    };
    if payer_spent == 0 {
        return Err(X402Error::payload(Diag::PayAssetNotAccepted, "the payer spends none of the pay asset"));
    }
    let payer_spent = u64::try_from(payer_spent).map_err(|_| unsupported("the payer's spend exceeds the uint64 range"))?;

    // 7. the engine: every order covenant and every KCC-20 leader
    let fee = check_economics_and_scripts(ctx, tx, &entries)?;

    // 8. the verified result
    let txid = tx.id().as_bytes();
    let watched = &outs[mj];
    let orders: Vec<Outpoint> = legs.iter().map(|l| l.outpoint).collect();
    let mut ext = Map::new();
    ext.insert("binding".into(), json!(crate::wire::BINDING_EXACT));
    ext.insert("profile".into(), json!(env.profile.as_str()));
    ext.insert("paymentOutputIndex".into(), json!(mj));
    ext.insert("finality".into(), json!(effective_finality(env.finality, pol).as_str()));
    ext.insert("transactionEncoding".into(), json!(TX_ENCODING));
    if let Some(m) = &merchant {
        ext.insert("custody".into(), json!(m.allowed.custody.as_str()));
    }
    ext.insert(
        "route".into(),
        json!({
            "binding": BINDING_SWAP,
            "payAsset": route.pay_asset,
            "orders": orders.iter().map(Outpoint::to_json).collect::<Vec<_>>(),
            "payerSpent": payer_spent.to_string(),
        }),
    );
    let payer_key = *payer_keys.iter().next().expect("non-empty");
    let verified = Verified {
        kind: if merchant.is_some() { PaymentKind::SwapToToken } else { PaymentKind::SwapToKas },
        profile: env.profile,
        txid,
        tx: tx.clone(),
        entries: entries.clone(),
        payer_address: address_of(&p2pk_spk(&payer_key), env.network),
        amount: env.amount,
        payment_output_index: mj as u32,
        merchant_output: WatchedOutput {
            outpoint: Outpoint::new(txid, mj as u32),
            script_public_key: watched.script_public_key.clone(),
            amount: watched.value,
        },
        consumed: tx.inputs.iter().map(outpoint_of).collect(),
        order_inputs: orders.clone(),
        fee,
        finality: effective_finality(env.finality, pol),
        custody: merchant.as_ref().map(|m| m.allowed.custody),
        authorization_expires_at_ms: expires_ms,
        request_hash: env.request_hash,
        requirements_hash: env.requirements_hash,
        payment_identifier,
        response_extension: ext,
    };
    let facts = SwapFacts {
        pay_asset: route.pay_asset.clone(),
        orders,
        legs: legs.iter().map(|l| LegFact { outpoint: l.outpoint, kind: l.kind, units: l.amount as u128, token: l.token }).collect(),
        payer_spent,
        payer_keys: payer_keys.into_iter().collect(),
        merchant_output: mj as u32,
    };
    Ok((verified, facts))
}

/// Output index of the merchant's gain in a swap transaction, computed the way the verifier does it
/// (the first output at index >= `k` that pays `payToScriptPublicKey` (KAS) or the merchant token
/// script). Used by the payer SDK to know `paymentOutputIndex` before the digest is fixed.
pub fn merchant_output_index(tx: &Transaction, k: usize, merchant_spk: &ScriptPublicKey, want_value: Option<u64>) -> Option<usize> {
    tx.outputs
        .iter()
        .enumerate()
        .skip(k)
        .find(|(_, o)| &o.script_public_key == merchant_spk && want_value.is_none_or(|v| o.value == v))
        .map(|(j, _)| j)
}

/// Hex of a txid (helper for callers that log verified facts).
pub fn txid_hex(v: &Verified) -> String {
    hex(&v.txid)
}
