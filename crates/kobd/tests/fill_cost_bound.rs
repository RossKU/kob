//! Regression: a fill costs the indexer the same however many fills its order already had.
//!
//! Every spend refreshed the order's `order_state` row by summing all its fill events, and the tip
//! read and the closing-event read walked all its UTXOs and events. A maker filling its own big ask
//! one unit at a time thus cost the follower about two reads per earlier fill on every fill, which
//! is quadratic in the fills. Now the filled amount is a running total, the tip is read through the
//! partial index of the unspent UTXOs, and only a closed order reads its closing event.
//!
//! The work is counted in SQLite page reads of the writer connection, which are the same on every
//! machine.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kobd::hex::Hash32;
use kobd::testkit::*;
use rusqlite::Connection;

/// The pages the connection has read so far, from its page cache or not.
fn page_reads(conn: &Connection) -> i64 {
    let mut total = 0;
    for op in [rusqlite::ffi::SQLITE_DBSTATUS_CACHE_HIT, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS] {
        let (mut cur, mut hi) = (0, 0);
        // SAFETY: the handle is the open connection's own, and the call only reads its counters.
        let rc = unsafe { rusqlite::ffi::sqlite3_db_status(conn.handle(), op, &mut cur, &mut hi, 0) };
        assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
        total += i64::from(cur);
    }
    total
}

/// Gives the order `k` more fills of one base unit and `k` more spent UTXOs, as `k` earlier fills
/// would have, in an imported block that no reorg reverts. Its stored filled amount grows by `k`.
fn grow_history(c: &Ctx, cov: &Hash32, k: i64) {
    if k == 0 {
        return;
    }
    let ing = c.hs.ingest.lock().unwrap();
    let conn = ing.conn();
    let order = "(SELECT id FROM orders WHERE covenant_id = ?1)";
    conn.execute(
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2) \
             INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, closes) \
             SELECT {order}, 0, 0, 0, randomblob(32), 0, 'fill', (SELECT token_id FROM orders WHERE covenant_id = ?1), 1, 1, 0 FROM n"
        ),
        rusqlite::params![&cov.0[..], k],
    )
    .unwrap();
    conn.execute(
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2) \
             INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa, spent_block, spent_entry) \
             SELECT randomblob(32), 0, {order}, 1, x'', 0, 0, 0, 'fill' FROM n"
        ),
        rusqlite::params![&cov.0[..], k],
    )
    .unwrap();
    conn.execute(
        &format!("UPDATE order_state SET filled_amount = filled_amount + ?2 WHERE order_id = {order}"),
        rusqlite::params![&cov.0[..], k],
    )
    .unwrap();
}

fn filled(c: &Ctx, cov: &Hash32) -> i64 {
    c.hs.query("SELECT filled_amount FROM order_state WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)", [&cov.0[..]])
}

fn fill_sum(c: &Ctx, cov: &Hash32) -> i64 {
    c.hs.query(
        "SELECT SUM(amount) FROM order_events WHERE kind = 'fill' AND order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
        [&cov.0[..]],
    )
}

/// The page reads of indexing one fill of an ask whose history holds `k` earlier fills.
///
/// It also checks the filled amount: the running total is the sum of the fill events after the
/// fill, after a reorg takes the fill back, and after the fill is accepted again.
async fn fill_with_history(k: i64) -> i64 {
    // A file, not memory: an in-memory store reads its pages through a mapping the counters miss.
    let dir = tempfile::tempdir().unwrap();
    let c = Ctx::with(Harness::new(kobd::indexer::db::open_writer(&dir.path().join("index.sqlite3"), "testnet-10").unwrap(), None));
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: WHOLE, t: None };
    let fill1 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill1]).await;
    assert_eq!(filled(&c, &cov), WHOLE);

    grow_history(&c, &cov, k);
    assert_eq!(filled(&c, &cov), fill_sum(&c, &cov));

    let a9 = AskState { amount_left: 9 * WHOLE, ..a };
    let order = c.w.order(&fill1, find_cov(&fill1, &cov).unwrap(), a9);
    let custody = c.w.token_at(&fill1, find_custody(&fill1, &cov, 9 * WHOLE).unwrap(), Kcc20State::custody(9 * WHOLE, cov.0, EXT));
    let fill2 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![Leg::Ask { order, custody, amount: WHOLE, t: None }])));
    let before = page_reads(c.hs.ingest.lock().unwrap().conn());
    c.push(&[&fill2]).await;
    let reads = page_reads(c.hs.ingest.lock().unwrap().conn()) - before;
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(filled(&c, &cov), 2 * WHOLE + k);
    assert_eq!(filled(&c, &cov), fill_sum(&c, &cov));

    // A reorg takes the fill back with one recount, and the fill accepted again adds it again.
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(filled(&c, &cov), WHOLE + k);
    assert_eq!(filled(&c, &cov), fill_sum(&c, &cov));
    c.w.include(&c.hs.node, &[&fill2]);
    c.hs.sync().await;
    assert_eq!(filled(&c, &cov), 2 * WHOLE + k);
    assert_eq!(filled(&c, &cov), fill_sum(&c, &cov));

    // A recount, which a revert does, reads every fill: the measure would see a per-fill one.
    let ing = c.hs.ingest.lock().unwrap();
    let before = page_reads(ing.conn());
    kobd::indexer::processor::refresh_order_state(ing.conn(), &cov).unwrap();
    let recount = page_reads(ing.conn()) - before;
    println!("history of {k} fills: the fill read {reads} pages, a recount {recount}");
    if k > 0 {
        assert!(recount > k / 100, "a recount of {k} fills read only {recount} pages: the measure does not see per-fill work");
    }
    reads
}

#[tokio::test]
async fn a_fill_of_an_order_with_a_long_history_costs_what_the_first_fills_did() {
    const K: i64 = 50_000;
    let short = fill_with_history(0).await;
    let long = fill_with_history(K).await;
    // A level or two more in the bigger B-trees, and nothing per earlier fill. A recount of the
    // fills and a walk of the UTXOs on every fill read about one page per fifty earlier fills each,
    // some 2 000 pages here.
    assert!(long <= short + 200, "a fill read {long} pages with {K} earlier fills, {short} with none");
}
