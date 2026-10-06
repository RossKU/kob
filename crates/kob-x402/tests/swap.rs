//! Swap-and-pay (`extra.route`, `kob-swap-v1`): payer SDK -> verifier -> MockChain, with real KOB order
//! UTXOs (bids of token A, asks of token B), plus the attack suite.
//!
//! Fixtures come from `kob-protocol/tests/common` (fixed keys, synthetic UTXO ids, the same order and
//! token states as the protocol's golden vectors). Every accepted transaction is also run through
//! `kob_protocol::verify::validate` (the script engine with enforced compute budgets, exact storage
//! mass, fee floor).
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutput, UtxoEntry};
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::{build_batch, route_batch, Batch, Leg, Payment, SwapRoute};
use kob_protocol::payload::Record;
use kob_protocol::script::{p2pk_spk, p2sh_spk, push_data};
use kob_protocol::state::{AnyState, AskState, BidState, Kcc20State, OrderState, SCHEME_COVID, SCHEME_P2PK};
use kob_protocol::tx::{
    assemble, masses, sighash, sign_locally, spk_to_string, OrderUtxo, SigPlan, SignRequest, TokenUtxo, TxJson, Utxo,
};
use kob_x402::canonical::requirements_hash;
use kob_x402::chain::{ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError};
use kob_x402::client::swap::{
    conflicting_orders, is_retryable_conflict, pay_swap, preflight_swap, prepare_revoke_swap, prepare_swap, revoke_swap,
    swap_requirements, MerchantGain, OrderRef, PayAssetSpec, PayerFunds, PreparedSwap, Quote, SwapOfferParams, SwapOptions,
    SwapPayment,
};
use kob_x402::common::{iso_from_ms, payload_commit_digest, PayloadCommit};
use kob_x402::error::{Diag, X402Error};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::SafeTx;
use kob_x402::testkit::MockChain;
use kob_x402::verify::{verify_payment, PaymentKind, Verified, VerifyCtx};
use kob_x402::wire::{hex, parse_hash32, Finality, Network, PaymentPayload, PaymentRequirements, BINDING_SWAP};

use common::{keys, pk, CARRIER, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

use kob_x402::testkit::hashtype::{resigned, signers, tampered_byte, with_tx, BOGUS, NON_ALL};

const T: TemplateId = TemplateId::Kcc20Ref;
const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const ATTACKER: u8 = 30;
const RH: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

// ------------------------------------------------------------------------------------------ world

fn allowed(id: [u8; 32], ticker: &str) -> AllowedToken {
    AllowedToken::new(id, T, EXT, Custody::Unconditional, ticker, 3)
}

fn base_policy() -> Policy {
    let mut p = Policy::new(Network::Testnet10);
    p.tokens.insert(allowed(TOKEN_COV, "AAA")).unwrap();
    p.tokens.insert(allowed(TOKEN_B, "BBB")).unwrap();
    p.limits.min_carrier_sompi = KAS;
    // this fixture quotes the 10 KAS test carrier; the payer ceiling (default 2 KAS) is covered by tests/carrier_ceiling.rs
    p.limits.max_carrier_sompi = CARRIER;
    p
}

fn op(u: &Utxo) -> Outpoint {
    Outpoint::new(u.transaction_id, u.index)
}

fn put(chain: &MockChain, u: &Utxo, spk: ScriptPublicKey) {
    chain.insert_utxo(
        op(u),
        ChainUtxo {
            amount: u.amount,
            script_public_key: spk,
            block_daa_score: u.block_daa_score,
            is_coinbase: false,
            covenant_id: u.covenant_id,
        },
    );
}

struct Fx {
    chain: MockChain,
    clock: FixedClock,
    policy: Policy,
    /// Bid of token A (5 whole tokens funded, continues after a 3-token fill), maker MAKER_B, cov 0xb1.
    bid1: OrderUtxo<BidState>,
    /// The same bid (same state) at another outpoint and covenant id (0xb2), maker MAKER_A.
    bid2: OrderUtxo<BidState>,
    /// A bid funded for exactly 3 whole tokens: a 3-token fill closes it (no continuation).
    bid_close1: OrderUtxo<BidState>,
    bid_close2: OrderUtxo<BidState>,
    /// Ask of token B (5 whole tokens) with its custody, maker MAKER_C, cov 0x5a.
    ask1: (OrderUtxo<AskState>, TokenUtxo),
    ask2: (OrderUtxo<AskState>, TokenUtxo),
    payer_tok: TokenUtxo,
    funding: kob_protocol::tx::KeyUtxo,
}

fn bid_at(tag: u8, covb: u8, maker: u8, whole: i64) -> OrderUtxo<BidState> {
    let bs = common::bid(maker, P245, T);
    let v = (bs.used(whole * WHOLE).expect("budget") + DC) as u64;
    common::order(tag, v, common::cov(covb), 1_000, bs)
}

fn ask_at(tag: u8, tag_c: u8, covb: u8, maker: u8) -> (OrderUtxo<AskState>, TokenUtxo) {
    let a = AskState { token_cov_id: TOKEN_B, ..common::ask_n(maker, P250, T, 5) };
    let c = common::cov(covb);
    (common::order(tag, CARRIER, c, 1_000, a), common::tok_of(tag_c, 5 * WHOLE, c, SCHEME_COVID, 1_000, TOKEN_B))
}

fn put_bid(chain: &MockChain, o: &OrderUtxo<BidState>) {
    put(chain, &o.utxo, o.state.spk());
}
fn put_tok(chain: &MockChain, t: &TokenUtxo) {
    put(chain, &t.utxo, t.state.spk_with(token_template(T)));
}
fn put_ask(chain: &MockChain, a: &(OrderUtxo<AskState>, TokenUtxo)) {
    put(chain, &a.0.utxo, a.0.state.spk());
    put_tok(chain, &a.1);
}

fn fx() -> Fx {
    let chain = MockChain::new();
    chain.advance_daa(NOW);
    let f = Fx {
        chain,
        clock: FixedClock::new(NOW_MS),
        policy: base_policy(),
        bid1: bid_at(20, 0xb1, common::MAKER_B, 5),
        bid2: bid_at(22, 0xb2, common::MAKER_A, 5),
        bid_close1: bid_at(23, 0xb3, common::MAKER_B, 3),
        bid_close2: bid_at(24, 0xb4, common::MAKER_B, 3),
        ask1: ask_at(10, 11, 0x5a, common::MAKER_C),
        ask2: ask_at(13, 14, 0x5b, common::MAKER_A),
        payer_tok: common::tok(21, 3 * WHOLE, pk(PAYER), SCHEME_P2PK, 1_000),
        funding: common::key_utxo(12, PAYER, 20 * KAS),
    };
    for b in [&f.bid1, &f.bid2, &f.bid_close1, &f.bid_close2] {
        put_bid(&f.chain, b);
    }
    put_ask(&f.chain, &f.ask1);
    put_ask(&f.chain, &f.ask2);
    put_tok(&f.chain, &f.payer_tok);
    put(&f.chain, &f.funding.utxo, p2pk_spk(&f.funding.pubkey));
    f
}

impl Fx {
    fn ctx(&self) -> VerifyCtx<'_> {
        VerifyCtx { chain: &self.chain, clock: &self.clock, policy: &self.policy }
    }
    fn verify(&self, offer: &PaymentRequirements, p: &PaymentPayload) -> Result<Verified, X402Error> {
        verify_payment(&self.ctx(), offer, p, RH)
    }
    fn a(&self) -> &AllowedToken {
        self.policy.tokens.find(&TOKEN_COV).unwrap()
    }
    fn b(&self) -> &AllowedToken {
        self.policy.tokens.find(&TOKEN_B).unwrap()
    }
    fn funds(&self, with_kas: bool) -> PayerFunds {
        PayerFunds {
            tokens: vec![self.payer_tok.clone()],
            funding: if with_kas { vec![self.funding.clone()] } else { vec![] },
            change: pk(PAYER),
        }
    }
    fn kas_only_funds(&self) -> PayerFunds {
        PayerFunds { tokens: vec![], funding: vec![self.funding.clone()], change: pk(PAYER) }
    }
    fn kas_offer(&self, amount: u64) -> PaymentRequirements {
        kas_offer_for(&self.a().clone(), amount, MERCHANT)
    }
    /// Token offer for `amount` units of B; pays with A and / or KAS.
    fn token_offer(&self, amount: u64, pay: Vec<PayAssetSpec>) -> PaymentRequirements {
        swap_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(MERCHANT),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Token { token: self.b(), carrier: CARRIER },
            pay_assets: pay,
        })
        .unwrap()
    }
    fn token_offer_ab(&self, amount: u64) -> PaymentRequirements {
        self.token_offer(amount, vec![PayAssetSpec::Token(self.a())])
    }
    fn sw1_quote(&self, bid: &OrderUtxo<BidState>) -> Quote {
        Quote { lock_time: NOW, orders: vec![OrderRef::bid(bid.clone(), 3 * WHOLE)] }
    }
    fn sw2_quote(&self) -> Quote {
        Quote { lock_time: NOW, orders: vec![OrderRef::ask(self.ask1.0.clone(), self.ask1.1.clone(), 2 * WHOLE)] }
    }
    fn sw3_quote(&self) -> Quote {
        Quote {
            lock_time: NOW,
            orders: vec![
                OrderRef::bid(self.bid1.clone(), 3 * WHOLE),
                OrderRef::ask(self.ask1.0.clone(), self.ask1.1.clone(), 2 * WHOLE),
            ],
        }
    }
    fn pay(&self, offer: &PaymentRequirements, quote: &Quote, funds: &PayerFunds) -> Result<SwapPayment, X402Error> {
        pay_swap(&self.policy, offer, quote, RH, &keys(), funds, NOW_MS, &SwapOptions::default())
    }
    fn prepare(&self, offer: &PaymentRequirements, quote: &Quote, funds: &PayerFunds) -> PreparedSwap {
        prepare_swap(&self.policy, offer, quote, RH, funds, NOW_MS, &SwapOptions::default()).unwrap()
    }
    /// Verifies, checks the engine, submits, mines and asserts acceptance of the merchant output.
    fn accept(&self, offer: &PaymentRequirements, p: &PaymentPayload) -> Verified {
        let v = self.verify(offer, p).unwrap_or_else(|e| panic!("verification failed: {e}"));
        kob_protocol::verify::validate(&v.tx, &v.entries).expect("engine validation");
        self.chain.submit(&v.tx).expect("submit");
        self.chain.mine(1);
        assert!(self.chain.is_accepted(&v.txid));
        let st = self.chain.output_status(&v.merchant_output.outpoint, &v.merchant_output.script_public_key).unwrap();
        assert!(matches!(st, OutputStatus::Accepted { .. }), "merchant output must be in the UTXO set");
        v
    }
}

fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
}

fn kas_offer_for(a: &AllowedToken, amount: u64, merchant: u8) -> PaymentRequirements {
    swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount,
        pay_to: &addr(merchant),
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(a)],
    })
    .unwrap()
}

fn diag(r: Result<Verified, X402Error>) -> Diag {
    match r {
        Ok(_) => panic!("verification unexpectedly succeeded"),
        Err(e) => e.diag,
    }
}

fn err_of<T: std::fmt::Debug>(r: Result<T, X402Error>) -> X402Error {
    r.expect_err("expected a failure")
}

/// Re-projects the signed transaction of `p` after `f` edited its safe-JSON form (no re-signing:
/// this is a party tampering with a signed payment).
fn tamper(p: &SwapPayment, f: impl FnOnce(&mut SafeTx)) -> PaymentPayload {
    let mut pl = p.payload.clone();
    let mut safe: SafeTx = serde_json::from_str(&pl.payload.transaction).unwrap();
    safe.id = None;
    f(&mut safe);
    pl.payload.transaction = safe.to_text();
    pl
}

/// A payer that builds a malformed transaction and signs it: `mutate` edits the unsigned transaction,
/// its UTXO entries and the signing plans; every sign request is recomputed and signed. No engine run
/// and no fee check on assembly.
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

/// A payment from a hand-built batch: `claimed_idx` is the payment output index the payer claims.
fn manual(offer: &PaymentRequirements, pay_asset: &str, mut batch: Batch, k: usize, claimed_idx: u32) -> PaymentPayload {
    let expires_at = iso_from_ms(NOW_MS + 60_000);
    let digest = digest_for(offer, pay_asset, claimed_idx, &expires_at);
    batch.records = vec![Record::X402 { reference: digest.to_vec() }];
    let built = build_batch(&batch, &kob_protocol::budget::lookup).unwrap();
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
        receivers: vec![],
        payments: vec![],
        funding: vec![],
        change: Some(pk(PAYER)),
        records: vec![],
        fee: Default::default(),
    }
}

fn kas_payment(amount: u64) -> Payment {
    Payment { script_public_key: spk_to_string(&p2pk_spk(&pk(MERCHANT))), amount }
}

/// SW1-shaped hand-built batch (bid of A sold into, payer token, merchant KAS payment).
fn sw1_batch(f: &Fx, amount: u64) -> Batch {
    Batch {
        legs: vec![Leg::Bid { order: f.bid1.clone(), amount: 3 * WHOLE, t: None }],
        taker_tokens: vec![f.payer_tok.clone()],
        taker: Some(pk(PAYER)),
        payments: vec![kas_payment(amount)],
        ..base_batch(vec![])
    }
}

/// SW3-shaped hand-built route with `ask_whole` whole tokens of B bought for the merchant.
fn sw3_route(f: &Fx, ask_whole: i64, carrier: u64) -> Batch {
    route_batch(&SwapRoute {
        lock_time: NOW,
        sell: vec![Leg::Bid { order: f.bid1.clone(), amount: 3 * WHOLE, t: None }],
        buy: vec![Leg::Ask { order: f.ask1.0.clone(), custody: f.ask1.1.clone(), amount: ask_whole * WHOLE, t: None }],
        tokens: vec![f.payer_tok.clone()],
        receiver: Some(pk(MERCHANT)),
        token_carrier: carrier,
        payments: vec![],
        funding: vec![f.funding.clone()],
        change: Some(pk(PAYER)),
        records: vec![],
        fee: Default::default(),
    })
    .unwrap()
}

// ------------------------------------------------------------------------------------------ happy paths

#[test]
fn sw1_token_a_sold_into_a_bid_pays_kas() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let prep = f.prepare(&offer, &f.sw1_quote(&f.bid1), &f.funds(false));
    // wallet path: the sign requests are exposed, signatures come back from outside
    assert_eq!(prep.sign_requests().len(), 1, "only the payer's token leader signs");
    let sigs = prep.sign_with(&keys()).unwrap();
    let p = prep.complete(&sigs).unwrap();
    assert_eq!(p.payer_spent, 3 * WHOLE as u64);
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let v = f.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToKas);
    assert_eq!(v.amount, KAS);
    assert_eq!(v.merchant_output.amount, KAS);
    assert_eq!(v.merchant_output.script_public_key, p2pk_spk(&pk(MERCHANT)));
    assert_eq!(v.order_inputs, vec![op(&f.bid1.utxo)]);
    assert_eq!(v.consumed.len(), 2);
    assert!(v.custody.is_none());
    let route = &v.response_extension["route"];
    assert_eq!(route["binding"], BINDING_SWAP);
    assert_eq!(route["payAsset"], hex(&TOKEN_COV));
    assert_eq!(route["payerSpent"], (3 * WHOLE).to_string());
    assert_eq!(route["orders"][0]["txid"], hex(&f.bid1.utxo.transaction_id));
    assert!(v.payment_identifier.is_some());
    // the merchant output is spendable KAS: exactly one output pays the merchant key
    let paid: Vec<_> = v.tx.outputs.iter().filter(|o| o.script_public_key == p2pk_spk(&pk(MERCHANT))).collect();
    assert_eq!(paid.len(), 1);
}

#[test]
fn sw1b_merchant_takes_the_whole_proceeds_without_change() {
    let f = fx();
    let base = f.prepare(&f.kas_offer(KAS), &f.sw1_quote(&f.bid1), &f.funds(false));
    let ci = base.built.fee.change_output.expect("baseline has KAS change") as usize;
    let change = base.built.tx.outputs[ci].value;
    // the merchant is quoted everything that would have been change
    let offer = f.kas_offer(KAS + change);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let v = f.accept(&offer, &p.payload);
    assert_eq!(v.merchant_output.amount, KAS + change);
    // outputs: bid delivery, bid continuation, merchant; no KAS change
    assert_eq!(v.tx.outputs.len(), 3, "{:?}", v.tx.outputs.iter().map(|o| o.value).collect::<Vec<_>>());
    assert!(v.tx.outputs.iter().all(|o| o.script_public_key != p2pk_spk(&pk(PAYER))));
}

#[test]
fn sw2_kas_buys_token_b_for_the_merchant() {
    let f = fx();
    let offer = f.token_offer(2 * WHOLE as u64, vec![PayAssetSpec::Kas]);
    let p = f.pay(&offer, &f.sw2_quote(), &f.kas_only_funds()).unwrap();
    assert!(p.payer_spent > 0);
    let v = f.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToToken);
    assert_eq!(v.custody, Some(Custody::Unconditional));
    assert_eq!(v.merchant_output.amount, CARRIER, "the merchant token output carries exactly the carrier");
    assert_eq!(v.response_extension["route"]["payAsset"], "KAS");
    let want = Kcc20State::p2pk(2 * WHOLE, pk(MERCHANT), EXT).spk_with(template(T));
    assert_eq!(v.merchant_output.script_public_key, want);
    // the payer paid KAS: the payment reports what the payer lost, including the fee and the carrier
    assert_eq!(v.response_extension["route"]["payerSpent"], p.payer_spent.to_string());
}

#[test]
fn sw3_token_a_to_token_b_through_two_orders() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    let v = f.accept(&offer, &p.payload);
    assert_eq!(v.kind, PaymentKind::SwapToToken);
    assert_eq!(v.order_inputs.len(), 2);
    assert_eq!(v.order_inputs[0], op(&f.bid1.utxo), "bids (sell legs) first");
    assert_eq!(v.order_inputs[1], op(&f.ask1.0.utxo));
    assert_eq!(v.response_extension["route"]["payerSpent"], (3 * WHOLE).to_string());
    // positional slots 0 and 1 belong to the orders; the merchant output is later
    assert!(v.payment_output_index >= 2);
    // both KCC-20 leaders are covered by the same commitment: change the payload and everything fails
    assert_eq!(f.chain.utxo(&v.merchant_output.outpoint).unwrap().amount, CARRIER);
}

#[test]
fn every_built_transaction_validates_in_the_engine_and_the_payload_round_trips() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    kob_protocol::verify::validate(&p.tx, &p.entries).unwrap();
    let h = kob_x402::wire::header_encode(&p.payload).unwrap();
    let back: PaymentPayload = kob_x402::wire::header_decode(&h).unwrap();
    assert_eq!(back, p.payload);
    f.accept(&offer, &back);
}

#[test]
fn preflight_checks_the_payers_bound() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let (v, facts) = preflight_swap(&f.ctx(), &offer, &p.payload, RH, Some(3 * WHOLE as u64)).unwrap();
    assert_eq!(facts.payer_spent, 3 * WHOLE as u64);
    assert_eq!(facts.pay_asset, hex(&TOKEN_COV));
    assert_eq!(facts.orders, v.order_inputs);
    assert_eq!(facts.legs.len(), 1);
    let e = err_of(preflight_swap(&f.ctx(), &offer, &p.payload, RH, Some(3 * WHOLE as u64 - 1)));
    assert_eq!(e.diag, Diag::Overpayment);
    // a payment that does not verify does not preflight
    f.chain.spend_externally(&op(&f.payer_tok.utxo));
    assert!(preflight_swap(&f.ctx(), &offer, &p.payload, RH, None).is_err());
}

#[test]
fn client_enforces_max_pay_and_refuses_borrow_enabled_tokens() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let mut o = SwapOptions { max_pay: Some(3 * WHOLE as u64 - 1), ..SwapOptions::default() };
    let e = err_of(prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &f.funds(false), NOW_MS, &o));
    assert_eq!(e.diag, Diag::Overpayment);
    o.max_pay = Some(3 * WHOLE as u64);
    prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &f.funds(false), NOW_MS, &o).unwrap();
    // G9: a borrow-enabled payer token is refused by the SDK
    let mut funds = f.funds(false);
    funds.tokens[0].state = Kcc20State { borrow_scheme: 1, ..funds.tokens[0].state.as_kcc20().unwrap().clone() }.into();
    let e = err_of(prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &funds, NOW_MS, &SwapOptions::default()));
    assert_eq!(e.diag, Diag::TokenBorrowEnabled);
    // not enough tokens
    let mut funds = f.funds(false);
    funds.tokens[0].state = funds.tokens[0].state.with_amount(WHOLE);
    let e = err_of(prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &funds, NOW_MS, &SwapOptions::default()));
    assert_eq!(e.diag, Diag::TokenConservation);
    // a KAS offer cannot be paid in KAS; a token offer needs asks
    let e = err_of(prepare_swap(&f.policy, &offer, &f.sw2_quote(), RH, &f.kas_only_funds(), NOW_MS, &SwapOptions::default()));
    assert!(matches!(e.diag, Diag::PayAssetNotAccepted | Diag::RouteUnsupported), "{e}");
    let toffer = f.token_offer_ab(2 * WHOLE as u64);
    let e = err_of(prepare_swap(&f.policy, &toffer, &f.sw1_quote(&f.bid1), RH, &f.funds(false), NOW_MS, &SwapOptions::default()));
    assert_eq!(e.diag, Diag::RouteUnsupported);
}

#[test]
fn client_ttl_is_bounded_by_max_timeout() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let o = SwapOptions { expires_in_ms: 601_000, ..SwapOptions::default() };
    let e = err_of(prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &f.funds(false), NOW_MS, &o));
    assert_eq!(e.diag, Diag::AuthorizationExceedsMaxTimeout);
}

#[test]
fn tiny_kas_change_is_folded_into_the_fee_within_the_bound_or_fails() {
    let f = fx();
    let base = f.prepare(&f.kas_offer(KAS), &f.sw1_quote(&f.bid1), &f.funds(false));
    let ci = base.built.fee.change_output.unwrap() as usize;
    let change = base.built.tx.outputs[ci].value;
    // leave about 0.5 KAS of change: valid for the builder, below a 1 KAS floor
    let amount = KAS + change - 50_000_000;
    let offer = f.kas_offer(amount);
    let floor = SwapOptions { min_change_sompi: KAS, ..SwapOptions::default() };
    // default fee bound 0.5 KAS: folding 0.5 KAS would exceed it -> refuse
    let e = err_of(prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &f.funds(false), NOW_MS, &floor));
    assert_eq!(e.diag, Diag::InvalidKaspaExactFee);
    // with a wider bound it is folded, with a warning
    let wide = SwapOptions { min_change_sompi: KAS, max_fee_sompi: Some(KAS), ..SwapOptions::default() };
    let prep = prepare_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &f.funds(false), NOW_MS, &wide).unwrap();
    assert_eq!(prep.warnings.len(), 1);
    assert!(prep.built.fee.change_output.is_none());
    assert!(prep.built.fee.fee > 40_000_000, "the change went to the fee: {}", prep.built.fee.fee);
    let mut policy = f.policy.clone();
    policy.limits.max_fee_sompi = KAS;
    let sigs = prep.sign_with(&keys()).unwrap();
    let p = prep.complete(&sigs).unwrap();
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &policy };
    let v = verify_payment(&ctx, &offer, &p.payload, RH).unwrap();
    assert_eq!(v.fee, prep.built.fee.fee);
    assert!(v.tx.outputs.iter().all(|o| o.script_public_key != p2pk_spk(&pk(PAYER))));
}

// ------------------------------------------------------------------------------------------ conflict and retry

#[test]
fn order_taken_by_someone_else_is_a_retryable_conflict_and_a_new_quote_succeeds() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let first = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    // someone fills bid1 first
    f.chain.spend_externally(&op(&f.bid1.utxo));
    let e = err_of(f.verify(&offer, &first.payload));
    assert_eq!(e.diag, Diag::OrderConflict);
    assert!(e.retryable);
    assert!(is_retryable_conflict(&e));
    assert_eq!(conflicting_orders(&e), vec![op(&f.bid1.utxo)]);
    assert_eq!(e.details.as_ref().unwrap()["orders"][0]["txid"], hex(&f.bid1.utxo.transaction_id));
    // the payer re-quotes with another bid and signs anew (the facilitator could never rebuild)
    let second = f.pay(&offer, &f.sw1_quote(&f.bid2), &f.funds(false)).unwrap();
    assert_ne!(second.txid, first.txid);
    let v = f.accept(&offer, &second.payload);
    assert_eq!(v.order_inputs, vec![op(&f.bid2.utxo)]);
}

#[test]
fn every_spent_order_is_listed_in_the_conflict() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    f.chain.spend_externally(&op(&f.bid1.utxo));
    f.chain.spend_externally(&op(&f.ask1.0.utxo));
    let e = err_of(f.verify(&offer, &p.payload));
    assert_eq!(e.diag, Diag::OrderConflict);
    let listed = conflicting_orders(&e);
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&op(&f.bid1.utxo)) && listed.contains(&op(&f.ask1.0.utxo)));
    // re-quote with the second pair and succeed
    let q = Quote {
        lock_time: NOW,
        orders: vec![OrderRef::bid(f.bid2.clone(), 3 * WHOLE), OrderRef::ask(f.ask2.0.clone(), f.ask2.1.clone(), 2 * WHOLE)],
    };
    let p2 = f.pay(&offer, &q, &f.funds(true)).unwrap();
    f.accept(&offer, &p2.payload);
}

#[test]
fn a_spent_payer_input_is_not_a_retryable_order_conflict() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    f.chain.spend_externally(&op(&f.funding.utxo));
    let e = err_of(f.verify(&offer, &p.payload));
    assert_eq!(e.diag, Diag::InvalidKaspaExactUtxo);
    assert!(!e.retryable);
    // an order AND a payer input gone: still the payer's problem
    f.chain.spend_externally(&op(&f.bid1.utxo));
    let e = err_of(f.verify(&offer, &p.payload));
    assert_eq!(e.diag, Diag::InvalidKaspaExactUtxo);
    assert!(!e.retryable);
}

#[test]
fn replay_of_an_already_settled_transaction_finds_its_inputs_gone() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    f.accept(&offer, &p.payload);
    let e = err_of(f.verify(&offer, &p.payload));
    assert_eq!(e.diag, Diag::InvalidKaspaExactUtxo);
    assert!(!e.retryable);
}

// ------------------------------------------------------------------------------------------ revoke

#[test]
fn revoke_with_a_kas_input_kills_the_signed_payment() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    let (rev, spent) = revoke_swap(&f.policy, &p, &keys(), None).unwrap();
    assert_eq!(spent, op(&f.funding.utxo));
    let entries = vec![p.entries.iter().find(|e| e.script_public_key == p2pk_spk(&pk(PAYER))).unwrap().clone()];
    kob_protocol::verify::validate(&rev, &entries).expect("the revocation is a valid transaction");
    f.chain.submit(&rev).unwrap();
    f.chain.mine(1);
    // the signed payment can no longer be verified or broadcast
    assert_eq!(diag(f.verify(&offer, &p.payload)), Diag::InvalidKaspaExactUtxo);
    assert!(matches!(f.chain.submit(&p.tx), Err(SubmitError::Conflict(_))));
}

#[test]
fn revoke_falls_back_to_a_payer_token_input() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let (rev, spent) = revoke_swap(&f.policy, &p, &keys(), None).unwrap();
    assert_eq!(spent, op(&f.payer_tok.utxo));
    f.chain.submit(&rev).unwrap();
    f.chain.mine(1);
    assert!(matches!(f.chain.submit(&p.tx), Err(SubmitError::Conflict(_))));
    assert!(revoke_swap(&f.policy, &p, &BTreeMap::new(), None).is_err(), "no local key, no revocation");
}

/// C5 X-10: a payer whose keys live in a wallet revokes the same way: the unsigned self-spend names the input and the
/// signature the wallet makes (one SIGHASH_ALL request), and the signed result is the local-key revocation.
#[test]
fn a_wallet_revokes_with_the_unsigned_self_spend() {
    let f = fx();
    for (offer, quote, kas) in
        [(f.token_offer_ab(2 * WHOLE as u64), f.sw3_quote(), true), (f.kas_offer(KAS), f.sw1_quote(&f.bid1), false)]
    {
        let p = f.pay(&offer, &quote, &f.funds(kas)).unwrap();
        let wallet = std::collections::BTreeSet::from([pk(PAYER)]);
        let (built, spent) = prepare_revoke_swap(&f.policy, &p, &wallet, Some(300)).unwrap();
        assert_eq!(spent, if kas { op(&f.funding.utxo) } else { op(&f.payer_tok.utxo) });
        assert!(!built.sign.is_empty() && built.sign.iter().all(|r| r.pubkey == pk(PAYER) && r.sighash_type == 1));
        let sigs = kob_protocol::tx::sign_locally(&built, &keys()).unwrap();
        let signed = kob_protocol::tx::finalize(&built, &sigs, kob_protocol::tx::FinalizeOptions { tighten_budgets: true }).unwrap();
        let (rev, entries) = signed.tx.to_tx().unwrap();
        kob_protocol::verify::validate(&rev, &entries).expect("the wallet-signed revocation is valid");
        let (local, _) = revoke_swap(&f.policy, &p, &keys(), Some(300)).unwrap();
        assert_eq!(rev.id(), local.id(), "the same transaction as the local-key revocation");
        // the reported id is the transaction's own (the KAS self-spend used to report the id of the transaction before its
        // fee was taken out of the output, which a node answers differently)
        let mut again = local.clone();
        again.finalize();
        assert_eq!(again.id(), local.id(), "the revocation's id is current");
        // another wallet owns no input of the payment
        assert!(prepare_revoke_swap(&f.policy, &p, &std::collections::BTreeSet::from([pk(ATTACKER)]), None).is_err());
    }
}

// ------------------------------------------------------------------------------------------ attacks: economics

#[test]
fn aliasing_merchant_is_the_maker_of_the_filled_ask_is_rejected() {
    // The payer buys B from the merchant's own ask and claims the ask's positional KAS payout as the payment.
    let mut f = fx();
    let merchant_ask = {
        let a = AskState { token_cov_id: TOKEN_B, ..common::ask_n(MERCHANT, P250, T, 5) };
        let c = common::cov(0x6a);
        (common::order(40, CARRIER, c, 1_000, a), common::tok_of(41, 5 * WHOLE, c, SCHEME_COVID, 1_000, TOKEN_B))
    };
    put_ask(&f.chain, &merchant_ask);
    let payout = 499_800_000u64;
    let offer = f.kas_offer(payout);
    let batch = route_batch(&SwapRoute {
        lock_time: NOW,
        sell: vec![Leg::Bid { order: f.bid1.clone(), amount: 3 * WHOLE, t: None }],
        buy: vec![Leg::Ask { order: merchant_ask.0.clone(), custody: merchant_ask.1.clone(), amount: 2 * WHOLE, t: None }],
        tokens: vec![f.payer_tok.clone()],
        receiver: None,
        token_carrier: CARRIER,
        payments: vec![],
        funding: vec![f.funding.clone()],
        change: Some(pk(PAYER)),
        records: vec![],
        fee: Default::default(),
    })
    .unwrap();
    let p = manual(&offer, &hex(&TOKEN_COV), batch, 2, 1);
    // the ask's payout is output 1 and pays the merchant exactly the quoted amount
    let (tx, _) = SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap().to_consensus().map(|x| (x.tx, ())).unwrap();
    assert_eq!(tx.outputs[1].value, payout);
    assert_eq!(tx.outputs[1].script_public_key, p2pk_spk(&pk(MERCHANT)));
    f.policy.swap_enabled = true;
    assert_eq!(diag(f.verify(&offer, &p)), Diag::RouteAliasing);
}

#[test]
fn merchant_underpay_and_overpay_by_one_sompi() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    for (delta, want) in [(-1i64, Diag::Underpayment), (1, Diag::Overpayment)] {
        let batch = sw1_batch(&f, (KAS as i64 + delta) as u64);
        let p = manual(&offer, &hex(&TOKEN_COV), batch, 1, 2);
        // the index the payer claims is irrelevant: the amount is wrong
        assert_eq!(diag(f.verify(&offer, &p)), want, "delta {delta}");
    }
    // a duplicate merchant output is an overpayment too
    let mut b = sw1_batch(&f, KAS);
    b.payments.push(kas_payment(KAS));
    let p = manual(&offer, &hex(&TOKEN_COV), b, 1, 2);
    assert_eq!(diag(f.verify(&offer, &p)), Diag::Overpayment);
}

#[test]
fn token_merchant_underpay_overpay_and_carrier() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    // 1 whole token of B instead of 2
    let p = manual(&offer, &hex(&TOKEN_COV), sw3_route(&f, 1, CARRIER), 2, 3);
    assert_eq!(diag(f.verify(&offer, &p)), Diag::Underpayment);
    // 3 whole tokens of B: the merchant output holds 3000 units
    let p = manual(&offer, &hex(&TOKEN_COV), sw3_route(&f, 3, CARRIER), 2, 3);
    assert_eq!(diag(f.verify(&offer, &p)), Diag::Overpayment);
    // exactly 2 whole tokens but a different carrier than quoted
    let p = manual(&offer, &hex(&TOKEN_COV), sw3_route(&f, 2, CARRIER / 2), 2, 4);
    let e = err_of(f.verify(&offer, &p));
    assert_eq!(e.diag, Diag::CarrierMismatch, "{e}");
}

#[test]
fn merchant_output_redirected_after_signing_swr2() {
    let f = fx();
    // KAS merchant
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let idx = p.payload.payload.payment_output_index as usize;
    let attacker = kob_x402::safe_tx::spk_to_hex(&p2pk_spk(&pk(ATTACKER)));
    let t = tamper(&p, |s| s.outputs[idx].script_public_key = attacker.clone());
    assert_eq!(diag(f.verify(&offer, &t)), Diag::Underpayment);
    // token merchant: the B output is redirected to a state owned by the attacker
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    let idx = p.payload.payload.payment_output_index as usize;
    let evil = kob_x402::safe_tx::spk_to_hex(&Kcc20State::p2pk(2 * WHOLE, pk(ATTACKER), EXT).spk_with(template(T)));
    let t = tamper(&p, |s| s.outputs[idx].script_public_key = evil.clone());
    assert_eq!(diag(f.verify(&offer, &t)), Diag::Underpayment);
    // and the untouched payment still verifies
    f.accept(&offer, &p.payload);
}

#[test]
fn extra_output_to_an_attacker_is_rejected() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let prep = f.prepare(&offer, &f.sw1_quote(&f.bid1), &f.funds(false));
    let stolen = 100_000_000u64;
    let p = attack(&prep, |tx, _, _| {
        let last = tx.outputs.len() - 1;
        tx.outputs[last].value -= stolen;
        tx.outputs.push(TransactionOutput { value: stolen, script_public_key: p2pk_spk(&pk(ATTACKER)), covenant: None });
    });
    assert_eq!(diag(f.verify(&offer, &p)), Diag::InvalidKaspaExactPaymentOutput);
    // an extra plain output to the payer's own key is payer-controlled and fine
    let p = attack(&prep, |tx, _, _| {
        let last = tx.outputs.len() - 1;
        tx.outputs[last].value -= stolen + 20_000_000;
        tx.outputs.push(TransactionOutput { value: stolen, script_public_key: p2pk_spk(&pk(PAYER)), covenant: None });
    });
    f.verify(&offer, &p).unwrap_or_else(|e| panic!("{e}"));
}

// ------------------------------------------------------------------------------------------ attacks: routing

#[test]
fn substituted_order_outpoint_after_signing_swr1() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    // bid_close1 closes on a 3-token fill (no continuation output bound to its covenant id)
    let p = f.pay(&offer, &f.sw1_quote(&f.bid_close1), &f.funds(false)).unwrap();
    f.accept(&offer, &p.payload); // sanity: the honest payment is fine (state is fresh below)
    let f = fx();
    let p = f.pay(&offer, &f.sw1_quote(&f.bid_close1), &f.funds(false)).unwrap();
    let new_op = op(&f.bid_close2.utxo);
    let cov2 = hex(&common::cov(0xb4));
    let t = tamper(&p, |s| {
        s.inputs[0].previous_outpoint.transaction_id = hex(&new_op.txid);
        s.inputs[0].previous_outpoint.index = new_op.index;
        s.inputs[0].utxo.as_mut().unwrap().covenant_id = Some(cov2.clone());
    });
    let mut t = t;
    t.payload.route.as_mut().unwrap().orders = vec![new_op.to_json()];
    let e = err_of(f.verify(&offer, &t));
    assert!(matches!(e.diag, Diag::InvalidKaspaExactTransaction | Diag::InvalidKaspaExactSignature), "{e}");
    assert!(e.message.contains("engine"), "the script engine rejects the stale signature: {e}");
    // the substituted payment was never accepted anywhere
    assert!(!f.chain.is_accepted(&t_txid(&t)));
}

fn t_txid(p: &PaymentPayload) -> [u8; 32] {
    SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap().tx.id().as_bytes()
}

#[test]
fn wrong_pay_asset() {
    let f = fx();
    let c_id = [0x72u8; 32];
    let c = allowed(c_id, "CCC"); // in the offer, not in the allowlist
    let offer = swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: KAS,
        pay_to: &addr(MERCHANT),
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(f.a()), PayAssetSpec::Token(&c)],
    })
    .unwrap();
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    f.verify(&offer, &p.payload).unwrap();
    // 1. payAsset not in offer.payAssets
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().pay_asset = hex(&TOKEN_B);
    assert_eq!(diag(f.verify(&offer, &t)), Diag::PayAssetNotAccepted);
    // 2. in the offer but not allowlisted
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().pay_asset = hex(&c_id);
    assert_eq!(diag(f.verify(&offer, &t)), Diag::PayAssetNotAccepted);
    // 3. payAsset == merchant asset: KAS offer that lists KAS
    let mut kas_listing = f.kas_offer(KAS);
    kas_listing.extra.get_mut("route").unwrap()["payAssets"] = serde_json::json!([{ "asset": "KAS" }]);
    let mut t = p.payload.clone();
    t.accepted = kas_listing.clone();
    t.payload.route.as_mut().unwrap().pay_asset = "KAS".into();
    assert_eq!(diag(f.verify(&kas_listing, &t)), Diag::PayAssetNotAccepted);
    //    ... and a token offer that lists the merchant token itself as a pay asset
    let mut tok_listing = f.token_offer_ab(2 * WHOLE as u64);
    tok_listing.extra.get_mut("route").unwrap()["payAssets"] = serde_json::json!([{
        "asset": hex(&TOKEN_B),
        "templateHash": hex(&template(T).hash),
        "extensionCommitment": hex(&EXT),
    }]);
    let p3 = f.pay(&f.token_offer_ab(2 * WHOLE as u64), &f.sw3_quote(), &f.funds(true)).unwrap();
    let mut t = p3.payload.clone();
    t.accepted = tok_listing.clone();
    t.payload.route.as_mut().unwrap().pay_asset = hex(&TOKEN_B);
    assert_eq!(diag(f.verify(&tok_listing, &t)), Diag::PayAssetNotAccepted);
    // 4. the offer entry's template / extension must equal the allowlist entry
    let mut lie = f.kas_offer(KAS);
    lie.extra.get_mut("route").unwrap()["payAssets"][0]["extensionCommitment"] = serde_json::json!(hex(&[0x01u8; 32]));
    let mut t = p.payload.clone();
    t.accepted = lie.clone();
    assert_eq!(diag(f.verify(&lie, &t)), Diag::PayAssetNotAccepted);
    // 5. the merchant-side builder refuses to list the merchant token as a pay asset
    assert!(swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: 5,
        pay_to: &addr(MERCHANT),
        max_timeout_seconds: 60,
        finality: Finality::Accepted,
        gain: MerchantGain::Token { token: f.b(), carrier: CARRIER },
        pay_assets: vec![PayAssetSpec::Token(f.b())],
    })
    .is_err());
}

#[test]
fn lookalike_order_template_and_lookalike_token_program() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    // splice a lookalike redeem (one prefix byte flipped) into a signed input and re-home the UTXO
    let lookalike = |p: &SwapPayment, input: usize, f: &Fx| -> PaymentPayload {
        let script = p.tx.inputs[input].signature_script.clone();
        let redeem = kob_x402::swap::redeem_of(&script).unwrap().to_vec();
        let mut bad = redeem.clone();
        let last = bad.len() - 3;
        bad[last] ^= 0x01;
        let cut = script.len() - push_data(&redeem).len();
        let mut new_script = script[..cut].to_vec();
        new_script.extend_from_slice(&push_data(&bad));
        let new_spk = p2sh_spk(&bad);
        let old = op_of(&p.tx, input);
        let entry = p.entries[input].clone();
        f.chain.spend_externally(&old);
        f.chain.insert_utxo(
            old,
            ChainUtxo {
                amount: entry.amount,
                script_public_key: new_spk.clone(),
                block_daa_score: entry.block_daa_score,
                is_coinbase: false,
                covenant_id: entry.covenant_id.map(|h| h.as_bytes()),
            },
        );
        tamper(p, |s| {
            s.inputs[input].signature_script = hex(&new_script);
            s.inputs[input].utxo.as_mut().unwrap().script_public_key = kob_x402::safe_tx::spk_to_hex(&new_spk);
        })
    };
    // order input 0 (KobBid): a covenant that is not a pinned KOB template
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let t = lookalike(&p, 0, &f);
    let e = err_of(f.verify(&offer, &t));
    assert_eq!(e.diag, Diag::UnknownOrderTemplate, "{e}");
    // payer token input 1 (allowlisted covenant id, different program)
    let f = fx();
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let t = lookalike(&p, 1, &f);
    assert_eq!(diag(f.verify(&offer, &t)), Diag::TokenTemplateMismatch);
}

fn op_of(tx: &Transaction, i: usize) -> Outpoint {
    Outpoint::new(tx.inputs[i].previous_outpoint.transaction_id.as_bytes(), tx.inputs[i].previous_outpoint.index)
}

#[test]
fn borrow_enabled_payer_token_input_is_rejected() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let prep = f.prepare(&offer, &f.sw1_quote(&f.bid1), &f.funds(false));
    let tpl = template(T);
    let evil_state = Kcc20State { borrow_scheme: 1, ..f.payer_tok.state.as_kcc20().unwrap().clone() };
    let evil_spk = evil_state.spk_with(tpl);
    // the borrow-enabled UTXO exists on chain in place of the honest one
    f.chain.spend_externally(&op(&f.payer_tok.utxo));
    put(&f.chain, &f.payer_tok.utxo, evil_spk.clone());
    let p = attack(&prep, |_, entries, plans| {
        entries[1].script_public_key = evil_spk.clone();
        if let SigPlan::TokenLeader { state, .. } = &mut plans[1] {
            *state = evil_state.clone();
        } else {
            panic!("input 1 is the payer's token leader");
        }
    });
    assert_eq!(diag(f.verify(&offer, &p)), Diag::TokenBorrowEnabled);
}

#[test]
fn borrow_enabled_merchant_token_state_is_rejected() {
    let f = fx();
    // (a) the offer's tokenScriptPublicKey enables borrowing
    let good = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&good, &f.sw3_quote(), &f.funds(true)).unwrap();
    let mut bad = good.clone();
    let evil = Kcc20State { borrow_scheme: 1, ..Kcc20State::p2pk(2 * WHOLE, pk(MERCHANT), EXT) }.spk_with(template(T));
    bad.extra.get_mut("token").unwrap()["tokenScriptPublicKey"] = serde_json::json!(kob_x402::safe_tx::spk_to_hex(&evil));
    let mut t = p.payload.clone();
    t.accepted = bad.clone();
    assert_eq!(diag(f.verify(&bad, &t)), Diag::TokenBorrowEnabled);
    // (b) the payer pays the merchant a borrow-enabled state
    let prep = f.prepare(&good, &f.sw3_quote(), &f.funds(true));
    let merchant_state = Kcc20State::p2pk(2 * WHOLE, pk(MERCHANT), EXT);
    let merchant_spk = merchant_state.spk_with(template(T));
    let evil_state = Kcc20State { borrow_scheme: 1, ..merchant_state.clone() };
    let evil_spk = evil_state.spk_with(template(T));
    let p = attack(&prep, |tx, _, plans| {
        let j = tx.outputs.iter().position(|o| o.script_public_key == merchant_spk).unwrap();
        tx.outputs[j].script_public_key = evil_spk.clone();
        for plan in plans.iter_mut() {
            if let SigPlan::TokenLeader { next_states, .. } = plan {
                for s in next_states.iter_mut() {
                    if *s == merchant_state {
                        *s = evil_state.clone();
                    }
                }
            }
        }
    });
    assert_eq!(diag(f.verify(&good, &p)), Diag::TokenBorrowEnabled);
}

#[test]
fn conditional_and_if_done_legs_are_unsupported() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    // a conditional bid (take-profit leg 0) taking the payer's tokens
    let cb = common::cond_bid(common::MAKER_B, T);
    let v = cb.escrow(2).expect("escrow") as u64;
    let cond = common::order(40, v, common::cov(0xe1), 2_000, cb);
    put(&f.chain, &cond.utxo, cond.state.spk());
    let batch = Batch {
        legs: vec![Leg::CondBid { order: cond, amount: 3 * WHOLE, leg: 0, evidence: None, t: None, merge: None }],
        taker_tokens: vec![f.payer_tok.clone()],
        taker: Some(pk(PAYER)),
        payments: vec![kas_payment(KAS)],
        ..base_batch(vec![])
    };
    let p = manual(&offer, &hex(&TOKEN_COV), batch, 1, 2);
    assert_eq!(diag(f.verify(&offer, &p)), Diag::RouteUnsupported);
    // an if-done buy entry
    let ib = common::ifd_bid(common::MAKER_B, 5, T);
    let ifd = common::order(50, ib.escrow().expect("escrow") as u64, common::cov(0xd1), 2_000, ib);
    put(&f.chain, &ifd.utxo, ifd.state.spk());
    let batch = Batch {
        legs: vec![Leg::IfdBid { order: ifd, amount: 3 * WHOLE, evidence: None, t: None }],
        taker_tokens: vec![f.payer_tok.clone()],
        taker: Some(pk(PAYER)),
        payments: vec![kas_payment(KAS)],
        ..base_batch(vec![])
    };
    let p = manual(&offer, &hex(&TOKEN_COV), batch, 1, 2);
    assert_eq!(diag(f.verify(&offer, &p)), Diag::RouteUnsupported);
}

#[test]
fn unknown_covenant_input_is_rejected() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let script = vec![0x51u8, 0x75, 0x51]; // OP_1 OP_DROP OP_1
    let unknown = Utxo { transaction_id: [0x99; 32], index: 0, amount: 5 * KAS, block_daa_score: 5, covenant_id: Some([0x98; 32]) };
    put(&f.chain, &unknown, p2sh_spk(&script));
    let mut t = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap().payload;
    let mut safe: SafeTx = serde_json::from_str(&t.payload.transaction).unwrap();
    safe.id = None;
    safe.inputs.push(kob_x402::safe_tx::SafeInput {
        previous_outpoint: kob_x402::safe_tx::SafeOutpoint { transaction_id: hex(&[0x99; 32]), index: 0 },
        sequence: "0".into(),
        sig_op_count: 0,
        compute_budget: Some(10),
        signature_script: hex(&push_data(&script)),
        utxo: Some(kob_x402::safe_tx::SafeUtxo {
            amount: (5 * KAS).to_string(),
            script_public_key: kob_x402::safe_tx::spk_to_hex(&p2sh_spk(&script)),
            block_daa_score: None,
            is_coinbase: None,
            covenant_id: Some(hex(&[0x98; 32])),
        }),
    });
    t.payload.transaction = safe.to_text();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::UnknownOrderTemplate);
}

#[test]
fn order_inputs_must_be_the_first_inputs() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let prep = f.prepare(&offer, &f.sw3_quote(), &f.funds(true));
    // move the payer's KAS funding input (last) to the front
    let p = attack(&prep, |tx, entries, plans| {
        let n = tx.inputs.len();
        tx.inputs.rotate_right(1);
        entries.rotate_right(1);
        plans.rotate_right(1);
        assert_eq!(tx.inputs.len(), n);
    });
    assert_eq!(diag(f.verify(&offer, &p)), Diag::RouteAliasing);
}

#[test]
fn custody_of_an_order_this_transaction_does_not_fill_is_rejected() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let prep = f.prepare(&offer, &f.sw3_quote(), &f.funds(true));
    // a stray token owned by ask2's covenant id joins the transaction as an extra input
    let stray = common::tok_of(60, WHOLE, common::cov(0x5b), SCHEME_COVID, 1_000, TOKEN_B);
    put_tok(&f.chain, &stray);
    let p = attack(&prep, |tx, entries, plans| {
        let st = stray.state.clone();
        let inp = TransactionInput::new_with_compute_budget(
            kaspa_consensus_core::tx::TransactionOutpoint::new(
                kaspa_consensus_core::tx::TransactionId::from_bytes(stray.utxo.transaction_id),
                stray.utxo.index,
            ),
            vec![],
            0,
            10,
        );
        tx.inputs.push(inp);
        entries.push(UtxoEntry::new(
            stray.utxo.amount,
            st.spk_with(token_template(T)),
            1_000,
            false,
            Some(kaspa_consensus_core::Hash::from_bytes(TOKEN_B)),
        ));
        plans.push(SigPlan::TokenDelegator {
            template: T,
            state: st.as_kcc20().unwrap().clone(),
            witness: kob_protocol::tx::Witness::CovenantId,
        });
    });
    let d = diag(f.verify(&offer, &p));
    assert_eq!(d, Diag::TokenOwnerScheme);
}

// ------------------------------------------------------------------------------------------ attacks: envelope and authorization

#[test]
fn tampered_accepted_request_hash_digest_and_route() {
    let f = fx();
    let offer = f.token_offer(2 * WHOLE as u64, vec![PayAssetSpec::Token(f.a()), PayAssetSpec::Kas]);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    f.verify(&offer, &p.payload).unwrap();
    // accepted differs from the server offer
    let mut t = p.payload.clone();
    t.accepted.amount = "1".into();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaX402Accepted);
    // requestHash
    let mut t = p.payload.clone();
    t.payload.request_hash = "b2".repeat(32);
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaX402Payload);
    let e = err_of(verify_payment(&f.ctx(), &offer, &p.payload, &"c3".repeat(32)));
    assert_eq!(e.diag, Diag::InvalidKaspaX402Payload);
    // digest
    let mut t = p.payload.clone();
    t.payload.authorization.digest = "00".repeat(32);
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidAuthorization);
    // route.payAsset: another listed and accepted asset; the commitment covers it
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().pay_asset = "KAS".into();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidAuthorization);
    // signature / inputIndex are not part of the payload commitment authorization
    let mut t = p.payload.clone();
    t.payload.authorization.signature = Some("00".repeat(64));
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidAuthorization);
    let mut t = p.payload.clone();
    t.payload.authorization.version = "kaspa-x402-exact-request-authorization-v1".into();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidAuthorization);
    // route hints must agree with the transaction
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().orders.reverse();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaX402Payload);
    // the paymentOutputIndex is committed and checked
    let mut t = p.payload.clone();
    t.payload.payment_output_index += 1;
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidAuthorization);
    // a payment without route against a route offer, and a route payload with a wrong binding
    let mut t = p.payload.clone();
    t.payload.route = None;
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaX402Payload);
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().binding = "kob-swap-v0".into();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaX402Payload);
}

#[test]
fn payload_commitment_missing_or_carrying_order_records() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let prep = f.prepare(&offer, &f.sw1_quote(&f.bid1), &f.funds(false));
    // no payload at all
    let p = attack(&prep, |tx, _, _| tx.payload = vec![]);
    assert_eq!(diag(f.verify(&offer, &p)), Diag::InvalidAuthorization);
    // a commitment to another digest
    let p = attack(&prep, |tx, _, _| tx.payload = kob_x402::common::commitment_payload(&[7u8; 32]));
    assert_eq!(diag(f.verify(&offer, &p)), Diag::InvalidAuthorization);
    // a KOB1 payload that also places an order
    let digest = prep.digest;
    let p = attack(&prep, |tx, _, _| {
        tx.payload = kob_protocol::payload::encode(&[
            Record::X402 { reference: digest.to_vec() },
            Record::order(0, &AnyState::KobBid(f.bid1.state.clone()), None, None),
        ])
        .unwrap();
    });
    assert_eq!(diag(f.verify(&offer, &p)), Diag::InvalidAuthorization);
    // the legacy X402 text payload cannot carry a commitment
    let p = attack(&prep, |tx, _, _| tx.payload = format!("X402:{}", hex(&digest)).into_bytes());
    assert_eq!(diag(f.verify(&offer, &p)), Diag::InvalidAuthorization);
}

#[test]
fn expired_and_too_far_authorizations() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    f.verify(&offer, &p.payload).unwrap();
    f.clock.set(NOW_MS + 61_000);
    assert_eq!(diag(f.verify(&offer, &p.payload)), Diag::ExpiredAuthorization);
    f.clock.set(NOW_MS);
    let o = SwapOptions { expires_in_ms: 3_600_000, enforce_ttl: false, ..SwapOptions::default() };
    let long = pay_swap(&f.policy, &offer, &f.sw1_quote(&f.bid1), RH, &keys(), &f.funds(false), NOW_MS, &o).unwrap();
    assert_eq!(diag(f.verify(&offer, &long.payload)), Diag::AuthorizationExceedsMaxTimeout);
}

#[test]
fn forged_utxo_hints() {
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    // amount
    let last = p.tx.inputs.len() - 1;
    let t = tamper(&p, |s| s.inputs[last].utxo.as_mut().unwrap().amount = (100 * KAS).to_string());
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaExactUtxo);
    // script
    let t = tamper(&p, |s| {
        s.inputs[last].utxo.as_mut().unwrap().script_public_key = kob_x402::safe_tx::spk_to_hex(&p2pk_spk(&pk(ATTACKER)))
    });
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaExactUtxo);
    // covenant id
    let t = tamper(&p, |s| s.inputs[0].utxo.as_mut().unwrap().covenant_id = Some(hex(&[0x01; 32])));
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaExactUtxo);
    // a missing hint cannot locate the input
    let t = tamper(&p, |s| s.inputs[0].utxo = None);
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaExactUtxo);
}

#[test]
fn oversized_artifact_and_duplicate_inputs() {
    let f = fx();
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    let mut small = f.policy.clone();
    small.limits.max_tx_json_bytes = 200;
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &small };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::InvalidKaspaExactTransaction);
    let mut few = f.policy.clone();
    few.limits.max_inputs = 1;
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &few };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::InvalidKaspaExactTransaction);
    let mut few = f.policy.clone();
    few.limits.max_signature_script_bytes = 100;
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &few };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::InvalidKaspaExactTransaction);
    // the same outpoint twice
    let t = tamper(&p, |s| {
        let dup = s.inputs[1].clone();
        s.inputs.push(dup);
    });
    let e = err_of(f.verify(&offer, &t));
    assert_eq!(e.diag, Diag::InvalidKaspaExactTransaction);
    assert!(e.message.contains("twice"), "{e}");
}

#[test]
fn policy_gates() {
    let mut f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    // swap disabled
    f.policy.swap_enabled = false;
    assert_eq!(diag(f.verify(&offer, &p.payload)), Diag::RouteUnsupported);
    f.policy.swap_enabled = true;
    // issuer-controlled merchant token is refused unless the operator opts in
    let mut pol = f.policy.clone();
    pol.tokens = Default::default();
    pol.tokens.insert(allowed(TOKEN_COV, "AAA")).unwrap();
    pol.tokens.insert(AllowedToken { custody: Custody::IssuerControlled, ..allowed(TOKEN_B, "BBB") }).unwrap();
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &pol };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::TokenCustodyPolicy);
    // ... where the offer must state the custody class truthfully
    // (same policy, operator opted in)
    pol.allow_issuer_controlled = true;
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &pol };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::TokenCustodyPolicy, "the offer says unconditional");
    // the merchant token must be allowlisted
    let mut pol = f.policy.clone();
    pol.tokens = Default::default();
    pol.tokens.insert(allowed(TOKEN_COV, "AAA")).unwrap();
    let ctx = VerifyCtx { chain: &f.chain, clock: &f.clock, policy: &pol };
    assert_eq!(diag(verify_payment(&ctx, &offer, &p.payload, RH)), Diag::TokenNotAllowlisted);
    // payment identifier is mandatory
    let mut t = p.payload.clone();
    t.extensions = None;
    assert_eq!(diag(f.verify(&offer, &t)), Diag::MissingKaspaPaymentIdentifier);
}

#[test]
fn offer_shape_is_validated() {
    let f = fx();
    let good = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&good, &f.sw3_quote(), &f.funds(true)).unwrap();
    for (what, edit) in [
        ("binding", Box::new(|r: &mut serde_json::Value| r["binding"] = "kob-swap-v0".into()) as Box<dyn Fn(&mut serde_json::Value)>),
        ("critical", Box::new(|r: &mut serde_json::Value| r["critical"] = false.into())),
        ("payAssets empty", Box::new(|r: &mut serde_json::Value| r["payAssets"] = serde_json::json!([]))),
        ("payAssets shape", Box::new(|r: &mut serde_json::Value| r["payAssets"] = serde_json::json!([{ "asset": "nonsense" }]))),
    ] {
        let mut bad = good.clone();
        edit(bad.extra.get_mut("route").unwrap());
        let mut t = p.payload.clone();
        t.accepted = bad.clone();
        let e = err_of(f.verify(&bad, &t));
        assert_eq!(e.diag, Diag::InvalidKaspaX402Binding, "{what}: {e}");
    }
}

// ------------------------------------------------------------------------------------------ SIGHASH_ALL

/// `p` is accepted; then every payer signature in turn is made under every other hash type. The script engine
/// accepts each valid-typed variant (they are correctly signed transactions), so only the verifier's own check can
/// refuse them: `token_owner_scheme` (conformance case `swap-neg-hashtype`).
#[track_caller]
fn assert_sighash_all_enforced(f: &Fx, offer: &PaymentRequirements, p: &SwapPayment, expect_signers: usize) {
    let ss = signers(&p.tx, &p.entries, &keys());
    assert_eq!(ss.len(), expect_signers, "payer signatures found in the transaction");
    f.verify(offer, &p.payload).unwrap_or_else(|e| panic!("the honest payment must verify: {e}"));
    for s in &ss {
        let again = resigned(&p.tx, &p.entries, s, 0x01);
        f.verify(offer, &with_tx(&p.payload, &again, &p.entries))
            .unwrap_or_else(|e| panic!("input {}: re-signed under ALL: {e}", s.input));
        for t in NON_ALL {
            let bad = resigned(&p.tx, &p.entries, s, t);
            kob_protocol::verify::validate(&bad, &p.entries)
                .unwrap_or_else(|e| panic!("input {}: the engine must accept hash type {t:#04x}: {e}", s.input));
            let e = err_of(f.verify(offer, &with_tx(&p.payload, &bad, &p.entries)));
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} hash type {t:#04x}: {e}", s.input);
            assert!(e.message.contains(&format!("input {}", s.input)), "{e}");
        }
        for t in BOGUS {
            let e = err_of(f.verify(offer, &with_tx(&p.payload, &tampered_byte(&p.tx, s, t), &p.entries)));
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} trailing byte {t:#04x}: {e}", s.input);
        }
    }
}

#[test]
fn swap_payer_token_witness_must_use_sighash_all() {
    // SW1: the payer's token leader is the only signature
    let f = fx();
    let offer = f.kas_offer(KAS);
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &f.funds(false)).unwrap();
    assert_sighash_all_enforced(&f, &offer, &p, 1);
}

#[test]
fn swap_payer_token_delegator_witness_must_use_sighash_all() {
    // the payer's 3 whole tokens come from two token UTXOs: a leader and a delegator sign
    let f = fx();
    let (a, b) = (common::tok(25, WHOLE, pk(PAYER), SCHEME_P2PK, 1_000), common::tok(26, 2 * WHOLE, pk(PAYER), SCHEME_P2PK, 1_000));
    put_tok(&f.chain, &a);
    put_tok(&f.chain, &b);
    let offer = f.kas_offer(KAS);
    let funds = PayerFunds { tokens: vec![a, b], funding: vec![], change: pk(PAYER) };
    let p = f.pay(&offer, &f.sw1_quote(&f.bid1), &funds).unwrap();
    assert_sighash_all_enforced(&f, &offer, &p, 2);
}

#[test]
fn swap_funding_input_signature_must_use_sighash_all() {
    // SW3 with a KAS funding input: token leader plus P2PK funding
    let f = fx();
    let offer = f.token_offer_ab(2 * WHOLE as u64);
    let p = f.pay(&offer, &f.sw3_quote(), &f.funds(true)).unwrap();
    let n = signers(&p.tx, &p.entries, &keys()).len();
    assert!(n >= 2, "a token owner and a funding input sign, got {n}");
    assert_sighash_all_enforced(&f, &offer, &p, n);
    // SW2: KAS pays, the funding input alone signs
    let f = fx();
    let offer = f.token_offer(2 * WHOLE as u64, vec![PayAssetSpec::Kas]);
    let p = f.pay(&offer, &f.sw2_quote(), &f.kas_only_funds()).unwrap();
    assert_sighash_all_enforced(&f, &offer, &p, 1);
}

/// A payer with a hundred small KAS coins: swap-and-pay spends the fewest coins, largest first, that pay the fee and the
/// carriers. The batch builder spends every funding coin it is given, and the client used to pass all of them: past the
/// verifier's 32-input bound no swap could be paid at all (TN10 soak 2026-10-06, a payer with 44 coins).
#[test]
fn a_payer_with_a_hundred_small_coins_spends_only_what_the_swap_needs() {
    let f = fx();
    let coins: Vec<kob_protocol::tx::KeyUtxo> = (100u8..200).map(|t| common::key_utxo(t, PAYER, KAS + u64::from(t))).collect();
    for c in &coins {
        put(&f.chain, &c.utxo, p2pk_spk(&c.pubkey));
    }
    let offer = f.token_offer(2 * WHOLE as u64, vec![PayAssetSpec::Kas]);
    let funds = PayerFunds { tokens: vec![], funding: coins.clone(), change: pk(PAYER) };
    let p = f.pay(&offer, &f.sw2_quote(), &funds).unwrap();
    let v = f.accept(&offer, &p.payload);
    let n = v.tx.inputs.len();
    assert!(n <= f.policy.limits.max_inputs, "{n} inputs");
    // the coins it spends are the largest ones, and one coin fewer would not have paid
    let mut sorted: Vec<u64> = coins.iter().map(|c| c.utxo.amount).collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let spent: u64 = v.entries.iter().filter(|e| e.script_public_key == p2pk_spk(&pk(PAYER))).map(|e| e.amount).sum();
    let k = v.entries.iter().filter(|e| e.script_public_key == p2pk_spk(&pk(PAYER))).count();
    assert!(k >= 1 && k < coins.len(), "{k} of {} coins", coins.len());
    assert_eq!(spent, sorted[..k].iter().sum::<u64>(), "the {k} largest coins");
    assert!(p.payer_spent > sorted[..k - 1].iter().sum::<u64>(), "one coin fewer does not pay the swap");
    // coins too small to pay within the bound: a clear refusal, nothing built
    let dust: Vec<kob_protocol::tx::KeyUtxo> = (100u8..200).map(|t| common::key_utxo(t, PAYER, 2_000_000)).collect();
    let e = err_of(f.pay(&offer, &f.sw2_quote(), &PayerFunds { tokens: vec![], funding: dust, change: pk(PAYER) }));
    assert!(e.to_string().contains("within the verifier's 32 inputs"), "{e}");
}
