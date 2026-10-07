//! Transport-level bounds of the read API and the x402 HTTP server (added 2026-10-06). Each test runs the
//! scenario against short configured deadlines and shows the bound holds.
//!
//! - read API: header-read deadline and connection caps (slow-header connections used to be held forever);
//! - x402: a per-address connection cap (one host used to hold every slot of the global cap) and a write-stall deadline
//!   (a client pipelining requests without reading used to park its slot forever);
//! - read API: IPv6 /64 rotation inside one site does not exhaust the global bucket;
//! - WebSocket: a session that only auto-answers server Pings is closed at the idle limit, one sending `{"op":"ping"}`
//!   stays.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use kob_executor::api::conn::ConnLimits;
use kob_executor::api::{self, ApiState};
use kob_executor::config::ApiConfig;
use kob_executor::indexer::db::{open_writer, ReadPool};
use kob_executor::indexer::status::HealthState;
use kob_executor::tokens::TokenAllowlist;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::ServiceExt;

struct Env {
    _dir: tempfile::TempDir,
    state: ApiState,
}

fn env(tweak: impl FnOnce(&mut ApiConfig)) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sqlite3");
    let _w = open_writer(&path, "testnet-10").unwrap();
    let pool = ReadPool::open(&path, 2).unwrap();
    let mut cfg = ApiConfig::default();
    tweak(&mut cfg);
    let (events, _) = tokio::sync::broadcast::channel(16);
    let state = ApiState {
        pool,
        health: Arc::new(HealthState::new("testnet-10")),
        events,
        tokens: Arc::new(TokenAllowlist::from_entries(std::iter::empty())),
        cfg,
        settle_depth_daa: 100,
        daa_per_second: 10,
        lag_alarm_hours: vec![1],
    };
    Env { _dir: dir, state }
}

/// `true` if the server closed the connection (EOF / reset, possibly after a short answer) within `wait`.
async fn closed_within(s: &mut TcpStream, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    let mut b = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, s.read(&mut b)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return true,
            Ok(Ok(_)) => continue, // an answer (e.g. the over-cap 503): keep reading until the close
            Err(_) => return false,
        }
    }
}

async fn get_status(addr: SocketAddr, path: &str) -> Option<String> {
    let mut s = TcpStream::connect(addr).await.ok()?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).lines().next().map(str::to_string)
}

// ----------------------------------------------------------------------------------------------------------------------
// Read API: slow-header connections are cut at the header deadline, and one address cannot open more than its cap.
#[tokio::test]
async fn api_slow_headers_are_cut_and_one_address_is_capped() {
    let e = env(|c| {
        c.header_timeout_ms = 400;
        c.max_connections_per_ip = 8;
        c.max_connections = 64;
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(api::serve_listener(listener, e.state.clone(), std::future::pending()));

    let mut conns = Vec::new();
    for _ in 0..12 {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = s.write_all(b"GET /v1/tokens HTTP/1.1\r\nHost: x\r\n").await;
        conns.push(s);
    }
    // the four over the per-address cap are answered 503 and closed at once, the first eight are held (for now)
    for s in conns[8..].iter_mut() {
        assert!(closed_within(s, Duration::from_millis(200)).await, "a connection over max_connections_per_ip is closed at once");
    }
    for s in conns[..8].iter_mut() {
        assert!(!closed_within(s, Duration::from_millis(5)).await, "a connection within the cap is held until its deadline");
    }
    // trickling header lines does not extend the deadline: every slow connection is gone after it
    for round in 0..3 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        for s in conns.iter_mut() {
            let _ = s.write_all(format!("X-Pad-{round}: a\r\n").as_bytes()).await;
        }
    }
    let mut still_open = 0;
    for s in conns.iter_mut() {
        if !closed_within(s, Duration::from_millis(600)).await {
            still_open += 1;
        }
    }
    assert_eq!(still_open, 0, "no slow-header connection outlives the header deadline");
    // the slots are free again: a normal request is served
    let line = get_status(addr, "/v1/tokens").await.unwrap_or_default();
    assert!(line.starts_with("HTTP/1.1 200"), "{line}");
    // an idle keep-alive connection is closed at the header deadline too
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /v1/tokens HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut b = [0u8; 4096];
    let n = s.read(&mut b).await.unwrap();
    assert!(String::from_utf8_lossy(&b[..n]).starts_with("HTTP/1.1 200"));
    assert!(closed_within(&mut s, Duration::from_millis(1500)).await, "idle keep-alive closed");
    server.abort();
}

// ----------------------------------------------------------------------------------------------------------------------
// x402 serve(): one host holding idle sockets fills only its own share; another address is still served.
fn tiny_app() -> Router {
    Router::new().route("/health", get(|| async { "x".repeat(200) }))
}

fn limits(max_connections: usize, max_per_ip: usize, header_ms: u64, write_ms: u64) -> ConnLimits {
    ConnLimits {
        header_timeout: Duration::from_millis(header_ms),
        write_timeout: Duration::from_millis(write_ms),
        max_connections,
        max_per_ip,
        ipv6_prefix_bits: 64,
        max_per_site: 0,
        ipv6_site_prefix_bits: 48,
        exempt: Default::default(),
    }
}

#[tokio::test]
async fn x402_one_host_cannot_hold_every_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = kob_executor::x402::http::serve(listener, tiny_app(), limits(16, 4, 5_000, 5_000), std::future::pending()).await;
    });
    let mut idle: Vec<TcpStream> = Vec::new();
    for _ in 0..4 {
        idle.push(TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // the same host's fifth is closed at once, never served (the best-effort 503 may be lost to the reset of a socket
    // closed with an unread request)
    let mut fifth = TcpStream::connect(addr).await.unwrap();
    let _ = fifth.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n").await;
    let mut got = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(1), fifth.read_to_end(&mut got)).await.is_ok();
    assert!(closed, "a connection over the per-address cap is closed");
    let head = String::from_utf8_lossy(&got);
    assert!(got.is_empty() || head.starts_with("HTTP/1.1 503"), "{head}");
    let mut b = vec![0u8; 1024];
    // another address (127.0.0.2 is loopback too) is served while the first host holds its share
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    if sock.bind("127.0.0.2:0".parse().unwrap()).is_ok() {
        let mut other = sock.connect(addr).await.unwrap();
        other.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(2), other.read(&mut b)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&b[..n]).starts_with("HTTP/1.1 200"));
    } else {
        println!("127.0.0.2 not bindable here: the second address is not exercised");
    }
    drop(idle);
    server.abort();
}

// x402 serve(): a client that pipelines requests and never reads the responses loses its slot at the write deadline.
#[tokio::test]
async fn x402_pipelining_non_reader_loses_its_slot() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    const CAP: usize = 4;
    let server = tokio::spawn(async move {
        let _ = kob_executor::x402::http::serve(listener, tiny_app(), limits(CAP, 0, 1_000, 300), std::future::pending()).await;
    });
    let req = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n".repeat(2048); // ~66 KB per chunk
    let mut held = Vec::new();
    for _ in 0..CAP {
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        let mut s = sock.connect(addr).await.unwrap();
        // write until the server stops reading (its send buffer is full of unread responses) or drops us
        let mut sent = 0usize;
        while sent < 16 * 1024 * 1024 {
            match tokio::time::timeout(Duration::from_millis(300), s.write_all(&req)).await {
                Ok(Ok(())) => sent += req.len(),
                _ => break,
            }
        }
        held.push(s);
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let line = get_status(addr, "/health").await.unwrap_or_default();
    assert!(line.starts_with("HTTP/1.1 200"), "the stalled connections were closed and a new client is served: {line:?}");
    drop(held);
    server.abort();
}

// ----------------------------------------------------------------------------------------------------------------------
// Read API rate limits: rotating the /64s of one IPv6 site does not exhaust the global bucket for everybody else.
async fn hit(router: &Router, ip: &str, path: &str) -> StatusCode {
    let mut req = Request::builder().uri(path).body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 40000)));
    router.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn api_ipv6_rotation_inside_one_site_does_not_starve_others() {
    let e = env(|_| {}); // default limits: 20 rps / 60 per /64, 100 rps / 300 per /48 site, 500 rps / 1000 global
    let router = api::router(e.state.clone());
    let mut passed = 0u32;
    for i in 0..3_000u32 {
        let ip = format!("2001:db8:200:{:x}::1", i % 128); // 128 /64s of one /48
        if hit(&router, &ip, "/v1/nope").await != StatusCode::TOO_MANY_REQUESTS {
            passed += 1;
        }
    }
    // without the site bucket all 3,000 pass (128 fresh 60-token /64 buckets) and empty the global bucket
    assert!(passed < 1_500, "the site bucket bounds the rotating attacker ({passed} passed)");
    for _ in 0..20 {
        assert_ne!(hit(&router, "198.51.100.7", "/v1/nope").await, StatusCode::TOO_MANY_REQUESTS, "another client is served");
    }
}

// ----------------------------------------------------------------------------------------------------------------------
// WebSocket: protocol Pongs do not keep a session alive; application pings do.
#[tokio::test]
async fn ws_session_without_application_messages_is_closed_at_the_idle_limit() {
    let e = env(|c| c.ws_idle_timeout_ms = 600);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(api::serve_listener(listener, e.state.clone(), std::future::pending()));

    // silent: never sends anything; driving the stream makes tungstenite auto-answer the server's Pings
    let (silent, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await.unwrap();
    let silent = tokio::spawn(async move {
        let (_tx, mut rx) = silent.split();
        let t0 = Instant::now();
        while let Some(m) = rx.next().await {
            if m.is_err() {
                break;
            }
        }
        t0.elapsed()
    });
    // chatty: an application ping every 150 ms
    let (chatty, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await.unwrap();
    let (mut tx, mut rx) = chatty.split();
    let chatty_rx = tokio::spawn(async move {
        while let Some(m) = rx.next().await {
            if m.is_err() {
                break;
            }
        }
    });
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(2_000) {
        tx.send(tokio_tungstenite::tungstenite::Message::Text(r#"{"op":"ping"}"#.into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let lived = tokio::time::timeout(Duration::from_secs(2), silent).await.expect("the silent session was closed").unwrap();
    assert!(lived < Duration::from_millis(1_500), "closed near the idle limit ({lived:?})");
    assert!(!chatty_rx.is_finished(), "a session sending application pings stays open");
    server.abort();
}
