//! Audit 2026-10-30 regressions of the x402 merchant path (offline, in-memory chain, real verifier):
//!
//! * X1: a `/settle` of a payment `/verify` saw must name the payment identifier `/verify` saw, not one of the settler's own.
//! * X3: an intent executed by another keeper pays the merchant: the payer's `/settle` succeeds with that transaction.
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig, IntentRuntime};
use kob_executor::x402::ledger::Ledger;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{BidState, OrderState, SCHEME_P2PK};
use kob_protocol::tx::{KeyUtxo, OrderUtxo, Utxo};
use kob_x402::canonical::http_request_hash;
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::client::intent::{intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::native::{native_requirements, pay_native, payment_identifier_extension, PayOptions};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::common::{requirements_hash_hex, signed_auth_digest, SignedAuth};
use kob_x402::intent::{build_execution, BookView, KeeperParams};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::{spk_to_hex, SafeTx};
use kob_x402::testkit::{pubkey, secret, MockChain};
use kob_x402::wire::{
    hex, parse_hash32, FacilitatorRequest, Finality, Network, PaymentPayload, PaymentRequirements, Profile, SettlementResponse,
    X402_VERSION,
};

use common::{keys, pk, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

const NOW_MS: u64 = 1_800_000_000_000;

fn diag(r: &SettlementResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

// ====================================================================================== native fixtures

const PAYER: u8 = 7;
const MERCH: u8 = 8;
const ATTACKER: u8 = 21;
const RH_URL: &str = "https://api.example.test/report";

struct Native {
    offer: PaymentRequirements,
    rh: String,
    payload: PaymentPayload,
}

fn native(chain: Arc<MockChain>, id: &str) -> Native {
    let op = chain.add_p2pk(&pubkey(PAYER), 500_000_000);
    let u = chain.utxo(&op).unwrap();
    let coin = KeyUtxo {
        utxo: Utxo {
            transaction_id: op.txid,
            index: op.index,
            amount: 500_000_000,
            block_daa_score: u.block_daa_score,
            covenant_id: None,
        },
        pubkey: pubkey(PAYER),
    };
    let addr = Address::new(Network::Testnet10.prefix(), Version::PubKey, &pubkey(MERCH)).to_string();
    let offer = native_requirements(Network::Testnet10, 20_000_000, &addr, 600, Finality::Accepted).unwrap();
    let rh = hex(&http_request_hash("GET", RH_URL, None, &requirements_hash_hex(&offer).unwrap()).unwrap());
    let opts = PayOptions { payment_id: Some(id.to_string()), ttl_seconds: Some(300), ..PayOptions::new(u64::MAX) };
    let payload = pay_native(&offer, &rh, &secret(PAYER), &[coin], NOW_MS, &opts).unwrap();
    Native { offer, rh, payload }
}

fn req(n: &Native, p: &PaymentPayload) -> FacilitatorRequest {
    FacilitatorRequest {
        x402_version: X402_VERSION,
        payment_payload: p.clone(),
        payment_requirements: n.offer.clone(),
        request_hash: Some(n.rh.clone()),
        resource: None,
    }
}

fn facilitator(chain: Arc<MockChain>, ledger: Arc<Ledger>) -> Facilitator {
    Facilitator::new(
        Policy::new(Network::Testnet10),
        chain,
        Arc::new(FixedClock::new(NOW_MS)),
        ledger,
        FacilitatorConfig { settle_wait: Duration::from_millis(400), poll_interval: Duration::from_millis(5), ..Default::default() },
    )
}

/// Anyone who has the transaction (public once broadcast) re-wraps it for an authorization of their OWN key and payment id:
/// the input-0 hint names the attacker's script, the digest is signed by the attacker.
fn forge(n: &Native, att_id: &str) -> PaymentPayload {
    let mut p = n.payload.clone();
    let mut st = SafeTx::parse(&p.payload.transaction, 256 * 1024).unwrap();
    st.inputs[0].utxo.as_mut().unwrap().script_public_key = spk_to_hex(&p2pk_spk(&pubkey(ATTACKER)));
    let txid = st.to_consensus().unwrap().tx.id().as_bytes();
    p.payload.transaction = st.to_text();
    p.extensions = Some(payment_identifier_extension(att_id));
    let a = &mut p.payload.authorization;
    let spk = spk_to_hex(&kaspa_txscript::pay_to_address_script(&Address::try_from(n.offer.pay_to.as_str()).unwrap()));
    let req_hash = parse_hash32(&requirements_hash_hex(&n.offer).unwrap()).unwrap();
    let digest = signed_auth_digest(&SignedAuth {
        network: Network::Testnet10,
        profile: Profile::StandardNative,
        transaction_id: &txid,
        payment_output_index: 0,
        amount: &n.offer.amount,
        pay_to: &n.offer.pay_to,
        pay_to_spk_hex: &spk,
        requirements_hash: &req_hash,
        request_hash: &parse_hash32(&n.rh).unwrap(),
        payment_identifier: att_id,
        challenge_id: None,
        input_index: 0,
        expires_at: &a.expires_at,
    })
    .unwrap();
    let mut sig = kob_protocol::tx::sign_digest(&secret(ATTACKER), &digest).unwrap();
    sig.truncate(64);
    a.digest = hex(&digest);
    a.signature = Some(hex(&sig));
    p
}

fn tx_of(p: &PaymentPayload) -> Transaction {
    SafeTx::parse(&p.payload.transaction, 256 * 1024).unwrap().to_consensus().unwrap().tx
}

#[test]
fn a_settle_of_a_verified_payment_must_name_the_verified_payment_id() {
    let chain = Arc::new(MockChain::new());
    let n = native(chain.clone(), "genuine-payer-id-0000001");
    let fac = facilitator(chain.clone(), Arc::new(Ledger::in_memory()));
    // the merchant verifies the payer's request (as an x402 verify -> settle flow does)
    let v = fac.verify_for("shop", &req(&n, &n.payload));
    assert!(v.is_valid, "{v:?}");
    // the payment is broadcast and accepted before the merchant's /settle (the merchant or the payer broadcast it)
    chain.submit(&tx_of(&n.payload)).unwrap();
    chain.mine(0);
    // anyone who read the public transaction presents it under a payment id and authorization key of their own
    let forged = forge(&n, "attacker-chosen-id-000001");
    let r = fac.settle("shop", &req(&n, &forged));
    assert!(!r.success, "the re-wrap under another payment id is refused: {}", diag(&r));
    // the payer's own settle afterwards is settled from the earlier verify, under the payer's id
    let own = fac.settle("shop", &req(&n, &n.payload));
    assert!(own.success, "the genuine payer is not locked out: {}", diag(&own));
    // a forged re-wrap WITHOUT an earlier verify is refused
    let chain2 = Arc::new(MockChain::new());
    let n2 = native(chain2.clone(), "genuine-payer-id-0000002");
    let fac2 = facilitator(chain2.clone(), Arc::new(Ledger::in_memory()));
    chain2.submit(&tx_of(&n2.payload)).unwrap();
    chain2.mine(0);
    let r2 = fac2.settle("shop", &req(&n2, &forge(&n2, "attacker-chosen-id-000002")));
    assert!(!r2.success, "{}", diag(&r2));
}

// ====================================================================================== third-party keeper

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const PAYER_K: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const KEEPER: u8 = common::KEEPER;
const RIVAL_KEEPER: u8 = 19;
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";

struct HookChain {
    inner: Arc<MockChain>,
    /// Runs once, right before the first submit of a transaction that spends an output of this (the creation).
    creation: Mutex<Option<Txid>>,
    #[allow(clippy::type_complexity)]
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ChainView for HookChain {
    fn utxos(&self, w: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        self.inner.utxos(w)
    }
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        self.inner.utxos_of(spk)
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        self.inner.virtual_daa_score()
    }
    fn output_status(&self, o: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        self.inner.output_status(o, spk)
    }
    fn in_mempool(&self, t: &Txid) -> Result<bool, ChainError> {
        self.inner.in_mempool(t)
    }
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let c = *self.creation.lock().unwrap();
        if c.is_some_and(|c| tx.inputs.iter().any(|i| i.previous_outpoint.transaction_id.as_bytes() == c)) {
            if let Some(h) = self.hook.lock().unwrap().take() {
                h();
            }
        }
        self.inner.submit(tx)
    }
    fn replace(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        self.inner.replace(tx)
    }
    fn watch_spend(&self, o: &Outpoint) {
        self.inner.watch_spend(o)
    }
    fn unwatch_spend(&self, o: &Outpoint) {
        self.inner.unwatch_spend(o)
    }
    fn spent_by(&self, o: &Outpoint) -> kob_x402::chain::SpentBy {
        self.inner.spent_by(o)
    }
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

fn bid_a(tag: u8, maker: u8, price: i64, n: i64) -> OrderUtxo<BidState> {
    let s = BidState { price, ..common::bid(maker, P245, P8) };
    let v = (s.used(n * WHOLE).unwrap() + DC) as u64;
    common::order(tag, v, common::cov(0x60 + tag), 1_000, s)
}

#[test]
fn an_intent_executed_by_another_keeper_settles_for_the_payer() {
    let chain = Arc::new(MockChain::new());
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.tokens.insert(AllowedToken::new(TOKEN_COV, P8, EXT, Custody::Unconditional, "AAA", 3)).unwrap();
    policy.tokens.insert(AllowedToken::new(TOKEN_B, P8, EXT, Custody::Unconditional, "BBB", 3)).unwrap();
    policy.limits.max_fee_sompi = KAS;
    let mut book = BookView::default();
    for b in [bid_a(20, common::MAKER_B, P250, 10), bid_a(21, common::MAKER_C, P245, 10)] {
        put(&chain, &b.utxo, b.state.spk());
        book.bids.push(b);
    }
    let tok = common::tok_of(30, 9 * WHOLE, pk(PAYER_K), SCHEME_P2PK, 900, TOKEN_COV);
    put(&chain, &tok.utxo, tok.state.spk_with(kob_protocol::artifacts::token_template(P8)));
    let funding = vec![common::key_utxo(31, PAYER_K, 100 * KAS), common::key_utxo(32, PAYER_K, 50 * KAS)];
    for f in &funding {
        put(&chain, &f.utxo, p2pk_spk(&f.pubkey));
    }
    let hook = Arc::new(HookChain { inner: chain.clone(), creation: Mutex::new(None), hook: Mutex::new(None) });
    let clock = Arc::new(FixedClock::new(NOW_MS));
    let ledger = Arc::new(Ledger::in_memory());
    let book = Arc::new(Mutex::new(book));
    let b2 = book.clone();
    let fac = Facilitator::new(
        policy.clone(),
        hook.clone(),
        clock,
        ledger.clone(),
        FacilitatorConfig { settle_wait: Duration::from_secs(3), poll_interval: Duration::from_millis(5), ..Default::default() },
    )
    .with_intents(IntentRuntime {
        book: Arc::new(move || Ok(b2.lock().unwrap().clone())),
        keeper: KeeperParams { keeper: pk(KEEPER), ..KeeperParams::default() },
        max_attempts: 10,
        lock_margin_daa: 0,
    });

    let addr = Address::new(Prefix::Testnet, Version::PubKey, &pk(MERCHANT)).to_string();
    let allowed_a = policy.tokens.find(&TOKEN_COV).unwrap();
    let offer = intent_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: 5 * KAS,
        pay_to: &addr,
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(allowed_a)],
    })
    .unwrap();
    let opts = IntentOptions { max_sell: Some(3 * WHOLE), ..IntentOptions::default() };
    let funds = PayerFunds { tokens: vec![tok], funding, change: pk(PAYER_K) };
    let p: IntentPayment = pay_intent(&policy, &offer, &hex(&TOKEN_COV), RH, &keys(), &funds, NOW_MS, &opts).unwrap();
    let creation = p.txid;
    *hook.creation.lock().unwrap() = Some(creation);

    // the rival keeper: right before the facilitator's execution reaches the node, it executes the same intent itself
    {
        let (chain, ledger, book) = (chain.clone(), ledger.clone(), book.clone());
        *hook.hook.lock().unwrap() = Some(Box::new(move || {
            let facts = ledger.get(&hex(&creation)).unwrap().intent.unwrap().facts;
            let mut f = facts.clone();
            let u = chain.utxo(&facts.outpoint()).expect("intent output on chain");
            f.intent.block_daa_score = u.block_daa_score;
            if let Some(l) = f.lock.as_mut() {
                l.utxo.block_daa_score = u.block_daa_score;
            }
            let snapshot = book.lock().unwrap().clone();
            let kp = KeeperParams { keeper: pk(RIVAL_KEEPER), ..KeeperParams::default() };
            let ex = build_execution(&f, &snapshot, &BTreeSet::new(), chain.daa(), &kp)
                .expect("rival plans")
                .expect("rival has an execution");
            chain.submit(&ex.tx).expect("rival submits");
            chain.mine(0);
        }));
    }
    let fr = FacilitatorRequest {
        x402_version: X402_VERSION,
        payment_payload: p.payload.clone(),
        payment_requirements: offer.clone(),
        request_hash: Some(RH.into()),
        resource: None,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let miner = {
        let (c, s) = (chain.clone(), stop.clone());
        thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(3));
                c.mine(0);
            }
        })
    };
    let r = fac.settle("shop", &fr);
    let again = fac.settle("shop", &fr);
    stop.store(true, Ordering::SeqCst);
    miner.join().unwrap();
    let merchant_outputs: Vec<u64> = chain.utxos_of(&p2pk_spk(&pk(MERCHANT))).unwrap().into_iter().map(|(_, u)| u.amount).collect();
    // the merchant was paid on chain by the rival's execution, exactly the offer
    assert_eq!(merchant_outputs, vec![5 * KAS]);
    // and the payer is told so, with the transaction that paid it (not ours)
    assert!(r.success, "settled although another keeper executed it: {} {:?}", diag(&r), r.extensions);
    assert!(again.success, "an identical retry is answered the same: {}", diag(&again));
    assert_eq!(r.transaction, again.transaction);
    let e = ledger.get(&hex(&creation)).unwrap();
    assert_eq!(e.state, kob_executor::x402::ledger::State::Accepted);
    let paid_by = chain.utxos_of(&p2pk_spk(&pk(MERCHANT))).unwrap().into_iter().next().unwrap().0;
    assert_eq!(r.transaction, hex(&paid_by.txid));
}
