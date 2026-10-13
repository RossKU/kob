//! The `kob` binary (`kob-cli`) takes the node endpoint from `KOB_NODE_WRPC` when `--node` is not given (a URL with a
//! password or an API key stays off the command line, which other local users see in `ps`), and its messages name the node
//! redacted.

use std::process::Command;

use secp256k1::{Keypair, Secp256k1, SecretKey};

const BIN: &str = env!("CARGO_BIN_EXE_kob-cli");
const SK: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const TX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
/// Nothing listens on port 1: the submit fails after the transaction is built and signed.
const SECRET_URL: &str = "ws://alice:s3cret@127.0.0.1:1/apikey/K?token=T";

fn issue(env: Option<&str>, extra: &[&str]) -> (bool, String) {
    let sk = SecretKey::from_slice(&[0x33; 32]).unwrap();
    let pk = Keypair::from_secret_key(&Secp256k1::new(), &sk).x_only_public_key().0.serialize();
    let pk: String = pk.iter().map(|b| format!("{b:02x}")).collect();
    let fund = format!("{TX}:0:5000000000:{pk}");
    let mut cmd = Command::new(BIN);
    cmd.args(["token", "issue", "--name", "Test", "--ticker", "TEST", "--decimals", "8", "--supply", "5", "--network", "testnet-10"])
        .args(["--fund-utxo", &fund, "--key", SK])
        .args(extra)
        .env_remove("KOB_NODE_WRPC");
    if let Some(v) = env {
        cmd.env("KOB_NODE_WRPC", v);
    }
    let out = cmd.output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn the_node_comes_from_the_environment_and_is_shown_redacted() {
    // neither --node nor the variable: refused before anything is built
    let (ok, out) = issue(None, &[]);
    assert!(!ok && out.contains("KOB_NODE_WRPC"), "{out}");
    // the variable alone: the transaction is built, signed and submitted to that node (unreachable here)
    let (ok, out) = issue(Some(SECRET_URL), &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("genesis tx") && out.contains("ws://127.0.0.1:1/..."), "{out}");
    for secret in ["alice", "s3cret", "apikey", "token=T"] {
        assert!(!out.contains(secret), "{secret} leaked: {out}");
    }
    // --node wins over the variable
    let (ok, out) = issue(Some("ws://127.0.0.1:2"), &["--node", SECRET_URL]);
    assert!(!ok && out.contains("ws://127.0.0.1:1/...") && !out.contains("127.0.0.1:2"), "{out}");
    assert!(!out.contains("s3cret"), "{out}");
}
