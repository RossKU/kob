//! Read-only checks of the node wire format against a live testnet-10 node (ignored by default):
//! `KOB_TN10_WRPC=ws://HOST:18210 cargo test -p kob-executor --test tn10_node -- --ignored`.

use kob_executor::matcher::node::{NodeApi, WrpcConfig, WrpcNode};

fn url() -> String {
    std::env::var("KOB_TN10_WRPC").unwrap_or_else(|_| "ws://127.0.0.1:18210".into())
}

#[tokio::test]
#[ignore = "needs a live testnet-10 node"]
async fn tn10_wire_format() {
    let node = WrpcNode::new(WrpcConfig::new(url()));
    let info = node.server_info().await.expect("getServerInfo");
    println!(
        "server {} network {} synced {} utxoindex {} daa {}",
        info.server_version, info.network_id, info.is_synced, info.has_utxo_index, info.virtual_daa_score
    );
    assert!(info.network_id.contains("testnet"));
    let dag = node.dag_info().await.expect("getBlockDagInfo");
    println!("sink {} daa {}", dag.sink, dag.virtual_daa_score);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let u = node.chain_from(&dag.sink).await.expect("getVirtualChainFromBlockV2");
    let txs: usize = u.added.iter().map(|b| b.accepted.len()).sum();
    println!("chain: {} removed, {} added, {} accepted transactions", u.removed.len(), u.added.len(), txs);
    assert!(!u.added.is_empty(), "the chain advances in 3 s");
    assert!(u.added.iter().all(|b| b.daa_score > 0));
    let addr = kob_executor::matcher::wallet::address_of(&[0x11; 32], &info.network_id).to_string();
    if info.has_utxo_index {
        let utxos = node.utxos_by_addresses(&[addr]).await.expect("getUtxosByAddresses");
        println!("utxos of a fresh address: {}", utxos.len());
    }
    // A malformed submission is rejected with a message, never accepted.
    let err = node.submit(serde_json::json!({"version": 1})).await.unwrap_err();
    println!("bad submit: {err}");
}
