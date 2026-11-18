//! Tests the removed spellings of the `kob` binary: each is a usage error (exit 2) that names what
//! replaces it, and none echoes a key. Nothing here contacts a node or an indexer.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_kob");
const SK: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const TX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const PK: &str = "aa2c17c5f5e5d5a3b6c0d2f1e4b7a9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2";

/// Exit code and stderr of one run (no key, node or indexer from the environment).
fn kob(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN)
        .args(args)
        .env_remove("KOB_NODE_WRPC")
        .env_remove("KOB_INDEXER_URL")
        .env_remove("KOB_KEY")
        .env("KOB_RESOLVERS", "http://127.0.0.1:1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// `kob token create` with the arguments that make it a complete dry run, and `extra`.
fn create(extra: &[&str]) -> (i32, String) {
    let fund = format!("{TX}:0:5000000000:{PK}");
    let mut a = vec![
        "token",
        "create",
        "--name",
        "Test",
        "--ticker",
        "TEST",
        "--decimals",
        "8",
        "--supply",
        "5",
        "--network",
        "testnet-10",
        "--dry-run",
        "--fund-utxo",
        &fund,
    ];
    a.extend(extra);
    kob(&a)
}

#[test]
fn token_issue_is_removed_for_token_create() {
    let (code, err) = kob(&["token", "issue", "--name", "X"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("`kob token issue` was removed") && err.contains("`kob token create`"), "{err}");
}

#[test]
fn a_key_on_the_command_line_is_removed_everywhere_and_never_echoed() {
    let eq = format!("--key={SK}");
    let envf = "env:KOB_KEY".to_string();
    let filef = "file:k.txt".to_string();
    for (name, args) in [
        ("token create hex", create(&["--key", SK])),
        ("token create =", create(&[&eq])),
        ("token create env:", create(&["--key", &envf])),
        ("token create file:", create(&["--key", &filef])),
        ("recover", kob(&["recover", "--from", "a.json", "--cancel", "--key", SK])),
        ("recover =", kob(&["recover", "--from", "a.json", "--cancel", &eq])),
        ("cancel", kob(&["cancel", "--view", "a.json", "--key", SK, "--network", "testnet-10"])),
        ("sweep", kob(&["sweep", "--view", "a.json", "--key", SK, "--network", "testnet-10"])),
        ("place", kob(&["place", "limit", "--key", SK, "--network", "testnet-10"])),
    ] {
        let (code, err) = args;
        assert_eq!(code, 2, "{name}: {err}");
        assert!(err.contains("--key was removed") && err.contains("--key-file") && err.contains("--key-env"), "{name}: {err}");
        assert!(!err.contains(SK), "{name}: the key is echoed: {err}");
    }
}

#[test]
fn the_image_spellings_and_the_prototype_program_are_removed() {
    for old in ["--logo", "--icon"] {
        let (code, err) = create(&[old, "https://e.org/a.png"]);
        assert_eq!(code, 2, "{old}: {err}");
        assert!(err.contains(&format!("{old} was removed")) && err.contains("--image"), "{old}: {err}");
    }
    // --image itself works
    let (code, err) = create(&["--image", "https://e.org/a.png"]);
    assert_eq!(code, 0, "{err}");
    for name in ["8x8-prototype", "8x8", "kcc20-ref-8x8"] {
        let (code, err) = create(&["--program", name]);
        assert_eq!(code, 2, "{name}: {err}");
        assert!(err.contains("no longer issued") && err.contains("public-mint"), "{name}: {err}");
    }
    // the programs that stay
    for name in ["public-mint", "3x3"] {
        let (code, err) = create(&["--program", name]);
        assert_eq!(code, 0, "{name}: {err}");
    }
    let (code, err) = create(&["--program", "8x8-prototype", "--help"]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn kob_order_is_removed_for_sweep_and_cancel_with_view() {
    for (args, want) in [
        (&["order", "sweep", "--view", "a.json"][..], "kob sweep --view FILE"),
        (&["order", "cancel", "--view", "a.json"], "kob cancel --view FILE"),
        (&["order"], "kob sweep or kob cancel"),
        (&["order", "nosuch"], "kob sweep or kob cancel"),
    ] {
        let (code, err) = kob(args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("was removed") && err.contains(want), "{args:?}: {err}");
    }
}

#[test]
fn add_program_is_not_in_the_user_cli() {
    let (code, err) = kob(&["registry", "add-program", "00"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("`kob registry add-program` was removed") && err.contains("kob-maint add-program"), "{err}");
    // the help pages no longer list it, the rest of the family stays
    let out = Command::new(BIN).args(["registry", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(!help.contains("add-program") && help.contains("verify-genesis") && help.contains("programs"), "{help}");
    let out = Command::new(BIN).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(!help.contains("add-program") && !help.contains("kob order sweep"), "{help}");
}
