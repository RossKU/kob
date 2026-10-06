//! Regression: the permanent record log keeps facts, not the attacker's bytes.
//!
//! Any transaction whose payload starts with `KOB1` is relevant, and its whole payload used to be stored forever (and replayed
//! on every rebuild), decodable or not. Now an undecodable payload is a five-byte stand-in that the processor rejects as
//! `payload:undecodable`, and a decodable one keeps only its placement (order) records, canonically re-encoded and at most one per
//! output of the transaction.

use kob_executor::hex::HexBytes;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::record::{Extractor, UNDECODABLE_PAYLOAD};
use kob_executor::testkit::*;

#[test]
fn a_junk_kob1_payload_is_recorded_as_a_stand_in() {
    let conn = open_memory("testnet-10").unwrap();
    let tokens = allowlist();
    let ex = Extractor { conn: &conn, tokens: &tokens, track_traded: false };
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
    let tokens = allowlist();
    let ex = Extractor { conn: &conn, tokens: &tokens, track_traded: false };
    let mut tx = noise_tx(1, false);
    // valid records that are not orders: unknown optional records
    let records: Vec<Record> = (0..50).map(|i| Record::Unknown { record_type: 0x90, value: vec![i as u8; 200] }).collect();
    tx.payload = HexBytes(encode(&records).unwrap());
    assert!(ex.extract(&tx, 0).unwrap().is_none(), "nothing of it is KOB's: no record, nothing stored");
}
