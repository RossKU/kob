//! The read-only trading commands: `kob book` (a token's KAS book or a pair book), `kob orders` (the maker's orders, as
//! "My orders" lists them) and `kob balances` (free KAS and tokens, and what the maker's live orders hold: custody,
//! strays, KAS locked; `web/src/kob/balances.ts`). Nothing is signed; no key is needed when `--maker` names the owner.

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

/// Help of `kob book`.
pub const BOOK_USAGE: &str = "\
kob book: a token's order book on KAS, or the book of a pair (the indexer's view; nothing is signed)

USAGE
  kob book <TOKEN> [--quote TOKEN_B] [--depth N] [--json]

  TOKEN is a ticker or a covenant id. Prices are KAS per whole token (a pair: TOKEN_B per whole TOKEN), levels
  aggregated by price, asks above the spread, bids below; the last fill closes the view.
";

/// Help of `kob orders`.
pub const ORDERS_USAGE: &str = "\
kob orders: the maker's orders (the indexer's view, as the web wallet's My orders lists them)

USAGE
  kob orders [--token TOKEN] [--all] [--maker KEY|ADDRESS] [--json]

  Live orders by default (--all: every status). The maker is --maker, else the key of --key-file / $KOB_KEY (only its
  public key is used). Each line: covenant id, kind, side, what is left, the price or legs, status and expiry.
";

/// Help of `kob balances`.
pub const BALANCES_USAGE: &str = "\
kob balances: free KAS and tokens of the maker, and what the maker's live orders hold

USAGE
  kob balances [--maker KEY|ADDRESS] [--json]

  free: the maker's own KAS UTXOs (node) and token UTXOs (indexer, each confirmed on the node when the token's program
  is known); in orders: tokens in the custody of live orders, stray tokens sent to them (only the maker's cancel
  recovers them), KAS locked (a buy's budget, carriers, tips and reserves).
";

fn bail(e: String, code: i32) -> i32 {
    eprintln!("error: {e}");
    code
}

/// Parsed arguments and settings of a read-only command (`--help` prints `usage`).
fn ctx(args: &[String], value_flags: &[&str], bool_flags: &[&str], usage: &str) -> Result<(Parsed, Common), i32> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{usage}{COMMON_HELP}");
        return Err(0);
    }
    let p = parse(args, value_flags, bool_flags).map_err(|e| bail(e, 2))?;
    let c = Common::from(&p).map_err(|e| bail(e, 2))?;
    Ok((p, c))
}

/// The owner a view is about: `--maker` (an x-only key or a P2PK address of the network), else the signing key's.
pub fn maker_of(p: &Parsed, c: &Common, argv: &[String]) -> Result<[u8; 32], String> {
    match p.get("--maker") {
        Some(m) => match super::actions::recipient(m, &c.network)? {
            (_, Some(k)) => Ok(k),
            _ => Err(format!("--maker `{m}` is not a P2PK address or an x-only key")),
        },
        None => c.signer(argv).map(|s| s.pubkey).map_err(|e| format!("{e} (or name the owner with --maker)")),
    }
}

/// Display facts of a token from the registry and the indexer lists (the short id alone when neither knows it).
fn unit_of(cov: &[u8; 32], reg: &[TokenFacts], idx: &[TokenFacts]) -> (TokenUnit, bool) {
    let f = reg.iter().find(|t| t.cov == *cov).or_else(|| idx.iter().find(|t| t.cov == *cov));
    match f {
        Some(t) => (
            TokenUnit {
                ticker: if t.ticker.is_empty() { show_id(cov, "") } else { t.ticker.clone() },
                decimals: t.decimals.unwrap_or(0),
            },
            t.decimals.is_some(),
        ),
        None => (TokenUnit { ticker: show_id(cov, ""), decimals: 0 }, false),
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

// ------------------------------------------------------------------------------------------------ book

/// The text lines of a book (asks highest first, the spread, bids highest first).
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

fn levels_json(levels: &[Level], price: &dyn Fn(i64) -> String) -> Value {
    json!(levels
        .iter()
        .map(
            |l| json!({"price": l.price.to_string(), "price_text": price(l.price), "amount": l.amount.to_string(), "orders": l.orders})
        )
        .collect::<Vec<_>>())
}

/// Print a view: its JSON with `--json`, else its lines.
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

/// Run `kob book`.
pub fn run_book(args: &[String]) -> i32 {
    let (p, c) = match ctx(args, &["--quote", "--depth"], &[], BOOK_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    print_view(&c, book_view(&c, &p))
}

/// The book view: its JSON and its lines.
pub fn book_view(c: &Common, p: &Parsed) -> Result<(Value, Vec<String>), String> {
    {
        let [key] = p.pos.as_slice() else { return Err("give exactly one token (ticker or covenant id)".into()) };
        let depth: u32 = match p.get("--depth") {
            Some(d) => d.parse().ok().filter(|d| (1..=500).contains(d)).ok_or(format!("--depth must be 1..500, got `{d}`"))?,
            None => 20,
        };
        let indexer = c.need_indexer()?;
        let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(indexer))?;
        // a token without a registry entry: its decimals from the scale of its orders (`kob place`'s open token)
        let order_scale = |cov: &[u8; 32]| data::order_scale(indexer, cov);
        let open = market::OpenToken { network: &c.network, order_scale: &order_scale, entered: None };
        let a = market::resolve_with(key, &reg, &idx, Some(&open))?;
        let b = p.get("--quote").map(|q| market::resolve_with(q, &reg, &idx, Some(&open))).transpose()?;
        let (book, last, quote_name): (Book, Option<i64>, String) = match &b {
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
        let ua = a.unit();
        let (qd, qn) = match &b {
            Some(bm) => (bm.decimals, if bm.ticker.is_empty() { "B".to_string() } else { bm.ticker.clone() }),
            None => (units::KAS_DECIMALS, "KAS".to_string()),
        };
        let price = |x: i64| units::price_per_whole(x, &ua, a.scale, qd, &qn);
        let amount = |x: i64| {
            format!("{} {}", units::fmt_decimal(x as i128, a.decimals), if a.ticker.is_empty() { "tokens" } else { &a.ticker })
        };
        let out = json!({
            "token": to_hex(&a.cov),
            "quote": b.as_ref().map(|x| to_hex(&x.cov)),
            "scale": a.scale,
            "asks": levels_json(&book.asks, &price),
            "bids": levels_json(&book.bids, &price),
            "best_ask": book.asks.first().map(|l| l.price.to_string()),
            "best_bid": book.bids.first().map(|l| l.price.to_string()),
            "last_fill": last.map(|l| l.to_string()),
        });
        let mut lines = vec![format!("book {} in {quote_name} per whole token (indexer {})", a.name(), http::shown(indexer))];
        lines.extend(book_lines(&book, &price, &amount, last));
        Ok((out, lines))
    }
}

// ------------------------------------------------------------------------------------------------ orders

/// The maker's orders of the indexer (`GET /v1/orders`, every page up to 20).
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

/// What one order trades, in words: its side, what is left, its price or legs (`order-facts.ts` in short).
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

/// Run `kob orders`.
pub fn run_orders(args: &[String]) -> i32 {
    let (p, c) = match ctx(args, &["--token", "--maker"], &["--all"], ORDERS_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if let Some(x) = p.pos.first() {
        return bail(format!("unexpected argument `{x}`"), 2);
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
                None => market::find(&reg, t)
                    .first()
                    .or(market::find(&idx, t).first())
                    .map(|f| f.cov)
                    .ok_or(format!("unknown token `{t}`"))?,
            }),
            None => None,
        };
        let items = fetch_orders(indexer, &maker, token.as_ref(), p.has("--all"))?;
        let mut rows = vec![];
        for o in &items {
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
            let ua = unit_of(&a, &reg, &idx);
            let ub = b.map(|b| unit_of(&b, &reg, &idx));
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
            rows.push(json!({
                "covenant_id": cov, "contract": contract, "market": market, "status": status, "what": what,
                "expiry_daa": expiry.filter(|e| *e < 1 << 60), "flags": flags,
                "value": o.get("current").and_then(|c| c.get("value")).cloned().unwrap_or(Value::Null),
            }));
        }
        let mut lines = vec![format!(
            "orders of {} ({}, indexer {})",
            to_hex(&maker),
            if p.has("--all") { "every status" } else { "live" },
            http::shown(indexer)
        )];
        if rows.is_empty() {
            lines.push("  (none)".into());
        }
        for r in &rows {
            let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
            let exp = r["expiry_daa"].as_i64().map(|e| format!(", expiry DAA {e}")).unwrap_or_default();
            let flags =
                r["flags"].as_array().map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            lines.push(format!(
                "  {} {} {}: {} [{}{}{}]",
                s("covenant_id"),
                s("contract"),
                s("market"),
                s("what"),
                s("status"),
                exp,
                if flags.is_empty() { String::new() } else { format!("; {flags}") }
            ));
        }
        Ok((json!({"maker": to_hex(&maker), "orders": rows}), lines))
    }
}

// ------------------------------------------------------------------------------------------------ balances

/// What the maker's live orders hold of one token (`EscrowedBalance`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Escrowed {
    /// Tokens in custody (base units).
    pub custody: i128,
    /// Stray tokens owned by live orders.
    pub strays: i128,
    /// KAS budget of buy-side orders.
    pub kas_escrow: i128,
    /// KAS locked that is not budget: carriers, tips, reserves.
    pub kas_carriers: i128,
    /// Live orders counted (under the order's token; a pair order under A).
    pub orders: usize,
}

/// An order the balances do not count: covenant id and why (`terminal`, `state-unknown`, `no-current-utxo`).
pub type Skipped = (String, &'static str);

const TERMINAL: &[&str] = &["filled", "cancelled", "refunded", "killed", "closed"];

/// Per token: what live orders hold (`escrowedBalancesDetailed` over indexer order views); the orders not counted
/// (terminal, state not proven, no current outpoint) are returned by covenant id with the reason.
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
        let token = s.token_cov_id();
        by.entry(token).or_default().orders += 1;
        let utxo_rows: Vec<&Value> = match o.get("pair").filter(|p| !p.is_null()) {
            Some(p) => p
                .get("custodies")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| c.get("utxo")).collect())
                .unwrap_or_default(),
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
        // a custody the view does not show counts what the state says it must hold
        for (t, amount) in s.custodies() {
            if !seen.contains(&t) {
                by.entry(t).or_default().custody += amount as i128;
            }
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
            // ask kinds and pair orders: the order UTXO is carriers, prefunds and tips, never a KAS trading budget
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

/// Whether an order sells, and the KAS of a buy's order UTXO that is not trading budget (its tips, delivery / exit
/// carriers and reserve).
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

/// The maker's free token holdings the indexer lists (`GET /v1/token-utxos?owner=`): per token, base units and UTXOs.
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

/// Run `kob balances`.
pub fn run_balances(args: &[String]) -> i32 {
    let (p, c) = match ctx(args, &["--maker"], &[], BALANCES_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if let Some(x) = p.pos.first() {
        return bail(format!("unexpected argument `{x}`"), 2);
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
        let open = market::OpenToken { network: &c.network, order_scale: &order_scale, entered: None };
        for t in tokens {
            let u = unit_of(&t, &reg, &idx);
            // free tokens: confirmed on the node when the token's market is known, else the indexer's figure
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
                "token": to_hex(&t), "ticker": u.0.ticker, "decimals": u.1.then_some(u.0.decimals),
                "free": free.to_string(), "free_utxos": utxos, "free_verified": verified,
                "in_orders": e.custody.to_string(), "strays": e.strays.to_string(),
                "kas_escrow": e.kas_escrow.to_string(), "kas_carriers": e.kas_carriers.to_string(),
                "kas_locked": (e.kas_escrow + e.kas_carriers).to_string(), "orders": e.orders,
                "text": {
                    "free": amount_text(free, &u), "in_orders": amount_text(e.custody, &u), "strays": amount_text(e.strays, &u),
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
            format!("balances of {}", to_hex(&maker)),
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
            lines.push(format!(
                "  {}: free {}{} in {} UTXO{}, in orders {}, strays {}, KAS locked {} ({} order{})",
                s(&r["ticker"]),
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
        // a terminal order and one without a proven state are skipped
        let (by, skipped) = escrowed(&[
            json!({"covenant_id": "aa", "status": "filled"}),
            json!({"covenant_id": "bb", "status": "open", "state_known": false}),
        ]);
        assert!(by.is_empty());
        assert_eq!(skipped, vec![("aa".to_string(), "terminal"), ("bb".to_string(), "state-unknown")]);
    }
}
