//! Read-only check of several nodes against live testnet-10 nodes: a window fetched from another node (`Full`, over `wss:`)
//! verifies against the primary's acceptance data with real headers and real transactions (header hashes, transaction ids
//! of v0 and v1 transactions, covenant bindings), and a tampered copy does not.
//!
//! Environment:
//! * `KOB_TN10_WRPC`               the primary, default `ws://127.0.0.1:18210` (a testnet-10 node of your own)
//! * `KOB_TN10_WRPC_OTHER`         the other node, default a public TN10 node of the Kaspa resolver (`wss:`)
//! * `KOB_SKIP_NETWORK_TESTS=1`    skip (offline runs)
//! * `KOB_REQUIRE_NETWORK_TESTS=1` fail instead of skip when a node is unreachable

use kob_executor::hex::Hash32;
use kob_executor::rpc::multi::{MultiNode, NodeSpec};
use kob_executor::rpc::types::Verbosity;
use kob_executor::rpc::verify::{verify_window, Rejection};
use kob_executor::rpc::{fetch_window_from, window_min_confirmations, ChainSource, Origin, WindowRequest, WrpcClient, WrpcConfig};
use std::sync::Arc;
use std::time::Duration;

const PRIMARY: &str = "ws://127.0.0.1:18210";
const OTHER: &str = "wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/json";

fn client(url: &str) -> Arc<WrpcClient> {
    let mut c = WrpcConfig::new(url.to_string());
    c.connect_timeout = Duration::from_secs(10);
    c.request_timeout = Duration::from_secs(120);
    c.vspc_connections = 1;
    Arc::new(WrpcClient::new(c))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_from_another_tn10_node_verifies_against_the_primary() {
    if std::env::var("KOB_SKIP_NETWORK_TESTS").is_ok() {
        eprintln!("skipped: KOB_SKIP_NETWORK_TESTS is set");
        return;
    }
    let p_url = std::env::var("KOB_TN10_WRPC").unwrap_or_else(|_| PRIMARY.to_string());
    let o_url = std::env::var("KOB_TN10_WRPC_OTHER").unwrap_or_else(|_| OTHER.to_string());
    let (primary, other) = (client(&p_url), client(&o_url));
    for (url, c) in [(&p_url, &primary), (&o_url, &other)] {
        match c.server_info().await {
            Ok(i) => {
                eprintln!("{url}: {} {} synced {}", i.server_version, i.network_id, i.is_synced);
                assert_eq!(i.network_id, "testnet-10");
            }
            Err(e) if std::env::var("KOB_REQUIRE_NETWORK_TESTS").is_err() => {
                eprintln!("skipped: {url} unreachable ({e})");
                return;
            }
            Err(e) => panic!("{url} unreachable: {e}"),
        }
    }
    // a window of 30 chain blocks starting 60 chain blocks behind the primary's sink
    let mut start = primary.dag_info().await.unwrap().sink;
    let mut chain = vec![];
    for _ in 0..60 {
        let raw =
            primary.call_raw("getBlock", serde_json::json!({ "hash": start.to_hex(), "includeTransactions": false })).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        start = Hash32::parse(v["block"]["verboseData"]["selectedParentHash"].as_str().unwrap()).unwrap();
    }
    let hashes = primary.chain_hashes(start).await.unwrap();
    assert!(hashes.removed_chain_block_hashes.is_empty());
    chain.extend(hashes.added_chain_block_hashes.iter().take(30));
    let end = *chain.last().unwrap();
    let w = WindowRequest::new(start, end);

    // the check alone, on both answers
    let f = fetch_window_from(&*other, &w, Verbosity::Full, Origin { node: 1, trusted: false }).await.unwrap();
    let end_blue = primary.block_blue_score(end).await.unwrap().unwrap();
    let p_sink = primary.sink_blue_score().await.unwrap();
    let ids = primary.chain_with_ids(start, window_min_confirmations(p_sink, end_blue, None)).await.unwrap();
    let n = verify_window(&f.raw, &ids).expect("a real window verifies");
    assert!(n >= 25, "{n} chain blocks verified");
    let txs: usize = f.raw.chain_block_accepted_transactions[..n].iter().map(|b| b.accepted_transactions.len()).sum();
    let v1 = f.raw.chain_block_accepted_transactions[..n]
        .iter()
        .flat_map(|b| &b.accepted_transactions)
        .filter(|t| t.version.is_some_and(|v| v > 0))
        .count();
    eprintln!("{n} chain blocks, {txs} transactions ({v1} of version 1) from {o_url} verify against {p_url}");
    // a High answer cannot be checked (no UTXO commitment, no version): refused, not trusted
    let high = fetch_window_from(&*other, &w, Verbosity::High, Origin { node: 1, trusted: false }).await.unwrap();
    assert!(matches!(verify_window(&high.raw, &ids), Err(Rejection::Lie(_))));
    // tampering is found
    let mut bad = fetch_window_from(&*other, &w, Verbosity::Full, Origin { node: 1, trusted: false }).await.unwrap();
    let t = bad
        .raw
        .chain_block_accepted_transactions
        .iter_mut()
        .flat_map(|b| b.accepted_transactions.iter_mut())
        .find(|t| !t.outputs.is_empty())
        .unwrap();
    t.outputs[0].value += 1;
    assert!(matches!(verify_window(&bad.raw, &ids), Err(Rejection::Lie(_))));

    // through the source: the other node serves, the primary checks
    let spec = |url: &str, fetch: bool| NodeSpec { url: url.into(), fetch, submit: false, connections: 1 };
    let m = MultiNode::new(vec![(spec(&p_url, false), primary.clone()), (spec(&o_url, true), other.clone())], client(&p_url));
    let f = m.fetch_window(w).await.unwrap();
    assert_eq!(f.origin, Origin { node: 1, trusted: false });
    // the planned blocks first (blocks past the end may follow: the sink moved while the request travelled)
    let k = f.raw.added_chain_block_hashes.len().min(chain.len());
    assert!(k > 0 && f.raw.added_chain_block_hashes[..k] == chain[..k]);
    let st = m.node_stats();
    assert_eq!(st[1].windows_ok, 1, "{st:?}");
    assert_eq!(st[1].lies, 0, "{st:?}");
}
