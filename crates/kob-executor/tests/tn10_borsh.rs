//! Read-only check of the Borsh windows against live testnet-10 nodes: the same window fetched over JSON and over Borsh
//! decodes to identical chain blocks and transactions (`High` from the primary, `Full` from another node), a `Full` Borsh
//! window verifies against the primary's acceptance ids fetched over Borsh, and Borsh is the smaller answer.
//!
//! Environment:
//! * `KOB_TN10_WRPC`               the primary (JSON), default `ws://127.0.0.1:18210`; Borsh at its port `17xxx`
//! * `KOB_TN10_WRPC_OTHER`         the other node (JSON), default a public TN10 node; Borsh at `.../wrpc/borsh`
//! * `KOB_SKIP_NETWORK_TESTS=1`    skip (offline runs)
//! * `KOB_REQUIRE_NETWORK_TESTS=1` fail instead of skip when a node is unreachable

use kob_executor::hex::Hash32;
use kob_executor::rpc::borsh::borsh_url_for;
use kob_executor::rpc::types::{RawVspcResponse, Verbosity};
use kob_executor::rpc::verify::verify_window;
use kob_executor::rpc::{fetch_window_from, window_min_confirmations, ChainSource, Origin, WindowRequest, WrpcClient, WrpcConfig};
use std::sync::Arc;
use std::time::Duration;

const PRIMARY: &str = "ws://127.0.0.1:18210";
const OTHER: &str = "wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/json";

fn client(url: &str, borsh: bool) -> Arc<WrpcClient> {
    let mut c = WrpcConfig::new(url.to_string());
    c.connect_timeout = Duration::from_secs(10);
    c.request_timeout = Duration::from_secs(120);
    c.vspc_connections = 1;
    c.borsh_url = if borsh { Some(borsh_url_for(url).expect("a Borsh endpoint for the node")) } else { None };
    Arc::new(WrpcClient::new(c))
}

fn same(a: &RawVspcResponse, b: &RawVspcResponse, what: &str) {
    let n = a.added_chain_block_hashes.len().min(b.added_chain_block_hashes.len());
    assert!(n > 0, "{what}: empty window");
    assert_eq!(a.added_chain_block_hashes[..n], b.added_chain_block_hashes[..n], "{what}: chain");
    assert_eq!(a.removed_chain_block_hashes, b.removed_chain_block_hashes, "{what}: removed");
    for (x, y) in a.chain_block_accepted_transactions.iter().zip(&b.chain_block_accepted_transactions) {
        assert_eq!(x.chain_block_header, y.chain_block_header, "{what}: header");
        assert_eq!(x.accepted_transactions, y.accepted_transactions, "{what}: transactions of {}", y.chain_block_header.hash);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn borsh_windows_equal_json_windows_on_tn10() {
    if std::env::var("KOB_SKIP_NETWORK_TESTS").is_ok() {
        eprintln!("skipped: KOB_SKIP_NETWORK_TESTS is set");
        return;
    }
    let p_url = std::env::var("KOB_TN10_WRPC").unwrap_or_else(|_| PRIMARY.to_string());
    let o_url = std::env::var("KOB_TN10_WRPC_OTHER").unwrap_or_else(|_| OTHER.to_string());
    let (primary, primary_b) = (client(&p_url, false), client(&p_url, true));
    let (other, other_b) = (client(&o_url, false), client(&o_url, true));
    for (url, c) in [(&p_url, &primary), (&o_url, &other)] {
        match c.server_info().await {
            Ok(i) => assert_eq!(i.network_id, "testnet-10"),
            Err(e) if std::env::var("KOB_REQUIRE_NETWORK_TESTS").is_err() => {
                eprintln!("skipped: {url} unreachable ({e})");
                return;
            }
            Err(e) => panic!("{url} unreachable: {e}"),
        }
    }
    // a window of 8 chain blocks starting 40 chain blocks behind the primary's sink
    let mut start = primary.dag_info().await.unwrap().sink;
    for _ in 0..40 {
        let raw =
            primary.call_raw("getBlock", serde_json::json!({ "hash": start.to_hex(), "includeTransactions": false })).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        start = Hash32::parse(v["block"]["verboseData"]["selectedParentHash"].as_str().unwrap()).unwrap();
    }
    let hashes = primary.chain_hashes(start).await.unwrap();
    let added = &hashes.added_chain_block_hashes;
    let end = *added[..added.len().min(8)].last().unwrap();
    let end_blue = primary.block_blue_score(end).await.unwrap().unwrap();
    let w = WindowRequest::new(start, end);

    // the primary at High: JSON and Borsh agree
    let j = fetch_window_from(&*primary, &w, Verbosity::High, Origin::PRIMARY).await.unwrap();
    let b = fetch_window_from(&*primary_b, &w, Verbosity::High, Origin::PRIMARY).await.unwrap();
    let n = j.raw.added_chain_block_hashes.len().min(b.raw.added_chain_block_hashes.len());
    same(&j.raw, &b.raw, "primary High");
    let txs: usize = b.raw.chain_block_accepted_transactions[..n].iter().map(|c| c.accepted_transactions.len()).sum();
    eprintln!(
        "primary High: {n} chain blocks, {txs} transactions; JSON {} bytes, Borsh {} bytes ({:.2}x)",
        j.raw.wire_bytes,
        b.raw.wire_bytes,
        j.raw.wire_bytes as f64 / b.raw.wire_bytes.max(1) as f64
    );
    assert!(b.raw.wire_bytes < j.raw.wire_bytes, "Borsh is the smaller answer");

    // the other node at Full over Borsh verifies against the primary's ids over Borsh, and equals its JSON answer
    let ob = fetch_window_from(&*other_b, &w, Verbosity::Full, Origin { node: 1, trusted: false }).await.unwrap();
    let oj = fetch_window_from(&*other, &w, Verbosity::Full, Origin { node: 1, trusted: false }).await.unwrap();
    same(&oj.raw, &ob.raw, "other Full");
    let p_sink = primary_b.sink_blue_score().await.unwrap();
    let ids = primary_b.chain_with_ids(start, window_min_confirmations(p_sink, end_blue, None)).await.unwrap();
    let ids_json = primary.chain_with_ids(start, window_min_confirmations(p_sink, end_blue, None)).await.unwrap();
    let k = ids.added_chain_block_hashes.len().min(ids_json.added_chain_block_hashes.len());
    assert_eq!(ids.added_chain_block_hashes[..k], ids_json.added_chain_block_hashes[..k]);
    let verified = verify_window(&ob.raw, &ids).expect("a Borsh Full window verifies");
    assert!(verified >= 6, "{verified} chain blocks verified");
    eprintln!(
        "other Full: {verified} chain blocks verified; JSON {} bytes, Borsh {} bytes ({:.2}x)",
        oj.raw.wire_bytes,
        ob.raw.wire_bytes,
        oj.raw.wire_bytes as f64 / ob.raw.wire_bytes.max(1) as f64
    );

    // a node error over Borsh is classified like the JSON one
    let unknown = Hash32([0x5a; 32]);
    let e = fetch_window_from(
        &*primary_b,
        &WindowRequest { end_blue: Some(end_blue), sink_blue: Some(p_sink), ..WindowRequest::new(unknown, end) },
        Verbosity::High,
        Origin::PRIMARY,
    )
    .await
    .err()
    .expect("an unknown start hash is refused");
    eprintln!("unknown start over Borsh: {e}");
    assert!(matches!(e, kob_executor::rpc::RpcError::Node(_)), "{e}");
}
