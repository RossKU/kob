//! The activity totals (`GET /v1/stats`) through the real write path: a fill of a real transaction records its fee and the script
//! that funded it (the record log's payer section), a reorg takes both back out of the totals, the same transaction accepted again
//! puts them back, a rebuild from the record log gives the same totals, and a log of frames written before payers were recorded
//! replays with every figure but the executors (unknown, not counted).

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kobd::indexer::activity::{self, ActivityView};
use kobd::indexer::db::open_memory;
use kobd::indexer::recordlog::{self, RecordLog};
use kobd::testkit::*;
use rusqlite::Connection;

fn totals(conn: &Connection) -> ActivityView {
    activity::activity(conn, None).unwrap()
}

fn live(c: &Ctx) -> ActivityView {
    totals(c.hs.ingest.lock().unwrap().conn())
}

/// (orders, fills, traders, executors, fees paid)
fn figures(v: &ActivityView) -> (i64, i64, i64, i64, String) {
    (v.orders, v.fills, v.traders, v.executors, v.fees_paid.clone())
}

#[tokio::test]
async fn a_fill_counts_its_fee_and_executor_and_a_reorg_takes_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(open_memory("testnet-10").unwrap(), Some(log)));
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(figures(&live(&c)), (1, 0, 1, 0, "0".into()));
    let placed = c.hs.snapshot();

    // the taker's batch fills 4 whole tokens at the order's tip: 4 x TIP sompi to the filler, funded by the taker's coin
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let fee = activity::fill_fee(Some(4 * WHOLE), Some(TIP), Some(SCALE)).unwrap();
    assert_eq!(fee, 4 * TIP);
    assert_eq!(figures(&live(&c)), (1, 1, 1, 1, fee.to_string()));
    assert_eq!(c.hs.query::<i64>("SELECT fee FROM order_events WHERE kind = 'fill'", []), fee);
    // the executor is the script of the taker's funding input
    let spk: Vec<u8> = c.hs.query("SELECT s.spk FROM order_events e JOIN scripts s ON s.id = e.executor_id WHERE e.kind = 'fill'", []);
    let funding = fill.tx.inputs.iter().find(|i| i.utxo.covenant_id.is_none()).expect("a funding input");
    assert_eq!(kobd::hex::encode(&spk), funding.utxo.script_public_key);
    // a window that starts after the fill counts nothing but what it holds
    let after: i64 = c.hs.query("SELECT MAX(ts) + 60000 FROM order_events", []);
    let w = activity::activity(c.hs.ingest.lock().unwrap().conn(), Some(after)).unwrap();
    assert_eq!(figures(&w), (0, 0, 0, 0, "0".into()));

    // the fill's block is reorged out: the totals are the ones before it, row for row
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(figures(&live(&c)), (1, 0, 1, 0, "0".into()));
    let rows =
        |s: &str| s.lines().filter(|l| l.starts_with("activity ") || l.starts_with("actor ")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(rows(&c.hs.snapshot()), rows(&placed));

    // accepted again in the next block
    c.w.include(&c.hs.node, &[&fill]);
    c.hs.sync().await;
    let filled = live(&c);
    assert_eq!(figures(&filled), (1, 1, 1, 1, fee.to_string()));
    let snap = c.hs.snapshot();
    drop(c);

    // a database rebuilt from the record log has the same totals and rows
    let logged = recordlog::read_all(dir.path()).unwrap();
    let rebuilt = |records: Vec<_>| {
        let mut ing =
            kobd::indexer::ingest::Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
        ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
        ing.replay(records).unwrap();
        ing
    };
    let ing = rebuilt(logged.records.clone());
    assert_eq!(totals(ing.conn()), filled);
    assert_eq!(snapshot(ing.conn()), snap);

    // the same log as a build before payers wrote it: every figure but the executors, who are unknown
    let mut old = logged.records.clone();
    for (_, b) in old.iter_mut() {
        for t in b.blocks.iter_mut().flat_map(|b| b.txs.iter_mut()) {
            t.payer = None;
        }
        // such a frame is format 2 (no payer section, no declaration)
        assert_eq!(b.encode(1)[..2], [0, recordlog::FORMAT]);
    }
    let ing = rebuilt(old);
    let v = totals(ing.conn());
    assert_eq!(figures(&v), (1, 1, 1, 0, fee.to_string()));
    assert_eq!(
        ing.conn().query_row("SELECT COUNT(*) FROM order_events WHERE executor_id IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap(),
        0
    );
    // and a frame with a payer is format 4
    assert!(logged.records.iter().any(|(_, b)| b.encode(1)[..2] == [0, recordlog::FORMAT_PAYER]));
}
