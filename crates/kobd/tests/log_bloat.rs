//! Regression: the permanent record log keeps facts, not the attacker's bytes.
//!
//! Any transaction whose payload starts with `KOB1` is relevant, but its whole payload is not
//! stored forever (and replayed on every rebuild), decodable or not. An undecodable payload is a
//! five-byte stand-in that the processor rejects as `payload:undecodable`, and a decodable one
//! keeps only its placement (order) records, canonically re-encoded and at most one per output of
//! the transaction.

use kobd::hex::HexBytes;
use kobd::indexer::db::open_memory;
use kobd::indexer::record::{Extractor, UNDECODABLE_PAYLOAD};
use kobd::testkit::*;

#[test]
fn a_junk_kob1_payload_is_recorded_as_a_stand_in() {
    let conn = open_memory("testnet-10").unwrap();
    let ex = Extractor { conn: &conn };
    for size in [1_000usize, 20_000, 90_000] {
        let mut tx = noise_tx(1, false);
        let mut payload = b"KOB1\x02\xff\xff".to_vec();
        payload.resize(size, 0xAB);
        tx.payload = HexBytes(payload);
        let rec = ex.extract(&tx, 0).unwrap().expect("the reject is still recorded");
        assert_eq!(rec.payload, UNDECODABLE_PAYLOAD, "{size}-byte junk");
        assert!(rec.encoded_len() < 400, "record of {} bytes for {size} bytes of junk", rec.encoded_len());
    }
}

#[test]
fn a_valid_payload_full_of_padding_records_keeps_no_padding() {
    use kob_protocol::payload::{encode, Record};
    let conn = open_memory("testnet-10").unwrap();
    let ex = Extractor { conn: &conn };
    let mut tx = noise_tx(1, false);
    // valid records that are not orders: unknown optional records
    let records: Vec<Record> = (0..50).map(|i| Record::Unknown { record_type: 0x90, value: vec![i as u8; 200] }).collect();
    tx.payload = HexBytes(encode(&records).unwrap());
    assert!(ex.extract(&tx, 0).unwrap().is_none(), "nothing of it is KOB's: no record, nothing stored");
}

/// Security review 2026-11-19 K-L6: transactions whose only relevance is a KOB1 payload that does not decode are
/// recorded (as `payload:undecodable` rejects, and in the permanent record log) only up to a bound per reorg window.
#[tokio::test]
async fn junk_kob1_payloads_are_recorded_within_a_bound_per_reorg_window() {
    use kobd::indexer::record::MAX_UNDECODABLE_IN_WINDOW;
    use kobd::indexer::recordlog::{self, RecordLog};
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let hs = Harness::new(open_memory("testnet-10").unwrap(), Some(log));
    let junk = |n: u64| {
        let mut tx = noise_tx(n, false);
        let mut payload = b"KOB1\x02\xff\xff".to_vec();
        payload.resize(64, 0xAB);
        tx.payload = HexBytes(payload);
        tx.verbose_data.transaction_id = kobd::rpc::verify::tx_id(&tx).unwrap();
        tx
    };
    hs.node.push_block((0..200).map(junk).collect());
    hs.node.push_block((200..400).map(junk).collect());
    hs.sync().await;
    let n: i64 = hs.query("SELECT COUNT(*) FROM rejects WHERE reason = 'payload:undecodable'", []);
    assert_eq!(n, MAX_UNDECODABLE_IN_WINDOW, "400 junk payloads, the first ones recorded");
    let logged: usize =
        recordlog::read_all(dir.path()).unwrap().records.iter().flat_map(|(_, b)| b.blocks.iter()).map(|b| b.txs.len()).sum();
    assert_eq!(logged as i64, MAX_UNDECODABLE_IN_WINDOW, "and only those in the record log");
}
