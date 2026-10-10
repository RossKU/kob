//! KCC-20 token profile (`kcc20`) of x402 exact: happy paths, attacks, the founder's two points
//! (custody, and authorization of script-authorized token transfers by the payload commitment).
//!
//! Fixtures: a fake KCC-20 token (covenant id `[0x70; 32]`, program `KCC20Ref_8x8` unless stated,
//! extension commitment `[0xee; 32]`) whose UTXOs live in a [`MockChain`]; the allowlist is
//! `Policy.tokens`. Every payment transaction any test builds also passes the script engine
//! (`kob_protocol::verify::validate`) unless the test deliberately tampers after signing and says so.

use std::collections::BTreeMap;

use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutput, UtxoEntry};
use kaspa_txscript::pay_to_script_hash_script;
use serde_json::json;

use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::{build, Action, SendTokens, TokenRecipient, TokenRef};
use kob_protocol::payload::{decode, Record};
use kob_protocol::script::{p2pk_spk, push_data};
use kob_protocol::state::{Kcc20State, SCHEME_P2PK};
use kob_protocol::tx::{
    finalize, masses, sighash, sign_digest, sign_locally, BuiltTx, FeeOptions, FinalizeOptions, KeyUtxo, SigPlan, TokenUtxo, Utxo,
    Witness,
};

use kob_x402::canonical::{requirements_hash, sha256};
use kob_x402::chain::{ChainView, FixedClock, Outpoint, OutputStatus};
use kob_x402::client::token::{
    build_kcc20_unsigned, finish_kcc20, kcc20_requirements, pay_kcc20, pay_kcc20_with, preflight_kcc20, revoke_kcc20, Kcc20Options,
    RevokeInput,
};
use kob_x402::common::{iso_from_ms, payload_commit_digest, PayloadCommit};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::{spk_to_hex, SafeTx};
use kob_x402::testkit::{pubkey, secret, MockChain};
use kob_x402::token::{leader_next_states, parse_pushes, redeem_of, Push};
use kob_x402::verify::{verify_payment, PaymentKind, Verified, VerifyCtx};
use kob_x402::wire::{
    hex, Authorization, ExactPayload, Finality, Network, PaymentPayload, PaymentRequirements, Profile, AUTH_VERSION_PAYLOAD,
    PAYLOAD_EXACT_TX, TX_ENCODING, X402_VERSION,
};
use kob_x402::{Diag, Reason, X402Error};

use kob_x402::testkit::hashtype::{resigned, signers, tampered_byte, BOGUS, NON_ALL};

const KAS: u64 = 100_000_000;
/// KAS carried by every fixture token UTXO.
const CARRIER: u64 = 10 * KAS;
/// The merchant token output's carrier in the offer.
const OFFER_CARRIER: u64 = KAS;
const COV: [u8; 32] = [0x70; 32];
const OTHER_COV: [u8; 32] = [0x71; 32];
const EXT: [u8; 32] = [0xee; 32];
const NOW_MS: u64 = 1_800_000_000_000;
const TIMEOUT: u64 = 120;
const NET: Network = Network::Testnet10;
const PID: &str = "pay-0123456789abcdef";

const PAYER: u8 = 1;
const PAYER2: u8 = 2;
const MERCHANT: u8 = 8;
const ATTACKER: u8 = 9;

fn keys() -> BTreeMap<[u8; 32], [u8; 32]> {
    (1..=40u8).map(|n| (pubkey(n), secret(n))).collect()
}

fn address(key: &[u8; 32]) -> String {
    Address::new(NET.prefix(), Version::PubKey, key).to_string()
}

fn request_hash() -> String {
    hex(&sha256(b"GET /premium-article"))
}

fn token(program: TemplateId, custody: Custody) -> AllowedToken {
    AllowedToken::new(COV, program, EXT, custody, "TST", 2)
}

// ------------------------------------------------------------------------------------------ fixture

struct Fx {
    chain: MockChain,
    clock: FixedClock,
    policy: Policy,
    token: AllowedToken,
}

impl Fx {
    fn new() -> Fx {
        Fx::with(token(TemplateId::Kcc20Ref8x8, Custody::Unconditional), |_| {})
    }

    fn with(token: AllowedToken, tweak: impl FnOnce(&mut Policy)) -> Fx {
        let mut policy = Policy::new(NET);
        policy.tokens.insert(token.clone()).unwrap();
        tweak(&mut policy);
        Fx { chain: MockChain::new(), clock: FixedClock::new(NOW_MS), policy, token }
    }

    fn ctx(&self) -> VerifyCtx<'_> {
        VerifyCtx { chain: &self.chain, clock: &self.clock, policy: &self.policy }
    }

    /// A payer-owned token UTXO on chain.
    fn tok(&self, owner: u8, amount: i64) -> TokenUtxo {
        self.tok_state(self.token.program, COV, Kcc20State::p2pk(amount, pubkey(owner), EXT), CARRIER)
    }

    fn tok_carrier(&self, owner: u8, amount: i64, carrier: u64) -> TokenUtxo {
        self.tok_state(self.token.program, COV, Kcc20State::p2pk(amount, pubkey(owner), EXT), carrier)
    }

    fn tok_state(&self, program: TemplateId, cov: [u8; 32], state: Kcc20State, carrier: u64) -> TokenUtxo {
        let op = self.chain.add_utxo(carrier, state.spk_with(template(program)), Some(cov));
        TokenUtxo { utxo: utxo_of(&self.chain, op), state: state.into() }
    }

    fn funding(&self, owner: u8, amount: u64) -> KeyUtxo {
        let op = self.chain.add_p2pk(&pubkey(owner), amount);
        KeyUtxo { utxo: utxo_of(&self.chain, op), pubkey: pubkey(owner) }
    }

    fn offer_for(&self, token: &AllowedToken, amount: u64) -> PaymentRequirements {
        kcc20_requirements(NET, token, amount, &address(&pubkey(MERCHANT)), OFFER_CARRIER, TIMEOUT, Finality::Accepted).unwrap()
    }

    fn offer(&self, amount: u64) -> PaymentRequirements {
        self.offer_for(&self.token, amount)
    }

    fn verify(&self, offer: &PaymentRequirements, p: &PaymentPayload) -> Result<Verified, X402Error> {
        verify_payment(&self.ctx(), offer, p, &request_hash())
    }
}

fn utxo_of(chain: &MockChain, op: Outpoint) -> Utxo {
    let u = chain.utxo(&op).unwrap();
    Utxo { transaction_id: op.txid, index: op.index, amount: u.amount, block_daa_score: u.block_daa_score, covenant_id: u.covenant_id }
}

fn opts() -> Kcc20Options {
    Kcc20Options { payment_id: Some(PID.into()), ..Kcc20Options::default() }
}

fn pay(_fx: &Fx, offer: &PaymentRequirements, tokens: Vec<TokenUtxo>, funding: Vec<KeyUtxo>) -> PaymentPayload {
    pay_kcc20_with(offer, &request_hash(), &keys(), tokens, funding, NOW_MS, &opts()).unwrap_or_else(|e| panic!("pay_kcc20: {e}"))
}

fn err_of(r: Result<Verified, X402Error>) -> X402Error {
    r.expect_err("verification must fail")
}

#[track_caller]
fn assert_diag(r: Result<Verified, X402Error>, diag: Diag) {
    let e = err_of(r);
    assert_eq!(e.diag, diag, "{e}");
}

/// The transaction of a payload with its trusted entries (from the chain, as the verifier resolves them).
fn parse_payload_tx(fx: &Fx, p: &PaymentPayload) -> (Transaction, Vec<UtxoEntry>) {
    let parsed = SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap();
    let entries = parsed
        .tx
        .inputs
        .iter()
        .map(|i| {
            fx.chain.utxo(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index)).unwrap().to_entry()
        })
        .collect();
    (parsed.tx, entries)
}

/// The script engine (enforced budgets, storage mass, fee floor) accepts the payment transaction.
#[track_caller]
fn assert_engine_ok(fx: &Fx, p: &PaymentPayload) {
    let (tx, entries) = parse_payload_tx(fx, p);
    kob_protocol::verify::validate(&tx, &entries).unwrap_or_else(|e| panic!("engine rejected the payment: {e}"));
}

/// Replaces the transaction of a payload (id and hints recomputed).
fn tx_text(tx: &Transaction, entries: &[UtxoEntry]) -> String {
    // a mutated transaction caches its old id until finalized
    let mut t = tx.clone();
    t.finalize();
    SafeTx::from_consensus(&t, entries).to_text()
}

fn with_tx(p: &PaymentPayload, tx: &Transaction, entries: &[UtxoEntry]) -> PaymentPayload {
    let mut q = p.clone();
    q.payload.transaction = tx_text(tx, entries);
    q
}

// ------------------------------------------------------------------------------------ forging tools

/// Digest of the commitment for `offer` at output `idx` expiring at `expires_at`.
fn digest_for(offer: &PaymentRequirements, idx: u32, expires_at: &str) -> [u8; 32] {
    let facts = kob_x402::token::parse_offer(offer).unwrap();
    payload_commit_digest(&PayloadCommit {
        network: NET,
        profile: Profile::Kcc20,
        route_pay_asset: None,
        asset: &offer.asset,
        amount: &offer.amount,
        pay_to: &offer.pay_to,
        pay_to_spk_hex: &spk_to_hex(&p2pk_spk(&facts.merchant_key)),
        payment_output_index: idx,
        requirements_hash: &requirements_hash(offer).unwrap(),
        request_hash: &kob_x402::wire::parse_hash32(&request_hash()).unwrap(),
        expires_at,
    })
    .unwrap()
}

fn expires_default() -> String {
    iso_from_ms(NOW_MS + TIMEOUT * 1_000)
}

/// A standard request paying `offer` (merchant output first), with the honest commitment record.
fn std_send(offer: &PaymentRequirements, tokens: Vec<TokenUtxo>, funding: Vec<KeyUtxo>) -> SendTokens {
    let facts = kob_x402::token::parse_offer(offer).unwrap();
    let sum: i64 = tokens.iter().map(|t| t.state.amount()).sum();
    let digest = digest_for(offer, 0, &expires_default());
    SendTokens {
        token: TokenRef { covenant_id: facts.asset, program: facts.program },
        tokens,
        recipients: vec![TokenRecipient { pubkey: facts.merchant_key, amount: facts.amount as i64, carrier: facts.carrier }],
        token_change: None,
        token_change_carrier: if sum > facts.amount as i64 { facts.carrier } else { 0 },
        funding,
        change: None,
        records: vec![Record::X402 { reference: digest.to_vec() }],
        fee: FeeOptions::default(),
    }
}

/// Signs and finalizes a built transaction and wraps it as a payload with `auth`.
fn wrap(offer: &PaymentRequirements, built: &BuiltTx, auth: Authorization, pay_idx: u32) -> PaymentPayload {
    let sigs = sign_locally(built, &keys()).unwrap();
    let signed = finalize(built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    let (tx, entries) = signed.tx.to_tx().unwrap();
    payload_of(offer, &tx, &entries, auth, pay_idx)
}

fn payload_of(
    offer: &PaymentRequirements,
    tx: &Transaction,
    entries: &[UtxoEntry],
    auth: Authorization,
    pay_idx: u32,
) -> PaymentPayload {
    PaymentPayload {
        x402_version: X402_VERSION,
        accepted: offer.clone(),
        payload: ExactPayload {
            kind: PAYLOAD_EXACT_TX.into(),
            profile: "kcc20".into(),
            payer_address: None,
            transaction: tx_text(tx, entries),
            transaction_encoding: TX_ENCODING.into(),
            payment_output_index: pay_idx,
            request_hash: request_hash(),
            challenge_id: None,
            authorization: auth,
            route: None,
        },
        resource: None,
        extensions: Some(json!({ "payment-identifier": { "info": { "required": true, "id": PID } } })),
    }
}

fn honest_auth(offer: &PaymentRequirements) -> Authorization {
    let expires_at = expires_default();
    Authorization {
        version: AUTH_VERSION_PAYLOAD.into(),
        input_index: None,
        digest: hex(&digest_for(offer, 0, &expires_at)),
        expires_at,
        signature: None,
    }
}

/// Builds a payment from `send` (any shape) with the honest authorization; engine-valid when the
/// builder produced it.
fn forge(offer: &PaymentRequirements, send: SendTokens) -> PaymentPayload {
    let built = build(&Action::SendTokens(send)).unwrap_or_else(|e| panic!("forge build: {e}"));
    wrap(offer, &built, honest_auth(offer), 0)
}

/// Builds the honest transaction, lets `tweak` edit the transaction and the signing plans, re-signs
/// every signer (so the result is properly signed for the tampered shape) and wraps it. The
/// transaction may no longer be engine-valid (covenant rules); verification rejects on shape first.
fn retx(
    offer: &PaymentRequirements,
    send: SendTokens,
    tweak: impl FnOnce(&mut Transaction, &mut Vec<UtxoEntry>, &mut Vec<SigPlan>),
) -> PaymentPayload {
    let built = build(&Action::SendTokens(send)).unwrap();
    let (mut tx, mut entries) = built.tx.to_tx().unwrap();
    let mut plans = built.plans.clone();
    tweak(&mut tx, &mut entries, &mut plans);
    let keys = keys();
    for i in 0..tx.inputs.len() {
        tx.inputs[i].signature_script.clear();
    }
    let digests: Vec<[u8; 32]> = (0..tx.inputs.len()).map(|i| sighash(&tx, &entries, i)).collect();
    for (i, plan) in plans.iter().enumerate() {
        let sig = plan.signer().map(|k| sign_digest(&keys[&k], &digests[i]).unwrap());
        tx.inputs[i].signature_script = plan.sigscript(sig.as_deref()).unwrap();
    }
    tx.set_storage_mass(masses(&tx, &entries).storage);
    payload_of(offer, &tx, &entries, honest_auth(offer), 0)
}

fn leader_plan(plans: &mut [SigPlan]) -> &mut SigPlan {
    plans.iter_mut().find(|p| matches!(p, SigPlan::TokenLeader { .. })).expect("leader")
}

// =================================================================================================
// requirements (merchant side)
// =================================================================================================

#[test]
fn requirements_parse_back_and_carry_the_token_fields() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    assert_eq!(offer.scheme, "exact");
    assert_eq!(offer.asset, hex(&COV));
    assert_eq!(offer.amount, "400");
    assert_eq!(offer.extra_str("profile"), Some("kcc20"));
    let t = offer.extra["token"].as_object().unwrap();
    assert_eq!(t["family"], "kcc20");
    assert_eq!(t["custody"], "unconditional");
    assert_eq!(t["carrier"], OFFER_CARRIER.to_string());
    assert_eq!(t["templateHash"], hex(&template(TemplateId::Kcc20Ref8x8).hash));
    assert_eq!(t["extensionCommitment"], hex(&EXT));
    let state = Kcc20State::p2pk(400, pubkey(MERCHANT), EXT);
    assert_eq!(t["tokenScriptPublicKey"], spk_to_hex(&state.spk_with(template(TemplateId::Kcc20Ref8x8))));
    let facts = kob_x402::token::parse_offer(&offer).unwrap();
    assert_eq!((facts.amount, facts.carrier, facts.merchant_key), (400, OFFER_CARRIER, pubkey(MERCHANT)));
    // the requirement round-trips through the wire codec unchanged
    let h = kob_x402::wire::header_encode(&offer).unwrap();
    assert_eq!(kob_x402::wire::header_decode::<PaymentRequirements>(&h).unwrap(), offer);
}

#[test]
fn requirements_refuse_a_non_schnorr_pay_to_and_bad_numbers() {
    let fx = Fx::new();
    let t = &fx.token;
    let mk = |addr: &str, amount: u64, carrier: u64| kcc20_requirements(NET, t, amount, addr, carrier, TIMEOUT, Finality::Accepted);
    // P2SH and ECDSA addresses cannot be a KCC-20 P2PK owner
    let p2sh = Address::new(NET.prefix(), Version::ScriptHash, &[7u8; 32]).to_string();
    let ecdsa = Address::new(NET.prefix(), Version::PubKeyECDSA, &[2u8; 33]).to_string();
    assert_eq!(mk(&p2sh, 5, KAS).unwrap_err().diag, Diag::InvalidKaspaX402Binding);
    assert_eq!(mk(&ecdsa, 5, KAS).unwrap_err().diag, Diag::InvalidKaspaX402Binding);
    // wrong network prefix
    let main = Address::new(Network::Mainnet.prefix(), Version::PubKey, &pubkey(MERCHANT)).to_string();
    assert_eq!(mk(&main, 5, KAS).unwrap_err().diag, Diag::InvalidKaspaX402Binding);
    let good = address(&pubkey(MERCHANT));
    assert_eq!(mk(&good, 0, KAS).unwrap_err().diag, Diag::InvalidKaspaX402Amount);
    assert_eq!(mk(&good, u64::MAX, KAS).unwrap_err().diag, Diag::InvalidKaspaX402Amount);
    assert_eq!(mk(&good, 5, 0).unwrap_err().diag, Diag::CarrierMismatch);
    assert!(kcc20_requirements(NET, t, 5, &good, KAS, TIMEOUT, Finality::Mempool).is_err());
}

// =================================================================================================
// happy paths
// =================================================================================================

#[test]
fn exact_amount_from_one_input_with_change() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let t = fx.tok(PAYER, 1_000);
    let p = pay(&fx, &offer, vec![t.clone()], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.kind, PaymentKind::Kcc20);
    assert_eq!(v.profile, Profile::Kcc20);
    assert_eq!(v.amount, 400);
    assert_eq!(v.custody, Some(Custody::Unconditional));
    assert_eq!(v.payment_output_index, 0);
    assert_eq!(v.finality, Finality::Accepted);
    assert_eq!(v.payment_identifier.as_deref(), Some(PID));
    assert_eq!(v.merchant_output.amount, OFFER_CARRIER);
    let facts = kob_x402::token::parse_offer(&offer).unwrap();
    assert_eq!(v.merchant_output.script_public_key, facts.token_spk);
    assert_eq!(v.merchant_output.outpoint, Outpoint::new(v.txid, 0));
    assert_eq!(v.consumed, vec![Outpoint::new(t.utxo.transaction_id, t.utxo.index)]);
    assert!(v.order_inputs.is_empty());
    assert!(v.fee > 0 && v.fee <= 25_000_000);
    assert_eq!(v.txid, v.tx.id().as_bytes());
    assert_eq!(v.payer_address.as_deref(), Some(address(&pubkey(PAYER)).as_str()));
    // canonical shape: merchant token output, token change, KAS change; one token input, no funding input
    assert_eq!((v.tx.inputs.len(), v.tx.outputs.len(), v.tx.version), (1, 3, 1));
    assert_eq!(v.tx.outputs[0].value, OFFER_CARRIER);
    let bind = v.tx.outputs[0].covenant.unwrap();
    assert_eq!((bind.authorizing_input, bind.covenant_id.as_bytes()), (0, COV));
    let change = Kcc20State::p2pk(600, pubkey(PAYER), EXT).spk_with(template(TemplateId::Kcc20Ref8x8));
    assert_eq!(v.tx.outputs[1].script_public_key, change);
    assert!(v.tx.outputs[1].covenant.is_some());
    assert_eq!(v.tx.outputs[2].script_public_key, p2pk_spk(&pubkey(PAYER)));
    assert!(v.tx.outputs[2].covenant.is_none());
    // the response extension names the token and its custody
    let e = &v.response_extension;
    assert_eq!(e["binding"], "kaspa-exact-v2");
    assert_eq!(e["profile"], "kcc20");
    assert_eq!(e["paymentOutputIndex"], 0);
    assert_eq!(e["token"]["asset"], hex(&COV));
    assert_eq!(e["token"]["custody"], "unconditional");
    assert_eq!(e["token"]["templateHash"], hex(&template(TemplateId::Kcc20Ref8x8).hash));
}

#[test]
fn exact_amount_from_two_inputs_and_fewest_inputs_wins() {
    let fx = Fx::new();
    let offer = fx.offer(500);
    // 300 + 250 >= 500 needs two inputs; the 40 stays untouched
    let (a, b, c) = (fx.tok(PAYER, 300), fx.tok(PAYER, 250), fx.tok(PAYER, 40));
    let p = pay(&fx, &offer, vec![a.clone(), c.clone(), b.clone()], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.tx.inputs.len(), 2);
    let spent: Vec<_> = v.consumed.clone();
    assert!(spent.contains(&Outpoint::new(a.utxo.transaction_id, a.utxo.index)));
    assert!(spent.contains(&Outpoint::new(b.utxo.transaction_id, b.utxo.index)));
    assert!(!spent.contains(&Outpoint::new(c.utxo.transaction_id, c.utxo.index)));
    let change = Kcc20State::p2pk(50, pubkey(PAYER), EXT).spk_with(template(TemplateId::Kcc20Ref8x8));
    assert_eq!(v.tx.outputs[1].script_public_key, change);

    // one large UTXO beats two small ones: the smallest input that covers is chosen
    let fx = Fx::new();
    let offer = fx.offer(800);
    let toks = vec![fx.tok(PAYER, 100), fx.tok(PAYER, 950), fx.tok(PAYER, 900), fx.tok(PAYER, 50)];
    let chosen = toks[2].clone();
    let p = pay(&fx, &offer, toks, vec![]);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.consumed, vec![Outpoint::new(chosen.utxo.transaction_id, chosen.utxo.index)]);
}

#[test]
fn inputs_of_two_owners_pay_together() {
    let fx = Fx::new();
    let offer = fx.offer(500);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300)], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.tx.inputs.len(), 2);
    // change goes to an owner among the inputs
    let change = Kcc20State::p2pk(100, pubkey(PAYER), EXT).spk_with(template(TemplateId::Kcc20Ref8x8));
    let change2 = Kcc20State::p2pk(100, pubkey(PAYER2), EXT).spk_with(template(TemplateId::Kcc20Ref8x8));
    assert!([change, change2].contains(&v.tx.outputs[1].script_public_key));
}

#[test]
fn exact_input_needs_no_token_change() {
    let fx = Fx::new();
    let offer = fx.offer(700);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 700)], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    // merchant token output + KAS change only
    assert_eq!(v.tx.outputs.len(), 2);
    assert!(v.tx.outputs[1].covenant.is_none());
}

#[test]
fn fee_comes_from_the_carrier_or_from_funding_when_the_carrier_is_too_small() {
    let fx = Fx::new();
    let offer = fx.offer(700);
    // The token input carries exactly the merchant carrier: nothing is left for the fee.
    let tiny = fx.tok_carrier(PAYER, 700, OFFER_CARRIER);
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![tiny.clone()], vec![], NOW_MS, &opts()).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaExactFee, "{e}");
    assert!(e.message.contains("funding"), "{e}");
    // A P2PK funding input pays the fee (and gets its change back).
    let f = fx.funding(PAYER, 2 * KAS);
    let p = pay(&fx, &offer, vec![tiny], vec![f.clone()]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.tx.inputs.len(), 2);
    assert!(v.consumed.contains(&Outpoint::new(f.utxo.transaction_id, f.utxo.index)));
    assert_eq!(v.tx.outputs.last().unwrap().script_public_key, p2pk_spk(&pubkey(PAYER)));
    // Funding inputs are only used when needed: a roomy carrier never touches them.
    let roomy = fx.tok(PAYER, 700);
    let f2 = fx.funding(PAYER, 2 * KAS);
    let p = pay(&fx, &offer, vec![roomy], vec![f2.clone()]);
    let v = fx.verify(&offer, &p).unwrap();
    assert!(!v.consumed.contains(&Outpoint::new(f2.utxo.transaction_id, f2.utxo.index)));
}

#[test]
fn issuer_controlled_token_is_accepted_only_when_policy_allows_and_the_offer_says_so() {
    let ic = token(TemplateId::Kcc20Ref8x8, Custody::IssuerControlled);
    // policy without the flag: rejected (custody policy), even though the offer is honest
    let fx = Fx::with(ic.clone(), |_| {});
    let offer = fx.offer_for(&ic, 400);
    assert_eq!(offer.extra["token"]["custody"], "issuer-controlled");
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    assert_diag(fx.verify(&offer, &p), Diag::TokenCustodyPolicy);
    // policy opt-in: accepted, and the verified fact says issuer-controlled
    let fx = Fx::with(ic.clone(), |p| p.allow_issuer_controlled = true);
    let offer = fx.offer_for(&ic, 400);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.custody, Some(Custody::IssuerControlled));
    assert_eq!(v.response_extension["token"]["custody"], "issuer-controlled");
    // the offer must state it: an issuer-controlled token offered as unconditional is a custody lie
    let lie = fx.offer_for(&token(TemplateId::Kcc20Ref8x8, Custody::Unconditional), 400);
    let p = pay(&fx, &lie, vec![fx.tok(PAYER, 1_000)], vec![]);
    assert_diag(fx.verify(&lie, &p), Diag::TokenCustodyPolicy);
}

#[test]
fn full_flow_verify_submit_mine_and_the_merchant_output_is_accepted() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let t = fx.tok(PAYER, 1_000);
    let p = pay(&fx, &offer, vec![t.clone()], vec![]);
    // payer-side preflight before disclosure
    let pre = preflight_kcc20(&fx.chain, &fx.clock, &fx.policy, &offer, &p, &request_hash()).unwrap();
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(pre.txid, v.txid);
    // broadcast the exact verified transaction
    let txid = fx.chain.submit(&v.tx).unwrap();
    assert_eq!(txid, v.txid);
    let mo = &v.merchant_output;
    assert_eq!(fx.chain.output_status(&mo.outpoint, &mo.script_public_key).unwrap(), OutputStatus::Mempool);
    fx.chain.mine(0);
    assert!(fx.chain.is_accepted(&txid));
    assert!(matches!(fx.chain.output_status(&mo.outpoint, &mo.script_public_key).unwrap(), OutputStatus::Accepted { .. }));
    // the merchant now owns a token UTXO of exactly `amount` units with exactly the carrier
    let u = fx.chain.utxo(&mo.outpoint).unwrap();
    assert_eq!((u.amount, u.covenant_id), (OFFER_CARRIER, Some(COV)));
    assert_eq!(u.script_public_key, Kcc20State::p2pk(400, pubkey(MERCHANT), EXT).spk_with(template(TemplateId::Kcc20Ref8x8)));
    // the payer's change is spendable by the payer: the token change and the KAS change exist
    assert_eq!(fx.chain.unspent_of(&Kcc20State::p2pk(600, pubkey(PAYER), EXT).spk_with(template(TemplateId::Kcc20Ref8x8))).len(), 1);
    // replaying the same payment fails: its inputs are gone
    let e = err_of(fx.verify(&offer, &p));
    assert_eq!((e.diag, e.reason), (Diag::InvalidKaspaExactUtxo, Reason::InvalidTransactionState));
    // a fresh payment of the change works too (the chain state is consistent)
    let change_utxo = fx.chain.unspent_of(&Kcc20State::p2pk(600, pubkey(PAYER), EXT).spk_with(template(TemplateId::Kcc20Ref8x8)));
    let (op, cu) = &change_utxo[0];
    let t2 = TokenUtxo {
        utxo: Utxo {
            transaction_id: op.txid,
            index: op.index,
            amount: cu.amount,
            block_daa_score: cu.block_daa_score,
            covenant_id: cu.covenant_id,
        },
        state: Kcc20State::p2pk(600, pubkey(PAYER), EXT).into(),
    };
    let offer2 = fx.offer(100);
    // (the change token carries only the offer carrier: the fee comes from a funding UTXO this time)
    let p2 = pay(&fx, &offer2, vec![t2], vec![fx.funding(PAYER, 2 * KAS)]);
    fx.verify(&offer2, &p2).unwrap();
}

#[test]
fn wallet_flow_unsigned_build_then_finish_with_external_signatures() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let t = fx.tok(PAYER, 1_000);
    let (built, template_) = build_kcc20_unsigned(&offer, &request_hash(), vec![t], vec![], NOW_MS, &opts()).unwrap();
    // one sign request: the token owner's witness; no plain P2PK signature exists
    assert_eq!(built.sign.len(), 1);
    assert_eq!(built.sign[0].pubkey, pubkey(PAYER));
    assert!(built.sign[0].redeem_script.is_some());
    assert_eq!(template_.payment_output_index, 0);
    assert_eq!(template_.authorization.version, AUTH_VERSION_PAYLOAD);
    // the wallet signs the digest of each request and returns signatures only
    let sigs = sign_locally(&built, &keys()).unwrap();
    let p = finish_kcc20(&built, &template_, &sigs).unwrap();
    fx.verify(&offer, &p).unwrap();
    // an unsigned / mis-signed set does not finish
    assert!(finish_kcc20(&built, &template_, &[]).is_err());
    let wrong = vec![kob_protocol::tx::InputSignature {
        input_index: 0,
        signature: sign_digest(&secret(ATTACKER), &built.sign[0].sighash).unwrap(),
    }];
    assert!(finish_kcc20(&built, &template_, &wrong).is_err());
    // the template round-trips as JSON (wallet hand-off)
    let j = serde_json::to_string(&template_).unwrap();
    assert_eq!(serde_json::from_str::<kob_x402::client::token::PayloadTemplate>(&j).unwrap(), template_);
}

#[test]
fn the_commitment_digest_is_in_the_transaction_payload_before_signing() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let (built, tpl) = build_kcc20_unsigned(&offer, &request_hash(), vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS, &opts()).unwrap();
    // the UNSIGNED transaction already carries the KOB1 X402 record equal to the authorization digest
    let pl = decode(&built.tx.payload).unwrap().unwrap();
    assert!(!pl.legacy);
    assert_eq!(
        pl.records,
        vec![Record::X402 { reference: kob_x402::wire::parse_hash32(&tpl.authorization.digest).unwrap().to_vec() }]
    );
    // and the digest is the one the verifier recomputes
    assert_eq!(tpl.authorization.digest, hex(&digest_for(&offer, 0, &tpl.authorization.expires_at)));
    assert_eq!(tpl.authorization.expires_at, iso_from_ms(NOW_MS + TIMEOUT * 1_000));
}

#[test]
fn dispatcher_routes_kcc20_and_rejects_the_other_profiles() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    assert_eq!(fx.verify(&offer, &p).unwrap().kind, PaymentKind::Kcc20);
    // the same payload under a standard-native requirement is not a kcc20 payment
    let mut native = offer.clone();
    native.extra.insert("profile".into(), json!("standard-native"));
    let mut q = p.clone();
    q.accepted = native.clone();
    q.payload.profile = "standard-native".into();
    assert!(verify_payment(&fx.ctx(), &native, &q, &request_hash()).is_err());
}

// =================================================================================================
// script parser
// =================================================================================================

#[test]
fn push_parser_is_strict() {
    let redeem = vec![0xaa; 300];
    let s = [push_data(&[1, 2, 3]), push_data(&redeem)].concat();
    assert_eq!(redeem_of(&s), Some(redeem.as_slice()));
    assert_eq!(parse_pushes(&s).unwrap().len(), 2);
    // small numbers are pushes but never a redeem
    assert_eq!(parse_pushes(&[0x51, 0x4f]).unwrap(), vec![Push::Number, Push::Number]);
    assert_eq!(redeem_of(&[0x03, 1, 2, 3, 0x51]), None);
    // non-push opcodes, truncation, empty
    assert!(parse_pushes(&[0xac]).is_none());
    assert!(parse_pushes(&[0x05, 1, 2]).is_none());
    assert!(parse_pushes(&[0x4c]).is_none());
    assert!(parse_pushes(&[0x4d, 0xff]).is_none());
    assert!(parse_pushes(&[0x4e, 0xff, 0xff, 0xff, 0xff, 1]).is_none());
    assert!(parse_pushes(&[0x50]).is_none());
    assert_eq!(redeem_of(&[]), None);
    assert_eq!(redeem_of(&[0x00]), None);
}

#[test]
fn leader_next_states_decode_matches_the_builder() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let send = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    let built = build(&Action::SendTokens(send)).unwrap();
    let SigPlan::TokenLeader { next_states, .. } = &built.plans[0] else { panic!("leader") };
    let sig = built.plans[0].sigscript(Some(&[7u8; 65])).unwrap();
    assert_eq!(leader_next_states(&sig).unwrap(), *next_states);
    // a delegator script has no argument arrays
    assert!(leader_next_states(&push_data(&[1, 2, 3])).is_none());
}

// =================================================================================================
// attack tooling
// =================================================================================================

/// Replaces input `i` by the token UTXO `by` (entry, outpoint, signing plan with its state).
fn swap_input(
    fx: &Fx,
    tx: &mut Transaction,
    entries: &mut [UtxoEntry],
    plans: &mut [SigPlan],
    i: usize,
    by: &TokenUtxo,
    program: TemplateId,
) {
    tx.inputs[i].previous_outpoint = by.utxo.outpoint();
    entries[i] = fx.chain.utxo(&Outpoint::new(by.utxo.transaction_id, by.utxo.index)).unwrap().to_entry();
    let by_state = by.state.as_kcc20().expect("a KCC-20 token").clone();
    let witness = if by_state.owner_scheme == SCHEME_P2PK { Witness::P2pk(by_state.owner) } else { Witness::CovenantId };
    plans[i] = match &plans[i] {
        SigPlan::TokenLeader { next_states, .. } => {
            SigPlan::TokenLeader { template: program, state: by_state.clone(), next_states: next_states.clone(), witness }
        }
        SigPlan::TokenDelegator { .. } => SigPlan::TokenDelegator { template: program, state: by_state, witness },
        other => panic!("not a token plan: {other:?}"),
    };
}

fn outpoint_json(op: Outpoint) -> kaspa_consensus_core::tx::TransactionOutpoint {
    kaspa_consensus_core::tx::TransactionOutpoint {
        transaction_id: kaspa_consensus_core::tx::TransactionId::from_bytes(op.txid),
        index: op.index,
    }
}

/// Appends an input spending `op` (which must be on the chain) with an arbitrary signature script.
fn append_input(fx: &Fx, p: &PaymentPayload, op: Outpoint, sigscript: Vec<u8>) -> PaymentPayload {
    let (mut tx, mut entries) = parse_payload_tx(fx, p);
    tx.inputs.push(TransactionInput::new_with_compute_budget(outpoint_json(op), sigscript, 0, 10));
    entries.push(fx.chain.utxo(&op).unwrap().to_entry());
    with_tx(p, &tx, &entries)
}

/// Edits the safe projection of a payload's transaction.
fn edit_safe(p: &PaymentPayload, f: impl FnOnce(&mut SafeTx)) -> PaymentPayload {
    let mut safe = SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap();
    f(&mut safe);
    let mut q = p.clone();
    q.payload.transaction = safe.to_text();
    q
}

type AuthTweak = Box<dyn Fn(&mut Authorization)>;
type PolicyTweak = Box<dyn Fn(&mut Policy)>;

fn honest_payload_for(fx: &Fx, offer: &PaymentRequirements) -> PaymentPayload {
    pay(fx, offer, vec![fx.tok(PAYER, 1_000)], vec![])
}

/// An honest payment of `amount` from a fresh 1000-unit token UTXO, plus the offer and the fixture.
fn honest(amount: u64) -> (Fx, PaymentRequirements, PaymentPayload) {
    let fx = Fx::new();
    let offer = fx.offer(amount);
    let p = honest_payload_for(&fx, &offer);
    fx.verify(&offer, &p).unwrap();
    (fx, offer, p)
}

fn verify_with(fx: &Fx, policy: &Policy, offer: &PaymentRequirements, p: &PaymentPayload) -> Result<Verified, X402Error> {
    verify_payment(&VerifyCtx { chain: &fx.chain, clock: &fx.clock, policy }, offer, p, &request_hash())
}

// =================================================================================================
// attacks: amounts, asset, program, extension
// =================================================================================================

#[test]
fn underpayment_and_overpayment_have_their_own_diagnostics() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    // honest control
    fx.verify(&offer, &forge(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]))).unwrap();
    for (paid, diag) in [(399, Diag::Underpayment), (1, Diag::Underpayment), (401, Diag::Overpayment), (1_000, Diag::Overpayment)] {
        let mut s = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
        s.recipients[0].amount = paid;
        s.token_change_carrier = if paid < 1_000 { OFFER_CARRIER } else { 0 };
        let p = forge(&offer, s);
        assert_engine_ok(&fx, &p); // the engine cannot tell: only the verifier enforces the offered amount
        assert_diag(fx.verify(&offer, &p), diag);
    }
    // the payer's inputs hold less than the amount: conservation
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 300)], vec![]);
    s.recipients[0].amount = 300;
    s.token_change_carrier = 0;
    assert_diag(fx.verify(&offer, &forge(&offer, s)), Diag::TokenConservation);
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![fx.tok(PAYER, 300)], vec![], NOW_MS, &opts()).unwrap_err();
    assert_eq!(e.diag, Diag::TokenConservation);
}

#[test]
fn wrong_asset_is_rejected_in_the_offer_and_in_the_inputs() {
    let fx = Fx::new();
    // (a) the offer names a token that is not on the allowlist
    let other = AllowedToken { covenant_id: OTHER_COV, ..fx.token.clone() };
    let offer = fx.offer_for(&other, 400);
    let t = fx.tok_state(TemplateId::Kcc20Ref8x8, OTHER_COV, Kcc20State::p2pk(1_000, pubkey(PAYER), EXT), CARRIER);
    let p = pay(&fx, &offer, vec![t], vec![]);
    assert_diag(fx.verify(&offer, &p), Diag::TokenNotAllowlisted);
    // (b) the offer is the allowlisted token, but the payer spends another token of the same program
    //     (same script shape: the merchant output spk is identical, only the covenant ids differ)
    let offer = fx.offer(400);
    let t = fx.tok_state(TemplateId::Kcc20Ref8x8, OTHER_COV, Kcc20State::p2pk(1_000, pubkey(PAYER), EXT), CARRIER);
    let mut s = std_send(&offer, vec![t], vec![]);
    s.token.covenant_id = OTHER_COV;
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::TokenNotAllowlisted);
    // (c) an asset that is not a canonical covenant id (KAS, padded)
    for asset in ["KAS".to_string(), format!("{} ", offer.asset), offer.asset[2..].to_string()] {
        let mut bad = offer.clone();
        bad.asset = asset;
        let mut q = honest_payload_for(&fx, &offer);
        q.accepted = bad.clone();
        assert_diag(fx.verify(&bad, &q), Diag::InvalidKaspaX402Binding);
    }
}

#[test]
fn token_not_on_the_allowlist_is_rejected() {
    let fx = Fx::with(token(TemplateId::Kcc20Ref8x8, Custody::Unconditional), |p| p.tokens = Default::default());
    let offer = fx.offer(400);
    let p = honest_payload_for(&fx, &offer);
    assert_diag(fx.verify(&offer, &p), Diag::TokenNotAllowlisted);
}

#[test]
fn unreviewed_template_hash_in_the_offer_is_rejected() {
    let fx = Fx::new();
    // the merchant offers the same covenant id but names the 3/3 reference program
    let lookalike = token(TemplateId::Kcc20Ref, Custody::Unconditional);
    let offer = fx.offer_for(&lookalike, 400);
    assert_eq!(offer.extra["token"]["templateHash"], hex(&template(TemplateId::Kcc20Ref).hash));
    let t = fx.tok_state(TemplateId::Kcc20Ref, COV, Kcc20State::p2pk(1_000, pubkey(PAYER), EXT), CARRIER);
    let p = pay(&fx, &offer, vec![t], vec![]);
    assert_diag(fx.verify(&offer, &p), Diag::TokenTemplateMismatch);
    // a templateHash that is no known program at all
    let mut junk = fx.offer(400);
    junk.extra.get_mut("token").unwrap().as_object_mut().unwrap().insert("templateHash".into(), json!(hex(&[9u8; 32])));
    let mut q = honest_payload_for(&fx, &fx.offer(400));
    q.accepted = junk.clone();
    assert_diag(fx.verify(&junk, &q), Diag::TokenTemplateMismatch);
}

#[test]
fn lookalike_token_program_in_an_input_is_rejected() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    // (a) same covenant id, different (reference 3/3) program, honestly built by the payer
    let t = fx.tok_state(TemplateId::Kcc20Ref, COV, Kcc20State::p2pk(1_000, pubkey(PAYER), EXT), CARRIER);
    let mut s = std_send(&offer, vec![t], vec![]);
    s.token.program = TemplateId::Kcc20Ref;
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::TokenTemplateMismatch);
    // (b) the allowlisted program with ONE suffix byte changed, the very same state: the P2SH hash
    //     matches the (attacker-created) UTXO, the redeem is not the reviewed program
    let (fx, offer, honest_p) = honest(400);
    let tpl = template(TemplateId::Kcc20Ref8x8);
    let state = Kcc20State::p2pk(1_000, pubkey(PAYER), EXT);
    let mut redeem = state.redeem_with(tpl);
    *redeem.last_mut().unwrap() ^= 0x01;
    let op = fx.chain.add_utxo(CARRIER, pay_to_script_hash_script(&redeem), Some(COV));
    let (mut tx, mut entries) = parse_payload_tx(&fx, &honest_p);
    tx.inputs[0].previous_outpoint = outpoint_json(op);
    entries[0] = fx.chain.utxo(&op).unwrap().to_entry();
    tx.inputs[0].signature_script = push_data(&redeem);
    assert_diag(fx.verify(&offer, &with_tx(&honest_p, &tx, &entries)), Diag::TokenTemplateMismatch);
}

/// The published public-mint build of the reference (`KCC20PublicMint`: the `KCC20` actor of upstream's `KCC20PublicMint`
/// app, its holders' state opening with the context `gen__kcc20_template`) pays like any other allowlisted KCC-20 program:
/// the offer names its actor-type handle, the payer's holder is spent by the program's own `transfer`, the merchant
/// output and the change carry the context, and the engine and the verifier accept it. A holder whose context field
/// is not the program's template hash, and the standalone build of the same covenant id, are other programs.
#[test]
fn published_public_mint_build_pays_and_its_lookalikes_are_rejected() {
    let pm = TemplateId::Kcc20PublicMint;
    let fx = Fx::with(token(pm, Custody::Unconditional), |_| {});
    let offer = fx.offer(400);
    assert_eq!(offer.extra["token"]["templateHash"], hex(&template(pm).hash));
    let t = fx.tok(PAYER, 1_000);
    let p = pay(&fx, &offer, vec![t], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    fx.chain.submit(&v.tx).unwrap();
    fx.chain.mine(0);
    let u = fx.chain.utxo(&v.merchant_output.outpoint).unwrap();
    let merchant = Kcc20State::p2pk(400, pubkey(MERCHANT), EXT);
    assert_eq!(u.script_public_key, merchant.spk_with(template(pm)));
    // the merchant's output is a holder of the published app: 0x6b, then 0x20 and the Sil template hash, then the state
    let redeem = merchant.redeem_with(template(pm));
    assert_eq!(redeem[1..34], [&[0x20u8][..], &template(pm).sil_hash[..]].concat());
    assert_eq!(fx.chain.unspent_of(&Kcc20State::p2pk(600, pubkey(PAYER), EXT).spk_with(template(pm))).len(), 1);

    // (a) a holder of the same covenant id whose context field is another value: the P2SH matches the UTXO, the program
    //     is not the allowlisted one
    let fx = Fx::with(token(pm, Custody::Unconditional), |_| {});
    let offer = fx.offer(400);
    let honest_p = pay(&fx, &offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    fx.verify(&offer, &honest_p).unwrap();
    let mut forged = Kcc20State::p2pk(1_000, pubkey(PAYER), EXT).redeem_with(template(pm));
    forged[2..34].copy_from_slice(&[0x5a; 32]);
    let op = fx.chain.add_utxo(CARRIER, pay_to_script_hash_script(&forged), Some(COV));
    let (mut tx, mut entries) = parse_payload_tx(&fx, &honest_p);
    tx.inputs[0].previous_outpoint = outpoint_json(op);
    entries[0] = fx.chain.utxo(&op).unwrap().to_entry();
    let ss = tx.inputs[0].signature_script.clone();
    let honest_redeem = Kcc20State::p2pk(1_000, pubkey(PAYER), EXT).redeem_with(template(pm));
    let at = ss.windows(honest_redeem.len()).position(|w| w == honest_redeem.as_slice()).expect("redeem in the sigscript");
    tx.inputs[0].signature_script = [&ss[..at], forged.as_slice()].concat();
    assert_diag(fx.verify(&offer, &with_tx(&honest_p, &tx, &entries)), Diag::TokenTemplateMismatch);

    // (b) the standalone build under the same covenant id, honestly built by the payer
    let t = fx.tok_state(TemplateId::Kcc20Ref, COV, Kcc20State::p2pk(1_000, pubkey(PAYER), EXT), CARRIER);
    let mut s = std_send(&offer, vec![t], vec![]);
    s.token.program = TemplateId::Kcc20Ref;
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::TokenTemplateMismatch);
}

#[test]
fn extension_commitment_mismatch_is_rejected() {
    let fx = Fx::new();
    // (a) the offer names another extension commitment than the reviewed one
    let wrong = AllowedToken { extension_commitment: [0xaa; 32], ..fx.token.clone() };
    let offer = fx.offer_for(&wrong, 400);
    let t = fx.tok_state(TemplateId::Kcc20Ref8x8, COV, Kcc20State::p2pk(1_000, pubkey(PAYER), [0xaa; 32]), CARRIER);
    let p = pay(&fx, &offer, vec![t], vec![]);
    assert_diag(fx.verify(&offer, &p), Diag::TokenTemplateMismatch);
    // (b) the offer is right, the payer's input carries another extension
    let offer = fx.offer(400);
    let t = fx.tok_state(TemplateId::Kcc20Ref8x8, COV, Kcc20State::p2pk(1_000, pubkey(PAYER), [0xbb; 32]), CARRIER);
    let p = forge(&offer, std_send(&offer, vec![t.clone()], vec![]));
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::TokenTemplateMismatch);
    // the SDK does not even try: such UTXOs are not spendable for this offer
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![t], vec![], NOW_MS, &opts()).unwrap_err();
    assert_eq!(e.diag, Diag::TokenConservation);
}

#[test]
fn borrow_enabled_and_non_p2pk_inputs_are_rejected_and_never_selected() {
    let (fx, offer, _) = honest(400);
    let program = TemplateId::Kcc20Ref8x8;
    let borrow = Kcc20State { borrow_scheme: 1, borrow_guard: [5; 32], ..Kcc20State::p2pk(1_000, pubkey(PAYER), EXT) };
    let bt = fx.tok_state(program, COV, borrow, CARRIER);
    let covowned = fx.tok_state(program, COV, Kcc20State::custody(1_000, [0xc0; 32], EXT), CARRIER);
    let good = fx.tok(PAYER, 1_000);
    for (by, diag) in [(&bt, Diag::TokenBorrowEnabled), (&covowned, Diag::TokenOwnerScheme)] {
        let p = retx(&offer, std_send(&offer, vec![good.clone()], vec![]), |tx, entries, plans| {
            swap_input(&fx, tx, entries, plans, 0, by, program);
        });
        assert_diag(fx.verify(&offer, &p), diag);
    }
    // G9: the payer SDK refuses them (and names the reason)
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![bt.clone()], vec![], NOW_MS, &opts()).unwrap_err();
    assert_eq!(e.diag, Diag::TokenBorrowEnabled, "{e}");
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![covowned.clone()], vec![], NOW_MS, &opts()).unwrap_err();
    assert_eq!(e.diag, Diag::TokenOwnerScheme, "{e}");
    // an ineligible UTXO next to an eligible one is skipped, not spent
    let p = pay(&fx, &offer, vec![bt.clone(), covowned.clone(), good.clone()], vec![]);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.consumed, vec![Outpoint::new(good.utxo.transaction_id, good.utxo.index)]);
}

#[test]
fn borrow_enabled_merchant_or_change_state_is_rejected() {
    let (fx, offer, _) = honest(400);
    let tpl = template(TemplateId::Kcc20Ref8x8);
    let good = fx.tok(PAYER, 1_000);
    // change output whose state enables borrowing
    let p = retx(&offer, std_send(&offer, vec![good.clone()], vec![]), |tx, _, plans| {
        let SigPlan::TokenLeader { next_states, .. } = leader_plan(plans) else { unreachable!() };
        next_states[1].borrow_scheme = 1;
        next_states[1].borrow_guard = [7; 32];
        tx.outputs[1].script_public_key = next_states[1].spk_with(tpl);
    });
    assert_diag(fx.verify(&offer, &p), Diag::TokenBorrowEnabled);
    // merchant output whose state enables borrowing (the merchant would receive a revocable balance)
    let p = retx(&offer, std_send(&offer, vec![good.clone()], vec![]), |tx, _, plans| {
        let SigPlan::TokenLeader { next_states, .. } = leader_plan(plans) else { unreachable!() };
        next_states[0].borrow_scheme = 1;
        next_states[0].borrow_guard = [7; 32];
        tx.outputs[0].script_public_key = next_states[0].spk_with(tpl);
    });
    assert_diag(fx.verify(&offer, &p), Diag::TokenBorrowEnabled);
    // a non-zero guard with borrowing disabled is still not the canonical merchant state
    let p = retx(&offer, std_send(&offer, vec![good.clone()], vec![]), |tx, _, plans| {
        let SigPlan::TokenLeader { next_states, .. } = leader_plan(plans) else { unreachable!() };
        next_states[0].borrow_guard = [7; 32];
        tx.outputs[0].script_public_key = next_states[0].spk_with(tpl);
    });
    assert!(fx.verify(&offer, &p).is_err());
}

// =================================================================================================
// attacks: outputs
// =================================================================================================

#[test]
fn wrong_carrier_is_rejected() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    for carrier in [OFFER_CARRIER + 1, 2 * KAS, OFFER_CARRIER / 2] {
        let mut s = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
        s.recipients[0].carrier = carrier;
        let p = forge(&offer, s);
        assert_engine_ok(&fx, &p);
        assert_diag(fx.verify(&offer, &p), Diag::CarrierMismatch);
    }
    // the offer's own carrier must respect the policy floor
    let low =
        kcc20_requirements(NET, &fx.token, 400, &address(&pubkey(MERCHANT)), OFFER_CARRIER / 2, TIMEOUT, Finality::Accepted).unwrap();
    let p = honest_payload_for(&fx, &low);
    assert_diag(fx.verify(&low, &p), Diag::CarrierMismatch);
}

#[test]
fn redirected_merchant_output_is_rejected() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    s.recipients[0].pubkey = pubkey(ATTACKER);
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // merchant output moved to another index while the digest still says 0
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    s.recipients = vec![
        TokenRecipient { pubkey: pubkey(ATTACKER), amount: 100, carrier: OFFER_CARRIER },
        TokenRecipient { pubkey: pubkey(MERCHANT), amount: 400, carrier: OFFER_CARRIER },
    ];
    let p = forge(&offer, s);
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // paymentOutputIndex outside the outputs
    let (fx, offer, p) = honest(400);
    let mut q = p.clone();
    q.payload.payment_output_index = 9;
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactPaymentOutput);
    // pointing at the payer's own change output breaks the digest (it commits to the index)
    let mut q = p.clone();
    q.payload.payment_output_index = 1;
    assert_diag(fx.verify(&offer, &q), Diag::InvalidAuthorization);
}

#[test]
fn extra_outputs_are_rejected() {
    let (fx, offer, _) = honest(400);
    let good = || std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    let kas_out = |key: u8| TransactionOutput { value: KAS, script_public_key: p2pk_spk(&pubkey(key)), covenant: None };
    // extra KAS output to the attacker (taken from the payer's change)
    let p = retx(&offer, good(), |tx, _, _| {
        let last = tx.outputs.len() - 1;
        tx.outputs[last].value -= KAS;
        tx.outputs.push(kas_out(ATTACKER));
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // a key that is no key of a payer INPUT is not payer change
    let p = retx(&offer, good(), |tx, _, _| {
        let last = tx.outputs.len() - 1;
        tx.outputs[last].value -= KAS;
        tx.outputs.push(kas_out(PAYER2));
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // two KAS change outputs
    let p = retx(&offer, good(), |tx, _, _| {
        let last = tx.outputs.len() - 1;
        tx.outputs[last].value -= KAS;
        tx.outputs.push(kas_out(PAYER));
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // extra token output to the attacker, engine-valid: inputs 405 = 400 (merchant) + 5 (attacker), no change
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 405)], vec![]);
    s.recipients.push(TokenRecipient { pubkey: pubkey(ATTACKER), amount: 5, carrier: OFFER_CARRIER });
    s.token_change_carrier = 0;
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // exactly spent inputs and a duplicate merchant token output
    let p = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 400)], vec![]), |tx, _, _| {
        let dup = tx.outputs[0].clone();
        tx.outputs.push(dup);
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // second merchant output next to the payment, engine-valid: 400 + 100 = 500 inputs
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 500)], vec![]);
    s.recipients.push(TokenRecipient { pubkey: pubkey(MERCHANT), amount: 100, carrier: OFFER_CARRIER });
    s.token_change_carrier = 0;
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // a second, payer-owned token change output
    let p = retx(&offer, good(), |tx, _, _| {
        let change = tx.outputs[1].clone();
        tx.outputs.push(change);
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn token_change_must_be_the_payers_and_conserve_the_remainder() {
    let (fx, offer, _) = honest(400);
    let tpl = template(TemplateId::Kcc20Ref8x8);
    // change to the attacker instead of the payer (engine-valid)
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    s.token_change = Some(pubkey(ATTACKER));
    let p = forge(&offer, s);
    assert_engine_ok(&fx, &p);
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // change smaller than the remainder (the payer diverts units)
    let p = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]), |tx, _, plans| {
        let SigPlan::TokenLeader { next_states, .. } = leader_plan(plans) else { unreachable!() };
        next_states[1].amount = 599;
        tx.outputs[1].script_public_key = next_states[1].spk_with(tpl);
    });
    assert_diag(fx.verify(&offer, &p), Diag::TokenConservation);
    // change whose owner scheme is not P2PK
    let p = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]), |tx, _, plans| {
        let SigPlan::TokenLeader { next_states, .. } = leader_plan(plans) else { unreachable!() };
        next_states[1].owner_scheme = 4;
        tx.outputs[1].script_public_key = next_states[1].spk_with(tpl);
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn leader_index_must_be_the_first_token_input() {
    let (fx, offer, _) = honest(500);
    let toks = || vec![fx.tok(PAYER, 300), fx.tok(PAYER, 250)];
    // control
    fx.verify(&offer, &forge(&offer, std_send(&offer, toks(), vec![]))).unwrap();
    // merchant output bound to the second token input
    let p = retx(&offer, std_send(&offer, toks(), vec![]), |tx, _, _| {
        tx.outputs[0].covenant.as_mut().unwrap().authorizing_input = 1;
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // change bound to another input than the leader
    let p = retx(&offer, std_send(&offer, toks(), vec![]), |tx, _, _| {
        tx.outputs[1].covenant.as_mut().unwrap().authorizing_input = 1;
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // merchant output bound to another covenant id
    let p = retx(&offer, std_send(&offer, toks(), vec![]), |tx, _, _| {
        tx.outputs[0].covenant.as_mut().unwrap().covenant_id = kaspa_consensus_core::Hash::from_bytes(OTHER_COV);
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // merchant output without any covenant binding
    let p = retx(&offer, std_send(&offer, toks(), vec![]), |tx, _, _| {
        tx.outputs[0].covenant = None;
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn merchant_owner_scheme_must_be_p2pk() {
    let (fx, offer, _) = honest(400);
    let tpl = template(TemplateId::Kcc20Ref8x8);
    // the merchant output is a covenant-owned (scheme 0x04) state under the merchant key bytes
    let p = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]), |tx, _, plans| {
        let SigPlan::TokenLeader { next_states, .. } = leader_plan(plans) else { unreachable!() };
        next_states[0].owner_scheme = 4;
        tx.outputs[0].script_public_key = next_states[0].spk_with(tpl);
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactPaymentOutput);
    // an offer whose payTo is not a Schnorr P2PK address cannot describe a P2PK-owned token output
    for addr in
        [Address::new(NET.prefix(), Version::ScriptHash, &[7u8; 32]), Address::new(NET.prefix(), Version::PubKeyECDSA, &[2u8; 33])]
    {
        let mut bad = offer.clone();
        bad.pay_to = addr.to_string();
        bad.extra.insert("payToScriptPublicKey".into(), json!(spk_to_hex(&kaspa_txscript::pay_to_address_script(&addr))));
        let mut q = honest_payload_for(&fx, &offer);
        q.accepted = bad.clone();
        assert_diag(fx.verify(&bad, &q), Diag::InvalidKaspaX402Binding);
    }
}

// =================================================================================================
// attacks: inputs
// =================================================================================================

#[test]
fn foreign_inputs_are_rejected() {
    let (fx, offer, p) = honest(400);
    let ord = template(TemplateId::KobAsk);
    // a KOB order UTXO (covenant P2SH that is not the token): "spend an order in the same transaction"
    let redeem = ord.redeem(&vec![0u8; ord.state_len]);
    let order_op = fx.chain.add_utxo(5 * KAS, pay_to_script_hash_script(&redeem), Some([0x99; 32]));
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, order_op, push_data(&redeem))), Diag::TokenTemplateMismatch);
    // an unknown P2SH
    let junk = vec![0x51u8, 0x75, 0x51];
    let junk_op = fx.chain.add_utxo(KAS, pay_to_script_hash_script(&junk), None);
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, junk_op, push_data(&junk))), Diag::TokenTemplateMismatch);
    // a P2SH input whose signature script is not push-only / is truncated / empty
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, junk_op, vec![0xac])), Diag::InvalidKaspaExactTransaction);
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, junk_op, vec![0x05, 1, 2])), Diag::InvalidKaspaExactTransaction);
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, junk_op, vec![])), Diag::InvalidKaspaExactTransaction);
    // a redeem that does not hash to the UTXO's script
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, junk_op, push_data(&[1, 2, 3]))), Diag::InvalidKaspaExactUtxo);
    // a covenant-bound output at a plain P2PK script
    let cov_p2pk = fx.chain.add_utxo(KAS, p2pk_spk(&pubkey(PAYER)), Some([0x99; 32]));
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, cov_p2pk, push_data(&[0u8; 65]))), Diag::InvalidKaspaExactUtxo);
    // scripts that are neither P2PK nor P2SH
    let odd = fx.chain.add_utxo(KAS, ScriptPublicKey::new(0, vec![0x51].into()), None);
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, odd, vec![])), Diag::InvalidKaspaExactUtxo);
    let v1 = fx.chain.add_utxo(KAS, ScriptPublicKey::new(1, vec![0x20; 34].into()), None);
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, v1, vec![])), Diag::InvalidKaspaExactUtxo);
    // a P2SH with the token's redeem but bound to no covenant id
    let t = fx.tok(PAYER, 5);
    let tpl = token_template(TemplateId::Kcc20Ref8x8);
    let stripped = fx.chain.add_utxo(CARRIER, t.state.spk_with(tpl), None);
    let redeem = t.state.redeem_with(tpl);
    assert_diag(fx.verify(&offer, &append_input(&fx, &p, stripped, push_data(&redeem))), Diag::TokenNotAllowlisted);
}

#[test]
fn token_inputs_beyond_the_program_slots_are_rejected() {
    let fx = Fx::with(token(TemplateId::Kcc20Ref, Custody::Unconditional), |_| {});
    let offer = fx.offer(500);
    let toks: Vec<_> = (0..3).map(|_| fx.tok(PAYER, 200)).collect();
    let p = pay(&fx, &offer, toks, vec![]);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.tx.inputs.len(), 3);
    // a fourth token input (a delegator script over a 4th UTXO)
    let t4 = fx.tok(PAYER, 200);
    let plan = SigPlan::TokenDelegator {
        template: TemplateId::Kcc20Ref,
        state: t4.state.as_kcc20().unwrap().clone(),
        witness: Witness::P2pk(pubkey(PAYER)),
    };
    let q = append_input(&fx, &p, Outpoint::new(t4.utxo.transaction_id, t4.utxo.index), plan.sigscript(Some(&[7u8; 65])).unwrap());
    let e = err_of(fx.verify(&offer, &q));
    assert_eq!(e.diag, Diag::InvalidKaspaExactTransaction, "{e}");
    assert!(e.message.contains("slots"), "{e}");
    // the SDK stays within the slots: it needs 4 x 200 for 700 units and refuses
    let toks: Vec<_> = (0..4).map(|_| fx.tok(PAYER, 200)).collect();
    let offer700 = fx.offer(700);
    let e = pay_kcc20_with(&offer700, &request_hash(), &keys(), toks, vec![], NOW_MS, &opts()).unwrap_err();
    assert_eq!(e.diag, Diag::TokenConservation, "{e}");
    assert!(e.message.contains("3 token inputs"), "{e}");
}

// =================================================================================================
// attacks: envelope, hints, chain state, resource bounds
// =================================================================================================

#[test]
fn tampered_accepted_and_request_hash_are_rejected() {
    let (fx, offer, p) = honest(400);
    // the payer alters the accepted requirement (cheaper amount, other recipient, other carrier)
    let mut q = p.clone();
    q.accepted.amount = "399".into();
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaX402Accepted);
    let mut q = p.clone();
    q.accepted.pay_to = address(&pubkey(ATTACKER));
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaX402Accepted);
    let mut q = p.clone();
    q.accepted.extra.get_mut("token").unwrap().as_object_mut().unwrap().insert("carrier".into(), json!("1"));
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaX402Accepted);
    // the payload's requestHash differs from the resource server's
    let other = hex(&sha256(b"another request"));
    let mut q = p.clone();
    q.payload.request_hash = other.clone();
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaX402Payload);
    assert_diag(verify_payment(&fx.ctx(), &offer, &p, &other), Diag::InvalidKaspaX402Payload);
    // a payment made for request A replayed for request B: rewriting requestHash breaks the commitment
    assert_diag(verify_payment(&fx.ctx(), &offer, &q, &other), Diag::InvalidAuthorization);
}

#[test]
fn payload_commitment_missing_mismatching_legacy_or_duplicated_is_rejected() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let digest = digest_for(&offer, 0, &expires_default());
    let send = |records: Vec<Record>| {
        let mut s = std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]);
        s.records = records;
        forge(&offer, s)
    };
    // control, and notes are allowed next to the commitment
    fx.verify(&offer, &send(vec![Record::X402 { reference: digest.to_vec() }])).unwrap();
    fx.verify(&offer, &send(vec![Record::Note { text: "hi".into() }, Record::X402 { reference: digest.to_vec() }])).unwrap();
    // missing payload, note only
    assert_diag(fx.verify(&offer, &send(vec![])), Diag::InvalidAuthorization);
    assert_diag(fx.verify(&offer, &send(vec![Record::Note { text: "hi".into() }])), Diag::InvalidAuthorization);
    // another digest, and the digest of another expiry
    assert_diag(fx.verify(&offer, &send(vec![Record::X402 { reference: vec![9; 32] }])), Diag::InvalidAuthorization);
    let other_exp = digest_for(&offer, 0, &iso_from_ms(NOW_MS + 10_000));
    assert_diag(fx.verify(&offer, &send(vec![Record::X402 { reference: other_exp.to_vec() }])), Diag::InvalidAuthorization);
    // two commitment records, a truncated reference
    let two = vec![Record::X402 { reference: digest.to_vec() }, Record::X402 { reference: digest.to_vec() }];
    assert_diag(fx.verify(&offer, &send(two)), Diag::InvalidAuthorization);
    assert_diag(fx.verify(&offer, &send(vec![Record::X402 { reference: digest[..16].to_vec() }])), Diag::InvalidAuthorization);
    // the legacy `X402:<hex>` text payload cannot carry a commitment
    let p = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]), |tx, _, _| {
        tx.payload = format!("X402:{}", hex(&digest)).into_bytes();
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidAuthorization);
}

#[test]
fn tampered_digest_and_authorization_shape_are_rejected() {
    let (fx, offer, p) = honest(400);
    let variants: Vec<AuthTweak> = vec![
        Box::new(|a| a.digest = hex(&[3u8; 32])),
        Box::new(|a| a.digest = "zz".into()),
        // extending the expiry after the fact changes the digest
        Box::new(|a| a.expires_at = iso_from_ms(NOW_MS + 60_000)),
        // wrong scheme, or signature material on a commitment
        Box::new(|a| a.version = "kaspa-x402-exact-request-authorization-v1".into()),
        Box::new(|a| a.signature = Some(hex(&[1u8; 64]))),
        Box::new(|a| a.input_index = Some(0)),
        // malformed timestamp
        Box::new(|a| a.expires_at = "tomorrow".into()),
    ];
    for (i, f) in variants.iter().enumerate() {
        let mut q = p.clone();
        f(&mut q.payload.authorization);
        let e = err_of(fx.verify(&offer, &q));
        assert_eq!(e.diag, Diag::InvalidAuthorization, "variant {i}: {e}");
    }
}

#[test]
fn expired_and_too_far_authorizations_are_rejected() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let p = honest_payload_for(&fx, &offer);
    fx.verify(&offer, &p).unwrap();
    // just before, exactly at and after the expiry
    fx.clock.set(NOW_MS + TIMEOUT * 1_000 - 1);
    fx.verify(&offer, &p).unwrap();
    fx.clock.set(NOW_MS + TIMEOUT * 1_000);
    assert_diag(fx.verify(&offer, &p), Diag::ExpiredAuthorization);
    fx.clock.set(NOW_MS + TIMEOUT * 1_000 + 5_000);
    let e = err_of(fx.verify(&offer, &p));
    assert_eq!((e.diag, e.reason), (Diag::ExpiredAuthorization, Reason::InvalidTransactionState));
    // an authorization that outlives maxTimeoutSeconds
    fx.clock.set(NOW_MS);
    let long = Kcc20Options { ttl_seconds: Some(TIMEOUT + 3_600), ..opts() };
    let p = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS, &long).unwrap();
    assert_diag(fx.verify(&offer, &p), Diag::AuthorizationExceedsMaxTimeout);
}

#[test]
fn forged_utxo_hints_and_chain_state_are_rejected() {
    let (fx, offer, p) = honest(400);
    // hint amount differs from the chain
    let q = edit_safe(&p, |s| s.inputs[0].utxo.as_mut().unwrap().amount = (CARRIER + 1).to_string());
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactUtxo);
    // hint covenant id differs from the chain
    let q = edit_safe(&p, |s| s.inputs[0].utxo.as_mut().unwrap().covenant_id = None);
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactUtxo);
    let q = edit_safe(&p, |s| s.inputs[0].utxo.as_mut().unwrap().covenant_id = Some(hex(&OTHER_COV)));
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactUtxo);
    // hint script is not the script the outpoint is locked by (wallet lookup key wrong)
    let q = edit_safe(&p, |s| s.inputs[0].utxo.as_mut().unwrap().script_public_key = spk_to_hex(&p2pk_spk(&pubkey(ATTACKER))));
    let e = err_of(fx.verify(&offer, &q));
    assert_eq!((e.diag, e.reason), (Diag::InvalidKaspaExactUtxo, Reason::InvalidTransactionState));
    // no hint at all
    let q = edit_safe(&p, |s| s.inputs[0].utxo = None);
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactUtxo);
    // a stated id that is not the recomputed one
    let q = edit_safe(&p, |s| s.id = Some(hex(&[1u8; 32])));
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactTransactionId);
    // unknown outpoint (never existed)
    let q = edit_safe(&p, |s| {
        s.id = None;
        s.inputs[0].previous_outpoint.index = 77;
    });
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactUtxo);
    // spent input
    let (tx, _) = parse_payload_tx(&fx, &p);
    fx.chain.spend_externally(&kob_x402::common::outpoint_of(&tx.inputs[0]));
    let e = err_of(fx.verify(&offer, &p));
    assert_eq!((e.diag, e.reason), (Diag::InvalidKaspaExactUtxo, Reason::InvalidTransactionState));
    // node outage fails closed and is retryable
    let (fx, offer, p) = honest(400);
    fx.chain.set_unavailable(true);
    let e = err_of(fx.verify(&offer, &p));
    assert_eq!((e.diag, e.retryable), (Diag::NodeUnavailable, true));
}

#[test]
fn duplicate_inputs_are_rejected() {
    let (fx, offer, _) = honest(400);
    let p = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]), |tx, entries, plans| {
        let (i, e, pl) = (tx.inputs[0].clone(), entries[0].clone(), plans[0].clone());
        tx.inputs.push(i);
        entries.push(e);
        plans.push(match pl {
            SigPlan::TokenLeader { template, state, witness, .. } => SigPlan::TokenDelegator { template, state, witness },
            o => o,
        });
    });
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactTransaction);
}

#[test]
fn transaction_shape_and_resource_bounds_are_enforced() {
    let (fx, offer, p) = honest(400);
    // gas and lock time
    for which in [0, 1] {
        let q = retx(&offer, std_send(&offer, vec![fx.tok(PAYER, 1_000)], vec![]), |tx, _, _| {
            if which == 0 {
                tx.gas = 1;
            } else {
                tx.lock_time = 5;
            }
        });
        assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactTransaction);
    }
    // version 0, foreign subnetwork, non-canonical numbers, garbage
    assert_diag(fx.verify(&offer, &edit_safe(&p, |s| s.version = 0)), Diag::InvalidKaspaExactTransaction);
    assert_diag(
        fx.verify(&offer, &edit_safe(&p, |s| s.subnetwork_id = "01".to_string() + &"0".repeat(38))),
        Diag::InvalidKaspaExactTransaction,
    );
    assert_diag(fx.verify(&offer, &edit_safe(&p, |s| s.storage_mass = "007".into())), Diag::InvalidKaspaExactTransaction);
    let mut q = p.clone();
    q.payload.transaction = "{\"not\":\"a tx\"}".into();
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaExactTransaction);
    // oversized artifact / signature script / payload / output count, fee bound
    let limits: Vec<(PolicyTweak, Diag)> = vec![
        (Box::new(|p| p.limits.max_tx_json_bytes = 1_000), Diag::InvalidKaspaExactTransaction),
        (Box::new(|p| p.limits.max_signature_script_bytes = 1_000), Diag::InvalidKaspaExactTransaction),
        (Box::new(|p| p.limits.max_payload_bytes = 10), Diag::InvalidKaspaExactTransaction),
        (Box::new(|p| p.limits.max_outputs = 2), Diag::InvalidKaspaExactTransaction),
        (Box::new(|p| p.limits.max_fee_sompi = 1_000), Diag::InvalidKaspaExactFee),
    ];
    for (i, (f, diag)) in limits.iter().enumerate() {
        let mut small = fx.policy.clone();
        f(&mut small);
        let e = err_of(verify_with(&fx, &small, &offer, &p));
        assert_eq!(e.diag, *diag, "limit {i}: {e}");
    }
}

#[test]
fn payment_identifier_rules() {
    let (fx, offer, p) = honest(400);
    let mut q = p.clone();
    q.extensions = None;
    assert_diag(fx.verify(&offer, &q), Diag::MissingKaspaPaymentIdentifier);
    q.extensions = Some(json!({ "payment-identifier": { "info": { "id": "short" } } }));
    assert_diag(fx.verify(&offer, &q), Diag::InvalidKaspaPaymentIdentifier);
    // a deployment that does not require it
    let mut lax = fx.policy.clone();
    lax.require_payment_identifier = false;
    q.extensions = None;
    assert_eq!(verify_with(&fx, &lax, &offer, &q).unwrap().payment_identifier, None);
}

#[test]
fn owner_and_funding_signatures_are_executed_by_the_engine() {
    // token-only payment tampered after signing: the KCC-20 owner witness no longer verifies
    let (fx, offer, p) = honest(400);
    let (mut tx, entries) = parse_payload_tx(&fx, &p);
    let last = tx.outputs.len() - 1;
    tx.outputs[last].value -= 1; // one sompi more fee: every SIGHASH_ALL changes
    tx.set_storage_mass(masses(&tx, &entries).storage);
    let e = err_of(fx.verify(&offer, &with_tx(&p, &tx, &entries)));
    assert_eq!(e.diag, Diag::InvalidKaspaExactTransaction, "{e}");
    assert!(e.message.contains("input 0"), "{e}");
    // a funding input with a bad signature is a signature failure
    let fx = Fx::new();
    let offer = fx.offer(400);
    let tiny = fx.tok_carrier(PAYER, 400, OFFER_CARRIER);
    let f = fx.funding(PAYER, 2 * KAS);
    let p = pay(&fx, &offer, vec![tiny], vec![f]);
    fx.verify(&offer, &p).unwrap();
    let (mut tx, entries) = parse_payload_tx(&fx, &p);
    tx.inputs[1].signature_script = push_data(&[[0u8; 64].as_slice(), &[0x01]].concat()); // an invalid SIGHASH_ALL signature
    assert_diag(fx.verify(&offer, &with_tx(&p, &tx, &entries)), Diag::InvalidKaspaExactSignature);
    // a fee below the relay floor
    let (fx, offer, p) = honest(400);
    let (mut tx, entries) = parse_payload_tx(&fx, &p);
    let last = tx.outputs.len() - 1;
    tx.outputs[last].value += 7_000_000; // fee down: below the relay floor (checked before the scripts run)
    tx.set_storage_mass(masses(&tx, &entries).storage);
    assert_diag(fx.verify(&offer, &with_tx(&p, &tx, &entries)), Diag::InvalidKaspaExactFee);
}

// =================================================================================================
// the founder's two points
// =================================================================================================

#[test]
fn founder_point_1_custody_is_an_enforced_field_not_a_label() {
    use Custody::{IssuerControlled as IC, Unconditional as UC};
    // (allowlist custody, offer custody, policy opt-in) -> outcome
    let cases = [
        (UC, UC, false, Ok(UC)),
        (UC, UC, true, Ok(UC)),
        (IC, IC, true, Ok(IC)),
        (IC, IC, false, Err(Diag::TokenCustodyPolicy)),
        // an offer cannot upgrade or downgrade the allowlist's classification
        (IC, UC, true, Err(Diag::TokenCustodyPolicy)),
        (IC, UC, false, Err(Diag::TokenCustodyPolicy)),
        (UC, IC, true, Err(Diag::TokenCustodyPolicy)),
        (UC, IC, false, Err(Diag::TokenCustodyPolicy)),
    ];
    for (allow, offered, opt_in, want) in cases {
        let fx = Fx::with(token(TemplateId::Kcc20Ref8x8, allow), |p| p.allow_issuer_controlled = opt_in);
        let offer = fx.offer_for(&token(TemplateId::Kcc20Ref8x8, offered), 400);
        let p = honest_payload_for(&fx, &offer);
        match (fx.verify(&offer, &p), want) {
            (Ok(v), Ok(c)) => {
                assert_eq!(v.custody, Some(c), "{allow:?}/{offered:?}/{opt_in}");
                assert_eq!(v.response_extension["token"]["custody"], c.as_str());
            }
            (Err(e), Err(d)) => assert_eq!(e.diag, d, "{allow:?}/{offered:?}/{opt_in}: {e}"),
            (got, want) => panic!("{allow:?}/{offered:?}/{opt_in}: got {:?}, want {want:?}", got.map(|v| v.custody)),
        }
    }
    // the custody string itself must be one of the two classes
    let fx = Fx::new();
    let mut offer = fx.offer(400);
    offer.extra.get_mut("token").unwrap().as_object_mut().unwrap().insert("custody".into(), json!("mostly-unconditional"));
    let mut q = honest_payload_for(&fx, &fx.offer(400));
    q.accepted = offer.clone();
    assert_diag(fx.verify(&offer, &q), Diag::TokenCustodyPolicy);
    // the registry-derived allowlist classifies custody from the template capabilities alone: the shipped listed tokens are the
    // KRON family, whose is_minter path is a declared mint authority, so every one is issuer-controlled (review B2 condition C5)
    let reg = kob_protocol::registry::Registry::default_registry();
    let list = kob_x402::policy::TokenAllowlist::from_registry(&reg);
    assert_eq!(list.iter().count(), 8);
    for t in list.iter() {
        let entry = reg.tokens.iter().find(|k| k.ticker == t.ticker).unwrap();
        assert!(!reg.template(&entry.template_id).unwrap().capabilities.is_empty(), "{}", t.ticker);
        assert_eq!((t.family, t.custody), (kob_protocol::registry::Family::Kron, Custody::IssuerControlled), "{}", t.ticker);
    }
}

#[test]
fn founder_point_2_owner_witnesses_alone_authorize_the_payment_through_the_commitment() {
    let fx = Fx::new();
    let offer = fx.offer(500);
    // two token inputs of two different owners, NO plain P2PK input, the fee paid from the carriers
    let (built, tpl) =
        build_kcc20_unsigned(&offer, &request_hash(), vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300)], vec![], NOW_MS, &opts()).unwrap();
    assert_eq!(built.plans.len(), 2);
    for plan in &built.plans {
        assert!(matches!(plan, SigPlan::TokenLeader { .. } | SigPlan::TokenDelegator { .. }), "no plain P2PK signer exists");
        assert!(plan.signer().is_some(), "the token owner's witness signature is the authorization");
    }
    let signers: Vec<_> = built.sign.iter().map(|r| r.pubkey).collect();
    assert_eq!(signers, vec![pubkey(PAYER), pubkey(PAYER2)]);
    let sigs = sign_locally(&built, &keys()).unwrap();
    let p = finish_kcc20(&built, &tpl, &sigs).unwrap();
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    assert!(v.entries.iter().all(|e| kob_x402::common::p2pk_key(&e.script_public_key).is_none()));
    // The binding's Schnorr-signed authorization has nobody to sign it here; the commitment lives in
    // the payload that both witnesses' SIGHASH_ALL cover. (a) the verifier recomputes it:
    assert_eq!(tpl.authorization.digest, hex(&digest_for(&offer, 0, &tpl.authorization.expires_at)));
    // (b) a payer who tampers the commitment fails: another digest in the payload...
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300)], vec![]);
    s.records = vec![Record::X402 { reference: vec![0x42; 32] }];
    assert_diag(fx.verify(&offer, &forge(&offer, s)), Diag::InvalidAuthorization);
    // ...or the authorization changed to a digest of a different request while the tx keeps the old
    let mut q = p.clone();
    q.payload.authorization.digest = hex(&digest_for(&offer, 0, &iso_from_ms(NOW_MS + 5_000)));
    assert_diag(fx.verify(&offer, &q), Diag::InvalidAuthorization);
    // (c) swapping the commitment inside the signed transaction (consistent authorization, so every
    //     offline check passes) is caught by the owners' witnesses: SIGHASH_ALL covers the payload
    let exp2 = iso_from_ms(NOW_MS + 60_000);
    let d2 = digest_for(&offer, 0, &exp2);
    let (mut tx, entries) = parse_payload_tx(&fx, &p);
    tx.payload = kob_x402::common::commitment_payload(&d2);
    let mut q = with_tx(&p, &tx, &entries);
    q.payload.authorization = Authorization {
        version: AUTH_VERSION_PAYLOAD.into(),
        input_index: None,
        expires_at: exp2.clone(),
        digest: hex(&d2),
        signature: None,
    };
    let e = err_of(fx.verify(&offer, &q));
    assert_eq!(e.diag, Diag::InvalidKaspaExactTransaction, "{e}");
    assert!(e.message.contains("input"), "{e}");
    // whereas re-signing the other payload takes the owners' keys, and then it is a valid, consistent payment
    let mut s = std_send(&offer, vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300)], vec![]);
    s.records = vec![Record::X402 { reference: d2.to_vec() }];
    let built2 = build(&Action::SendTokens(s)).unwrap();
    let auth2 =
        Authorization { version: AUTH_VERSION_PAYLOAD.into(), input_index: None, expires_at: exp2, digest: hex(&d2), signature: None };
    fx.verify(&offer, &wrap(&offer, &built2, auth2, 0)).unwrap();
}

// =================================================================================================
// payer SDK: selection, preflight, revoke
// =================================================================================================

#[test]
fn preflight_runs_the_verifier_against_the_payers_own_policy() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let p = honest_payload_for(&fx, &offer);
    preflight_kcc20(&fx.chain, &fx.clock, &fx.policy, &offer, &p, &request_hash()).unwrap();
    // a payer that does not trust this token refuses to disclose the payment
    let strict = Policy::new(NET);
    let e = preflight_kcc20(&fx.chain, &fx.clock, &strict, &offer, &p, &request_hash()).unwrap_err();
    assert_eq!(e.diag, Diag::TokenNotAllowlisted);
    // an unaffordable fee is refused before signing
    let tight = Kcc20Options { max_fee_sompi: 1_000, ..opts() };
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS, &tight).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaExactFee);
    // a missing local key: nothing is spendable
    let mut only_other = BTreeMap::new();
    only_other.insert(pubkey(ATTACKER), secret(ATTACKER));
    let e = pay_kcc20(&offer, &request_hash(), &only_other, vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS).unwrap_err();
    assert_eq!(e.diag, Diag::TokenConservation, "{e}");
    // pay_kcc20 (default options) declares a fresh random payment id, which the default policy accepts
    let p = pay_kcc20(&offer, &request_hash(), &keys(), vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS).unwrap();
    let id = fx.verify(&offer, &p).unwrap().payment_identifier.expect("a default payment id");
    assert!(id.starts_with("pay_") && id.len() == 52, "{id}");
    let q = pay_kcc20(&offer, &request_hash(), &keys(), vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS).unwrap();
    assert_ne!(fx.verify(&offer, &q).unwrap().payment_identifier.as_deref(), Some(id.as_str()));
    // an explicit id outside ^[A-Za-z0-9_-]{16,128}$ is refused before signing
    let bad = Kcc20Options { payment_id: Some("short".into()), ..opts() };
    let e = pay_kcc20_with(&offer, &request_hash(), &keys(), vec![fx.tok(PAYER, 1_000)], vec![], NOW_MS, &bad).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaPaymentIdentifier);
}

#[test]
fn revoke_by_self_spending_a_token_input_makes_the_payment_unspendable() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let t = fx.tok(PAYER, 1_000);
    let p = pay(&fx, &offer, vec![t.clone()], vec![]);
    let v = fx.verify(&offer, &p).unwrap();
    let token_ref = TokenRef { covenant_id: COV, program: TemplateId::Kcc20Ref8x8 };
    let signed = revoke_kcc20(&RevokeInput::Token { token: token_ref.clone(), utxo: t.clone() }, &keys(), None).unwrap();
    let (rtx, rentries) = signed.tx.to_tx().unwrap();
    kob_protocol::verify::validate(&rtx, &rentries).unwrap();
    // revoked before the merchant broadcasts: the payment conflicts with the mempool spend
    fx.chain.submit(&rtx).unwrap();
    assert!(matches!(fx.chain.submit(&v.tx), Err(kob_x402::chain::SubmitError::Conflict(_))));
    fx.chain.mine(0);
    let e = err_of(fx.verify(&offer, &p));
    assert_eq!((e.diag, e.reason), (Diag::InvalidKaspaExactUtxo, Reason::InvalidTransactionState));
    // the payer got its tokens back
    let back = Kcc20State::p2pk(1_000, pubkey(PAYER), EXT).spk_with(template(TemplateId::Kcc20Ref8x8));
    assert_eq!(fx.chain.unspent_of(&back).len(), 1);
    // a covenant-owned input cannot be revoked through this helper
    let bad = fx.tok_state(TemplateId::Kcc20Ref8x8, COV, Kcc20State::custody(5, [0xc0; 32], EXT), CARRIER);
    assert!(revoke_kcc20(&RevokeInput::Token { token: token_ref, utxo: bad }, &keys(), None).is_err());
}

#[test]
fn revoke_by_self_spending_the_funding_input() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let tiny = fx.tok_carrier(PAYER, 400, OFFER_CARRIER);
    let f = fx.funding(PAYER, 2 * KAS);
    let p = pay(&fx, &offer, vec![tiny], vec![f.clone()]);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.tx.inputs.len(), 2);
    let signed = revoke_kcc20(&RevokeInput::Kas(f.clone()), &keys(), None).unwrap();
    let (rtx, rentries) = signed.tx.to_tx().unwrap();
    kob_protocol::verify::validate(&rtx, &rentries).unwrap();
    assert_eq!(rtx.outputs.len(), 1);
    assert_eq!(rtx.outputs[0].script_public_key, p2pk_spk(&pubkey(PAYER)));
    assert!(signed.fee.fee >= signed.fee.min_fee);
    fx.chain.submit(&rtx).unwrap();
    fx.chain.mine(0);
    assert_diag(fx.verify(&offer, &p), Diag::InvalidKaspaExactUtxo);
    // no key, no revocation
    let f2 = fx.funding(PAYER, 2 * KAS);
    assert!(revoke_kcc20(&RevokeInput::Kas(f2), &BTreeMap::new(), None).is_err());
    // a UTXO too small to pay its own fee
    let dust = fx.funding(PAYER, 1_000);
    assert!(revoke_kcc20(&RevokeInput::Kas(dust), &keys(), None).is_err());
}

// ------------------------------------------------------------------------------------ program families

#[test]
fn a_kaspacom_template_token_is_an_ordinary_kcc20_merchant_asset() {
    // KaspaCom's third-party program (25.5 KB, 8 / 8 slots) needs no special case: same profile, same rules
    let kc = TemplateId::Kcc20KaspaCom025;
    let fx = Fx::with(token(kc, Custody::Unconditional), |_| {});
    let offer = fx.offer(400);
    assert_eq!(offer.extra["token"]["templateHash"], hex(&template(kc).hash));
    let t = fx.tok(PAYER, 1_000);
    let p = pay(&fx, &offer, vec![t], vec![]);
    assert_engine_ok(&fx, &p);
    let v = fx.verify(&offer, &p).unwrap();
    assert_eq!(v.kind, PaymentKind::Kcc20);
    let txid = fx.chain.submit(&v.tx).unwrap();
    fx.chain.mine(0);
    assert!(fx.chain.is_accepted(&txid));
    let u = fx.chain.utxo(&v.merchant_output.outpoint).unwrap();
    assert_eq!(u.script_public_key, Kcc20State::p2pk(400, pubkey(MERCHANT), EXT).spk_with(template(kc)));
    // a token on the reference 8x8 program cannot pass for it: the pinned program is the allowlist's
    let other = fx.offer_for(&token(TemplateId::Kcc20Ref8x8, Custody::Unconditional), 400);
    let p = pay(
        &fx,
        &other,
        vec![fx.tok_state(TemplateId::Kcc20Ref8x8, COV, Kcc20State::p2pk(1_000, pubkey(PAYER), EXT), CARRIER)],
        vec![],
    );
    assert_diag(fx.verify(&other, &p), Diag::TokenTemplateMismatch);
}

#[test]
fn a_kron_token_is_not_an_asset_of_the_kcc20_profile() {
    let kron = AllowedToken::new(COV, TemplateId::KronToken2433, [0; 32], Custody::Unconditional, "KRN", 0);
    // merchant side: no offer for a KRON token
    let e = kcc20_requirements(NET, &kron, 5, &address(&pubkey(MERCHANT)), OFFER_CARRIER, TIMEOUT, Finality::Accepted).unwrap_err();
    assert_eq!(e.diag, Diag::TokenNotAllowlisted, "{e}");
    // verifier side: an offer that names the KRON program for an allowlisted KRON token is refused
    let fx = Fx::with(token(TemplateId::Kcc20Ref8x8, Custody::Unconditional), |p| {
        p.tokens = Default::default();
        p.tokens.insert(kron.clone()).unwrap();
    });
    let honest = Fx::new();
    let mut offer = honest.offer(400);
    offer.extra.get_mut("token").unwrap()["templateHash"] = json!(hex(&token_template(TemplateId::KronToken2433).hash));
    let p = pay(&honest, &honest.offer(400), vec![honest.tok(PAYER, 1_000)], vec![]);
    let mut t = p.clone();
    t.accepted = offer.clone();
    let e = err_of(fx.verify(&offer, &t));
    assert_eq!(e.diag, Diag::TokenTemplateMismatch, "{e}");
}

// =================================================================================================
// every payer signature is SIGHASH_ALL (profile step 8 and section 5.1; conformance kcc20-neg-owner)
// =================================================================================================

/// `p` verifies; then every payer signature in turn is made under every other hash type. The script engine accepts
/// each valid-typed variant (the negatives are real, correctly signed transactions), so only the verifier's own
/// hash-type check can refuse them.
#[track_caller]
fn assert_sighash_all_enforced(fx: &Fx, offer: &PaymentRequirements, p: &PaymentPayload, expect_signers: usize) {
    let (tx, entries) = parse_payload_tx(fx, p);
    let ss = signers(&tx, &entries, &keys());
    assert_eq!(ss.len(), expect_signers, "payer signatures found in the transaction");
    fx.verify(offer, p).unwrap_or_else(|e| panic!("the honest payment must verify: {e}"));
    for s in &ss {
        // control: the same re-signing under SIGHASH_ALL verifies, so the helper produces valid payments
        let again = resigned(&tx, &entries, s, 0x01);
        fx.verify(offer, &with_tx(p, &again, &entries)).unwrap_or_else(|e| panic!("input {}: re-signed under ALL: {e}", s.input));
        for t in NON_ALL {
            let bad = resigned(&tx, &entries, s, t);
            kob_protocol::verify::validate(&bad, &entries)
                .unwrap_or_else(|e| panic!("input {}: the engine must accept hash type {t:#04x}: {e}", s.input));
            let e = err_of(fx.verify(offer, &with_tx(p, &bad, &entries)));
            assert_eq!(
                (e.reason, e.diag),
                (Reason::InvalidPayload, Diag::TokenOwnerScheme),
                "input {} hash type {t:#04x}: {e}",
                s.input
            );
            assert!(e.message.contains(&format!("input {}", s.input)), "{e}");
        }
        for t in BOGUS {
            let e = err_of(fx.verify(offer, &with_tx(p, &tampered_byte(&tx, s, t), &entries)));
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} trailing byte {t:#04x}: {e}", s.input);
        }
    }
}

#[test]
fn a_token_owner_witness_must_use_sighash_all() {
    // one owner: the leader's witness
    let fx = Fx::new();
    let offer = fx.offer(400);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 1_000)], vec![]);
    assert_sighash_all_enforced(&fx, &offer, &p, 1);
}

#[test]
fn a_delegator_owner_witness_must_use_sighash_all() {
    // two owners: the leader's and a delegator's witness are both checked
    let fx = Fx::new();
    let offer = fx.offer(500);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300)], vec![]);
    assert_sighash_all_enforced(&fx, &offer, &p, 2);
    // three inputs, two delegators
    let fx = Fx::new();
    let offer = fx.offer(700);
    let p = pay(&fx, &offer, vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300), fx.tok(PAYER, 200)], vec![]);
    assert_sighash_all_enforced(&fx, &offer, &p, 3);
}

#[test]
fn a_funding_input_signature_must_use_sighash_all() {
    let fx = Fx::new();
    let offer = fx.offer(400);
    let tiny = fx.tok_carrier(PAYER, 400, OFFER_CARRIER);
    let f = fx.funding(PAYER, 2 * KAS);
    let p = pay(&fx, &offer, vec![tiny], vec![f]);
    assert_sighash_all_enforced(&fx, &offer, &p, 2);
}

#[test]
fn the_hash_type_rule_holds_for_every_token_program() {
    for program in [
        TemplateId::Kcc20Ref,
        TemplateId::Kcc20Ref4x5,
        TemplateId::Kcc20Ref8x8,
        TemplateId::Kcc20Ref16x16,
        TemplateId::Kcc20P2,
        TemplateId::Kcc20KaspaCom025,
    ] {
        let fx = Fx::with(token(program, Custody::Unconditional), |_| {});
        let offer = fx.offer(500);
        let p = pay(&fx, &offer, vec![fx.tok(PAYER, 300), fx.tok(PAYER2, 300)], vec![]);
        assert_sighash_all_enforced(&fx, &offer, &p, 2);
    }
}

#[test]
fn the_refusal_names_the_hash_type() {
    // a SIGHASH_NONE signature is a valid engine input; the verifier says what is wrong with it
    let (fx, offer, p) = honest(400);
    let (tx, entries) = parse_payload_tx(&fx, &p);
    let s = signers(&tx, &entries, &keys())[0];
    let none = resigned(&tx, &entries, &s, 0x02);
    let e = err_of(fx.verify(&offer, &with_tx(&p, &none, &entries)));
    assert_eq!((e.diag, e.message.contains("SIGHASH_NONE")), (Diag::TokenOwnerScheme, true), "{e}");
    let any = resigned(&tx, &entries, &s, 0x81);
    let e = err_of(fx.verify(&offer, &with_tx(&p, &any, &entries)));
    assert!(e.message.contains("SIGHASH_ALL|ANYONECANPAY"), "{e}");
}
