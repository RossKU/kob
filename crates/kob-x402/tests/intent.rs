//! Intent-based swap-and-pay (`kob-intent-v1`): payer SDK -> verifier -> MockChain (engine-validated),
//! then the keeper's planner and builder execute the intent against the book.
//!
//! Positive: KasToToken, TokenToKas and TokenSwap are created, verified, broadcast, executed and the
//! merchant output is accepted; the planner skips an order lost to a conflict and the second execution
//! settles WITHOUT a new payer signature; the payer cancels an unexecuted intent.
//! Negative: an intent for another merchant, a payload replayed for another offer, terms or actor changed
//! after signing, a creation the payer broadcast itself, a pay token on a program the router does not trade
//! (KaspaCom's, pending review), a KRON token as the merchant asset, a KRON lock that is not above maxSell.
//! Other programs: the 3/3 reference program (and a 3/3 token A sold for an 8/8 token B) and a KRON token A
//! (sold into KobBidKrons, for KAS and for a KCC-20 token B) are paid, verified and executed the same way.
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build_create_intent, intent_budgets, CreateIntent};
use kob_protocol::payload::Record;
use kob_protocol::router::IntentState;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AskState, BidState, OrderState, SCHEME_COVID, SCHEME_P2PK};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, OrderUtxo, TokenUtxo, Utxo};
use kob_x402::chain::{ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus};
use kob_x402::client::intent::{cancel_intent, intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::common::PayloadCommit;
use kob_x402::error::{Diag, X402Error};
use kob_x402::intent::{build_execution, intent_commit_digest, verify_intent, BookView, IntentFacts, KeeperParams};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::SafeTx;
use kob_x402::testkit::MockChain;
use kob_x402::verify::{verify_payment, PaymentKind, Verified, VerifyCtx};
use kob_x402::wire::{hex, parse_hash32, Finality, Network, PaymentPayload, PaymentRequirements};

use common::{keys, pk, CARRIER, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

use kob_x402::testkit::hashtype::{resigned, signers, tampered_byte, with_tx, BOGUS, NON_ALL};

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const OTHER_MERCHANT: u8 = 9;
const KEEPER: u8 = common::KEEPER;
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";

fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
}

fn allowed(id: [u8; 32], ticker: &str, prog: TemplateId) -> AllowedToken {
    AllowedToken::new(id, prog, EXT, Custody::Unconditional, ticker, 3)
}

fn put(chain: &MockChain, u: &Utxo, spk: ScriptPublicKey) {
    chain.insert_utxo(
        Outpoint::new(u.transaction_id, u.index),
        ChainUtxo {
            amount: u.amount,
            script_public_key: spk,
            block_daa_score: u.block_daa_score,
            is_coinbase: false,
            covenant_id: u.covenant_id,
        },
    );
}
fn put_tok(chain: &MockChain, t: &TokenUtxo) {
    put(chain, &t.utxo, t.state.spk_with(kob_protocol::artifacts::token_template(P8)));
}

struct Fx {
    chain: MockChain,
    clock: FixedClock,
    policy: Policy,
    asks: Vec<(OrderUtxo<AskState>, TokenUtxo)>,
    bids: Vec<OrderUtxo<BidState>>,
    funds: PayerFunds,
}

fn ask_b(tag: u8, maker: u8, whole: i64) -> (OrderUtxo<AskState>, TokenUtxo) {
    let c = common::cov(0x50 + tag);
    let s = AskState { token_cov_id: TOKEN_B, ..common::ask_n(maker, P250, P8, whole) };
    (common::order(tag, CARRIER, c, 1_000, s), common::tok_of(tag + 100, whole * WHOLE, c, SCHEME_COVID, 1_000, TOKEN_B))
}
fn bid_a(tag: u8, maker: u8, whole: i64) -> OrderUtxo<BidState> {
    let s = common::bid(maker, P245, P8);
    let v = (s.used(whole * WHOLE).expect("budget") + DC) as u64;
    common::order(tag, v, common::cov(0x60 + tag), 1_000, s)
}

fn fx() -> Fx {
    let chain = MockChain::new();
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.tokens.insert(allowed(TOKEN_COV, "AAA", P8)).unwrap();
    policy.tokens.insert(allowed(TOKEN_B, "BBB", P8)).unwrap();
    policy.tokens.insert(allowed([0x72; 32], "REF", TemplateId::Kcc20Ref)).unwrap();
    policy.limits.max_fee_sompi = KAS;
    let asks = vec![ask_b(10, common::MAKER_A, 10), ask_b(11, common::MAKER_C, 10)];
    let bids = vec![bid_a(20, common::MAKER_B, 10), bid_a(21, common::MAKER_C, 10)];
    for (o, c) in &asks {
        put(&chain, &o.utxo, o.state.spk());
        put_tok(&chain, c);
    }
    for b in &bids {
        put(&chain, &b.utxo, b.state.spk());
    }
    let tok = common::tok_of(30, 9 * WHOLE, pk(PAYER), SCHEME_P2PK, 900, TOKEN_COV);
    put_tok(&chain, &tok);
    let funding = common::key_utxo(31, PAYER, 100 * KAS);
    put(&chain, &funding.utxo, p2pk_spk(&funding.pubkey));
    Fx {
        chain,
        clock: FixedClock::new(NOW_MS),
        policy,
        asks,
        bids,
        funds: PayerFunds { tokens: vec![tok], funding: vec![funding], change: pk(PAYER) },
    }
}

impl Fx {
    fn ctx(&self) -> VerifyCtx<'_> {
        VerifyCtx { chain: &self.chain, clock: &self.clock, policy: &self.policy }
    }
    fn tok(&self, id: [u8; 32]) -> &AllowedToken {
        self.policy.tokens.find(&id).unwrap()
    }
    fn kas_offer(&self, amount: u64, merchant: u8) -> PaymentRequirements {
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(merchant),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Kas,
            pay_assets: vec![PayAssetSpec::Token(self.tok(TOKEN_COV))],
        })
        .unwrap()
    }
    fn token_offer(&self, amount: u64, merchant: u8) -> PaymentRequirements {
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(merchant),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Token { token: self.tok(TOKEN_B), carrier: KAS },
            pay_assets: vec![PayAssetSpec::Kas, PayAssetSpec::Token(self.tok(TOKEN_COV))],
        })
        .unwrap()
    }
    fn pay(&self, offer: &PaymentRequirements, pay_asset: &str, opts: &IntentOptions) -> Result<IntentPayment, X402Error> {
        let funds = if pay_asset == "KAS" { PayerFunds { tokens: vec![], ..self.funds.clone() } } else { self.funds.clone() };
        pay_intent(&self.policy, offer, pay_asset, RH, &keys(), &funds, NOW_MS, opts)
    }
    fn verify(&self, offer: &PaymentRequirements, p: &PaymentPayload) -> Result<(Verified, IntentFacts), X402Error> {
        verify_intent(&self.ctx(), offer, p, RH)
    }
    fn book(&self) -> BookView {
        BookView { asks: self.asks.clone(), bids: self.bids.clone() }
    }
    fn keeper(&self) -> KeeperParams {
        KeeperParams { keeper: pk(KEEPER), ..KeeperParams::default() }
    }
    /// Broadcasts the creation, mines, executes against the book (minus `exclude`), mines; returns the
    /// execution's merchant outpoint.
    fn settle(&self, v: &Verified, f: &IntentFacts, exclude: &BTreeSet<Outpoint>) -> Outpoint {
        self.chain.submit(&v.tx).expect("creation submit");
        self.chain.mine(1);
        let ex = build_execution(f, &self.book(), exclude, NOW, &self.keeper()).unwrap().expect("the book executes the intent");
        self.chain.submit(&ex.tx).unwrap_or_else(|e| panic!("execution submit: {e:?}"));
        self.chain.mine(1);
        let st = self.chain.output_status(&ex.merchant_output, &ex.merchant_spk).unwrap();
        assert!(matches!(st, OutputStatus::Accepted { .. }), "the merchant output is accepted");
        ex.merchant_output
    }
}

fn t2k_opts() -> IntentOptions {
    IntentOptions { max_sell: Some(3 * WHOLE), ..IntentOptions::default() }
}

#[test]
fn token_to_kas_is_created_verified_and_executed_by_the_keeper() {
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT); // 3 whole tokens at 2.45 + tip release ~7.35 KAS
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    assert_eq!(v.kind, PaymentKind::IntentToKas);
    assert_eq!(facts.state, p.state);
    assert_eq!(facts.lock, p.lock);
    // the generic dispatch reaches the intent verifier
    assert!(verify_payment(&f.ctx(), &offer, &p.payload, RH).is_ok());
    let mo = f.settle(&v, &facts, &BTreeSet::new());
    let got = f.chain.utxo(&mo).unwrap();
    assert_eq!((got.amount, got.script_public_key), (5 * KAS, p2pk_spk(&pk(MERCHANT))));
}

#[test]
fn kas_to_token_and_token_swap_are_executed() {
    let f = fx();
    let offer = f.token_offer(2 * WHOLE as u64, MERCHANT);
    // KAS -> B: the payer allows the asks' quote of 2 whole tokens and 2 KAS of extras
    let p = f
        .pay(&offer, "KAS", &IntentOptions { max_pay: Some(2 * 250_000_000), max_extra: Some(2 * KAS), ..IntentOptions::default() })
        .unwrap();
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    assert_eq!(v.kind, PaymentKind::IntentToToken);
    let mo = f.settle(&v, &facts, &BTreeSet::new());
    assert_eq!(f.chain.utxo(&mo).unwrap().amount, KAS, "the merchant token output carries the offer's carrier");

    // A -> B
    let f = fx();
    let p = f.pay(&offer, &hex(&TOKEN_COV), &IntentOptions { max_sell: Some(4 * WHOLE), ..IntentOptions::default() }).unwrap();
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    f.settle(&v, &facts, &BTreeSet::new());
}

#[test]
fn a_conflicting_fill_is_retried_without_the_payer() {
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    f.chain.submit(&v.tx).unwrap();
    f.chain.mine(1);
    // the keeper's first plan takes the best bid; someone else fills it first
    let first = build_execution(&facts, &f.book(), &BTreeSet::new(), NOW, &f.keeper()).unwrap().unwrap();
    let taken = first.orders[0];
    f.chain.spend_externally(&taken);
    let e = f.chain.submit(&first.tx).unwrap_err();
    assert!(matches!(e, kob_x402::chain::SubmitError::Conflict(_)), "{e:?}");
    // plan again without the lost order: the same intent, no new payer signature
    let exclude: BTreeSet<Outpoint> = [taken].into();
    let second = build_execution(&facts, &f.book(), &exclude, NOW, &f.keeper()).unwrap().expect("another bid fits");
    assert_ne!(second.orders, first.orders);
    f.chain.submit(&second.tx).unwrap();
    f.chain.mine(1);
    assert!(matches!(f.chain.output_status(&second.merchant_output, &second.merchant_spk).unwrap(), OutputStatus::Accepted { .. }));
}

#[test]
fn the_payer_cancels_an_unexecuted_intent() {
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let (v, _) = f.verify(&offer, &p.payload).unwrap();
    f.chain.submit(&v.tx).unwrap();
    f.chain.mine(1);
    let built = cancel_intent(&p, None).unwrap();
    let sigs = sign_locally(&built, &keys()).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    let (tx, _) = signed.tx.to_tx().unwrap();
    f.chain.submit(&tx).unwrap();
    f.chain.mine(1);
    let intent = Outpoint::new(p.intent.transaction_id, p.intent.index);
    assert!(f.chain.utxo(&intent).is_none(), "the intent is spent by the cancel");
    let back = tx.outputs.iter().find(|o| o.covenant.is_some()).unwrap();
    let st = kob_protocol::state::TokenState::user(kob_protocol::Family::Kcc20, 3 * WHOLE, pk(PAYER), EXT);
    assert_eq!(back.script_public_key, st.spk_with(kob_protocol::artifacts::token_template(P8)), "the locked tokens are back");
}

fn diag<T: std::fmt::Debug>(r: Result<T, X402Error>) -> Diag {
    r.expect_err("expected a failure").diag
}

#[test]
fn an_intent_cannot_be_presented_for_another_offer_or_merchant() {
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    // another merchant's offer of the same amount: accepted differs, and with accepted rewritten the
    // commitment in the signed creation does not match
    let other = f.kas_offer(5 * KAS, OTHER_MERCHANT);
    assert_eq!(diag(f.verify(&other, &p.payload)), Diag::InvalidKaspaX402Accepted);
    let mut moved = p.payload.clone();
    moved.accepted = other.clone();
    assert_eq!(diag(f.verify(&other, &moved)), Diag::InvalidAuthorization);
    // the same merchant, another amount (another offer): likewise
    let cheaper = f.kas_offer(4 * KAS, MERCHANT);
    let mut moved = p.payload.clone();
    moved.accepted = cheaper.clone();
    assert_eq!(diag(f.verify(&cheaper, &moved)), Diag::InvalidAuthorization);
    // another request (resource): the request hash is committed
    let other_rh = "d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2";
    let mut moved = p.payload.clone();
    moved.payload.request_hash = other_rh.into();
    assert_eq!(diag(verify_intent(&f.ctx(), &offer, &moved, other_rh)), Diag::InvalidAuthorization);
}

#[test]
fn an_intent_for_another_merchant_does_not_match_the_offer() {
    // A creation whose commitment names the offer but whose intent pays someone else (the payer signs it
    // itself): the verifier recomputes the intent from the OFFER and refuses the script.
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let expires_at = p.payload.payload.authorization.expires_at.clone();
    let digest = intent_commit_digest(
        &PayloadCommit {
            network: Network::Testnet10,
            profile: offer.profile().unwrap(),
            route_pay_asset: Some(&hex(&TOKEN_COV)),
            asset: &offer.asset,
            amount: &offer.amount,
            pay_to: &offer.pay_to,
            pay_to_spk_hex: offer.extra_str("payToScriptPublicKey").unwrap(),
            payment_output_index: 0,
            requirements_hash: &kob_x402::canonical::requirements_hash(&offer).unwrap(),
            request_hash: &parse_hash32(RH).unwrap(),
            expires_at: &expires_at,
        },
        "TokenToKas_sell",
    )
    .unwrap();
    let IntentState::TokenToKas { payer, token, program, merchant_kas, max_sell, deadline, .. } = p.state.clone() else {
        unreachable!()
    };
    let thief_state =
        IntentState::TokenToKas { payer, merchant: pk(OTHER_MERCHANT), token, program, merchant_kas, max_sell, deadline };
    let req = CreateIntent {
        actor: "TokenToKas_sell".into(),
        state: thief_state,
        value: KAS,
        tokens: f.funds.tokens.clone(),
        lock_amount: max_sell,
        lock_carrier: KAS,
        token_change_carrier: KAS,
        funding: f.funds.funding.clone(),
        change: Some(pk(PAYER)),
        records: vec![Record::X402 { reference: digest.to_vec() }],
        fee: Default::default(),
    };
    let built = build_create_intent(&req, &intent_budgets).unwrap();
    let sigs = sign_locally(&built, &keys()).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    let (tx, entries) = signed.tx.to_tx().unwrap();
    let mut forged = p.payload.clone();
    forged.payload.transaction = SafeTx::from_consensus(&tx, &entries).to_text();
    assert_eq!(diag(f.verify(&offer, &forged)), Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn terms_and_actor_are_bound() {
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    // a larger maxSell: the recomputed intent differs from the signed one
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().intent.as_mut().unwrap().max_sell = Some((5 * WHOLE).to_string());
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidKaspaExactPaymentOutput);
    // another actor: the commitment names the actor
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().intent.as_mut().unwrap().actor = "TokenToKas_sell_out".into();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::InvalidAuthorization);
    // an actor of another kind
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().intent.as_mut().unwrap().actor = "KasToToken_buy".into();
    assert_eq!(diag(f.verify(&offer, &t)), Diag::RouteUnsupported);
}

#[test]
fn a_creation_the_payer_broadcast_is_refused_and_other_programs_use_the_signed_route() {
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    f.chain.submit(&p.tx).unwrap();
    f.chain.mine(1);
    let e = f.verify(&offer, &p.payload).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaExactUtxo, "{e}");
    // a pay token on a program the router does not trade (KaspaCom's, pending review) cannot be offered through it
    let kc = AllowedToken::new([0x74; 32], TemplateId::Kcc20KaspaCom025, EXT, Custody::Unconditional, "KCM", 3);
    let r = intent_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: 5 * KAS,
        pay_to: &addr(MERCHANT),
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(&kc)],
    });
    assert_eq!(r.unwrap_err().diag, Diag::RouteUnsupported);
    // nor is a KRON token ever the merchant's
    let kron = AllowedToken::new(KRON_TOKEN, K1, [0; 32], Custody::Unconditional, "KRN", 0);
    let r = intent_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: 5 * WHOLE as u64,
        pay_to: &addr(MERCHANT),
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Token { token: &kron, carrier: KAS },
        pay_assets: vec![PayAssetSpec::Kas],
    });
    assert_eq!(r.unwrap_err().diag, Diag::RouteUnsupported);
    // and the expired authorization of an intent is refused like any other
    let f2 = fx();
    let p2 = f2.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    f2.clock.set(NOW_MS + 301_000);
    assert_eq!(diag(f2.verify(&offer, &p2.payload)), Diag::ExpiredAuthorization);
}

// ------------------------------------------------------------------------------------------ other programs

const REF_TOKEN: [u8; 32] = [0x72; 32];
const KRON_TOKEN: [u8; 32] = [0x73; 32];
const P3: TemplateId = TemplateId::Kcc20Ref;
const K1: TemplateId = TemplateId::KronToken2433;

/// `f` with a 3/3 token (REF) and a KRON token (KRN) the payer holds, and bids of both in the book.
fn fx_programs() -> Fx {
    let mut f = fx();
    f.policy.tokens.insert(AllowedToken::new(KRON_TOKEN, K1, [0; 32], Custody::Unconditional, "KRN", 0)).unwrap();
    for (tag, token, prog) in [(40u8, REF_TOKEN, P3), (41, REF_TOKEN, P3), (42, KRON_TOKEN, K1), (43, KRON_TOKEN, K1)] {
        let s = BidState { token_cov_id: token, ..common::bid(common::MAKER_B, P245, prog) };
        let v = (s.used(10 * WHOLE).expect("budget") + DC) as u64;
        let o = common::order(tag, v, common::cov(0x60 + tag), 1_000, s);
        put(&f.chain, &o.utxo, o.state.spk_for(prog.family()));
        f.bids.push(o);
    }
    for (tag, token, prog) in [(44u8, REF_TOKEN, P3), (45, KRON_TOKEN, K1)] {
        let t = TokenUtxo {
            utxo: common::utxo(tag, CARRIER, 900, Some(token)),
            state: kob_protocol::state::TokenState::user(prog.family(), 9 * WHOLE, pk(PAYER), common::ext_for(prog)),
        };
        put(&f.chain, &t.utxo, t.state.spk_with(kob_protocol::artifacts::token_template(prog)));
        f.funds.tokens.push(t);
    }
    f
}

impl Fx {
    fn offer_paid_with(&self, gain: MerchantGain<'_>, amount: u64, pay: [u8; 32]) -> PaymentRequirements {
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(MERCHANT),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain,
            pay_assets: vec![PayAssetSpec::Token(self.tok(pay))],
        })
        .unwrap()
    }
}

#[test]
fn the_3x3_program_and_a_cross_program_swap_are_executed() {
    let f = fx_programs();
    // REF (3/3) -> KAS
    let offer = f.offer_paid_with(MerchantGain::Kas, 5 * KAS, REF_TOKEN);
    let p = f.pay(&offer, &hex(&REF_TOKEN), &t2k_opts()).unwrap();
    assert!(matches!(p.state, IntentState::TokenToKas { program: P3, .. }), "{:?}", p.state);
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    let mo = f.settle(&v, &facts, &BTreeSet::new());
    assert_eq!(f.chain.utxo(&mo).unwrap().amount, 5 * KAS);
    // REF (3/3) -> B (8/8): one intent across two programs
    let f = fx_programs();
    let offer = f.offer_paid_with(MerchantGain::Token { token: f.tok(TOKEN_B), carrier: KAS }, 2 * WHOLE as u64, REF_TOKEN);
    let p = f.pay(&offer, &hex(&REF_TOKEN), &IntentOptions { max_sell: Some(4 * WHOLE), ..IntentOptions::default() }).unwrap();
    assert!(matches!(p.state, IntentState::TokenSwap { program_a: P3, program_b: P8, .. }), "{:?}", p.state);
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    f.settle(&v, &facts, &BTreeSet::new());
    // the 3/3 program has no room for a three-bid sell: refused before anything is signed
    let offer = f.offer_paid_with(MerchantGain::Kas, 5 * KAS, REF_TOKEN);
    let e = f.pay(&offer, &hex(&REF_TOKEN), &IntentOptions { actor: Some("TokenToKas_sell3".into()), ..t2k_opts() }).unwrap_err();
    assert_eq!(e.diag, Diag::RouteUnsupported, "{e}");
}

#[test]
fn a_kron_token_is_paid_through_kron_intents() {
    // KRN -> KAS through KobBidKrons
    let f = fx_programs();
    let offer = f.offer_paid_with(MerchantGain::Kas, 5 * KAS, KRON_TOKEN);
    let p = f.pay(&offer, &hex(&KRON_TOKEN), &t2k_opts()).unwrap();
    assert_eq!(p.actor, "TokenToKasKron_sell");
    assert_eq!(p.lock.as_ref().unwrap().state.amount(), 3 * WHOLE + 1, "a KRON intent locks one unit more than it may sell");
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    let mo = f.settle(&v, &facts, &BTreeSet::new());
    let got = f.chain.utxo(&mo).unwrap();
    assert_eq!((got.amount, got.script_public_key), (5 * KAS, p2pk_spk(&pk(MERCHANT))));
    // KRN -> B (KCC-20): KobBidKrons of A, KobAsks of B
    let f = fx_programs();
    let offer = f.offer_paid_with(MerchantGain::Token { token: f.tok(TOKEN_B), carrier: KAS }, 2 * WHOLE as u64, KRON_TOKEN);
    let p = f.pay(&offer, &hex(&KRON_TOKEN), &IntentOptions { max_sell: Some(4 * WHOLE), ..IntentOptions::default() }).unwrap();
    assert_eq!(p.actor, "TokenSwapKron_swap");
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    f.settle(&v, &facts, &BTreeSet::new());
    // the creation spends the payer's key-held KRON tokens next to a P2PK input of their owner (address presence)
    assert!(p.entries.iter().any(|e| e.script_public_key == p2pk_spk(&pk(PAYER)) && e.covenant_id.is_none()));
    // a lock that is not above maxSell is refused by the payer SDK and by the verifier
    let offer = f.offer_paid_with(MerchantGain::Kas, 5 * KAS, KRON_TOKEN);
    let e = f.pay(&offer, &hex(&KRON_TOKEN), &IntentOptions { lock_amount: Some(3 * WHOLE), ..t2k_opts() }).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaX402Payload, "{e}");
    let f = fx_programs();
    let p = f.pay(&offer, &hex(&KRON_TOKEN), &t2k_opts()).unwrap();
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().intent.as_mut().unwrap().lock_amount = Some((3 * WHOLE).to_string());
    let e = f.verify(&offer, &t).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaX402Payload, "{e}");
    // a KCC-20 actor for a KRON pay token is refused (the actor's family)
    let mut t = p.payload.clone();
    t.payload.route.as_mut().unwrap().intent.as_mut().unwrap().actor = "TokenToKas_sell".into();
    assert!(matches!(diag(f.verify(&offer, &t)), Diag::RouteUnsupported | Diag::InvalidAuthorization));
}

// ------------------------------------------------------------------------------------------ SIGHASH_ALL

/// The creation `p` verifies; then every payer signature in turn is made under every other hash type. The script
/// engine accepts each valid-typed variant, so only the verifier's own check refuses them (`token_owner_scheme`).
#[track_caller]
fn assert_sighash_all_enforced(f: &Fx, offer: &PaymentRequirements, p: &IntentPayment, expect_signers: usize) {
    let ss = signers(&p.tx, &p.entries, &keys());
    assert_eq!(ss.len(), expect_signers, "payer signatures found in the creation");
    f.verify(offer, &p.payload).unwrap_or_else(|e| panic!("the honest creation must verify: {e}"));
    for s in &ss {
        let again = resigned(&p.tx, &p.entries, s, 0x01);
        f.verify(offer, &with_tx(&p.payload, &again, &p.entries))
            .unwrap_or_else(|e| panic!("input {}: re-signed under ALL: {e}", s.input));
        for t in NON_ALL {
            let bad = resigned(&p.tx, &p.entries, s, t);
            kob_protocol::verify::validate(&bad, &p.entries)
                .unwrap_or_else(|e| panic!("input {}: the engine must accept hash type {t:#04x}: {e}", s.input));
            let e = f.verify(offer, &with_tx(&p.payload, &bad, &p.entries)).unwrap_err();
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} hash type {t:#04x}: {e}", s.input);
            assert!(e.message.contains(&format!("input {}", s.input)), "{e}");
        }
        for t in BOGUS {
            let e = f.verify(offer, &with_tx(&p.payload, &tampered_byte(&p.tx, s, t), &p.entries)).unwrap_err();
            assert_eq!(e.diag, Diag::TokenOwnerScheme, "input {} trailing byte {t:#04x}: {e}", s.input);
        }
    }
}

#[test]
fn intent_creation_payer_signatures_must_use_sighash_all() {
    // TokenToKas: the payer's token leader (and any funding input) sign the creation
    let f = fx();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let n = signers(&p.tx, &p.entries, &keys()).len();
    assert!(n >= 1);
    assert_sighash_all_enforced(&f, &offer, &p, n);
    // KasToToken: P2PK inputs only
    let f = fx();
    let offer = f.token_offer(2 * WHOLE as u64, MERCHANT);
    let p = f
        .pay(&offer, "KAS", &IntentOptions { max_pay: Some(2 * 250_000_000), max_extra: Some(2 * KAS), ..IntentOptions::default() })
        .unwrap();
    let n = signers(&p.tx, &p.entries, &keys()).len();
    assert!(n >= 1);
    assert_sighash_all_enforced(&f, &offer, &p, n);
    // TokenSwap
    let f = fx();
    let p = f.pay(&offer, &hex(&TOKEN_COV), &IntentOptions { max_sell: Some(4 * WHOLE), ..IntentOptions::default() }).unwrap();
    let n = signers(&p.tx, &p.entries, &keys()).len();
    assert!(n >= 1);
    assert_sighash_all_enforced(&f, &offer, &p, n);
}

/// A payer with a hundred small KAS coins creates an intent from the fewest coins, largest first, that pay it (the builder
/// spends every funding coin it is given; with all of them the facilitator refused the creation: "input count must be
/// 1..=32", TN10 soak 2026-10-06).
#[test]
fn a_payer_with_a_hundred_small_coins_creates_an_intent_from_the_largest_it_needs() {
    let mut f = fx();
    let coins: Vec<kob_protocol::tx::KeyUtxo> = (100u8..200).map(|t| common::key_utxo(t, PAYER, KAS + u64::from(t))).collect();
    for c in &coins {
        put(&f.chain, &c.utxo, p2pk_spk(&c.pubkey));
    }
    f.funds.funding = coins.clone();
    let offer = f.kas_offer(5 * KAS, MERCHANT);
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let (v, facts) = f.verify(&offer, &p.payload).unwrap();
    let k = v.entries.iter().filter(|e| e.script_public_key == p2pk_spk(&pk(PAYER))).count();
    assert!(k >= 1 && v.tx.inputs.len() <= f.policy.limits.max_inputs, "{k} payer coins, {} inputs", v.tx.inputs.len());
    let mut sorted: Vec<u64> = coins.iter().map(|c| c.utxo.amount).collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let spent: u64 = v.entries.iter().filter(|e| e.script_public_key == p2pk_spk(&pk(PAYER))).map(|e| e.amount).sum();
    assert_eq!(spent, sorted[..k].iter().sum::<u64>(), "the {k} largest coins");
    f.settle(&v, &facts, &BTreeSet::new());
    // a hundred dust coins: still within the bound (only what the fee needs)
    let f = fx();
    let dust: Vec<kob_protocol::tx::KeyUtxo> = (100u8..200).map(|t| common::key_utxo(t, PAYER, 2_000_000)).collect();
    for c in &dust {
        put(&f.chain, &c.utxo, p2pk_spk(&c.pubkey));
    }
    let f = Fx { funds: PayerFunds { funding: dust, ..f.funds.clone() }, ..f };
    let p = f.pay(&offer, &hex(&TOKEN_COV), &t2k_opts()).unwrap();
    let (v, _) = f.verify(&offer, &p.payload).unwrap();
    assert!(v.tx.inputs.len() <= f.policy.limits.max_inputs, "{} inputs", v.tx.inputs.len());
}
