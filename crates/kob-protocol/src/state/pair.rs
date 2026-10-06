//! Pair orders (protocol v3): an order of a token/token pair A/B is an amount of the BASE token A at a price in the QUOTE
//! token B per WHOLE A (`price` base units of B per `scale(A)` base units of A), with the maker-favour rounding of every KOB
//! order (the ceil for what the maker receives, the floor for what it pays). Tips, keeper tips and carriers are KAS (tips:
//! sompi per whole A, rounded down).
//!
//! Three templates, each ONE template for both sides and both token families (`contracts/v2/KobPair.sil`,
//! `KobCondPair.sil`, `KobIfdPair.sil`; the headers are the reference):
//!
//! * [`PairState`] (`KobPair`, kind 0x08): plain ask / bid with IOC / FOK, Dutch / rising decay, TWAP / DCA, close. Side
//!   ASK (1) sells A (its custody S = A) for B (T = B); side BID (2) holds a B escrow (S = B) and buys A (T = A).
//! * [`CondPairState`] (`KobCondPair`, 0x09): stop, stop-limit, trailing, take-profit / limit leg, OCO, and the exits of
//!   `KobIfdPair` (repeat fields). Same S / T convention.
//! * [`IfdPairState`] (`KobIfdPair`, 0x0a): IFD / IFO / bracket / repeat entries, tokens named A and B. Side BID
//!   (buy-first) holds a B escrow; side ASK (sell-first) holds A (exactly `amountLeft`) and a B PREFUND custody.
//!
//! Option (2) of the founder rules: each pair order enforces only its own guarantees (what it receives and pays, its exact
//! custodies, the stray guards of both tokens, its positional outputs). Fillers may route through the KAS books, net any
//! number of opposite pair orders, or fill from inventory. Pair conditionals arm from trigger evidence in one of two modes:
//! two KAS-book fills of A and B in the same transaction (the implied rate, [`implied_le`] / [`implied_ge`]) or a fill of a
//! resting `KobPair` of the same pair ([`PairEvidence`]).
//!
//! Every helper is the covenant's own arithmetic (the split `quoteOf` of [`quote_of`], exact for scales up to 10^9): `None`
//! wherever the covenant would fail (an overflow, a refused input).

use crate::artifacts::TemplateId;
use crate::family::Family;
use crate::registry::KRON_MAX_OUTPUT_AMOUNT;

use super::{
    band, band_bps, ceil_div, check_quote, check_scale, cond_next_armed, decay_origin, entry_merged_armed, entry_next_armed,
    entry_price, family_of_code, min_fill_ok, quote_of, stop_band, Booking, OrderState, Round, StateCodec, StateError, MAX_STOP_PRICE,
    SIDE_ASK, SIDE_BID, TIF_FOK, TIF_GTC,
};

kob_state!(
    /// `KobPair`: plain order of a pair A/B. Side ASK sells A (custody S = A, exactly `amountLeft`) and receives at least
    /// `ceil(n * price(t) / scale(A))` of B at its positional output; side BID pays exactly `floor(n * price(t) / scale(A))`
    /// of B from its escrow custody (S = B) and receives exactly n of A. The order UTXO prefunds the delivery carrier of
    /// every fill and the KAS tip.
    PairState, TemplateId::KobPair, "KobPair", order: true, {
        /// T deliveries, S refunds and returns, cancel key (x-only).
        maker: [u8; 32] => "maker",
        /// 1 ASK (sells base A), 2 BID (buys base A).
        side: i64 => "side",
        /// The token sold and held in custody (A for an ask, B for a bid).
        s_cov_id: [u8; 32] => "sCovId",
        s_tpl_hash: [u8; 32] => "sTplHash",
        s_pre: i64 => "sPre",
        s_suf: i64 => "sSuf",
        /// Family code of S: 1 KCC-20, 2 KRON.
        s_family: i64 => "sFamily",
        /// Base units per whole S.
        s_scale: i64 => "sScale",
        /// The token bought (B for an ask, A for a bid).
        t_cov_id: [u8; 32] => "tCovId",
        t_tpl_hash: [u8; 32] => "tTplHash",
        t_pre: i64 => "tPre",
        t_suf: i64 => "tSuf",
        t_family: i64 => "tFamily",
        /// KCC-20 extension commitment of the T deliveries (zero for a KRON T).
        t_ext: [u8; 32] => "tExt",
        t_scale: i64 => "tScale",
        /// Smallest fill (base units of A) unless it takes all that is left.
        min_fill: i64 => "minFill",
        /// Limit: B base units per whole A (the start price when decaying).
        price: i64 => "price",
        /// Priority tip, sompi per whole A (prefunded on the order UTXO, released rounded down).
        tip: i64 => "tip",
        /// 0 GTC/GTD, 1 IOC, 2 FOK.
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// Sompi on each T delivery (prefunded on the order UTXO).
        delivery_carrier: i64 => "deliveryCarrier",
        /// TWAP / DCA: least DAA between fills (0 = off).
        interval: i64 => "interval",
        /// TWAP / DCA: most base units of A per fill (0 = off).
        max_fill: i64 => "maxFill",
        /// Decay (ASK down, BID up): B base units per whole A per `decayStep` (0 = off).
        slope: i64 => "slope",
        price_end: i64 => "priceEnd",
        decay_step: i64 => "decayStep",
        /// Mutable: base units of A still to trade.
        amount_left: i64 => "amountLeft",
        /// Mutable: the exact custody of S (an ask: `== amountLeft`).
        custody: i64 => "custody",
    }
);

kob_state!(
    /// `KobCondPair`: conditional order of a pair A/B (stop, stop-limit, trailing, take-profit / limit leg, OCO) and the
    /// exit of every `KobIfdPair` fill (repeat fields written by its entry). Side ASK sells A (S = A) with a stop BELOW
    /// the market; side BID holds a B escrow (S = B) and buys A with a stop ABOVE it.
    CondPairState, TemplateId::KobCondPair, "KobCondPair", order: true, {
        maker: [u8; 32] => "maker",
        side: i64 => "side",
        s_cov_id: [u8; 32] => "sCovId",
        s_tpl_hash: [u8; 32] => "sTplHash",
        s_pre: i64 => "sPre",
        s_suf: i64 => "sSuf",
        s_family: i64 => "sFamily",
        s_scale: i64 => "sScale",
        t_cov_id: [u8; 32] => "tCovId",
        t_tpl_hash: [u8; 32] => "tTplHash",
        t_pre: i64 => "tPre",
        t_suf: i64 => "tSuf",
        t_family: i64 => "tFamily",
        t_ext: [u8; 32] => "tExt",
        t_scale: i64 => "tScale",
        min_fill: i64 => "minFill",
        tip: i64 => "tip",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// Sompi on each maker token output (delivery or profit).
        delivery_carrier: i64 => "deliveryCarrier",
        /// 0 = no take-profit / limit leg; B base units per whole A.
        tp_price: i64 => "tpPrice",
        slip_bps: i64 => "slipBps",
        trail_step: i64 => "trailStep",
        trail_gap: i64 => "trailGap",
        trail_wait: i64 => "trailWait",
        /// Smallest A evidence fill (base units of A) that arms or trails.
        min_touch: i64 => "minTouch",
        min_rest_daa: i64 => "minRestDaa",
        band_daa: i64 => "bandDaa",
        keeper_tip: i64 => "keeperTip",
        /// Mutable (trailing): 0 = no stop leg.
        stop_price: i64 => "stopPrice",
        /// Mutable: 0 not armed, 1 armed (origin = this UTXO's DAA), >= 2 the auction origin.
        armed: i64 => "armed",
        /// Mutable: base units of A still to trade.
        amount_left: i64 => "amountLeft",
        /// Mutable: the exact custody of S.
        custody: i64 => "custody",
        /// Repeat IFD: covenant id of the entry this exit re-arms (0 = none).
        parent: [u8; 32] => "parent",
        /// Repeat IFD: ASK exit the entry's budget rate, BID exit its proceeds rate (B per whole A).
        rpt_price: i64 => "rptPrice",
        /// Repeat IFD: BID exit the entry's prefund rate (0 for an ASK exit).
        rpt_pre: i64 => "rptPre",
        rpt_until: i64 => "rptUntil",
    }
);

kob_state!(
    /// `KobIfdPair`: if-done entry of a pair A/B. Side BID (buy-first) holds a B escrow (`custody`) and buys A into a
    /// fresh `KobCondPair` ASK exit per fill; side ASK (sell-first) holds A (exactly `amountLeft`) and a B prefund
    /// (`custody`) and sells A into a fresh `KobCondPair` BID exit holding the proceeds plus the prefund of the fill.
    IfdPairState, TemplateId::KobIfdPair, "KobIfdPair", order: true, {
        maker: [u8; 32] => "maker",
        /// 2 BID (buy-first), 1 ASK (sell-first).
        side: i64 => "side",
        a_cov_id: [u8; 32] => "aCovId",
        a_tpl_hash: [u8; 32] => "aTplHash",
        a_pre: i64 => "aPre",
        a_suf: i64 => "aSuf",
        a_family: i64 => "aFamily",
        a_scale: i64 => "aScale",
        /// KCC-20 extension commitment of NEW A outputs (zero for KRON).
        a_ext: [u8; 32] => "aExt",
        b_cov_id: [u8; 32] => "bCovId",
        b_tpl_hash: [u8; 32] => "bTplHash",
        b_pre: i64 => "bPre",
        b_suf: i64 => "bSuf",
        b_family: i64 => "bFamily",
        b_scale: i64 => "bScale",
        b_ext: [u8; 32] => "bExt",
        /// B base units per whole A (the limit of a stop entry).
        price: i64 => "price",
        /// ASK: buy-back budget beyond the proceeds, B base units per whole A (>= 0).
        prefund: i64 => "prefund",
        tip: i64 => "tip",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// KAS on each exit custody.
        delivery_carrier: i64 => "deliveryCarrier",
        /// KAS on each exit UTXO (and on a custody a merge creates).
        exit_carrier: i64 => "exitCarrier",
        min_fill: i64 => "minFill",
        /// 0 = limit entry; else the stop trigger (B per whole A).
        entry_stop: i64 => "entryStop",
        band_daa: i64 => "bandDaa",
        min_touch: i64 => "minTouch",
        min_rest_daa: i64 => "minRestDaa",
        keeper_tip: i64 => "keeperTip",
        /// Mutable: 0, 1 or the auction origin.
        armed: i64 => "armed",
        /// Mutable: base units of A still to buy / sell (ASK: the A custody holds exactly this).
        amount_left: i64 => "amountLeft",
        /// Mutable: the exact B held (BID the escrow, ASK the prefund; 0 = no custody UTXO).
        custody: i64 => "custody",
        /// Mutable: 0 = off, else 1 + base units of re-arms left.
        rpt_amount: i64 => "rptAmount",
        /// The committed exit: the `KobCondPair` state up to `armed` (432 bytes).
        exit_state: Vec<u8> => "exitState",
    }
);

/// Length of the committed exit a `KobIfdPair` stores (`KobCondPair` state up to `armed`).
pub const IFD_PAIR_EXIT_COMMIT: usize = 432;

/// One token of a pair order as its state names it (covenant id, program template hash and lengths, family code, scale)
/// and, where the state carries one, the extension commitment of the outputs the order creates of that token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairToken {
    pub cov_id: [u8; 32],
    pub tpl_hash: [u8; 32],
    pub prefix_len: i64,
    pub suffix_len: i64,
    /// Family code (1 KCC-20, 2 KRON).
    pub family: i64,
    pub scale: i64,
    pub ext: Option<[u8; 32]>,
}

impl PairToken {
    /// The family, if the code is valid.
    pub fn family_of(&self) -> Option<Family> {
        family_of_code(self.family)
    }
}

/// The two tokens of a pair order: the base token A and the quote token B.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairTokens {
    pub a: PairToken,
    pub b: PairToken,
}

fn side_ok(side: i64) -> bool {
    side == SIDE_ASK || side == SIDE_BID
}

#[allow(clippy::too_many_arguments)]
fn token(cov_id: [u8; 32], tpl_hash: [u8; 32], pre: i64, suf: i64, family: i64, scale: i64, ext: Option<[u8; 32]>) -> PairToken {
    PairToken { cov_id, tpl_hash, prefix_len: pre, suffix_len: suf, family, scale, ext }
}

/// Validation shared by the pair states: a valid side, valid family codes, a KRON token carries no extension commitment,
/// two different tokens.
fn check_pair_tokens(name: &str, side: i64, t: &PairTokens) -> Result<(), StateError> {
    let bad = |m: String| Err(StateError::Codec("AnyState", format!("{name}: {m}")));
    if !side_ok(side) {
        return bad(format!("side must be 1 (ASK) or 2 (BID), got {side}"));
    }
    for (what, k) in [("A", &t.a), ("B", &t.b)] {
        match k.family_of() {
            None => return bad(format!("the family code of token {what} must be 1 (KCC-20) or 2 (KRON), got {}", k.family)),
            Some(Family::Kron) if k.ext.is_some_and(|e| e != [0; 32]) => {
                return bad(format!("a KRON token {what} has no extension commitment (must be zero)"))
            }
            _ => {}
        }
    }
    if t.a.cov_id == t.b.cov_id {
        return bad("a pair order needs two different tokens (A != B)".into());
    }
    Ok(())
}

/// A KRON token output holds at most `10^9` base units (both pinned KRON programs refuse more).
fn kron_cap(fam: Option<Family>, amount: i64, what: &str) -> Result<(), String> {
    if fam == Some(Family::Kron) && amount > KRON_MAX_OUTPUT_AMOUNT {
        return Err(format!("{what}: a KRON token UTXO holds at most {KRON_MAX_OUTPUT_AMOUNT} base units (got {amount})"));
    }
    Ok(())
}

// ---------------------------------------------------------------- trigger evidence (both conditionals)

/// Trigger evidence of a pair conditional (`pairEv` of `KobCondPair` / `stopTouch` of `KobIfdPair`), argument `evMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum PairEvidence {
    /// Mode 0: a resting KAS-book order of A quoting `a` (sompi per whole A) and one of B quoting `b` (sompi per whole B),
    /// both filled in the transaction. The implied rate is `a * scale(B) / b` B base units per whole A.
    #[serde(rename_all = "camelCase")]
    KasBooks {
        #[serde(with = "crate::json::field")]
        a: i64,
        #[serde(with = "crate::json::field")]
        b: i64,
    },
    /// Mode 1: a resting `KobPair` of the same pair quoting `price` (B base units per whole A), filled in the transaction.
    #[serde(rename_all = "camelCase")]
    Pair {
        #[serde(with = "crate::json::field")]
        price: i64,
    },
}

impl PairEvidence {
    /// The `evMode` argument.
    pub fn mode(&self) -> i64 {
        match self {
            PairEvidence::KasBooks { .. } => 0,
            PairEvidence::Pair { .. } => 1,
        }
    }
    /// The covenant's `(a, b)`: mode 0 the two quotes, mode 1 `(price, scale(B))` (every comparison `a <= floor(x * b /
    /// sB)` then reads `price <= x`).
    pub fn ab(&self, b_scale: i64) -> (i64, i64) {
        match *self {
            PairEvidence::KasBooks { a, b } => (a, b),
            PairEvidence::Pair { price } => (price, b_scale),
        }
    }
}

/// `a <= floor(x * b / sB)`: the implied rate is at most `x` (B per whole A). `None` where the covenant's `quoteOf` fails.
pub fn implied_le(a: i64, b: i64, x: i64, b_scale: i64) -> Option<bool> {
    if b <= 0 {
        return None;
    }
    Some(a <= quote_of(x, b, b_scale, Round::Down)?)
}

/// `a >= ceil(x * b / sB)`: the implied rate is at least `x`. `None` where the covenant's `quoteOf` fails.
pub fn implied_ge(a: i64, b: i64, x: i64, b_scale: i64) -> Option<bool> {
    if b <= 0 {
        return None;
    }
    Some(a >= quote_of(x, b, b_scale, Round::Up)?)
}

/// The least B evidence fill (base units of B) of a mode-0 trigger at the stop `stop`: `ceil(minTouch * stop / scale(A))`,
/// the B value of the A threshold at the stop.
pub fn min_touch_b(min_touch: i64, stop: i64, a_scale: i64) -> Option<i64> {
    quote_of(min_touch, stop, a_scale, Round::Up)
}

// ---------------------------------------------------------------- KobPair

/// The covenant amounts of one `KobPair` fill of n base units of A (at quote p).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairFill {
    /// S released from the custody (ASK: n; BID: exactly `floor(n * p / scale(A))`).
    pub s_out: i64,
    /// Least T delivered to the maker (ASK: `ceil(n * p / scale(A))`; BID: exactly n).
    pub t_out: i64,
    /// KAS tip released to the filler: `floor(n * tip / scale(A))`.
    pub tip_kas: i64,
    /// The order continues (GTC with something left).
    pub rest: bool,
    /// S left in the custody after the fill (its rest, or its return to the maker; 0: nothing at the custody's index).
    pub out_amount: i64,
}

impl PairState {
    pub fn is_ask(&self) -> bool {
        self.side == SIDE_ASK
    }
    /// The S token (held) and the T token (bought).
    pub fn s_token(&self) -> PairToken {
        token(self.s_cov_id, self.s_tpl_hash, self.s_pre, self.s_suf, self.s_family, self.s_scale, None)
    }
    pub fn t_token(&self) -> PairToken {
        token(self.t_cov_id, self.t_tpl_hash, self.t_pre, self.t_suf, self.t_family, self.t_scale, Some(self.t_ext))
    }
    /// A (base) and B (quote).
    pub fn tokens(&self) -> PairTokens {
        if self.is_ask() {
            PairTokens { a: self.s_token(), b: self.t_token() }
        } else {
            PairTokens { a: self.t_token(), b: self.s_token() }
        }
    }
    /// Base units per whole A (the price denominator).
    pub fn a_scale(&self) -> i64 {
        if self.is_ask() {
            self.s_scale
        } else {
            self.t_scale
        }
    }
    /// Decay origin of the UTXO created at `utxo_daa` (`activeFrom`, or the opening of the TWAP / DCA slice).
    pub fn origin(&self, utxo_daa: i64) -> Option<i64> {
        decay_origin(self.active_from, self.interval, utxo_daa)
    }
    /// The quote at auction time `t`: an ask decays down to `priceEnd`, a bid rises up to it; the constant price without
    /// decay. `None` where the covenant's arithmetic fails.
    pub fn price_at(&self, t: i64, utxo_daa: i64) -> Option<i64> {
        if self.slope == 0 {
            return Some(self.price);
        }
        if self.slope < 0 || self.decay_step <= 0 {
            return None;
        }
        let moved = self.slope.checked_mul(t.checked_sub(self.origin(utxo_daa)?)? / self.decay_step)?;
        Some(if self.is_ask() {
            self.price.checked_sub(moved)?.max(self.price_end)
        } else {
            self.price.checked_add(moved)?.min(self.price_end)
        })
    }
    /// The highest quote a bid can pay (`priceEnd` of a rising bid, else `price`); for an ask its highest quote.
    pub fn price_max(&self) -> i64 {
        if self.slope != 0 {
            if self.is_ask() {
                self.price.max(self.price_end)
            } else {
                self.price_end
            }
        } else {
            self.price
        }
    }
    /// S released for n base units of A at quote p (ASK n, BID `floor(n * p / scale(A))`).
    pub fn s_out(&self, n: i64, p: i64) -> Option<i64> {
        if self.is_ask() {
            Some(n)
        } else {
            quote_of(n, p, self.t_scale, Round::Down)
        }
    }
    /// The least T the maker receives for n base units of A at quote p (ASK `ceil(n * p / scale(A))`, BID n).
    pub fn t_out_min(&self, n: i64, p: i64) -> Option<i64> {
        if self.is_ask() {
            quote_of(n, p, self.s_scale, Round::Up)
        } else {
            Some(n)
        }
    }
    /// KAS tip a fill of n releases: `floor(n * tip / scale(A))`.
    pub fn tip_kas(&self, n: i64) -> Option<i64> {
        quote_of(n, self.tip, self.a_scale(), Round::Down)
    }
    /// The covenant's quantity rules: `0 < n <= amountLeft`, the minimum fill, the TWAP / DCA `maxFill`.
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill) && (self.max_fill == 0 || n <= self.max_fill)
    }
    /// The complete covenant rule of a fill of n at quote p of an order UTXO holding `value` sompi: the quantity rules, a
    /// positive quote, an ask's custody holding its whole amount, an escrow that pays the quote, a continuation that keeps
    /// something of S and is funded (`value >= deliveryCarrier + tip`), FOK completeness. The amounts it moves, or why it
    /// is refused.
    pub fn fill(&self, n: i64, p: i64, value: i64) -> Result<PairFill, String> {
        if !self.fill_ok(n) {
            return Err(format!(
                "a pair fill of {n} needs 0 < n <= amountLeft {}, n >= minFill {} unless it takes everything left, n <= maxFill {} (0 = off)",
                self.amount_left, self.min_fill, self.max_fill
            ));
        }
        if p <= 0 {
            return Err("the quote must be positive".into());
        }
        if self.tip < 0 || self.delivery_carrier < 0 {
            return Err("tip and deliveryCarrier must be >= 0".into());
        }
        if self.custody <= 0 {
            return Err("a pair order holds a positive custody".into());
        }
        if self.is_ask() && self.custody != self.amount_left {
            return Err("an ask's custody must be its whole remaining amount".into());
        }
        let overflow = || "the covenant arithmetic overflows here".to_string();
        let s_out = self.s_out(n, p).ok_or_else(overflow)?;
        let t_out = self.t_out_min(n, p).ok_or_else(overflow)?;
        let tip_kas = self.tip_kas(n).ok_or_else(overflow)?;
        let out_amount = self.custody - s_out;
        if out_amount < 0 {
            return Err(format!("the escrow of {} cannot pay {s_out}", self.custody));
        }
        let rest = self.amount_left - n > 0 && self.tif == TIF_GTC;
        if rest {
            if out_amount == 0 {
                return Err("a partial fill must leave something in the custody (the escrow is used up)".into());
            }
            if value < self.delivery_carrier.saturating_add(tip_kas) {
                return Err(format!(
                    "the order's {value} sompi do not fund the delivery carrier and the tip of a partial fill ({} + {tip_kas})",
                    self.delivery_carrier
                ));
            }
        } else if self.tif == TIF_FOK && n != self.amount_left {
            return Err("a FOK pair order is filled completely".into());
        }
        Ok(PairFill { s_out, t_out, tip_kas, rest, out_amount })
    }
    /// Whether anyone can take n base units now (the quote is takeable, [`PairState::fill`]).
    pub fn takeable(&self, n: i64, t: i64, utxo_daa: i64, value: i64) -> bool {
        self.price_at(t, utxo_daa).is_some_and(|p| self.fill(n, p, value).is_ok())
    }
    /// The largest n anyone can take now at quote p (0 when nothing is takeable): the whole amount if possible, else the
    /// largest partial fill the covenant accepts.
    pub fn max_takeable(&self, p: i64, value: i64) -> i64 {
        if self.fill(self.amount_left, p, value).is_ok() {
            return self.amount_left;
        }
        // partial fills: every rule is monotone in n (the tip, the escrow and maxFill grow with it)
        let (mut lo, mut hi) = (0i64, self.amount_left - 1);
        if self.max_fill > 0 {
            hi = hi.min(self.max_fill);
        }
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
            let ok = self.fill(mid, p, value).is_ok() || mid < self.min_fill;
            if ok {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if lo > 0 && self.fill(lo, p, value).is_ok() {
            lo
        } else {
            0
        }
    }
    /// A bid's B escrow for `amount` base units of A: `floor(amount * pMax / scale(A)) + 1`. Each fill pays exactly
    /// `floor(n * p / scale(A))` with `p <= pMax`, and floors are subadditive, so the floor of the whole amount pays every
    /// split; the one unit keeps the rest of every partial fill positive (the covenant's rest needs a custody). `_fills` is
    /// not used (no slack per fill: none is consumed); it stays for the callers.
    pub fn bid_escrow(&self, amount: i64, _fills: i64) -> Option<i64> {
        quote_of(amount, self.price_max(), self.t_scale, Round::Down)?.checked_add(1)
    }
    /// KAS an order UTXO needs for `fills` fills: one delivery carrier each and the tip of the whole amount.
    pub fn kas_value(&self, fills: i64) -> Option<i64> {
        fills.max(1).checked_mul(self.delivery_carrier)?.checked_add(self.tip_kas(self.amount_left)?)
    }
    /// The deliveries a new order must fund itself: one when it never rests after a fill (IOC / FOK, or `minFill` takes
    /// everything), else a partial fill and the fill of its rest (a continuation pays each of its deliveries and needs a
    /// positive value), a TWAP / DCA order at least one per `maxFill` slice.
    pub fn funded_fills(&self) -> i64 {
        if self.tif != TIF_GTC || self.min_fill >= self.amount_left {
            1
        } else if self.max_fill > 0 {
            ceil_div(self.amount_left, self.max_fill).max(2)
        } else {
            2
        }
    }
    /// Most fills the order can take (`ceil(amountLeft / minFill)`).
    pub fn max_fills(&self) -> i64 {
        ceil_div(self.amount_left, self.min_fill.max(1))
    }
    pub(crate) fn validate(&self) -> Result<(), StateError> {
        check_pair_tokens("KobPair", self.side, &self.tokens())
    }
    pub(crate) fn check_numbers(&self) -> Result<(), String> {
        let n = "KobPair";
        check_scale(self.s_scale).map_err(|e| format!("{n}: S {e}"))?;
        check_scale(self.t_scale).map_err(|e| format!("{n}: T {e}"))?;
        let sa = self.a_scale();
        check_quote(self.amount_left, self.price.max(self.price_end).max(0), sa, &format!("{n} price"))?;
        check_quote(self.amount_left, self.tip.max(0), sa, &format!("{n} tip"))?;
        let t = self.tokens();
        kron_cap(t.a.family_of(), self.amount_left, &format!("{n} amountLeft"))?;
        kron_cap(family_of_code(self.s_family), self.custody, &format!("{n} custody"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------- KobCondPair

impl CondPairState {
    pub fn is_ask(&self) -> bool {
        self.side == SIDE_ASK
    }
    pub fn s_token(&self) -> PairToken {
        token(self.s_cov_id, self.s_tpl_hash, self.s_pre, self.s_suf, self.s_family, self.s_scale, None)
    }
    pub fn t_token(&self) -> PairToken {
        token(self.t_cov_id, self.t_tpl_hash, self.t_pre, self.t_suf, self.t_family, self.t_scale, Some(self.t_ext))
    }
    pub fn tokens(&self) -> PairTokens {
        if self.is_ask() {
            PairTokens { a: self.s_token(), b: self.t_token() }
        } else {
            PairTokens { a: self.t_token(), b: self.s_token() }
        }
    }
    pub fn a_scale(&self) -> i64 {
        if self.is_ask() {
            self.s_scale
        } else {
            self.t_scale
        }
    }
    pub fn b_scale(&self) -> i64 {
        if self.is_ask() {
            self.t_scale
        } else {
            self.s_scale
        }
    }
    /// True for an exit booked by a repeating entry.
    pub fn is_booked(&self) -> bool {
        self.parent != [0; 32]
    }
    /// Stop-leg price for a fill at auction time `t` (`trigger`: armed by the evidence of this very transaction, which
    /// trades at the stop itself when `bandDaa > 0`). ASK `stop - band`, BID `stop + band` (band rounded down).
    pub fn stop_at(&self, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        let bps = match (trigger || self.armed == 0, self.armed) {
            (true, _) => {
                if self.band_daa > 0 {
                    0
                } else {
                    self.slip_bps
                }
            }
            (false, a) => {
                let origin = if a == 1 { utxo_daa } else { a };
                band_bps(self.slip_bps, self.band_daa, origin, t)?
            }
        };
        let b = stop_band(self.stop_price, bps)?;
        if self.is_ask() {
            Some(self.stop_price - b)
        } else {
            self.stop_price.checked_add(b)
        }
    }
    /// Leg price (0 = take-profit / limit, 1 = stop) at time `t` (`None` where the covenant's stop arithmetic fails).
    pub fn leg_price(&self, leg: i64, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        if leg == 0 {
            Some(self.tp_price)
        } else {
            self.stop_at(trigger, t, utxo_daa)
        }
    }
    /// The band's worst stop price (ASK the floor, BID the ceiling).
    pub fn stop_worst(&self) -> i64 {
        let b = band(self.stop_price, self.slip_bps);
        if self.is_ask() {
            self.stop_price - b
        } else {
            self.stop_price.saturating_add(b)
        }
    }
    /// BID: the worst price either leg can pay.
    pub fn worst(&self) -> i64 {
        self.tp_price.max(if self.stop_price > 0 { self.stop_worst() } else { 0 })
    }
    /// ASK: the lowest price either leg can receive (0 if no leg).
    pub fn lowest(&self) -> i64 {
        let s = if self.stop_price > 0 { Some(self.stop_worst()) } else { None };
        let t = if self.tp_price > 0 { Some(self.tp_price) } else { None };
        match (s, t) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => 0,
        }
    }
    /// S released for n at leg price `lp` (ASK n; BID at most `floor(n * lp / scale(A))`, the most the maker pays).
    pub fn s_out(&self, n: i64, lp: i64) -> Option<i64> {
        if self.is_ask() {
            Some(n)
        } else {
            quote_of(n, lp, self.a_scale(), Round::Down)
        }
    }
    /// The least T the maker receives for n at leg price `lp` (ASK `ceil(n * lp / scale(A))`; BID n).
    pub fn t_out_min(&self, n: i64, lp: i64) -> Option<i64> {
        if self.is_ask() {
            quote_of(n, lp, self.a_scale(), Round::Up)
        } else {
            Some(n)
        }
    }
    /// KAS tip a fill of n releases: `floor(n * tip / scale(A))`.
    pub fn tip_kas(&self, n: i64) -> Option<i64> {
        quote_of(n, self.tip, self.a_scale(), Round::Down)
    }
    /// Repeat IFD: `ceil(n * rptPrice / scale(A))` (ASK exit: the entry's budget; BID exit: the entry's proceeds).
    pub fn rpt_proceeds(&self, n: i64) -> Option<i64> {
        quote_of(n, self.rpt_price, self.a_scale(), Round::Up)
    }
    /// Repeat IFD (BID exit): the prefund returned to the entry, `ceil(n * rptPre / scale(A))`.
    pub fn rpt_back(&self, n: i64) -> Option<i64> {
        quote_of(n, self.rpt_pre, self.a_scale(), Round::Up)
    }
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill)
    }
    /// `armed` of the continuation after a fill.
    pub fn next_armed(&self, leg: i64, trigger: bool, utxo_daa: i64) -> i64 {
        cond_next_armed(self.armed, self.band_daa, leg, trigger, utxo_daa)
    }
    pub fn max_fills(&self) -> i64 {
        ceil_div(self.amount_left, self.min_fill.max(1))
    }
    /// BID: the B escrow for all of `amountLeft` at the worst leg plus one base unit: each fill pays at most its floor, so
    /// the floor of the whole amount covers any split, and the one unit keeps the rest of every partial fill positive.
    /// `_fills` is not used (no slack per fill: none is consumed); it stays for the callers.
    pub fn bid_escrow(&self, _fills: i64) -> Option<i64> {
        quote_of(self.amount_left, self.worst(), self.a_scale(), Round::Down)?.checked_add(1)
    }
    /// KAS an order UTXO needs for `fills` fills: one delivery carrier each and the tip of the whole amount.
    pub fn kas_value(&self, fills: i64) -> Option<i64> {
        fills.max(1).checked_mul(self.delivery_carrier)?.checked_add(self.tip_kas(self.amount_left)?)
    }
    /// The deliveries a new order (or an exit) must fund itself: one when `minFill` takes everything, else a partial fill
    /// and the fill of its rest (a continuation pays each of its deliveries and needs a positive value).
    pub fn funded_fills(&self) -> i64 {
        if self.min_fill >= self.amount_left {
            1
        } else {
            2
        }
    }
    /// Whether the evidence arms the stop leg: ASK (sell stop) the rate fell to at most the stop (an ask of A and a bid of
    /// B, or a pair ASK), BID (buy stop) the rate rose to at least the stop (a bid of A and an ask of B, or a pair BID).
    /// `None` where the covenant's arithmetic fails.
    pub fn arms(&self, ev: PairEvidence) -> Option<bool> {
        let (a, b) = ev.ab(self.b_scale());
        if self.is_ask() {
            implied_le(a, b, self.stop_price, self.b_scale())
        } else {
            implied_ge(a, b, self.stop_price, self.b_scale())
        }
    }
    /// The trailing ratchet the evidence justifies: the k the covenant accepts (valid and maximal), or `None` (no step is
    /// justified, or the covenant's arithmetic fails). ASK trails UP on a high rate (a bid of A and an ask of B, or a pair
    /// BID): `s' = stop + k * step` with `s' + gap <= rate`, below `tpPrice`; BID trails DOWN on a low rate (an ask of A
    /// and a bid of B, or a pair ASK): `s' = stop - k * step` with `s' - gap >= rate`, above `max(tpPrice, 0)`.
    pub fn trail_k(&self, ev: PairEvidence) -> Option<i64> {
        let (step, gap, s) = (self.trail_step, self.trail_gap, self.stop_price);
        if step <= 0 || gap < 0 || s <= 0 {
            return None;
        }
        let sb = self.b_scale();
        let (a, b) = ev.ab(sb);
        if b <= 0 || a < 0 {
            return None;
        }
        let k = if self.is_ask() {
            // a >= ceil(x * b / sB)  <=>  x * b <= a * sB  <=>  x <= floor(a * sB / b), x = s + k * step + gap
            let x_max = (a as i128 * sb as i128) / b as i128;
            let mut k = (x_max - gap as i128 - s as i128).div_euclid(step as i128);
            if self.tp_price > 0 {
                k = k.min((self.tp_price as i128 - 1 - s as i128).div_euclid(step as i128));
            }
            k
        } else {
            // a <= floor(x * b / sB)  <=>  x * b >= a * sB  <=>  x >= ceil(a * sB / b), x = s - k * step - gap >= 0
            let x_min = (a as i128 * sb as i128 + b as i128 - 1) / b as i128;
            let floor_p = self.tp_price.max(0) as i128;
            let lower = (floor_p + 1).max(x_min + gap as i128);
            (s as i128 - lower).div_euclid(step as i128)
        };
        let k = i64::try_from(k).ok().filter(|k| *k >= 1)?;
        self.trail_check(ev, k).then_some(k)
    }
    /// The covenant's trailing rule for a given k: VALID and MAXIMAL (mirrors `KobCondPair.settle`, `update` with k >= 1).
    pub fn trail_check(&self, ev: PairEvidence, k: i64) -> bool {
        let (step, gap) = (self.trail_step, self.trail_gap);
        if k <= 0 || step <= 0 || gap < 0 || self.stop_price <= 0 {
            return false;
        }
        let sb = self.b_scale();
        let (a, b) = ev.ab(sb);
        if b <= 0 {
            return false;
        }
        let Some(ks) = k.checked_mul(step) else { return false };
        if self.is_ask() {
            let Some(ns) = self.stop_price.checked_add(ks) else { return false };
            let mut cap = false;
            if self.tp_price > 0 {
                if ns >= self.tp_price {
                    return false;
                }
                cap = ns.checked_add(step).is_none_or(|v| v >= self.tp_price);
            }
            let Some(x) = ns.checked_add(gap) else { return false };
            if implied_ge(a, b, x, sb) != Some(true) {
                return false;
            }
            cap || x.checked_add(step).and_then(|y| quote_of(y, b, sb, Round::Up)).is_some_and(|q2| a < q2)
        } else {
            let Some(ns) = self.stop_price.checked_sub(ks) else { return false };
            let floor_p = self.tp_price.max(0);
            if ns <= floor_p {
                return false;
            }
            let x = ns - gap;
            if x < 0 || implied_le(a, b, x, sb) != Some(true) {
                return false;
            }
            let y = x - step;
            let cap = ns - step <= floor_p || y < 0;
            cap || quote_of(y, b, sb, Round::Down).is_some_and(|q2| a > q2)
        }
    }
    /// The least B evidence fill of a mode-0 trigger at the current stop (`ceil(minTouch * stop / scale(A))`).
    pub fn min_touch_b(&self) -> Option<i64> {
        min_touch_b(self.min_touch, self.stop_price, self.a_scale())
    }
    pub(crate) fn validate(&self) -> Result<(), StateError> {
        check_pair_tokens("KobCondPair", self.side, &self.tokens())
    }
    pub(crate) fn check_numbers(&self) -> Result<(), String> {
        let n = "KobCondPair";
        check_scale(self.s_scale).map_err(|e| format!("{n}: S {e}"))?;
        check_scale(self.t_scale).map_err(|e| format!("{n}: T {e}"))?;
        let sa = self.a_scale();
        let legs = self.tp_price.max(self.stop_price).max(self.worst()).max(0);
        check_quote(self.amount_left, legs, sa, &format!("{n} leg price"))?;
        check_quote(self.amount_left, self.tip.max(0), sa, &format!("{n} tip"))?;
        check_quote(self.amount_left, self.rpt_price.max(0), sa, &format!("{n} rptPrice"))?;
        check_quote(self.amount_left, self.rpt_pre.max(0), sa, &format!("{n} rptPre"))?;
        let t = self.tokens();
        kron_cap(t.a.family_of(), self.amount_left, &format!("{n} amountLeft"))?;
        kron_cap(family_of_code(self.s_family), self.custody, &format!("{n} custody"))?;
        Ok(())
    }
    pub(crate) fn stop_ok(&self) -> bool {
        self.stop_price > 0 && self.stop_price <= MAX_STOP_PRICE && (0..=10_000).contains(&self.slip_bps)
    }
}

// ---------------------------------------------------------------- KobIfdPair

/// The repeat fields an entry writes behind the committed exit (`0x08 amountLeft 0x08 custody 0x20 parent 0x08 rptPrice
/// 0x08 rptPre 0x08 rptUntil`), exactly as `KobIfdPair.fill` splices them.
fn exit_tail(amount: i64, custody: i64, parent: [u8; 32], rpt_price: i64, rpt_pre: i64, until: i64) -> Vec<u8> {
    let mut v = vec![0x08];
    v.extend_from_slice(&amount.to_le_bytes());
    v.push(0x08);
    v.extend_from_slice(&custody.to_le_bytes());
    v.push(0x20);
    v.extend_from_slice(&parent);
    for x in [rpt_price, rpt_pre, until] {
        v.push(0x08);
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

impl IfdPairState {
    /// Buy-first (side BID: buys A with its B escrow; the exit sells it).
    pub fn is_buy_first(&self) -> bool {
        self.side == SIDE_BID
    }
    pub fn tokens(&self) -> PairTokens {
        PairTokens {
            a: token(self.a_cov_id, self.a_tpl_hash, self.a_pre, self.a_suf, self.a_family, self.a_scale, Some(self.a_ext)),
            b: token(self.b_cov_id, self.b_tpl_hash, self.b_pre, self.b_suf, self.b_family, self.b_scale, Some(self.b_ext)),
        }
    }
    /// The exit prefix an entry commits to (the exit's state up to `armed`; it must not be booked).
    pub fn commit_exit(exit: &CondPairState) -> Vec<u8> {
        exit.encode()[..IFD_PAIR_EXIT_COMMIT].to_vec()
    }
    /// The committed exit order (amountLeft, custody and the repeat fields zero).
    pub fn exit(&self) -> Result<CondPairState, StateError> {
        if self.exit_state.len() != IFD_PAIR_EXIT_COMMIT {
            return Err(StateError::Codec("KobIfdPair", format!("exitState must be {IFD_PAIR_EXIT_COMMIT} bytes")));
        }
        CondPairState::decode(&[self.exit_state.clone(), exit_tail(0, 0, [0; 32], 0, 0, 0)].concat())
    }
    /// The KAS an exit needs (its UTXO value is `exitCarrier`), from the exit's own rules: a keeper arming its stop leg
    /// takes up to its `keeperTip` (an update keeps `value - keeperTip`); then it funds its deliveries itself (a partial
    /// fill and the fill of its rest when its `minFill` is below the entry's amount, else one) and the tip of the
    /// largest amount it can hold (the entry's whole amount), or its refund (`refundTip`). Trailing updates draw on the
    /// same value (each up to `keeperTip`): a maker who lets the stop trail funds more.
    pub fn exit_carrier_needed(&self) -> Option<i64> {
        let e = CondPairState { amount_left: self.amount_left, ..self.exit().ok()? };
        let fills = e.kas_value(e.funded_fills())?;
        let reserve = if e.stop_price > 0 { e.keeper_tip.max(0) } else { 0 };
        reserve.checked_add(fills.max(e.refund_tip.max(0)))
    }
    /// The exit a fill of n creates holding `custody` (BID: n of A; ASK: the proceeds plus the prefund of n, in B), booked
    /// when the entry repeats and `rptAmount > n` (`rptPrice = price`, `rptPre = prefund` for a sell-first entry).
    pub fn exit_for(&self, n: i64, custody: i64, booking: Option<Booking>) -> Result<CondPairState, StateError> {
        let mut x = CondPairState { amount_left: n, custody, ..self.exit()? };
        if let Some(b) = booking {
            x.parent = b.parent;
            x.rpt_price = self.price;
            x.rpt_pre = if self.is_buy_first() { 0 } else { self.prefund };
            x.rpt_until = b.until;
        }
        Ok(x)
    }
    /// Whether `x` is exactly an exit this entry books with parent `me`: what the entry's merge compares (every byte of the
    /// committed exit outside the mutable stopPrice / armed, parent, rptPrice, rptPre; amountLeft, custody and rptUntil
    /// are not compared).
    pub fn books_exit(&self, me: [u8; 32], x: &CondPairState) -> bool {
        let booking = Booking { parent: me, until: x.rpt_until };
        self.exit_for(x.amount_left, x.custody, Some(booking))
            .map(|e| CondPairState { stop_price: x.stop_price, armed: x.armed, ..e } == *x)
            .unwrap_or(false)
    }
    /// Entry quote at time `t` (a stop entry's auction from `entryStop` to `price`; the limit otherwise).
    pub fn price_at(&self, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        entry_price(self.price, self.entry_stop, self.band_daa, self.armed, trigger, t, utxo_daa)
    }
    pub fn next_armed(&self, utxo_daa: i64) -> i64 {
        entry_next_armed(self.entry_stop, self.band_daa, self.armed, utxo_daa)
    }
    pub fn merged_armed(&self, utxo_daa: i64) -> i64 {
        entry_merged_armed(self.amount_left, self.band_daa, self.armed, utxo_daa)
    }
    /// BID: the most B a fill of n releases at quote p, `floor(n * p / scale(A))`.
    pub fn spend(&self, n: i64, p: i64) -> Option<i64> {
        quote_of(n, p, self.a_scale, Round::Down)
    }
    /// ASK: the least proceeds of n at quote p, `ceil(n * p / scale(A))`.
    pub fn proceeds(&self, n: i64, p: i64) -> Option<i64> {
        quote_of(n, p, self.a_scale, Round::Up)
    }
    /// ASK: the prefund of n, `ceil(n * prefund / scale(A))` (moved to the exit on a fill, returned on a merge).
    pub fn pre_of(&self, n: i64) -> Option<i64> {
        quote_of(n, self.prefund, self.a_scale, Round::Up)
    }
    /// BID: the budget a merge of m returns to the escrow, `ceil(m * price / scale(A))`.
    pub fn merge_budget(&self, m: i64) -> Option<i64> {
        quote_of(m, self.price, self.a_scale, Round::Up)
    }
    /// KAS tip a fill of n releases: `floor(n * tip / scale(A))`.
    pub fn tip_kas(&self, n: i64) -> Option<i64> {
        quote_of(n, self.tip, self.a_scale, Round::Down)
    }
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill)
    }
    pub fn max_fills(&self) -> i64 {
        ceil_div(self.amount_left, self.min_fill.max(1))
    }
    /// The B custody a new entry needs: BID the spend of the whole amount at the limit (each fill releases at most its
    /// floor, so the floor of the whole amount covers any split); ASK the prefund of the whole amount plus one base unit
    /// per possible fill but the last (each fill takes its ceil).
    pub fn b_custody_needed(&self) -> Option<i64> {
        if self.is_buy_first() {
            self.spend(self.amount_left, self.price)
        } else {
            self.pre_of(self.amount_left)?.checked_add((self.max_fills() - 1).max(0))
        }
    }
    /// KAS an entry UTXO needs: per possible fill one delivery carrier (the exit's custody) and one exit carrier, the tip
    /// of the whole amount, and a repeating entry one more exit carrier (a merge's new custody).
    pub fn kas_value(&self) -> Option<i64> {
        self.max_fills()
            .checked_mul(self.delivery_carrier.checked_add(self.exit_carrier)?)?
            .checked_add(self.tip_kas(self.amount_left)?)?
            .checked_add(if self.rpt_amount > 0 { self.exit_carrier } else { 0 })
    }
    /// Whether the evidence arms the stop entry: BID (buy stop) the rate rose to at least `entryStop`, ASK (sell stop) it
    /// fell to at most `entryStop`.
    pub fn arms(&self, ev: PairEvidence) -> Option<bool> {
        let (a, b) = ev.ab(self.b_scale);
        if self.is_buy_first() {
            implied_ge(a, b, self.entry_stop, self.b_scale)
        } else {
            implied_le(a, b, self.entry_stop, self.b_scale)
        }
    }
    /// The least B evidence fill of a mode-0 trigger at the entry stop.
    pub fn min_touch_b(&self) -> Option<i64> {
        min_touch_b(self.min_touch, self.entry_stop, self.a_scale)
    }
    pub(crate) fn validate(&self) -> Result<(), StateError> {
        check_pair_tokens("KobIfdPair", self.side, &self.tokens())?;
        if self.exit_state.len() != IFD_PAIR_EXIT_COMMIT {
            return Err(StateError::Codec(
                "AnyState",
                format!("KobIfdPair: exitState must be {IFD_PAIR_EXIT_COMMIT} bytes, got {}", self.exit_state.len()),
            ));
        }
        Ok(())
    }
    pub(crate) fn check_numbers(&self) -> Result<(), String> {
        let n = "KobIfdPair";
        check_scale(self.a_scale).map_err(|e| format!("{n}: A {e}"))?;
        check_scale(self.b_scale).map_err(|e| format!("{n}: B {e}"))?;
        let sa = self.a_scale;
        check_quote(self.amount_left, self.price.max(self.entry_stop).max(0), sa, &format!("{n} price"))?;
        check_quote(self.amount_left, self.prefund.max(0), sa, &format!("{n} prefund"))?;
        check_quote(self.amount_left, self.tip.max(0), sa, &format!("{n} tip"))?;
        let rates = self.price.max(0).checked_add(self.prefund.max(0)).ok_or_else(|| format!("{n}: price + prefund overflows"))?;
        check_quote(self.amount_left, rates, sa, &format!("{n} price + prefund"))?;
        if let Ok(x) = self.exit() {
            let legs = x.tp_price.max(x.stop_price).max(x.worst()).max(0);
            check_quote(self.amount_left, legs, sa, &format!("{n} exit leg price"))?;
        }
        let t = self.tokens();
        kron_cap(t.a.family_of(), self.amount_left, &format!("{n} amountLeft"))?;
        kron_cap(t.b.family_of(), self.custody, &format!("{n} custody"))?;
        Ok(())
    }
}

/// `true` when a fill of `n` of `s` would be its last one (sells out, or ends an IOC / FOK).
pub fn pair_terminates(s: &PairState, n: i64) -> bool {
    s.amount_left - n <= 0 || s.tif != TIF_GTC
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::template;

    fn compiled<S: StateCodec>(id: TemplateId) -> S {
        let t = template(id);
        S::decode(&t.contract().compiled.bytecode[1..1 + t.state_len]).unwrap()
    }

    /// The hand-coded offsets the covenants read match the typed encoding (`KobCondPair.rd` reads a KobPair's side, S, T
    /// and their scales; the entry's merge compares the committed exit up to stopPrice).
    #[test]
    fn covenant_offsets_match_the_encoding() {
        let p: PairState = compiled(TemplateId::KobPair);
        let st = p.encode();
        assert_eq!(st.len(), 414);
        assert_eq!(i64::from_le_bytes(st[34..42].try_into().unwrap()), p.side);
        assert_eq!(&st[43..75], &p.s_cov_id);
        assert_eq!(i64::from_le_bytes(st[136..144].try_into().unwrap()), p.s_scale);
        assert_eq!(&st[145..177], &p.t_cov_id);
        assert_eq!(i64::from_le_bytes(st[271..279].try_into().unwrap()), p.t_scale);
        assert_eq!(i64::from_le_bytes(st[289..297].try_into().unwrap()), p.price);
        assert_eq!(i64::from_le_bytes(st[316..324].try_into().unwrap()), p.active_from);
        assert_eq!(i64::from_le_bytes(st[352..360].try_into().unwrap()), p.interval);
        assert_eq!(i64::from_le_bytes(st[370..378].try_into().unwrap()), p.slope);
        let c: CondPairState = compiled(TemplateId::KobCondPair);
        let cs = c.encode();
        assert_eq!(cs.len(), 510);
        assert_eq!(i64::from_le_bytes(cs[289..297].try_into().unwrap()), c.tip);
        assert_eq!(i64::from_le_bytes(cs[325..333].try_into().unwrap()), c.delivery_carrier);
        assert_eq!(i64::from_le_bytes(cs[415..423].try_into().unwrap()), c.stop_price);
        assert_eq!(i64::from_le_bytes(cs[433..441].try_into().unwrap()), c.amount_left);
        assert_eq!(i64::from_le_bytes(cs[442..450].try_into().unwrap()), c.custody);
        assert_eq!(&cs[451..483], &c.parent);
        let i: IfdPairState = compiled(TemplateId::KobIfdPair);
        assert_eq!(i.exit_state.len(), IFD_PAIR_EXIT_COMMIT);
        let i = IfdPairState { exit_state: IfdPairState::commit_exit(&c), ..i };
        let x = i.exit_for(7, 9, Some(Booking { parent: [5; 32], until: 11 })).unwrap();
        assert_eq!((x.amount_left, x.custody, x.parent, x.rpt_price, x.rpt_until), (7, 9, [5; 32], i.price, 11));
        assert!(i.books_exit([5; 32], &CondPairState { stop_price: 3, armed: 1, ..x.clone() }));
        assert!(!i.books_exit([6; 32], &x));
    }

    #[test]
    fn trailing_k_is_valid_and_maximal() {
        let c: CondPairState = compiled(TemplateId::KobCondPair);
        // ASK: stop 1000, step 10, gap 5, B scale 1000 (pair mode reads the rate directly)
        let ask = CondPairState {
            side: SIDE_ASK,
            stop_price: 1_000,
            trail_step: 10,
            trail_gap: 5,
            tp_price: 0,
            t_scale: 1_000,
            ..c.clone()
        };
        assert_eq!(ask.trail_k(PairEvidence::Pair { price: 1_036 }), Some(3));
        assert!(ask.trail_check(PairEvidence::Pair { price: 1_036 }, 3));
        assert!(!ask.trail_check(PairEvidence::Pair { price: 1_036 }, 2), "not maximal");
        assert_eq!(ask.trail_k(PairEvidence::Pair { price: 1_014 }), None);
        let capped = CondPairState { tp_price: 1_025, ..ask.clone() };
        assert_eq!(capped.trail_k(PairEvidence::Pair { price: 2_000 }), Some(2));
        // BID: stop 1000 trails down
        let bid = CondPairState { side: SIDE_BID, stop_price: 1_000, trail_step: 10, trail_gap: 5, tp_price: 0, s_scale: 1_000, ..c };
        assert_eq!(bid.trail_k(PairEvidence::Pair { price: 964 }), Some(3));
        assert!(!bid.trail_check(PairEvidence::Pair { price: 964 }, 2));
        // mode 0: a = 2 * b scaled: rate = a * sB / b
        let k = bid.trail_k(PairEvidence::KasBooks { a: 964_000, b: 1_000_000 }).unwrap();
        assert_eq!(k, 3);
    }
}
