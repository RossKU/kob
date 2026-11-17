//! The default price table of a curve (`docs/design/curve-ask-spec.md` §7).
//!
//! A geometric table `p_i = p0·r^i`, `r = multiple^(1/(n − 1))`, with `p0` solved so that a
//! sell-out, bought level by level, realises at most `total` and as close to it as the integer
//! prices allow. Everything is integer arithmetic (Q40 ratios), so every platform builds the same
//! table.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::{quote_of, Round, CURVE_MAX_LEVELS};

/// UI default: levels of a new curve.
pub const DEFAULT_CURVE_LEVELS: usize = 100;
/// UI default: what the curve realises if it sells out, in sompi (10,000 KAS).
pub const DEFAULT_CURVE_TOTAL_SOMPI: i64 = 1_000_000_000_000;
/// UI default: the last level's price over the first's, in thousandths (5×).
pub const DEFAULT_CURVE_MULTIPLE_MILLI: u64 = 5_000;

const Q: u32 = 40;
const ONE: u128 = 1 << Q;
/// Fraction bits of `p0` in the search.
const P0_BITS: u32 = 24;

/// A computed table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurveTable {
    /// Base units per level.
    #[serde(with = "crate::json::field")]
    pub level_size: i64,
    /// Sompi per whole token of each level.
    #[serde(with = "crate::json::field_vec")]
    pub table: Vec<i64>,
    /// What a sell-out bought level by level pays: `Σ ⌈s_i·p_i / scale⌉` (at most the target).
    #[serde(with = "crate::json::field")]
    pub realised: i64,
}

/// The requested curve: sell `amount` base units in `levels` levels for `total` sompi, the last
/// level `multiple_milli / 1000` times the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurveSpec {
    #[serde(with = "crate::json::field")]
    pub amount: i64,
    pub levels: usize,
    #[serde(with = "crate::json::field")]
    pub total: i64,
    #[serde(with = "crate::json::field")]
    pub multiple_milli: u64,
    /// Base units per whole token.
    #[serde(with = "crate::json::field")]
    pub scale: i64,
    /// Sompi per whole token taken from every level: the first price is at least this (and 1).
    #[serde(with = "crate::json::field", default)]
    pub tip: i64,
}

impl CurveSpec {
    /// The UI defaults for `amount` (the whole supply) at `scale`.
    pub fn defaults(amount: i64, scale: i64) -> Self {
        CurveSpec {
            amount,
            levels: DEFAULT_CURVE_LEVELS,
            total: DEFAULT_CURVE_TOTAL_SOMPI,
            multiple_milli: DEFAULT_CURVE_MULTIPLE_MILLI,
            scale,
            tip: 0,
        }
    }
}

fn bad<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(format!("curve: {}", m.into())))
}

/// `x^k` in Q40, truncated after every product; `None` past `cap`.
fn pow_q(x: u128, k: usize, cap: u128) -> Option<u128> {
    let mut acc = ONE;
    for _ in 0..k {
        acc = acc.checked_mul(x)? >> Q;
        if acc > cap {
            return None;
        }
    }
    Some(acc)
}

/// The level prices for `p0` in Q24 and the weights `r^i` in Q40.
fn prices(p0: u128, w: &[u128]) -> Option<Vec<i64>> {
    w.iter().map(|wi| i64::try_from(p0.checked_mul(*wi)? >> (Q + P0_BITS)).ok()).collect()
}

/// `Σ ⌈s_i·p_i / scale⌉`, `None` on overflow.
fn realised(sizes: &[i64], table: &[i64], scale: i64) -> Option<i64> {
    sizes.iter().zip(table).try_fold(0i64, |acc, (s, p)| acc.checked_add(quote_of(*s, *p, scale, Round::Up)?))
}

/// The table of `spec` (spec §7).
pub fn curve_table(spec: &CurveSpec) -> Result<CurveTable> {
    let CurveSpec { amount, levels, total, multiple_milli, scale, tip } = *spec;
    if amount <= 0 || total <= 0 || scale <= 0 || tip < 0 {
        return bad("amount, total and scale must be > 0, tip >= 0");
    }
    if levels == 0 || levels > CURVE_MAX_LEVELS {
        return bad(format!("1 to {CURVE_MAX_LEVELS} levels, got {levels}"));
    }
    if multiple_milli < 1_000 {
        return bad("the last level is at least the first (multiple >= 1)");
    }
    // fewer levels when the amount is small: every level holds at least one base unit
    let level_size = (amount + levels as i64 - 1) / levels as i64;
    let n = ((amount + level_size - 1) / level_size) as usize;
    let mut sizes = vec![level_size; n];
    sizes[n - 1] = amount - (n as i64 - 1) * level_size;
    // r = multiple^(1/(n-1)) in Q40: the largest r with r^(n-1) <= multiple
    let m = (multiple_milli as u128) * ONE / 1_000;
    let r = if n == 1 {
        ONE
    } else {
        let (mut lo, mut hi) = (ONE, m);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            match pow_q(mid, n - 1, m) {
                Some(_) => lo = mid,
                None => hi = mid - 1,
            }
        }
        lo
    };
    let mut w = Vec::with_capacity(n);
    let mut acc = ONE;
    for _ in 0..n {
        w.push(acc);
        acc = acc.saturating_mul(r) >> Q;
    }
    // p0 in Q24: the largest whose table realises at most the target
    let fits = |p0: u128| prices(p0, &w).and_then(|t| realised(&sizes, &t, scale)).is_some_and(|r| r <= total);
    let (mut lo, mut hi) = (0u128, (i64::MAX as u128) << P0_BITS);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut table = prices(lo, &w).expect("fits");
    let mut got = realised(&sizes, &table, scale).expect("fits");
    // spend what is left, from the top level down, keeping the table non-decreasing
    for i in (0..n).rev() {
        let here = quote_of(sizes[i], table[i], scale, Round::Up).expect("fits");
        let budget = here + (total - got);
        let mut p = i64::try_from((budget as i128) * (scale as i128) / (sizes[i] as i128)).unwrap_or(i64::MAX);
        if i + 1 < n {
            p = p.min(table[i + 1]);
        }
        if p > table[i] {
            let now = quote_of(sizes[i], p, scale, Round::Up).expect("within the budget");
            got += now - here;
            table[i] = p;
        }
    }
    if table[0] < tip.max(1) {
        return bad(format!(
            "the first level would be priced {} sompi per whole token, below {} (raise the total or lower the amount)",
            table[0],
            tip.max(1)
        ));
    }
    debug_assert!(got <= total && table.windows(2).all(|x| x[0] <= x[1]));
    Ok(CurveTable { level_size, table, realised: got })
}
