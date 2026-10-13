//! `kob place <kind>`: a new order, planned exactly like the web wallet's order ticket (`plan.rs`).

use kob_protocol::json::to_hex;
use kob_protocol::state::AnyState;
use serde_json::json;

use super::data;
use super::flow::{finish, Ready};
use super::market::{self, TokenMarket};
use super::opts::{parse, Common, Parsed, COMMON_HELP};
use super::plan::{
    plan_order, Activation, CondIntent, CondKind, Crossing, Env, IfdIntent, ImmediateIntent, Intent, Lifetime, LimitIntent,
    MarketIntent, PairPart, Plan, Side,
};
use super::show::Units;
use super::units::{
    self, parse_bps, parse_duration_daa, parse_kas, parse_kas_price, parse_pair_price, parse_token_amount, parse_when, When,
};

/// Help of `kob place`.
pub const USAGE: &str = "\
kob place: place a new order (the web wallet's planner: same defaults, same checks, same transaction)

USAGE
  kob place <KIND> --token TOKEN --side buy|sell --amount AMOUNT [options]

KINDS
  limit          --price P [--gtd WHEN | --day] [--active-from WHEN] [--crossing auction|limit|reject] [--max-fills N]
  ioc | fok      --price P [--life DURATION] [--active-from WHEN]
  market         [--slippage BPS] [--all-or-nothing]          an IOC auction from the best price to the bound
  stop-market    --stop P [--slippage BPS]
  stop-limit     --stop P --limit P
  take-profit    --take-profit P
  oco            --take-profit P --stop P [--limit P]
  ifd            --price P (the entry) and --take-profit P or --stop P (the exit); --side is the entry's side:
                 buy = buy first and sell the exit later, sell = sell first and buy back; [--prefund P] (sell first)
  ifo            as ifd with both --take-profit and --stop
  pair-limit     --quote TOKEN_B --price P (B per whole TOKEN) [same options as limit]

AMOUNTS (always with a unit; never rounded)
  AMOUNT         12.5TUSD or 12.5tok (whole tokens, up to the token's decimals), 1250000000base (base units)
  P              KAS per whole token: 0.25KAS or 25000000sompi; a pair: B per whole A, e.g. 2500TUSD or 2500tok
  --tip T        priority tip, KAS per whole token (default 0): a sell receives price - tip, a buy pays price + tip
  --exit-tip T   the if-done exit's tip
  --min-fill A   smallest fill (default: worth 10 KAS at the limit; IOC / FOK / market 1 base unit; if-done ceil(amount/4))
  --min-touch A  stop trigger threshold (default: max(min fill, 25% of the amount))
  --carrier KAS  KAS on every covenant UTXO of the order (default 2KAS; at least 0.08KAS and the token program's floor)
  WHEN           2026-10-20T00:00Z (UTC) or daa:N;   DURATION 30s, 5m or 300daa;   BPS 3% or 300bps

SAFETY
  --ack-no-guards       place even when the indexer could not answer for the self-trade / book checks
  --ack-market-start    accept a market auction start the CLI cannot check against an independent reference
  GTC orders end after 90 days idle (renew with `kob replace <id> --renew`).

";

const VALUE_FLAGS: &[&str] = &[
    "--token",
    "--quote",
    "--side",
    "--amount",
    "--price",
    "--tip",
    "--min-fill",
    "--gtd",
    "--active-from",
    "--crossing",
    "--max-fills",
    "--life",
    "--slippage",
    "--stop",
    "--limit",
    "--take-profit",
    "--min-touch",
    "--exit-tip",
    "--prefund",
    "--carrier",
];
const BOOL_FLAGS: &[&str] = &["--day", "--all-or-nothing", "--ack-no-guards", "--ack-market-start"];

/// The least carrier `--carrier` accepts: a token or order output carries a covenant id (KIP-9 storage plurality 2), so
/// below 0.08 KAS its storage mass alone exceeds the block limit.
pub const MIN_CARRIER: u64 = 8_000_000;

fn side_of(p: &Parsed) -> Result<Side, String> {
    match p.get("--side") {
        Some("buy") => Ok(Side::Buy),
        Some("sell") => Ok(Side::Sell),
        Some(o) => Err(format!("--side must be buy or sell, got `{o}`")),
        None => Err("--side buy|sell is required".into()),
    }
}

fn when_of(p: &Parsed, k: &str) -> Result<Option<When>, String> {
    p.get(k).map(|v| parse_when(v, k)).transpose()
}

fn lifetime_of(p: &Parsed) -> Result<Lifetime, String> {
    match (when_of(p, "--gtd")?, p.has("--day")) {
        (Some(_), true) => Err("--gtd and --day exclude each other".into()),
        (Some(When::Unix(u)), _) => Ok(Lifetime::GtdUnix(u as i64)),
        (Some(When::Daa(d)), _) => Ok(Lifetime::GtdDaa(d as i64)),
        (None, true) => Ok(Lifetime::Day),
        (None, false) => Ok(Lifetime::Gtc),
    }
}

fn activation_of(p: &Parsed) -> Result<Option<Activation>, String> {
    Ok(when_of(p, "--active-from")?.map(|w| match w {
        When::Unix(u) => Activation::Unix(u as i64),
        When::Daa(d) => Activation::Daa(d as i64),
    }))
}

/// The intent of `kind` from the arguments (`quote`: a pair's B).
pub fn intent_of(kind: &str, p: &Parsed, m: &TokenMarket, quote: Option<&TokenMarket>) -> Result<Intent, String> {
    let side = side_of(p)?;
    let unit = m.unit();
    let amount = parse_token_amount(p.get("--amount").ok_or("--amount is required")?, &unit, "--amount")?;
    let opt_amount = |k: &str| p.get(k).map(|v| parse_token_amount(v, &unit, k)).transpose();
    let price_of = |k: &str| -> Result<Option<i64>, String> {
        match (p.get(k), quote) {
            (None, _) => Ok(None),
            (Some(v), Some(b)) => parse_pair_price(v, &unit, m.scale, &b.unit(), k).map(Some),
            (Some(v), None) => parse_kas_price(v, &unit, m.scale, k, false).map(Some),
        }
    };
    let need = |k: &str| -> Result<i64, String> { price_of(k)?.ok_or_else(|| format!("{kind} needs {k}")) };
    let tip_of = |k: &str| -> Result<i64, String> {
        p.get(k).map(|v| parse_kas_price(v, &unit, m.scale, k, true)).transpose().map(Option::unwrap_or_default)
    };
    let tip = tip_of("--tip")?;
    let min_fill = opt_amount("--min-fill")?;
    let max_fills = p
        .get("--max-fills")
        .map(|v| v.parse::<i64>().map_err(|_| format!("--max-fills must be an integer, got `{v}`")))
        .transpose()?;
    let crossing = match p.get("--crossing") {
        None | Some("auction") => Crossing::Auction,
        Some("limit") => Crossing::Limit,
        Some("reject") => Crossing::Reject,
        Some(o) => return Err(format!("--crossing must be auction, limit or reject, got `{o}`")),
    };
    let slip = p.get("--slippage").map(|v| parse_bps(v, "--slippage")).transpose()?;
    let unused = |ks: &[&str]| -> Result<(), String> {
        for k in ks {
            if p.vals.contains_key(*k) || p.has(k) {
                return Err(format!("{k} does not apply to {kind}"));
            }
        }
        Ok(())
    };
    if quote.is_some() && kind != "pair-limit" {
        return Err("--quote applies to pair-limit only".into());
    }
    let limit = |i: Option<LimitIntent>| -> Result<LimitIntent, String> {
        if let Some(i) = i {
            return Ok(i);
        }
        unused(&[
            "--life",
            "--slippage",
            "--stop",
            "--limit",
            "--take-profit",
            "--min-touch",
            "--exit-tip",
            "--prefund",
            "--all-or-nothing",
        ])?;
        Ok(LimitIntent {
            side,
            amount,
            price: need("--price")?,
            tip,
            min_fill,
            lifetime: lifetime_of(p)?,
            active_from: activation_of(p)?,
            crossing,
            max_fills,
        })
    };
    Ok(match kind {
        "limit" => Intent::Limit(limit(None)?),
        "pair-limit" => {
            if quote.is_none() {
                return Err("pair-limit needs --quote TOKEN_B".into());
            }
            Intent::PairLimit(limit(None)?)
        }
        "ioc" | "fok" => {
            unused(&[
                "--gtd",
                "--day",
                "--crossing",
                "--max-fills",
                "--slippage",
                "--stop",
                "--limit",
                "--take-profit",
                "--prefund",
                "--exit-tip",
            ])?;
            Intent::Immediate(ImmediateIntent {
                fok: kind == "fok",
                side,
                amount,
                price: need("--price")?,
                tip,
                min_fill,
                life_daa: p.get("--life").map(|v| parse_duration_daa(v, "--life")).transpose()?,
                active_from: activation_of(p)?,
            })
        }
        "market" => {
            unused(&[
                "--price",
                "--gtd",
                "--day",
                "--crossing",
                "--max-fills",
                "--life",
                "--stop",
                "--limit",
                "--take-profit",
                "--prefund",
                "--exit-tip",
                "--active-from",
            ])?;
            Intent::Market(MarketIntent { side, amount, tip, min_fill, slippage_bps: slip, all_or_nothing: p.has("--all-or-nothing") })
        }
        "stop-market" | "stop-limit" | "take-profit" | "oco" => {
            unused(&["--price", "--crossing", "--life", "--prefund", "--exit-tip", "--all-or-nothing"])?;
            let k = match kind {
                "stop-market" => CondKind::StopMarket,
                "stop-limit" => CondKind::StopLimit,
                "take-profit" => CondKind::TakeProfit,
                _ => CondKind::Oco,
            };
            Intent::Cond(CondIntent {
                kind: k,
                side,
                amount,
                tip,
                min_fill,
                stop: price_of("--stop")?,
                limit: price_of("--limit")?,
                take_profit: price_of("--take-profit")?,
                slip_bps: slip,
                min_touch: opt_amount("--min-touch")?,
                lifetime: lifetime_of(p)?,
                active_from: activation_of(p)?,
                max_fills,
            })
        }
        "ifd" | "ifo" => {
            unused(&["--crossing", "--life", "--slippage", "--max-fills", "--all-or-nothing", "--min-touch"])?;
            let (tp, stop) = (price_of("--take-profit")?, price_of("--stop")?);
            match (kind, tp.is_some(), stop.is_some()) {
                ("ifd", true, true) | ("ifd", false, false) => {
                    return Err("ifd needs exactly one exit leg: --take-profit or --stop (both: ifo)".into())
                }
                ("ifo", a, b) if !(a && b) => return Err("ifo needs both --take-profit and --stop".into()),
                _ => {}
            }
            if p.get("--prefund").is_some() && side == Side::Buy {
                return Err("--prefund applies to a sell-first entry only".into());
            }
            Intent::Ifd(IfdIntent {
                side,
                amount,
                tip,
                min_fill,
                price: need("--price")?,
                take_profit: tp,
                stop,
                stop_limit: price_of("--limit")?,
                exit_tip: tip_of("--exit-tip")?,
                prefund: p.get("--prefund").map(|v| parse_kas_price(v, &unit, m.scale, "--prefund", true)).transpose()?,
                lifetime: lifetime_of(p)?,
                active_from: activation_of(p)?,
            })
        }
        other => return Err(format!("unknown order kind `{other}` (see `kob place --help`)")),
    })
}

/// A state tip (sompi per `scale` base units) as KAS per whole token (a token of more than 9 decimals has a scale of 10^9
/// base units, less than one whole token).
pub fn tip_per_whole(tip: i64, m: &TokenMarket) -> String {
    units::kas(tip as i128 * 10i128.pow(m.decimals) / m.scale as i128)
}

/// Plan lines of a placed order.
pub fn plan_lines(plan: &Plan, m: &TokenMarket, quote: Option<&TokenMarket>) -> Vec<String> {
    let Some(d) = &plan.disclosure else { return vec![] };
    let u = m.unit();
    let (qd, qn) = match quote {
        Some(b) => (b.decimals, if b.ticker.is_empty() { "B".to_string() } else { b.ticker.clone() }),
        None => (8, "KAS".to_string()),
    };
    let price = |p: i64| units::price_per_whole(p, &u, m.scale, qd, &qn);
    let mut v = vec![];
    for (k, s) in plan.states.iter().enumerate() {
        v.push(format!("{} {}", if k == 0 { "order:" } else { "committed exit:" }, s.template_id().name()));
    }
    v.push(format!("side: {} {}", d.side.name(), m.name()));
    v.push(format!("amount: {}", units::tokens(d.amount as i128, &u)));
    if let Some(p) = d.limit_price {
        v.push(format!("limit / worst price: {}", price(p)));
    }
    if let Some(p) = d.expected_price {
        v.push(format!("expected start: {}", price(p)));
    }
    if quote.is_none() {
        if let Some(a) = d.all_in_price {
            v.push(format!(
                "all-in price: {} ({} {} the tip of {} per token)",
                price(a),
                if d.side == Side::Sell { "you receive the price" } else { "you pay the price" },
                if d.side == Side::Sell { "minus" } else { "plus" },
                tip_per_whole(d.tip, m)
            ));
        }
        if let Some(t) = d.all_in_total {
            v.push(format!(
                "{} for the whole amount: {}",
                if d.side == Side::Sell { "you receive at least" } else { "you pay at most" },
                units::kas(t as i128)
            ));
        }
    } else if let (Some(t), Some(b)) = (d.all_in_total, quote) {
        v.push(format!(
            "{} for the whole amount: {}",
            if d.side == Side::Sell { "you receive at least" } else { "you pay at most" },
            units::tokens(t as i128, &b.unit())
        ));
        v.push(format!("tip: {} per whole {} (KAS, prefunded)", tip_per_whole(d.tip, m), m.name()));
    }
    v.push(format!("minimum fill: {}", units::tokens(d.min_fill as i128, &u)));
    if let Some(t) = d.min_touch {
        v.push(format!("trigger threshold: {}", units::tokens(t as i128, &u)));
    }
    let exp = match d.expiry_kind {
        "gtc" => "GTC (refundable after 90 days idle)".to_string(),
        "day" => format!("day order until {}", units::fmt_utc(d.deadline.unwrap_or(0) as u64)),
        k => k.to_ascii_uppercase(),
    };
    v.push(format!("expiry: {exp}, DAA {} (around {})", d.expiry_daa, units::fmt_utc(d.expiry_unix.max(0) as u64)));
    if let Some((daa, unix)) = d.activates_at {
        v.push(format!("active from: DAA {daa} (around {})", units::fmt_utc(unix.max(0) as u64)));
    }
    for c in &d.carriers {
        v.push(format!(
            "{}: {}{}{}",
            c.kind,
            units::kas(c.amount),
            if c.count != 1 { format!(" x {}", c.count) } else { String::new() },
            if c.kept { " (stays in your wallet)" } else { "" }
        ));
    }
    v.push(format!("KAS locked in the order (returned on fill, cancel or refund): {}", units::kas(d.kas_locked)));
    if d.tokens_escrowed > 0 {
        v.push(format!("tokens into custody: {}", units::tokens(d.tokens_escrowed as i128, &u)));
    }
    if let (true, Some(b)) = (d.quote_escrowed > 0, quote) {
        v.push(format!("{} into custody: {}", b.name(), units::tokens(d.quote_escrowed as i128, &b.unit())));
    }
    v.push(format!("refund tip: {} (a keeper earns it refunding the order after it ends)", units::kas(d.refund_tip as i128)));
    if d.keeper_tip > 0 {
        v.push(format!("keeper tip per arm / trail: {}", units::kas(d.keeper_tip as i128)));
    }
    v
}

/// The planning environment of `kob place`, read from the node and the indexer.
pub fn env_for(c: &Common, p: &Parsed, maker: [u8; 32]) -> Result<(Env, TokenMarket, Option<TokenMarket>), String> {
    let node = c.need_node()?.to_string();
    let indexer = c.need_indexer()?.to_string();
    let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(&indexer))?;
    let a = market::resolve(p.get("--token").ok_or("--token is required")?, &reg, &idx)?;
    let b = match p.get("--quote") {
        Some(q) => Some(market::resolve(q, &reg, &idx)?),
        None => None,
    };
    let clock = data::clock(&node)?;
    let funding = data::funding(&node, &c.network, &maker)?;
    let side = side_of(p)?;
    let mut guards = vec![];
    let carrier = match p.get("--carrier") {
        Some(v) => {
            let k = parse_kas(v, "--carrier", false)?;
            if k < MIN_CARRIER {
                return Err(format!(
                    "--carrier must be at least 0.08KAS (the covenant output minimum), got {}",
                    units::kas(k as i128)
                ));
            }
            Some(k)
        }
        None => None,
    };
    // a sell (and a sell-first entry) holds tokens of A; a pair buy holds B (below)
    let needs_a = side == Side::Sell;
    let token_utxos = if needs_a { data::token_utxos(&indexer, &node, &c.network, &a, &maker)? } else { vec![] };
    // an unlisted KCC-20 token's extension commitment is the indexer's: the wallet's own holdings of the token (their states
    // confirmed on the node) are a reference without the indexer, and one of another commitment blocks (the web app's
    // open-token-extension-mismatch); a buy would otherwise pin a commitment that only a look-alike genesis can fill
    for m in [Some(&a), b.as_ref()].into_iter().flatten() {
        if m.family != kob_protocol::Family::Kcc20 || m.listing != market::Listing::Unlisted {
            continue;
        }
        let held =
            if m.cov == a.cov && needs_a { token_utxos.clone() } else { data::token_utxos(&indexer, &node, &c.network, m, &maker)? };
        if let Some(u) = held.iter().find(|u| u.state.extension() != m.ext) {
            return Err(format!(
                "{}: the indexer names extension commitment {}, but your token UTXO {}:{} (checked on the node) has {}: the token \
                 has more than one commitment; refused (check the token's genesis)",
                m.name(),
                to_hex(&m.ext),
                to_hex(&u.utxo.transaction_id),
                u.utxo.index,
                to_hex(&u.state.extension())
            ));
        }
    }
    let (book, own, last_fill, pair) = match &b {
        None => {
            let book = data::book(&indexer, &a).unwrap_or_else(|_| {
                guards.push("book".to_string());
                Default::default()
            });
            let own = data::own_orders(&indexer, &a, &maker).unwrap_or_else(|_| {
                guards.push("own-orders".to_string());
                vec![]
            });
            (book, own, data::last_fill(&indexer, &a).ok().flatten(), None)
        }
        Some(bm) => {
            let book = data::pair_book(&indexer, &a, bm).unwrap_or_else(|_| {
                guards.push("book".to_string());
                Default::default()
            });
            let own = data::own_pair_orders(&indexer, &a, bm, &maker).unwrap_or_else(|_| {
                guards.push("own-orders".to_string());
                vec![]
            });
            let kas_a =
                data::book(&indexer, &a)
                    .ok()
                    .and_then(|bk| data::firm_reference(&bk, a.scale))
                    .or(data::last_fill(&indexer, &a).ok().flatten());
            let quote_utxos = if side == Side::Buy { data::token_utxos(&indexer, &node, &c.network, bm, &maker)? } else { vec![] };
            (book, own, None, Some(PairPart { quote: bm.clone(), quote_utxos, kas_per_whole_a: kas_a }))
        }
    };
    Ok((
        Env {
            market: a.clone(),
            maker,
            clock,
            book,
            own,
            guards_unavailable: guards,
            guards_acknowledged: p.has("--ack-no-guards"),
            last_fill,
            market_start_ack: p.has("--ack-market-start"),
            funding,
            token_utxos,
            fee_rate: c.fee_rate,
            carrier,
            pair,
        },
        a,
        b,
    ))
}

/// Units of the plan's tokens.
pub fn units_of(a: &TokenMarket, b: Option<&TokenMarket>) -> Units {
    let mut u = Units::new();
    u.insert(a.cov, a.unit());
    if let Some(b) = b {
        u.insert(b.cov, b.unit());
    }
    u
}

/// The ready-to-show plan of a planned order.
pub fn ready(plan: &Plan, title: String, a: &TokenMarket, b: Option<&TokenMarket>) -> Option<Ready> {
    let built = plan.built.clone()?;
    let cov = built.covenants.first().map(|c| to_hex(&c.covenant_id)).unwrap_or_default();
    Some(Ready {
        title,
        lines: plan_lines(plan, a, b),
        issues: plan.issues.clone(),
        file: format!("{cov}.place.json"),
        json: json!({
            "states": plan.states.iter().map(|s: &AnyState| serde_json::to_value(s).unwrap_or_default()).collect::<Vec<_>>(),
            "request": plan.request.as_ref().map(|r| serde_json::to_value(r).unwrap_or_default()),
        }),
        built,
    })
}

/// What `kob place` signs: the settings, the key, the plan and the markets.
type Planned = (Common, crate::common::Signer, Plan, TokenMarket, Option<TokenMarket>);

/// Run `kob place`.
pub fn run(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}{COMMON_HELP}");
        return if args.is_empty() { 2 } else { 0 };
    }
    let p = match parse(args, VALUE_FLAGS, BOOL_FLAGS) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let Some(kind) = p.pos.first().cloned() else {
        eprintln!("error: the order kind is missing (see `kob place --help`)");
        return 2;
    };
    if p.pos.len() > 1 {
        eprintln!("error: unexpected argument `{}`", p.pos[1]);
        return 2;
    }
    let step = || -> Result<Planned, (i32, String)> {
        let c = Common::from(&p).map_err(|e| (2, e))?;
        let signer = c.signer(args).map_err(|e| (2, e))?;
        let (env, a, b) = env_for(&c, &p, signer.pubkey).map_err(|e| (1, e))?;
        let intent = intent_of(&kind, &p, &a, b.as_ref()).map_err(|e| (2, e))?;
        let mut plan = plan_order(&env, &intent);
        // a registry token that is not tradable is refused, an unlisted one flagged (`web/src/kob/decode.ts` token-unlisted)
        for i in [Some(&a), b.as_ref()].into_iter().flatten().filter_map(|m| m.listing_issue(true)) {
            plan.ok &= i.severity != super::plan::Severity::Error;
            plan.issues.push(i);
        }
        Ok((c, signer, plan, a, b))
    };
    let (c, signer, plan, a, b) = match step() {
        Ok(x) => x,
        Err((code, e)) => {
            eprintln!("error: {e}");
            return code;
        }
    };
    let side = p.get("--side").unwrap_or("");
    let title = format!("place {kind} {side} {}{}", a.name(), b.as_ref().map(|b| format!(" for {}", b.name())).unwrap_or_default());
    match ready(&plan, title, &a, b.as_ref()) {
        Some(r) if plan.ok || plan.built.is_some() => finish(&c, &signer, r, &units_of(&a, b.as_ref()), "this order"),
        _ => {
            super::flow::print_issues(&plan.issues, false);
            eprintln!("error: the order could not be planned: nothing signed");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::artifacts::TemplateId;

    /// The tip a pair order shows is the one typed, per whole token, also above 9 decimals (scale 10^9).
    #[test]
    fn a_tip_shows_per_whole_token_above_nine_decimals() {
        let a = TokenMarket::new([0x70; 32], "BIG", 12, TemplateId::Kcc20Ref, Some([0xee; 32]), None).unwrap();
        let tip = units::parse_kas_price("0.01KAS", &a.unit(), a.scale, "--tip", true).unwrap();
        assert_eq!(tip, 1_000);
        assert_eq!(tip_per_whole(tip, &a), units::kas(1_000_000));
        let small = TokenMarket::new([0x71; 32], "S", 3, TemplateId::Kcc20Ref, Some([0xee; 32]), None).unwrap();
        let tip = units::parse_kas_price("0.01KAS", &small.unit(), small.scale, "--tip", true).unwrap();
        assert_eq!(tip_per_whole(tip, &small), units::kas(1_000_000));
    }
}
