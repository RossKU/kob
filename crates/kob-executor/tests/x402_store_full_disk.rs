//! The x402 ledger and invoice store on a full disk: a record whose write fails is cut back off the file and the
//! operation fails, so the next record starts on a clean line and every acknowledged record survives a restart. A file
//! whose last line was written whole but does not parse (a partial record with a complete one glued after it) refuses to
//! open instead of being dropped as an unfinished write.
//!
//! The full-disk tests need a small tmpfs and skip themselves without it:
//! `mount -t tmpfs -o size=2m tmpfs /mnt/kob-small && KOB_SMALL_TMPFS=/mnt/kob-small cargo test -p kob-executor --test
//! x402_store_full_disk -- --test-threads=1`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use kob_executor::x402::facilitator::{InvoiceRecord, InvoiceStore};
use kob_executor::x402::ledger::{Ledger, LedgerError, NewEntry, Watched};
use kob_x402::chain::Outpoint;
use kob_x402::invoice::Invoice;
use kob_x402::wire::{hex, Network};

fn small_fs(sub: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("KOB_SMALL_TMPFS")?);
    let d = base.join(sub);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Some(d)
}

/// Fills the file system of `dir`, then frees about `leave` bytes; returns the filler (remove it to free the space).
fn fill(dir: &Path, leave: u64) -> PathBuf {
    let p = dir.join("filler");
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).open(&p).unwrap();
    let chunk = vec![0u8; 4096];
    let mut n = 0u64;
    loop {
        match f.write(&chunk) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k as u64,
        }
    }
    f.set_len(n.saturating_sub(leave)).unwrap();
    f.sync_all().unwrap();
    p
}

fn entry(tx: u8, pad: usize) -> NewEntry {
    let mut ext = serde_json::Map::new();
    ext.insert("pad".into(), serde_json::Value::String("x".repeat(pad)));
    NewEntry {
        txid: [tx; 32],
        request_hash: [tx; 32],
        requirements_hash: [9; 32],
        payment_id: None,
        profile: "standard-native".into(),
        kind: "native".into(),
        merchant: "m1".into(),
        network: "kaspa:testnet-10".into(),
        payer: None,
        amount: "100".into(),
        finality: "accepted".into(),
        consumed: vec![Outpoint::new([tx; 32], 0)],
        order_inputs: vec![],
        watched: Watched { txid: hex(&[tx; 32]), index: 0, spk: "0000".into(), amount: 100 },
        extension: ext,
        now_ms: 1_000,
        intent: None,
        invoice: None,
    }
}

fn record(id: &str, pad: usize) -> InvoiceRecord {
    InvoiceRecord {
        id: id.into(),
        merchant: "m1".into(),
        invoice: Invoice::new(Network::Testnet10, id, 1_900_000_000_000, Some("x".repeat(pad)), vec![]),
        expires_ms: 1_900_000_000_000,
        created_ms: 1_000,
        extra: vec![],
    }
}

#[test]
fn a_ledger_write_that_fails_on_a_full_disk_is_cut_back_and_later_records_survive() {
    let Some(dir) = small_fs("ledger") else { return eprintln!("KOB_SMALL_TMPFS not set: skipped") };
    let p = dir.join("x402-ledger.jsonl");
    {
        let l = Ledger::open(&p).unwrap();
        l.claim(entry(1, 10)).unwrap();
        let len = std::fs::metadata(&p).unwrap().len();
        let filler = fill(&dir, 4096);
        assert!(l.claim(entry(2, 64 * 1024)).is_err(), "the write cannot complete");
        assert_eq!(std::fs::metadata(&p).unwrap().len(), len, "the partial record is cut back");
        assert!(l.get(&hex(&[2; 32])).is_none(), "and not recorded in memory");
        std::fs::remove_file(filler).unwrap();
        l.claim(entry(3, 10)).unwrap();
        l.claim(entry(4, 10)).unwrap();
    }
    let l = Ledger::open(&p).expect("a clean file opens");
    assert_eq!(l.len(), 3);
    for t in [1u8, 3, 4] {
        assert!(l.get(&hex(&[t; 32])).is_some(), "record {t} survives");
        assert!(l.is_consumed(&Outpoint::new([t; 32], 0)));
    }
    assert!(l.get(&hex(&[2; 32])).is_none());
}

#[test]
fn an_invoice_store_write_that_fails_on_a_full_disk_is_cut_back_and_later_records_survive() {
    let Some(dir) = small_fs("invoices") else { return eprintln!("KOB_SMALL_TMPFS not set: skipped") };
    let p = dir.join("invoices.jsonl");
    {
        let s = InvoiceStore::open(&p).unwrap();
        s.insert(record("inv-1", 10)).unwrap();
        let len = std::fs::metadata(&p).unwrap().len();
        let filler = fill(&dir, 4096);
        assert!(s.insert(record("inv-2", 64 * 1024)).is_err(), "the write cannot complete");
        assert_eq!(std::fs::metadata(&p).unwrap().len(), len, "the partial record is cut back");
        assert!(s.get("inv-2").is_none());
        std::fs::remove_file(filler).unwrap();
        s.insert(record("inv-3", 10)).unwrap();
        s.insert(record("inv-4", 10)).unwrap();
    }
    let s = InvoiceStore::open(&p).expect("a clean file opens");
    assert_eq!(s.len(), 3);
    assert!(s.get("inv-2").is_none());
}

#[test]
fn a_whole_last_line_that_does_not_parse_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    // the ledger: a partial record with a complete one glued after it, newline-terminated (written whole)
    let p = dir.path().join("x402-ledger.jsonl");
    {
        let l = Ledger::open(&p).unwrap();
        l.claim(entry(1, 10)).unwrap();
        l.claim(entry(2, 10)).unwrap();
    }
    let text = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let glued = format!("{}\n{}{}\n", lines[0], &lines[1][..40], lines[1]);
    std::fs::write(&p, &glued).unwrap();
    let e = Ledger::open(&p).err().expect("refused");
    assert!(matches!(e, LedgerError::Corrupt { line: 2, .. }), "{e}");
    assert!(e.to_string().contains("Ledger recovery"), "{e}");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), glued, "the file is left as it is");
    // the same bytes without the final newline are an unfinished write: dropped
    std::fs::write(&p, glued.trim_end_matches('\n')).unwrap();
    assert_eq!(Ledger::open(&p).unwrap().len(), 1);

    // the invoice store, likewise
    let p = dir.path().join("invoices.jsonl");
    {
        let s = InvoiceStore::open(&p).unwrap();
        s.insert(record("inv-1", 10)).unwrap();
        s.insert(record("inv-2", 10)).unwrap();
    }
    let text = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let glued = format!("{}\n{}{}\n", lines[0], &lines[1][..40], lines[1]);
    std::fs::write(&p, &glued).unwrap();
    let e = InvoiceStore::open(&p).err().expect("refused");
    assert!(e.contains("line 2") && e.contains("Ledger recovery"), "{e}");
    std::fs::write(&p, glued.trim_end_matches('\n')).unwrap();
    assert_eq!(InvoiceStore::open(&p).unwrap().len(), 1);
}
