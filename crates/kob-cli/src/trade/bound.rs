//! The price bound of `kob take`: `--max-price` when the fill buys tokens, `--min-price` when it
//! sells them.
//!
//! A take fills a resting order at the order's own terms at the transaction's lock time. Between
//! the decision and the signature the maker can amend the order, and a decaying or rising
//! auction moves with the lock time, which comes from the node's DAA clock. The covenant only
//! keeps the order's own terms, so the bound is the taker's: the fill is refused before signing
//! when its price is worse.
//!
//! The price compared is the one the fill settles at, per whole token, the way the web wallet's
//! "worst price (all-in)" counts a tip. The taker is the fill's executor and keeps the order's
//! tip: taking a sell order pays the maker `price - tip` per token, taking a buy order receives
//! `price + tip`, both at the order's price at the signed lock time. The covenant's rounding of
//! the total (less than 1 sompi, in the maker's favour), the network fee and the carrier of the
//! taker's token output (which stays the taker's) are not part of it, as in the web wallet.

use kob_protocol::state::{quote_of, AnyState, Round};

use super::opts::Parsed;
use super::plan::{Issue, Severity};
use super::units::{self, parse_kas, TokenUnit};

/// The bound of a fill that buys tokens (taking a sell order).
pub const MAX_FLAG: &str = "--max-price";
/// The bound of a fill that sells tokens (taking a buy order).
pub const MIN_FLAG: &str = "--min-price";

/// A price bound in sompi per whole token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// `--max-price`: the fill pays at most this.
    Max(i64),
    /// `--min-price`: the fill receives at least this.
    Min(i64),
}

impl Bound {
    /// Sompi per whole token.
    pub fn per_token(self) -> i64 {
        match self {
            Bound::Max(v) | Bound::Min(v) => v,
        }
    }
    /// The option that gave it.
    pub fn flag(self) -> &'static str {
        match self {
            Bound::Max(_) => MAX_FLAG,
            Bound::Min(_) => MIN_FLAG,
        }
    }
}

/// The bound of the command line. `--yes` skips the confirmation and so needs one.
///
/// The value reads as `--price` does (`0.24KAS`, `24000000sompi`): KAS per whole token. It need
/// not be on the order's tick, because it is compared, never placed.
pub fn parse_bound(p: &Parsed) -> Result<Option<Bound>, String> {
    let read = |flag: &str| p.get(flag).map(|v| parse_kas(v, flag, false).map(|k| k as i64)).transpose();
    match (read(MAX_FLAG)?, read(MIN_FLAG)?) {
        (Some(_), Some(_)) => Err(format!(
            "give {MAX_FLAG} (a fill that buys: taking a sell order) or {MIN_FLAG} (a fill that sells: taking a buy order), not both"
        )),
        (Some(v), None) => Ok(Some(Bound::Max(v))),
        (None, Some(v)) => Ok(Some(Bound::Min(v))),
        (None, None) if p.has("--yes") => Err(format!(
            "a price bound is required when the confirmation is skipped: with --yes give {MAX_FLAG} P (taking a sell order) or {MIN_FLAG} P (taking a buy order)"
        )),
        (None, None) => Ok(None),
    }
}

/// What a take settles at, in state units (sompi per `scale` base units).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// The order's price at the lock time.
    pub price: i64,
    /// The order's tip, which the taker keeps.
    pub tip: i64,
    /// Whether the taker buys tokens, i.e. the order sells.
    pub taker_buys: bool,
    /// The signed lock time the price is taken at.
    pub lock: i64,
}

impl Terms {
    /// The taker's price: `price - tip` paid to a sell order, `price + tip` received from a buy
    /// order.
    pub fn rate(&self) -> i64 {
        if self.taker_buys {
            self.price - self.tip
        } else {
            self.price.saturating_add(self.tip)
        }
    }
    /// The KAS the fill of `n` base units pays the maker of a sell order (rounded up) or receives
    /// from a buy order (rounded down), as the covenant computes it.
    pub fn kas(&self, n: i64, scale: i64) -> Option<i64> {
        quote_of(n, self.rate(), scale, if self.taker_buys { Round::Up } else { Round::Down })
    }
}

/// The terms of a take of the order `s` with lock time `lock`, for the kinds `kob take` fills.
///
/// A decaying ask or a rising bid is priced at `lock`; every other kind at its limit (the
/// take-profit leg of a conditional order, the entry of an if-done order without an entry stop).
/// `None` for another kind, and where the covenant's auction arithmetic fails.
pub fn terms(s: &AnyState, lock: i64, utxo_daa: i64) -> Option<Terms> {
    let (price, tip, taker_buys) = match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => (x.price_at(lock, utxo_daa)?, x.tip, true),
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => (x.price_at(lock, utxo_daa)?, x.tip, false),
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => (x.tp_price, x.tip, true),
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => (x.tp_price, x.tip, false),
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => (x.price, x.tip, true),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => (x.price, x.tip, false),
        _ => return None,
    };
    Some(Terms { price, tip, taker_buys, lock })
}

/// The taker's price per whole token, for people.
pub fn rate_text(t: &Terms, scale: i64, unit: &TokenUnit) -> String {
    units::price_per_whole(t.rate(), unit, scale, units::KAS_DECIMALS, "KAS")
}

/// A bound per whole token, for people.
pub fn bound_text(b: Bound, unit: &TokenUnit) -> String {
    let tk = if unit.ticker.is_empty() { "token" } else { unit.ticker.as_str() };
    format!("{} KAS per {tk}", units::fmt_decimal(b.per_token() as i128, units::KAS_DECIMALS))
}

/// Checks the fill against the bound.
///
/// `Err` is a usage mistake: the bound of the other side (`--min-price` on a fill that buys). A
/// price worse than the bound is an `error` finding, so nothing is signed and `--json` prints
/// the plan with the reason. A price exactly at the bound passes.
pub fn check(bound: Option<Bound>, t: &Terms, scale: i64, unit: &TokenUnit) -> Result<Option<Issue>, String> {
    let Some(b) = bound else { return Ok(None) };
    match (b, t.taker_buys) {
        (Bound::Min(_), true) => {
            return Err(format!("this fill buys tokens (the order sells): bound it with {MAX_FLAG}, not {MIN_FLAG}"))
        }
        (Bound::Max(_), false) => {
            return Err(format!("this fill sells tokens (the order buys): bound it with {MIN_FLAG}, not {MAX_FLAG}"))
        }
        _ => {}
    }
    // rate / scale base units against bound / 10^decimals base units, exactly.
    let lhs = t.rate() as i128 * 10i128.pow(unit.decimals);
    let rhs = b.per_token() as i128 * scale as i128;
    let how = format!(
        "the order's price {} at the lock time (DAA {}), {} its tip {} that you keep",
        units::price_per_whole(t.price, unit, scale, units::KAS_DECIMALS, "KAS"),
        t.lock,
        if t.taker_buys { "less" } else { "plus" },
        units::price_per_whole(t.tip, unit, scale, units::KAS_DECIMALS, "KAS"),
    );
    let issue = |code, verb, side| Issue {
        code,
        severity: Severity::Error,
        message: format!(
            "this fill {verb} {} ({how}), {side} your {} {}: nothing signed",
            rate_text(t, scale, unit),
            b.flag(),
            bound_text(b, unit)
        ),
    };
    Ok(match b {
        Bound::Max(_) if lhs > rhs => Some(issue("TAKE_PRICE_ABOVE_MAX", "pays", "above")),
        Bound::Min(_) if lhs < rhs => Some(issue("TAKE_PRICE_BELOW_MIN", "receives", "below")),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::opts::parse;
    use kob_protocol::state::{AskState, BidState};

    fn tst() -> TokenUnit {
        TokenUnit { ticker: "TST".into(), decimals: 3 }
    }

    fn args(s: &str) -> Parsed {
        let v: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        parse(&v, &[MAX_FLAG, MIN_FLAG], &[]).unwrap()
    }

    fn ask(price: i64, tip: i64) -> AskState {
        AskState {
            maker: [1; 32],
            token_cov_id: [0x70; 32],
            token_tpl_hash: [0; 32],
            tpl_prefix_len: 0,
            tpl_suffix_len: 0,
            scale: 1000,
            min_fill: 1000,
            price,
            tip,
            tif: 0,
            active_from: 0,
            expiry_daa: 0,
            refund_tip: 0,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 1,
            amount_left: 10_000,
            extension_commitment: [0; 32],
        }
    }

    fn bid(price: i64, tip: i64) -> BidState {
        BidState {
            maker: [1; 32],
            token_cov_id: [0x70; 32],
            token_tpl_hash: [0; 32],
            tpl_prefix_len: 0,
            tpl_suffix_len: 0,
            extension_commitment: [0; 32],
            scale: 1000,
            min_fill: 1000,
            price,
            tip,
            tif: 0,
            active_from: 0,
            expiry_daa: 0,
            refund_tip: 0,
            reserve: 0,
            delivery_carrier: 0,
            interval: 0,
            max_fill: 0,
            slope: 0,
            price_end: 0,
            decay_step: 1,
        }
    }

    fn code(r: Result<Option<Issue>, String>) -> Option<&'static str> {
        r.unwrap().map(|i| i.code)
    }

    #[test]
    fn yes_needs_a_bound_and_one_side_only() {
        let e = parse_bound(&args("--yes")).unwrap_err();
        assert!(e.contains("a price bound is required when the confirmation is skipped"), "{e}");
        assert_eq!(parse_bound(&args("")), Ok(None), "without --yes the bound is optional");
        assert_eq!(parse_bound(&args("--yes --max-price 0.24KAS")), Ok(Some(Bound::Max(24_000_000))));
        assert_eq!(parse_bound(&args("--min-price 24000000sompi")), Ok(Some(Bound::Min(24_000_000))));
        assert!(parse_bound(&args("--max-price 1KAS --min-price 1KAS")).unwrap_err().contains("not both"));
        // The price syntax of --price: a unit is required and zero is not a bound.
        assert!(parse_bound(&args("--max-price 0.24")).unwrap_err().contains("no unit"));
        assert!(parse_bound(&args("--max-price 0KAS")).is_err());
        // Off the order's tick is fine: the bound is only compared.
        assert_eq!(parse_bound(&args("--max-price 0.00000001KAS")), Ok(Some(Bound::Max(1))));
    }

    #[test]
    fn a_buy_over_the_bound_is_refused_and_one_at_the_bound_passes() {
        // 3 KAS per TST: 3 decimals and scale 1000, so the state price is sompi per whole token.
        let t = terms(&AnyState::KobAsk(ask(300_000_000, 0)), 999_990, 990_000).unwrap();
        assert_eq!((t.rate(), t.kas(4000, 1000)), (300_000_000, Some(1_200_000_000)));
        assert_eq!(code(check(Some(Bound::Max(300_000_000)), &t, 1000, &tst())), None, "exactly at the bound");
        assert_eq!(code(check(Some(Bound::Max(299_999_999)), &t, 1000, &tst())), Some("TAKE_PRICE_ABOVE_MAX"));
        let i = check(Some(Bound::Max(299_000_000)), &t, 1000, &tst()).unwrap().unwrap();
        assert_eq!(i.severity, Severity::Error);
        assert!(i.message.contains("pays 3 KAS per TST") && i.message.contains("--max-price 2.99 KAS per TST"), "{}", i.message);
        assert_eq!(code(check(None, &t, 1000, &tst())), None);
    }

    #[test]
    fn the_tip_the_taker_keeps_counts() {
        // A sell order at 3 KAS with a 0.1 KAS tip: the maker receives 2.9 KAS, the taker keeps the
        // tip, so the fill pays 2.9 KAS per token, which a 2.9 KAS bound accepts.
        let t = terms(&AnyState::KobAsk(ask(300_000_000, 10_000_000)), 0, 0).unwrap();
        assert_eq!(t.rate(), 290_000_000);
        assert_eq!(code(check(Some(Bound::Max(290_000_000)), &t, 1000, &tst())), None);
        assert_eq!(code(check(Some(Bound::Max(289_999_999)), &t, 1000, &tst())), Some("TAKE_PRICE_ABOVE_MAX"));
        // A buy order at 2 KAS with a 0.1 KAS tip pays the taker 2.1 KAS per token.
        let t = terms(&AnyState::KobBid(bid(200_000_000, 10_000_000)), 0, 0).unwrap();
        assert_eq!(t.rate(), 210_000_000);
        assert_eq!(code(check(Some(Bound::Min(210_000_000)), &t, 1000, &tst())), None);
        assert_eq!(code(check(Some(Bound::Min(210_000_001)), &t, 1000, &tst())), Some("TAKE_PRICE_BELOW_MIN"));
    }

    #[test]
    fn a_sell_under_the_bound_is_refused_and_one_at_the_bound_passes() {
        let t = terms(&AnyState::KobBid(bid(200_000_000, 0)), 999_990, 990_000).unwrap();
        assert!(!t.taker_buys);
        assert_eq!(t.kas(2000, 1000), Some(400_000_000));
        assert_eq!(code(check(Some(Bound::Min(200_000_000)), &t, 1000, &tst())), None, "exactly at the bound");
        assert_eq!(code(check(Some(Bound::Min(200_000_001)), &t, 1000, &tst())), Some("TAKE_PRICE_BELOW_MIN"));
        assert_eq!(code(check(Some(Bound::Min(150_000_000)), &t, 1000, &tst())), None);
    }

    #[test]
    fn the_bound_of_the_other_side_is_a_usage_mistake() {
        let buy = terms(&AnyState::KobAsk(ask(300_000_000, 0)), 0, 0).unwrap();
        assert!(check(Some(Bound::Min(1)), &buy, 1000, &tst()).unwrap_err().contains("bound it with --max-price"));
        let sell = terms(&AnyState::KobBid(bid(200_000_000, 0)), 0, 0).unwrap();
        assert!(check(Some(Bound::Max(1)), &sell, 1000, &tst()).unwrap_err().contains("bound it with --min-price"));
    }

    #[test]
    fn a_dutch_ask_is_priced_at_the_signed_lock_time() {
        // From 4 KAS at DAA 999 000 down 0.001 KAS per DAA to 2 KAS.
        let dutch = AnyState::KobAsk(AskState { active_from: 999_000, slope: 100_000, price_end: 200_000_000, ..ask(400_000_000, 0) });
        let at = |lock| terms(&dutch, lock, 990_000).unwrap();
        assert_eq!(at(999_990).price, 301_000_000);
        // A lagging node signs an earlier lock time, at which the ask is dearer.
        assert_eq!(at(999_490).price, 351_000_000);
        let b = Some(Bound::Max(305_000_000));
        assert_eq!(code(check(b, &at(999_990), 1000, &tst())), None);
        let i = check(b, &at(999_490), 1000, &tst()).unwrap().unwrap();
        assert_eq!(i.code, "TAKE_PRICE_ABOVE_MAX");
        assert!(i.message.contains("DAA 999490") && i.message.contains("3.51 KAS per TST"), "{}", i.message);
    }

    #[test]
    fn a_rate_off_the_whole_token_grid_compares_exactly() {
        // Scale 10^9 for a 12-decimal token: the state price is sompi per 10^9 base units, so a
        // whole token is 1000 times it.
        let big = TokenUnit { ticker: "BIG".into(), decimals: 12 };
        let t = Terms { price: 100_001, tip: 0, taker_buys: true, lock: 0 };
        assert_eq!(code(check(Some(Bound::Max(100_001_000)), &t, 1_000_000_000, &big)), None);
        assert_eq!(code(check(Some(Bound::Max(100_000_999)), &t, 1_000_000_000, &big)), Some("TAKE_PRICE_ABOVE_MAX"));
    }
}
