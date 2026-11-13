//! The KAS/USD price served at `GET /v1/price/kas-usd`.
//!
//! The route is opt-in with `[api] usd_price`, `--usd-price` or `KOB_USD_PRICE=1`. The web app
//! shows an approximate USD value next to KAS when the viewer asks for it. The browser must not
//! call a price API itself, because the CSP stays closed and the address of a viewer must not go
//! to a third party. The indexer fetches the price and serves it.
//!
//! # Sources
//!
//! The primary is the keyless simple-price API of CoinGecko, an aggregate over exchanges.
//!
//! The fallback is a volume-weighted average of the KAS/USDT spot tickers of several exchanges
//! ([`EXCHANGES`]). Each is one public request that returns the last price and the 24 h quote
//! volume.
//! - The price is `sum(last_i * volume_i) / sum(volume_i)`.
//! - Sources more than [`OUTLIER`] (3 %) from the median of the sources are dropped first.
//! - Fewer than [`MIN_EXCHANGES`] (2) left means no fallback value.
//! - USDT is taken as USD, which is an approximation.
//!
//! Every source is a URL and a JSON path ([`Source`], [`Exchange`]). The calls go out in
//! parallel over the blocking TLS client of the node resolver (`rpc::tls::http_get`). Each has a
//! 4 s connect and read timeout and a 5 s ceiling, so one slow exchange costs nothing but its
//! own vote.
//!
//! # Answer
//!
//! The answer is CoinGecko's when it answered, else the average. When both answered and differ
//! by more than [`DIVERGENCE`] (2 %), the server logs a warning. The warning is never sent to the
//! client. The body is `{"usd": <number>, "at": <unix seconds of the round>, "source":
//! "<source>"}`. `source` is `coingecko`, or `vwap:<exchange>,<exchange>,...` with the exchanges
//! used.
//!
//! A round that yields nothing keeps the last good value with its own `at`, so a client can
//! judge how old it is. Before the first success the route answers 503.
//!
//! # Pace
//!
//! - The upstreams are asked at most once per [`REFRESH`] (60 s) whatever the number of
//!   clients, and only when a client asks. With nobody on USD they are never called.
//!   Concurrent requests share one round.
//! - An upstream that fails or rate-limits (429) is left alone for twice as long after each
//!   failure in a row, up to [`MAX_BACKOFF`] (10 min). After its first success it is asked at
//!   the normal pace again. Each source has its own count, so one rate-limited site does not
//!   slow the others.
//! - The last reading of an exchange counts in the average for five periods at most, so a
//!   source in its back-off does not vote with an old price for long.

use super::{ApiError, App};
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The upstreams are called at most once per this long.
pub const REFRESH: Duration = Duration::from_secs(60);
/// The longest an upstream is left alone after failures in a row.
///
/// The wait doubles from `REFRESH` with each failure.
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// An exchange's last reading is used in the average for at most this many refresh periods.
const READING_PERIODS: u32 = 5;
/// Connect and read timeout of one upstream call.
const CALL_TIMEOUT: Duration = Duration::from_secs(4);
/// What one source may take in all, so a peer that trickles bytes cannot hold a round longer.
const CEILING: Duration = Duration::from_secs(5);
/// A price at or above this is refused as not credible. A wrong number is worse than none.
///
/// KAS has never traded above about 0.2 USD, so 10 USD is 50 times its all-time high: far enough
/// that a real rally passes, near enough that a unit mix-up (cents or a wrong pair, which show up
/// as 100 to 1000 times too large) is refused and the caller keeps the fallback.
const MAX_USD: f64 = 10.0;
/// The largest distance, as a fraction, of the last price of an exchange from the median.
///
/// An exchange further away is left out of the average.
pub const OUTLIER: f64 = 0.03;
/// The fewest exchanges the average needs (after the outliers are dropped).
pub const MIN_EXCHANGES: usize = 2;
/// The primary and the average differ by more than this fraction: the server logs a warning.
pub const DIVERGENCE: f64 = 0.02;

/// The primary source: one URL, the JSON path of the price in USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    /// The `source` field of an answer it gave.
    pub name: &'static str,
    /// The URL of the request.
    pub url: &'static str,
    /// The keys from the JSON root to the price. A key is a field name or an index into an array.
    ///
    /// The value is a number or a decimal string.
    pub path: &'static [&'static str],
}

/// The KAS/USDT spot ticker of one exchange.
///
/// It is one URL, the path of the last price and the path of the 24 h volume in USDT, the quote
/// volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exchange {
    /// The name in `vwap:<names>`: lower case letters and digits.
    pub name: &'static str,
    /// The URL of the request.
    pub url: &'static str,
    /// The path of the last price.
    pub last: &'static [&'static str],
    /// The path of the 24 h quote volume.
    pub volume: &'static [&'static str],
}

/// The primary: CoinGecko.
pub const PRIMARY: Source = Source {
    name: "coingecko",
    url: "https://api.coingecko.com/api/v3/simple/price?ids=kaspa&vs_currencies=usd",
    path: &["kaspa", "usd"],
};

/// The exchanges of the fallback.
///
/// Each lists KAS/USDT spot with the last price and the quote volume in one public request.
pub const EXCHANGES: [Exchange; 4] = [
    Exchange {
        name: "bybit",
        url: "https://api.bybit.com/v5/market/tickers?category=spot&symbol=KASUSDT",
        last: &["result", "list", "0", "lastPrice"],
        volume: &["result", "list", "0", "turnover24h"],
    },
    Exchange {
        name: "kucoin",
        url: "https://api.kucoin.com/api/v1/market/stats?symbol=KAS-USDT",
        last: &["data", "last"],
        volume: &["data", "volValue"],
    },
    Exchange {
        name: "gate",
        url: "https://api.gateio.ws/api/v4/spot/tickers?currency_pair=KAS_USDT",
        last: &["0", "last"],
        volume: &["0", "quote_volume"],
    },
    Exchange {
        name: "mexc",
        url: "https://api.mexc.com/api/v3/ticker/24hr?symbol=KASUSDT",
        last: &["lastPrice"],
        volume: &["quoteVolume"],
    },
];

/// The primary and the exchanges of the fallback.
#[derive(Debug, Clone)]
pub struct SourceSet {
    /// The primary source.
    pub primary: Source,
    /// The exchanges of the fallback.
    pub exchanges: Vec<Exchange>,
}

impl SourceSet {
    /// CoinGecko and [`EXCHANGES`].
    pub fn live() -> Self {
        SourceSet { primary: PRIMARY, exchanges: EXCHANGES.to_vec() }
    }
}

/// What an upstream call returns: the HTTP status and the body, or a transport error.
pub type FetchResult = Result<(u16, String), String>;
/// The upstream call for a URL. It blocks, so it runs on the blocking pool. Tests inject a mock.
pub type Fetch = Arc<dyn Fn(&str) -> FetchResult + Send + Sync>;

/// One good quote.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    /// Price of one KAS in USD.
    pub usd: f64,
    /// Unix seconds of the round that produced it.
    pub at: u64,
    /// `coingecko`, or `vwap:<exchange>,<exchange>,...`.
    pub source: String,
}

/// The pace of the refresh. Tests use short ones.
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    /// The upstreams are asked at most once per this long.
    pub refresh: Duration,
    /// What one call may take in all.
    pub ceiling: Duration,
    /// The longest wait after failures in a row.
    pub max_backoff: Duration,
}

impl Tuning {
    /// [`REFRESH`], [`CEILING`], [`MAX_BACKOFF`].
    pub const LIVE: Tuning = Tuning { refresh: REFRESH, ceiling: CEILING, max_backoff: MAX_BACKOFF };
}

/// How one upstream has been doing: failures in a row and the earliest it is asked again.
#[derive(Debug, Clone, Copy, Default)]
struct Health {
    fails: u32,
    next_at: Option<Instant>,
}

impl Health {
    fn due(&self, now: Instant) -> bool {
        self.next_at.is_none_or(|t| now >= t)
    }
    fn ok(&mut self) {
        *self = Health::default();
    }
    /// Records one more failure and returns the wait.
    ///
    /// The wait is twice as long as after the failure before, starting at `2 * refresh`, and at
    /// most `max_backoff`.
    fn failed(&mut self, now: Instant, t: &Tuning) -> Duration {
        self.fails = self.fails.saturating_add(1);
        let wait = t.refresh.saturating_mul(1u32 << self.fails.min(16)).min(t.max_backoff.max(t.refresh));
        self.next_at = Some(now + wait);
        wait
    }
}

#[derive(Default, Clone)]
struct ExchangeState {
    health: Health,
    /// The last good reading and when it was taken.
    reading: Option<(Reading, Instant)>,
}

#[derive(Default)]
struct Slot {
    good: Option<Quote>,
    /// When the upstreams were last asked (success or not).
    attempted: Option<Instant>,
    primary: Health,
    exchanges: Vec<ExchangeState>,
}

struct Inner {
    fetch: Fetch,
    sources: SourceSet,
    tuning: Tuning,
    state: Mutex<Slot>,
    /// One round at a time. The requests that find one due wait here and then find it done.
    gate: tokio::sync::Mutex<()>,
}

/// The cached price and its refresh.
#[derive(Clone)]
pub struct UsdPrice(Arc<Inner>);

impl UsdPrice {
    /// The live price: CoinGecko and the exchanges over the blocking TLS client.
    pub fn live() -> Self {
        Self::with_fetch(Arc::new(|url| crate::rpc::tls::http_get(url, CALL_TIMEOUT)), SourceSet::live(), Tuning::LIVE)
    }

    /// A price over any fetch, sources and pace. Tests use it.
    pub fn with_fetch(fetch: Fetch, sources: SourceSet, tuning: Tuning) -> Self {
        let slot = Slot { exchanges: vec![ExchangeState::default(); sources.exchanges.len()], ..Slot::default() };
        UsdPrice(Arc::new(Inner { fetch, sources, tuning, state: Mutex::new(slot), gate: tokio::sync::Mutex::new(()) }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn due(&self) -> bool {
        self.lock().attempted.is_none_or(|t| t.elapsed() >= self.0.tuning.refresh)
    }

    /// The last good quote, refreshed first when the upstreams have not been asked for a period.
    ///
    /// `None` before the first success.
    pub async fn get(&self) -> Option<Quote> {
        if self.due() {
            let _one_at_a_time = self.0.gate.lock().await;
            if self.due() {
                // Detached, so that a cancelled request (the guard timeout, a closed connection)
                // neither loses the result nor leaves the attempt marked with nothing stored.
                let this = self.clone();
                let _ = tokio::spawn(async move { this.round().await }).await;
            }
        }
        self.lock().good.clone()
    }

    /// One upstream call on the blocking pool, within the ceiling.
    async fn call(&self, url: &'static str) -> FetchResult {
        let fetch = self.0.fetch.clone();
        let ceiling = self.0.tuning.ceiling;
        match tokio::time::timeout(ceiling, tokio::task::spawn_blocking(move || fetch(url))).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(format!("fetch task: {e}")),
            Err(_) => Err(format!("no answer within {} ms", ceiling.as_millis())),
        }
    }

    /// The call, or `None` when its upstream is still in its back-off.
    async fn call_if(&self, due: bool, url: &'static str) -> Option<FetchResult> {
        if due {
            Some(self.call(url).await)
        } else {
            None
        }
    }

    /// Asks the primary and every exchange that is not in its back-off, all at once.
    ///
    /// It keeps what they say.
    async fn round(&self) {
        let t = self.0.tuning;
        let (primary, exchanges) = (self.0.sources.primary, &self.0.sources.exchanges);
        let now = Instant::now();
        let (p_due, x_due): (bool, Vec<bool>) = {
            let mut g = self.lock();
            g.attempted = Some(now);
            (g.primary.due(now), g.exchanges.iter().map(|e| e.health.due(now)).collect())
        };
        let (p, e) = tokio::join!(
            self.call_if(p_due, primary.url),
            futures_util::future::join_all(exchanges.iter().zip(&x_due).map(|(x, due)| self.call_if(*due, x.url)))
        );
        let now = Instant::now();
        let mut coingecko = None;
        let mut readings = Vec::new();
        {
            let mut g = self.lock();
            if let Some(r) = p {
                match r.and_then(|(s, b)| parse_number(s, &b, primary.path)) {
                    Ok(v) => {
                        g.primary.ok();
                        coingecko = Some(v);
                    }
                    Err(err) => {
                        let wait = g.primary.failed(now, &t);
                        tracing::warn!("usd price: {}: {err} (not asked again for {} s)", primary.name, wait.as_secs());
                    }
                }
            }
            for ((x, r), st) in exchanges.iter().zip(e).zip(g.exchanges.iter_mut()) {
                let Some(r) = r else { continue };
                match r.and_then(|(s, b)| parse_ticker(s, &b, x)) {
                    Ok((last, volume)) => {
                        st.health.ok();
                        st.reading = Some((Reading { name: x.name, last, volume }, now));
                    }
                    Err(err) => {
                        let wait = st.health.failed(now, &t);
                        tracing::debug!("usd price: {}: {err} (not asked again for {} s)", x.name, wait.as_secs());
                    }
                }
            }
            // The average is made of the last good reading of each exchange, while it is not too
            // old.
            let fresh = t.refresh.saturating_mul(READING_PERIODS);
            for st in &g.exchanges {
                if let Some((r, at)) = &st.reading {
                    if now.duration_since(*at) <= fresh {
                        readings.push(r.clone());
                    }
                }
            }
        }
        let average = vwap(&readings);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let quote = match (coingecko, average) {
            (Some(usd), average) => {
                if let Ok(a) = &average {
                    if diverges(usd, a.usd, DIVERGENCE) {
                        tracing::warn!(
                            "usd price: {} says {usd} and the exchanges' average ({}) says {}: they differ by more than {} %",
                            primary.name,
                            a.names.join(","),
                            a.usd,
                            DIVERGENCE * 100.0
                        );
                    }
                }
                Some(Quote { usd, at, source: primary.name.to_string() })
            }
            (None, Ok(a)) => Some(Quote { usd: a.usd, at, source: format!("vwap:{}", a.names.join(",")) }),
            (None, Err(err)) => {
                tracing::debug!("usd price: no exchange average: {err}");
                None
            }
        };
        match quote {
            Some(q) => self.lock().good = Some(q),
            None => {
                let kept = if self.lock().good.is_some() { "serving the last good price" } else { "no price yet" };
                tracing::warn!("usd price: no source gave a price ({kept})");
            }
        }
    }
}

/// The reading of one exchange.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The name of the exchange.
    pub name: &'static str,
    /// Last price (USDT per KAS).
    pub last: f64,
    /// 24 h volume in USDT.
    pub volume: f64,
}

/// The volume-weighted average of the exchanges and the ones it used.
#[derive(Debug, Clone, PartialEq)]
pub struct Vwap {
    /// The average price in USD.
    pub usd: f64,
    /// The exchanges that went into it.
    pub names: Vec<&'static str>,
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// The volume-weighted average `sum(last * volume) / sum(volume)` of the readings.
///
/// The median is taken once, over every reading with a positive finite price and volume. The
/// readings more than [`OUTLIER`] from it are dropped. It is an error when fewer than
/// [`MIN_EXCHANGES`] are left.
pub fn vwap(readings: &[Reading]) -> Result<Vwap, String> {
    let ok: Vec<&Reading> =
        readings.iter().filter(|r| r.last.is_finite() && r.last > 0.0 && r.volume.is_finite() && r.volume > 0.0).collect();
    if ok.len() < MIN_EXCHANGES {
        return Err(format!("{} of the exchanges answered with a usable price and volume (need {MIN_EXCHANGES})", ok.len()));
    }
    let mut lasts: Vec<f64> = ok.iter().map(|r| r.last).collect();
    lasts.sort_by(f64::total_cmp);
    let mid = median(&lasts);
    let kept: Vec<&&Reading> = ok.iter().filter(|r| (r.last - mid).abs() / mid <= OUTLIER).collect();
    if kept.len() < MIN_EXCHANGES {
        return Err(format!(
            "{} of the exchanges are within {} % of the median {mid} (need {MIN_EXCHANGES})",
            kept.len(),
            OUTLIER * 100.0
        ));
    }
    let weight: f64 = kept.iter().map(|r| r.volume).sum();
    let usd = kept.iter().map(|r| r.last * r.volume).sum::<f64>() / weight;
    if !(usd.is_finite() && usd > 0.0 && usd < MAX_USD) {
        return Err(format!("the exchanges' average {usd} is not credible"));
    }
    Ok(Vwap { usd, names: kept.iter().map(|r| r.name).collect() })
}

/// Whether two prices differ by more than `limit`, a fraction of the first.
pub fn diverges(a: f64, b: f64, limit: f64) -> bool {
    (a - b).abs() / a > limit
}

/// The number at `path` of a JSON answer.
///
/// A step of the path is a key, or an index into an array. The number is a JSON number or a
/// decimal string.
fn number_at(v: &serde_json::Value, path: &[&str]) -> Result<f64, String> {
    let mut at = v;
    for k in path {
        let next = match at {
            serde_json::Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => at.get(*k),
        };
        at = next.ok_or_else(|| format!("upstream answer has no {}", path.join(".")))?;
    }
    match at {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) if s.len() <= 32 => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|x| x.is_finite())
    .ok_or_else(|| format!("upstream {} is not a number", path.join(".")))
}

fn json_of(status: u16, body: &str) -> Result<serde_json::Value, String> {
    if status != 200 {
        return Err(format!("upstream answered HTTP {status}"));
    }
    serde_json::from_str(body).map_err(|_| "upstream answer is not JSON".to_string())
}

/// The price at `path` of the primary's answer: positive and below [`MAX_USD`].
pub fn parse_number(status: u16, body: &str, path: &[&str]) -> Result<f64, String> {
    let usd = number_at(&json_of(status, body)?, path)?;
    if usd > 0.0 && usd < MAX_USD {
        Ok(usd)
    } else {
        Err(format!("upstream price {usd} is not credible"))
    }
}

/// The last price and the 24 h quote volume of an exchange's answer.
pub fn parse_ticker(status: u16, body: &str, x: &Exchange) -> Result<(f64, f64), String> {
    let v = json_of(status, body)?;
    Ok((number_at(&v, x.last)?, number_at(&v, x.volume)?))
}

/// Handler of `GET /v1/price/kas-usd`.
pub async fn kas_usd(State(app): State<Arc<App>>) -> Result<Response, ApiError> {
    let Some(price) = app.state.price.as_ref() else { return Err(ApiError::not_found("no such route")) };
    match price.get().await {
        Some(q) => {
            let body = serde_json::json!({"usd": q.usd, "at": q.at, "source": q.source});
            Ok(([(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))], body.to_string()).into_response())
        }
        // No `Retry-After`. The web client polls on its own schedule and shows KAS meanwhile, and
        // a `Retry-After` would make it wait and retry.
        None => Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "price_unavailable", "no KAS price yet")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const P: Source = Source { name: "alpha", url: "https://a.example/p", path: &["kaspa", "usd"] };
    pub(crate) const X: Exchange = Exchange { name: "x", url: "https://x.example/t", last: &["last"], volume: &["vol"] };
    pub(crate) const Y: Exchange =
        Exchange { name: "y", url: "https://y.example/t", last: &["d", "0", "last"], volume: &["d", "0", "vol"] };
    pub(crate) const Z: Exchange = Exchange { name: "z", url: "https://z.example/t", last: &["last"], volume: &["vol"] };

    pub(crate) fn set() -> SourceSet {
        SourceSet { primary: P, exchanges: vec![X, Y, Z] }
    }
    pub(crate) fn p_ok(usd: &str) -> FetchResult {
        Ok((200, format!("{{\"kaspa\":{{\"usd\":{usd}}}}}")))
    }
    /// A ticker answer in the shape of X and Z (strings, as the exchanges send them).
    pub(crate) fn t_ok(last: &str, vol: &str) -> FetchResult {
        Ok((200, format!("{{\"last\":\"{last}\",\"vol\":\"{vol}\"}}")))
    }
    /// The same in the shape of Y (an array, as Bybit and Gate send them).
    pub(crate) fn y_ok(last: &str, vol: &str) -> FetchResult {
        Ok((200, format!("{{\"d\":[{{\"last\":\"{last}\",\"vol\":{vol}}}]}}")))
    }

    type Script = (String, Vec<FetchResult>, usize);

    /// A mock upstream without network.
    ///
    /// Per URL it has a script of answers, where the last repeats, and an optional delay. It
    /// records the calls it got.
    #[derive(Clone, Default)]
    pub(crate) struct Mock {
        scripts: Arc<Mutex<Vec<Script>>>,
        delays: Arc<Mutex<Vec<(String, Duration)>>>,
        pub(crate) log: Arc<Mutex<Vec<String>>>,
        times: Arc<Mutex<Vec<(String, Instant)>>>,
    }
    impl Mock {
        pub(crate) fn on(self, url: &str, script: Vec<FetchResult>) -> Self {
            let mut scripts = self.scripts.lock().unwrap();
            scripts.retain(|(u, ..)| u != url);
            scripts.push((url.to_string(), script, 0));
            drop(scripts);
            self
        }
        pub(crate) fn slow(self, url: &str, d: Duration) -> Self {
            self.delays.lock().unwrap().push((url.to_string(), d));
            self
        }
        pub(crate) fn fetch(&self) -> Fetch {
            let m = self.clone();
            Arc::new(move |url| {
                m.log.lock().unwrap().push(url.to_string());
                m.times.lock().unwrap().push((url.to_string(), Instant::now()));
                let delay = m.delays.lock().unwrap().iter().find(|(u, _)| u == url).map(|(_, d)| *d);
                if let Some(d) = delay {
                    std::thread::sleep(d);
                }
                let mut s = m.scripts.lock().unwrap();
                let (_, script, n) = s.iter_mut().find(|(u, ..)| u == url).ok_or_else(|| format!("no mock for {url}"))?;
                let r = script[(*n).min(script.len() - 1)].clone();
                *n += 1;
                r
            })
        }
        pub(crate) fn calls(&self) -> usize {
            self.log.lock().unwrap().len()
        }
        pub(crate) fn calls_to(&self, url: &str) -> usize {
            self.log.lock().unwrap().iter().filter(|u| *u == url).count()
        }
        /// The gaps between the successive calls to `url`.
        fn gaps_of(&self, url: &str) -> Vec<Duration> {
            let t: Vec<Instant> = self.times.lock().unwrap().iter().filter(|(u, _)| u == url).map(|(_, at)| *at).collect();
            t.windows(2).map(|w| w[1].duration_since(w[0])).collect()
        }
    }

    /// A mock answering all four sources.
    ///
    /// The primary answers `p`, and `None` means it is down. The exchanges answer 0.0300, 0.0302
    /// and 0.0301 with volumes 100, 200 and 300.
    pub(crate) fn full(p: Option<&str>) -> Mock {
        Mock::default()
            .on(P.url, vec![p.map_or(Err("down".into()), p_ok)])
            .on(X.url, vec![t_ok("0.0300", "100")])
            .on(Y.url, vec![y_ok("0.0302", "200")])
            .on(Z.url, vec![t_ok("0.0301", "300")])
    }

    /// A pace of `refresh_ms` with no back-off. The cap is the refresh itself.
    pub(crate) fn flat(refresh_ms: u64) -> Tuning {
        let d = Duration::from_millis(refresh_ms);
        Tuning { refresh: d, ceiling: Duration::from_secs(2), max_backoff: d }
    }

    fn price(m: &Mock, refresh_ms: u64) -> UsdPrice {
        UsdPrice::with_fetch(m.fetch(), set(), flat(refresh_ms))
    }

    fn r(name: &'static str, last: f64, volume: f64) -> Reading {
        Reading { name, last, volume }
    }

    #[test]
    fn the_live_sources_are_https_and_have_distinct_names() {
        let s = SourceSet::live();
        assert_eq!((s.primary.name, s.exchanges.len()), ("coingecko", 4));
        assert_eq!((REFRESH, MAX_BACKOFF), (Duration::from_secs(60), Duration::from_secs(600)), "once a minute, backed off to ten");
        assert_eq!(Tuning::LIVE.refresh, REFRESH);
        let mut names: Vec<&str> = s.exchanges.iter().map(|x| x.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 4);
        assert!(names.iter().all(|n| n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())));
        assert!(s.exchanges.iter().all(|x| x.url.starts_with("https://")) && s.primary.url.starts_with("https://"));
    }

    #[test]
    fn the_live_sources_read_the_answers_the_apis_gave() {
        // Recorded answers, trimmed: a number, strings, and an array in the path.
        assert_eq!(parse_number(200, r#"{"kaspa":{"usd":0.02980916}}"#, PRIMARY.path), Ok(0.02980916));
        assert!(parse_number(200, r#"{"kaspa":{"usd":10.0}}"#, PRIMARY.path).is_err(), "at the ceiling");
        assert!(parse_number(200, r#"{"kaspa":{"usd":29.8}}"#, PRIMARY.path).is_err(), "a price 1000 times too high");
        assert!(vwap(&[r("a", 29.8, 100.0), r("b", 29.9, 100.0)]).is_err(), "the average is held to the ceiling too");
        assert!(parse_number(200, r#"{"kaspa":{"usd":0.5}}"#, PRIMARY.path).is_ok());
        let bybit = r#"{"retCode":0,"result":{"category":"spot","list":[{"symbol":"KASUSDT","lastPrice":"0.02989","turnover24h":"819695.2927411","volume24h":"27857931.29"}]}}"#;
        let kucoin =
            r#"{"code":"200000","data":{"symbol":"KAS-USDT","last":"0.02988","volValue":"773382.37126766","vol":"26072393.273"}}"#;
        let gate = r#"[{"currency_pair":"KAS_USDT","last":"0.02987","base_volume":"18311574.53","quote_volume":"539694.2283946018"}]"#;
        let mexc = r#"{"symbol":"KASUSDT","lastPrice":"0.029842","volume":"8339637.81","quoteVolume":"245837.604441"}"#;
        for (x, body, last, vol) in [
            (0, bybit, 0.02989, 819695.2927411),
            (1, kucoin, 0.02988, 773382.37126766),
            (2, gate, 0.02987, 539694.2283946018),
            (3, mexc, 0.029842, 245837.604441),
        ] {
            assert_eq!(parse_ticker(200, body, &EXCHANGES[x]), Ok((last, vol)), "{}", EXCHANGES[x].name);
        }
    }

    #[test]
    fn answers_that_are_not_a_price_are_refused() {
        let p = &["kaspa", "usd"][..];
        assert_eq!(parse_number(200, r#"{"kaspa":{"usd":" 0.05 "}}"#, p), Ok(0.05));
        for (s, b) in [
            (429, r#"{"kaspa":{"usd":0.05}}"#),
            (200, "<html>"),
            (200, "{}"),
            (200, r#"{"kaspa":{}}"#),
            (200, r#"{"kaspa":{"usd":"cheap"}}"#),
            (200, r#"{"kaspa":{"usd":[0.05]}}"#),
            (200, r#"{"kaspa":{"usd":0}}"#),
            (200, r#"{"kaspa":{"usd":-1}}"#),
            (200, r#"{"kaspa":{"usd":1e9}}"#),
            (200, r#"{"kaspa":{"usd":"1e9"}}"#),
            (200, r#"{"kaspa":{"usd":"NaN"}}"#),
            (200, r#"{"kaspa":{"usd":"inf"}}"#),
            (200, r#"{"kaspa":{"usd":null}}"#),
        ] {
            assert!(parse_number(s, b, p).is_err(), "{s} {b}");
        }
        assert!(parse_ticker(200, r#"{"d":[]}"#, &Y).is_err(), "an empty list");
        assert!(parse_ticker(200, r#"{"d":[{"last":"0.03"}]}"#, &Y).is_err(), "no volume");
        assert!(parse_ticker(500, r#"{"last":"0.03","vol":"1"}"#, &X).is_err());
    }

    #[test]
    fn the_average_weights_each_exchange_by_its_volume() {
        let v = vwap(&[r("a", 0.0300, 100.0), r("b", 0.0306, 300.0)]).unwrap();
        // The average is (0.0300 * 100 + 0.0306 * 300) / 400.
        assert!((v.usd - 0.03045).abs() < 1e-12, "{}", v.usd);
        assert_eq!(v.names, ["a", "b"]);
        // Equal volumes give the plain mean. One huge volume gives nearly its price.
        assert!((vwap(&[r("a", 0.0300, 1.0), r("b", 0.0306, 1.0)]).unwrap().usd - 0.0303).abs() < 1e-12);
        assert!((vwap(&[r("a", 0.0300, 1.0), r("b", 0.0306, 1e9)]).unwrap().usd - 0.0306).abs() < 1e-6);
    }

    #[test]
    fn an_exchange_more_than_three_percent_from_the_median_is_dropped_first() {
        let rs = [r("a", 0.0300, 100.0), r("b", 0.0302, 100.0), r("c", 0.0301, 100.0), r("d", 0.0400, 100_000.0)];
        let v = vwap(&rs).unwrap();
        assert_eq!(v.names, ["a", "b", "c"], "the huge-volume outlier does not drag the average");
        assert!((v.usd - 0.0301).abs() < 1e-12, "{}", v.usd);
        // The median decides. A low outlier is dropped too, and a reading just inside the limit
        // stays.
        let v = vwap(&[r("a", 0.0200, 50.0), r("b", 0.0300, 100.0), r("c", 0.0301, 100.0), r("d", 0.0309, 100.0)]).unwrap();
        assert_eq!(v.names, ["b", "c", "d"], "median 0.03005: 0.0309 is 2.8 % away, 0.0200 is 33 %");
        // Either side of the limit.
        let v = vwap(&[r("a", 0.0300, 1.0), r("b", 0.0300, 1.0), r("c", 0.0308, 1.0)]).unwrap();
        assert_eq!(v.names, ["a", "b", "c"], "2.7 % off");
        let v = vwap(&[r("a", 0.0300, 1.0), r("b", 0.0300, 1.0), r("c", 0.0310, 1.0)]).unwrap();
        assert_eq!(v.names, ["a", "b"], "3.3 % off");
    }

    #[test]
    fn fewer_than_two_usable_exchanges_is_no_value() {
        assert!(vwap(&[]).is_err());
        assert!(vwap(&[r("a", 0.03, 100.0)]).is_err(), "one is not an average");
        assert!(vwap(&[r("a", 0.03, 100.0), r("b", 0.03, 0.0)]).is_err(), "no volume: no vote");
        assert!(vwap(&[r("a", 0.03, 100.0), r("b", f64::NAN, 5.0)]).is_err());
        assert!(vwap(&[r("a", 0.03, 100.0), r("b", -1.0, 5.0)]).is_err());
        // Two far apart: the median is between them and both are over the limit.
        assert!(vwap(&[r("a", 0.0300, 100.0), r("b", 0.0330, 100.0)]).is_err());
        // Three where two outliers on both sides leave one.
        assert!(vwap(&[r("a", 0.0300, 1.0), r("b", 0.0200, 1.0), r("c", 0.0400, 1.0)]).is_err());
    }

    #[test]
    fn the_divergence_check_is_a_fraction_of_the_first_price() {
        assert!(!diverges(0.0300, 0.03059, 0.02));
        assert!(diverges(0.0300, 0.03061, 0.02));
        assert!(diverges(0.0300, 0.0293, 0.02));
    }

    #[tokio::test]
    async fn the_primary_answers_when_it_can_and_the_exchanges_are_asked_too_for_the_comparison() {
        let m = full(Some("0.05"));
        let p = price(&m, 60_000);
        let all = futures_util::future::join_all((0..40).map(|_| {
            let p = p.clone();
            async move { p.get().await }
        }))
        .await;
        assert!(all.iter().all(|q| q.as_ref().is_some_and(|q| q.usd == 0.05 && q.source == "alpha")));
        assert_eq!(m.calls(), 4, "one round over the four sources for forty clients");
        for _ in 0..10 {
            assert!(p.get().await.is_some());
        }
        assert_eq!(m.calls(), 4, "within the period: the cache");
    }

    #[tokio::test]
    async fn the_exchange_average_answers_when_the_primary_failed_and_names_who_was_used() {
        let m = full(None);
        let q = price(&m, 60_000).get().await.unwrap();
        assert_eq!(q.source, "vwap:x,y,z");
        // The average is (0.0300*100 + 0.0302*200 + 0.0301*300) / 600.
        assert!((q.usd - (0.0300 * 100.0 + 0.0302 * 200.0 + 0.0301 * 300.0) / 600.0).abs() < 1e-12, "{}", q.usd);
        // One of them is far off, so it is not in the label.
        let m = full(None).on(Z.url, vec![t_ok("0.0450", "99999")]);
        let q = price(&m, 60_000).get().await.unwrap();
        assert_eq!(q.source, "vwap:x,y");
        assert!((q.usd - (0.0300 * 100.0 + 0.0302 * 200.0) / 300.0).abs() < 1e-12, "{}", q.usd);
    }

    #[tokio::test]
    async fn one_exchange_timing_out_costs_only_its_own_vote() {
        let m = full(None).slow(Y.url, Duration::from_millis(600));
        let p = UsdPrice::with_fetch(
            m.fetch(),
            set(),
            Tuning { refresh: Duration::from_secs(60), ceiling: Duration::from_millis(150), max_backoff: Duration::from_secs(600) },
        );
        let started = Instant::now();
        let q = p.get().await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the round did not wait for the slow exchange: {:?}",
            started.elapsed()
        );
        assert_eq!(q.source, "vwap:x,z");
        // The primary is slow, so the average answers.
        let m = full(Some("0.05")).slow(P.url, Duration::from_millis(600));
        let p = UsdPrice::with_fetch(
            m.fetch(),
            set(),
            Tuning { refresh: Duration::from_secs(60), ceiling: Duration::from_millis(150), max_backoff: Duration::from_secs(600) },
        );
        assert!(p.get().await.unwrap().source.starts_with("vwap:"));
        // Two of the three are slow. One left is no average, and with no primary there is no price.
        let m = full(None).slow(Y.url, Duration::from_millis(600)).slow(Z.url, Duration::from_millis(600));
        let p = UsdPrice::with_fetch(
            m.fetch(),
            set(),
            Tuning { refresh: Duration::from_secs(60), ceiling: Duration::from_millis(150), max_backoff: Duration::from_secs(600) },
        );
        assert_eq!(p.get().await, None);
    }

    #[tokio::test]
    async fn both_answering_and_far_apart_still_serves_the_primary() {
        // The average is 0.0301 and the primary 0.05. The log gets a warning and the client gets
        // the number of the primary.
        let q = price(&full(Some("0.05")), 60_000).get().await.unwrap();
        assert_eq!((q.usd, q.source.as_str()), (0.05, "alpha"));
    }

    #[tokio::test]
    async fn the_upstreams_are_asked_again_after_the_period() {
        let m = Mock::default()
            .on(P.url, vec![p_ok("0.05"), p_ok("0.06")])
            .on(X.url, vec![t_ok("0.05", "1")])
            .on(Y.url, vec![y_ok("0.05", "1")])
            .on(Z.url, vec![t_ok("0.05", "1")]);
        let p = price(&m, 80);
        assert_eq!(p.get().await.unwrap().usd, 0.05);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(p.get().await.unwrap().usd, 0.06);
        assert_eq!(m.calls_to(P.url), 2);
    }

    #[tokio::test]
    async fn a_failed_round_keeps_the_last_good_value_and_its_time_and_recovers() {
        let m = Mock::default()
            .on(P.url, vec![p_ok("0.05"), Err("down".into()), Ok((429, String::new())), p_ok("0.07")])
            .on(X.url, vec![Ok((500, String::new()))])
            .on(Y.url, vec![Err("down".into())])
            .on(Z.url, vec![Ok((200, "{}".into()))]);
        let p = price(&m, 60);
        let first = p.get().await.unwrap();
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_millis(90)).await;
            assert_eq!(p.get().await, Some(first.clone()), "kept with its own `at` and source");
        }
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert_eq!(p.get().await.unwrap().usd, 0.07, "recovered");
    }

    #[tokio::test]
    async fn before_the_first_success_there_is_no_price_and_a_failed_round_is_not_repeated_inside_the_period() {
        let m = Mock::default()
            .on(P.url, vec![Err("down".into()), p_ok("0.05")])
            .on(X.url, vec![Err("down".into())])
            .on(Y.url, vec![Err("down".into())])
            .on(Z.url, vec![Err("down".into())]);
        let p = price(&m, 100);
        assert_eq!(p.get().await, None);
        assert_eq!(p.get().await, None);
        assert_eq!(m.calls(), 4, "one round, then no hammering of a failing upstream");
        tokio::time::sleep(Duration::from_millis(130)).await;
        assert_eq!(p.get().await.unwrap().usd, 0.05);
    }

    /// A pace for the back-off tests, 60 ms doubling to 240 ms.
    const BACKOFF: Tuning =
        Tuning { refresh: Duration::from_millis(60), ceiling: Duration::from_secs(2), max_backoff: Duration::from_millis(240) };

    /// Calls `get` every 10 ms until `done` or `limit`.
    async fn drive(p: &UsdPrice, limit: Duration, done: impl Fn() -> bool) {
        let t = Instant::now();
        while !done() && t.elapsed() < limit {
            let _ = p.get().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_rate_limited_primary_is_left_alone_for_twice_as_long_after_each_failure_up_to_the_cap_and_asked_at_the_normal_pace_after_a_success(
    ) {
        let m = full(None).on(
            P.url,
            vec![
                Ok((429, "slow down".into())),
                Ok((429, String::new())),
                Err("down".into()),
                Ok((429, String::new())),
                p_ok("0.05"),
                p_ok("0.05"),
            ],
        );
        let p = UsdPrice::with_fetch(m.fetch(), set(), BACKOFF);
        drive(&p, Duration::from_secs(5), || m.calls_to(P.url) >= 6).await;
        let g = m.gaps_of(P.url);
        assert_eq!(g.len(), 5, "{g:?}");
        let ms = |d: Duration| d.as_millis();
        // After the 1st failure 2 x 60, after the 2nd 4 x 60 capped at 240, then the cap.
        assert!(ms(g[0]) >= 120, "{g:?}");
        assert!(ms(g[1]) >= 240, "{g:?}");
        assert!(ms(g[2]) >= 240, "{g:?}");
        assert!(ms(g[3]) >= 240, "{g:?}");
        // The cap holds and there is no further doubling. The bound is generous for a loaded
        // machine.
        assert!(ms(g[1]) < 1_000 && ms(g[2]) < 1_000 && ms(g[3]) < 1_000, "{g:?}");
        // The 5th call succeeded, so the next one is a normal period later, not the cap.
        assert!(ms(g[4]) >= 60 && ms(g[4]) < 200, "{g:?}");
        // The exchanges did not fail. They were asked at the normal pace meanwhile, so more often
        // than the primary.
        assert!(m.calls_to(X.url) > m.calls_to(P.url), "x {} p {}", m.calls_to(X.url), m.calls_to(P.url));
        // A price was served all along, first the average of the exchanges, then CoinGecko's.
        let q = p.get().await.unwrap();
        assert!(q.source == "alpha" || q.source.starts_with("vwap:"), "{q:?}");
    }

    #[tokio::test]
    async fn the_last_good_value_keeps_being_served_with_its_own_time_while_every_source_backs_off() {
        let m = Mock::default()
            .on(P.url, vec![p_ok("0.05"), Ok((429, String::new()))])
            .on(X.url, vec![Ok((429, String::new()))])
            .on(Y.url, vec![Ok((429, String::new()))])
            .on(Z.url, vec![Ok((429, String::new()))]);
        let p = UsdPrice::with_fetch(m.fetch(), set(), BACKOFF);
        let first = p.get().await.unwrap();
        assert_eq!((first.usd, first.source.as_str()), (0.05, "alpha"));
        drive(&p, Duration::from_millis(900), || false).await;
        assert_eq!(p.get().await, Some(first), "the same quote, its own time");
        // 900 ms of 60 ms periods would be 15 rounds. With the back-off each source is asked a
        // handful of times.
        assert!(m.calls_to(P.url) <= 6, "{}", m.calls_to(P.url));
        assert!(m.calls_to(X.url) <= 6, "{}", m.calls_to(X.url));
    }

    #[tokio::test]
    async fn an_exchange_that_fails_is_skipped_and_its_old_reading_counts_for_five_periods_only() {
        // The primary is down throughout, so the average is what is served.
        let m = Mock::default()
            .on(P.url, vec![Err("down".into())])
            .on(X.url, vec![t_ok("0.0300", "100"), Ok((429, String::new()))])
            .on(Y.url, vec![y_ok("0.0302", "200")])
            .on(Z.url, vec![t_ok("0.0301", "300")]);
        let tune =
            Tuning { refresh: Duration::from_millis(50), ceiling: Duration::from_secs(2), max_backoff: Duration::from_millis(400) };
        let p = UsdPrice::with_fetch(m.fetch(), set(), tune);
        assert_eq!(p.get().await.unwrap().source, "vwap:x,y,z");
        // Its 429 follows. Its reading from before still counts for a while (5 x 50 ms), and it is
        // not asked again at once.
        drive(&p, Duration::from_millis(100), || false).await;
        assert!(p.get().await.unwrap().source.starts_with("vwap:"));
        drive(&p, Duration::from_millis(900), || false).await;
        let q = p.get().await.unwrap();
        assert_eq!(q.source, "vwap:y,z", "x's reading is too old now and x is in its back-off");
        assert!(m.calls_to(X.url) <= 6 && m.calls_to(Y.url) >= 10, "x {} y {}", m.calls_to(X.url), m.calls_to(Y.url));
    }
}
