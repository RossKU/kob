//! Live: every node call of the executor over a public TN10 node (the Kaspa resolver, Borsh over TLS), through each of its
//! three clients: the indexer's (`rpc::WrpcClient`), the matcher's and the keepers' (`matcher::node::WrpcNode`) and the x402
//! facilitator's (`x402::node`). Read calls are checked for sense; the submissions send a transaction whose input does not
//! exist, which the node refuses (nothing is spent, no key is used): the refusal proves the call reached the node and came
//! back decoded.
//!
//!   cargo test -p kob-executor --test public_nodes -- --ignored --nocapture

use kob_executor::matcher::node::{NodeApi, WrpcConfig as NodeConfig, WrpcNode};
use kob_executor::rpc::{resolver, ChainSource, WrpcClient, WrpcConfig};
use kob_x402::chain::ChainView;

fn unspendable() -> kaspa_consensus_core::tx::Transaction {
    use kaspa_consensus_core::subnets::SubnetworkId;
    use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput};
    Transaction::new(
        1,
        vec![TransactionInput::new_with_compute_budget(
            TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([0x5a; 32]), 0),
            vec![0x51],
            0,
            1,
        )],
        vec![TransactionOutput::new(1_000_000, ScriptPublicKey::from_vec(0, vec![0x51]))],
        0,
        SubnetworkId::from([0; 20]),
        0,
        vec![],
    )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn every_call_over_a_public_testnet_node() {
    resolver::set_network("testnet-10");
    // the indexer's client
    let c = WrpcClient::new(WrpcConfig::new("resolver"));
    let info = c.server_info().await.expect("getServerInfo");
    assert_eq!(info.network_id, "testnet-10");
    let dag = c.dag_info().await.expect("getBlockDagInfo");
    let sink_blue = c.sink_blue_score().await.expect("getSinkBlueScore");
    assert!(c.block_exists(dag.sink).await.expect("getBlock"));
    let b = c.block_blue_score(dag.pruning_point_hash).await.expect("getBlock").expect("the pruning point is known");
    assert!(b < sink_blue);
    let hashes = c.chain_hashes(dag.pruning_point_hash).await.expect("getVirtualChainFromBlock");
    let ids = c.chain_with_ids(dag.pruning_point_hash, Some(sink_blue.saturating_sub(b).saturating_sub(50))).await.expect("ids");
    println!(
        "indexer client: node {}, {} synced={}, sink blue {sink_blue}, {} chain blocks from the pruning point, {} with ids",
        resolver::current("resolver").map(|u| resolver::host(&u)).unwrap_or_default(),
        info.server_version,
        info.is_synced,
        hashes.added_chain_block_hashes.len(),
        ids.added_chain_block_hashes.len()
    );
    // the matcher's client
    let n = WrpcNode::new(NodeConfig::new("resolver"));
    let s = n.server_info().await.expect("getServerInfo");
    assert!(s.has_utxo_index, "public nodes serve the UTXO index");
    let d = n.dag_info().await.expect("getBlockDagInfo");
    let fee = n.fee_estimate().await.expect("getFeeEstimate");
    let addr = "kaspatest:qz484dy4edqk52xx6srdd4a6dcwp7euxw86fl43ve2wm8umqts20kyujkrssm".to_string();
    let utxos = n.utxos_by_addresses(&[addr]).await.expect("getUtxosByAddresses");
    let up = n.chain_from(&d.sink).await.expect("getVirtualChainFromBlockV2 (Low)");
    let tx = kob_protocol::issue::rpc_transaction_json(&unspendable());
    let refused = n.submit(tx.clone()).await.expect_err("an input that does not exist");
    let refused_rbf = n.submit_replacement(tx).await.expect_err("nothing to replace");
    println!(
        "matcher client: fee estimate {fee:?}, {} UTXOs, {} new chain blocks; submit refused: {refused}; replacement refused: {refused_rbf}",
        utxos.len(),
        up.added.len()
    );
    // the x402 facilitator's client (blocking)
    let chain = tokio::task::spawn_blocking(|| {
        let c = kob_executor::x402::node::NodeChain::connect(
            "resolver",
            kob_x402::wire::Network::Testnet10,
            std::time::Duration::from_secs(20),
        );
        let daa = c.virtual_daa_score().expect("getBlockDagInfo");
        let pool = c.in_mempool(&[0x5a; 32]).expect("getMempoolEntry");
        let sub = c.submit(&unspendable()).expect_err("an input that does not exist");
        (daa, pool, format!("{sub:?}"))
    })
    .await
    .unwrap();
    assert!(!chain.1, "an unknown transaction is not in the mempool");
    println!("x402 client: virtual DAA {}, in mempool {}, submit refused: {}", chain.0, chain.1, chain.2);
}
