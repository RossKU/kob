//! Archive pruning (`kobd::indexer::archive`): a store that keeps its history deletes the final spent UTXO rows of the orders that are no
//! longer live, the amends of final blocks and the spent holdings past a retention, and answers the API as a store that keeps everything.
//!
//! Two stores follow the same synthetic chain, one that prunes and one that does not. The read API of both answers the same JSON on its
//! main endpoints after every few rounds of orders (placed, filled, partly filled, amended, cancelled), after a reorg inside the window
//! that undoes a closing, and after a fill of an order that stayed live through all of it; a rebuild of the pruning store from its record
//! log (`kobd db replay` prunes at its end) holds the same rows; and with a short retention the spent holdings past it go while the token
//! keeps its program (the token list) and a live order keeps its spent custody.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::*;
use http_body_util::BodyExt;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::api::{self, ApiState};
use kobd::config::ApiConfig;
use kobd::hex::Hash32;
use kobd::indexer::archive::ArchiveConfig;
use kobd::indexer::db::{open_writer_with, ReadPool};
use kobd::indexer::ingest::{Ingest, IngestConfig};
use kobd::indexer::recordlog::RecordLog;
use kobd::indexer::status::{FollowerState, HealthState};
use kobd::testkit::*;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

/// The reorg window in DAA (one DAA a block).
const WINDOW: u64 = 40;

fn ingest_cfg(retention: Option<u64>) -> IngestConfig {
    IngestConfig {
        reorg_window_daa: WINDOW,
        // every batch is logged: a rebuild from the log ends at the cursor the store followed to, with the same chain blocks
        checkpoint_daa: 1,
        archive: retention.map(|r| ArchiveConfig {
            // small steps, so that a batch crosses many stretch boundaries; the time budget never ends one here
            step_rows: 3,
            rows_per_batch: 25,
            budget: std::time::Duration::from_secs(60),
            ..ArchiveConfig::new(r)
        }),
        ..IngestConfig::default()
    }
}

/// A store on disk following `node`: archive pruning with a holdings retention of `retention` DAA, or none (it keeps everything).
fn harness(node: &Arc<MockNode>, path: &Path, retention: Option<u64>, log: Option<RecordLog>) -> Harness {
    Harness::with_node(node.clone(), open_writer_with(path, "testnet-10", false).unwrap(), log, ingest_cfg(retention))
}

/// The read API over a store file, with the same follower state for every store.
fn router(path: &Path, node_daa: u64) -> axum::Router {
    let health = Arc::new(HealthState::new("testnet-10"));
    health.update(|s| {
        s.state = FollowerState::Following;
        s.node_daa = Some(node_daa);
        s.cursor_daa = node_daa;
    });
    let mut cfg = ApiConfig::default();
    cfg.rate_limit.per_ip_rps = 0.0;
    cfg.rate_limit.global_rps = 0.0;
    let (events, _) = tokio::sync::broadcast::channel(16);
    api::router(ApiState {
        pool: ReadPool::open(path, 2).unwrap(),
        generation: Default::default(),
        health,
        events,
        tokens: Arc::new(allowlist()),
        cfg,
        settle_depth_daa: 10,
        daa_per_second: 10,
        lag_alarm_hours: vec![1],
        min_order_value_sompi: 1,
        logo: None,
        price: None,
    })
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let mut req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 4000))));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(status.is_success(), "{uri}: {status} {}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

/// The main endpoints of the read API, and every order with its events.
fn uris(h: &Harness) -> Vec<String> {
    let t = Hash32(TOKEN_COV).to_hex();
    let mut v: Vec<String> = [
        "/v1/health".to_string(),
        "/v1/tokens".to_string(),
        format!("/v1/books/{t}"),
        format!("/v1/books/{t}?aggregate=false"),
        "/v1/orders?limit=500".to_string(),
        format!("/v1/orders?token={t}&limit=500"),
        "/v1/fills?limit=500".to_string(),
        "/v1/strays".to_string(),
        "/v1/token-events".to_string(),
        format!("/v1/token-utxos?token={t}&limit=500"),
        format!("/v1/trades/{t}?limit=500"),
        format!("/v1/candles/{t}?interval=1m"),
        format!("/v1/candles/{t}?interval=1h"),
        "/v1/stats".to_string(),
        format!("/v1/stats/{t}"),
        format!("/v1/depth/{t}"),
        "/v1/pairs".to_string(),
    ]
    .into();
    let g = h.ingest.lock().unwrap();
    let mut st = g.conn().prepare("SELECT covenant_id FROM orders ORDER BY covenant_id").unwrap();
    for cov in st.query_map([], |r| r.get::<_, Vec<u8>>(0)).unwrap() {
        let cov = Hash32::from_slice(&cov.unwrap()).unwrap().to_hex();
        v.push(format!("/v1/orders/{cov}"));
        v.push(format!("/v1/orders/{cov}/events?limit=500"));
    }
    v
}

/// Every endpoint of [`uris`] (and the spent holdings when `spent`) answers the same JSON from both stores.
async fn same_answers(full: (&Harness, &Path), pruned: (&Harness, &Path), node: &MockNode, spent: bool, what: &str) -> usize {
    compare(full, pruned, node, spent, what, false).await
}

/// [`same_answers`]; `renumbered`: the second store numbers its chain blocks apart from the first (a rebuild from the log), so the
/// numbers (`block_seq`, `last_block_seq`) are not compared.
async fn compare(
    full: (&Harness, &Path),
    pruned: (&Harness, &Path),
    node: &MockNode,
    spent: bool,
    what: &str,
    renumbered: bool,
) -> usize {
    let (a, b) = (router(full.1, node.tip_daa()), router(pruned.1, node.tip_daa()));
    let mut list = uris(full.0);
    if spent {
        list.push(format!("/v1/token-utxos?token={}&spent=true&limit=500", Hash32(TOKEN_COV).to_hex()));
    }
    for u in &list {
        let (x, y) = (get(&a, u).await, get(&b, u).await);
        if renumbered {
            assert_eq!(unsequenced(x), unsequenced(y), "{what}: {u}");
        } else {
            assert_eq!(x, y, "{what}: {u}");
        }
    }
    list.len()
}

/// An answer without the store's own numbers of its chain blocks (`block_seq`, `last_block`, `last_block_seq`, and the page cursors that
/// name them).
fn unsequenced(v: Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.into_iter()
                .filter(|(k, _)| !matches!(k.as_str(), "block_seq" | "last_block" | "last_block_seq" | "next_cursor"))
                .map(|(k, v)| (k, unsequenced(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(unsequenced).collect()),
        v => v,
    }
}

fn count(h: &Harness, table: &str) -> i64 {
    h.query(&format!("SELECT COUNT(*) FROM {table}"), [])
}

/// An ask whose UTXO, custody and terms the test follows.
struct Live {
    /// The transaction of the order's current UTXO.
    tx: SignedTx,
    /// Its custody: the transaction and output (an amend leaves it where it was).
    custody: (SignedTx, usize),
    state: AskState,
    cov: Hash32,
    left: i64,
}

fn new_ask(w: &World, bump: i64) -> Live {
    let a = ask(MAKER_A, P250 + bump);
    let tx = w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let cov = w.cov(&tx, 0);
    Live { custody: (tx.clone(), 1), tx, state: a, cov, left: 10 * WHOLE }
}

fn custody_of(w: &World, l: &Live) -> TokenUtxo {
    w.token_at(&l.custody.0, l.custody.1, Kcc20State::custody(l.left, l.cov.0, EXT))
}

fn order_of(w: &World, l: &Live) -> OrderUtxo<AnyState> {
    w.order(&l.tx, find_cov(&l.tx, &l.cov).unwrap_or(0), AnyState::KobAsk(l.state.clone()))
}

/// Fills `amount` of `l`; returns the transaction and what is left of the order.
fn fill(w: &World, node: &MockNode, l: &Live, amount: i64) -> (SignedTx, Option<Live>) {
    let order = w.order(&l.tx, find_cov(&l.tx, &l.cov).unwrap_or(0), l.state.clone());
    let leg = Leg::Ask { order, custody: custody_of(w, l), amount, t: None };
    let t = w.sign(&Action::Batch(batch(w, node.tip_daa(), vec![leg])));
    let left = l.left - amount;
    let next = (left > 0).then(|| {
        let custody = (t.clone(), find_custody(&t, &l.cov, left).expect("custody"));
        Live { tx: t.clone(), custody, state: AskState { amount_left: left, ..l.state.clone() }, cov: l.cov, left }
    });
    (t, next)
}

fn cancel(w: &World, l: &Live) -> SignedTx {
    w.sign(&Action::CancelOrder(CancelOrder {
        retired: None,
        prefund: None,
        order: order_of(w, l),
        custody: Some(custody_of(w, l)),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }))
}

/// An in-place amend of `l` to a price `bump` sompi higher; returns the transaction and the amended order.
fn amend(w: &World, l: &Live, bump: i64) -> (SignedTx, Live) {
    let amended = AskState { price: l.state.price + bump, ..l.state.clone() };
    let t = w.sign(&Action::AmendOrder(AmendOrder {
        order: order_of(w, l),
        amended: AnyState::KobAsk(amended.clone()),
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    let next = Live { tx: t.clone(), custody: l.custody.clone(), state: amended, cov: l.cov, left: l.left };
    (t, next)
}

/// `rounds` rounds of one order each: filled, cancelled, partly filled (filled to the end eight rounds later) or amended and partly
/// filled (left open), with empty blocks between. Returns the orders still live.
async fn traffic(w: &World, node: &Arc<MockNode>, stores: &[&Harness], rounds: usize, salt: i64) -> Vec<Live> {
    let mut pending: Vec<(usize, Live)> = vec![];
    let mut open: Vec<Live> = vec![];
    for r in 0..rounds {
        let l = new_ask(w, salt + r as i64);
        w.include(node, &[&l.tx]);
        match r % 4 {
            0 => {
                let (t, _) = fill(w, node, &l, 10 * WHOLE);
                w.include(node, &[&t]);
            }
            1 => {
                w.include(node, &[&cancel(w, &l)]);
            }
            2 => {
                let (t, rest) = fill(w, node, &l, 4 * WHOLE);
                w.include(node, &[&t]);
                pending.push((r, rest.unwrap()));
            }
            _ => {
                let (t, amended) = amend(w, &l, 1_000);
                w.include(node, &[&t]);
                let (t, rest) = fill(w, node, &amended, 3 * WHOLE);
                w.include(node, &[&t]);
                open.push(rest.unwrap());
            }
        }
        if let Some(i) = pending.iter().position(|(at, _)| r >= at + 8) {
            let (_, old) = pending.remove(i);
            let (t, rest) = fill(w, node, &old, old.left);
            assert!(rest.is_none());
            w.include(node, &[&t]);
        }
        node.push_empty(3);
        for h in stores {
            h.sync().await;
        }
    }
    open.extend(pending.into_iter().map(|(_, l)| l));
    open
}

#[tokio::test]
async fn a_pruned_archive_answers_the_api_as_a_store_that_keeps_everything_and_a_replay_holds_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let mut w = World::new();
    w.validate = false; // the engine is not what is under test: the indexer's rows are
    let (pf, pa) = (dir.path().join("full.sqlite3"), dir.path().join("archive.sqlite3"));
    let log_dir = dir.path().join("records");
    let (log, _) = RecordLog::open(&log_dir, 1 << 20, 0).unwrap();
    let full = harness(&node, &pf, None, None);
    // a retention that keeps every holding: only the rows no read needs go
    let arch = harness(&node, &pa, Some(u64::MAX / 8), Some(log));

    let mut checked = 0;
    let mut live = vec![];
    for round in 0..6 {
        live.extend(traffic(&w, &node, &[&full, &arch], 10, 100 * round).await);
        checked += same_answers((&full, &pf), (&arch, &pa), &node, true, &format!("after {} rounds", 10 * (round + 1))).await;
    }
    assert!(checked > 400, "{checked} answers compared");
    // the pruning deleted every final spent UTXO row of the orders closed by a final spend and the amends of final blocks, and nothing else
    // (the live orders, most of this traffic, keep theirs); no holding (the retention keeps them)
    let final_seq: i64 = arch.query("SELECT MIN(seq) FROM blocks", []);
    for t in ["order_utxos", "token_utxos", "order_amends"] {
        let (a, b) = (count(&full, t), count(&arch, t));
        assert!(b < a, "{t}: {b} rows kept of {a}");
    }
    let dead = |h: &Harness, t: &str| -> i64 {
        h.query(
            &format!(
                "SELECT COUNT(*) FROM {t} u JOIN order_state s ON s.order_id = u.order_id WHERE u.spent_block > 0 AND u.spent_block < ?1 \
                 AND s.status NOT IN ('open','partial') AND s.last_block < ?1"
            ),
            [final_seq],
        )
    };
    for t in ["order_utxos", "token_utxos"] {
        assert!(dead(&full, t) > 0);
        assert_eq!(dead(&arch, t), 0, "{t}: a final spent row of a closed order is left");
        // every other row is still there
        let others = format!(
            "SELECT COUNT(*) FROM {t} u JOIN order_state s ON s.order_id = u.order_id WHERE u.spent_block IS NULL OR u.spent_block >= ?1 \
             OR s.status IN ('open','partial') OR s.last_block >= ?1"
        );
        assert_eq!(full.query::<i64>(&others, [final_seq]), arch.query::<i64>(&others, [final_seq]), "{t}");
    }
    assert_eq!(arch.query::<i64>("SELECT COUNT(*) FROM order_amends WHERE block_seq < ?1", [final_seq]), 0);
    assert_eq!(count(&full, "token_holdings"), count(&arch, "token_holdings"));
    for t in ["orders", "order_events", "order_state", "pair_fills", "candles", "activity", "holding_programs"] {
        assert_eq!(count(&full, t), count(&arch, t), "{t}: never pruned");
    }
    let mark: i64 = arch.query("SELECT CAST(v AS INTEGER) FROM meta WHERE k = 'archive_seq'", []);
    assert_eq!(mark, final_seq, "every final block was visited");

    // a reorg inside the window undoes the cancel of the oldest live order (its earlier rows are final, its rows are never pruned)
    let oldest = live.remove(0);
    let c = cancel(&w, &oldest);
    w.include(&node, &[&c]);
    node.push_empty(2);
    full.sync().await;
    arch.sync().await;
    assert_eq!(arch.status(&oldest.cov), "cancelled");
    same_answers((&full, &pf), (&arch, &pa), &node, true, "after the cancel").await;
    node.reorg(3, vec![vec![], vec![], vec![], vec![]]);
    full.sync().await;
    arch.sync().await;
    assert_eq!(arch.status(&oldest.cov), full.status(&oldest.cov));
    assert_ne!(arch.status(&oldest.cov), "cancelled", "the revert gave the order back");
    same_answers((&full, &pf), (&arch, &pa), &node, true, "after the reorg").await;
    // the order goes on: filled to the end once its first fill is long final
    node.push_empty(2 * WINDOW as usize);
    let (t, rest) = fill(&w, &node, &oldest, oldest.left);
    assert!(rest.is_none());
    w.include(&node, &[&t]);
    node.push_empty(WINDOW as usize + 5);
    full.sync().await;
    arch.sync().await;
    assert_eq!(arch.status(&oldest.cov), "filled");
    same_answers((&full, &pf), (&arch, &pa), &node, true, "after the last fill").await;
    let left: i64 = arch.query(
        "SELECT COUNT(*) FROM order_utxos WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
        [&oldest.cov.0[..]],
    );
    assert_eq!(left, 0, "its rows went once the fill that closed it was final");

    // a rebuild from the record log (`kobd db replay`: it prunes once its frames are in, then the follower re-syncs the tail from the node
    // and prunes as it goes) holds the same rows as the store that pruned as it went, and answers the same
    let pr = dir.path().join("replay.sqlite3");
    {
        let log = kobd::indexer::recordlog::read_all(&log_dir).unwrap();
        let conn = open_writer_with(&pr, "testnet-10", false).unwrap();
        let mut ing = Ingest::new(conn, processor(), None).with_config(ingest_cfg(Some(u64::MAX / 8)));
        ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: log.records[0].1.start, daa: 0 }).unwrap();
        ing.replay(log.records).unwrap();
        ing.archive_prune_all().unwrap();
    }
    let replayed = harness(&node, &pr, Some(u64::MAX / 8), None);
    replayed.sync().await;
    // (a log keeps the chain blocks of its transactions only, so a rebuild ends with none in the window, which keeps everything until the
    // follower applies one: the chain goes on)
    node.push_empty(2);
    for h in [&full, &arch, &replayed] {
        h.sync().await;
    }
    // (the chain blocks of the log's frames are numbered apart from the ones the store applied: a final block's number is not compared)
    assert_eq!(unnumbered(&replayed.snapshot()), unnumbered(&arch.snapshot()), "a replay holds what the store pruned as it went");
    compare((&full, &pf), (&replayed, &pr), &node, true, "the replayed store", true).await;
}

/// A snapshot with the numbers of the chain blocks out of the window taken out (`MISSING-BLOCK-<seq>`: the numbering is the store's own).
fn unnumbered(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("MISSING-BLOCK-") {
        let (head, tail) = rest.split_at(i + "MISSING-BLOCK-".len());
        out.push_str(head);
        rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

#[tokio::test]
async fn spent_holdings_past_the_retention_go_and_the_token_keeps_its_program_and_a_live_order_its_custody() {
    const RETENTION: u64 = 120;
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let mut w = World::new();
    w.validate = false;
    let (pf, pa) = (dir.path().join("full.sqlite3"), dir.path().join("archive.sqlite3"));
    let full = harness(&node, &pf, None, None);
    let arch = harness(&node, &pa, Some(RETENTION), None);
    let live = traffic(&w, &node, &[&full, &arch], 40, 0).await;
    node.push_empty(10);
    full.sync().await;
    arch.sync().await;

    // the answers that do not list spent holdings are the same
    same_answers((&full, &pf), (&arch, &pa), &node, false, "short retention").await;
    // the spent holdings: those of the retention, the window and the live orders' custody stay, every other one went
    // (txid, created DAA, spent block, role, owner)
    type Spent = (Vec<u8>, i64, Option<i64>, String, Vec<u8>);
    let spent = |h: &Harness| -> Vec<Spent> {
        let g = h.ingest.lock().unwrap();
        let mut st = g
            .conn()
            .prepare(
                "SELECT x.txid, x.created_daa, x.spent_block, x.role, w.h FROM token_holdings x JOIN ids w ON w.id = x.owner_id \
                 WHERE x.spent_block IS NOT NULL ORDER BY x.txid, x.idx",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    let (all, kept) = (spent(&full), spent(&arch));
    let final_seq: i64 = arch.query("SELECT MIN(seq) FROM blocks", []);
    let before = node.tip_daa().saturating_sub(RETENTION) as i64;
    let live_covs: Vec<Vec<u8>> = live.iter().map(|l| l.cov.0.to_vec()).collect();
    let expected: Vec<_> = all
        .iter()
        .filter(|(_, daa, block, role, owner)| {
            *daa >= before || block.unwrap() >= final_seq || (role == "custody" && live_covs.contains(owner))
        })
        .cloned()
        .collect();
    assert!(expected.len() < all.len() && !expected.is_empty(), "{} of {} spent holdings in the retention", expected.len(), all.len());
    assert_eq!(kept, expected);
    let live_custody = kept.iter().filter(|k| k.3 == "custody" && live_covs.contains(&k.4) && k.1 < before).count();
    assert!(live_custody > 0, "a live order's spent custody older than the retention stays");
    // the API lists exactly those
    let t = Hash32(TOKEN_COV).to_hex();
    let api = router(&pa, node.tip_daa());
    let listed = get(&api, &format!("/v1/token-utxos?token={t}&spent=true&limit=1000")).await;
    let listed_spent = listed["items"].as_array().unwrap().iter().filter(|i| i["spent"] == Value::Bool(true)).count();
    assert_eq!(listed_spent, kept.len());
    // the unspent holdings are all there, and the token keeps the program they proved
    assert_eq!(count(&full, "token_holdings") - all.len() as i64, count(&arch, "token_holdings") - kept.len() as i64);
    let programs = |h: &Harness| -> Vec<(i64, String)> {
        let g = h.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT token_id, program FROM holding_programs ORDER BY 1, 2").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(programs(&full), programs(&arch));
    assert!(!programs(&arch).is_empty());
}

/// Measurement on a copy of a real store (`KOB_ARCHIVE_DB=<a copy of an index.sqlite3 of schema 9 or later, never a live store>`,
/// `-- --ignored --nocapture`): `kobd db migrate` when it is of an earlier schema, then the archive pruning at its defaults until it finds
/// nothing more (the time of each batch, and what it deleted), then `kobd db vacuum`.
#[test]
#[ignore]
fn measure_the_archive_pruning_on_a_copy() {
    let Some(path) = std::env::var_os("KOB_ARCHIVE_DB") else { return };
    let path = std::path::PathBuf::from(path);
    let mut icfg = kobd::config::IndexerConfig {
        network: "testnet-10".into(),
        data_dir: path.parent().unwrap().to_path_buf(),
        ..Default::default()
    };
    icfg.records.enabled = false;
    assert_eq!(icfg.db_path(), path, "the copy is named index.sqlite3");
    let t = std::time::Instant::now();
    let m = kobd::indexer::storage::migrate(&icfg, kobd::indexer::storage::Options::default(), |l| println!("migrate: {l}")).unwrap();
    println!("migrate from schema {}: {:.1} s", m.from_schema, t.elapsed().as_secs_f64());
    if let Some(old) = m.kept_old {
        std::fs::remove_file(old).unwrap();
    }
    let conn = open_writer_with(&path, "testnet-10", false).unwrap();
    let t = std::time::Instant::now();
    kobd::indexer::archive::ensure_indexes(&conn).unwrap();
    println!("index: {:.1} s", t.elapsed().as_secs_f64());
    let rows = |c: &rusqlite::Connection| -> Vec<(String, i64)> {
        ["order_utxos", "token_utxos", "order_amends", "token_holdings"]
            .iter()
            .map(|t| (t.to_string(), c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()))
            .collect()
    };
    println!("before: {:?}", rows(&conn));
    let cfg = IngestConfig { archive: Some(ArchiveConfig::new(30 * 86_400 * 10)), ..IngestConfig::default() };
    let mut ing = Ingest::new(conn, processor(), None).with_config(cfg);
    let (mut times, mut deleted) = (vec![], 0);
    let all = std::time::Instant::now();
    loop {
        let t = std::time::Instant::now();
        let r = ing.archive_prune(None).unwrap().expect("a store that prunes");
        times.push(t.elapsed());
        deleted += r.rows;
        if !r.more {
            break;
        }
    }
    times.sort();
    let pct = |p: usize| times[(times.len() - 1) * p / 100];
    println!(
        "{} batches, {deleted} rows, {:.1} s of work; batch median {:?}, p99 {:?}, longest {:?}",
        times.len(),
        all.elapsed().as_secs_f64(),
        pct(50),
        pct(99),
        pct(100)
    );
    println!("after: {:?}", rows(ing.conn()));
    drop(ing);
    let v = kobd::indexer::storage::vacuum(&icfg, |l| println!("vacuum: {l}")).unwrap();
    println!("vacuum: {v:?}");
}

/// A big store: `n` closed orders, each with two spent UTXOs, a spent custody token UTXO and two spent holdings, all final (spent in
/// blocks below the window), by set-based SQL. A batch at the defaults takes a bounded time whatever the size, and the batches delete it
/// all; the orders and their states stay. (The history was written as by a store that did not prune: the pruning starts from its first
/// block, `archive_seq` unset.)
#[tokio::test]
async fn a_batch_on_a_big_store_takes_a_bounded_time() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    node.push_empty(4 * WINDOW as usize);
    let h = Harness::with_node(
        node.clone(),
        open_writer_with(&dir.path().join("big.sqlite3"), "testnet-10", false).unwrap(),
        None,
        IngestConfig { reorg_window_daa: WINDOW, archive: Some(ArchiveConfig::new(WINDOW)), ..IngestConfig::default() },
    );
    h.sync().await;
    let final_seq: i64 = h.query("SELECT MIN(seq) FROM blocks", []);
    assert!(final_seq > 50, "{final_seq}");
    let n = 30_000i64;
    h.ingest
        .lock()
        .unwrap()
        .conn()
        .execute_batch(&format!(
            "INSERT OR IGNORE INTO ids (h) VALUES (zeroblob(32)); \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {n}) \
             INSERT INTO orders (covenant_id, contract, template_id, family, side, in_book, genesis_state, genesis_txid, genesis_out, genesis_block, genesis_daa, listed, origin, genesis_value) \
                 SELECT CAST(printf('c%031d', i) AS BLOB), 'KobAsk', (SELECT id FROM ids WHERE h = zeroblob(32)), 1, 1, 1, zeroblob(8), CAST(printf('u%031d', i) AS BLOB), 0, 1, 0, 0, 'chain', 7 FROM n; \
             INSERT INTO ids (h) SELECT covenant_id FROM orders WHERE covenant_id != zeroblob(32); \
             INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, state_known, last_block, last_daa) \
                 SELECT id, 'filled', 0, 0, 1, 1, 1 + id % ({final_seq} - 1), id FROM orders; \
             INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa, spent_block) \
                 SELECT CAST(printf('u%031d', id) AS BLOB), k, id, 7, zeroblob(4), 1, 0, 1 + id % ({final_seq} - 1) FROM orders, (SELECT 0 AS k UNION SELECT 1); \
             INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa, spent_block) \
                 SELECT CAST(printf('u%031d', id) AS BLOB), 2, 1, id, 5, 3, 'custody', 1, 0, 1 + id % ({final_seq} - 1) FROM orders; \
             INSERT INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa, spent_block) \
                 SELECT CAST(printf('u%031d', o.id) AS BLOB), 2 + k, 1, 'p', 1, i.id, 4, 5, 1, x'08', 'custody', 1, 0, 1 + o.id % ({final_seq} - 1) \
                 FROM orders o JOIN ids i ON i.h = o.covenant_id, (SELECT 0 AS k UNION SELECT 1); \
             DELETE FROM meta WHERE k = 'archive_seq';"
        ))
        .unwrap();
    let (mut times, mut batches, mut rows) = (vec![], 0, 0);
    loop {
        let t = std::time::Instant::now();
        let r = h.ingest.lock().unwrap().archive_prune(None).unwrap().unwrap();
        times.push(t.elapsed());
        batches += 1;
        rows += r.rows;
        assert!(r.rows > 0 || !r.more, "every batch makes progress: {r:?}");
        if !r.more {
            break;
        }
        assert!(batches < 10_000, "the pruning never ended");
    }
    times.sort();
    let (median, worst) = (times[times.len() / 2], times[times.len() - 1]);
    println!("{batches} batches, {rows} rows; median {median:?}, longest {worst:?}");
    // a batch is steps of a stretch of blocks and a few hundred rows that stop after 12 ms of work (a commit on a slow disk of a CI runner
    // can take a second once in a while: the median is what is bounded closely)
    assert!(median < std::time::Duration::from_millis(300), "the median batch took {median:?}");
    assert!(worst < std::time::Duration::from_secs(10), "a batch took {worst:?}");
    let left: Vec<i64> = ["order_utxos", "token_utxos", "token_holdings"].iter().map(|t| count(&h, t)).collect();
    assert_eq!(rows as i64, 5 * n, "left: {left:?}");
    for t in ["order_utxos", "token_utxos", "token_holdings"] {
        assert_eq!(count(&h, t), 0, "{t}");
    }
    assert_eq!(count(&h, "orders"), n);
    assert_eq!(count(&h, "order_state"), n);
}
