//! A store whose cursor the node pruned (a downtime longer than the node's retention) does not halt at the gap for good: the
//! follower moves it to the node's pruning point, with the orders it holds reconciled with the node's UTXO set, and follows
//! from there (`kob_executor::indexer::recovery`). The skipped chain is reported in the health.
//!
//! The same recovery serves the fast start's stores (`tests/fast_start.rs`); these are the normal start's.

use std::time::{Duration, Instant};

use kob_executor::config::{IndexerConfig, StartMode};
use kob_executor::hex::{Hash32, HexBytes};
use kob_executor::indexer::db::open_reader;
use kob_executor::indexer::recovery::MAX_GAP_REBASES;
use kob_executor::indexer::status::FollowerState;
use kob_executor::indexer::Indexer;
use kob_executor::rpc::types::{AddressUtxo, AddressUtxoEntry, Outpoint};
use kob_executor::script::{spk_address, spk_bytes};
use kob_executor::testkit::*;
use kob_protocol::state::*;
use kob_protocol::tx::SignedTx;

fn config(dir: &std::path::Path) -> IndexerConfig {
    let tokens = dir.join("tokens.json");
    std::fs::write(
        &tokens,
        format!(r#"[{{"ticker":"TST","family":"kcc20","covenant_id":"{}"}}]"#, kob_executor::hex::encode(&TOKEN_COV)),
    )
    .unwrap();
    let mut cfg = IndexerConfig {
        network: "testnet-10".into(),
        data_dir: dir.join("data"),
        tokens_path: Some(tokens),
        // the mock node's pruning point is the block at its retention floor: the first block until the test moves it
        start: StartMode::PruningPoint,
        poll_interval_ms: 5,
        backoff_ms: 5,
        max_backoff_ms: 20,
        rules: kob_executor::tokens::ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..Default::default() },
        ..IndexerConfig::default()
    };
    cfg.api.enabled = false;
    cfg
}

async fn until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn small_ask(w: &World, bump: i64) -> SignedTx {
    let a = AskState { amount_left: 5 * WHOLE, ..ask(MAKER_A, P250 + bump) };
    w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 5 * WHOLE)
}

fn status(cfg: &IndexerConfig, cov: &Hash32) -> Option<String> {
    let conn = open_reader(&cfg.db_path(), "testnet-10").unwrap();
    conn.query_row("SELECT status FROM order_state WHERE covenant_id = ?1", [&cov.0[..]], |r| r.get(0)).ok()
}

/// An unspent order output as the node's `getUtxosByAddresses` reports it.
fn node_utxo(spk: &[u8], cov: Hash32, tx: &SignedTx) -> (String, AddressUtxo) {
    (
        spk_address(spk, "testnet-10").unwrap(),
        AddressUtxo {
            outpoint: Outpoint { transaction_id: Hash32(tx.tx.id), index: 0 },
            utxo_entry: AddressUtxoEntry {
                amount: tx.tx.outputs[0].value,
                script_public_key: HexBytes(spk.to_vec()),
                block_daa_score: 1_050,
                covenant_id: Some(cov),
            },
        },
    )
}

/// The store follows the chain, stops, and the node prunes past its cursor before the next start. The follower used to halt
/// there for good (state `gap`, nothing indexed, the operator's `index rebase`); it now continues from the node's pruning point:
/// the order the node still holds stays open, the one it no longer holds is closed with reason `gap`, an order created after
/// the pruning point is indexed, and the health says that a stretch of chain was skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_store_whose_cursor_the_node_pruned_follows_from_the_new_pruning_point() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let w = World::new();
    let (c1, c2) = (small_ask(&w, 1), small_ask(&w, 2));
    w.include(&node, &[&c1, &c2]);
    node.push_empty(10);
    let cfg = config(dir.path());
    let (cov1, cov2) = (w.cov(&c1, 0), w.cov(&c2, 0));

    // first run: both orders are indexed
    let old_daa = {
        let idx = Indexer::open(cfg.clone()).unwrap();
        let running = idx.spawn(node.clone()).unwrap();
        until("both orders are indexed", 30, || {
            idx.health.snapshot().cursor_hash == Some(node.tip()) && status(&cfg, &cov2).is_some()
        })
        .await;
        running.stop().await;
        idx.health.snapshot().cursor_daa
    };
    assert_eq!(status(&cfg, &cov1).as_deref(), Some("open"));

    // the downtime: the chain grows, an order is created at its end, the node prunes everything before it
    node.push_empty(20);
    let c3 = small_ask(&w, 3);
    w.include(&node, &[&c3]);
    node.push_empty(5);
    let floor = node.chain().len() - 8;
    node.set_retention_floor(floor);
    let pruning_point = node.chain()[floor].clone();
    // the node holds the first order's output; the second was spent while the store was down
    node.set_utxos(vec![node_utxo(
        &spk_bytes(&AnyState::KobAsk(AskState { amount_left: 5 * WHOLE, ..ask(MAKER_A, P250 + 1) }).spk()),
        cov1,
        &c1,
    )]);

    let idx = Indexer::open(cfg.clone()).unwrap();
    let running = idx.spawn(node.clone()).unwrap();
    let cov3 = w.cov(&c3, 0);
    until("the store follows from the new pruning point", 30, || {
        let h = idx.health.snapshot();
        h.state == FollowerState::Following && h.cursor_hash == Some(node.tip()) && status(&cfg, &cov3).is_some()
    })
    .await;
    let h = idx.health.snapshot();
    assert_eq!(h.gaps.len(), 1, "{:?}", h.gaps);
    let g = &h.gaps[0];
    assert_eq!((g.from_daa, g.to_daa, g.to_hash), (old_daa, pruning_point.daa, pruning_point.hash));
    assert_eq!((g.orders_closed, g.orders_adopted), (1, 0));
    assert!(h.gap_reason.is_none() && h.last_error.is_none(), "{h:?}");
    assert_eq!(status(&cfg, &cov1).as_deref(), Some("open"), "the order the node still holds stays");
    assert_eq!(status(&cfg, &cov2).as_deref(), Some("closed"), "the order it no longer holds is closed, not dropped");
    assert_eq!(status(&cfg, &cov3).as_deref(), Some("open"), "an order created after the pruning point is indexed");
    // the closed order keeps its history: its row and its placement are still there
    let conn = open_reader(&cfg.db_path(), "testnet-10").unwrap();
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 3);
    let reason: Option<String> =
        conn.query_row("SELECT spent_entry FROM order_utxos WHERE txid = ?1", [&c2.tx.id[..]], |r| r.get(0)).unwrap();
    assert_eq!(reason.as_deref(), Some("gap"));
    // where an operator looks
    assert!(h.status_line().contains("skipped"), "{}", h.status_line());
    let v = serde_json::to_value(&h).unwrap();
    assert_eq!(v["gaps"][0]["from_daa"], old_daa);
    assert!(h.metrics_text().contains("kob_indexer_gaps_total 1"), "{}", h.metrics_text());
    running.stop().await;
}

/// A node that keeps its pruning point ahead of the store (here: a pruning point that is itself below the node's retention
/// root, so every look finds the gap again) is not chased for ever: after [`MAX_GAP_REBASES`] moves the store stays halted at
/// the gap, which the health shows, and a stop still ends it at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_store_the_node_keeps_pruning_is_moved_a_bounded_number_of_times() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    node.push_empty(5);
    let cfg = config(dir.path());
    let idx = Indexer::open(cfg.clone()).unwrap();
    let running = idx.spawn(node.clone()).unwrap();
    until("the store is at the tip", 30, || idx.health.snapshot().cursor_hash == Some(node.tip())).await;
    node.push_empty(3);
    node.set_retention_floor(10_000);
    until("the moves are used up", 30, || idx.health.snapshot().gaps.len() as u32 == MAX_GAP_REBASES).await;
    until("the store halts at the gap", 30, || idx.health.snapshot().state == FollowerState::Gap).await;
    let calls = node.vspc_calls();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let h = idx.health.snapshot();
    assert_eq!(h.gaps.len() as u32, MAX_GAP_REBASES, "no further move");
    assert!(h.gap_reason.is_some_and(|r| r.contains("downtime exceeded")), "the halt says why");
    assert_eq!(node.vspc_calls(), calls, "a halted store does not ask the node again");
    tokio::time::timeout(Duration::from_secs(5), running.stop()).await.expect("a halted store stops");
}

/// A move that cannot be made now (the node does not know its pruning point) is tried again later, does not count against the
/// bound, and the stop is not held up by the wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_the_node_cannot_serve_yet_is_retried_and_stoppable() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    node.push_empty(5);
    let cfg = config(dir.path());
    let idx = Indexer::open(cfg.clone()).unwrap();
    let running = idx.spawn(node.clone()).unwrap();
    until("the store is at the tip", 30, || idx.health.snapshot().cursor_hash == Some(node.tip())).await;
    node.push_empty(10);
    let floor = node.chain().len() - 4;
    let pp = node.chain()[floor].hash;
    node.set_retention_floor(floor);
    node.forget(pp);
    until("the move is tried", 30, || idx.health.snapshot().last_error.is_some_and(|e| e.contains("moving the store past the gap")))
        .await;
    let h = idx.health.snapshot();
    assert!(h.gaps.is_empty() && h.state == FollowerState::Gap, "{h:?}");
    tokio::time::timeout(Duration::from_secs(5), running.stop()).await.expect("the wait is stoppable");
}
