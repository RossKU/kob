//! Payer / merchant SDK: intent-based swap-and-pay (`extra.route` binding `kob-intent-v1`).
//!
//! Merchant: [`intent_requirements`] builds an offer entry (`standard-native` or `kcc20`) whose
//! `extra.route` accepts router intents: `{ binding: "kob-intent-v1", critical: true, router, payAssets }`.
//!
//! Payer: [`prepare_intent`] builds the **creation** transaction (the payer's one signature): it locks the
//! payer's KAS (pay asset `KAS`, intent `KasToToken`) or token A (intent `TokenToKas` / `TokenSwap`) in the
//! router actor of the fill shape the payer expects, with the payer's worst case as the intent's terms
//! (`maxPay` + `maxExtra` sompi, or `maxSell` units), and commits the x402 digest in its payload. The
//! facilitator verifies it, broadcasts it and executes the intent (retrying on conflicts without the payer).
//! The payer keeps the [`IntentPayment`]: after the authorization expired without a settlement it cancels
//! the intent with [`cancel_intent`] (one more signature) and gets everything back.

use std::collections::BTreeMap;

use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use kob_protocol::build::{
    build_cancel_intent, build_create_intent, intent_budgets, CancelIntent, CreateIntent, INTENT_OUTPUT, LOCK_OUTPUT,
};
use kob_protocol::family::Family;
use kob_protocol::payload::Record;
use kob_protocol::router::{Actor, IntentKind, IntentState, ROUTER_ARTIFACT_ID};
use kob_protocol::state::TokenState;
use kob_protocol::tx::{BuiltTx, FeeMode, FeeOptions, InputSignature, KeyUtxo, SignRequest, TokenUtxo, Utxo};
use serde_json::{json, Value};

use crate::canonical::sha256;
use crate::chain::Txid;
use crate::client::swap::{swap_requirements, PayerFunds, SwapOfferParams};
use crate::common::{address_of, iso_from_ms, PayloadCommit};
use crate::error::{Diag, Result, X402Error};
use crate::intent::{intent_commit_digest, parse_intent_offer, router_capable, router_deliverable};
use crate::policy::{AllowedToken, Policy};
use crate::safe_tx::{spk_to_hex, SafeTx};
use crate::swap::{merchant_token, PAY_ASSET_KAS};
use crate::wire::{
    hex, parse_hash32, Authorization, ExactPayload, IntentTerms, PaymentPayload, PaymentRequirements, Profile, RoutePayload,
    AUTH_VERSION_PAYLOAD, BINDING_INTENT, PAYLOAD_EXACT_TX, TX_ENCODING, X402_VERSION,
};

fn err(diag: Diag, msg: impl Into<String>) -> X402Error {
    X402Error::payload(diag, msg)
}

// ------------------------------------------------------------------------------------------ merchant

/// Builds an intent offer entry: the exact requirement of the merchant-gain profile plus
/// `extra.route = { binding: "kob-intent-v1", critical: true, router, payAssets }`. Every pay token must be on a
/// program the router trades ([`router_capable`]: KCC-20 or KRON, not KaspaCom's while it is pending review) and
/// the merchant token a KCC-20 one ([`router_deliverable`]).
pub fn intent_requirements(p: &SwapOfferParams) -> Result<PaymentRequirements> {
    use crate::client::swap::{MerchantGain, PayAssetSpec};
    let refuse = |t: &AllowedToken| {
        X402Error::requirements(
            Diag::RouteUnsupported,
            format!("the router does not trade {} ({} program): offer it through kob-swap-v1", t.ticker, t.program.name()),
        )
    };
    if let MerchantGain::Token { token, .. } = p.gain {
        if !router_deliverable(token) {
            return Err(refuse(token));
        }
    }
    for a in &p.pay_assets {
        if let PayAssetSpec::Token(t) = a {
            if !router_capable(t) {
                return Err(refuse(t));
            }
        }
    }
    let mut r = swap_requirements(p)?;
    let route = r.extra.get_mut("route").and_then(Value::as_object_mut).expect("swap_requirements sets extra.route");
    route.insert("binding".into(), json!(BINDING_INTENT));
    route.insert("router".into(), json!(ROUTER_ARTIFACT_ID));
    Ok(r)
}

// ------------------------------------------------------------------------------------------ payer

/// The payer's terms and knobs of [`prepare_intent`].
#[derive(Clone, Debug)]
pub struct IntentOptions {
    /// Router actor (the fill shape the keeper must use). Default: the one-order resting shape of the
    /// intent kind (`KasToToken_buy`, `TokenToKas_sell`, `TokenSwap_swap`), the most likely to survive a
    /// conflicting fill. See [`actor_for`].
    pub actor: Option<String>,
    /// KasToToken: most sompi the asks may demand at their quotes (required).
    pub max_pay: Option<u64>,
    /// KasToToken: most sompi for the merchant's carrier, fillers and the network fee (default: the
    /// merchant carrier + 1 KAS).
    pub max_extra: Option<u64>,
    /// TokenToKas / TokenSwap: most units of the pay token sold (required).
    pub max_sell: Option<i64>,
    /// TokenToKas / TokenSwap: units locked (default `max_sell`, KRON `max_sell + 1`: a KRON intent locks more than
    /// it may sell, its change output holds at least one unit); the rest comes back at execution.
    pub lock_amount: Option<i64>,
    /// TokenToKas / TokenSwap: KAS of the intent UTXO, what the keeper may spend on fillers and the
    /// network fee besides the proceeds (default 1 KAS).
    pub keeper_value: u64,
    /// KAS carrier of the locked token UTXO (default: the policy's minimum carrier).
    pub lock_carrier: Option<u64>,
    /// Authorization lifetime = how long the facilitator may execute. Default: 5 minutes, or the offer's
    /// `maxTimeoutSeconds` when shorter; an explicit value above `maxTimeoutSeconds` is an error.
    pub expires_in_ms: Option<u64>,
    /// Fee rate of the creation (sompi per gram), e.g. the node's estimate of the dynamic fee policy (default: the
    /// relay floor).
    pub fee_rate: Option<u64>,
    pub fee_mode: FeeMode,
    /// Explicit `payment-identifier` id (default: derived from the request hash and the creation id).
    pub payment_identifier: Option<String>,
}

impl Default for IntentOptions {
    fn default() -> Self {
        IntentOptions {
            actor: None,
            max_pay: None,
            max_extra: None,
            max_sell: None,
            lock_amount: None,
            keeper_value: 100_000_000,
            lock_carrier: None,
            expires_in_ms: None,
            fee_rate: None,
            fee_mode: FeeMode::Relay,
            payment_identifier: None,
        }
    }
}

/// The actor of a fill shape: `asks` asks (the last resting or not) and `bids` bids, selling a token A of `a_family`
/// (KCC-20 for a KasToToken intent).
pub fn actor_for(
    kind: IntentKind,
    asks: usize,
    bids: usize,
    last_ask_rests: bool,
    last_bid_rests: bool,
    a_family: Family,
) -> Option<&'static Actor> {
    Actor::by_shape(&kob_protocol::router::Shape { kind, asks, bids, last_ask_rests, last_bid_rests, a_family })
}

/// A built, not yet signed intent creation.
#[derive(Clone, Debug)]
pub struct PreparedIntent {
    pub built: BuiltTx,
    pub offer: PaymentRequirements,
    pub request_hash: String,
    pub pay_asset: String,
    pub actor: String,
    pub state: IntentState,
    pub terms: IntentTerms,
    pub digest: [u8; 32],
    pub expires_at: String,
    pub payer_address: Option<String>,
    /// The payer's worst case: sompi (KasToToken: the intent value plus the creation fee) and token units
    /// (TokenToKas / TokenSwap: `max_sell`, plus the KAS of the intent and the creation fee).
    pub worst_kas: u64,
    pub worst_tokens: i64,
    pub payment_identifier: Option<String>,
}

/// A signed intent creation, ready for `PAYMENT-SIGNATURE`; keep it to cancel the intent later.
#[derive(Clone, Debug)]
pub struct IntentPayment {
    pub payload: PaymentPayload,
    pub tx: Transaction,
    pub entries: Vec<UtxoEntry>,
    /// The creation's id (the facilitator's settlement reports the execution's id).
    pub txid: Txid,
    pub actor: String,
    pub state: IntentState,
    /// The intent UTXO the creation makes.
    pub intent: Utxo,
    /// Token intents: the locked token UTXO.
    pub lock: Option<TokenUtxo>,
}

fn kind_of(merchant_token: bool, pay_kas: bool) -> Result<IntentKind> {
    match (merchant_token, pay_kas) {
        (true, true) => Ok(IntentKind::KasToToken),
        (false, false) => Ok(IntentKind::TokenToKas),
        (true, false) => Ok(IntentKind::TokenSwap),
        (false, true) => Err(err(Diag::PayAssetNotAccepted, "a KAS offer cannot be paid in KAS")),
    }
}

fn positive(v: Option<u64>, what: &str) -> Result<u64> {
    v.filter(|x| *x > 0 && *x <= i64::MAX as u64)
        .ok_or_else(|| err(Diag::InvalidKaspaX402Payload, format!("{what} is required (positive)")))
}

/// Builds an intent creation (unsigned). `pay_asset` is `"KAS"` or the pay token's covenant id (one of
/// the offer's `payAssets`).
pub fn prepare_intent(
    policy: &Policy,
    offer: &PaymentRequirements,
    pay_asset: &str,
    request_hash: &str,
    funds: &PayerFunds,
    now_ms: u64,
    opts: &IntentOptions,
) -> Result<PreparedIntent> {
    if !policy.swap_enabled {
        return Err(err(Diag::RouteUnsupported, "swap-and-pay is disabled"));
    }
    let pay_offers = parse_intent_offer(offer)?;
    if !pay_offers.iter().any(|e| e.asset == pay_asset) {
        return Err(err(Diag::PayAssetNotAccepted, "the offer does not accept this pay asset"));
    }
    let network = offer.network()?;
    let profile = offer.profile()?;
    let amount = offer.amount_u64()?;
    let rh = parse_hash32(request_hash).ok_or_else(|| err(Diag::InvalidKaspaX402Payload, "requestHash is not 32-byte hex"))?;
    let pay_to = kaspa_addresses::Address::try_from(offer.pay_to.as_str())
        .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo: {e}")))?;
    let pay_to_spk = kaspa_txscript::pay_to_address_script(&pay_to);
    let merchant_key = crate::common::p2pk_key(&pay_to_spk).ok_or_else(|| {
        X402Error::requirements(Diag::TokenOwnerScheme, "payTo must be a Schnorr P2PK address for an intent payment")
    })?;
    let env = crate::common::Envelope {
        network,
        amount,
        profile,
        finality: offer.finality()?,
        pay_to,
        pay_to_spk: pay_to_spk.clone(),
        requirements_hash: crate::canonical::requirements_hash(offer)?,
        request_hash: rh,
        max_timeout_seconds: offer.max_timeout_seconds,
    };
    let merchant = match profile {
        Profile::StandardNative => None,
        Profile::Kcc20 => Some(merchant_token(policy, offer, &env)?),
        Profile::Additive => return Err(X402Error::requirements(Diag::UnsupportedKaspaExactProfile, "additive is not supported")),
    };
    let pay_kas = pay_asset == PAY_ASSET_KAS;
    let kind = kind_of(merchant.is_some(), pay_kas)?;
    let pay_token: Option<&AllowedToken> = if pay_kas {
        None
    } else {
        let id = parse_hash32(pay_asset).ok_or_else(|| err(Diag::PayAssetNotAccepted, "pay asset is not a covenant id"))?;
        let t = policy.tokens.find(&id).ok_or_else(|| err(Diag::PayAssetNotAccepted, "the pay token is not accepted"))?;
        if !router_capable(t) {
            return Err(err(Diag::RouteUnsupported, format!("the router does not trade the {} program", t.program.name())));
        }
        Some(t)
    };
    if merchant.as_ref().is_some_and(|m| !router_deliverable(&m.allowed)) {
        return Err(err(Diag::RouteUnsupported, "the router does not deliver the merchant token (KCC-20 programs only)"));
    }
    let a_family = pay_token.map(|t| t.program.family()).unwrap_or(Family::Kcc20);
    let actor = match &opts.actor {
        Some(a) => Actor::by_name(a).ok_or_else(|| err(Diag::RouteUnsupported, format!("{a} is not a router actor")))?,
        None => match kind {
            IntentKind::KasToToken => actor_for(kind, 1, 0, true, false, a_family),
            IntentKind::TokenToKas => actor_for(kind, 0, 1, false, true, a_family),
            IntentKind::TokenSwap => actor_for(kind, 1, 1, true, true, a_family),
        }
        .expect("default shapes exist"),
    };
    if actor.shape.kind != kind {
        return Err(err(Diag::RouteUnsupported, format!("{} is not a {} intent", actor.name, kind.as_str())));
    }
    let payer = funds.change;
    let amount_i = i64::try_from(amount).map_err(|_| err(Diag::InvalidKaspaX402Amount, "amount exceeds the token range"))?;
    let lock_carrier = opts.lock_carrier.unwrap_or(policy.limits.min_carrier_sompi);
    let ttl_cap = env.max_timeout_seconds.saturating_mul(1_000);
    let lifetime = opts.expires_in_ms.unwrap_or(300_000.min(ttl_cap));
    if lifetime == 0 || lifetime > ttl_cap {
        return Err(err(Diag::AuthorizationExceedsMaxTimeout, "the intent lifetime must be within maxTimeoutSeconds"));
    }
    // The authorization expires at the intent's deadline: from it on anyone may expire the intent (the router returns
    // the payer's KAS and tokens); the verifier recomputes the deadline from `expiresAt`.
    let deadline_ms = now_ms + lifetime;
    let deadline = i64::try_from(deadline_ms).map_err(|_| err(Diag::InvalidAuthorization, "the deadline is out of range"))?;
    let (state, terms, value, lock_amount) = match kind {
        IntentKind::KasToToken => {
            let m = merchant.as_ref().expect("token gain");
            let max_pay = positive(opts.max_pay, "maxPay")?;
            let max_extra = opts.max_extra.unwrap_or(m.carrier + 100_000_000);
            let st = IntentState::KasToToken {
                payer,
                merchant: merchant_key,
                token: m.id,
                program: m.allowed.program,
                amount: amount_i,
                max_pay: max_pay as i64,
                max_extra: max_extra as i64,
                b_extension: m.allowed.extension_commitment,
                deadline,
            };
            let terms = IntentTerms {
                actor: actor.name.into(),
                payer: hex(&payer),
                max_pay: Some(max_pay.to_string()),
                max_extra: Some(max_extra.to_string()),
                max_sell: None,
                lock_output_index: None,
                lock_amount: None,
            };
            (
                st,
                terms,
                max_pay.checked_add(max_extra).ok_or_else(|| err(Diag::InvalidKaspaX402Amount, "maxPay + maxExtra overflows"))?,
                0,
            )
        }
        IntentKind::TokenToKas | IntentKind::TokenSwap => {
            let t = pay_token.expect("token pay asset");
            let max_sell = positive(opts.max_sell.map(|x| x.max(0) as u64), "maxSell")? as i64;
            let kron = t.program.family() == Family::Kron;
            let lock = opts.lock_amount.unwrap_or(if kron { max_sell.saturating_add(1) } else { max_sell });
            if lock < max_sell {
                return Err(err(Diag::InvalidKaspaX402Payload, "lockAmount is below maxSell"));
            }
            if kron && lock <= max_sell {
                return Err(err(Diag::InvalidKaspaX402Payload, "a KRON intent locks more than maxSell (its change holds a unit)"));
            }
            let st = if kind == IntentKind::TokenToKas {
                IntentState::TokenToKas {
                    payer,
                    merchant: merchant_key,
                    token: t.covenant_id,
                    program: t.program,
                    merchant_kas: amount_i,
                    max_sell,
                    lock_amount: lock,
                    lock_extension: t.extension_commitment,
                    deadline,
                }
            } else {
                IntentState::TokenSwap {
                    payer,
                    merchant: merchant_key,
                    token_a: t.covenant_id,
                    program_a: t.program,
                    token_b: merchant.as_ref().expect("token gain").id,
                    program_b: merchant.as_ref().expect("token gain").allowed.program,
                    max_sell_a: max_sell,
                    amount_b: amount_i,
                    lock_amount: lock,
                    lock_extension: t.extension_commitment,
                    b_extension: merchant.as_ref().expect("token gain").allowed.extension_commitment,
                    deadline,
                }
            };
            let terms = IntentTerms {
                actor: actor.name.into(),
                payer: hex(&payer),
                max_pay: None,
                max_extra: None,
                max_sell: Some(max_sell.to_string()),
                lock_output_index: Some(LOCK_OUTPUT),
                lock_amount: (lock != max_sell).then(|| lock.to_string()),
            };
            (st, terms, opts.keeper_value, lock)
        }
    };
    state.check().map_err(|e| err(Diag::InvalidKaspaX402Payload, e.to_string()))?;
    state.check_actor(actor).map_err(|e| err(Diag::RouteUnsupported, e.to_string()))?;
    let expires_at = iso_from_ms(deadline_ms);
    let digest = intent_commit_digest(
        &PayloadCommit {
            network,
            profile,
            route_pay_asset: Some(pay_asset),
            asset: &offer.asset,
            amount: &offer.amount,
            pay_to: &offer.pay_to,
            pay_to_spk_hex: &spk_to_hex(&pay_to_spk),
            payment_output_index: INTENT_OUTPUT,
            requirements_hash: &env.requirements_hash,
            request_hash: &rh,
            expires_at: &expires_at,
        },
        actor.name,
    )?;
    // tokens: the pay token's key-owned, borrow-disabled UTXOs, largest first, enough for the lock
    let mut tokens = vec![];
    if let Some(t) = pay_token {
        let mut sorted: Vec<&TokenUtxo> = funds
            .tokens
            .iter()
            .filter(|x| {
                x.utxo.covenant_id == Some(t.covenant_id)
                    && x.state.is_user()
                    && x.state.is_plain()
                    && x.state.extension() == t.extension_commitment
            })
            .collect();
        sorted.sort_by_key(|x| std::cmp::Reverse(x.state.amount()));
        let mut have = 0i64;
        for x in sorted {
            if have >= lock_amount {
                break;
            }
            have += x.state.amount();
            tokens.push(x.clone());
        }
        if have < lock_amount {
            return Err(err(
                Diag::TokenConservation,
                format!("the payer holds {have} units of the pay token, the intent locks {lock_amount}"),
            ));
        }
    } else if !funds.tokens.is_empty() {
        return Err(err(Diag::PayAssetNotAccepted, "paying KAS: no payer tokens are spent"));
    }
    let req_with = |funding: Vec<KeyUtxo>| CreateIntent {
        actor: actor.name.into(),
        state: state.clone(),
        value,
        tokens: tokens.clone(),
        lock_amount,
        lock_carrier,
        token_change_carrier: lock_carrier,
        funding,
        change: Some(funds.change),
        records: vec![Record::X402 { reference: digest.to_vec() }],
        fee: FeeOptions { fee_rate: opts.fee_rate, fee_mode: opts.fee_mode },
    };
    // KAS funding: the fewest of the payer's coins, largest first, that pay the value, the carriers and the fee, within the
    // verifier's input bound (the builder spends every funding coin it is given; a payer with more coins than the bound could
    // not create an intent at all: TN10 soak 2026-10-06, "input count must be 1..=32")
    let mut coins: Vec<KeyUtxo> = funds.funding.clone();
    coins.sort_by(|a, b| {
        b.utxo.amount.cmp(&a.utxo.amount).then((a.utxo.transaction_id, a.utxo.index).cmp(&(b.utxo.transaction_id, b.utxo.index)))
    });
    coins.dedup_by(|a, b| a.utxo.transaction_id == b.utxo.transaction_id && a.utxo.index == b.utxo.index);
    let mut built = None;
    let mut short: Option<(u64, u64)> = None;
    for n in usize::from(!coins.is_empty())..=coins.len().min(policy.limits.max_inputs) {
        match build_create_intent(&req_with(coins[..n].to_vec()), &intent_budgets) {
            Ok(b) => {
                built = Some(b);
                break;
            }
            Err(kob_protocol::Error::InsufficientFunds { need, have }) => short = Some((need, have)),
            Err(e) => return Err(e.into()),
        }
    }
    let Some(built) = built else {
        let (need, have) = short.unwrap_or((0, 0));
        return Err(err(
            Diag::InvalidKaspaExactTransaction,
            format!(
                "the payer's KAS coins do not pay the intent's value, carriers and fee within the verifier's {} inputs (need {need} sompi, have {have} sompi in the {} largest coins)",
                policy.limits.max_inputs,
                coins.len().min(policy.limits.max_inputs)
            ),
        ));
    };
    if built.fee.fee > policy.limits.max_fee_sompi {
        return Err(err(Diag::InvalidKaspaExactFee, format!("the creation fee {} exceeds the bound", built.fee.fee)));
    }
    let worst_kas = value + built.fee.fee + if kind.locks_tokens() { lock_carrier } else { 0 };
    Ok(PreparedIntent {
        built,
        offer: offer.clone(),
        request_hash: request_hash.to_ascii_lowercase(),
        pay_asset: pay_asset.to_string(),
        actor: actor.name.into(),
        state,
        terms,
        digest,
        expires_at,
        payer_address: address_of(&kob_protocol::script::p2pk_spk(&funds.change), network),
        worst_kas,
        worst_tokens: if kind.locks_tokens() { lock_amount } else { 0 },
        payment_identifier: opts.payment_identifier.clone(),
    })
}

impl PreparedIntent {
    /// The signatures a wallet must produce (the payer's only signatures).
    pub fn sign_requests(&self) -> &[SignRequest] {
        &self.built.sign
    }

    /// Signs every request with local keys (x-only pubkey -> secret key).
    pub fn sign_with(&self, keys: &BTreeMap<[u8; 32], [u8; 32]>) -> Result<Vec<InputSignature>> {
        kob_protocol::tx::sign_locally(&self.built, keys).map_err(Into::into)
    }

    /// Finalizes with the wallet signatures and builds the `PAYMENT-SIGNATURE` payload. The creation is
    /// NOT broadcast by the payer: the facilitator does it (a creation the payer broadcasts itself is
    /// refused, because anyone holding the offer could then present it first).
    pub fn complete(&self, signatures: &[InputSignature]) -> Result<IntentPayment> {
        let signed = kob_protocol::tx::finalize(&self.built, signatures, kob_protocol::tx::FinalizeOptions { tighten_budgets: true })?;
        let (tx, entries) = signed.tx.to_tx()?;
        let txid = tx.id().as_bytes();
        let id = self.payment_identifier.clone().unwrap_or_else(|| {
            let mut pre = b"kob-x402-intent-payment-id-v1".to_vec();
            pre.extend_from_slice(self.request_hash.as_bytes());
            pre.extend_from_slice(&txid);
            hex(&sha256(&pre))
        });
        let intent_id = self
            .built
            .covenants
            .first()
            .map(|c| c.covenant_id)
            .ok_or_else(|| err(Diag::Internal, "the creation makes no covenant"))?;
        let intent = Utxo {
            transaction_id: txid,
            index: INTENT_OUTPUT,
            amount: tx.outputs[INTENT_OUTPUT as usize].value,
            block_daa_score: 0,
            covenant_id: Some(intent_id),
        };
        let lock = self.state.locked_token().zip(self.state.locked_program()).map(|(tok, program)| {
            let amount =
                self.terms.lock_amount.as_deref().and_then(|s| s.parse::<i64>().ok()).unwrap_or(self.state.max_sell().unwrap_or(0));
            let ext = self.lock_extension();
            TokenUtxo {
                utxo: Utxo {
                    transaction_id: txid,
                    index: LOCK_OUTPUT,
                    amount: tx.outputs[LOCK_OUTPUT as usize].value,
                    block_daa_score: 0,
                    covenant_id: Some(tok),
                },
                state: TokenState::custody(program.family(), amount, intent_id, ext),
            }
        });
        let payload = PaymentPayload {
            x402_version: X402_VERSION,
            accepted: self.offer.clone(),
            payload: ExactPayload {
                kind: PAYLOAD_EXACT_TX.into(),
                profile: self.offer.profile()?.as_str().into(),
                payer_address: self.payer_address.clone(),
                transaction: SafeTx::from_consensus(&tx, &entries).to_text(),
                transaction_encoding: TX_ENCODING.into(),
                payment_output_index: INTENT_OUTPUT,
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
                    binding: BINDING_INTENT.into(),
                    pay_asset: self.pay_asset.clone(),
                    orders: vec![],
                    intent: Some(self.terms.clone()),
                }),
            },
            resource: None,
            extensions: Some(json!({ "payment-identifier": { "info": { "required": true, "id": id } } })),
        };
        Ok(IntentPayment { payload, tx, entries, txid, actor: self.actor.clone(), state: self.state.clone(), intent, lock })
    }

    /// The extension commitment of the locked tokens (the payer's token inputs share it).
    fn lock_extension(&self) -> [u8; 32] {
        self.built
            .plans
            .iter()
            .find_map(|p| match p {
                kob_protocol::tx::SigPlan::TokenLeader { state, .. } => Some(state.extension_commitment),
                _ => None,
            })
            .unwrap_or([0; 32])
    }
}

/// Builds, signs locally and assembles an intent payment (see [`prepare_intent`]).
#[allow(clippy::too_many_arguments)]
pub fn pay_intent(
    policy: &Policy,
    offer: &PaymentRequirements,
    pay_asset: &str,
    request_hash: &str,
    secrets: &BTreeMap<[u8; 32], [u8; 32]>,
    funds: &PayerFunds,
    now_ms: u64,
    opts: &IntentOptions,
) -> Result<IntentPayment> {
    let prepared = prepare_intent(policy, offer, pay_asset, request_hash, funds, now_ms, opts)?;
    let sigs = prepared.sign_with(secrets)?;
    prepared.complete(&sigs)
}

/// The payer's cancel of an intent that was not executed (unsigned: the payer signs the intent input,
/// SIGHASH_ALL). Everything returns to the payer: the intent's KAS and, for a token intent, the locked
/// tokens (with their carrier). Broadcast it once the authorization expired without a settlement; before
/// that it races the facilitator's execution (either may win, never both). `fee_rate`: sompi per gram (default
/// the relay floor); the fee comes out of the intent's own KAS.
pub fn cancel_intent(payment: &IntentPayment, fee_rate: Option<u64>) -> Result<BuiltTx> {
    let r = CancelIntent {
        actor: payment.actor.clone(),
        state: payment.state.clone(),
        intent: payment.intent.clone(),
        lock: payment.lock.clone(),
        to: None,
        fee: FeeOptions { fee_rate, fee_mode: FeeMode::Relay },
    };
    Ok(build_cancel_intent(&r, &intent_budgets)?)
}

/// True when the payload is an intent payment (`route.binding` = `kob-intent-v1`).
pub fn is_intent_payload(p: &PaymentPayload) -> bool {
    p.payload.route.as_ref().is_some_and(|r| r.binding == BINDING_INTENT)
}
