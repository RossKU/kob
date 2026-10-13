//! Amounts, prices, durations and dates as the trading commands read them: every number carries its unit, a bare number is
//! refused. Nothing is rounded: a value that does not fit its unit exactly (more decimals than the token has, a price that is
//! not a whole number of sompi per order scale) is an error naming the nearest valid values.
//!
//! | what | units | example |
//! |---|---|---|
//! | token amount | the token's ticker or `tok` (whole tokens, up to its decimals), `base` (base units) | `12.5TUSD`, `1250000000base` |
//! | KAS amount | `KAS` (up to 8 decimals), `sompi` | `2KAS`, `200000000sompi` |
//! | price on a KAS book | `KAS` or `sompi` per whole token | `0.25KAS` |
//! | price on a pair A/B | B per whole A: B's ticker or `tok` (whole B), `base` (B base units) | `1.5TUSD` |
//! | tip | KAS per whole token (also on a pair) | `0.001KAS` |
//! | basis points | `%` (two decimals) or `bps` | `3%`, `300bps` |
//! | duration | `s`, `m`, `h`, `d` (wall clock, at 10 DAA/s) or `daa` | `30s`, `600daa` |
//! | date | `YYYY-MM-DDTHH:MM[:SS]Z` (UTC) or `daa:N` | `2026-10-20T00:00Z` |

/// KAS decimals.
pub const KAS_DECIMALS: u32 = 8;

/// A token's display facts: ticker and decimals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUnit {
    /// Ticker (may be empty for an unlisted token: only `tok` / `base` then).
    pub ticker: String,
    /// Decimals.
    pub decimals: u32,
}

fn split_unit(s: &str) -> (&str, &str) {
    let s = s.trim();
    let at = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    (&s[..at], s[at..].trim())
}

/// A non-negative decimal with at most `decimals` fractional digits, scaled to an integer.
pub fn parse_decimal(num: &str, decimals: u32, what: &str) -> Result<i128, String> {
    if num.is_empty() || num == "." || num.matches('.').count() > 1 || !num.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(format!("{what}: `{num}` is not a number"));
    }
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    if frac.len() > decimals as usize {
        return Err(format!("{what}: `{num}` has more than {decimals} decimals"));
    }
    let scale = 10i128.pow(decimals);
    let whole: i128 = if int.is_empty() { 0 } else { int.parse().map_err(|_| format!("{what}: `{num}` is out of range"))? };
    let part: i128 = if frac.is_empty() { 0 } else { frac.parse::<i128>().map_err(|_| format!("{what}: `{num}` is not a number"))? };
    let part = part * 10i128.pow(decimals - frac.len() as u32);
    whole.checked_mul(scale).and_then(|w| w.checked_add(part)).ok_or_else(|| format!("{what}: `{num}` is out of range"))
}

fn to_i64(v: i128, what: &str, raw: &str) -> Result<i64, String> {
    i64::try_from(v).map_err(|_| format!("{what}: `{raw}` is out of range"))
}

fn no_unit(what: &str, raw: &str, hint: &str) -> String {
    format!("{what}: `{raw}` has no unit (amounts always carry one: {hint})")
}

/// A token amount in base units (positive).
pub fn parse_token_amount(raw: &str, t: &TokenUnit, what: &str) -> Result<i64, String> {
    let (num, unit) = split_unit(raw);
    let hint = format!("e.g. 12.5{} for whole tokens or 1250base for base units", if t.ticker.is_empty() { "tok" } else { &t.ticker });
    let v = match unit.to_ascii_lowercase().as_str() {
        "" => return Err(no_unit(what, raw, &hint)),
        "base" => {
            if num.contains('.') {
                return Err(format!("{what}: base units are whole numbers (`{raw}`)"));
            }
            parse_decimal(num, 0, what)?
        }
        u if u == "tok" || u == "token" || u == "tokens" || (!t.ticker.is_empty() && u == t.ticker.to_ascii_lowercase()) => {
            parse_decimal(num, t.decimals, what)?
        }
        other => return Err(format!("{what}: unknown unit `{other}` ({hint})")),
    };
    let v = to_i64(v, what, raw)?;
    if v <= 0 {
        return Err(format!("{what}: `{raw}` must be positive"));
    }
    Ok(v)
}

/// A KAS amount in sompi (`zero_ok`: zero allowed).
pub fn parse_kas(raw: &str, what: &str, zero_ok: bool) -> Result<u64, String> {
    let (num, unit) = split_unit(raw);
    let v = match unit.to_ascii_lowercase().as_str() {
        "" => return Err(no_unit(what, raw, "e.g. 2KAS or 200000000sompi")),
        "kas" => parse_decimal(num, KAS_DECIMALS, what)?,
        "sompi" => {
            if num.contains('.') {
                return Err(format!("{what}: sompi are whole numbers (`{raw}`)"));
            }
            parse_decimal(num, 0, what)?
        }
        other => return Err(format!("{what}: unknown unit `{other}` (KAS or sompi)")),
    };
    let v = u64::try_from(v).ok().filter(|v| *v <= i64::MAX as u64).ok_or_else(|| format!("{what}: `{raw}` is out of range"))?;
    if v == 0 && !zero_ok {
        return Err(format!("{what}: `{raw}` must be positive"));
    }
    Ok(v)
}

/// `per_whole * scale / 10^decimals`, which must be exact: the state price of a price per whole token.
fn to_state(per_whole: i128, decimals: u32, scale: i64, what: &str, raw: &str) -> Result<i64, String> {
    let num = per_whole * scale as i128;
    let den = 10i128.pow(decimals);
    if num % den != 0 {
        return Err(format!(
            "{what}: `{raw}` is not a whole number of quote units per {scale} base units (the order scale): {} or {} are",
            num / den,
            num / den + 1
        ));
    }
    to_i64(num / den, what, raw)
}

/// A price per whole token on a KAS book (or a tip: `zero_ok`), as the state price (sompi per `scale` base units).
pub fn parse_kas_price(raw: &str, t: &TokenUnit, scale: i64, what: &str, zero_ok: bool) -> Result<i64, String> {
    let per_whole = parse_kas(raw, what, zero_ok)?;
    to_state(per_whole as i128, t.decimals, scale, what, raw)
}

/// A price of a pair A/B (B per whole A), as the state price (B base units per `scale(A)` base units of A).
pub fn parse_pair_price(raw: &str, a: &TokenUnit, a_scale: i64, b: &TokenUnit, what: &str) -> Result<i64, String> {
    let per_whole_b_base = parse_token_amount(raw, b, what)? as i128;
    to_state(per_whole_b_base, a.decimals, a_scale, what, raw)
}

/// Basis points: `3%` (two decimals) or `300bps`.
pub fn parse_bps(raw: &str, what: &str) -> Result<i64, String> {
    let (num, unit) = split_unit(raw);
    let v = match unit.to_ascii_lowercase().as_str() {
        "%" => parse_decimal(num, 2, what)?,
        "bps" => parse_decimal(num, 0, what)?,
        "" => return Err(no_unit(what, raw, "e.g. 3% or 300bps")),
        other => return Err(format!("{what}: unknown unit `{other}` (% or bps)")),
    };
    to_i64(v, what, raw)
}

/// A duration in DAA: `daa` as is, wall-clock units at the nominal 10 DAA per second (rounded up).
pub fn parse_duration_daa(raw: &str, what: &str) -> Result<i64, String> {
    let (num, unit) = split_unit(raw);
    if num.contains('.') {
        return Err(format!("{what}: `{raw}` must be a whole number"));
    }
    let n = to_i64(parse_decimal(num, 0, what)?, what, raw)?;
    let secs = match unit.to_ascii_lowercase().as_str() {
        "daa" => return Ok(n),
        "s" | "sec" | "secs" => n,
        "m" | "min" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" | "days" => n.saturating_mul(86_400),
        "" => return Err(no_unit(what, raw, "e.g. 30s, 5m or 300daa")),
        other => return Err(format!("{what}: unknown unit `{other}` (s, m, h, d or daa)")),
    };
    Ok(secs.saturating_mul(10))
}

/// A point in time: UTC unix seconds or a DAA score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// UTC unix seconds.
    Unix(u64),
    /// DAA score.
    Daa(u64),
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date of days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DDTHH:MM[:SS]Z` (UTC only) or `daa:N`.
pub fn parse_when(raw: &str, what: &str) -> Result<When, String> {
    let s = raw.trim();
    if let Some(d) = s.strip_prefix("daa:") {
        return d.parse::<u64>().map(When::Daa).map_err(|_| format!("{what}: `{raw}` is not a DAA score"));
    }
    let bad = || format!("{what}: `{raw}` is not a UTC time (YYYY-MM-DDTHH:MM[:SS]Z) or daa:N");
    let body = s.strip_suffix('Z').or_else(|| s.strip_suffix('z')).ok_or_else(bad)?;
    let (date, time) = body.split_once(['T', 't', ' ']).ok_or_else(bad)?;
    let dp: Vec<&str> = date.split('-').collect();
    let tp: Vec<&str> = time.split(':').collect();
    if dp.len() != 3 || !(2..=3).contains(&tp.len()) {
        return Err(bad());
    }
    let n = |x: &str| x.parse::<i64>().map_err(|_| bad());
    let (y, mo, d) = (n(dp[0])?, n(dp[1])?, n(dp[2])?);
    let (h, mi, se) = (n(tp[0])?, n(tp[1])?, if tp.len() == 3 { n(tp[2])? } else { 0 });
    if !(1..=12).contains(&mo) || !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&se) || y < 1970 {
        return Err(bad());
    }
    let days = days_from_civil(y, mo, d);
    if civil_from_days(days) != (y, mo, d) {
        return Err(bad());
    }
    Ok(When::Unix((days * 86_400 + h * 3600 + mi * 60 + se) as u64))
}

/// `2026-10-20 00:00:00 UTC`.
pub fn fmt_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    let s = unix % 86_400;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", s / 3600, (s / 60) % 60, s % 60)
}

/// An integer with `decimals` fractional digits, trailing zeros trimmed.
pub fn fmt_decimal(v: i128, decimals: u32) -> String {
    let neg = v < 0;
    let a = v.unsigned_abs();
    let scale = 10u128.pow(decimals);
    let mut frac = format!("{:0width$}", a % scale, width = decimals as usize);
    while frac.ends_with('0') {
        frac.pop();
    }
    format!("{}{}{}{}", if neg { "-" } else { "" }, a / scale, if frac.is_empty() { "" } else { "." }, frac)
}

/// `2.5 KAS`.
pub fn kas(sompi: i128) -> String {
    format!("{} KAS", fmt_decimal(sompi, KAS_DECIMALS))
}

/// `12.5 TUSD (1250000000 base units)`.
pub fn tokens(base: i128, t: &TokenUnit) -> String {
    let tk = if t.ticker.is_empty() { "tokens".to_string() } else { t.ticker.clone() };
    format!("{} {tk} ({base} base units)", fmt_decimal(base, t.decimals))
}

/// A state price (quote units per `scale` base units) as quote per whole token, exact where possible.
pub fn price_per_whole(state_price: i64, t: &TokenUnit, scale: i64, quote_decimals: u32, quote: &str) -> String {
    // quote per whole token = price * 10^decimals / scale, in quote base units
    let num = state_price as i128 * 10i128.pow(t.decimals);
    let per_whole = num / scale as i128;
    let exact = num % scale as i128 == 0;
    let tk = if t.ticker.is_empty() { "token".to_string() } else { t.ticker.clone() };
    format!("{}{} {quote} per {tk}", if exact { "" } else { "~" }, fmt_decimal(per_whole, quote_decimals))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tusd() -> TokenUnit {
        TokenUnit { ticker: "TUSD".into(), decimals: 8 }
    }

    #[test]
    fn token_amounts_need_a_unit() {
        let t = tusd();
        assert_eq!(parse_token_amount("12.5TUSD", &t, "amount"), Ok(1_250_000_000));
        assert_eq!(parse_token_amount("12.5tusd", &t, "amount"), Ok(1_250_000_000));
        assert_eq!(parse_token_amount("0.00000001tok", &t, "amount"), Ok(1));
        assert_eq!(parse_token_amount("1250base", &t, "amount"), Ok(1250));
        for bad in ["12.5", "12", "1.000000001TUSD", "1.5base", "0TUSD", "-1TUSD", "1e3base", "5USD", "abc", ""] {
            assert!(parse_token_amount(bad, &t, "amount").is_err(), "{bad}");
        }
        assert!(parse_token_amount("12.5", &t, "amount").unwrap_err().contains("no unit"));
    }

    #[test]
    fn kas_amounts_and_prices() {
        assert_eq!(parse_kas("2KAS", "c", false), Ok(200_000_000));
        assert_eq!(parse_kas("0.08kas", "c", false), Ok(8_000_000));
        assert_eq!(parse_kas("5sompi", "c", false), Ok(5));
        assert!(parse_kas("0KAS", "c", false).is_err());
        assert_eq!(parse_kas("0KAS", "c", true), Ok(0));
        assert!(parse_kas("2", "c", false).is_err());
        assert!(parse_kas("0.000000001KAS", "c", false).is_err());
        let t = tusd();
        // 8 decimals, scale 10^8: the state price is sompi per whole token
        assert_eq!(parse_kas_price("0.25KAS", &t, 100_000_000, "price", false), Ok(25_000_000));
        // 12 decimals, scale 10^9: a price per whole token must divide by 1000
        let t12 = TokenUnit { ticker: "BIG".into(), decimals: 12 };
        assert_eq!(parse_kas_price("1KAS", &t12, 1_000_000_000, "price", false), Ok(100_000));
        assert!(parse_kas_price("1001sompi", &t12, 1_000_000_000, "price", false).unwrap_err().contains("1 or 2"));
    }

    #[test]
    fn pair_prices_are_b_per_whole_a() {
        let a = TokenUnit { ticker: "TETH".into(), decimals: 8 };
        let b = tusd();
        assert_eq!(parse_pair_price("2500TUSD", &a, 100_000_000, &b, "price"), Ok(250_000_000_000));
        assert_eq!(parse_pair_price("7base", &a, 100_000_000, &b, "price"), Ok(7));
        assert!(parse_pair_price("2500KAS", &a, 100_000_000, &b, "price").is_err());
    }

    #[test]
    fn bps_durations_and_dates() {
        assert_eq!(parse_bps("3%", "s"), Ok(300));
        assert_eq!(parse_bps("2.5%", "s"), Ok(250));
        assert_eq!(parse_bps("300bps", "s"), Ok(300));
        assert!(parse_bps("300", "s").is_err());
        assert_eq!(parse_duration_daa("30s", "d"), Ok(300));
        assert_eq!(parse_duration_daa("600daa", "d"), Ok(600));
        assert_eq!(parse_duration_daa("2m", "d"), Ok(1200));
        assert!(parse_duration_daa("30", "d").is_err());
        assert_eq!(parse_when("2026-10-20T00:00Z", "t"), Ok(When::Unix(1_792_454_400)));
        assert_eq!(parse_when("2026-10-20T01:02:03Z", "t"), Ok(When::Unix(1_792_454_400 + 3723)));
        assert_eq!(parse_when("daa:123", "t"), Ok(When::Daa(123)));
        for bad in ["2026-10-20", "2026-02-30T00:00Z", "2026-10-20T00:00", "2026-10-20T24:00Z", "tomorrow"] {
            assert!(parse_when(bad, "t").is_err(), "{bad}");
        }
        assert_eq!(fmt_utc(1_792_454_400 + 3723), "2026-10-20 01:02:03 UTC");
    }

    #[test]
    fn formatting() {
        assert_eq!(kas(250_000_000), "2.5 KAS");
        assert_eq!(kas(-1), "-0.00000001 KAS");
        assert_eq!(tokens(1_250_000_000, &tusd()), "12.5 TUSD (1250000000 base units)");
        assert_eq!(price_per_whole(25_000_000, &tusd(), 100_000_000, 8, "KAS"), "0.25 KAS per TUSD");
    }
}
