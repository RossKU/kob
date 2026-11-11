//! The binary end to end: a book snapshot file (JSON round trip) planned offline by
//! `kobd match --offline` and `kobd keep --offline`; no key on the command line.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_protocol::state::*;
use kobd::matcher::book::MemoryBook;

fn snapshot(dir: &std::path::Path, b: &MemoryBook) -> std::path::PathBuf {
    let json = serde_json::to_string_pretty(b).unwrap();
    let back: MemoryBook = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, b, "snapshot JSON round trip");
    let p = dir.join("book.json");
    std::fs::write(&p, json).unwrap();
    p
}

fn exe(args: &[&str]) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kobd"))
        .args(args)
        .env_remove("KOB_OPERATOR_KEY")
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env_remove("KOB_NETWORK")
        .env_remove("KOB_NODE_WRPC")
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

/// The exit status and stderr of a run that fails.
fn exe_code(args: &[&str]) -> (Option<i32>, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kobd"))
        .args(args)
        .env_remove("KOB_OPERATOR_KEY")
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env_remove("KOB_NETWORK")
        .env_remove("KOB_NODE_WRPC")
        .env_remove("KOB_INDEX_NODES")
        .env("RUST_LOG", "error")
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected a failure");
    (out.status.code(), String::from_utf8(out.stderr).unwrap())
}

fn exe_fails(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kobd"))
        .args(args)
        .env_remove("KOB_OPERATOR_KEY")
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env_remove("KOB_NETWORK")
        .env_remove("KOB_NODE_WRPC")
        .env("RUST_LOG", "error")
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected a failure");
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn the_executor_needs_a_role_and_a_key_and_db_has_the_tools() {
    let dir = std::env::temp_dir().join(format!("kob-cli-run-{}", std::process::id()));
    let data = dir.to_str().unwrap();
    // both roles off is the Indexer
    let err = exe_fails(&["--executor", "--no-match", "--no-keep", "--data-dir", data]);
    assert!(err.contains("kobd --indexer"), "{err}");
    // the keyed roles never start without a key (and never touch the data directory first)
    let err = exe_fails(&["--executor", "--data-dir", data, "--network", "testnet-10"]);
    assert!(err.contains("no operator key"), "{err}");
    assert!(!dir.exists());
    // options and no configuration: the roles must be named
    let err = exe_fails(&["--data-dir", data, "--network", "testnet-10"]);
    assert!(err.contains("name the roles"), "{err}");
    // the Indexer takes the indexer's options only
    let (code, err) = exe_code(&["--indexer", "--dry-run", "--data-dir", data, "--network", "testnet-10"]);
    assert!(err.contains("the Indexer takes the indexer's options only") && code == Some(2), "{code:?} {err}");
    // the option of earlier builds for a further node: a usage error that names the new options, and nothing starts
    for role in ["--executor", "--indexer"] {
        let (code, err) = exe_code(&[role, "--node", "ws://127.0.0.1:18210", "--data-dir", data, "--network", "testnet-10"]);
        assert!(err.contains("--node is now --extra-node") && err.contains("--rpc-url") && code == Some(2), "{role}: {code:?} {err}");
    }
    // the commands of earlier builds are gone, with no alias and no hint: an unknown command, and nothing starts
    for args in [vec!["run", "--data-dir", data, "--network", "testnet-10"], vec!["index", "--config", "kobd.toml", "migrate"]] {
        let err = exe_fails(&args);
        assert!(err.contains("unrecognized subcommand"), "{args:?}: {err}");
    }
    assert!(!dir.exists());
    assert!(!dir.exists());
    // the reference build implies no network: the Executor, `db`, `match` and `keep` refuse to guess one (never testnet-10)
    if kob_protocol::artifacts::deployment_network().is_none() {
        for args in [
            vec!["--executor", "--data-dir", data],
            vec!["db", "export-orders", "--data-dir", data],
            vec!["match", "--book-file", "book.json"],
            vec!["keep", "--book-file", "book.json"],
        ] {
            let err = exe_fails(&args);
            assert!(err.contains("--network mainnet or --network testnet-10"), "{args:?}: {err}");
        }
        assert!(!dir.exists());
    }
    // the indexer's own subcommands and global options still parse after the refactor
    let (out, _) = exe(&["db", "export-orders", "--data-dir", data, "--network", "testnet-10", "--live-only"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_object(), "{out}");
    // a read-only tool: it creates nothing (and takes no writer lock)
    assert!(!dir.exists());
    // the matcher's book snapshot of an empty store: a MemoryBook `match --book-file` reads, to stdout or atomically to a file
    let (out, _) = exe(&["db", "export-book", "--data-dir", data, "--network", "testnet-10"]);
    let b: MemoryBook = serde_json::from_str(out.trim()).unwrap();
    assert!(b.orders.is_empty(), "{out}");
    let file = std::env::temp_dir().join(format!("kob-cli-book-{}.json", std::process::id()));
    exe(&["db", "export-book", "--data-dir", data, "--network", "testnet-10", "--out", file.to_str().unwrap()]);
    assert_eq!(serde_json::from_str::<MemoryBook>(&std::fs::read_to_string(&file).unwrap()).unwrap(), b);
    let _ = std::fs::remove_file(&file);
    assert!(!dir.exists());
    let (help, _) = exe(&["match", "--help"]);
    assert!(help.contains("--max-book-lag-daa"), "{help}");
    // `kobd --help`: the starts and the commands; the options of a start are on its roles' page
    let (top, _) = exe(&["--help"]);
    for cmd in
        ["setup", "config", "db", "logo-reports", "match", "keep", "withdraw", "kobd --executor --help", "kobd --indexer --help"]
    {
        assert!(top.contains(cmd), "{top}");
    }
    assert!(!top.contains("kobd run") && !top.contains("kobd index") && !top.contains("--dry-run"), "{top}");
    let (help, _) = exe(&["--executor", "--help"]);
    for flag in [
        "--dry-run",
        "--config",
        "--listen",
        "--pause-file",
        "--key-file",
        "--max-book-lag-daa",
        "--no-match",
        "--no-keep",
        "--data-dir",
        "--keeper-min-profit",
        "--no-weighted-select",
        "--select-alpha",
        "--select-max-defer-daa",
    ] {
        assert!(help.contains(flag), "{help}");
    }
    // the Indexer's page: its options only
    let (index, _) = exe(&["--indexer", "--help"]);
    assert!(index.contains("--listen") && index.contains("--extra-node") && !index.contains("\n      --dry-run"), "{index}");
    // the command list: db and logo-reports, no index
    let (cmds, _) = exe(&["help"]);
    assert!(cmds.contains("db ") && cmds.contains("logo-reports") && !cmds.contains("index "), "{cmds}");
    // a database tool lists the store's options, not a start's
    let (db, _) = exe(&["db", "export-orders", "--help"]);
    assert!(
        db.contains("--data-dir") && !db.contains("--fast-start") && !db.contains("--listen") && !db.contains("--rpc-url"),
        "{db}"
    );
    // `--version` names the network the build is bound to and the commit
    let (v, _) = exe(&["--version"]);
    assert!(v.starts_with("kobd ") && v.contains(" build") && v.contains("commit "), "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn offline_match_from_a_kron_snapshot_file() {
    // the JSON snapshot carries the KRON kinds and 46-byte KRON token states (`id_type`, `is_minter`)
    let dir = std::env::temp_dir().join(format!("kob-cli-kron-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let b = book(vec![l_bid(1, bid(1, P260, T3), 5 * WHOLE), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let k = to_kron(&input(&b));
    let kb = MemoryBook { daa_score: b.daa_score, orders: k.orders, wallet_tokens: vec![], keeper_only: vec![] };
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

/// Audit round 4 R4-L-6: a configuration file that does not parse, or is missing, is a mistake a restart does not cure: exit
/// status 2, which the sample systemd unit (RestartPreventExitStatus=2) and kobd-supervise.cmd do not start again.
#[test]
fn a_configuration_that_is_missing_or_does_not_parse_exits_with_2() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("kobd.toml");
    std::fs::write(&bad, "netwrok = \"testnet-10\"\n").unwrap();
    for (file, what) in [(&bad, "netwrok"), (&dir.path().join("none.toml"), "kobd setup")] {
        let (code, err) = exe_code(&["--executor", "--config", file.to_str().unwrap()]);
        assert_eq!(code, Some(2), "{err}");
        assert!(err.contains(what), "{err}");
        let (code, _) = exe_code(&["config", "check", "--config", file.to_str().unwrap()]);
        assert_eq!(code, Some(2));
    }
}
