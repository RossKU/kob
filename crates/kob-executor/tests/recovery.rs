//! Restart, record-log replay and the recovery hooks (maker export/import, gap reconciliation).

mod common;

use common::*;
use kob_executor::config::{IndexerConfig, StartMode};
use kob_executor::hex::{Hash32, HexBytes};
use kob_executor::indexer::db::{open_memory, open_writer};
use kob_executor::indexer::ingest::Cursor;
use kob_executor::indexer::recordlog::{self, LogBatch, LogCursor, RecordLog};
use kob_executor::indexer::{replay_from_log, Indexer};
use kob_executor::recover::{export_orders, import_orders, reconcile_open_orders};
use kob_executor::rpc::types::{AddressUtxo, AddressUtxoEntry, Outpoint};
use kob_executor::script::{spk_address, spk_bytes};
use kob_executor::testkit::*;
use kob_protocol::artifacts::template;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use std::sync::Arc;

fn test_config(dir: &std::path::Path, node: &MockNode) -> IndexerConfig {
    let tokens_path = dir.join("tokens.json");
    std::fs::write(&tokens_path, format!(r#"[{{"ticker":"TST","covenantId":"{}"}}]"#, Hash32(TOKEN_COV))).unwrap();
    let mut cfg = IndexerConfig {
        data_dir: dir.join("data"),
        tokens_path: Some(tokens_path),
        start: StartMode::Hash(node.anchor()),
        ..Default::default()
    };
    cfg.rules.min_order_value_sompi = 1;
    cfg.rules.max_expiry_span_daa = 1 << 40;
    cfg.poll_interval_ms = 1;
    cfg.backoff_ms = 1;
    cfg.max_backoff_ms = 5;
    cfg.api.enabled = false;
    cfg
}

async fn sync<S: kob_executor::rpc::ChainSource>(f: &kob_executor::indexer::follower::Follower<S>) {
    for _ in 0..200 {
        if matches!(f.step().await, kob_executor::indexer::follower::StepOutcome::Idle) {
            return;
        }
    }
    panic!("did not settle");
}

fn snap(idx: &Indexer) -> String {
    snapshot(idx.ingest.lock().unwrap().conn())
}

fn order_status(idx: &Indexer, cov: &Hash32) -> String {
    idx.ingest
        .lock()
        .unwrap()
        .conn()
        .query_row("SELECT status FROM order_state WHERE covenant_id = ?1", [&cov.0[..]], |r| r.get(0))
        .unwrap()
}

/// Create an ask of 10 whole tokens, fill 4 of them, in two blocks pushed to `node`.
fn ask_and_fill(w: &World, node: &MockNode, price: i64) -> (SignedTx, SignedTx, AskState) {
    let a = ask(MAKER_A, price);
    let create = w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    w.include(node, &[&create]);
    let cov = w.cov(&create, 0);
    let custody = w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = w.sign(&Action::Batch(batch(w, node.tip_daa(), vec![leg])));
    w.include(node, &[&fill]);
    (create, fill, a)
}

#[tokio::test]
async fn restart_continues_from_the_stored_cursor_and_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let cfg = test_config(dir.path(), &node);
    let w = World::new();
    let a = ask(MAKER_A, P250);
    let create = w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    w.include(&node, &[&create]);
    let cov = w.cov(&create, 0);
    let before = {
        let idx = Indexer::open(cfg.clone()).unwrap();
        let f = idx.follower(node.clone());
        sync(&f).await;
        assert_eq!(idx.health.snapshot().cursor_hash, Some(node.tip()));
        snap(&idx)
    };
    // the process restarts: state and cursor come from disk; a fill of the pre-restart order is understood
    let custody = w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: w.order(&create, 0, a), custody, amount: 3 * WHOLE, t: None };
    let fill = w.sign(&Action::Batch(batch(&w, node.tip_daa(), vec![leg])));
    w.include(&node, &[&fill]);
    let idx = Indexer::open(cfg).unwrap();
    assert_eq!(idx.health.snapshot().cursor_hash.unwrap(), *node.chain_hashes().get(1).unwrap());
    assert_eq!(snap(&idx), before);
    let f = idx.follower(node.clone());
    let calls = node.vspc_calls();
    sync(&f).await;
    assert!(node.vspc_calls() > calls);
    assert_eq!(order_status(&idx, &cov), "partial");
    assert_eq!(idx.health.snapshot().cursor_hash, Some(node.tip()));
}

#[tokio::test]
async fn record_log_replay_rebuilds_an_identical_database_and_holds_no_raw_data() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let cfg = test_config(dir.path(), &node);
    let w = World::new();
    let (c1, f1, _) = ask_and_fill(&w, &node, P250);
    let b = bid(MAKER_B, P245);
    let create_b = w.create_tx(AnyState::KobBid(b.clone()), b.escrow(6 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    w.include(&node, &[&create_b]);
    node.push_block(vec![noise_tx(1, true)]);
    let idx = Indexer::open(cfg.clone()).unwrap();
    let f = idx.follower(node.clone());
    sync(&f).await;
    // reorg: the bid's block and the ask's fill disappear; the bid is cancelled instead
    let cov_b = w.cov(&create_b, 0);
    let cancel = w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order: w.order(&create_b, 0, AnyState::KobBid(b)),
        custody: None,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![w.coin(MAKER_B, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    let daa = node.tip_daa();
    w.note_daa(&[&create_b], daa - 2);
    node.reorg(3, vec![vec![wire_tx(&create_b.tx)], vec![wire_tx(&cancel.tx)], vec![]]);
    sync(&f).await;
    let live = snap(&idx);
    assert!(live.contains("cancelled"));
    assert_eq!(order_status(&idx, &cov_b), "cancelled");
    let cov_a = w.cov(&c1, 0);
    assert_eq!(order_status(&idx, &cov_a), "open", "the fill was reorged out");
    drop(f);
    let log_dir = idx.cfg.records_dir();
    let records = recordlog::read_all(&log_dir).unwrap().records;
    assert!(records.len() >= 2);
    assert!(records.iter().any(|(_, b)| !b.removed.is_empty()), "the reorg is in the log");
    let _ = f1;

    // nothing raw is kept: no token program, no template code, no signature script bytes
    let mut all = Vec::new();
    for p in recordlog::segment_paths(&log_dir).unwrap() {
        all.extend(std::fs::read(p).unwrap());
    }
    let contains = |needle: &[u8]| all.windows(needle.len()).any(|w| w == needle);
    assert!(!contains(&template(T3).suffix[..64]), "the token program must not be stored");
    assert!(!contains(&template(kob_protocol::artifacts::TemplateId::KobAsk).suffix[..64]), "template code must not be stored");
    drop(idx);

    // rebuild elsewhere from the log alone
    let mut cfg2 = cfg.clone();
    cfg2.data_dir = dir.path().join("rebuilt");
    cfg2.records.dir = Some(log_dir.clone());
    let rep = replay_from_log(&cfg2).unwrap();
    let (n, orders) = (rep.frames, rep.orders);
    assert_eq!(n as usize, records.len());
    assert_eq!(orders, 2);
    assert_eq!((rep.skipped.len(), rep.dropped.dropped()), (0, 0));
    let conn = open_writer(&cfg2.db_path(), "testnet-10").unwrap();
    assert_eq!(snapshot(&conn), live);

    // replay refuses to overwrite an existing database
    assert!(replay_from_log(&cfg2).is_err());
    drop(conn);
    // and the rebuilt database goes live on the same log: it resumes from the logged cursor and re-syncs the
    // tail from the node
    let idx2 = Indexer::open(cfg2).unwrap();
    let f2 = idx2.follower(node.clone());
    node.push_block(vec![]);
    sync(&f2).await;
    assert_eq!(idx2.health.snapshot().cursor_hash, Some(node.tip()));
}

#[tokio::test]
async fn open_refuses_empty_database_next_to_a_populated_log_and_repairs_a_crash_tail() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let cfg = test_config(dir.path(), &node);
    let w = World::new();
    let create = w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    w.include(&node, &[&create]);
    {
        let idx = Indexer::open(cfg.clone()).unwrap();
        sync(&idx.follower(node.clone())).await;
    }
    // simulate a crash after the log append but before the database commit: one extra frame
    {
        let conn = open_writer(&cfg.db_path(), "testnet-10").unwrap();
        let next = kob_executor::indexer::ingest::records_next_n(&conn).unwrap();
        assert_eq!(next, 1);
        drop(conn);
        let (mut log, _) = RecordLog::open(&cfg.records_dir(), 1 << 20, next).unwrap();
        log.append(
            &LogBatch {
                start: node.anchor(),
                removed: vec![],
                cursor: LogCursor { hash: node.tip(), daa: 5 },
                blocks: vec![],
                ops: vec![],
            },
            0,
        )
        .unwrap();
        assert_eq!(log.next_n(), 2);
    }
    let idx = Indexer::open(cfg.clone()).unwrap();
    assert_eq!(idx.ingest.lock().unwrap().records_next(), Some(1), "the uncommitted frame was cut off");
    drop(idx);
    // database deleted, log kept: `open` must not start over next to the log
    std::fs::remove_file(cfg.db_path()).unwrap();
    for ext in ["-wal", "-shm"] {
        let mut p = cfg.db_path().into_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
    let err = Indexer::open(cfg).err().expect("must refuse");
    assert!(err.to_string().contains("replay"), "{err}");
}

/// A database restored from an older backup next to the live record log: `open` refuses (the frames the backup lacks are
/// more than one unclean shutdown leaves) and keeps every frame; `replay --onto-database` applies them to the backup,
/// which then matches the live database and opens.
#[tokio::test]
async fn a_database_older_than_the_record_log_is_refused_and_brought_forward_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let cfg = test_config(dir.path(), &node);
    let w = World::new();
    let backup = dir.path().join("backup.sqlite3");
    let live = {
        let idx = Indexer::open(cfg.clone()).unwrap();
        let f = idx.follower(node.clone());
        let create = w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
        w.include(&node, &[&create]);
        sync(&f).await;
        idx.ingest.lock().unwrap().conn().execute("VACUUM INTO ?1", [backup.to_str().unwrap()]).unwrap();
        // later frames the backup does not hold, one batch each
        ask_and_fill(&w, &node, P245);
        sync(&f).await;
        let b = bid(MAKER_B, P245);
        let create_b = w.create_tx(AnyState::KobBid(b.clone()), b.escrow(6 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
        w.include(&node, &[&create_b]);
        sync(&f).await;
        snap(&idx)
    };
    let frames = recordlog::record_count(&cfg.records_dir()).unwrap();
    let backup_next = kob_executor::indexer::ingest::records_next_n(&open_writer(&backup, "testnet-10").unwrap()).unwrap();
    assert!(frames >= backup_next + 2, "{frames} frames, backup at {backup_next}");
    // restore the backup over the live database
    for ext in ["-wal", "-shm"] {
        let mut p = cfg.db_path().into_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
    std::fs::copy(&backup, cfg.db_path()).unwrap();
    let err = Indexer::open(cfg.clone()).err().expect("must refuse");
    assert!(err.to_string().contains("replay --onto-database"), "{err}");
    assert_eq!(recordlog::record_count(&cfg.records_dir()).unwrap(), frames, "no frame was removed");
    // the explicit recovery
    let rep = kob_executor::indexer::replay_onto_database(&cfg).unwrap();
    assert_eq!((rep.from, rep.replay.frames), (backup_next, frames));
    assert_eq!(recordlog::record_count(&cfg.records_dir()).unwrap(), frames);
    let idx = Indexer::open(cfg.clone()).unwrap();
    assert_eq!(idx.ingest.lock().unwrap().records_next(), Some(frames));
    assert_eq!(snap(&idx), live);
    drop(idx);
    // a second run has nothing left to apply
    let rep = kob_executor::indexer::replay_onto_database(&cfg).unwrap();
    assert_eq!((rep.from, rep.replay.frames, rep.replay.relevant), (frames, frames, 0));
}

/// An unspent output as the node's `getUtxosByAddresses` reports it.
fn node_utxo(spk: &[u8], cov: Hash32, txid: Hash32, idx: u32, amount: u64) -> (String, AddressUtxo) {
    (
        spk_address(spk, "testnet-10").unwrap(),
        AddressUtxo {
            outpoint: Outpoint { transaction_id: txid, index: idx },
            utxo_entry: AddressUtxoEntry {
                amount,
                script_public_key: HexBytes(spk.to_vec()),
                block_daa_score: 1_050,
                covenant_id: Some(cov),
            },
        },
    )
}

#[tokio::test]
async fn maker_export_import_is_verified_against_the_node_utxo_set() {
    // A healthy indexer that knows a partially filled ask
    let hs_a = Harness::new(open_memory("testnet-10").unwrap(), None);
    let w = World::new();
    let (create, fill, a) = ask_and_fill(&w, &hs_a.node, 300_000_000);
    hs_a.sync().await;
    let cov = w.cov(&create, 0);
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    let export = export_orders(hs_a.ingest.lock().unwrap().conn(), Some(&a.maker), true).unwrap();
    assert_eq!(export.orders.len(), 1);
    assert_eq!(export.orders[0].covenant_id, Some(cov));
    assert_eq!(export.orders[0].extension_commitment, Some(Hash32(EXT)), "the placement custody's extension commitment");
    assert_eq!(export.orders[0].state.0, AnyState::KobAsk(a6.clone()).encode(), "the CURRENT script's state");
    let json = serde_json::to_string(&export).unwrap();
    let export: kob_executor::recover::MakerExport = serde_json::from_str(&json).unwrap();

    // The node's UTXO set: the order's continuation and its custody token UTXO
    let oi = find_cov(&fill, &cov).unwrap();
    let ci = find_custody(&fill, &cov, 6 * WHOLE).unwrap();
    let order_spk = spk_bytes(&AnyState::KobAsk(a6.clone()).spk());
    let custody_spk = spk_bytes(&Kcc20State::custody(6 * WHOLE, cov.0, EXT).spk_with(template(T3)));
    let utxos = vec![
        node_utxo(&order_spk, cov, Hash32(fill.tx.id), oi as u32, fill.tx.outputs[oi].value),
        node_utxo(&custody_spk, Hash32(TOKEN_COV), Hash32(fill.tx.id), ci as u32, fill.tx.outputs[ci].value),
    ];

    // A new indexer that lost the history: fresh database, a node that only has the UTXO set
    let hs_b = Harness::new(open_memory("testnet-10").unwrap(), None);
    hs_b.sync().await; // initialises the cursor at the node's anchor
    hs_b.node.set_utxos(utxos.clone());
    let rep = import_orders(&hs_b.ingest, hs_b.node.as_ref(), "testnet-10", &export).await.unwrap();
    assert_eq!((rep.imported, rep.already_known), (1, 0), "{rep:?}");
    assert!(rep.custody_unverified.is_empty(), "{rep:?}");
    assert_eq!(hs_b.status(&cov), "open");
    assert_eq!(hs_b.query::<String>("SELECT origin FROM orders", []), "import");
    assert_eq!(hs_b.query::<i64>("SELECT remaining_amount FROM order_state", []), 6 * WHOLE);
    assert_eq!(hs_b.query::<i64>("SELECT price FROM orders", []), 300_000_000);
    assert_eq!(hs_b.tip_state(&cov), Some(AnyState::KobAsk(a6.clone())));
    let cv = {
        let g = hs_b.ingest.lock().unwrap();
        let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: None, settle_depth_daa: 100, now_unix: None };
        kob_executor::indexer::reads::order(g.conn(), &ctx, &cov).unwrap().unwrap().custody.unwrap()
    };
    assert!(cv.ok, "the custody UTXO was verified against the node: {cv:?}");
    // importing again is idempotent
    let rep = import_orders(&hs_b.ingest, hs_b.node.as_ref(), "testnet-10", &export).await.unwrap();
    assert_eq!((rep.imported, rep.already_known), (0, 1));
    // the imported lineage is live: a later cancel on chain closes it
    let custody = w.token_at(&fill, ci, Kcc20State::custody(6 * WHOLE, cov.0, EXT));
    let cancel = w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order: w.order(&fill, oi, AnyState::KobAsk(a6.clone())),
        custody: Some(custody),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    hs_b.node.push_block(vec![wire_tx(&cancel.tx)]);
    hs_b.sync().await;
    assert_eq!(hs_b.status(&cov), "cancelled");

    // an export without the extension commitment imports with unverified custody (and is not liquidity)
    let mut no_ext = export.clone();
    no_ext.orders[0].extension_commitment = None;
    let hs_d = Harness::new(open_memory("testnet-10").unwrap(), None);
    hs_d.sync().await;
    hs_d.node.set_utxos(utxos.clone());
    let rep = import_orders(&hs_d.ingest, hs_d.node.as_ref(), "testnet-10", &no_ext).await.unwrap();
    assert_eq!(rep.imported, 1);
    assert_eq!(rep.custody_unverified, vec![cov.to_hex()]);
    {
        let g = hs_d.ingest.lock().unwrap();
        let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: None, settle_depth_daa: 100, now_unix: None };
        let bk = kob_executor::indexer::reads::book(g.conn(), &ctx, &Hash32(TOKEN_COV), 10, true).unwrap();
        let kob_executor::indexer::reads::BookSide::Levels(asks) = bk.asks else { panic!() };
        assert!(asks.is_empty(), "an ask without verified custody is not listed");
    }

    // forged parameters (a better price) rebuild a different script that the node does not have
    let mut forged = export.clone();
    let mut s = AskState { price: 1, ..a6.clone() };
    s.amount_left = 6 * WHOLE;
    forged.orders[0].state = HexBytes(AnyState::KobAsk(s).encode());
    let hs_c = Harness::new(open_memory("testnet-10").unwrap(), None);
    hs_c.sync().await;
    hs_c.node.set_utxos(utxos);
    let rep = import_orders(&hs_c.ingest, hs_c.node.as_ref(), "testnet-10", &forged).await.unwrap();
    assert_eq!(rep.imported, 0);
    assert_eq!(rep.not_found.len(), 1);
    assert_eq!(hs_c.query::<i64>("SELECT COUNT(*) FROM orders", []), 0);
    // wrong network is refused outright; so is an unknown template
    let mut wrong = export.clone();
    wrong.network = "mainnet".into();
    assert!(import_orders(&hs_c.ingest, hs_c.node.as_ref(), "testnet-10", &wrong).await.is_err());
    let mut unknown = export;
    unknown.orders[0].template_hash = Hash32([1; 32]);
    let rep = import_orders(&hs_c.ingest, hs_c.node.as_ref(), "testnet-10", &unknown).await.unwrap();
    assert_eq!(rep.rejected.len(), 1);
}

#[tokio::test]
async fn reconcile_closes_spent_outputs_and_adopts_continuations() {
    let hs = Harness::new(open_memory("testnet-10").unwrap(), None);
    let w = World::new();
    let creates: Vec<SignedTx> = (1..=3)
        .map(|i| {
            let a = AskState { amount_left: 5 * WHOLE, ..ask(MAKER_A, P250 + i) };
            w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 5 * WHOLE)
        })
        .collect();
    w.include(&hs.node, &creates.iter().collect::<Vec<_>>());
    hs.sync().await;
    let covs: Vec<Hash32> = creates.iter().map(|c| w.cov(c, 0)).collect();
    let spks: Vec<Vec<u8>> =
        (1..=3).map(|i| spk_bytes(&AnyState::KobAsk(AskState { amount_left: 5 * WHOLE, ..ask(MAKER_A, P250 + i) }).spk())).collect();
    // During the downtime: order 1 is still there, order 2 was spent (gone), order 3 was partially
    // filled and continues as a new output under the same script
    let cont3 = (h("later-tx", 3), 1u32);
    hs.node.set_utxos(vec![
        node_utxo(&spks[0], covs[0], Hash32(creates[0].tx.id), 0, creates[0].tx.outputs[0].value),
        node_utxo(&spks[2], covs[2], cont3.0, cont3.1, 7_000_000),
    ]);
    let cursor = Cursor { hash: hs.node.tip(), daa: 1_100 };
    let rep = reconcile_open_orders(&hs.ingest, hs.node.as_ref(), "testnet-10", cursor).await.unwrap();
    assert_eq!((rep.closed, rep.adopted), (1, 1), "{rep:?}");
    assert_eq!(hs.status(&covs[0]), "open");
    assert_eq!(hs.status(&covs[1]), "closed");
    assert_eq!(hs.status(&covs[2]), "open");
    let cur: Vec<u8> = hs.query("SELECT cur_txid FROM order_state WHERE covenant_id = ?1", [&covs[2].0[..]]);
    assert_eq!(cur, cont3.0 .0.to_vec());
    assert_eq!(hs.query::<i64>("SELECT cur_value FROM order_state WHERE covenant_id = ?1", [&covs[2].0[..]]), 7_000_000);
    // custody of a gap-closed or gap-adopted order is unknown until its next spend: never counted
    let live: i64 = hs.query(
        "SELECT COUNT(*) FROM token_utxos WHERE owner IN (?1, ?2) AND spent_block IS NULL",
        rusqlite::params![&covs[1].0[..], &covs[2].0[..]],
    );
    assert_eq!(live, 0);
    let live: i64 = hs.query("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&covs[0].0[..]]);
    assert_eq!(live, 1, "an untouched order keeps its custody");
    let _ = Arc::new(());
}

#[tokio::test]
async fn kron_maker_export_import_is_verified_against_the_node_utxo_set() {
    // the same recovery for a KRON ask: the custody is a 46-byte KRON token (`id_type` 2, no extension)
    let hs_a = Harness::new(open_memory("testnet-10").unwrap(), None);
    let w = World::new();
    let a = match kron(AnyState::KobAsk(ask(MAKER_A, 300_000_000))) {
        AnyState::KobAskKron(x) => x,
        _ => unreachable!(),
    };
    let create = w.create_tx(AnyState::KobAskKron(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    w.include(&hs_a.node, &[&create]);
    let cov = w.cov(&create, 0);
    let cust = |n: i64| TokenState::custody(kob_protocol::family::Family::Kron, n, cov.0, EXT);
    let leg = Leg::Ask {
        order: w.order(&create, 0, a.clone()),
        custody: w.token_at(&create, 1, cust(10 * WHOLE)),
        amount: 4 * WHOLE,
        t: None,
    };
    let fill = w.sign(&Action::Batch(batch(&w, hs_a.node.tip_daa(), vec![leg])));
    w.include(&hs_a.node, &[&fill]);
    hs_a.sync().await;
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    let export = export_orders(hs_a.ingest.lock().unwrap().conn(), Some(&a.maker), true).unwrap();
    assert_eq!(export.orders.len(), 1);
    assert_eq!(export.orders[0].state.0, AnyState::KobAskKron(a6.clone()).encode(), "the CURRENT script's state");
    let export: kob_executor::recover::MakerExport = serde_json::from_str(&serde_json::to_string(&export).unwrap()).unwrap();

    let oi = find_cov(&fill, &cov).unwrap();
    let ci = find_custody_for(kob_protocol::family::Family::Kron, &fill, &cov, 6 * WHOLE).unwrap();
    let order_spk = spk_bytes(&AnyState::KobAskKron(a6.clone()).spk());
    let custody_spk = spk_bytes(&cust(6 * WHOLE).spk_with(kob_protocol::artifacts::token_template(K3)));
    let utxos = vec![
        node_utxo(&order_spk, cov, Hash32(fill.tx.id), oi as u32, fill.tx.outputs[oi].value),
        node_utxo(&custody_spk, Hash32(TOKEN_COV_KRON), Hash32(fill.tx.id), ci as u32, fill.tx.outputs[ci].value),
    ];
    let hs_b = Harness::new(open_memory("testnet-10").unwrap(), None);
    hs_b.sync().await;
    hs_b.node.set_utxos(utxos);
    let rep = import_orders(&hs_b.ingest, hs_b.node.as_ref(), "testnet-10", &export).await.unwrap();
    assert_eq!((rep.imported, rep.already_known), (1, 0), "{rep:?}");
    assert!(rep.custody_unverified.is_empty(), "{rep:?}");
    assert_eq!(hs_b.tip_state(&cov), Some(AnyState::KobAskKron(a6)));
    let cv = {
        let g = hs_b.ingest.lock().unwrap();
        let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: None, settle_depth_daa: 100, now_unix: None };
        kob_executor::indexer::reads::order(g.conn(), &ctx, &cov).unwrap().unwrap().custody.unwrap()
    };
    assert!(cv.ok, "the KRON custody UTXO was verified against the node: {cv:?}");
}

/// TN10 soak 10-01: `index import-orders` run next to a running `run` interleaved two writers of one record log and broke
/// its hash chain. Every writer of a data directory now holds an exclusive OS lock; readers do not need it.
#[tokio::test]
async fn a_data_directory_has_one_writer_and_readers_work_next_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let cfg = test_config(dir.path(), &node);
    let w = World::new();
    let (create, _, _) = ask_and_fill(&w, &node, P250);
    let idx = Indexer::open(cfg.clone()).unwrap();
    sync(&idx.follower(node.clone())).await;
    let frames = recordlog::record_count(&cfg.records_dir()).unwrap();
    assert!(frames > 0);

    // a second writer (import-orders, rebase, run, index) is refused with a clear message and writes nothing
    let err = Indexer::open(cfg.clone()).err().expect("a second writer must be refused").to_string();
    assert!(err.contains("in use by another kob-executor writer") && err.contains("export-orders"), "{err}");
    assert!(err.contains(&format!("pid {}", std::process::id())), "the holder is named: {err}");
    // replay builds a new database into the same directory: also a writer
    let mut cfg_replay = cfg.clone();
    cfg_replay.records.dir = Some(cfg.records_dir());
    assert!(replay_from_log(&cfg_replay).unwrap_err().to_string().contains("in use"));
    // a separate record-log directory is locked too
    let mut other = cfg.clone();
    other.data_dir = dir.path().join("other-data");
    other.records.dir = Some(cfg.records_dir());
    assert!(Indexer::open(other).err().expect("the record log is locked").to_string().contains("in use"));

    // read-only tools work next to the writer
    let conn = kob_executor::indexer::db::open_reader(&cfg.db_path(), &cfg.network).unwrap();
    let export = export_orders(&conn, None, false).unwrap();
    assert!(serde_json::to_string(&export).unwrap().contains(&w.cov(&create, 0).to_hex()));
    assert!(kob_executor::indexer::db::open_reader(&cfg.db_path(), "mainnet").is_err());
    drop(conn);
    // the log was not touched by the refused writers
    assert_eq!(recordlog::record_count(&cfg.records_dir()).unwrap(), frames);
    let chained = recordlog::read_all(&cfg.records_dir()).unwrap();
    assert!(!chained.torn && chained.frames == frames);

    // the lock goes with the process (here: the indexer); the next writer opens the directory
    drop(idx);
    let idx = Indexer::open(cfg).unwrap();
    assert_eq!(order_status(&idx, &w.cov(&create, 0)), "partial");
}
