//! The two roles at the command line: `setup --executor | --indexer | --executor --indexer`, the removed `--role` (refused), and a start
//! with role flags against a configuration set up for other roles. The binary end to end; nothing here contacts a node.

use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kobd"))
        .args(args)
        .env("KOBD_DIR", dir)
        .env_remove("KOB_OPERATOR_KEY")
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env_remove("KOB_NETWORK")
        .env_remove("KOB_NODE_WRPC")
        .env("RUST_LOG", "error")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn network() -> &'static str {
    kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10")
}

/// `setup --non-interactive` with `extra`; the JSON object it prints and what it wrote to stderr.
fn setup(dir: &Path, extra: &[&str]) -> (serde_json::Value, String) {
    let mut args = vec!["setup", "--non-interactive", "--node", "resolver", "--new-key", "--network", network()];
    args.extend_from_slice(extra);
    let out = run(dir, &args);
    assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    (serde_json::from_str(text(&out.stdout).trim()).unwrap(), text(&out.stderr))
}

fn config(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("kobd.toml")).unwrap()
}

#[test]
fn setup_takes_the_role_flags_and_writes_each_roles_configuration() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();

    let (v, err) = setup(dir, &["--executor"]);
    assert_eq!((v["roles"].clone(), v["role"].as_str()), (serde_json::json!(["executor"]), Some("run")));
    assert!(v["address"].is_string() && !err.contains("deprecated"), "{err}");
    let c = config(dir);
    assert!(c.contains("# Roles: Executor.") && c.contains("lean = true") && c.contains("fast_start = true"), "{c}");
    assert!(c.contains("role = \"run\"") && c.contains("key_file = \"kob-operator.key\""), "{c}");

    let (v, _) = setup(dir, &["--indexer"]);
    assert_eq!(
        (v["roles"].clone(), v["role"].as_str(), v["address"].is_null()),
        (serde_json::json!(["indexer"]), Some("index"), true)
    );
    let c = config(dir);
    assert!(c.contains("# Roles: Indexer.") && !c.contains("lean") && !c.contains("key_file") && !c.contains("fast_start"), "{c}");

    // both: the Executor's launch over a full store with the record log (its default), no lean cap
    let (v, _) = setup(dir, &["--executor", "--indexer"]);
    assert_eq!((v["roles"].clone(), v["role"].as_str()), (serde_json::json!(["executor", "indexer"]), Some("run")));
    let c = config(dir);
    assert!(c.contains("# Roles: Executor and Indexer.") && c.contains("lean = false") && c.contains("fast_start = true"), "{c}");
    assert!(c.contains("role = \"run\"") && c.contains("key_file = \"kob-operator.key\""), "{c}");
    assert!(!c.contains("[records]"), "the record log stays at its default (on): {c}");
}

#[test]
fn the_role_option_of_earlier_builds_is_an_unknown_option() {
    let d = tempfile::tempdir().unwrap();
    for role in ["match", "index", "index,match"] {
        let out =
            run(d.path(), &["setup", "--non-interactive", "--node", "resolver", "--new-key", "--network", network(), "--role", role]);
        assert!(!out.status.success(), "--role {role}");
        let err = text(&out.stderr);
        assert!(err.contains("unexpected argument '--role'"), "{err}");
    }
    assert!(!d.path().join("kobd.toml").exists() && !d.path().join("kob-operator.key").exists());
}

#[test]
fn a_non_interactive_setup_without_a_role_is_an_error_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let out = run(d.path(), &["setup", "--non-interactive", "--node", "resolver", "--new-key", "--network", network()]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("needs a role: --executor, --indexer or both"), "{err}");
    assert!(!d.path().join("kobd.toml").exists() && !d.path().join("kob-operator.key").exists());
    // an unknown or empty old-style role, and the old and the new spelling together
    for role in ["", "keep", "index,", "executor"] {
        let out = run(d.path(), &["setup", "--non-interactive", "--node", "resolver", "--role", role]);
        assert!(!out.status.success(), "--role {role:?}");
    }
    let out = run(d.path(), &["setup", "--non-interactive", "--node", "resolver", "--role", "match", "--executor"]);
    assert!(!out.status.success());
    assert!(!d.path().join("kobd.toml").exists());
}

#[test]
fn a_start_with_role_flags_needs_a_configuration_of_exactly_those_roles() {
    let d = tempfile::tempdir().unwrap();
    // no configuration and no terminal: nobody can answer the setup
    let out = run(d.path(), &["--executor"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("no kobd.toml") && err.contains("no terminal"), "{err}");
    // a configuration of the Executor does not start as the Indexer, nor as both
    setup(d.path(), &["--executor"]);
    for flags in [&["--indexer"][..], &["--executor", "--indexer"][..]] {
        let out = run(d.path(), flags);
        assert!(!out.status.success(), "{flags:?}");
        let err = text(&out.stderr);
        assert!(err.contains("is set up for the Executor but the command line asks for the"), "{flags:?}: {err}");
        assert!(err.contains("kobd setup"), "{err}");
    }
    setup(d.path(), &["--executor", "--indexer"]);
    let err = text(&run(d.path(), &["--executor"]).stderr);
    assert!(err.contains("is set up for the Executor and Indexer but the command line asks for the Executor"), "{err}");
    setup(d.path(), &["--indexer"]);
    let err = text(&run(d.path(), &["--executor", "--indexer"]).stderr);
    assert!(err.contains("is set up for the Indexer but the command line asks for the Executor and Indexer"), "{err}");
    // the same check for a configuration named with --config, from another directory
    let other = tempfile::tempdir().unwrap();
    let cfg = d.path().join("kobd.toml");
    let err = text(&run(other.path(), &["--executor", "--config", cfg.to_str().unwrap()]).stderr);
    assert!(err.contains("is set up for the Indexer but the command line asks for the Executor"), "{err}");
    // the Indexer's configuration takes no option of the Executor
    let err = text(&run(d.path(), &["--dry-run"]).stderr);
    assert!(err.contains("the Indexer takes the indexer's options only") && err.contains("--dry-run"), "{err}");
    // a plain --config file names no roles: the command line must
    let plain = other.path().join("plain.toml");
    std::fs::write(&plain, format!("network = \"{}\"\n", network())).unwrap();
    let err = text(&run(other.path(), &["--config", plain.to_str().unwrap()]).stderr);
    assert!(err.contains("has no [launch] table") && err.contains("kobd --executor --config"), "{err}");
}

#[test]
fn the_help_names_the_roles() {
    let d = tempfile::tempdir().unwrap();
    for args in [&["--help"][..], &["setup", "--help"][..]] {
        let out = run(d.path(), args);
        let help = text(&out.stdout);
        assert!(out.status.success() && help.contains("--executor") && help.contains("--indexer"), "{args:?}: {help}");
    }
    let help = text(&run(d.path(), &["setup", "--help"]).stdout);
    assert!(!help.contains("--role"), "no --role: {help}");
}
