//! The binary end to end: a book snapshot file (JSON round trip) planned offline by
//! `kob-executor match --offline` and `kob-executor keep --offline`; no key on the command line.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::MemoryBook;
use kob_protocol::state::*;

fn snapshot(dir: &std::path::Path, b: &MemoryBook) -> std::path::PathBuf {
    let json = serde_json::to_string_pretty(b).unwrap();
    let back: MemoryBook = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, b, "snapshot JSON round trip");
    let p = dir.join("book.json");
    std::fs::write(&p, json).unwrap();
    p
}

fn exe(args: &[&str]) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kob-executor"))
        .args(args)
        .env_remove("KOB_OPERATOR_KEY")
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env("RUST_LOG", "error")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    (String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
}

#[test]
fn offline_match_and_keep_from_a_snapshot_file() {
    let dir = std::env::temp_dir().join(format!("kob-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut b = book(vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let mut ioc = ask(3, P250, 2 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    ioc.expiry_daa = NO_EXPIRY;
    b.orders.push(listed(cid(3), AnyState::KobAsk(ioc), CARRIER, 1_000));
    let p = snapshot(&dir, &b);
    let (out, _) = exe(&["match", "--book-file", p.to_str().unwrap(), "--offline"]);
    let lines: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 1, "{out}");
    assert_eq!(lines[0]["engineValid"], true);
    assert!(lines[0]["profit"].as_i64().unwrap() > 0);
    assert_eq!(lines[0]["fills"].as_array().unwrap().len(), 2);
    // The keeper kills the IOC (UTXO DAA 1000: its kill time passed long ago).
    let (out, _) = exe(&["keep", "--book-file", p.to_str().unwrap(), "--offline"]);
    let jobs: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(jobs.iter().any(|j| j["kind"] == "kill" && j["engineValid"] == true), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

fn exe_fails(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kob-executor"))
        .args(args)
        .env_remove("KOB_OPERATOR_KEY")
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env("RUST_LOG", "error")
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected a failure");
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn run_needs_a_role_and_a_key_and_index_keeps_its_subcommands() {
    let dir = std::env::temp_dir().join(format!("kob-cli-run-{}", std::process::id()));
    let data = dir.to_str().unwrap();
    // both roles off is `index`
    let err = exe_fails(&["run", "--no-match", "--no-keep", "--data-dir", data]);
    assert!(err.contains("kob-executor index"), "{err}");
    // the keyed roles never start without a key (and never touch the data directory first)
    let err = exe_fails(&["run", "--data-dir", data]);
    assert!(err.contains("no operator key"), "{err}");
    assert!(!dir.exists());
    // the indexer's own subcommands and global options still parse after the refactor
    let (out, _) = exe(&["index", "export-orders", "--data-dir", data, "--network", "testnet-10", "--live-only"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_object(), "{out}");
    // a read-only tool: it creates nothing (and takes no writer lock)
    assert!(!dir.exists());
    let (help, _) = exe(&["--help"]);
    for cmd in ["run", "index", "match", "keep"] {
        assert!(help.contains(cmd), "{help}");
    }
    let (help, _) = exe(&["run", "--help"]);
    for flag in ["--key-file", "--max-book-lag-daa", "--no-match", "--no-keep", "--data-dir", "--keeper-min-profit"] {
        assert!(help.contains(flag), "{help}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn offline_match_from_a_kron_snapshot_file() {
    // the JSON snapshot carries the KRON kinds and 46-byte KRON token states (`id_type`, `is_minter`)
    let dir = std::env::temp_dir().join(format!("kob-cli-kron-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let b = book(vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let k = to_kron(&input(&b));
    let kb = MemoryBook { daa_score: b.daa_score, orders: k.orders, wallet_tokens: vec![] };
    let p = snapshot(&dir, &kb);
    let json = std::fs::read_to_string(&p).unwrap();
    assert!(json.contains("KobAskKron") && json.contains("id_type") || json.contains("idType"), "{json}");
    let (out, _) = exe(&["match", "--book-file", p.to_str().unwrap(), "--offline"]);
    let lines: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 1, "{out}");
    assert_eq!(lines[0]["engineValid"], true);
    assert_eq!(lines[0]["fills"].as_array().unwrap().len(), 2);
    assert!(lines[0]["fills"][0]["kind"].as_str().unwrap().ends_with("Kron"), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}
