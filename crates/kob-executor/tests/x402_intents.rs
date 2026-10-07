//! The facilitator settles intent-based swap-and-pay (`kob-intent-v1`) and invoices end to end on its
//! in-memory chain (every transaction engine-validated on submit; a miner thread accepts the mempool).
//!
//! * an intent is created (the payer signs once), its first execution loses an order to a conflicting
//!   fill, the facilitator re-plans and settles WITHOUT a new payer signature;
//! * an expired invoice refuses a payment and reports it as late once it reaches the chain anyway;
//! * a second payment of a paid invoice is a duplicate, reported when it reaches the chain;
//! * a payment for another merchant (or another offer) is refused;
//! * an intent the book cannot execute ends at the invoice's deadline and the payer cancels it.
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig, IntentRuntime, InvoiceRuntime, InvoiceStore};
use kob_executor::x402::ledger::{Ledger, State};
use kob_executor::x402::testutil::is_repeat_of;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AskState, BidState, OrderState, SCHEME_COVID, SCHEME_P2PK};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, OrderUtxo, TokenUtxo, Utxo};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::client::intent::{cancel_intent, intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::native::{native_requirements, pay_native, PayOptions};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::intent::{BookView, KeeperParams};
use kob_x402::invoice::{Invoice, InvoiceState};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::testkit::MockChain;
use kob_x402::wire::{
    hex, FacilitatorRequest, Finality, Network, PaymentPayload, PaymentRequirements, SettlementResponse, X402_VERSION,
};

use common::{keys, pk, sk, CARRIER, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const OTHER_MERCHANT: u8 = 9;
const KEEPER: u8 = common::KEEPER;
const SHOP: &str = "shop";
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";

fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
}

// ------------------------------------------------------------------------------------------ chain with a rival

/// The mock chain, plus a rival taker: when armed with an order outpoint, the first submitted transaction
/// that spends it is preceded by a rival fill of that order (the order is gone; the submit conflicts).
struct RivalChain {
    inner: Arc<MockChain>,
    rival: Mutex<Option<Outpoint>>,
    rivals: std::sync::atomic::AtomicU64,
    /// (transaction id, fee) of every submitted transaction whose inputs the chain knows.
    fees: Mutex<Vec<(Txid, u64)>>,
}

impl ChainView for RivalChain {
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        self.inner.utxos(wanted)
    }
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        self.inner.utxos_of(spk)
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        self.inner.virtual_daa_score()
    }
    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        self.inner.output_status(out, spk)
    }
    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError> {
        self.inner.in_mempool(txid)
    }
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let mut r = self.rival.lock().unwrap();
        if let Some(o) = *r {
            if tx
                .inputs
                .iter()
                .any(|i| i.previous_outpoint.transaction_id.as_bytes() == o.txid && i.previous_outpoint.index == o.index)
            {
                self.inner.spend_externally(&o);
                self.rivals.fetch_add(1, Ordering::SeqCst);
                *r = None;
            }
        }
        drop(r);
        let ins: Option<u64> = tx
            .inputs
            .iter()
            .map(|i| {
                self.inner
                    .utxo(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index))
                    .map(|u| u.amount)
            })
            .sum();
        if let Some(ins) = ins {
            let outs: u64 = tx.outputs.iter().map(|o| o.value).sum();
            self.fees.lock().unwrap().push((tx.id().as_bytes(), ins.saturating_sub(outs)));
        }
        self.inner.submit(tx)
    }
    fn replace(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        self.inner.replace(tx)
    }
}

// ------------------------------------------------------------------------------------------ world

fn allowed(id: [u8; 32], ticker: &str) -> AllowedToken {
    AllowedToken::new(id, P8, EXT, Custody::Unconditional, ticker, 3)
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
    view: Arc<RivalChain>,
    clock: Arc<FixedClock>,
    policy: Policy,
    book: Arc<Mutex<BookView>>,
    funds: PayerFunds,
    fac: Arc<Facilitator>,
    ledger: Arc<Ledger>,
}

fn world() -> World {
    let chain = Arc::new(MockChain::new());
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.tokens.insert(allowed(TOKEN_COV, "AAA")).unwrap();
    policy.tokens.insert(allowed(TOKEN_B, "BBB")).unwrap();
    policy.limits.max_fee_sompi = KAS;
    let mut book = BookView::default();
    // the best bid (2.50) is the keeper's first choice; the second (2.45) is the fallback
    for b in [bid_a(20, common::MAKER_B, P250, 10), bid_a(21, common::MAKER_C, P245, 10)] {
        put(&chain, &b.utxo, b.state.spk());
        book.bids.push(b);
    }
    {
        let a = ask_b(10, common::MAKER_A, 10);
        put(&chain, &a.0.utxo, a.0.state.spk());
        put(&chain, &a.1.utxo, a.1.state.spk_with(kob_protocol::artifacts::token_template(P8)));
        book.asks.push(a);
    }
    let tok = common::tok_of(30, 9 * WHOLE, pk(PAYER), SCHEME_P2PK, 900, TOKEN_COV);
    put(&chain, &tok.utxo, tok.state.spk_with(kob_protocol::artifacts::token_template(P8)));
    let funding = vec![common::key_utxo(31, PAYER, 100 * KAS), common::key_utxo(32, PAYER, 50 * KAS)];
    for f in &funding {
        put(&chain, &f.utxo, p2pk_spk(&f.pubkey));
    }
    let view =
        Arc::new(RivalChain { inner: chain.clone(), rival: Mutex::new(None), rivals: Default::default(), fees: Default::default() });
    let clock = Arc::new(FixedClock::new(NOW_MS));
    let ledger = Arc::new(Ledger::in_memory());
    let book = Arc::new(Mutex::new(book));
    let b2 = book.clone();
    let fac = Facilitator::new(
        policy.clone(),
        view.clone(),
        clock.clone(),
        ledger.clone(),
        FacilitatorConfig {
            settle_wait: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
            reorg_watch_daa: 10_000,
            kill_switch_file: None,
            ..Default::default()
        },
    )
    .with_intents(IntentRuntime {
        book: Arc::new(move || Ok(b2.lock().unwrap().clone())),
        keeper: KeeperParams { keeper: pk(KEEPER), ..KeeperParams::default() },
        max_attempts: 10,
        lock_margin_daa: 0,
    })
    .with_invoices(InvoiceRuntime {
        store: Arc::new(InvoiceStore::in_memory()),
        max_lifetime_ms: 86_400_000,
        public_url: Some("https://pay.example".into()),
        max_open_per_merchant: 100,
        max_extra_payments: 16,
    });
    World {
        chain,
        view,
        clock,
        policy,
        book,
        funds: PayerFunds { tokens: vec![tok], funding, change: pk(PAYER) },
        fac: Arc::new(fac),
        ledger,
    }
}

/// Mines the mempool every few ms while `f` runs.
fn with_miner<T>(w: &World, f: impl FnOnce() -> T) -> T {
    let stop = Arc::new(AtomicBool::new(false));
    let (chain, s2) = (w.chain.clone(), stop.clone());
    let miner = thread::spawn(move || {
        while !s2.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(3));
            chain.mine(0);
        }
    });
    let r = f();
    stop.store(true, Ordering::SeqCst);
    miner.join().unwrap();
    r
}

impl World {
    fn tok(&self, id: [u8; 32]) -> &AllowedToken {
        self.policy.tokens.find(&id).unwrap()
    }
    fn kas_intent_offer(&self, amount: u64, merchant: u8) -> PaymentRequirements {
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
    fn kas_offer(&self, amount: u64, merchant: u8) -> PaymentRequirements {
        native_requirements(Network::Testnet10, amount, &addr(merchant), 600, Finality::Accepted).unwrap()
    }
    fn pay_t2k(&self, offer: &PaymentRequirements, rh: &str, max_sell: i64) -> IntentPayment {
        let opts = IntentOptions { max_sell: Some(max_sell), ..IntentOptions::default() };
        // the payer funds it from its unspent coins
        let unspent = |u: &Utxo| self.chain.utxo(&Outpoint::new(u.transaction_id, u.index)).is_some();
        let funds =
            PayerFunds { funding: self.funds.funding.iter().filter(|f| unspent(&f.utxo)).cloned().collect(), ..self.funds.clone() };
        pay_intent(&self.policy, offer, &hex(&TOKEN_COV), rh, &keys(), &funds, NOW_MS, &opts).unwrap()
    }
    fn pay_kas(&self, offer: &PaymentRequirements, rh: &str, utxo: usize) -> PaymentPayload {
        let opts = PayOptions { ttl_seconds: Some(300), ..PayOptions::new(offer.amount_u64().unwrap()) };
        pay_native(offer, rh, &sk(PAYER), &self.funds.funding[utxo..utxo + 1], NOW_MS, &opts).unwrap()
    }
    fn invoice(&self, accepts: Vec<PaymentRequirements>, expires_in_ms: u64) -> String {
        let inv = Invoice::new(Network::Testnet10, "order-42", NOW_MS + expires_in_ms, Some("2 coffees".into()), accepts);
        let r = self.fac.register_invoice(SHOP, inv, &|_| true).unwrap();
        assert_eq!(r["url"], format!("https://pay.example/invoices/{}", r["id"].as_str().unwrap()));
        r["id"].as_str().unwrap().to_string()
    }
    fn status(&self, id: &str) -> kob_x402::invoice::InvoiceStatus {
        self.fac.invoice_status(id).unwrap()
    }
}

fn request(offer: &PaymentRequirements, p: &PaymentPayload) -> FacilitatorRequest {
    FacilitatorRequest {
        x402_version: X402_VERSION,
        payment_payload: p.clone(),
        payment_requirements: offer.clone(),
        request_hash: Some(RH.into()),
        resource: None,
    }
}

fn diag(r: &SettlementResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

// ------------------------------------------------------------------------------------------ tests

#[test]
fn an_intent_settles_after_a_conflicting_fill_without_a_new_signature() {
    let w = world();
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    // verify never broadcasts
    let v = w.fac.verify(&request(&offer, &p.payload));
    assert!(v.is_valid, "{v:?}");
    assert_eq!(w.chain.submit_count(), 0);
    // a rival takes the best bid just before the keeper's first execution reaches the node
    let best = w.book.lock().unwrap().bids[0].utxo.clone();
    *w.view.rival.lock().unwrap() = Some(Outpoint::new(best.transaction_id, best.index));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert!(r.success, "{r:?}");
    assert_eq!(w.view.rivals.load(Ordering::SeqCst), 1, "the first execution lost its order");
    let kob = &r.extensions.as_ref().unwrap()["kob"]["intent"];
    assert_eq!(kob["creation"], hex(&p.txid));
    assert_eq!(kob["executions"], 2, "re-planned once, without the payer");
    assert_ne!(r.transaction, hex(&p.txid), "the settlement reports the execution, not the creation");
    // the merchant output of the execution: exactly the offer's amount to the merchant
    let mi = r.extensions.as_ref().unwrap()["kaspa"]["paymentOutputIndex"].as_u64().unwrap() as u32;
    let out = w.chain.utxo(&Outpoint::new(kob_x402::wire::parse_hash32(&r.transaction).unwrap(), mi)).unwrap();
    assert_eq!((out.amount, out.script_public_key), (5 * KAS, p2pk_spk(&pk(MERCHANT))));
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Accepted);
    let rec = e.intent.unwrap();
    assert_eq!(rec.lost.len(), 1, "the lost order is remembered");
    // an identical retry returns the cached response
    let again = w.fac.settle(SHOP, &request(&offer, &p.payload));
    assert!(is_repeat_of(&again, &r), "{again:?}");
    // the creation is public once broadcast: under another payment identifier, or none, it is not this payment
    let mut other = p.payload.clone();
    other.extensions =
        Some(serde_json::json!({ "payment-identifier": { "info": { "required": true, "id": "another-payer-id-000000001" } } }));
    let o = w.fac.settle(SHOP, &request(&offer, &other));
    assert!(!o.success, "{o:?}");
    assert_eq!(diag(&o), "kaspa_payment_identifier_conflict");
    other.extensions = None;
    assert!(!w.fac.settle(SHOP, &request(&offer, &other)).success);
    assert!(w.ledger.get_by_payment_id("another-payer-id-000000001").is_none());
}

#[test]
fn an_expired_invoice_refuses_and_reports_a_late_payment() {
    let w = world();
    let offer = w.kas_offer(KAS, MERCHANT);
    let id = w.invoice(vec![offer.clone()], 60_000);
    assert_eq!(w.status(&id).status, InvoiceState::Unpaid);
    let late = w.pay_kas(&offer, &id, 0);
    w.clock.set(NOW_MS + 61_000);
    assert_eq!(w.status(&id).status, InvoiceState::Expired);
    let r = w.fac.pay_invoice(&id, late.clone());
    assert!(!r.success);
    assert_eq!(diag(&r), "invoice_expired");
    assert_eq!(w.chain.submit_count(), 0, "a late payment is not broadcast");
    let s = w.status(&id);
    assert_eq!(s.extra_payments.len(), 1);
    assert_eq!((s.extra_payments[0].kind.as_str(), s.extra_payments[0].observed.as_str()), ("late", "refused"));
    // the payer broadcasts it anyway: the reconcile reports it (a refund is due)
    let tx = kob_x402::safe_tx::SafeTx::parse(&late.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap().tx;
    w.chain.submit(&tx).unwrap();
    w.chain.mine(1);
    let rep = w.fac.reconcile();
    assert_eq!(rep.extra_payments_seen, 1);
    let s = w.status(&id);
    assert_eq!(s.status, InvoiceState::Expired);
    assert_eq!(s.extra_payments[0].observed, "accepted");
    assert!(s.extra_payments[0].accepted_daa_score.is_some());
}

#[test]
fn a_second_payment_of_a_paid_invoice_is_a_duplicate() {
    let w = world();
    let offer = w.kas_offer(KAS, MERCHANT);
    let id = w.invoice(vec![offer.clone(), w.kas_intent_offer(5 * KAS, MERCHANT)], 600_000);
    let first = w.pay_kas(&offer, &id, 0);
    let r = with_miner(&w, || w.fac.pay_invoice(&id, first.clone()));
    assert!(r.success, "{r:?}");
    let s = w.status(&id);
    assert_eq!(s.status, InvoiceState::Paid);
    assert_eq!(s.payment.as_ref().unwrap().transaction, r.transaction);
    assert_eq!(s.payment.as_ref().unwrap().accepted_index, 0);
    // the identical retry is not a duplicate
    let again = w.fac.pay_invoice(&id, first);
    assert!(is_repeat_of(&again, &r), "{again:?}");
    // another payment (another UTXO) of the same invoice: refused before broadcast, kept as evidence
    let submits = w.chain.submit_count();
    let second = w.pay_kas(&offer, &id, 1);
    let d = w.fac.pay_invoice(&id, second.clone());
    assert!(!d.success);
    assert_eq!(diag(&d), "invoice_paid");
    assert_eq!(w.chain.submit_count(), submits);
    // so is an intent payment of the paid invoice
    let intent_offer = w.fac.get_invoice(&id).unwrap().accepts[1].clone();
    let p = w.pay_t2k(&intent_offer, &id, 3 * WHOLE);
    assert_eq!(diag(&w.fac.pay_invoice(&id, p.payload.clone())), "invoice_paid");
    let s = w.status(&id);
    assert_eq!(s.extra_payments.len(), 2);
    assert!(s.extra_payments.iter().all(|x| x.kind == "duplicate" && x.observed == "refused"));
    // the second KAS payment reaches the chain anyway: reported
    let tx = kob_x402::safe_tx::SafeTx::parse(&second.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap().tx;
    w.chain.submit(&tx).unwrap();
    w.chain.mine(1);
    w.fac.reconcile();
    let s = w.status(&id);
    assert_eq!(s.status, InvoiceState::Paid);
    let dup = s.extra_payments.iter().find(|x| x.transaction == hex(&tx.id().as_bytes())).unwrap();
    assert_eq!(dup.observed, "accepted");
}

#[test]
fn a_payment_for_another_merchant_or_offer_is_refused() {
    let w = world();
    let mine = w.kas_intent_offer(5 * KAS, MERCHANT);
    let id = w.invoice(vec![mine.clone()], 600_000);
    // the payer built its intent for another merchant's offer of the same amount
    let other = w.kas_intent_offer(5 * KAS, OTHER_MERCHANT);
    let p = w.pay_t2k(&other, &id, 3 * WHOLE);
    let r = w.fac.pay_invoice(&id, p.payload.clone());
    assert_eq!(diag(&r), "invalid_kaspa_x402_accepted");
    // ... and relabelled it as the invoice's entry: the commitment in the signed creation does not match
    let mut forged = p.payload.clone();
    forged.accepted = mine.clone();
    let r = w.fac.pay_invoice(&id, forged);
    assert_eq!(diag(&r), "invalid_authorization");
    // an intent signed for another request (not this invoice) is refused likewise
    let p = w.pay_t2k(&mine, RH, 3 * WHOLE);
    assert_eq!(diag(&w.fac.pay_invoice(&id, p.payload.clone())), "invalid_kaspa_x402_payload");
    assert_eq!(w.chain.submit_count(), 0, "nothing was broadcast");
    assert_eq!(w.status(&id).status, InvoiceState::Unpaid);
    // unknown invoices
    let r = w.fac.pay_invoice(&"ab".repeat(32), p.payload);
    assert_eq!(diag(&r), "invoice_unknown");
}

#[test]
fn an_unexecuted_intent_ends_at_the_deadline_and_the_payer_cancels_it() {
    let w = world();
    // the invoice expires before the intent's own authorization: the invoice caps the deadline
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    let id = w.invoice(vec![offer.clone()], 120_000);
    // one bid only, and a rival takes it as soon as the keeper's execution names it
    {
        let mut b = w.book.lock().unwrap();
        b.bids.truncate(1);
        let o = b.bids[0].utxo.clone();
        *w.view.rival.lock().unwrap() = Some(Outpoint::new(o.transaction_id, o.index));
    }
    let p = w.pay_t2k(&offer, &id, 3 * WHOLE);
    w.fac.set_settle_wait(Duration::from_millis(300));
    let r = with_miner(&w, || w.fac.pay_invoice(&id, p.payload.clone()));
    assert!(!r.success);
    assert_eq!(diag(&r), "settlement_pending", "{r:?}");
    assert_eq!(w.status(&id).status, InvoiceState::Pending);
    // liquidity comes back only after the deadline: the facilitator does not execute any more
    w.clock.set(NOW_MS + 121_000);
    w.book.lock().unwrap().bids.push(bid_a(22, common::MAKER_A, P245, 10));
    let o = w.book.lock().unwrap().bids.last().unwrap().clone();
    put(&w.chain, &o.utxo, o.state.spk());
    let rep = w.fac.reconcile();
    assert_eq!(rep.intents_driven, 1);
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(e.reason.as_deref().unwrap().starts_with("intent_expired"), "{:?}", e.reason);
    let s = w.status(&id);
    assert_eq!(s.status, InvoiceState::Expired);
    assert_eq!(s.attempts.len(), 1);
    // the facilitator will expire the intent on chain, but not before the intent's own deadline (the authorization's
    // expiry, 300 s): the invoice ended the payment earlier
    let x = e.intent.as_ref().unwrap().expiry.clone().expect("expiry pending");
    assert_eq!((x.txid.as_deref(), x.outcome.as_deref()), (None, None));
    let submits = w.chain.submit_count();
    assert!(w.fac.reconcile().expiries_driven >= 1);
    assert_eq!(w.chain.submit_count(), submits, "no expiry before the intent's deadline");
    // the payer cancels: its KAS and its locked tokens come back
    let built = cancel_intent(&p, None).unwrap();
    let sigs = sign_locally(&built, &keys()).unwrap();
    let (tx, _) = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap().tx.to_tx().unwrap();
    w.chain.submit(&tx).unwrap();
    w.chain.mine(1);
    assert!(w.chain.utxo(&Outpoint::new(p.intent.transaction_id, p.intent.index)).is_none());
    let back = w.chain.unspent_of(
        &kob_protocol::state::TokenState::user(kob_protocol::Family::Kcc20, 3 * WHOLE, pk(PAYER), EXT)
            .spk_with(kob_protocol::artifacts::token_template(P8)),
    );
    assert_eq!(back.len(), 1, "the locked tokens are the payer's again");
    w.fac.reconcile();
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert_eq!(e.intent.unwrap().expiry.unwrap().outcome.as_deref(), Some("spent"), "the cancel ended the intent");
}

#[test]
fn an_unexecuted_intent_is_expired_on_chain_by_the_facilitator_after_its_deadline() {
    let w = world();
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    // one bid only, and a rival takes it as soon as the keeper's execution names it: nothing can execute the intent
    {
        let mut b = w.book.lock().unwrap();
        b.bids.truncate(1);
        let o = b.bids[0].utxo.clone();
        *w.view.rival.lock().unwrap() = Some(Outpoint::new(o.transaction_id, o.index));
    }
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    assert_eq!(p.state.deadline(), (NOW_MS + 300_000) as i64, "the intent's deadline is the authorization's expiry");
    w.fac.set_settle_wait(Duration::from_millis(300));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert_eq!(diag(&r), "settlement_pending", "{r:?}");
    // at the deadline the payment ends, and the facilitator submits the expiry (no signature): the router returns the
    // intent's KAS and the locked tokens to the payer, and nobody can execute the intent any more
    w.clock.set(NOW_MS + 300_000);
    let rep = w.fac.reconcile();
    assert_eq!(rep.intents_driven, 1);
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(e.reason.as_deref().unwrap().starts_with("intent_expired"), "{:?}", e.reason);
    let x = e.intent.unwrap().expiry.expect("expiry");
    let expiry = kob_x402::wire::parse_hash32(x.txid.as_deref().expect("the expiry was submitted")).unwrap();
    assert_eq!(x.outcome, None);
    w.chain.mine(1);
    assert!(w.chain.is_accepted(&expiry));
    assert!(w.chain.utxo(&Outpoint::new(p.intent.transaction_id, p.intent.index)).is_none(), "the intent is spent");
    let back = w.chain.unspent_of(
        &kob_protocol::state::TokenState::user(kob_protocol::Family::Kcc20, 3 * WHOLE, pk(PAYER), EXT)
            .spk_with(kob_protocol::artifacts::token_template(P8)),
    );
    assert_eq!(back.len(), 1, "the locked tokens are the payer's again");
    let kas = w.chain.unspent_of(&p2pk_spk(&pk(PAYER)));
    assert!(kas.iter().any(|(o, u)| o.txid == expiry && u.amount + kob_protocol::router::EXPIRE_MAX_FEE >= p.intent.amount));
    w.fac.reconcile();
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.intent.unwrap().expiry.unwrap().outcome.as_deref(), Some("expired"));
}

/// While the facilitator is paused (kill switch, `killSwitchFile`, or `run --pause-file`) the reconcile loop broadcasts
/// nothing: no intent execution and no expiry. The intent waits and is expired once the pause is lifted.
#[test]
fn a_paused_facilitator_broadcasts_no_intent_execution_or_expiry() {
    let w = world();
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    {
        let mut b = w.book.lock().unwrap();
        b.bids.truncate(1);
        let o = b.bids[0].utxo.clone();
        *w.view.rival.lock().unwrap() = Some(Outpoint::new(o.transaction_id, o.index));
    }
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    w.fac.set_settle_wait(Duration::from_millis(300));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert_eq!(diag(&r), "settlement_pending", "{r:?}");
    w.fac.set_kill(true);
    let submits = w.chain.submit_count();
    w.clock.set(NOW_MS + 300_000);
    for _ in 0..3 {
        w.fac.reconcile();
    }
    assert_eq!(w.chain.submit_count(), submits, "nothing is broadcast while paused");
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_ne!(e.state, State::Failed, "the intent waits");
    assert!(e.intent.unwrap().expiry.is_none_or(|x| x.txid.is_none()));
    // lifted: the deadline has passed, the facilitator ends the intent and submits the expiry
    w.fac.set_kill(false);
    w.fac.reconcile();
    w.fac.reconcile();
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(e.intent.unwrap().expiry.expect("expiry").txid.is_some(), "the expiry was submitted");
    assert!(w.chain.submit_count() > submits);
}

/// C5 X-8: an expiry that waits in the mempool is replaced (replace-by-fee) at the high rate a minute later: the new one
/// pays more within EXPIRE_MAX_FEE, takes the old one's place, and ends the intent.
#[test]
fn a_waiting_expiry_is_replaced_at_the_high_rate() {
    let w = world();
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    {
        let mut b = w.book.lock().unwrap();
        b.bids.truncate(1);
        let o = b.bids[0].utxo.clone();
        *w.view.rival.lock().unwrap() = Some(Outpoint::new(o.transaction_id, o.index));
    }
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    w.fac.set_settle_wait(Duration::from_millis(300));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert_eq!(diag(&r), "settlement_pending", "{r:?}");
    // the expiry at the normal rate (the floor here), left waiting in the mempool
    w.clock.set(NOW_MS + 300_000);
    w.fac.reconcile();
    let x = w.ledger.get(&hex(&p.txid)).unwrap().intent.unwrap().expiry.unwrap();
    let first = kob_x402::wire::parse_hash32(x.txid.as_deref().expect("submitted")).unwrap();
    assert_eq!(x.rate, Some(100));
    assert!(w.chain.in_mempool(&first).unwrap());
    let fee_of = |t: Txid| w.view.fees.lock().unwrap().iter().find(|(x, _)| *x == t).map(|(_, f)| *f);
    // the estimate rises; before EXPIRY_BUMP_MS nothing is replaced
    *w.fac.fees.lock().unwrap() = board(1_000, 0).unwrap();
    w.clock.set(NOW_MS + 300_000 + 30_000);
    w.fac.reconcile();
    assert_eq!(w.chain.replace_count(), 0, "not before a minute in the mempool");
    w.clock.set(NOW_MS + 300_000 + kob_executor::x402::facilitator::EXPIRY_BUMP_MS + 1);
    w.fac.reconcile();
    assert_eq!(w.chain.replace_count(), 1);
    let x = w.ledger.get(&hex(&p.txid)).unwrap().intent.unwrap().expiry.unwrap();
    let second = kob_x402::wire::parse_hash32(x.txid.as_deref().unwrap()).unwrap();
    assert_ne!(second, first);
    assert!(x.rate.unwrap() > 100, "{:?}", x.rate);
    assert!(!w.chain.in_mempool(&first).unwrap() && w.chain.in_mempool(&second).unwrap(), "the replacement took its place");
    w.chain.mine(1);
    assert!(w.chain.is_accepted(&second));
    let kas = w.chain.unspent_of(&p2pk_spk(&pk(PAYER)));
    let back = kas.iter().find(|(o, _)| o.txid == second).map(|(_, u)| u.amount).unwrap();
    assert!(back + kob_protocol::router::EXPIRE_MAX_FEE >= p.intent.amount, "the replacement pays within EXPIRE_MAX_FEE");
    assert!(p.intent.amount - back > fee_of(first).unwrap_or(0), "and more than the expiry it replaced");
    w.fac.reconcile();
    assert_eq!(w.ledger.get(&hex(&p.txid)).unwrap().intent.unwrap().expiry.unwrap().outcome.as_deref(), Some("expired"));
}

// ------------------------------------------------------------------------------------------ fee policy

/// The fee of the execution that settles a fresh intent, with `rates` on the facilitator's fee board (none: the floor).
fn execution_fee(rates: Option<kob_executor::fee::FeeRates>) -> u64 {
    let w = world();
    if let Some(r) = rates {
        *w.fac.fees.lock().unwrap() = r;
    }
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert!(r.success, "{r:?}");
    let txid = kob_x402::wire::parse_hash32(&r.transaction).unwrap();
    let fees = w.view.fees.lock().unwrap();
    fees.iter().find(|(t, _)| *t == txid).map(|(_, f)| *f).expect("the execution's inputs were known")
}

fn board(high: u64, max_tx_fee: u64) -> Option<kob_executor::fee::FeeRates> {
    Some(kob_executor::fee::FeeRates { floor: 100, low: 100, normal: 200, high, max_tx_fee, estimated: true })
}

#[test]
fn intent_executions_pay_the_high_rate_within_the_cap_and_what_the_intent_holds() {
    let floor = execution_fee(None);
    // the runner's high rate (the merchant waits): four times the floor's fee
    let high = execution_fee(board(400, 0));
    assert!((4 * floor - 4 * floor / 20..=4 * floor + 4 * floor / 20).contains(&high), "floor {floor}, at 400: {high}");
    // the total cap: at most it
    let capped = execution_fee(board(400, 2 * floor));
    assert!(capped <= 2 * floor && capped > floor, "floor {floor}, capped at {}: {capped}", 2 * floor);
    // a rate the intent cannot pay: the keeper's own rate
    assert_eq!(execution_fee(board(10_000_000, 0)), floor);
}
