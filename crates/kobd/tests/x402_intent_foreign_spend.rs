//! An intent spent outside the facilitator settles only on the output the router fixes for the merchant (review p6, F1).
//!
//! Offline: an in-memory chain whose submit runs the real script engine, the real verifier and the real facilitator, with the
//! spend reports the chain follower gives `kobd --executor`.
//!
//! The payer's `cancel` checks only the payer's signature, so it can share a transaction with anything that pays the merchant's
//! own money back to the merchant: the payout of the merchant's own `KobAsk`, the refund of the merchant's own `KobBid`, or a token
//! delivery into the merchant's `KobBid`. None of those may settle the payment. An execution by another keeper still does.
#![allow(clippy::type_complexity)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AskState, BidState, OrderState, SCHEME_COVID, SCHEME_P2PK};
use kob_protocol::tx::{
    finalize, sighash, sign_locally, spk_to_string, FinalizeOptions, KeyUtxo, OrderUtxo, SigPlan, SignRequest, TokenUtxo, TxInputJson,
    TxOutputJson, Utxo, UtxoJson,
};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SpentBy, SpentOutput, SubmitError, Txid};
use kob_x402::client::intent::{cancel_intent, intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::intent::{build_execution, BookView, KeeperParams};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::spk_from_hex;
use kob_x402::testkit::MockChain;
use kob_x402::wire::{hex, FacilitatorRequest, Finality, Network, PaymentRequirements, SettlementResponse, X402_VERSION};
use kobd::x402::facilitator::{Facilitator, FacilitatorConfig, IntentRuntime};
use kobd::x402::ledger::{Ledger, State};

use common::{keys, pk, CARRIER, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

const NOW_MS: u64 = 1_800_000_000_000;
const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const PAYER_K: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const KEEPER: u8 = common::KEEPER;
const RIVAL_KEEPER: u8 = 19;
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
/// What the KAS merchant's offer asks for.
const KAS_AMOUNT: u64 = 5 * KAS;

type Rewrite = Box<dyn Fn(SpentBy) -> SpentBy + Send + Sync>;

/// A chain that runs a hook right before the first submit of a transaction spending an output of the creation, and that can
/// rewrite the follower's report of a spend (the transaction the follower saw is a larger one than the mock chain ran).
struct HookChain {
    inner: Arc<MockChain>,
    creation: Mutex<Option<Txid>>,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    rewrite: Mutex<Option<Rewrite>>,
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
    fn watch_spend(&self, o: &Outpoint) -> bool {
        self.inner.watch_spend(o)
    }
    fn unwatch_spend(&self, o: &Outpoint) {
        self.inner.unwatch_spend(o)
    }
    fn spent_by(&self, o: &Outpoint) -> SpentBy {
        let s = self.inner.spent_by(o);
        match (&s, self.rewrite.lock().unwrap().as_ref()) {
            (SpentBy::Spent { .. }, Some(f)) => f(s),
            _ => s,
        }
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

/// What the merchant receives.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Merchant {
    /// KAS, paid with token A (TokenToKas: the router pins the merchant's KAS at output j).
    Kas,
    /// 2 whole units of token B, paid with KAS (KasToToken: the router pins the merchant's token B at output j + 1).
    Token,
}

struct World {
    chain: Arc<MockChain>,
    hook: Arc<HookChain>,
    ledger: Arc<Ledger>,
    book: Arc<Mutex<BookView>>,
    policy: Policy,
    funds: PayerFunds,
    fac: Facilitator,
    merchant: Merchant,
}

fn world(merchant: Merchant) -> World {
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
    let hook =
        Arc::new(HookChain { inner: chain.clone(), creation: Mutex::new(None), hook: Mutex::new(None), rewrite: Mutex::new(None) });
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
    World { chain, hook, ledger, book, policy, funds, fac, merchant }
}

impl World {
    fn offer(&self) -> PaymentRequirements {
        let addr = Address::new(Prefix::Testnet, Version::PubKey, &pk(MERCHANT)).to_string();
        let (amount, gain, pay_assets) = match self.merchant {
            Merchant::Kas => (KAS_AMOUNT, MerchantGain::Kas, vec![PayAssetSpec::Token(self.policy.tokens.find(&TOKEN_COV).unwrap())]),
            Merchant::Token => (
                2 * WHOLE as u64,
                MerchantGain::Token { token: self.policy.tokens.find(&TOKEN_B).unwrap(), carrier: KAS },
                vec![PayAssetSpec::Kas],
            ),
        };
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr,
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain,
            pay_assets,
        })
        .unwrap()
    }
    fn pay(&self, offer: &PaymentRequirements) -> IntentPayment {
        match self.merchant {
            Merchant::Kas => {
                let opts = IntentOptions { max_sell: Some(3 * WHOLE), ..IntentOptions::default() };
                pay_intent(&self.policy, offer, &hex(&TOKEN_COV), RH, &keys(), &self.funds, NOW_MS, &opts).unwrap()
            }
            Merchant::Token => {
                let funds = PayerFunds { tokens: vec![], ..self.funds.clone() };
                let opts = IntentOptions { max_pay: Some(2 * 250_000_000), max_extra: Some(2 * KAS), ..IntentOptions::default() };
                pay_intent(&self.policy, offer, "KAS", RH, &keys(), &funds, NOW_MS, &opts).unwrap()
            }
        }
    }
    /// The merchant's script: P2PK for KAS, the token script over the merchant's state for token B.
    fn merchant_spk(&self, offer: &PaymentRequirements) -> ScriptPublicKey {
        match self.merchant {
            Merchant::Kas => p2pk_spk(&pk(MERCHANT)),
            Merchant::Token => {
                spk_from_hex(offer.extra["token"]["tokenScriptPublicKey"].as_str().expect("token script in the offer")).unwrap()
            }
        }
    }
    /// An output that pays the merchant everything the offer promises, at the merchant's script, as the merchant's own ask payout,
    /// bid refund or a delivery into its bid does.
    fn merchant_paying_output(&self, offer: &PaymentRequirements, value: u64) -> SpentOutput {
        let covenant_id = (self.merchant == Merchant::Token).then_some(TOKEN_B);
        SpentOutput { script_public_key: self.merchant_spk(offer), value, covenant_id }
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
        let req = FacilitatorRequest {
            x402_version: X402_VERSION,
            payment_payload: p.payload.clone(),
            payment_requirements: offer.clone(),
            request_hash: Some(RH.into()),
            resource: None,
        };
        let r = self.fac.settle("shop", &req);
        stop.store(true, Ordering::SeqCst);
        miner.join().unwrap();
        r
    }
    /// The payer's cancel of `p` is accepted right before the facilitator's own execution reaches the node.
    fn cancel_first(&self, p: &IntentPayment, cancel: Transaction) {
        *self.hook.creation.lock().unwrap() = Some(p.txid);
        let chain = self.chain.clone();
        *self.hook.hook.lock().unwrap() = Some(Box::new(move || {
            chain.submit(&cancel).expect("the script engine accepts the payer's cancel");
            chain.mine(0);
        }));
    }
    #[track_caller]
    fn assert_intent_spent(&self, p: &IntentPayment, r: &SettlementResponse, what: &str) {
        assert!(!r.success, "{what}: the merchant was told it is paid: {r:?}");
        let e = self.ledger.get(&hex(&p.txid)).unwrap();
        assert_eq!(e.state, State::Failed, "{what}");
        assert!(e.reason.as_deref().unwrap_or("").starts_with("intent_spent"), "{what}: {:?}", e.reason);
    }
}

/// The payer's signed cancel of `p`. With `extra` outputs, they are appended and funded by one more input of the payer, `coin`,
/// appended too.
fn payer_cancel(p: &IntentPayment, extra: &[TxOutputJson], coin: Option<&KeyUtxo>) -> Transaction {
    let mut b = cancel_intent(p, None).unwrap();
    if let Some(k) = coin {
        let sequence = b.tx.inputs[0].sequence;
        b.tx.inputs.push(TxInputJson {
            transaction_id: k.utxo.transaction_id,
            index: k.utxo.index,
            sequence,
            sig_op_count: 1,
            compute_budget: kob_protocol::budget::lookup("p2pk").unwrap(),
            signature_script: vec![],
            utxo: UtxoJson {
                address: None,
                amount: k.utxo.amount,
                script_public_key: spk_to_string(&p2pk_spk(&k.pubkey)),
                block_daa_score: k.utxo.block_daa_score,
                is_coinbase: false,
                covenant_id: None,
            },
        });
        b.plans.push(SigPlan::P2pk { pubkey: k.pubkey });
        b.roles.push("p2pk".into());
        b.sign.push(SignRequest {
            input_index: b.tx.inputs.len() - 1,
            pubkey: k.pubkey,
            sighash_type: 1,
            sighash: [0; 32],
            redeem_script: None,
        });
        let paid: u64 = extra.iter().map(|o| o.value).sum();
        b.tx.outputs.push(TxOutputJson {
            value: k.utxo.amount - paid - 100_000,
            script_public_key: spk_to_string(&p2pk_spk(&k.pubkey)),
            covenant: None,
        });
    }
    b.tx.outputs.extend(extra.iter().cloned());
    let (tx, entries) = b.tx.to_tx().unwrap();
    for r in b.sign.iter_mut() {
        r.sighash = sighash(&tx, &entries, r.input_index);
    }
    let sigs = sign_locally(&b, &keys()).unwrap();
    let signed = finalize(&b, &sigs, FinalizeOptions { tighten_budgets: true }).expect("finalize the cancel");
    signed.tx.to_tx().unwrap().0
}

/// The follower's report of a larger transaction around the spend: `before` inputs (and their outputs, one each) come before
/// the intent's input, and `after` outputs are appended.
fn composed(before: Vec<SpentOutput>, after: Vec<SpentOutput>) -> Rewrite {
    Box::new(move |s| match s {
        SpentBy::Spent { txid, block_daa_score, input_index, sig_script, outputs } => SpentBy::Spent {
            txid,
            block_daa_score,
            input_index: input_index + before.len() as u32,
            sig_script,
            outputs: before.iter().cloned().chain(outputs).chain(after.iter().cloned()).collect(),
        },
        other => other,
    })
}

/// The payer's cancel shares a transaction with a fill of the merchant's own KobAsk. The ask is input 0 and pays the merchant
/// at output 0, at least the offer's amount; the intent is input 1, its lock input 2.
#[test]
fn a_cancel_next_to_a_fill_of_the_merchants_own_ask_does_not_pay_a_kas_merchant() {
    let w = world(Merchant::Kas);
    let offer = w.offer();
    let p = w.pay(&offer);
    w.cancel_first(&p, payer_cancel(&p, &[], None));
    let payout = w.merchant_paying_output(&offer, KAS_AMOUNT + 2 * KAS);
    *w.hook.rewrite.lock().unwrap() = Some(composed(vec![payout], vec![]));
    let r = w.settle(&offer, &p);
    w.assert_intent_spent(&p, &r, "cancel + own ask fill");
}

/// The payer's cancel shares a transaction with `refund()` of the merchant's own refundable KobBid, which anyone may call. Its
/// refund pays the merchant's escrow at the bid's own input index, here after the intent and its lock. The transaction is a
/// real one the script engine accepts: the cancel constrains no output but the payer's.
#[test]
fn a_cancel_next_to_a_refund_of_the_merchants_own_bid_does_not_pay_a_kas_merchant() {
    let w = world(Merchant::Kas);
    let offer = w.offer();
    let p = w.pay(&offer);
    // the payer's own coin stands in for the bid's escrow: the output is the same bytes
    let coin = common::key_utxo(40, PAYER_K, 20 * KAS);
    put(&w.chain, &coin.utxo, p2pk_spk(&coin.pubkey));
    let refund = TxOutputJson { value: KAS_AMOUNT + KAS, script_public_key: spk_to_string(&p2pk_spk(&pk(MERCHANT))), covenant: None };
    let cancel = payer_cancel(&p, &[refund], Some(&coin));
    assert!(cancel.outputs.iter().any(|o| o.script_public_key == p2pk_spk(&pk(MERCHANT)) && o.value >= KAS_AMOUNT));
    w.cancel_first(&p, cancel);
    let r = w.settle(&offer, &p);
    w.assert_intent_spent(&p, &r, "cancel + own bid refund");
    // and as the follower reports the bid before the intent
    let w = world(Merchant::Kas);
    let p = w.pay(&offer);
    w.cancel_first(&p, payer_cancel(&p, &[], None));
    *w.hook.rewrite.lock().unwrap() = Some(composed(vec![w.merchant_paying_output(&offer, KAS_AMOUNT + KAS)], vec![]));
    let r = w.settle(&offer, &p);
    w.assert_intent_spent(&p, &r, "own bid refund + cancel");
}

/// A token merchant that also bids for token B: the payer sells exactly the offer's amount into the merchant's KobBid next to
/// its cancel. The delivery is byte-identical to the merchant's token output (the token script over the merchant's state, bound
/// to token B). It does not pay the intent wherever it lands, even at output j + 1 where an execution's delivery would be.
#[test]
fn a_cancel_next_to_a_delivery_into_the_merchants_own_bid_does_not_pay_a_token_merchant() {
    for (layout, before, at_pinned) in [("delivery before the intent", true, false), ("delivery at output j + 1", false, true)] {
        let w = world(Merchant::Token);
        let offer = w.offer();
        let p = w.pay(&offer);
        let cancel = payer_cancel(&p, &[], None);
        let delivery = w.merchant_paying_output(&offer, KAS);
        let rewrite: Rewrite = if before {
            composed(vec![delivery], vec![])
        } else {
            assert!(at_pinned);
            // the cancel of a KasToToken intent has the payer's KAS at output j = 0; output j + 1 = 1 is the delivery
            assert_eq!(cancel.outputs.len(), 1, "a KasToToken cancel pays only the payer");
            composed(vec![], vec![delivery])
        };
        w.cancel_first(&p, cancel);
        *w.hook.rewrite.lock().unwrap() = Some(rewrite);
        let r = w.settle(&offer, &p);
        w.assert_intent_spent(&p, &r, layout);
    }
}

/// A rival keeper's execution, planned against the same book, accepted right before the facilitator's own execution reaches
/// the node. `report` rewrites the follower's report of it.
fn rival_first(w: &World, p: &IntentPayment) -> Arc<Mutex<Option<kob_x402::intent::Execution>>> {
    *w.hook.creation.lock().unwrap() = Some(p.txid);
    let out = Arc::new(Mutex::new(None));
    let (chain, ledger, book, o, creation) = (w.chain.clone(), w.ledger.clone(), w.book.clone(), out.clone(), p.txid);
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
        let ex = build_execution(&f, &snapshot, &BTreeSet::new(), chain.daa(), &kp).unwrap().expect("the rival has an execution");
        chain.submit(&ex.tx).expect("rival submits");
        chain.mine(0);
        *o.lock().unwrap() = Some(ex);
    }));
    out
}

/// An honest execution by another keeper pays the merchant at the router-pinned output and settles, for a KAS merchant (output
/// j) and a token merchant (output j + 1).
#[test]
fn an_execution_by_another_keeper_still_settles() {
    for m in [Merchant::Kas, Merchant::Token] {
        let w = world(m);
        let offer = w.offer();
        let p = w.pay(&offer);
        let rival = rival_first(&w, &p);
        let r = w.settle(&offer, &p);
        assert!(r.success, "{m:?}: settled by the rival's execution: {r:?}");
        let ex = rival.lock().unwrap().clone().expect("the rival executed");
        assert_eq!(r.transaction, hex(&ex.tx.id().as_bytes()), "{m:?}");
        let rec = w.ledger.get(&hex(&p.txid)).unwrap().intent.unwrap();
        let paid = rec.executions.last().unwrap();
        assert_eq!((paid.txid.as_str(), paid.merchant_index), (r.transaction.as_str(), ex.merchant_output.index), "{m:?}");
        let held = w.chain.utxos_of(&w.merchant_spk(&offer)).unwrap();
        assert!(held.iter().any(|(o, _)| *o == ex.merchant_output), "{m:?}: the merchant holds the rival's output: {held:?}");
    }
}

/// The position is checked on its own: an execution reported one input later pins another output, which does not pay the
/// merchant, so the payment is not credited (a follower that misreports the input cannot make an output pay).
#[test]
fn an_execution_whose_merchant_output_is_not_at_the_pinned_position_is_not_credited() {
    let w = world(Merchant::Kas);
    let offer = w.offer();
    let p = w.pay(&offer);
    let _rival = rival_first(&w, &p);
    *w.hook.rewrite.lock().unwrap() = Some(Box::new(|s| match s {
        SpentBy::Spent { txid, block_daa_score, input_index, sig_script, outputs } => {
            SpentBy::Spent { txid, block_daa_score, input_index: input_index + 1, sig_script, outputs }
        }
        other => other,
    }));
    let r = w.settle(&offer, &p);
    w.assert_intent_spent(&p, &r, "execution at a shifted position");
}
