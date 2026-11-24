//! The `Cache-Control` policy of the answers of the read API.
//!
//! A CDN in front of a public indexer absorbs the repeated reads (`docs/ops/chart-server.md`).
//! The origin, a small PC behind a tunnel, then serves each distinct read about once per TTL.
//! The policy depends on the route class and on the question asked. All values below are
//! `public, max-age=N, s-maxage=N` unless stated otherwise.
//!
//! - Live, N = 1: books, depth, orders (lists, one order, its events), fills, trades, strays,
//!   token UTXOs, token events, the pair book and pair fills.
//! - Open candles, N = 3: a candle query whose range reaches the present, that is, with no `to`
//!   or a recent one.
//! - 24 h stats of a token, N = 2.
//! - Lists, N = 5 with `stale-while-revalidate=5`: the token list and the pair list.
//! - Activity totals, N = 10 with `stale-while-revalidate=10`.
//! - KAS/USD price (`/v1/price/kas-usd`, opt-in), N = 60.
//! - Token declarations (the KCC-23 launch proof of a token), N = 60.
//! - Closed candles, N = 3600: a candle query whose `to` is at least an hour in the past, while
//!   the store is caught up (N = 3 otherwise). No bucket in it can change.
//! - Never, `no-store`: health, readiness, metrics, the WebSocket, every method other than `GET`
//!   and `HEAD`, every answer that is not a success, and a request that carries the bypass
//!   parameter.
//!
//! # Bypass parameter
//!
//! A client that has just changed something itself, such as placing, cancelling or amending an
//! order, must see the change at once. It adds [`BYPASS_PARAM`], `_nc=<anything>`. The server
//! ignores the value. The CDN keys its cache on the whole query, so a fresh value is a miss. The
//! answer is `no-store`, so nothing piles up under such keys.
//!
//! An answer that already carries a `Cache-Control` of its own keeps it. The logo blocklist is
//! one.

use axum::http::{header, HeaderValue, Method, Response, StatusCode};

/// The query parameter that makes an answer uncacheable and the key of a CDN new.
///
/// Its value is ignored.
pub const BYPASS_PARAM: &str = "_nc";

/// A candle query whose `to` is at least this old is closed.
///
/// No bucket in it can still change, because fills reach a caught-up indexer well within this
/// time. A store that is behind gets the short TTL instead ([`apply`]).
const CLOSED_AFTER_MS: i64 = 3_600_000;

/// The TTL of a closed candle window.
const CLOSED_SECS: u32 = 3_600;

/// The `Cache-Control` choice for one answer.
///
/// `NoStore` is `no-store`. `Public` is a public lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    NoStore,
    /// `public, max-age=N, s-maxage=N` and `stale-while-revalidate=M` when `M` is not 0.
    Public {
        secs: u32,
        stale: u32,
    },
}

impl Policy {
    /// The header value of the policy.
    pub fn header_value(&self) -> HeaderValue {
        match self {
            Policy::NoStore => HeaderValue::from_static("no-store"),
            Policy::Public { secs, stale: 0 } => {
                HeaderValue::from_str(&format!("public, max-age={secs}, s-maxage={secs}")).expect("ascii")
            }
            Policy::Public { secs, stale } => {
                HeaderValue::from_str(&format!("public, max-age={secs}, s-maxage={secs}, stale-while-revalidate={stale}"))
                    .expect("ascii")
            }
        }
    }
}

const fn public(secs: u32) -> Policy {
    Policy::Public { secs, stale: 0 }
}

/// A query component as the handler reads it: `+` is a space and `%XX` a byte.
///
/// So `t%6F=` is `to=`, as the handler sees it.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The decoded `(key, value)` pairs of a query, in order.
fn pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    query.split('&').filter(|kv| !kv.is_empty()).map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (decode(k), decode(v))
    })
}

fn query_value(query: &str, key: &str) -> Option<String> {
    pairs(query).find(|(k, _)| k == key).map(|(_, v)| v)
}

fn has_repeated(query: &str, key: &str) -> bool {
    pairs(query).filter(|(k, _)| k == key).count() > 1
}

/// The policy of a successful answer to `method path?query`.
///
/// `now_ms` is the wall clock in unix milliseconds.
pub fn policy(method: &Method, path: &str, query: &str, now_ms: i64) -> Policy {
    if (method != Method::GET && method != Method::HEAD) || query_value(query, BYPASS_PARAM).is_some() {
        return Policy::NoStore;
    }
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match seg.as_slice() {
        ["v1", "health" | "metrics" | "ws" | "logo-reports"] | ["v1", "health", "ready"] => Policy::NoStore,
        ["v1", "tokens"] | ["v1", "pairs"] => Policy::Public { secs: 5, stale: 5 },
        ["v1", "tokens", _, "declaration"] => public(60),
        ["v1", "price", "kas-usd"] => public(60),
        ["v1", "stats"] => Policy::Public { secs: 10, stale: 10 },
        ["v1", "stats", _] => public(2),
        ["v1", "candles" | "quotes", _] | ["v1", "pairs", _, _, "candles"] => {
            // The handler reads the LAST `to` of a repeated key, so the policy never trusts a
            // repeated one: `?to=1&to=<now>` must not cache live candles as closed.
            let closed = !has_repeated(query, "to")
                && query_value(query, "to")
                    .and_then(|v| v.parse::<i64>().ok())
                    // judged on the window the answer runs to: `to` widened up to its bucket end (R7C2b-5); without a
                    // readable interval, the widest (1 d)
                    .map(|to| {
                        let iv = query_value(query, "interval").and_then(|i| crate::indexer::market::interval_ms(&i)).unwrap_or(86_400_000);
                        to.div_euclid(iv).saturating_add(i64::from(to.rem_euclid(iv) != 0)).saturating_mul(iv)
                    })
                    .is_some_and(|to| to <= now_ms.saturating_sub(CLOSED_AFTER_MS));
            public(if closed { CLOSED_SECS } else { 3 })
        }
        // Books, depth, orders, fills, trades, strays, token UTXOs and events, the pair book and
        // pair fills.
        ["v1", ..] => public(1),
        _ => Policy::NoStore,
    }
}

/// Sets the `Cache-Control` of a response, unless it has one or is a protocol switch.
///
/// A success gets the policy of its route. Anything else gets `no-store`. A closed candle window
/// gets the TTL of an open one unless `current()` says the store is caught up: a store that is
/// catching up or halted may not hold every fill of the window yet (C2-L5).
pub fn apply<B>(resp: &mut Response<B>, method: &Method, path: &str, query: &str, now_ms: i64, current: impl FnOnce() -> bool) {
    if resp.status() == StatusCode::SWITCHING_PROTOCOLS || resp.headers().contains_key(header::CACHE_CONTROL) {
        return;
    }
    let p = if resp.status() == StatusCode::OK { policy(method, path, query, now_ms) } else { Policy::NoStore };
    let p = if matches!(p, Policy::Public { secs: CLOSED_SECS, .. }) && !current() { public(3) } else { p };
    resp.headers_mut().insert(header::CACHE_CONTROL, p.header_value());
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000;

    fn cc(method: Method, path: &str, query: &str) -> String {
        policy(&method, path, query, NOW).header_value().to_str().unwrap().to_string()
    }
    fn get(path: &str, query: &str) -> String {
        cc(Method::GET, path, query)
    }

    #[test]
    fn route_classes() {
        let live = "public, max-age=1, s-maxage=1";
        for p in [
            "/v1/books/ab",
            "/v1/depth/ab",
            "/v1/orders",
            "/v1/orders/ab",
            "/v1/orders/ab/events",
            "/v1/fills",
            "/v1/trades/ab",
            "/v1/strays",
            "/v1/token-utxos",
            "/v1/token-events",
            "/v1/pairs/ab/cd/book",
            "/v1/pairs/ab/cd/fills",
            "/v1/pairs/ab/cd/orders",
        ] {
            assert_eq!(get(p, ""), live, "{p}");
        }
        assert_eq!(get("/v1/orders", "maker=ab&status=open"), live, "owner lists are public chain data");
        assert_eq!(get("/v1/tokens", ""), "public, max-age=5, s-maxage=5, stale-while-revalidate=5");
        assert_eq!(get("/v1/pairs", "token=ab"), "public, max-age=5, s-maxage=5, stale-while-revalidate=5");
        assert_eq!(get("/v1/stats", "since=5"), "public, max-age=10, s-maxage=10, stale-while-revalidate=10");
        assert_eq!(get("/v1/stats/ab", ""), "public, max-age=2, s-maxage=2");
        assert_eq!(get("/v1/tokens/ab/declaration", ""), "public, max-age=60, s-maxage=60");
        assert_eq!(get("/v1/price/kas-usd", ""), "public, max-age=60, s-maxage=60");
    }

    #[test]
    fn candles_are_open_unless_their_range_ended_long_ago() {
        let open = "public, max-age=3, s-maxage=3";
        let closed = "public, max-age=3600, s-maxage=3600";
        for p in ["/v1/candles/ab", "/v1/pairs/ab/cd/candles"] {
            assert_eq!(get(p, "interval=5m"), open, "no `to`: reaches now");
            assert_eq!(
                get(p, &format!("interval=1m&from=5&to={}", NOW - 60_000)),
                open,
                "ended a minute ago: the newest bucket may change"
            );
            assert_eq!(get(p, &format!("interval=1m&to={}", NOW + 60_000)), open);
            assert_eq!(get(p, &format!("interval=1m&from=5&to={}", NOW - 3_660_000)), closed);
            assert_eq!(get(p, &format!("to={}&interval=1d", NOW - 86_400_000)), closed);
            // R7C2b-5: `to` an hour old inside the current day: the answer runs to the end of the open day
            assert_eq!(get(p, &format!("interval=1d&to={}", NOW - 3_660_000)), open, "the snapped window reaches now");
            assert_eq!(get(p, "to=soon"), open, "a `to` that is not a number is the server's 400, never cached long");
            let (old, now) = (NOW - 86_400_000, NOW);
            assert_eq!(get(p, &format!("to={old}&to={now}")), open, "a repeated `to`: the handler reads the last one");
            assert_eq!(get(p, &format!("to={now}&to={old}")), open, "a repeated `to` is never cached long");
            // The handler decodes the keys, so `t%6F` is a second `to`.
            assert_eq!(get(p, &format!("to={old}&t%6F={now}")), open, "an encoded repeated `to`");
            assert_eq!(get(p, &format!("%74o={old}")), closed, "an encoded `to` alone is read as `to`");
            assert_eq!(get(p, "to=1%2B2"), open, "a value that decodes to no number");
        }
        assert_eq!(get("/v1/books/ab", "%5Fnc=1"), "no-store", "an encoded bypass parameter");
    }

    #[test]
    fn never_cached() {
        for p in ["/v1/health", "/v1/health/ready", "/v1/metrics", "/v1/ws", "/v1/logo-reports"] {
            assert_eq!(get(p, ""), "no-store", "{p}");
        }
        assert_eq!(cc(Method::POST, "/v1/logo-reports", ""), "no-store");
        assert_eq!(cc(Method::POST, "/v1/orders", ""), "no-store");
        assert_eq!(get("/other", ""), "no-store", "outside /v1; an unknown /v1 route is a 404, which apply() never caches");
        // The bypass parameter of a client that just acted.
        assert_eq!(get("/v1/books/ab", "_nc=17"), "no-store");
        assert_eq!(get("/v1/orders", "maker=ab&_nc=1"), "no-store");
        assert_eq!(get("/v1/books/ab", "aggregate=true&_nc"), "no-store");
        assert_eq!(get("/v1/books/ab", "xnc=1"), "public, max-age=1, s-maxage=1");
    }

    #[test]
    fn apply_keeps_an_own_header_and_never_caches_an_error() {
        let mut ok = Response::new(());
        apply(&mut ok, &Method::GET, "/v1/books/ab", "", NOW, || true);
        assert_eq!(ok.headers()[header::CACHE_CONTROL], "public, max-age=1, s-maxage=1");
        let mut err = Response::new(());
        *err.status_mut() = StatusCode::TOO_MANY_REQUESTS;
        apply(&mut err, &Method::GET, "/v1/books/ab", "", NOW, || true);
        assert_eq!(err.headers()[header::CACHE_CONTROL], "no-store");
        let mut own = Response::new(());
        own.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=5"));
        apply(&mut own, &Method::GET, "/v1/logo-blocklist", "", NOW, || true);
        assert_eq!(own.headers()[header::CACHE_CONTROL], "public, max-age=5");
        let mut ws = Response::new(());
        *ws.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        apply(&mut ws, &Method::GET, "/v1/ws", "", NOW, || true);
        assert!(ws.headers().get(header::CACHE_CONTROL).is_none());
    }

    /// C2-L5: a closed candle window is kept for an hour only while the store is caught up.
    #[test]
    fn closed_candles_are_short_lived_while_the_store_is_behind() {
        let q = format!("interval=1m&to={}", NOW - 2 * CLOSED_AFTER_MS);
        let cc = |current: bool| {
            let mut r = Response::new(());
            apply(&mut r, &Method::GET, "/v1/candles/ab", &q, NOW, || current);
            r.headers()[header::CACHE_CONTROL].to_str().unwrap().to_string()
        };
        assert_eq!(cc(true), "public, max-age=3600, s-maxage=3600");
        assert_eq!(cc(false), "public, max-age=3, s-maxage=3");
    }
}
