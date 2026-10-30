//! Audit 2026-10-30 I7 (b): a declaration section rode on any transaction that was relevant to KOB, so a token transfer or stray send
//! carrying a 16 KiB `0x00{..}` payload put those 16 KiB into the permanent record log for the price of its fee. The section is logged
//! only when it binds; the payload stays a database candidate like any other.

mod common;

use common::*;
use kob_executor::indexer::recordlog::{self, RecordLog};
use kob_executor::testkit::*;
use kob_protocol::family::Family;
use kob_protocol::kcc23;
use kob_protocol::state::*;

fn junk_declaration() -> Vec<u8> {
    let mut body = br#"{"pad":""#.to_vec();
    body.resize(kcc23::MAX_READ_PAYLOAD - 3, b'a');
    body.extend_from_slice(br#""}"#);
    [&[0u8][..], &body].concat()
}

fn logged_bytes(dir: &std::path::Path) -> usize {
    recordlog::read_all(dir).unwrap().records.iter().map(|(_, b)| b.encode(0).len()).sum()
}

#[tokio::test]
async fn a_declaration_payload_on_a_relevant_token_transfer_stays_out_of_the_record_log() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(conn(), Some(log)));
    let create = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let before = logged_bytes(dir.path());

    // a stray send (relevant to KOB: tokens to an order id) that carries a 16 KiB declaration payload
    let plain = c.w.stray(cov, 3 * WHOLE, TAKER);
    c.push(&[&plain]).await;
    let plain_cost = logged_bytes(dir.path()) - before;
    let junk = c.w.stray_with_payload(Family::Kcc20, cov, 4 * WHOLE, TAKER, Some(junk_declaration()));
    let before_junk = logged_bytes(dir.path());
    c.push(&[&junk]).await;
    let junk_cost = logged_bytes(dir.path()) - before_junk;
    assert!(
        junk_cost < plain_cost + 200,
        "a stray with a 16 KiB declaration payload cost {junk_cost} bytes of record log, a plain one {plain_cost}"
    );
    // it is still a candidate in the database (a later genesis can bind it)
    let n: i64 = c.hs.query("SELECT COUNT(*) FROM kcc23_candidates", []);
    assert_eq!(n, 1, "kept as a database candidate");
    // and it is a stray like any other
    let strays: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos WHERE role = 'stray'", []);
    assert_eq!(strays, 2);
}
