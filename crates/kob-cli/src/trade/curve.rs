//! `kob place curve`: one sell order whose price steps up level by level (`KobCurveAsk`,
//! `docs/design/curve-ask-spec.md`).

use kob_protocol::defaults::{curve_min_fill, curve_table, CurveSpec, CurveTable};
use kob_protocol::json::to_hex;
use kob_protocol::state::{AnyState, CurveAskState, CURVE_MAX_CROSS};
use serde_json::json;

use super::flow::{finish, Ready};
use super::market::TokenMarket;
use super::opts::{parse, Common, Parsed, COMMON_HELP};
use super::place::{env_for, units_of};
use super::plan::{build_create, Clock, CreateSpec, Severity};
use super::units::{self, parse_kas, parse_token_amount, parse_when, When};

/// The help text of `kob place curve`.
pub const USAGE: &str = "\
kob place curve: sell along a stepped price curve (one order, one custody; the price rises level by level)

USAGE
  kob place curve --token TOKEN [--amount AMOUNT] [--levels N] [--total KAS] [--multiple M] [options]

  --amount AMOUNT   what the curve sells (default: every token of TOKEN this wallet holds)
  --levels N        price levels, 1 to 1000 (default 100; fewer when the amount is small)
  --total KAS       what a sell-out realises, e.g. 10000KAS (default 10000KAS): the prices are rounded so that a
                    sell-out bought level by level pays at most this, as close to it as whole sompi allow
  --multiple M      the last level's price over the first's (default 5; geometric steps; 1 = one flat price)
  --tip T           KAS per whole token taken from every level (default 0)
  --min-fill A      smallest fill (default: worth 10 KAS at the first level)
  --active-from W   no fill before WHEN (2026-10-20T00:00Z or daa:N)
  --carrier KAS     KAS on the order and on its custody (default 1KAS)
  --decimals N      as kob place

A fill crosses at most 8 levels. The curve has no expiry: it sells until it is sold out or you cancel it.
";

const VALUE_FLAGS: &[&str] =
    &["--token", "--amount", "--levels", "--total", "--multiple", "--tip", "--min-fill", "--active-from", "--carrier", "--decimals"];
const BOOL_FLAGS: &[&str] = &["--ack-no-guards", "--ack-market-start"];

/// `--multiple` in thousandths (`5`, `2.5`).
fn parse_multiple(v: &str) -> Result<u64, String> {
    let bad = || format!("--multiple must be a number of at most 3 decimals and at least 1, got `{v}`");
    let (int, frac) = v.split_once('.').unwrap_or((v, ""));
    if frac.len() > 3 || int.is_empty() || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let m = int.parse::<u64>().map_err(|_| bad())?.checked_mul(1_000).ok_or_else(bad)?
        + format!("{frac:0<3}").parse::<u64>().map_err(|_| bad())?;
    if m < 1_000 {
        return Err(bad());
    }
    Ok(m)
}

/// The curve of the command line: its table and its state.
pub fn curve_of(
    p: &Parsed,
    m: &TokenMarket,
    maker: [u8; 32],
    held: i64,
    clock: &Clock,
) -> Result<(CurveTable, CurveAskState), String> {
    let unit = m.unit();
    let amount = match p.get("--amount") {
        Some(v) => parse_token_amount(v, &unit, "--amount")?,
        None if held > 0 => held,
        None => return Err(format!("this wallet holds no {}: give --amount", m.name())),
    };
    let mut spec = CurveSpec::defaults(amount, m.scale);
    if let Some(v) = p.get("--levels") {
        spec.levels = v.parse().map_err(|_| format!("--levels must be a whole number, got `{v}`"))?;
    }
    if let Some(v) = p.get("--total") {
        spec.total = i64::try_from(parse_kas(v, "--total", false)?).map_err(|_| "--total is too large".to_string())?;
    }
    if let Some(v) = p.get("--multiple") {
        spec.multiple_milli = parse_multiple(v)?;
    }
    if let Some(v) = p.get("--tip") {
        spec.tip = units::parse_kas_price(v, &unit, m.scale, "--tip", true)?;
    }
    let t = curve_table(&spec).map_err(|e| e.to_string())?;
    let min_fill = match p.get("--min-fill") {
        Some(v) => parse_token_amount(v, &unit, "--min-fill")?,
        None => curve_min_fill(amount, t.table[0], t.level_size, m.scale),
    };
    let active_from = match p.get("--active-from").map(|v| parse_when(v, "--active-from")).transpose()? {
        None => 0,
        Some(When::Daa(d)) => d as i64,
        Some(When::Unix(u)) => {
            let secs = (u as i64).saturating_sub(clock.unix).max(0);
            clock.daa.saturating_add(secs.saturating_mul(clock.rate_milli) / 1_000)
        }
    };
    let state = CurveAskState {
        maker,
        token_cov_id: m.cov,
        token_tpl_hash: m.tpl_hash,
        tpl_prefix_len: m.prefix_len,
        tpl_suffix_len: m.suffix_len,
        scale: m.scale,
        min_fill,
        tip: spec.tip,
        active_from,
        level_size: t.level_size,
        total: amount,
        amount_left: amount,
        extension_commitment: m.ext,
        table: t.table.clone(),
    };
    state.check_numbers()?;
    Ok((t, state))
}

/// The plan lines of a curve.
pub fn lines(t: &CurveTable, s: &CurveAskState, m: &TokenMarket, carrier: i64) -> Vec<String> {
    let u = m.unit();
    let price = |p: i64| units::price_per_whole(p, &u, m.scale, 8, "KAS");
    vec![
        "order: KobCurveAsk".into(),
        format!("amount: {}", units::tokens(s.total as i128, &u)),
        format!(
            "levels: {} of {} each (the last {})",
            s.levels(),
            units::tokens(s.level_size as i128, &u),
            units::tokens((s.total - (s.levels() as i64 - 1) * s.level_size) as i128, &u)
        ),
        format!("first level: {}; last level: {}", price(s.table[0]), price(*s.table.last().expect("a level"))),
        format!("sold out (each level bought whole): {}", units::kas(t.realised as i128)),
        format!("tip: {} per token, taken from every level", super::place::tip_per_whole(s.tip, m)),
        format!("minimum fill: {}", units::tokens(s.min_fill as i128, &u)),
        format!("one fill crosses at most {CURVE_MAX_CROSS} levels"),
        "expiry: none (it sells until sold out or cancelled)".into(),
        format!("tokens into custody: {}", units::tokens(s.total as i128, &u)),
        format!("KAS locked in the order (returned on sell-out or cancel): {}", units::kas(2 * carrier as i128)),
    ]
}

pub fn run(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[USAGE, COMMON_HELP], true) {
        return code;
    }
    let p = match parse(args, VALUE_FLAGS, BOOL_FLAGS) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Some(x) = p.pos.first() {
        eprintln!("error: unexpected argument `{x}`");
        return 2;
    }
    let step = || -> Result<_, (i32, String)> {
        let c = Common::from(&p).map_err(|e| (2, e))?;
        let signer = c.signer(args).map_err(|e| (2, e))?;
        super::flow::check_network_first(&c).map_err(|e| (1, e))?;
        let (env, a, _, notes) = env_for(&c, &p, "curve", signer.pubkey).map_err(|e| (1, e))?;
        if a.family != kob_protocol::Family::Kcc20 {
            return Err((2, format!("{}: a curve sells KCC-20 tokens only", a.name())));
        }
        let held: i64 = env.token_utxos.iter().filter(|u| u.state.is_holder(a.program)).map(|u| u.state.amount()).sum();
        let (t, state) = curve_of(&p, &a, signer.pubkey, held, &env.clock).map_err(|e| (2, e))?;
        let carrier = env.carrier();
        let order = AnyState::KobCurveAsk(state.clone());
        let res = build_create(&env, &CreateSpec { order, value: carrier, token_amount: state.total, deadline: None });
        let mut issues = notes;
        issues.extend(res.issues);
        if let Some(i) = a.listing_issue(true) {
            issues.push(i);
        }
        Ok((c, signer, a, t, state, res.built, carrier, issues))
    };
    let (c, signer, a, t, state, built, carrier, issues) = match step() {
        Ok(x) => x,
        Err((code, e)) => {
            eprintln!("error: {e}");
            return code;
        }
    };
    let Some(built) = built.filter(|_| !issues.iter().any(|i| i.severity == Severity::Error)) else {
        super::flow::print_issues(&issues, false);
        eprintln!("error: the curve could not be planned: nothing signed");
        return 1;
    };
    let cov = built.covenants.first().map(|x| to_hex(&x.covenant_id)).unwrap_or_default();
    let r = Ready {
        title: format!("place curve sell {}", a.name()),
        lines: lines(&t, &state, &a, carrier),
        issues,
        file: format!("{cov}.place.json"),
        json: json!({ "states": [serde_json::to_value(AnyState::KobCurveAsk(state)).unwrap_or_default()], "table": t }),
        built,
    };
    finish(&c, &signer, r, &units_of(&a, None), "this curve")
}

#[cfg(test)]
mod tests {
    use super::super::tests_plan::{env, maker, market, CLOCK, MAKER_SK, TOK};
    use super::*;
    use kob_protocol::artifacts::TemplateId;

    /// The command line's curve is placed: the whole holding by default, the table of the defaults helper, a placement
    /// that signs, validates in the engine and recovers as planned.
    #[test]
    fn a_curve_is_planned_signed_and_recovered() {
        let m = market(TemplateId::Kcc20Ref);
        let e = env(&m);
        let args: Vec<String> =
            ["--token", "TST", "--levels", "50", "--total", "1000KAS", "--multiple", "4"].iter().map(|x| x.to_string()).collect();
        let p = parse(&args, VALUE_FLAGS, BOOL_FLAGS).unwrap();
        let (t, s) = curve_of(&p, &m, maker(), 100 * TOK, &CLOCK).unwrap();
        assert_eq!((s.total, s.levels(), s.level_size, s.amount_left), (100 * TOK, 50, 2 * TOK, 100 * TOK));
        assert!(t.realised <= 1_000 * 100_000_000 && t.realised > 999 * 100_000_000);
        let ratio = *s.table.last().unwrap() as f64 / s.table[0] as f64;
        assert!((ratio - 4.0).abs() < 0.001, "x{ratio}");
        let order = AnyState::KobCurveAsk(s.clone());
        let res = build_create(&e, &CreateSpec { order: order.clone(), value: e.carrier(), token_amount: s.total, deadline: None });
        assert!(res.issues.is_empty(), "{:?}", res.issues);
        let built = res.built.unwrap();
        let sigs = kob_protocol::tx::sign_locally(&built, &std::collections::BTreeMap::from([(maker(), MAKER_SK)])).unwrap();
        let signed = kob_protocol::tx::finalize(&built, &sigs, Default::default()).unwrap();
        kob_protocol::verify::validate_signed(&signed).unwrap();
        let rec = kob_protocol::payload::recover_orders(&built.tx).unwrap();
        assert_eq!(rec[0].order, order);
        assert!(lines(&t, &s, &m, e.carrier()).iter().any(|l| l.starts_with("levels: 50 of 2")));
    }

    #[test]
    fn multiple_parses_in_thousandths() {
        assert_eq!(parse_multiple("5"), Ok(5_000));
        assert_eq!(parse_multiple("2.5"), Ok(2_500));
        assert_eq!(parse_multiple("1.001"), Ok(1_001));
        assert!(parse_multiple("0.9").is_err());
        assert!(parse_multiple("1.0001").is_err());
        assert!(parse_multiple("x").is_err());
    }
}
