//! `kobd logo-reports`: the administrator's side of the token-logo blocklist, run as the binary against a database file
//! (next to a running executor it is a short write to the same file).

use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_writer;
use kob_executor::logo_reports::{blocklist, file_report, Reason, Report, Source};

const COOLDOWN_MS: i64 = 21_600_000;

fn network() -> &'static str {
    kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10")
}

fn run(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_kobd"))
        .args(["logo-reports", "--network", network(), "--data-dir", dir.to_str().unwrap()])
        .args(args)
        .env_remove("KOB_NETWORK")
        .env_remove("KOB_NODE_WRPC")
        .env_remove("KOB_INDEX_CONFIG")
        .env("RUST_LOG", "error")
        .output()
        .unwrap()
}

fn ok(dir: &std::path::Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn list_restore_hide_and_export_import() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Hash32([0xaa; 32]), Hash32([0xbb; 32]));
    {
        let mut conn = open_writer(&dir.path().join("index.sqlite3"), network()).unwrap();
        let r = |id: Hash32, source, reference| Report { covenant_id: id, reason: Reason::Illegal, source, reference };
        file_report(&mut conn, &r(a, Source::Web, None), 1_000, COOLDOWN_MS).unwrap();
        file_report(&mut conn, &r(a, Source::Github, Some(41)), 2_000, COOLDOWN_MS).unwrap();
        file_report(&mut conn, &r(b, Source::Web, None), 3_000, COOLDOWN_MS).unwrap();
    }
    let rows: serde_json::Value = serde_json::from_str(&ok(dir.path(), &["list", "--json"])).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    let row_a = rows.as_array().unwrap().iter().find(|r| r["covenant_id"] == a.to_hex()).unwrap();
    assert_eq!((row_a["reports"].as_i64(), row_a["state"].as_str(), row_a["issues"][0].as_i64()), (Some(2), Some("hidden"), Some(41)));
    let table = ok(dir.path(), &["list"]);
    assert!(table.contains(&a.to_hex()) && table.contains("#41"), "{table}");

    // restore one, hide the other for good; a second restore of a restored token and of a never-listed one fail with a reason
    ok(dir.path(), &["restore", &a.to_hex()]);
    ok(dir.path(), &["hide", &b.to_hex()]);
    let again = run(dir.path(), &["restore", &a.to_hex()]);
    assert!(!again.status.success() && String::from_utf8_lossy(&again.stderr).contains("already restored"));
    let never = run(dir.path(), &["restore", &b.to_hex()]);
    assert!(!never.status.success() && String::from_utf8_lossy(&never.stderr).contains("--force"));
    let unknown = run(dir.path(), &["restore", &Hash32([0xcc; 32]).to_hex()]);
    assert!(!unknown.status.success() && String::from_utf8_lossy(&unknown.stderr).contains("not in the blocklist"));
    let bad = run(dir.path(), &["restore", "nothex"]);
    assert!(!bad.status.success());
    {
        let conn = open_writer(&dir.path().join("index.sqlite3"), network()).unwrap();
        assert_eq!(blocklist(&conn).unwrap().0, vec![b.to_hex()], "a is shown again, b stays hidden");
    }

    // export, rebuild into a new directory, import
    let export = ok(dir.path(), &["export"]);
    let fresh = tempfile::tempdir().unwrap();
    open_writer(&fresh.path().join("index.sqlite3"), network()).unwrap();
    let file = fresh.path().join("logo.json");
    std::fs::write(&file, &export).unwrap();
    assert!(ok(fresh.path(), &["import", file.to_str().unwrap()]).contains("imported 2 listed token(s) and 3 report(s)"));
    assert_eq!(ok(fresh.path(), &["export"]), export);
    // restore --force undoes a permanent hide
    ok(fresh.path(), &["restore", &b.to_hex(), "--force"]);
    let conn = open_writer(&fresh.path().join("index.sqlite3"), network()).unwrap();
    assert!(blocklist(&conn).unwrap().0.is_empty());
}

#[test]
fn a_missing_database_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["list"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("does not exist"));
}
