//! End to end: mock node -> follower -> database -> REST and WebSocket on a real socket, with real
//! protocol v3 transactions.

mod common;

use common::*;
use futures_util::{SinkExt, StreamExt};
use kob_executor::config::{IndexerConfig, StartMode};
use kob_executor::hex::Hash32;
use kob_executor::indexer::follower::StepOutcome;
use kob_executor::indexer::Indexer;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

async fn http_get(addr: std::net::SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    // chunked bodies: strip chunk framing if present
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

async fn settle<S: kob_executor::rpc::ChainSource>(f: &kob_executor::indexer::follower::Follower<S>) {
    loop {
        if matches!(f.step().await, StepOutcome::Idle) {
            break;
        }
    }
}

#[tokio::test]
async fn rest_and_websocket_follow_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let tokens_path = dir.path().join("tokens.json");
    std::fs::write(&tokens_path, format!(r#"{{"tokens":[{{"ticker":"TST","covenantId":"{}"}}]}}"#, Hash32(TOKEN_COV))).unwrap();
    let mut cfg = IndexerConfig {
        data_dir: dir.path().join("data"),
        tokens_path: Some(tokens_path),
        start: StartMode::Hash(node.anchor()),
        ..Default::default()
    };
    cfg.rules.min_order_value_sompi = 1;
    cfg.rules.max_expiry_span_daa = 1 << 40;
    cfg.api.rate_limit.per_ip_rps = 0.0; // rate limiting is covered by the api unit tests
    let idx = Indexer::open(cfg).unwrap();
    let follower = idx.follower(node.clone());

    let w = World::new();
    let a = ask(MAKER_A, 300_000_000);
    let b0 = bid(MAKER_B, 200_000_000);
    let create_a = w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let create_b = w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    w.include(&node, &[&create_a, &create_b]);
    settle(&follower).await;
    let cov_a = w.cov(&create_a, 0);
    let cov_b = w.cov(&create_b, 0);
    // a stray on the ask, to see the strays endpoint
    let stray = w.stray(cov_a, 77, TAKER);
    w.include(&node, &[&stray]);
    settle(&follower).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = idx.api_state().unwrap();
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        kob_executor::api::serve_listener(listener, state, async move {
            let _ = stop_rx.changed().await;
        })
        .await
        .unwrap();
    });
    let token = Hash32(TOKEN_COV);

    // REST: book, order, health
    let (st, book) = http_get(addr, &format!("/v1/books/{token}")).await;
    assert_eq!(st, 200, "{book}");
    assert_eq!(book["asks"].as_array().unwrap().len(), 1, "{book}");
    assert_eq!(book["asks"][0]["amount"], (10 * WHOLE).to_string(), "the stray adds no liquidity: {book}");
    assert_eq!(book["bids"].as_array().unwrap().len(), 1, "{book}");
    let (st, o) = http_get(addr, &format!("/v1/orders/{cov_a}")).await;
    assert_eq!(st, 200);
    assert_eq!(o["status"], "open", "{o}");
    assert_eq!(o["contract"], "KobAsk");
    assert_eq!(o["custody"]["ok"], true, "{o}");
    assert_eq!(o["strays"][0]["amount"], "77", "{o}");
    assert_eq!(o["state"]["kind"], "KobAsk");
    assert_eq!(o["state"]["state"]["amountLeft"], (10 * WHOLE).to_string());
    assert_eq!(o["extension_commitment"], Hash32(EXT).to_hex());
    assert!(o["refund_due_daa"].as_i64().is_some());
    let (_, ob) = http_get(addr, &format!("/v1/orders/{cov_b}")).await;
    assert!(ob.get("custody").is_none(), "bids have no custody: {ob}");
    let (st, s) = http_get(addr, "/v1/strays").await;
    assert_eq!(st, 200);
    assert_eq!(s["items"].as_array().unwrap().len(), 1, "{s}");
    assert_eq!(s["items"][0]["owner"], cov_a.to_hex());
    assert_eq!(s["items"][0]["lost"], false);
    let (st, te) = http_get(addr, "/v1/token-events").await;
    assert_eq!(st, 200);
    assert_eq!(te["items"].as_array().unwrap().len(), 1, "one token identity seen: {te}");
    let (st, hl) = http_get(addr, "/v1/health").await;
    assert_eq!(st, 200);
    assert_eq!(hl["state"], "following", "{hl}");
    assert!(hl.get("records_next_n").is_some(), "{hl}");
    let (st, _) = http_get(addr, "/v1/orders/zz").await;
    assert_eq!(st, 400);

    // WebSocket: subscribe to fills and the book, then fill the ask
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await.unwrap();
    ws.send(Message::text(format!(r#"{{"op":"subscribe","channels":["fills","book:{token}","reorg"]}}"#))).await.unwrap();
    let ack = ws.next().await.unwrap().unwrap();
    assert!(ack.to_text().unwrap().contains("subscribed"), "{ack:?}");
    let custody = w.token_at(&create_a, 1, Kcc20State::custody(10 * WHOLE, cov_a.0, EXT));
    let leg = Leg::Ask { order: w.order(&create_a, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = w.sign(&Action::Batch(batch(&w, node.tip_daa(), vec![leg])));
    w.include(&node, &[&fill]);
    follower.step().await;
    let mut got_fill = false;
    let mut got_book = false;
    for _ in 0..4 {
        let m = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.expect("frame").unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
        match v["channel"].as_str() {
            Some("fills") => {
                assert_eq!(v["data"]["amount"], 4 * WHOLE);
                got_fill = true;
            }
            Some(c) if c.starts_with("book:") => got_book = true,
            _ => {}
        }
        if got_fill && got_book {
            break;
        }
    }
    assert!(got_fill && got_book);
    let (_, fills) = http_get(addr, "/v1/fills").await;
    assert_eq!(fills["items"].as_array().map(|a| a.len()), Some(1), "{fills}");
    let (_, o) = http_get(addr, &format!("/v1/orders/{cov_a}")).await;
    assert_eq!(o["status"], "partial");
    assert_eq!(o["amount_left"], (6 * WHOLE).to_string());
    assert_eq!(o["amount_estimated"], false);
    assert_eq!(o["state"]["state"]["amountLeft"], (6 * WHOLE).to_string());

    // reorg: the fill disappears, the client is told
    node.reorg(1, vec![vec![], vec![]]);
    follower.step().await;
    let mut got_reorg = false;
    for _ in 0..4 {
        let m = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.expect("frame").unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
        if v["channel"] == "reorg" {
            got_reorg = true;
            break;
        }
    }
    assert!(got_reorg);
    let (_, o) = http_get(addr, &format!("/v1/orders/{cov_a}")).await;
    assert_eq!(o["status"], "open");
    stop_tx.send(true).unwrap();
    server.await.unwrap();
    let _ = Arc::new(());
}
