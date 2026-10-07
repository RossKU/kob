//! Numeric sanity of decoded order states.
//!
//! `AnyState::decode` is total over `i64`: an order UTXO can carry any numbers, because the covenant
//! only polices them when the order is *spent*. The economics helpers of `kob_protocol::state` mirror the script
//! (checked arithmetic, `None` where the covenant fails), but the matcher, the keepers and the indexer also form sums and
//! products of their own, so a hostile value (`decayStep = 0` with a slope, `scale = 0`, `amountLeft = 9e18`, a stop leg
//! quoting one sompi, a full fill worth more than an `i64`) would make them stall or mislead. [`check`] is the gate every
//! order state passes before it is listed, planned or kept: the protocol's numeric gate
//! (`kob_protocol::state::AnyState::check_numbers`: `scale` a power of ten in `1..=10^9`, every full fill worth less than
//! `2^62` quote units), the covenant's own acceptance rules (`minFill >= 0`, a bid's `minFill > 0`, `tip >= 0`,
//! `slope >= 0`, `decayStep > 0` when decaying, ...) and bounds on every field so that no sum or product the matcher forms
//! can overflow. The bounds are far beyond any real order (the whole KAS supply is 2^61.3 sompi), so an honest order never
//! trips one; a state that does is left unlisted (`bad_state:<field>`), still indexed and cancellable by its maker. Last, the
//! token program(s) the state pins must resolve as the builders resolve them (`kob_protocol::build::order_programs`: template
//! hash, prefix / suffix lengths, family), so that no order the planner accepts is one the builder refuses.

use kob_protocol::state::*;

/// Most sompi in any single money field (about 1.15e18, a third of the whole supply).
pub const MONEY_MAX: i128 = 1 << 60;
/// Most sompi an order can ever move over all its fills (its full fill at its rates, its carriers).
pub const NOTIONAL_MAX: i128 = 1 << 61;
/// Most sompi per whole token (quotes, take-profits, trailing steps, tips).
pub const PRICE_MAX: i64 = 1 << 60;
/// Latest DAA-like field value (activation, interval, waits): about 3.6e7 years at 10 DAA/s.
pub const DAA_MAX: i64 = 1 << 50;
/// Longest stop-band auction, DAA.
pub const BAND_MAX: i64 = 1 << 22;
/// Longest auction window the decay/rise arithmetic must survive (about 545 years at 10 DAA/s): the bound of an order whose
/// own window (`expiryDaa - activeFrom`, see [`window`]) is longer.
pub const T_MAX: i128 = 1 << 34;
/// Most base units of a token amount any output may hold (an `i64`).
const AMOUNT_MAX: i64 = i64::MAX;

struct Ck(Result<(), String>);

impl Ck {
    fn fail(&mut self, name: &str, why: &str) {
        if self.0.is_ok() {
            self.0 = Err(format!("bad_state:{name}:{why}"));
        }
    }
    /// `lo <= v <= hi`.
    fn range(&mut self, name: &str, v: i64, lo: i64, hi: i64) {
        if v == i64::MIN {
            // no canonical 8-byte state push carries it: no builder could encode the state
            self.fail("encoding", "unencodable");
            return;
        }
        if v < lo || v > hi {
            self.fail(name, if v < lo { "below_minimum" } else { "above_maximum" });
        }
    }
    fn le(&mut self, name: &str, v: i128, hi: i128) {
        if v > hi {
            self.fail(name, "overflow");
        }
    }
    /// The full value of `amount` base units at `rate` per `scale` (rounded up) is at most `hi` (an invalid input fails).
    fn quote_le(&mut self, name: &str, amount: i64, rate: i128, scale: i64, hi: i128) {
        let ok = match i64::try_from(rate) {
            Ok(r) => quote_exact(amount, r, scale, Round::Up).is_some_and(|v| v <= hi),
            Err(_) => false,
        };
        if !ok {
            self.fail(name, "overflow");
        }
    }
}

/// Fields every order kind has.
struct Common {
    scale: i64,
    min_fill: i64,
    tip: i64,
    active_from: i64,
    refund_tip: i64,
    tpl_lens: (i64, i64),
    expiry_daa: i64,
}

fn common(c: &mut Ck, s: Common) {
    if let Err(e) = check_scale(s.scale) {
        c.fail("scale", if e.contains("power of ten") { "not_a_power_of_ten" } else { "out_of_range" });
    }
    c.range("minFill", s.min_fill, 0, AMOUNT_MAX);
    c.range("tip", s.tip, 0, PRICE_MAX);
    c.range("activeFrom", s.active_from, 0, DAA_MAX);
    c.range("refundTip", s.refund_tip, 0, MONEY_MAX as i64);
    c.range("tplPrefixLen", s.tpl_lens.0, 0, 1 << 20);
    c.range("tplSuffixLen", s.tpl_lens.1, 0, 1 << 20);
    c.range("expiryDaa", s.expiry_daa, 0, i64::MAX);
}

/// Fields of every pair order: both tokens (scales, template lengths, family codes), the common numbers.
fn pair_common(c: &mut Ck, t: &PairTokens, min_fill: i64, tip: i64, active_from: i64, expiry_daa: i64, refund_tip: i64) {
    common(
        c,
        Common { scale: t.a.scale, min_fill, tip, active_from, tpl_lens: (t.a.prefix_len, t.a.suffix_len), expiry_daa, refund_tip },
    );
    if check_scale(t.b.scale).is_err() {
        c.fail("bScale", "out_of_range");
    }
    c.range("bPrefixLen", t.b.prefix_len, 0, 1 << 20);
    c.range("bSuffixLen", t.b.suffix_len, 0, 1 << 20);
    c.range("aFamily", t.a.family, 1, 2);
    c.range("bFamily", t.b.family, 1, 2);
}

/// The window over which the matcher, the keepers and the indexer ever evaluate an order's decay / rise path, DAA: `t - origin`
/// with `origin >= activeFrom` (the activation, or a later TWAP / DCA slice opening) and `t < refund_due <= expiryDaa` (the
/// matcher excludes an order from its refund time on: `matcher::candidate::excluded_by_expiry`; IOC / FOK die even earlier), at
/// most [`T_MAX`]. The covenant itself does not clamp `t - origin` (`price - slope * floor((t - origin) / decayStep)`, then
/// the `priceEnd` clamp); the filler chooses `t` and a fill whose product overflows the script's integers fails on chain, so
/// the bound that matters is the window this executor evaluates. Bounding by [`T_MAX`] alone refused every auction of a
/// high-priced token (protocol v3 prices per whole token: TBTC ~2e14 sompi, a 3 % market auction over 300 DAA moves 2e10
/// sompi per DAA, above the 2^26 that `slope * 2^34 <= 2^60` allows) although its path over its own minutes fits easily.
fn window(active_from: i64, expiry_daa: i64) -> i128 {
    (expiry_daa as i128 - active_from as i128).clamp(0, T_MAX)
}

/// Decay / rise path of an ask or bid (`slope` 0 = a constant price) over `window` DAA: the highest quote the path reaches.
fn path(c: &mut Ck, price: i64, price_end: i64, slope: i64, decay_step: i64, window: i128) -> i64 {
    c.range("price", price, 1, PRICE_MAX);
    c.range("slope", slope, 0, i64::MAX);
    if slope == 0 {
        // not read without a slope, but an 8-byte state push cannot carry i64::MIN
        c.range("priceEnd", price_end, i64::MIN + 1, i64::MAX);
        c.range("decayStep", decay_step, i64::MIN + 1, i64::MAX);
        return price;
    }
    c.range("priceEnd", price_end, 0, PRICE_MAX);
    c.range("decayStep", decay_step, 1, DAA_MAX);
    if decay_step >= 1 {
        c.le("slope", slope as i128 * (window / decay_step as i128), MONEY_MAX);
    }
    price.max(price_end)
}

/// Most fills an amount can take at a minimum fill (`ceil(amount / max(minFill, 1))`), as i128.
fn fills(amount: i64, min_fill: i64) -> i128 {
    let (a, m) = (amount.max(0) as i128, min_fill.max(1) as i128);
    (a + m - 1) / m
}

/// Checks a decoded order state (any family). `Ok` means the protocol's numeric gate holds, every sum and product the
/// matcher, keepers and indexer form from it fits, no divisor is zero, the numbers satisfy the covenant's own
/// acceptance rules, and the token program(s) the state pins are ones the builders can spend it with.
pub fn check(state: &AnyState) -> Result<(), String> {
    // A state built in memory (not decoded from the chain) may be unencodable: a malformed committed exit or a KRON
    // extension commitment (`validate`), or a number the 8-byte state pushes cannot carry (i64::MIN, which the range
    // checks below refuse in every field). Encoding the state here would cost more than all the rest of planning.
    if state.validate().is_err() {
        return Err("bad_state:encoding:unencodable".into());
    }
    let mut c = Ck(Ok(()));
    match state {
        AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
            common(
                &mut c,
                Common {
                    scale: s.scale,
                    min_fill: s.min_fill,
                    tip: s.tip,
                    active_from: s.active_from,
                    tpl_lens: (s.tpl_prefix_len, s.tpl_suffix_len),
                    expiry_daa: s.expiry_daa,
                    refund_tip: s.refund_tip,
                },
            );
            c.range("tif", s.tif, 0, 2);
            c.range("interval", s.interval, 0, DAA_MAX);
            c.range("maxFill", s.max_fill, 0, AMOUNT_MAX);
            let pmax = path(&mut c, s.price, s.price_end, s.slope, s.decay_step, window(s.active_from, s.expiry_daa));
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            c.quote_le("notional", s.amount_left, pmax as i128 + s.tip as i128, s.scale, NOTIONAL_MAX);
        }
        AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
            common(
                &mut c,
                Common {
                    scale: s.scale,
                    min_fill: s.min_fill,
                    tip: s.tip,
                    active_from: s.active_from,
                    tpl_lens: (s.tpl_prefix_len, s.tpl_suffix_len),
                    expiry_daa: s.expiry_daa,
                    refund_tip: s.refund_tip,
                },
            );
            // the covenant requires minFill > 0 (its termination rule reads one minimum fill of buying power)
            c.range("minFill", s.min_fill, 1, AMOUNT_MAX);
            c.range("tif", s.tif, 0, 2);
            c.range("interval", s.interval, 0, DAA_MAX);
            c.range("maxFill", s.max_fill, 0, AMOUNT_MAX);
            c.range("reserve", s.reserve, 0, MONEY_MAX as i64);
            c.range("deliveryCarrier", s.delivery_carrier, 0, MONEY_MAX as i64);
            let pmax = path(&mut c, s.price, s.price_end, s.slope, s.decay_step, window(s.active_from, s.expiry_daa));
            c.le("budgetRate", pmax as i128 + s.tip as i128, MONEY_MAX);
            // one minimum fill's budget must be computable (the covenant's continuation rule)
            c.quote_le("minFillBudget", s.min_fill, pmax as i128 + s.tip as i128, s.scale, NOTIONAL_MAX);
        }
        AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => {
            common(
                &mut c,
                Common {
                    scale: s.scale,
                    min_fill: s.min_fill,
                    tip: s.tip,
                    active_from: s.active_from,
                    tpl_lens: (s.tpl_prefix_len, s.tpl_suffix_len),
                    expiry_daa: s.expiry_daa,
                    refund_tip: s.refund_tip,
                },
            );
            cond_legs(&mut c, s.tp_price, s.stop_price, s.slip_bps, s.trail_step, s.trail_gap, s.armed, s.band_daa);
            c.range("trailWait", s.trail_wait, 0, DAA_MAX);
            c.range("minTouch", s.min_touch, 0, AMOUNT_MAX);
            c.range("minRestDaa", s.min_rest_daa, 0, DAA_MAX);
            c.range("keeperTip", s.keeper_tip, 0, MONEY_MAX as i64);
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            c.range("rptPrice", s.rpt_price, 0, PRICE_MAX);
            c.range("rptUntil", s.rpt_until, 0, i64::MAX);
            let pmax = s.tp_price.max(s.stop_price) as i128;
            c.quote_le("notional", s.amount_left, pmax + s.tip as i128 + s.rpt_price as i128, s.scale, NOTIONAL_MAX);
        }
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => {
            common(
                &mut c,
                Common {
                    scale: s.scale,
                    min_fill: s.min_fill,
                    tip: s.tip,
                    active_from: s.active_from,
                    tpl_lens: (s.tpl_prefix_len, s.tpl_suffix_len),
                    expiry_daa: s.expiry_daa,
                    refund_tip: s.refund_tip,
                },
            );
            cond_legs(&mut c, s.tp_price, s.stop_price, s.slip_bps, s.trail_step, s.trail_gap, s.armed, s.band_daa);
            c.range("trailWait", s.trail_wait, 0, DAA_MAX);
            c.range("minTouch", s.min_touch, 0, AMOUNT_MAX);
            c.range("minRestDaa", s.min_rest_daa, 0, DAA_MAX);
            c.range("keeperTip", s.keeper_tip, 0, MONEY_MAX as i64);
            c.range("deliveryCarrier", s.delivery_carrier, 0, MONEY_MAX as i64);
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            c.range("rptPrice", s.rpt_price, 0, PRICE_MAX);
            c.range("rptPre", s.rpt_pre, 0, PRICE_MAX);
            c.range("rptUntil", s.rpt_until, 0, i64::MAX);
            // multiply first, like the covenants (`stopPrice * bps / 10000`): dividing first would underestimate the ceiling
            let ceil = s.stop_price as i128 + s.stop_price as i128 * s.slip_bps.clamp(0, 10_000) as i128 / 10_000;
            let pmax = (s.tp_price as i128).max(ceil);
            let rate = pmax + s.tip as i128 + s.rpt_price as i128 + s.rpt_pre as i128;
            c.quote_le("notional", s.amount_left, rate, s.scale, NOTIONAL_MAX);
            c.le("carriers", fills(s.amount_left, s.min_fill) * s.delivery_carrier as i128, NOTIONAL_MAX);
        }
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => {
            common(
                &mut c,
                Common {
                    scale: s.scale,
                    min_fill: s.min_fill,
                    tip: s.tip,
                    active_from: s.active_from,
                    tpl_lens: (s.tpl_prefix_len, s.tpl_suffix_len),
                    expiry_daa: s.expiry_daa,
                    refund_tip: s.refund_tip,
                },
            );
            entry(&mut c, s.price, s.entry_stop, s.band_daa, s.armed, s.min_touch, s.min_rest_daa, s.keeper_tip);
            // a buy-stop trigger above the limit can be armed but never filled
            if s.entry_stop > s.price {
                c.fail("entryStop", "beyond_limit");
            }
            c.range("deliveryCarrier", s.delivery_carrier, 0, MONEY_MAX as i64);
            c.range("exitCarrier", s.exit_carrier, 0, MONEY_MAX as i64);
            c.range("rptAmount", s.rpt_amount, 0, AMOUNT_MAX);
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            let pmax = s.price.max(s.entry_stop) as i128;
            c.quote_le("notional", s.amount_left, pmax + s.tip as i128, s.scale, NOTIONAL_MAX);
            let per_fill = s.delivery_carrier as i128 + s.exit_carrier as i128;
            c.le("carriers", fills(s.amount_left, s.min_fill) * per_fill, NOTIONAL_MAX);
            match s.exit() {
                Ok(x) => {
                    if let Err(e) = check(&AnyState::KobCondAsk(CondAskState { amount_left: 1, ..x }).into_family(state.family())) {
                        c.fail("exitState", &e);
                    }
                }
                Err(_) => c.fail("exitState", "undecodable"),
            }
        }
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => {
            common(
                &mut c,
                Common {
                    scale: s.scale,
                    min_fill: s.min_fill,
                    tip: s.tip,
                    active_from: s.active_from,
                    tpl_lens: (s.tpl_prefix_len, s.tpl_suffix_len),
                    expiry_daa: s.expiry_daa,
                    refund_tip: s.refund_tip,
                },
            );
            entry(&mut c, s.price, s.entry_stop, s.band_daa, s.armed, s.min_touch, s.min_rest_daa, s.keeper_tip);
            // a sell-stop trigger below the limit can be armed but never filled
            if s.entry_stop > 0 && s.entry_stop < s.price {
                c.fail("entryStop", "beyond_limit");
            }
            c.range("prefund", s.prefund, 0, PRICE_MAX);
            c.range("exitCarrier", s.exit_carrier, 0, MONEY_MAX as i64);
            c.range("rptAmount", s.rpt_amount, 0, AMOUNT_MAX);
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            let pmax = s.price.max(s.entry_stop) as i128;
            c.quote_le("notional", s.amount_left, pmax + s.tip as i128 + s.prefund as i128, s.scale, NOTIONAL_MAX);
            c.le("carriers", fills(s.amount_left, s.min_fill) * s.exit_carrier as i128, NOTIONAL_MAX);
            match s.exit() {
                Ok(x) => {
                    if let Err(e) = check(&AnyState::KobCondBid(CondBidState { amount_left: 1, ..x }).into_family(state.family())) {
                        c.fail("exitState", &e);
                    }
                }
                Err(_) => c.fail("exitState", "undecodable"),
            }
        }
        AnyState::KobPair(s) => {
            let t = s.tokens();
            pair_common(&mut c, &t, s.min_fill, s.tip, s.active_from, s.expiry_daa, s.refund_tip);
            c.range("side", s.side, 1, 2);
            c.range("tif", s.tif, 0, 2);
            c.range("interval", s.interval, 0, DAA_MAX);
            c.range("maxFill", s.max_fill, 0, AMOUNT_MAX);
            c.range("deliveryCarrier", s.delivery_carrier, 0, MONEY_MAX as i64);
            // the price is B base units per whole A (any token amount, not sompi)
            c.range("price", s.price, 1, AMOUNT_MAX);
            c.range("slope", s.slope, 0, i64::MAX);
            if s.slope == 0 {
                c.range("priceEnd", s.price_end, i64::MIN + 1, i64::MAX);
                c.range("decayStep", s.decay_step, i64::MIN + 1, i64::MAX);
            } else {
                c.range("priceEnd", s.price_end, 1, AMOUNT_MAX);
                c.range("decayStep", s.decay_step, 1, DAA_MAX);
                if s.decay_step >= 1 {
                    c.le("slope", s.slope as i128 * (window(s.active_from, s.expiry_daa) / s.decay_step as i128), AMOUNT_MAX as i128);
                }
            }
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            c.range("custody", s.custody, 0, AMOUNT_MAX);
            let pmax = s.price.max(if s.slope != 0 { s.price_end } else { 0 });
            c.quote_le("delivery", s.amount_left, pmax as i128, t.a.scale, AMOUNT_MAX as i128);
            c.le("carriers", fills(s.amount_left, s.min_fill) * s.delivery_carrier as i128, NOTIONAL_MAX);
            c.quote_le("tips", s.amount_left, s.tip as i128, t.a.scale, NOTIONAL_MAX);
        }
        AnyState::KobCondPair(s) => {
            let t = s.tokens();
            pair_common(&mut c, &t, s.min_fill, s.tip, s.active_from, s.expiry_daa, s.refund_tip);
            c.range("side", s.side, 1, 2);
            cond_legs(&mut c, s.tp_price, s.stop_price, s.slip_bps, s.trail_step, s.trail_gap, s.armed, s.band_daa);
            c.range("trailWait", s.trail_wait, 0, DAA_MAX);
            c.range("minTouch", s.min_touch, 0, AMOUNT_MAX);
            c.range("minRestDaa", s.min_rest_daa, 0, DAA_MAX);
            c.range("keeperTip", s.keeper_tip, 0, MONEY_MAX as i64);
            c.range("deliveryCarrier", s.delivery_carrier, 0, MONEY_MAX as i64);
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            c.range("custody", s.custody, 0, AMOUNT_MAX);
            c.range("rptPrice", s.rpt_price, 0, PRICE_MAX);
            c.range("rptPre", s.rpt_pre, 0, PRICE_MAX);
            c.range("rptUntil", s.rpt_until, 0, i64::MAX);
            let ceil = s.stop_price as i128 + s.stop_price as i128 * s.slip_bps.clamp(0, 10_000) as i128 / 10_000;
            let pmax = (s.tp_price as i128).max(ceil);
            c.quote_le("notional", s.amount_left, pmax + s.rpt_price as i128 + s.rpt_pre as i128, t.a.scale, AMOUNT_MAX as i128);
            c.quote_le("tips", s.amount_left, s.tip as i128, t.a.scale, NOTIONAL_MAX);
            c.le("carriers", fills(s.amount_left, s.min_fill) * s.delivery_carrier as i128, NOTIONAL_MAX);
        }
        AnyState::KobIfdPair(s) => {
            let t = s.tokens();
            pair_common(&mut c, &t, s.min_fill, s.tip, s.active_from, s.expiry_daa, s.refund_tip);
            c.range("side", s.side, 1, 2);
            entry(&mut c, s.price, s.entry_stop, s.band_daa, s.armed, s.min_touch, s.min_rest_daa, s.keeper_tip);
            // a stop entry whose trigger is beyond its limit can be armed but never filled
            let buy = s.is_buy_first();
            if s.entry_stop > 0 && ((buy && s.entry_stop > s.price) || (!buy && s.entry_stop < s.price)) {
                c.fail("entryStop", "beyond_limit");
            }
            c.range("prefund", s.prefund, 0, AMOUNT_MAX);
            c.range("deliveryCarrier", s.delivery_carrier, 0, MONEY_MAX as i64);
            c.range("exitCarrier", s.exit_carrier, 0, MONEY_MAX as i64);
            c.range("rptAmount", s.rpt_amount, 0, AMOUNT_MAX);
            c.range("amountLeft", s.amount_left, 0, AMOUNT_MAX);
            c.range("custody", s.custody, 0, AMOUNT_MAX);
            let pmax = s.price.max(s.entry_stop) as i128;
            c.quote_le("notional", s.amount_left, pmax + s.prefund as i128, s.a_scale, AMOUNT_MAX as i128);
            c.quote_le("tips", s.amount_left, s.tip as i128, s.a_scale, NOTIONAL_MAX);
            let per_fill = s.delivery_carrier as i128 + s.exit_carrier as i128;
            c.le("carriers", fills(s.amount_left, s.min_fill) * per_fill, NOTIONAL_MAX);
            match s.exit() {
                Ok(x) => {
                    if let Err(e) = check(&AnyState::KobCondPair(CondPairState { amount_left: 1, custody: 1, ..x })) {
                        c.fail("exitState", &e);
                    }
                }
                Err(_) => c.fail("exitState", "undecodable"),
            }
        }
    }
    if c.0.is_ok() {
        // the protocol's own gate (scale, every full fill below 2^62): never wider than the bounds above, checked last so
        // that a failing field is named first
        if state.check_numbers().is_err() {
            c.fail("numbers", "outside_the_numeric_gate");
        }
    }
    if c.0.is_ok() && kob_protocol::build::order_programs(state).is_err() {
        // the token program(s) the order pins, resolved as the builders resolve them (template hash, prefix / suffix lengths,
        // family; a pair order: both tokens): an order the builders cannot spend is never listed or planned
        c.fail("tokenProgram", "not_the_pinned_program");
    }
    c.0
}

#[allow(clippy::too_many_arguments)]
fn cond_legs(c: &mut Ck, tp: i64, stop: i64, slip_bps: i64, trail_step: i64, trail_gap: i64, armed: i64, band_daa: i64) {
    c.range("tpPrice", tp, 0, PRICE_MAX);
    // the covenants refuse a stop leg whose `stopPrice * slipBps` does not fit an i64
    c.range("stopPrice", stop, 0, MAX_STOP_PRICE);
    c.range("slipBps", slip_bps, 0, 10_000);
    c.range("trailStep", trail_step, 0, PRICE_MAX);
    c.range("trailGap", trail_gap, 0, PRICE_MAX);
    c.range("armed", armed, 0, DAA_MAX);
    c.range("bandDaa", band_daa, 0, BAND_MAX);
}

#[allow(clippy::too_many_arguments)]
fn entry(c: &mut Ck, price: i64, entry_stop: i64, band_daa: i64, armed: i64, min_touch: i64, min_rest: i64, keeper_tip: i64) {
    c.range("price", price, 1, PRICE_MAX);
    c.range("entryStop", entry_stop, 0, PRICE_MAX);
    c.range("bandDaa", band_daa, 0, BAND_MAX);
    c.range("armed", armed, 0, DAA_MAX);
    c.range("minTouch", min_touch, 0, AMOUNT_MAX);
    c.range("minRestDaa", min_rest, 0, DAA_MAX);
    c.range("keeperTip", keeper_tip, 0, MONEY_MAX as i64);
}

/// `state.custody_amount()` of a state that passes [`check`] (`None` for a state that holds no tokens or fails it).
pub fn custody_amount(state: &AnyState) -> Option<i64> {
    check(state).ok()?;
    state.custody_amount()
}
