//! Storage policy: no raw node responses, only compact extracted KOB records
//! forever and chain-block rows for the reorg window. These tests measure what that costs and keep it
//! from regressing; `cargo test -p kob-executor --test storage -- --nocapture` prints the numbers that
//! `docs/ops/executor.md` quotes.

mod common;

use common::*;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::recordlog::{self, RecordLog};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// Sum of the signature-script bytes of a transaction (what the node ships and the indexer drops).
fn sigscript_bytes(t: &SignedTx) -> usize {
    t.tx.inputs.iter().map(|i| i.signature_script.len()).sum()
}

fn wire_json_bytes(t: &SignedTx) -> usize {
    serde_json::to_string(&wire_tx(&t.tx)).unwrap().len()
}

fn log_bytes(dir: &std::path::Path) -> u64 {
    recordlog::log_bytes(dir).unwrap()
}

struct Lifecycle {
    name: &'static str,
    txs: usize,
    log: u64,
    raw_json: usize,
    sigscripts: usize,
}

#[tokio::test]
async fn compact_records_are_a_small_fraction_of_the_raw_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("records");
    let (log, _) = RecordLog::open(&log_dir, 1 << 26, 0).unwrap();
    let hs = Harness::with_node(MockNode::new("testnet-10"), open_memory("testnet-10").unwrap(), Some(log), wide_window());
    let c = Ctx::with(hs);
    let mut rows: Vec<Lifecycle> = vec![];
    let mut mark = |c: &Ctx, name: &'static str, txs: &[&SignedTx], before: u64| {
        rows.push(Lifecycle {
            name,
            txs: txs.len(),
            log: log_bytes(&log_dir) - before,
            raw_json: txs.iter().map(|t| wire_json_bytes(t)).sum(),
            sigscripts: txs.iter().map(|t| sigscript_bytes(t)).sum(),
        });
        let _ = c;
    };

    // 1. a limit ask: create, one partial fill, cancel
    let before = log_bytes(&log_dir);
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let a6 = AskState { amount_left: 6 * WHOLE, ..a };
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order: c.w.order(&fill, find_cov(&fill, &cov).unwrap(), AnyState::KobAsk(a6)),
        custody: Some(c.w.token_at(&fill, find_custody(&fill, &cov, 6 * WHOLE).unwrap(), Kcc20State::custody(6 * WHOLE, cov.0, EXT))),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    c.push(&[&cancel]).await;
    mark(&c, "limit ask: create + partial fill + cancel", &[&create, &fill, &cancel], before);

    // 2. a limit bid: create + one partial fill
    let before = log_bytes(&log_dir);
    let b0 = bid(MAKER_B, P245);
    let create_b = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create_b]).await;
    let mut bt = batch(&c.w, c.daa(), vec![Leg::Bid { order: c.w.order(&create_b, 0, b0), amount: 4 * WHOLE, t: None }]);
    bt.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill_b = c.w.sign(&Action::Batch(bt));
    c.push(&[&fill_b]).await;
    mark(&c, "limit bid: create + partial fill", &[&create_b, &fill_b], before);

    // 3. a repeat IFD entry: create, fill (books an exit), take-profit with merge
    let before = log_bytes(&log_dir);
    let ib = IfdBidState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) };
    let create_i = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create_i]).await;
    let icov = c.w.cov(&create_i, 0);
    let entry_daa = c.w.utxo(&create_i, 0).block_daa_score as i64;
    let lock = c.daa();
    let mut bt = batch(
        &c.w,
        lock,
        vec![Leg::IfdBid { order: c.w.order(&create_i, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None }],
    );
    bt.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill_i = c.w.sign(&Action::Batch(bt));
    c.push(&[&fill_i]).await;
    let (xi, exit) = find_fresh(&fill_i, &[icov]).unwrap();
    let entry6 = IfdBidState { amount_left: 6 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() };
    let exit4 = ib.exit_for(4 * WHOLE, Some(Booking { parent: icov.0, until: rpt_until(ib.expiry_daa, entry_daa).unwrap() })).unwrap();
    let leg = Leg::CondAsk {
        order: c.w.order(&fill_i, xi, exit4),
        custody: c.w.token_at(&fill_i, find_custody(&fill_i, &exit, 4 * WHOLE).unwrap(), Kcc20State::custody(4 * WHOLE, exit.0, EXT)),
        amount: 3 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(c.w.order(&fill_i, find_cov(&fill_i, &icov).unwrap(), entry6)),
    };
    let tp = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&tp]).await;
    mark(&c, "repeat IFD: create + fill (booked exit) + take-profit merge", &[&create_i, &fill_i, &tp], before);

    println!("\nstorage per order lifecycle (log = permanent extracted records; raw = wire JSON of the same txs)");
    println!("{:<62} {:>4} {:>8} {:>9} {:>9} {:>7}", "scenario", "txs", "log B", "raw JSON", "sigscript", "log/raw");
    let (mut tl, mut tr) = (0u64, 0usize);
    for r in &rows {
        println!(
            "{:<62} {:>4} {:>8} {:>9} {:>9} {:>6.1}%",
            r.name,
            r.txs,
            r.log,
            r.raw_json,
            r.sigscripts,
            100.0 * r.log as f64 / r.raw_json as f64
        );
        tl += r.log;
        tr += r.raw_json;
        assert!(r.log < 12_000, "{}: a whole order lifecycle must stay a few KB, got {}", r.name, r.log);
        assert!((r.log as f64) < 0.20 * r.raw_json as f64, "{}: records must be a small fraction of the raw transactions", r.name);
    }
    println!("total: {tl} B of records for {tr} B of raw transaction JSON ({:.1}%)", 100.0 * tl as f64 / tr as f64);
    let per_tx = tl as f64 / rows.iter().map(|r| r.txs).sum::<usize>() as f64;
    println!("average record-log bytes per KOB transaction: {per_tx:.0}");
    let orders: i64 = c.hs.query("SELECT COUNT(*) FROM orders", []);
    println!("orders: {orders}, record-log bytes per order (incl. its fills/cancels): {:.0}", tl as f64 / orders as f64);
}

#[test]
fn chain_block_window_costs_bytes_per_block_not_per_day_of_history() {
    // `blocks` holds (seq, hash, daa, blue, ts) for the reorg window only. Measure its on-disk cost per
    // row with the real schema (table + hash index + daa index), then scale to the window.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blocks.sqlite3");
    let conn = kob_executor::indexer::db::open_writer(&path, "testnet-10").unwrap();
    let n = 100_000u64;
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut st = tx.prepare("INSERT INTO blocks (hash, daa) VALUES (?1, ?2)").unwrap();
        for i in 0..n {
            st.execute(rusqlite::params![&h("b", i).0[..], 1_000_000 + i as i64]).unwrap();
        }
    }
    tx.commit().unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").unwrap();
    drop(conn);
    let size = std::fs::metadata(&path).unwrap().len() as f64;
    let empty = {
        let p2 = dir.path().join("empty.sqlite3");
        let c2 = kob_executor::indexer::db::open_writer(&p2, "testnet-10").unwrap();
        c2.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").unwrap();
        drop(c2);
        std::fs::metadata(&p2).unwrap().len() as f64
    };
    let per_block = (size - empty) / n as f64;
    let window_blocks = 12.0 * 3600.0 * 10.0;
    println!(
        "\nchain-block window: {per_block:.1} B per block on disk; 12 h at 10 BPS = {window_blocks:.0} blocks = {:.1} MB steady state",
        per_block * window_blocks / 1e6
    );
    assert!(per_block < 120.0, "{per_block}");
}

#[tokio::test]
async fn derived_database_and_log_cost_per_order_at_scale() {
    // 300 orders (asks and bids), each created and partially filled, through the real write path with a
    // record log: what a busy day of KOB traffic costs on disk.
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("records");
    let db_path = dir.path().join("index.sqlite3");
    let (log, _) = RecordLog::open(&log_dir, 1 << 26, 0).unwrap();
    let conn = kob_executor::indexer::db::open_writer(&db_path, "testnet-10").unwrap();
    let empty = {
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").unwrap();
        std::fs::metadata(&db_path).unwrap().len()
    };
    let hs = Harness::with_node(MockNode::new("testnet-10"), conn, Some(log), wide_window());
    let mut c = Ctx::with(hs);
    c.w.validate = false;
    let n = 150;
    let mut raw = 0usize;
    for i in 0..n {
        let a = AskState { price: P250 + i, ..ask(MAKER_A, P250) };
        let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
        let cov = c.w.cov(&create, 0);
        c.w.include(&c.hs.node, &[&create]);
        let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
        let leg = Leg::Ask { order: c.w.order(&create, 0, a), custody, amount: 4 * WHOLE, t: None };
        let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
        c.w.include(&c.hs.node, &[&fill]);
        let b0 = bid(MAKER_B, P245 + i);
        let create_b = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
        c.w.include(&c.hs.node, &[&create_b]);
        raw += wire_json_bytes(&create) + wire_json_bytes(&fill) + wire_json_bytes(&create_b);
    }
    c.hs.sync().await;
    let orders: i64 = c.hs.query("SELECT COUNT(*) FROM orders", []);
    assert_eq!(orders, 2 * n);
    let log_b = log_bytes(&log_dir);
    c.hs.ingest.lock().unwrap().conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").unwrap();
    let db = std::fs::metadata(&db_path).unwrap().len();
    // the blocks of the window (one per include here) are part of `db`; subtract their share
    let blocks: i64 = c.hs.query("SELECT COUNT(*) FROM blocks", []);
    let per_order_db = (db - empty) as f64 / orders as f64;
    println!(
        "\n{orders} orders / {} txs: record log {log_b} B ({:.0} B per order), derived database {} B ({per_order_db:.0} B per order incl. {blocks} block rows), raw JSON of the same txs {raw} B",
        3 * n,
        log_b as f64 / orders as f64,
        db - empty
    );
    assert!(per_order_db < 6_000.0, "{per_order_db}");
    let _ = &mut c;
}
