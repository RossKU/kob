//! Node URLs that carry credentials: every flag that takes one has an environment (or configuration-file) alternative, so the
//! URL need not be on the command line (`ps` shows arguments to every local user), and what the process prints names the node
//! by its redacted URL (scheme, host and port).

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Command as ClapCommand};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_kob-executor");
const SECRETS: [&str; 4] = ["alice", "s3cret", "apikey", "token=T"];

/// `(flag, environment variable)` of every argument of `cmd` whose value is a node or indexer URL.
fn url_flags(cmd: ClapCommand) -> Vec<(String, Option<String>)> {
    cmd.get_arguments()
        .filter(|a| matches!(a.get_long(), Some("rpc-url" | "node")))
        .map(|a| (a.get_long().unwrap().to_string(), a.get_env().map(|e| e.to_string_lossy().into_owned())))
        .collect()
}

#[test]
fn every_node_url_flag_has_an_environment_alternative() {
    use kob_executor::index_cli::IndexOpts;
    use kob_executor::matcher::cli::CommonArgs;
    use kob_executor::x402::X402Args;
    let want = |cmd: ClapCommand, expect: &[(&str, &str)]| {
        let got = url_flags(cmd);
        let expect: Vec<(String, Option<String>)> = expect.iter().map(|(f, e)| (f.to_string(), Some(e.to_string()))).collect();
        assert_eq!(got, expect);
    };
    // `index` and `run` (the indexer options; `run`'s facilitator uses the indexer's node)
    want(IndexOpts::augment_args(ClapCommand::new("index")), &[("rpc-url", "KOB_NODE_WRPC"), ("node", "KOB_INDEX_NODES")]);
    // `match` and `keep`
    want(CommonArgs::augment_args(ClapCommand::new("match")), &[("rpc-url", "KOB_NODE_WRPC")]);
    // `x402`
    want(X402Args::augment_args(ClapCommand::new("x402")), &[("node", "KOB_NODE_WRPC")]);
}

/// A node that answers `getServerInfo` as a mainnet node.
fn mainnet_node() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let Ok(mut ws) = tungstenite::accept(stream) else { return };
                while let Ok(msg) = ws.read() {
                    let Ok(req) = serde_json::from_str::<Value>(msg.to_text().unwrap_or("")) else { continue };
                    let reply = json!({
                        "id": req["id"],
                        "params": { "networkId": "mainnet", "hasUtxoIndex": true, "isSynced": true, "virtualDaaScore": 7 }
                    });
                    if ws.send(tungstenite::Message::text(reply.to_string())).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `cmd` with stdout and stderr captured, until it exits or `until` matches a line or `timeout` passes; returns
/// (exited successfully, everything printed).
fn run_until(mut cmd: Command, until: &str, timeout: Duration) -> (Option<bool>, String) {
    let mut child = Child(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap());
    let (tx, rx) = mpsc::channel::<String>();
    for r in [Box::new(child.0.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>, Box::new(child.0.stderr.take().unwrap())] {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(r).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    drop(tx);
    let mut out = String::new();
    let t = Instant::now();
    let mut seen_at = None;
    let mut closed = false;
    while t.elapsed() < timeout {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(l) => {
                out.push_str(&l);
                out.push('\n');
                if !until.is_empty() && l.contains(until) && seen_at.is_none() {
                    seen_at = Some(Instant::now());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                closed = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // after the awaited line, a little longer: the connection attempts that follow it are logged too
        if seen_at.is_some_and(|s| s.elapsed() > Duration::from_secs(2)) {
            break;
        }
    }
    // both pipes closed: the process is exiting, give it a moment to be reaped (a Windows runner can lag behind the pipes)
    let reap = Instant::now();
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(s)) => break Some(s.success()),
            Ok(None) if closed && reap.elapsed() < Duration::from_secs(10) => thread::sleep(Duration::from_millis(50)),
            _ => break None,
        }
    };
    (status, out)
}

#[test]
fn the_facilitator_takes_its_node_from_the_environment() {
    let port = mainnet_node();
    let url = format!("ws://alice:s3cret@127.0.0.1:{port}/apikey/K?token=T");
    let mut cmd = Command::new(BIN);
    cmd.args(["x402", "--network", "kaspa:testnet-10", "--auth", "open", "--open-auth-no-proxy"])
        .args(["--listen", "127.0.0.1:0", "--ledger", ":memory:"])
        .env("KOB_NODE_WRPC", &url)
        .env_remove("KOB_X402_CONFIG")
        .env("RUST_LOG", "info")
        .env("NO_COLOR", "1");
    let (status, out) = run_until(cmd, "", Duration::from_secs(60));
    // the node of the variable (a mainnet node) is refused; the default local node would only have been a warning
    assert_eq!(status, Some(false), "{out}");
    assert!(out.contains("mainnet"), "{out}");
    for s in SECRETS {
        assert!(!out.contains(s), "{s} leaked: {out}");
    }
}

#[test]
fn the_matcher_takes_its_node_from_the_environment_and_logs_it_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path().join("book.json");
    let mut cmd = Command::new(BIN);
    cmd.args(["match", "--network", "testnet-10", "--dry-run", "--book-file", book.to_str().unwrap()])
        .env("KOB_NODE_WRPC", "ws://alice:s3cret@127.0.0.1:1/apikey/K?token=T")
        .env("KOB_OPERATOR_KEY", "33".repeat(32))
        .env_remove("KOB_OPERATOR_KEY_FILE")
        .env_remove("CREDENTIALS_DIRECTORY")
        .env("RUST_LOG", "info")
        .env("NO_COLOR", "1");
    let (_, out) = run_until(cmd, "matcher starting", Duration::from_secs(60));
    let line = out.lines().find(|l| l.contains("matcher starting")).unwrap_or_else(|| panic!("no start line: {out}"));
    assert!(line.contains("ws://127.0.0.1:1/..."), "{line}");
    for s in SECRETS {
        assert!(!out.contains(s), "{s} leaked: {out}");
    }
}
