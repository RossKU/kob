//! Lean storage (`kob_executor::indexer::lean`): the store keeps the live book and a short retention of closed orders.
//!
//! Two stores follow the same synthetic chain, one that keeps its history and one that is lean. After every round of orders the
//! book the matcher and keepers read (`book::snapshot`) and the live rows must be identical; the lean store's file must stay
//! bounded while the other grows with the chain.

mod common;

use common::*;
use kob_executor::config::IndexerConfig;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book;
use kob_executor::indexer::db::{open_reader, open_writer_with};
use kob_executor::indexer::ingest::IngestConfig;
use kob_executor::indexer::lean::{self, LeanConfig};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use std::path::Path;

const WINDOW: u64 = 60;
const RETENTION: u64 = 100;

fn harness(node: &std::sync::Arc<MockNode>, path: &Path, lean: bool) -> Harness {
    harness_with(node, path, lean.then_some((RETENTION, 0)))
}

/// A store that is lean with (retention in DAA, size cap in bytes) or keeps its history.
fn harness_with(node: &std::sync::Arc<MockNode>, path: &Path, lean: Option<(u64, u64)>) -> Harness {
    let conn = open_writer_with(path, "testnet-10", lean.is_some()).unwrap();
    let cfg = IngestConfig {
        reorg_window_daa: WINDOW,
        checkpoint_daa: u64::MAX / 4,
        lean: lean.map(|(retention_daa, max_db_bytes)| LeanConfig {
            retention_daa,
            max_db_bytes,
            orders_per_batch: 7,
            rows_per_batch: 40,
            pages_per_batch: 50,
        }),
        ..IngestConfig::default()
    };
    Harness::with_node(node.clone(), conn, None, cfg)
}

/// An ask that is partly filled and open.
struct Live {
    /// The transaction whose outputs are the order's current UTXO and custody.
    tx: SignedTx,
    state: AskState,
    cov: Hash32,
    left: i64,
}

fn new_ask(w: &World, bump: i64) -> (SignedTx, AskState, Hash32) {
    let a = ask(MAKER_A, P250 + bump);
    let create = w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let cov = w.cov(&create, 0);
    (create, a, cov)
}

/// Fills `amount` of the live ask in a batch; returns the batch and what is left of the order.
fn fill(w: &World, node: &MockNode, live: &Live, first: bool, amount: i64) -> (SignedTx, Option<Live>) {
    let (oi, ci) = if first {
        (0, 1)
    } else {
        (find_cov(&live.tx, &live.cov).expect("continuation"), find_custody(&live.tx, &live.cov, live.left).expect("custody"))
    };
    let order = w.order(&live.tx, oi, live.state.clone());
    let custody = w.token_at(&live.tx, ci, Kcc20State::custody(live.left, live.cov.0, EXT));
    let b = batch(w, node.tip_daa(), vec![Leg::Ask { order, custody, amount, t: None }]);
    let t = w.sign(&Action::Batch(b));
    let left = live.left - amount;
    let next =
        (left > 0).then(|| Live { tx: t.clone(), state: AskState { amount_left: left, ..live.state.clone() }, cov: live.cov, left });
    (t, next)
}

fn cancel(w: &World, live: &Live) -> SignedTx {
    let ci = find_custody(&live.tx, &live.cov, live.left).unwrap_or(1);
    let oi = find_cov(&live.tx, &live.cov).unwrap_or(0);
    let req = CancelOrder {
        prefund: None,
        order: w.order(&live.tx, oi, AnyState::KobAsk(live.state.clone())),
        custody: Some(w.token_at(&live.tx, ci, Kcc20State::custody(live.left, live.cov.0, EXT))),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    };
    w.sign(&Action::CancelOrder(req))
}

fn size(h: &Harness) -> i64 {
    h.query::<i64>("PRAGMA page_count", []) * h.query::<i64>("PRAGMA page_size", [])
}

/// Prunes until a batch finds nothing more.
fn prune_all(h: &Harness) -> (usize, usize) {
    let (mut orders, mut rows) = (0, 0);
    for _ in 0..10_000 {
        let r = h.ingest.lock().unwrap().lean_prune(None).unwrap().expect("a lean store");
        orders += r.orders;
        rows += r.rows;
        if !r.more {
            return (orders, rows);
        }
    }
    panic!("the pruning never ended");
}

fn book_json(h: &Harness) -> String {
    let g = h.ingest.lock().unwrap();
    serde_json::to_string(&book::snapshot(g.conn(), None).unwrap()).unwrap()
}

/// The live rows a reader of the store sees: (order, status, filled, events, current UTXO) of every open or partial order.
fn live_rows(h: &Harness) -> Vec<String> {
    let g = h.ingest.lock().unwrap();
    let mut st = g
        .conn()
        .prepare(
            "SELECT hex(s.covenant_id), s.status, s.filled_amount, s.remaining_amount, hex(s.cur_txid), s.cur_idx, \
             (SELECT COUNT(*) FROM order_events e WHERE e.covenant_id = s.covenant_id), \
             (SELECT COUNT(*) FROM token_utxos t WHERE t.owner = s.covenant_id AND t.spent_block IS NULL) \
             FROM order_state s WHERE s.status IN ('open','partial') ORDER BY s.covenant_id",
        )
        .unwrap();
    st.query_map([], |r| {
        Ok(format!(
            "{} {} {} {:?} {} {} {} {}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[tokio::test]
async fn lean_pruning_keeps_what_the_matcher_reads_and_the_file_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let mut w = World::new();
    w.validate = false; // the engine is not what is under test: the indexer's rows are
    let full = harness(&node, &dir.path().join("full.sqlite3"), false);
    let lean = harness(&node, &dir.path().join("lean.sqlite3"), true);
    assert_eq!(lean.query::<i64>("PRAGMA auto_vacuum", []), 2, "a lean store is incremental-vacuum capable from its creation");
    assert_eq!(full.query::<i64>("PRAGMA auto_vacuum", []), 0);

    // one order a round: every third is filled, cancelled, or filled in two steps (the second step ten rounds later)
    let mut pending: Vec<(usize, Live)> = vec![];
    let mut closed = 0;
    let (mut size_mid, mut size_full_mid) = (0, 0);
    const ROUNDS: usize = 300;
    for r in 0..ROUNDS {
        let (create, a, cov) = new_ask(&w, r as i64);
        w.include(&node, &[&create]);
        let live = Live { tx: create, state: a, cov, left: 10 * WHOLE };
        match r % 3 {
            0 => {
                let (t, rest) = fill(&w, &node, &live, true, 10 * WHOLE);
                assert!(rest.is_none());
                w.include(&node, &[&t]);
                closed += 1;
            }
            1 => {
                let c = cancel(&w, &live);
                w.include(&node, &[&c]);
                closed += 1;
            }
            _ => {
                let (t, rest) = fill(&w, &node, &live, true, 4 * WHOLE);
                w.include(&node, &[&t]);
                pending.push((r, rest.unwrap()));
            }
        }
        // the partly filled order of ten rounds ago is filled to the end
        if let Some(i) = pending.iter().position(|(at, _)| r >= at + 10) {
            let (_, old) = pending.remove(i);
            let (t, rest) = fill(&w, &node, &old, false, old.left);
            assert!(rest.is_none());
            w.include(&node, &[&t]);
            closed += 1;
        }
        node.push_empty(3);
        full.sync().await;
        lean.sync().await;
        prune_all(&lean);

        // what the matcher and the keepers read, and what an open order's state is derived from, is the same
        assert_eq!(book_json(&full), book_json(&lean), "round {r}: the book differs");
        assert_eq!(live_rows(&full), live_rows(&lean), "round {r}: the live rows differ");
        if r == ROUNDS / 2 {
            size_mid = size(&lean);
            size_full_mid = size(&full);
        }
    }
    assert!(closed > 150 && !pending.is_empty());

    // the history is gone from the lean store, the live book is whole
    let (all, kept): (i64, i64) = (full.query("SELECT COUNT(*) FROM orders", []), lean.query("SELECT COUNT(*) FROM orders", []));
    assert_eq!(all, ROUNDS as i64);
    assert!(kept < all / 2, "the lean store keeps {kept} of {all} orders");
    let closed_kept: i64 = lean.query("SELECT COUNT(*) FROM order_state WHERE status NOT IN ('open','partial')", []);
    // a closed order stays for the retention (100 DAA, about 20 rounds of 5 blocks, 2 of 3 rounds close)
    assert!(closed_kept <= 20, "{closed_kept} closed orders are older than the retention");
    for t in ["order_events", "order_utxos", "token_utxos", "token_holdings"] {
        // the unspent holdings of key owners are live data (the operator's own are read by the maintenance jobs); every spent row
        // and every row of a closed order goes
        let spent = if t == "token_holdings" { " WHERE spent_block IS NOT NULL OR role IN ('custody','stray')" } else { "" };
        let (a, b): (i64, i64) =
            (full.query(&format!("SELECT COUNT(*) FROM {t}{spent}"), []), lean.query(&format!("SELECT COUNT(*) FROM {t}{spent}"), []));
        assert!(b * 2 < a, "{t}: {b} rows kept of {a}");
    }
    // the health counters follow the deletions
    let counted: i64 = lean.query("SELECT n FROM counters WHERE name = 'orders'", []);
    assert_eq!(counted, kept);

    // the file: the lean store gives pages back and stays about the size it had at half time; the other keeps growing
    let (end_lean, end_full) = (size(&lean), size(&full));
    assert!(end_lean < end_full / 2, "lean {end_lean} bytes, full {end_full}");
    // what still grows is the live data (the synthetic takers never spend the tokens they receive: one unspent holding a round)
    let (grew_lean, grew_full) = (end_lean - size_mid, end_full - size_full_mid);
    assert!(
        grew_lean * 3 < grew_full,
        "second half: the lean file grew {grew_lean} bytes ({size_mid} to {end_lean}), the full one {grew_full}"
    );
    assert!(end_full as f64 > size_full_mid as f64 * 1.5, "the full file grows with the chain: {size_full_mid} to {end_full}");

    // the store goes on after the pruning: a revert inside the window, and a fill of the oldest live order
    let tip_before = node.tip();
    node.reorg(2, vec![vec![], vec![]]);
    full.sync().await;
    lean.sync().await;
    assert_ne!(node.tip(), tip_before);
    assert_eq!(book_json(&full), book_json(&lean));
    let (old_at, old) = {
        let g = lean.ingest.lock().unwrap();
        let cov: Vec<u8> = g
            .conn()
            .query_row("SELECT covenant_id FROM order_state WHERE status = 'partial' ORDER BY last_block LIMIT 1", [], |r| r.get(0))
            .expect("a partly filled order is still live");
        drop(g);
        let cov = Hash32::from_slice(&cov).unwrap();
        let (_, l) = pending.iter().find(|(_, l)| l.cov == cov).expect("a pending order of the test");
        (cov, Live { tx: l.tx.clone(), state: l.state.clone(), cov: l.cov, left: l.left })
    };
    let (t, rest) = fill(&w, &node, &old, false, old.left);
    assert!(rest.is_none());
    w.include(&node, &[&t]);
    node.push_empty(3);
    full.sync().await;
    lean.sync().await;
    assert_eq!(lean.status(&old_at), "filled");
    assert_eq!(full.status(&old_at), "filled");
    let f: i64 = lean.query("SELECT filled_amount FROM order_state WHERE covenant_id = ?1", [&old_at.0[..]]);
    assert_eq!(f, 10 * WHOLE, "the fills of an order that was live all along are never pruned");
    assert_eq!(book_json(&full), book_json(&lean));

    // the pruning respects the reorg window: nothing stamped with a block of the window is deleted, whatever the retention
    let (before_orders, _) = (lean.query::<i64>("SELECT COUNT(*) FROM orders", []), 0);
    let r = lean.ingest.lock().unwrap().lean_prune(None).unwrap().unwrap();
    let after: i64 = lean.query("SELECT COUNT(*) FROM orders", []);
    assert_eq!(before_orders - after, r.orders as i64);
    let newest_closed: i64 =
        lean.query("SELECT COALESCE(MAX(last_daa), 0) FROM order_state WHERE status NOT IN ('open','partial')", []);
    assert!(newest_closed as u64 + WINDOW > node.tip_daa().saturating_sub(WINDOW), "orders that closed inside the window stay");
}

/// The size cap: only above it, the oldest closed orders go first, whatever the retention; the live book is never touched.
#[tokio::test]
async fn above_the_size_cap_the_oldest_closed_orders_go_first() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let mut w = World::new();
    w.validate = false;
    let full = harness(&node, &dir.path().join("full.sqlite3"), false);
    // a retention that keeps everything: only the cap deletes
    let lean = harness_with(&node, &dir.path().join("lean.sqlite3"), Some((u64::MAX / 8, 0)));
    let mut live = vec![];
    for r in 0..60 {
        let (create, a, cov) = new_ask(&w, r);
        w.include(&node, &[&create]);
        let l = Live { tx: create, state: a, cov, left: 10 * WHOLE };
        if r % 10 == 0 {
            let (t, rest) = fill(&w, &node, &l, true, 4 * WHOLE);
            w.include(&node, &[&t]);
            live.push(rest.unwrap());
        } else {
            let (t, _) = fill(&w, &node, &l, true, 10 * WHOLE);
            w.include(&node, &[&t]);
        }
        node.push_empty(3);
    }
    node.push_empty(2 * WINDOW as usize);
    full.sync().await;
    lean.sync().await;
    // no cap, a retention that keeps everything: nothing to delete
    assert_eq!(prune_all(&lean), (0, 0));
    let orders: i64 = lean.query("SELECT COUNT(*) FROM orders", []);
    assert_eq!(orders, 60);
    let closed_daa = |h: &Harness| -> Vec<i64> {
        let g = h.ingest.lock().unwrap();
        let mut st =
            g.conn().prepare("SELECT last_daa FROM order_state WHERE status NOT IN ('open','partial') ORDER BY last_daa").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    let all_closed = closed_daa(&lean);
    assert_eq!(all_closed.len(), 54);

    // a cap a little under what the store uses (the schema and the live book are most of a store this small): some of the closed
    // orders must go, the oldest first
    let used = lean::used_bytes(lean.ingest.lock().unwrap().conn()).unwrap();
    let cap = used * 85 / 100;
    lean.ingest.lock().unwrap().set_lean_cap(cap);
    let (deleted, _) = prune_all(&lean);
    let after = lean::used_bytes(lean.ingest.lock().unwrap().conn()).unwrap();
    assert!(deleted > 0 && deleted < all_closed.len() && after <= cap, "{deleted} orders deleted, {after} bytes used, cap {cap}");
    let kept = closed_daa(&lean);
    assert_eq!(kept.len(), all_closed.len() - deleted);
    assert_eq!(kept, all_closed[deleted..], "the oldest closed orders went first, the newest stay");
    // the live book is whole, and the store is not pruned further once under the cap
    assert_eq!(book_json(&full), book_json(&lean));
    assert_eq!(live_rows(&full), live_rows(&lean));
    assert_eq!(live_rows(&lean).len(), live.len());
    assert_eq!(prune_all(&lean), (0, 0));
    // a cap below what the live book alone needs deletes every closed order it may and leaves the rest
    lean.ingest.lock().unwrap().set_lean_cap(1);
    prune_all(&lean);
    assert!(closed_daa(&lean).is_empty());
    assert_eq!(book_json(&full), book_json(&lean));
    assert_eq!(live_rows(&full), live_rows(&lean));
}

#[test]
fn a_store_becomes_incremental_once_and_keeps_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite3");
    {
        let c = open_writer_with(&path, "testnet-10", false).unwrap();
        assert_eq!(lean::auto_vacuum_mode(&c).unwrap(), 0);
        c.execute("INSERT INTO rejects (block_seq, txid, reason) VALUES (1, x'01', 'kept')", []).unwrap();
    }
    let c = open_writer_with(&path, "testnet-10", true).unwrap();
    assert_eq!(lean::auto_vacuum_mode(&c).unwrap(), 2, "rewritten once");
    let n: i64 = c.query_row("SELECT COUNT(*) FROM rejects WHERE reason = 'kept'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    // nothing to release on a store with no free pages; a second open changes nothing
    assert_eq!(lean::release_pages(&c, 10).unwrap(), 0);
    drop(c);
    let c = open_writer_with(&path, "testnet-10", true).unwrap();
    assert_eq!(lean::auto_vacuum_mode(&c).unwrap(), 2);
    // a lean store opened without lean stays what it is
    drop(c);
    assert_eq!(lean::auto_vacuum_mode(&open_writer_with(&path, "testnet-10", false).unwrap()).unwrap(), 2);
}

/// The indexer of a lean configuration: no record log, the health says so, and the follower prunes by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lean_indexer_prunes_by_itself_and_keeps_no_record_log() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let mut w = World::new();
    w.validate = false;
    // history: one order filled and one cancelled long ago, one still open
    let (c1, a1, cov1) = new_ask(&w, 1);
    let (c2, a2, cov2) = new_ask(&w, 2);
    let (c3, _, cov3) = new_ask(&w, 3);
    w.include(&node, &[&c1, &c2, &c3]);
    let l1 = Live { tx: c1, state: a1, cov: cov1, left: 10 * WHOLE };
    let (t, _) = fill(&w, &node, &l1, true, 10 * WHOLE);
    w.include(&node, &[&t]);
    let l2 = Live { tx: c2, state: a2, cov: cov2, left: 10 * WHOLE };
    let cx = cancel(&w, &l2);
    w.include(&node, &[&cx]);
    node.push_empty(10);

    let tokens = dir.path().join("tokens.json");
    std::fs::write(
        &tokens,
        format!(r#"[{{"ticker":"TST","family":"kcc20","covenant_id":"{}"}}]"#, kob_executor::hex::encode(&TOKEN_COV)),
    )
    .unwrap();
    let mut cfg = IndexerConfig {
        network: "testnet-10".into(),
        data_dir: dir.path().join("data"),
        tokens_path: Some(tokens),
        start: kob_executor::config::StartMode::Hash(node.anchor()),
        poll_interval_ms: 5,
        lean: true,
        lean_retention_days: 0,
        reorg_window_hours: 0,
        rules: kob_executor::tokens::ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..Default::default() },
        ..IndexerConfig::default()
    };
    cfg.api.enabled = false;
    cfg.records.enabled = true; // lean storage turns the log off whatever the configuration says
    let idx = kob_executor::indexer::Indexer::open(cfg.clone()).unwrap();
    assert!(!idx.cfg.records.enabled && !cfg.records_dir().join("seg-000000000000.kobrec").exists());
    assert!(idx.health.snapshot().lean);
    let running = idx.spawn_follower(node.clone());
    let reader = open_reader(&cfg.db_path(), "testnet-10").unwrap();
    let t0 = std::time::Instant::now();
    loop {
        let n: i64 = reader.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0)).unwrap();
        let state = idx.health.snapshot();
        if n == 1 && state.lean_orders_pruned_total == 2 {
            break;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "{n} orders, {:?}", state.lean_orders_pruned_total);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let left: Vec<u8> = reader.query_row("SELECT covenant_id FROM orders", [], |r| r.get(0)).unwrap();
    assert_eq!(left, cov3.0.to_vec(), "the open order is the one that stays");
    assert!(idx.health.snapshot().metrics_text().contains("kob_indexer_lean_orders_pruned_total 2"));
    running.stop().await;
}
