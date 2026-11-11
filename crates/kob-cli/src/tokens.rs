//! `kob tokens [SEARCH] [--all] [--json]`: the tokens with a KOB market (the indexer's `GET /v1/tokens`, `docs/api-reference.md` 3.4), and
//! with `--all` every token kascov.io lists for the network, to find tokens that were never traded on KOB. Read-only: no key, no
//! node. kascov is a discovery list only: its ticker and decimals are the token's own claims, and the one fact taken from it
//! besides is the program hash, which says whether KOB knows the program (`kob registry programs`).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kob_protocol::json::to_hex;
use serde_json::{json, Value};

use crate::trade::data;
use crate::trade::http;
use crate::trade::opts::{parse, Common, COMMON_HELP};

/// Help of `kob tokens`.
pub const USAGE: &str = "\
kob tokens: the tokens with a KOB market, and with --all every token kascov.io lists (read-only; no key, no node)

USAGE
  kob tokens [SEARCH] [--all] [--refresh] [--json]

  SEARCH       a ticker prefix (case-insensitive) or a covenant-id prefix (hex).
  Default      the indexer's tokens (GET /v1/tokens): every token an order was placed for, or a registry lists. Columns:
               TICKER (the registry's, else the token's verified KCC-23 declaration, else blank), COVENANT ID (full: tickers
               collide, the id is the identity), DEC (decimals), PROGRAM (the token program and its slots), ASKS and BIDS
               (live orders), WARN (the indexer's factual `warnings` and `powers`, such as live_mint_authority or freeze;
               `genesis_unverified` is left out: it only says the registry has no genesis check for the token).
  --all        also the tokens kascov.io lists for the network (https://kascov.io/data/<network>/tokens.json): ticker
               (claimed_ticker) and decimals (claimed_decimals) are the token's claims; PROGRAM is the KOB program its hash
               names, `unsupported` when KOB has no such program and `unidentified` when kascov gives no program hash; ASKS
               and BIDS are `-`. A token the indexer lists is shown once, with the indexer's facts. The list is fetched only
               when you run the command, and the local copy (in $KOB_CACHE_DIR or the per-user cache directory) is reused only
               for the server's own `Cache-Control: max-age` (30 s when it names none); after that one request, conditional
               (`If-None-Match` / `If-Modified-Since`) when the server gave an ETag or Last-Modified, so an unchanged list
               costs a 304. Nothing is ever fetched in the background.
  --refresh    fetch the kascov.io list now, whatever the local copy says (implies --all).
  --json       one JSON document: {network, indexer, tokens: [{ticker, covenant_id, decimals, program, open_asks, open_bids,
               warnings, source}], kascov}.

KOB makes no judgement of a token: nothing here says a token is good, verified or official.
";

/// Where kascov publishes its token list (`$KOB_KASCOV_URL` replaces the base, for a mirror and for tests).
pub const KASCOV_BASE: &str = "https://kascov.io";
/// How long a cached list is reused when the server names no `max-age`.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30);
/// Declarations asked for tokens without a registry ticker (one request each).
const DECLARATION_BUDGET: usize = 40;
/// Timeout of one request.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// One row of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Ticker, empty when nothing names one.
    pub ticker: String,
    /// Covenant id (64 hex).
    pub cov: String,
    /// Decimals, when known.
    pub decimals: Option<u32>,
    /// The token program as the row's source names it.
    pub program: String,
    /// Live asks (`None`: a token only kascov lists).
    pub asks: Option<u64>,
    /// Live bids.
    pub bids: Option<u64>,
    /// The indexer's `warnings` and `powers`.
    pub warn: Vec<String>,
    /// `indexer` or `kascov`.
    pub source: &'static str,
}

/// `kob-protocol`'s label of a program: its registry id and slots (`kcc20-ref-public-mint 3/3`).
pub fn program_label(registry_id: &str) -> String {
    match kob_protocol::profile::by_registry_id(registry_id) {
        Some(p) => format!("{} {}/{}", p.registry_id, p.slots.inputs, p.slots.outputs),
        None => registry_id.to_string(),
    }
}

/// The program a kascov token runs, by the hashes kascov gives (`standard.view_template_hash`, else `standard.template_hash`):
/// the KOB program's label (with its review status when not `reviewed`), `unsupported` for a hash KOB has no program for,
/// `unidentified` when kascov gives none.
pub fn kascov_program(token: &Value) -> String {
    let std = token.get("standard").filter(|s| s.is_object());
    let Some(std) = std else { return "unidentified".into() };
    let hashes: Vec<[u8; 32]> = ["view_template_hash", "template_hash"]
        .iter()
        .filter_map(|k| std.get(*k).and_then(Value::as_str))
        .filter_map(kob_protocol::registry::parse_hex32)
        .collect();
    if hashes.is_empty() {
        return "unidentified".into();
    }
    for h in &hashes {
        if let Some(p) = kob_protocol::profile::by_hash(h) {
            let label = format!("{} {}/{}", p.registry_id, p.slots.inputs, p.slots.outputs);
            return match p.review.status {
                kob_protocol::profile::ProgramReview::Reviewed => label,
                kob_protocol::profile::ProgramReview::PendingReview => format!("{label} (pending-review)"),
                kob_protocol::profile::ProgramReview::TestOnly => format!("{label} (test-only)"),
            };
        }
    }
    "unsupported".into()
}

/// The rows of `GET /v1/tokens`.
pub fn indexer_rows(v: &Value) -> Result<Vec<Row>, String> {
    let items = v.get("tokens").and_then(Value::as_array).ok_or("indexer /v1/tokens: no `tokens` array")?;
    let mut rows = vec![];
    for t in items {
        let Some(cov) = t.get("covenant_id").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32) else { continue };
        let strings = |k: &str| -> Vec<String> {
            t.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        let program = match (t.get("template_id").and_then(Value::as_str), t.get("program_ambiguous").and_then(Value::as_bool)) {
            (Some(id), _) => program_label(id),
            (None, Some(true)) => "ambiguous".into(),
            _ => "-".into(),
        };
        rows.push(Row {
            ticker: t.get("ticker").and_then(Value::as_str).unwrap_or("").to_string(),
            cov: to_hex(&cov),
            decimals: t.get("decimals").and_then(Value::as_u64).map(|d| d as u32),
            program,
            asks: t.get("open_asks").and_then(Value::as_u64),
            bids: t.get("open_bids").and_then(Value::as_u64),
            warn: strings("warnings").into_iter().filter(|w| w != "genesis_unverified").chain(strings("powers")).collect(),
            source: "indexer",
        });
    }
    Ok(rows)
}

/// The rows of kascov's list that the indexer's do not already show.
pub fn kascov_rows(list: &Value, have: &std::collections::BTreeSet<String>) -> Result<Vec<Row>, String> {
    let items = list.get("tokens").and_then(Value::as_array).ok_or("kascov: no `tokens` array")?;
    Ok(items
        .iter()
        .filter_map(|t| {
            let cov =
                t.get("covenant_id").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32).map(|c| to_hex(&c))?;
            if have.contains(&cov) {
                return None;
            }
            Some(Row {
                ticker: t.get("claimed_ticker").and_then(Value::as_str).unwrap_or("").to_string(),
                cov,
                decimals: t.get("claimed_decimals").and_then(Value::as_u64).map(|d| d as u32),
                program: kascov_program(t),
                asks: None,
                bids: None,
                warn: vec![],
                source: "kascov",
            })
        })
        .collect())
}

/// Whether `row` matches a search: a case-insensitive ticker prefix or a covenant-id prefix.
pub fn matches(row: &Row, search: &str) -> bool {
    let s = search.trim().to_ascii_lowercase();
    s.is_empty() || row.ticker.to_ascii_lowercase().starts_with(&s) || row.cov.starts_with(&s)
}

/// The table: a header and one line per row, columns aligned, the covenant id whole.
pub fn table(rows: &[Row]) -> Vec<String> {
    let cell = |r: &Row| {
        [
            if r.ticker.is_empty() { String::new() } else { r.ticker.clone() },
            r.cov.clone(),
            r.decimals.map_or("-".into(), |d| d.to_string()),
            r.program.clone(),
            r.asks.map_or("-".into(), |n| n.to_string()),
            r.bids.map_or("-".into(), |n| n.to_string()),
            r.warn.join(","),
        ]
    };
    let head = ["TICKER", "COVENANT ID", "DEC", "PROGRAM", "ASKS", "BIDS", "WARN"].map(String::from);
    let body: Vec<[String; 7]> = rows.iter().map(cell).collect();
    let show_warn = body.iter().any(|r| !r[6].is_empty());
    let cols = if show_warn { 7 } else { 6 };
    let widths: Vec<usize> =
        (0..cols).map(|i| body.iter().map(|r| r[i].chars().count()).chain([head[i].len()]).max().unwrap_or(0)).collect();
    let line = |r: &[String; 7]| -> String {
        let mut out = String::new();
        for i in 0..cols {
            let c = &r[i];
            // numbers right-aligned, the rest left-aligned
            if matches!(i, 2 | 4 | 5) {
                out.push_str(&format!("{c:>w$}", w = widths[i]));
            } else {
                out.push_str(&format!("{c:<w$}", w = widths[i]));
            }
            if i + 1 < cols {
                out.push_str("  ");
            }
        }
        out.trim_end().to_string()
    };
    std::iter::once(line(&head)).chain(body.iter().map(line)).collect()
}

/// The per-user cache directory of `kob` (`$KOB_CACHE_DIR`, else `%LOCALAPPDATA%\kob\cache`, `$XDG_CACHE_HOME/kob`,
/// `~/.cache/kob`).
pub fn cache_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(d) = env("KOB_CACHE_DIR") {
        return Some(d);
    }
    if cfg!(windows) {
        return env("LOCALAPPDATA").map(|d| d.join("kob").join("cache"));
    }
    env("XDG_CACHE_HOME").map(|d| d.join("kob")).or_else(|| env("HOME").map(|h| h.join(".cache").join("kob")))
}

/// Where the list was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The local copy, within the server's `max-age`: no request.
    Cache,
    /// A request just made.
    Fetched,
    /// A conditional request just made: the server said the copy is current (304).
    Revalidated,
    /// The local copy, past its `max-age`: the request failed.
    StaleCache,
}

/// What the server gave to ask again with (`If-None-Match`, `If-Modified-Since`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    /// `ETag`.
    pub etag: Option<String>,
    /// `Last-Modified`.
    pub last_modified: Option<String>,
}

/// An answer of the server.
#[derive(Debug, Clone)]
pub enum Fetched {
    /// 200: the list, how long it may be reused, and the validators.
    Body { body: Value, max_age: Duration, validators: Validators },
    /// 304: the local copy is current; it may be reused for `max_age` from now.
    NotModified { max_age: Duration },
}

/// The reuse time of a `Cache-Control` header: its `max-age` ([`DEFAULT_MAX_AGE`] when it has none; none at all for
/// `no-store` / `no-cache`). `s-maxage` is for shared caches and is not read.
pub fn max_age_of(cache_control: Option<&str>) -> Duration {
    let mut age = None;
    for d in cache_control.unwrap_or("").split(',').map(|d| d.trim().to_ascii_lowercase()) {
        if d == "no-store" || d == "no-cache" {
            return Duration::ZERO;
        }
        if let Some(n) = d.strip_prefix("max-age=") {
            age = n.trim_matches('"').parse::<u64>().ok().map(Duration::from_secs);
        }
    }
    age.unwrap_or(DEFAULT_MAX_AGE)
}

fn meta_json(now: u64, max_age: Duration, v: &Validators) -> Value {
    json!({"fetched_at": now, "max_age": max_age.as_secs(), "etag": v.etag, "last_modified": v.last_modified})
}

/// kascov's list of `network` through the cache in `dir` (`kascov-<network>-tokens.json` and its `.meta.json`). The copy is
/// reused while it is younger than the server's `max-age` at the time it was fetched (`force`: never); past it, one
/// request (`fetch`, given the validators of the copy), whose 304 renews the copy and whose 200 replaces it. When the request
/// fails and a copy exists, that one is returned (the caller says so). `now` is unix seconds. Nothing runs in the background.
pub fn cached_list(
    dir: Option<&Path>,
    network: &str,
    now: u64,
    force: bool,
    fetch: impl FnOnce(&Validators) -> Result<Fetched, String>,
) -> Result<(Value, Origin), String> {
    let file = dir.map(|d| d.join(format!("kascov-{network}-tokens.json")));
    let meta_file = file.as_ref().map(|f| f.with_extension("meta.json"));
    let read = |p: &PathBuf| std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let cached: Option<(Value, Value)> =
        file.as_ref().zip(meta_file.as_ref()).and_then(|(f, m)| Some((read(f)?, read(m).unwrap_or(Value::Null))));
    let validators = cached
        .as_ref()
        .filter(|_| !force)
        .map(|(_, m)| Validators {
            etag: m["etag"].as_str().map(str::to_owned),
            last_modified: m["last_modified"].as_str().map(str::to_owned),
        })
        .unwrap_or_default();
    if let (false, Some((v, m))) = (force, &cached) {
        let (at, age) = (m["fetched_at"].as_u64(), m["max_age"].as_u64());
        if at.zip(age).is_some_and(|(at, age)| now.saturating_sub(at) < age) {
            return Ok((v.clone(), Origin::Cache));
        }
    }
    // a cache that cannot be written is not an error
    let store = |v: &Value, max_age: Duration, val: &Validators| {
        let (Some(f), Some(m)) = (&file, &meta_file) else { return };
        let _ = f.parent().map(std::fs::create_dir_all);
        let put = |p: &PathBuf, text: String| {
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, p)).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        };
        put(f, v.to_string());
        put(m, meta_json(now, max_age, val).to_string());
    };
    match fetch(&validators) {
        Ok(Fetched::Body { body, max_age, validators }) => {
            store(&body, max_age, &validators);
            Ok((body, Origin::Fetched))
        }
        Ok(Fetched::NotModified { max_age }) => match cached {
            Some((v, _)) => {
                store(&v, max_age, &validators);
                Ok((v, Origin::Revalidated))
            }
            None => Err("the server answered 304 (not modified) to a request that was not conditional".into()),
        },
        Err(e) => match cached {
            Some((v, _)) => Ok((v, Origin::StaleCache)),
            None => Err(e),
        },
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Run `kob tokens`.
pub fn run(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[USAGE, COMMON_HELP], false) {
        return code;
    }
    let bail = |e: String, code: i32| {
        eprintln!("error: {e}");
        code
    };
    let p = match parse(args, &[], &["--all", "--refresh"]) {
        Ok(p) => p,
        Err(e) => return bail(e, 2),
    };
    let c = match Common::from(&p) {
        Ok(c) => c,
        Err(e) => return bail(e, 2),
    };
    if p.pos.len() > 1 {
        return bail("give at most one search (a ticker or covenant id prefix)".into(), 2);
    }
    let base = std::env::var("KOB_KASCOV_URL").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| KASCOV_BASE.to_string());
    let refresh = p.has("--refresh");
    match view(&c, p.pos.first().map(String::as_str).unwrap_or(""), p.has("--all") || refresh, refresh, &base, cache_dir().as_deref())
    {
        Ok((doc, lines)) => {
            if c.json {
                println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
            } else {
                for l in lines {
                    println!("{l}");
                }
            }
            0
        }
        Err(e) => bail(e, 1),
    }
}

/// The view: its JSON document and its text lines.
pub fn view(
    c: &Common,
    search: &str,
    all: bool,
    refresh: bool,
    kascov_base: &str,
    cache: Option<&Path>,
) -> Result<(Value, Vec<String>), String> {
    let indexer = c.need_indexer()?;
    let listed = http::get_ok(indexer, "/v1/tokens", data::INDEXER_TIMEOUT)?;
    let mut rows = indexer_rows(&listed)?;
    // a token without a registry ticker: the symbol of its verified KCC-23 declaration, when the indexer holds one (one request
    // each, at most DECLARATION_BUDGET; the rest stay blank)
    let (reg, _) = data::token_lists(c.registry.as_deref(), &c.network, None)?;
    let held: std::collections::BTreeSet<String> = listed
        .get("tokens")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| t.get("declaration").and_then(Value::as_bool) == Some(true))
        .filter_map(|t| t.get("covenant_id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut asked = 0;
    for r in rows.iter_mut().filter(|r| r.ticker.is_empty()) {
        let cov = kob_protocol::registry::parse_hex32(&r.cov).unwrap_or_default();
        if let Some(f) = reg.iter().find(|t| t.cov == cov) {
            r.ticker = f.ticker.clone();
        }
        if r.ticker.is_empty() && held.contains(&r.cov) && asked < DECLARATION_BUDGET {
            asked += 1;
            if let Ok(Some(d)) = data::declaration(indexer, &cov) {
                r.ticker = d.symbol.unwrap_or_default();
                if r.decimals.is_none() {
                    r.decimals = d.decimals;
                }
            }
        }
    }
    let mut note = None;
    let mut kascov_doc = Value::Null;
    if all {
        let have: std::collections::BTreeSet<String> = rows.iter().map(|r| r.cov.clone()).collect();
        let url = format!("{}/data/{}/tokens.json", kascov_base.trim_end_matches('/'), c.network);
        let (base, path) = url_base(&url);
        let (list, origin) = cached_list(cache, &c.network, now_secs(), refresh, |v| {
            let mut headers: Vec<(&str, &str)> = vec![];
            if let Some(e) = &v.etag {
                headers.push(("If-None-Match", e));
            }
            if let Some(l) = &v.last_modified {
                headers.push(("If-Modified-Since", l));
            }
            let r = http::get_with(&base, &path, &headers, FETCH_TIMEOUT).map_err(|e| format!("kascov list: {e}"))?;
            let max_age = max_age_of(r.header("cache-control"));
            match r.status {
                200 => Ok(Fetched::Body {
                    max_age,
                    validators: Validators {
                        etag: r.header("etag").map(str::to_owned),
                        last_modified: r.header("last-modified").map(str::to_owned),
                    },
                    body: r.body,
                }),
                304 => Ok(Fetched::NotModified { max_age }),
                s => Err(format!("kascov list {}: HTTP {s}", http::shown(&url))),
            }
        })
        .map_err(|e| {
            format!("the kascov.io list is not reachable and none is cached ({e}): without --all this works from the indexer alone")
        })?;
        if origin == Origin::StaleCache {
            note = Some(format!(
                "note: kascov.io is not reachable; showing the list cached earlier (cache: {})",
                cache.map_or("-".into(), |d| d.display().to_string())
            ));
        }
        rows.extend(kascov_rows(&list, &have)?);
        kascov_doc = json!({"url": http::shown(&url), "origin": match origin {
            Origin::Cache => "cache",
            Origin::Fetched => "fetched",
            Origin::Revalidated => "revalidated",
            Origin::StaleCache => "stale-cache",
        }});
    }
    let shown: Vec<Row> = rows.into_iter().filter(|r| matches(r, search)).collect();
    // a full covenant id that is in no list: said so (it may still be a token; only the lists do not have it)
    let missing = (kob_protocol::registry::parse_hex32(search.trim()).is_some() && shown.is_empty()).then(|| {
        if all {
            format!(
                "{} is in neither the indexer's list nor kascov.io's: kascov.io may not have indexed it yet. KOB's indexer and the node can still read it: `kob book {0}`, `kob place ... --token {0}`",
                search.trim()
            )
        } else {
            format!(
                "{} is not in the indexer's list (no order of it was placed there, and no registry lists it): `kob tokens {0} --all` asks kascov.io; kascov.io may not have indexed it yet, and KOB's indexer and the node can still read it with `kob book {0}`",
                search.trim()
            )
        }
    });
    let doc = json!({
        "network": c.network,
        "indexer": http::shown(indexer),
        "search": search,
        "tokens": shown.iter().map(|r| json!({
            "ticker": r.ticker, "covenant_id": r.cov, "decimals": r.decimals, "program": r.program,
            "open_asks": r.asks, "open_bids": r.bids, "warnings": r.warn, "source": r.source,
        })).collect::<Vec<_>>(),
        "kascov": kascov_doc,
        "note": missing,
    });
    let mut lines = vec![format!(
        "{} token(s) on {}{} (indexer {}{})",
        shown.len(),
        c.network,
        if search.is_empty() { String::new() } else { format!(" matching `{search}`") },
        http::shown(indexer),
        if all { ", plus the kascov.io list" } else { "" }
    )];
    if let Some(n) = note {
        lines.push(n);
    }
    lines.extend(table(&shown));
    if let Some(m) = missing_note(&doc) {
        lines.push(m);
    }
    Ok((doc, lines))
}

/// Split a URL into the base (`scheme://host[:port]`) and the path the HTTP client takes separately.
fn url_base(url: &str) -> (String, String) {
    let rest_at = url.find("://").map_or(0, |i| i + 3);
    match url[rest_at..].find('/') {
        Some(i) => (url[..rest_at + i].to_string(), url[rest_at + i..].to_string()),
        None => (url.to_string(), "/".to_string()),
    }
}

#[cfg(test)]
mod tests;

fn missing_note(doc: &Value) -> Option<String> {
    doc["note"].as_str().map(|m| format!("note: {m}"))
}
