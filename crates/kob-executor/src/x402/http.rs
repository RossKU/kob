//! The facilitator HTTP service (axum 0.7).
//!
//! Routes (and nothing else; there is deliberately no reservation or await endpoint):
//!
//! | Route | Access |
//! |---|---|
//! | `GET /supported` | public (empty while the kill switch is on) |
//! | `POST /verify` | merchant API key |
//! | `POST /settle` | merchant API key |
//! | `GET /health` | public, no chain access |
//! | `GET /metrics` | admin key or loopback peer |
//! | `POST /invoices` | merchant API key (invoices enabled) |
//! | `GET /invoices/{id}` | public: the invoice (its canonical JSON hashes to `id`) |
//! | `GET /invoices/{id}/status` | public: invoice-level status (a read, never an await) |
//! | `POST /invoices/{id}/pay` | public: settles one payment of the invoice (per-IP rate limit, settle slots) |
//!
//! Admission of a POST, in this order and before any verification or chain access:
//! kill switch (503) -> per-IP rate limit (429 + Retry-After) -> content type (415) -> Bearer API key
//! (401) -> per-merchant rate limit (429) -> declared body length (413) -> streamed body under **one**
//! overall deadline (408 / 413) -> JSON structural depth and parse (400) -> the merchant's allowed
//! `payTo` / asset (403). Results of verification and settlement are HTTP 200 with `isValid: false` /
//! `success: false` in the body. Unauthenticated callers reach the ledger only through `POST /invoices/{id}/pay`, and
//! only with a payment that verifies completely against a registered, unexpired, unpaid invoice.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, MethodRouter};
use axum::Router;
use http_body_util::BodyExt;
use kob_x402::error::Diag;
use kob_x402::wire::{header_encode, FacilitatorRequest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use super::config::{AuthMode, Built, Merchant};
use super::facilitator::{Facilitator, Metrics};
use super::ratelimit::{Limiter, Rate};
use crate::api::client_ip::{rate_key, ClientIpResolver, TrustedProxies};

/// Deepest JSON nesting a request body may have.
pub const MAX_JSON_DEPTH: usize = 32;

/// Shared state of the service.
pub struct AppState {
    pub fac: Arc<Facilitator>,
    pub auth: AuthMode,
    pub merchants: Vec<Merchant>,
    pub admin_key: Option<[u8; 32]>,
    pub max_body: usize,
    pub body_deadline: Duration,
    ip_limiter: Limiter<IpAddr>,
    merchant_limiter: Limiter<String>,
    merchant_default: Rate,
    settle_slots: Arc<Semaphore>,
    /// Per-merchant settle slots, created on first use.
    merchant_slots: std::sync::Mutex<std::collections::HashMap<String, Arc<Semaphore>>>,
    per_merchant_settles: usize,
    verify_slots: Arc<Semaphore>,
    resolver: ClientIpResolver,
    /// `/metrics` may be read by a loopback peer without the admin key.
    metrics_loopback: bool,
}

impl AppState {
    pub fn new(fac: Arc<Facilitator>, built: &Built) -> AppState {
        let c = &built.cfg;
        AppState {
            fac,
            auth: c.auth,
            merchants: built.merchants.clone(),
            admin_key: built.admin_key_hash,
            max_body: c.max_body_bytes,
            body_deadline: c.body_deadline(),
            ip_limiter: Limiter::new(c.rate_limit.per_ip.rate(), 100_000),
            merchant_limiter: Limiter::new(c.rate_limit.per_merchant.rate(), 10_000),
            merchant_default: c.rate_limit.per_merchant.rate(),
            settle_slots: Arc::new(Semaphore::new(c.max_concurrent_settles)),
            merchant_slots: std::sync::Mutex::new(std::collections::HashMap::new()),
            per_merchant_settles: c.max_settles_per_merchant.min(c.max_concurrent_settles).max(1),
            verify_slots: Arc::new(Semaphore::new(c.max_concurrent_verifies)),
            resolver: ClientIpResolver::new(TrustedProxies::parse(&c.trusted_proxies).unwrap_or_default(), &c.client_ip_header),
            metrics_loopback: c.metrics_loopback && c.trusted_proxies.is_empty(),
        }
    }

    fn metrics(&self) -> &Metrics {
        &self.fac.metrics
    }
}

/// One route of the service.
struct Route {
    path: &'static str,
    method: &'static str,
    handler: MethodRouter<Arc<AppState>>,
}

fn routes() -> Vec<Route> {
    vec![
        Route { path: "/supported", method: "GET", handler: get(supported_h) },
        Route { path: "/verify", method: "POST", handler: axum::routing::post(verify_h) },
        Route { path: "/settle", method: "POST", handler: axum::routing::post(settle_h) },
        Route { path: "/health", method: "GET", handler: get(health_h) },
        Route { path: "/metrics", method: "GET", handler: get(metrics_h) },
        Route { path: "/invoices", method: "POST", handler: axum::routing::post(invoice_create_h) },
        Route { path: "/invoices/{id}", method: "GET", handler: get(invoice_get_h) },
        Route { path: "/invoices/{id}/status", method: "GET", handler: get(invoice_status_h) },
        Route { path: "/invoices/{id}/pay", method: "POST", handler: axum::routing::post(invoice_pay_h) },
    ]
}

/// `(method, path)` of every route the service exposes (the router is built from the same table).
pub fn route_list() -> Vec<(&'static str, &'static str)> {
    routes().iter().map(|r| (r.method, r.path)).collect()
}

/// The router.
pub fn router(state: Arc<AppState>) -> Router {
    let mut r = Router::new();
    for route in routes() {
        r = r.route(route.path, route.handler);
    }
    r.with_state(state)
}

// -------------------------------------------------------------------------------------- responses

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

fn error_response(status: StatusCode, code: &str, message: &str, diag: Diag, retry_after: Option<u64>) -> Response {
    let mut r = json_response(
        status,
        &json!({
            "error": code,
            "message": message,
            "extensions": { "kaspa": { "diagnostic": diag.as_str(), "retryable": retry_after.is_some() || status == StatusCode::SERVICE_UNAVAILABLE } },
        }),
    );
    if let Some(s) = retry_after {
        if let Ok(v) = HeaderValue::from_str(&s.to_string()) {
            r.headers_mut().insert(header::RETRY_AFTER, v);
        }
    }
    if status == StatusCode::REQUEST_TIMEOUT || status == StatusCode::PAYLOAD_TOO_LARGE {
        r.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    r
}

fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())
}

/// The client a request is attributed to: the forwarded-client header when the peer is a trusted proxy, else the peer.
fn client_ip(st: &AppState, req: &Request) -> IpAddr {
    let peer = peer_ip(req).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    rate_key(st.resolver.resolve(peer, req.headers()), 64)
}

/// Headers a reverse proxy typically adds: a request that carries one did not come from a local operator.
const FORWARDING_HEADERS: [&str; 8] = [
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
    "forwarded",
    "via",
    "cf-connecting-ip",
    "true-client-ip",
];

fn forwarded(headers: &HeaderMap) -> bool {
    FORWARDING_HEADERS.iter().any(|h| headers.contains_key(*h))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim().to_string())
}

fn key_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The merchant a Bearer key belongs to (constant-time over every configured key).
fn authenticate<'a>(st: &'a AppState, headers: &HeaderMap) -> Option<&'a Merchant> {
    let token = bearer(headers)?;
    let h = key_hash(&token);
    let mut found = None;
    for m in &st.merchants {
        if bool::from(m.key_hash.ct_eq(&h)) {
            found = Some(m);
        }
    }
    found
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false)
}

/// False if the JSON text nests deeper than `max` (strings and escapes are skipped correctly).
pub fn json_depth_ok(bytes: &[u8], max: usize) -> bool {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for &b in bytes {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return false;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

enum BodyError {
    TooLarge,
    Timeout,
    Broken,
}

/// Reads the body with a streamed byte limit under one overall deadline (a per-chunk timeout would let
/// a slow-loris client hold the connection forever).
async fn read_body(body: Body, max: usize, deadline: Duration) -> Result<Bytes, BodyError> {
    let read = async {
        let mut body = body;
        let mut buf: Vec<u8> = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| BodyError::Broken)?;
            if let Some(data) = frame.data_ref() {
                if buf.len() + data.len() > max {
                    return Err(BodyError::TooLarge);
                }
                buf.extend_from_slice(data);
            }
        }
        Ok(Bytes::from(buf))
    };
    match tokio::time::timeout(deadline, read).await {
        Ok(r) => r,
        Err(_) => Err(BodyError::Timeout),
    }
}

fn rate_limited(st: &AppState, retry: u64) -> Response {
    Metrics::inc(&st.metrics().http_rate_limited);
    error_response(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many requests", Diag::RateLimited, Some(retry))
}

fn ip_check(st: &AppState, req: &Request) -> Result<(), Response> {
    let ip = client_ip(st, req);
    st.ip_limiter.check(&ip, st.fac.clock.now_ms()).map_err(|retry| rate_limited(st, retry))
}

fn killed_response(st: &AppState) -> Response {
    Metrics::inc(&st.metrics().http_kill_switch);
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "facilitator_disabled",
        "the facilitator is temporarily disabled by its operator",
        Diag::Internal,
        Some(30),
    )
}

/// Everything before dispatch: returns the authenticated merchant and the parsed request.
async fn admit(st: &Arc<AppState>, req: Request) -> Result<(Merchant, FacilitatorRequest), Response> {
    let (merchant, bytes) = admit_raw(st, req, true).await?;
    let parsed: FacilitatorRequest = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(_) => {
            Metrics::inc(&st.metrics().http_bad_requests);
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "the request body is not a facilitator request",
                Diag::InvalidKaspaX402Payload,
                None,
            ));
        }
    };
    if st.auth == AuthMode::Required {
        let ok = [&parsed.payment_requirements, &parsed.payment_payload.accepted]
            .iter()
            .all(|r| merchant.allowed_pay_to.contains(&r.pay_to) && merchant.allowed_assets.contains(&r.asset));
        if !ok {
            Metrics::inc(&st.metrics().http_forbidden);
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "this API key may not request that payTo or asset",
                Diag::Unauthorized,
                None,
            ));
        }
    }
    Ok((merchant, parsed))
}

/// Admission up to a structurally sane JSON body: kill switch, rate limits, content type, the API key
/// (`keyed_route`: merchant routes; otherwise the caller is anonymous and only the per-IP bucket applies), body size and
/// deadline, JSON depth. Returns the merchant (`Merchant::open()` for anonymous callers) and the body.
async fn admit_raw(st: &Arc<AppState>, req: Request, keyed_route: bool) -> Result<(Merchant, Bytes), Response> {
    if st.fac.killed() {
        return Err(killed_response(st));
    }
    // The per-IP bucket is the defence against ANONYMOUS traffic: a request with a valid merchant key is limited by its merchant's
    // bucket instead, so junk from a shared address (a proxy that is not declared, a NAT) cannot starve the merchants.
    let keyed = keyed_route && st.auth == AuthMode::Required && authenticate(st, req.headers()).is_some();
    if !keyed {
        ip_check(st, &req)?;
    }
    if !is_json_content_type(req.headers()) {
        Metrics::inc(&st.metrics().http_bad_requests);
        return Err(error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "content-type must be application/json",
            Diag::InvalidKaspaX402Payload,
            None,
        ));
    }
    let merchant = match st.auth {
        _ if !keyed_route => Merchant::open(),
        AuthMode::Open => Merchant::open(),
        AuthMode::Required => match authenticate(st, req.headers()) {
            Some(m) => m.clone(),
            None => {
                Metrics::inc(&st.metrics().http_unauthorized);
                return Err(error_response(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "a valid API key is required",
                    Diag::Unauthorized,
                    None,
                ));
            }
        },
    };
    if keyed_route {
        let rate = merchant.rate.unwrap_or(st.merchant_default);
        st.merchant_limiter.check_with(&merchant.id, rate, st.fac.clock.now_ms()).map_err(|retry| rate_limited(st, retry))?;
    }

    let declared = req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > st.max_body as u64) {
        Metrics::inc(&st.metrics().http_too_large);
        return Err(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body too large",
            Diag::InvalidKaspaX402Payload,
            None,
        ));
    }
    let bytes = match read_body(req.into_body(), st.max_body, st.body_deadline).await {
        Ok(b) => b,
        Err(BodyError::TooLarge) => {
            Metrics::inc(&st.metrics().http_too_large);
            return Err(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "request body too large",
                Diag::InvalidKaspaX402Payload,
                None,
            ));
        }
        Err(BodyError::Timeout) => {
            Metrics::inc(&st.metrics().http_body_timeouts);
            return Err(error_response(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "the request body was not received in time",
                Diag::InvalidKaspaX402Payload,
                None,
            ));
        }
        Err(BodyError::Broken) => {
            Metrics::inc(&st.metrics().http_bad_requests);
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "the request body could not be read",
                Diag::InvalidKaspaX402Payload,
                None,
            ));
        }
    };
    if !json_depth_ok(&bytes, MAX_JSON_DEPTH) {
        Metrics::inc(&st.metrics().http_bad_requests);
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_payload",
            "JSON nesting is too deep",
            Diag::InvalidKaspaX402Payload,
            None,
        ));
    }
    Ok((merchant, bytes))
}

fn internal_error() -> Response {
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "unexpected_settle_error", "internal error", Diag::Internal, None)
}

// -------------------------------------------------------------------------------------- handlers

async fn supported_h(State(st): State<Arc<AppState>>, req: Request) -> Response {
    if let Err(r) = ip_check(&st, &req) {
        return r;
    }
    json_response(StatusCode::OK, &st.fac.supported())
}

async fn health_h(State(st): State<Arc<AppState>>, req: Request) -> Response {
    if let Err(r) = ip_check(&st, &req) {
        return r;
    }
    json_response(StatusCode::OK, &json!({ "status": "ok", "network": st.fac.policy.network.as_str(), "enabled": !st.fac.killed() }))
}

async fn metrics_h(State(st): State<Arc<AppState>>, req: Request) -> Response {
    if let Err(r) = ip_check(&st, &req) {
        return r;
    }
    // A loopback peer is the operator only when nothing sits in front of the facilitator: never with `trustedProxies`, never when
    // the request carries a forwarding header, and never when the operator switched it off.
    let loopback = st.metrics_loopback && !forwarded(req.headers()) && peer_ip(&req).is_some_and(|ip| ip.is_loopback());
    let admin = st.admin_key.is_some_and(|k| bearer(req.headers()).is_some_and(|t| bool::from(key_hash(&t).ct_eq(&k))));
    if !(loopback || admin) {
        Metrics::inc(&st.metrics().http_unauthorized);
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", "metrics are for the operator", Diag::Unauthorized, None);
    }
    let fac = st.fac.clone();
    let text = tokio::task::spawn_blocking(move || fac.metrics_text()).await.unwrap_or_default();
    let mut r = Response::new(Body::from(text));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; version=0.0.4"));
    r
}

async fn verify_h(State(st): State<Arc<AppState>>, req: Request) -> Response {
    let (_merchant, parsed) = match admit(&st, req).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Ok(permit) = st.verify_slots.clone().try_acquire_owned() else {
        Metrics::inc(&st.metrics().http_busy);
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "too many verifications in flight",
            Diag::RateLimited,
            Some(1),
        );
    };
    let fac = st.fac.clone();
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        fac.verify(&parsed)
    })
    .await
    {
        Ok(resp) => json_response(StatusCode::OK, &serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(_) => internal_error(),
    }
}

async fn settle_h(State(st): State<Arc<AppState>>, req: Request) -> Response {
    let (merchant, parsed) = match admit(&st, req).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    // per-merchant slots first: one merchant cannot hold the whole pool
    let mslots = st
        .merchant_slots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(merchant.id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(st.per_merchant_settles)))
        .clone();
    let (Ok(mpermit), Ok(permit)) = (mslots.try_acquire_owned(), st.settle_slots.clone().try_acquire_owned()) else {
        Metrics::inc(&st.metrics().http_busy);
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "busy", "too many settlements in flight", Diag::RateLimited, Some(1));
    };
    let fac = st.fac.clone();
    // The settlement runs on a blocking task that outlives a disconnecting client: a broadcast in
    // flight must never be cancelled by a dropped connection.
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _mpermit = mpermit;
        fac.settle(&merchant.id, &parsed)
    });
    match task.await {
        Ok(resp) => {
            let mut r = json_response(StatusCode::OK, &serde_json::to_value(&resp).unwrap_or(Value::Null));
            if let Ok(enc) = header_encode(&resp) {
                if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(b"payment-response"), HeaderValue::from_str(&enc)) {
                    r.headers_mut().insert(name, val);
                }
            }
            r
        }
        Err(_) => internal_error(),
    }
}

// -------------------------------------------------------------------------------------- invoices

fn x402_error_response(e: &kob_x402::error::X402Error) -> Response {
    let status = match e.diag {
        Diag::InvoiceUnknown => StatusCode::NOT_FOUND,
        Diag::Unauthorized => StatusCode::FORBIDDEN,
        Diag::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        Diag::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    };
    let mut body = json!({ "error": e.reason.as_str(), "message": e.message });
    if let Value::Object(ext) = kob_x402::wire::failure_extension(e) {
        body["extensions"] = Value::Object(ext);
    }
    json_response(status, &body)
}

fn invoices_enabled(st: &AppState) -> Result<(), Response> {
    if st.fac.invoices.is_none() {
        return Err(error_response(StatusCode::NOT_FOUND, "not_found", "invoices are not enabled", Diag::InvoiceUnknown, None));
    }
    Ok(())
}

fn invoice_id_ok(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn unknown_invoice() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "unknown invoice", Diag::InvoiceUnknown, None)
}

async fn invoice_create_h(State(st): State<Arc<AppState>>, req: Request) -> Response {
    if let Err(r) = invoices_enabled(&st) {
        return r;
    }
    let (merchant, bytes) = match admit_raw(&st, req, true).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let inv: kob_x402::invoice::Invoice = match serde_json::from_slice(&bytes) {
        Ok(i) => i,
        Err(e) => {
            Metrics::inc(&st.metrics().http_bad_requests);
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                &format!("not an invoice: {e}"),
                Diag::InvalidInvoice,
                None,
            );
        }
    };
    let fac = st.fac.clone();
    let required = st.auth == AuthMode::Required;
    let res = tokio::task::spawn_blocking(move || {
        let allowed = |r: &kob_x402::wire::PaymentRequirements| {
            !required || (merchant.allowed_pay_to.contains(&r.pay_to) && merchant.allowed_assets.contains(&r.asset))
        };
        fac.register_invoice(&merchant.id, inv, &allowed)
    })
    .await;
    match res {
        Ok(Ok(v)) => json_response(StatusCode::OK, &v),
        Ok(Err(e)) => x402_error_response(&e),
        Err(_) => internal_error(),
    }
}

async fn invoice_get_h(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    req: Request,
) -> Response {
    if let Err(r) = ip_check(&st, &req).and_then(|_| invoices_enabled(&st)) {
        return r;
    }
    if !invoice_id_ok(&id) {
        return unknown_invoice();
    }
    match st.fac.get_invoice(&id) {
        Ok(inv) => json_response(StatusCode::OK, &serde_json::to_value(inv).unwrap_or(Value::Null)),
        Err(e) => x402_error_response(&e),
    }
}

async fn invoice_status_h(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    req: Request,
) -> Response {
    if let Err(r) = ip_check(&st, &req).and_then(|_| invoices_enabled(&st)) {
        return r;
    }
    if !invoice_id_ok(&id) {
        return unknown_invoice();
    }
    let fac = st.fac.clone();
    match tokio::task::spawn_blocking(move || fac.invoice_status(&id)).await {
        Ok(Ok(s)) => json_response(StatusCode::OK, &serde_json::to_value(s).unwrap_or(Value::Null)),
        Ok(Err(e)) => x402_error_response(&e),
        Err(_) => internal_error(),
    }
}

async fn invoice_pay_h(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    req: Request,
) -> Response {
    if let Err(r) = invoices_enabled(&st) {
        return r;
    }
    if !invoice_id_ok(&id) {
        return unknown_invoice();
    }
    let (_anonymous, bytes) = match admit_raw(&st, req, false).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let payload: kob_x402::wire::PaymentPayload = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(_) => {
            Metrics::inc(&st.metrics().http_bad_requests);
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "the body is not an x402 payment payload",
                Diag::InvalidKaspaX402Payload,
                None,
            );
        }
    };
    let Ok(permit) = st.settle_slots.clone().try_acquire_owned() else {
        Metrics::inc(&st.metrics().http_busy);
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "busy", "too many settlements in flight", Diag::RateLimited, Some(1));
    };
    let fac = st.fac.clone();
    // like /settle: the settlement outlives a disconnecting client
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        fac.pay_invoice(&id, payload)
    });
    match task.await {
        Ok(resp) => {
            let mut r = json_response(StatusCode::OK, &serde_json::to_value(&resp).unwrap_or(Value::Null));
            if let Ok(enc) = header_encode(&resp) {
                if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(b"payment-response"), HeaderValue::from_str(&enc)) {
                    r.headers_mut().insert(name, val);
                }
            }
            r
        }
        Err(_) => internal_error(),
    }
}

// ----------------------------------------------------------------------------------------- server

/// Serves `app` on `listener` until `shutdown` resolves: HTTP/1 with a header-read deadline, a
/// connection cap and the peer address attached to every request.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    header_timeout: Duration,
    max_connections: usize,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::service::TowerToHyperService;
    let slots = Arc::new(Semaphore::new(max_connections));
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("x402: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = &mut shutdown => return Ok(()),
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            drop(stream); // over the connection cap
            continue;
        };
        let svc = TowerToHyperService::new(app.clone().layer(axum::Extension(ConnectInfo(peer))));
        tokio::spawn(async move {
            let _permit = permit;
            let io = TokioIo::new(stream);
            let mut b = hyper::server::conn::http1::Builder::new();
            b.timer(TokioTimer::new()).header_read_timeout(header_timeout);
            if let Err(e) = b.serve_connection(io, svc).await {
                if !e.is_incomplete_message() {
                    eprintln!("x402: connection from {peer}: {e}");
                }
            }
        });
    }
}
