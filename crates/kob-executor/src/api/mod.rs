//! Read API: REST + WebSocket over the indexer database.
//!
//! Everything is served from the database through the read pool; the node is never contacted, so
//! public load cannot reach it. Every route passes one guard middleware that resolves the client
//! address (proxy aware), applies the per-client and global rate limits, the concurrency cap and the
//! request timeout, and adds CORS headers when configured.
//!
//! Routes (all GET):
//! - `/v1/health`, `/v1/health/ready`
//! - `/v1/tokens`
//! - `/v1/books/{token}?depth=&aggregate=` (KAS books; pair orders are in no KAS book)
//! - `/v1/pairs?token=`, `/v1/pairs/{base}/{quote}/book?depth=` (pair orders and the implied KAS-route quotes),
//!   `/v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=` (derived from the two KAS series),
//!   `/v1/pairs/{base}/{quote}/fills?limit=&before=` (pair-order fills: volume, counterparty, never a price)
//! - `/v1/orders?maker=&token=&status=&limit=&cursor=`, `/v1/orders/{covenant_id}`, `/v1/orders/{covenant_id}/events`
//! - `/v1/fills?token=&side=&limit=&before=`
//! - `/v1/ws` (WebSocket, see [`ws`])

pub mod client_ip;
pub mod ratelimit;
pub mod rest;
pub mod ws;

use crate::config::ApiConfig;
use crate::indexer::db::{DbError, ReadPool};
use crate::indexer::reads::ReadCtx;
use crate::indexer::status::{HealthState, IndexEvent};
use crate::tokens::TokenAllowlist;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use client_ip::{ClientIpResolver, TrustedProxies};
use ratelimit::{retry_after_secs, RateLimiter};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Semaphore};

/// Everything the API needs from the rest of the process.
#[derive(Clone)]
pub struct ApiState {
    pub pool: ReadPool,
    pub health: Arc<HealthState>,
    /// The follower publishes one event per committed batch here.
    pub events: tokio::sync::broadcast::Sender<Arc<IndexEvent>>,
    pub tokens: Arc<TokenAllowlist>,
    pub cfg: ApiConfig,
    pub settle_depth_daa: u64,
    pub daa_per_second: u64,
    pub lag_alarm_hours: Vec<u64>,
}

impl ApiState {
    /// Check the configuration up front (invalid `trusted_proxies` entries would otherwise be
    /// ignored, which fails closed but silently).
    pub fn validate(&self) -> Result<(), String> {
        TrustedProxies::parse(&self.cfg.trusted_proxies).map(|_| ()).map_err(|e| format!("api.trusted_proxies: {e}"))
    }
}

/// Shared per-server state behind every handler.
pub struct App {
    pub state: ApiState,
    pub(crate) limiter: RateLimiter,
    resolver: ClientIpResolver,
    concurrency: Arc<Semaphore>,
    pub(crate) ws: ws::WsRegistry,
    pub(crate) shutdown: watch::Sender<bool>,
}

impl App {
    pub(crate) fn ctx(&self) -> ReadCtx {
        ReadCtx {
            node_daa: self.state.health.snapshot().node_daa,
            settle_depth_daa: self.state.settle_depth_daa,
            now_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()),
        }
    }
}

/// The client address a request is attributed to (set by the guard).
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

/// JSON error body `{"error":{"code":..,"message":..}}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into(), retry_after: None }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        let busy = matches!(&e, DbError::Sqlite(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked));
        if busy {
            let mut a = ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "db_busy", "database busy, retry shortly");
            a.retry_after = Some(1);
            a
        } else {
            tracing::error!("api db error: {e}");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error")
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({"error": {"code": self.code, "message": self.message}});
        let mut resp = (self.status, Json(body)).into_response();
        if let Some(s) = self.retry_after {
            resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(s));
        }
        resp
    }
}

/// Build the router. Rate limiting uses the system clock.
pub fn router(state: ApiState) -> Router {
    build(state, None).0
}

pub(crate) fn build(state: ApiState, limiter: Option<RateLimiter>) -> (Router, Arc<App>) {
    let proxies = match TrustedProxies::parse(&state.cfg.trusted_proxies) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("ignoring api.trusted_proxies (client headers will not be trusted): {e}");
            TrustedProxies::default()
        }
    };
    let (shutdown, _) = watch::channel(false);
    let app = Arc::new(App {
        limiter: limiter.unwrap_or_else(|| RateLimiter::new(state.cfg.rate_limit.clone())),
        resolver: ClientIpResolver::new(proxies, &state.cfg.client_ip_header),
        concurrency: Arc::new(Semaphore::new(state.cfg.max_concurrent_requests.max(1))),
        ws: ws::WsRegistry::default(),
        shutdown,
        state,
    });
    let router = Router::new()
        .route("/v1/health", get(rest::health))
        .route("/v1/health/ready", get(rest::ready))
        .route("/v1/metrics", get(rest::metrics))
        .route("/v1/tokens", get(rest::tokens))
        .route("/v1/books/{token}", get(rest::book))
        .route("/v1/pairs", get(rest::pairs))
        .route("/v1/pairs/{base}/{quote}/book", get(rest::pair_book))
        .route("/v1/pairs/{base}/{quote}/candles", get(rest::pair_candles))
        .route("/v1/pairs/{base}/{quote}/fills", get(rest::pair_fills))
        .route("/v1/orders", get(rest::orders))
        .route("/v1/orders/{covenant_id}", get(rest::order))
        .route("/v1/orders/{covenant_id}/events", get(rest::order_events))
        .route("/v1/fills", get(rest::fills))
        .route("/v1/strays", get(rest::strays))
        .route("/v1/token-events", get(rest::token_events))
        .route("/v1/token-utxos", get(rest::token_utxos))
        .route("/v1/trades/{token}", get(rest::trades))
        .route("/v1/candles/{token}", get(rest::candles))
        .route("/v1/stats/{token}", get(rest::stats))
        .route("/v1/depth/{token}", get(rest::depth))
        .route("/v1/ws", get(ws::handler))
        .fallback(|| async { ApiError::not_found("no such route") })
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app.clone());
    (router, app)
}

/// Rate-limit weight of a route: the queries that scan (candles, stats, depth, trades, books, holdings, strays) cost `heavy`
/// tokens, everything else one.
fn route_cost(path: &str, heavy: u32) -> u32 {
    const HEAVY: [&str; 8] =
        ["/v1/candles/", "/v1/stats/", "/v1/depth/", "/v1/trades/", "/v1/books/", "/v1/token-utxos", "/v1/strays", "/v1/pairs"];
    if HEAVY.iter().any(|p| path.starts_with(p)) {
        heavy.max(1)
    } else {
        1
    }
}

/// Client identity, rate limit, concurrency cap, timeout and CORS for every request.
async fn guard(State(app): State<Arc<App>>, mut req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let ip = app.resolver.resolve(peer, req.headers());
    req.extensions_mut().insert(ClientIp(ip));
    let cors = app.state.cfg.cors_allow_origin.clone();

    let cost = route_cost(req.uri().path(), app.state.cfg.rate_limit.heavy_route_cost);
    let mut resp = if let Err(wait) = app.limiter.check_cost(ip, cost) {
        let mut e = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "rate limit exceeded");
        e.retry_after = Some(retry_after_secs(wait));
        e.into_response()
    } else if req.method() == Method::OPTIONS && cors.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        match app.concurrency.clone().try_acquire_owned() {
            Err(_) => {
                let mut e = ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "overloaded", "too many requests in flight");
                e.retry_after = Some(1);
                e.into_response()
            }
            Ok(_permit) => {
                let limit = Duration::from_millis(app.state.cfg.request_timeout_ms.max(1));
                match tokio::time::timeout(limit, next.run(req)).await {
                    Ok(r) => r,
                    Err(_) => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", "request timed out").into_response(),
                }
            }
        }
    };
    if let Some(origin) = cors {
        if let Ok(v) = HeaderValue::from_str(&origin) {
            let h = resp.headers_mut();
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, OPTIONS"));
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        }
    }
    resp
}

/// Bind `cfg.listen` and serve until `shutdown` resolves.
pub async fn serve(state: ApiState, shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(state.cfg.listen).await?;
    tracing::info!("read API listening on {}", listener.local_addr()?);
    serve_listener(listener, state, shutdown).await
}

/// Serve on an already bound listener (tests bind port 0).
pub async fn serve_listener(
    listener: tokio::net::TcpListener,
    state: ApiState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let (router, app) = build(state, None);
    let signal = app.shutdown.clone();
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown.await;
            // Upgraded WebSocket connections are not tracked by graceful shutdown; tell them to close.
            let _ = signal.send(true);
        })
        .await
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cost_tests {
    use super::route_cost;

    #[test]
    fn scanning_routes_cost_the_heavy_weight() {
        for p in [
            "/v1/candles/ab",
            "/v1/stats/ab",
            "/v1/depth/ab",
            "/v1/trades/ab",
            "/v1/books/ab",
            "/v1/strays",
            "/v1/token-utxos",
            "/v1/pairs",
            "/v1/pairs/ab/cd/book",
            "/v1/pairs/ab/cd/candles",
            "/v1/pairs/ab/cd/fills",
        ] {
            assert_eq!(route_cost(p, 5), 5, "{p}");
        }
        for p in ["/v1/health", "/v1/tokens", "/v1/orders/ab", "/v1/ws"] {
            assert_eq!(route_cost(p, 5), 1, "{p}");
        }
        assert_eq!(route_cost("/v1/stats/ab", 0), 1, "a zero weight still costs one");
    }
}
