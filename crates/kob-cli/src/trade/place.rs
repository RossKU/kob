//! `kob place <kind>`: a new order, planned exactly like the web wallet's order ticket (`plan.rs`, `plan_pair.rs`).

use kob_protocol::json::to_hex;
use kob_protocol::state::AnyState;
use serde_json::json;

use super::data;
use super::flow::{finish, Ready};
use super::market::{self, TokenMarket};
use super::opts::{parse, Common, Parsed, COMMON_HELP};
use super::plan::{
    plan_order, Activation, CloseIntent, CondIntent, CondKind, Crossing, DutchIntent, Env, IfdIntent, IfdMore, ImmediateIntent,
    Intent, Issue, Lifetime, LimitIntent, MarketIntent, PairPart, Plan, ScheduleIntent, Severity, Side, StopKnobs, StreamingIntent,
    Trail,
};
use super::show::Units;
use super::units::{
    self, parse_bps, parse_duration_daa, parse_kas, parse_kas_price, parse_pair_price, parse_token_amount, parse_when, When,
};

/// Help of `kob place`.
pub const USAGE: &str = "\
kob place: place a new order (the web wallet's planner: same defaults, same checks, same transaction)

USAGE
  kob place <KIND> --token TOKEN [--quote TOKEN_B] --side buy|sell --amount AMOUNT [options]

KINDS
  limit          --price P [--gtd WHEN | --day] [--active-from WHEN] [--crossing auction|limit|reject] [--max-fills N]
  ioc | fok      --price P [--life DURATION] [--active-from WHEN]
  market         [--slippage BPS] [--all-or-nothing] [--floor-price P]
                                                              an IOC auction from the best price to the bound
  streaming      --displayed-price P --tolerance BPS [--all-or-nothing]
                                                              an IOC auction from the price you saw, within your tolerance
  close          [--amount AMOUNT] [--slippage BPS] [--all-or-nothing] [--floor-price P]
                                                              a market sell of the held tokens (no --amount: all of them)
  twap | dca     --price P --slice AMOUNT --interval DURATION [--price-end P [--slice-auction DURATION]]
                 [--gtd WHEN | --day] [--active-from WHEN] [dca: --max-fills N]
                                                              twap sells, dca buys (--side may be left out)
  dutch          --price P --price-end P --duration DURATION [--step DURATION] [--gtd WHEN | --day] [--active-from WHEN]
                                                              sell: a decaying price; buy: a rising bid
  stop-market    --stop P [--slippage BPS]
  stop-limit     --stop P --limit P
  trailing-stop  --stop P --trail-step P --trail-gap P [--trail-wait DURATION] [--trail-updates N] [--take-profit P]
                 [--slippage BPS]
  take-profit    --take-profit P
  oco            --take-profit P --stop P [--limit P]
                 every stop: [--min-touch AMOUNT] [--band DURATION] [--keeper-tip KAS] [--min-rest DURATION]
  ifd            --price P (the entry) and --take-profit P or --stop P (the exit); --side is the entry's side:
                 buy = buy first and sell the exit later, sell = sell first and buy back; [--prefund P] (sell first)
  ifo            as ifd with both --take-profit and --stop
  repeat-ifd     as ifd with --take-profit only: the entry re-arms after every take-profit [--repeat-count K] (none:
  repeat-ifo     unlimited within the 90-day bound); repeat-ifo adds the --stop that ends the repeat
                 if-done options: [--entry-stop P] (a stop entry), [--limit P] (the exit stop-limit), [--exit-tip T],
                 [--exit-min-fill AMOUNT] [--exit-min-touch AMOUNT] [--exit-slippage BPS], a trailing exit stop with
                 --trail-step P --trail-gap P [--trail-wait DURATION] [--trail-updates N]
  pair-limit     limit with --quote TOKEN_B (every kind takes --quote: the order trades TOKEN for TOKEN_B, a KobPair /
                 KobCondPair / KobIfdPair; prices are TOKEN_B per whole TOKEN, the tips stay KAS)

AMOUNTS (always with a unit; never rounded)
  AMOUNT         12.5TUSD or 12.5tok (whole tokens, up to the token's decimals), 1250000000base (base units)
  P              KAS per whole token: 0.25KAS or 25000000sompi; a pair: B per whole A, e.g. 2500TUSD or 2500tok
  --tip T        priority tip, KAS per whole token (default 0): a sell receives price - tip, a buy pays price + tip
  --exit-tip T   the if-done exit's tip
  --min-fill A   smallest fill (default: worth 10 KAS at the limit; IOC / FOK / market 1 base unit; if-done ceil(amount/4))
  --min-touch A  stop trigger threshold (default: max(min fill, 25% of the amount))
  --carrier KAS  KAS on every covenant UTXO of the order (default 2KAS; at least 0.08KAS and the token program's floor)
  --decimals N   the first order of a token without a registry entry and without orders: its decimals (0..=9; the scale
                 is 10^N). The chain records no decimals: give the ones the issuer chose. Once the token has orders, their
                 scale decides (a --decimals that disagrees is refused)
  WHEN           2026-10-20T00:00Z (UTC) or daa:N;   DURATION 30s, 5m or 300daa;   BPS 3% or 300bps

SAFETY
  --ack-no-guards       place even when the indexer could not answer for the self-trade / book checks
  --ack-market-start    accept a market / close auction start the CLI cannot check against an independent reference
                        (the start is the indexer's best price: every market / close sell needs it, or --floor-price;
                        a buy when it is worth 100 KAS or more or far above the last fill)
  --floor-price P       a market / close sell never fills below P (the bound is max(slippage bound, P))
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
    "--displayed-price",
    "--tolerance",
    "--slice",
    "--interval",
    "--price-end",
    "--slice-auction",
    "--duration",
    "--step",
    "--trail-step",
    "--trail-gap",
    "--trail-wait",
    "--trail-updates",
    "--band",
    "--keeper-tip",
    "--min-rest",
    "--entry-stop",
    "--exit-min-fill",
    "--exit-min-touch",
    "--exit-slippage",
    "--repeat-count",
    "--decimals",
    "--floor-price",
];
const BOOL_FLAGS: &[&str] = &["--day", "--all-or-nothing", "--ack-no-guards", "--ack-market-start"];

/// Options every kind takes (`--decimals`: the first order of a token without a registry entry and without orders, any kind).
const ANY_KIND: &[&str] =
    &["--token", "--quote", "--side", "--amount", "--tip", "--carrier", "--ack-no-guards", "--ack-market-start", "--decimals"];
const LIFE: &[&str] = &["--gtd", "--day", "--active-from"];
const STOP_KNOBS: &[&str] = &["--min-touch", "--band", "--keeper-tip", "--min-rest"];
const TRAIL: &[&str] = &["--trail-step", "--trail-gap", "--trail-wait", "--trail-updates"];

/// The kind-specific options a kind takes (besides [`ANY_KIND`]).
fn kind_flags(kind: &str) -> Option<Vec<&'static str>> {
    let v = |xs: &[&[&'static str]]| Some(xs.concat());
    match kind {
        "limit" | "pair-limit" => v(&[&["--price", "--min-fill", "--crossing", "--max-fills"], LIFE]),
        "ioc" | "fok" => v(&[&["--price", "--min-fill", "--life", "--active-from"]]),
        "market" => v(&[&["--min-fill", "--slippage", "--all-or-nothing", "--floor-price"]]),
        "streaming" => v(&[&["--min-fill", "--displayed-price", "--tolerance", "--all-or-nothing"]]),
        "close" => v(&[&["--slippage", "--all-or-nothing", "--floor-price"]]),
        "twap" => v(&[&["--price", "--min-fill", "--slice", "--interval", "--price-end", "--slice-auction"], LIFE]),
        "dca" => v(&[&["--price", "--min-fill", "--slice", "--interval", "--price-end", "--slice-auction", "--max-fills"], LIFE]),
        "dutch" => v(&[&["--price", "--min-fill", "--price-end", "--duration", "--step", "--max-fills"], LIFE]),
        "stop-market" => v(&[&["--stop", "--min-fill", "--slippage", "--max-fills"], STOP_KNOBS, LIFE]),
        "stop-limit" => v(&[&["--stop", "--limit", "--min-fill", "--max-fills"], STOP_KNOBS, LIFE]),
        "trailing-stop" => v(&[&["--stop", "--take-profit", "--min-fill", "--slippage", "--max-fills"], TRAIL, STOP_KNOBS, LIFE]),
        "take-profit" => v(&[&["--take-profit", "--min-fill", "--max-fills"], LIFE]),
        "oco" => v(&[&["--take-profit", "--stop", "--limit", "--min-fill", "--slippage", "--max-fills"], STOP_KNOBS, LIFE]),
        "ifd" | "ifo" | "repeat-ifd" | "repeat-ifo" => v(&[
            &[
                "--price",
                "--min-fill",
                "--take-profit",
                "--stop",
                "--limit",
                "--exit-tip",
                "--prefund",
                "--entry-stop",
                "--exit-min-fill",
                "--exit-min-touch",
                "--exit-slippage",
            ],
            TRAIL,
            LIFE,
            if kind.starts_with("repeat-") { &["--repeat-count"] } else { &[] },
        ]),
        _ => None,
    }
}

/// The least carrier `--carrier` accepts: a token or order output carries a covenant id (KIP-9 storage plurality 2), so
/// below 0.08 KAS its storage mass alone exceeds the block limit.
pub const MIN_CARRIER: u64 = 8_000_000;

/// The side of `kind`: `--side`, or the side a kind implies (close and twap sell, dca buys).
pub fn side_for(kind: &str, p: &Parsed) -> Result<Side, String> {
    let fixed = match kind {
        "close" | "twap" => Some(Side::Sell),
        "dca" => Some(Side::Buy),
        _ => None,
    };
    let given = match p.get("--side") {
        Some("buy") => Some(Side::Buy),
        Some("sell") => Some(Side::Sell),
        Some(o) => return Err(format!("--side must be buy or sell, got `{o}`")),
        None => None,
    };
    match (fixed, given) {
        (Some(f), Some(g)) if f != g => Err(format!("{kind} is a {} order (--side {} does not apply)", f.name(), g.name())),
        (Some(f), _) => Ok(f),
        (None, Some(g)) => Ok(g),
        (None, None) => Err("--side buy|sell is required".into()),
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

/// The kind is known, takes every option given, and has its side.
pub fn check_flags(kind: &str, p: &Parsed) -> Result<Side, String> {
    let Some(allowed) = kind_flags(kind) else { return Err(format!("unknown order kind `{kind}` (see `kob place --help`)")) };
    for k in VALUE_FLAGS.iter().chain(BOOL_FLAGS) {
        if (p.vals.contains_key(*k) || p.has(k)) && !ANY_KIND.contains(k) && !allowed.contains(k) {
            return Err(format!("{k} does not apply to {kind}"));
        }
    }
    if kind == "pair-limit" && p.get("--quote").is_none() {
        return Err("pair-limit needs --quote TOKEN_B".into());
    }
    side_for(kind, p)
}

/// The intent of `kind` from the arguments (`quote`: a pair's B).
pub fn intent_of(kind: &str, p: &Parsed, m: &TokenMarket, quote: Option<&TokenMarket>) -> Result<Intent, String> {
    let side = check_flags(kind, p)?;
    if kind == "pair-limit" && quote.is_none() {
        return Err("pair-limit needs --quote TOKEN_B".into());
    }
    let unit = m.unit();
    let amount_of = |k: &str| p.get(k).map(|v| parse_token_amount(v, &unit, k)).transpose();
    let amount = match amount_of("--amount")? {
        Some(a) => a,
        None if kind == "close" => 0,
        None => return Err("--amount is required".into()),
    };
    let price_of = |k: &str| -> Result<Option<i64>, String> {
        match (p.get(k), quote) {
            (None, _) => Ok(None),
            (Some(v), Some(b)) => parse_pair_price(v, &unit, m.scale, &b.unit(), k).map(Some),
            (Some(v), None) => parse_kas_price(v, &unit, m.scale, k, false).map(Some),
        }
    };
    let delta_of = |k: &str| -> Result<Option<i64>, String> {
        match (p.get(k), quote) {
            (None, _) => Ok(None),
            (Some(v), Some(b)) => units::parse_pair_delta(v, &unit, m.scale, &b.unit(), k).map(Some),
            (Some(v), None) => parse_kas_price(v, &unit, m.scale, k, true).map(Some),
        }
    };
    let need = |k: &str| -> Result<i64, String> { price_of(k)?.ok_or_else(|| format!("{kind} needs {k}")) };
    let tip_of = |k: &str| -> Result<i64, String> {
        p.get(k).map(|v| parse_kas_price(v, &unit, m.scale, k, true)).transpose().map(Option::unwrap_or_default)
    };
    let int_of = |k: &str| -> Result<Option<i64>, String> {
        p.get(k).map(|v| v.parse::<i64>().map_err(|_| format!("{k} must be an integer, got `{v}`"))).transpose()
    };
    let dur_of = |k: &str| p.get(k).map(|v| parse_duration_daa(v, k)).transpose();
    let bps_of = |k: &str| p.get(k).map(|v| parse_bps(v, k)).transpose();
    let tip = tip_of("--tip")?;
    let min_fill = amount_of("--min-fill")?;
    let max_fills = int_of("--max-fills")?;
    let crossing = match p.get("--crossing") {
        None | Some("auction") => Crossing::Auction,
        Some("limit") => Crossing::Limit,
        Some("reject") => Crossing::Reject,
        Some(o) => return Err(format!("--crossing must be auction, limit or reject, got `{o}`")),
    };
    let slip = bps_of("--slippage")?;
    let knobs = StopKnobs {
        band_daa: dur_of("--band")?,
        keeper_tip: p.get("--keeper-tip").map(|v| parse_kas(v, "--keeper-tip", true).map(|x| x as i64)).transpose()?,
        min_rest: dur_of("--min-rest")?,
    };
    let trail = match (delta_of("--trail-step")?, delta_of("--trail-gap")?) {
        (None, None) => {
            if p.get("--trail-wait").is_some() || p.get("--trail-updates").is_some() {
                return Err("--trail-wait / --trail-updates need --trail-step and --trail-gap".into());
            }
            None
        }
        (Some(step), Some(gap)) => Some(Trail { step, gap, wait: dur_of("--trail-wait")?, updates: int_of("--trail-updates")? }),
        _ => return Err("a trailing stop needs both --trail-step and --trail-gap".into()),
    };
    Ok(match kind {
        "limit" | "pair-limit" => Intent::Limit(LimitIntent {
            side,
            amount,
            price: need("--price")?,
            tip,
            min_fill,
            lifetime: lifetime_of(p)?,
            active_from: activation_of(p)?,
            crossing,
            max_fills,
        }),
        "ioc" | "fok" => Intent::Immediate(ImmediateIntent {
            fok: kind == "fok",
            side,
            amount,
            price: need("--price")?,
            tip,
            min_fill,
            life_daa: dur_of("--life")?,
            active_from: activation_of(p)?,
        }),
        "market" => Intent::Market(MarketIntent {
            side,
            amount,
            tip,
            min_fill,
            slippage_bps: slip,
            all_or_nothing: p.has("--all-or-nothing"),
            floor: price_of("--floor-price")?,
        }),
        "streaming" => Intent::Streaming(StreamingIntent {
            side,
            amount,
            tip,
            min_fill,
            displayed_price: need("--displayed-price")?,
            tolerance_bps: bps_of("--tolerance")?
                .ok_or("streaming needs --tolerance BPS (what you accept beyond the displayed price)")?,
            all_or_nothing: p.has("--all-or-nothing"),
        }),
        "close" => Intent::Close(CloseIntent {
            amount: (amount > 0).then_some(amount),
            tip,
            slippage_bps: slip,
            all_or_nothing: p.has("--all-or-nothing"),
            floor: price_of("--floor-price")?,
        }),
        "twap" | "dca" => Intent::Schedule(ScheduleIntent {
            dca: kind == "dca",
            amount,
            tip,
            min_fill,
            slice: amount_of("--slice")?.ok_or_else(|| format!("{kind} needs --slice AMOUNT"))?,
            interval_daa: dur_of("--interval")?.ok_or_else(|| format!("{kind} needs --interval DURATION"))?,
            price: need("--price")?,
            price_end: price_of("--price-end")?,
            slice_auction_daa: dur_of("--slice-auction")?,
            lifetime: lifetime_of(p)?,
            active_from: activation_of(p)?,
            max_fills,
        }),
        "dutch" => Intent::Dutch(DutchIntent {
            side,
            amount,
            tip,
            min_fill,
            price: need("--price")?,
            price_end: need("--price-end")?,
            duration_daa: dur_of("--duration")?.ok_or("dutch needs --duration DURATION")?,
            step_daa: dur_of("--step")?,
            lifetime: lifetime_of(p)?,
            active_from: activation_of(p)?,
            max_fills,
        }),
        "stop-market" | "stop-limit" | "take-profit" | "oco" | "trailing-stop" => {
            let k = match kind {
                "stop-market" => CondKind::StopMarket,
                "stop-limit" => CondKind::StopLimit,
                "take-profit" => CondKind::TakeProfit,
                "trailing-stop" => CondKind::Trailing,
                _ => CondKind::Oco,
            };
            if k == CondKind::Trailing && trail.is_none() {
                return Err("trailing-stop needs --trail-step P and --trail-gap P".into());
            }
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
                min_touch: amount_of("--min-touch")?,
                lifetime: lifetime_of(p)?,
                active_from: activation_of(p)?,
                max_fills,
                trail,
                knobs,
            })
        }
        _ => {
            // ifd, ifo, repeat-ifd, repeat-ifo
            let (tp, stop) = (price_of("--take-profit")?, price_of("--stop")?);
            match (kind, tp.is_some(), stop.is_some()) {
                ("ifd", true, true) | ("ifd", false, false) => {
                    return Err("ifd needs exactly one exit leg: --take-profit or --stop (both: ifo)".into())
                }
                ("ifo" | "repeat-ifo", a, b) if !(a && b) => return Err(format!("{kind} needs both --take-profit and --stop")),
                ("repeat-ifd", a, b) if !a || b => return Err("repeat-ifd needs --take-profit only (with a stop: repeat-ifo)".into()),
                _ => {}
            }
            if trail.is_some() && stop.is_none() {
                return Err("a trailing exit needs an exit --stop".into());
            }
            if p.get("--prefund").is_some() && side == Side::Buy {
                return Err("--prefund applies to a sell-first entry only".into());
            }
            let prefund = match (p.get("--prefund"), quote) {
                (None, _) => None,
                (Some(v), Some(b)) => Some(parse_pair_price(v, &unit, m.scale, &b.unit(), "--prefund")?),
                (Some(v), None) => Some(parse_kas_price(v, &unit, m.scale, "--prefund", true)?),
            };
            let repeat = if kind.starts_with("repeat-") { Some(int_of("--repeat-count")?) } else { None };
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
                prefund,
                lifetime: lifetime_of(p)?,
                active_from: activation_of(p)?,
                more: IfdMore {
                    entry_stop: price_of("--entry-stop")?,
                    exit_slip_bps: bps_of("--exit-slippage")?,
                    exit_min_touch: amount_of("--exit-min-touch")?,
                    exit_trail: trail,
                    exit_min_fill: amount_of("--exit-min-fill")?,
                    repeat,
                    ..Default::default()
                },
            })
        }
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
    if m.entered_decimals {
        v.push(format!(
            "decimals: {} (from --decimals: the first order of this token sets its market's price scale, {} base units per whole token)",
            m.decimals, m.scale
        ));
    }
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

/// The planning environment of `kob place`, read from the node and the indexer, with the findings of the tokens'
/// extension commitments ([`env_of`]).
pub type PlaceEnv = (Env, TokenMarket, Option<TokenMarket>, Vec<Issue>);

/// [`PlaceEnv`] of the command line `p`.
pub fn env_for(c: &Common, p: &Parsed, kind: &str, maker: [u8; 32]) -> Result<PlaceEnv, String> {
    let indexer = c.need_indexer()?.to_string();
    let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(&indexer))?;
    let entered = p
        .get("--decimals")
        .map(|v| v.parse::<u32>().map_err(|_| format!("--decimals must be a whole number from 0 to 9, got `{v}`")))
        .transpose()?;
    let order_scale = |cov: &[u8; 32]| data::order_scale(&indexer, cov);
    let open = |entered| market::OpenToken { network: &c.network, order_scale: &order_scale, entered };
    let mut a = market::resolve_with(p.get("--token").ok_or("--token is required")?, &reg, &idx, Some(&open(entered)))?;
    let mut b = match p.get("--quote") {
        Some(q) => Some(market::resolve_with(q, &reg, &idx, Some(&open(None)))?),
        None => None,
    };
    // a commitment no source names: the one the wallet's own holdings of the token have (checked on the node), never assumed
    let mut found = vec![];
    for m in [Some(&mut a), b.as_mut()].into_iter().flatten().filter(|m| m.ext_source == market::ExtSource::Unknown) {
        let held = data::token_utxos(&indexer, c.need_node()?, &c.network, m, &maker)?;
        let exts: Vec<[u8; 32]> = held.iter().map(|u| u.state.extension()).collect();
        found.extend(m.take_ext_from_holdings(&exts)?);
    }
    let side = side_for(kind, p)?;
    let if_done = matches!(kind, "ifd" | "ifo" | "repeat-ifd" | "repeat-ifo");
    let carrier = carrier_arg(p)?;
    // a sell (and a sell-first entry) holds tokens of A; a pair buy holds B, a sell-first pair entry a B prefund
    let needs = Needs { a: side == Side::Sell, b: b.is_some() && (side == Side::Buy || if_done) };
    let (mut env, mut notes) = env_of(c, &a, b.as_ref(), needs, maker)?;
    notes.extend(found);
    env.carrier = carrier;
    env.guards_acknowledged = p.has("--ack-no-guards");
    env.market_start_ack = p.has("--ack-market-start");
    Ok((env, a, b, notes))
}

/// `--carrier`, checked against the covenant output minimum.
pub fn carrier_arg(p: &Parsed) -> Result<Option<u64>, String> {
    match p.get("--carrier") {
        Some(v) => {
            let k = parse_kas(v, "--carrier", false)?;
            if k < MIN_CARRIER {
                return Err(format!(
                    "--carrier must be at least 0.08KAS (the covenant output minimum), got {}",
                    units::kas(k as i128)
                ));
            }
            Ok(Some(k))
        }
        None => Ok(None),
    }
}

/// Which of the maker's token UTXOs a plan needs: of A, of B.
#[derive(Debug, Clone, Copy, Default)]
pub struct Needs {
    /// The maker's UTXOs of A.
    pub a: bool,
    /// The maker's UTXOs of B (a pair).
    pub b: bool,
}

/// The planning environment of an order of A (a pair: A/B) for `maker`, read from the node and the indexer: the clock,
/// the KAS and token UTXOs, the book, the maker's own orders and the last fill (`web/src/app/env.ts`).
///
/// With it, what the wallet's own holdings tell about an unlisted KCC-20 token's extension commitment (the indexer's): a
/// mismatch is refused here; no holdings to compare with is a warning (the web app's unchecked commitment).
pub fn env_of(
    c: &Common,
    a: &TokenMarket,
    b: Option<&TokenMarket>,
    needs: Needs,
    maker: [u8; 32],
) -> Result<(Env, Vec<Issue>), String> {
    let node = c.need_node()?.to_string();
    let indexer = c.need_indexer()?.to_string();
    let clock = data::clock(&node)?;
    let funding = data::funding(&node, &c.network, &maker)?;
    let mut guards = vec![];
    let token_utxos = if needs.a { data::token_utxos(&indexer, &node, &c.network, a, &maker)? } else { vec![] };
    // an unlisted KCC-20 token's extension commitment is the indexer's: the wallet's own holdings of the token (their states
    // confirmed on the node) are a reference without the indexer, and one of another commitment blocks (the web app's
    // open-token-extension-mismatch); a buy would otherwise pin a commitment that only a look-alike genesis can fill. The
    // indexer's commitment is never taken alone: without holdings to compare with, the plan says it is unchecked
    let mut notes = vec![];
    for m in [Some(a), b].into_iter().flatten() {
        if m.family != kob_protocol::Family::Kcc20 || m.listing != market::Listing::Unlisted {
            continue;
        }
        match m.ext_source {
            // taken from these holdings (`env_for`), with its finding
            market::ExtSource::Holdings => continue,
            market::ExtSource::Unknown => {
                return Err(format!("{}: its extension commitment is unknown (no source names it): nothing planned", m.name()))
            }
            market::ExtSource::Lists | market::ExtSource::Order => {}
        }
        let origin = if m.ext_source == market::ExtSource::Order { "the order's own state" } else { "the indexer" };
        let held =
            if m.cov == a.cov && needs.a { token_utxos.clone() } else { data::token_utxos(&indexer, &node, &c.network, m, &maker)? };
        if let Some(u) = held.iter().find(|u| u.state.extension() != m.ext) {
            return Err(format!(
                "{}: {origin} names extension commitment {}, but your token UTXO {}:{} (checked on the node) has {}: the token \
                 has more than one commitment; refused (check the token's genesis)",
                m.name(),
                to_hex(&m.ext),
                to_hex(&u.utxo.transaction_id),
                u.utxo.index,
                to_hex(&u.state.extension())
            ));
        }
        notes.push(if held.is_empty() {
            Issue {
                code: "TOKEN_EXTENSION_UNCHECKED",
                severity: Severity::Warning,
                message: format!(
                    "{}: its extension commitment {} is from {origin}, and nothing checks it (no registry entry, and this wallet \
                     holds none of the token): an order pins it, and only tokens of that commitment fill it; check the token's \
                     genesis yourself",
                    m.name(),
                    to_hex(&m.ext)
                ),
            }
        } else {
            Issue {
                code: "TOKEN_EXTENSION_CHECKED",
                severity: Severity::Info,
                message: format!(
                    "{}: the extension commitment {} from {origin} matches your {} token UTXO(s) of it (checked on the node)",
                    m.name(),
                    to_hex(&m.ext),
                    held.len()
                ),
            }
        });
    }
    let (book, own, last_fill, pair) = match b {
        None => {
            let book = data::book(&indexer, a).unwrap_or_else(|_| {
                guards.push("book".to_string());
                Default::default()
            });
            let own = data::own_orders(&indexer, a, &maker).unwrap_or_else(|_| {
                guards.push("own-orders".to_string());
                vec![]
            });
            (book, own, data::last_fill(&indexer, a).ok().flatten(), None)
        }
        Some(bm) => {
            let book = data::pair_book(&indexer, a, bm).unwrap_or_else(|_| {
                guards.push("book".to_string());
                Default::default()
            });
            let own = data::own_pair_orders(&indexer, a, bm, &maker).unwrap_or_else(|_| {
                guards.push("own-orders".to_string());
                vec![]
            });
            let kas_of = |m: &TokenMarket| {
                data::book(&indexer, m)
                    .ok()
                    .and_then(|bk| data::firm_reference(&bk, m.scale))
                    .or(data::last_fill(&indexer, m).ok().flatten())
            };
            let quote_utxos = if needs.b { data::token_utxos(&indexer, &node, &c.network, bm, &maker)? } else { vec![] };
            (
                book,
                own,
                None,
                Some(PairPart { quote: bm.clone(), quote_utxos, kas_per_whole_a: kas_of(a), kas_per_whole_b: kas_of(bm) }),
            )
        }
    };
    let env = Env {
        market: a.clone(),
        maker,
        clock,
        book,
        own,
        guards_unavailable: guards,
        guards_acknowledged: false,
        last_fill,
        market_start_ack: false,
        funding,
        token_utxos,
        fee_rate: c.fee_rate,
        carrier: None,
        pair,
    };
    Ok((env, notes))
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
        check_flags(&kind, &p).map_err(|e| (2, e))?;
        let (env, a, b, notes) = env_for(&c, &p, &kind, signer.pubkey).map_err(|e| (1, e))?;
        let intent = intent_of(&kind, &p, &a, b.as_ref()).map_err(|e| (2, e))?;
        let mut plan = plan_order(&env, &intent);
        plan.issues.extend(notes);
        // a registry token that is not tradable is refused, an unlisted one flagged (`web/src/kob/decode.ts` token-unlisted)
        for i in [Some(&a), b.as_ref()].into_iter().flatten().filter_map(|m| m.listing_issue(true)) {
            plan.ok &= i.severity != Severity::Error;
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
    let side = side_for(&kind, &p).map(Side::name).unwrap_or("");
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

    /// `--decimals` (the first order of a token without orders) is taken by every kind: it was parsed but no kind listed it, so
    /// every `kob place ... --decimals N` was refused (`--decimals does not apply to limit`).
    #[test]
    fn every_kind_takes_decimals_for_a_first_order() {
        let args = |kind: &str| -> Vec<String> {
            ["--token", "TKN", "--side", "sell", "--amount", "1tok", "--decimals", "2"]
                .iter()
                .map(|s| s.to_string())
                .chain([kind.to_string()])
                .collect()
        };
        for kind in ["limit", "ioc", "market", "stop-market", "oco", "ifd", "twap", "dutch", "trailing-stop"] {
            let p = crate::trade::opts::parse(&args(kind), VALUE_FLAGS, BOOL_FLAGS).unwrap();
            assert!(check_flags(kind, &p).is_ok(), "{kind}: {:?}", check_flags(kind, &p));
        }
    }
}
