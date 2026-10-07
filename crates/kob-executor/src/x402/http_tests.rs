//! HTTP layer tests: admission, authentication, rate limits, kill switch, routes. In-process through
//! `tower::ServiceExt::oneshot`, plus a few over a real TCP listener (slow-loris).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use futures_util::stream::{self, StreamExt};
use http_body_util::BodyExt;
use kob_x402::wire::{header_decode, hex, SettlementResponse};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::config::{api_key_hash, X402Config};
use super::http::{json_depth_ok, route_list, router, serve, AppState, MAX_JSON_DEPTH};
use super::testutil::*;

const KEY1: &str = "secret-key-1";
const KEY2: &str = "secret-key-2";
const ID1: &str = "payment-id-0000000001";

struct App {
    app: Router,
    fx: Fixture,
    state: Arc<AppState>,
}

fn cfg_json(extra: &str) -> String {
    format!(
        r#"{{"merchants":[
            {{"id":"shop","apiKeySha256":"{}","allowedPayTo":["{}"],"allowedAssets":["KAS"]}},
            {{"id":"other","apiKeySha256":"{}","allowedPayTo":["{}"],"allowedAssets":["KAS"]}}
        ]{extra}}}"#,
        hex(&api_key_hash(KEY1)),
        address_of_key(MERCHANT_KEY),
        hex(&api_key_hash(KEY2)),
        address_of_key(PAYER_KEY),
    )
}

fn app_with(extra: &str) -> App {
    let fx = Fixture::new();
    let built = X402Config::from_json(&cfg_json(extra)).unwrap().build().unwrap();
    let state = Arc::new(AppState::new(fx.fac.clone(), &built));
    App { app: router(state.clone()), fx, state }
}

fn app() -> App {
    app_with("")
}

fn app_invoices() -> App {
    let fx = Fixture::with_invoices();
    let built = X402Config::from_json(&cfg_json("")).unwrap().build().unwrap();
    let state = Arc::new(AppState::new(fx.fac.clone(), &built));
    App { app: router(state.clone()), fx, state }
}

fn peer(ip: &str) -> ConnectInfo<SocketAddr> {
    ConnectInfo(format!("{ip}:5000").parse().unwrap())
}

async fn send(app: &Router, mut req: Request<Body>, ip: &str) -> (StatusCode, axum::http::HeaderMap, Bytes) {
    req.extensions_mut().insert(peer(ip));
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status, parts.headers, bytes)
}

fn post(path: &str, key: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder().method("POST").uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    b.body(Body::from(body)).unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
}

fn json_of(b: &Bytes) -> Value {
    serde_json::from_slice(b).unwrap_or_else(|_| panic!("not json: {:?}", String::from_utf8_lossy(b)))
}

fn body_of(req: &kob_x402::wire::FacilitatorRequest) -> Vec<u8> {
    serde_json::to_vec(req).unwrap()
}

fn mine_after_submit(chain: &Arc<kob_x402::testkit::MockChain>) -> std::thread::JoinHandle<()> {
    let c = chain.clone();
    let base = c.submit_count();
    std::thread::spawn(move || {
        let t = Instant::now();
        while c.submit_count() <= base && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(10));
        c.mine(0);
    })
}

// -------------------------------------------------------------------------------------------- routes

#[tokio::test]
async fn the_route_table_is_exactly_the_public_api_and_has_no_reservation_endpoints() {
    let mut routes = route_list();
    routes.sort();
    assert_eq!(
        routes,
        vec![
            ("GET", "/health"),
            ("GET", "/invoices/{id}"),
            ("GET", "/invoices/{id}/status"),
            ("GET", "/metrics"),
            ("GET", "/supported"),
            ("POST", "/invoices"),
            ("POST", "/invoices/{id}/pay"),
            ("POST", "/settle"),
            ("POST", "/verify"),
        ]
    );
    for (_, path) in &routes {
        for banned in ["reserve", "await", "reservation", "challenge", "head"] {
            assert!(!path.contains(banned), "{path}");
        }
    }
    let a = app();
    for path in [
        "/reserve",
        "/await",
        "/reservation",
        "/reservations",
        "/api/reserve",
        "/settle/await",
        "/v1/await",
        "/",
        "/admin",
        "/challenge",
    ] {
        for method in ["GET", "POST", "PUT", "DELETE"] {
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::empty())
                .unwrap();
            let (status, _, _) = send(&a.app, req, "10.0.0.1").await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        }
    }
    // wrong methods on real routes
    let (status, _, _) = send(&a.app, get("/verify"), "10.0.0.1").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _, _) = send(&a.app, post("/supported", None, vec![]), "10.0.0.1").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(a.fx.ledger.is_empty());
}

#[tokio::test]
async fn supported_and_health_are_public_json() {
    let a = app();
    let (status, headers, body) = send(&a.app, get("/supported"), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let v = json_of(&body);
    assert_eq!(v["kinds"][0]["network"], "kaspa:testnet-10");
    assert_eq!(v["kinds"][0]["extra"]["binding"], "kaspa-exact-v2");
    assert_eq!(v["kinds"][0]["extra"]["profiles"], json!(["standard-native"]));
    let (status, headers, body) = send(&a.app, get("/health"), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(json_of(&body)["status"], "ok");
}

// -------------------------------------------------------------------------------------- authentication

#[tokio::test]
async fn unauthenticated_callers_cannot_create_ledger_state() {
    let a = app();
    let input = a.fx.fund(500_000_000);
    let (req, _) = a.fx.payment(&[input], 100_000_000, ID1, 7);
    let body = body_of(&req);
    for auth in [None, Some("wrong-key"), Some("")] {
        let (status, _, b) = send(&a.app, post("/settle", auth, body.clone()), "10.0.0.1").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{auth:?}");
        assert_eq!(json_of(&b)["extensions"]["kaspa"]["diagnostic"], "unauthorized");
        let (status, _, _) = send(&a.app, post("/verify", auth, body.clone()), "10.0.0.1").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // a malformed Authorization header
    let mut r = post("/settle", None, body.clone());
    r.headers_mut().insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::UNAUTHORIZED);
    assert!(a.fx.ledger.is_empty(), "no ledger state without authentication");
    assert_eq!(a.fx.chain.submit_count(), 0);
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0, "not even verified");
    assert_eq!(a.state.fac.metrics.http_unauthorized.load(Ordering::Relaxed), 7);
}

#[tokio::test]
async fn authenticated_verify_and_settle_roundtrip() {
    let a = app();
    let input = a.fx.fund(500_000_000);
    let (req, tx) = a.fx.payment(&[input], 100_000_000, ID1, 7);
    let body = body_of(&req);
    let (status, headers, b) = send(&a.app, post("/verify", Some(KEY1), body.clone()), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let v = json_of(&b);
    assert_eq!(v["isValid"], true);
    assert_eq!(v["payer"], address_of_key(PAYER_KEY));
    assert!(a.fx.ledger.is_empty());

    let miner = mine_after_submit(&a.fx.chain);
    let (status, headers, b) = send(&a.app, post("/settle", Some(KEY1), body.clone()), "10.0.0.1").await;
    miner.join().unwrap();
    assert_eq!(status, StatusCode::OK);
    let s = json_of(&b);
    assert_eq!(s["success"], true);
    assert_eq!(s["transaction"], hex(&tx.id().as_bytes()));
    assert_eq!(s["extensions"]["kaspa"]["finality"], "accepted");
    // PAYMENT-RESPONSE header carries the same response
    let hdr: SettlementResponse = header_decode(headers.get("payment-response").unwrap().to_str().unwrap()).unwrap();
    assert!(hdr.success);
    assert_eq!(hdr.transaction, hex(&tx.id().as_bytes()));
    // the ledger entry is attributed to the authenticated merchant
    assert_eq!(a.fx.ledger.get(&hex(&tx.id().as_bytes())).unwrap().merchant, "shop");
    // retry: idempotent, cached
    let (status, _, b2) = send(&a.app, post("/settle", Some(KEY1), body), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    let mut again = json_of(&b2);
    assert_eq!(again["extensions"]["kob"]["replayed"], true, "a repeat answer is marked");
    again["extensions"]["kob"].as_object_mut().unwrap().remove("replayed");
    assert_eq!(again, s);
    assert_eq!(a.fx.chain.submit_count(), 1);
}

#[tokio::test]
async fn a_failed_settlement_is_http_200_with_success_false_and_a_diagnostic() {
    let a = app();
    let input = a.fx.fund(500_000_000);
    let (mut req, _) = a.fx.payment(&[input], 100_000_000, ID1, 7);
    req.request_hash = None;
    let (status, _, b) = send(&a.app, post("/settle", Some(KEY1), body_of(&req)), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    let s = json_of(&b);
    assert_eq!(s["success"], false);
    assert_eq!(s["errorReason"], "invalid_payload");
    assert_eq!(s["transaction"], "");
    assert!(s["extensions"]["kaspa"]["diagnostic"].is_string());
    let (status, _, b) = send(&a.app, post("/verify", Some(KEY1), body_of(&req)), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&b)["isValid"], false);
    assert_eq!(json_of(&b)["invalidReason"], "invalid_payload");
}

#[tokio::test]
async fn the_merchant_key_only_permits_its_own_pay_to_and_assets() {
    let a = app();
    let input = a.fx.fund(500_000_000);
    let (req, _) = a.fx.payment(&[input], 100_000_000, ID1, 7); // pays the merchant fixture key
                                                                // merchant "other" may only request payments to the payer fixture address
    let (status, _, b) = send(&a.app, post("/settle", Some(KEY2), body_of(&req)), "10.0.0.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&b)["error"], "forbidden");
    let (status, _, _) = send(&a.app, post("/verify", Some(KEY2), body_of(&req)), "10.0.0.1").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // an asset the key may not request
    let mut token = req.clone();
    token.payment_requirements.asset = "aa".repeat(32);
    token.payment_payload.accepted.asset = "aa".repeat(32);
    assert_eq!(send(&a.app, post("/settle", Some(KEY1), body_of(&token)), "10.0.0.1").await.0, StatusCode::FORBIDDEN);
    // accepted differing from the requirements is not a way around the check
    let mut sneaky = req.clone();
    sneaky.payment_payload.accepted.pay_to = address_of_key(PAYER_KEY);
    assert_eq!(send(&a.app, post("/settle", Some(KEY1), body_of(&sneaky)), "10.0.0.1").await.0, StatusCode::FORBIDDEN);
    assert!(a.fx.ledger.is_empty());
    assert_eq!(a.fx.chain.submit_count(), 0);
    assert_eq!(a.state.fac.metrics.http_forbidden.load(Ordering::Relaxed), 4);
}

// --------------------------------------------------------------------------------------------- admission

#[tokio::test]
async fn content_type_must_be_json() {
    let a = app();
    let mut r = post("/verify", Some(KEY1), b"{}".to_vec());
    r.headers_mut().insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let r = Request::builder()
        .method("POST")
        .uri("/verify")
        .header(header::AUTHORIZATION, format!("Bearer {KEY1}"))
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // parameters are fine
    let mut r = post("/verify", Some(KEY1), b"{}".to_vec());
    r.headers_mut().insert(header::CONTENT_TYPE, "application/json; charset=utf-8".parse().unwrap());
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::BAD_REQUEST); // parsed, but not a facilitator request
}

#[tokio::test]
async fn declared_and_streamed_body_limits() {
    let a = app_with(r#","maxBodyBytes":2048"#);
    // declared: rejected before a single byte is read (the stream would stall forever)
    let stalled = Body::from_stream(stream::pending::<Result<Bytes, std::io::Error>>());
    let mut r = Request::builder()
        .method("POST")
        .uri("/settle")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {KEY1}"))
        .body(stalled)
        .unwrap();
    r.headers_mut().insert(header::CONTENT_LENGTH, "5000".parse().unwrap());
    let t = Instant::now();
    let (status, _, b) = send(&a.app, r, "10.0.0.1").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json_of(&b)["error"], "payload_too_large");
    assert!(t.elapsed() < Duration::from_secs(2));
    // streamed without a declared length: cut off at the limit
    let chunks = stream::iter((0..100).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b' '; 100]))));
    let r = Request::builder()
        .method("POST")
        .uri("/settle")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {KEY1}"))
        .body(Body::from_stream(chunks))
        .unwrap();
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::PAYLOAD_TOO_LARGE);
    // at the limit is fine
    let ok = post("/verify", Some(KEY1), vec![b' '; 2048]);
    assert_eq!(send(&a.app, ok, "10.0.0.1").await.0, StatusCode::BAD_REQUEST); // not JSON, but not too large
    assert_eq!(a.state.fac.metrics.http_too_large.load(Ordering::Relaxed), 2);
    assert!(a.fx.ledger.is_empty());
}

#[tokio::test]
async fn a_stalled_body_fails_at_the_overall_deadline_before_any_verification() {
    let a = app_with(r#","bodyDeadlineMs":300"#);
    let input = a.fx.fund(500_000_000);
    let (req, _) = a.fx.payment(&[input], 100_000_000, ID1, 7);
    let full = body_of(&req);
    let first = Bytes::from(full[..40].to_vec());
    let body = stream::once(async move { Ok::<_, std::io::Error>(first) }).chain(stream::pending());
    let r = Request::builder()
        .method("POST")
        .uri("/settle")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {KEY1}"))
        .body(Body::from_stream(body))
        .unwrap();
    let t = Instant::now();
    let (status, _, b) = send(&a.app, r, "10.0.0.1").await;
    let took = t.elapsed();
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(json_of(&b)["error"], "request_timeout");
    assert!(took >= Duration::from_millis(290) && took < Duration::from_secs(3), "{took:?}");
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0, "no verification");
    assert_eq!(a.fx.chain.submit_count(), 0, "no chain access");
    assert!(a.fx.ledger.is_empty());
}

#[tokio::test]
async fn a_trickling_body_cannot_reset_the_deadline() {
    let a = app_with(r#","bodyDeadlineMs":400"#);
    // one byte every 60 ms, forever: a per-chunk timeout would never fire
    let trickle = stream::unfold(0u32, |n| async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        Some((Ok::<_, std::io::Error>(Bytes::from_static(b" ")), n + 1))
    });
    let r = Request::builder()
        .method("POST")
        .uri("/settle")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {KEY1}"))
        .body(Body::from_stream(trickle))
        .unwrap();
    let t = Instant::now();
    let (status, _, _) = send(&a.app, r, "10.0.0.1").await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn json_depth_scanner() {
    assert!(json_depth_ok(b"{}", 1));
    assert!(!json_depth_ok(b"[[]]", 1));
    assert!(json_depth_ok(b"[[]]", 2));
    // brackets inside strings (with escapes) do not count
    assert!(json_depth_ok(br#"{"a":"[[[[[[[[","b":"\"[[[[["}"#, 1));
    let deep = format!("{}{}", "[".repeat(MAX_JSON_DEPTH + 1), "]".repeat(MAX_JSON_DEPTH + 1));
    assert!(!json_depth_ok(deep.as_bytes(), MAX_JSON_DEPTH));
    let ok = format!("{}{}", "[".repeat(MAX_JSON_DEPTH), "]".repeat(MAX_JSON_DEPTH));
    assert!(json_depth_ok(ok.as_bytes(), MAX_JSON_DEPTH));
}

#[tokio::test]
async fn deeply_nested_and_malformed_json_are_400_before_verification() {
    let a = app();
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    for body in [deep.into_bytes(), b"{not json".to_vec(), b"[]".to_vec(), b"{\"x402Version\":2}".to_vec(), vec![]] {
        let (status, _, b) = send(&a.app, post("/settle", Some(KEY1), body), "10.0.0.1").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json_of(&b)["error"], "invalid_payload");
    }
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0);
    assert!(a.fx.ledger.is_empty());
}

// -------------------------------------------------------------------------------------------- rate limits

#[tokio::test]
async fn per_ip_rate_limit_answers_429_with_retry_after() {
    let a = app_with(r#","rateLimit":{"perIp":{"burst":3,"perSecond":0.5},"perMerchant":{"burst":100,"perSecond":100}}"#);
    for _ in 0..3 {
        assert_eq!(send(&a.app, get("/supported"), "10.0.0.7").await.0, StatusCode::OK);
    }
    let (status, headers, b) = send(&a.app, get("/supported"), "10.0.0.7").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers[header::RETRY_AFTER], "2");
    assert_eq!(json_of(&b)["error"], "rate_limited");
    // anonymous POSTs share the per-IP bucket and are limited before authentication or any body read
    let (status, _, _) = send(&a.app, post("/settle", None, vec![]), "10.0.0.7").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    // a request with a valid merchant key is limited by its merchant's bucket instead: the exhausted address does not
    // starve it
    let (status, _, _) = send(&a.app, post("/settle", Some(KEY1), b"{}".to_vec()), "10.0.0.7").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // another address is unaffected
    assert_eq!(send(&a.app, get("/supported"), "10.0.0.8").await.0, StatusCode::OK);
    assert!(a.state.fac.metrics.http_rate_limited.load(Ordering::Relaxed) >= 2);
}

#[tokio::test]
async fn per_merchant_rate_limit_is_independent_of_the_ip() {
    let a = app_with(r#","rateLimit":{"perIp":{"burst":1000,"perSecond":1000},"perMerchant":{"burst":2,"perSecond":0.1}}"#);
    let junk = b"{}".to_vec();
    for i in 0..2 {
        // parsed, then rejected as not a facilitator request: the merchant bucket was still charged
        let ip = format!("10.0.1.{i}");
        assert_eq!(send(&a.app, post("/verify", Some(KEY1), junk.clone()), &ip).await.0, StatusCode::BAD_REQUEST);
    }
    let (status, headers, _) = send(&a.app, post("/verify", Some(KEY1), junk.clone()), "10.0.1.99").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "same merchant from a new address");
    assert!(headers.contains_key(header::RETRY_AFTER));
    // the other merchant has its own bucket
    assert_eq!(send(&a.app, post("/verify", Some(KEY2), junk), "10.0.1.99").await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_merchant_override_beats_the_default() {
    let fx = Fixture::new();
    let key_hash = hex(&api_key_hash(KEY1));
    let cfg = format!(
        r#"{{"merchants":[{{"id":"shop","apiKeySha256":"{key_hash}","allowedPayTo":["{}"],"allowedAssets":["KAS"],"rateLimit":{{"burst":1,"perSecond":0.01}}}}]}}"#,
        address_of_key(MERCHANT_KEY)
    );
    let built = X402Config::from_json(&cfg).unwrap().build().unwrap();
    let app = router(Arc::new(AppState::new(fx.fac.clone(), &built)));
    assert_eq!(send(&app, post("/verify", Some(KEY1), b"{}".to_vec()), "10.0.0.1").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(send(&app, post("/verify", Some(KEY1), b"{}".to_vec()), "10.0.0.1").await.0, StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------------------- kill switch

#[tokio::test]
async fn kill_switch_answers_503_and_empties_supported() {
    let a = app();
    let input = a.fx.fund(500_000_000);
    let (req, _) = a.fx.payment(&[input], 100_000_000, ID1, 7);
    a.fx.fac.set_kill(true);
    for path in ["/verify", "/settle"] {
        let (status, headers, b) = send(&a.app, post(path, Some(KEY1), body_of(&req)), "10.0.0.1").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(json_of(&b)["error"], "facilitator_disabled");
        assert!(headers.contains_key(header::RETRY_AFTER));
    }
    let (status, _, b) = send(&a.app, get("/supported"), "10.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json_of(&b)["kinds"].as_array().unwrap().is_empty());
    assert_eq!(json_of(&send(&a.app, get("/health"), "10.0.0.1").await.2)["enabled"], false);
    assert!(a.fx.ledger.is_empty());
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0);
    a.fx.fac.set_kill(false);
    assert_eq!(send(&a.app, post("/verify", Some(KEY1), body_of(&req)), "10.0.0.1").await.0, StatusCode::OK);
}

#[tokio::test]
async fn kill_switch_file_is_honoured_by_the_service() {
    use super::facilitator::{Facilitator, FacilitatorConfig};
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("x402.kill");
    let fx = Fixture::new();
    let fac = Arc::new(
        Facilitator::new(
            fx.fac.policy.clone(),
            fx.chain.clone(),
            fx.clock.clone(),
            fx.ledger.clone(),
            FacilitatorConfig { kill_switch_file: Some(flag.clone()), ..Default::default() },
        )
        .with_verifier(fx.verifier.clone()),
    );
    let built = X402Config::from_json(&cfg_json("")).unwrap().build().unwrap();
    let app = router(Arc::new(AppState::new(fac, &built)));
    assert_eq!(send(&app, post("/verify", Some(KEY1), b"{}".to_vec()), "10.0.0.1").await.0, StatusCode::BAD_REQUEST);
    std::fs::write(&flag, b"stop").unwrap();
    assert_eq!(send(&app, post("/verify", Some(KEY1), b"{}".to_vec()), "10.0.0.1").await.0, StatusCode::SERVICE_UNAVAILABLE);
    std::fs::remove_file(&flag).unwrap();
    assert_eq!(send(&app, post("/verify", Some(KEY1), b"{}".to_vec()), "10.0.0.1").await.0, StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------------------------------------------- metrics

#[tokio::test]
async fn metrics_are_for_loopback_or_the_admin_key() {
    let a = app_with(&format!(r#","adminKeySha256":"{}""#, hex(&api_key_hash("admin-secret"))));
    // loopback
    let (status, headers, b) = send(&a.app, get("/metrics"), "127.0.0.1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/plain"));
    assert!(String::from_utf8_lossy(&b).contains("kob_x402_settle_requests 0"));
    // remote without credentials, or with a merchant key
    assert_eq!(send(&a.app, get("/metrics"), "10.0.0.1").await.0, StatusCode::UNAUTHORIZED);
    let mut r = get("/metrics");
    r.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {KEY1}").parse().unwrap());
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::UNAUTHORIZED);
    // remote with the admin key
    let mut r = get("/metrics");
    r.headers_mut().insert(header::AUTHORIZATION, "Bearer admin-secret".parse().unwrap());
    assert_eq!(send(&a.app, r, "10.0.0.1").await.0, StatusCode::OK);
    // no peer information at all counts as remote
    let resp = a.app.clone().oneshot(get("/metrics")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ------------------------------------------------------------------------------------------ open auth

#[tokio::test]
async fn open_mode_on_loopback_needs_no_key() {
    let fx = Fixture::new();
    let built = X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"listen":"127.0.0.1:0"}"#).unwrap().build().unwrap();
    let app = router(Arc::new(AppState::new(fx.fac.clone(), &built)));
    let input = fx.fund(500_000_000);
    let (req, tx) = fx.payment(&[input], 100_000_000, ID1, 7);
    let miner = mine_after_submit(&fx.chain);
    let (status, _, b) = send(&app, post("/settle", None, body_of(&req)), "127.0.0.1").await;
    miner.join().unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&b)["success"], true);
    assert_eq!(fx.ledger.get(&hex(&tx.id().as_bytes())).unwrap().merchant, "open");
}

#[tokio::test]
async fn open_mode_refuses_requests_through_a_proxy_or_from_another_host() {
    let fx = Fixture::new();
    let built = X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"listen":"127.0.0.1:0"}"#).unwrap().build().unwrap();
    let app = router(Arc::new(AppState::new(fx.fac.clone(), &built)));
    let input = fx.fund(500_000_000);
    let (req, _) = fx.payment(&[input], 100_000_000, ID1, 7);
    // a reverse proxy on the same host: the peer is loopback, the request carries a forwarding header
    for h in ["x-forwarded-for", "forwarded", "x-real-ip", "via"] {
        let mut r = post("/settle", None, body_of(&req));
        r.headers_mut().insert(h, "203.0.113.9".parse().unwrap());
        let (status, _, _) = send(&app, r, "127.0.0.1").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{h}");
    }
    let (status, _, _) = send(&app, post("/settle", None, body_of(&req)), "192.0.2.7").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(fx.chain.submit_count(), 0, "nothing was settled");
}

// ----------------------------------------------------------------------------------- real TCP server

async fn spawn_server(a: &App, header_timeout: Duration) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(a.state.clone());
    let h = tokio::spawn(async move {
        let limits = crate::api::conn::ConnLimits {
            header_timeout,
            max_connections: 64,
            ..crate::x402::config::X402Config::default().conn_limits()
        };
        let _ = serve(listener, app, limits, std::future::pending()).await;
    });
    (addr, h)
}

async fn read_response<R: tokio::io::AsyncRead + Unpin>(stream: &mut R, limit: Duration) -> String {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(limit, async {
        let mut tmp = [0u8; 4096];
        loop {
            match stream.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() > 20 {
                        // headers complete; a short JSON body follows in the same segment in practice
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(50), stream.read(&mut tmp)).await {
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        break;
                    }
                }
            }
        }
    })
    .await;
    String::from_utf8_lossy(&buf).to_string()
}

#[tokio::test]
async fn slow_loris_body_over_tcp_fails_before_verification_or_chain_access() {
    use tokio::io::AsyncWriteExt;
    let a = app_with(r#","bodyDeadlineMs":400"#);
    let (addr, server) = spawn_server(&a, Duration::from_secs(5)).await;
    let s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut r, mut w) = s.into_split();
    let head = format!(
        "POST /settle HTTP/1.1
Host: x
Content-Type: application/json
Authorization: Bearer {KEY1}
Content-Length: 4000

{{\"x402Version\":"
    );
    w.write_all(head.as_bytes()).await.unwrap();
    let t = Instant::now();
    // the client keeps trickling one byte every 100 ms: each chunk would reset a per-read timeout
    let trickle = tokio::spawn(async move {
        for _ in 0..50 {
            if w.write_all(b" ").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let resp = read_response(&mut r, Duration::from_secs(5)).await;
    assert!(resp.starts_with("HTTP/1.1 408"), "{resp}");
    assert!(t.elapsed() < Duration::from_millis(2500), "{:?}", t.elapsed());
    assert_eq!(a.state.fac.metrics.http_body_timeouts.load(Ordering::Relaxed), 1);
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0);
    assert_eq!(a.fx.chain.submit_count(), 0);
    assert!(a.fx.ledger.is_empty());
    trickle.abort();
    server.abort();
}

#[tokio::test]
async fn a_stalled_body_gets_a_408_response_over_tcp() {
    use tokio::io::AsyncWriteExt;
    let a = app_with(r#","bodyDeadlineMs":300"#);
    let (addr, server) = spawn_server(&a, Duration::from_secs(5)).await;
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!("POST /verify HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nAuthorization: Bearer {KEY1}\r\nContent-Length: 4000\r\n\r\n{{\"x402");
    s.write_all(head.as_bytes()).await.unwrap();
    let t = Instant::now();
    let resp = read_response(&mut s, Duration::from_secs(5)).await;
    assert!(resp.starts_with("HTTP/1.1 408"), "{resp}");
    assert!(resp.to_ascii_lowercase().contains("connection: close"));
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(a.fx.verifier.calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn a_slow_header_client_is_dropped() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let a = app();
    let (addr, server) = spawn_server(&a, Duration::from_millis(300)).await;
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(b"POST /settle HTTP/1.1\r\nHost: x\r\n").await.unwrap(); // never finishes the headers
    let t = Instant::now();
    let mut buf = [0u8; 256];
    let r = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await.expect("the server must close the connection");
    // closed (EOF, reset or an error status), not held open
    assert!(matches!(r, Ok(0) | Err(_)) || String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 408"), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(a.fx.ledger.is_empty());
    server.abort();
}

#[tokio::test]
async fn a_full_settlement_over_tcp() {
    use tokio::io::AsyncWriteExt;
    let a = app();
    let (addr, server) = spawn_server(&a, Duration::from_secs(5)).await;
    let input = a.fx.fund(500_000_000);
    let (req, tx) = a.fx.payment(&[input], 100_000_000, ID1, 7);
    let body = body_of(&req);
    let miner = mine_after_submit(&a.fx.chain);
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /settle HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nAuthorization: Bearer {KEY1}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(&body).await.unwrap();
    let mut out = Vec::new();
    {
        use tokio::io::AsyncReadExt;
        tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut out)).await.unwrap().unwrap();
    }
    miner.join().unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains(&hex(&tx.id().as_bytes())));
    assert!(text.to_ascii_lowercase().contains("payment-response:"));
    server.abort();
}

// -------------------------------------------------------------------------------------------- invoices

#[tokio::test]
async fn invoices_are_registered_by_a_merchant_served_publicly_and_paid_once() {
    let a = app_invoices();
    let reqs = kas_requirements(100_000_000, kob_x402::wire::Finality::Accepted, 60);
    let inv =
        kob_x402::invoice::Invoice::new(kob_x402::wire::Network::Testnet10, "order-7", START_MS + 600_000, None, vec![reqs.clone()]);
    let body = serde_json::to_vec(&inv).unwrap();
    // no key: 401; a key whose merchant may not request that payTo: 403
    let (st, _, _) = send(&a.app, post("/invoices", None, body.clone()), "10.0.0.1").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _, b) = send(&a.app, post("/invoices", Some(KEY2), body.clone()), "10.0.0.1").await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&b));
    let (st, _, b) = send(&a.app, post("/invoices", Some(KEY1), body.clone()), "10.0.0.1").await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    let id = json_of(&b)["id"].as_str().unwrap().to_string();
    assert_eq!(json_of(&b)["url"], format!("/invoices/{id}"));
    // idempotent
    let (_, _, b2) = send(&a.app, post("/invoices", Some(KEY1), body), "10.0.0.1").await;
    assert_eq!(json_of(&b2)["created"], false);
    // public: the invoice (it hashes to its id) and its status
    let (st, _, b) = send(&a.app, get(&format!("/invoices/{id}")), "10.0.0.2").await;
    assert_eq!(st, StatusCode::OK);
    let fetched: kob_x402::invoice::Invoice = serde_json::from_slice(&b).unwrap();
    kob_x402::invoice::check_id(&fetched, &id).unwrap();
    let (_, _, b) = send(&a.app, get(&format!("/invoices/{id}/status")), "10.0.0.2").await;
    assert_eq!(json_of(&b)["status"], "unpaid");
    let (st, _, _) = send(&a.app, get(&format!("/invoices/{}", "00".repeat(32))), "10.0.0.2").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = send(&a.app, get("/invoices/not-hex/status"), "10.0.0.2").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // an anonymous payer pays it (request hash = the invoice id)
    let input = a.fx.fund(300_000_000);
    let (tx, entries) = build_kas_payment(
        &a.fx.chain,
        PAYER_KEY,
        &[input],
        &kob_x402::testkit::p2pk_spk(&kob_x402::testkit::pubkey(MERCHANT_KEY)),
        100_000_000,
    );
    let freq = request_for(&tx, &entries, &reqs, kob_x402::wire::parse_hash32(&id).unwrap(), ID1);
    let pay_body = serde_json::to_vec(&freq.payment_payload).unwrap();
    let miner = mine_after_submit(&a.fx.chain);
    let (st, h, b) = send(&a.app, post(&format!("/invoices/{id}/pay"), None, pay_body.clone()), "10.0.0.3").await;
    miner.join().unwrap();
    assert_eq!(st, StatusCode::OK);
    let r: SettlementResponse = serde_json::from_slice(&b).unwrap();
    assert!(r.success, "{r:?}");
    assert!(h.get("payment-response").is_some());
    let (_, _, b) = send(&a.app, get(&format!("/invoices/{id}/status")), "10.0.0.2").await;
    let s = json_of(&b);
    assert_eq!(s["status"], "paid");
    assert_eq!(s["payment"]["transaction"], hex(&tx.id().as_bytes()));
    assert_eq!(a.fx.ledger.get(&hex(&tx.id().as_bytes())).unwrap().invoice.as_deref(), Some(id.as_str()));
}

#[tokio::test]
async fn invoices_are_off_unless_configured() {
    let a = app();
    let inv = kob_x402::invoice::Invoice::new(
        kob_x402::wire::Network::Testnet10,
        "x",
        START_MS + 600_000,
        None,
        vec![kas_requirements(100_000_000, kob_x402::wire::Finality::Accepted, 60)],
    );
    let (st, _, _) = send(&a.app, post("/invoices", Some(KEY1), serde_json::to_vec(&inv).unwrap()), "10.0.0.1").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = send(&a.app, get(&format!("/invoices/{}", "00".repeat(32))), "10.0.0.1").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(a.state.fac.supported()["kinds"][0]["extra"].get("invoices").is_none());
}
