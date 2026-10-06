//! KCC-20 token profile (`kcc20`) of `exact`.
//!
//! A payment is one version-1 transaction in which the payer spends KCC-20 UTXOs (and optionally plain
//! P2PK KAS UTXOs for the fee) and the merchant receives exactly `amount` token units in exactly one
//! output whose script is the token program with the merchant's state. The verifier
//!
//! * never trusts the payer's view of the chain: every input is resolved from the trusted
//!   [`crate::chain::ChainView`] and classified from the *trusted* script public key;
//! * classifies a token input only if its redeem script (the last push of the signature script) hashes
//!   to the trusted script, matches the allowlisted program's prefix and suffix, decodes to a
//!   [`Kcc20State`] the profile accepts (P2PK owner, borrowing disabled, pinned extension) and the UTXO
//!   carries the token's covenant id;
//! * recomputes every output it lets through (merchant output, payer token change, payer KAS change)
//!   from the offer and the input states and rejects anything else;
//! * requires the authorization to be a payload commitment (`kob-x402-payload-commitment-v1`): the
//!   digest is embedded in the transaction payload, so every input authorizer's SIGHASH_ALL (the
//!   KCC-20 owner witnesses, the funding key) covers it. Nothing here assumes one P2PK signer;
//! * runs every input through the script engine ([`crate::common::check_economics_and_scripts`]).
//!
//! Custody (`extra.token.custody`) is enforced against the allowlist: an offer cannot describe an
//! issuer-controlled token as unconditional, and issuer-controlled tokens are only served when the
//! policy opts in (`docs/spec/x402-kcc20-profile.md`).

use std::collections::BTreeSet;

use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kaspa_txscript::pay_to_script_hash_script;
use serde_json::{json, Map, Value};

use kob_protocol::artifacts::{template, template_by_hash, Template, TemplateId};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{Kcc20State, StateCodec, SCHEME_P2PK};

use crate::chain::Outpoint;
use crate::common::{
    address_of, check_economics_and_scripts, check_envelope, check_expiry, check_payload_commitment, effective_finality, outpoint_of,
    p2pk_key, parse_tx, payload_commit_digest, payment_identifier, resolve_entries, Envelope, PayloadCommit,
};
use crate::error::{Diag, Reason, Result, X402Error};
use crate::policy::{AllowedToken, Custody, Policy};
use crate::safe_tx::spk_to_hex;
use crate::verify::{PaymentKind, Verified, VerifyCtx, WatchedOutput};
use crate::wire::{
    hex, parse_hash32, parse_u64_canonical, PaymentPayload, PaymentRequirements, Profile, AUTH_VERSION_PAYLOAD, BINDING_EXACT,
    TX_ENCODING,
};

/// `extra.token.family` of this profile.
pub const TOKEN_FAMILY: &str = "kcc20";

// ------------------------------------------------------------------------------------------------ offer

/// The facts of a `kcc20` offer, parsed and re-derived without any policy (the payer SDK uses this
/// too). Everything here comes from the requirement, nothing from a payload.
#[derive(Clone, Debug)]
pub struct TokenOffer {
    /// KCC-20 covenant id (`asset`).
    pub asset: [u8; 32],
    /// Token base units the merchant receives (`amount`).
    pub amount: u64,
    /// The token program the offer names (a known, pinned KCC-20 template).
    pub program: TemplateId,
    pub template_hash: [u8; 32],
    pub extension_commitment: [u8; 32],
    pub custody: Custody,
    /// KAS value of the merchant token output, exactly.
    pub carrier: u64,
    /// Script public key of the merchant token output, recomputed from program and state.
    pub token_spk: ScriptPublicKey,
    /// The merchant's owner key (the payload of the `payTo` Schnorr address).
    pub merchant_key: [u8; 32],
    /// The state the merchant output must carry.
    pub merchant_state: Kcc20State,
}

fn hash_field(obj: &Map<String, Value>, key: &str, diag: Diag) -> Result<[u8; 32]> {
    let s =
        obj.get(key).and_then(Value::as_str).ok_or_else(|| X402Error::requirements(diag, format!("extra.token.{key} is missing")))?;
    parse_hash32(s)
        .filter(|h| hex(h) == s)
        .ok_or_else(|| X402Error::requirements(diag, format!("extra.token.{key} must be 64 lowercase hex characters")))
}

/// The Schnorr P2PK owner key of a `payTo` address (`Version::PubKey`, 32-byte payload).
pub fn p2pk_owner_of_address(pay_to: &Address) -> Result<[u8; 32]> {
    if pay_to.version != Version::PubKey || pay_to.payload.len() != 32 {
        return Err(X402Error::requirements(
            Diag::InvalidKaspaX402Binding,
            "kcc20: payTo must be a Schnorr P2PK address (the merchant's token owner key)",
        ));
    }
    Ok(pay_to.payload.as_slice().try_into().expect("32 bytes"))
}

/// Parses and validates the structure of a `kcc20` requirement: asset, `extra.token`, `payTo`, and
/// recomputes `tokenScriptPublicKey`. No allowlist, no policy (see [`check_offer_policy`]).
pub fn parse_offer(offered: &PaymentRequirements) -> Result<TokenOffer> {
    let network = offered.network()?;
    let amount = offered.amount_u64()?;
    if amount > i64::MAX as u64 {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Amount, "token amount exceeds the KCC-20 state range"));
    }
    let asset = parse_hash32(&offered.asset).filter(|a| hex(a) == offered.asset).ok_or_else(|| {
        X402Error::requirements(Diag::InvalidKaspaX402Binding, "kcc20: asset must be a 64 lowercase hex covenant id")
    })?;
    let pay_to = Address::try_from(offered.pay_to.as_str())
        .map_err(|e| X402Error::requirements(Diag::InvalidKaspaX402Binding, format!("payTo is not a Kaspa address: {e}")))?;
    if pay_to.prefix != network.prefix() {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "payTo prefix does not match the network"));
    }
    let merchant_key = p2pk_owner_of_address(&pay_to)?;
    let tok = offered
        .extra
        .get("token")
        .and_then(Value::as_object)
        .ok_or_else(|| X402Error::requirements(Diag::InvalidKaspaX402Binding, "kcc20: extra.token is missing"))?;
    if tok.get("family").and_then(Value::as_str) != Some(TOKEN_FAMILY) {
        return Err(X402Error::requirements(Diag::InvalidKaspaX402Binding, "kcc20: extra.token.family must be kcc20"));
    }
    let template_hash = hash_field(tok, "templateHash", Diag::TokenTemplateMismatch)?;
    let extension_commitment = hash_field(tok, "extensionCommitment", Diag::TokenTemplateMismatch)?;
    let program = template_by_hash(&template_hash).filter(|t| t.id.is_token()).map(|t| t.id).ok_or_else(|| {
        X402Error::requirements(Diag::TokenTemplateMismatch, "extra.token.templateHash is not a known KCC-20 program")
    })?;
    let custody = tok.get("custody").and_then(Value::as_str).and_then(Custody::parse).ok_or_else(|| {
        X402Error::requirements(Diag::TokenCustodyPolicy, "extra.token.custody must be unconditional or issuer-controlled")
    })?;
    let carrier = tok.get("carrier").and_then(Value::as_str).and_then(parse_u64_canonical).filter(|c| *c > 0).ok_or_else(|| {
        X402Error::requirements(Diag::CarrierMismatch, "extra.token.carrier must be a canonical positive uint64 string")
    })?;
    let merchant_state = Kcc20State::p2pk(amount as i64, merchant_key, extension_commitment);
    let token_spk = merchant_state.spk_with(template(program));
    let claimed = tok.get("tokenScriptPublicKey").and_then(Value::as_str).unwrap_or("");
    if claimed != spk_to_hex(&token_spk) {
        return Err(X402Error::requirements(
            Diag::InvalidKaspaX402Binding,
            "extra.token.tokenScriptPublicKey does not match the program and the merchant state",
        ));
    }
    Ok(TokenOffer {
        asset,
        amount,
        program,
        template_hash,
        extension_commitment,
        custody,
        carrier,
        token_spk,
        merchant_key,
        merchant_state,
    })
}

/// Checks a parsed offer against the verifier policy and returns the allowlisted token: allowlisted,
/// custody equal to the allowlist's (and issuer-controlled only if the policy opts in), program and
/// extension commitment equal to the reviewed ones, carrier at least the policy floor.
pub fn check_offer_policy<'a>(policy: &'a Policy, offer: &TokenOffer) -> Result<&'a AllowedToken> {
    let allowed = policy
        .tokens
        .find(&offer.asset)
        .ok_or_else(|| X402Error::requirements(Diag::TokenNotAllowlisted, "the token is not on this verifier's allowlist"))?;
    if !allowed.is_merchant_capable() {
        return Err(X402Error::requirements(
            Diag::TokenNotAllowlisted,
            "the token is a KRON token: the kcc20 profile accepts KCC-20 tokens only (a KRON token is a swap-and-pay pay asset)",
        ));
    }
    if offer.custody != allowed.custody {
        return Err(X402Error::requirements(
            Diag::TokenCustodyPolicy,
            format!(
                "offer states custody {:?} but the allowlist classifies the token as {:?}",
                offer.custody.as_str(),
                allowed.custody.as_str()
            ),
        ));
    }
    if allowed.custody == Custody::IssuerControlled && !policy.allow_issuer_controlled {
        return Err(X402Error::requirements(Diag::TokenCustodyPolicy, "issuer-controlled tokens are not accepted by this verifier"));
    }
    if allowed.template_hash() != offer.template_hash {
        return Err(X402Error::requirements(
            Diag::TokenTemplateMismatch,
            "extra.token.templateHash is not the reviewed program of this token",
        ));
    }
    if allowed.extension_commitment != offer.extension_commitment {
        return Err(X402Error::requirements(
            Diag::TokenTemplateMismatch,
            "extra.token.extensionCommitment is not the reviewed one of this token",
        ));
    }
    if offer.carrier < policy.limits.min_carrier_sompi {
        return Err(X402Error::requirements(
            Diag::CarrierMismatch,
            format!("carrier {} is below the policy floor {}", offer.carrier, policy.limits.min_carrier_sompi),
        ));
    }
    if offer.carrier > policy.limits.max_carrier_sompi {
        return Err(X402Error::requirements(
            Diag::CarrierMismatch,
            format!("carrier {} is above the policy ceiling {}", offer.carrier, policy.limits.max_carrier_sompi),
        ));
    }
    Ok(allowed)
}

// ------------------------------------------------------------------------------------- script parsing

/// One item of a push-only script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push<'a> {
    /// A data push (`OP_0`, direct, `OP_PUSHDATA1/2/4`).
    Data(&'a [u8]),
    /// A small-number opcode (`OP_1NEGATE`, `OP_1`..`OP_16`).
    Number,
}

/// Parses a push-only script. Any other opcode, a truncated push or an oversized item list is
/// malformed (`None`).
pub fn parse_pushes(script: &[u8]) -> Option<Vec<Push<'_>>> {
    const MAX_ITEMS: usize = 1024;
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < script.len() {
        if out.len() >= MAX_ITEMS {
            return None;
        }
        let op = script[i];
        i += 1;
        let len = match op {
            0x00 => 0usize,
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
            0x4f | 0x51..=0x60 => {
                out.push(Push::Number);
                continue;
            }
            _ => return None,
        };
        let end = i.checked_add(len)?;
        out.push(Push::Data(script.get(i..end)?));
        i = end;
    }
    Some(out)
}

/// The redeem script of a P2SH spend: the last push of the signature script, which must be a
/// non-empty data push.
pub fn redeem_of(signature_script: &[u8]) -> Option<&[u8]> {
    match parse_pushes(signature_script)?.last()? {
        Push::Data(d) if !d.is_empty() => Some(d),
        _ => None,
    }
}

fn is_p2sh(spk: &ScriptPublicKey) -> bool {
    let s = spk.script();
    spk.version() == 0 && s.len() == 35 && s[0] == 0xaa && s[1] == 0x20 && s[34] == 0x87
}

/// Best-effort decode of the `next_states` argument of a KCC-20 leader signature script (the KCC-1 `State[]` record
/// array of the token program's `transfer` entry, decoded strictly by [`kob_protocol::kcc20::transfer_next_states`]).
/// Used ONLY to name the reason of a rejection precisely (underpayment, overpayment, borrow-enabled output); acceptance
/// is always by recomputation, and the script engine binds these states to the real outputs. Every KCC-20 program KOB
/// knows has the same `transfer` signature and state layout, so the reference program's ABI decodes them all.
pub fn leader_next_states(signature_script: &[u8]) -> Option<Vec<Kcc20State>> {
    kob_protocol::kcc20::transfer_next_states(template(TemplateId::Kcc20Ref), signature_script)
}

// ------------------------------------------------------------------------------------------ verifier

/// A classified token input.
struct TokenInput {
    index: usize,
    state: Kcc20State,
}

/// What a classified transaction input is.
enum InputKind {
    /// A plain P2PK KAS input of this key.
    Plain([u8; 32]),
    Token(Kcc20State),
}

fn bad_utxo(msg: impl Into<String>) -> X402Error {
    X402Error::payload(Diag::InvalidKaspaExactUtxo, msg)
}

fn bad_out(msg: impl Into<String>) -> X402Error {
    X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, msg)
}

/// Classifies one input from its TRUSTED entry (never from the payer's hint).
fn classify_input(i: usize, sig_script: &[u8], entry: &UtxoEntry, offer: &TokenOffer, tpl: &Template) -> Result<InputKind> {
    let spk = &entry.script_public_key;
    if let Some(key) = p2pk_key(spk) {
        if entry.covenant_id.is_some() {
            return Err(bad_utxo(format!("input {i}: a covenant-bound output at a P2PK script is not a payer funding input")));
        }
        return Ok(InputKind::Plain(key));
    }
    if !is_p2sh(spk) {
        return Err(bad_utxo(format!("input {i}: neither a P2PK funding input nor an allowlisted token output")));
    }
    let redeem = redeem_of(sig_script)
        .ok_or_else(|| X402Error::payload(Diag::InvalidKaspaExactTransaction, format!("input {i}: malformed signature script")))?;
    if pay_to_script_hash_script(redeem) != *spk {
        return Err(bad_utxo(format!("input {i}: the redeem script does not hash to the UTXO's script")));
    }
    let state_bytes = tpl.state_of(redeem).ok_or_else(|| {
        X402Error::payload(
            Diag::TokenTemplateMismatch,
            format!("input {i}: not an instance of the allowlisted {} program", tpl.id.name()),
        )
    })?;
    if entry.covenant_id.map(|h| h.as_bytes()) != Some(offer.asset) {
        return Err(X402Error::payload(Diag::TokenNotAllowlisted, format!("input {i}: a token of another covenant id")));
    }
    let state = Kcc20State::decode(state_bytes)
        .map_err(|e| X402Error::payload(Diag::TokenTemplateMismatch, format!("input {i}: token state: {e}")))?;
    if state.owner_scheme != SCHEME_P2PK {
        return Err(X402Error::payload(
            Diag::TokenOwnerScheme,
            format!("input {i}: token owner scheme {:#04x} is not P2PK", state.owner_scheme),
        ));
    }
    if state.borrow_scheme != 0 {
        return Err(X402Error::payload(Diag::TokenBorrowEnabled, format!("input {i}: token has borrowing enabled")));
    }
    if state.extension_commitment != offer.extension_commitment {
        return Err(X402Error::payload(Diag::TokenTemplateMismatch, format!("input {i}: token extension commitment differs")));
    }
    if state.amount <= 0 {
        return Err(bad_utxo(format!("input {i}: token amount must be positive")));
    }
    Ok(InputKind::Token(state))
}

/// Names why the merchant output does not carry the offered state, from the leader's `next_states`
/// (diagnostic only; the rejection itself is decided by the recomputed script comparison).
fn merchant_mismatch(tx: &Transaction, leader: usize, token_out_rank: usize, offer: &TokenOffer) -> X402Error {
    let generic = || bad_out("the payment output does not carry the offered token state");
    let Some(states) = leader_next_states(&tx.inputs[leader].signature_script) else { return generic() };
    let Some(s) = states.get(token_out_rank) else { return generic() };
    if s.borrow_scheme != 0 {
        return X402Error::payload(Diag::TokenBorrowEnabled, "the merchant output enables borrowing");
    }
    if s.owner != offer.merchant_key || s.owner_scheme != SCHEME_P2PK {
        return bad_out("the merchant output is not owned by the merchant's P2PK key");
    }
    if s.extension_commitment != offer.extension_commitment || s.borrow_guard != [0u8; 32] {
        return X402Error::payload(Diag::TokenTemplateMismatch, "the merchant output carries another extension commitment");
    }
    if s.amount < offer.amount as i64 {
        return X402Error::payload(
            Diag::Underpayment,
            format!("the merchant output carries {} units, {} were required", s.amount, offer.amount),
        );
    }
    if s.amount > offer.amount as i64 {
        return X402Error::payload(
            Diag::Overpayment,
            format!("the merchant output carries {} units, exactly {} are required", s.amount, offer.amount),
        );
    }
    generic()
}

/// Verifies a `kcc20` payment (binding steps 1 to 11, token profile). Fails closed.
pub fn verify_kcc20(ctx: &VerifyCtx, offered: &PaymentRequirements, payload: &PaymentPayload, request_hash: &str) -> Result<Verified> {
    // 1. envelope, offer, policy (offline)
    let env = check_envelope(ctx, offered, payload, request_hash)?;
    if env.profile != Profile::Kcc20 {
        return Err(X402Error::new(Reason::UnsupportedScheme, Diag::UnsupportedKaspaExactProfile, "not a kcc20 requirement"));
    }
    let offer = parse_offer(offered)?;
    let allowed = check_offer_policy(ctx.policy, &offer)?;
    let tpl = template(allowed.program);
    let payment_identifier = payment_identifier(ctx, payload)?;

    // 2. transaction shape (offline)
    let parsed = parse_tx(ctx, payload)?;
    let tx = &parsed.tx;
    if tx.version != 1 {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "kcc20 payments are version-1 transactions"));
    }
    if tx.gas != 0 || tx.lock_time != 0 {
        return Err(X402Error::payload(Diag::InvalidKaspaExactTransaction, "gas and lockTime must be 0"));
    }
    let pay_idx = payload.payload.payment_output_index as usize;
    if pay_idx >= tx.outputs.len() {
        return Err(bad_out("paymentOutputIndex is outside the outputs"));
    }

    // 3. authorization: payload commitment (offline)
    let expires_at_ms = check_authorization(ctx, &env, offered, payload, tx, pay_idx)?;

    // 4. trusted inputs
    let entries = resolve_entries(ctx, &parsed)?;
    let mut token_inputs: Vec<TokenInput> = Vec::new();
    let mut plain_keys: BTreeSet<[u8; 32]> = BTreeSet::new();
    for (i, (inp, entry)) in tx.inputs.iter().zip(&entries).enumerate() {
        match classify_input(i, &inp.signature_script, entry, &offer, tpl)? {
            InputKind::Plain(k) => {
                plain_keys.insert(k);
            }
            InputKind::Token(state) => token_inputs.push(TokenInput { index: i, state }),
        }
    }
    let leader = token_inputs.first().ok_or_else(|| bad_utxo("the transaction spends no token input"))?.index;
    let (max_in, _) = allowed.program.token_slots().expect("allowlisted programs are token programs");
    if token_inputs.len() > max_in {
        return Err(X402Error::payload(
            Diag::InvalidKaspaExactTransaction,
            format!("{} token inputs exceed the program's {max_in} slots", token_inputs.len()),
        ));
    }
    let mut owners: Vec<[u8; 32]> = Vec::new();
    for t in &token_inputs {
        if !owners.contains(&t.state.owner) {
            owners.push(t.state.owner);
        }
    }
    let payer_keys: BTreeSet<[u8; 32]> = owners.iter().copied().chain(plain_keys.iter().copied()).collect();
    // every payer signature (funding P2PK inputs and the token owners' witnesses) is SIGHASH_ALL: checked on the
    // signature scripts BEFORE the engine, which accepts NONE / SINGLE / ANYONECANPAY signatures too (step 8)
    crate::sighash::check_payer_inputs(
        tx,
        &entries,
        (0..tx.inputs.len()).filter(|i| !token_inputs.iter().any(|t| t.index == *i)),
        token_inputs.iter().map(|t| (t.index, t.state.owner_scheme)),
    )?;
    let token_in_sum = token_inputs
        .iter()
        .try_fold(0i64, |s, t| s.checked_add(t.state.amount))
        .ok_or_else(|| X402Error::payload(Diag::TokenConservation, "token input amounts overflow"))?;
    if token_in_sum < offer.amount as i64 {
        return Err(X402Error::payload(
            Diag::TokenConservation,
            format!("the payer's token inputs hold {token_in_sum} units, {} are required", offer.amount),
        ));
    }
    let change_amount = token_in_sum - offer.amount as i64;

    // 5. outputs: exactly the canonical set
    let binding_ok = |o: &kaspa_consensus_core::tx::TransactionOutput| {
        o.covenant.is_some_and(|c| c.authorizing_input as usize == leader && c.covenant_id.as_bytes() == offer.asset)
    };
    let pay_out = &tx.outputs[pay_idx];
    // rank of the payment output among the token outputs (outputs bound to a covenant), for diagnostics
    let rank = tx.outputs[..pay_idx].iter().filter(|o| o.covenant.is_some()).count();
    if pay_out.script_public_key != offer.token_spk {
        return Err(merchant_mismatch(tx, leader, rank, &offer));
    }
    if pay_out.value != offer.carrier {
        return Err(X402Error::payload(
            Diag::CarrierMismatch,
            format!("the payment output carries {} sompi, the offer's carrier is exactly {}", pay_out.value, offer.carrier),
        ));
    }
    if !binding_ok(pay_out) {
        return Err(bad_out("the payment output is not bound to the token covenant under the leader input"));
    }
    let change_spks: Vec<ScriptPublicKey> = if change_amount > 0 {
        owners.iter().map(|o| Kcc20State::p2pk(change_amount, *o, offer.extension_commitment).spk_with(tpl)).collect()
    } else {
        Vec::new()
    };
    let (mut token_change, mut kas_change) = (false, false);
    for (j, o) in tx.outputs.iter().enumerate() {
        if j == pay_idx {
            continue;
        }
        if o.covenant.is_some() {
            // a token output: only the payer's own change
            if !binding_ok(o) {
                return Err(bad_out(format!("output {j}: a covenant binding other than the token under the leader input")));
            }
            if change_amount == 0 {
                return Err(bad_out(format!("output {j}: an extra token output (the inputs are spent exactly)")));
            }
            if token_change {
                return Err(bad_out(format!("output {j}: a second token change output")));
            }
            if !change_spks.contains(&o.script_public_key) {
                let s = leader_next_states(&tx.inputs[leader].signature_script)
                    .and_then(|st| st.get(tx.outputs[..j].iter().filter(|x| x.covenant.is_some()).count()).cloned());
                return Err(match s {
                    Some(s) if s.borrow_scheme != 0 => {
                        X402Error::payload(Diag::TokenBorrowEnabled, format!("output {j}: token change enables borrowing"))
                    }
                    Some(s) if s.amount != change_amount => X402Error::payload(
                        Diag::TokenConservation,
                        format!("output {j}: token change of {} units, {change_amount} expected", s.amount),
                    ),
                    _ => bad_out(format!("output {j}: not the payer's token change")),
                });
            }
            token_change = true;
        } else {
            match p2pk_key(&o.script_public_key) {
                Some(k) if payer_keys.contains(&k) => {
                    if kas_change {
                        return Err(bad_out(format!("output {j}: a second KAS change output")));
                    }
                    kas_change = true;
                }
                _ => return Err(bad_out(format!("output {j}: not the payment and not payer change"))),
            }
        }
    }

    // 6. fee, mass, every input through the script engine (KCC-20 owner witnesses, funding signatures)
    let fee = check_economics_and_scripts(ctx, tx, &entries)?;

    // 7. the verified fact sheet
    let txid = tx.id().as_bytes();
    let first_owner = token_inputs[0].state.owner;
    let finality = effective_finality(env.finality, ctx.policy);
    let mut ext = Map::new();
    ext.insert("binding".into(), json!(BINDING_EXACT));
    ext.insert("profile".into(), json!(Profile::Kcc20.as_str()));
    ext.insert("paymentOutputIndex".into(), json!(pay_idx));
    ext.insert("finality".into(), json!(finality.as_str()));
    ext.insert("transactionEncoding".into(), json!(TX_ENCODING));
    ext.insert("custody".into(), json!(allowed.custody.as_str()));
    ext.insert(
        "token".into(),
        json!({ "asset": hex(&offer.asset), "custody": allowed.custody.as_str(), "templateHash": hex(&offer.template_hash) }),
    );
    Ok(Verified {
        kind: PaymentKind::Kcc20,
        profile: Profile::Kcc20,
        txid,
        tx: tx.clone(),
        entries: entries.clone(),
        payer_address: address_of(&p2pk_spk(&first_owner), env.network),
        amount: env.amount,
        payment_output_index: pay_idx as u32,
        merchant_output: WatchedOutput {
            outpoint: Outpoint::new(txid, pay_idx as u32),
            script_public_key: offer.token_spk.clone(),
            amount: offer.carrier,
        },
        consumed: tx.inputs.iter().map(outpoint_of).collect(),
        order_inputs: Vec::new(),
        fee,
        finality,
        custody: Some(allowed.custody),
        authorization_expires_at_ms: expires_at_ms,
        request_hash: env.request_hash,
        requirements_hash: env.requirements_hash,
        payment_identifier,
        response_extension: ext,
    })
}

/// The payload-commitment authorization: version, no signature material, recomputed digest equal to
/// the stated one, digest committed in the transaction payload, expiry window. Returns the expiry (ms).
fn check_authorization(
    ctx: &VerifyCtx,
    env: &Envelope,
    offered: &PaymentRequirements,
    payload: &PaymentPayload,
    tx: &Transaction,
    pay_idx: usize,
) -> Result<u64> {
    let a = &payload.payload.authorization;
    if a.version != AUTH_VERSION_PAYLOAD {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "authorization.version must be kob-x402-payload-commitment-v1"));
    }
    if a.signature.is_some() || a.input_index.is_some() {
        return Err(X402Error::payload(Diag::InvalidAuthorization, "a payload commitment carries no signature and no inputIndex"));
    }
    let stated = parse_hash32(&a.digest)
        .ok_or_else(|| X402Error::payload(Diag::InvalidAuthorization, "authorization.digest is not 32-byte hex"))?;
    let pay_to_spk_hex = spk_to_hex(&env.pay_to_spk);
    let digest = payload_commit_digest(&PayloadCommit {
        network: env.network,
        profile: Profile::Kcc20,
        route_pay_asset: None,
        asset: &offered.asset,
        amount: &offered.amount,
        pay_to: &offered.pay_to,
        pay_to_spk_hex: &pay_to_spk_hex,
        payment_output_index: pay_idx as u32,
        requirements_hash: &env.requirements_hash,
        request_hash: &env.request_hash,
        expires_at: &a.expires_at,
    })?;
    if digest != stated {
        return Err(X402Error::payload(
            Diag::InvalidAuthorization,
            "authorization.digest differs from the recomputed payload-commitment digest",
        ));
    }
    check_payload_commitment(tx, &digest)?;
    check_expiry(ctx.clock.now_ms(), env.max_timeout_seconds, &a.expires_at)
}
