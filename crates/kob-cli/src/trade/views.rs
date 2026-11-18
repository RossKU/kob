//! The read-only trading commands.
//!
//! - `kob book` shows the KAS book of a token or a pair book.
//! - `kob orders` lists the orders of the maker, as "My orders" does.
//! - `kob balances` shows the free KAS and tokens, and what the live orders of the maker hold:
//!   custody, strays and locked KAS. See `web/src/kob/balances.ts`.
//!
//! Nothing is signed, and no key is needed when `--maker` names the owner.

use std::collections::BTreeMap;

use kob_protocol::json::to_hex;
use kob_protocol::state::{AnyState, TokenState};
use serde_json::{json, Value};

use super::data;
use super::http;
use super::market::{self, show_id, TokenFacts, TokenMarket};
use super::opts::{parse, Common, Parsed, COMMON_HELP};
use super::plan::{Book, Level};
use super::units::{self, TokenUnit};

/// The help text of `kob book`.
pub const BOOK_USAGE: &str = "\
kob book: a token's order book on KAS, or the book of a pair (the indexer's view; nothing is signed)

USAGE
  kob book <TOKEN> [--quote TOKEN_B] [--depth N] [--json]

  TOKEN is a ticker or a covenant id. Prices are KAS per whole token (a pair: TOKEN_B per whole TOKEN), levels
  aggregated by price, asks above the spread, bids below; the last fill closes the view. A token without a registry
  ticker is named by the ticker its KCC-23 declaration names, marked (declared on chain, unverified).
";

/// The help text of `kob orders`.
pub const ORDERS_USAGE: &str = "\
kob orders: the maker's orders (the indexer's view, as the web wallet's My orders lists them)

USAGE
  kob orders [--token TOKEN] [--all] [--maker KEY|ADDRESS] [--json]

  Live orders by default (--all: every status). The maker is --maker, else the key of --key-file / $KOB_KEY (only its
  public key is used). Each line: covenant id, kind, side, what is left, the price or legs, status and expiry.
";

/// The help text of `kob balances`.
pub const BALANCES_USAGE: &str = "\
kob balances: free KAS and tokens of the maker, and what the maker's live orders hold

USAGE
  kob balances [--maker KEY|ADDRESS] [--json]

  free: the maker's own KAS UTXOs (node) and token UTXOs (indexer, each confirmed on the node when the token's program
  is known); in orders: tokens in the custody of live orders, stray tokens sent to them (only the maker's cancel
  recovers them), KAS locked (a buy's budget, carriers, tips and reserves). A token without a registry ticker is named
  by the ticker its KCC-23 declaration names, marked (declared on chain, unverified).
";

fn bail(e: String, code: i32) -> i32 {
    eprintln!("error: {e}");
    code
}

/// The parsed arguments and settings of a read-only command. `--help` prints `usage`.
fn ctx(args: &[String], value_flags: &[&str], bool_flags: &[&str], usage: &str) -> Result<(Parsed, Common), i32> {
    if let Some(code) = crate::help::help_or_usage(args, &[usage, COMMON_HELP], false) {
        return Err(code);
    }
    let p = parse(args, value_flags, bool_flags).map_err(|e| bail(e, 2))?;
    let c = Common::from(&p).map_err(|e| bail(e, 2))?;
    Ok((p, c))
}

/// The owner that a view is about.
///
/// It is `--maker`, an x-only key or a P2PK address of the network, else the signing key.
pub fn maker_of(p: &Parsed, c: &Common, argv: &[String]) -> Result<[u8; 32], String> {
    match p.get("--maker") {
        Some(m) => match super::actions::recipient(m, &c.network)? {
            (_, Some(k)) => Ok(k),
            _ => Err(format!("--maker `{m}` is not a P2PK address or an x-only key")),
        },
        None => c.signer(argv).map(|s| s.pubkey).map_err(|e| format!("{e} (or name the owner with --maker)")),
    }
}

/// The display facts of a token from the registry and the indexer lists.
///
/// The ticker is the list's, else `declared` ([`declared_ticker`]), else the full id.
pub(crate) fn unit_of(cov: &[u8; 32], reg: &[TokenFacts], idx: &[TokenFacts], declared: Option<&str>) -> (TokenUnit, bool) {
    let f = reg.iter().find(|t| t.cov == *cov).or_else(|| idx.iter().find(|t| t.cov == *cov));
    let ticker = f
        .map(|t| t.ticker.clone())
        .filter(|t| !t.is_empty())
        .or_else(|| declared.map(str::to_owned))
        .unwrap_or_else(|| show_id(cov, ""));
    let decimals = f.and_then(|t| t.decimals);
    (TokenUnit { ticker, decimals: decimals.unwrap_or(0) }, decimals.is_some())
}

/// The marker of a ticker that only the token's KCC-23 declaration names, as the web app's.
const DECLARED_MARK: &str = "declared on chain, unverified";

/// The declared ticker of a token the registry and the indexer give no ticker, for a view.
///
/// It is the symbol of its KCC-23 declaration (the indexer's launch proof, verified against the
/// covenant id, [`data::declaration`]) when it can be a ticker (`market::ticker_of_symbol`). It is
/// asked for a token the indexer marks declared or does not list, as `market::resolve_with` does.
/// No declaration, one that does not verify or cannot be fetched: `None`, the view unchanged.
fn declared_ticker(indexer: &str, cov: &[u8; 32], reg: &[TokenFacts], idx: &[TokenFacts]) -> Option<String> {
    declared_ticker_with(cov, reg, idx, &|c| data::declaration(indexer, c).ok().flatten())
}

/// [`declared_ticker`] with the declaration from `declaration`.
fn declared_ticker_with(
    cov: &[u8; 32],
    reg: &[TokenFacts],
    idx: &[TokenFacts],
    declaration: &dyn Fn(&[u8; 32]) -> Option<market::Declared>,
) -> Option<String> {
    let listed = reg.iter().chain(idx).any(|t| t.cov == *cov && !t.ticker.is_empty());
    let undeclared = idx.iter().any(|t| t.cov == *cov && !t.declared);
    if listed || undeclared {
        return None;
    }
    market::ticker_of_symbol(declaration(cov)?.symbol.as_deref())
}

/// The name of a token in a line of a view: `ticker` (the list's), or a declared ticker with the
/// full id and [`DECLARED_MARK`].
fn view_name(cov: &[u8; 32], ticker: &str, declared: Option<&str>) -> String {
    match declared {
        Some(d) => format!("{d} ({}, {DECLARED_MARK})", to_hex(cov)),
        None => ticker.to_string(),
    }
}

fn amount_text(base: i128, u: &(TokenUnit, bool)) -> String {
    if u.1 {
        format!("{} {}", units::fmt_decimal(base, u.0.decimals), u.0.ticker)
    } else {
        format!("{base} base units of {}", u.0.ticker)
    }
}

fn i64_of(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn hex32(v: Option<&Value>) -> Option<[u8; 32]> {
    v.and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32)
}

// ---------------------------------------------------------------------------- book

/// The text lines of a book: asks highest first, the spread, then bids highest first.
pub fn book_lines(book: &Book, price: &dyn Fn(i64) -> String, amount: &dyn Fn(i64) -> String, last: Option<i64>) -> Vec<String> {
    let row = |l: &Level| {
        format!("    {:>28}  {:>28}  {} order{}", price(l.price), amount(l.amount), l.orders, if l.orders == 1 { "" } else { "s" })
    };
    let mut v = vec!["  asks".to_string()];
    if book.asks.is_empty() {
        v.push("    (none)".into());
    }
    v.extend(book.asks.iter().rev().map(row));
    match (book.asks.first(), book.bids.first()) {
        // A momentarily crossed book is normal, because an executor is about to match it. The
        // spread is never negative.
        (Some(a), Some(b)) if b.price > a.price => {
            v.push(format!("  -- crossed by {} (best ask {}, best bid {})", price(b.price - a.price), price(a.price), price(b.price)))
        }
        (Some(a), Some(b)) => {
            v.push(format!("  -- spread {} (best ask {}, best bid {})", price(a.price - b.price), price(a.price), price(b.price)))
        }
        _ => v.push("  --".into()),
    }
    v.push("  bids".into());
    if book.bids.is_empty() {
        v.push("    (none)".into());
    }
    v.extend(book.bids.iter().map(row));
    if let Some(l) = last {
        v.push(format!("  last fill: {}", price(l)));
    }
    v
}

/// Whether the best bid is above the best ask.
fn is_crossed(book: &Book) -> bool {
    matches!((book.asks.first(), book.bids.first()), (Some(a), Some(b)) if b.price > a.price)
}

fn levels_json(levels: &[Level], price: &dyn Fn(i64) -> String) -> Value {
    json!(levels
        .iter()
        .map(
            |l| json!({"price": l.price.to_string(), "price_text": price(l.price), "amount": l.amount.to_string(), "orders": l.orders})
        )
        .collect::<Vec<_>>())
}

/// Prints a view: its JSON with `--json`, else its lines.
fn print_view(c: &Common, r: Result<(Value, Vec<String>), String>) -> i32 {
    match r {
        Ok((v, lines)) => {
            if c.json {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
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

/// Runs `kob book`.
pub fn run_book(args: &[String]) -> i32 {
    let (p, c) = match ctx(args, &["--quote", "--depth"], &[], BOOK_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    // A mistake in the arguments is a usage error (2), whatever the indexer would have said.
    if let Err(e) = book_args(&p) {
        return bail(e, 2);
    }
    print_view(&c, book_view(&c, &p))
}

/// The arguments of `kob book` that need no network.
///
/// They are the token, as ticker or covenant id, and the depth.
fn book_args(p: &Parsed) -> Result<(&str, u32), String> {
    let [key] = p.pos.as_slice() else { return Err("give exactly one token (ticker or covenant id)".into()) };
    let depth: u32 = match p.get("--depth") {
        Some(d) => d.parse().ok().filter(|d| (1..=500).contains(d)).ok_or(format!("--depth must be 1..500, got `{d}`"))?,
        None => 20,
    };
    Ok((key.as_str(), depth))
}

/// The book view: its JSON and its lines.
pub fn book_view(c: &Common, p: &Parsed) -> Result<(Value, Vec<String>), String> {
    {
        let (key, depth) = book_args(p)?;
        let indexer = c.need_indexer()?;
        let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(indexer))?;
        // A token without a registry entry gets its decimals from the scale of its market, as the
        // open token of `kob place` does. A token without orders, whose decimals nothing tells,
        // has an empty book, which is shown without them.
        let order_scale = |cov: &[u8; 32]| data::order_scale(indexer, cov);
        let remembered = |cov: &[u8; 32]| crate::recent::first_order_decimals(&c.network, cov);
        let open = market::OpenToken { remembered: Some(&remembered), view: true, ..market::OpenToken::new(&c.network, &order_scale) };
        let a = market::resolve_with(key, &reg, &idx, Some(&open))?;
        let b = p.get("--quote").map(|q| market::resolve_with(q, &reg, &idx, Some(&open))).transpose()?;
        let unknown: Vec<&TokenMarket> =
            [Some(&a), b.as_ref()].into_iter().flatten().filter(|m| m.decimals_from == market::DecimalsFrom::Unknown).collect();
        let (book, last, quote_name): (Book, Option<i64>, String) = match &b {
            _ if !unknown.is_empty() => (Book::default(), None, b.as_ref().map_or("KAS".into(), |bm| bm.name())),
            None => {
                let v = http::get_ok(indexer, &format!("/v1/books/{}?depth={depth}", to_hex(&a.cov)), data::INDEXER_TIMEOUT)?;
                (data::book_from_indexer(a.scale, &v), data::last_fill(indexer, &a).ok().flatten(), "KAS".into())
            }
            Some(bm) => {
                let v = http::get_ok(
                    indexer,
                    &format!("/v1/pairs/{}/{}/book?depth={depth}", to_hex(&a.cov), to_hex(&bm.cov)),
                    data::INDEXER_TIMEOUT,
                )?;
                (data::pair_book_from(&v, a.scale), None, bm.name())
            }
        };
        // A token the lists give no ticker is named by its declared ticker, marked.
        let da = if a.ticker.is_empty() { declared_ticker(indexer, &a.cov, &reg, &idx) } else { None };
        let db = b.as_ref().filter(|bm| bm.ticker.is_empty()).and_then(|bm| declared_ticker(indexer, &bm.cov, &reg, &idx));
        let a_ticker = if a.ticker.is_empty() { da.clone().unwrap_or_default() } else { a.ticker.clone() };
        let ua = TokenUnit { ticker: a_ticker.clone(), ..a.unit() };
        let (qd, qn) = match &b {
            Some(bm) => {
                (bm.decimals, if bm.ticker.is_empty() { db.clone().unwrap_or_else(|| "B".to_string()) } else { bm.ticker.clone() })
            }
            None => (units::KAS_DECIMALS, "KAS".to_string()),
        };
        let quote_name = match (&b, &db) {
            (Some(bm), Some(d)) => view_name(&bm.cov, "", Some(d)),
            _ => quote_name,
        };
        let price = |x: i64| units::price_per_whole(x, &ua, a.scale, qd, &qn);
        let amount = |x: i64| {
            format!("{} {}", units::fmt_decimal(x as i128, a.decimals), if a_ticker.is_empty() { "tokens" } else { &a_ticker })
        };
        let out = json!({
            "network": c.network,
            "token": to_hex(&a.cov),
            "quote": b.as_ref().map(|x| to_hex(&x.cov)),
            "declared_ticker": da.clone(),
            "quote_declared_ticker": db.clone(),
            "scale": (a.decimals_from != market::DecimalsFrom::Unknown).then_some(a.scale),
            "asks": levels_json(&book.asks, &price),
            "bids": levels_json(&book.bids, &price),
            "best_ask": book.asks.first().map(|l| l.price.to_string()),
            "best_bid": book.bids.first().map(|l| l.price.to_string()),
            "crossed": is_crossed(&book),
            "last_fill": last.map(|l| l.to_string()),
        });
        let mut lines = vec![format!(
            "book {} in {quote_name} per whole token (network {}, indexer {})",
            if da.is_some() { view_name(&a.cov, "", da.as_deref()) } else { a.name() },
            c.network,
            http::shown(indexer)
        )];
        lines.extend(book_lines(&book, &price, &amount, last));
        for m in unknown {
            lines.push(format!(
                "  {}: no order of it yet, so no price scale: the chain does not record decimals, and its first order names them \
                 (`kob place ... --decimals N`)",
                m.name()
            ));
        }
        Ok((out, lines))
    }
}

// ---------------------------------------------------------------------------- orders

/// The orders of the maker from the indexer (`GET /v1/orders`), every page up to 20.
pub fn fetch_orders(indexer: &str, maker: &[u8; 32], token: Option<&[u8; 32]>, all: bool) -> Result<Vec<Value>, String> {
    let mut out = vec![];
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let mut path = format!("/v1/orders?maker={}", to_hex(maker));
        if let Some(t) = token {
            path.push_str(&format!("&token={}", to_hex(t)));
        }
        if !all {
            path.push_str("&status=active");
        }
        path.push_str("&limit=200");
        if let Some(c) = &cursor {
            path.push_str(&format!("&cursor={c}"));
        }
        let v = http::get_ok(indexer, &path, data::INDEXER_TIMEOUT)?;
        out.extend(v.get("items").and_then(Value::as_array).cloned().unwrap_or_default());
        match v.get("next_cursor").and_then(Value::as_str) {
            Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

/// What one order trades, in words: its side, what is left, and its price or legs.
///
/// It is `order-facts.ts` in short.
pub fn describe(s: &AnyState, unit: &(TokenUnit, bool), quote: &(TokenUnit, bool), pair: bool) -> String {
    let (qd, qn) = if pair { (quote.0.decimals, quote.0.ticker.clone()) } else { (units::KAS_DECIMALS, "KAS".to_string()) };
    let p = |x: i64| units::price_per_whole(x, &unit.0, s.scale(), qd, &qn);
    let left = s.amount_left().map(|a| amount_text(a as i128, unit));
    let legs = |tp: i64, stop: i64, trail: i64| {
        let mut v = vec![];
        if tp > 0 {
            v.push(format!("take-profit {}", p(tp)));
        }
        if stop > 0 {
            v.push(format!("stop {}", p(stop)));
        }
        if trail > 0 {
            v.push(format!("trailing by {}", p(trail)));
        }
        v.join(", ")
    };
    let decay = |price: i64, end: i64, slope: i64| {
        if slope > 0 && end > 0 && end != price {
            format!("from {} to {}", p(price), p(end))
        } else {
            format!("at {}", p(price))
        }
    };
    let (side, what) = match s {
        AnyState::KobCurveAsk(x) => {
            ("sell", format!("curve, {} levels from {} to {}", x.levels(), p(x.table[0]), p(*x.table.last().unwrap_or(&0))))
        }
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => ("sell", decay(x.price, x.price_end, x.slope)),
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => ("buy", decay(x.price, x.price_end, x.slope)),
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => ("sell", legs(x.tp_price, x.stop_price, x.trail_step)),
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => ("buy", legs(x.tp_price, x.stop_price, x.trail_step)),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => {
            let e = x.exit().ok();
            ("buy first", ifd_text(&p, x.price, x.entry_stop, e.map(|e| (e.tp_price, e.stop_price)), x.rpt_amount))
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => {
            let e = x.exit().ok();
            ("sell first", ifd_text(&p, x.price, x.entry_stop, e.map(|e| (e.tp_price, e.stop_price)), x.rpt_amount))
        }
        AnyState::KobPair(x) => (if x.is_ask() { "sell" } else { "buy" }, decay(x.price, x.price_end, x.slope)),
        AnyState::KobCondPair(x) => (if x.is_ask() { "sell" } else { "buy" }, legs(x.tp_price, x.stop_price, x.trail_step)),
        AnyState::KobIfdPair(x) => {
            let e = x.exit().ok();
            (
                if x.is_buy_first() { "buy first" } else { "sell first" },
                ifd_text(&p, x.price, x.entry_stop, e.map(|e| (e.tp_price, e.stop_price)), x.rpt_amount),
            )
        }
    };
    match left {
        Some(l) => format!("{side} {l} {what}"),
        None => format!("{side} {what}"),
    }
}

fn ifd_text(p: &dyn Fn(i64) -> String, price: i64, stop: i64, exit: Option<(i64, i64)>, rpt: i64) -> String {
    let mut v = vec![if stop > 0 { format!("entry {} (stop {})", p(price), p(stop)) } else { format!("entry {}", p(price)) }];
    if let Some((tp, sl)) = exit {
        if tp > 0 {
            v.push(format!("exit take-profit {}", p(tp)));
        }
        if sl > 0 {
            v.push(format!("exit stop {}", p(sl)));
        }
    }
    if rpt > 0 {
        v.push("repeating".into());
    }
    v.join(", ")
}

/// One order view of the indexer as `kob orders` shows it, and `kob watch order`: its JSON row.
pub fn order_row(o: &Value, reg: &[TokenFacts], idx: &[TokenFacts]) -> Value {
    let cov = hex32(o.get("covenant_id")).map(|x| to_hex(&x)).unwrap_or_default();
    let contract = o.get("contract").and_then(Value::as_str).unwrap_or("?").to_string();
    let status = o.get("status").and_then(Value::as_str).unwrap_or("?").to_string();
    let state: Option<AnyState> = o
        .get("state")
        .filter(|_| o.get("state_known").and_then(Value::as_bool) == Some(true))
        .and_then(|s| serde_json::from_value(s.clone()).ok());
    let (a, b) = match state.as_ref().and_then(AnyState::pair_tokens) {
        Some(pt) => (pt.a.cov_id, Some(pt.b.cov_id)),
        None => (state.as_ref().map(AnyState::token_cov_id).or(hex32(o.get("token"))).unwrap_or_default(), None),
    };
    let ua = unit_of(&a, reg, idx, None);
    let ub = b.map(|b| unit_of(&b, reg, idx, None));
    let what = match &state {
        Some(s) => describe(s, &ua, ub.as_ref().unwrap_or(&ua), b.is_some()),
        None => format!(
            "{} (state not proven by the indexer)",
            if o.get("side").and_then(Value::as_i64) == Some(1) { "sell" } else { "buy" }
        ),
    };
    let market = match &ub {
        Some(q) => format!("{}/{}", ua.0.ticker, q.0.ticker),
        None => ua.0.ticker.clone(),
    };
    let expiry = state.as_ref().and_then(AnyState::expiry).map(|e| e.0);
    // How it delivers; a pair order delivers the token it receives.
    let delivery = state.as_ref().and_then(|s| {
        let t = match s {
            AnyState::KobPair(p) => p.t_cov_id,
            AnyState::KobCondPair(p) => p.t_cov_id,
            _ => s.token_cov_id(),
        };
        super::delivery::text(s, &unit_of(&t, reg, idx, None).0)
    });
    let bought = state.as_ref().and_then(AnyState::bought_custody);
    let mut flags = vec![];
    if o.get("expired").and_then(Value::as_bool) == Some(true) {
        flags.push("expired".to_string());
    }
    if let Some(d) = i64_of(o.get("refund_due_daa")) {
        flags.push(format!("refundable from DAA {d}"));
    }
    if o.get("possibly_frozen").and_then(Value::as_bool) == Some(true) {
        flags.push("possibly frozen".into());
    }
    json!({
                "covenant_id": cov, "contract": contract, "market": market, "status": status, "what": what,
                "expiry_daa": expiry.filter(|e| *e < 1 << 60), "flags": flags, "delivery": delivery,
                "bought": bought.map(|b| b.to_string()),
                "value": o.get("current").and_then(|c| c.get("value")).cloned().unwrap_or(Value::Null),
    })
}

/// The text line of an [`order_row`].
pub fn order_row_line(r: &Value) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
    let exp = r["expiry_daa"].as_i64().map(|e| format!(", expiry DAA {e}")).unwrap_or_default();
    let flags = r["flags"].as_array().map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let delivery = r["delivery"].as_str().map(|d| format!("; {d}")).unwrap_or_default();
    format!(
        "{} {} {}: {}{delivery} [{}{}{}]",
        s("covenant_id"),
        s("contract"),
        s("market"),
        s("what"),
        s("status"),
        exp,
        if flags.is_empty() { String::new() } else { format!("; {flags}") }
    )
}

/// Runs `kob orders`.
pub fn run_orders(args: &[String]) -> i32 {
    let (p, c) = match ctx(args, &["--token", "--maker"], &["--all"], ORDERS_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if let Some(x) = p.pos.first() {
        return bail(format!("unexpected argument `{x}`"), 2);
    }
    // No owner, with no --maker and no key, is a usage error (2), not a failure of the indexer or
    // the node.
    if let Err(e) = maker_of(&p, &c, args) {
        return bail(e, 2);
    }
    print_view(&c, orders_view(&c, &p, args))
}

/// The orders view: its JSON and its lines.
pub fn orders_view(c: &Common, p: &Parsed, args: &[String]) -> Result<(Value, Vec<String>), String> {
    {
        let maker = maker_of(p, c, args)?;
        let indexer = c.need_indexer()?;
        let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(indexer))?;
        let token = match p.get("--token") {
            Some(t) => Some(match kob_protocol::registry::parse_hex32(t) {
                Some(c) => c,
                None => market::unique_cov(&reg, &idx, t)?.ok_or(format!("unknown token `{t}`"))?,
            }),
            None => None,
        };
        let items = fetch_orders(indexer, &maker, token.as_ref(), p.has("--all"))?;
        let rows: Vec<Value> = items.iter().map(|o| order_row(o, &reg, &idx)).collect();
        let mut lines = vec![format!(
            "orders of {} ({}, network {}, indexer {})",
            to_hex(&maker),
            if p.has("--all") { "every status" } else { "live" },
            c.network,
            http::shown(indexer)
        )];
        if rows.is_empty() {
            lines.push("  (none)".into());
        }
        for r in &rows {
            lines.push(format!("  {}", order_row_line(r)));
        }
        Ok((json!({"network": c.network, "maker": to_hex(&maker), "orders": rows}), lines))
    }
}

// ---------------------------------------------------------------------------- balances

/// What the live orders of the maker hold of one token (`EscrowedBalance`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Escrowed {
    /// Tokens in custody, in base units.
    pub custody: i128,
    /// Tokens bought by collected orders and held by them: the maker's, until claimed.
    pub bought: i128,
    /// Stray tokens that live orders own.
    pub strays: i128,
    /// The KAS budget of buy-side orders.
    pub kas_escrow: i128,
    /// The KAS locked that is not budget: carriers, tips and reserves.
    pub kas_carriers: i128,
    /// The number of live orders counted, under the token of the order.
    ///
    /// A pair order counts under A.
    pub orders: usize,
}

/// An order that the balances do not count, as covenant id and reason.
///
/// The reason is `terminal`, `state-unknown` or `no-current-utxo`.
pub type Skipped = (String, &'static str);

const TERMINAL: &[&str] = &["filled", "cancelled", "refunded", "killed", "closed"];

/// What live orders hold, per token (`escrowedBalancesDetailed` over indexer order views).
///
/// The orders that are not counted are returned by covenant id with the reason. They are
/// terminal ones, ones whose state is not proven, and ones without a current outpoint.
pub fn escrowed(views: &[Value]) -> (BTreeMap<[u8; 32], Escrowed>, Vec<Skipped>) {
    let mut by: BTreeMap<[u8; 32], Escrowed> = BTreeMap::new();
    let mut skipped = vec![];
    for o in views {
        let cov = o.get("covenant_id").and_then(Value::as_str).unwrap_or("").to_string();
        let status = o.get("status").and_then(Value::as_str).unwrap_or("");
        if TERMINAL.contains(&status) {
            skipped.push((cov, "terminal"));
            continue;
        }
        let state: Option<AnyState> = o
            .get("state")
            .filter(|_| o.get("state_known").and_then(Value::as_bool) == Some(true))
            .and_then(|s| serde_json::from_value(s.clone()).ok());
        let Some(s) = state else {
            skipped.push((cov, "state-unknown"));
            continue;
        };
        let Some(value) = o.get("current").and_then(|c| i64_of(c.get("value"))) else {
            skipped.push((cov, "no-current-utxo"));
            continue;
        };
        // An unbound KobCurveAsk (placed in its token's genesis) names no token in its state: the
        // view's `token` is its custody's.
        let view_token = hex32(o.get("token"));
        let bind = |t: [u8; 32]| if t == [0; 32] { view_token.unwrap_or(t) } else { t };
        let token = bind(s.token_cov_id());
        by.entry(token).or_default().orders += 1;
        let utxo_rows: Vec<&Value> = match o.get("pair").filter(|p| !p.is_null()) {
            Some(p) => p
                .get("custodies")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| c.get("utxo")).collect())
                .unwrap_or_default(),
            // A buy owns no sell custody: what the view calls its custody is the bought-custody, counted below.
            None if !reserved_kas(&s).0 => vec![],
            None => o.get("custody").and_then(|c| c.get("utxo")).into_iter().collect(),
        };
        let mut carriers = 0i128;
        let mut seen = vec![];
        for u in utxo_rows.into_iter().filter(|u| !u.is_null() && u.get("spent").and_then(Value::as_bool) != Some(true)) {
            let Some(t) = hex32(u.get("token")) else { continue };
            let amount = u.get("state").and_then(|st| i64_of(st.get("amount"))).or(i64_of(u.get("amount"))).unwrap_or(0);
            by.entry(t).or_default().custody += amount as i128;
            carriers += i64_of(u.get("value")).unwrap_or(0) as i128;
            seen.push(t);
        }
        // A custody that the view does not show counts what the state says it must hold.
        for (t, amount) in s.custodies() {
            let t = bind(t);
            if !seen.contains(&t) {
                by.entry(t).or_default().custody += amount as i128;
            }
        }
        // The bought-custody of a collected order: the view's UTXO, else what the state says it holds.
        if let (Some(b), Ok(Some((_, _, t)))) = (s.bought_custody(), super::data::bought_target(&s, [0; 32])) {
            let u = o.get("bought_custody").and_then(|c| c.get("utxo")).filter(|u| !u.is_null());
            by.entry(t).or_default().bought += u.and_then(|u| i64_of(u.get("amount"))).unwrap_or(b) as i128;
            carriers += u.and_then(|u| i64_of(u.get("value"))).unwrap_or(0) as i128;
        }
        for u in o.get("strays").and_then(Value::as_array).into_iter().flatten() {
            if u.get("spent").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(t) = hex32(u.get("token")) else { continue };
            let amount = u.get("state").and_then(|st| i64_of(st.get("amount"))).or(i64_of(u.get("amount"))).unwrap_or(0);
            by.entry(t).or_default().strays += amount as i128;
            carriers += i64_of(u.get("value")).unwrap_or(0) as i128;
        }
        let value = value as i128;
        let b = by.entry(token).or_default();
        let (sell, reserved) = reserved_kas(&s);
        if sell || s.is_pair() {
            // For ask kinds and pair orders, the order UTXO is carriers, prefunds and tips, and
            // never a KAS trading budget.
            carriers += value;
        } else {
            let non_budget = reserved.min(value);
            carriers += non_budget;
            b.kas_escrow += value - non_budget;
        }
        b.kas_carriers += carriers;
    }
    (by, skipped)
}

/// Whether an order sells, and the KAS of a buy's order UTXO that is not trading budget.
///
/// That KAS is the tips, the delivery and exit carriers, and the reserve.
fn reserved_kas(s: &AnyState) -> (bool, i128) {
    let r = |x: &[i64]| x.iter().map(|v| (*v).max(0) as i128).sum::<i128>();
    match s {
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => (false, r(&[x.refund_tip, x.reserve, x.delivery_carrier])),
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => (false, r(&[x.refund_tip, x.keeper_tip, x.delivery_carrier])),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => {
            (false, r(&[x.refund_tip, x.keeper_tip, x.delivery_carrier, x.exit_carrier]))
        }
        _ => (true, 0),
    }
}

/// The free token holdings of the maker that the indexer lists (`GET /v1/token-utxos?owner=`).
///
/// The result is, per token, the base units and the number of UTXOs.
fn indexer_holdings(indexer: &str, maker: &[u8; 32]) -> Result<BTreeMap<[u8; 32], (i128, usize)>, String> {
    let mut out: BTreeMap<[u8; 32], (i128, usize)> = BTreeMap::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let mut path = format!("/v1/token-utxos?owner={}&limit=500", to_hex(maker));
        if let Some(c) = &cursor {
            path.push_str(&format!("&cursor={c}"));
        }
        let v = http::get_ok(indexer, &path, data::INDEXER_TIMEOUT)?;
        for it in v.get("items").and_then(Value::as_array).into_iter().flatten() {
            if it.get("spent").and_then(Value::as_bool) == Some(true)
                || it.get("role").and_then(Value::as_str).is_some_and(|r| r != "owned")
            {
                continue;
            }
            let Some(t) = hex32(it.get("token")) else { continue };
            let owned = it
                .get("state")
                .and_then(|s| serde_json::from_value::<TokenState>(s.clone()).ok())
                .is_none_or(|s| s.is_user() && s.owner() == *maker);
            if !owned {
                continue;
            }
            let e = out.entry(t).or_default();
            e.0 += i64_of(it.get("amount")).unwrap_or(0) as i128;
            e.1 += 1;
        }
        match v.get("next_cursor").and_then(Value::as_str) {
            Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

/// Runs `kob balances`.
pub fn run_balances(args: &[String]) -> i32 {
    let (p, c) = match ctx(args, &["--maker"], &[], BALANCES_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if let Some(x) = p.pos.first() {
        return bail(format!("unexpected argument `{x}`"), 2);
    }
    if let Err(e) = maker_of(&p, &c, args) {
        return bail(e, 2);
    }
    print_view(&c, balances_view(&c, &p, args))
}

/// The balances view: its JSON and its lines.
pub fn balances_view(c: &Common, p: &Parsed, args: &[String]) -> Result<(Value, Vec<String>), String> {
    {
        let maker = maker_of(p, c, args)?;
        let node = c.need_node()?;
        let indexer = c.need_indexer()?;
        let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(indexer))?;
        let free_kas: i128 = data::funding(node, &c.network, &maker)?.iter().map(|u| u.utxo.amount as i128).sum();
        let orders = fetch_orders(indexer, &maker, None, false)?;
        let (held, skipped) = escrowed(&orders);
        let listed = indexer_holdings(indexer, &maker)?;
        let mut tokens: Vec<[u8; 32]> = held.keys().chain(listed.keys()).copied().collect();
        tokens.sort();
        tokens.dedup();
        let mut rows = vec![];
        let mut locked = 0i128;
        let order_scale = |cov: &[u8; 32]| data::order_scale(indexer, cov);
        let remembered = |cov: &[u8; 32]| crate::recent::first_order_decimals(&c.network, cov);
        let open = market::OpenToken { remembered: Some(&remembered), ..market::OpenToken::new(&c.network, &order_scale) };
        for t in tokens {
            let declared = declared_ticker(indexer, &t, &reg, &idx);
            let u = unit_of(&t, &reg, &idx, declared.as_deref());
            // Free tokens are confirmed on the node when the market of the token is known, else the
            // figure of the indexer is used.
            let resolved: Option<TokenMarket> = market::resolve_with(&to_hex(&t), &reg, &idx, Some(&open)).ok();
            let (free, verified, utxos) = match &resolved {
                Some(m) => {
                    let us = data::token_utxos(indexer, node, &c.network, m, &maker)?;
                    let usable = super::plan::usable_token_utxos(m, &maker, &us);
                    (usable.iter().map(|x| x.state.amount() as i128).sum::<i128>(), true, usable.len())
                }
                None => listed.get(&t).map(|x| (x.0, false, x.1)).unwrap_or((0, false, 0)),
            };
            let e = held.get(&t).cloned().unwrap_or_default();
            locked += e.kas_escrow + e.kas_carriers;
            rows.push(json!({
                "token": to_hex(&t), "ticker": unit_of(&t, &reg, &idx, None).0.ticker, "declared_ticker": declared.clone(),
                "decimals": u.1.then_some(u.0.decimals),
                "free": free.to_string(), "free_utxos": utxos, "free_verified": verified,
                "in_orders": e.custody.to_string(), "bought": e.bought.to_string(), "strays": e.strays.to_string(),
                "kas_escrow": e.kas_escrow.to_string(), "kas_carriers": e.kas_carriers.to_string(),
                "kas_locked": (e.kas_escrow + e.kas_carriers).to_string(), "orders": e.orders,
                "text": {
                    "name": view_name(&t, &u.0.ticker, declared.as_deref()),
                    "free": amount_text(free, &u), "in_orders": amount_text(e.custody, &u), "bought": amount_text(e.bought, &u),
                    "strays": amount_text(e.strays, &u),
                    "kas_locked": units::kas(e.kas_escrow + e.kas_carriers),
                },
            }));
        }
        let out = json!({
            "maker": to_hex(&maker),
            "kas": {"free": free_kas.to_string(), "locked": locked.to_string(), "total": (free_kas + locked).to_string()},
            "tokens": rows.clone(),
            "skipped": skipped.iter().map(|(c, r)| json!({"covenant_id": c, "reason": r})).collect::<Vec<_>>(),
        });
        let mut lines = vec![
            format!("balances of {} (network {})", to_hex(&maker), c.network),
            format!(
                "  KAS: free {}, locked in orders {}, total {}",
                units::kas(free_kas),
                units::kas(locked),
                units::kas(free_kas + locked)
            ),
        ];
        for r in rows.iter() {
            let t = &r["text"];
            let s = |v: &Value| v.as_str().unwrap_or("").to_string();
            let n = |v: &Value| v.as_u64().unwrap_or(0);
            let bought = if s(&r["bought"]) == "0" {
                String::new()
            } else {
                format!(", bought {} held by orders (kob claim)", s(&t["bought"]))
            };
            lines.push(format!(
                "  {}: free {}{} in {} UTXO{}, in orders {}{bought}, strays {}, KAS locked {} ({} order{})",
                s(&t["name"]),
                s(&t["free"]),
                if r["free_verified"].as_bool() == Some(true) { "" } else { " (indexer, not confirmed)" },
                n(&r["free_utxos"]),
                if n(&r["free_utxos"]) == 1 { "" } else { "s" },
                s(&t["in_orders"]),
                s(&t["strays"]),
                s(&t["kas_locked"]),
                n(&r["orders"]),
                if n(&r["orders"]) == 1 { "" } else { "s" },
            ));
        }
        for (cov, why) in &skipped {
            lines.push(format!("  not counted: {cov} ({why})"));
        }
        Ok((out, lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_lines_and_escrowed_balances() {
        let book = Book {
            asks: vec![Level { price: 250, amount: 5, orders: 2, tip: 0 }, Level { price: 260, amount: 7, orders: 1, tip: 0 }],
            bids: vec![Level { price: 245, amount: 3, orders: 1, tip: 0 }],
            ..Default::default()
        };
        let l = book_lines(&book, &|p| format!("{p}p"), &|a| format!("{a}a"), Some(248));
        let row = |s: &str| l.iter().position(|x| x.starts_with("    ") && x.contains(s)).unwrap();
        let spread = l.iter().position(|x| x.contains("spread 5p (best ask 250p, best bid 245p)")).unwrap();
        assert!(row("260p") < row("250p") && row("250p") < spread && spread < row("245p"), "{l:?}");
        assert!(l.last().unwrap().contains("last fill: 248p"));
        assert!(!is_crossed(&book));
        // A crossed book, with the best bid above the best ask, says by how much and never shows a
        // negative spread.
        let crossed = Book {
            asks: vec![Level { price: 250, amount: 5, orders: 1, tip: 0 }],
            bids: vec![Level { price: 260, amount: 3, orders: 1, tip: 0 }],
            ..Default::default()
        };
        assert!(is_crossed(&crossed));
        let l = book_lines(&crossed, &|p| format!("{p}p"), &|a| format!("{a}a"), None);
        assert!(l.iter().any(|x| x.contains("crossed by 10p (best ask 250p, best bid 260p)")), "{l:?}");
        assert!(!l.iter().any(|x| x.contains("spread")), "{l:?}");
        // A locked book, with bid equal to ask, has a spread of zero and is not crossed.
        let locked = Book {
            asks: vec![Level { price: 250, amount: 5, orders: 1, tip: 0 }],
            bids: vec![Level { price: 250, amount: 3, orders: 1, tip: 0 }],
            ..Default::default()
        };
        assert!(!is_crossed(&locked));
        // A terminal order and one without a proven state are skipped.
        let (by, skipped) = escrowed(&[
            json!({"covenant_id": "aa", "status": "filled"}),
            json!({"covenant_id": "bb", "status": "open", "state_known": false}),
        ]);
        assert!(by.is_empty());
        assert_eq!(skipped, vec![("aa".to_string(), "terminal"), ("bb".to_string(), "state-unknown")]);
    }

    /// An unbound curve (placed in its token's genesis, `tokenCovId` zero) counts under its custody's token, as the view
    /// names it: in orders, its KAS locked and one order (TN10: `kob balances` showed 800 in a curve as orders 0).
    #[test]
    fn an_unbound_curve_counts_under_its_custody_token() {
        let tok = [7u8; 32];
        let c = kob_protocol::state::CurveAskState {
            maker: [1; 32],
            token_cov_id: [0; 32],
            token_tpl_hash: [2; 32],
            tpl_prefix_len: 1,
            tpl_suffix_len: 1,
            scale: 1,
            min_fill: 1,
            tip: 0,
            active_from: 0,
            level_size: 100,
            total: 800,
            amount_left: 800,
            extension_commitment: [0; 32],
            table: vec![10; 8],
        };
        let view = json!({
            "covenant_id": "cc", "status": "open", "state_known": true, "token": to_hex(&tok),
            "state": serde_json::to_value(AnyState::KobCurveAsk(c)).unwrap(),
            "current": {"value": "100000000"},
            "custody": {"utxo": {"token": to_hex(&tok), "amount": "800", "value": "20000000", "spent": false}},
        });
        let (by, skipped) = escrowed(&[view]);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(by.keys().collect::<Vec<_>>(), [&tok], "nothing under the zero id");
        let e = &by[&tok];
        assert_eq!((e.custody, e.orders, e.kas_carriers, e.kas_escrow), (800, 1, 120_000_000, 0));
    }

    fn facts(cov: u8, ticker: &str, declared: bool) -> TokenFacts {
        TokenFacts {
            cov: [cov; 32],
            ticker: ticker.into(),
            decimals: None,
            tpl_hash: None,
            ext: None,
            tick: None,
            genesis_verified: false,
            not_tradable: None,
            market_scale: None,
            declared,
            program_ambiguous: false,
        }
    }

    /// The name a view gives a token: what `kob balances` and `kob book` print for it.
    fn shown(cov: u8, reg: &[TokenFacts], idx: &[TokenFacts], symbol: Option<&str>) -> (String, String, Option<String>) {
        let d = |_: &[u8; 32]| Some(market::Declared { decimals: Some(2), symbol: symbol.map(str::to_owned), name: None });
        let declared = declared_ticker_with(&[cov; 32], reg, idx, &d);
        let u = unit_of(&[cov; 32], reg, idx, declared.as_deref());
        (view_name(&[cov; 32], &u.0.ticker, declared.as_deref()), u.0.ticker, declared)
    }

    #[test]
    fn a_registry_ticker_wins_over_a_declared_one() {
        let reg = [facts(1, "REG", false)];
        let idx = [facts(1, "", true)];
        assert_eq!(shown(1, &reg, &idx, Some("FAKE")), ("REG".into(), "REG".into(), None));
        // The indexer's ticker too.
        assert_eq!(shown(1, &[], &[facts(1, "IDX", true)], Some("FAKE")), ("IDX".into(), "IDX".into(), None));
    }

    #[test]
    fn a_declared_ticker_is_shown_with_its_marker_and_the_full_id() {
        let id = "02".repeat(32);
        let want = (format!("DEC ({id}, declared on chain, unverified)"), "DEC".to_string(), Some("DEC".to_string()));
        // A token the indexer marks declared, and one it does not list (held or in an order).
        assert_eq!(shown(2, &[], &[facts(2, "", true)], Some("DEC")), want);
        assert_eq!(shown(2, &[], &[], Some("DEC")), want);
    }

    #[test]
    fn without_a_ticker_or_declaration_the_full_id_is_shown() {
        let id = "03".repeat(32);
        let none = (id.clone(), id.clone(), None);
        // No declaration.
        assert_eq!(shown(3, &[], &[facts(3, "", true)], None), none);
        // The indexer lists it as not declared: no declaration is asked.
        assert_eq!(shown(3, &[], &[facts(3, "", false)], Some("DEC")), none);
        // A symbol that cannot be a ticker (a unit word, not ASCII, a control character).
        for bad in ["tokens", "Ünï", "A\u{1b}[31m"] {
            assert_eq!(shown(3, &[], &[], Some(bad)), none, "{bad:?}");
        }
    }
}
