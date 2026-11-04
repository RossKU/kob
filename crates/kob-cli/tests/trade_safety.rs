//! The trading commands as a user runs them (the `kob` binary): a key on the command line is refused and never echoed,
//! the network defaults to mainnet and a test network is chosen at run time, an unreachable node or indexer is reported with its URL redacted, and nothing is signed
//! without `--yes` or a terminal.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_kob");
const SK: &str = "3333333333333333333333333333333333333333333333333333333333333333";

fn kob(args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(BIN);
    cmd.args(args).env_remove("KOB_NODE_WRPC").env_remove("KOB_INDEXER_URL").env_remove("KOB_KEY");
    // no test reaches the public Kaspa resolvers: the only one nothing answers
    cmd.env("KOB_RESOLVERS", "http://127.0.0.1:1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

const PLACE: &[&str] =
    &["place", "limit", "--token", "TST", "--side", "sell", "--amount", "1TST", "--price", "1KAS", "--network", "testnet-10"];

#[test]
fn a_key_on_the_command_line_is_refused_and_never_echoed() {
    let mut a = PLACE.to_vec();
    a.extend(["--key", SK]);
    let (code, out) = kob(&a, &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--key-file") && !out.contains(SK), "{out}");
    // the key as a positional argument while the environment holds it
    let mut a = vec!["cancel", SK];
    a.extend(["--network", "testnet-10"]);
    let (code, out) = kob(&a, &[("KOB_KEY", SK)]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("command line") && !out.contains(SK), "{out}");
}

#[test]
fn the_network_defaults_to_mainnet_and_urls_are_redacted() {
    // no network named: mainnet, like kaspad; no node is named either: the public nodes (the plan of a command that gets
    // further names the network)
    let (code, out) =
        kob(&["place", "limit", "--token", "TST", "--side", "sell", "--amount", "1TST", "--price", "1KAS"], &[("KOB_KEY", SK)]);
    // the network is checked on the node first: the public nodes the (here unreachable) resolver names
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("no Kaspa resolver named a mainnet node") && !out.contains("--network is required"), "{out}");
    // the kaspad-style switches: --testnet and --netsuffix=10 are --network testnet-10 (here a view that needs a maker)
    for sw in [&["--testnet"][..], &["--netsuffix=10"], &["--testnet", "--netsuffix", "10"]] {
        let mut a = vec!["orders", "--indexer", "http://127.0.0.1:1"];
        a.extend(sw);
        let (code, out) = kob(&a, &[]);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("--maker"), "{out}");
    }
    let (code, out) = kob(&["orders", "--testnet", "--network", "mainnet"], &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("different networks"), "{out}");
    // an unreachable node / indexer with credentials: refused, redacted
    let (code, out) = kob(
        PLACE,
        &[
            ("KOB_KEY", SK),
            ("KOB_NODE_WRPC", "ws://alice:s3cret@127.0.0.1:1/k?apikey=Z"),
            ("KOB_INDEXER_URL", "http://bob:pw@127.0.0.1:1/api?token=T"),
        ],
    );
    assert_ne!(code, 0, "{out}");
    for secret in ["s3cret", "alice", "apikey", "bob", ":pw", "token=T", SK] {
        assert!(!out.contains(secret), "{secret} leaked: {out}");
    }
    // help works without anything
    let (code, out) = kob(&["place", "--help"], &[]);
    assert_eq!(code, 0);
    assert!(out.contains("--key-file") && out.contains("pair-limit") && out.contains("trailing-stop"), "{out}");
    for cmd in ["book", "orders", "balances", "merge"] {
        let (code, out) = kob(&[cmd, "--help"], &[]);
        assert_eq!(code, 0, "{cmd}: {out}");
    }
    // a view of nobody: no key and no --maker
    let (code, out) = kob(&["orders", "--network", "testnet-10", "--indexer", "http://127.0.0.1:1"], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("--maker"), "{out}");
}
