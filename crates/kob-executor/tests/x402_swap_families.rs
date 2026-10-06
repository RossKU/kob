//! The facilitator settles swap-and-pay payments across token families end to end on its in-memory
//! chain: a KRON pay asset, a KaspaCom-template token, KRON bids together with KCC-20 asks in one
//! transaction. The real verifier runs (`ProtocolVerifier`: order covenants and token programs in the
//! rusty-kaspa engine), the payment is broadcast, mined and observed, and the ledger consumes the
//! outpoints. The fixtures are the ones of `kob-x402/tests/swap_families.rs`.
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;
#[path = "../../kob-x402/tests/families_support/mod.rs"]
mod support;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig};
use kob_executor::x402::ledger::{Ledger, State};
use kob_x402::client::swap::{OrderRef, SwapPayment};
use kob_x402::wire::{hex, FacilitatorRequest, PaymentRequirements, SettlementResponse, X402_VERSION};

use common::{pk, WHOLE};
use support::*;

const MERCHANT_ID: &str = "shop";

fn facilitator(w: &World) -> Facilitator {
    Facilitator::new(
        w.policy.clone(),
        w.chain.clone(),
        w.clock.clone(),
        Arc::new(Ledger::in_memory()),
        FacilitatorConfig {
            settle_wait: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
            reorg_watch_daa: 10_000,
            kill_switch_file: None,
            ..Default::default()
        },
    )
}

fn request(offer: &PaymentRequirements, p: &SwapPayment) -> FacilitatorRequest {
    FacilitatorRequest {
        x402_version: X402_VERSION,
        payment_payload: p.payload.clone(),
        payment_requirements: offer.clone(),
        request_hash: Some(RH.into()),
        resource: None,
    }
}

/// Settles `p` through `fac`: a miner thread accepts the mempool once the broadcast reached the chain.
fn settle(w: &World, fac: &Facilitator, offer: &PaymentRequirements, p: &SwapPayment) -> SettlementResponse {
    let chain = w.chain.clone();
    let base = chain.submit_count();
    let miner = thread::spawn(move || {
        let t = std::time::Instant::now();
        while chain.submit_count() <= base && t.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(20));
        chain.mine(0);
    });
    let r = fac.settle(MERCHANT_ID, &request(offer, p));
    miner.join().unwrap();
    r
}

/// Verify (read-only), settle, and check the response and the ledger.
fn assert_settles(w: &World, offer: &PaymentRequirements, p: &SwapPayment, pay_asset: &str, amount: &str) {
    let fac = facilitator(w);
    let v = fac.verify(&request(offer, p));
    assert!(v.is_valid, "verify: {v:?}");
    assert_eq!(w.chain.submit_count(), 0, "verify never broadcasts");
    let r = settle(w, &fac, offer, p);
    assert!(r.success, "{r:?}");
    assert_eq!(r.transaction, hex(&p.txid));
    assert_eq!(r.amount.as_deref(), Some(amount));
    let kaspa = &r.extensions.as_ref().unwrap()["kaspa"];
    assert_eq!(kaspa["route"]["payAsset"], pay_asset);
    assert_eq!(kaspa["route"]["binding"], "kob-swap-v1");
    let e = fac.ledger.get(&hex(&p.txid)).unwrap();
    assert_eq!(e.state, State::Accepted);
    assert_eq!(e.merchant, MERCHANT_ID);
    assert!(!e.order_inputs.is_empty(), "the orders the payment consumed are recorded");
    // the payer's inputs and the orders are consumed: a replay of the same payment is refused
    assert!(w.chain.is_accepted(&p.txid));
    for o in &p.tx.inputs {
        assert!(fac
            .ledger
            .is_consumed(&kob_x402::chain::Outpoint::new(o.previous_outpoint.transaction_id.as_bytes(), o.previous_outpoint.index)));
    }
    assert_eq!(fac.metrics.settle_success.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn kron_pay_asset_settles_for_a_kas_merchant() {
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(common::KAS, &[K]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], true));
    assert_settles(&w, &offer, &p, &hex(&K.cov), &common::KAS.to_string());
}

#[test]
fn kron_pay_asset_settles_for_a_kcc20_merchant_token_in_one_cross_family_transaction() {
    let w = World::new(&[K, B3]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(B3, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(K, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(B3, 2 * WHOLE as u64, &[K]);
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0, ask.1, 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    assert_settles(&w, &offer, &p, &hex(&K.cov), &(2 * WHOLE).to_string());
}

#[test]
fn kcc20_pay_asset_settles_for_a_kaspacom_template_merchant_token() {
    let w = World::new(&[A8, KC]);
    let bid = w.put_bid(A8, 20, 0xb1, common::MAKER_B, 5);
    let ask = w.put_ask(KC, 10, 11, 0x5a, common::MAKER_C);
    let held = w.put_holding(A8, 21, 3 * WHOLE, pk(PAYER));
    let offer = w.token_offer(KC, 2 * WHOLE as u64, &[A8]);
    let q = quote(vec![OrderRef::bid(bid, 3 * WHOLE), OrderRef::ask(ask.0, ask.1, 2 * WHOLE)]);
    let p = w.pay(&offer, &q, &w.funds(vec![held], true));
    assert_settles(&w, &offer, &p, &hex(&A8.cov), &(2 * WHOLE).to_string());
}

#[test]
fn kaspacom_template_pay_asset_settles_for_a_kas_merchant() {
    let w = World::new(&[KC]);
    let bid = w.put_bid(KC, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(KC, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(common::KAS, &[KC]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid, 3 * WHOLE)]), &w.funds(vec![held], false));
    assert_settles(&w, &offer, &p, &hex(&KC.cov), &common::KAS.to_string());
}

#[test]
fn a_kron_order_taken_by_someone_else_is_a_retryable_conflict() {
    let w = World::new(&[K]);
    let bid = w.put_bid(K, 20, 0xb1, common::MAKER_B, 5);
    let held = w.put_holding(K, 21, 5 * WHOLE, pk(PAYER));
    let offer = w.kas_offer(common::KAS, &[K]);
    let p = w.pay(&offer, &quote(vec![OrderRef::bid(bid.clone(), 3 * WHOLE)]), &w.funds(vec![held], true));
    // another taker fills the KRON bid first
    w.chain.spend_externally(&op(&bid.utxo));
    let fac = facilitator(&w);
    let v = fac.verify(&request(&offer, &p));
    assert!(!v.is_valid);
    let ext = v.extensions.unwrap();
    assert_eq!(ext["kaspa"]["diagnostic"], "order_conflict");
    assert_eq!(ext["kaspa"]["retryable"], true);
    assert_eq!(ext["kaspa"]["details"]["orders"][0]["txid"], hex(&bid.utxo.transaction_id));
}

#[test]
fn supported_lists_both_families_and_offers_the_kcc20_profile_only_for_kcc20_tokens() {
    // a KRON-only allowlist: swap pay assets, but no kcc20 merchant profile
    let w = World::new(&[K]);
    let s = facilitator(&w).supported();
    let extra = &s["kinds"][0]["extra"];
    assert_eq!(extra["profiles"], serde_json::json!(["standard-native"]));
    assert_eq!(extra["routeBindings"], serde_json::json!(["kob-swap-v1"]));
    assert_eq!(extra["tokens"][0]["family"], "kron");
    // a KaspaCom-template token is an ordinary KCC-20 entry
    let w = World::new(&[K, KC]);
    let s = facilitator(&w).supported();
    let extra = &s["kinds"][0]["extra"];
    assert_eq!(extra["profiles"], serde_json::json!(["standard-native", "kcc20"]));
    let toks = extra["tokens"].as_array().unwrap();
    let kc = toks.iter().find(|t| t["asset"] == hex(&KC.cov)).unwrap();
    assert_eq!(kc["family"], "kcc20");
    assert_eq!(kc["templateHash"], hex(&kob_protocol::artifacts::token_template(KC.prog).hash));
}
