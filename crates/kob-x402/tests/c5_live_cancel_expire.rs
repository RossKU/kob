//! C5 liveness audit: the payer-side exits of a router intent (`build_cancel_intent`, `build_expire_intent`).
//!
//! 1. `build_cancel_intent` of a TOKEN intent with `lock: None` is accepted and builds a cancel that spends only
//!    the intent. The `cancel` entry has `emits none`, so the intent covenant ends there, and the locked tokens
//!    (owner scheme 0x04, owner = the intent's covenant id) can then never be spent again: the KCC-20 owner check is
//!    `OpCovInputCount(owner) > 0`, and no UTXO of that covenant id exists any more. `build_expire_intent` refuses the
//!    same input (`a token intent's expiry returns its locked tokens`); the cancel falls through its `_ => {}` arm.
//!    The wasm / TS handle (`IntentHandle.lock?: ... | null`) makes the omission easy.
//! 2. An intent whose own KAS is below the fee of its cancel / expiry can be neither cancelled nor expired:
//!    `CancelIntent` has no funding input, the fee is taken from the intent UTXO only, and nothing in the SDK or
//!    the verifier bounds `keeperValue` from below. (`docs/argent.md`: the expiry of a token intent costs about
//!    0.022-0.033 KAS.)
//! 3. The router's `expire` is permissionless, but it pins only output j (the payer's KAS, at most EXPIRE_MAX_FEE
//!    less) and the token output's SCRIPT; the VALUE of the token output (the lock carrier, default 1 KAS = ten
//!    times EXPIRE_MAX_FEE) is not constrained, so any bot may expire the intent and keep most of the carrier.
//!
//! Each test fails on current code and passes once the stated fix lands.
//! Run: cargo test -p kob-x402 --test c5_live_cancel_expire   (NOT run by the auditor: RAM)
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, TransactionOutput};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build_cancel_intent, build_expire_intent, intent_budgets, CancelIntent, ExpireIntent};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::SCHEME_P2PK;
use kob_protocol::tx::{finalize, FinalizeOptions, TokenUtxo, Utxo};
use kob_protocol::verify::{execute, validate};
use kob_x402::chain::{ChainUtxo, Outpoint};
use kob_x402::client::intent::{cancel_intent, intent_requirements, pay_intent, IntentOptions, IntentPayment};
use kob_x402::client::swap::{MerchantGain, PayAssetSpec, PayerFunds, SwapOfferParams};
use kob_x402::error::X402Error;
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::testkit::MockChain;
use kob_x402::wire::{hex, Finality, Network, PaymentRequirements};

use common::{keys, pk, EXT, KAS, NOW, TOKEN_COV, WHOLE};

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const RH: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";

fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
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
    policy: Policy,
    funds: PayerFunds,
}

/// The `intent.rs` fixture, reduced to what a payer needs to create an intent (no book).
fn fx() -> Fx {
    let chain = MockChain::new();
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.tokens.insert(AllowedToken::new(TOKEN_COV, P8, EXT, Custody::Unconditional, "AAA", 3)).unwrap();
    policy.limits.max_fee_sompi = KAS;
    let tok = common::tok_of(30, 9 * WHOLE, pk(PAYER), SCHEME_P2PK, 900, TOKEN_COV);
    put_tok(&chain, &tok);
    let funding = common::key_utxo(31, PAYER, 100 * KAS);
    put(&chain, &funding.utxo, p2pk_spk(&funding.pubkey));
    Fx { policy, funds: PayerFunds { tokens: vec![tok], funding: vec![funding], change: pk(PAYER) } }
}

impl Fx {
    fn kas_offer(&self, amount: u64, merchant: u8) -> PaymentRequirements {
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
    fn pay(&self, opts: &IntentOptions) -> Result<IntentPayment, X402Error> {
        let offer = self.kas_offer(5 * KAS, MERCHANT);
        pay_intent(&self.policy, &offer, &hex(&TOKEN_COV), RH, &keys(), &self.funds, NOW_MS, opts)
    }
}

fn t2k_opts() -> IntentOptions {
    IntentOptions { max_sell: Some(3 * WHOLE), ..IntentOptions::default() }
}

/// C5-4. A cancel of a token intent must carry the locked tokens or be refused by the builder.
// C5: expected to fail until `build_cancel_intent` rejects `(Some(locked token), None)` like `build_expire_intent` does.
#[test]
fn c5_a_cancel_of_a_token_intent_without_its_lock_is_refused_not_built() {
    let f = fx();
    let p = f.pay(&t2k_opts()).unwrap();
    assert!(p.lock.is_some(), "the SDK hands the payer the lock of a token intent");
    let r = CancelIntent {
        actor: p.actor.clone(),
        state: p.state.clone(),
        intent: p.intent.clone(),
        lock: None, // a handle that lost (or never stored) `lock`
        to: None,
        fee: Default::default(),
    };
    let built = build_cancel_intent(&r, &intent_budgets);
    assert!(
        built.is_err(),
        "C5-4: a cancel was built that spends the intent and leaves the {} locked tokens orphaned for ever",
        3 * WHOLE
    );
}

/// C5-5. The payer must be able to exit its intent whatever `keeperValue` it chose, or the SDK / verifier must refuse
/// a value that cannot pay for the exit. Either a creation with a 0.01 KAS intent is refused, or its cancel builds.
// C5: expected to fail until `prepare_intent` / `verify_intent` bound the intent's KAS from below (the cancel and expiry
// fee of the largest token actor), or `CancelIntent` takes funding inputs.
#[test]
fn c5_an_intent_that_cannot_pay_for_its_own_exit_is_refused_or_cancellable() {
    let f = fx();
    let opts = IntentOptions { keeper_value: 1_000_000, ..t2k_opts() }; // 0.01 KAS
    match f.pay(&opts) {
        Err(_) => {} // refused at creation: fine
        Ok(p) => {
            let c = cancel_intent(&p, None);
            assert!(c.is_ok(), "C5-5: the payer locked tokens in an intent it cannot cancel: {:?}", c.err());
            let x = build_expire_intent(
                &ExpireIntent {
                    actor: p.actor.clone(),
                    state: p.state.clone(),
                    intent: p.intent.clone(),
                    lock: p.lock.clone(),
                    fee: Default::default(),
                },
                &intent_budgets,
            );
            assert!(x.is_ok(), "C5-5: nor can anyone expire it: {:?}", x.err());
        }
    }
}

/// C5-6. An expiry that keeps the lock carrier must be rejected by the router: the payer's KAS back at out j (at most
/// EXPIRE_MAX_FEE less) and the tokens at out j + 1 are the only things the expirer owes, and it may keep only the fee.
// C5: expected to fail until the router's `expire` pins the token output's value
// (`tx.outputs[me + 1].value + EXPIRE_MAX_FEE >= tx.inputs[me + 1].value`, regenerate the router, new template pins).
#[test]
fn c5_an_expirer_cannot_keep_the_lock_carrier() {
    let f = fx();
    let p = f.pay(&t2k_opts()).unwrap();
    let r = ExpireIntent {
        actor: p.actor.clone(),
        state: p.state.clone(),
        intent: p.intent.clone(),
        lock: p.lock.clone(),
        fee: Default::default(),
    };
    let built = build_expire_intent(&r, &intent_budgets).unwrap();
    let s = finalize(&built, &[], FinalizeOptions { tighten_budgets: true }).unwrap();
    let (tx, entries) = s.tx.to_tx().unwrap();
    validate(&tx, &entries).expect("the honest expiry is valid");
    let carrier = tx.outputs[1].value;
    assert!(carrier >= kob_protocol::router::EXPIRE_MAX_FEE, "the carrier ({carrier}) is at least the documented fee cap");
    // a bot expires the intent: the payer gets its KAS and its tokens back, but the token output carries a fifth of
    // the carrier and the rest goes to the bot
    let mut bad = tx.clone();
    bad.outputs[1].value = carrier / 5;
    bad.outputs.push(TransactionOutput::new(carrier - carrier / 5, p2pk_spk(&pk(30))));
    let res = execute(&bad, &entries, true).unwrap();
    assert!(
        res[0].is_err(),
        "C5-6: the router accepted an expiry that moves {} sompi of the payer's lock carrier to a third party",
        carrier - carrier / 5
    );
}
