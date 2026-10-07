//! Swap-and-pay across token families: a KRON token or a KaspaCom-template KCC-20 token as the pay
//! asset, KRON bids and KCC-20 asks in one transaction, KaspaCom-template merchant tokens.
//!
//! Every accepted transaction is submitted to a `MockChain`, which runs each order covenant and each
//! token program through the rusty-kaspa engine (enforced budgets, exact storage mass, fee floor).
//! The world (tokens, order UTXOs, offers) is in `families_support`; nothing here special-cases a
//! program: templates come from `kob_protocol::artifacts`, states from `TokenState`.
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;
#[path = "families_support/mod.rs"]
mod support;

use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::{Batch, Leg, Payment};
use kob_protocol::payload::Record;
use kob_protocol::registry::{Capability, Escrow, Family, Registry, ReviewStatus, Source, Status, Template, Token};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{Kcc20State, KronState, TokenState};
use kob_protocol::tx::{assemble, masses, sighash, sign_locally, spk_to_string, SigPlan, SignRequest, TxJson};
use kob_x402::canonical::requirements_hash;
use kob_x402::chain::ChainView;
use kob_x402::chain::Outpoint;
use kob_x402::client::swap::{
    prepare_swap, swap_requirements, MerchantGain, OrderRef, PayAssetSpec, PreparedSwap, SwapOfferParams, SwapOptions, SwapPayment,
};
use kob_x402::client::token::kcc20_requirements;
use kob_x402::common::{iso_from_ms, payload_commit_digest, PayloadCommit};
use kob_x402::error::{Diag, X402Error};
use kob_x402::policy::{AllowedToken, Custody, TokenAllowlist};
use kob_x402::safe_tx::SafeTx;
use kob_x402::swap::{order_template_of, redeem_of};
use kob_x402::verify::{verify_payment, PaymentKind, Verified, VerifyCtx};
use kob_x402::wire::{hex, parse_hash32, Finality, Network, PaymentPayload, PaymentRequirements};

use common::{keys, pk, CARRIER, EXT, KAS, NOW, WHOLE};
use support::*;

// ------------------------------------------------------------------------------------------ helpers

fn diag(r: Result<Verified, X402Error>) -> Diag {
    match r {
        Ok(_) => panic!("verification unexpectedly succeeded"),
        Err(e) => e.diag,
    }
}

fn err_of<T: std::fmt::Debug>(r: Result<T, X402Error>) -> X402Error {
    r.expect_err("expected a failure")
}

/// The order-kind template of input `i` of a signed swap.
fn kind_of_input(v: &Verified, i: usize) -> Option<TemplateId> {
    order_template_of(redeem_of(&v.tx.inputs[i].signature_script)?)
}

/// Outputs of `tx` that are token outputs of `t` in exactly `state`.
fn token_outputs_in(tx: &Transaction, t: Tok, state: &TokenState) -> usize {
    let spk = t.spk(state);
    tx.outputs.iter().filter(|o| o.script_public_key == spk && o.covenant.map(|c| c.covenant_id.as_bytes()) == Some(t.cov)).count()
}

/// Commitment digest for a hand-built swap transaction.
fn digest_for(offer: &PaymentRequirements, pay_asset: &str, idx: u32, expires_at: &str) -> [u8; 32] {
    payload_commit_digest(&PayloadCommit {
        network: Network::Testnet10,
        profile: offer.profile().unwrap(),
        route_pay_asset: Some(pay_asset),
        asset: &offer.asset,
        amount: &offer.amount,
        pay_to: &offer.pay_to,
        pay_to_spk_hex: offer.extra_str("payToScriptPublicKey").unwrap(),
        payment_output_index: idx,
        requirements_hash: &requirements_hash(offer).unwrap(),
        request_hash: &parse_hash32(RH).unwrap(),
        expires_at,
    })
    .unwrap()
}

/// A payment from a hand-built batch (the payer SDK is bypassed): `claimed_idx` is the claimed output.
fn manual(offer: &PaymentRequirements, pay_asset: &str, mut batch: Batch, k: usize, claimed_idx: u32) -> PaymentPayload {
    let expires_at = iso_from_ms(NOW_MS + 60_000);
    let digest = digest_for(offer, pay_asset, claimed_idx, &expires_at);
    batch.records = vec![Record::X402 { reference: digest.to_vec() }];
    let built = kob_protocol::build::build_batch(&batch, &kob_protocol::budget::lookup).unwrap();
    let orders: Vec<Outpoint> = built.tx.inputs.iter().take(k).map(|i| Outpoint::new(i.transaction_id, i.index)).collect();
    let prep = PreparedSwap {
        built: built.clone(),
        offer: offer.clone(),
        request_hash: RH.into(),
        pay_asset: pay_asset.into(),
        orders,
        payment_output_index: claimed_idx,
        digest,
        expires_at,
        payer_spent: 0,
        kas_spent: 0,
        payer_address: None,
        warnings: vec![],
        payment_identifier: None,
    };
    let sigs = sign_locally(&built, &keys()).unwrap();
    let (tx, entries) = assemble(&built, &sigs).unwrap();
    prep.assemble_payload(&tx, &entries).unwrap()
}

fn base_batch(legs: Vec<Leg>) -> Batch {
    Batch {
        lock_time: NOW,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: None,
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![],
        change: Some(pk(PAYER)),
        records: vec![],
        fee: Default::default(),
    }
}

/// A payer that builds a malformed transaction and signs it: `mutate` edits the unsigned transaction,
/// its UTXO entries and the signing plans; every sign request is recomputed and signed.
fn attack(prep: &PreparedSwap, mutate: impl FnOnce(&mut Transaction, &mut Vec<UtxoEntry>, &mut Vec<SigPlan>)) -> PaymentPayload {
    let mut built = prep.built.clone();
    let (mut tx, mut entries) = built.tx.to_tx().unwrap();
    let mut plans = built.plans.clone();
    mutate(&mut tx, &mut entries, &mut plans);
    tx.set_storage_mass(masses(&tx, &entries).storage);
    built.tx = TxJson::from_tx(&tx, &entries);
    built.plans = plans.clone();
    built.sign = plans
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            p.signer().map(|pk| SignRequest {
                input_index: i,
                pubkey: pk,
                sighash_type: 1,
                sighash: sighash(&tx, &entries, i),
                redeem_script: p.redeem(),
            })
        })
        .collect();
    let sigs = sign_locally(&built, &keys()).unwrap();
    let (tx2, entries2) = assemble(&built, &sigs).unwrap();
    PreparedSwap { built, ..prep.clone() }.assemble_payload(&tx2, &entries2).unwrap()
}

fn prepare(
    w: &World,
    offer: &PaymentRequirements,
    quote: &kob_x402::client::swap::Quote,
    funds: &kob_x402::client::swap::PayerFunds,
) -> PreparedSwap {
    prepare_swap(&w.policy, offer, quote, RH, funds, NOW_MS, &SwapOptions::default()).unwrap()
}

/// The fee of a settled swap is the node's relay floor for its mass (no priority premium).
fn assert_floor_fee(v: &Verified) {
    let mass = masses(&v.tx, &v.entries);
    let floor = kob_protocol::tx::min_fee(&mass, kob_protocol::tx::MIN_FEE_RATE);
    assert!(v.fee >= floor, "fee {} below the floor {floor}", v.fee);
    // tiny KAS change is folded into the fee by design, so allow the folded change on top
    assert!(v.fee < floor + 10_000_000, "fee {} is far above the floor {floor}", v.fee);
}

// ------------------------------------------------------------------------------------------ accepted payments

#[test]
fn a_kron_pay_asset_pays_a_kas_merchant() {
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[K]);
    let quote = quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE)]);
    let prep = prepare(&w, &offer, &quote, &w.funds(vec![held.clone()], true));
    // the KRON token needs no signature of its own: the payer's KAS input is the only signer
    assert_eq!(prep.sign_requests().len(), 1);
    let p = prep.complete(&prep.sign_with(&keys()).unwrap()).unwrap();
    assert_eq!(p.payer_spent, 3 * WHOLE as u64);
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToKas);
    assert_eq!(v.merchant_output.amount, KAS);
    assert_eq!(v.merchant_output.script_public_key, p2pk_spk(&pk(MERCHANT)));
    assert_eq!(v.order_inputs, vec![op(&bid.utxo)]);
    // order input 0 is a KRON bid, spent with the KRON token program at input 1
    assert_eq!(kind_of_input(&v, 0), Some(TemplateId::KobBidKron));
    assert_eq!(v.consumed.len(), 3, "bid, KRON token, payer KAS");
    // the payer's KRON change (2 whole tokens) sits under the KRON program, held by address presence
    let change = TokenState::Kron(KronState::addr(2 * WHOLE, pk(PAYER)));
    assert_eq!(token_outputs_in(&v.tx, K, &change), 1);
    assert_floor_fee(&v);
    let r = &v.response_extension["route"];
    assert_eq!(r["payAsset"], hex(&K.cov));
    assert_eq!(r["payerSpent"], (3 * WHOLE).to_string());
    assert!(v.custody.is_none());
}

#[test]
fn a_kron_pay_asset_pays_a_kcc20_merchant_token_across_families() {
    let w = World::new(&[K, B3]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(B3, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(K, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(B3, 2 * WHOLE as u64, &[K]);
    let q = quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE), OrderRef::ask(ask.0.clone(), ask.1.clone(), 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    assert_eq!(p.payer_spent, 3 * WHOLE as u64);
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToToken);
    assert_eq!(v.custody, Some(Custody::Unconditional));
    assert_eq!(v.order_inputs, vec![op(&bid.utxo), op(&ask.0.utxo)], "sell legs first");
    // one transaction, two families: a KRON bid and a KCC-20 ask
    assert_eq!(kind_of_input(&v, 0), Some(TemplateId::KobBidKron));
    assert_eq!(kind_of_input(&v, 1), Some(TemplateId::KobAsk));
    let want = Kcc20State::p2pk(2 * WHOLE, pk(MERCHANT), EXT).spk_with(kob_protocol::artifacts::template(B3.prog));
    assert_eq!(v.merchant_output.script_public_key, want);
    assert_eq!(v.merchant_output.amount, CARRIER, "exactly the offered carrier");
    assert_eq!(v.response_extension["route"]["payAsset"], hex(&K.cov));
    assert_floor_fee(&v);
}

#[test]
fn a_kcc20_8x8_pay_asset_pays_a_kaspacom_template_merchant_token() {
    let w = World::new(&[A8, KC]);
    let bid = w.put_bid(A8, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(A8, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(KC, 2 * WHOLE as u64, &[A8]);
    let q = quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE), OrderRef::ask(ask.0.clone(), ask.1.clone(), 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToToken);
    assert_eq!(kind_of_input(&v, 0), Some(TemplateId::KobBid));
    assert_eq!(kind_of_input(&v, 1), Some(TemplateId::KobAsk));
    // the offer names the KaspaCom template hash, and the merchant token sits under that program
    assert_eq!(offer.extra["token"]["templateHash"], hex(&token_template(TemplateId::Kcc20KaspaCom025).hash));
    let want = Kcc20State::p2pk(2 * WHOLE, pk(MERCHANT), EXT).spk_with(kob_protocol::artifacts::template(KC.prog));
    assert_eq!(v.merchant_output.script_public_key, want);
    assert_eq!(v.response_extension["route"]["payAsset"], hex(&A8.cov));
}

#[test]
fn a_kaspacom_template_pay_asset_pays_kas() {
    let w = World::new(&[KC]);
    let bid = w.put_bid(KC, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(KC, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[KC]);
    let q = quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], false));
    assert_eq!(p.payer_spent, 3 * WHOLE as u64);
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToKas);
    assert_eq!(v.merchant_output.amount, KAS);
    assert_eq!(kind_of_input(&v, 0), Some(TemplateId::KobBid));
    // the payer's KaspaCom-template change (2 whole tokens)
    let change = TokenState::Kcc20(Kcc20State::p2pk(2 * WHOLE, pk(PAYER), EXT));
    assert_eq!(token_outputs_in(&v.tx, KC, &change), 1);
}

#[test]
fn a_kron_pay_asset_pays_a_kaspacom_template_merchant_token() {
    let w = World::new(&[K, KC]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(K, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(KC, 2 * WHOLE as u64, &[K]);
    let q = quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE), OrderRef::ask(ask.0.clone(), ask.1.clone(), 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToToken);
    assert_eq!(kind_of_input(&v, 0), Some(TemplateId::KobBidKron));
    assert_eq!(kind_of_input(&v, 1), Some(TemplateId::KobAsk));
}

#[test]
fn a_kas_payer_buys_a_kaspacom_template_merchant_token() {
    // the KAS pay asset with a KaspaCom-template merchant token: a plain ask leg, no bid
    let w = World::new(&[KC]);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let offer = swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: 2 * WHOLE as u64,
        pay_to: &addr(MERCHANT),
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Token { token: w.allowed(KC), carrier: CARRIER },
        pay_assets: vec![PayAssetSpec::Kas],
    })
    .unwrap();
    let q = quote(vec![OrderRef::ask(ask.0.clone(), ask.1.clone(), 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![], true));
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToToken);
    assert_eq!(v.response_extension["route"]["payAsset"], "KAS");
}

// ------------------------------------------------------------------------------------------ custody labelling

#[test]
fn issuer_controlled_labelling_gates_a_kaspacom_template_token_as_pay_and_merchant_asset() {
    // Operator labels the token issuer-controlled (its template is, or may be, reachable by an issuer).
    let mut w = World::new(&[KC, A8]);
    w.policy.tokens = TokenAllowlist::new();
    w.policy.tokens.insert(A8.allowed()).unwrap();
    w.policy.tokens.insert(AllowedToken { custody: Custody::IssuerControlled, ..KC.allowed() }).unwrap();
    // as a pay asset
    let bid = w.put_bid(KC, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(KC, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[KC]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], false));
    assert_eq!(diag(w.verify(&offer, &p.payload)), Diag::TokenCustodyPolicy);
    w.policy.allow_issuer_controlled = true;
    w.accept(&offer, &p.payload);
    // as a merchant asset the offer must say so, and the verifier reports it
    let mut w = World::new(&[A8]);
    w.policy.tokens.insert(AllowedToken { custody: Custody::IssuerControlled, ..KC.allowed() }).unwrap();
    let bid = w.put_bid(A8, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(A8, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(KC, 2 * WHOLE as u64, &[A8]);
    assert_eq!(offer.extra["token"]["custody"], "issuer-controlled");
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0, ask.1, 2 * WHOLE)]);
    w.policy.allow_issuer_controlled = true; // the payer's own policy accepts the risk
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    let mut strict = w.policy.clone();
    strict.allow_issuer_controlled = false;
    let ctx = VerifyCtx { chain: &*w.chain, clock: &*w.clock, policy: &strict };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::TokenCustodyPolicy);
    let v = w.accept(&offer, &p.payload);
    assert_eq!(v.custody, Some(Custody::IssuerControlled));
}

// ------------------------------------------------------------------------------------------ allowlist and templates

#[test]
fn a_kron_token_outside_the_allowlist_is_refused() {
    // 1. the SDK-built payment, verified by a policy that does not list KRON
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[K]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE)]), &w.funds(vec![held.clone()], true));
    let other = World::new(&[A8]);
    let ctx = VerifyCtx { chain: &*w.chain, clock: &*w.clock, policy: &other.policy };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::PayAssetNotAccepted);
    // 2. a payer who claims an allowlisted pay asset (A8) while spending KRON orders and KRON tokens
    let w = World::new(&[A8]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[A8]);
    let batch = Batch {
        legs: vec![Leg::Bid { order: bid, amount: 3 * WHOLE, t: None }],
        taker_tokens: vec![held],
        taker: Some(pk(PAYER)),
        payments: vec![Payment { script_public_key: spk_to_string(&p2pk_spk(&pk(MERCHANT))), amount: KAS }],
        funding: vec![w.funding.clone()],
        ..base_batch(vec![])
    };
    let p = manual(&offer, &hex(&A8.cov), batch, 1, 2);
    assert_eq!(diag(w.verify(&offer, &p)), Diag::TokenNotAllowlisted);
}

#[test]
fn a_wrong_template_hash_in_the_allowlist_is_refused_in_both_families() {
    // (token on chain, the program the allowlist wrongly pins for its covenant id)
    let cases = [
        (K, TemplateId::KronToken2732),     // another KRON program
        (KC, TemplateId::Kcc20Ref8x8),      // a KaspaCom-template token listed as the reference 8x8
        (A8, TemplateId::Kcc20KaspaCom025), // ... and the other way round
        (K, TemplateId::Kcc20Ref8x8),       // a KRON token listed as a KCC-20 token (family mismatch)
        (A8, TemplateId::KronToken2433),    // a KCC-20 token listed as a KRON token
    ];
    for (real, wrong_prog) in cases {
        let w = World::new(&[real]);
        let bid = w.put_bid(real, 20, 0xb1, common::MAKER_B, 5);
        let held = w.put_holding(real, 21, 5 * WHOLE, pk(PAYER));
        // the wrong entry: same covenant id, other program (extension commitment of that program's family)
        let ext = if wrong_prog.family() == Family::Kron { [0; 32] } else { EXT };
        let wrong = AllowedToken::new(real.cov, wrong_prog, ext, Custody::Unconditional, real.ticker, 3);
        let mut bad = w.policy.clone();
        bad.tokens = TokenAllowlist::new();
        bad.tokens.insert(wrong.clone()).unwrap();
        // the merchant (with the wrong list) offers what its list says; the payer builds the honest transaction
        let offer = swap_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount: KAS,
            pay_to: &addr(MERCHANT),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Kas,
            pay_assets: vec![PayAssetSpec::Token(&wrong)],
        })
        .unwrap();
        let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], true));
        let ctx = VerifyCtx { chain: &*w.chain, clock: &*w.clock, policy: &bad };
        let e = err_of(verify_payment(&ctx, &offer, &p.payload, RH));
        assert_eq!(e.diag, Diag::TokenTemplateMismatch, "{} listed as {}: {e}", real.ticker, wrong_prog.name());
        // ... while the honest allowlist accepts the same transaction shape
    }
}

// ------------------------------------------------------------------------------------------ KRON token states

#[test]
fn a_kron_minter_or_a_non_address_presence_token_is_refused_by_the_verifier() {
    let mut cases: Vec<(&str, KronState, Diag)> = vec![];
    let base = KronState::addr(5 * WHOLE, pk(PAYER));
    cases.push(("minter", KronState { is_minter: 1, ..base.clone() }, Diag::TokenBorrowEnabled));
    cases.push(("id_type 0 (pubkey)", KronState { id_type: 0, ..base.clone() }, Diag::TokenOwnerScheme));
    cases.push(("id_type 1 (script hash)", KronState { id_type: 1, ..base.clone() }, Diag::TokenOwnerScheme));
    for (what, evil, want) in cases {
        let w = World::new(&[K]);
        let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
        let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
        let offer = w.kas_offer(KAS, &[K]);
        let prep = prepare(&w, &offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held.clone()], true));
        let evil_spk = evil.spk_with(token_template(K.prog));
        // the odd UTXO exists on chain in place of the honest one
        w.chain.spend_externally(&op(&held.utxo));
        put(&w.chain, &held.utxo, evil_spk.clone());
        let p = attack(&prep, |_, entries, plans| {
            entries[1].script_public_key = evil_spk.clone();
            let SigPlan::KronToken { state, .. } = &mut plans[1] else { panic!("input 1 is the payer's KRON token") };
            *state = evil.clone();
        });
        let e = err_of(w.verify(&offer, &p));
        assert_eq!(e.diag, want, "{what}: {e}");
    }
}

#[test]
fn the_payer_sdk_refuses_kron_minters_foreign_tokens_and_missing_presence() {
    let w = World::new(&[K, A8]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let offer = w.kas_offer(KAS, &[K]);
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE)]);
    let good = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    // a minter UTXO
    let minter = kob_protocol::tx::TokenUtxo {
        state: TokenState::Kron(KronState { is_minter: 1, ..KronState::addr(5 * WHOLE, pk(PAYER)) }),
        ..good.clone()
    };
    let e = err_of(w.try_pay(&offer, &q, &w.funds(vec![minter], true)));
    assert_eq!(e.diag, Diag::TokenBorrowEnabled, "{e}");
    // a KRON token held by a pubkey (id_type 0), not by address presence
    let pubkey_owned = kob_protocol::tx::TokenUtxo {
        state: TokenState::Kron(KronState { id_type: 0, ..KronState::addr(5 * WHOLE, pk(PAYER)) }),
        ..good.clone()
    };
    assert_eq!(err_of(w.try_pay(&offer, &q, &w.funds(vec![pubkey_owned], true))).diag, Diag::TokenOwnerScheme);
    // a KCC-20 token UTXO offered for the KRON pay asset
    let wrong_family = w.put_holding(A8, 22, 5 * WHOLE, pk(PAYER));
    let wf = kob_protocol::tx::TokenUtxo {
        utxo: kob_protocol::tx::Utxo { covenant_id: Some(K.cov), ..wrong_family.utxo.clone() },
        ..wrong_family
    };
    assert_eq!(err_of(w.try_pay(&offer, &q, &w.funds(vec![wf], true))).diag, Diag::PayAssetNotAccepted);
    // address presence needs a P2PK input of the token owner: without funding the builder refuses
    let e = err_of(w.try_pay(&offer, &q, &w.funds(vec![good.clone()], false)));
    assert!(e.message.contains("P2PK input of that key"), "{e}");
}

#[test]
fn the_verifier_requires_the_p2pk_input_that_authorises_a_key_held_kron_token() {
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[K]);
    let prep = prepare(&w, &offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], true));
    // drop the payer's KAS input (index 2): nothing authorises the KRON token any more
    let p = attack(&prep, |tx, entries, plans| {
        assert!(matches!(plans[2], SigPlan::P2pk { .. }));
        tx.inputs.remove(2);
        entries.remove(2);
        plans.remove(2);
    });
    let e = err_of(w.verify(&offer, &p));
    assert_eq!(e.diag, Diag::TokenOwnerScheme, "{e}");
    assert!(e.message.contains("P2PK input"), "{e}");
}

// ------------------------------------------------------------------------------------------ aliasing

#[test]
fn aliasing_a_kcc20_ask_payout_as_the_merchant_kas_is_rejected_on_a_kron_route() {
    // The payer buys B from the merchant's own KCC-20 ask (funded by KRON sold into a KRON bid) and
    // claims the ask's positional KAS payout as the payment to the merchant.
    let w = World::new(&[K, B3]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(B3, 40, 41, 0x6a, MERCHANT);
    let held = w.put_holding(K, 21, 3 * WHOLE, pk(PAYER));
    let payout = 499_800_000u64;
    let offer = w.kas_offer(payout, &[K]);
    let batch = kob_protocol::build::route_batch(&kob_protocol::build::SwapRoute {
        lock_time: NOW,
        sell: vec![Leg::Bid { order: bid, amount: 3 * WHOLE, t: None }],
        buy: vec![Leg::Ask { order: ask.0, custody: ask.1, amount: 2 * WHOLE, t: None }],
        tokens: vec![held],
        receiver: None,
        token_carrier: CARRIER,
        payments: vec![],
        funding: vec![w.funding.clone()],
        change: Some(pk(PAYER)),
        records: vec![],
        fee: Default::default(),
    })
    .unwrap();
    let p = manual(&offer, &hex(&K.cov), batch, 2, 1);
    let tx = SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap().tx;
    assert_eq!(tx.outputs[1].script_public_key, p2pk_spk(&pk(MERCHANT)), "the ask's positional payout pays the merchant");
    assert_eq!(diag(w.verify(&offer, &p)), Diag::RouteAliasing);
}

// ------------------------------------------------------------------------------------------ roles

#[test]
fn a_kron_token_is_a_pay_asset_never_a_merchant_asset() {
    let w = World::new(&[K, A8]);
    // merchant side: the offer builder refuses
    let e = err_of(swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: 5,
        pay_to: &addr(MERCHANT),
        max_timeout_seconds: 60,
        finality: Finality::Accepted,
        gain: MerchantGain::Token { token: w.allowed(K), carrier: CARRIER },
        pay_assets: vec![PayAssetSpec::Token(w.allowed(A8))],
    }));
    assert_eq!(e.diag, Diag::TokenNotAllowlisted, "{e}");
    let e = err_of(kcc20_requirements(Network::Testnet10, w.allowed(K), 5, &addr(MERCHANT), CARRIER, 60, Finality::Accepted));
    assert_eq!(e.diag, Diag::TokenNotAllowlisted, "{e}");
    // verifier side: a swap offer rewritten to name the KRON token as the merchant's kcc20 asset
    let w2 = World::new(&[K, A8, B3]);
    let bid = w2.put_bid(A8, 20, 0xb1, common::MAKER_B, 5);
    let ask = w2.put_ask(B3, 10, 11, 0x5a, common::MAKER_C);
    let held = w2.put_holding(A8, 21, 3 * WHOLE, pk(PAYER));
    let offer = w2.token_offer(B3, 2 * WHOLE as u64, &[A8]);
    let p = w2.pay(
        &offer,
        &quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0, ask.1, 2 * WHOLE)]),
        &w2.funds(vec![held], true),
    );
    let mut bad = offer.clone();
    bad.asset = hex(&K.cov);
    bad.extra.get_mut("token").unwrap()["templateHash"] = serde_json::json!(hex(&token_template(K.prog).hash));
    bad.extra.get_mut("token").unwrap()["extensionCommitment"] = serde_json::json!(hex(&[0u8; 32]));
    let mut t = p.payload.clone();
    t.accepted = bad.clone();
    assert_eq!(diag(w2.verify(&bad, &t)), Diag::TokenNotAllowlisted);
}

// ------------------------------------------------------------------------------------------ registry

fn registry_template(id: &str, prog: TemplateId, review: ReviewStatus, capabilities: Vec<Capability>) -> Template {
    let t = token_template(prog);
    Template {
        id: id.into(),
        family: prog.family(),
        template_hash: hex(&t.hash),
        prefix_len: t.prefix.len() as u32,
        suffix_len: t.suffix.len() as u32,
        state_len: t.state_len as u32,
        max_token_inputs: t.slots.0 as u32,
        max_token_outputs: t.slots.1 as u32,
        escrow: Escrow { owner_scheme: None, borrow_scheme: None, id_type: None, is_minter: None, delivery_id_type: None },
        review_status: review,
        capabilities,
        risks: vec![],
        source: Source { path: format!("test/{id}"), upstream: None, note: None },
    }
}

fn registry_token(ticker: &str, cov: [u8; 32], template_id: &str, prog: TemplateId, status: Status, verified: bool) -> Token {
    Token {
        ticker: ticker.into(),
        name: ticker.into(),
        family: prog.family(),
        covenant_id: hex(&cov),
        template_id: template_id.into(),
        extension_commitment: (prog.family() == Family::Kcc20).then(|| hex(&EXT)),
        extension_class: kob_protocol::registry::ExtensionClass::None,
        decimals: 3,
        lot_size: None,
        tick: None,
        max_token_inputs: None,
        max_token_outputs: None,
        status,
        verified,
        genesis_verified: verified.then_some(true),
        genesis: verified.then(|| kob_protocol::registry::GenesisRecord {
            txid: hex(&[0x11; 32]),
            daa_score: 1,
            outputs: vec![0],
            supply: 1,
            minter_outputs: vec![],
            live_minters: Some(vec![]),
            checked_at_daa: 2,
            source: "test".into(),
        }),
        warning: None,
        official: verified && status == Status::Listed,
        display: None,
    }
}

#[test]
fn from_registry_lists_reviewed_tokens_of_both_families_and_labels_custody_from_capabilities() {
    let reg = Registry {
        schema: None,
        schema_version: 1,
        network: "testnet-10".into(),
        templates: vec![
            registry_template("kron-2433", TemplateId::KronToken2433, ReviewStatus::Reviewed, vec![]),
            // KaspaCom's program has mint, public-mint and burn entries: not `unconditional` (profile 4.1: no mint or burn entry)
            registry_template(
                "kcc20-kaspacom",
                TemplateId::Kcc20KaspaCom025,
                ReviewStatus::Reviewed,
                vec![Capability::MintAuthority, Capability::PublicMint, Capability::Burn],
            ),
            // a (hypothetical) reviewed program whose authority can freeze: issuer-controlled custody
            registry_template("kcc20-freezable", TemplateId::Kcc20Ref4x5, ReviewStatus::Reviewed, vec![Capability::Freeze]),
            registry_template("kcc20-ref-8x8", TemplateId::Kcc20Ref8x8, ReviewStatus::PendingReview, vec![]),
        ],
        tokens: vec![
            registry_token("KRN", K.cov, "kron-2433", TemplateId::KronToken2433, Status::Listed, true),
            registry_token("KCM", KC.cov, "kcc20-kaspacom", TemplateId::Kcc20KaspaCom025, Status::Listed, true),
            registry_token("FRZ", B3.cov, "kcc20-freezable", TemplateId::Kcc20Ref4x5, Status::Listed, true),
            registry_token("AAA", A8.cov, "kcc20-ref-8x8", TemplateId::Kcc20Ref8x8, Status::Listed, true), // unreviewed program
            registry_token("XXX", [0x75; 32], "kcc20-kaspacom", TemplateId::Kcc20KaspaCom025, Status::PendingReview, true), // not listed
            registry_token("YYY", [0x74; 32], "kcc20-kaspacom", TemplateId::Kcc20KaspaCom025, Status::Listed, false), // not verified
        ],
    };
    let list = TokenAllowlist::from_registry(&reg);
    let listed: Vec<_> = list.iter().map(|t| (t.ticker.as_str(), t.family, t.program, t.extension_commitment, t.custody)).collect();
    assert_eq!(
        listed,
        vec![
            ("KRN", Family::Kron, TemplateId::KronToken2433, [0u8; 32], Custody::Unconditional),
            ("KCM", Family::Kcc20, TemplateId::Kcc20KaspaCom025, EXT, Custody::IssuerControlled),
            ("FRZ", Family::Kcc20, TemplateId::Kcc20Ref4x5, EXT, Custody::IssuerControlled),
        ]
    );
    assert!(!list.find(&K.cov).unwrap().is_merchant_capable(), "a KRON token is a pay asset only");
    assert!(list.find(&KC.cov).unwrap().is_merchant_capable());
    // the allowlist refuses a token whose family and program disagree, and a KRON extension commitment
    let mut l = TokenAllowlist::new();
    assert!(l.insert(AllowedToken { family: Family::Kcc20, ..K.allowed() }).is_err());
    assert!(l.insert(AllowedToken { extension_commitment: EXT, ..K.allowed() }).is_err());
    assert!(l.insert(K.allowed()).is_ok());
}

// ------------------------------------------------------------------------------------------ revoke

#[test]
fn revoking_a_kron_payment_uses_the_payers_kas_input() {
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[K]);
    let p: SwapPayment = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], true));
    let (rev, spent) = kob_x402::client::swap::revoke_swap(&w.policy, &p, &keys(), None).unwrap();
    assert_eq!(spent, op(&w.funding.utxo));
    w.chain.submit(&rev).unwrap();
    w.chain.mine(1);
    assert!(matches!(w.chain.submit(&p.tx), Err(kob_x402::chain::SubmitError::Conflict(_))));
}

// ------------------------------------------------------------------------------------------ carrier ceiling

#[test]
fn an_oversized_merchant_carrier_is_refused_by_the_swap_payer_and_the_verifier() {
    let mut w = World::new(&[A8, B3]);
    let bid = w.put_bid(A8, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(B3, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(A8, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(B3, 2 * WHOLE as u64, &[A8]); // quotes the 10 KAS test carrier
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0, ask.1, 2 * WHOLE)]);
    let funds = w.funds(vec![held], true);
    let built = w.pay(&offer, &q, &funds); // fine under the ceiling of this world (10 KAS)
    w.verify(&offer, &built.payload).expect("verifies under the wide ceiling");
    // the default payer ceiling (2 KAS): the payer SDK refuses to build, and the verifier refuses the built payment
    w.policy.limits.max_carrier_sompi = 2 * KAS;
    let e = w.try_pay(&offer, &q, &funds).unwrap_err();
    assert_eq!(e.diag, Diag::CarrierMismatch, "{e}");
    assert_eq!(diag(w.verify(&offer, &built.payload)), Diag::CarrierMismatch);
}

// ------------------------------------------------------------------------------------------ SIGHASH_ALL

use kob_x402::testkit::hashtype::{resigned, signers, tampered_byte, with_tx, BOGUS, NON_ALL};

/// `p` is accepted; then every payer signature in turn is made under every other hash type (engine-valid for the
/// valid types) and the verifier refuses each with `token_owner_scheme`.
#[track_caller]
fn assert_sighash_all_enforced(w: &World, offer: &PaymentRequirements, p: &SwapPayment, expect_signers: usize) {
    let ss = signers(&p.tx, &p.entries, &keys());
    assert_eq!(ss.len(), expect_signers, "payer signatures found in the transaction");
    w.verify(offer, &p.payload).unwrap_or_else(|e| panic!("the honest payment must verify: {e}"));
    for s in &ss {
        let again = resigned(&p.tx, &p.entries, s, 0x01);
        w.verify(offer, &with_tx(&p.payload, &again, &p.entries))
            .unwrap_or_else(|e| panic!("input {}: re-signed under ALL: {e}", s.input));
        for t in NON_ALL {
            let bad = resigned(&p.tx, &p.entries, s, t);
            kob_protocol::verify::validate(&bad, &p.entries)
                .unwrap_or_else(|e| panic!("input {}: the engine must accept hash type {t:#04x}: {e}", s.input));
            let e = err_of(w.verify(offer, &with_tx(&p.payload, &bad, &p.entries)));
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} hash type {t:#04x}: {e}", s.input);
        }
        for t in BOGUS {
            let e = err_of(w.verify(offer, &with_tx(&p.payload, &tampered_byte(&p.tx, s, t), &p.entries)));
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} trailing byte {t:#04x}: {e}", s.input);
        }
    }
}

#[test]
fn every_family_signs_with_sighash_all_only() {
    // a KRON token has no signature of its own: the P2PK input that authorizes it is the one checked
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[K]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], true));
    assert_sighash_all_enforced(&w, &offer, &p, 1);
    // KRON -> KaspaCom-template merchant token across families: KRON presence input only
    let w = World::new(&[K, KC]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(K, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(KC, 2 * WHOLE as u64, &[K]);
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0.clone(), ask.1.clone(), 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    let n = signers(&p.tx, &p.entries, &keys()).len();
    assert!(n >= 1);
    assert_sighash_all_enforced(&w, &offer, &p, n);
    // a KaspaCom-template payer token: its KCC-20 owner witness is the signature
    let w = World::new(&[KC]);
    let bid = w.put_bid(KC, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(KC, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(KAS, &[KC]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], false));
    assert_sighash_all_enforced(&w, &offer, &p, 1);
    // the reference 8x8 program, token to token
    let w = World::new(&[A8, KC]);
    let bid = w.put_bid(A8, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(A8, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(KC, 2 * WHOLE as u64, &[A8]);
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0.clone(), ask.1.clone(), 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    let n = signers(&p.tx, &p.entries, &keys()).len();
    assert!(n >= 1);
    assert_sighash_all_enforced(&w, &offer, &p, n);
}
