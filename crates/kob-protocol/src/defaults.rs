//! Wallet defaults (`docs/spec/matcher.md` §10) and the keeper tips derived from measured fees.
//!
//! **Tips.** A refund or an arm / trail update is sent by a keeper (an update: by a matcher, inside the batch
//! that fills its evidence) and paid only by the order's
//! own `refundTip` / `keeperTip`. The fee of that transaction depends on the token program (an
//! ask-side refund pushes the whole token program in its custody input), so one constant cannot
//! fit every token: 0.03 KAS covered a 3/3 refund but not a 4/5 or 8/8 one. The defaults are
//! therefore per program and derived from measured fees: `data/keeper_tips.json` holds, for every
//! supported program (both families), the largest fee floor of a keeper transaction (a funded keeper with its own
//! change output, the costlier shape) over every KAS order kind (`KobAsk` .. `KobIfdAsk` and their KRON twins), measured
//! by `tests/keeper_tips.rs` through the builders (`KOB_REGEN=1` rewrites it), and the default tip is twice that fee
//! rounded up to 0.001 KAS: the keeper earns about one fee, and a keeper without capital (no funding, no change) is always
//! covered.
//!
//! The pair kinds (`KobPair`, `KobCondPair`, `KobIfdPair`) have their own table, `data/pair_keeper_tips.json`, per
//! program PAIR (`<program of A>+<program of B>`): a pair order's keeper transaction carries both tokens' programs (a
//! sell-first `KobIfdPair`'s refund returns two custodies), and its larger fees must not raise the tips of the KAS kinds.
//! [`tips_for`] selects the table by the order's kind.
//!
//! **Day orders.** [`day_order`] maps "until the next 00:00 UTC" to an on-chain `expiryDaa` with
//! a 1% margin and the wall-clock `deadline` published in the placement record (§10.10).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::artifacts::TemplateId;
use crate::error::{Error, Result};

/// Market order: auction length from the touch to the slippage bound (20 s).
pub const MARKET_AUCTION_DAA: i64 = 200;
/// Market / stop slippage bound, basis points (3%).
pub const SLIPPAGE_BPS: i64 = 300;
/// Market order: activation delay after placement (DAA).
pub const MARKET_ACTIVATION_DAA: i64 = 30;
/// Wallet life of an IOC / FOK order (the covenant kill is at 600 DAA).
pub const IOC_LIFE_DAA: i64 = 300;
/// Stop auction length (30 s).
pub const STOP_BAND_DAA: i64 = 300;
/// Trigger rest time R (5 s, founder 2026-09-30): the evidence order must have been exposed at its quote
/// for at least this long before the transaction that fills it arms a stop (`minRestDaa`).
pub const MIN_REST_DAA: i64 = 50;
/// Default minimum fill, in sompi of quote value (10 KAS, one default delivery carrier): the wallet's `minFill` is the
/// amount worth this much at the order's limit price ([`default_min_fill`]). Every fill of a bid-side order moves a
/// delivery carrier onto a new token UTXO and every fill restarts TWAP / DCA intervals, so the minimum bounds the fills
/// (carriers, UTXOs) a filler can force on the maker to `ceil(amount / minFill)` (founder to confirm the value).
pub const DEFAULT_MIN_FILL_SOMPI: i64 = 1_000_000_000;

/// Wallet default of an order's `minFill` (base units): `clamp(ceil(DEFAULT_MIN_FILL_SOMPI * scale / price), 1, amount)`,
/// the amount worth [`DEFAULT_MIN_FILL_SOMPI`] at the limit `price` (sompi per whole token of `scale` base units), at least
/// one base unit and at most the order's amount. IOC / FOK and market orders use 1 (they live one auction; nothing to
/// grief). A non-positive price or amount gives 1.
pub fn default_min_fill(amount: i64, price: i64, scale: i64) -> i64 {
    if amount <= 0 || price <= 0 || scale <= 0 {
        return 1;
    }
    let want = (DEFAULT_MIN_FILL_SOMPI as i128 * scale as i128 + price as i128 - 1) / price as i128;
    want.clamp(1, amount as i128) as i64
}

/// Wallet default of an if-done entry's `minFill`: `ceil(amount / 4)` (at most four fills: an entry prefunds a
/// delivery and an exit carrier per possible fill, so a 10-KAS minimum would prefund up to 2 x 10 KAS per fill).
pub fn default_min_fill_ifd(amount: i64) -> i64 {
    if amount <= 0 {
        return 1;
    }
    (amount / 4 + i64::from(amount % 4 != 0)).max(1)
}

/// Wallet default of the `minFill` of IOC / FOK orders, market orders and pair market orders: 1 base unit (they live
/// one auction; there is nothing to grief).
pub const DEFAULT_MIN_FILL_IMMEDIATE: i64 = 1;

/// Wallet default of a pair order's `minFill` (base units of A): the amount of A worth [`DEFAULT_MIN_FILL_SOMPI`] on
/// A's KAS book at placement, `kas_per_whole_a` being the KAS value (sompi per whole A) the caller quotes
/// ([`default_min_fill`] at that price); without a quote, `ceil(amount / 4)`.
pub fn default_min_fill_pair(amount: i64, kas_per_whole_a: Option<i64>, scale: i64) -> i64 {
    match kas_per_whole_a {
        Some(p) if p > 0 => default_min_fill(amount, p, scale),
        _ => default_min_fill_ifd(amount),
    }
}

/// Wallet default of a stop's `minTouch` (the smallest trigger evidence, base units of a plain order of the same scale):
/// the order's own minimum fill (founder 2026-10-03, was "1 lot"). The user may choose another per order: the minimum
/// fill, 25% / 50% / 100% of the order's amount (100% = strongest protection against stop hunting) or a custom amount.
pub fn default_min_touch(min_fill: i64) -> i64 {
    min_fill.max(1)
}

/// Wallet default of an order's `scale` (base units per whole token, the price denominator): `10^min(decimals, 9)`.
pub fn default_scale(decimals: u32) -> i64 {
    10i64.pow(decimals.min(9))
}
/// Nominal DAA rate, milli-DAA per second (10 DAA/s), and its accepted measurement range.
pub const DAA_RATE_MILLI: u64 = 10_000;
pub const DAA_RATE_MILLI_MIN: u64 = 9_500;
pub const DAA_RATE_MILLI_MAX: u64 = 10_500;
/// Tips are rounded up to this many sompi (0.001 KAS).
pub const TIP_ROUNDING: u64 = 100_000;

/// Measured keeper fees and the derived default tips of one token program (sompi).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tips {
    /// Largest fee floor of a refund / kill / close over every order kind.
    #[serde(with = "crate::json::field")]
    pub refund_fee: u64,
    /// Default `refundTip` (2 × `refundFee`, rounded up).
    #[serde(with = "crate::json::field")]
    pub refund_tip: u64,
    /// Largest fee floor of an arm / trail update over every updatable kind.
    #[serde(with = "crate::json::field")]
    pub update_fee: u64,
    /// Default `keeperTip` (2 × `updateFee`, rounded up).
    #[serde(with = "crate::json::field")]
    pub keeper_tip: u64,
}

/// Default tip for a measured keeper fee: twice the fee, rounded up to [`TIP_ROUNDING`].
pub fn tip_for_fee(fee: u64) -> u64 {
    (2 * fee).div_ceil(TIP_ROUNDING) * TIP_ROUNDING
}

const TIPS_JSON: &str = include_str!("../data/keeper_tips.json");

/// The committed table (program name -> tips).
pub fn tips_table() -> &'static BTreeMap<String, Tips> {
    static T: OnceLock<BTreeMap<String, Tips>> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(TIPS_JSON).expect("keeper tips json"))
}

/// Default tips of a token program (the KAS order kinds).
pub fn tips(program: TemplateId) -> Result<Tips> {
    tips_table().get(program.name()).copied().ok_or_else(|| Error::Invalid(format!("no keeper tips for {}", program.name())))
}

const PAIR_TIPS_JSON: &str = include_str!("../data/pair_keeper_tips.json");

/// The committed pair table (`<program of A>+<program of B>` -> tips).
pub fn pair_tips_table() -> &'static BTreeMap<String, Tips> {
    static T: OnceLock<BTreeMap<String, Tips>> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(PAIR_TIPS_JSON).expect("pair keeper tips json"))
}

/// The key of a program pair in the pair table.
pub fn pair_tips_key(a: TemplateId, b: TemplateId) -> String {
    format!("{}+{}", a.name(), b.name())
}

/// Default tips of a pair order whose base token A runs program `a` and quote token B program `b`.
pub fn pair_tips(a: TemplateId, b: TemplateId) -> Result<Tips> {
    let k = pair_tips_key(a, b);
    pair_tips_table().get(&k).copied().ok_or_else(|| Error::Invalid(format!("no keeper tips for the pair {k}")))
}

/// Default tips of an order, by its kind: a pair order's from the pair table (its two programs), any other kind's from the
/// table of its token program.
pub fn tips_for(s: &crate::state::AnyState) -> Result<Tips> {
    if s.is_pair() {
        let (a, b) = crate::build::pair_programs(s)?;
        return pair_tips(a.1, b.1);
    }
    let (p, x) = s.token_tpl_lens();
    tips(crate::build::token_program(&s.token_tpl_hash().expect("orders pin a token program"), p, x)?)
}

/// A day order's on-chain expiry and wall-clock deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayOrder {
    /// `D0 + ⌈Δ·r⌉ + ⌈0.01·Δ·r⌉`: the refund opens at or after midnight unless the DAA rate
    /// beats `r` by more than 1%.
    #[serde(with = "crate::json::field")]
    pub expiry_daa: u64,
    /// The next 00:00 UTC (unix seconds), for the placement record.
    #[serde(with = "crate::json::field")]
    pub deadline: u64,
}

/// Day order ending at the next 00:00 UTC. `d0` and `t0` are the node's virtual DAA score
/// and the UTC wall clock (unix seconds) read together; `rate_milli` is the DAA rate measured over
/// the last hour in milli-DAA per second (default 10,000, clamped to 9,500..=10,500).
pub fn day_order(d0: u64, t0: u64, rate_milli: Option<u64>) -> DayOrder {
    let r = rate_milli.unwrap_or(DAA_RATE_MILLI).clamp(DAA_RATE_MILLI_MIN, DAA_RATE_MILLI_MAX);
    let deadline = (t0 / 86_400 + 1) * 86_400;
    let delta = deadline - t0;
    let estimate = (delta * r).div_ceil(1_000);
    let margin = (delta * r).div_ceil(100_000);
    DayOrder { expiry_daa: d0 + estimate + margin, deadline }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_order_ends_at_utc_midnight_with_a_one_percent_margin() {
        // 15:00:00 UTC: 9 h to midnight.
        let t0 = 1_790_694_000;
        let d = day_order(1_000_000, t0, None);
        assert_eq!(d.deadline, 1_790_726_400);
        assert_eq!(d.expiry_daa, 1_000_000 + 324_000 + 3_240);
        // A measured rate is clamped.
        assert_eq!(day_order(0, t0, Some(20_000)), day_order(0, t0, Some(10_500)));
        // Exactly at midnight the order runs for a whole day.
        assert_eq!(day_order(0, 1_790_726_400, None).deadline, 1_790_726_400 + 86_400);
    }

    #[test]
    fn min_fill_scale_and_touch_defaults() {
        // 10 KAS at 2.5 KAS per whole token of 10^8 base units: 4 whole tokens
        assert_eq!(default_min_fill(1_000_000_000_000, 250_000_000, 100_000_000), 400_000_000);
        // rounded up: 10 KAS at 3 KAS per token of 1000 base units is 3333.3.. base units
        assert_eq!(default_min_fill(1_000_000, 300_000_000, 1_000), 3_334);
        // clamped to the amount and to one base unit
        assert_eq!(default_min_fill(5, 300_000_000, 1_000), 5);
        assert_eq!(default_min_fill(1_000, i64::MAX, 1), 1);
        assert_eq!(default_min_fill(0, 1, 1), 1);
        assert_eq!(default_min_fill(i64::MAX, 1, 1_000_000_000), 1_000_000_000_000_000_000);
        assert_eq!(
            (default_scale(0), default_scale(8), default_scale(9), default_scale(18)),
            (1, 100_000_000, 1_000_000_000, 1_000_000_000)
        );
        assert_eq!((default_min_touch(400), default_min_touch(0)), (400, 1));
        // if-done entries: at most four fills; pair orders: 10 KAS of A at the quoted KAS price, else as an entry
        assert_eq!(
            (default_min_fill_ifd(10), default_min_fill_ifd(8), default_min_fill_ifd(1), default_min_fill_ifd(0)),
            (3, 2, 1, 1)
        );
        assert_eq!(default_min_fill_pair(1_000_000, Some(300_000_000), 1_000), 3_334);
        assert_eq!(default_min_fill_pair(1_000_000, None, 1_000), 250_000);
        assert_eq!(default_min_fill_pair(1_000_000, Some(0), 1_000), 250_000);
        assert_eq!(DEFAULT_MIN_FILL_IMMEDIATE, 1);
    }

    #[test]
    fn every_program_has_tips_covering_its_fees() {
        let programs: Vec<TemplateId> = TemplateId::ALL.into_iter().filter(|t| t.is_token()).collect();
        for id in &programs {
            let t = tips(*id).unwrap();
            assert_eq!(t.refund_tip, tip_for_fee(t.refund_fee), "{}", id.name());
            assert_eq!(t.keeper_tip, tip_for_fee(t.update_fee), "{}", id.name());
            assert!(t.refund_tip >= 2 * t.refund_fee && t.keeper_tip >= 2 * t.update_fee);
        }
        // every program pair has its pair tips
        for a in &programs {
            for b in &programs {
                let t = pair_tips(*a, *b).unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(t.refund_tip, tip_for_fee(t.refund_fee), "{}", pair_tips_key(*a, *b));
                assert_eq!(t.keeper_tip, tip_for_fee(t.update_fee), "{}", pair_tips_key(*a, *b));
            }
        }
        assert_eq!(pair_tips_table().len(), programs.len() * programs.len());
    }
}
