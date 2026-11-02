//! Regression tests of the round-5 x402 audit fixes (audit 2026-10-30 X14, X15, X18, X17) and their obvious bypasses. Offline.
#![allow(clippy::too_many_arguments, clippy::type_complexity, dead_code, unused_imports)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
use kob_protocol::tx::{finalize, sighash, sign_locally, spk_to_string, BuiltTx, FinalizeOptions, OrderUtxo, TokenUtxo, TxOutputJson, Utxo};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SpentBy, SubmitError, Tracked, Txid};
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

/// The chain view of the probes: the in-memory chain (real script engine, real `CovenantsContext`) plus the knobs a real follower has.
struct Spy {
    inner: Arc<MockChain>,
    creation: Mutex<Option<Txid>>,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    cap: Mutex<usize>,
    watched: Mutex<BTreeSet<Outpoint>>,
    tracked_over: Mutex<BTreeMap<Txid, Tracked>>,
    hide: Mutex<BTreeSet<Outpoint>>,
    lag: AtomicBool,
    log: Mutex<Vec<String>>,
}

impl ChainView for Spy {
    fn utxos(&self, w: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        let h = self.hide.lock().unwrap();
        Ok(self.inner.utxos(w)?.into_iter().zip(w).map(|(u, (o, _))| if h.contains(o) { None } else { u }).collect())
    }
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        let h = self.hide.lock().unwrap();
        Ok(self.inner.utxos_of(spk)?.into_iter().filter(|(o, _)| !h.contains(o)).collect())
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        self.inner.virtual_daa_score()
    }
    fn output_status(&self, o: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        if self.hide.lock().unwrap().contains(o) {
            return Ok(OutputStatus::Unknown);
        }
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
    fn tracked(&self, t: &Txid) -> Tracked {
        self.tracked_over.lock().unwrap().get(t).copied().unwrap_or(Tracked::Untracked)
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
        self.log.lock().unwrap().push("unwatch".into());
    }
    fn spent_by(&self, o: &Outpoint) -> SpentBy {
        if self.lag.load(Ordering::SeqCst) {
            return SpentBy::Unspent;
        }
        self.inner.spent_by(o)
    }
}

fn put(chain: &MockChain, u: &Utxo, spk: ScriptPublicKey) {
    chain.insert_utxo(
        Outpoint::new(u.transaction_id, u.index),
        ChainUtxo { amount: u.amount, script_public_key: spk, block_daa_score: u.block_daa_score, is_coinbase: false, covenant_id: u.covenant_id },
    );
}

fn bid_tok(tag: u8, maker: u8, price: i64, n: i64, token: [u8; 32]) -> OrderUtxo<BidState> {
    let s = BidState { price, token_cov_id: token, ..common::bid(maker, P245, P8) };
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
    spy: Arc<Spy>,
    ledger: Arc<Ledger>,
    book: Arc<Mutex<BookView>>,
    /// 0: the book as it is; 1: the next read sees it, later reads see an empty book; 2: an empty book
    mode: Arc<AtomicU8>,
    clock: Arc<FixedClock>,
    policy: Policy,
    fac: Facilitator,
}

fn world() -> World {
    world_with_min_carrier(Policy::new(Network::Testnet10).limits.min_carrier_sompi)
}

fn world_with_min_carrier(min_carrier: u64) -> World {
    let chain = Arc::new(MockChain::new());
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.tokens.insert(AllowedToken::new(TOKEN_COV, P8, EXT, Custody::Unconditional, "AAA", 3)).unwrap();
    policy.tokens.insert(AllowedToken::new(TOKEN_B, P8, EXT, Custody::Unconditional, "BBB", 3)).unwrap();
    policy.limits.max_fee_sompi = KAS;
    policy.limits.min_carrier_sompi = min_carrier;
    let mut book = BookView::default();
    for b in [
        bid_tok(20, common::MAKER_B, P250, 10, TOKEN_COV),
        bid_tok(21, common::MAKER_C, P245, 10, TOKEN_COV),
        bid_tok(22, common::MAKER_B, P250, 10, TOKEN_B),
        bid_tok(23, common::MAKER_C, P245, 10, TOKEN_B),
    ] {
        put(&chain, &b.utxo, b.state.spk());
        book.bids.push(b);
    }
    for a in [ask_b(10, common::MAKER_A, 10), ask_b(11, common::MAKER_C, 10)] {
        put(&chain, &a.0.utxo, a.0.state.spk());
        put(&chain, &a.1.utxo, a.1.state.spk_with(kob_protocol::artifacts::token_template(P8)));
        book.asks.push(a);
    }
    for (tag, tok) in [(30u8, TOKEN_COV), (34u8, TOKEN_B)] {
        let t = common::tok_of(tag, 9 * WHOLE, pk(PAYER_K), SCHEME_P2PK, 900, tok);
        put(&chain, &t.utxo, t.state.spk_with(kob_protocol::artifacts::token_template(P8)));
    }
    for tag in 31..=39u8 {
        let f = common::key_utxo(tag, PAYER_K, 100 * KAS);
        put(&chain, &f.utxo, p2pk_spk(&f.pubkey));
    }
    let spy = Arc::new(Spy {
        inner: chain.clone(),
        creation: Mutex::new(None),
        hook: Mutex::new(None),
        cap: Mutex::new(usize::MAX),
        watched: Mutex::new(BTreeSet::new()),
        tracked_over: Mutex::new(BTreeMap::new()),
        hide: Mutex::new(BTreeSet::new()),
        lag: AtomicBool::new(false),
        log: Mutex::new(vec![]),
    });
    let ledger = Arc::new(Ledger::in_memory());
    let book = Arc::new(Mutex::new(book));
    let mode = Arc::new(AtomicU8::new(0));
    let (b2, m2) = (book.clone(), mode.clone());
    let clock = Arc::new(FixedClock::new(NOW_MS));
    let fac = Facilitator::new(
        policy.clone(),
        spy.clone(),
        clock.clone(),
        ledger.clone(),
        FacilitatorConfig { settle_wait: Duration::from_millis(600), poll_interval: Duration::from_millis(5), ..Default::default() },
    )
    .with_intents(IntentRuntime {
        book: Arc::new(move || match m2.load(Ordering::SeqCst) {
            0 => Ok(b2.lock().unwrap().clone()),
            1 => {
                m2.store(2, Ordering::SeqCst);
                Ok(b2.lock().unwrap().clone())
            }
            _ => Ok(BookView::default()),
        }),
        keeper: KeeperParams { keeper: pk(KEEPER), ..KeeperParams::default() },
        max_attempts: 10,
        lock_margin_daa: 0,
    });
    World { chain, spy, ledger, book, mode, clock, policy, fac }
}

fn funds(tag: u8, tokens: Vec<TokenUtxo>) -> PayerFunds {
    PayerFunds { tokens, funding: vec![common::key_utxo(tag, PAYER_K, 100 * KAS)], change: pk(PAYER_K) }
}

fn maddr() -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(MERCHANT)).to_string()
}

impl World {
    /// KAS buys 2 whole units of token B for the merchant (KasToToken).
    fn offer_buy(&self) -> PaymentRequirements {
        self.offer_buy_carrier(KAS)
    }
    fn offer_buy_carrier(&self, carrier: u64) -> PaymentRequirements {
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount: 2 * WHOLE as u64,
            pay_to: &maddr(),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Token { token: self.policy.tokens.find(&TOKEN_B).unwrap(), carrier },
            pay_assets: vec![PayAssetSpec::Kas, PayAssetSpec::Token(self.policy.tokens.find(&TOKEN_COV).unwrap())],
        })
        .unwrap()
    }
    fn pay_buy(&self, offer: &PaymentRequirements, tag: u8) -> IntentPayment {
        let opts = IntentOptions { max_pay: Some(2 * 250_000_000), max_extra: Some(2 * KAS), ..IntentOptions::default() };
        pay_intent(&self.policy, offer, "KAS", RH, &keys(), &funds(tag, vec![]), NOW_MS, &opts).unwrap()
    }
    /// A KAS merchant paid by selling a token (TokenToKas); two pay tokens are offered.
    fn offer_sell(&self, amount: u64) -> PaymentRequirements {
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &maddr(),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Kas,
            pay_assets: vec![
                PayAssetSpec::Token(self.policy.tokens.find(&TOKEN_COV).unwrap()),
                PayAssetSpec::Token(self.policy.tokens.find(&TOKEN_B).unwrap()),
            ],
        })
        .unwrap()
    }
    fn pay_sell(&self, offer: &PaymentRequirements, token: [u8; 32], tok_tag: u8, fund_tag: u8, keeper_value: u64) -> IntentPayment {
        let tok = common::tok_of(tok_tag, 9 * WHOLE, pk(PAYER_K), SCHEME_P2PK, 900, token);
        put(&self.chain, &tok.utxo, tok.state.spk_with(kob_protocol::artifacts::token_template(P8)));
        let opts = IntentOptions { max_sell: Some(3 * WHOLE), keeper_value, ..IntentOptions::default() };
        pay_intent(&self.policy, offer, &hex(&token), RH, &keys(), &funds(fund_tag, vec![tok]), NOW_MS, &opts).unwrap()
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
    fn miner(&self) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (self.chain.clone(), stop.clone());
        let h = thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(3));
                c.mine(0);
            }
        });
        (stop, h)
    }
    /// A rival keeper executes the intent right before the facilitator's own execution reaches the node.
    fn rival(&self, creation: Txid, carrier: Option<u64>) {
        *self.spy.creation.lock().unwrap() = Some(creation);
        let (chain, ledger, book) = (self.chain.clone(), self.ledger.clone(), self.book.clone());
        *self.spy.hook.lock().unwrap() = Some(Box::new(move || {
            let facts = ledger.get(&hex(&creation)).unwrap().intent.unwrap().facts;
            let mut f = facts.clone();
            let u = chain.utxo(&facts.outpoint()).expect("intent output on chain");
            f.intent.block_daa_score = u.block_daa_score;
            if let Some(l) = f.lock.as_mut() {
                l.utxo.block_daa_score = u.block_daa_score;
            }
            if let Some(c) = carrier {
                f.merchant_value = c;
            }
            let snapshot = book.lock().unwrap().clone();
            let kp = KeeperParams { keeper: pk(RIVAL_KEEPER), ..KeeperParams::default() };
            let ex = build_execution(&f, &snapshot, &BTreeSet::new(), chain.daa(), &kp).unwrap().expect("rival has an execution");
            chain.submit(&ex.tx).expect("rival submits");
            chain.mine(0);
        }));
    }
}

fn merchant_token_spk(offer: &PaymentRequirements) -> ScriptPublicKey {
    spk_from_hex(offer.extra["token"]["tokenScriptPublicKey"].as_str().unwrap()).unwrap()
}

// ====================================================================================================================
// X14 candidate: one merchant output credited to two intents (the payer's cancel of both intents in ONE transaction)
// ====================================================================================================================

/// Two cancels of two TokenToKas intents (token A1 and token A2, so each lock is the first input of its own covenant group) in one
/// transaction, plus ONE output to the merchant. Consensus: `cancel(sig)` constrains no output.
fn merge_cancels(b1: &BuiltTx, b2: &BuiltTx, merchant: &ScriptPublicKey, pays: &[u64], margin: u64) -> BuiltTx {
    let mut m = b1.clone();
    let in_off = b1.tx.inputs.len();
    let out_off = b1.tx.outputs.len();
    m.tx.inputs.extend(b2.tx.inputs.clone());
    for mut o in b2.tx.outputs.clone() {
        if let Some(c) = o.covenant.as_mut() {
            c.authorizing_input += in_off as u16;
        }
        m.tx.outputs.push(o);
    }
    m.plans.extend(b2.plans.clone());
    m.roles.extend(b2.roles.clone());
    for mut s in b2.sign.clone() {
        s.input_index += in_off;
        m.sign.push(s);
    }
    let change = m.tx.outputs.iter().position(|o| o.covenant.is_none()).expect("b1 has a KAS change output");
    eprintln!("X14 b1 outputs: {:?}", m.tx.outputs.iter().map(|o| (o.value, o.covenant.is_some())).collect::<Vec<_>>());
    m.tx.outputs[change].value -= pays.iter().sum::<u64>() + margin;
    let _ = out_off;
    for pay in pays {
        m.tx.outputs.push(TxOutputJson { value: *pay, script_public_key: spk_to_string(merchant), covenant: None });
    }
    m
}

/// Both intents are created on chain, the payer cancels both in ONE transaction that carries `pays` outputs to the merchant, and the
/// identical retries run in the order given (`first_second` false: the second intent is asked first). Returns whether each intent
/// settled, and what the merchant holds.
fn x14(amount: u64, pays: &[u64], first_second: bool) -> (bool, bool, Vec<u64>) {
    let w = world();
    let offer = w.offer_sell(amount);
    let p1 = w.pay_sell(&offer, TOKEN_COV, 60, 31, 6 * KAS);
    let p2 = w.pay_sell(&offer, TOKEN_B, 61, 32, 6 * KAS);
    let (req1, req2) = (w.request(&offer, &p1), w.request(&offer, &p2));
    let (stop, miner) = w.miner();
    w.fac.set_settle_wait(Duration::from_millis(300));
    // both creations reach the chain; the book cannot execute them afterwards, so both stay in flight
    w.mode.store(1, Ordering::SeqCst);
    let _ = w.fac.settle("shop", &req1);
    w.mode.store(1, Ordering::SeqCst);
    let _ = w.fac.settle("shop", &req2);
    for p in [&p1, &p2] {
        let e = w.ledger.get(&hex(&p.txid)).unwrap();
        assert!(w.chain.utxo(&e.intent.as_ref().unwrap().facts.outpoint()).is_some(), "intent on chain");
    }
    let b1 = cancel_intent(&p1, None).unwrap();
    let b2 = cancel_intent(&p2, None).unwrap();
    let merchant = p2pk_spk(&pk(MERCHANT));
    let mut margin = 200_000u64;
    let signed = loop {
        let mut m = merge_cancels(&b1, &b2, &merchant, pays, margin);
        let (tx, entries) = m.tx.to_tx().unwrap();
        for r in m.sign.iter_mut() {
            r.sighash = sighash(&tx, &entries, r.input_index);
        }
        let sigs = sign_locally(&m, &keys()).unwrap();
        match finalize(&m, &sigs, FinalizeOptions { tighten_budgets: true }) {
            Ok(s) => break s,
            Err(_) => {
                margin *= 2;
                assert!(margin < 50_000_000, "cannot build the merged cancel");
            }
        }
    };
    let (cancel, _) = signed.tx.to_tx().unwrap();
    w.chain.submit(&cancel).expect("consensus (script engine + covenant context) accepts the merged cancel");
    thread::sleep(Duration::from_millis(60));
    // the identical retries, twice (a refused intent never turns into a success)
    let (mut ok1, mut ok2) = (false, false);
    for _ in 0..2 {
        if first_second {
            ok1 |= w.fac.settle("shop", &req1).success;
            ok2 |= w.fac.settle("shop", &req2).success;
        } else {
            ok2 |= w.fac.settle("shop", &req2).success;
            ok1 |= w.fac.settle("shop", &req1).success;
        }
    }
    stop.store(true, Ordering::SeqCst);
    miner.join().unwrap();
    let held: Vec<u64> = w.chain.utxos_of(&merchant).unwrap().into_iter().map(|(_, u)| u.amount).collect();
    (ok1, ok2, held)
}

/// X14: one 5 KAS merchant output backs ONE intent, whichever is asked first.
#[test]
fn x14_one_merchant_output_settles_one_intent_only() {
    for first_second in [true, false] {
        let (ok1, ok2, held) = x14(5 * KAS, &[5 * KAS], first_second);
        assert_eq!(held, vec![5 * KAS]);
        assert_eq!(u8::from(ok1) + u8::from(ok2), 1, "exactly one intent is paid by one output (asked first: {first_second}): {ok1} {ok2}");
    }
}

/// X14 bypass: the merchant output is split in two, or doubled: a second intent finds its own unclaimed matching output.
#[test]
fn x14_two_outputs_settle_two_intents() {
    let (ok1, ok2, held) = x14(2 * KAS, &[2 * KAS, 2 * KAS], true);
    assert_eq!(held.len(), 2);
    assert!(ok1 && ok2, "two matching outputs back two intents: {ok1} {ok2}");
    // one output below the promised value and one at it: the small one pays nobody
    let (ok1, ok2, _) = x14(2 * KAS, &[2 * KAS - 1, 2 * KAS], true);
    assert_eq!(u8::from(ok1) + u8::from(ok2), 1, "a short output backs no intent: {ok1} {ok2}");
}

// ====================================================================================================================
// X15: the watch slot of a creation the node refuses is never released
// ====================================================================================================================

#[test]
fn x15_a_refused_creation_releases_its_watch_slot() {
    let w = world();
    let offer = w.offer_buy();
    *w.spy.cap.lock().unwrap() = 2;
    let (stop, miner) = w.miner();
    w.fac.set_settle_wait(Duration::from_millis(300));
    // the node refuses the creation (a standardness rule the verifier does not model, e.g. a dust change output)
    w.chain.set_submit_failure(Some(SubmitError::Rejected("transaction is not standard: dust output".into())));
    for i in 0..2 {
        let p = w.pay_buy(&offer, 31 + i);
        let r = w.fac.settle("shop", &w.request(&offer, &p));
        assert!(!r.success);
        assert_eq!(w.ledger.get(&hex(&p.txid)).unwrap().state, State::Failed);
    }
    // bypass: the Conflict arm (an input spent elsewhere) releases its slot too
    w.chain.set_submit_failure(Some(SubmitError::Conflict("input already spent".into())));
    for i in 0..3 {
        let p = w.pay_buy(&offer, 33 + i);
        assert!(!w.fac.settle("shop", &w.request(&offer, &p)).success);
    }
    let held = w.spy.watched.lock().unwrap().len();
    // nothing is in flight now; an honest payment follows
    w.chain.set_submit_failure(None);
    let p = w.pay_buy(&offer, 36);
    let r = w.fac.settle("shop", &w.request(&offer, &p));
    stop.store(true, Ordering::SeqCst);
    miner.join().unwrap();
    assert_eq!(held, 0, "X15: {held} watch slots stay held by creations that never reached the chain");
    assert!(r.success, "X15: a fail-closed refusal of an honest payment with an empty system");
}

#[test]
fn x9_the_same_payment_can_be_retried_after_the_full_table_refusal() {
    let w = world();
    let offer = w.offer_buy();
    let p = w.pay_buy(&offer, 31);
    let req = w.request(&offer, &p);
    *w.spy.cap.lock().unwrap() = 0;
    let r = w.fac.settle("shop", &req);
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    eprintln!("X9 full table: success={} {} entry={:?} reason={:?}", r.success, diag(&r), e.state, e.reason);
    assert!(!r.success);
    assert!(!w.chain.in_mempool(&p.txid).unwrap());
    *w.spy.cap.lock().unwrap() = usize::MAX;
    let (stop, miner) = w.miner();
    let again = w.fac.settle("shop", &req);
    stop.store(true, Ordering::SeqCst);
    miner.join().unwrap();
    eprintln!("X9 retry of the identical request: success={} {}", again.success, diag(&again));
    assert!(again.success, "the identical request settles once the table has room");
}

// ====================================================================================================================
// X18: a rival keeper may pay a smaller carrier, down to the policy minimum
// ====================================================================================================================

/// The offer quotes a 1.5 KAS carrier; a rival keeper executes the intent paying `rival_carrier` with the exact token state; the
/// facilitator's policy minimum carrier is `min_carrier`. Returns whether the payer is told "paid", and the number of token outputs
/// the merchant holds.
fn x18(rival_carrier: u64, min_carrier: u64) -> (bool, usize) {
    let w = world_with_min_carrier(min_carrier);
    let offer = w.offer_buy_carrier(KAS + KAS / 2);
    let p = w.pay_buy(&offer, 31);
    let mspk = merchant_token_spk(&offer);
    w.rival(p.txid, Some(rival_carrier));
    let (stop, miner) = w.miner();
    let r = w.fac.settle("shop", &w.request(&offer, &p));
    stop.store(true, Ordering::SeqCst);
    miner.join().unwrap();
    (r.success, w.chain.utxos_of(&mspk).unwrap().len())
}

#[test]
fn x18_a_rival_execution_with_the_minimum_carrier_pays_the_merchant() {
    let (paid, held) = x18(KAS, KAS);
    assert_eq!(held, 1, "the merchant holds the exact token amount");
    assert!(paid, "the payer is told paid");
}

/// X18 bypass: a carrier below the policy minimum is still no payment (the floor is the policy minimum, not zero).
#[test]
fn x18_a_carrier_below_the_policy_minimum_is_no_payment() {
    let (paid, held) = x18(KAS, KAS + KAS / 10);
    assert_eq!(held, 1);
    assert!(!paid);
}
