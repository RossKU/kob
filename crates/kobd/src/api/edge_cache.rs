//! The `Cache-Control` of the read API's answers, so that a CDN in front of a public indexer (Cloudflare, docs/ops/chart-server.md) absorbs the repeated reads and
//! the origin, a small PC behind a tunnel, serves each distinct one about once per TTL. The policy is by route class and by the question asked:
//!
//! | class | answers | `Cache-Control` |
//! |---|---|---|
//! | live | books, depth, orders (lists, one order, its events), fills, trades, strays, token UTXOs, token events, the pair book and pair fills | `public, max-age=1, s-maxage=1` |
//! | open candles | a candle query whose range reaches the present (no `to`, or a recent one); the 24 h stats of a token | `public, max-age=3, s-maxage=3` (stats: 2) |
//! | lists | the token list, the pair list, the activity totals | `public, max-age=5, s-maxage=5, stale-while-revalidate=5` (totals: 10) |
//! | price | the KAS/USD price (`/v1/price/kas-usd`, opt-in; the indexer asks its upstream at most once a minute) | `public, max-age=60, s-maxage=60` |
//! | declarations | a token's KCC-23 launch proof | `public, max-age=60, s-maxage=60` |
//! | closed candles | a candle query whose `to` is more than an hour ago: no bucket in it can change | `public, max-age=3600, s-maxage=3600` |
//! | never | health, readiness, metrics, the WebSocket, every other method, every answer that is not a success, and a request that carries the bypass parameter | `no-store` |
//!
//! The bypass parameter ([`BYPASS_PARAM`], `_nc=<anything>`) is for a client that has just changed something itself (placed, cancelled, amended an order) and
//! must see it at once: the server ignores it, the CDN keys the cache on the whole query so a fresh value is a miss, and the answer is `no-store` so nothing
//! piles up under such keys. An answer that already carries a `Cache-Control` of its own (the logo blocklist's) keeps it.

use axum::http::{header, HeaderValue, Method, Response, StatusCode};

/// The query parameter that makes an answer uncacheable and a CDN's key new (its value is ignored).
pub const BYPASS_PARAM: &str = "_nc";

/// A query whose `to` is older than this is closed: no bucket it holds can still change (late fills are minutes behind the chain at most).
const CLOSED_AFTER_MS: i64 = 3_600_000;

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

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').filter_map(|kv| kv.split_once('=').or(Some((kv, "")))).find(|(k, _)| *k == key).map(|(_, v)| v)
}

/// The policy of a successful answer to `method path?query`, `now_ms` the wall clock (unix ms).
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
        ["v1", "candles", _] | ["v1", "pairs", _, _, "candles"] => {
            let closed = query_value(query, "to")
                .and_then(|v| v.parse::<i64>().ok())
                .is_some_and(|to| to <= now_ms.saturating_sub(CLOSED_AFTER_MS));
            public(if closed { 3_600 } else { 3 })
        }
        // books, depth, orders, fills, trades, strays, token UTXOs and events, the pair book and pair fills
        ["v1", ..] => public(1),
        _ => Policy::NoStore,
    }
}

/// Set the response's `Cache-Control` unless it has one (or is a protocol switch): a success gets the policy of its route, anything else `no-store`.
pub fn apply<B>(resp: &mut Response<B>, method: &Method, path: &str, query: &str, now_ms: i64) {
    if resp.status() == StatusCode::SWITCHING_PROTOCOLS || resp.headers().contains_key(header::CACHE_CONTROL) {
        return;
    }
    let p = if resp.status() == StatusCode::OK { policy(method, path, query, now_ms) } else { Policy::NoStore };
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
            assert_eq!(get(p, &format!("interval=1m&from=5&to={}", NOW - 3_600_000)), closed);
            assert_eq!(get(p, &format!("to={}&interval=1d", NOW - 86_400_000)), closed);
            assert_eq!(get(p, "to=soon"), open, "a `to` that is not a number is the server's 400, never cached long");
        }
    }

    #[test]
    fn never_cached() {
        for p in ["/v1/health", "/v1/health/ready", "/v1/metrics", "/v1/ws", "/v1/logo-reports"] {
            assert_eq!(get(p, ""), "no-store", "{p}");
        }
        assert_eq!(cc(Method::POST, "/v1/logo-reports", ""), "no-store");
        assert_eq!(cc(Method::POST, "/v1/orders", ""), "no-store");
        assert_eq!(get("/other", ""), "no-store", "outside /v1; an unknown /v1 route is a 404, which apply() never caches");
        // the bypass parameter of a client that just acted
        assert_eq!(get("/v1/books/ab", "_nc=17"), "no-store");
        assert_eq!(get("/v1/orders", "maker=ab&_nc=1"), "no-store");
        assert_eq!(get("/v1/books/ab", "aggregate=true&_nc"), "no-store");
        assert_eq!(get("/v1/books/ab", "xnc=1"), "public, max-age=1, s-maxage=1");
    }

    #[test]
    fn apply_keeps_an_own_header_and_never_caches_an_error() {
        let mut ok = Response::new(());
        apply(&mut ok, &Method::GET, "/v1/books/ab", "", NOW);
        assert_eq!(ok.headers()[header::CACHE_CONTROL], "public, max-age=1, s-maxage=1");
        let mut err = Response::new(());
        *err.status_mut() = StatusCode::TOO_MANY_REQUESTS;
        apply(&mut err, &Method::GET, "/v1/books/ab", "", NOW);
        assert_eq!(err.headers()[header::CACHE_CONTROL], "no-store");
        let mut own = Response::new(());
        own.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=5"));
        apply(&mut own, &Method::GET, "/v1/logo-blocklist", "", NOW);
        assert_eq!(own.headers()[header::CACHE_CONTROL], "public, max-age=5");
        let mut ws = Response::new(());
        *ws.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        apply(&mut ws, &Method::GET, "/v1/ws", "", NOW);
        assert!(ws.headers().get(header::CACHE_CONTROL).is_none());
    }
}
