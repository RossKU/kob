//! Pair planners for conditional and if-done orders on a token pair A/B.
//!
//! This module plans the pair conditionals (`KobCondPair`: stop-market, stop-limit, trailing stop,
//! take-profit and OCO) and the pair if-done entries (`KobIfdPair` with a committed `KobCondPair`
//! exit: IFD, IFO, stop entries and repeat). It ports `web/src/kob/orders/pair-cond.ts`,
//! `pair-ifd.ts` and the placement tail of `pair-common.ts` (`placePair`). The plain pair kinds
//! of `plan.rs` share that tail.
//!
//! # Units
//!
//! - Amounts are base units of A. A custody is in base units of its own token.
//! - Prices are base units of B per whole A.
//! - Tips, keeper tips and carriers are KAS. A `tip` is in sompi per whole A, prefunded on the
//!   order UTXO, and is never part of a B price.
//!
//! Every rounded amount is the covenant's own arithmetic
//! (`kob_protocol::state::{CondPairState, IfdPairState}`).

use kob_protocol::build::{check_new_order, min_order_value, CreateOrder};
use kob_protocol::defaults::{default_min_fill_ifd, default_min_fill_pair, pair_tips, Tips};
use kob_protocol::state::{AnyState, CondPairState, IfdPairState, SIDE_ASK, SIDE_BID};
use kob_protocol::tx::KeyUtxo;
use kob_protocol::Family;

use super::market::TokenMarket;
use super::plan::*;

/// Fills for which an if-done exit budgets a delivery carrier, enough for two partial take-profits.
pub const DEFAULT_EXIT_FILLS: i64 = 2;
/// KAS cost of the merge in a buy-first pair repeat.
///
/// The tip rate spreads it over the smallest take-profit fill of the exit.
pub const PAIR_MERGE_COST_BUY_FIRST: i64 = 2_370_000;
/// KAS cost of the merge in a sell-first pair repeat, spread like [`PAIR_MERGE_COST_BUY_FIRST`].
pub const PAIR_MERGE_COST_SELL_FIRST: i64 = 1_690_000;
/// B token outputs of a sell-first repeating exit's partial take-profit that re-arms.
pub const PAIR_SELL_FIRST_MERGE_B_OUTPUTS: usize = 4;

fn ceil_div(a: i128, b: i128) -> i128 {
    (a + b - 1) / b
}

/// The family code of new outputs of a pair token: 2 for KRON, else 1.
fn fam(m: &TokenMarket) -> i64 {
    if m.family == Family::Kron {
        2
    } else {
        1
    }
}
/// The extension commitment of new outputs of a pair token: zero for KRON.
fn ext(m: &TokenMarket) -> [u8; 32] {
    if m.family == Family::Kron {
        [0; 32]
    } else {
        m.ext
    }
}

/// The default tips of an order of this pair (`pairTipsOf`).
pub(crate) fn tips_of(env: &Env, pair: &PairPart) -> Result<Tips, Issue> {
    pair_tips(env.market.program, pair.quote.program).map_err(|e| error("BUILD_REJECTED", e.to_string()))
}

/// The priority tip a pair order carries when the user sets none, in sompi per whole A.
///
/// It must equal `defaultPairTip` of `web/src/kob/orders/pair-common.ts`. It is the keeper tip of
/// the pair (`pair_keeper_tips.json`, at least twice the fee of one update) over the whole amount,
/// rounded up. The executor nets two crossing pair orders only when their tips pay the network fee
/// of the netting, because a netting moves no KAS. An untipped crossing pair order would wait for a
/// route through the KAS books. It is 0 without an amount or a table row for the pair.
pub fn default_pair_tip(env: &Env, amount: i64) -> i64 {
    let Some(pair) = &env.pair else { return 0 };
    if amount <= 0 {
        return 0;
    }
    let Ok(t) = tips_of(env, pair) else { return 0 };
    i64::try_from(ceil_div(t.keeper_tip as i128 * env.market.scale as i128, amount as i128)).unwrap_or(i64::MAX)
}

/// Whether the tip typed for a pair order is below the default (`pairTipBelowDefault`).
///
/// The total over `amount` is compared. A crossing pair order with such a tip may wait for a route
/// through the KAS books.
pub fn pair_tip_below_default(env: &Env, amount: i64, tip: i64) -> bool {
    let Some(pair) = &env.pair else { return false };
    if amount <= 0 {
        return false;
    }
    let Ok(t) = tips_of(env, pair) else { return false };
    amount as i128 * tip as i128 / (env.market.scale as i128) < t.keeper_tip as i128
}

/// The note for an explicit tip below the default, one line on stderr.
pub const PAIR_TIP_NOTE: &str = "With no tip, a crossing pair order may wait for a route through the KAS books.";

/// Fills in the tip defaults of a pair order (`kob place` with `--quote`).
///
/// A tip that was not typed (`tip_typed` or `exit_tip_typed` false) becomes the default of the pair
/// over the amount of the order, for a close the held amount. A typed tip stays as typed, zero
/// included. It returns whether a typed tip is below the default, so that the caller can say so in
/// one line. An order on a KAS book (no pair) is untouched.
pub fn apply_pair_tip_defaults(env: &Env, intent: &mut Intent, tip_typed: bool, exit_tip_typed: bool) -> bool {
    if env.pair.is_none() {
        return false;
    }
    let (amount, tip, exit_tip): (i64, &mut i64, Option<&mut i64>) = match intent {
        Intent::Limit(i) => (i.amount, &mut i.tip, None),
        Intent::Immediate(i) => (i.amount, &mut i.tip, None),
        Intent::Market(i) => (i.amount, &mut i.tip, None),
        Intent::Streaming(i) => (i.amount, &mut i.tip, None),
        Intent::Close(i) => (i.amount.unwrap_or_else(|| held_amount(env)), &mut i.tip, None),
        Intent::Schedule(i) => (i.amount, &mut i.tip, None),
        Intent::Dutch(i) => (i.amount, &mut i.tip, None),
        Intent::Cond(i) => (i.amount, &mut i.tip, None),
        Intent::Ifd(i) => (i.amount, &mut i.tip, Some(&mut i.exit_tip)),
    };
    let mut below = false;
    for (typed, t) in [(tip_typed, Some(tip)), (exit_tip_typed, exit_tip)] {
        let Some(t) = t else { continue };
        if typed {
            below |= pair_tip_below_default(env, amount, *t);
        } else {
            *t = default_pair_tip(env, amount);
        }
    }
    below
}

/// The environment the shared leg and expiry helpers run in for a pair (`legEnvOf`).
///
/// Prices are B per whole A, so the KAS tick does not apply. The default keeper and refund tips
/// are those of the pair.
fn leg_env(env: &Env, t: &Tips) -> Env {
    Env {
        market: TokenMarket { tick: 1, keeper_tip: t.keeper_tip as i64, refund_tip: t.refund_tip as i64, ..env.market.clone() },
        ..env.clone()
    }
}

/// Refuses an order whose B value at `rate` reaches 2^62.
fn check_pair_notional(env: &Env, amount: i64, rate: i64) -> Vec<Issue> {
    if amount <= 0 || rate <= 0 {
        return vec![];
    }
    if ceil_div(amount as i128 * rate as i128, env.market.scale as i128) >= QUOTE_LIMIT {
        return vec![error("PAIR_NOTIONAL_TOO_LARGE", "the order value in the quote token is too large")];
    }
    vec![]
}

/// Refuses a negative KAS tip (sompi per whole A), or one whose total over `amount` reaches 2^62.
fn check_pair_tip(env: &Env, amount: i64, tip: i64, what: &str) -> Vec<Issue> {
    if tip < 0 {
        return vec![error("TIP_NEGATIVE", format!("{what} cannot be negative"))];
    }
    if ceil_div(amount as i128 * tip as i128, env.market.scale as i128) >= QUOTE_LIMIT {
        return vec![error("TIP_TOO_LARGE", format!("{what} is too large for this quantity"))];
    }
    vec![]
}

/// Everything a pair order type decides; [`place_pair_order`] checks, builds and discloses it.
pub(crate) struct PairSpec {
    pub(crate) order: AnyState,
    /// The committed exit of an if-done entry (listed last in `states`).
    pub(crate) exit: Option<AnyState>,
    /// KAS on the order UTXO, itemised (its value is their sum).
    pub(crate) lines: Vec<CarrierLine>,
    pub(crate) deadline: Option<i64>,
    pub(crate) side: Side,
    pub(crate) amount: i64,
    pub(crate) min_fill: i64,
    pub(crate) min_touch: Option<i64>,
    pub(crate) limit_price: Option<i64>,
    /// The B value of the whole amount at the limit.
    /// A sell receives at least it and a buy pays at most it.
    pub(crate) all_in_total: Option<i64>,
    pub(crate) expected_price: Option<i64>,
    pub(crate) worst_price: Option<i64>,
    pub(crate) tip: i64,
    pub(crate) refund_tip: i64,
    pub(crate) keeper_tip: i64,
    pub(crate) expiry_kind: &'static str,
    pub(crate) expiry_daa: i64,
    pub(crate) activates_at: Option<i64>,
    /// The token and amount that a minimum fill delivers. `true` means B. For the KRON output cap.
    pub(crate) delivery: (bool, i64),
    pub(crate) notes: Vec<&'static str>,
    /// KAS on each custody / token change output.
    pub(crate) carrier: i64,
}

/// The common tail of every pair planner (`placePair`).
///
/// It checks the KRON caps, `check_new_order` and `min_order_value`. It then builds the order,
/// drawing each custody from the maker's UTXOs of its own token, runs the recover-and-compare
/// self-check and writes the disclosure.
pub(crate) fn place_pair_order(env: &Env, pair: &PairPart, mut issues: Vec<Issue>, spec: PairSpec) -> Plan {
    let a = &env.market;
    let b = &pair.quote;
    let order = spec.order.clone();
    let states: Vec<AnyState> = std::iter::once(order.clone()).chain(spec.exit.clone()).collect();
    let fail = |issues: Vec<Issue>| Plan { ok: false, issues, states: states.clone(), request: None, built: None, disclosure: None };
    // KRON caps on what the order holds and delivers.
    for (tok, amount) in order.custodies() {
        let m = if tok == a.cov { a } else { b };
        if m.family == Family::Kron && amount > KRON_MAX_OUTPUT_AMOUNT {
            issues.push(error(
                "PAIR_KRON_CUSTODY_TOO_LARGE",
                format!("a KRON custody of {} holds at most {KRON_MAX_OUTPUT_AMOUNT} base units", m.name()),
            ));
        }
    }
    let dm = if spec.delivery.0 { b } else { a };
    if dm.family == Family::Kron && spec.delivery.1 > KRON_MAX_OUTPUT_AMOUNT {
        issues.push(error("PAIR_KRON_DELIVERY_TOO_LARGE", "a minimum fill would deliver more than a KRON output holds"));
    }
    if has_error(&issues) {
        return fail(issues);
    }
    if let Err(e) = check_new_order(&order) {
        issues.push(error("BUILD_REJECTED", format!("the protocol refused this order: {e}")));
        return fail(issues);
    }
    let value: i128 = spec.lines.iter().map(|l| l.amount * l.count as i128).sum();
    let min_value = min_order_value(&order) as i128;
    if value < min_value || !(0..=i64::MAX as i128).contains(&value) {
        issues.push(error("BUILD_REJECTED", format!("the order UTXO needs at least {min_value} sompi (planned {value})")));
        return fail(issues);
    }
    let value = value as i64;
    let dc = spec.carrier;
    let env = &Env { carrier: Some(dc as u64), ..env.clone() };
    // The custodies, each drawn from the maker's UTXOs of its own token.
    let mut tokens = vec![];
    let mut draws: Vec<([u8; 32], i64)> = vec![];
    let mut outputs_kas = 0i128;
    let mut inputs_kas = 0i128;
    let mut changes = vec![];
    for (tok, amount) in order.custodies() {
        let (m, utxos) = if tok == a.cov { (a, &env.token_utxos) } else { (b, &pair.quote_utxos) };
        let floor = order_carrier_floor(m);
        if dc < floor {
            issues.push(error(
                "CARRIER_BELOW_FLOOR",
                format!("the carrier ({dc} sompi) is below {floor} sompi, the least a token output of {} carries", m.name()),
            ));
            return fail(issues);
        }
        let usable = usable_token_utxos(m, &env.maker, utxos);
        let picked = match select_tokens(&usable, amount, m.slots.0.min(MAX_TOKEN_INPUTS), &m.name()) {
            Ok(p) => p,
            Err(e) => {
                issues.push(e);
                return fail(issues);
            }
        };
        let have: i128 = picked.iter().map(|u| u.state.amount() as i128).sum();
        inputs_kas += picked.iter().map(|u| u.utxo.amount as i128).sum::<i128>();
        outputs_kas += dc as i128 + if have > amount as i128 { dc as i128 } else { 0 };
        changes.push((tok, have - amount as i128));
        tokens.extend(picked);
        draws.push((tok, amount));
    }
    let need = (value as i128 + outputs_kas - inputs_kas).max(0);
    let request = |funding: Vec<KeyUtxo>| CreateOrder {
        order: order.clone(),
        value: value as u64,
        tokens: tokens.clone(),
        token_carrier: if tokens.is_empty() { 0 } else { dc as u64 },
        funding,
        change: None,
        lock_time: 0,
        deadline: spec.deadline.map(|x| x as u64),
        records: vec![],
        fee: fee_options(env),
    };
    let (req, built) = match build_with_reserve(env, need, request, |bt| verify_built(bt, &order, value, spec.deadline, &draws)) {
        Ok(x) => x,
        Err(e) => {
            issues.push(e);
            return fail(issues);
        }
    };
    let mut carriers = spec.lines.clone();
    let (mut escrow_a, mut escrow_b) = (0, 0);
    for (k, (tok, amount)) in draws.iter().enumerate() {
        let is_a = *tok == a.cov;
        carriers.push(CarrierLine {
            kind: if is_a { "custody carrier" } else { "quote custody carrier" },
            amount: dc as i128,
            count: 1,
            kept: false,
        });
        if changes[k].1 > 0 {
            carriers.push(CarrierLine {
                kind: if is_a { "token change carrier" } else { "quote change carrier" },
                amount: dc as i128,
                count: 1,
                kept: true,
            });
        }
        if is_a {
            escrow_a += amount;
        } else {
            escrow_b += amount;
        }
    }
    let kas_locked = carriers.iter().filter(|c| !c.kept).map(|c| c.amount * c.count as i128).sum();
    let mut notes = spec.notes.clone();
    notes.extend(["pairTipKas", "carrierReturned"]);
    let disclosure = Disclosure {
        side: spec.side,
        amount: spec.amount,
        scale: a.scale,
        min_fill: spec.min_fill,
        min_touch: spec.min_touch,
        limit_price: spec.limit_price,
        // A tip is KAS and never part of a B price, so the all-in price is the limit itself.
        all_in_price: spec.limit_price,
        all_in_total: spec.all_in_total,
        expected_price: spec.expected_price,
        worst_price: spec.worst_price,
        tip: spec.tip,
        carriers,
        kas_locked,
        tokens_escrowed: escrow_a,
        quote_escrowed: escrow_b,
        fee: built.fee.fee,
        refund_tip: spec.refund_tip,
        keeper_tip: spec.keeper_tip,
        expiry_kind: spec.expiry_kind,
        expiry_daa: spec.expiry_daa,
        expiry_unix: spec.deadline.unwrap_or_else(|| daa_to_unix(&env.clock, spec.expiry_daa)),
        deadline: spec.deadline,
        activates_at: spec.activates_at.filter(|x| *x > env.clock.daa).map(|x| (x, daa_to_unix(&env.clock, x))),
        notes,
    };
    Plan { ok: !has_error(&issues), issues, states, request: Some(req), built: Some(built), disclosure: Some(disclosure) }
}

/// The KAS lines of a `KobPair` or `KobCondPair` order UTXO.
///
/// They are `fills` delivery carriers and the tip prefund of the whole amount.
pub(crate) fn pair_kas_lines(total: i64, fills: i64, dc: i64) -> Vec<CarrierLine> {
    let n = fills.max(1);
    let tip_total = total - dc * n;
    let mut lines = vec![CarrierLine { kind: "delivery carrier", amount: dc as i128, count: n, kept: false }];
    if tip_total > 0 {
        lines.push(CarrierLine { kind: "tip prefund", amount: tip_total as i128, count: 1, kept: false });
    }
    lines
}

/// A new `KobCondPair` of the maker from validated legs.
///
/// A sell has side ASK and holds A. A buy has side BID and holds B.
#[allow(clippy::too_many_arguments)]
fn cond_pair_state(
    env: &Env,
    pair: &PairPart,
    side: Side,
    amount: i64,
    min_fill: i64,
    tip: i64,
    active_from: i64,
    expiry_daa: i64,
    refund_tip: i64,
    carrier: i64,
    custody: i64,
    l: &Legs,
) -> CondPairState {
    let (s, t) = if side == Side::Sell { (&env.market, &pair.quote) } else { (&pair.quote, &env.market) };
    CondPairState {
        maker: env.maker,
        side: if side == Side::Sell { SIDE_ASK } else { SIDE_BID },
        s_cov_id: s.cov,
        s_tpl_hash: s.tpl_hash,
        s_pre: s.prefix_len,
        s_suf: s.suffix_len,
        s_family: fam(s),
        s_scale: s.scale,
        t_cov_id: t.cov,
        t_tpl_hash: t.tpl_hash,
        t_pre: t.prefix_len,
        t_suf: t.suffix_len,
        t_family: fam(t),
        t_ext: ext(t),
        t_scale: t.scale,
        min_fill,
        tip,
        active_from,
        expiry_daa,
        refund_tip,
        delivery_carrier: carrier,
        tp_price: l.tp,
        slip_bps: l.slip,
        trail_step: l.trail_step,
        trail_gap: l.trail_gap,
        trail_wait: l.trail_wait,
        min_touch: l.min_touch,
        min_rest_daa: l.min_rest,
        band_daa: l.band,
        keeper_tip: l.keeper_tip,
        stop_price: l.stop,
        armed: 0,
        amount_left: amount,
        custody,
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
        s_ext: ext(s),
    }
}

/// Plans a stop, take-profit, OCO or trailing stop on a pair (`planPairLegs`).
pub(crate) fn plan_pair_cond(env: &Env, i: &CondIntent) -> Plan {
    let Some(pair) = &env.pair else { return failed(vec![error("PAIR_MISSING", "a pair order needs a quote token")]) };
    let a = &env.market;
    let sell = i.side == Side::Sell;
    let amount = i.amount;
    let mut log = check_amount(amount, a, sell);
    if i.tip < 0 {
        log.push(error("TIP_NEGATIVE", "the priority tip cannot be negative"));
    }
    let carrier = env.carrier();
    let tips = match tips_of(env, pair) {
        Ok(t) => t,
        Err(e) => return failed(vec![e]),
    };
    cond_shape(i, &mut log);
    if has_error(&log) {
        return failed(log);
    }
    let lenv = leg_env(env, &tips);
    let input = cond_leg_input(i);
    // The default minimum fill of a resting pair order is the amount of A worth 10 KAS on the KAS
    // book of A.
    let min_fill = i.min_fill.unwrap_or_else(|| default_min_fill_pair(amount, pair.kas_per_whole_a, a.scale));
    if i.min_fill.is_none() && pair.kas_per_whole_a.is_none() {
        log.push(warning(
            "PAIR_KAS_REFERENCE_MISSING",
            format!("no KAS price of {} is known: the default minimum fill is a quarter of the amount", a.name()),
        ));
    }
    log.extend(check_min_fill(min_fill, amount, "the minimum fill"));
    log.extend(check_pair_tip(env, amount, i.tip, "the tip"));
    if has_error(&log) {
        return failed(log);
    }
    let legs = resolve_legs(&lenv, &mut log, i.side, &input, "", min_fill, amount, lenv.market.keeper_tip);
    let act = resolve_activation(&lenv, &mut log, i.active_from);
    let expiry = act.as_ref().and_then(|x| resolve_expiry(&lenv, &mut log, i.lifetime, x.active_from));
    let (Some(legs), Some(act), Some(expiry)) = (legs, act, expiry) else { return failed(log) };
    let has_stop = legs.stop > 0;
    let has_tp = legs.tp > 0;
    log.extend(check_pair_notional(env, amount, legs.tp.max(legs.stop).max(legs.stop_worst)));
    if has_error(&log) {
        return failed(log);
    }
    // Self-trade on the B prices. Tips are KAS, so they never make a B price cross.
    log.extend(check_self_trade(&env.own, i.side, if sell { ask_worst(&legs) } else { bid_worst(&legs) }, 0));
    if has_stop {
        if let (true, Some(x)) = (sell, env.book.asks.first()) {
            if x.price <= legs.stop {
                log.push(warning("COND_STOP_ALREADY_REACHED", "the market is already at or below the stop"));
            }
        }
        if let (false, Some(x)) = (sell, env.book.bids.first()) {
            if x.price >= legs.stop {
                log.push(warning("COND_STOP_ALREADY_REACHED", "the market is already at or above the stop"));
            }
        }
    }
    if has_tp {
        if let Some(t) = crossing_touch(&env.book, i.side, legs.tp, 0) {
            log.push(warning("PAIR_TP_CROSSES", format!("the take-profit crosses the pair book ({t}): it can fill at once")));
        }
    }
    // KAS delivery carriers: one per budgeted fill, at most one per possible fill.
    let possible = ceil_div(amount as i128, min_fill as i128) as i64;
    let fills = i.max_fills.unwrap_or(possible.min(MAX_DEFAULT_PAIR_FILLS));
    if fills < 1 || fills > possible {
        log.push(error("MAX_FILLS_INVALID", format!("the number of fills must be 1..{possible}")));
        return failed(log);
    }
    if fills < possible {
        log.push(info(
            "PAIR_FILLS_LIMITED",
            format!("{fills} delivery carriers: after {} partial fills what is left fills only in full", fills - 1),
        ));
    }
    let mut st = cond_pair_state(
        env,
        pair,
        i.side,
        amount,
        min_fill,
        i.tip,
        act.active_from,
        expiry.daa,
        tips.refund_tip as i64,
        carrier,
        amount,
        &legs,
    );
    if !sell {
        // The B escrow is the whole amount at the worst leg, rounded down, plus one base unit.
        let Some(escrow) = st.bid_escrow(st.max_fills()) else {
            log.push(error("PAIR_NOTIONAL_TOO_LARGE", "the order value in the quote token is too large"));
            return failed(log);
        };
        st.custody = escrow;
    }
    // At least the deliveries that the protocol requires a new order to fund itself.
    let funded = fills.max(st.funded_fills());
    let Some(total) = st.kas_value(funded) else {
        log.push(error("TIP_TOO_LARGE", "the priority tip is too large for this quantity"));
        return failed(log);
    };
    let mut lines = pair_kas_lines(total, funded, carrier);
    // An arming or trailing keeper takes its tip from the order UTXO, so fund it on top.
    if has_stop && legs.keeper_tip * legs.expected_updates > 0 {
        lines.push(CarrierLine { kind: "keeper reserve", amount: legs.keeper_tip as i128, count: legs.expected_updates, kept: false });
    }
    let stop_worst = st.stop_worst();
    let limit_price = if has_tp { legs.tp } else { stop_worst };
    let expected = if has_tp { (!has_stop).then_some(legs.tp) } else { Some(legs.stop) };
    let worst = if sell { st.lowest() } else { st.worst() };
    let b_of = |n: i64, p: i64| if sell { st.t_out_min(n, p) } else { st.s_out(n, p) };
    let n1 = min_fill.min(amount);
    let delivery = if sell { (true, st.t_out_min(n1, legs.tp.max(legs.stop)).unwrap_or(0)) } else { (false, n1) };
    let mut notes = cond_notes(i.kind);
    if has_stop {
        notes.push("keeperReserve");
    }
    place_pair_order(
        env,
        pair,
        log,
        PairSpec {
            all_in_total: b_of(amount, limit_price),
            order: AnyState::KobCondPair(st.clone()),
            exit: None,
            lines,
            deadline: expiry.deadline,
            side: i.side,
            amount,
            min_fill,
            min_touch: has_stop.then_some(legs.min_touch),
            limit_price: Some(limit_price),
            expected_price: expected,
            worst_price: Some(worst),
            tip: i.tip,
            refund_tip: st.refund_tip,
            keeper_tip: legs.keeper_tip,
            expiry_kind: expiry.kind,
            expiry_daa: expiry.daa,
            activates_at: act.activates_at,
            delivery,
            notes,
            carrier,
        },
    )
}

/// Plans IFD, IFO, stop entries and repeat on a pair (`planPairIfd`).
///
/// The entry is a `KobIfdPair` with a committed `KobCondPair` exit.
pub(crate) fn plan_pair_ifd(env: &Env, i: &IfdIntent) -> Plan {
    let Some(pair) = &env.pair else { return failed(vec![error("PAIR_MISSING", "a pair order needs a quote token")]) };
    let a = &env.market;
    let b = &pair.quote;
    let amount = i.amount;
    let buy_first = i.side == Side::Buy;
    let exit_side = i.side.other();
    let repeat = i.more.repeat.is_some();
    let mut log = check_amount(amount, a, true);
    if i.tip < 0 {
        log.push(error("TIP_NEGATIVE", "the priority tip cannot be negative"));
    }
    let carrier = env.carrier();
    let tips = match tips_of(env, pair) {
        Ok(t) => t,
        Err(e) => return failed(vec![e]),
    };
    if has_error(&log) || !check_exit_shape(i, &mut log) {
        return failed(log);
    }
    let lenv = leg_env(env, &tips);
    log.extend(price_check(env, i.price, "the entry price"));
    if let Some(st) = i.more.entry_stop {
        log.extend(price_check(env, st, "the entry stop"));
    }
    if has_error(&log) || !check_entry_stop(i, &mut log) {
        return failed(log);
    }
    log.extend(check_pair_tip(env, amount, i.tip, "the tip"));
    // The default minimum fill of the entry is `ceil(amount / 4)`.
    let min_fill = i.min_fill.unwrap_or_else(|| default_min_fill_ifd(amount));
    if min_fill < 1 || min_fill > amount {
        log.push(error("COND_MIN_FILL_INVALID", "the minimum fill must be 1..the amount"));
        return failed(log);
    }
    let fills = ceil_div(amount as i128, min_fill as i128) as i64;
    let Some((rpt_amount, count)) = repeat_amount(i.more.repeat, amount, &mut log) else { return failed(log) };
    // The exit is built first, because the entry commits to it.
    if i.exit_tip < 0 {
        log.push(error("TIP_NEGATIVE", "the exit tip cannot be negative"));
        return failed(log);
    }
    let exit_input = exit_leg_input(i);
    let exit_min_default = default_min_fill_pair(amount, pair.kas_per_whole_a, a.scale);
    // The smallest take-profit fill an exit can see. The merge tip and the default trigger
    // threshold scale with it.
    let exit_min_base = i.more.exit_min_fill.unwrap_or(exit_min_default.min(min_fill));
    // A partial take-profit of a sell-first repeating exit that re-arms moves four B outputs. A B
    // program with fewer token outputs takes profit only by a fill that sells the exit out.
    let whole_exit = !buy_first && repeat && b.slots.1 < PAIR_SELL_FIRST_MERGE_B_OUTPUTS;
    let exit_min_fill = i.more.exit_min_fill.unwrap_or(if whole_exit { amount } else { exit_min_base });
    if whole_exit {
        let msg = format!(
            "{} allows {} token outputs per transaction: while the entry lives, each exit takes profit only by a fill of its whole amount",
            b.name(),
            b.slots.1
        );
        log.push(if exit_min_fill < amount { warning("PAIR_EXIT_WHOLE_FILLS", msg) } else { info("PAIR_EXIT_WHOLE_FILLS", msg) });
    }
    log.extend(check_min_fill(exit_min_fill, amount, "the exit minimum fill"));
    if has_error(&log) {
        return failed(log);
    }
    let merge_tip = if repeat {
        merge_tip_rate(if buy_first { PAIR_MERGE_COST_BUY_FIRST } else { PAIR_MERGE_COST_SELL_FIRST }, a.scale, exit_min_base)
    } else {
        0
    };
    let exit_tip = i.exit_tip + merge_tip;
    log.extend(check_pair_tip(env, amount, exit_tip, "the exit tip"));
    let legs = resolve_legs(&lenv, &mut log, exit_side, &exit_input, "exit ", exit_min_base, amount, lenv.market.keeper_tip);
    let act = resolve_activation(&lenv, &mut log, i.active_from);
    let entry_expiry = act.as_ref().and_then(|x| resolve_expiry(&lenv, &mut log, i.lifetime, x.active_from));
    let exit_exp = exit_expiry(&lenv, &mut log, i.more.exit_expiry_daa);
    let (Some(legs), Some(act), Some(entry_expiry), Some(exit_exp)) = (legs, act, entry_expiry, exit_exp) else {
        return failed(log);
    };
    if has_error(&log) {
        return failed(log);
    }
    // Economics in B. The tips are KAS, outside the rates.
    if legs.tp > 0 {
        let profit = if buy_first { legs.tp - i.price } else { i.price - legs.tp };
        if profit <= 0 {
            let msg = format!("the exit take-profit loses {} {} per whole token against the entry", -profit, b.name());
            if repeat {
                log.push(error("PAIR_TP_NOT_PROFITABLE", msg));
                return failed(log);
            }
            log.push(warning("PAIR_TP_NOT_PROFITABLE", msg));
        }
    }
    let exit_worst = if buy_first { ask_worst(&legs) } else { bid_worst(&legs) };
    // Sell-first: the prefund is the B per whole A beyond the entry price that covers the worst
    // buy-back of the exit.
    let mut prefund = 0;
    if !buy_first {
        let min_prefund = (exit_worst - i.price).max(0);
        prefund = i.prefund.unwrap_or(min_prefund);
        if prefund < 0 {
            log.push(error("COND_PREFUND_INVALID", "the prefund must not be negative"));
            return failed(log);
        }
        if prefund < min_prefund {
            log.push(error(
                "PAIR_PREFUND_SHORT",
                format!("the prefund must be at least {min_prefund} {} per whole token (given {prefund})", b.name()),
            ));
            return failed(log);
        }
    }
    let entry_top = i.price.max(i.more.entry_stop.unwrap_or(0));
    log.extend(check_pair_notional(env, amount, if buy_first { entry_top } else { entry_top + prefund }));
    log.extend(check_pair_notional(env, amount, legs.tp.max(legs.stop).max(legs.stop_worst)));
    if has_error(&log) {
        return failed(log);
    }
    // Self-trade. The entry is a real order now and the exit only later, so the exit only warns.
    log.extend(check_self_trade(&env.own, i.side, i.price, 0));
    for mut w in check_self_trade(&env.own, exit_side, exit_worst, 0) {
        w.severity = Severity::Warning;
        log.push(w);
    }
    if i.more.entry_stop.is_none() {
        if let Some(t) = touch_price(&env.book, i.side) {
            if if buy_first { t <= i.price } else { t >= i.price } {
                log.push(warning("COND_ENTRY_CROSSES", "the entry crosses the book: it fills right away at its limit"));
            }
        }
    }
    if has_error(&log) {
        return failed(log);
    }
    let Some(es) = entry_stop_of(i, i.price, min_fill, count, tips.keeper_tip as i64, &mut log) else { return failed(log) };
    let stop_entry = es.stop > 0;
    let keeper_reserve = es.keeper_tip * es.arm_updates;
    // The exit is committed. The entry writes its custody and amount per fill.
    let exit_draft =
        cond_pair_state(env, pair, exit_side, amount, exit_min_fill, exit_tip, 0, exit_exp, tips.refund_tip as i64, carrier, 0, &legs);
    let Some(exit_tip_kas) = exit_draft.tip_kas(amount) else {
        log.push(error("TIP_TOO_LARGE", "the exit tip is too large for this quantity"));
        return failed(log);
    };
    // The exit UTXO funds its own fills (a delivery carrier each, up to two), its KAS tip and the
    // updates of its keeper.
    let exit_fills = DEFAULT_EXIT_FILLS.min(ceil_div(amount as i128, exit_min_fill as i128) as i64);
    let exit_keeper = if legs.stop > 0 { legs.keeper_tip * legs.expected_updates } else { 0 };
    let exit_planned = carrier * exit_fills + exit_tip_kas + exit_keeper;
    let exit_state = IfdPairState::commit_exit(&exit_draft);
    let fa = |m: &TokenMarket| (m.cov, m.tpl_hash, m.prefix_len, m.suffix_len, fam(m), m.scale, ext(m));
    let (ac, at, ap, asf, afm, asc, aex) = fa(a);
    let (bc, bt, bp, bsf, bfm, bsc, bex) = fa(b);
    let draft_with = |exit_carrier: i64| IfdPairState {
        maker: env.maker,
        side: if buy_first { SIDE_BID } else { SIDE_ASK },
        a_cov_id: ac,
        a_tpl_hash: at,
        a_pre: ap,
        a_suf: asf,
        a_family: afm,
        a_scale: asc,
        a_ext: aex,
        b_cov_id: bc,
        b_tpl_hash: bt,
        b_pre: bp,
        b_suf: bsf,
        b_family: bfm,
        b_scale: bsc,
        b_ext: bex,
        price: i.price,
        prefund,
        tip: i.tip,
        active_from: act.active_from,
        expiry_daa: entry_expiry.daa,
        refund_tip: tips.refund_tip as i64,
        delivery_carrier: carrier,
        exit_carrier,
        min_fill,
        entry_stop: es.stop,
        band_daa: es.band,
        min_touch: es.min_touch,
        min_rest_daa: es.min_rest,
        keeper_tip: es.keeper_tip,
        armed: 0,
        amount_left: amount,
        custody: 0,
        rpt_amount,
        exit_state: exit_state.clone(),
    };
    // Never below what the protocol requires of every exit.
    let needed = draft_with(exit_planned).exit_carrier_needed();
    let exit_carrier = needed.filter(|n| *n > exit_planned).unwrap_or(exit_planned);
    let mut order = draft_with(exit_carrier);
    // The B custody the entry needs: its escrow when buy-first, its prefund when sell-first.
    let Some(b_custody) = order.b_custody_needed() else {
        log.push(error("PAIR_NOTIONAL_TOO_LARGE", "the order value in the quote token is too large"));
        return failed(log);
    };
    order.custody = b_custody;
    let (Some(kas_value), Some(tip_kas_total)) = (order.kas_value(), order.tip_kas(amount)) else {
        log.push(error("TIP_TOO_LARGE", "the priority tip is too large for this quantity"));
        return failed(log);
    };
    let exit_carriers = fills + i64::from(repeat);
    let mut lines = vec![
        CarrierLine { kind: "delivery carrier", amount: carrier as i128, count: fills, kept: false },
        CarrierLine { kind: "exit carrier", amount: exit_carrier as i128, count: exit_carriers, kept: false },
    ];
    if tip_kas_total > 0 {
        lines.push(CarrierLine { kind: "tip prefund", amount: tip_kas_total as i128, count: 1, kept: false });
    }
    if carrier as i128 * fills as i128 + exit_carrier as i128 * exit_carriers as i128 + tip_kas_total as i128 != kas_value as i128 {
        log.push(error("PLAN_MISMATCH", "the entry carriers differ from the protocol's entry value"));
        return failed(log);
    }
    if keeper_reserve > 0 {
        lines.push(CarrierLine { kind: "keeper reserve", amount: es.keeper_tip as i128, count: es.arm_updates, kept: false });
    }
    // The exit that a fill of the whole amount creates. Buy-first it holds n of A. Sell-first it
    // holds, in B, the proceeds plus the prefund of n.
    let exit_custody = if buy_first {
        Some(amount)
    } else {
        order.proceeds(amount, i.price).zip(order.pre_of(amount)).and_then(|(p, q)| p.checked_add(q))
    };
    let Some(exit_custody) = exit_custody else {
        log.push(error("PAIR_NOTIONAL_TOO_LARGE", "the order value in the quote token is too large"));
        return failed(log);
    };
    let exit_order = match order.exit_for(amount, exit_custody, None) {
        Ok(x) => x,
        Err(e) => return failed(vec![error("BUILD_REJECTED", e.to_string())]),
    };
    let n1 = min_fill.min(amount);
    let delivery = if buy_first {
        (false, n1)
    } else {
        (true, order.proceeds(n1, i.price).unwrap_or(0).saturating_add(order.pre_of(n1).unwrap_or(0)))
    };
    let all_in_total = if buy_first { order.spend(amount, i.price) } else { order.proceeds(amount, i.price) };
    let notes = ifd_notes(i, &legs, stop_entry, count);
    place_pair_order(
        env,
        pair,
        log,
        PairSpec {
            order: AnyState::KobIfdPair(order.clone()),
            exit: Some(AnyState::KobCondPair(exit_order)),
            lines,
            deadline: entry_expiry.deadline,
            side: i.side,
            amount,
            min_fill,
            min_touch: stop_entry.then_some(es.min_touch),
            limit_price: Some(i.price),
            all_in_total,
            expected_price: stop_entry.then_some(es.stop),
            worst_price: Some(i.price),
            tip: i.tip,
            refund_tip: order.refund_tip,
            keeper_tip: es.keeper_tip,
            expiry_kind: entry_expiry.kind,
            expiry_daa: entry_expiry.daa,
            activates_at: act.activates_at,
            delivery,
            notes,
            carrier,
        },
    )
}
