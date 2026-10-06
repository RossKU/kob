//! End-to-end tests of the REST and WebSocket API against a seeded on-disk SQLite database.

use super::*;
use crate::hex::{self, Hash32};
use crate::indexer::db::{open_writer, ReadPool};
use crate::indexer::status::{FillNotice, FollowerState};
use crate::tokens::{TokenAllowlist, TokenEntry};
use axum::body::Body;
use axum::http::{HeaderMap, Request};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use rusqlite::{params, Connection};
use serde_json::Value;
use tower::ServiceExt;

const TOKEN: u8 = 0x11;
const MAKER: u8 = 0xaa;
/// One whole token of the fixtures in base units: the token has 3 decimals (scale 1 000). Prices are sompi per whole token.
const W: i64 = 1_000;

fn cid(n: u8) -> Vec<u8> {
    vec![n; 32]
}

fn h(n: u8) -> String {
    hex::encode(&cid(n))
}

struct Env {
    _dir: tempfile::TempDir,
    w: Connection,
    state: ApiState,
}

fn env(tweak: impl FnOnce(&mut ApiConfig)) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sqlite3");
    let w = open_writer(&path, "testnet-10").unwrap();
    let pool = ReadPool::open(&path, 2).unwrap();
    let health = Arc::new(HealthState::new("testnet-10"));
    health.update(|s| {
        s.state = FollowerState::Following;
        s.node_daa = Some(1000);
        s.cursor_daa = 990;
    });
    let mut cfg = ApiConfig::default();
    cfg.rate_limit.per_ip_rps = 0.0;
    cfg.rate_limit.global_rps = 0.0;
    tweak(&mut cfg);
    let tokens = TokenAllowlist::from_entries([TokenEntry {
        ticker: "KRON".into(),
        covenant_id: Hash32([TOKEN; 32]),
        template_hash: None,
        extension_commitment: None,
        decimals: Some(3),
        family: None,
        enabled: true,
        official: false,
        template_id: None,
        powers: vec![],
    }]);
    let (events, _) = tokio::sync::broadcast::channel(16);
    let state = ApiState {
        pool,
        health,
        events,
        tokens: Arc::new(tokens),
        cfg,
        settle_depth_daa: 100,
        daa_per_second: 10,
        lag_alarm_hours: vec![1, 6, 12, 24],
    };
    Env { _dir: dir, w, state }
}

/// An order of scale 1 000 (one whole token) and minimum fill 1 000 at `price` sompi per whole token; `initial` and
/// `remaining` in base units.
#[allow(clippy::too_many_arguments)]
fn seed_order(
    c: &Connection,
    id: u8,
    side: i64,
    price: i64,
    block: i64,
    status: &str,
    initial: Option<i64>,
    remaining: Option<i64>,
    listed: bool,
    maker: u8,
) {
    c.execute(
        "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, scale, min_fill, price, tip, \
         expiry_daa, in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, unlisted_reason) \
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, 1000, 1000, ?7, 0, 900, 1, ?8, x'00', ?9, ?10, ?11, ?12)",
        params![
            cid(id),
            if side == 1 { "KobAsk" } else { "KobBid" },
            cid(0xee),
            side,
            cid(maker),
            cid(TOKEN),
            price,
            initial,
            block,
            block * 100,
            listed as i64,
            if listed { None } else { Some("token_not_allowlisted") }
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) \
         VALUES (?1, ?2, ?3, ?4, ?8, ?5, 0, 123456789012, 1, ?6, ?7)",
        params![cid(id), status, initial.unwrap_or(0) - remaining.unwrap_or(0), remaining, cid(id + 100), block, block * 100, initial.is_some() as i64],
    )
    .unwrap();
    // ask-side orders are liquidity only with an exact live custody UTXO (`amountLeft` base units)
    if side == 1 && remaining.unwrap_or(0) > 0 {
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) \
             VALUES (?1, 1, ?2, ?3, ?4, 1000, 'custody', ?5, ?6)",
            params![cid(id + 100), cid(TOKEN), cid(id), remaining.unwrap(), block, block * 100],
        )
        .unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn seed_event(c: &Connection, cov: u8, block: i64, daa: i64, kind: &str, side: i64, amount: i64, price: i64) {
    c.execute(
        "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail) \
         VALUES (?1, ?2, ?3, 1700000000000, ?4, 0, ?5, ?6, ?7, ?8, ?9, 5000, 0, '{\"k\":1}')",
        params![cid(cov), block, daa, cid(block as u8 + 50), kind, cid(TOKEN), side, amount, price],
    )
    .unwrap();
}

fn seed_all(c: &Connection) {
    seed_order(c, 1, 1, 300_000, 1, "open", Some(10 * W), Some(10 * W), true, MAKER);
    seed_order(c, 2, 1, 250_000, 2, "partial", Some(10 * W), Some(4 * W), true, MAKER);
    seed_order(c, 3, 1, 250_000, 3, "open", Some(5 * W), Some(5 * W), true, MAKER);
    seed_order(c, 4, 1, 100_000, 4, "open", Some(5 * W), Some(5 * W), false, 0xbb);
    seed_order(c, 5, 1, 90_000, 5, "filled", Some(5 * W), Some(0), true, 0xbb);
    seed_order(c, 6, 2, 240_000, 1, "open", None, Some(3 * W), true, 0xcc);
    seed_order(c, 7, 2, 260_000, 2, "open", None, Some(7 * W), true, 0xcc);
    // a child (exit) of order 2
    c.execute("UPDATE orders SET parent = ?1 WHERE covenant_id = ?2", params![cid(2), cid(5)]).unwrap();
    seed_event(c, 2, 2, 200, "create", 1, 0, 250_000);
    seed_event(c, 2, 6, 800, "fill", 1, 2 * W, 250_000);
    seed_event(c, 2, 8, 985, "fill", 1, W, 250_000);
    seed_event(c, 6, 9, 990, "fill", 2, 3 * W, 240_000);
    c.execute("INSERT INTO blocks (seq, hash, daa) VALUES (9, ?1, 990)", params![cid(0x70)]).unwrap();
}

fn peer(ip: &str) -> axum::extract::ConnectInfo<SocketAddr> {
    axum::extract::ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 4000))
}

async fn get_from(app: &Router, uri: &str, from: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(peer(from));
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (parts.status, parts.headers, v)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let (s, _, v) = get_from(app, uri, "127.0.0.1", &[]).await;
    (s, v)
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|o| o["covenant_id"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn health_and_ready() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let (s, v) = get(&app, "/v1/health").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["state"], "following");
    assert_eq!(v["ok"], true);
    assert_eq!(v["caught_up"], true, "the matcher plans only while the store is caught up");
    assert_eq!(v["lag_daa"], 10);
    assert_eq!(v["lag_seconds"], 1);
    assert!(v["alarms"].as_array().unwrap().is_empty());
    assert_eq!(v["counters"]["orders_total"], 7);
    assert_eq!(v["counters"]["orders_by_status"]["open"], 5);
    assert_eq!(v["counters"]["last_block_seq"], 9);
    let (s, v) = get(&app, "/v1/health/ready").await;
    assert_eq!((s, &v["ready"]), (StatusCode::OK, &Value::Bool(true)));

    // 2 hours behind: alarm at 1h, not ready, health still 200
    e.state.health.update(|h| {
        h.node_daa = Some(990 + 2 * 3600 * 10);
    });
    let (s, v) = get(&app, "/v1/health").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], false);
    assert_eq!(v["alarms"].as_array().unwrap().len(), 1);
    assert_eq!(v["alarms"][0]["lag_hours_at_least"], 1);
    let (s, _) = get(&app, "/v1/health/ready").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);

    e.state.health.update(|h| {
        h.node_daa = Some(1000);
        h.state = FollowerState::Gap;
    });
    let (s, v) = get(&app, "/v1/health/ready").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["ready"], false);
}

#[tokio::test]
async fn tokens_list_counts() {
    let e = env(|_| {});
    seed_all(&e.w);
    let (s, v) = get(&router(e.state.clone()), "/v1/tokens").await;
    assert_eq!(s, StatusCode::OK);
    let t = &v["tokens"][0];
    assert_eq!(t["ticker"], "KRON");
    assert_eq!(t["covenant_id"], h(TOKEN));
    assert_eq!((t["decimals"].as_i64(), t["scale"].as_i64()), (Some(3), Some(1000)));
    assert_eq!(t["open_asks"], 3);
    assert_eq!(t["open_bids"], 2);
}

#[tokio::test]
async fn book_aggregated_and_per_order() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let (s, v) = get(&app, &format!("/v1/books/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["aggregated"], true);
    let asks = v["asks"].as_array().unwrap();
    assert_eq!(asks.len(), 2, "unlisted and filled orders are excluded");
    assert_eq!(
        (asks[0]["price"].as_str(), asks[0]["amount"].as_str(), asks[0]["orders"].as_i64(), asks[0]["scale"].as_i64()),
        (Some("250000"), Some("9000"), Some(2), Some(1000))
    );
    assert_eq!(asks[0]["amount_estimated"], false);
    assert_eq!((asks[1]["price"].as_str(), asks[1]["amount"].as_str()), (Some("300000"), Some("10000")));
    let bids = v["bids"].as_array().unwrap();
    assert_eq!(bids[0]["price"], "260000");
    assert_eq!(bids[1]["price"], "240000");
    assert_eq!(bids[0]["amount_estimated"], true);

    let (_, v) = get(&app, &format!("/v1/books/{}?aggregate=false", h(TOKEN))).await;
    assert_eq!(v["aggregated"], false);
    // asks ascending by price, ties by age (genesis block), then id; bids descending
    assert_eq!(ids(&v["asks"]), vec![h(2), h(3), h(1)]);
    assert_eq!(ids(&v["bids"]), vec![h(7), h(6)]);
    assert_eq!(v["asks"][0]["status"], "partial");
    assert_eq!(v["asks"][0]["amount_left"], "4000");
    assert_eq!((v["asks"][0]["scale"].as_i64(), v["asks"][0]["min_fill"].as_str()), (Some(1000), Some("1000")));
    assert_eq!(v["asks"][0]["confirmations"], 800);
    assert_eq!(v["asks"][0]["settled"], true);
    // order 2 expires at DAA 900 < node DAA 1000
    assert_eq!(v["asks"][0]["expired"], true);

    let (_, v) = get(&app, &format!("/v1/books/{}?aggregate=false&depth=1", h(TOKEN))).await;
    assert_eq!(ids(&v["asks"]), vec![h(2)]);
    assert_eq!(ids(&v["bids"]), vec![h(7)]);

    // an unknown token has an empty book, a malformed one is a 400
    let (s, v) = get(&app, &format!("/v1/books/{}", h(0x55))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["asks"].as_array().unwrap().is_empty());
    let (s, v) = get(&app, "/v1/books/xyz").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "bad_request");
    let (s, _) = get(&app, &format!("/v1/books/{}?depth=0", h(TOKEN))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, &format!("/v1/books/{}?aggregate=maybe", h(TOKEN))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn order_by_id() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let (s, v) = get(&app, &format!("/v1/orders/{}", h(2))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["contract"], "KobAsk");
    assert_eq!(v["status"], "partial");
    assert_eq!(v["price"], "250000");
    assert_eq!(v["current"]["value"], "123456789012");
    assert_eq!(v["filled_amount"], "6000");
    assert_eq!(v["amount_left"], "4000");
    assert_eq!((v["initial_amount"].as_str(), v["scale"].as_i64(), v["min_fill"].as_str()), (Some("10000"), Some(1000), Some("1000")));
    // base units and whole-token prices only: no field of the retired lot geometry is left in the view
    assert!(!v.to_string().contains("lot"), "{v}");
    assert_eq!(v["maker"], h(MAKER));
    assert_eq!(v["listed"], true);
    assert_eq!(v["children"][0], h(5));
    let (_, v) = get(&app, &format!("/v1/orders/{}", h(4))).await;
    assert_eq!(v["listed"], false);
    assert_eq!(v["unlisted_reason"], "token_not_allowlisted");
    let (_, v) = get(&app, &format!("/v1/orders/{}", h(5))).await;
    assert_eq!(v["parent"], h(2));
    assert_eq!(v["expired"], false, "closed orders are never expired");

    let (s, v) = get(&app, &format!("/v1/orders/{}", h(0x42))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
    let (s, _) = get(&app, "/v1/orders/nothex").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, "/v1/orders/abcd").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn orders_by_maker_paging_and_filters() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let base = format!("/v1/orders?maker={}", h(MAKER));
    let (s, v) = get(&app, &format!("{base}&limit=2")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ids(&v["items"]), vec![h(3), h(2)]);
    let cur = v["next_cursor"].as_str().unwrap().to_string();
    assert_eq!(cur, format!("2:{}", h(2)));
    let (_, v) = get(&app, &format!("{base}&limit=2&cursor={cur}")).await;
    assert_eq!(ids(&v["items"]), vec![h(1)]);
    assert!(v["next_cursor"].is_null());

    let (_, v) = get(&app, &format!("/v1/orders?token={}&status=active", h(TOKEN))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 6);
    let (_, v) = get(&app, "/v1/orders?status=filled").await;
    assert_eq!(ids(&v["items"]), vec![h(5)]);
    // every IOC / FOK / market order that returned its rest ends `killed`: the API filters it like any other status
    let (s, v) = get(&app, "/v1/orders?status=killed").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["items"].as_array().unwrap().is_empty());
    let (s, _) = get(&app, "/v1/orders?status=bogus").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, "/v1/orders?cursor=garbage").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, "/v1/orders?maker=zz").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // page size is capped by config
    let (_, v) = get(&app, "/v1/orders?limit=100000").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 7);
}

#[tokio::test]
async fn page_size_cap() {
    let e = env(|c| c.max_page_size = 3);
    seed_all(&e.w);
    let app = router(e.state.clone());
    let (_, v) = get(&app, "/v1/orders?limit=100000").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 3);
    assert!(v["next_cursor"].is_string());
}

#[tokio::test]
async fn events_and_fills() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let (s, v) = get(&app, &format!("/v1/orders/{}/events", h(2))).await;
    assert_eq!(s, StatusCode::OK);
    let kinds: Vec<&str> = v["items"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["create", "fill", "fill"]);
    assert_eq!(v["items"][1]["price"], "250000");
    assert_eq!(v["items"][1]["amount"], "2000");
    assert_eq!(v["items"][1]["payout"], "5000");
    assert_eq!(v["items"][1]["detail"]["k"], 1);
    let (s, _) = get(&app, &format!("/v1/orders/{}/events", h(0x42))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, v) = get(&app, &format!("/v1/fills?token={}", h(TOKEN))).await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "creates are not fills");
    assert!(items[0]["id"].as_i64() > items[1]["id"].as_i64(), "newest first");
    assert_eq!(items[0]["confirmations"], 10);
    assert_eq!(items[0]["settled"], false);
    assert_eq!(items[2]["confirmations"], 200);
    assert_eq!(items[2]["settled"], true);
    let (_, v) = get(&app, &format!("/v1/fills?token={}&limit=2", h(TOKEN))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    let cur = v["next_cursor"].as_str().unwrap();
    let (_, v) = get(&app, &format!("/v1/fills?before={cur}")).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    let (_, v) = get(&app, "/v1/fills?side=bid").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["items"][0]["side"], 2);
    let (_, v) = get(&app, &format!("/v1/fills?token={}", h(0x55))).await;
    assert!(v["items"].as_array().unwrap().is_empty());
    let (s, _) = get(&app, "/v1/fills?side=3").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, "/v1/fills?before=x").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn arm_events_carry_their_evidence_and_receipts_are_gone() {
    let e = env(|_| {});
    seed_all(&e.w);
    // an `arm` of order 3 by the plain ask filled at input 1 of the same transaction (protocol v2.6: no receipt)
    let detail = format!(
        r#"{{"input":0,"entry":"update","verified":true,"evidence":{{"input":1,"order":"{}","side":1,"price":"95"}}}}"#,
        h(0x44)
    );
    e.w.execute(
        "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail) \
         VALUES (?1, 7, 900, 1700000000000, ?2, 0, 'arm', ?3, 1, NULL, NULL, NULL, 0, ?4)",
        params![cid(3), cid(0x57), cid(TOKEN), detail],
    )
    .unwrap();
    let app = router(e.state.clone());
    let (s, v) = get(&app, &format!("/v1/orders/{}/events", h(3))).await;
    assert_eq!(s, StatusCode::OK);
    let arm = v["items"].as_array().unwrap().iter().find(|x| x["kind"] == "arm").expect("the arm event");
    assert_eq!(arm["detail"]["evidence"]["order"], h(0x44));
    assert_eq!(arm["detail"]["evidence"]["input"], 1);
    assert_eq!(arm["detail"]["evidence"]["price"], "95");
    // the v2.4 receipt endpoint is retired
    let (s, v) = get(&app, "/v1/receipts").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
}

#[tokio::test]
async fn unknown_route_and_cors() {
    let e = env(|c| c.cors_allow_origin = Some("https://kob.example".into()));
    let app = router(e.state.clone());
    let (s, h_, v) = get_from(&app, "/v1/nope", "127.0.0.1", &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
    assert_eq!(h_.get("access-control-allow-origin").unwrap(), "https://kob.example");
    let mut req = Request::builder().method("OPTIONS").uri("/v1/orders").body(Body::empty()).unwrap();
    req.extensions_mut().insert(peer("127.0.0.1"));
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(resp.headers().contains_key("access-control-allow-methods"));
}

#[tokio::test]
async fn rate_limit_returns_429_with_retry_after() {
    let e = env(|c| {
        c.rate_limit.per_ip_rps = 0.01;
        c.rate_limit.per_ip_burst = 2;
    });
    let app = router(e.state.clone());
    assert_eq!(get_from(&app, "/v1/tokens", "203.0.113.5", &[]).await.0, StatusCode::OK);
    assert_eq!(get_from(&app, "/v1/tokens", "203.0.113.5", &[]).await.0, StatusCode::OK);
    let (s, hd, v) = get_from(&app, "/v1/tokens", "203.0.113.5", &[]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "rate_limited");
    let ra: u64 = hd.get("retry-after").unwrap().to_str().unwrap().parse().unwrap();
    assert!(ra >= 1);
    // another client is unaffected
    assert_eq!(get_from(&app, "/v1/tokens", "203.0.113.6", &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_keys_follow_trusted_proxy_headers() {
    let e = env(|c| {
        c.rate_limit.per_ip_rps = 0.01;
        c.rate_limit.per_ip_burst = 1;
        c.trusted_proxies = vec!["10.0.0.0/8".into()];
    });
    let app = router(e.state.clone());
    let via_proxy = |xff: &'static str| {
        let app = app.clone();
        async move { get_from(&app, "/v1/tokens", "10.1.1.1", &[("x-forwarded-for", xff)]).await.0 }
    };
    assert_eq!(via_proxy("9.9.9.9").await, StatusCode::OK);
    // same client again: limited, even with a spoofed extra left-most entry
    assert_eq!(via_proxy("9.9.9.9").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(via_proxy("1.2.3.4, 9.9.9.9").await, StatusCode::TOO_MANY_REQUESTS);
    // a different real client behind the same proxy has its own bucket
    assert_eq!(via_proxy("8.8.8.8").await, StatusCode::OK);
    // an untrusted peer cannot pick its bucket with the header
    assert_eq!(get_from(&app, "/v1/tokens", "203.0.113.1", &[("x-forwarded-for", "5.5.5.5")]).await.0, StatusCode::OK);
    assert_eq!(get_from(&app, "/v1/tokens", "203.0.113.1", &[("x-forwarded-for", "6.6.6.6")]).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn global_limit_applies_across_clients() {
    let e = env(|c| {
        c.rate_limit.global_rps = 0.01;
        c.rate_limit.global_burst = 2;
    });
    let app = router(e.state.clone());
    assert_eq!(get_from(&app, "/v1/tokens", "1.1.1.1", &[]).await.0, StatusCode::OK);
    assert_eq!(get_from(&app, "/v1/tokens", "2.2.2.2", &[]).await.0, StatusCode::OK);
    assert_eq!(get_from(&app, "/v1/tokens", "3.3.3.3", &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn invalid_trusted_proxy_config_is_reported() {
    let e = env(|c| c.trusted_proxies = vec!["not-an-ip".into()]);
    assert!(e.state.validate().is_err());
    let ok = env(|c| c.trusted_proxies = vec!["10.0.0.0/8".into(), "::1".into()]);
    assert!(ok.state.validate().is_ok());
}

async fn next_json<S>(rx: &mut S) -> Value
where
    S: futures_util::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), rx.next()).await.expect("timeout").expect("closed").expect("ws error");
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn websocket_end_to_end() {
    use tokio_tungstenite::tungstenite::Message as M;
    let e = env(|c| {
        c.max_ws_per_ip = 1;
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let events = e.state.events.clone();
    let server = tokio::spawn(serve_listener(listener, e.state.clone(), async move {
        let _ = stop_rx.await;
    }));

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await.unwrap();
    let book = format!("book:{}", h(TOKEN));
    ws.send(M::text(format!(r#"{{"op":"subscribe","channels":["fills","{book}","reorg"]}}"#))).await.unwrap();
    let ack = next_json(&mut ws).await;
    assert_eq!(ack["type"], "subscribed");
    assert_eq!(ack["data"]["channels"].as_array().unwrap().len(), 3);

    // a second connection from the same address exceeds max_ws_per_ip
    let second = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await;
    assert!(second.is_err(), "per-IP WebSocket limit must reject the upgrade");

    let fill = FillNotice {
        order: Hash32([1; 32]),
        token: Some(Hash32([TOKEN; 32])),
        side: 1,
        price: Some(250_000),
        amount: 2 * W,
        payout: None,
        txid: Hash32([2; 32]),
        block: Hash32([3; 32]),
        daa: 995,
    };
    events
        .send(Arc::new(IndexEvent {
            cursor_hash: Some(Hash32([3; 32])),
            cursor_daa: 995,
            reverted_blocks: 1,
            added_blocks: 2,
            orders: vec![],
            tokens: vec![Hash32([TOKEN; 32])],
            fills: vec![fill],
        }))
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let f = next_json(&mut ws).await;
        seen.push(f["type"].as_str().unwrap().to_string());
        if f["type"] == "fill" {
            assert_eq!(f["data"]["amount"], 2000);
            assert_eq!(f["channel"], "fills");
        }
    }
    seen.sort();
    assert_eq!(seen, vec!["book", "fill", "reorg"]);

    ws.send(M::text(r#"{"op":"ping"}"#)).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "pong");

    // graceful shutdown closes the socket
    let _ = stop_tx.send(());
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(M::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok());
    tokio::time::timeout(Duration::from_secs(5), server).await.expect("server stops").unwrap().unwrap();
}

// ---------------------------------------------------------------------------------------------
// GET /v1/token-utxos

fn seed_holding(c: &Connection, n: u8, token: u8, owner: &[u8], family: i64, amount: i64, block: i64, spent: bool) {
    use kob_protocol::state::{Kcc20State, KronState, TokenState};
    let (program, state) = if family == 2 {
        ("KronToken2433", TokenState::Kron(KronState::addr(amount, owner.try_into().unwrap())))
    } else {
        ("KCC20Ref_8x8", TokenState::Kcc20(Kcc20State::p2pk(amount, owner.try_into().unwrap(), [0xee; 32])))
    };
    let kind = if family == 2 { 3 } else { 0 };
    c.execute(
        "INSERT INTO token_holdings (txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, created_block, created_daa, spent_block, spent_txid) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1000000000, ?9, 'owned', ?10, ?11, ?12, ?13)",
        params![
            cid(n),
            n as i64 % 3,
            cid(token),
            program,
            family,
            owner,
            kind,
            amount,
            state.encode(),
            block,
            block * 100,
            spent.then_some(block + 1),
            spent.then(|| cid(n + 100))
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn token_utxos_by_owner_token_pagination_and_errors() {
    use kaspa_addresses::{Address, Prefix, Version};
    let e = env(|_| {});
    let key = [0x42u8; 32];
    seed_holding(&e.w, 1, TOKEN, &key, 1, 500, 1, false);
    seed_holding(&e.w, 2, TOKEN, &key, 1, 700, 2, true);
    seed_holding(&e.w, 3, 0x12, &key, 2, 900, 3, false);
    seed_holding(&e.w, 4, TOKEN, &[0x43u8; 32], 1, 100, 4, false);
    let app = router(e.state.clone());
    let owner = hex::encode(&key);

    // by owner: unspent only by default, both families
    let (s, v) = get(&app, &format!("/v1/token-utxos?owner={owner}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(v["next_cursor"].is_null());
    let kcc = &items[0];
    assert_eq!(kcc["family"], "kcc20");
    assert_eq!(kcc["token"], h(TOKEN));
    assert_eq!(kcc["owner"], owner);
    assert_eq!(kcc["owner_kind"], 0);
    assert_eq!(kcc["amount"], "500");
    assert_eq!(kcc["value"], "1000000000");
    assert_eq!(kcc["role"], "owned");
    assert_eq!(kcc["program"], "KCC20Ref_8x8");
    assert_eq!(kcc["spent"], false);
    assert_eq!(kcc["created_daa"], 100);
    assert_eq!(kcc["confirmations"], 900);
    assert_eq!(kcc["settled"], true);
    assert_eq!(kcc["index"], 1);
    // the state is the kob-wasm token state JSON
    assert_eq!(kcc["state"]["owner_scheme"], 0);
    assert_eq!(kcc["state"]["amount"], "500");
    assert_eq!(kcc["state"]["owner"], owner);
    assert_eq!(kcc["state_hex"].as_str().unwrap().len(), 224);
    assert_eq!(kcc["template_hash"].as_str().unwrap().len(), 64);
    let kron = &items[1];
    assert_eq!(kron["family"], "kron");
    assert_eq!(kron["state"]["id_type"], 3);
    assert_eq!(kron["state"]["is_minter"], 0);
    assert_eq!(kron["state_hex"].as_str().unwrap().len(), 92);

    // include spent
    let (_, v) = get(&app, &format!("/v1/token-utxos?owner={owner}&spent=true")).await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let spent = items.iter().find(|i| i["spent"] == true).unwrap();
    assert_eq!(spent["spent_txid"], h(102));

    // by token, by owner + token
    let (_, v) = get(&app, &format!("/v1/token-utxos?token={}", h(TOKEN))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    let (_, v) = get(&app, &format!("/v1/token-utxos?token={}&owner={owner}", h(0x12))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    let (_, v) = get(&app, &format!("/v1/token-utxos?token={}", h(0x77))).await;
    assert!(v["items"].as_array().unwrap().is_empty());

    // a Kaspa P2PK address names the same owner
    let addr = Address::new(Prefix::Testnet, Version::PubKey, &key).to_string();
    let (s, v) = get(&app, &format!("/v1/token-utxos?owner={addr}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 2);

    // pagination: oldest first, keyset cursor
    let (_, p1) = get(&app, &format!("/v1/token-utxos?owner={owner}&spent=true&limit=2")).await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 2);
    let cur = p1["next_cursor"].as_str().expect("more pages").to_string();
    let (_, p2) = get(&app, &format!("/v1/token-utxos?owner={owner}&spent=true&limit=2&cursor={cur}")).await;
    assert_eq!(p2["items"].as_array().unwrap().len(), 1);
    assert!(p2["next_cursor"].is_null());
    let mut all: Vec<_> =
        p1["items"].as_array().unwrap().iter().chain(p2["items"].as_array().unwrap()).map(|i| i["amount"].clone()).collect();
    all.sort_by_key(|a| a.as_str().unwrap().parse::<i64>().unwrap());
    assert_eq!(all, ["500", "700", "900"]);

    // errors
    for uri in [
        "/v1/token-utxos".to_string(),
        "/v1/token-utxos?limit=5".to_string(),
        "/v1/token-utxos?owner=xyz".to_string(),
        format!("/v1/token-utxos?owner={}", &owner[..62]),
        format!("/v1/token-utxos?token={}", &owner[..62]),
        format!("/v1/token-utxos?owner={owner}&spent=maybe"),
        format!("/v1/token-utxos?owner={owner}&limit=0"),
        format!("/v1/token-utxos?owner={owner}&cursor=garbage"),
        // a script-hash address is not a key owner
        format!("/v1/token-utxos?owner={}", Address::new(Prefix::Testnet, Version::ScriptHash, &key)),
    ] {
        let (s, v) = get(&app, &uri).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}: {v}");
        assert!(v["error"]["message"].is_string(), "{uri}");
    }
}

#[tokio::test]
async fn aggregated_levels_are_per_price_and_scale_ordered_by_price_per_base_unit() {
    let e = env(|_| {});
    // most orders quote the token's whole token (scale 1 000); some quote 10 whole tokens (scale 10 000): the same price per
    // base unit is a 10 times larger `price` there
    let rescale = |id: u8, scale: i64| {
        e.w.execute("UPDATE orders SET scale = ?1 WHERE covenant_id = ?2", params![scale, cid(id)]).unwrap();
    };
    seed_order(&e.w, 21, 1, 250_000, 1, "open", Some(5 * W), Some(5 * W), true, MAKER); // scale 1 000: 250 per base unit
    seed_order(&e.w, 22, 1, 2_500_000, 2, "open", Some(4 * W), Some(4 * W), true, MAKER); // scale 10 000: 250 per base unit
    rescale(22, 10_000);
    seed_order(&e.w, 23, 1, 2_400_000, 3, "open", Some(3 * W), Some(3 * W), true, MAKER); // scale 10 000: 240 per base unit
    rescale(23, 10_000);
    seed_order(&e.w, 24, 1, 251_000, 4, "open", Some(2 * W), Some(2 * W), true, MAKER); // 251 per base unit
    seed_order(&e.w, 25, 1, 250_000, 5, "open", Some(6 * W), Some(6 * W), true, MAKER); // joins order 21's level
    seed_order(&e.w, 26, 2, 2_450_000, 1, "open", None, Some(3 * W), true, 0xcc); // bid, scale 10 000: 245 per base unit
    rescale(26, 10_000);
    seed_order(&e.w, 27, 2, 246_000, 2, "open", None, Some(3 * W), true, 0xcc); // bid, scale 1 000: 246 per base unit
    let app = router(e.state.clone());
    let level = |l: &Value| {
        (
            l["price"].as_str().unwrap().to_string(),
            l["scale"].as_i64().unwrap(),
            l["amount"].as_str().unwrap().to_string(),
            l["orders"].as_i64().unwrap(),
        )
    };
    let lv = |p: &str, s: i64, a: &str, n: i64| (p.to_string(), s, a.to_string(), n);

    let (s, v) = get(&app, &format!("/v1/books/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    let asks: Vec<_> = v["asks"].as_array().unwrap().iter().map(level).collect();
    // ascending by price per base unit; equal prices per base unit are two rows (one per scale), the smaller raw price first;
    // the raw price order (250000, 251000, 2400000, 2500000) would have put the cheapest level in the middle
    assert_eq!(
        asks,
        [
            lv("2400000", 10_000, "3000", 1),
            lv("250000", 1000, "11000", 2),
            lv("2500000", 10_000, "4000", 1),
            lv("251000", 1000, "2000", 1),
        ]
    );
    let bids: Vec<_> = v["bids"].as_array().unwrap().iter().map(level).collect();
    assert_eq!(bids, [lv("246000", 1000, "3000", 1), lv("2450000", 10_000, "3000", 1)]);

    // depth cuts on the price per base unit
    let (_, v) = get(&app, &format!("/v1/books/{}?depth=2", h(TOKEN))).await;
    let asks: Vec<_> = v["asks"].as_array().unwrap().iter().map(level).collect();
    assert_eq!(asks.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["2400000", "250000"]);
    let (_, v) = get(&app, &format!("/v1/books/{}?depth=1", h(TOKEN))).await;
    assert_eq!(v["bids"].as_array().unwrap().len(), 1);
    assert_eq!(v["bids"][0]["price"], "246000");
    // the per-order book follows the same order
    let (_, v) = get(&app, &format!("/v1/books/{}?aggregate=false", h(TOKEN))).await;
    assert_eq!(ids(&v["asks"]), vec![h(23), h(21), h(22), h(25), h(24)]);
    assert_eq!(ids(&v["bids"]), vec![h(27), h(26)]);
}

/// Amounts sum in 128 bits: two listed asks whose remaining amounts sum past an `i64` at one level are one level, never an
/// error.
#[tokio::test]
async fn aggregated_amounts_never_overflow() {
    let e = env(|_| {});
    seed_order(&e.w, 31, 1, 1, 1, "open", Some(i64::MAX), Some(i64::MAX), true, MAKER);
    seed_order(&e.w, 32, 1, 1, 2, "open", Some(i64::MAX), Some(i64::MAX), true, MAKER);
    let (s, v) = get(&router(e.state.clone()), &format!("/v1/books/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["asks"][0]["amount"], (2 * i64::MAX as i128).to_string());
    assert_eq!(v["asks"][0]["orders"], 2);
}

fn seed_fill(c: &Connection, cov: u8, tx: u8, ts: i64, side: i64, amount: i64, price: i64) {
    c.execute(
        "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail) \
         VALUES (?1, 9, 990, ?2, ?3, 0, 'fill', ?4, ?5, ?6, ?7, NULL, 0, NULL)",
        params![cid(cov), ts, cid(tx), cid(TOKEN), side, amount, price],
    )
    .unwrap();
}

#[tokio::test]
async fn market_data_trades_candles_stats_depth() {
    let e = env(|_| {});
    let c = &e.w;
    seed_order(c, 1, 1, 300_000, 1, "open", Some(10 * W), Some(10 * W), true, MAKER);
    seed_order(c, 2, 1, 250_000, 2, "partial", Some(10 * W), Some(4 * W), true, MAKER);
    seed_order(c, 3, 1, 250_000, 3, "open", Some(5 * W), Some(5 * W), true, MAKER);
    seed_order(c, 6, 2, 240_000, 1, "open", None, Some(3 * W), true, 0xcc);
    seed_order(c, 7, 2, 260_000, 2, "open", None, Some(7 * W), true, 0xcc);
    let t0 = 1_700_000_000_000i64;
    // A: ask 2 (genesis DAA 200) and bid 7 (200) cross, 2 whole tokens: a tie goes to the asks (resting), a buy at the ask's 250 000
    seed_fill(c, 2, 0xa1, t0, 1, 2 * W, 250_000);
    seed_fill(c, 7, 0xa1, t0, 2, 2 * W, 260_000);
    // B: a wallet sells directly into bid 6: one-sided, the seller aggressed at the bid's 240 000
    seed_fill(c, 6, 0xb1, t0 + 90_000, 2, W, 240_000);
    // C: ask 1 (100) is older than bid 7 (200): a buy at 300 000
    seed_fill(c, 1, 0xc1, t0 + 400_000, 1, W, 300_000);
    seed_fill(c, 7, 0xc1, t0 + 400_000, 2, W, 260_000);
    let app = router(e.state.clone());

    // trades: newest first, price per basis (the token's standard scale: 3 decimals, 1 000 base units = one whole token)
    let (s, v) = get(&app, &format!("/v1/trades/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["price_basis"], "1000");
    assert_eq!(v["decimals"], 3);
    let items = v["items"].as_array().unwrap();
    let got: Vec<(&str, &str, &str, i64)> = items
        .iter()
        .map(|t| {
            (t["price"].as_str().unwrap(), t["amount"].as_str().unwrap(), t["side"].as_str().unwrap(), t["fills"].as_i64().unwrap())
        })
        .collect();
    assert_eq!(got, vec![("300000", "1000", "buy", 2), ("240000", "1000", "sell", 1), ("250000", "2000", "buy", 2)]);
    assert_eq!(items[2]["quote"], "500000");
    assert_eq!(items[0]["txid"], h(0xc1));
    assert!(v["next_cursor"].is_null());
    let (_, p1) = get(&app, &format!("/v1/trades/{}?limit=1", h(TOKEN))).await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 1);
    let cur = p1["next_cursor"].as_str().unwrap().to_string();
    let (_, p2) = get(&app, &format!("/v1/trades/{}?limit=1&before={cur}", h(TOKEN))).await;
    assert_eq!(p2["items"][0]["price"], "240000");

    // candles: 5m buckets hold A + B, then C; 1m buckets are three; empty buckets are omitted
    let (s, v) = get(&app, &format!("/v1/candles/{}?interval=5m", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    let cs = v["items"].as_array().unwrap();
    assert_eq!(cs.len(), 2);
    assert_eq!(cs[0]["t"].as_i64(), Some(t0.div_euclid(300_000) * 300_000));
    assert_eq!(
        (cs[0]["o"].as_str(), cs[0]["h"].as_str(), cs[0]["l"].as_str(), cs[0]["c"].as_str()),
        (Some("250000"), Some("250000"), Some("240000"), Some("240000"))
    );
    assert_eq!(
        (cs[0]["volume"].as_str(), cs[0]["quote_volume"].as_str(), cs[0]["trades"].as_i64()),
        (Some("3000"), Some("740000"), Some(2))
    );
    assert_eq!(cs[1]["c"], "300000");
    let (_, v) = get(&app, &format!("/v1/candles/{}?interval=1m", h(TOKEN))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 3);
    let (_, v) = get(&app, &format!("/v1/candles/{}?interval=1m&from={}&to={}", h(TOKEN), t0 + 60_000, t0 + 120_000)).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    let (_, v) = get(&app, &format!("/v1/candles/{}?interval=1m&limit=1", h(TOKEN))).await;
    assert_eq!(v["items"][0]["c"], "300000");
    let (s, _) = get(&app, &format!("/v1/candles/{}?interval=2m", h(TOKEN))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, &format!("/v1/candles/{}?interval=1m&from=10&to=5", h(TOKEN))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // stats: 24 h window ending at the newest event
    let (s, v) = get(&app, &format!("/v1/stats/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (v["last"].as_str(), v["last_side"].as_str(), v["last_ts"].as_i64()),
        (Some("300000"), Some("buy"), Some(t0 + 400_000))
    );
    assert_eq!(
        (v["open_24h"].as_str(), v["high_24h"].as_str(), v["low_24h"].as_str()),
        (Some("250000"), Some("300000"), Some("240000"))
    );
    assert_eq!(v["change_24h_bps"], 2000);
    assert_eq!((v["volume_24h"].as_str(), v["trades_24h"].as_i64()), (Some("4000"), Some(3)));
    // this fixture's book is crossed (bid 260 above ask 250): the spread is negative
    assert_eq!((v["best_bid"].as_str(), v["best_ask"].as_str(), v["mid"].as_str()), (Some("260000"), Some("250000"), Some("255000")));
    assert_eq!(v["spread_bps"], -392);
    assert_eq!((v["open_asks"].as_i64(), v["open_bids"].as_i64()), (Some(3), Some(2)));

    // depth: per-basis levels, best first, cumulative
    let (s, v) = get(&app, &format!("/v1/depth/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    let asks = v["asks"].as_array().unwrap();
    assert_eq!(
        (asks[0]["price"].as_str(), asks[0]["amount"].as_str(), asks[0]["orders"].as_i64()),
        (Some("250000"), Some("9000"), Some(2))
    );
    assert_eq!((asks[1]["cum_amount"].as_str(), asks[1]["cum_quote"].as_str()), (Some("19000"), Some("5250000")));
    let bids = v["bids"].as_array().unwrap();
    assert_eq!((bids[0]["price"].as_str(), bids[0]["estimated"].as_bool()), (Some("260000"), Some(true)));
    assert_eq!(bids[1]["cum_amount"], "10000");
    let (_, v) = get(&app, &format!("/v1/depth/{}?levels=1", h(TOKEN))).await;
    assert_eq!(v["asks"].as_array().unwrap().len(), 1);

    // an unknown token: empty, never an error
    let (s, v) = get(&app, &format!("/v1/stats/{}", h(0x55))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["last"].is_null() && v["best_bid"].is_null());
    assert_eq!(v["trades_24h"], 0);
    let (_, v) = get(&app, &format!("/v1/candles/{}", h(0x55))).await;
    assert!(v["items"].as_array().unwrap().is_empty());
}

// ------------------------------------------------------------------------------------------------ pair books (pair orders)

/// A live, listed order with a real state (the pair book decodes it): its current UTXO, `order_state` (`remaining` base units)
/// and exact custodies (a pair order: one per custody of its state, each of its own token).
fn seed_live(c: &Connection, id: u8, state: &kob_protocol::state::AnyState, block: i64, remaining: i64) {
    let t = state.template_id();
    let terms = crate::model::terms_of(state);
    let in_book = crate::model::in_book(t);
    let bytes = state.encode();
    let quote = state.pair_tokens().map(|p| p.b.cov_id.to_vec());
    c.execute(
        "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, scale, min_fill, price, tip, tif, \
         expiry_daa, active_from, in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, quote_cov_id) \
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?14, ?15, ?16, ?17, 1, ?18)",
        params![
            cid(id),
            t.name(),
            cid(0xee),
            crate::model::side_of(state) as i64,
            cid(MAKER),
            state.token_cov_id().to_vec(),
            terms.scale,
            terms.min_fill,
            if in_book { terms.price } else { None },
            terms.tip,
            terms.tif,
            terms.expiry_daa,
            in_book as i64,
            state.amount_left(),
            bytes,
            block,
            block * 100,
            quote,
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_utxos (txid, idx, covenant_id, value, spk, state, created_block, created_daa) VALUES (?1, 0, ?2, 5000000000, x'00', ?3, ?4, ?5)",
        params![cid(id + 100), cid(id), bytes, block, block * 100],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) \
         VALUES (?1, 'open', 0, ?2, 1, ?3, 0, 5000000000, 1, ?4, ?5)",
        params![cid(id), remaining, cid(id + 100), block, block * 100],
    )
    .unwrap();
    let custodies: Vec<([u8; 32], i64)> = if state.is_pair() {
        state.custodies()
    } else {
        state.custody_amount().filter(|a| *a > 0).map(|a| (state.token_cov_id(), a)).into_iter().collect()
    };
    for (k, (token, amount)) in custodies.into_iter().enumerate() {
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1000, 'custody', ?6, ?7)",
            params![cid(id + 100), k as i64 + 1, token.to_vec(), cid(id), amount, block, block * 100],
        )
        .unwrap();
    }
}

/// A `KobPair` of `amount` base units of A = `base` (scale 1 000) at `price` base units of B = `quote` per whole A: an ASK
/// (custody: the amount of A) or a BID (custody: the B escrow of four fills).
fn pair_of(base: u8, quote: u8, ask: bool, price: i64, amount: i64) -> kob_protocol::state::AnyState {
    use crate::testkit::{rtip, tok_fields, T3};
    use kob_protocol::state::{PairState, SIDE_ASK, SIDE_BID};
    let (th, p, s) = tok_fields(T3);
    let (sc, tc) = if ask { (base, quote) } else { (quote, base) };
    let mut x = PairState {
        maker: [MAKER; 32],
        side: if ask { SIDE_ASK } else { SIDE_BID },
        s_cov_id: [sc; 32],
        s_tpl_hash: th,
        s_pre: p,
        s_suf: s,
        s_family: 1,
        s_scale: 1000,
        t_cov_id: [tc; 32],
        t_tpl_hash: th,
        t_pre: p,
        t_suf: s,
        t_family: 1,
        t_ext: [0xee; 32],
        t_scale: 1000,
        min_fill: 1000,
        price,
        tip: 0,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: rtip(T3),
        delivery_carrier: 200_000_000,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 0,
        amount_left: amount,
        custody: amount,
    };
    if !ask {
        x.custody = x.bid_escrow(amount, 4).unwrap();
    }
    kob_protocol::state::AnyState::KobPair(x)
}

const BASE: u8 = 0x21;
const QUOTE: u8 = 0x22;

fn seed_pair(c: &Connection) {
    use kob_protocol::state::{AnyState, AskState, BidState};
    // direct: an ask of base at 2 quote per base (5 whole tokens), a bid of base at 0.6 quote per base (5 whole tokens), and
    // an ask of the OTHER orientation (quote/base): it is in the quote/base book only
    seed_live(c, 0x31, &pair_of(BASE, QUOTE, true, 2000, 5 * W), 1, 5 * W);
    seed_live(c, 0x32, &pair_of(BASE, QUOTE, false, 600, 5 * W), 2, 5 * W);
    seed_live(c, 0x33, &pair_of(QUOTE, BASE, true, 500, 4 * W), 2, 4 * W);
    // the KAS route: a bid of base (260 100 000 sompi all-in per whole token of 1 000 base units, buying power 3 whole tokens)
    // and an ask of quote (129 900 000 per whole token after its tip, 10 whole tokens)
    let b = BidState { token_cov_id: [BASE; 32], ..crate::testkit::bid(1, 260_000_000) };
    seed_live(c, 0x41, &AnyState::KobBid(b), 3, 3 * W);
    let a = AskState { token_cov_id: [QUOTE; 32], ..crate::testkit::ask(2, 130_000_000) };
    seed_live(c, 0x42, &AnyState::KobAsk(a), 4, 10 * W);
}

#[tokio::test]
async fn pair_book_direct_and_route_levels() {
    let e = env(|_| {});
    seed_pair(&e.w);
    let app = router(e.state.clone());
    let (s, v) = get(&app, &format!("/v1/pairs/{}/{}/book?depth=10", h(BASE), h(QUOTE))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["base"].as_str(), v["quote"].as_str(), v["daa_score"].as_u64()),
        (Some(h(BASE).as_str()), Some(h(QUOTE).as_str()), Some(1000))
    );
    assert_eq!(
        v["asks"],
        serde_json::json!([{"source": "direct", "price_num": "2", "price_den": "1", "amount": "5000", "orders": 1}])
    );
    // bids, best first: the route (sell base to the KAS bid, buy quote from the KAS ask: 260.1 / 129.9 = 867 / 433),
    // bounded by the bid's 3 000 base units; then the pair BID (600 quote base units per whole base = 3 / 5)
    assert_eq!(
        v["bids"],
        serde_json::json!([
            {"source": "route", "price_num": "867", "price_den": "433", "amount": "3000", "orders": 2},
            {"source": "direct", "price_num": "3", "price_den": "5", "amount": "5000", "orders": 1}
        ])
    );
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/book?depth=1", h(BASE), h(QUOTE))).await;
    assert_eq!(v["bids"].as_array().unwrap().len(), 1);
    // the other orientation is its own book: the quote/base pair ask (0.5 base per quote), never the base/quote orders inverted
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/book", h(QUOTE), h(BASE))).await;
    let direct: Vec<&Value> = v["asks"].as_array().unwrap().iter().filter(|l| l["source"] == "direct").collect();
    assert_eq!(direct.len(), 1);
    assert_eq!((direct[0]["price_num"].as_str(), direct[0]["price_den"].as_str()), (Some("1"), Some("2")));
    assert!(v["bids"].as_array().unwrap().iter().all(|l| l["source"] != "direct"));
    // errors
    let (s, _) = get(&app, &format!("/v1/pairs/{}/{}/book", h(BASE), h(BASE))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, &format!("/v1/pairs/zz/{}/book", h(QUOTE))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // an unknown pair is empty, never an error
    let (s, v) = get(&app, &format!("/v1/pairs/{}/{}/book", h(0x55), h(0x56))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["asks"].as_array().unwrap().is_empty() && v["bids"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn pairs_list_and_pair_orders_in_the_order_views() {
    let e = env(|_| {});
    seed_pair(&e.w);
    // a pair order whose custody is not exact (no live custody row) is no liquidity: neither listed nor in the book
    seed_live(&e.w, 0x34, &pair_of(BASE, QUOTE, true, 1900, 2 * W), 5, 2 * W);
    e.w.execute("DELETE FROM token_utxos WHERE owner = ?1", [cid(0x34)]).unwrap();
    let app = router(e.state.clone());
    let row = |base: u8, quote: u8, asks: i64, bids: i64| {
        serde_json::json!({"base": h(base), "quote": h(quote), "direct_asks": asks, "direct_bids": bids, "entry_asks": 0, "entry_bids": 0,
                           "conditionals": 0})
    };
    let want = serde_json::json!([row(BASE, QUOTE, 1, 1), row(QUOTE, BASE, 1, 0)]);
    let (s, v) = get(&app, "/v1/pairs").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, want, "one entry per oriented pair (base = the orders' A)");
    assert_eq!(get(&app, &format!("/v1/pairs?token={}", h(QUOTE))).await.1, want);
    assert_eq!(get(&app, &format!("/v1/pairs?token={}", h(0x55))).await.1, serde_json::json!([]));
    assert_eq!(get(&app, "/v1/pairs?token=zz").await.0, StatusCode::BAD_REQUEST);
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/book", h(BASE), h(QUOTE))).await;
    assert!(v["asks"].as_array().unwrap().iter().all(|l| l["price_num"] != "19"), "the order without custody is not quoted");
    // the KAS book of base does not hold the pair order; the order view shows its pair, side, price and custody
    let (_, v) = get(&app, &format!("/v1/books/{}?aggregate=false", h(BASE))).await;
    assert!(!v.to_string().contains(&h(0x31)));
    let (s, v) = get(&app, &format!("/v1/orders/{}", h(0x31))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((v["contract"].as_str(), v["in_book"].as_bool(), v["side"].as_i64()), (Some("KobPair"), Some(false), Some(1)));
    assert!(v["price"].is_null() && v["quote"].is_null(), "a pair order has no KAS price");
    let x = &v["pair"];
    assert_eq!((x["base"].as_str(), x["quote"].as_str()), (Some(h(BASE).as_str()), Some(h(QUOTE).as_str())));
    assert_eq!((x["side"].as_str(), x["price"].as_str(), x["base_scale"].as_i64()), (Some("ask"), Some("2000"), Some(1000)));
    assert_eq!((x["price_num"].as_str(), x["price_den"].as_str(), x["amount_left"].as_str()), (Some("2"), Some("1"), Some("5000")));
    assert_eq!(
        (x["quote_now"].as_str(), x["base_family"].as_str(), x["quote_family"].as_str()),
        (Some("2000"), Some("kcc20"), Some("kcc20"))
    );
    let cs = x["custodies"].as_array().unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(
        (cs[0]["token"].as_str(), cs[0]["role"].as_str(), cs[0]["expected_amount"].as_str()),
        (Some(h(BASE).as_str()), Some("base"), Some("5000"))
    );
    assert_eq!(cs[0]["ok"], true);
    assert_eq!(cs[0]["utxo"]["amount"], "5000");
    assert!(v["custody"]["ok"].as_bool().unwrap());
    // a pair BID holds a B escrow: its custody is of the quote token
    let (_, v) = get(&app, &format!("/v1/orders/{}", h(0x32))).await;
    assert_eq!((v["side"].as_i64(), v["pair"]["side"].as_str()), (Some(2), Some("bid")));
    let cs = v["pair"]["custodies"].as_array().unwrap();
    assert_eq!(
        (cs[0]["token"].as_str(), cs[0]["role"].as_str(), cs[0]["ok"].as_bool()),
        (Some(h(QUOTE).as_str()), Some("quote"), Some(true))
    );
    assert!(v["custody"]["ok"].as_bool().unwrap());
    let (_, v) = get(&app, &format!("/v1/orders/{}", h(0x34))).await;
    assert_eq!(v["custody"]["ok"], false);
    // listed under either of its tokens
    let (_, v) = get(&app, &format!("/v1/orders?token={}", h(QUOTE))).await;
    let got = ids(&v["items"]);
    assert!(got.contains(&h(0x31)) && got.contains(&h(0x32)) && got.contains(&h(0x33)) && got.contains(&h(0x42)));
    assert!(!got.contains(&h(0x41)), "the bid of base is not an order of quote");
}

/// A KAS fill event of `token` (no pair fields): a trade of the token.
#[allow(clippy::too_many_arguments)]
fn seed_kas_fill(c: &Connection, cov: u8, token: u8, tx: u8, ts: i64, side: i64, amount: i64, price: Option<i64>) {
    c.execute(
        "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail) \
         VALUES (?1, 9, 990, ?2, ?3, 0, 'fill', ?4, ?5, ?6, ?7, NULL, 0, NULL)",
        params![cid(cov), ts, cid(tx), cid(token), side, amount, price],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn seed_pair_fill(c: &Connection, cov: u8, tx: u8, ts: i64, side: i64, amount_a: i64, amount_b: i64, price: i64, counterparty: &str) {
    c.execute(
        "INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, covenant_id, contract, base_cov_id, quote_cov_id, side, amount_a, \
         amount_b, price, a_scale, tip_kas, counterparty) VALUES (9, 990, ?1, ?2, 0, ?3, 'KobPair', ?4, ?5, ?6, ?7, ?8, ?9, 1000, 0, ?10)",
        params![ts, cid(tx), cid(cov), cid(BASE), cid(QUOTE), side, amount_a, amount_b, price, counterparty],
    )
    .unwrap();
}

const T0: i64 = 1_700_000_100_000;

#[tokio::test]
async fn pair_candles_come_from_the_two_kas_series_and_pair_fills_are_volume_only() {
    let e = env(|_| {});
    seed_pair(&e.w);
    // bucket 1 (5 m): base trades at 2.60 and 2.80 KAS, quote at 1.30; bucket 2: base at 2.70 only (quote carries 1.30 forward)
    seed_kas_fill(&e.w, 0x41, BASE, 0x61, T0, 2, W, Some(260_000_000));
    seed_kas_fill(&e.w, 0x41, BASE, 0x62, T0 + 1_000, 2, W, Some(280_000_000));
    seed_kas_fill(&e.w, 0x42, QUOTE, 0x63, T0 + 2_000, 1, W, Some(130_000_000));
    seed_kas_fill(&e.w, 0x41, BASE, 0x64, T0 + 300_000, 2, W, Some(270_000_000));
    // a pair fill (a netting fill of the pair orders): its event has no price and is never a KAS trade
    seed_kas_fill(&e.w, 0x31, BASE, 0x65, T0 + 3_000, 1, 2 * W, None);
    seed_pair_fill(&e.w, 0x31, 0x65, T0 + 3_000, 1, 2 * W, 4 * W, 2000, "netting");
    seed_pair_fill(&e.w, 0x32, 0x65, T0 + 3_000, 2, 2 * W, 4 * W, 2000, "netting");
    seed_pair_fill(&e.w, 0x31, 0x66, T0 + 301_000, 1, W, 2 * W, 2000, "route");
    let app = router(e.state.clone());

    // the KAS series of base: two trades in bucket 1, one in bucket 2 (the pair fill is not one)
    let (_, v) = get(&app, &format!("/v1/candles/{}?interval=5m", h(BASE))).await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{v}");
    assert_eq!(items[0]["trades"], 2);
    let (_, v) = get(&app, &format!("/v1/trades/{}", h(BASE))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 3, "pair fills never make KAS trades");
    let (_, v) = get(&app, &format!("/v1/stats/{}", h(BASE))).await;
    assert_eq!(v["last"], "270000000", "the last price is the last KAS trade, not the later pair fill");

    let (s, v) = get(&app, &format!("/v1/pairs/{}/{}/candles?interval=5m", h(BASE), h(QUOTE))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["price_source"].as_str(), v["price_basis"].as_str(), v["quote_price_basis"].as_str()),
        (Some("kas_books"), Some("1000"), Some("1000"))
    );
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{v}");
    let rate = |c: &Value, k: &str| {
        (
            c[k]["value"].as_str().unwrap().to_string(),
            c[k]["num"].as_str().unwrap().to_string(),
            c[k]["den"].as_str().unwrap().to_string(),
        )
    };
    // open 2.60 / 1.30 = 2 000 quote base units per 1 000 base units; high 2.80 / 1.30 (the bound) = 28 000 / 13, floored
    assert_eq!(rate(&items[0], "o"), ("2000".into(), "2000".into(), "1".into()));
    assert_eq!(rate(&items[0], "h"), ("2153".into(), "28000".into(), "13".into()));
    assert_eq!(rate(&items[0], "l"), ("2000".into(), "2000".into(), "1".into()));
    assert_eq!(rate(&items[0], "c"), ("2153".into(), "28000".into(), "13".into()));
    assert_eq!((items[0]["a_traded"].as_bool(), items[0]["b_traded"].as_bool()), (Some(true), Some(true)));
    assert_eq!(
        (items[0]["pair_volume_a"].as_str(), items[0]["pair_volume_b"].as_str(), items[0]["pair_fills"].as_u64()),
        (Some("4000"), Some("8000"), Some(2))
    );
    // bucket 2: quote did not trade, its last close carries forward: 2.70 / 1.30 = 27 000 / 13
    assert_eq!(rate(&items[1], "o"), ("2076".into(), "27000".into(), "13".into()));
    assert_eq!((items[1]["a_traded"].as_bool(), items[1]["b_traded"].as_bool()), (Some(true), Some(false)));
    assert_eq!(items[1]["pair_fills"], 1);
    // errors and an unknown pair
    assert_eq!(get(&app, &format!("/v1/pairs/{}/{}/candles?interval=7m", h(BASE), h(QUOTE))).await.0, StatusCode::BAD_REQUEST);
    let (s, v) = get(&app, &format!("/v1/pairs/{}/{}/candles", h(0x55), h(0x56))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["items"].as_array().unwrap().is_empty());

    // pair fills, newest first, with their counterparty and no price source; the 24 h pair volume
    let (s, v) = get(&app, &format!("/v1/pairs/{}/{}/fills", h(BASE), h(QUOTE))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!((items[0]["counterparty"].as_str(), items[0]["price_source"].as_str()), (Some("route"), Some("none")));
    assert_eq!((items[1]["counterparty"].as_str(), items[1]["side"].as_str()), (Some("netting"), Some("bid")));
    assert_eq!(
        (items[2]["price"].as_str(), items[2]["price_num"].as_str(), items[2]["price_den"].as_str()),
        (Some("2000"), Some("2"), Some("1"))
    );
    assert_eq!((items[2]["amount_a"].as_str(), items[2]["amount_b"].as_str()), (Some("2000"), Some("4000")));
    assert_eq!(v["volume_24h"], serde_json::json!({"amount_a": "5000", "amount_b": "10000", "fills": 3}));
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/fills?limit=1", h(BASE), h(QUOTE))).await;
    let cursor = v["next_cursor"].as_str().unwrap().to_string();
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/fills?limit=5&before={cursor}", h(BASE), h(QUOTE))).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    assert!(v["next_cursor"].is_null());
    // the other orientation has no fills
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/fills", h(QUOTE), h(BASE))).await;
    assert!(v["items"].as_array().unwrap().is_empty());
}
