//! REST handlers of the read API. Each reads the database through the pool and returns JSON.

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

/// A hot read through the response cache ([`super::cache`]).
///
/// The JSON body of `read` is kept for the data generation of the store under `key`. The key is
/// made of the parameters of the request, with the DAA score of the node and the wall-clock
/// second for the reads that depend on them. The body is the one `Json` would send.
///
/// Everyone who asks for the key shares one computation, so it must not wait for its starter's
/// own queue (R4C2-3). The starter takes its heavy permit first, waiting at most
/// `db::CLIENT_WAIT`, and only then starts the computation; a request that finds the key kept or
/// being computed takes none and joins. The read runs in a task of its own, outside any client's
/// scope, and holds the starter's permit, so it still counts against the starter's reads and
/// queues in the lane like any other read. A request that goes away leaves it running.
async fn cached<T, F>(app: &Arc<App>, key: String, read: F) -> ApiResult
where
    T: Serialize + Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> crate::indexer::db::DbResult<T> + Send + 'static,
{
    let generation = app.state.generation.get();
    let permit = if app.cache.contains(generation, &key) { None } else { crate::indexer::db::client_permit(false).await? };
    let pool = app.state.pool.clone();
    let body = app
        .cache
        .get_or_compute(generation, key, move || async move {
            // The entry was found and has gone since (a failure): this request starts it after all.
            let permit = match permit {
                Some(p) => Some(p),
                None => crate::indexer::db::client_permit(false).await?,
            };
            let task = tokio::spawn(async move {
                let _permit = permit;
                pool.with(read).await
            });
            let v = task.await.map_err(|e| {
                tracing::error!("api: a cached read failed: {e}");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error")
            })??;
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

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// `GET /v1/health`: the follower state, the lag and alarms, and the counters of the store.
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
    // A follower that hangs, or a task that died before it could say so, leaves its last state in
    // place: no progress for long is not ok either (C-M1).
    let stale = snap.stale(unix_ms());
    let ok = running && !stale && alarms.is_empty();
    // The counters are informational. A busy database must not turn the health endpoint into an
    // error, nor make it wait: they are served from memory (`health_counters`). A lean store, the
    // one of the Executor, prunes old closed orders; `history_from` is the time from which its
    // trade history is complete. It is null for a full store.
    let (counters, history_from) = health_counters(&app, snap.lean).await;
    let mut v = serde_json::to_value(&snap).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("ok".into(), json!(ok));
        o.insert("caught_up".into(), json!(snap.caught_up()));
        o.insert("stale".into(), json!(stale));
        // What the matcher, keepers and maintenance gate on: caught up, or within `max_lag_secs`
        // of the node.
        o.insert("within_lag_tolerance".into(), json!(snap.within_lag_tolerance()));
        o.insert("lag_daa".into(), json!(lag_daa));
        o.insert("lag_seconds".into(), json!(lag_seconds));
        o.insert("bytes_per_tx".into(), json!(snap.bytes_per_tx()));
        o.insert("alarms".into(), json!(alarms));
        o.insert("settle_depth_daa".into(), json!(app.state.settle_depth_daa));
        // The smallest order the executor lists, in sompi, as a decimal string like every amount.
        // A smaller order is indexed and never listed.
        o.insert("min_order_value_sompi".into(), json!(app.state.min_order_value_sompi.to_string()));
        o.insert("counters".into(), json!(counters));
        o.insert("history_from".into(), json!(history_from));
        // Which build answers: the crate version, the commit it was built from (null when the
        // build did not say), and the KOB1 payload version it reads and writes. See
        // `docs/ops/release.md`, section 7.
        if let Some(b) = crate::version::health_fields().as_object() {
            o.extend(b.clone());
        }
    }
    json_ok(v)
}

/// How old the store counters of `/v1/health` may be before a request reads them again. A commit
/// (a new data generation) makes them old at once.
pub const HEALTH_COUNTERS_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// How long `/v1/health` waits for a read of the counters it started before it answers with the
/// ones it has.
pub const HEALTH_COUNTERS_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// The store counters of `/v1/health`, read at most once a second ([`health_counters`]).
#[derive(Default)]
pub(crate) struct HealthCounters {
    /// When they were read, for which data generation, and whether for a lean store.
    at: Option<(std::time::Instant, u64, bool)>,
    counters: Option<reads::Counters>,
    history_from: Option<i64>,
    reading: bool,
}

/// The store counters of `/v1/health` and, for a lean store, `history_from` (R3C2-1).
///
/// They come from memory. One read on the light lane refreshes them when they are more than
/// [`HEALTH_COUNTERS_EVERY`] old or a commit went through since, at most one at a time and outside the requests' own lane
/// limits; a request waits for it at most [`HEALTH_COUNTERS_WAIT`]. So a starved light lane delays
/// the counters, never the health answer, and a flood of health checks reads the store at most
/// once a second.
async fn health_counters(app: &Arc<App>, lean: bool) -> (Option<reads::Counters>, Option<i64>) {
    let start = {
        let mut g = app.health_counters.lock().unwrap_or_else(|e| e.into_inner());
        let generation = app.state.generation.get();
        let fresh = g.at.is_some_and(|(t, gen, l)| t.elapsed() < HEALTH_COUNTERS_EVERY && gen == generation && l == lean);
        let start = !fresh && !g.reading;
        g.reading |= start;
        start
    };
    if start {
        let a = app.clone();
        let generation = app.state.generation.get();
        let read = tokio::spawn(async move {
            let counters = a.state.pool.with_light(reads::counters).await.ok();
            let history_from = if lean { a.state.pool.with_light(reads::history_from).await.ok().flatten() } else { None };
            let mut g = a.health_counters.lock().unwrap_or_else(|e| e.into_inner());
            g.at = Some((std::time::Instant::now(), generation, lean));
            // A failed read keeps the last counters.
            if counters.is_some() {
                g.counters = counters;
                g.history_from = history_from;
            }
            g.reading = false;
        });
        let _ = tokio::time::timeout(HEALTH_COUNTERS_WAIT, read).await;
    }
    let g = app.health_counters.lock().unwrap_or_else(|e| e.into_inner());
    (g.counters.clone(), if lean { g.history_from } else { None })
}

/// `GET /v1/metrics`: the follower state in the Prometheus text format.
///
/// The numbers are the ones `/v1/health` serves.
pub async fn metrics(State(app): State<Arc<App>>) -> ApiResult {
    // The series of the indexer, then those of the Executor runner (`kobd --executor`).
    let snap = app.state.health.snapshot();
    let text = snap.metrics_text() + &snap.executor_metrics;
    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], text).into_response())
}

/// `GET /v1/health/ready`: the load-balancer probe.
///
/// It answers 200 only while the follower is following or catching up, with a lag below the
/// smallest alarm. Otherwise it answers 503.
pub async fn ready(State(app): State<Arc<App>>) -> ApiResult {
    let snap = app.state.health.snapshot();
    let lag_seconds = snap.lag_daa().map(|d| d / app.state.daa_per_second.max(1));
    let min_alarm = app.state.lag_alarm_hours.iter().copied().min();
    let (ready, reason) = match snap.state {
        FollowerState::Following | FollowerState::CatchingUp if snap.stale(unix_ms()) => (false, "no progress for 10 minutes"),
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

/// `GET /v1/tokens`: the token list.
pub async fn tokens(State(app): State<Arc<App>>) -> ApiResult {
    let allow = app.state.tokens.clone();
    cached(&app, "tokens".into(), move |c| Ok(json!({ "tokens": reads::token_list(c, &allow)? }))).await
}

/// `GET /v1/tokens/{covenant_id}/declaration`: the KCC-23 launch proof of the covenant id.
///
/// The proof is a `kob_protocol::kcc23::LaunchProof` in the format `kob-kcc23-proof/1`. It holds
/// the preparation and genesis transactions in the fields their ids commit to. The indexer
/// stores a proof only after `LaunchProof::verify` accepts it, and a client verifies it again
/// itself. An indexer can withhold a declaration and cannot forge one.
///
/// It is one primary-key lookup on the light lane. It answers 404 when the indexer holds none.
pub async fn token_declaration(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let id = parse_hash("covenant_id", &id)?;
    match app.state.pool.with_light(move |c| crate::indexer::kcc23::proof_json(c, &id)).await? {
        // The proof is stored as `serde_json::to_string(&proof)` and served as it is.
        Some(json) => {
            Ok(([(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"))], json).into_response())
        }
        None => Err(ApiError::not_found("no KCC-23 declaration for this covenant id")),
    }
}

/// `GET /v1/creator-locks?key=`: the tokens whose KCC-23 declaration locks a share for `key` (an
/// x-only key in hex or a P2PK address), each with its lock as `LaunchProof::verify` found it
/// against the genesis (`until`, `authority`, `amount`, `outputs`, `supply`, `program`). A client
/// that relies on one verifies the token's launch proof itself
/// (`GET /v1/tokens/{covenant_id}/declaration`). The locked UTXOs still unspent are
/// `GET /v1/token-utxos?owner=<authority>&token=<covenant_id>`.
pub async fn creator_locks(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    // `?due=<unix ms>`: the automatic locks due then, for keepers (no key)
    if let Some(due) = q.get("due") {
        if q.contains_key("key") {
            return Err(ApiError::bad_request("give key or due, not both"));
        }
        let due: u64 = due.parse().map_err(|_| ApiError::bad_request("due must be a unix time in milliseconds"))?;
        let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
        let v = app.state.pool.with(move |c| crate::indexer::kcc23::due_locks(c, due, n)).await?;
        return json_ok(json!({ "items": lock_items(v), "next_cursor": null }));
    }
    let key: [u8; 32] = q
        .get("key")
        .map(|s| parse_owner(s))
        .transpose()?
        .ok_or_else(|| ApiError::bad_request("key is required"))?
        .try_into()
        .map_err(|_| ApiError::bad_request("key must be 32 bytes"))?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let cursor = match q.get("cursor") {
        None => None,
        Some(s) => Some(reads::parse_order_cursor(s).ok_or_else(|| ApiError::bad_request("invalid cursor"))?),
    };
    let (v, next) = app
        .state
        .pool
        .with(move |c| crate::indexer::kcc23::creator_locks(c, &key, n, cursor.as_ref().map(|(b, id)| (*b, id.as_slice()))))
        .await?;
    let next_cursor = next.map(|(b, id)| format!("{b}:{}", hex::encode(&id)));
    json_ok(json!({ "items": lock_items(v), "next_cursor": next_cursor }))
}

/// The items of `GET /v1/creator-locks`: each verified lock with its token's covenant id, genesis
/// and symbol.
fn lock_items(v: Vec<kob_protocol::kcc23::VerifiedDeclaration>) -> Vec<Value> {
    v.into_iter()
        .filter_map(|d| {
            let l = d.lock?;
            let mut lock = serde_json::to_value(&l).ok()?;
            lock["covenant_id"] = json!(Hash32(d.covenant_id).to_hex());
            lock["genesis_txid"] = json!(Hash32(d.genesis_txid).to_hex());
            lock["symbol"] = json!(d.token.symbol);
            Some(lock)
        })
        .collect()
}

/// `GET /v1/books/{token}?depth=&aggregate=`: the KAS book of a token. Pair orders are not in it.
pub async fn book(State(app): State<Arc<App>>, Path(token): Path<String>, Query(q): Params) -> ApiResult {
    let token = parse_hash("token", &token)?;
    let depth = limit(&q, "depth", 20, app.state.cfg.max_page_size)?;
    let aggregate = flag(&q, "aggregate")?.unwrap_or(true);
    let ctx = app.ctx();
    let key = format!("book {token} {depth} {aggregate} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| reads::book(c, &ctx, &token, depth, aggregate)).await
}

/// `GET /v1/pairs?token=`: the pairs with listed live pair orders, a JSON array.
///
/// See `indexer::pairs::pairs`.
pub async fn pairs(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let token = opt_hash(&q, "token")?;
    let ctx = app.ctx();
    // Through the response cache like the books, because a pair read checks the custody of every
    // live pair order.
    let key = format!("pairs {token:?} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| crate::indexer::pairs::pairs(c, &ctx, token.as_ref())).await
}

/// `GET /v1/pairs/{base}/{quote}/book?depth=`: the levels of the pair orders and the implied
/// quotes of the KAS route.
///
/// The levels are `direct` for `KobPair` and `entry` for `KobIfdPair`.
pub async fn pair_book(State(app): State<Arc<App>>, Path((base, quote)): Path<(String, String)>, Query(q): Params) -> ApiResult {
    let (base, quote) = pair_path(&base, &quote)?;
    let depth = limit(&q, "depth", 20, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let key = format!("pair-book {base} {quote} {depth} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| crate::indexer::pairs::pair_book(c, &ctx, &base, &quote, depth)).await
}

fn pair_path(base: &str, quote: &str) -> Result<(Hash32, Hash32), ApiError> {
    let base = parse_hash("base", base)?;
    let quote = parse_hash("quote", quote)?;
    if base == quote {
        return Err(ApiError::bad_request("base and quote must be different tokens"));
    }
    Ok((base, quote))
}

/// `GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=`: the pair candles.
///
/// They are derived from the KAS series of the two tokens, never from pair fills. See
/// `indexer::pairs::pair_candles`.
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
    // Cached as the token candles are.
    let key = format!("pair-candles {base} {quote} {interval} {from:?} {to:?} {n}");
    cached(&app, key, move |c| {
        let decimals = (reads::token_decimals(c, &tokens, &base)?, reads::token_decimals(c, &tokens, &quote)?);
        crate::indexer::pairs::pair_candles(c, &base, &quote, decimals, &interval, iv, from, to, n)
    })
    .await
}

/// `GET /v1/pairs/{base}/{quote}/fills?limit=&before=`: the pair-order fills, newest first, and
/// the 24 h pair volume.
///
/// A fill names its `counterparty` (route, netting or inventory) and has `price_source` none.
pub async fn pair_fills(State(app): State<Arc<App>>, Path((base, quote)): Path<(String, String)>, Query(q): Params) -> ApiResult {
    let (base, quote) = pair_path(&base, &quote)?;
    let before = opt_i64(&q, "before")?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app.state.pool.with(move |c| crate::indexer::pairs::pair_fills(c, &ctx, &base, &quote, before, n)).await?;
    json_ok(v)
}

/// `GET /v1/orders/{covenant_id}`: one order.
pub async fn order(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let id = parse_hash("covenant_id", &id)?;
    let ctx = app.ctx();
    // One order is a few index lookups, on the light lane.
    match app.state.pool.with_light(move |c| reads::order(c, &ctx, &id)).await? {
        // The genesis warning of its tokens is on every order view of that token, see
        // `docs/spec/matcher.md` section 1.2a.
        Some(o) => json_ok(o.with_warnings(|t| app.state.tokens.warnings_for(t))),
        None => Err(ApiError::not_found("unknown covenant id")),
    }
}

/// `GET /v1/orders?maker=&token=&status=&limit=&cursor=`: a page of orders.
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
    // Cached by the parsed filter, so junk parameters do not defeat the cache.
    let key = format!("orders {maker:?} {token:?} {status:?} {cursor:?} {n} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    let filter = OrderFilter { maker, token, status };
    let tokens = app.state.tokens.clone();
    cached(&app, key, move |c| {
        let mut v = reads::orders(c, &ctx, &filter, n, cursor)?;
        v.items = v.items.into_iter().map(|o| o.with_warnings(|t| tokens.warnings_for(t))).collect();
        Ok(v)
    })
    .await
}

/// `GET /v1/orders/{covenant_id}/events`: the events of one order.
pub async fn order_events(State(app): State<Arc<App>>, Path(id): Path<String>, Query(q): Params) -> ApiResult {
    let id = parse_hash("covenant_id", &id)?;
    let after = opt_i64(&q, "after")?;
    let n = limit(&q, "limit", 100, app.state.cfg.max_page_size)?;
    let ctx = app.ctx();
    let v = app
        .state
        .pool
        .with_light(move |c| {
            if !reads::order_exists(c, &id)? {
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

/// `GET /v1/fills?token=&side=&limit=&before=`: a page of fills.
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
    let key = format!("fills {token:?} {side:?} {before:?} {n} {:?} {:?}", ctx.node_daa, ctx.now_unix);
    cached(&app, key, move |c| reads::fills(c, &ctx, token.as_ref(), side, before, n)).await
}

/// `GET /v1/strays?maker=&limit=`: the stray token UTXOs, newest first.
///
/// A stray is a token UTXO sent to an order id outside the protocol.
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

/// `GET /v1/token-events?token=&limit=`: the token registry events of one token or of all.
pub async fn token_events(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let token = opt_hash(&q, "token")?;
    let n = limit(&q, "limit", 50, app.state.cfg.max_page_size)?;
    let v = app.state.pool.with(move |c| reads::token_events(c, token.as_ref(), n)).await?;
    json_ok(json!({ "items": v }))
}

/// The 32-byte owner that an `owner=` parameter names.
///
/// It is an x-only public key or a covenant id in hex, or a Kaspa P2PK address.
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

/// `GET /v1/token-utxos`: the token UTXOs of tracked tokens with proven states.
///
/// They are selected by owner, by token or by both. See `docs/ops/executor.md`, Part B 5.1.
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

// Market data, see `docs/ops/executor.md`, Part B 5.3.

// The price basis of the market views is the standard scale of the token. It is the decimals
// from its KCC-23 declaration, else its registry entry, else the default of its program (see
// [`reads::token_decimals`]). Without any of them it is the scale of its first order. Each
// read resolves the scale in its own database connection.

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

/// `GET /v1/candles/{token}?interval=1m|5m|1h|1d&from=&to=&limit=`: OHLCV candles.
///
/// The candles are in ascending order, and empty buckets are omitted.
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

/// `GET /v1/quotes/{token}?interval=&from=&to=&limit=`: the top-of-book history, `[t, bid, ask]`.
///
/// The parameters are those of the candles. See `indexer::quotes`.
pub async fn quotes(State(app): State<Arc<App>>, Path(token): Path<String>, Query(q): Params) -> ApiResult {
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
    let key = format!("quotes {token} {interval} {from:?} {to:?} {n}");
    cached(&app, key, move |c| {
        let decimals = reads::token_decimals(c, &tokens, &token)?;
        crate::indexer::quotes::quotes(c, &token, decimals, &interval, iv, from, to, n)
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

/// `GET /v1/stats?since=<unix ms>`: the activity totals.
///
/// The totals are the orders placed, the traders, the fees paid to fillers and the KAS locked in
/// live orders. The range runs from `since`, rounded down to the minute, to the newest block.
/// Without `since` it is all time.
pub async fn activity(State(app): State<Arc<App>>, Query(q): Params) -> ApiResult {
    let since = match opt_i64(&q, "since")? {
        None => None,
        Some(s) if s >= 0 => Some(s - s % 60_000),
        Some(_) => return Err(ApiError::bad_request("since must be a unix time in milliseconds")),
    };
    let lean = app.state.health.snapshot().lean;
    cached(&app, format!("activity {since:?}"), move |c| {
        let mut v = serde_json::to_value(crate::indexer::activity::activity(c, since)?).unwrap_or(Value::Null);
        // The totals of a lean store cover what it still holds. `history_from` is the time it
        // holds them from, in unix ms (`reads::history_from`). It is null for a full store.
        if let Some(o) = v.as_object_mut() {
            o.insert("history_from".into(), json!(if lean { reads::history_from(c)? } else { None }));
        }
        Ok(v)
    })
    .await
}

/// `GET /v1/depth/{token}?levels=`: the cumulative depth of the listed book.
///
/// Levels are merged by the price per basis.
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

// Token-logo reports.

/// The largest body read for a logo report, in bytes.
///
/// A report is a covenant id, a reason and perhaps an issue number.
pub const LOGO_REPORT_MAX_BYTES: usize = 1024;

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum ClaimedSource {
    Github,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogoReportBody {
    covenant_id: String,
    reason: crate::logo_reports::Reason,
    source: Option<ClaimedSource>,
    #[serde(rename = "ref")]
    reference: Option<u32>,
}

/// `POST /v1/logo-reports`: reports the logo of a token.
///
/// The body is `{covenant_id, reason: "illegal"|"impersonation"|"other", source?: "github",
/// ref?: <issue number>}`. No free text is accepted.
///
/// The answer is `200 {"result":"hidden"|"recorded","covenant_id":..}`. The first report of a
/// token hides its logo at once. A report for a token that the operator restored, inside the
/// cooldown, is only recorded. Reports are rate limited per client and per day
/// (`[logo_reports]`). Beyond the limit the answer is 429 with `Retry-After`.
pub async fn logo_report(
    State(app): State<Arc<App>>,
    axum::Extension(client): axum::Extension<super::ClientIp>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult {
    let logo = app.state.logo.clone().ok_or_else(|| ApiError::not_found("no such route"))?;
    // JSON only: a cross-site form or a `no-cors` fetch (text/plain) cannot send it
    let json = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", "the body must be application/json"));
    }
    // a browser's page must be the app's (`[logo_reports] origins`, else a single `cors_allow_origin`)
    if let Some(origin) = headers.get(axum::http::header::ORIGIN) {
        let allowed: Vec<&str> = match logo.origins() {
            [] => app.state.cfg.cors_allow_origin.as_deref().filter(|o| *o != "*").into_iter().collect(),
            list => list.iter().map(String::as_str).collect(),
        };
        let ok = origin.to_str().is_ok_and(|o| allowed.iter().any(|a| a.trim_end_matches('/').eq_ignore_ascii_case(o)));
        if !ok {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden_origin", "logo reports are not accepted from this origin"));
        }
    }
    let b: LogoReportBody = serde_json::from_slice(&body).map_err(|_| {
        ApiError::bad_request("the body must be JSON {\"covenant_id\": 64 hex, \"reason\": \"illegal\"|\"impersonation\"|\"other\", \"source\"?: \"github\", \"ref\"?: issue number}")
    })?;
    let covenant_id = parse_hash("covenant_id", &b.covenant_id)?;
    if b.reference.is_some() && b.source.is_none() {
        return Err(ApiError::bad_request("ref belongs to source \"github\""));
    }
    if b.reference == Some(0) {
        return Err(ApiError::bad_request("ref must be an issue number"));
    }
    let report = crate::logo_reports::Report {
        covenant_id,
        reason: b.reason,
        source: if b.source.is_some() { crate::logo_reports::Source::Github } else { crate::logo_reports::Source::Web },
        reference: b.reference,
    };
    let ip = client.0;
    let filed = tokio::task::spawn_blocking(move || logo.report(ip, &report))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error"))?;
    match filed {
        Ok(f) => json_ok(json!({"result": f.as_str(), "covenant_id": covenant_id.to_hex()})),
        Err(crate::logo_reports::ReportError::Limited(secs)) => {
            let mut e = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many logo reports; try again later");
            e.retry_after = Some(secs);
            Err(e)
        }
        Err(crate::logo_reports::ReportError::UnknownToken) => {
            Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_token", "no token with that covenant id is indexed"))
        }
        Err(crate::logo_reports::ReportError::Db(e)) => Err(e.into()),
    }
}

/// `GET /v1/logo-blocklist`: the tokens whose logo the web app does not load.
///
/// The body is `{"hidden":[covenant ids],"updated_ms":..}`. It is cached for 5 seconds.
pub async fn logo_blocklist(State(app): State<Arc<App>>) -> ApiResult {
    let logo = app.state.logo.clone().ok_or_else(|| ApiError::not_found("no such route"))?;
    let body = tokio::task::spawn_blocking(move || logo.blocklist_body())
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error"))??;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json")),
            (axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("public, max-age=5")),
        ],
        body,
    )
        .into_response())
}

// Token-logo votes.

/// The largest body read for a logo vote, in bytes.
///
/// A vote is a covenant id, up or down, an address, a day, a nonce and a signature.
pub const LOGO_VOTE_MAX_BYTES: usize = 1024;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogoVoteBody {
    covenant_id: String,
    vote: crate::logo_votes::Vote,
    address: String,
    day: String,
    nonce: String,
    signature: String,
}

/// Whether a browser's `Origin` may write: one of `list`, else the single `cors_allow_origin`.
/// A request without `Origin` (a script) is not a browser's.
fn origin_allowed(headers: &axum::http::HeaderMap, list: &[String], cors: Option<&str>) -> bool {
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else { return true };
    let allowed: Vec<&str> = match list {
        [] => cors.filter(|o| *o != "*").into_iter().collect(),
        list => list.iter().map(String::as_str).collect(),
    };
    origin.to_str().is_ok_and(|o| allowed.iter().any(|a| a.trim_end_matches('/').eq_ignore_ascii_case(o)))
}

/// `POST /v1/logo-votes`: votes up or down on a token's logo with a wallet's signed message.
///
/// The body is `{covenant_id, vote: "up"|"down", address, day: "YYYY-MM-DD", nonce, signature}`.
/// The signature (64 bytes, hex) is the Kaspa personal-message signature of the text
/// [`crate::logo_votes::message`] by the key of `address`; the nonce carries the work
/// `GET /v1/logo-votes` names (`pow_bits`).
///
/// The answer is `200 {"result":"counted"|"changed"|"unchanged","covenant_id",up,down}`. A wallet
/// votes once per `interval_secs` across all logos: earlier, 429 `too_soon` with `Retry-After`.
pub async fn logo_vote(
    State(app): State<Arc<App>>,
    axum::Extension(client): axum::Extension<super::ClientIp>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult {
    use crate::logo_votes::VoteError;
    let votes = app.state.votes.clone().ok_or_else(|| ApiError::not_found("no such route"))?;
    let json_body = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"));
    if !json_body {
        return Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", "the body must be application/json"));
    }
    if !origin_allowed(&headers, votes.origins(), app.state.cfg.cors_allow_origin.as_deref()) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden_origin", "logo votes are not accepted from this origin"));
    }
    let shape = "the body must be JSON {\"covenant_id\": 64 hex, \"vote\": \"up\"|\"down\", \"address\", \"day\": \"YYYY-MM-DD\", \"nonce\": decimal, \"signature\": 128 hex}";
    let b: LogoVoteBody = serde_json::from_slice(&body).map_err(|_| ApiError::bad_request(shape))?;
    let covenant_id = parse_hash("covenant_id", &b.covenant_id)?;
    let nonce_ok = !b.nonce.is_empty()
        && b.nonce.len() <= crate::logo_votes::MAX_NONCE_DIGITS
        && b.nonce.bytes().all(|c| c.is_ascii_digit())
        && b.nonce.parse::<u64>().is_ok();
    let day_ok =
        b.day.len() == 10 && b.day.bytes().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == b'-' } else { c.is_ascii_digit() });
    let signature: Option<[u8; 64]> = crate::hex::decode(&b.signature).ok().and_then(|s| <[u8; 64]>::try_from(s.as_slice()).ok());
    let (true, true, Some(signature)) = (nonce_ok, day_ok, signature) else {
        return Err(ApiError::bad_request(shape));
    };
    if b.address.len() > 100 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "unsupported_address", "not a Kaspa address"));
    }
    let ballot = crate::logo_votes::Ballot { covenant_id, vote: b.vote, address: b.address, day: b.day, nonce: b.nonce, signature };
    let ip = client.0;
    let cast = tokio::task::spawn_blocking(move || votes.vote(ip, &ballot))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error"))?;
    let wait = |code: &'static str, msg: &str, secs: u64| {
        let mut e = ApiError::new(StatusCode::TOO_MANY_REQUESTS, code, msg);
        e.retry_after = Some(secs);
        e
    };
    match cast {
        Ok(a) => json_ok(json!({"result": a.cast.as_str(), "covenant_id": covenant_id.to_hex(), "up": a.up, "down": a.down})),
        Err(VoteError::StaleDay) => {
            Err(ApiError::new(StatusCode::BAD_REQUEST, "stale_day", "the day must be today or yesterday (UTC)"))
        }
        Err(VoteError::BadPow) => {
            Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_pow", "the nonce does not carry the work asked for"))
        }
        Err(VoteError::BadSignature) => {
            Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_signature", "the signature does not verify for this address and message"))
        }
        Err(VoteError::Address(why)) => Err(ApiError::new(StatusCode::BAD_REQUEST, "unsupported_address", why)),
        Err(VoteError::UnknownToken) => {
            Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_token", "no token with that covenant id is indexed"))
        }
        Err(VoteError::NoHistory) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "no_history",
            "this address has no KOB history old enough to vote (an order it placed or a token it held, at least 7 days ago and before the token was created), and has not held this token for 7 days",
        )),
        Err(VoteError::TooSoon(secs)) => Err(wait("too_soon", "one vote per wallet per 24 hours; try again later", secs)),
        Err(VoteError::Limited(secs)) => Err(wait("rate_limited", "too many logo votes; try again later", secs)),
        Err(VoteError::Db(e)) => Err(e.into()),
    }
}

/// `GET /v1/logo-votes`: the vote totals per token and the tokens the rule hides by default.
///
/// The body is `{"logos":[{covenant_id,up,down}],"hidden":[..],"min_voters","hide_at","pow_bits",
/// "interval_secs","updated_ms"}`. It never names a voter. It is cached for 5 seconds.
pub async fn logo_votes(State(app): State<Arc<App>>) -> ApiResult {
    let votes = app.state.votes.clone().ok_or_else(|| ApiError::not_found("no such route"))?;
    let body = tokio::task::spawn_blocking(move || votes.totals_body())
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error"))??;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json")),
            (axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("public, max-age=5")),
        ],
        body,
    )
        .into_response())
}
