//! Regression: the x402 facilitator behind a local reverse proxy.
//!
//! Every request used to come from 127.0.0.1: `/metrics` ("admin key or loopback peer") was readable by anybody who could reach the
//! proxy, and the per-IP limiter was one shared bucket that anonymous junk (counted before authentication) exhausted, so every
//! merchant got `429`. Now: `trustedProxies` + `clientIpHeader` attribute requests to the forwarded client; `/metrics` never
//! trusts the loopback peer when proxies are configured or when the request carries a forwarding header; a request with a valid
//! merchant key is limited by the merchant's bucket, not the shared per-IP one; settle and verify have concurrency caps
//! (per merchant and global).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use kaspa_addresses::{Address, Version};
use kob_executor::x402::config::{api_key_hash, X402Config};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig};
use kob_executor::x402::http::{router, AppState};
use kob_executor::x402::ledger::Ledger;
use kob_x402::chain::FixedClock;
use kob_x402::testkit::{pubkey, MockChain};
use kob_x402::wire::{hex, Network};
use tower::ServiceExt;

async fn send(app: &axum::Router, mut req: Request<Body>, peer: &str) -> StatusCode {
    let addr: SocketAddr = format!("{peer}:40000").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    app.clone().oneshot(req).await.unwrap().status()
}

fn app(extra: &str) -> axum::Router {
    let pay_to = Address::new(Network::Testnet10.prefix(), Version::PubKey, &pubkey(8)).to_string();
    let cfg = format!(
        r#"{{{extra}"merchants":[{{"id":"shop","apiKeySha256":"{}","allowedPayTo":["{pay_to}"],"allowedAssets":["KAS"]}}]}}"#,
        hex(&api_key_hash("merchant-secret-key"))
    );
    let built = X402Config::from_json(&cfg).unwrap().build().unwrap();
    let fac = Arc::new(Facilitator::new(
        built.policy.clone(),
        Arc::new(MockChain::new()),
        Arc::new(FixedClock::new(1_800_000_000_000)),
        Arc::new(Ledger::in_memory()),
        FacilitatorConfig { settle_wait: Duration::from_millis(50), ..Default::default() },
    ));
    router(Arc::new(AppState::new(fac, &built)))
}

fn metrics(forwarded_for: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri("/metrics");
    if let Some(f) = forwarded_for {
        b = b.header("x-forwarded-for", f);
    }
    b.body(Body::empty()).unwrap()
}

fn junk(forwarded_for: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("POST").uri("/verify").header(header::CONTENT_TYPE, "application/json");
    if let Some(f) = forwarded_for {
        b = b.header("x-forwarded-for", f);
    }
    b.body(Body::from("{}")).unwrap()
}

fn legit() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/verify")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer merchant-secret-key")
        .body(Body::from("{}"))
        .unwrap()
}

#[tokio::test]
async fn metrics_do_not_trust_the_loopback_peer_behind_a_proxy() {
    // declared proxy: the loopback peer is the proxy, not the operator
    let a = app(r#""trustedProxies":["127.0.0.1"],"#);
    assert_eq!(send(&a, metrics(None), "127.0.0.1").await, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&a, metrics(Some("203.0.113.9")), "127.0.0.1").await, StatusCode::UNAUTHORIZED);
    // undeclared proxy, but the request carries a forwarding header: not a local operator either
    let a = app("");
    assert_eq!(send(&a, metrics(Some("203.0.113.9")), "127.0.0.1").await, StatusCode::UNAUTHORIZED);
    // the operator on the box (no proxy in front, no forwarding header) still reads it
    assert_eq!(send(&a, metrics(None), "127.0.0.1").await, StatusCode::OK);
    // and the operator can switch the loopback shortcut off
    let a = app(r#""metricsLoopback":false,"#);
    assert_eq!(send(&a, metrics(None), "127.0.0.1").await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_junk_from_a_shared_address_cannot_starve_a_merchant() {
    let a = app("");
    let proxy = "127.0.0.1";
    assert_eq!(send(&a, legit(), proxy).await, StatusCode::BAD_REQUEST, "400: the body is not a facilitator request");
    let mut limited = 0;
    for _ in 0..60 {
        if send(&a, junk(None), proxy).await == StatusCode::TOO_MANY_REQUESTS {
            limited += 1;
        }
    }
    assert!(limited > 0, "the anonymous bucket does fill");
    for _ in 0..10 {
        assert_eq!(send(&a, legit(), proxy).await, StatusCode::BAD_REQUEST, "the merchant has its own bucket");
    }
}

#[tokio::test]
async fn behind_a_trusted_proxy_each_client_has_its_own_bucket() {
    let a = app(r#""trustedProxies":["127.0.0.1"],"#);
    let proxy = "127.0.0.1";
    for _ in 0..60 {
        let _ = send(&a, junk(Some("203.0.113.1")), proxy).await;
    }
    assert_eq!(send(&a, junk(Some("203.0.113.1")), proxy).await, StatusCode::TOO_MANY_REQUESTS, "client 1 is limited");
    assert_eq!(send(&a, junk(Some("203.0.113.2")), proxy).await, StatusCode::UNAUTHORIZED, "client 2 is not");
    // a spoofed header from a peer that is NOT a trusted proxy is ignored
    let mut statuses = vec![];
    for i in 0..60 {
        statuses.push(send(&a, junk(Some(&format!("198.51.100.{i}"))), "192.0.2.7").await);
    }
    assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS), "rotating the header does not rotate the bucket");
}

/// `run` refuses an open facilitator next to a read API that serves the outside (a public address, or a reverse proxy declared in
/// `trusted_proxies`): a relay on that host could reach the facilitator from the loopback address without any header.
#[test]
fn open_auth_is_refused_next_to_a_public_or_proxied_read_api() {
    use kob_executor::config::ApiConfig;
    use kob_executor::executor::open_auth_beside_api;
    use kob_executor::x402::config::AuthMode;
    let local = ApiConfig { enabled: true, listen: "127.0.0.1:8090".parse().unwrap(), ..ApiConfig::default() };
    assert!(open_auth_beside_api(AuthMode::Open, &local).is_ok());
    let public = ApiConfig { listen: "0.0.0.0:8090".parse().unwrap(), ..local.clone() };
    assert!(open_auth_beside_api(AuthMode::Open, &public).is_err());
    let proxied = ApiConfig { trusted_proxies: vec!["127.0.0.1".into()], ..local.clone() };
    assert!(open_auth_beside_api(AuthMode::Open, &proxied).unwrap_err().to_string().contains("required"));
    // keyed merchants are fine anywhere; a disabled API says nothing about the host
    assert!(open_auth_beside_api(AuthMode::Required, &public).is_ok());
    assert!(open_auth_beside_api(AuthMode::Open, &ApiConfig { enabled: false, ..public }).is_ok());
}

#[test]
fn the_concurrency_settings_are_validated_and_defaulted() {
    let d = X402Config::default();
    assert_eq!((d.max_settles_per_merchant, d.max_concurrent_verifies), (8, 64));
    assert!(d.max_settles_per_merchant < d.max_concurrent_settles, "one merchant cannot hold the whole pool");
    let mut c = X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"maxSettlesPerMerchant":0}"#).unwrap();
    assert!(c.build().is_err());
    c.max_settles_per_merchant = 1;
    c.trusted_proxies = vec!["not-an-ip".into()];
    assert!(c.build().is_err());
    // a volatile ledger is refused on mainnet
    let m = X402Config::from_json(
        r#"{"network":"kaspa:mainnet","allowMainnet":true,"ledger":":memory:","auth":"open","openAuthNoProxy":true}"#,
    )
    .unwrap();
    assert!(m.build().unwrap_err().to_string().contains("volatile"));
}
