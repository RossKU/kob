//! Audit 2026-10-30 round 4 regressions of the x402 merchant path (offline, in-memory chain whose submit runs the real script
//! engine, the real verifier and the real facilitator):
//!
//! * X8: a token merchant is NOT paid by the payer's own cancel carrying a plain (covenant-less) copy of the merchant's token
//!   script; an execution by another keeper (a real token output) still settles.
//! * X9: the spend of the intent output is followed from the claim on; a full watch table refuses the payment before any broadcast.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

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
use kob_executor::x402::ledger::{Ledger, State};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AskState, BidState, OrderState, SCHEME_COVID, SCHEME_P2PK};
use kob_protocol::tx::{finalize, sighash, sign_locally, spk_to_string, FinalizeOptions, OrderUtxo, TokenUtxo, TxOutputJson, Utxo};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SpentBy, SubmitError, Txid};
use kob_x402::client::intent::{cancel_intent, intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::intent::{build_execution, BookView, KeeperParams};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::spk_from_hex;
use kob_x402::testkit::MockChain;
use kob_x402::wire::{hex, FacilitatorRequest, Finality, Network, PaymentRequirements, SettlementResponse, X402_VERSION};

use common::{keys, pk, CARRIER, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

const NOW_MS: u64 = 1_800_000_000_000;
const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const PAYER_K: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const KEEPER: u8 = common::KEEPER;
const RIVAL_KEEPER: u8 = 19;
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";

fn diag(r: &SettlementResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

/// A chain that runs a hook right before the first submit of a transaction spending an output of the creation, can refuse watches,
/// and records the order of watch and broadcast calls.
struct HookChain {
    inner: Arc<MockChain>,
    creation: Mutex<Option<Txid>>,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Watch table size (a `watch_spend` beyond it answers false).
    cap: Mutex<usize>,
    watched: Mutex<BTreeSet<Outpoint>>,
    log: Mutex<Vec<String>>,
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
        let id = tx.id().as_bytes();
        let c = *self.creation.lock().unwrap();
        if c == Some(id) {
            self.log.lock().unwrap().push("submit(creation)".into());
        }
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
    fn watch_spend(&self, o: &Outpoint) -> bool {
        let mut w = self.watched.lock().unwrap();
        if w.contains(o) {
            return true;
        }
        if w.len() >= *self.cap.lock().unwrap() {
            self.log.lock().unwrap().push("watch refused".into());
            return false;
        }
        w.insert(*o);
        self.log.lock().unwrap().push("watch".into());
        true
    }
    fn unwatch_spend(&self, o: &Outpoint) {
        self.watched.lock().unwrap().remove(o);
    }
    fn spent_by(&self, o: &Outpoint) -> SpentBy {
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

fn ask_b(tag: u8, maker: u8, n: i64) -> (OrderUtxo<AskState>, TokenUtxo) {
    let c = common::cov(0x50 + tag);
    let s = AskState { token_cov_id: TOKEN_B, ..common::ask_n(maker, P250, P8, n) };
    (common::order(tag, CARRIER, c, 1_000, s), common::tok_of(tag + 100, n * WHOLE, c, SCHEME_COVID, 1_000, TOKEN_B))
}

struct World {
    chain: Arc<MockChain>,
    hook: Arc<HookChain>,
    ledger: Arc<Ledger>,
    book: Arc<Mutex<BookView>>,
    policy: Policy,
    funds: PayerFunds,
    fac: Facilitator,
}

fn world() -> World {
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
    for a in [ask_b(10, common::MAKER_A, 10), ask_b(11, common::MAKER_C, 10)] {
        put(&chain, &a.0.utxo, a.0.state.spk());
        put(&chain, &a.1.utxo, a.1.state.spk_with(kob_protocol::artifacts::token_template(P8)));
        book.asks.push(a);
    }
    let tok = common::tok_of(30, 9 * WHOLE, pk(PAYER_K), SCHEME_P2PK, 900, TOKEN_COV);
    put(&chain, &tok.utxo, tok.state.spk_with(kob_protocol::artifacts::token_template(P8)));
    let funding = vec![common::key_utxo(31, PAYER_K, 100 * KAS), common::key_utxo(32, PAYER_K, 50 * KAS)];
    for f in &funding {
        put(&chain, &f.utxo, p2pk_spk(&f.pubkey));
    }
    let hook = Arc::new(HookChain {
        inner: chain.clone(),
        creation: Mutex::new(None),
        hook: Mutex::new(None),
        cap: Mutex::new(usize::MAX),
        watched: Mutex::new(BTreeSet::new()),
        log: Mutex::new(vec![]),
    });
    let ledger = Arc::new(Ledger::in_memory());
    let book = Arc::new(Mutex::new(book));
    let b2 = book.clone();
    let fac = Facilitator::new(
        policy.clone(),
        hook.clone(),
        Arc::new(FixedClock::new(NOW_MS)),
        ledger.clone(),
        FacilitatorConfig { settle_wait: Duration::from_secs(3), poll_interval: Duration::from_millis(5), ..Default::default() },
    )
    .with_intents(IntentRuntime {
        book: Arc::new(move || Ok(b2.lock().unwrap().clone())),
        keeper: KeeperParams { keeper: pk(KEEPER), ..KeeperParams::default() },
        max_attempts: 10,
        lock_margin_daa: 0,
    });
    let funds = PayerFunds { tokens: vec![tok], funding, change: pk(PAYER_K) };
    World { chain, hook, ledger, book, policy, funds, fac }
}

impl World {
    /// KAS buys 2 whole units of token B for the merchant (KasToToken).
    fn offer(&self) -> PaymentRequirements {
        let addr = Address::new(Prefix::Testnet, Version::PubKey, &pk(MERCHANT)).to_string();
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount: 2 * WHOLE as u64,
            pay_to: &addr,
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Token { token: self.policy.tokens.find(&TOKEN_B).unwrap(), carrier: KAS },
            pay_assets: vec![PayAssetSpec::Kas, PayAssetSpec::Token(self.policy.tokens.find(&TOKEN_COV).unwrap())],
        })
        .unwrap()
    }
    fn pay(&self, offer: &PaymentRequirements) -> IntentPayment {
        let funds = PayerFunds { tokens: vec![], ..self.funds.clone() };
        let opts = IntentOptions { max_pay: Some(2 * 250_000_000), max_extra: Some(2 * KAS), ..IntentOptions::default() };
        pay_intent(&self.policy, offer, "KAS", RH, &keys(), &funds, NOW_MS, &opts).unwrap()
    }
    fn request(&self, offer: &PaymentRequirements, p: &IntentPayment) -> FacilitatorRequest {
        FacilitatorRequest {
            x402_version: X402_VERSION,
            payment_payload: p.payload.clone(),
            payment_requirements: offer.clone(),
            request_hash: Some(RH.into()),
            resource: None,
        }
    }
    /// `/settle` with a miner thread that accepts the mempool every few milliseconds.
    fn settle(&self, offer: &PaymentRequirements, p: &IntentPayment) -> SettlementResponse {
        let stop = Arc::new(AtomicBool::new(false));
        let miner = {
            let (c, s) = (self.chain.clone(), stop.clone());
            thread::spawn(move || {
                while !s.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(3));
                    c.mine(0);
                }
            })
        };
        let r = self.fac.settle("shop", &self.request(offer, p));
        stop.store(true, Ordering::SeqCst);
        miner.join().unwrap();
        r
    }
}

/// The merchant's token script of the offer.
fn merchant_token_spk(offer: &PaymentRequirements) -> ScriptPublicKey {
    spk_from_hex(offer.extra["token"]["tokenScriptPublicKey"].as_str().expect("token script in the offer")).unwrap()
}

#[test]
fn x8_the_payers_cancel_with_a_plain_copy_of_the_merchant_token_output_does_not_pay_the_merchant() {
    let w = world();
    let offer = w.offer();
    let p = w.pay(&offer);
    let mspk = merchant_token_spk(&offer);
    *w.hook.creation.lock().unwrap() = Some(p.txid);
    // the payer's cancel, re-signed with one more output: the merchant's token script and carrier, bound to NO covenant
    let mut b = cancel_intent(&p, None).unwrap();
    b.tx.outputs[0].value -= KAS + 50_000;
    b.tx.outputs.push(TxOutputJson { value: KAS, script_public_key: spk_to_string(&mspk), covenant: None });
    let (tx, entries) = b.tx.to_tx().unwrap();
    for r in b.sign.iter_mut() {
        r.sighash = sighash(&tx, &entries, r.input_index);
    }
    let sigs = sign_locally(&b, &keys()).unwrap();
    let signed = finalize(&b, &sigs, FinalizeOptions { tighten_budgets: true }).expect("finalize the fake cancel");
    let (cancel, _) = signed.tx.to_tx().unwrap();
    assert!(cancel.outputs.iter().any(|o| o.script_public_key == mspk && o.covenant.is_none()));
    {
        let (chain, cancel) = (w.chain.clone(), cancel.clone());
        *w.hook.hook.lock().unwrap() = Some(Box::new(move || {
            chain.submit(&cancel).expect("the script engine accepts the payer's cancel (it constrains no output)");
            chain.mine(0);
        }));
    }
    let r = w.settle(&offer, &p);
    assert!(!r.success, "the merchant was told it is paid by the payer's own cancel: {r:?}");
    assert_ne!(r.transaction, hex(&cancel.id().as_bytes()));
    let held = w.chain.utxos_of(&mspk).unwrap();
    assert!(held.iter().all(|(_, u)| u.covenant_id != Some(TOKEN_B)), "the merchant holds no token B: {held:?}");
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(e.reason.as_deref().unwrap_or("").starts_with("intent_spent"), "{:?}", e.reason);
}

#[test]
fn x8_an_execution_by_another_keeper_pays_a_token_merchant_and_settles() {
    let w = world();
    let offer = w.offer();
    let p = w.pay(&offer);
    let mspk = merchant_token_spk(&offer);
    *w.hook.creation.lock().unwrap() = Some(p.txid);
    let rival_tx = Arc::new(Mutex::new(None::<Transaction>));
    {
        // the rival executes right before the facilitator's own execution reaches the node
        let (chain, ledger, book, out, creation) = (w.chain.clone(), w.ledger.clone(), w.book.clone(), rival_tx.clone(), p.txid);
        *w.hook.hook.lock().unwrap() = Some(Box::new(move || {
            let facts = ledger.get(&hex(&creation)).unwrap().intent.unwrap().facts;
            let mut f = facts.clone();
            let u = chain.utxo(&facts.outpoint()).expect("intent output on chain");
            f.intent.block_daa_score = u.block_daa_score;
            if let Some(l) = f.lock.as_mut() {
                l.utxo.block_daa_score = u.block_daa_score;
            }
            let snapshot = book.lock().unwrap().clone();
            let kp = KeeperParams { keeper: pk(RIVAL_KEEPER), ..KeeperParams::default() };
            let ex = build_execution(&f, &snapshot, &BTreeSet::new(), chain.daa(), &kp).unwrap().unwrap();
            chain.submit(&ex.tx).expect("rival submits");
            chain.mine(0);
            *out.lock().unwrap() = Some(ex.tx);
        }));
    }
    let r = w.settle(&offer, &p);
    assert!(r.success, "settled by the rival's execution: {} {r:?}", diag(&r));
    let rival = rival_tx.lock().unwrap().clone().expect("the rival executed");
    assert_eq!(r.transaction, hex(&rival.id().as_bytes()));
    let held = w.chain.utxos_of(&mspk).unwrap();
    assert!(held.iter().any(|(_, u)| u.covenant_id == Some(TOKEN_B)), "the merchant holds token B: {held:?}");
}

#[test]
fn x9_the_spend_of_the_intent_is_followed_before_the_creation_is_broadcast() {
    let w = world();
    let offer = w.offer();
    let p = w.pay(&offer);
    *w.hook.creation.lock().unwrap() = Some(p.txid);
    let _ = w.settle(&offer, &p);
    let log = w.hook.log.lock().unwrap().clone();
    let i_watch = log.iter().position(|l| l == "watch");
    let i_submit = log.iter().position(|l| l == "submit(creation)");
    assert!(i_watch.is_some() && i_submit.is_some(), "{log:?}");
    assert!(i_watch < i_submit, "watched only after the broadcast: {log:?}");
}

#[test]
fn x9_a_full_watch_table_refuses_the_payment_before_anything_is_broadcast() {
    let w = world();
    let offer = w.offer();
    let p = w.pay(&offer);
    *w.hook.creation.lock().unwrap() = Some(p.txid);
    *w.hook.cap.lock().unwrap() = 0;
    let r = w.fac.settle("shop", &w.request(&offer, &p));
    assert!(!r.success, "{r:?}");
    let log = w.hook.log.lock().unwrap().clone();
    assert!(log.contains(&"watch refused".to_string()), "{log:?}");
    assert!(!log.contains(&"submit(creation)".to_string()), "the creation was broadcast with no watch: {log:?}");
    assert!(!w.chain.in_mempool(&p.txid).unwrap());
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
}
