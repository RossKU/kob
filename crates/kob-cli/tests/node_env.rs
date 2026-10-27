//! The `kob` binary (`kob-cli`) takes the node endpoint from `KOB_NODE_WRPC` when `--node` is not given (a URL with a
//! password or an API key stays off the command line, which other local users see in `ps`), and its messages name the node
//! redacted.

use std::process::{Command, Stdio};

use secp256k1::{Keypair, Secp256k1, SecretKey};

const BIN: &str = env!("CARGO_BIN_EXE_kob-cli");
const SK: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const TX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
/// Nothing listens on port 1: the submit fails after the transaction is built and signed.
const SECRET_URL: &str = "ws://alice:s3cret@127.0.0.1:1/apikey/K?token=T";

fn issue(env: Option<&str>, extra: &[&str]) -> (bool, String) {
    issue_in(env, extra).1
}

/// The run itself: its working directory (where a submitting run keeps its files) and the outcome.
fn issue_in(env: Option<&str>, extra: &[&str]) -> (std::path::PathBuf, (bool, String)) {
    let sk = SecretKey::from_slice(&[0x33; 32]).unwrap();
    let pk = Keypair::from_secret_key(&Secp256k1::new(), &sk).x_only_public_key().0.serialize();
    let pk: String = pk.iter().map(|b| format!("{b:02x}")).collect();
    let fund = format!("{TX}:0:5000000000:{pk}");
    // a submitting run keeps its signed creation in ./kob-create-<id16>: run where that lands outside the source tree
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let cwd = std::env::temp_dir().join(format!("kob-node-env-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let mut cmd = Command::new(BIN);
    cmd.current_dir(&cwd);
    cmd.args(["token", "issue", "--name", "Test", "--ticker", "TEST", "--decimals", "8", "--supply", "5", "--network", "testnet-10"])
        .args(["--fund-utxo", &fund, "--key", SK])
        .args(extra)
        .stdin(Stdio::null())
        .env_remove("KOB_NODE_WRPC");
    if let Some(v) = env {
        cmd.env("KOB_NODE_WRPC", v);
    }
    let out = cmd.output().unwrap();
    (cwd, (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
}

#[test]
fn the_node_comes_from_the_environment_and_is_shown_redacted() {
    // neither --node nor the variable: refused before anything is built
    let (ok, out) = issue(None, &[]);
    assert!(!ok && out.contains("KOB_NODE_WRPC"), "{out}");
    // the variable alone: the transaction is built, signed and submitted to that node (unreachable here)
    let (ok, out) = issue(Some(SECRET_URL), &["--yes"]);
    assert!(!ok, "{out}");
    assert!(out.contains("genesis tx") && out.contains("ws://127.0.0.1:1/..."), "{out}");
    for secret in ["alice", "s3cret", "apikey", "token=T"] {
        assert!(!out.contains(secret), "{secret} leaked: {out}");
    }
    // --node wins over the variable
    let (ok, out) = issue(Some("ws://127.0.0.1:2"), &["--node", SECRET_URL, "--yes"]);
    assert!(!ok && out.contains("ws://127.0.0.1:1/...") && !out.contains("127.0.0.1:2"), "{out}");
    assert!(!out.contains("s3cret"), "{out}");
}

/// Without `--yes` and without a terminal `kob token issue` shows the plan, signs and writes nothing, submits nothing and exits 1;
/// `--yes` together with `--dry-run` is refused.
#[test]
fn token_issue_refuses_without_yes_when_there_is_no_terminal() {
    // the node is never contacted: the refusal comes first
    let (cwd, (ok, out)) = issue_in(Some("ws://127.0.0.1:1"), &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("genesis tx") && out.contains("not signed: pass --yes"), "{out}");
    let kept: Vec<_> =
        std::fs::read_dir(&cwd).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("kob-create-")).collect();
    assert!(kept.is_empty(), "nothing written: {kept:?}");
    // a dry run needs no --yes; both together are a contradiction
    let (ok, out) = issue(Some("ws://127.0.0.1:1"), &["--dry-run"]);
    assert!(ok, "{out}");
    let (ok, out) = issue(Some("ws://127.0.0.1:1"), &["--dry-run", "--yes"]);
    assert!(!ok && out.contains("exclude each other"), "{out}");
}
