//! Payer / merchant SDK: `kcc20` token payments.
//!
//! Merchant side: [`kcc20_requirements`] builds the `accepts` entry for an allowlisted token.
//!
//! Payer side:
//!
//! * [`pay_kcc20`] / [`pay_kcc20_with`]: select the token inputs, build the payment with
//!   [`kob_protocol::build::SendTokens`], commit the payload-commitment digest in the transaction
//!   payload BEFORE signing (the merchant output index is known: `SendTokens` lays recipients first),
//!   sign locally, finalize with tightened budgets and return the `PaymentPayload`;
//! * [`build_kcc20_unsigned`] + [`finish_kcc20`]: the same in two steps for wallets that hold the keys
//!   (the TS SDK): the wallet signs the `sign` requests of the returned [`BuiltTx`] and returns
//!   signatures only;
//! * [`preflight_kcc20`]: runs the verifier's own logic against a [`ChainView`] before the payload is
//!   disclosed to anyone;
//! * [`revoke_kcc20`]: self-spend of one input of an issued payment, so the signed payment can never
//!   confirm (the payer's TTL enforcement: the signed payload cannot be un-signed, only made unspendable).
//!
//! The payer never spends borrow-enabled or non-P2PK-owned token UTXOs (a borrow or a covenant owner
//! can invalidate or redirect a signed transaction), and prefers the fewest token inputs that cover the
//! amount within the program's slot limit. Funding inputs are added only when the token carriers cannot
//! pay the fee (the fee of a token payment is normally covered by the carrier slack).

use std::collections::BTreeMap;

use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutput, UtxoEntry};
use kaspa_txscript::pay_to_address_script;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use kob_protocol::artifacts::template;
use kob_protocol::build::{build, Action, SendTokens, TokenRecipient, TokenRef};
use kob_protocol::payload::Record;
use kob_protocol::script::{p2pk_spk, push_data};
use kob_protocol::state::SCHEME_P2PK;
use kob_protocol::tx::{
    finalize, masses, min_fee, sighash, sign_digest, sign_locally, BuiltTx, FeeMode, FeeOptions, FeeReport, FinalizeOptions,
    InputSignature, KeyUtxo, SignedTx, TokenUtxo, TxJson, MIN_FEE_RATE,
};
use kob_protocol::Error as ProtocolError;

use crate::canonical::requirements_hash;
use crate::chain::{ChainView, Clock};
use crate::common::{check_payload_commitment, iso_from_ms, payload_commit_digest, PayloadCommit};
use crate::error::{Diag, Result, X402Error};
use crate::policy::{AllowedToken, Limits, Policy};
use crate::safe_tx::{spk_to_hex, SafeTx};
use crate::token::{parse_offer, verify_kcc20, TokenOffer, TOKEN_FAMILY};
use crate::verify::{Verified, VerifyCtx};
use crate::wire::{
    hex, parse_hash32, Authorization, ExactPayload, Finality, Network, PaymentPayload, PaymentRequirements, Profile,
    AUTH_VERSION_PAYLOAD, BINDING_EXACT, PAYLOAD_EXACT_TX, SCHEME_EXACT, TX_ENCODING, X402_VERSION,
};

// ------------------------------------------------------------------------------------------ merchant

/// Builds the `accepts` entry for `amount` units of `token`, paid to the merchant's Schnorr P2PK
/// address `pay_to_address`, with the merchant token output carrying exactly `carrier` sompi.
pub fn kcc20_requirements(
    network: Network,
    token: &AllowedToken,
    amount: u64,
    pay_to_address: &str,
    carrier: u64,
    max_timeout_seconds: u64,
    finality: Finality,
) -> Result<PaymentRequirements> {
    if !token.is_merchant_capable() {
        return Err(X402Error::requirements(
            Diag::TokenNotAllowlisted,
            "the kcc20 profile takes KCC-20 tokens only (a KRON token is a swap-and-pay pay asset)",
        ));
    }
    if amount == 0 || amount > i64::MAX as u64 {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Amount, "amount must be within 1..=2^63-1 token units"));
    }
    if carrier == 0 {
        return Err(X402Error::requirements(Diag::CarrierMismatch, "carrier must be positive"));
    }
    if finality < Finality::Accepted {
        return Err(X402Error::requirements(Diag::InvalidKaspaExactFinality, "finality must be accepted or confirmed"));
    }
    let addr = kaspa_addresses::Address::try_from(pay_to_address)
        .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo is not a Kaspa address: {e}")))?;
    if addr.prefix != network.prefix() {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "payTo prefix does not match the network"));
    }
    let owner = crate::token::p2pk_owner_of_address(&addr)?;
    let tpl = template(token.program);
    let state = kob_protocol::state::Kcc20State::p2pk(amount as i64, owner, token.extension_commitment);
    let mut tok = Map::new();
    tok.insert("family".into(), json!(TOKEN_FAMILY));
    tok.insert("templateHash".into(), json!(hex(&tpl.hash)));
    tok.insert("extensionCommitment".into(), json!(hex(&token.extension_commitment)));
    tok.insert("custody".into(), json!(token.custody.as_str()));
    tok.insert("carrier".into(), json!(carrier.to_string()));
    tok.insert("tokenScriptPublicKey".into(), json!(spk_to_hex(&state.spk_with(tpl))));
    tok.insert("decimals".into(), json!(token.decimals));
    tok.insert("ticker".into(), json!(token.ticker));
    let mut extra = Map::new();
    extra.insert("binding".into(), json!(BINDING_EXACT));
    extra.insert("profile".into(), json!(Profile::Kcc20.as_str()));
    extra.insert("finality".into(), json!(finality.as_str()));
    extra.insert("transactionEncoding".into(), json!(TX_ENCODING));
    extra.insert("payToScriptPublicKey".into(), json!(spk_to_hex(&pay_to_address_script(&addr))));
    extra.insert("token".into(), Value::Object(tok));
    let req = PaymentRequirements {
        scheme: SCHEME_EXACT.into(),
        network: network.as_str().into(),
        amount: amount.to_string(),
        asset: hex(&token.covenant_id),
        pay_to: addr.to_string(),
        max_timeout_seconds,
        extra,
        other: Map::new(),
    };
    // self-check: the requirement must parse back to exactly what was asked for
    let facts = parse_offer(&req)?;
    debug_assert_eq!((facts.amount, facts.carrier, facts.program), (amount, carrier, token.program));
    Ok(req)
}

// ------------------------------------------------------------------------------------------ payer

/// Payer options.
#[derive(Clone, Debug)]
pub struct Kcc20Options {
    /// Sompi per gram (default and floor: the relay minimum).
    pub fee_rate: Option<u64>,
    /// [`FeeMode::Relay`] (default: the node's relay floor) or [`FeeMode::Priority`] (storage-inclusive).
    pub fee_mode: FeeMode,
    /// KAS value of the payer's token change output (default: the offer's carrier).
    pub token_change_carrier: Option<u64>,
    /// Authorization lifetime in seconds (default and maximum useful value: `maxTimeoutSeconds`).
    pub ttl_seconds: Option<u64>,
    /// `payment-identifier` extension id (`^[A-Za-z0-9_-]{16,128}$`); required by the default policy.
    pub payment_id: Option<String>,
    /// Refuse a payment whose fee exceeds this many sompi (payer-side bound).
    pub max_fee_sompi: u64,
    /// Refuse an offer whose merchant carrier (KAS the payer funds into the merchant token output), or a payer token
    /// change carrier, exceeds this many sompi (default [`Limits::default`]'s `max_carrier_sompi`).
    pub max_carrier_sompi: u64,
}

impl Default for Kcc20Options {
    fn default() -> Self {
        Kcc20Options {
            fee_rate: None,
            fee_mode: FeeMode::Relay,
            token_change_carrier: None,
            ttl_seconds: None,
            payment_id: None,
            max_fee_sompi: 25_000_000,
            max_carrier_sompi: Limits::default().max_carrier_sompi,
        }
    }
}

/// Everything of the `PaymentPayload` except the signed transaction: what a wallet flow carries from
/// [`build_kcc20_unsigned`] to [`finish_kcc20`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadTemplate {
    pub accepted: PaymentRequirements,
    pub request_hash: String,
    pub payment_output_index: u32,
    pub authorization: Authorization,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
}

fn protocol_err(e: ProtocolError) -> X402Error {
    X402Error::payload(Diag::InvalidKaspaExactTransaction, e.to_string())
}

/// Chooses the token inputs: eligible UTXOs only (this token, this extension, P2PK owner, borrowing
/// disabled, positive amount, owner key held), the fewest that cover the amount, within the slots.
fn select_tokens(
    offer: &TokenOffer,
    utxos: Vec<TokenUtxo>,
    slots: usize,
    have_key: &dyn Fn(&[u8; 32]) -> bool,
) -> Result<Vec<TokenUtxo>> {
    let need = offer.amount as i64;
    let (mut borrow, mut scheme, mut keyless) = (0usize, 0usize, 0usize);
    let mut eligible: Vec<TokenUtxo> = Vec::new();
    for t in utxos {
        let Some(s) = t.state.as_kcc20() else { continue };
        if t.utxo.covenant_id != Some(offer.asset) || s.extension_commitment != offer.extension_commitment || s.amount <= 0 {
            continue;
        }
        if s.borrow_scheme != 0 {
            borrow += 1;
        } else if s.owner_scheme != SCHEME_P2PK {
            scheme += 1;
        } else if !have_key(&s.owner) {
            keyless += 1;
        } else {
            eligible.push(t);
        }
    }
    let total: i64 = eligible.iter().map(|t| t.state.amount()).fold(0i64, |a, b| a.saturating_add(b));
    let why = |extra: &str| -> X402Error {
        let diag = if borrow > 0 {
            Diag::TokenBorrowEnabled
        } else if scheme > 0 {
            Diag::TokenOwnerScheme
        } else {
            Diag::TokenConservation
        };
        X402Error::payload(
            diag,
            format!(
                "cannot cover {need} units{extra}: spendable {total}; refused {borrow} borrow-enabled, {scheme} non-P2PK-owned, {keyless} \
                 without a local key"
            ),
        )
    };
    if total < need {
        return Err(why(""));
    }
    // one input: the smallest that covers (an exact input avoids the change output)
    if let Some(one) = eligible.iter().filter(|t| t.state.amount() >= need).min_by_key(|t| t.state.amount()) {
        return Ok(vec![one.clone()]);
    }
    eligible.sort_by(|a, b| b.state.amount().cmp(&a.state.amount()));
    let (mut picked, mut sum) = (Vec::new(), 0i64);
    for t in eligible {
        if picked.len() == slots {
            return Err(why(&format!(" within the program's {slots} token inputs")));
        }
        sum = sum.saturating_add(t.state.amount());
        picked.push(t);
        if sum >= need {
            return Ok(picked);
        }
    }
    Err(why(""))
}

/// Builds the unsigned payment: token selection, the commitment digest, the transaction with the
/// commitment in its payload, and the sign requests. Returns the [`BuiltTx`] (sign its `sign`
/// requests) and the payload template for [`finish_kcc20`].
pub fn build_kcc20_unsigned(
    offer: &PaymentRequirements,
    request_hash: &str,
    token_utxos: Vec<TokenUtxo>,
    funding: Vec<KeyUtxo>,
    now_ms: u64,
    opts: &Kcc20Options,
) -> Result<(BuiltTx, PayloadTemplate)> {
    build_unsigned_with(offer, request_hash, token_utxos, funding, now_ms, opts, &|_| true)
}

fn build_unsigned_with(
    offer: &PaymentRequirements,
    request_hash: &str,
    token_utxos: Vec<TokenUtxo>,
    mut funding: Vec<KeyUtxo>,
    now_ms: u64,
    opts: &Kcc20Options,
    have_key: &dyn Fn(&[u8; 32]) -> bool,
) -> Result<(BuiltTx, PayloadTemplate)> {
    if offer.profile()? != Profile::Kcc20 {
        return Err(X402Error::requirements(Diag::UnsupportedKaspaExactProfile, "not a kcc20 requirement"));
    }
    let network = offer.network()?;
    let facts = parse_offer(offer)?;
    if facts.carrier > opts.max_carrier_sompi {
        return Err(X402Error::requirements(
            Diag::CarrierMismatch,
            format!(
                "the offer's carrier {} sompi exceeds the payer bound {} (the payer funds the carrier)",
                facts.carrier, opts.max_carrier_sompi
            ),
        ));
    }
    if opts.token_change_carrier.is_some_and(|c| c > opts.max_carrier_sompi) {
        return Err(X402Error::payload(
            Diag::CarrierMismatch,
            format!("the token change carrier exceeds the payer bound {}", opts.max_carrier_sompi),
        ));
    }
    let rh = parse_hash32(request_hash)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaX402Payload, "requestHash is not 32-byte hex"))?;
    let req_hash = requirements_hash(offer)?;
    let (max_in, _) = facts.program.token_slots().expect("token program");
    let tokens = select_tokens(&facts, token_utxos, max_in, have_key)?;

    // the commitment, BEFORE the transaction exists: the merchant output is output 0 (recipients first)
    let ttl = opts.ttl_seconds.unwrap_or(offer.max_timeout_seconds);
    let expires_at = iso_from_ms(now_ms.saturating_add(ttl.saturating_mul(1_000)));
    let pay_idx = 0u32;
    let pay_to_spk_hex = spk_to_hex(&p2pk_spk(&facts.merchant_key));
    let digest = payload_commit_digest(&PayloadCommit {
        network,
        profile: Profile::Kcc20,
        route_pay_asset: None,
        asset: &offer.asset,
        amount: &offer.amount,
        pay_to: &offer.pay_to,
        pay_to_spk_hex: &pay_to_spk_hex,
        payment_output_index: pay_idx,
        requirements_hash: &req_hash,
        request_hash: &rh,
        expires_at: &expires_at,
    })?;

    let token_sum: i64 = tokens.iter().map(|t| t.state.amount()).sum();
    let request = |funding: Vec<KeyUtxo>| SendTokens {
        token: TokenRef { covenant_id: facts.asset, program: facts.program },
        tokens: tokens.clone(),
        recipients: vec![TokenRecipient { pubkey: facts.merchant_key, amount: facts.amount as i64, carrier: facts.carrier }],
        token_change: None,
        token_change_carrier: if token_sum > facts.amount as i64 { opts.token_change_carrier.unwrap_or(facts.carrier) } else { 0 },
        funding,
        change: None,
        records: vec![Record::X402 { reference: digest.to_vec() }],
        fee: FeeOptions { fee_rate: opts.fee_rate, fee_mode: opts.fee_mode },
    };
    // Fee from the token carriers if they suffice; otherwise add funding inputs, largest first.
    funding.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount));
    let mut used: Vec<KeyUtxo> = Vec::new();
    let mut remaining = funding.into_iter();
    let built = loop {
        match build(&Action::SendTokens(request(used.clone()))) {
            Ok(b) => break b,
            Err(ProtocolError::InsufficientFunds { need, have }) => match remaining.next() {
                Some(f) => used.push(f),
                None => {
                    return Err(X402Error::payload(
                        Diag::InvalidKaspaExactFee,
                        format!(
                        "the token carriers cannot pay the merchant carrier, the change carrier and the fee: need {need} sompi, have \
                             {have}; supply P2PK funding UTXOs"
                    ),
                    ))
                }
            },
            Err(e) => return Err(protocol_err(e)),
        }
    };
    if built.fee.fee > opts.max_fee_sompi {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactFee,
            format!("the fee {} exceeds the payer bound {}", built.fee.fee, opts.max_fee_sompi),
        ));
    }
    // the merchant output must be exactly what the offer asks for, at the committed index
    let out0 = built
        .tx
        .outputs
        .first()
        .ok_or_else(|| X402Error::new(crate::error::Reason::UnexpectedSettleError, Diag::Internal, "no outputs"))?;
    if out0.script_public_key != spk_to_hex_json(&facts) || out0.value != facts.carrier {
        return Err(X402Error::new(
            crate::error::Reason::UnexpectedSettleError,
            Diag::Internal,
            "the built merchant output differs from the offer",
        ));
    }
    let payer_address = facts_owner_address(&tokens, network);
    let extensions = opts.payment_id.as_ref().map(|id| json!({ "payment-identifier": { "info": { "required": true, "id": id } } }));
    let template = PayloadTemplate {
        accepted: offer.clone(),
        request_hash: request_hash.to_string(),
        payment_output_index: pay_idx,
        authorization: Authorization {
            version: AUTH_VERSION_PAYLOAD.into(),
            input_index: None,
            expires_at,
            digest: hex(&digest),
            signature: None,
        },
        payer_address,
        extensions,
    };
    Ok((built, template))
}

fn spk_to_hex_json(facts: &TokenOffer) -> String {
    spk_to_hex(&facts.token_spk)
}

fn facts_owner_address(tokens: &[TokenUtxo], network: Network) -> Option<String> {
    tokens.first().and_then(|t| crate::common::address_of(&p2pk_spk(&t.state.owner()), network))
}

/// Finalizes a built payment with the wallet's (or local) signatures: checks every signature against
/// its digest, tightens the compute budgets, checks that the transaction still commits to the
/// authorization digest, and assembles the `PaymentPayload`.
pub fn finish_kcc20(built: &BuiltTx, template: &PayloadTemplate, signatures: &[InputSignature]) -> Result<PaymentPayload> {
    let signed = finalize(built, signatures, FinalizeOptions { tighten_budgets: true }).map_err(protocol_err)?;
    let (tx, entries) = signed.tx.to_tx().map_err(protocol_err)?;
    let digest = parse_hash32(&template.authorization.digest)
        .ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "authorization.digest is not 32-byte hex"))?;
    check_payload_commitment(&tx, &digest)?;
    Ok(PaymentPayload {
        x402_version: X402_VERSION,
        accepted: template.accepted.clone(),
        payload: ExactPayload {
            kind: PAYLOAD_EXACT_TX.into(),
            profile: Profile::Kcc20.as_str().into(),
            payer_address: template.payer_address.clone(),
            transaction: SafeTx::from_consensus(&tx, &entries).to_text(),
            transaction_encoding: TX_ENCODING.into(),
            payment_output_index: template.payment_output_index,
            request_hash: template.request_hash.clone(),
            challenge_id: None,
            authorization: template.authorization.clone(),
            route: None,
        },
        resource: None,
        extensions: template.extensions.clone(),
    })
}

/// [`pay_kcc20_with`] with default options.
pub fn pay_kcc20(
    offer: &PaymentRequirements,
    request_hash: &str,
    payer_secrets: &BTreeMap<[u8; 32], [u8; 32]>,
    token_utxos: Vec<TokenUtxo>,
    funding: Vec<KeyUtxo>,
    now_ms: u64,
) -> Result<PaymentPayload> {
    pay_kcc20_with(offer, request_hash, payer_secrets, token_utxos, funding, now_ms, &Kcc20Options::default())
}

/// Builds, signs locally and finalizes a payment for `offer`. `payer_secrets` maps x-only public keys
/// to secret keys (the owners of the token inputs and of the funding inputs).
pub fn pay_kcc20_with(
    offer: &PaymentRequirements,
    request_hash: &str,
    payer_secrets: &BTreeMap<[u8; 32], [u8; 32]>,
    token_utxos: Vec<TokenUtxo>,
    funding: Vec<KeyUtxo>,
    now_ms: u64,
    opts: &Kcc20Options,
) -> Result<PaymentPayload> {
    let funding: Vec<KeyUtxo> = funding.into_iter().filter(|f| payer_secrets.contains_key(&f.pubkey)).collect();
    let (built, template) =
        build_unsigned_with(offer, request_hash, token_utxos, funding, now_ms, opts, &|k| payer_secrets.contains_key(k))?;
    let signatures = sign_locally(&built, payer_secrets).map_err(protocol_err)?;
    finish_kcc20(&built, &template, &signatures)
}

/// Payer-side preflight: the verifier's own logic (the payer's policy and trusted chain view) run over
/// the payload before it is disclosed. A payload that does not pass here must not be sent.
pub fn preflight_kcc20(
    chain: &dyn ChainView,
    clock: &dyn Clock,
    policy: &Policy,
    offer: &PaymentRequirements,
    payload: &PaymentPayload,
    request_hash: &str,
) -> Result<Verified> {
    verify_kcc20(&VerifyCtx { chain, clock, policy }, offer, payload, request_hash)
}

// -------------------------------------------------------------------------------------------- revoke

/// The input a revocation spends.
#[derive(Clone, Debug)]
pub enum RevokeInput {
    /// A plain P2PK KAS input of the payment (preferred: the cheapest self-spend).
    Kas(KeyUtxo),
    /// A token input of the payment, transferred back to its owner (half its carrier stays on the
    /// token output, the rest returns as KAS change after the fee).
    Token { token: TokenRef, utxo: TokenUtxo },
}

/// Builds and signs a transaction that spends one input of an issued payment back to its owner, so
/// the payment (which spends the same outpoint) can never confirm. Broadcast it before the
/// authorization expires or whenever the payer decides not to pay; once the payment is accepted
/// on chain the outpoint is gone and this is a no-op conflict.
pub fn revoke_kcc20(input: &RevokeInput, payer_secrets: &BTreeMap<[u8; 32], [u8; 32]>, fee_rate: Option<u64>) -> Result<SignedTx> {
    match input {
        RevokeInput::Token { token, utxo } => {
            if utxo.state.as_kcc20().is_none_or(|s| s.owner_scheme != SCHEME_P2PK || s.borrow_scheme != 0) {
                return Err(X402Error::payload(
                    Diag::TokenOwnerScheme,
                    "only a P2PK-owned, borrow-disabled token input can be revoked here",
                ));
            }
            let carrier = (utxo.utxo.amount / 2).max(1);
            let built = build(&Action::SendTokens(SendTokens {
                token: token.clone(),
                tokens: vec![utxo.clone()],
                recipients: vec![TokenRecipient { pubkey: utxo.state.owner(), amount: utxo.state.amount(), carrier }],
                token_change: None,
                token_change_carrier: 0,
                funding: vec![],
                change: None,
                records: vec![],
                fee: FeeOptions { fee_rate, fee_mode: FeeMode::Relay },
            }))
            .map_err(protocol_err)?;
            let sigs = sign_locally(&built, payer_secrets).map_err(protocol_err)?;
            finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).map_err(protocol_err)
        }
        RevokeInput::Kas(k) => revoke_kas(k, payer_secrets, fee_rate.unwrap_or(MIN_FEE_RATE)),
    }
}

fn revoke_kas(k: &KeyUtxo, secrets: &BTreeMap<[u8; 32], [u8; 32]>, fee_rate: u64) -> Result<SignedTx> {
    if fee_rate < MIN_FEE_RATE {
        return Err(X402Error::payload(Diag::InvalidKaspaExactFee, "fee rate is below the relay minimum"));
    }
    let secret = secrets
        .get(&k.pubkey)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaExactSignature, "no local key for the funding input"))?;
    let spk = p2pk_spk(&k.pubkey);
    let entry = UtxoEntry::new(k.utxo.amount, spk.clone(), k.utxo.block_daa_score, false, None);
    let budget = kob_protocol::budget::lookup("p2pk").map_err(protocol_err)?;
    let mk = |value: u64, sigscript: Vec<u8>| {
        Transaction::new(
            1,
            vec![TransactionInput::new_with_compute_budget(k.utxo.outpoint(), sigscript, 0, budget)],
            vec![TransactionOutput { value, script_public_key: spk.clone(), covenant: None }],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        )
    };
    let placeholder = push_data(&[0u8; 65]);
    let entries = std::slice::from_ref(&entry);
    // fixed point: value = amount - fee(value); the storage mass falls as the value grows
    let mut value = k.utxo.amount;
    for _ in 0..32 {
        let fee = min_fee(&masses(&mk(value, placeholder.clone()), entries), fee_rate);
        let next = k.utxo.amount.saturating_sub(fee);
        if next == value || next == 0 {
            value = next;
            break;
        }
        value = next;
    }
    if value == 0 {
        return Err(X402Error::payload(Diag::InvalidKaspaExactFee, "the funding input cannot pay its own revocation fee"));
    }
    let tx = mk(value, placeholder.clone());
    tx.set_storage_mass(masses(&tx, entries).storage);
    let digest = sighash(&tx, entries, 0);
    let sig = sign_digest(secret, &digest).map_err(protocol_err)?;
    let mut tx = tx;
    tx.inputs[0].signature_script = push_data(&sig);
    let report = masses(&tx, entries);
    let fee = k.utxo.amount - value;
    let min = min_fee(&report, fee_rate);
    if fee < min {
        return Err(X402Error::payload(Diag::InvalidKaspaExactFee, format!("revocation fee {fee} is below the floor {min}")));
    }
    kob_protocol::verify::validate(&tx, entries).map_err(protocol_err)?;
    Ok(SignedTx {
        tx: TxJson::from_tx(&tx, entries),
        fee: FeeReport { fee, min_fee: min, fee_rate, fee_mode: FeeMode::Relay, mass: report, change_output: None },
    })
}
