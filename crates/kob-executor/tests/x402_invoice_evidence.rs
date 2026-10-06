//! Refused invoice payments kept as evidence are bounded (security review 2026-10-06).
//!
//! `POST /invoices/<id>/pay` is unauthenticated. A payment to an expired (or paid) invoice is refused, verified and kept as
//! evidence with the merchant output it would pay, so the reconcile can report it if the payer broadcasts it anyway. Before
//! the fix every refused payment with a new txid was kept and the WHOLE record rewritten (with an fsync) each time: one
//! funded output gave endless fee variants, so the store grew quadratically and the reconcile watch list without bound.
//! Now a refused payment is kept once per funding (the same spent outputs) (fee variants of one funding conflict: at most one can confirm) and at
//! most `maxExtraPaymentsPerInvoice` per invoice; nothing more is written.

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use kaspa_addresses::{Address, Prefix, Version};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig, InvoiceRuntime, InvoiceStore};
use kob_executor::x402::ledger::Ledger;
use kob_protocol::script::p2pk_spk;
use kob_x402::chain::{ChainUtxo, FixedClock, Outpoint};
use kob_x402::client::native::{native_requirements, pay_native, PayOptions};
use kob_x402::invoice::Invoice;
use kob_x402::policy::Policy;
use kob_x402::testkit::MockChain;
use kob_x402::wire::{Finality, Network};

use common::{pk, sk, KAS, NOW};

const NOW_MS: u64 = 1_800_000_000_000;
const PAYER: u8 = common::TAKER;
const MERCHANT: u8 = common::MERCHANT;
const MAX_EXTRA: usize = 4;

fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
}

#[test]
fn refused_invoice_payments_are_kept_once_per_funding_and_capped() {
    let chain = Arc::new(MockChain::new());
    chain.advance_daa(NOW);
    let mut policy = Policy::new(Network::Testnet10);
    policy.limits.max_fee_sompi = KAS;
    // funded payer outputs, never spent
    let fundings: Vec<_> = (0..6u8).map(|i| common::key_utxo(31 + i, PAYER, 10 * KAS)).collect();
    for f in &fundings {
        chain.insert_utxo(
            Outpoint::new(f.utxo.transaction_id, f.utxo.index),
            ChainUtxo {
                amount: f.utxo.amount,
                script_public_key: p2pk_spk(&f.pubkey),
                block_daa_score: f.utxo.block_daa_score,
                is_coinbase: false,
                covenant_id: None,
            },
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("invoices.jsonl");
    let clock = Arc::new(FixedClock::new(NOW_MS));
    let fac = Facilitator::new(
        policy,
        chain.clone(),
        clock.clone(),
        Arc::new(Ledger::in_memory()),
        FacilitatorConfig { settle_wait: Duration::from_secs(1), poll_interval: Duration::from_millis(5), ..Default::default() },
    )
    .with_invoices(InvoiceRuntime {
        store: Arc::new(InvoiceStore::open(&store_path).unwrap()),
        max_lifetime_ms: 86_400_000,
        public_url: None,
        max_open_per_merchant: 100,
        max_extra_payments: MAX_EXTRA,
    });

    // the merchant registers a small invoice that expires after 60 s
    let offer = native_requirements(Network::Testnet10, KAS / 10, &addr(MERCHANT), 600, Finality::Accepted).unwrap();
    let inv = Invoice::new(Network::Testnet10, "coffee-1", NOW_MS + 60_000, None, vec![offer.clone()]);
    let id = fac.register_invoice("shop", inv, &|_| true).unwrap()["id"].as_str().unwrap().to_string();
    clock.set(NOW_MS + 61_000); // expired
    let pay = |k: usize, bump: u64| {
        let mut opts = PayOptions { ttl_seconds: Some(300), ..PayOptions::new(KAS / 10) };
        opts.fee_rate += bump;
        let p = pay_native(&offer, &id, &sk(PAYER), std::slice::from_ref(&fundings[k]), NOW_MS + 61_000, &opts).unwrap();
        let r = fac.pay_invoice(&id, p);
        assert!(!r.success, "a late payment is refused");
    };

    // the review's attack: 200 fee variants of ONE funded output, each a new txid
    pay(0, 0);
    let after_first = std::fs::metadata(&store_path).unwrap().len();
    for i in 1..200u64 {
        pay(0, i);
    }
    let st = fac.invoice_status(&id).unwrap();
    assert_eq!(st.extra_payments.len(), 1, "fee variants of one funding are one piece of evidence");
    assert_eq!(std::fs::metadata(&store_path).unwrap().len(), after_first, "a conflicting variant writes nothing");

    // distinct fundings: kept up to the cap, then rejected without a write
    for k in 1..fundings.len() {
        pay(k, 0);
    }
    let st = fac.invoice_status(&id).unwrap();
    assert_eq!(st.extra_payments.len(), MAX_EXTRA, "at most maxExtraPaymentsPerInvoice refused payments are kept");
    let full = std::fs::metadata(&store_path).unwrap().len();
    for i in 0..50u64 {
        pay(5, 1_000 + i);
    }
    assert_eq!(std::fs::metadata(&store_path).unwrap().len(), full, "a full invoice writes nothing more");
    // the kept evidence survives a restart
    drop(fac);
    let store = InvoiceStore::open(&store_path).unwrap();
    assert_eq!(store.get(&id).unwrap().extra.len(), MAX_EXTRA);
}
