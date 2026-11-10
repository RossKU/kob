//! End-to-end tests of the REST and WebSocket API against a seeded on-disk SQLite database.

use super::*;
use crate::hex::{self, Hash32};
use crate::indexer::db::{open_writer, ReadPool};
use crate::indexer::ids::intern;
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
        template_id: None,
        powers: vec![],
    }]);
    let (events, _) = tokio::sync::broadcast::channel(16);
    let state = ApiState {
        pool,
        generation: Default::default(),
        health,
        events,
        tokens: Arc::new(tokens),
        cfg,
        settle_depth_daa: 100,
        daa_per_second: 10,
        lag_alarm_hours: vec![1, 6, 12, 24],
        min_order_value_sompi: 100_000_000,
        logo: None,
        price: None,
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
    let template = kob_protocol::artifacts::pinned_hash(if side == 1 {
        kob_protocol::artifacts::TemplateId::KobAsk
    } else {
        kob_protocol::artifacts::TemplateId::KobBid
    });
    c.execute(
        "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, scale, min_fill, price, tip, \
         expiry_daa, in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, unlisted_reason) \
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, 1000, 1000, ?7, 0, 100000000, 1, ?8, x'00', ?9, ?10, ?11, ?12)",
        params![
            cid(id),
            if side == 1 { "KobAsk" } else { "KobBid" },
            intern(c, &template).unwrap(),
            side,
            intern(c, &cid(maker)).unwrap(),
            intern(c, &cid(TOKEN)).unwrap(),
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
        "INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), ?2, ?3, ?4, ?8, ?5, 0, 123456789012, 1, ?6, ?7)",
        params![cid(id), status, initial.unwrap_or(0) - remaining.unwrap_or(0), remaining, cid(id + 100), block, block * 100, initial.is_some() as i64],
    )
    .unwrap();
    // ask-side orders are liquidity only with an exact live custody UTXO (`amountLeft` base units)
    if side == 1 && remaining.unwrap_or(0) > 0 {
        intern(c, &cid(TOKEN)).unwrap();
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
             VALUES (?1, 1, (SELECT id FROM ids WHERE h = ?2), (SELECT id FROM orders WHERE covenant_id = ?3), ?4, 1000, 'custody', ?5, ?6)",
            params![cid(id + 100), cid(TOKEN), cid(id), remaining.unwrap(), block, block * 100],
        )
        .unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn seed_event(c: &Connection, cov: u8, block: i64, daa: i64, kind: &str, side: i64, amount: i64, price: i64) {
    intern(c, &cid(TOKEN)).unwrap();
    c.execute(
        "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, payout, closes, detail) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), ?2, ?3, 1700000000000, ?4, 0, ?5, (SELECT id FROM ids WHERE h = ?6), ?7, ?8, ?9, 5000, 0, '{\"k\":1}')",
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
    // the build that answers: the crate version, its commit when the build set one, the KOB1 payload version it handles
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["commit"].as_str(), option_env!("KOB_GIT_COMMIT").map(str::trim).filter(|c| !c.is_empty()));
    assert_eq!(v["payload_version"], kob_protocol::payload::PAYLOAD_VERSION);
    // the smallest order the executor lists, in sompi as a decimal string (the web app warns below it)
    assert_eq!(v["min_order_value_sompi"], "100000000");
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

/// The hot reads are kept for the store's data generation (`api::cache`): within one the answer is the kept one, and a
/// change the write path commits (a new generation) is read at once. The reads that depend on the clock key on it as well.
#[tokio::test]
async fn hot_reads_are_kept_for_one_data_generation() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let asks = |v: &Value| v["tokens"][0]["open_asks"].as_i64();
    let (_, v) = get(&app, "/v1/tokens").await;
    assert_eq!(asks(&v), Some(3));
    // what the follower would commit with a new generation: ask 1 filled
    e.w.execute(
        "UPDATE order_state SET status = 'filled' WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
        params![cid(1)],
    )
    .unwrap();
    let (_, v) = get(&app, "/v1/tokens").await;
    assert_eq!(asks(&v), Some(3), "the same generation: the kept answer");
    e.state.generation.bump();
    let (_, v) = get(&app, "/v1/tokens").await;
    assert_eq!(asks(&v), Some(2), "a new generation reads the store again");
    // a book at another node DAA score is another read
    let book = format!("/v1/books/{}?aggregate=false", h(TOKEN));
    let (_, v) = get(&app, &book).await;
    assert_eq!(ids(&v["asks"]), vec![h(2), h(3)]);
    e.w.execute("UPDATE orders SET active_from = 1001 WHERE covenant_id = ?1", params![cid(3)]).unwrap();
    e.state.generation.bump();
    let (_, v) = get(&app, &book).await;
    assert_eq!(ids(&v["asks"]), vec![h(2)]);
    e.state.health.update(|h| h.node_daa = Some(1001));
    let (_, v) = get(&app, &book).await;
    assert_eq!(ids(&v["asks"]), vec![h(2), h(3)], "active from DAA 1001");
}

/// A token whose registry entry says `genesis_verified: true`: its order views carry no warning.
#[tokio::test]
async fn an_order_of_a_genesis_verified_token_has_no_warning() {
    let mut e = env(|_| {});
    seed_all(&e.w);
    let tokens = (*e.state.tokens).clone().with_genesis([(Hash32([TOKEN; 32]), true)]);
    e.state.tokens = Arc::new(tokens);
    let (_, v) = get(&router(e.state.clone()), &format!("/v1/orders/{}", h(2))).await;
    assert_eq!(v["warnings"], serde_json::json!([]));
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
    // every order of the book is fillable now (an expired one is not in the book: `the_kas_book_lists_only_...`)
    assert_eq!(v["asks"][0]["expired"], false);

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

/// The KAS book (`/v1/books`, aggregated and per order, and the stats' best bid and ask) lists only the orders a conforming
/// matcher may fill at the node's DAA score (`docs/spec/matcher.md` §2.2, §2.3), as the pair books do: not one past its soft
/// expiry, nor one not yet active, nor an IOC past its kill time, nor a day order past its deadline. They stay in
/// `/v1/orders`, flagged `expired`, for their makers and the refund keepers.
#[tokio::test]
async fn the_kas_book_lists_only_orders_a_matcher_may_fill_now() {
    let e = env(|_| {});
    seed_all(&e.w);
    let set = |sql: &str, id: u8| {
        e.w.execute(sql, params![cid(id)]).unwrap();
    };
    // node DAA 1000: ask 2 expires at it, ask 3 activates after it, ask 1 is an IOC whose kill time passed (its UTXO at DAA
    // 100, killed from 700), bid 7 a day order whose deadline passed
    set("UPDATE orders SET expiry_daa = 1000 WHERE covenant_id = ?1", 2);
    set("UPDATE orders SET active_from = 1001 WHERE covenant_id = ?1", 3);
    set("UPDATE orders SET tif = 1 WHERE covenant_id = ?1", 1);
    set("UPDATE orders SET deadline = 1 WHERE covenant_id = ?1", 7);
    let app = router(e.state.clone());
    let (s, v) = get(&app, &format!("/v1/books/{}", h(TOKEN))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["asks"].as_array().unwrap().is_empty(), "{v}");
    let bids = v["bids"].as_array().unwrap();
    assert_eq!(bids.len(), 1, "{v}");
    assert_eq!(bids[0]["price"], "240000");
    let (_, v) = get(&app, &format!("/v1/books/{}?aggregate=false", h(TOKEN))).await;
    assert!(ids(&v["asks"]).is_empty(), "{v}");
    assert_eq!(ids(&v["bids"]), vec![h(6)]);
    // the orders themselves stay visible, the expired one flagged
    let (_, v) = get(&app, &format!("/v1/orders/{}", h(2))).await;
    assert_eq!((v["status"].as_str(), v["expired"].as_bool()), (Some("partial"), Some(true)));
    // one block earlier every one of them was fillable (the IOC from its activation to its kill time)
    e.state.health.update(|h| h.node_daa = Some(650));
    set("UPDATE orders SET active_from = 600 WHERE covenant_id = ?1", 3);
    set("UPDATE orders SET deadline = NULL WHERE covenant_id = ?1", 7);
    let (_, v) = get(&app, &format!("/v1/books/{}?aggregate=false", h(TOKEN))).await;
    assert_eq!(ids(&v["asks"]), vec![h(2), h(3), h(1)]);
    assert_eq!(ids(&v["bids"]), vec![h(7), h(6)]);
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
    // base units and whole-token prices only: no field of the older lot geometry is left in the view
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

    // docs/spec/matcher.md 1.2a: the token's genesis is not verified: every order view carries the warning
    assert_eq!(v["warnings"], serde_json::json!(["genesis_unverified"]));
    let (_, page) = get(&app, &format!("/v1/orders?token={}", h(TOKEN))).await;
    let items = page["items"].as_array().unwrap();
    assert!(!items.is_empty() && items.iter().all(|o| o["warnings"] == serde_json::json!(["genesis_unverified"])), "{page}");

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
    intern(&e.w, &cid(TOKEN)).unwrap();
    e.w.execute(
        "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, payout, closes, detail) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), 7, 900, 1700000000000, ?2, 0, 'arm', (SELECT id FROM ids WHERE h = ?3), 1, NULL, NULL, NULL, 0, ?4)",
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
            // the quantities of a fill are decimal strings, as in REST
            assert_eq!(f["data"]["amount"], "2000");
            assert_eq!(f["data"]["price"], "250000");
            assert!(f["data"]["payout"].is_null());
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
    intern(c, &cid(token)).unwrap();
    intern(c, owner).unwrap();
    c.execute(
        "INSERT INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa, spent_block, spent_txid) \
         VALUES (?1, ?2, (SELECT id FROM ids WHERE h = ?3), ?4, ?5, (SELECT id FROM ids WHERE h = ?6), ?7, ?8, 1000000000, ?9, 'owned', ?10, ?11, ?12, ?13)",
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
    intern(c, &cid(TOKEN)).unwrap();
    c.execute(
        "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, payout, closes, detail) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), 9, 990, ?2, ?3, 0, 'fill', (SELECT id FROM ids WHERE h = ?4), ?5, ?6, ?7, NULL, 0, NULL)",
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
    // depth within 2 % of the mid (255 000: 249 900 .. 260 100): the 250 000 ask level (9 000 base units = 2 250 000 sompi) and the 260 000 bid level (7 000 = 1 820 000);
    // the 300 000 ask and the 240 000 bid are outside
    assert_eq!(v["depth_2pct_quote"], "4070000");

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
    assert!(v["last"].is_null() && v["best_bid"].is_null() && v["depth_2pct_quote"].is_null());
    assert_eq!(v["trades_24h"], 0);
    let (_, v) = get(&app, &format!("/v1/candles/{}", h(0x55))).await;
    assert!(v["items"].as_array().unwrap().is_empty());
}

/// The raw answer of `uri`: status, headers and the body bytes as they came (not decoded).
async fn raw(app: &Router, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(peer("127.0.0.1"));
    let (parts, body) = app.clone().oneshot(req).await.unwrap().into_parts();
    (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
}

/// A JSON read is gzip for a client that accepts it and plain for one that does not; small bodies, the Prometheus text and an `Accept-Encoding` without gzip are plain.
#[tokio::test]
async fn json_answers_are_gzip_only_for_a_client_that_accepts_it() {
    use std::io::Read;
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let (s, h0, plain) = raw(&app, "/v1/orders", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h0.get("content-encoding").is_none(), "no Accept-Encoding: the JSON as it is");
    let want: Value = serde_json::from_slice(&plain).expect("plain JSON");
    assert!(plain.len() > 1_000, "the fixture is big enough to compress: {} B", plain.len());

    let (s, hz, packed) = raw(&app, "/v1/orders", &[("accept-encoding", "gzip")]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hz.get("content-encoding").and_then(|v| v.to_str().ok()), Some("gzip"));
    assert_eq!(hz.get("content-type").and_then(|v| v.to_str().ok()), Some("application/json"));
    assert!(
        hz.get("vary").and_then(|v| v.to_str().ok()).is_some_and(|v| v.to_ascii_lowercase().contains("accept-encoding")),
        "{hz:?}"
    );
    assert!(packed.len() * 2 < plain.len(), "{} B gzip of {} B", packed.len(), plain.len());
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&packed[..]).read_to_end(&mut out).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), want, "the same document");

    // a cached read (the response cache keeps the plain body) and a browser's list of encodings
    let (_, hc, _) = raw(&app, &format!("/v1/stats/{}", h(TOKEN)), &[("accept-encoding", "br, gzip, deflate")]).await;
    assert!(hc.get("content-encoding").is_none(), "a body below the threshold is sent as it is");
    let (_, hb, _) = raw(&app, "/v1/orders", &[("accept-encoding", "br")]).await;
    assert!(hb.get("content-encoding").is_none(), "only gzip is offered");
    let (_, hi, _) = raw(&app, "/v1/orders", &[("accept-encoding", "identity")]).await;
    assert!(hi.get("content-encoding").is_none());

    // the Prometheus text is not compressed, nor is a tiny answer
    let (s, hm, _) = raw(&app, "/v1/metrics", &[("accept-encoding", "gzip")]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(hm.get("content-encoding").is_none());
    let (_, hr, _) = raw(&app, "/v1/health/ready", &[("accept-encoding", "gzip")]).await;
    assert!(hr.get("content-encoding").is_none());
}

/// The `Cache-Control` each class of route sends through the whole router (the policy table itself is tested in `edge_cache`).
#[tokio::test]
async fn cache_control_by_route_class() {
    let e = env(|_| {});
    seed_all(&e.w);
    let app = router(e.state.clone());
    let cc = |h: &HeaderMap| h.get("cache-control").and_then(|v| v.to_str().ok()).map(str::to_string);
    let t0 = 1_700_000_000_000i64;
    for (uri, want) in [
        (format!("/v1/books/{}", h(TOKEN)), "public, max-age=1, s-maxage=1"),
        ("/v1/orders".to_string(), "public, max-age=1, s-maxage=1"),
        (format!("/v1/orders?maker={}", h(MAKER)), "public, max-age=1, s-maxage=1"),
        ("/v1/fills".to_string(), "public, max-age=1, s-maxage=1"),
        (format!("/v1/trades/{}", h(TOKEN)), "public, max-age=1, s-maxage=1"),
        ("/v1/tokens".to_string(), "public, max-age=5, s-maxage=5, stale-while-revalidate=5"),
        ("/v1/pairs".to_string(), "public, max-age=5, s-maxage=5, stale-while-revalidate=5"),
        ("/v1/stats".to_string(), "public, max-age=10, s-maxage=10, stale-while-revalidate=10"),
        (format!("/v1/stats/{}", h(TOKEN)), "public, max-age=2, s-maxage=2"),
        (format!("/v1/candles/{}?interval=1h", h(TOKEN)), "public, max-age=3, s-maxage=3"),
        (format!("/v1/candles/{}?interval=1h&to={}", h(TOKEN), t0), "public, max-age=3600, s-maxage=3600"),
        ("/v1/health".to_string(), "no-store"),
        ("/v1/health/ready".to_string(), "no-store"),
        ("/v1/metrics".to_string(), "no-store"),
        (format!("/v1/books/{}?_nc=123", h(TOKEN)), "no-store"),
        // an error is never kept: a bad token, an unknown route, a bad range
        ("/v1/books/zz".to_string(), "no-store"),
        ("/v1/nothing".to_string(), "no-store"),
        (format!("/v1/candles/{}?interval=2m", h(TOKEN)), "no-store"),
    ] {
        let (_, hd, _) = get_from(&app, &uri, "127.0.0.1", &[]).await;
        assert_eq!(cc(&hd).as_deref(), Some(want), "{uri}");
    }
    // a compressed answer carries it too, and the bypass parameter changes nothing else
    let (_, hz, _) = raw(&app, "/v1/orders", &[("accept-encoding", "gzip")]).await;
    assert_eq!(cc(&hz).as_deref(), Some("public, max-age=1, s-maxage=1"));
    let (s, _, v) = get_from(&app, &format!("/v1/books/{}?_nc=9", h(TOKEN)), "127.0.0.1", &[]).await;
    let (s0, _, v0) = get_from(&app, &format!("/v1/books/{}", h(TOKEN)), "127.0.0.1", &[]).await;
    assert_eq!((s, v), (s0, v0), "the server ignores the bypass parameter");
    // a POST is never kept
    let mut req = Request::builder().method("POST").uri("/v1/orders").body(Body::empty()).unwrap();
    req.extensions_mut().insert(peer("127.0.0.1"));
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(cc(resp.headers()).as_deref(), Some("no-store"));
}

/// `GET /v1/stats?since=`: the activity totals over a range of the store, all time without `since`.
#[tokio::test]
async fn activity_totals_over_a_range() {
    let e = env(|_| {});
    let c = &e.w;
    // blocks 1 and 2 are old, 3 and 4 recent: makers MAKER (orders 1, 3), 0xbb (order 2), 0xcc (order 4)
    seed_order(c, 1, 1, 300_000, 1, "open", Some(10 * W), Some(10 * W), true, MAKER);
    seed_order(c, 2, 1, 250_000, 2, "partial", Some(10 * W), Some(4 * W), true, 0xbb);
    seed_order(c, 3, 1, 250_000, 3, "open", Some(5 * W), Some(5 * W), true, MAKER);
    seed_order(c, 4, 2, 240_000, 4, "open", None, Some(3 * W), true, 0xcc);
    // tips: order 1 and 3 pay 2 000 sompi per whole token, order 2 pays 500, order 4 none
    c.execute("UPDATE orders SET tip = 2000 WHERE covenant_id IN (?1, ?2)", params![cid(1), cid(3)]).unwrap();
    c.execute("UPDATE orders SET tip = 500 WHERE covenant_id = ?1", params![cid(2)]).unwrap();
    let t0 = 1_700_000_000_000i64;
    // the scripts that funded the fills: A the old one (block 2), B the one of block 3; block 4's is not recorded
    let a = crate::indexer::activity::intern_script(c, &[0, 0, 0x20, 0xa1, 0xac]).unwrap();
    let b = crate::indexer::activity::intern_script(c, &[0, 0, 0x20, 0xb1, 0xac]).unwrap();
    assert_eq!(crate::indexer::activity::intern_script(c, &[0, 0, 0x20, 0xa1, 0xac]).unwrap(), a, "once per script");
    for (cov, block, ts, kind, amount, tip, executor) in [
        (1u8, 1i64, t0 - 10 * 86_400_000, "create", 0, 0, None),
        (2, 2, t0 - 9 * 86_400_000, "create", 0, 0, None),
        (3, 3, t0 - 60_000, "create", 0, 0, None),
        (4, 4, t0, "create", 0, 0, None),
        (2, 2, t0 - 9 * 86_400_000, "fill", 2 * W, 500, Some(a)),
        (1, 3, t0 - 30_000, "fill", W, 2000, Some(b)),
        (3, 4, t0, "fill", 3 * W / 2, 2000, None),
    ] {
        let fee = (kind == "fill").then(|| crate::indexer::activity::fill_fee(Some(amount), Some(tip), Some(W)).unwrap());
        c.execute(
            "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, fee, executor_id)              VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), ?2, ?3, ?4, ?5, 0, ?6, (SELECT id FROM ids WHERE h = ?7), 1, ?8, 250000, 0, ?9, ?10)",
            params![cid(cov), block, block * 100, ts, cid(block as u8 + 60), kind, cid(TOKEN), amount, fee, executor],
        )
        .unwrap();
    }
    // live order UTXOs: two unspent (1 000 000 each) and one spent
    c.execute("INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa, spent_block) VALUES (?1, 0, (SELECT id FROM orders WHERE covenant_id = ?2), 7, x'00', 1, 100, 1)", params![cid(0x90), cid(1)]).unwrap();
    for (n, cov) in [(0x91u8, 1u8), (0x92, 4)] {
        c.execute("INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa) VALUES (?1, 0, (SELECT id FROM orders WHERE covenant_id = ?2), 1000000, x'00', 1, 100)", params![cid(n), cid(cov)]).unwrap();
    }
    let app = router(e.state.clone());

    // all time: 4 orders, 3 makers; fees 2 whole x 500 + 1 whole x 2000 + 1.5 whole x 2000 = 6 000; locked 2 000 000
    let (s, v) = get(&app, "/v1/stats").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["since"].is_null());
    assert_eq!((v["orders"].as_i64(), v["traders"].as_i64(), v["fills"].as_i64()), (Some(4), Some(3), Some(3)));
    assert_eq!((v["fees_paid"].as_str(), v["kas_locked"].as_str()), (Some("6000"), Some("2000000")));
    assert_eq!(v["executors"].as_i64(), Some(2), "A and B; the fill without a recorded script is not counted");
    assert_eq!(v["ts"].as_i64(), Some(t0));

    // the last day: orders 3 and 4 (makers MAKER and 0xcc), the fills of blocks 3 and 4 (1 whole and 1.5 whole at 2 000)
    let (s, v) = get(&app, &format!("/v1/stats?since={}", t0 - 86_400_000 + 17)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["since"].as_i64(), Some((t0 - 86_400_000 + 17) / 60_000 * 60_000), "rounded down to the minute");
    assert_eq!((v["orders"].as_i64(), v["traders"].as_i64(), v["fills"].as_i64()), (Some(2), Some(2), Some(2)));
    assert_eq!(v["fees_paid"], "5000");
    assert_eq!(v["executors"].as_i64(), Some(1), "B");
    // from a minute inside an hour: the minutes up to the hour and the hours after it, every bucket once
    let (_, v) = get(&app, &format!("/v1/stats?since={}", t0 - 30_000)).await;
    assert_eq!((v["fills"].as_i64(), v["fees_paid"].as_str()), (Some(2), Some("5000")));
    let (_, v) = get(&app, &format!("/v1/stats?since={}", t0 - 10_000)).await;
    assert_eq!((v["fills"].as_i64(), v["fees_paid"].as_str(), v["orders"].as_i64()), (Some(1), Some("3000"), Some(1)));
    assert_eq!(v["kas_locked"], "2000000", "locked is the state now, whatever the range");

    // a range with nothing in it
    let (_, v) = get(&app, &format!("/v1/stats?since={}", t0 + 3_600_000)).await;
    assert_eq!((v["orders"].as_i64(), v["traders"].as_i64(), v["fees_paid"].as_str()), (Some(0), Some(0), Some("0")));
    assert_eq!((v["fills"].as_i64(), v["executors"].as_i64()), (Some(0), Some(0)));

    // the aggregates follow a delete as a revert does it: the fills and orders of block 4 go, and the totals are those of blocks 1 to 3
    c.execute_batch("DELETE FROM order_events WHERE block_seq = 4; DELETE FROM orders WHERE genesis_block = 4;").unwrap();
    e.state.generation.bump();
    let (_, v) = get(&app, "/v1/stats").await;
    assert_eq!((v["orders"].as_i64(), v["traders"].as_i64(), v["fills"].as_i64()), (Some(3), Some(2), Some(2)));
    assert_eq!((v["fees_paid"].as_str(), v["executors"].as_i64()), (Some("3000"), Some(2)));
    c.execute_batch("DELETE FROM order_events WHERE block_seq = 3; DELETE FROM orders WHERE genesis_block = 3;").unwrap();
    e.state.generation.bump();
    let (_, v) = get(&app, "/v1/stats").await;
    assert_eq!((v["orders"].as_i64(), v["traders"].as_i64(), v["fills"].as_i64()), (Some(2), Some(2), Some(1)));
    assert_eq!((v["fees_paid"].as_str(), v["executors"].as_i64()), (Some("1000"), Some(1)));
    let left: i64 = c.query_row("SELECT COUNT(*) FROM activity WHERE t >= ?1", [t0 - 3_600_000], |r| r.get(0)).unwrap();
    assert_eq!(left, 0, "an emptied bucket is no row");

    let (s, _) = get(&app, "/v1/stats?since=-5").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&app, "/v1/stats?since=yesterday").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// An empty store answers zeros, never an error.
#[tokio::test]
async fn activity_totals_of_an_empty_store() {
    let e = env(|_| {});
    let (s, v) = get(&router(e.state.clone()), "/v1/stats").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["orders"].as_i64(), v["traders"].as_i64(), v["fees_paid"].as_str(), v["kas_locked"].as_str()),
        (Some(0), Some(0), Some("0"), Some("0"))
    );
    assert_eq!((v["fills"].as_i64(), v["executors"].as_i64()), (Some(0), Some(0)));
    assert!(v["ts"].is_null());
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
        "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, scale, min_fill, price, tip, tif, \
         expiry_daa, active_from, in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, quote_id) \
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?14, ?15, ?16, ?17, 1, ?18)",
        params![
            cid(id),
            t.name(),
            intern(c, &kob_protocol::artifacts::pinned_hash(t)).unwrap(),
            crate::model::side_of(state) as i64,
            intern(c, &cid(MAKER)).unwrap(),
            intern(c, &state.token_cov_id()).unwrap(),
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
            quote.map(|q| intern(c, &q).unwrap()),
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_utxos (txid, idx, order_id, value, spk, state, created_block, created_daa) VALUES (?1, 0, (SELECT id FROM orders WHERE covenant_id = ?2), 5000000000, x'00', ?3, ?4, ?5)",
        params![cid(id + 100), cid(id), bytes, block, block * 100],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), 'open', 0, ?2, 1, ?3, 0, 5000000000, 1, ?4, ?5)",
        params![cid(id), remaining, cid(id + 100), block, block * 100],
    )
    .unwrap();
    let custodies: Vec<([u8; 32], i64)> = if state.is_pair() {
        state.custodies()
    } else {
        state.custody_amount().filter(|a| *a > 0).map(|a| (state.token_cov_id(), a)).into_iter().collect()
    };
    for (k, (token, amount)) in custodies.into_iter().enumerate() {
        intern(c, &token).unwrap();
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
             VALUES (?1, ?2, (SELECT id FROM ids WHERE h = ?3), (SELECT id FROM orders WHERE covenant_id = ?4), ?5, 1000, 'custody', ?6, ?7)",
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
        s_ext: [0xee; 32],
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
    e.w.execute("DELETE FROM token_utxos WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)", [cid(0x34)]).unwrap();
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
    intern(c, &cid(token)).unwrap();
    c.execute(
        "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, payout, closes, detail) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), 9, 990, ?2, ?3, 0, 'fill', (SELECT id FROM ids WHERE h = ?4), ?5, ?6, ?7, NULL, 0, NULL)",
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

/// `GET /v1/tokens/{covenant_id}/declaration`: the KCC-23 launch proof the indexer holds (as the write path stores it), 404 in
/// the API's error shape when it holds none, 400 for a malformed id; `GET /v1/tokens` flags the tokens that have one.
#[tokio::test]
async fn token_declarations_are_served_as_launch_proofs() {
    use crate::indexer::kcc23::{apply, proof_tx, DeclSection};
    use crate::testkit::{kcc23_genesis, kcc23_prep, kcc23_txid, kcc23_wire, KCC23_EXAMPLE};
    let mut e = env(|_| {});
    seed_all(&e.w);
    let prep = kcc23_prep(1, kob_protocol::kcc23::encode(KCC23_EXAMPLE).unwrap());
    let (gen, cov) = kcc23_genesis(&prep, 0, 1);
    let ptx = |t| proof_tx(&kcc23_wire(t, false)).unwrap();
    apply(&e.w, 9, 990, &kcc23_txid(&prep), &DeclSection { tx: ptx(&prep), preparation: true, binds: vec![], refs: vec![] }).unwrap();
    let declared = apply(
        &e.w,
        9,
        990,
        &kcc23_txid(&gen),
        &DeclSection { tx: ptx(&gen), preparation: false, binds: vec![ptx(&prep)], refs: vec![] },
    );
    assert_eq!(declared.unwrap(), vec![cov]);
    let mut entries = e.state.tokens.list().into_iter().cloned().collect::<Vec<_>>();
    entries.push(TokenEntry { ticker: "EXT".into(), covenant_id: cov, decimals: Some(8), ..entries[0].clone() });
    e.state.tokens = Arc::new(TokenAllowlist::from_entries(entries));
    let app = router(e.state.clone());

    let (s, headers, v) = get_from(&app, &format!("/v1/tokens/{cov}/declaration"), "127.0.0.1", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "application/json");
    let proof = kob_protocol::kcc23::LaunchProof::of(&prep, &gen, kaspa_consensus_core::Hash::from_bytes(cov.0));
    assert_eq!(v, serde_json::to_value(&proof).unwrap());
    assert_eq!(v["format"], "kob-kcc23-proof/1");
    // what a client does with it
    let back: kob_protocol::kcc23::LaunchProof = serde_json::from_value(v).unwrap();
    assert_eq!(back.verify().unwrap().token.symbol.as_deref(), Some("EXT"));

    let (s, v) = get(&app, &format!("/v1/tokens/{}/declaration", h(TOKEN))).await;
    assert_eq!((s, v["error"]["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
    let (s, v) = get(&app, "/v1/tokens/zz/declaration").await;
    assert_eq!((s, v["error"]["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));

    let (_, v) = get(&app, "/v1/tokens").await;
    let row = |id: &str| v["tokens"].as_array().unwrap().iter().find(|t| t["covenant_id"] == id).cloned().unwrap();
    assert_eq!(row(&cov.to_hex())["declaration"], true);
    assert!(row(&h(TOKEN)).get("declaration").is_none(), "absent without a declaration (older clients see no new key)");
}

/// `/v1/health` reports the minimum order value the executor lists, whatever its configuration sets.
#[tokio::test]
async fn health_reports_the_configured_minimum_order_value() {
    let mut e = env(|_| {});
    e.state.min_order_value_sompi = 250_000_000;
    let (s, v) = get(&router(e.state.clone()), "/v1/health").await;
    assert_eq!((s, v["min_order_value_sompi"].as_str()), (StatusCode::OK, Some("250000000")));
    e.state.min_order_value_sompi = 0;
    let (_, v) = get(&router(e.state.clone()), "/v1/health").await;
    assert_eq!(v["min_order_value_sompi"], "0");
}

/// The decimals of a token, which are the price basis of the market views (candles, stats, trades, depth, pair candles) and the `decimals`
/// of `GET /v1/tokens`: its verified KCC-23 declaration, else its registry entry, else its program's default (KaspaRocket: 3), else
/// none (the scale of its first order); `decimals_source` names which.
#[tokio::test]
async fn token_decimals_follow_the_declaration_then_the_registry_then_the_program_default() {
    use crate::indexer::kcc23::{apply, proof_tx, DeclSection};
    use crate::testkit::{kcc23_genesis, kcc23_prep, kcc23_txid, kcc23_wire, KCC23_EXAMPLE};
    use crate::tokens::StrictTemplate;
    const ROCKET: u8 = 0x61; // a KaspaRocket token without an entry or a declaration
    const REGISTERED: u8 = 0x62; // a KaspaRocket token with a registry entry that says 5
    const PLAIN: u8 = 0x63; // a token nothing is known about
    let mut e = env(|_| {});
    seed_all(&e.w);
    let profile = kob_protocol::profile::by_name("KaspaRocketToken11458").expect("the KaspaRocket program");
    assert_eq!(profile.default_decimals.as_ref().map(|d| d.decimals), Some(3));
    let tpl = Hash32::parse(&profile.template_hash).unwrap();
    let seen = |cov: &Hash32| {
        e.w.execute(
            "INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) VALUES (?1, ?2, NULL, 'seen', ?3, 9, 990)",
            params![&cov.0[..], &tpl.0[..], &cov.0[..]],
        )
        .unwrap();
    };
    seen(&Hash32([ROCKET; 32]));
    seen(&Hash32([REGISTERED; 32]));
    // a token that declared 8 decimals at its creation, and is of the KaspaRocket program too
    let prep = kcc23_prep(1, kob_protocol::kcc23::encode(KCC23_EXAMPLE).unwrap());
    let (gen, declared) = kcc23_genesis(&prep, 0, 1);
    let ptx = |t| proof_tx(&kcc23_wire(t, false)).unwrap();
    apply(&e.w, 9, 990, &kcc23_txid(&prep), &DeclSection { tx: ptx(&prep), preparation: true, binds: vec![], refs: vec![] }).unwrap();
    apply(&e.w, 9, 990, &kcc23_txid(&gen), &DeclSection { tx: ptx(&gen), preparation: false, binds: vec![ptx(&prep)], refs: vec![] })
        .unwrap();
    seen(&declared);
    let mut entries = e.state.tokens.list().into_iter().cloned().collect::<Vec<_>>();
    entries.push(TokenEntry {
        ticker: "REG".into(),
        covenant_id: Hash32([REGISTERED; 32]),
        template_hash: Some(tpl),
        decimals: Some(5),
        ..entries[0].clone()
    });
    let strict = StrictTemplate { id: profile.registry_id.clone(), family: kob_protocol::Family::Kcc20, powers: vec![] };
    e.state.tokens = Arc::new(TokenAllowlist::from_entries(entries).with_strict_templates([(tpl, strict)]));
    let app = router(e.state.clone());

    for (token, want) in [
        (h(ROCKET), Some(3)),         // the program default
        (h(REGISTERED), Some(5)),     // the registry entry, not the program default
        (declared.to_hex(), Some(8)), // the declaration, not the program default
        (h(TOKEN), Some(3)),          // the registry entry of the fixture
        (h(PLAIN), None),             // nothing says: the first order's scale
    ] {
        for path in ["stats", "candles", "trades", "depth"] {
            let (s, v) = get(&app, &format!("/v1/{path}/{token}")).await;
            assert_eq!(s, StatusCode::OK, "{path} {token}");
            assert_eq!(v["decimals"].as_i64(), want, "{path} of {token}: {v}");
        }
    }
    let (_, v) = get(&app, &format!("/v1/pairs/{}/{}/candles", h(ROCKET), h(REGISTERED))).await;
    assert_eq!((v["decimals"].as_i64(), v["quote_decimals"].as_i64()), (Some(3), Some(5)), "{v}");

    let (_, v) = get(&app, "/v1/tokens").await;
    let row = |id: &str| {
        v["tokens"].as_array().unwrap().iter().find(|t| t["covenant_id"] == id).cloned().unwrap_or_else(|| panic!("{id} listed"))
    };
    let facts = |id: &str| {
        let r = row(id);
        (r["decimals"].as_i64(), r["decimals_source"].as_str().map(str::to_string), r["scale"].as_i64())
    };
    assert_eq!(facts(&h(ROCKET)), (Some(3), Some("program".into()), None), "the default of the program; the scale stays the market's");
    assert_eq!(facts(&h(REGISTERED)), (Some(5), Some("registry".into()), Some(100_000)));
    assert_eq!(facts(&declared.to_hex()), (Some(8), Some("declaration".into()), None));
    assert_eq!(facts(&h(TOKEN)), (Some(3), Some("registry".into()), Some(1000)));
}

/// While the store catches up with a lag below the first alarm threshold the probe answers 200 (it is not "caught up": that is
/// `state = following`); the health reports the pace and ETA of the catch-up, and a store that began after the oldest history says so.
#[tokio::test]
async fn ready_while_catching_up_and_the_health_names_the_catch_up_and_a_partial_history() {
    let e = env(|_| {});
    let app = router(e.state.clone());
    e.state.health.update(|h| {
        h.state = FollowerState::CatchingUp;
        h.node_daa = Some(990 + 5_000);
        h.catch_up = Some(crate::indexer::status::CatchUp { rate_daa_per_sec: 150.0, eta_seconds: Some(36) });
        h.book_complete = false;
        h.history_from_daa = Some(990);
    });
    let (s, v) = get(&app, "/v1/health/ready").await;
    assert_eq!((s, &v["ready"], &v["state"]), (StatusCode::OK, &Value::Bool(true), &Value::String("catching_up".into())));
    let (s, v) = get(&app, "/v1/health").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((v["state"].as_str(), v["caught_up"].clone()), (Some("catching_up"), Value::Bool(false)));
    assert_eq!((v["catch_up"]["rate_daa_per_sec"].as_f64(), v["catch_up"]["eta_seconds"].as_u64()), (Some(150.0), Some(36)));
    assert_eq!((v["book_complete"].clone(), v["history_from_daa"].as_u64()), (Value::Bool(false), Some(990)));
    // a complete store that follows has neither field
    e.state.health.update(|h| {
        h.state = FollowerState::Following;
        h.node_daa = Some(1000);
        h.catch_up = None;
        h.book_complete = true;
        h.history_from_daa = None;
    });
    let (_, v) = get(&app, "/v1/health").await;
    assert!(v.get("catch_up").is_none() && v.get("history_from_daa").is_none());
    assert_eq!(v["book_complete"], true);
}

/// `GET /v1/metrics` is public like the rest of `/v1/`; `[api] metrics = false` removes the route.
#[tokio::test]
async fn the_metrics_route_follows_the_config() {
    let on = env(|_| {});
    let (s, _, _) = get_from(&router(on.state.clone()), "/v1/metrics", "127.0.0.1", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let off = env(|c| c.metrics = false);
    let app = router(off.state.clone());
    let (s, v) = get(&app, "/v1/metrics").await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    // everything else is still served
    assert_eq!(get(&app, "/v1/health").await.0, StatusCode::OK);
}

// ------------------------------------------------------------------------------------------------ logo reports

async fn post_with(app: &Router, body: &str, from: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().method("POST").uri("/v1/logo-reports").header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::from(body.to_string())).unwrap();
    req.extensions_mut().insert(peer(from));
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status, parts.headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn post_from(app: &Router, body: &str, from: &str) -> (StatusCode, HeaderMap, Value) {
    post_with(app, body, from, &[]).await
}

fn report_body(id: u8, reason: &str) -> String {
    format!("{{\"covenant_id\":\"{}\",\"reason\":\"{reason}\"}}", h(id))
}

struct LogoEnv {
    _env: Env,
    state: ApiState,
    logo: Arc<crate::logo_reports::LogoReports>,
    now: Arc<std::sync::atomic::AtomicI64>,
}

fn logo_env(cfg: crate::config::LogoReportsConfig, tweak: impl FnOnce(&mut ApiConfig)) -> LogoEnv {
    let e = env(tweak);
    let now = Arc::new(std::sync::atomic::AtomicI64::new(1_000_000));
    let clock = now.clone();
    let logo = Arc::new(crate::logo_reports::LogoReports::with_connection(
        crate::indexer::db::open_memory("testnet-10").unwrap(),
        cfg,
        64,
        Box::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
    ));
    let mut state = e.state.clone();
    state.logo = Some(logo.clone());
    LogoEnv { _env: e, state, logo, now }
}

fn enabled() -> crate::config::LogoReportsConfig {
    crate::config::LogoReportsConfig { enabled: true, ..Default::default() }
}

#[tokio::test]
async fn logo_routes_are_absent_while_the_feature_is_off() {
    let e = env(|_| {});
    let app = router(e.state.clone());
    let (s, _) = get(&app, "/v1/logo-blocklist").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = post_from(&app, &report_body(1, "other"), "127.0.0.1").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_report_hides_the_logo_at_once_and_the_blocklist_serves_it() {
    let e = logo_env(enabled(), |_| {});
    let app = router(e.state.clone());
    let (s, _, v) = get_from(&app, "/v1/logo-blocklist", "127.0.0.1", &[]).await;
    assert_eq!((s, v), (StatusCode::OK, serde_json::json!({"hidden": [], "updated_ms": 0})));
    let (s, _, v) = post_from(&app, &report_body(7, "impersonation"), "10.1.1.1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, serde_json::json!({"result": "hidden", "covenant_id": h(7)}));
    let (s, hd, v) = get_from(&app, "/v1/logo-blocklist", "127.0.0.1", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, serde_json::json!({"hidden": [h(7)], "updated_ms": 1_000_000}));
    assert_eq!(hd.get("cache-control").unwrap(), "public, max-age=5");
    // a GitHub report of the same token (upper-case hex), from another client: still hidden
    let gh = format!("{{\"covenant_id\":\"{}\",\"reason\":\"other\",\"source\":\"github\",\"ref\":12}}", h(7).to_uppercase());
    let (s, _, v) = post_from(&app, &gh, "10.1.1.2").await;
    assert_eq!((s, v["result"].as_str()), (StatusCode::OK, Some("hidden")));
}

#[tokio::test]
async fn a_report_in_the_cooldown_after_a_restore_is_only_recorded() {
    let e = logo_env(enabled(), |_| {});
    let app = router(e.state.clone());
    let body = report_body(3, "illegal");
    assert_eq!(post_from(&app, &body, "10.1.1.1").await.2["result"], "hidden");
    // the operator restores it (the subcommand writes through a connection of its own; here the service's)
    crate::logo_reports::restore(&mut e.logo.test_connection(), &Hash32([3; 32]), 1_000_000, false).unwrap();
    e.now.fetch_add(60_000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(post_from(&app, &body, "10.1.1.2").await.2["result"], "recorded");
    // the list is cached for 5 s, then shows the restored token as shown
    e.now.fetch_add(5_000, std::sync::atomic::Ordering::SeqCst);
    let (_, _, v) = get_from(&app, "/v1/logo-blocklist", "127.0.0.1", &[]).await;
    assert_eq!(v["hidden"], serde_json::json!([]));
    // after the cooldown (6 h) it hides again
    e.now.fetch_add(6 * 3_600_000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(post_from(&app, &body, "10.1.1.3").await.2["result"], "hidden");
    let (_, _, v) = get_from(&app, "/v1/logo-blocklist", "127.0.0.1", &[]).await;
    assert_eq!(v["hidden"], serde_json::json!([h(3)]));
}

#[tokio::test]
async fn bad_reports_are_refused_and_cost_no_budget() {
    let e = logo_env(crate::config::LogoReportsConfig { per_client_per_hour: 2, ..enabled() }, |_| {});
    let app = router(e.state.clone());
    for bad in [
        "not json".to_string(),
        "{}".to_string(),
        report_body(1, "spam"),
        format!("{{\"covenant_id\":\"{}\",\"reason\":\"other\",\"note\":\"free text\"}}", h(1)),
        "{\"covenant_id\":\"zz\",\"reason\":\"other\"}".to_string(),
        format!("{{\"covenant_id\":\"{}\",\"reason\":\"other\",\"source\":\"twitter\"}}", h(1)),
        format!("{{\"covenant_id\":\"{}\",\"reason\":\"other\",\"ref\":5}}", h(1)),
        format!("{{\"covenant_id\":\"{}\",\"reason\":\"other\",\"source\":\"github\",\"ref\":0}}", h(1)),
    ] {
        let (s, _, v) = post_from(&app, &bad, "10.1.1.1").await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    // an oversize body is refused before it is read
    let big = format!("{{\"covenant_id\":\"{}\",\"reason\":\"other\",\"x\":\"{}\"}}", h(1), "a".repeat(2000));
    assert_eq!(post_from(&app, &big, "10.1.1.1").await.0, StatusCode::PAYLOAD_TOO_LARGE);
    // none of that used the client's two reports per hour
    for n in 1..=2u8 {
        assert_eq!(post_from(&app, &report_body(n, "other"), "10.1.1.1").await.0, StatusCode::OK);
    }
    let (s, hd, v) = post_from(&app, &report_body(9, "other"), "10.1.1.1").await;
    assert_eq!((s, v["error"]["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")));
    assert!(hd.get("retry-after").is_some());
    // another client is not affected
    assert_eq!(post_from(&app, &report_body(9, "other"), "10.1.1.2").await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_daily_total_and_the_trusted_proxy_rule_bound_reports() {
    let e = logo_env(crate::config::LogoReportsConfig { max_per_day: 4, per_client_per_hour: 1, ..enabled() }, |c| {
        c.trusted_proxies = vec!["10.9.9.9".into()];
    });
    let app = router(e.state.clone());
    // behind the trusted proxy the forwarded address is the client: three clients, one report each
    for n in 1..=3u8 {
        let xff = format!("203.0.113.{n}");
        assert_eq!(post_with(&app, &report_body(n, "other"), "10.9.9.9", &[("x-forwarded-for", &xff)]).await.0, StatusCode::OK);
    }
    // the same forwarded client again: its hourly budget is spent (the proxy itself is not the client)
    let (s, _, _) = post_with(&app, &report_body(5, "other"), "10.9.9.9", &[("x-forwarded-for", "203.0.113.1")]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // a fourth client is admitted (the refused request above was not counted); a fifth finds the day's total spent
    assert_eq!(post_with(&app, &report_body(4, "other"), "10.9.9.9", &[("x-forwarded-for", "203.0.113.4")]).await.0, StatusCode::OK);
    let (s, _, _) = post_with(&app, &report_body(6, "other"), "10.9.9.9", &[("x-forwarded-for", "203.0.113.5")]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn cors_allows_the_post_of_a_report() {
    let e = logo_env(enabled(), |c| c.cors_allow_origin = Some("https://kob.example".into()));
    let app = router(e.state.clone());
    let mut req = Request::builder().method("OPTIONS").uri("/v1/logo-reports").body(Body::empty()).unwrap();
    req.extensions_mut().insert(peer("127.0.0.1"));
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(resp.headers().get("access-control-allow-methods").unwrap(), "GET, POST, OPTIONS");
    assert_eq!(resp.headers().get("access-control-expose-headers").unwrap(), "retry-after");
}

// the KAS/USD price route (`api::price`): a mocked upstream, no network

use crate::api::price::tests::{full, p_ok, set, t_ok, Mock, P, X, Y, Z};

fn price_env(m: &Mock) -> Env {
    let e = env(|_| {});
    let mut state = e.state.clone();
    state.price = Some(crate::api::price::UsdPrice::with_fetch(m.fetch(), set(), crate::api::price::Tuning::LIVE));
    Env { state, ..e }
}

#[tokio::test]
async fn the_price_route_is_absent_while_the_feature_is_off() {
    let e = env(|_| {});
    let (s, _) = get(&router(e.state.clone()), "/v1/price/kas-usd").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_price_route_serves_the_cached_quote_with_a_short_cache_header() {
    let m = full(Some("0.0576"));
    let app = router(price_env(&m).state.clone());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let (s, hd, v) = get_from(&app, "/v1/price/kas-usd", "127.0.0.1", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((v["usd"].as_f64(), v["source"].as_str()), (Some(0.0576), Some("alpha")));
    assert!(v["at"].as_u64().is_some_and(|at| at >= now - 5 && at <= now + 5), "{v}");
    assert_eq!(hd.get("cache-control").unwrap(), "public, max-age=60, s-maxage=60");
    // 25 more clients: no further call to any upstream, the same `at`
    for i in 0..25 {
        let (s, _, w) = get_from(&app, "/v1/price/kas-usd", &format!("10.0.0.{i}"), &[]).await;
        assert_eq!((s, w["at"].clone()), (StatusCode::OK, v["at"].clone()));
    }
    assert_eq!(m.calls(), 4, "one round: the primary and the three exchanges");
}

#[tokio::test]
async fn the_price_route_serves_the_exchange_average_and_names_the_exchanges_when_the_primary_failed() {
    let m = full(None).on(Z.url, vec![t_ok("0.0450", "5")]);
    let app = router(price_env(&m).state.clone());
    let (s, _, v) = get_from(&app, "/v1/price/kas-usd", "127.0.0.1", &[]).await;
    assert_eq!((s, v["source"].as_str()), (StatusCode::OK, Some("vwap:x,y")), "the outlier is not named");
    let usd = v["usd"].as_f64().unwrap();
    assert!((usd - (0.0300 * 100.0 + 0.0302 * 200.0) / 300.0).abs() < 1e-12, "{usd}");
}

#[tokio::test]
async fn the_price_route_answers_503_before_the_first_success_and_never_caches_it() {
    let m = Mock::default()
        .on(P.url, vec![Err("upstream down".into())])
        .on(X.url, vec![Err("down".into())])
        .on(Y.url, vec![Err("down".into())])
        .on(Z.url, vec![p_ok("0")]);
    let app = router(price_env(&m).state.clone());
    let (s, hd, v) = get_from(&app, "/v1/price/kas-usd", "127.0.0.1", &[]).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"]["code"], "price_unavailable");
    assert_eq!(hd.get("cache-control").unwrap(), "no-store");
    // inside the period the failed upstreams are not asked again
    assert_eq!(get(&app, "/v1/price/kas-usd").await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(m.calls(), 4, "one round");
}
