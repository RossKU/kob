//! Regression: `/verify` does not call a payment valid that the node cannot accept.
//!
//! A payer input that is a coinbase output younger than the maturity is refused by consensus; `/verify` used to say valid.

use std::sync::Arc;
use std::time::Duration;

use kaspa_addresses::{Address, Version};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig};
use kob_executor::x402::ledger::Ledger;
use kob_protocol::tx::{KeyUtxo, Utxo};
use kob_x402::canonical::http_request_hash;
use kob_x402::chain::{ChainUtxo, FixedClock};
use kob_x402::client::native::{native_requirements, pay_native, PayOptions};
use kob_x402::common::requirements_hash_hex;
use kob_x402::policy::Policy;
use kob_x402::testkit::{pubkey, secret, MockChain};
use kob_x402::wire::{hex, FacilitatorRequest, Finality, Network};

const PAYER: u8 = 7;
const MERCHANT: u8 = 8;
const NOW_MS: u64 = 1_800_000_000_000;

fn request(chain: &MockChain, coinbase_daa: Option<u64>) -> FacilitatorRequest {
    let op = chain.add_p2pk(&pubkey(PAYER), 500_000_000);
    let mut u = chain.utxo(&op).unwrap();
    if let Some(d) = coinbase_daa {
        u.is_coinbase = true;
        u.block_daa_score = d;
        chain.insert_utxo(op, ChainUtxo { ..u.clone() });
    }
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
    let addr = Address::new(Network::Testnet10.prefix(), Version::PubKey, &pubkey(MERCHANT)).to_string();
    let offer = native_requirements(Network::Testnet10, 20_000_000, &addr, 60, Finality::Accepted).unwrap();
    let rh = requirements_hash_hex(&offer).unwrap();
    let request_hash = hex(&http_request_hash("GET", "https://api.example.test/x", None, &rh).unwrap());
    let payload = pay_native(&offer, &request_hash, &secret(PAYER), &[coin], NOW_MS, &PayOptions::new(u64::MAX)).unwrap();
    FacilitatorRequest {
        x402_version: 2,
        payment_payload: payload,
        payment_requirements: offer,
        request_hash: Some(request_hash),
        resource: None,
    }
}

fn facilitator(chain: Arc<MockChain>, ledger: Arc<Ledger>, clock: Arc<FixedClock>, grace: Duration) -> Facilitator {
    Facilitator::new(
        Policy::new(Network::Testnet10),
        chain,
        clock,
        ledger,
        FacilitatorConfig {
            settle_wait: Duration::from_secs(1),
            poll_interval: Duration::from_millis(5),
            pending_grace: grace,
            ..Default::default()
        },
    )
}

#[test]
fn an_immature_coinbase_input_does_not_verify() {
    let chain = Arc::new(MockChain::new());
    let fac = facilitator(chain.clone(), Arc::new(Ledger::in_memory()), Arc::new(FixedClock::new(NOW_MS)), Duration::from_secs(900));
    // created in the current chain block: consensus maturity is not reached
    let req = request(&chain, Some(chain.daa()));
    let v = fac.verify(&req);
    assert!(!v.is_valid, "{v:?}");
    let why = format!("{:?}", v.extensions);
    assert!(why.contains("maturity"), "{why}");
}

#[test]
fn a_mature_coinbase_input_verifies() {
    let chain = Arc::new(MockChain::new());
    let fac = facilitator(chain.clone(), Arc::new(Ledger::in_memory()), Arc::new(FixedClock::new(NOW_MS)), Duration::from_secs(900));
    let daa = chain.daa();
    let req = request(&chain, Some(daa.saturating_sub(10_000)));
    let v = fac.verify(&req);
    assert!(v.is_valid, "{v:?}");
}
