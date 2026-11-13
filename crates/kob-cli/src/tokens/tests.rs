use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::trade::market::{self, TokenFacts};
use crate::trade::opts::{parse, Common};

fn hash_of(registry_id: &str) -> String {
    kob_protocol::profile::by_registry_id(registry_id).unwrap().template_hash.clone()
}

fn cov(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn row(ticker: &str, c: &str, decimals: Option<u32>, program: &str, asks: Option<u64>, bids: Option<u64>, warn: &[&str]) -> Row {
    Row {
        ticker: ticker.into(),
        cov: c.into(),
        decimals,
        program: program.into(),
        asks,
        bids,
        warn: warn.iter().map(|s| s.to_string()).collect(),
        source: "indexer",
    }
}

#[test]
fn the_table_shows_the_whole_covenant_id_and_a_warn_column_only_when_there_is_one() {
    let rows = vec![
        row("TUSD", &cov(1), Some(8), "kcc20-ref-public-mint 3/3", Some(12), Some(3), &[]),
        row("", &cov(2), None, "kron-2433 4/5", Some(0), Some(100), &[]),
    ];
    let t = table(&rows);
    assert_eq!(t.len(), 3);
    assert!(t[0].starts_with("TICKER  COVENANT ID") && t[0].ends_with("ASKS  BIDS"), "{t:?}");
    assert!(!t[0].contains("WARN"), "{t:?}");
    // The id is never shortened.
    assert!(t[1].contains(&cov(1)) && t[2].contains(&cov(2)), "{t:?}");
    // Columns line up: every line has the covenant id at the same column.
    let at = |l: &str, c: &str| l.find(c).unwrap();
    assert_eq!(at(&t[1], &cov(1)), at(&t[2], &cov(2)));
    assert!(t[1].starts_with("TUSD  ") && t[1].contains("  8  kcc20-ref-public-mint 3/3  "), "{t:?}");
    assert!(t[2].contains("   -  kron-2433 4/5"), "{t:?}");
    // A warning adds the column.
    let mut rows = rows;
    rows[0].warn = vec!["live_mint_authority".into(), "freeze".into()];
    let t = table(&rows);
    assert!(t[0].ends_with("WARN") && t[1].ends_with("live_mint_authority,freeze"), "{t:?}");
    assert!(t[2].trim_end().ends_with("100"), "{t:?}");
}

#[test]
fn the_indexer_rows_carry_its_facts_only() {
    let v = json!({"tokens": [
        {"ticker": "TUSD", "covenant_id": cov(1), "template_id": "kcc20-ref-public-mint", "decimals": 8, "open_asks": 4, "open_bids": 3,
         "warnings": ["genesis_unverified", "live_mint_authority"], "powers": ["freeze"], "declaration": true},
        {"ticker": "", "covenant_id": cov(2), "template_id": null, "decimals": null, "open_asks": 0, "open_bids": 1,
         "warnings": [], "powers": [], "program_ambiguous": true},
        {"ticker": "X", "covenant_id": "zz"},
        {"ticker": "ODD", "covenant_id": cov(3), "template_id": "not-a-registered-program", "open_asks": 1, "open_bids": 1},
    ]});
    let rows = indexer_rows(&v).unwrap();
    assert_eq!(rows.len(), 3, "a row without a covenant id is not a token");
    assert_eq!(rows[0].program, "kcc20-ref-public-mint 3/3");
    // `genesis_unverified` only says the registry has no check, so it is left out. The rest is as
    // the indexer says it.
    assert_eq!(rows[0].warn, ["live_mint_authority", "freeze"]);
    assert_eq!((rows[0].asks, rows[0].bids, rows[0].decimals), (Some(4), Some(3), Some(8)));
    assert_eq!((rows[1].program.as_str(), rows[1].decimals), ("ambiguous", None));
    assert_eq!(rows[2].program, "not-a-registered-program");
    assert!(indexer_rows(&json!({})).is_err());
}

fn kascov_fixture() -> Value {
    json!({"network": "mainnet", "tokens_total": 6, "tokens": [
        // KCC-20 public mint, known by its view hash.
        {"covenant_id": cov(10), "claimed_ticker": "PMT", "claimed_decimals": 6, "listed_ticker": "NOTUSED",
         "standard": {"template_hash": cov(0xee), "view_template_hash": hash_of("kcc20-ref-public-mint")}},
        // A program KOB lists but has not reviewed: said so.
        {"covenant_id": cov(11), "claimed_ticker": "EIGHT", "standard": {"view_template_hash": hash_of("kcc20-ref-8x8")}},
        // A program hash KOB has no program for.
        {"covenant_id": cov(12), "claimed_ticker": "ODD", "claimed_decimals": 0, "standard": {"template_hash": cov(0xab), "view_template_hash": cov(0xcd)}},
        // No program hash at all (the 46-byte layout).
        {"covenant_id": cov(13), "layout": "kcc20-46"},
        // Already shown by the indexer.
        {"covenant_id": cov(1), "claimed_ticker": "TUSD"},
        // Not a token id.
        {"covenant_id": "nonsense"},
    ]})
}

#[test]
fn kascov_is_a_discovery_list_whose_program_hash_is_mapped_to_kob_programs() {
    let have: BTreeSet<String> = [cov(1)].into_iter().collect();
    let rows = kascov_rows(&kascov_fixture(), &have).unwrap();
    assert_eq!(rows.len(), 4, "{rows:?}");
    let by = |c: &str| rows.iter().find(|r| r.cov == cov(c.parse().unwrap())).unwrap();
    let r = by("10");
    assert_eq!((r.ticker.as_str(), r.decimals, r.program.as_str()), ("PMT", Some(6), "kcc20-ref-public-mint 3/3"));
    assert_eq!((r.asks, r.bids, r.source), (None, None, "kascov"));
    assert_eq!(by("11").program, "kcc20-ref-8x8 8/8 (pending-review)");
    assert_eq!(by("12").program, "unsupported");
    assert_eq!((by("13").program.as_str(), by("13").ticker.as_str()), ("unidentified", ""));
    // The table marks them with "-".
    let t = table(&rows);
    assert!(t[1].split_whitespace().rev().take(2).all(|w| w == "-"), "{t:?}");
    assert!(kascov_rows(&json!({}), &have).is_err());
}

#[test]
fn search_is_a_ticker_prefix_or_a_covenant_id_prefix() {
    let r = row("TUSD", &("ab".to_string() + &cov(1)[2..]), None, "-", None, None, &[]);
    for yes in ["", "t", "TU", "tusd", "ab", "ABC", "ab01"] {
        let expect = !yes.eq_ignore_ascii_case("abc");
        assert_eq!(matches(&r, yes), expect, "{yes}");
    }
    for no in ["usd", "TUSDX", "01", "b"] {
        assert!(!matches(&r, no), "{no}");
    }
}

fn body(n: i64, max_age: u64, etag: Option<&str>) -> Fetched {
    Fetched::Body {
        body: json!({"tokens": [n]}),
        max_age: Duration::from_secs(max_age),
        validators: Validators { etag: etag.map(str::to_string), last_modified: None },
    }
}

#[test]
fn max_age_is_the_servers_and_30_seconds_without_one() {
    assert_eq!(max_age_of(Some("public, max-age=30, s-maxage=60, stale-while-revalidate=300")), Duration::from_secs(30));
    assert_eq!(max_age_of(Some("max-age=120")), Duration::from_secs(120));
    assert_eq!(max_age_of(Some("public, s-maxage=60")), DEFAULT_MAX_AGE);
    assert_eq!(max_age_of(None), Duration::from_secs(30));
    assert_eq!(max_age_of(Some("max-age=soon")), DEFAULT_MAX_AGE);
    // Nothing to reuse when the server says so.
    assert_eq!(max_age_of(Some("no-store")), Duration::ZERO);
    assert_eq!(max_age_of(Some("max-age=60, no-cache")), Duration::ZERO);
}

#[test]
fn the_local_copy_is_reused_within_max_age_only_and_revalidated_with_the_etag() {
    let dir = std::env::temp_dir().join(format!("kob-tokens-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let calls = AtomicUsize::new(0);
    let seen: std::cell::RefCell<Vec<Validators>> = Default::default();
    let fetch = |r: Result<Fetched, String>| {
        let calls = &calls;
        let seen = &seen;
        move |v: &Validators| {
            calls.fetch_add(1, Ordering::SeqCst);
            seen.borrow_mut().push(v.clone());
            r
        }
    };
    let get = |now: u64, force: bool, r: Result<Fetched, String>| cached_list(Some(&dir), "mainnet", now, force, fetch(r));
    let now = 1_000_000;
    // Nothing cached: one request (not conditional), stored with the server's max-age (30 s).
    let (v, o) = get(now, false, Ok(body(1, 30, Some("\"e1\"")))).unwrap();
    assert_eq!((v["tokens"][0].as_i64(), o), (Some(1), Origin::Fetched));
    assert_eq!(seen.borrow()[0], Validators::default());
    // 29 s later: the copy, no request.
    let (v, o) = get(now + 29, false, Ok(body(2, 30, None))).unwrap();
    assert_eq!((v["tokens"][0].as_i64(), o), (Some(1), Origin::Cache));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // 31 s later: one conditional request with the ETag. The 304 keeps the copy and renews it.
    let (v, o) = get(now + 31, false, Ok(Fetched::NotModified { max_age: Duration::from_secs(30) })).unwrap();
    assert_eq!((v["tokens"][0].as_i64(), o), (Some(1), Origin::Revalidated));
    assert_eq!(seen.borrow()[1].etag.as_deref(), Some("\"e1\""));
    let (_, o) = get(now + 60, false, Ok(body(9, 30, None))).unwrap();
    assert_eq!(o, Origin::Cache, "renewed at now + 31");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // A changed list: 200 replaces the copy (and its validators).
    let (v, o) = get(now + 100, false, Ok(body(3, 60, Some("\"e3\"")))).unwrap();
    assert_eq!((v["tokens"][0].as_i64(), o), (Some(3), Origin::Fetched));
    assert_eq!(seen.borrow()[2].etag.as_deref(), Some("\"e1\""));
    // --refresh: a request now, unconditional, whatever the copy says.
    let (v, o) = get(now + 101, true, Ok(body(4, 60, None))).unwrap();
    assert_eq!((v["tokens"][0].as_i64(), o), (Some(4), Origin::Fetched));
    assert_eq!(seen.borrow()[3], Validators::default());
    // The server is down: the older list, said so.
    let (v, o) = get(now + 4000, false, Err("connection refused".into())).unwrap();
    assert_eq!((v["tokens"][0].as_i64(), o), (Some(4), Origin::StaleCache));
    // Another network has its own file. Nothing cached and down: the error.
    let e = cached_list(Some(&dir), "testnet-10", now, false, fetch(Err("connection refused".into()))).unwrap_err();
    assert_eq!(e, "connection refused");
    // A 304 with nothing cached cannot be served.
    let e =
        cached_list(Some(&dir), "testnet-10", now, false, fetch(Ok(Fetched::NotModified { max_age: DEFAULT_MAX_AGE }))).unwrap_err();
    assert!(e.contains("304"), "{e}");
    // No cache directory: every call asks.
    let (_, o) = cached_list(None, "mainnet", now, false, fetch(Ok(body(5, 30, None)))).unwrap();
    assert_eq!(o, Origin::Fetched);
    // No max-age (zero): never reused.
    let (_, _) = get(now + 5000, false, Ok(body(6, 0, None))).unwrap();
    let (_, o) = get(now + 5000, false, Ok(body(7, 0, None))).unwrap();
    assert_eq!(o, Origin::Fetched);
    let _ = std::fs::remove_dir_all(&dir);
}

fn common(indexer: &str, extra: &str) -> Common {
    let mut a: Vec<String> = vec!["--network".into(), "mainnet".into(), "--indexer".into(), indexer.into()];
    a.extend(extra.split_whitespace().map(str::to_string));
    Common::from(&parse(&a, &[], &["--all"]).unwrap()).unwrap()
}

#[test]
fn the_command_end_to_end_with_a_mock_indexer_and_a_mock_kascov() {
    use crate::trade::http::mock;
    let routes: BTreeMap<String, Value> = [
        (
            "/v1/tokens".to_string(),
            json!({"tokens": [
                {"ticker": "TUSD", "covenant_id": cov(1), "template_id": "kcc20-ref-public-mint", "decimals": 8, "open_asks": 2, "open_bids": 1,
                 "warnings": ["genesis_unverified"], "powers": []},
                {"ticker": "", "covenant_id": cov(2), "template_id": "kron-2433", "decimals": 3, "open_asks": 0, "open_bids": 5,
                 "warnings": ["live_mint_authority"], "powers": []},
            ]}),
        ),
        ("/data/mainnet/tokens.json".to_string(), kascov_fixture()),
    ]
    .into_iter()
    .collect();
    let (url, state) = mock::start(routes);
    state.lock().unwrap().headers.insert(
        "/data/mainnet/tokens.json".into(),
        vec![("Cache-Control".into(), "public, max-age=30".into()), ("ETag".into(), "\"v1\"".into())],
    );
    let kascov_requests = || state.lock().unwrap().seen.iter().filter(|p| p.as_str() == "/data/mainnet/tokens.json").count();
    let dir = std::env::temp_dir().join(format!("kob-tokens-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let c = common(&url, "");
    let show = |search: &str, all: bool, refresh: bool| view(&c, search, all, refresh, &url, Some(&dir));
    // The default: the indexer's tokens only, kascov is not asked.
    let (doc, lines) = show("", false, false).unwrap();
    assert!(lines[0].starts_with("2 token(s) on mainnet (indexer http://127.0.0.1:"), "{lines:?}");
    assert_eq!(doc["tokens"].as_array().unwrap().len(), 2);
    assert_eq!(doc["tokens"][1]["warnings"], json!(["live_mint_authority"]));
    assert_eq!(doc["kascov"], Value::Null);
    assert_eq!(kascov_requests(), 0);
    // A search by covenant-id prefix.
    let (doc, lines) = show(&cov(2)[..6], false, false).unwrap();
    assert_eq!(doc["tokens"].as_array().unwrap().len(), 1);
    assert!(lines[0].contains("matching `020202`"), "{lines:?}");
    // --all: plus the four kascov-only tokens. One request.
    let (doc, lines) = show("", true, false).unwrap();
    assert_eq!(doc["tokens"].as_array().unwrap().len(), 6);
    assert_eq!(doc["kascov"]["origin"], "fetched");
    assert!(lines[0].contains("plus the kascov.io list"), "{lines:?}");
    let pmt = lines.iter().find(|l| l.starts_with("PMT")).unwrap();
    assert!(pmt.contains(&cov(10)) && pmt.contains("kcc20-ref-public-mint 3/3"), "{pmt}");
    // Within max-age: the copy, no request.
    let (doc, _) = show("pm", true, false).unwrap();
    assert_eq!((doc["kascov"]["origin"].as_str(), doc["tokens"].as_array().unwrap().len()), (Some("cache"), 1));
    assert_eq!(kascov_requests(), 1);
    // --refresh: a request now (not conditional).
    let (doc, _) = show("pm", true, true).unwrap();
    assert_eq!(doc["kascov"]["origin"], "fetched");
    assert_eq!(kascov_requests(), 2);
    let sent = state.lock().unwrap().request_headers.clone();
    let kascov_sent: Vec<_> =
        state.lock().unwrap().seen.iter().zip(&sent).filter(|(p, _)| p.contains("/data/")).map(|(_, h)| h.clone()).collect();
    assert!(
        kascov_sent.iter().all(|h| !h.iter().any(|(k, _)| k == "if-none-match")),
        "the first and the forced request are unconditional"
    );
    // Past max-age (the clock of the copy is moved back in its metadata): one conditional request,
    // answered 304.
    let meta = dir.join("kascov-mainnet-tokens.meta.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&meta).unwrap()).unwrap();
    assert_eq!((m["max_age"].as_u64(), m["etag"].as_str()), (Some(30), Some("\"v1\"")));
    m["fetched_at"] = json!(m["fetched_at"].as_u64().unwrap() - 120);
    std::fs::write(&meta, m.to_string()).unwrap();
    let (doc, _) = show("pm", true, false).unwrap();
    assert_eq!(doc["kascov"]["origin"], "revalidated");
    assert_eq!(kascov_requests(), 3);
    let last = state.lock().unwrap().request_headers.last().unwrap().clone();
    assert!(last.iter().any(|(k, v)| k == "if-none-match" && v == "\"v1\""), "{last:?}");
    // A full covenant id in no list: said so, with the way on.
    let unknown = cov(0x77);
    let (doc, lines) = show(&unknown, true, false).unwrap();
    assert_eq!(doc["tokens"].as_array().unwrap().len(), 0);
    let note = lines.last().unwrap();
    assert!(
        note.starts_with("note: ")
            && note.contains("kascov.io may not have indexed it yet")
            && note.contains(&format!("kob book {unknown}")),
        "{note}"
    );
    assert!(doc["note"].as_str().unwrap().contains("neither the indexer's list nor kascov.io's"));
    let (_, lines) = show(&unknown, false, false).unwrap();
    assert!(lines.last().unwrap().contains("--all") && lines.last().unwrap().contains("not in the indexer's list"), "{lines:?}");
    // A prefix is not a full id: no note.
    let (doc, _) = show("77", true, false).unwrap();
    assert_eq!(doc["note"], Value::Null);
    // kascov unreachable, nothing cached: a clear error that names the way on.
    let _ = std::fs::remove_dir_all(&dir);
    let e = view(&c, "", true, false, "http://127.0.0.1:1", Some(&dir)).unwrap_err();
    assert!(e.contains("kascov.io list is not reachable") && e.contains("without --all"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn facts(ticker: &str, c: u8) -> TokenFacts {
    TokenFacts {
        cov: [c; 32],
        ticker: ticker.into(),
        decimals: Some(3),
        tpl_hash: None,
        ext: None,
        tick: None,
        genesis_verified: false,
        not_tradable: None,
        market_scale: None,
        declared: false,
        program_ambiguous: false,
    }
}

#[test]
fn a_ticker_that_names_two_tokens_is_refused_with_every_candidate() {
    let reg = vec![facts("TUSD", 1)];
    let idx = vec![facts("TUSD", 1), facts("tusd", 2), facts("OTHER", 3)];
    let e = market::unique_cov(&reg, &idx, "TUSD").unwrap_err();
    assert!(e.contains("`TUSD` names 2 tokens") && e.contains(&cov(1)) && e.contains(&cov(2)) && !e.contains(&cov(3)), "{e}");
    assert!(e.contains("(registry)") && e.contains("(indexer)"), "{e}");
    // The same token in both lists is one token. A covenant id is never ambiguous. An unknown name
    // is none.
    assert_eq!(market::unique_cov(&reg, &[facts("TUSD", 1)], "tusd").unwrap(), Some([1; 32]));
    assert_eq!(market::unique_cov(&reg, &idx, &cov(2)).unwrap(), Some([2; 32]));
    assert_eq!(market::unique_cov(&reg, &idx, "NOPE").unwrap(), None);
    // The resolver of every trading command refuses it too, instead of using the registry.
    let e = market::resolve_with("TUSD", &reg, &idx, None).unwrap_err();
    assert!(e.contains("names 2 tokens") && e.contains(&cov(2)), "{e}");
    let e = market::resolve_with("TUSD", &[], &[facts("TUSD", 4), facts("TUSD", 5)], None).unwrap_err();
    assert!(e.contains(&cov(4)) && e.contains(&cov(5)), "{e}");
}
