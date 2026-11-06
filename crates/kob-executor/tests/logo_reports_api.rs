//! The token-logo blocklist on a real socket next to a running indexer: a report hides the logo, the administrator's subcommand (a separate process,
//! writing the same database file) restores it, and the list the web app reads follows. Also: the feature is off by default.

use kob_executor::config::IndexerConfig;
use kob_executor::indexer::Indexer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "6dab14d380a53585391106f3e951842ad91b743b72c93c68961bc9952d5b7d50";

fn network() -> &'static str {
    kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10")
}

async fn http(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len());
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        let mut out = String::new();
        let mut rest = body;
        while let Some((len, tail)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(len.trim(), 16).unwrap_or(0);
            if n == 0 {
                break;
            }
            out.push_str(&tail[..n]);
            rest = &tail[n + 2..];
        }
        out
    } else {
        body.to_string()
    };
    (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

fn config(dir: &std::path::Path, enabled: bool) -> IndexerConfig {
    let mut cfg = IndexerConfig { network: network().into(), data_dir: dir.join("data"), ..Default::default() };
    cfg.api.rate_limit.per_ip_rps = 0.0;
    cfg.logo_reports.enabled = enabled;
    cfg
}

#[tokio::test]
async fn off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let idx = Indexer::open(config(dir.path(), false)).unwrap();
    let state = idx.api_state().unwrap();
    assert!(state.logo.is_none());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        kob_executor::api::serve_listener(listener, state, async move {
            let _ = stop_rx.await;
        })
        .await
    });
    assert_eq!(http(addr, "GET", "/v1/logo-blocklist", "").await.0, 404);
    assert_eq!(http(addr, "POST", "/v1/logo-reports", &format!("{{\"covenant_id\":\"{ID}\",\"reason\":\"other\"}}")).await.0, 404);
    let _ = stop_tx.send(());
    let _ = server.await;
}

#[tokio::test]
async fn a_report_hides_and_the_administrators_subcommand_restores_next_to_the_running_indexer() {
    let dir = tempfile::tempdir().unwrap();
    let idx = Indexer::open(config(dir.path(), true)).unwrap();
    let state = idx.api_state().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        kob_executor::api::serve_listener(listener, state, async move {
            let _ = stop_rx.await;
        })
        .await
    });

    let (s, v) = http(addr, "GET", "/v1/logo-blocklist", "").await;
    assert_eq!((s, v["hidden"].as_array().map(Vec::len), v["updated_ms"].as_i64()), (200, Some(0), Some(0)));
    let (s, v) = http(addr, "POST", "/v1/logo-reports", &format!("{{\"covenant_id\":\"{ID}\",\"reason\":\"impersonation\"}}")).await;
    assert_eq!((s, v["result"].as_str()), (200, Some("hidden")), "{v}");
    let (_, v) = http(addr, "GET", "/v1/logo-blocklist", "").await;
    assert_eq!(v["hidden"], serde_json::json!([ID]));

    // the administrator: a separate process, the same database file, while the indexer holds it
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kobd"))
        .args(["logo-reports", "--network", network(), "--data-dir"])
        .arg(dir.path().join("data"))
        .args(["restore", ID])
        .env_remove("KOB_NETWORK")
        .env_remove("KOB_INDEX_CONFIG")
        .env("RUST_LOG", "error")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // the list is served from a body rendered at most every 5 seconds
    tokio::time::sleep(std::time::Duration::from_millis(5_200)).await;
    let (_, v) = http(addr, "GET", "/v1/logo-blocklist", "").await;
    assert_eq!(v["hidden"], serde_json::json!([]));
    // a report inside the cooldown is only recorded
    let (_, v) = http(addr, "POST", "/v1/logo-reports", &format!("{{\"covenant_id\":\"{ID}\",\"reason\":\"other\"}}")).await;
    assert_eq!(v["result"], "recorded");
    let _ = stop_tx.send(());
    let _ = server.await;
}
