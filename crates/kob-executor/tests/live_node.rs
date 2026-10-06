//! Shape checks of `NodeChain` against a live read-only testnet-10 node (`KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`).
//! Read-only: the only write attempted is a `submitTransaction` of a transaction whose inputs do not
//! exist, which the node refuses. Skipped when `KOB_SKIP_NETWORK_TESTS=1`.

use std::time::Duration;

use kaspa_addresses::Address;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutpoint, TransactionOutput};
use kaspa_consensus_core::Hash;
use kaspa_txscript::pay_to_address_script;
use kob_executor::x402::node::NodeChain;
use kob_x402::chain::{ChainView, Outpoint, OutputStatus, SubmitError};
use kob_x402::testkit::{p2pk_spk, pubkey};
use kob_x402::wire::Network;

fn node_url() -> String {
    std::env::var("KOB_TN10_WRPC").unwrap_or_else(|_| "ws://127.0.0.1:18210".into())
}

fn skip() -> bool {
    if std::env::var("KOB_SKIP_NETWORK_TESTS").as_deref() == Ok("1") {
        eprintln!("KOB_SKIP_NETWORK_TESTS=1: skipping the live node test");
        return true;
    }
    false
}

fn chain() -> NodeChain {
    NodeChain::connect(&node_url(), Network::Testnet10, Duration::from_secs(20))
}

#[test]
fn live_server_info_and_virtual_daa_score() {
    if skip() {
        return;
    }
    let c = chain();
    c.check_network().expect("the node is a synced TN10 node with a UTXO index");
    let daa = c.virtual_daa_score().unwrap();
    assert!(daa > 500_000_000, "TN10 virtual DAA score {daa}");
    // the score moves forward
    std::thread::sleep(Duration::from_secs(2));
    assert!(c.virtual_daa_score().unwrap() >= daa);
}

#[test]
fn live_utxo_lookups() {
    if skip() {
        return;
    }
    let c = chain();
    // an unused address: no UTXOs, nothing accepted, nothing in the mempool
    let empty = p2pk_spk(&pubkey(200));
    assert!(c.utxos_of(&empty).unwrap().is_empty());
    let op = Outpoint::new([0x42; 32], 0);
    assert_eq!(c.utxos(&[(op, empty.clone())]).unwrap(), vec![None]);
    assert_eq!(c.output_status(&op, &empty).unwrap(), OutputStatus::Unknown);
    assert!(!c.in_mempool(&[0x42; 32]).unwrap());

    // a mining address of the network (shape check of a populated reply; skipped softly if it was emptied)
    let addr = Address::try_from("kaspatest:qz3gmz7u9442zx3v9kxxfeeyjv2u0yq83yl3a7eccqsky4aemvtxxj55nmfjh").unwrap();
    let spk = pay_to_address_script(&addr);
    let utxos = c.utxos_of(&spk).unwrap();
    let Some((op, u)) = utxos.first().cloned() else {
        eprintln!("the sample address has no UTXOs any more; populated-reply checks skipped");
        return;
    };
    assert_eq!(u.script_public_key, spk);
    assert!(u.amount > 0 && u.block_daa_score > 0);
    let found = c.utxos(&[(op, spk.clone())]).unwrap();
    assert_eq!(found[0].as_ref().unwrap(), &u);
    // the same outpoint claimed under a different script is not the entry
    assert_eq!(c.utxos(&[(op, empty.clone())]).unwrap(), vec![None]);
    match c.output_status(&op, &spk).unwrap() {
        OutputStatus::Accepted { block_daa_score } => assert_eq!(block_daa_score, u.block_daa_score),
        other => eprintln!("output status changed while testing: {other:?}"), // spent between the two calls
    }
}

#[test]
fn live_submit_of_a_transaction_with_unknown_inputs_is_a_conflict_not_a_success() {
    if skip() {
        return;
    }
    let c = chain();
    let spk = p2pk_spk(&pubkey(200));
    let tx = Transaction::new(
        0,
        vec![TransactionInput::new(TransactionOutpoint::new(Hash::from_bytes([0x11; 32]), 0), vec![0x41; 66], 0, 1)],
        vec![TransactionOutput::new(100_000_000, spk)],
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    match c.submit(&tx) {
        Err(SubmitError::Conflict(m)) => assert!(m.to_lowercase().contains("orphan"), "{m}"),
        other => panic!("expected a conflict (orphan) from the node, got {other:?}"),
    }
}
