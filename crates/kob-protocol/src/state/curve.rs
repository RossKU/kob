//! `KobCurveAsk` state (`docs/design/curve-ask-spec.md` §1).
//!
//! Fixed-size fields, then the price table: one minimal push of `8·nLevels` bytes. Its length
//! varies, so the codec is written here rather than read from the artifact's fixed span.

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;
use serde::{Deserialize, Serialize};

use super::{check_scale, quote_exact, quote_of, Round, StateError, QUOTE_LIMIT};
use crate::artifacts::{template, TemplateId};

/// State offset of the table push.
pub const CURVE_TABLE_AT: usize = 213;
/// Redeem window of `amountLeft` (the only mutable field).
pub const CURVE_LEFT_WINDOW: (usize, usize) = (173, 181);
/// Redeem window of `tokenCovId` (zero while unbound; the first fill writes it).
pub const CURVE_COV_WINDOW: (usize, usize) = (35, 67);
/// Most levels one fill crosses (`MAX_CROSS` of the contract).
pub const CURVE_MAX_CROSS: usize = 8;
/// Most levels a curve may have (a gate: every level adds 8 B to every fill).
pub const CURVE_MAX_LEVELS: usize = 1_000;
const NAME: &str = "KobCurveAsk";

/// `KobCurveAsk`: one custody sold along a price table, level by level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurveAskState {
    #[serde(rename = "maker", with = "crate::json::field")]
    pub maker: [u8; 32],
    /// Zero while unbound (created in its token's genesis); the first fill writes it.
    #[serde(rename = "tokenCovId", with = "crate::json::field")]
    pub token_cov_id: [u8; 32],
    #[serde(rename = "tokenTplHash", with = "crate::json::field")]
    pub token_tpl_hash: [u8; 32],
    #[serde(rename = "tplPrefixLen", with = "crate::json::field")]
    pub tpl_prefix_len: i64,
    #[serde(rename = "tplSuffixLen", with = "crate::json::field")]
    pub tpl_suffix_len: i64,
    #[serde(rename = "scale", with = "crate::json::field")]
    pub scale: i64,
    #[serde(rename = "minFill", with = "crate::json::field")]
    pub min_fill: i64,
    /// Sompi per whole token, taken from every level (all-in prices).
    #[serde(rename = "tip", with = "crate::json::field")]
    pub tip: i64,
    #[serde(rename = "activeFrom", with = "crate::json::field")]
    pub active_from: i64,
    /// Base units per level.
    #[serde(rename = "levelSize", with = "crate::json::field")]
    pub level_size: i64,
    /// Base units the curve sells in all (immutable).
    #[serde(rename = "total", with = "crate::json::field")]
    pub total: i64,
    /// Base units in custody (mutable).
    #[serde(rename = "amountLeft", with = "crate::json::field")]
    pub amount_left: i64,
    #[serde(rename = "extensionCommitment", with = "crate::json::field")]
    pub extension_commitment: [u8; 32],
    /// Sompi per whole token of each level.
    #[serde(rename = "table", with = "table_field")]
    pub table: Vec<i64>,
}

/// Most levels a table can hold: one push of at most 65,535 bytes, 8 per level.
pub const CURVE_TABLE_MAX_ENTRIES: usize = 65_535 / 8;

/// The table's JSON, as `json::field_vec`. A table with no level, or more than one push holds, is
/// refused as it is read, so every state read encodes ([`CurveAskState::try_encode`]): an indexer
/// that serves one gets an error, never a panic (a trap in kob-wasm).
mod table_field {
    use serde::{de, Deserializer, Serializer};

    use super::CURVE_TABLE_MAX_ENTRIES;

    pub fn serialize<S: Serializer>(v: &[i64], s: S) -> Result<S::Ok, S::Error> {
        crate::json::field_vec::serialize(v, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i64>, D::Error> {
        let v: Vec<i64> = crate::json::field_vec::deserialize(d)?;
        if v.is_empty() || v.len() > CURVE_TABLE_MAX_ENTRIES {
            return Err(de::Error::custom(format!("a curve table has 1 to {CURVE_TABLE_MAX_ENTRIES} levels, got {}", v.len())));
        }
        Ok(v)
    }
}

fn err(m: impl Into<String>) -> StateError {
    StateError::Codec(NAME, m.into())
}

/// The minimal push header of `len` data bytes (1 to 65,535).
fn push_header(len: usize) -> Result<Vec<u8>, StateError> {
    Ok(match len {
        1..=75 => vec![len as u8],
        76..=255 => vec![0x4c, len as u8],
        256..=65_535 => vec![0x4d, len as u8, (len >> 8) as u8],
        _ => return Err(err(format!("a table of {len} bytes has no push"))),
    })
}

impl CurveAskState {
    /// Number of levels.
    pub fn levels(&self) -> usize {
        self.table.len()
    }

    /// True while `tokenCovId` is zero.
    pub fn is_unbound(&self) -> bool {
        self.token_cov_id == [0; 32]
    }

    /// The units sold so far.
    pub fn sold(&self) -> i64 {
        // saturating: a hostile state reads as a bound `walk` refuses, never a wrap (R8F1c-3)
        self.total.saturating_sub(self.amount_left)
    }

    /// The current level: `(total - amountLeft) / levelSize`.
    pub fn level(&self) -> i64 {
        if self.level_size > 0 {
            self.sold() / self.level_size
        } else {
            0
        }
    }

    /// The covenant's walk: the maker's proceeds of a fill of `n`, `None` where it refuses (more
    /// than 8 levels, past the table, a level below the tip, an overflow).
    pub fn proceeds(&self, n: i64) -> Option<i64> {
        self.walk(n).map(|(c, _)| c)
    }

    /// Proceeds and levels touched of a fill of `n`, as the contract computes them.
    pub fn walk(&self, n: i64) -> Option<(i64, usize)> {
        if n <= 0 || n > self.amount_left || self.level_size <= 0 || self.amount_left > self.total || self.scale <= 0 || self.tip < 0 {
            return None;
        }
        let (mut pos, end) = (self.sold(), self.sold().checked_add(n)?);
        let (mut cost, mut crossed) = (0i64, 0usize);
        while pos < end {
            if crossed == CURVE_MAX_CROSS {
                return None;
            }
            let lvl = pos / self.level_size;
            let upto = (lvl + 1).checked_mul(self.level_size)?.min(end);
            let p = *self.table.get(usize::try_from(lvl).ok()?)?;
            if p < self.tip {
                return None;
            }
            cost = cost.checked_add(quote_of(upto - pos, p - self.tip, self.scale, Round::Up)?)?;
            pos = upto;
            crossed += 1;
        }
        Some((cost, crossed))
    }

    /// The most base units `budget` buys in one fill (at most 8 levels), the taker's market buy.
    pub fn max_for_budget(&self, budget: i64) -> i64 {
        let (mut lo, mut hi) = (0, self.amount_left.max(0));
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
            match self.proceeds(mid) {
                Some(c) if c <= budget => lo = mid,
                _ => hi = mid - 1,
            }
        }
        lo
    }

    /// The most base units one fill can take from here: the rest of 8 levels (or of the curve).
    pub fn max_fill(&self) -> i64 {
        if self.level_size <= 0 {
            return 0;
        }
        let to = self.level().saturating_add(CURVE_MAX_CROSS as i64).saturating_mul(self.level_size);
        to.saturating_sub(self.sold()).min(self.amount_left).max(0)
    }

    /// The level price of the next unit (`None` when sold out or past the table).
    pub fn price_now(&self) -> Option<i64> {
        if self.amount_left <= 0 {
            return None;
        }
        self.table.get(usize::try_from(self.level()).ok()?).copied()
    }

    /// Whether `n` is a fill the covenant accepts by size: `n <= amountLeft`, `n >= minFill`
    /// unless it takes all, within 8 levels.
    pub fn fill_ok(&self, n: i64) -> bool {
        n > 0 && n <= self.amount_left && (n >= self.min_fill || n == self.amount_left) && self.walk(n).is_some()
    }

    /// The state span.
    pub fn try_encode(&self) -> Result<Vec<u8>, StateError> {
        if self.table.is_empty() {
            return Err(err("the table has no level"));
        }
        let mut v = Vec::with_capacity(CURVE_TABLE_AT + 3 + 8 * self.table.len());
        let b32 = |v: &mut Vec<u8>, b: &[u8; 32]| {
            v.push(0x20);
            v.extend_from_slice(b);
        };
        // 8-byte script numbers, sign-magnitude as the covenant reads them (R8F1a-2)
        let n = |i: i64| super::int8_bytes(i).ok_or_else(|| err("a curve number has no 8-byte script number (i64::MIN)"));
        let int = |v: &mut Vec<u8>, i: i64| -> Result<(), StateError> {
            v.push(0x08);
            v.extend_from_slice(&n(i)?);
            Ok(())
        };
        b32(&mut v, &self.maker);
        b32(&mut v, &self.token_cov_id);
        b32(&mut v, &self.token_tpl_hash);
        for i in [
            self.tpl_prefix_len,
            self.tpl_suffix_len,
            self.scale,
            self.min_fill,
            self.tip,
            self.active_from,
            self.level_size,
            self.total,
            self.amount_left,
        ] {
            int(&mut v, i)?;
        }
        b32(&mut v, &self.extension_commitment);
        debug_assert_eq!(v.len(), CURVE_TABLE_AT);
        v.extend(push_header(8 * self.table.len())?);
        for p in &self.table {
            v.extend_from_slice(&n(*p)?);
        }
        Ok(v)
    }

    /// The state span; panics on a state [`CurveAskState::try_encode`] refuses (an empty table).
    pub fn encode(&self) -> Vec<u8> {
        self.try_encode().unwrap_or_else(|e| panic!("{e}"))
    }

    /// Decodes a state span: canonical pushes only, the table a minimal push of whole entries.
    pub fn decode(b: &[u8]) -> Result<Self, StateError> {
        if b.len() < CURVE_TABLE_AT + 9 {
            return Err(err(format!("state span of {} bytes is too short", b.len())));
        }
        let mut at = 0usize;
        let mut b32 = |want: u8| -> Result<[u8; 32], StateError> {
            if b[at] != want {
                return Err(err(format!("byte {at}: expected push {want:#04x}, got {:#04x}", b[at])));
            }
            let x: [u8; 32] = b[at + 1..at + 33].try_into().expect("32");
            at += 33;
            Ok(x)
        };
        let maker = b32(0x20)?;
        let token_cov_id = b32(0x20)?;
        let token_tpl_hash = b32(0x20)?;
        let mut ints = [0i64; 9];
        for x in ints.iter_mut() {
            if b[at] != 0x08 {
                return Err(err(format!("byte {at}: expected an 8-byte push, got {:#04x}", b[at])));
            }
            *x = super::int8_value(b[at + 1..at + 9].try_into().expect("8"))
                .ok_or_else(|| err(format!("byte {at}: a negative zero")))?;
            at += 9;
        }
        if b[at] != 0x20 {
            return Err(err(format!("byte {at}: expected push 0x20, got {:#04x}", b[at])));
        }
        let extension_commitment: [u8; 32] = b[at + 1..at + 33].try_into().expect("32");
        at += 33;
        let (h, len) = match b[at] {
            op @ 0x01..=0x4b => (1, op as usize),
            0x4c => (2, *b.get(at + 1).ok_or_else(|| err("truncated table push"))? as usize),
            0x4d => (
                3,
                u16::from_le_bytes([*b.get(at + 1).unwrap_or(&0), *b.get(at + 2).ok_or_else(|| err("truncated table push"))?])
                    as usize,
            ),
            op => return Err(err(format!("the table is not a data push ({op:#04x})"))),
        };
        if b.len() != at + h + len || len == 0 || len % 8 != 0 || push_header(len)?.len() != h {
            return Err(err(format!("the table push is not a minimal push of whole entries ({} bytes)", b.len() - at)));
        }
        let table = b[at + h..]
            .chunks(8)
            .map(|c| super::int8_value(c.try_into().expect("8")).ok_or_else(|| err("a table entry is a negative zero")))
            .collect::<Result<Vec<i64>, _>>()?;
        let [tpl_prefix_len, tpl_suffix_len, scale, min_fill, tip, active_from, level_size, total, amount_left] = ints;
        let s = CurveAskState {
            maker,
            token_cov_id,
            token_tpl_hash,
            tpl_prefix_len,
            tpl_suffix_len,
            scale,
            min_fill,
            tip,
            active_from,
            level_size,
            total,
            amount_left,
            extension_commitment,
            table,
        };
        debug_assert_eq!(s.encode(), b);
        Ok(s)
    }

    /// Redeem script: the template's prefix and suffix around this state.
    pub fn redeem(&self) -> Vec<u8> {
        let t = template(TemplateId::KobCurveAsk);
        [t.prefix.as_slice(), &self.encode(), t.suffix.as_slice()].concat()
    }

    /// P2SH script public key.
    pub fn spk(&self) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem())
    }

    /// Decodes an instance from its redeem script.
    pub fn from_redeem(redeem: &[u8]) -> Result<Self, StateError> {
        let t = template(TemplateId::KobCurveAsk);
        if redeem.len() <= t.prefix.len() + t.suffix.len() || !redeem.starts_with(&t.prefix) || !redeem.ends_with(&t.suffix) {
            return Err(StateError::NotInstance(NAME));
        }
        Self::decode(&redeem[t.prefix.len()..redeem.len() - t.suffix.len()])
    }

    /// The placement gates (spec §5), for builders and indexers.
    ///
    /// `scale` a power of ten `<= 10^9`; `1 <= nLevels <= 1,000`; `levelSize > 0`;
    /// `0 < amountLeft <= total <= nLevels·levelSize`; `tip >= 0`; every price `>= tip` and `> 0`;
    /// `levelSize·max(table)/scale < 2^62`; `minFill >= 1`; `activeFrom >= 0`.
    pub fn check_numbers(&self) -> Result<(), String> {
        let e = |m: String| Err(format!("{NAME}: {m}"));
        check_scale(self.scale).map_err(|x| format!("{NAME}: {x}"))?;
        if self.table.is_empty() || self.table.len() > CURVE_MAX_LEVELS {
            return e(format!("a curve has 1 to {CURVE_MAX_LEVELS} levels, got {}", self.table.len()));
        }
        if self.level_size <= 0 {
            return e(format!("levelSize must be > 0, got {}", self.level_size));
        }
        let cap = (self.table.len() as i128) * (self.level_size as i128);
        if self.amount_left <= 0 || self.amount_left > self.total || (self.total as i128) > cap {
            return e(format!("needs 0 < amountLeft ({}) <= total ({}) <= nLevels·levelSize ({cap})", self.amount_left, self.total));
        }
        if self.tip < 0 {
            return e(format!("tip must be >= 0, got {}", self.tip));
        }
        // a level priced at the tip would sell for nothing (the maker gets price - tip)
        if let Some((j, p)) = self.table.iter().enumerate().find(|(_, p)| **p <= self.tip || **p <= 0) {
            return e(format!("level {j} is priced {p}: every price must be > tip ({}) and > 0", self.tip));
        }
        let max = *self.table.iter().max().expect("non-empty");
        match quote_exact(self.level_size, max, self.scale, Round::Up) {
            Some(v) if v < QUOTE_LIMIT => {}
            _ => return e(format!("levelSize·max(table)/scale must be below 2^62 (levelSize {}, max {max})", self.level_size)),
        }
        if self.min_fill < 1 {
            return e(format!("minFill must be >= 1, got {}", self.min_fill));
        }
        // one fill crosses at most MAX_CROSS levels: from any position it reaches at least 7 levels + 1 base unit, so a larger minFill
        // leaves a position with no acceptable fill
        let reach = (CURVE_MAX_CROSS as i128 - 1) * self.level_size as i128 + 1;
        if self.min_fill as i128 > reach {
            return e(format!("minFill ({}) must be at most {reach} (7 levels + 1: what one fill always reaches)", self.min_fill));
        }
        if self.active_from < 0 {
            return e(format!("activeFrom must be >= 0, got {}", self.active_from));
        }
        Ok(())
    }
}
