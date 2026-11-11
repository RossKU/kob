//! The help rule and the exit codes of the `kob` binary: an explicit `--help` prints on stdout and exits 0, a usage printed
//! because of a mistake goes to stderr and exits 2, `kob help <command>` is `kob <command> --help`, and a mistake in the
//! arguments is exit 2 on the views too. Nothing here contacts a node or an indexer.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_kob");

/// Exit code, stdout and stderr of one run (no key, node or indexer from the environment).
fn kob(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .args(args)
        .env_remove("KOB_NODE_WRPC")
        .env_remove("KOB_INDEXER_URL")
        .env_remove("KOB_KEY")
        .env("KOB_RESOLVERS", "http://127.0.0.1:1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_is_reachable_every_way_and_prints_on_stdout() {
    for (a, b) in [
        (&["help", "book"][..], &["book", "--help"][..]),
        (&["help", "place"], &["place", "--help"]),
        (&["help", "token", "create"], &["token", "create", "--help"]),
        (&["help", "registry", "validate"], &["registry", "validate", "--help"]),
        (&["help", "token"], &["token", "--help"]),
    ] {
        let (ca, oa, ea) = kob(a);
        let (cb, ob, _) = kob(b);
        assert_eq!((ca, cb, &oa), (0, 0, &ob), "{a:?}: {ea}");
        assert!(!oa.is_empty() && ea.is_empty(), "{a:?}: {ea}");
    }
    for args in [&["token", "--help"][..], &["token", "-h"]] {
        let (code, out, err) = kob(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(out.contains("kob token create") && out.contains("kob token resume") && err.is_empty(), "{args:?}: {out}{err}");
    }
    for args in [&["registry", "--help"][..], &["registry", "-h"]] {
        let (code, out, err) = kob(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(out.contains("kob registry validate") && out.contains("verify-genesis") && err.is_empty(), "{args:?}: {out}{err}");
    }
    for args in [
        &["registry", "validate", "--help"][..],
        &["registry", "validate", "-h"],
        &["registry", "programs", "--help"],
        &["registry", "programs", "-h"],
        &["registry", "verify-genesis", "--bogus", "--help"],
    ] {
        let (code, out, err) = kob(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(out.contains("usage: kob registry") && err.is_empty(), "{args:?}: {out}{err}");
    }
    let (code, out, _) = kob(&["--help"]);
    assert_eq!(code, 0);
    for want in [
        "READ (no key)",
        "TRADE (a key",
        "TOKENS AND REGISTRY",
        "GLOBAL OPTIONS",
        "--key-file",
        "--dry-run",
        "crates/kob-cli/README.md",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    assert!(out.is_ascii(), "the help text is ASCII");
}

#[test]
fn a_usage_printed_because_of_a_mistake_goes_to_stderr_with_exit_2() {
    for args in [
        &["place"][..],
        &["watch"],
        &["order"],
        &["token"],
        &["registry"],
        &["token", "resume"],
        &["token", "nosuch"],
        &["registry", "nosuch"],
        &["order", "nosuch"],
        &["nosuch"],
        &["help", "nosuch"],
    ] {
        let (code, out, err) = kob(args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(out.is_empty(), "{args:?}: nothing on stdout, got {out}");
        assert!(err.contains("USAGE") || err.contains("usage"), "{args:?}: {err}");
    }
}

#[test]
fn argument_mistakes_of_the_views_are_exit_2() {
    // no token, a depth out of range, no owner: the user's mistake, not a failure of the indexer (1)
    for (args, want) in [
        (&["book", "--network", "testnet-10"][..], "exactly one token"),
        (&["book", "TUSD", "--depth", "0", "--network", "testnet-10"], "--depth"),
        (&["orders", "--network", "testnet-10"], "--maker"),
        (&["balances", "--network", "testnet-10"], "--maker"),
        (&["orders", "--maker", "nonsense", "--network", "testnet-10"], "nonsense"),
    ] {
        let (code, out, err) = kob(args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(err.contains("error:") && err.contains(want) && out.is_empty(), "{args:?}: {out}{err}");
    }
}

#[test]
fn the_version_line_names_the_build_and_the_commit() {
    for flag in ["--version", "-V"] {
        let (code, out, err) = kob(&[flag]);
        assert_eq!(code, 0, "{err}");
        let line = out.trim_end();
        let v = line.strip_prefix(&format!("kob {} (", env!("CARGO_PKG_VERSION"))).unwrap_or_else(|| panic!("{line}"));
        assert!(v.ends_with(')') && v.contains(" build, commit ") || v.contains("development build, no network, commit "), "{line}");
    }
}

#[test]
fn an_unreachable_indexer_is_an_english_sentence_with_exit_1() {
    let (code, out, err) = kob(&["book", "TUSD", "--indexer", "http://127.0.0.1:1", "--network", "testnet-10"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("cannot connect to the indexer http://127.0.0.1:1 ("), "{err}");
    assert!(err.contains("(connection refused)") || err.contains("(timed out)"), "{err}");
    assert_eq!(err.matches("127.0.0.1").count(), 1, "the host is named once: {err}");
    assert!(out.is_empty(), "{out}");
}
