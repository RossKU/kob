//! Replace by cancel-replace from the order kinds' own intents (`web/src/ui/orders/replace-intent.ts`, `replace-env.ts`):
//! a live order the in-place / quick amend does not cover (a trailing stop, an if-done entry, any pair order) is turned
//! back into the intent it was planned from, the user's changes are applied, the planner re-plans it in an environment
//! where the order's own KAS and custody come back, and the planned order becomes the replacement of ONE cancel + new
//! order transaction (`actions::build_replace`).
//!
//! Not replaced: repeat entries (their booked exits re-arm the entry: the position is cancelled instead), booked exits
//! (bound to their entry), IOC / FOK / market auctions (they end within their window).

use kob_protocol::state::{AnyState, CondPairState, IfdPairState, PairState, SIDE_ASK, TIF_GTC};
use kob_protocol::tx::{KeyUtxo, TokenUtxo, Utxo};

use super::plan::{
    Activation, CondIntent, CondKind, Crossing, DutchIntent, Env, IfdIntent, IfdMore, Intent, Lifetime, LimitIntent, ScheduleIntent,
    Side, StopKnobs, Trail, MAX_IDLE_DAA, SLIPPAGE_BPS,
};

/// A dated expiry above this is "no date" (the GTC of an exit).
const NO_DATE_DAA: i64 = 1 << 60;
/// A GTC is placement + 90 days: an expiry this close to genesis + 90 days is a GTC.
const GTC_TOLERANCE_DAA: i64 = 100_000;
/// The stand-in transaction id of the order's own KAS and custodies in the planning environment.
const SYNTH_TXID: [u8; 32] = [0xff; 32];

/// What the user changes (state prices: per whole token, a pair B per whole A; amounts base units of A).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Limit price (a pair limit), take-profit (a conditional), entry price (an if-done entry).
    pub price: Option<i64>,
    /// An if-done exit's take-profit (a conditional's take-profit, as `price`).
    pub take_profit: Option<i64>,
    /// Stop (a conditional), exit stop (an if-done entry).
    pub stop: Option<i64>,
    /// The stop leg's limit.
    pub stop_limit: Option<i64>,
    /// Base units.
    pub amount: Option<i64>,
    /// Tip (KAS per whole token).
    pub tip: Option<i64>,
    /// An if-done exit's tip.
    pub exit_tip: Option<i64>,
    /// Trailing step and gap.
    pub trail_step: Option<i64>,
    /// Trailing gap.
    pub trail_gap: Option<i64>,
    /// An if-done entry's stop.
    pub entry_stop: Option<i64>,
    /// A fresh 90-day GTC.
    pub renew: bool,
}

/// The order's lifetime class: GTC (re-placed as a fresh GTC), a date, a day order.
pub fn lifetime_of(expiry_daa: i64, genesis_daa: Option<i64>, deadline: Option<u64>, renew: bool) -> Lifetime {
    if renew {
        return Lifetime::Gtc;
    }
    if deadline.is_some() {
        return Lifetime::Day;
    }
    let gtc = expiry_daa >= NO_DATE_DAA
        || genesis_daa
            .is_some_and(|g| (g + MAX_IDLE_DAA - GTC_TOLERANCE_DAA..=g + MAX_IDLE_DAA + GTC_TOLERANCE_DAA).contains(&expiry_daa));
    if gtc {
        Lifetime::Gtc
    } else {
        Lifetime::GtdDaa(expiry_daa)
    }
}

/// Whether `s` is replaced through its intent (not the quick amend of `actions::replacement`).
pub fn replans(s: &AnyState) -> bool {
    match s {
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.trail_step > 0,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.trail_step > 0,
        AnyState::KobIfdBid(_)
        | AnyState::KobIfdBidKron(_)
        | AnyState::KobIfdAsk(_)
        | AnyState::KobIfdAskKron(_)
        | AnyState::KobPair(_)
        | AnyState::KobCondPair(_)
        | AnyState::KobIfdPair(_) => true,
        _ => false,
    }
}

/// The trigger of a stop leg as an intent: the stop, its band (the default 3 % as `slip_bps`, any other band as the
/// stop-limit whose limit is the band's worst price), the knobs, the trail.
struct StopTerms {
    stop: i64,
    /// The band as the state holds it (a trailing stop keeps it: its ticket has no stop-limit).
    slip_raw: i64,
    slip_bps: Option<i64>,
    limit: Option<i64>,
    knobs: StopKnobs,
    min_touch: i64,
    trail: Option<Trail>,
}

#[allow(clippy::too_many_arguments)]
fn stop_terms(
    side: Side,
    stop: i64,
    slip: i64,
    band: i64,
    keeper_tip: i64,
    min_touch: i64,
    min_rest: i64,
    trail: (i64, i64, i64),
) -> StopTerms {
    let mv = (stop as i128 * slip as i128 / 10_000) as i64;
    let worst = if side == Side::Sell { stop - mv } else { stop + mv };
    let (slip_bps, limit) = if slip == SLIPPAGE_BPS || worst <= 0 { (Some(slip), None) } else { (None, Some(worst)) };
    StopTerms {
        stop,
        slip_raw: slip,
        slip_bps,
        limit,
        knobs: StopKnobs { band_daa: Some(band), keeper_tip: Some(keeper_tip), min_rest: Some(min_rest) },
        min_touch: min_touch.max(1),
        trail: (trail.0 > 0).then_some(Trail { step: trail.0, gap: trail.1, wait: Some(trail.2), updates: None }),
    }
}

/// A conditional (KAS or pair) as its intent, with the changes applied.
#[allow(clippy::too_many_arguments)]
fn cond_intent(
    side: Side,
    amount: i64,
    min_fill: i64,
    tip: i64,
    tp: i64,
    t: StopTerms,
    has_stop: bool,
    lifetime: Lifetime,
    active_from: Option<Activation>,
    ch: &Changes,
) -> Result<Intent, String> {
    let amount = ch.amount.unwrap_or(amount);
    let tp = ch.price.or(ch.take_profit).or((tp > 0).then_some(tp));
    let stop = if has_stop { Some(ch.stop.unwrap_or(t.stop)) } else { None };
    if !has_stop && (ch.stop.is_some() || ch.stop_limit.is_some() || ch.trail_step.is_some() || ch.trail_gap.is_some()) {
        return Err("this order has no stop leg".into());
    }
    if ch.entry_stop.is_some() || ch.exit_tip.is_some() {
        return Err("--entry-stop and --exit-tip apply to an if-done entry".into());
    }
    let mut trail = t.trail;
    if ch.trail_step.is_some() || ch.trail_gap.is_some() {
        let Some(tr) = trail.as_mut() else {
            return Err("this stop does not trail: --trail-step / --trail-gap apply to a trailing stop".into());
        };
        tr.step = ch.trail_step.unwrap_or(tr.step);
        tr.gap = ch.trail_gap.unwrap_or(tr.gap);
    }
    if trail.is_some() && ch.stop_limit.is_some() {
        return Err("a trailing stop keeps its band: --stop-limit does not apply".into());
    }
    let limit = ch.stop_limit.or(t.limit);
    let slip = if ch.stop_limit.is_some() { None } else { t.slip_bps };
    let kind = match (trail.is_some(), stop.is_some(), tp.is_some(), limit.is_some()) {
        (true, ..) => CondKind::Trailing,
        (false, false, _, _) => CondKind::TakeProfit,
        (false, true, true, _) => CondKind::Oco,
        (false, true, false, true) => CondKind::StopLimit,
        (false, true, false, false) => CondKind::StopMarket,
    };
    Ok(Intent::Cond(CondIntent {
        kind,
        side,
        amount,
        tip: ch.tip.unwrap_or(tip),
        min_fill: (min_fill > 0 && min_fill <= amount).then_some(min_fill),
        stop,
        limit: if kind == CondKind::Trailing { None } else { limit },
        take_profit: tp,
        slip_bps: if kind == CondKind::Trailing { Some(t.slip_raw) } else { slip },
        min_touch: has_stop.then_some(t.min_touch),
        lifetime,
        active_from,
        max_fills: None,
        trail,
        knobs: if has_stop { t.knobs } else { StopKnobs::default() },
    }))
}

/// The ticket intent of a live order, with the user's changes (`intentFromOrder`). `now_daa` keeps a future activation;
/// `lifetime` is the order's class ([`lifetime_of`]).
pub fn intent_from_order(s: &AnyState, lifetime: Lifetime, now_daa: i64, ch: &Changes) -> Result<Intent, String> {
    let act = |active_from: i64| (active_from > now_daa).then_some(Activation::Daa(active_from));
    match s {
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => {
            if x.parent != [0; 32] {
                return Err("an exit of a repeating position is bound to its entry: cancel the position instead".into());
            }
            let t = stop_terms(
                Side::Sell,
                x.stop_price,
                x.slip_bps,
                x.band_daa,
                x.keeper_tip,
                x.min_touch,
                x.min_rest_daa,
                (x.trail_step, x.trail_gap, x.trail_wait),
            );
            cond_intent(
                Side::Sell,
                x.amount_left,
                x.min_fill,
                x.tip,
                x.tp_price,
                t,
                x.stop_price > 0,
                lifetime,
                act(x.active_from),
                ch,
            )
        }
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => {
            if x.parent != [0; 32] {
                return Err("an exit of a repeating position is bound to its entry: cancel the position instead".into());
            }
            let t = stop_terms(
                Side::Buy,
                x.stop_price,
                x.slip_bps,
                x.band_daa,
                x.keeper_tip,
                x.min_touch,
                x.min_rest_daa,
                (x.trail_step, x.trail_gap, x.trail_wait),
            );
            cond_intent(Side::Buy, x.amount_left, x.min_fill, x.tip, x.tp_price, t, x.stop_price > 0, lifetime, act(x.active_from), ch)
        }
        AnyState::KobCondPair(x) => cond_pair_intent(x, lifetime, act(x.active_from), ch),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => {
            let exit = x.exit().map_err(|e| e.to_string())?;
            let e = Entry {
                side: Side::Buy,
                amount: x.amount_left,
                price: x.price,
                tip: x.tip,
                min_fill: x.min_fill,
                stop: x.entry_stop,
                band: x.band_daa,
                keeper_tip: x.keeper_tip,
                min_touch: x.min_touch,
                min_rest: x.min_rest_daa,
                rpt_amount: x.rpt_amount,
                prefund: 0,
                active_from: act(x.active_from),
            };
            let xt = ExitTerms {
                tp: exit.tp_price,
                t: stop_terms(
                    Side::Sell,
                    exit.stop_price,
                    exit.slip_bps,
                    exit.band_daa,
                    exit.keeper_tip,
                    exit.min_touch,
                    exit.min_rest_daa,
                    (exit.trail_step, exit.trail_gap, exit.trail_wait),
                ),
                tip: exit.tip,
                min_fill: exit.min_fill,
                expiry_daa: exit.expiry_daa,
            };
            ifd_intent(e, xt, lifetime, ch)
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => {
            let exit = x.exit().map_err(|e| e.to_string())?;
            let e = Entry {
                side: Side::Sell,
                amount: x.amount_left,
                price: x.price,
                tip: x.tip,
                min_fill: x.min_fill,
                stop: x.entry_stop,
                band: x.band_daa,
                keeper_tip: x.keeper_tip,
                min_touch: x.min_touch,
                min_rest: x.min_rest_daa,
                rpt_amount: x.rpt_amount,
                prefund: x.prefund,
                active_from: act(x.active_from),
            };
            let xt = ExitTerms {
                tp: exit.tp_price,
                t: stop_terms(
                    Side::Buy,
                    exit.stop_price,
                    exit.slip_bps,
                    exit.band_daa,
                    exit.keeper_tip,
                    exit.min_touch,
                    exit.min_rest_daa,
                    (exit.trail_step, exit.trail_gap, exit.trail_wait),
                ),
                tip: exit.tip,
                min_fill: exit.min_fill,
                expiry_daa: exit.expiry_daa,
            };
            ifd_intent(e, xt, lifetime, ch)
        }
        AnyState::KobIfdPair(x) => ifd_pair_intent(x, lifetime, act(x.active_from), ch),
        AnyState::KobPair(x) => pair_intent(x, lifetime, now_daa, act(x.active_from), ch),
        other => Err(format!("a {} is amended by `kob replace` with its own flags (not replanned)", other.template_id().name())),
    }
}

fn cond_pair_intent(x: &CondPairState, lifetime: Lifetime, active_from: Option<Activation>, ch: &Changes) -> Result<Intent, String> {
    if x.parent != [0; 32] {
        return Err("an exit of a repeating position is bound to its entry: cancel the position instead".into());
    }
    let side = if x.side == SIDE_ASK { Side::Sell } else { Side::Buy };
    let t = stop_terms(
        side,
        x.stop_price,
        x.slip_bps,
        x.band_daa,
        x.keeper_tip,
        x.min_touch,
        x.min_rest_daa,
        (x.trail_step, x.trail_gap, x.trail_wait),
    );
    cond_intent(side, x.amount_left, x.min_fill, x.tip, x.tp_price, t, x.stop_price > 0, lifetime, active_from, ch)
}

struct Entry {
    side: Side,
    amount: i64,
    price: i64,
    tip: i64,
    min_fill: i64,
    stop: i64,
    band: i64,
    keeper_tip: i64,
    min_touch: i64,
    min_rest: i64,
    rpt_amount: i64,
    prefund: i64,
    active_from: Option<Activation>,
}

struct ExitTerms {
    tp: i64,
    t: StopTerms,
    tip: i64,
    min_fill: i64,
    expiry_daa: i64,
}

fn ifd_intent(e: Entry, x: ExitTerms, lifetime: Lifetime, ch: &Changes) -> Result<Intent, String> {
    if e.rpt_amount > 0 {
        return Err("a repeating entry is not replaced (its booked exits re-arm it): cancel the position and place it again".into());
    }
    if (ch.trail_step.is_some() || ch.trail_gap.is_some()) && x.t.trail.is_none() && x.t.stop == 0 && ch.stop.is_none() {
        return Err("a trailing exit needs an exit stop".into());
    }
    let amount = ch.amount.unwrap_or(e.amount);
    let has_stop = x.t.stop > 0 || ch.stop.is_some();
    let stop = has_stop.then(|| ch.stop.unwrap_or(x.t.stop));
    let mut trail = x.t.trail;
    if ch.trail_step.is_some() || ch.trail_gap.is_some() {
        let tr = trail.get_or_insert(Trail { step: 0, gap: 0, wait: None, updates: None });
        tr.step = ch.trail_step.unwrap_or(tr.step);
        tr.gap = ch.trail_gap.unwrap_or(tr.gap);
    }
    let stop_limit = ch.stop_limit.or(if x.t.stop > 0 { x.t.limit } else { None });
    // a dated exit keeps its date when the entry is dated (an exit's GTC bound counts from its own creation)
    let exit_expiry_daa =
        (matches!(lifetime, Lifetime::GtdDaa(_)) && x.expiry_daa > 0 && x.expiry_daa < NO_DATE_DAA).then_some(x.expiry_daa);
    let entry_stop = ch.entry_stop.or((e.stop > 0).then_some(e.stop));
    let stop_entry_kept = ch.entry_stop.is_none() && e.stop > 0;
    let sell_first = e.side == Side::Sell;
    Ok(Intent::Ifd(IfdIntent {
        side: e.side,
        amount,
        tip: ch.tip.unwrap_or(e.tip),
        min_fill: (e.min_fill > 0 && e.min_fill <= amount).then_some(e.min_fill),
        price: ch.price.unwrap_or(e.price),
        take_profit: ch.take_profit.or((x.tp > 0).then_some(x.tp)),
        stop,
        stop_limit,
        exit_tip: ch.exit_tip.unwrap_or(x.tip),
        prefund: (sell_first && e.prefund > 0 && ch.price.is_none() && ch.stop.is_none() && ch.take_profit.is_none())
            .then_some(e.prefund),
        lifetime,
        active_from: e.active_from,
        more: IfdMore {
            entry_stop,
            entry_knobs: if stop_entry_kept {
                StopKnobs { band_daa: Some(e.band), keeper_tip: Some(e.keeper_tip), min_rest: Some(e.min_rest) }
            } else {
                StopKnobs::default()
            },
            entry_min_touch: (stop_entry_kept && e.min_touch > 0).then_some(e.min_touch),
            exit_slip_bps: if stop_limit.is_some() { None } else { x.t.slip_bps.filter(|_| x.t.stop > 0) },
            exit_knobs: if x.t.stop > 0 { x.t.knobs } else { StopKnobs::default() },
            exit_min_touch: (x.t.stop > 0).then_some(x.t.min_touch),
            exit_trail: trail,
            exit_min_fill: (x.min_fill > 0 && x.min_fill <= amount).then_some(x.min_fill),
            exit_expiry_daa,
            repeat: None,
        },
    }))
}

fn ifd_pair_intent(x: &IfdPairState, lifetime: Lifetime, active_from: Option<Activation>, ch: &Changes) -> Result<Intent, String> {
    let exit = x.exit().map_err(|e| e.to_string())?;
    let side = if x.is_buy_first() { Side::Buy } else { Side::Sell };
    let e = Entry {
        side,
        amount: x.amount_left,
        price: x.price,
        tip: x.tip,
        min_fill: x.min_fill,
        stop: x.entry_stop,
        band: x.band_daa,
        keeper_tip: x.keeper_tip,
        min_touch: x.min_touch,
        min_rest: x.min_rest_daa,
        rpt_amount: x.rpt_amount,
        prefund: x.prefund,
        active_from,
    };
    let xt = ExitTerms {
        tp: exit.tp_price,
        t: stop_terms(
            side.other(),
            exit.stop_price,
            exit.slip_bps,
            exit.band_daa,
            exit.keeper_tip,
            exit.min_touch,
            exit.min_rest_daa,
            (exit.trail_step, exit.trail_gap, exit.trail_wait),
        ),
        tip: exit.tip,
        min_fill: exit.min_fill,
        expiry_daa: exit.expiry_daa,
    };
    ifd_intent(e, xt, lifetime, ch)
}

fn ceil_div(a: i64, b: i64) -> i64 {
    ((a as i128 + b as i128 - 1) / b as i128) as i64
}

/// A resting pair order (limit, timed limit, TWAP / DCA, Dutch / rising) as its intent.
fn pair_intent(
    x: &PairState,
    lifetime: Lifetime,
    now_daa: i64,
    active_from: Option<Activation>,
    ch: &Changes,
) -> Result<Intent, String> {
    if x.tif != TIF_GTC {
        return Err("an IOC / FOK / market pair order ends within its window: there is nothing to replace".into());
    }
    if ch.stop.is_some() || ch.stop_limit.is_some() || ch.take_profit.is_some() || ch.trail_step.is_some() || ch.trail_gap.is_some() {
        return Err("a pair limit has no stop or take-profit leg".into());
    }
    if ch.entry_stop.is_some() || ch.exit_tip.is_some() {
        return Err("--entry-stop and --exit-tip apply to an if-done entry".into());
    }
    let side = if x.side == SIDE_ASK { Side::Sell } else { Side::Buy };
    let amount = ch.amount.unwrap_or(x.amount_left);
    let tip = ch.tip.unwrap_or(x.tip);
    let min_fill = (x.min_fill > 0 && x.min_fill <= amount).then_some(x.min_fill);
    if x.interval > 0 && x.max_fill > 0 {
        // TWAP / DCA: the per-slice auction from the price to its end at `slope` per DAA
        let diff = (x.price - x.price_end).abs();
        let (price_end, slice_auction) =
            if x.slope > 0 && x.price_end > 0 && diff > 0 { (Some(x.price_end), Some(ceil_div(diff, x.slope))) } else { (None, None) };
        return Ok(Intent::Schedule(ScheduleIntent {
            dca: side == Side::Buy,
            amount,
            tip,
            min_fill: min_fill.map(|m| m.min(x.max_fill)),
            slice: x.max_fill.min(amount),
            interval_daa: x.interval,
            price: ch.price.unwrap_or(x.price),
            price_end,
            slice_auction_daa: slice_auction,
            lifetime,
            active_from,
            max_fills: None,
        }));
    }
    if x.slope > 0 && x.price_end > 0 && ch.price.is_none() {
        // Dutch / rising: the decay goes on from where it is now (same end and pace); ended: a limit at its end
        let sell = side == Side::Sell;
        let step = x.decay_step.max(1);
        let origin = x.active_from;
        let steps = if now_daa > origin { (now_daa - origin) / step } else { 0 };
        let moved = steps.saturating_mul(x.slope);
        let start = if sell { x.price.saturating_sub(moved) } else { x.price.saturating_add(moved) };
        let ended = if sell { start <= x.price_end } else { start >= x.price_end };
        if !ended {
            let left = ceil_div(if sell { start - x.price_end } else { x.price_end - start }, x.slope);
            return Ok(Intent::Dutch(DutchIntent {
                side,
                amount,
                tip,
                min_fill,
                price: start,
                price_end: x.price_end,
                duration_daa: left * step,
                step_daa: Some(step),
                lifetime,
                active_from: None,
                max_fills: None,
            }));
        }
        return Ok(Intent::Limit(limit_of(side, amount, x.price_end, tip, min_fill, lifetime, None)));
    }
    let price = ch.price.unwrap_or(if x.slope > 0 && x.price_end > 0 { x.price_end } else { x.price });
    Ok(Intent::Limit(limit_of(side, amount, price, tip, min_fill, lifetime, active_from)))
}

fn limit_of(
    side: Side,
    amount: i64,
    price: i64,
    tip: i64,
    min_fill: Option<i64>,
    lifetime: Lifetime,
    active_from: Option<Activation>,
) -> LimitIntent {
    LimitIntent { side, amount, price, tip, min_fill, lifetime, active_from, crossing: Crossing::Auction, max_fills: None }
}

/// The planning environment of a REPLACEMENT (`replaceEnv`): the order's own custody tokens and KAS come back in the
/// same transaction, so the planner may count them (stand-in UTXOs it never spends: only the planned state, value and
/// carriers are kept), and the order itself is no own order to self-trade against.
pub fn replace_env(env: &Env, cov: &[u8; 32], s: &AnyState, value: u64) -> Env {
    let mut out = env.clone();
    out.own.retain(|o| o.cov != *cov);
    out.funding.push(KeyUtxo {
        utxo: Utxo { transaction_id: SYNTH_TXID, index: 0, amount: value, block_daa_score: 0, covenant_id: None },
        pubkey: env.maker,
    });
    let held = |token: [u8; 32]| -> i64 { s.custodies().iter().filter(|c| c.0 == token).map(|c| c.1).sum() };
    let synth = |m: &super::market::TokenMarket, amount: i64, index: u32| TokenUtxo {
        utxo: Utxo { transaction_id: SYNTH_TXID, index, amount: 100_000_000, block_daa_score: 0, covenant_id: Some(m.cov) },
        state: kob_protocol::state::TokenState::user(m.program, amount, env.maker, m.ext),
    };
    let a = held(env.market.cov);
    if a > 0 {
        out.token_utxos.push(synth(&env.market, a, 1));
    }
    if let Some(p) = out.pair.as_mut() {
        let b = held(p.quote.cov);
        if b > 0 {
            let u = synth(&p.quote, b, 2);
            p.quote_utxos.push(u);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifetime_classes() {
        let g = 1_000_000;
        assert_eq!(lifetime_of(g + MAX_IDLE_DAA + 10, Some(g), None, false), Lifetime::Gtc);
        assert_eq!(lifetime_of(g + 500_000, Some(g), None, false), Lifetime::GtdDaa(g + 500_000));
        assert_eq!(lifetime_of(g + 500_000, None, None, false), Lifetime::GtdDaa(g + 500_000));
        assert_eq!(lifetime_of(1 << 62, None, None, false), Lifetime::Gtc);
        assert_eq!(lifetime_of(g + 500_000, Some(g), Some(1_790_726_400), false), Lifetime::Day);
        assert_eq!(lifetime_of(g + 500_000, Some(g), Some(1_790_726_400), true), Lifetime::Gtc);
    }
}
