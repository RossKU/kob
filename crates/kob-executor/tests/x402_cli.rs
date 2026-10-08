//! The `kob-executor x402` binary: argument and configuration errors, startup against a mock node,
//! and the public routes over a real socket.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_kob-executor");

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn unknown_role_and_bad_arguments_fail() {
    let out = Command::new(BIN).arg("nonsense").output().unwrap();
    assert!(!out.status.success());
    let out = Command::new(BIN).args(["x402", "--bogus"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn strict_config_is_enforced_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");
    std::fs::write(&cfg, r#"{"auth":"open","bogusField":1}"#).unwrap();
    let out = Command::new(BIN).args(["x402", "--config", cfg.to_str().unwrap()]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("bogusField"), "{}", String::from_utf8_lossy(&out.stderr));
    // open auth on a public address is refused
    let out = Command::new(BIN).args(["x402", "--auth", "open", "--listen", "0.0.0.0:0", "--ledger", ":memory:"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("loopback"));
    // open auth on loopback without the operator's statement that nothing relays to it
    let out = Command::new(BIN).args(["x402", "--auth", "open", "--listen", "127.0.0.1:0", "--ledger", ":memory:"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("openAuthNoProxy"), "{}", String::from_utf8_lossy(&out.stderr));
    // required auth without merchants
    let out = Command::new(BIN).args(["x402", "--ledger", ":memory:"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("merchant"));
}

/// A node that answers `getServerInfo` on every connection.
fn mock_node(network_id: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let Ok(mut ws) = tungstenite::accept(stream) else { return };
                while let Ok(msg) = ws.read() {
                    let Ok(text) = msg.to_text() else { continue };
                    let Ok(req) = serde_json::from_str::<Value>(text) else { continue };
                    let reply = match req["method"].as_str() {
                        Some("getServerInfo") => json!({
                            "id": req["id"],
                            "params": { "networkId": network_id, "hasUtxoIndex": true, "isSynced": true, "virtualDaaScore": 7 }
                        }),
                        _ => json!({ "id": req["id"], "error": { "code": 0, "message": "unsupported", "data": null } }),
                    };
                    if ws.send(tungstenite::Message::text(reply.to_string())).is_err() {
                        break;
                    }
                }
            });
        }
    });
    url
}

fn http(port: u16, req: &str) -> String {
    try_http(port, req).expect("the facilitator accepts connections")
}

/// One request; `None` when nothing listens on the port.
fn try_http(port: u16, req: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(req.as_bytes()).ok()?;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    Some(out)
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts the facilitator on a free port and waits until it answers `GET /health` itself. A port from [`free_port`] is free only
/// when it is picked: a listener of a test running in parallel (a mock node binds port 0) can take it before the facilitator binds,
/// and that listener accepts connections too. Then the facilitator exits (address in use) and it is started again on another port.
fn start_facilitator(node: &str, ledger: &str) -> (Child, u16) {
    for _ in 0..5 {
        let port = free_port();
        let mut child = Child(
            Command::new(BIN)
                .args([
                    "x402",
                    "--auth",
                    "open",
                    "--open-auth-no-proxy",
                    "--node",
                    node,
                    "--listen",
                    &format!("127.0.0.1:{port}"),
                    "--ledger",
                    ledger,
                ])
                .stderr(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(60) {
            if let Ok(Some(_)) = child.0.try_wait() {
                break;
            }
            if try_http(port, "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .is_some_and(|r| r.contains("\"status\":\"ok\""))
            {
                return (child, port);
            }
            thread::sleep(Duration::from_millis(50));
        }
        if child.0.try_wait().ok().flatten().is_none() {
            panic!("the facilitator did not start");
        }
    }
    panic!("the facilitator exited early on every port tried");
}

#[test]
fn the_service_starts_against_a_node_and_serves_the_public_routes() {
    let node = mock_node("testnet-10");
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.jsonl");
    let (_child, port) = start_facilitator(&node, ledger.to_str().unwrap());
    let r = http(port, "GET /supported HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.contains("kaspa-exact-v2") && r.contains("kaspa:testnet-10"));
    let r = http(port, "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(r.contains("\"status\":\"ok\""));
    let r = http(port, "POST /reserve HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    assert!(r.starts_with("HTTP/1.1 404"), "{r}");
    // metrics from loopback
    let r = http(port, "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(r.contains("kob_x402_settle_requests"), "{r}");
    // the durable ledger file exists
    assert!(ledger.exists());
}

#[test]
fn a_node_on_another_network_is_refused_at_startup() {
    let node = mock_node("mainnet");
    let out = Command::new(BIN)
        .args([
            "x402",
            "--auth",
            "open",
            "--open-auth-no-proxy",
            "--node",
            &node,
            "--listen",
            &format!("127.0.0.1:{}", free_port()),
            "--ledger",
            ":memory:",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("mainnet"), "{}", String::from_utf8_lossy(&out.stderr));
}
