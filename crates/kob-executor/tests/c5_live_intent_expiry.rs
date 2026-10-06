//! C5 liveness audit: "every state can be exited" for facilitator-held router intents (x402 `kob-intent-v1`).
//!
//! The facilitator promises (docs/spec/x402-swap-and-pay.md 17.7, "retrying until the expiry is accepted or the
//! intent is spent otherwise") that an intent which ends without a payment is expired ON CHAIN by it, so the
//! payer's KAS and locked tokens come back without the payer doing anything. Three ways the code leaves an
//! on-chain intent behind with nobody scheduled to exit it (the payer's own `cancel` still works, but only if it
//! kept its intent handle):
//!
//! 1. `drive_intent` gives up after `maxAttempts` executions (`facilitator/intent.rs`, "intent_not_executable:
//!    N executions failed") WITHOUT creating an `ExpiryRecord`; the entry is `failed`, `drive_intent` returns
//!    `Dead` before the deadline branch that creates the record, and `reconcile` only drives failed entries that
//!    have one. The intent stays on chain for ever (and stays executable by any keeper).
//! 2. The expiry window is anchored at the intent's deadline (`until_ms = deadline + 1 h`), not at the moment the
//!    facilitator first looks at the intent: after an outage longer than an hour past the deadline the record is
//!    created already out of date and `drive_expiry` answers `abandoned` WITHOUT ever submitting the expiry.
//! 3. A creation that was seen and then vanishes from the UTXO set (a reorg that re-queues it) is reported as
//!    `intent_spent` and the entry fails at once; the mempool is only consulted for creations never seen. When the
//!    creation is mined again, the intent exists, nobody executes it and nobody expires it.
//!
//! Every test below demonstrates the bug: it fails on current code and passes once the facilitator exits the state.
//! Run: cargo test -p kob-executor --test c5_live_intent_expiry   (NOT run by the auditor: RAM)
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

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
use kob_protocol::tx::{OrderUtxo, TokenUtxo, Utxo};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::client::intent::{intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::intent::{BookView, KeeperParams};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::testkit::MockChain;
use kob_x402::wire::{
    hex, FacilitatorRequest, Finality, Network, PaymentPayload, PaymentRequirements, SettlementResponse, X402_VERSION,
};

use common::{keys, pk, CARRIER, DC, EXT, KAS, NOW, P245, P250, TOKEN_B, TOKEN_COV, WHOLE};

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const KEEPER: u8 = common::KEEPER;
const SHOP: &str = "shop";
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
/// The intent's own deadline: the authorization lifetime `pay_intent` uses by default (5 minutes).
const LIFETIME_MS: u64 = 300_000;
/// `EXPIRY_WINDOW_MS` of facilitator/intent.rs (private there): how long past the deadline the facilitator retries.
const EXPIRY_WINDOW_MS: u64 = 3_600_000;

fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
}

// ------------------------------------------------------------------------------------------ chain with a rival

/// The mock chain, plus a rival taker (the first submitted transaction that spends the armed order is preceded by
/// a rival fill of it: the submit conflicts), plus `requeued`: after a reorg the node re-queues every transaction,
/// so `in_mempool` is true and every unspent-looking output reads as `Mempool`.
struct RivalChain {
    inner: Arc<MockChain>,
    rival: Mutex<Option<Outpoint>>,
    requeued: AtomicBool,
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
        match self.inner.output_status(out, spk)? {
            OutputStatus::Unknown if self.requeued.load(Ordering::SeqCst) => Ok(OutputStatus::Mempool),
            s => Ok(s),
        }
    }
    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError> {
        Ok(self.requeued.load(Ordering::SeqCst) || self.inner.in_mempool(txid)?)
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
                *r = None;
            }
        }
        drop(r);
        self.inner.submit(tx)
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

/// The `x402_intents.rs` world with one difference: `max_attempts` is a parameter. The book holds ONE bid (the one
/// a rival takes as soon as the keeper's first execution names it), so nothing can execute the intent.
fn world(max_attempts: usize) -> World {
    let chain = Arc::new(MockChain::new());
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.tokens.insert(allowed(TOKEN_COV, "AAA")).unwrap();
    policy.tokens.insert(allowed(TOKEN_B, "BBB")).unwrap();
    policy.limits.max_fee_sompi = KAS;
    let mut book = BookView::default();
    {
        let b = bid_a(20, common::MAKER_B, P250, 10);
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
    let view = Arc::new(RivalChain { inner: chain.clone(), rival: Mutex::new(None), requeued: AtomicBool::new(false) });
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
        max_attempts,
        lock_margin_daa: 0,
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
    fn kas_intent_offer(&self, amount: u64, merchant: u8) -> PaymentRequirements {
        intent_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(merchant),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Kas,
            pay_assets: vec![PayAssetSpec::Token(self.policy.tokens.find(&TOKEN_COV).unwrap())],
        })
        .unwrap()
    }
    fn pay_t2k(&self, offer: &PaymentRequirements, rh: &str, max_sell: i64) -> IntentPayment {
        let opts = IntentOptions { max_sell: Some(max_sell), ..IntentOptions::default() };
        let unspent = |u: &Utxo| self.chain.utxo(&Outpoint::new(u.transaction_id, u.index)).is_some();
        let funds =
            PayerFunds { funding: self.funds.funding.iter().filter(|f| unspent(&f.utxo)).cloned().collect(), ..self.funds.clone() };
        pay_intent(&self.policy, offer, &hex(&TOKEN_COV), rh, &keys(), &funds, NOW_MS, &opts).unwrap()
    }
    /// A rival takes the only bid when the keeper's first execution names it: the intent cannot be executed.
    fn arm_rival(&self) {
        let o = self.book.lock().unwrap().bids[0].utxo.clone();
        *self.view.rival.lock().unwrap() = Some(Outpoint::new(o.transaction_id, o.index));
    }
    fn intent_is_spent(&self, p: &IntentPayment) -> bool {
        self.chain.utxo(&Outpoint::new(p.intent.transaction_id, p.intent.index)).is_none()
    }
    fn tokens_are_back(&self) -> bool {
        let spk = kob_protocol::state::TokenState::user(kob_protocol::Family::Kcc20, 3 * WHOLE, pk(PAYER), EXT)
            .spk_with(kob_protocol::artifacts::token_template(P8));
        self.chain.unspent_of(&spk).len() == 1
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

/// C5-1. The payer is told `intent_not_executable` (attempts exhausted) while its KAS and tokens sit in an intent on
/// chain. The deadline passes, the facilitator is running, and nobody exits the intent.
// C5: expected to fail until the max-attempts give-up (and every other `give_up` of an intent that may be on chain)
// records an ExpiryRecord, so the reconcile loop expires the intent from its deadline on.
#[test]
fn c5_an_intent_given_up_after_max_attempts_is_still_expired_on_chain() {
    let w = world(1);
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    w.arm_rival();
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    w.fac.set_settle_wait(Duration::from_millis(500));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert!(!r.success);
    assert_eq!(diag(&r), "intent_not_executable", "{r:?}");
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(!w.intent_is_spent(&p), "the intent is on chain, holding the payer's KAS and tokens");
    // the deadline passes; the facilitator keeps reconciling
    w.clock.set(NOW_MS + LIFETIME_MS);
    for _ in 0..3 {
        w.fac.reconcile();
        w.chain.mine(1);
    }
    assert!(w.intent_is_spent(&p), "C5-1: the intent is still on chain after its deadline; nobody will ever expire it");
    assert!(w.tokens_are_back(), "C5-1: the locked tokens are the payer's again");
}

/// C5-2. The facilitator was down (or its reconcile loop stalled) for more than `EXPIRY_WINDOW_MS` past the deadline.
/// On the first reconcile the expiry record is created already expired and the outcome is `abandoned` without a
/// single attempt: every intent that ended during the outage is left on chain.
// C5: expected to fail until the retry window starts at the first reconcile that sees the intent (or is dropped), not at
// the deadline.
#[test]
fn c5_an_outage_longer_than_the_expiry_window_does_not_abandon_the_intent() {
    let w = world(10);
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    w.arm_rival();
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    w.fac.set_settle_wait(Duration::from_millis(500));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert_eq!(diag(&r), "settlement_pending", "{r:?}");
    assert!(!w.intent_is_spent(&p));
    // the facilitator comes back two hours after the deadline
    w.clock.set(NOW_MS + LIFETIME_MS + 2 * EXPIRY_WINDOW_MS);
    for _ in 0..3 {
        w.fac.reconcile();
        w.chain.mine(1);
    }
    let x = w.ledger.get(&hex(&p.txid)).unwrap().intent.unwrap().expiry.expect("an expiry record");
    assert_ne!(x.outcome.as_deref(), Some("abandoned"), "C5-2: the record says abandoned although no expiry was ever submitted");
    assert!(w.intent_is_spent(&p), "C5-2: the intent is still on chain");
    assert!(w.tokens_are_back());
}

/// C5-3. The creation is reorged out and re-queued by the node. The facilitator already saw it (`seen_daa`), so a
/// missing intent UTXO is reported as "spent outside this facilitator" and the entry fails; the mempool is not asked.
/// The creation is mined again: the intent exists with no one executing or expiring it.
// C5: expected to fail until a missing intent whose creation is in the mempool (or whose creation output reads as
// `Mempool`) keeps the entry waiting, exactly as for a creation that was never seen.
#[test]
fn c5_a_reorged_creation_that_is_requeued_does_not_fail_the_intent() {
    let w = world(10);
    let offer = w.kas_intent_offer(5 * KAS, MERCHANT);
    w.arm_rival();
    let p = w.pay_t2k(&offer, RH, 3 * WHOLE);
    w.fac.set_settle_wait(Duration::from_millis(500));
    let r = with_miner(&w, || w.fac.settle(SHOP, &request(&offer, &p.payload)));
    assert_eq!(diag(&r), "settlement_pending", "{r:?}");
    let seen = w.ledger.get(&hex(&p.txid)).unwrap().intent.unwrap().seen_daa;
    assert!(seen.is_some(), "the facilitator saw the intent on chain");
    // reorg: the block with the creation is dropped, the node re-queues the creation transaction
    w.chain.spend_externally(&Outpoint::new(p.intent.transaction_id, p.intent.index));
    w.view.requeued.store(true, Ordering::SeqCst);
    w.fac.reconcile();
    let e = w.ledger.get(&hex(&p.txid)).unwrap();
    assert_ne!(e.state, State::Failed, "C5-3: the entry failed ({:?}) although the creation is in the mempool again", e.reason);
}
