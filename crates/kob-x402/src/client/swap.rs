//! Payer / merchant SDK: swap-and-pay (`extra.route`, binding `kob-swap-v1`).
//!
//! Merchant: [`swap_requirements`] builds an offer entry (`standard-native` or `kcc20`) with `extra.route`.
//! A pay asset may be a token of either family (KCC-20 programs, KaspaCom's included, KRON); the merchant
//! token is a KCC-20 token.
//!
//! Payer: a [`Quote`] names the KOB orders (plain bids of the pay token, plain asks of the merchant token;
//! the order kinds follow the tokens' families, e.g. `KobBidKron` for a KRON pay token) and the base units to fill.
//! A KRON token is held by address presence, so `funds.funding` must hold a P2PK UTXO of the token owner. [`prepare_swap`] builds the transaction with `kob_protocol::build::{Batch, SwapRoute}`,
//! commits the x402 payload-commitment digest as a `KOB1` `X402` record (the digest needs the merchant
//! output index, so the transaction is laid out once with a placeholder digest and rebuilt with the real
//! one; the index is asserted identical), refuses borrow-enabled payer tokens (G9), enforces the payer's
//! `max_pay` and never leaves a tiny KAS change output behind (G4: folded into the fee within the fee
//! bound, with a warning, else the payment fails). [`PreparedSwap`] exposes the sign requests for a wallet
//! or signs locally; [`PreparedSwap::complete`] finalizes (`tighten_budgets`) and assembles the
//! `PAYMENT-SIGNATURE` object.
//!
//! A payment whose order was taken meanwhile fails with `order_conflict` (retryable, see
//! [`is_retryable_conflict`]): re-quote and call [`prepare_swap`] again, which signs anew. A signed
//! payment that must not be broadcast any more is killed with [`revoke_swap`] (self-spend of one input, G13).

use std::collections::BTreeMap;

use kaspa_addresses::Address;
use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use kaspa_txscript::pay_to_address_script;
use kob_protocol::build::{
    build_batch, build_send_tokens, route_batch, Batch, Leg, Payment, SendTokens, SwapRoute, TokenPayee, TokenRecipient, TokenRef,
};
use kob_protocol::payload::Record;
use kob_protocol::state::{AskState, BidState, TokenState};
use kob_protocol::tx::{
    masses, min_fee, sighash, spk_to_string, target_fee, BuiltTx, FeeMode, FeeOptions, FeeReport, InputSignature, KeyUtxo, OrderUtxo,
    SignRequest, TokenUtxo, TxJson, MIN_FEE_RATE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::canonical::requirements_hash;
use crate::chain::{Outpoint, Txid};
use crate::common::{address_of, is_p2pk, iso_from_ms, p2pk_key, payload_commit_digest, PayloadCommit};
use crate::error::{Diag, Result, X402Error};
use crate::policy::{AllowedToken, Policy};
use crate::safe_tx::{spk_to_hex, SafeTx};
use crate::swap::{merchant_output_index, merchant_token, parse_route_offer, LegKind, MerchantToken, SwapFacts, PAY_ASSET_KAS};
use crate::verify::{Verified, VerifyCtx};
use crate::wire::{
    hex, parse_hash32, Authorization, ExactPayload, Finality, Network, PaymentPayload, PaymentRequirements, Profile, RoutePayload,
    ASSET_KAS, AUTH_VERSION_PAYLOAD, BINDING_EXACT, BINDING_SWAP, PAYLOAD_EXACT_TX, SCHEME_EXACT, TX_ENCODING, X402_VERSION,
};

// ------------------------------------------------------------------------------------------ merchant

/// What the merchant receives.
#[derive(Clone, Copy, Debug)]
pub enum MerchantGain<'a> {
    /// KAS (`standard-native`): `amount` sompi at `payTo`.
    Kas,
    /// A KCC-20 token (`kcc20`): `amount` units in a token output of `carrier` sompi owned by `payTo`.
    Token { token: &'a AllowedToken, carrier: u64 },
}

/// One asset the payer may pay with.
#[derive(Clone, Copy, Debug)]
pub enum PayAssetSpec<'a> {
    /// KAS (only when the merchant receives a token).
    Kas,
    Token(&'a AllowedToken),
}

/// Inputs of [`swap_requirements`].
pub struct SwapOfferParams<'a> {
    pub network: Network,
    /// Sompi (KAS) or token base units.
    pub amount: u64,
    /// Merchant address (a Schnorr P2PK address for a token gain).
    pub pay_to: &'a str,
    pub max_timeout_seconds: u64,
    pub finality: Finality,
    pub gain: MerchantGain<'a>,
    pub pay_assets: Vec<PayAssetSpec<'a>>,
}

/// Builds a swap-and-pay offer entry: the exact requirement of the merchant-gain profile plus
/// `extra.route = { binding: "kob-swap-v1", critical: true, payAssets }`.
pub fn swap_requirements(p: &SwapOfferParams) -> Result<PaymentRequirements> {
    if p.amount == 0 || p.max_timeout_seconds == 0 || p.max_timeout_seconds > u32::MAX as u64 {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Amount, "amount and maxTimeoutSeconds must be positive"));
    }
    if p.pay_assets.is_empty() {
        return Err(X402Error::requirements(Diag::PayAssetNotAccepted, "a swap offer needs at least one pay asset"));
    }
    let addr = Address::try_from(p.pay_to)
        .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo is not a Kaspa address: {e}")))?;
    if addr.prefix != p.network.prefix() {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "payTo prefix does not match the network"));
    }
    let spk = pay_to_address_script(&addr);
    let mut extra = Map::new();
    extra.insert("binding".into(), json!(BINDING_EXACT));
    extra.insert("finality".into(), json!(p.finality.as_str()));
    extra.insert("transactionEncoding".into(), json!(TX_ENCODING));
    extra.insert("payToScriptPublicKey".into(), json!(spk_to_hex(&spk)));
    let asset = match p.gain {
        MerchantGain::Kas => {
            extra.insert("profile".into(), json!(Profile::StandardNative.as_str()));
            ASSET_KAS.to_string()
        }
        MerchantGain::Token { token, carrier } => {
            if !token.is_merchant_capable() {
                return Err(X402Error::requirements(
                    Diag::TokenNotAllowlisted,
                    "only a KCC-20 token can be the merchant gain of a kcc20 offer (a KRON token is a pay asset)",
                ));
            }
            let owner = p2pk_key(&spk).ok_or_else(|| {
                X402Error::requirements(Diag::TokenOwnerScheme, "payTo must be a Schnorr P2PK address for a token payment")
            })?;
            let amount = i64::try_from(p.amount)
                .map_err(|_| X402Error::requirements(Diag::InvalidKaspaX402Amount, "amount exceeds the token range"))?;
            if carrier == 0 {
                return Err(X402Error::requirements(Diag::CarrierMismatch, "the carrier must be positive"));
            }
            let tpl = kob_protocol::artifacts::template(token.program);
            let state = kob_protocol::state::Kcc20State::p2pk(amount, owner, token.extension_commitment);
            extra.insert("profile".into(), json!(Profile::Kcc20.as_str()));
            extra.insert(
                "token".into(),
                json!({
                    "family": "kcc20",
                    "templateHash": hex(&token.template_hash()),
                    "extensionCommitment": hex(&token.extension_commitment),
                    "custody": token.custody.as_str(),
                    "carrier": carrier.to_string(),
                    "tokenScriptPublicKey": spk_to_hex(&state.spk_with(tpl)),
                    "decimals": token.decimals,
                    "ticker": token.ticker,
                }),
            );
            hex(&token.covenant_id)
        }
    };
    let mut list = Vec::new();
    for a in &p.pay_assets {
        list.push(match a {
            PayAssetSpec::Kas => {
                if matches!(p.gain, MerchantGain::Kas) {
                    return Err(X402Error::requirements(Diag::PayAssetNotAccepted, "KAS cannot be the pay asset of a KAS offer"));
                }
                json!({ "asset": PAY_ASSET_KAS })
            }
            PayAssetSpec::Token(t) => {
                if hex(&t.covenant_id) == asset {
                    return Err(X402Error::requirements(Diag::PayAssetNotAccepted, "the pay asset equals the merchant asset"));
                }
                json!({
                    "asset": hex(&t.covenant_id),
                    "templateHash": hex(&t.template_hash()),
                    "extensionCommitment": hex(&t.extension_commitment),
                })
            }
        });
    }
    extra.insert("route".into(), json!({ "binding": BINDING_SWAP, "critical": true, "payAssets": list }));
    Ok(PaymentRequirements {
        scheme: SCHEME_EXACT.into(),
        network: p.network.as_str().into(),
        amount: p.amount.to_string(),
        asset,
        pay_to: p.pay_to.to_string(),
        max_timeout_seconds: p.max_timeout_seconds,
        extra,
        other: Map::new(),
    })
}

// ------------------------------------------------------------------------------------------ payer types

/// One order of a quote: a plain bid of the pay token or a plain ask of the merchant token, with the
/// UTXOs it needs (asks: their exact custody) and the base units to fill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRef {
    pub leg: Leg,
}

impl OrderRef {
    /// A bid the payer sells `amount` base units into.
    pub fn bid(order: OrderUtxo<BidState>, amount: i64) -> Self {
        OrderRef { leg: Leg::Bid { order, amount, t: None } }
    }
    /// An ask the payer buys `amount` base units from (`custody` holds exactly the ask's `amountLeft`).
    pub fn ask(order: OrderUtxo<AskState>, custody: TokenUtxo, amount: i64) -> Self {
        OrderRef { leg: Leg::Ask { order, custody, amount, t: None } }
    }
    pub fn kind(&self) -> Option<LegKind> {
        match self.leg {
            Leg::Bid { .. } => Some(LegKind::Bid),
            Leg::Ask { .. } => Some(LegKind::Ask),
            _ => None,
        }
    }
    /// The order UTXO's outpoint.
    pub fn outpoint(&self) -> Outpoint {
        let u = match &self.leg {
            Leg::Bid { order, .. } => &order.utxo,
            Leg::Ask { order, .. } => &order.utxo,
            Leg::CondAsk { order, .. } => &order.utxo,
            Leg::CondBid { order, .. } => &order.utxo,
            Leg::IfdBid { order, .. } => &order.utxo,
            Leg::IfdAsk { order, .. } => &order.utxo,
            Leg::Pair { order, .. } => &order.utxo,
            Leg::CondPair { order, .. } => &order.utxo,
            Leg::IfdPair { order, .. } => &order.utxo,
        };
        Outpoint::new(u.transaction_id, u.index)
    }
    /// Base units this leg fills.
    pub fn amount(&self) -> i64 {
        match &self.leg {
            Leg::Bid { amount, .. } | Leg::Ask { amount, .. } => *amount,
            _ => 0,
        }
    }
    /// Token covenant id the order trades.
    pub fn token(&self) -> Option<[u8; 32]> {
        match &self.leg {
            Leg::Bid { order, .. } => Some(order.state.token_cov_id),
            Leg::Ask { order, .. } => Some(order.state.token_cov_id),
            _ => None,
        }
    }
    /// Token base units this leg moves.
    pub fn units(&self) -> i128 {
        match &self.leg {
            Leg::Bid { amount, .. } | Leg::Ask { amount, .. } => *amount as i128,
            _ => 0,
        }
    }
}

/// A quote: the orders a swap uses and the DAA score the transaction proves (`lockTime`; use the
/// current virtual DAA score minus a small margin, it must not exceed the verifier's virtual DAA).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    #[serde(with = "kob_protocol::json::field")]
    pub lock_time: u64,
    pub orders: Vec<OrderRef>,
}

/// The payer's assets.
#[derive(Clone, Debug)]
pub struct PayerFunds {
    /// P2PK-owned UTXOs of the pay token (all of one token; empty when paying KAS).
    pub tokens: Vec<TokenUtxo>,
    /// P2PK KAS UTXOs (fees, carriers, or the payment itself when paying KAS).
    pub funding: Vec<KeyUtxo>,
    /// Key that receives the token / KAS change.
    pub change: [u8; 32],
}

/// The payer's bound on what a swap costs, qualified by the asset it is counted in: a bound never changes unit
/// with the pay asset the route happens to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayBound {
    /// `"KAS"` or the pay token's covenant id (64 hex).
    pub asset: String,
    /// Most units of `asset` the payment may take (sompi incl. the fee for KAS, base units for a token).
    pub amount: u64,
}

impl PayBound {
    /// A bound in sompi on a swap paid with KAS.
    pub fn kas(sompi: u64) -> Self {
        PayBound { asset: PAY_ASSET_KAS.to_string(), amount: sompi }
    }
    /// A bound in base units on a swap paid with the token `covenant_id`.
    pub fn token(covenant_id: &[u8; 32], units: u64) -> Self {
        PayBound { asset: hex(covenant_id), amount: units }
    }
    /// Refuses a route whose pay asset is not the one this bound counts.
    fn check_asset(&self, pay_asset: &str) -> Result<()> {
        if self.asset.eq_ignore_ascii_case(pay_asset) {
            Ok(())
        } else {
            Err(err(Diag::PayAssetNotAccepted, format!("the payer's bound counts {}, the swap pays with {pay_asset}", self.asset)))
        }
    }
}

/// Knobs of [`prepare_swap`].
#[derive(Clone, Debug)]
pub struct SwapOptions {
    /// Payer bound on what the swap costs, in units of the asset it names (token units; sompi incl. the fee
    /// when paying KAS). A route that pays with another asset is refused. `None` = unbounded (not recommended).
    pub max_pay: Option<PayBound>,
    /// Authorization lifetime; clamped to `maxTimeoutSeconds` (an explicit larger value is an error).
    pub expires_in_ms: u64,
    /// Fee rate (sompi per gram), at least the relay minimum.
    pub fee_rate: Option<u64>,
    /// [`FeeMode::Relay`] (default: the node's relay floor) or [`FeeMode::Priority`] (storage-inclusive).
    pub fee_mode: FeeMode,
    /// KAS carrier of the payer's own token change (default: the policy's minimum carrier).
    pub change_carrier: Option<u64>,
    /// KAS change below this is folded into the fee (default 0.1 KAS); see G4.
    pub min_change_sompi: u64,
    /// Fee bound (default: the policy's `max_fee_sompi`).
    pub max_fee_sompi: Option<u64>,
    /// Explicit `payment-identifier` id (default: a fresh random one, [`crate::client::native::random_payment_id`]).
    pub payment_identifier: Option<String>,
    /// Refuse an `expires_in_ms` beyond the offer's `maxTimeoutSeconds` (default true; the diagnostics
    /// suite turns it off to produce over-long authorizations).
    pub enforce_ttl: bool,
}

impl Default for SwapOptions {
    fn default() -> Self {
        SwapOptions {
            max_pay: None,
            expires_in_ms: 60_000,
            fee_rate: None,
            fee_mode: FeeMode::Relay,
            change_carrier: None,
            min_change_sompi: 10_000_000,
            max_fee_sompi: None,
            payment_identifier: None,
            enforce_ttl: true,
        }
    }
}

/// A built, not yet signed swap payment.
#[derive(Clone, Debug)]
pub struct PreparedSwap {
    pub built: BuiltTx,
    pub offer: PaymentRequirements,
    pub request_hash: String,
    /// `route.payAsset`.
    pub pay_asset: String,
    /// Order outpoints (the first `k` inputs).
    pub orders: Vec<Outpoint>,
    pub payment_output_index: u32,
    pub digest: [u8; 32],
    pub expires_at: String,
    /// Pay-asset units the payer gives up.
    pub payer_spent: u64,
    pub payer_address: Option<String>,
    pub warnings: Vec<String>,
    pub payment_identifier: Option<String>,
}

/// A signed swap payment ready for `PAYMENT-SIGNATURE`.
#[derive(Clone, Debug)]
pub struct SwapPayment {
    pub payload: PaymentPayload,
    pub tx: Transaction,
    pub entries: Vec<UtxoEntry>,
    pub txid: Txid,
    pub fee: u64,
    pub payer_spent: u64,
    pub warnings: Vec<String>,
}

fn err(diag: Diag, msg: impl Into<String>) -> X402Error {
    X402Error::payload(diag, msg)
}

/// True for the retryable race of a swap payment: an order was taken; re-quote and re-sign.
pub fn is_retryable_conflict(e: &X402Error) -> bool {
    e.diag == Diag::OrderConflict && e.retryable
}

/// The order outpoints an `order_conflict` names (empty for any other error).
pub fn conflicting_orders(e: &X402Error) -> Vec<Outpoint> {
    let Some(list) = e.details.as_ref().and_then(|d| d.get("orders")).and_then(Value::as_array) else { return vec![] };
    list.iter()
        .filter_map(|o| {
            let txid = parse_hash32(o.get("txid")?.as_str()?)?;
            Some(Outpoint::new(txid, u32::try_from(o.get("index")?.as_u64()?).ok()?))
        })
        .collect()
}

// ------------------------------------------------------------------------------------------ building

fn offer_facts(
    pol: &Policy,
    offer: &PaymentRequirements,
    request_hash: &str,
) -> Result<(crate::common::Envelope, Option<MerchantToken>)> {
    let network = offer.network()?;
    let profile = offer.profile()?;
    let amount = offer.amount_u64()?;
    let finality = offer.finality()?;
    let pay_to = Address::try_from(offer.pay_to.as_str())
        .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo: {e}")))?;
    let pay_to_spk = pay_to_address_script(&pay_to);
    if offer.extra_str("payToScriptPublicKey").map(str::to_ascii_lowercase) != Some(spk_to_hex(&pay_to_spk))
        || pay_to.prefix != network.prefix()
    {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "payToScriptPublicKey does not match payTo"));
    }
    let request_hash =
        parse_hash32(request_hash).ok_or_else(|| err(Diag::InvalidKaspaX402Payload, "requestHash is not 32-byte hex"))?;
    let env = crate::common::Envelope {
        network,
        amount,
        profile,
        finality,
        pay_to,
        pay_to_spk,
        requirements_hash: requirements_hash(offer)?,
        request_hash,
        max_timeout_seconds: offer.max_timeout_seconds,
    };
    let merchant = match profile {
        Profile::StandardNative => {
            if offer.asset != ASSET_KAS {
                return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "a standard-native offer pays KAS"));
            }
            None
        }
        Profile::Kcc20 => Some(merchant_token(pol, offer, &env)?),
        Profile::Additive => return Err(X402Error::requirements(Diag::UnsupportedKaspaExactProfile, "additive is not supported")),
    };
    Ok((env, merchant))
}

/// Checks a payer token UTXO: the pay token, of its family, key-owned, borrow disabled (G9; a KRON token
/// must not be a minter), pinned extension.
fn check_payer_token(t: &TokenUtxo, allowed: &AllowedToken) -> Result<()> {
    if t.utxo.covenant_id != Some(allowed.covenant_id) {
        return Err(err(Diag::PayAssetNotAccepted, "a payer token UTXO is not of the pay asset"));
    }
    if t.state.family() != allowed.family {
        return Err(err(
            Diag::PayAssetNotAccepted,
            format!("a payer token UTXO is not a {} token", allowed.family.as_str().to_ascii_uppercase()),
        ));
    }
    match &t.state {
        TokenState::Kcc20(s) if s.borrow_scheme != 0 || s.borrow_guard != [0; 32] => {
            return Err(err(
                Diag::TokenBorrowEnabled,
                "a payer token UTXO has borrowing enabled: a third-party borrow could invalidate the signed payment; consolidate it first",
            ));
        }
        TokenState::Kron(s) if s.is_minter != 0 => {
            return Err(err(Diag::TokenBorrowEnabled, "a payer token UTXO is a KRON minter UTXO"));
        }
        _ => {}
    }
    if !t.state.is_user() {
        return Err(err(Diag::TokenOwnerScheme, "a payer token UTXO is not key-owned"));
    }
    if t.state.extension() != allowed.extension_commitment {
        return Err(err(Diag::TokenNotAllowlisted, "a payer token UTXO carries another extension commitment"));
    }
    if t.state.amount() <= 0 {
        return Err(err(Diag::TokenConservation, "a payer token UTXO holds no units"));
    }
    Ok(())
}

/// Drops the last output (the payer's KAS change) and re-derives the sign requests: the change goes to
/// the fee. Only the sighash of each request changes.
fn fold_change_into_fee(built: &BuiltTx) -> Result<BuiltTx> {
    let (mut tx, entries) = built.tx.to_tx()?;
    let last = tx.outputs.len().checked_sub(1).ok_or_else(|| err(Diag::Internal, "no output to fold"))?;
    if built.fee.change_output != Some(last as u32) {
        return Err(err(Diag::Internal, "the change output is not the last output"));
    }
    tx.outputs.pop();
    tx.set_storage_mass(masses(&tx, &entries).storage);
    let sign: Vec<SignRequest> =
        built.sign.iter().map(|r| SignRequest { sighash: sighash(&tx, &entries, r.input_index), ..r.clone() }).collect();
    let total_in: u64 = entries.iter().map(|e| e.amount).sum();
    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
    let mass = masses(&tx, &entries);
    Ok(BuiltTx {
        tx: TxJson::from_tx(&tx, &entries),
        sign,
        fee: FeeReport {
            fee: total_in - total_out,
            min_fee: target_fee(&mass, built.fee.fee_rate, built.fee.fee_mode),
            mass,
            change_output: None,
            ..built.fee.clone()
        },
        ..built.clone()
    })
}

/// Builds a swap payment (unsigned). See the module docs.
pub fn prepare_swap(
    policy: &Policy,
    offer: &PaymentRequirements,
    quote: &Quote,
    request_hash: &str,
    funds: &PayerFunds,
    now_ms: u64,
    opts: &SwapOptions,
) -> Result<PreparedSwap> {
    if !policy.swap_enabled {
        return Err(err(Diag::RouteUnsupported, "swap-and-pay is disabled"));
    }
    let (env, merchant) = offer_facts(policy, offer, request_hash)?;
    let pay_offers = parse_route_offer(offer)?;
    let k = quote.orders.len();
    if k == 0 {
        return Err(err(Diag::RouteUnsupported, "the quote names no order"));
    }
    let mut bids = vec![];
    let mut asks = vec![];
    for o in &quote.orders {
        if o.amount() <= 0 {
            return Err(err(Diag::RouteUnsupported, "an order leg fills nothing"));
        }
        match o.kind() {
            Some(LegKind::Bid) => bids.push(o.clone()),
            Some(LegKind::Ask) => asks.push(o.clone()),
            None => return Err(err(Diag::RouteUnsupported, "conditional / if-done legs are not supported by swap-and-pay")),
        }
    }
    // pay asset: the bids' token, or KAS when the payer only buys the merchant token
    let pay_token: Option<&AllowedToken> = match bids.first().and_then(OrderRef::token) {
        Some(id) => {
            if bids.iter().any(|b| b.token() != Some(id)) {
                return Err(err(Diag::RouteUnsupported, "all bids of a swap sell the same pay token"));
            }
            Some(policy.tokens.find(&id).ok_or_else(|| err(Diag::PayAssetNotAccepted, "the pay token is not accepted"))?)
        }
        None => None,
    };
    let pay_asset = pay_token.map(|t| hex(&t.covenant_id)).unwrap_or_else(|| PAY_ASSET_KAS.to_string());
    if !pay_offers.iter().any(|e| e.asset == pay_asset) {
        return Err(err(Diag::PayAssetNotAccepted, "the offer does not accept this pay asset"));
    }
    if let Some(b) = &opts.max_pay {
        b.check_asset(&pay_asset)?;
    }
    let max_pay = opts.max_pay.as_ref().map(|b| b.amount);
    match (&merchant, pay_token) {
        (None, None) => return Err(err(Diag::PayAssetNotAccepted, "a KAS offer cannot be paid in KAS")),
        (Some(m), Some(t)) if m.id == t.covenant_id => {
            return Err(err(Diag::PayAssetNotAccepted, "the pay asset equals the merchant asset"))
        }
        (None, _) if !asks.is_empty() => return Err(err(Diag::RouteUnsupported, "a KAS offer is paid through bids only")),
        (Some(m), _) => {
            if asks.is_empty() {
                return Err(err(Diag::RouteUnsupported, "a token offer needs asks of the merchant token"));
            }
            if asks.iter().any(|a| a.token() != Some(m.id)) {
                return Err(err(Diag::RouteUnsupported, "every ask of a swap sells the merchant token"));
            }
        }
        _ => {}
    }
    // payer tokens (refuse borrow-enabled ones, G9), selected to cover the bids
    let needed: i128 = bids.iter().map(OrderRef::units).sum();
    let mut tokens: Vec<TokenUtxo> = vec![];
    match pay_token {
        Some(a) => {
            for t in &funds.tokens {
                check_payer_token(t, a)?;
            }
            let mut sorted = funds.tokens.clone();
            sorted.sort_by_key(|t| std::cmp::Reverse(t.state.amount()));
            let mut have = 0i128;
            for t in sorted {
                if have >= needed {
                    break;
                }
                have += t.state.amount() as i128;
                tokens.push(t);
            }
            if have < needed {
                return Err(err(
                    Diag::TokenConservation,
                    format!("the payer holds {have} units of the pay token, the swap needs {needed}"),
                ));
            }
            if let Some(max) = max_pay {
                if needed > max as i128 {
                    return Err(err(
                        Diag::Overpayment,
                        format!("the swap costs {needed} units of the pay token, above the payer's bound {max}"),
                    ));
                }
            }
        }
        None => {
            if !funds.tokens.is_empty() {
                return Err(err(Diag::PayAssetNotAccepted, "paying KAS: no payer tokens are spent"));
            }
        }
    }
    // authorization lifetime
    let ttl_cap = env.max_timeout_seconds.saturating_mul(1_000);
    if opts.enforce_ttl && opts.expires_in_ms > ttl_cap {
        return Err(err(Diag::AuthorizationExceedsMaxTimeout, "the requested authorization lifetime exceeds maxTimeoutSeconds"));
    }
    let expires_at = iso_from_ms(now_ms + opts.expires_in_ms);
    let fee = FeeOptions { fee_rate: opts.fee_rate, fee_mode: opts.fee_mode };
    let carrier_change = opts.change_carrier.unwrap_or(policy.limits.min_carrier_sompi);
    if carrier_change > policy.limits.max_carrier_sompi {
        return Err(err(Diag::CarrierMismatch, "the payer token change carrier exceeds the policy ceiling"));
    }
    let merchant_spk_str = spk_to_string(&env.pay_to_spk);
    let (merchant_spk, want_value) = match &merchant {
        None => (env.pay_to_spk.clone(), Some(env.amount)),
        Some(m) => (m.spk.clone(), None),
    };

    // one build of the batch with the given KAS funding; the builder spends EVERY funding coin it is given, so the coins are
    // selected below (a builder refusal stays a `kob_protocol::Error` so a funding shortfall can be told apart)
    let build_raw = |digest: &[u8; 32], funding: &[KeyUtxo]| -> Result<std::result::Result<BuiltTx, kob_protocol::Error>> {
        let records = vec![Record::X402 { reference: digest.to_vec() }];
        let legs_of = |v: &[OrderRef]| v.iter().map(|o| o.leg.clone()).collect::<Vec<_>>();
        let change = Some(funds.change);
        let batch: Batch = match (&merchant, bids.is_empty(), asks.is_empty()) {
            (None, false, true) => Batch {
                lock_time: quote.lock_time,
                legs: legs_of(&bids),
                updates: vec![],
                taker_tokens: tokens.clone(),
                taker: change,
                taker_token_carrier: carrier_change,
                keep_surplus: vec![],
                keep_carrier: None,
                receivers: vec![],
                payments: vec![Payment { script_public_key: merchant_spk_str.clone(), amount: env.amount }],
                funding: funding.to_vec(),
                change,
                records,
                fee: fee.clone(),
            },
            (Some(m), true, false) => Batch {
                lock_time: quote.lock_time,
                legs: legs_of(&asks),
                updates: vec![],
                taker_tokens: vec![],
                taker: change,
                taker_token_carrier: m.carrier,
                keep_surplus: vec![],
                keep_carrier: None,
                receivers: vec![TokenPayee { covenant_id: m.id, pubkey: m.owner }],
                payments: vec![],
                funding: funding.to_vec(),
                change,
                records,
                fee: fee.clone(),
            },
            (Some(m), false, false) => route_batch(&SwapRoute {
                lock_time: quote.lock_time,
                sell: legs_of(&bids),
                buy: legs_of(&asks),
                tokens: tokens.clone(),
                receiver: Some(m.owner),
                token_carrier: m.carrier,
                payments: vec![],
                funding: funding.to_vec(),
                change,
                records,
                fee: fee.clone(),
            })?,
            _ => return Err(err(Diag::RouteUnsupported, "unsupported combination of legs and merchant gain")),
        };
        Ok(build_batch(&batch, &kob_protocol::budget::lookup))
    };
    // KAS funding: the fewest of the payer's coins, largest first, that pay the fee and the carriers, within the verifier's
    // input bound (passing every coin made a payer with more coins than the bound unable to pay at all; TN10 soak 2026-10-06)
    let mut coins: Vec<KeyUtxo> = funds.funding.clone();
    coins.sort_by(|a, b| {
        b.utxo.amount.cmp(&a.utxo.amount).then((a.utxo.transaction_id, a.utxo.index).cmp(&(b.utxo.transaction_id, b.utxo.index)))
    });
    coins.dedup_by(|a, b| a.utxo.transaction_id == b.utxo.transaction_id && a.utxo.index == b.utxo.index);
    // at least one coin when the payer has any (a KRON pay token is held by address presence: its owner's coin rides along)
    let first = usize::from(!coins.is_empty());
    let mut chosen: Option<usize> = None;
    let mut short: Option<(u64, u64)> = None;
    for n in first..=coins.len().min(policy.limits.max_inputs) {
        match build_raw(&[0u8; 32], &coins[..n])? {
            Ok(_) => {
                chosen = Some(n);
                break;
            }
            Err(kob_protocol::Error::InsufficientFunds { need, have }) => short = Some((need, have)),
            Err(e) => return Err(e.into()),
        }
    }
    let Some(n) = chosen else {
        let (need, have) = short.unwrap_or((0, 0));
        return Err(err(
            Diag::InvalidKaspaExactTransaction,
            format!(
                "the payer's KAS coins do not pay the swap's fee and carriers within the verifier's {} inputs (need {need} sompi, have {have} sompi in the {} largest coins)",
                policy.limits.max_inputs,
                coins.len().min(policy.limits.max_inputs)
            ),
        ));
    };
    let funding: Vec<KeyUtxo> = coins[..n].to_vec();
    let build_with = |digest: &[u8; 32]| -> Result<BuiltTx> { Ok(build_raw(digest, &funding)??) };
    let index_of = |built: &BuiltTx| -> Result<usize> {
        let (tx, _) = built.tx.to_tx()?;
        merchant_output_index(&tx, k, &merchant_spk, want_value)
            .ok_or_else(|| err(Diag::Internal, "the built transaction has no merchant output"))
    };

    // pass 1 fixes the layout (the merchant output index), pass 2 commits the digest
    let layout = build_with(&[0u8; 32])?;
    let idx = index_of(&layout)?;
    let digest = payload_commit_digest(&PayloadCommit {
        network: env.network,
        profile: env.profile,
        route_pay_asset: Some(&pay_asset),
        asset: &offer.asset,
        amount: &offer.amount,
        pay_to: &offer.pay_to,
        pay_to_spk_hex: &spk_to_hex(&env.pay_to_spk),
        payment_output_index: idx as u32,
        requirements_hash: &env.requirements_hash,
        request_hash: &env.request_hash,
        expires_at: &expires_at,
    })?;
    let mut built = build_with(&digest)?;
    let idx2 = index_of(&built)?;
    debug_assert_eq!(idx, idx2, "the merchant output index moved between the layout pass and the committed build");
    if idx != idx2 {
        return Err(err(Diag::Internal, "the merchant output index moved between the layout pass and the committed build"));
    }

    // G4: no tiny KAS change output
    let mut warnings = vec![];
    if let Some(ci) = built.fee.change_output {
        let value = built.tx.outputs[ci as usize].value;
        if value < opts.min_change_sompi {
            let max_fee = opts.max_fee_sompi.unwrap_or(policy.limits.max_fee_sompi);
            if built.fee.fee.saturating_add(value) > max_fee {
                return Err(err(
                    Diag::InvalidKaspaExactFee,
                    format!(
                        "the KAS change of {value} sompi is below the {} sompi floor and folding it into the fee would exceed the fee bound {max_fee}",
                        opts.min_change_sompi
                    ),
                ));
            }
            warnings
                .push(format!("folded a KAS change of {value} sompi into the fee (below the {} sompi floor)", opts.min_change_sompi));
            built = fold_change_into_fee(&built)?;
        }
    }
    let max_fee = opts.max_fee_sompi.unwrap_or(policy.limits.max_fee_sompi);
    if built.fee.fee > max_fee {
        return Err(err(Diag::InvalidKaspaExactFee, format!("the fee {} exceeds the bound {max_fee}", built.fee.fee)));
    }
    if built.tx.outputs.len() > policy.limits.max_outputs || built.tx.inputs.len() > policy.limits.max_inputs {
        return Err(err(Diag::InvalidKaspaExactTransaction, "the transaction exceeds the verifier's input / output bounds"));
    }
    // what the payer gives up
    let payer_spent: u64 = match pay_token {
        Some(_) => u64::try_from(needed).map_err(|_| err(Diag::Overpayment, "the swap exceeds the uint64 range"))?,
        None => {
            let put: u64 = funding.iter().map(|f| f.utxo.amount).sum();
            let back: u64 = built.fee.change_output.map(|ci| built.tx.outputs[ci as usize].value).unwrap_or(0);
            let spent = put.saturating_sub(back);
            if let Some(max) = max_pay {
                if spent > max {
                    return Err(err(Diag::Overpayment, format!("the swap costs {spent} sompi, above the payer's bound {max}")));
                }
            }
            spent
        }
    };
    let orders: Vec<Outpoint> = built.tx.inputs.iter().take(k).map(|i| Outpoint::new(i.transaction_id, i.index)).collect();
    Ok(PreparedSwap {
        built,
        offer: offer.clone(),
        request_hash: request_hash.to_ascii_lowercase(),
        pay_asset,
        orders,
        payment_output_index: idx as u32,
        digest,
        expires_at,
        payer_spent,
        payer_address: address_of(&kob_protocol::script::p2pk_spk(&funds.change), env.network),
        warnings,
        payment_identifier: opts.payment_identifier.clone(),
    })
}

impl PreparedSwap {
    /// The signatures a wallet must produce (input index, key, SIGHASH_ALL digest, redeem script).
    pub fn sign_requests(&self) -> &[SignRequest] {
        &self.built.sign
    }

    /// Signs every request with local keys (x-only pubkey -> secret key).
    pub fn sign_with(&self, keys: &BTreeMap<[u8; 32], [u8; 32]>) -> Result<Vec<InputSignature>> {
        kob_protocol::tx::sign_locally(&self.built, keys).map_err(Into::into)
    }

    /// Finalizes with wallet signatures: assembles the signature scripts, commits the exact compute
    /// budgets (`tighten_budgets`), and builds the `PAYMENT-SIGNATURE` payload.
    pub fn complete(&self, signatures: &[InputSignature]) -> Result<SwapPayment> {
        let signed = kob_protocol::tx::finalize(&self.built, signatures, kob_protocol::tx::FinalizeOptions { tighten_budgets: true })?;
        let (tx, entries) = signed.tx.to_tx()?;
        let txid = tx.id().as_bytes();
        let k = self.orders.len();
        if (self.payment_output_index as usize) < k || self.payment_output_index as usize >= tx.outputs.len() {
            return Err(err(Diag::Internal, "the merchant output index is out of range"));
        }
        let payload = self.assemble_payload(&tx, &entries)?;
        Ok(SwapPayment {
            payload,
            tx,
            entries,
            txid,
            fee: signed.fee.fee,
            payer_spent: self.payer_spent,
            warnings: self.warnings.clone(),
        })
    }

    /// Wraps a signed transaction into the `PAYMENT-SIGNATURE` object (offer, authorization, route,
    /// payment identifier). No engine run; [`PreparedSwap::complete`] is the normal entry point.
    pub fn assemble_payload(&self, tx: &Transaction, entries: &[UtxoEntry]) -> Result<PaymentPayload> {
        let id = self.payment_identifier.clone().unwrap_or_else(crate::client::native::random_payment_id);
        let payload = PaymentPayload {
            x402_version: X402_VERSION,
            accepted: self.offer.clone(),
            payload: ExactPayload {
                kind: PAYLOAD_EXACT_TX.into(),
                profile: self.offer.profile()?.as_str().into(),
                payer_address: self.payer_address.clone(),
                transaction: SafeTx::from_consensus(tx, entries).to_text(),
                transaction_encoding: TX_ENCODING.into(),
                payment_output_index: self.payment_output_index,
                request_hash: self.request_hash.clone(),
                challenge_id: None,
                authorization: Authorization {
                    version: AUTH_VERSION_PAYLOAD.into(),
                    input_index: None,
                    expires_at: self.expires_at.clone(),
                    digest: hex(&self.digest),
                    signature: None,
                },
                route: Some(RoutePayload {
                    binding: BINDING_SWAP.into(),
                    pay_asset: self.pay_asset.clone(),
                    orders: self.orders.iter().map(Outpoint::to_json).collect(),
                    intent: None,
                }),
            },
            resource: None,
            extensions: Some(json!({ "payment-identifier": { "info": { "required": true, "id": id } } })),
        };
        Ok(payload)
    }
}

/// Builds, signs locally and assembles a swap payment (see [`prepare_swap`]).
#[allow(clippy::too_many_arguments)]
pub fn pay_swap(
    policy: &Policy,
    offer: &PaymentRequirements,
    quote: &Quote,
    request_hash: &str,
    secrets: &BTreeMap<[u8; 32], [u8; 32]>,
    funds: &PayerFunds,
    now_ms: u64,
    opts: &SwapOptions,
) -> Result<SwapPayment> {
    let prepared = prepare_swap(policy, offer, quote, request_hash, funds, now_ms, opts)?;
    let sigs = prepared.sign_with(secrets)?;
    prepared.complete(&sigs)
}

/// Payer-side preflight: runs the facilitator's verification logic against a chain view and checks the
/// payer's own bound. Returns the verified result and the payer-side facts.
pub fn preflight_swap(
    ctx: &VerifyCtx,
    offer: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
    max_pay: Option<&PayBound>,
) -> Result<(Verified, SwapFacts)> {
    let (v, facts) = crate::swap::verify_swap_facts(ctx, offer, payload, request_hash)?;
    if let Some(b) = max_pay {
        b.check_asset(&facts.pay_asset)?;
    }
    if max_pay.is_some_and(|b| facts.payer_spent > b.amount) {
        return Err(err(
            Diag::Overpayment,
            format!("the payment costs {} units of the pay asset, above the payer's bound", facts.payer_spent),
        ));
    }
    Ok((v, facts))
}

// ------------------------------------------------------------------------------------------ revoke

/// Kills a signed payment that must not be broadcast any more (G13): a transaction that spends one of
/// its payer-owned inputs back to the payer. Broadcast it (with a higher fee rate to win the race). A
/// payer P2PK KAS input is preferred (a plain KAS self-spend); otherwise a P2PK-owned token input is
/// sent back to its owner. `secrets` maps x-only keys to secret keys.
pub fn revoke_swap(
    policy: &Policy,
    payment: &SwapPayment,
    secrets: &BTreeMap<[u8; 32], [u8; 32]>,
    fee_rate: Option<u64>,
) -> Result<(Transaction, Outpoint)> {
    let keys: std::collections::BTreeSet<[u8; 32]> = secrets.keys().copied().collect();
    let (built, op) = prepare_revoke_swap(policy, payment, &keys, fee_rate)?;
    let sigs = kob_protocol::tx::sign_locally(&built, secrets)?;
    let signed = kob_protocol::tx::finalize(&built, &sigs, kob_protocol::tx::FinalizeOptions { tighten_budgets: true })?;
    let (rev, _) = signed.tx.to_tx()?;
    Ok((rev, op))
}

/// The unsigned revocation of [`revoke_swap`] for the payer keys `payer_keys` (a wallet signs `built.sign`, C5 X-10; then
/// `kob_protocol::tx::finalize`): the same choice of input, a payer P2PK KAS input first, else a P2PK-owned KCC-20 token
/// input sent back to its owner. Returns the transaction to sign and the payment input it spends.
pub fn prepare_revoke_swap(
    policy: &Policy,
    payment: &SwapPayment,
    payer_keys: &std::collections::BTreeSet<[u8; 32]>,
    fee_rate: Option<u64>,
) -> Result<(BuiltTx, Outpoint)> {
    let rate = fee_rate.unwrap_or(MIN_FEE_RATE * 2).max(MIN_FEE_RATE);
    let tx = &payment.tx;
    // 1. a payer P2PK KAS input
    for (i, e) in payment.entries.iter().enumerate() {
        let Some(key) = p2pk_key(&e.script_public_key).filter(|_| is_p2pk(&e.script_public_key) && e.covenant_id.is_none()) else {
            continue;
        };
        if !payer_keys.contains(&key) {
            continue;
        }
        let op = crate::common::outpoint_of(&tx.inputs[i]);
        let mut rev = Transaction::new(
            1,
            vec![kaspa_consensus_core::tx::TransactionInput::new_with_compute_budget(
                tx.inputs[i].previous_outpoint,
                vec![],
                0,
                kob_protocol::budget::lookup("p2pk").map_err(X402Error::from)?,
            )],
            vec![kaspa_consensus_core::tx::TransactionOutput {
                value: e.amount,
                script_public_key: e.script_public_key.clone(),
                covenant: None,
            }],
            0,
            kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let entries = vec![e.clone()];
        let dummy = kob_protocol::script::push_data(&[0u8; 65]);
        let mut fee = 0u64;
        for _ in 0..8 {
            if e.amount <= fee {
                return Err(err(Diag::InvalidKaspaExactFee, "the input is too small to pay the revocation fee"));
            }
            rev.outputs[0].value = e.amount - fee;
            rev.inputs[0].signature_script = dummy.clone();
            rev.set_storage_mass(masses(&rev, &entries).storage);
            let need = min_fee(&masses(&rev, &entries), rate);
            if need == fee {
                break;
            }
            fee = need;
        }
        rev.outputs[0].value = e.amount - fee;
        rev.inputs[0].signature_script = vec![];
        rev.set_storage_mass(masses(&rev, &entries).storage);
        let report = masses(&rev, &entries);
        let built = BuiltTx {
            tx: kob_protocol::tx::TxJson::from_tx(&rev, &entries),
            plans: vec![kob_protocol::tx::SigPlan::P2pk { pubkey: key }],
            roles: vec!["p2pk".into()],
            sign: vec![kob_protocol::tx::SignRequest {
                input_index: 0,
                pubkey: key,
                sighash_type: kob_protocol::script::SIGHASH_ALL,
                sighash: sighash(&rev, &entries, 0),
                redeem_script: None,
            }],
            fee: kob_protocol::tx::FeeReport {
                fee,
                min_fee: min_fee(&report, rate),
                fee_rate: rate,
                fee_mode: FeeMode::Relay,
                mass: report,
                change_output: None,
            },
            covenants: vec![],
        };
        return Ok((built, op));
    }
    // 2. a payer P2PK-owned token input, sent back to its owner
    for (i, e) in payment.entries.iter().enumerate() {
        let Some(id) = e.covenant_id.map(|h| h.as_bytes()) else { continue };
        let Some(allowed) = policy.tokens.find(&id) else { continue };
        let Some(redeem) = crate::swap::redeem_of(&tx.inputs[i].signature_script) else { continue };
        let tpl = kob_protocol::artifacts::token_template(allowed.program);
        let Ok(state) = TokenState::from_redeem_with(tpl, redeem) else { continue };
        // a KRON token is authorised by a P2PK input of its owner: step 1 covers every such payment
        if allowed.family != kob_protocol::registry::Family::Kcc20 || !state.is_user() || !payer_keys.contains(&state.owner()) {
            continue;
        }
        let op = crate::common::outpoint_of(&tx.inputs[i]);
        let utxo = kob_protocol::tx::Utxo {
            transaction_id: op.txid,
            index: op.index,
            amount: e.amount,
            block_daa_score: e.block_daa_score,
            covenant_id: Some(id),
        };
        let req = SendTokens {
            token: TokenRef { covenant_id: id, program: allowed.program },
            tokens: vec![TokenUtxo { utxo, state: state.clone() }],
            recipients: vec![TokenRecipient { pubkey: state.owner(), amount: state.amount(), carrier: e.amount - e.amount / 4 }],
            token_change: None,
            token_change_carrier: 0,
            funding: vec![],
            change: Some(state.owner()),
            records: vec![],
            fee: FeeOptions::rate(rate),
        };
        return Ok((build_send_tokens(&req, &kob_protocol::budget::lookup)?, op));
    }
    Err(err(Diag::Unauthorized, "no payer-owned input to revoke with (no payer key owns a P2PK or token input)"))
}
