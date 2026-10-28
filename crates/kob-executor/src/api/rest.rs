//! REST handlers. Each reads the database through the pool and returns JSON.

use super::{ApiError, App};
use crate::hex::{self, Hash32};
use crate::indexer::market;
use crate::indexer::reads::{self, OrderFilter, ORDER_STATUSES};
use crate::indexer::status::FollowerState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Params = Query<HashMap<String, String>>;
type ApiResult = Result<Response, ApiError>;

fn json_ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

/// A hot read through the response cache ([`super::cache`]): the JSON body of `read` kept for the store's data generation
/// under `key`, the request's parameters (with the node's DAA score and the wall-clock second for the reads that depend on
/// them). The body is the one `Json` would send.
async fn cached<T, F>(app: &Arc<App>, key: String, read: F) -> ApiResult
where
    T: Serialize + Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> crate::indexer::db::DbResult<T> + Send + 'static,
{
    let generation = app.state.generation.get();
    let body = app
        .cache
        .get_or_compute(generation, key, || async {
            let v = app.state.pool.with(read).await?;
            serde_json::to_vec(&v).map(axum::body::Bytes::from).map_err(|e| {
                tracing::error!("api: cannot serialise a response: {e}");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error")
            })
        })
        .await?;
    Ok(([(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"))], body).into_response())
}

fn parse_hash(what: &str, s: &str) -> Result<Hash32, ApiError> {
    Hash32::parse(s).map_err(|_| ApiError::bad_request(format!("{what} must be 32 bytes of hex")))
}

fn opt_hash(q: &HashMap<String, String>, key: &str) -> Result<Option<Hash32>, ApiError> {
    q.get(key).map(|s| parse_hash(key, s)).transpose()
}

fn opt_i64(q: &HashMap<String, String>, key: &str) -> Result<Option<i64>, ApiError> {
    q.get(key).map(|s| s.parse::<i64>().map_err(|_| ApiError::bad_request(format!("{key} must be an integer")))).transpose()
}

fn limit(q: &HashMap<String, String>, key: &str, default: usize, max: usize) -> Result<usize, ApiError> {
    let max = max.max(1);
    match q.get(key) {
        None => Ok(default.min(max)),
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n >= 1 => Ok(n.min(max)),
            _ => Err(ApiError::bad_request(format!("{key} must be a positive integer"))),
        },
    }
}

fn flag(q: &HashMap<String, String>, key: &str) -> Result<Option<bool>, ApiError> {
    match q.get(key).map(String::as_str) {
        None => Ok(None),
        Some("true") | Some("1") => Ok(Some(true)),
        Some("false") | Some("0") => Ok(Some(false)),
        Some(_) => Err(ApiError::bad_request(format!("{key} must be true or false"))),
    }
}

pub async fn health(State(app): State<Arc<App>>) -> ApiResult {
    let snap = app.state.health.snapshot();
    let lag_daa = snap.lag_daa();
    let lag_seconds = lag_daa.map(|d| d / app.state.daa_per_second.max(1));
    let mut hours = app.state.lag_alarm_hours.clone();
    hours.sort_unstable();
    let alarms: Vec<Value> = hours
        .iter()
        .filter(|h| lag_seconds.is_some_and(|l| l >= **h * 3600))
        .map(|h| json!({"lag_hours_at_least": h, "lag_seconds": lag_seconds}))
        .collect();
    let running = matches!(snap.state, FollowerState::Following | FollowerState::CatchingUp);
    let ok = running && alarms.is_empty();
    // Counters are informational: a busy database must not turn the health endpoint into an error. A few rows (kept by the
    // write path), read on the light lane: never behind the heavy reads.
    let counters = app.state.pool.with_light(reads::counters).await.ok();
    let mut v = serde_json::to_value(&snap).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("ok".into(), json!(ok));
        o.insert("caught_up".into(), json!(snap.caught_up()));
        // what the matcher, keepers and maintenance gate on: caught up, or within `max_lag_secs` of the node
        o.insert("within_lag_tolerance".into(), json!(snap.within_lag_tolerance()));
        o.insert("lag_daa".into(), json!(lag_daa));
        o.insert("lag_seconds".into(), json!(lag_seconds));
        o.insert("bytes_per_tx".into(), json!(snap.bytes_per_tx()));
        o.insert("alarms".into(), json!(alarms));
        o.insert("settle_depth_daa".into(), json!(app.state.settle_depth_daa));
        // the smallest order the executor lists (sompi, a decimal string like every amount): a smaller one is indexed, never listed
        o.insert("min_order_value_sompi".into(), json!(app.state.min_order_value_sompi.to_string()));
        o.insert("counters".into(), json!(counters));
        // which build answers: the crate version, the commit it was built from (null when the build did not say), and the KOB1
        // payload version it reads and writes (docs/ops/release.md, 7)
        if let Some(b) = crate::version::health_fields().as_object() {
            o.extend(b.clone());
        }
    }
    json_ok(v)
}

/// `GET /v1/metrics`: the follower's state in the Prometheus text format (the same numbers `/v1/health` serves).
pub async fn metrics(State(app): State<Arc<App>>) -> ApiResult {
    let text = app.state.health.snapshot().metrics_text();
    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], text).into_response())
}

/// Load-balancer probe: 200 only while following (or catching up) with lag below the smallest alarm.
pub async fn ready(State(app): State<Arc<App>>) -> ApiResult {
    let snap = app.state.health.snapshot();
    let lag_seconds = snap.lag_daa().map(|d| d / app.state.daa_per_second.max(1));
    let min_alarm = app.state.lag_alarm_hours.iter().copied().min();
    let (ready, reason) = match snap.state {
        FollowerState::Following | FollowerState::CatchingUp => match (lag_seconds, min_alarm) {
            (None, _) => (false, "node position unknown"),
            (Some(l), Some(h)) if l >= h * 3600 => (false, "indexer lag above the first alarm threshold"),
            _ => (true, "ok"),
        },
        _ => (false, "indexer is not following the chain"),
    };
    let status = if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    Ok((status, Json(json!({"ready": ready, "reason": reason, "state": snap.state, "lag_seconds": lag_seconds}))).into_response())
}

pub async fn tokens(State(app): State<Arc<App>>) -> ApiResult {
    let allow = app.state.tokens.clone();
    cached(&app, "tokens".into(), move |c| Ok(json!({ "tokens": reads::token_list(c, &allow)? }))).await
}

/// `GET /v1/tokens/{covenant_id}/declaration`: the KCC-23 launch proof of the covenant id (`kob_protocol::kcc23::LaunchProof`,
/// format `kob-kcc23-proof/1`: the preparation and genesis transactions in the fields their ids commit to). The indexer kept it
/// only after `LaunchProof::verify` accepted it, and a client verifies it again itself: an indexer can withhold a declaration,
/// never forge one. One primary-key lookup, on the light lane; 404 when the indexer holds none.
pub async fn token_declaration(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let id = parse_hash("covenant_id", &id)?;
    match app.state.pool.with_light(move |c| crate::indexer::kcc23::proof_json(c, &id)).await? {
        // stored as `serde_json::to_string(&proof)`: served as it is
        Some(json) => {
            Ok(([(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"))], json).into_response())
        }
        None => Err(ApiError::not_found("no KCC-23 declaration for this covenant id")),
    }
}

pub async fn book(State(app): State<Arc<App>>, Path(token): Path<String>, Query(q): Params) -> ApiResult {
    let token = parse_hash("token", &token)?;
    let depth = limit(&q, "depth", 20, app.state.cfg.max_page_size)?;
    let aggregate = flag(&q, "aggregate")?.unwrap_or(true);
    let ctx = app.ctx();
    let key = format!("book {token} {depth} {aggregate} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| reads::book(c, &ctx, &token, depth, aggregate)).await
}

/// `GET /v1/pairs?token=`: pairs with listed live pair orders (`indexer::pairs::pairs`), a JSON array.
pub async fn pairs(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let token = opt_hash(&q, "token")?;
    let ctx = app.ctx();
    let v = app.state.pool.with(move |c| crate::indexer::pairs::pairs(c, &ctx, token.as_ref())).await?;
    json_ok(v)
}

/// `GET /v1/pairs/{base}/{quote}/book?depth=`: the pair orders' levels (`direct` KobPair, `entry` KobIfdPair) and the implied
/// quotes of the KAS route.
pub async fn pair_book(State(app): State<Arc<App>>, Path((base, quote)): Path<(String, String)>, Query(q): Params) -> ApiResult {
    let (base, quote) = pair_path(&base, &quote)?;
    let depth = limit(&q, "depth", 20, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app.state.pool.with(move |c| crate::indexer::pairs::pair_book(c, &ctx, &base, &quote, depth)).await?;
    json_ok(v)
}

fn pair_path(base: &str, quote: &str) -> Result<(Hash32, Hash32), ApiError> {
    let base = parse_hash("base", base)?;
    let quote = parse_hash("quote", quote)?;
    if base == quote {
        return Err(ApiError::bad_request("base and quote must be different tokens"));
    }
    Ok((base, quote))
}

/// `GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=`: pair candles derived from the KAS series of the two
/// tokens (`indexer::pairs::pair_candles`; never from pair fills).
pub async fn pair_candles(State(app): State<Arc<App>>, Path((base, quote)): Path<(String, String)>, Query(q): Params) -> ApiResult {
    let (base, quote) = pair_path(&base, &quote)?;
    let interval = q.get("interval").cloned().unwrap_or_else(|| "5m".into());
    let iv = market::interval_ms(&interval).ok_or_else(|| ApiError::bad_request("interval must be one of 1m, 5m, 1h, 1d"))?;
    let from = opt_i64(&q, "from")?;
    let to = opt_i64(&q, "to")?;
    if let (Some(f), Some(t)) = (from, to) {
        if t <= f {
            return Err(ApiError::bad_request("to must be after from"));
        }
    }
    let n = limit(&q, "limit", 500, 1500)?;
    let tokens = app.state.tokens.clone();
    let v = app
        .state
        .pool
        .with(move |c| {
            let decimals = (reads::token_decimals(c, &tokens, &base)?, reads::token_decimals(c, &tokens, &quote)?);
            crate::indexer::pairs::pair_candles(c, &base, &quote, decimals, &interval, iv, from, to, n)
        })
        .await?;
    json_ok(v)
}

/// `GET /v1/pairs/{base}/{quote}/fills?limit=&before=`: pair-order fills of the pair, newest first, and the 24 h pair volume
/// (`counterparty` route / netting / inventory, `price_source` none).
pub async fn pair_fills(State(app): State<Arc<App>>, Path((base, quote)): Path<(String, String)>, Query(q): Params) -> ApiResult {
    let (base, quote) = pair_path(&base, &quote)?;
    let before = opt_i64(&q, "before")?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app.state.pool.with(move |c| crate::indexer::pairs::pair_fills(c, &ctx, &base, &quote, before, n)).await?;
    json_ok(v)
}

pub async fn order(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let id = parse_hash("covenant_id", &id)?;
    let ctx = app.ctx();
    // one order: a few index lookups, on the light lane
    match app.state.pool.with_light(move |c| reads::order(c, &ctx, &id)).await? {
        // the genesis warning of its token(s) (docs/spec/matcher.md 1.2a: on every order view of that token)
        Some(o) => json_ok(o.with_warnings(|t| app.state.tokens.warnings_for(t))),
        None => Err(ApiError::not_found("unknown covenant id")),
    }
}

pub async fn orders(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let maker = match q.get("maker") {
        None => None,
        Some(s) => Some(hex::decode(s).ok().filter(|b| !b.is_empty()).ok_or_else(|| ApiError::bad_request("maker must be hex"))?),
    };
    let token = opt_hash(&q, "token")?;
    let status = match q.get("status") {
        None => None,
        Some(s) if ORDER_STATUSES.contains(&s.as_str()) => Some(s.clone()),
        Some(_) => return Err(ApiError::bad_request(format!("status must be one of {}", ORDER_STATUSES.join(", ")))),
    };
    let cursor = match q.get("cursor") {
        None => None,
        Some(s) => Some(reads::parse_order_cursor(s).ok_or_else(|| ApiError::bad_request("invalid cursor"))?),
    };
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let filter = OrderFilter { maker, token, status };
    let mut v = app.state.pool.with(move |c| reads::orders(c, &ctx, &filter, n, cursor)).await?;
    v.items = v.items.into_iter().map(|o| o.with_warnings(|t| app.state.tokens.warnings_for(t))).collect();
    json_ok(v)
}

pub async fn order_events(State(app): State<Arc<App>>, Path(id): Path<String>, Query(q): Params) -> ApiResult {
    let id = parse_hash("covenant_id", &id)?;
    let after = opt_i64(&q, "after")?;
    let n = limit(&q, "limit", 100, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app
        .state
        .pool
        .with_light(move |c| {
            if reads::order(c, &ctx, &id)?.is_none() {
                return Ok(None);
            }
            Ok(Some(reads::order_events(c, &ctx, &id, n, after)?))
        })
        .await?;
    match v {
        Some(p) => json_ok(p),
        None => Err(ApiError::not_found("unknown covenant id")),
    }
}

pub async fn fills(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let token = opt_hash(&q, "token")?;
    let side = match q.get("side").map(String::as_str) {
        None => None,
        Some("1") | Some("ask") | Some("sell") => Some(1),
        Some("2") | Some("bid") | Some("buy") => Some(2),
        Some(_) => return Err(ApiError::bad_request("side must be 1|2|ask|bid")),
    };
    let before = opt_i64(&q, "before")?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app.state.pool.with(move |c| reads::fills(c, &ctx, token.as_ref(), side, before, n)).await?;
    json_ok(v)
}

pub async fn strays(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let maker = match q.get("maker") {
        None => None,
        Some(s) => Some(hex::decode(s).ok().filter(|b| !b.is_empty()).ok_or_else(|| ApiError::bad_request("maker must be hex"))?),
    };
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app.state.pool.with(move |c| reads::strays(c, &ctx, maker.as_deref(), n)).await?;
    json_ok(json!({ "items": v }))
}

pub async fn token_events(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let token = opt_hash(&q, "token")?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let v = app.state.pool.with(move |c| reads::token_events(c, token.as_ref(), n)).await?;
    json_ok(json!({ "items": v }))
}

/// The 32-byte owner a `owner=` parameter names: an x-only public key or covenant id in hex, or a Kaspa P2PK address.
fn parse_owner(s: &str) -> Result<Vec<u8>, ApiError> {
    if let Ok(h) = Hash32::parse(s) {
        return Ok(h.0.to_vec());
    }
    match kaspa_addresses::Address::try_from(s) {
        Ok(a) if a.version == kaspa_addresses::Version::PubKey && a.payload.len() == 32 => Ok(a.payload.to_vec()),
        Ok(_) => Err(ApiError::bad_request("owner address must be a P2PK (Schnorr) address")),
        Err(_) => Err(ApiError::bad_request("owner must be 32 bytes of hex (public key or covenant id) or a Kaspa P2PK address")),
    }
}

/// Token UTXOs of tracked tokens with proven states, by owner and/or token (`docs/ops/executor.md`, Part B 5.1).
pub async fn token_utxos(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let owner = q.get("owner").map(|s| parse_owner(s)).transpose()?;
    let token = opt_hash(&q, "token")?;
    if owner.is_none() && token.is_none() {
        return Err(ApiError::bad_request("owner or token is required"));
    }
    let include_spent = flag(&q, "spent")?.unwrap_or(false);
    let cursor = match q.get("cursor") {
        None => None,
        Some(s) => Some(reads::parse_holding_cursor(s).ok_or_else(|| ApiError::bad_request("invalid cursor"))?),
    };
    let n = limit(&q, "limit", 100, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let filter = reads::HoldingFilter { owner, token, include_spent };
    let v = app.state.pool.with(move |c| reads::holdings(c, &ctx, &filter, n, cursor)).await?;
    json_ok(v)
}

// ------------------------------------------------------------------------------------------------ market data (B 5.3)

// The market views' price basis is the token's standard scale: its decimals from its KCC-23 declaration, else its registry entry,
// else its program's default ([`reads::token_decimals`]); without any the scale of its first order. Each read resolves them in
// its own database connection.

/// `GET /v1/trades/{token}?limit=&before=`: trades (fills grouped per transaction), newest first.
pub async fn trades(State(app): State<Arc<App>>, Path(token): Path<String>, Query(q): Params) -> ApiResult {
    let token = parse_hash("token", &token)?;
    let before = opt_i64(&q, "before")?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let tokens = app.state.tokens.clone();
    let ctx = app.ctx();
    let v = app
        .state
        .pool
        .with(move |c| {
            let decimals = reads::token_decimals(c, &tokens, &token)?;
            market::trades(c, &ctx, &token, decimals, before, n)
        })
        .await?;
    json_ok(v)
}

/// `GET /v1/candles/{token}?interval=1m|5m|1h|1d&from=&to=&limit=`: OHLCV, ascending, empty buckets omitted.
pub async fn candles(State(app): State<Arc<App>>, Path(token): Path<String>, Query(q): Params) -> ApiResult {
    let token = parse_hash("token", &token)?;
    let interval = q.get("interval").cloned().unwrap_or_else(|| "5m".into());
    let iv = market::interval_ms(&interval).ok_or_else(|| ApiError::bad_request("interval must be one of 1m, 5m, 1h, 1d"))?;
    let from = opt_i64(&q, "from")?;
    let to = opt_i64(&q, "to")?;
    if let (Some(f), Some(t)) = (from, to) {
        if t <= f {
            return Err(ApiError::bad_request("to must be after from"));
        }
    }
    let n = limit(&q, "limit", 500, 1500)?;
    let tokens = app.state.tokens.clone();
    let key = format!("candles {token} {interval} {from:?} {to:?} {n}");
    cached(&app, key, move |c| {
        let decimals = reads::token_decimals(c, &tokens, &token)?;
        market::candles(c, &token, decimals, &interval, iv, from, to, n)
    })
    .await
}

/// `GET /v1/stats/{token}`: 24 h statistics and the top of the book.
pub async fn stats(State(app): State<Arc<App>>, Path(token): Path<String>) -> ApiResult {
    let token = parse_hash("token", &token)?;
    let tokens = app.state.tokens.clone();
    let ctx = app.ctx();
    let key = format!("stats {token} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| {
        let decimals = reads::token_decimals(c, &tokens, &token)?;
        market::stats(c, &ctx, &token, decimals)
    })
    .await
}

/// `GET /v1/depth/{token}?levels=`: cumulative depth of the listed book, levels merged by per-basis price.
pub async fn depth(State(app): State<Arc<App>>, Path(token): Path<String>, Query(q): Params) -> ApiResult {
    let token = parse_hash("token", &token)?;
    let n = limit(&q, "levels", 50, app.state.cfg.max_page_size)?;
    let tokens = app.state.tokens.clone();
    let ctx = app.ctx();
    let key = format!("depth {token} {n} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| {
        let decimals = reads::token_decimals(c, &tokens, &token)?;
        market::depth(c, &ctx, &token, decimals, n)
    })
    .await
}
