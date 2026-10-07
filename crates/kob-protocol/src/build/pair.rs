//! Builders of the pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`; `crate::state::pair`): the order rules a new pair
//! order must pass, and the matcher legs of every pair shape (fills, evidence-armed fills, updates, repeat merges, refunds).
//!
//! Founder rules (option 2): a pair order enforces only its own guarantees, so the builders never require a particular
//! counterparty. A batch may route a pair fill through the KAS books of both tokens (a pair ASK's A sold to `KobBid`s of A
//! and its B bought from `KobAsk`s of B; a pair BID's B sold to `KobBid`s of B and its A bought from `KobAsk`s of A), net
//! any number of opposite pair orders (pair ASKs and pair BIDs of the same pair in one transaction: the A one side releases
//! is what the other receives, and the B likewise), or fill from the matcher's inventory (taker tokens). Every token
//! amount is the covenant's exact rounded amount; a token surplus goes to the delivery of the first pair ASK buying that
//! token (its guarantee is a minimum), else to the batch's taker. Trigger evidence of pair conditionals: two KAS-book legs
//! (one of A, one of B: the implied rate) or a resting `KobPair` leg of the same pair; prices are recorded only from
//! KAS-book fills (indexer rule, `docs/spec/matcher.md`).

use super::*;

/// The base token A and the quote token B of a pair order with their programs: each a supported program of the family
/// its state names (template lengths included), two different tokens.
pub fn pair_programs(s: &AnyState) -> Result<(Token, Token)> {
    s.validate()?;
    let t = s.pair_tokens().ok_or_else(|| Error::Invalid(format!("{} is not a pair order", s.template_id().name())))?;
    let prog = |k: &PairToken, what: &str| -> Result<Token> {
        let p = token_program(&k.tpl_hash, k.prefix_len, k.suffix_len)?;
        if Some(p.family()) != k.family_of() {
            return invalid(format!("pair order: token {what} program {} is not of the family its state names", p.name()));
        }
        Ok((k.cov_id, p))
    };
    let (a, b) = (prog(&t.a, "A")?, prog(&t.b, "B")?);
    if a.0 == b.0 {
        return invalid("a pair order needs two different tokens (A != B)");
    }
    Ok((a, b))
}

/// The role suffix of a pair input: `@<program of A>+<program of B>`.
pub fn pair_suffix(a: Token, b: Token) -> String {
    format!("@{}+{}", a.1.name(), b.1.name())
}

fn side_name(ask: bool) -> &'static str {
    if ask {
        "ask"
    } else {
        "bid"
    }
}

/// The order rules of a new pair order (builders refuse what would be unfillable, unrefundable or refused by the wallet
/// rules; the covenants enforce the rest at fill time).
pub(crate) fn check_new_pair(s: &AnyState) -> Result<()> {
    let (a, b) = pair_programs(s)?;
    s.check_numbers().map_err(Error::Invalid)?;
    check_key(&s.maker(), "order maker")?;
    let floor_of = |program: TemplateId| program.min_token_output().unwrap_or(0).max(crate::tx::DUST_OUTPUT_MIN) as i64;
    let token_carrier = |what: &str, v: i64, program: TemplateId| -> Result<()> {
        let floor = floor_of(program);
        if v < floor {
            return invalid(format!(
                "{what} {v} sompi is below {floor} sompi: the least KAS a token output of {} can carry",
                program.name()
            ));
        }
        Ok(())
    };
    let base = |min_fill: i64, tip: i64, refund_tip: i64| -> Result<()> {
        if min_fill <= 0 {
            return invalid("minFill must be positive (at least one base unit)");
        }
        if tip < 0 || refund_tip < 0 {
            return invalid("tip and refundTip must be >= 0");
        }
        if a.1.family() == Family::Kron {
            kron_output(Family::Kron, min_fill, "minFill")?;
        }
        Ok(())
    };
    let prog_of = |cov: [u8; 32]| if cov == a.0 { a.1 } else { b.1 };
    match s {
        AnyState::KobPair(p) => {
            base(p.min_fill, p.tip, p.refund_tip)?;
            if !(0..=2).contains(&p.tif) || p.price <= 0 || p.amount_left <= 0 || p.custody <= 0 {
                return invalid("pair order: tif 0..=2, price > 0, amountLeft > 0, custody > 0");
            }
            if p.slope < 0 || p.interval < 0 || p.max_fill < 0 {
                return invalid("slope, interval and maxFill must be >= 0");
            }
            if p.max_fill > 0 && p.max_fill < p.min_fill {
                return invalid(format!("maxFill {} is below minFill {}: the order could never fill", p.max_fill, p.min_fill));
            }
            if p.slope > 0 {
                if p.decay_step <= 0 {
                    return invalid("a decaying / rising order needs decayStep > 0");
                }
                if p.is_ask() && !(1..=p.price).contains(&p.price_end) {
                    return invalid("a decaying pair ask needs 0 < priceEnd <= price");
                }
                if !p.is_ask() && p.price_end < p.price {
                    return invalid("a rising pair bid needs priceEnd >= price");
                }
            }
            token_carrier("deliveryCarrier", p.delivery_carrier, prog_of(p.t_cov_id))?;
            if p.is_ask() {
                if p.custody != p.amount_left {
                    return invalid("a pair ask holds exactly its amount (custody == amountLeft)");
                }
            } else {
                let need =
                    need(quote_of(p.amount_left, p.price_max(), p.t_scale, Round::Down), "pair bid: the escrow of the whole amount")?;
                if p.custody < need {
                    return invalid(format!(
                        "a pair bid's escrow of {} cannot pay its whole amount at its highest price ({need}); fund it with PairState::bid_escrow",
                        p.custody
                    ));
                }
            }
            // the T a minimum fill delivers fits one output of T
            let t_min = need(p.t_out_min(p.min_fill.min(p.amount_left), p.price_max()), "pair order: T of a minimum fill")?;
            kron_output(prog_of(p.t_cov_id).family(), t_min, "the T of a minimum fill")?;
            need(p.kas_value(1), "pair order: the KAS of one fill")?;
        }
        AnyState::KobCondPair(c) => check_new_cond_pair(c, &a, &b, false)?,
        AnyState::KobIfdPair(i) => {
            base(i.min_fill, i.tip, i.refund_tip)?;
            if i.entry_stop < 0 || i.band_daa < 0 || i.keeper_tip < 0 || i.rpt_amount < 0 || i.min_touch < 0 {
                return invalid("entryStop, bandDaa, keeperTip, rptAmount and minTouch must be >= 0");
            }
            if i.armed != 0 {
                return invalid("a new entry must not be armed");
            }
            if i.amount_left <= 0 || i.price <= 0 || i.delivery_carrier < 0 || i.exit_carrier < 0 || i.prefund < 0 {
                return invalid("pair entry: amountLeft and price > 0, carriers and prefund >= 0");
            }
            let need_b = need(i.b_custody_needed(), "pair entry: the B custody it needs")?;
            if i.custody < need_b {
                return invalid(format!(
                    "pair entry: its B custody of {} is below the {need_b} it needs ({})",
                    i.custody,
                    if i.is_buy_first() { "the spend of its whole amount" } else { "the prefund of its whole amount, split" }
                ));
            }
            let e = i.exit()?;
            if i.is_buy_first() {
                if i.entry_stop > i.price {
                    return invalid("a buy-stop entry's trigger must not be above its limit");
                }
                // the exit's custody (n of A) carries deliveryCarrier
                token_carrier("deliveryCarrier", i.delivery_carrier, a.1)?;
                if e.side != SIDE_ASK {
                    return invalid("a buy-first entry's exit sells A (side ASK)");
                }
            } else {
                if i.entry_stop > 0 && i.entry_stop < i.price {
                    return invalid("a sell-stop entry's trigger must not be below its limit");
                }
                token_carrier("deliveryCarrier", i.delivery_carrier, b.1)?;
                if e.side != SIDE_BID {
                    return invalid("a sell-first entry's exit buys A back (side BID)");
                }
            }
            if i.exit_carrier < crate::tx::DUST_OUTPUT_MIN as i64 {
                return invalid("exitCarrier is dust (the exit UTXO carries it)");
            }
            if i.rpt_amount > 0 {
                // a merge's new custody (of A or B) carries exitCarrier
                token_carrier("exitCarrier", i.exit_carrier, a.1)?;
                token_carrier("exitCarrier", i.exit_carrier, b.1)?;
            }
            let et = e.tokens();
            let it = i.tokens();
            let same = |x: &PairToken, y: &PairToken| {
                (x.cov_id, x.tpl_hash, x.prefix_len, x.suffix_len, x.family, x.scale)
                    == (y.cov_id, y.tpl_hash, y.prefix_len, y.suffix_len, y.family, y.scale)
            };
            if e.maker != i.maker || !same(&et.a, &it.a) || !same(&et.b, &it.b) {
                return invalid("the exit must be the maker's order of the same pair (tokens, programs, scales)");
            }
            if e.t_ext != if i.is_buy_first() { i.b_ext } else { i.a_ext } {
                return invalid("the exit's deliveries carry the entry's extension commitment of that token");
            }
            if e.custody != 0 || e.amount_left != 0 || e.is_booked() || e.rpt_price != 0 || e.rpt_pre != 0 || e.rpt_until != 0 {
                return invalid("the committed exit carries no amount, custody or repeat fields (the entry writes them)");
            }
            let exit_need = need(i.exit_carrier_needed(), "pair entry: the KAS its exit needs")?;
            if i.exit_carrier < exit_need {
                return invalid(format!(
                    "exitCarrier {} is below the {exit_need} sompi its exit needs (IfdPairState::exit_carrier_needed: the \
                     keeper's arming tip, then its own deliveries and tip, or its refund)",
                    i.exit_carrier
                ));
            }
            // the exit as a fill of one minimum fill creates it
            let n = i.min_fill.min(i.amount_left);
            let x_cust = if i.is_buy_first() {
                n
            } else {
                need(i.proceeds(n, i.price), "pair entry: proceeds")?
                    .checked_add(need(i.pre_of(n), "pair entry: prefund")?)
                    .ok_or_else(|| Error::Invalid("overflow".into()))?
            };
            check_new_cond_pair(&CondPairState { amount_left: n, custody: x_cust, ..e.clone() }, &a, &b, true)?;
            if !i.is_buy_first() && i.price.saturating_add(i.prefund) < e.worst() {
                return invalid("prefund does not cover the exit's worst buy-back (its stop band or limit), per whole A");
            }
            if i.rpt_amount > 0 {
                let beats = if i.is_buy_first() { e.tp_price > i.price } else { e.tp_price > 0 && e.tp_price < i.price };
                if !beats {
                    return invalid("a repeating entry needs a take-profit that beats the entry");
                }
            }
        }
        _ => return invalid("not a pair order"),
    }
    Ok(())
}

fn check_new_cond_pair(c: &CondPairState, a: &Token, b: &Token, exit: bool) -> Result<()> {
    let prog_of = |cov: [u8; 32]| if cov == a.0 { a.1 } else { b.1 };
    if c.min_fill <= 0 {
        return invalid("minFill must be positive (at least one base unit)");
    }
    if c.tip < 0 || c.refund_tip < 0 || c.delivery_carrier < 0 {
        return invalid("tip, refundTip and deliveryCarrier must be >= 0");
    }
    if c.tp_price <= 0 && c.stop_price <= 0 {
        return invalid("a conditional order needs a take-profit or a stop leg");
    }
    if !(0..=10_000).contains(&c.slip_bps) {
        return invalid("slipBps must be within 0..=10000");
    }
    if c.stop_price > MAX_STOP_PRICE {
        return invalid(format!("stopPrice must be at most {MAX_STOP_PRICE} (stopPrice * slipBps must fit in 63 bits)"));
    }
    if c.trail_step < 0 || c.trail_gap < 0 || c.trail_wait < 0 || c.armed != 0 {
        return invalid("trailStep, trailGap, trailWait must be >= 0 and a new order must not be armed");
    }
    if c.band_daa < 0 || c.keeper_tip < 0 || c.min_touch < 0 || c.min_rest_daa < 0 {
        return invalid("bandDaa, keeperTip, minTouch and minRestDaa must be >= 0");
    }
    if c.amount_left <= 0 || c.custody <= 0 {
        return invalid("amountLeft and custody must be positive");
    }
    if !exit && (c.is_booked() || c.rpt_price != 0 || c.rpt_pre != 0 || c.rpt_until != 0) {
        return invalid("repeat fields are written by an if-done entry, never by a new order");
    }
    if c.is_ask() {
        if c.custody != c.amount_left {
            return invalid("a conditional pair ask holds exactly its amount (custody == amountLeft)");
        }
        if c.lowest() <= 0 {
            return invalid("every leg price of a conditional pair ask (the stop band's floor included) must be positive");
        }
    } else {
        let need = need(quote_of(c.amount_left, c.worst(), c.a_scale(), Round::Down), "conditional pair bid: escrow")?;
        if c.custody < need {
            return invalid(format!("a conditional pair bid's escrow of {} cannot pay its worst leg ({need})", c.custody));
        }
    }
    let t_prog = prog_of(c.t_cov_id);
    let floor = t_prog.min_token_output().unwrap_or(0).max(crate::tx::DUST_OUTPUT_MIN) as i64;
    if c.delivery_carrier < floor {
        return invalid(format!(
            "deliveryCarrier {} is below the {floor} sompi a token output of {} carries",
            c.delivery_carrier,
            t_prog.name()
        ));
    }
    if exit && !c.is_ask() {
        // a re-arming sell-first exit pays its profit on the custody's token
        let s_prog = prog_of(c.s_cov_id);
        let floor = s_prog.min_token_output().unwrap_or(0).max(crate::tx::DUST_OUTPUT_MIN) as i64;
        if c.delivery_carrier < floor {
            return invalid(format!("deliveryCarrier is below the {floor} sompi a token output of {} carries", s_prog.name()));
        }
    }
    let worst = if c.is_ask() { c.tp_price.max(c.stop_price) } else { c.worst() };
    let t_min = need(c.t_out_min(c.min_fill.min(c.amount_left), worst.max(1)), "conditional pair: T of a minimum fill")?;
    kron_output(t_prog.family(), t_min, "the T of a minimum fill")?;
    need(c.kas_value(1), "conditional pair: the KAS of one fill")?;
    Ok(())
}

/// Smallest KAS value of a new pair order UTXO: the delivery carriers it funds itself and the tip of its whole amount.
/// An order that can rest after a fill funds a partial fill AND the fill of its rest (`funded_fills`: below that its
/// first partial fill is impossible, or leaves a continuation that can never deliver), a TWAP / DCA order one delivery
/// per `maxFill` slice; an IOC / FOK order, or one whose `minFill` takes everything, one. An entry: a delivery and an
/// exit carrier per possible fill, the tip, a repeating entry's extra exit carrier.
pub(crate) fn min_pair_value(s: &AnyState) -> i64 {
    let sat = |v: Option<i64>| v.unwrap_or(i64::MAX);
    match s {
        AnyState::KobPair(p) => sat(p.kas_value(p.funded_fills())),
        AnyState::KobCondPair(c) => sat(c.kas_value(c.funded_fills())),
        AnyState::KobIfdPair(i) => sat(i.kas_value()),
        _ => 1,
    }
}

// ---------------------------------------------------------------- matcher legs

/// What a pair leg computes before the layout (its amounts; which of its custodies keep an output at their own index).
#[allow(clippy::large_enum_variant)] // one per leg of a batch
pub(crate) enum PairCalc {
    Pair { t_arg: i64, f: PairFill },
    Cond(CondCalc),
    Ifd(IfdCalc),
}

pub(crate) struct CondCalc {
    leg: i64,
    trigger: bool,
    t_arg: i64,
    s_out: i64,
    t_out: i64,
    tip_kas: i64,
    left: i64,
    out_amount: i64,
    /// Re-arming: (proceeds = the entry's budget / proceeds of n, back = a sell-first entry's prefund of n).
    rearm: Option<(i64, i64)>,
    /// The maker's token output: T delivery (`false`) or a re-arming BID exit's B profit on the custody's token (`true`).
    d_out: i64,
    d_s: bool,
}

pub(crate) struct IfdCalc {
    trigger: bool,
    t_arg: i64,
    amt: i64,
    a_new: i64,
    b_new: i64,
    x_cust: i64,
    booked: bool,
    cont: bool,
    tip_kas: i64,
    exit: CondPairState,
}

/// The tokens of a pair leg: (A, B, the custody token S of a KobPair / KobCondPair, or the main custody token of an entry).
pub(crate) fn leg_pair_tokens(l: &Leg) -> Result<Option<(Token, Token)>> {
    Ok(match l {
        Leg::Pair { order, .. } => Some(pair_programs(&AnyState::KobPair(order.state.clone()))?),
        Leg::CondPair { order, .. } => Some(pair_programs(&AnyState::KobCondPair(order.state.clone()))?),
        Leg::IfdPair { order, .. } => Some(pair_programs(&AnyState::KobIfdPair(order.state.clone()))?),
        _ => None,
    })
}

/// The main token of a pair leg: the custody token of a KobPair / KobCondPair (S), an entry's A (sell-first) or B
/// (buy-first).
pub(crate) fn leg_main_token(l: &Leg, a: Token, b: Token) -> Token {
    let s_is_a = match l {
        Leg::Pair { order, .. } => order.state.is_ask(),
        Leg::CondPair { order, .. } => order.state.is_ask(),
        Leg::IfdPair { order, .. } => !order.state.is_buy_first(),
        _ => true,
    };
    if s_is_a {
        a
    } else {
        b
    }
}

/// The amounts of a pair leg (the covenant's arithmetic; refusals of what the covenant refuses).
pub(crate) fn calc_leg(l: &Leg, lock: i64, strict: bool) -> Result<Option<PairCalc>> {
    let r = match l {
        Leg::Pair { order, custody, amount, t } => {
            let s = &order.state;
            let n = *amount;
            let udaa = order.utxo.block_daa_score as i64;
            if strict && lock < s.active_from {
                return invalid(format!("pair order not active before DAA {}", s.active_from));
            }
            if custody.state.amount() != s.custody {
                return invalid(format!(
                    "the pair order's custody holds {}, its state needs exactly {}",
                    custody.state.amount(),
                    s.custody
                ));
            }
            check_custody_ext(custody, s.s_ext)?;
            let t_arg =
                if s.slope != 0 { auction_t(*t, lock, need(s.origin(udaa), "decay origin")?, "decaying pair order")? } else { 0 };
            let p = need(s.price_at(t_arg, udaa), "pair order quote")?;
            let f = s.fill(n, p, order.utxo.amount as i64).map_err(|e| Error::Invalid(format!("pair fill: {e}")))?;
            PairCalc::Pair { t_arg, f }
        }
        Leg::CondPair { order, custody, amount, leg, evidence, t, merge, .. } => {
            let s = &order.state;
            let n = *amount;
            let udaa = order.utxo.block_daa_score as i64;
            if strict && lock < s.active_from {
                return invalid("conditional pair order not active yet");
            }
            if !s.fill_ok(n) {
                return invalid(format!(
                    "a conditional pair fill of {n} needs n <= amountLeft {} and n >= minFill {} unless it takes everything left",
                    s.amount_left, s.min_fill
                ));
            }
            if custody.state.amount() != s.custody {
                return invalid("the conditional pair order's custody does not hold exactly its custody field");
            }
            check_custody_ext(custody, s.s_ext)?;
            let leg = *leg as i64;
            let mut trigger = false;
            let mut t_arg = 0;
            match leg {
                0 if s.tp_price > 0 => {}
                1 if s.stop_ok() => {
                    if s.armed == 0 {
                        if evidence.is_none() {
                            return invalid("an unarmed stop leg needs trigger evidence");
                        }
                        trigger = true;
                    } else if s.band_daa > 0 {
                        let origin = armed_origin(s.armed, udaa).expect("armed");
                        t_arg = auction_t(*t, lock, origin, "stop auction")?;
                    }
                }
                _ => return invalid("invalid conditional leg for this order"),
            }
            if leg == 0 && evidence.is_some() {
                return invalid("a take-profit / limit fill takes no trigger evidence");
            }
            let lp = need(s.leg_price(leg, trigger, t_arg, udaa), "conditional pair leg price")?;
            if lp <= 0 {
                return invalid("the leg price must be positive");
            }
            let mut s_out = need(s.s_out(n, lp), "conditional pair S out")?;
            let mut t_out = need(s.t_out_min(n, lp), "conditional pair T out")?;
            let tip_kas = need(s.tip_kas(n), "conditional pair tip")?;
            if s.tip < 0 || s.delivery_carrier < 0 {
                return invalid("tip and deliveryCarrier must be >= 0");
            }
            let left = s.amount_left - n;
            let mut out_amount = s.custody - s_out;
            let mut rearm = None;
            let mut d_out = t_out;
            let mut d_s = false;
            if s.is_booked() {
                if leg == 0 {
                    if merge.is_none() && lock < s.rpt_until {
                        return invalid("a booked exit takes profit only with its entry's merge until rptUntil");
                    }
                } else if merge.is_some() {
                    return invalid("a stop fill never re-arms its entry");
                }
            } else if merge.is_some() {
                return invalid("only a booked exit's take-profit merges its entry");
            }
            if merge.is_some() {
                if n >= MERGE_SHIFT {
                    return invalid("a booked exit's amount must be below 2^53 (the merge argument)");
                }
                let proceeds = need(s.rpt_proceeds(n), "repeat proceeds")?;
                if s.is_ask() {
                    // the entry's B custody takes the budget; the maker's profit must be positive (deliver one more unit
                    // of B when the rounded minimum leaves nothing)
                    if t_out <= proceeds {
                        t_out = add(proceeds, 1, "repeat delivery")?;
                    }
                    d_out = t_out - proceeds;
                    rearm = Some((proceeds, 0));
                } else {
                    d_s = true;
                    let back = need(s.rpt_back(n), "repeat prefund")?;
                    if left > 0 {
                        // the maker's profit proceeds - sOut must be positive: pay at most proceeds - 1
                        s_out = s_out.min(proceeds - 1);
                        out_amount = s.custody - proceeds - back;
                        d_out = proceeds - s_out;
                    } else {
                        s_out = s_out.min(s.custody - back - 1);
                        out_amount = 0;
                        d_out = s.custody - back - s_out;
                    }
                    if s_out < 0 {
                        return invalid("the re-arming exit's profit would not be positive");
                    }
                    rearm = Some((proceeds, back));
                }
                if d_out <= 0 {
                    return invalid("the re-arming exit's profit must be positive");
                }
            }
            if out_amount < 0 {
                return invalid(format!("the conditional pair order's custody of {} cannot release what this fill needs", s.custody));
            }
            if left > 0 && out_amount == 0 {
                return invalid("a partial conditional pair fill must leave something in the custody");
            }
            if left > 0 && order.utxo.amount < (s.delivery_carrier + tip_kas) as u64 {
                return invalid("the order's KAS do not fund the delivery carrier and the tip of a partial fill");
            }
            PairCalc::Cond(CondCalc { leg, trigger, t_arg, s_out, t_out, tip_kas, left, out_amount, rearm, d_out, d_s })
        }
        Leg::IfdPair { order, amount, evidence, t, .. } => {
            let s = &order.state;
            let n = *amount;
            let udaa = order.utxo.block_daa_score as i64;
            let cov_id = order.utxo.covenant_id.ok_or_else(|| Error::Invalid("entry UTXO has no covenant id".into()))?;
            if strict && lock < s.active_from {
                return invalid("pair entry not active yet");
            }
            if !s.fill_ok(n) {
                return invalid("pair entry fill: n <= amountLeft and n >= minFill unless it takes everything left");
            }
            if s.tip < 0 || s.delivery_carrier < 0 || s.exit_carrier < 0 {
                return invalid("tip and carriers must be >= 0");
            }
            let buy = s.is_buy_first();
            let mut trigger = false;
            let mut t_arg = 0;
            if s.entry_stop > 0 {
                if (buy && s.entry_stop > s.price) || (!buy && s.entry_stop < s.price) {
                    return invalid("a stop entry whose trigger is beyond its limit never fills");
                }
                if s.armed == 0 {
                    if evidence.is_none() {
                        return invalid("an unarmed stop entry needs trigger evidence");
                    }
                    trigger = true;
                } else if s.band_daa > 0 {
                    t_arg = auction_t(*t, lock, armed_origin(s.armed, udaa).expect("armed"), "stop-entry auction")?;
                }
            } else if evidence.is_some() {
                return invalid("a limit entry takes no trigger evidence");
            }
            let booked = s.rpt_amount > n;
            if booked && n >= MERGE_SHIFT {
                return invalid("a booked exit's amount must be below 2^53 (the merge argument)");
            }
            if booked && t_arg == 0 {
                t_arg = t.unwrap_or(lock);
            }
            if t_arg > lock {
                return invalid("time t must be <= lockTime");
            }
            let p = need(s.price_at(trigger, t_arg, udaa), "pair entry price")?;
            if p <= 0 {
                return invalid("the entry quote must be positive");
            }
            let tip_kas = need(s.tip_kas(n), "pair entry tip")?;
            let new_left = s.amount_left - n;
            let cont = new_left > 0 || s.rpt_amount > 0;
            let (amt, a_new, b_new, x_cust) = if buy {
                let amt = need(s.spend(n, p), "pair entry spend")?;
                let b_new = sub(s.custody, amt, "pair entry escrow")?;
                if b_new < 0 {
                    return invalid(format!("the entry's escrow of {} cannot pay {amt}", s.custody));
                }
                (amt, 0, b_new, n)
            } else {
                if s.prefund < 0 {
                    return invalid("prefund must be >= 0");
                }
                let amt = need(s.proceeds(n, p), "pair entry proceeds")?;
                let used = if cont { need(s.pre_of(n), "pair entry prefund")? } else { s.custody };
                if used > s.custody {
                    return invalid(format!(
                        "the entry's prefund custody of {} cannot pay the prefund {used} of this fill",
                        s.custody
                    ));
                }
                (amt, new_left, s.custody - used, add(amt, used, "the exit's custody")?)
            };
            let booking = booked.then_some(Booking { parent: cov_id, until: need(rpt_until(s.expiry_daa, t_arg, udaa), "rptUntil")? });
            let exit = s.exit_for(n, x_cust, booking)?;
            PairCalc::Ifd(IfdCalc { trigger, t_arg, amt, a_new, b_new, x_cust, booked, cont, tip_kas, exit })
        }
        _ => return Ok(None),
    };
    Ok(Some(r))
}

/// The custody inputs of a pair leg: `(slot, utxo, token, positional)`, positional when the transaction keeps an output of
/// that token at the custody's own input index (its rest or return; a merged entry's existing custody).
pub(crate) fn leg_custodies<'a>(l: &'a Leg, calc: &PairCalc, a: Token, b: Token) -> Result<Vec<(Slot, &'a TokenUtxo, Token, bool)>> {
    let mut v = vec![];
    match (l, calc) {
        (Leg::Pair { order, custody, .. }, PairCalc::Pair { f, .. }) => {
            v.push((Slot::Custody, custody, if order.state.is_ask() { a } else { b }, f.out_amount > 0));
        }
        (Leg::CondPair { order, custody, merge, .. }, PairCalc::Cond(c)) => {
            v.push((Slot::Custody, custody, if order.state.is_ask() { a } else { b }, c.out_amount > 0));
            if let Some(m) = merge {
                if let Some(x) = &m.a_custody {
                    v.push((Slot::EntryA, x, a, true));
                }
                if let Some(x) = &m.b_custody {
                    v.push((Slot::EntryB, x, b, true));
                }
            }
        }
        (Leg::IfdPair { a_custody, b_custody, .. }, PairCalc::Ifd(c)) => {
            if let Some(x) = a_custody {
                v.push((Slot::A, x, a, c.a_new > 0));
            }
            if let Some(x) = b_custody {
                v.push((Slot::B, x, b, c.b_new > 0));
            }
        }
        _ => return invalid("pair leg and its amounts disagree"),
    }
    Ok(v)
}

/// The evidence arguments of a pair conditional: the evidence, `(evA, evB, tk, evMode)`. `ask_a`: the A leg must be an
/// ask (mode 0: an ask of A and a bid of B; mode 1: a pair ASK); otherwise a bid of A and an ask of B, or a pair BID.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pair_evidence(
    b: &Batch,
    lay: &Layout,
    ev: Option<usize>,
    ev_b: Option<usize>,
    ask_a: bool,
    a: Token,
    bt: Token,
    sa: i64,
    sb: i64,
    min_touch: i64,
    min_touch_b: Option<i64>,
    min_rest: i64,
    lock: i64,
) -> Result<(PairEvidence, [i64; 4])> {
    let k = ev.ok_or_else(|| Error::Invalid("trigger evidence is required".into()))?;
    let leg = b.legs.get(k).ok_or_else(|| Error::Invalid(format!("trigger evidence {k} is not a leg of the batch")))?;
    match ev_b {
        Some(kb) => {
            // mode 0: a KAS-book leg of A and one of B, filled in this transaction
            let lb = b.legs.get(kb).ok_or_else(|| Error::Invalid(format!("trigger evidence {kb} is not a leg of the batch")))?;
            // a leg that cannot be evidence at all is that leg's refusal, not the reader's
            let ta = touch_of(leg).map_err(|e| blame("evidence", k, e))?;
            let tb = touch_of(lb).map_err(|e| blame("evidence", kb, e))?;
            let want_a = if ask_a { SIDE_ASK } else { SIDE_BID };
            if ta.side != want_a || tb.side == want_a {
                return invalid(if ask_a {
                    "this trigger reads a resting ask of A and a resting bid of B (the rate fell)"
                } else {
                    "this trigger reads a resting bid of A and a resting ask of B (the rate rose)"
                });
            }
            ta.check(a.0, sa, min_touch, min_rest, lock)?;
            let mb = min_touch_b.ok_or_else(|| Error::Invalid("the B threshold of the trigger overflows".into()))?;
            tb.check(bt.0, sb, mb, min_rest, lock)?;
            if tb.price <= 0 {
                return invalid("the B quote of the evidence must be positive");
            }
            let tk = if ask_a { lay.at[&(k, Slot::Custody)] } else { lay.at[&(kb, Slot::Custody)] };
            Ok((PairEvidence::KasBooks { a: ta.price, b: tb.price }, [k as i64, kb as i64, tk as i64, 0]))
        }
        None => {
            // mode 1: a resting KobPair of this pair, filled in this transaction
            let Leg::Pair { order, custody, amount, .. } = leg else {
                return Err(blame(
                    "evidence",
                    k,
                    Error::Invalid("pair-order evidence is a KobPair leg of the batch (or name a KAS-book leg of each token)".into()),
                ));
            };
            let s = &order.state;
            if s.is_ask() != ask_a {
                return invalid(if ask_a { "this trigger reads a resting pair ASK" } else { "this trigger reads a resting pair BID" });
            }
            let t = s.tokens();
            if (t.a.cov_id, t.b.cov_id, t.a.scale, t.b.scale) != (a.0, bt.0, sa, sb) {
                return invalid("pair-order evidence must be an order of the same pair at the same scales");
            }
            if s.slope != 0 {
                return Err(blame("evidence", k, Error::Invalid("a decaying pair order is never trigger evidence".into())));
            }
            if *amount < min_touch {
                return invalid("trigger evidence below the order's minTouch");
            }
            let exposed = (order.utxo.block_daa_score as i64 + s.interval).max(s.active_from).max(custody.utxo.block_daa_score as i64);
            if exposed.saturating_add(min_rest) > lock {
                return invalid("trigger evidence was exposed for less than minRestDaa before the lock time");
            }
            Ok((PairEvidence::Pair { price: s.price }, [k as i64, 0, lay.at[&(k, Slot::Custody)] as i64, 1]))
        }
    }
}

/// Whether the evidence's A leg is an ask (mode 0: the first leg is a KobAsk; mode 1: the pair leg sells A).
fn evidence_ask_a(b: &Batch, ev: usize, ev_b: Option<usize>) -> Result<bool> {
    let l = b.legs.get(ev).ok_or_else(|| Error::Invalid(format!("trigger evidence {ev} is not a leg of the batch")))?;
    Ok(match (l, ev_b) {
        (Leg::Ask { .. }, Some(_)) => true,
        (Leg::Bid { .. }, Some(_)) => false,
        (Leg::Pair { order, .. }, None) => order.state.is_ask(),
        _ => return invalid("trigger evidence: a KAS-book leg of A and one of B, or one KobPair leg"),
    })
}

/// First input of a token in the batch (a template source), or an error.
fn first_in(lay: &Layout, t: Token, what: &str) -> Result<i64> {
    lay.first_token_in
        .get(&t.0)
        .map(|x| *x as i64)
        .ok_or_else(|| Error::Invalid(format!("{what}: no input of {} in the batch", t.1.name())))
}

/// The plan of a pair leg (its input, positional output, extras, token flows).
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_leg(
    b: &Batch,
    lay: &Layout,
    i: usize,
    l: &Leg,
    calc: &PairCalc,
    a: Token,
    bt: Token,
    lock: i64,
) -> Result<LegPlan> {
    let suffix = pair_suffix(a, bt);
    match (l, calc) {
        (Leg::Pair { order, custody, amount, .. }, PairCalc::Pair { t_arg, f, .. }) => {
            let s = &order.state;
            let n = *amount;
            let ask = s.is_ask();
            let (stok, ttok) = if ask { (a, bt) } else { (bt, a) };
            let v = order.utxo.amount as i64;
            let cv = custody.utxo.amount as i64;
            let maker_ext = if ttok.1.family() == Family::Kcc20 { s.t_ext } else { [0; 32] };
            let mut extras = vec![];
            let (delivery, branch) = if f.rest {
                extras.push(Extra::Cont(
                    kas_out(v - s.delivery_carrier - f.tip_kas, "pair continuation")?,
                    PairState { amount_left: s.amount_left - n, custody: f.out_amount, ..s.clone() }.spk(),
                ));
                let st = custody.state.with_amount(f.out_amount);
                extras.push(Extra::AtInput { slot: Slot::Custody, value: custody.utxo.amount, state: st, token: stok, patch: None });
                (s.delivery_carrier, "rest")
            } else if f.out_amount > 0 {
                let st = custody.state.with_amount(f.out_amount).with_user_owner(s.maker);
                extras.push(Extra::AtInput { slot: Slot::Custody, value: custody.utxo.amount, state: st, token: stok, patch: None });
                (v - f.tip_kas, "return")
            } else {
                (v + cv - f.tip_kas, "close")
            };
            let mut role = format!("KobPair.settle.{}.{branch}", side_name(ask));
            if s.interval > 0 {
                role.push_str(".twap");
            }
            if s.slope != 0 {
                role.push_str(".decay");
            }
            let t_tpl = first_in(lay, ttok, "pair order (T template source)")?;
            Ok(LegPlan {
                suffix,
                pos: PosOut::Deliver(
                    kas_out(delivery, "pair delivery carrier")?,
                    TokenState::user(ttok.1.family(), f.t_out, s.maker, maker_ext),
                    ttok,
                ),
                role,
                sequence: s.interval.max(0) as u64,
                plan: entry(
                    TemplateId::KobPair,
                    s.encode(),
                    "settle",
                    vec![
                        nb(n),
                        Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                        Arg::Int(t_tpl),
                        Arg::Int(*t_arg),
                        Arg::Int(f.s_out),
                        Arg::Int(f.t_out),
                    ],
                ),
                extras,
                flows: vec![(stok, f.s_out, 0), (ttok, 0, f.t_out)],
                merge: None,
                adjust: if ask { Some((ttok, 5)) } else { None },
            })
        }
        (Leg::CondPair { order, custody, amount, evidence, evidence_b, merge, .. }, PairCalc::Cond(c)) => {
            let s = &order.state;
            let n = *amount;
            let ask = s.is_ask();
            let (stok, ttok) = if ask { (a, bt) } else { (bt, a) };
            let udaa = order.utxo.block_daa_score as i64;
            let v = order.utxo.amount as i64;
            let cv = custody.utxo.amount as i64;
            let mut ev_args = [0i64; 4];
            if c.trigger {
                let (ev, args) = pair_evidence(
                    b,
                    lay,
                    *evidence,
                    *evidence_b,
                    ask,
                    a,
                    bt,
                    s.a_scale(),
                    s.b_scale(),
                    s.min_touch,
                    s.min_touch_b(),
                    s.min_rest_daa,
                    lock,
                )?;
                if s.arms(ev) != Some(true) {
                    return invalid(if ask {
                        "a pair stop sell arms only when the rate fell to its stop or below"
                    } else {
                        "a pair buy stop arms only when the rate rose to its stop or above"
                    });
                }
                ev_args = args;
            }
            let mut extras = vec![];
            if c.left > 0 {
                let next = CondPairState {
                    amount_left: c.left,
                    custody: c.out_amount,
                    armed: s.next_armed(c.leg, c.trigger, udaa),
                    ..s.clone()
                };
                extras.push(Extra::Cont(kas_out(v - s.delivery_carrier - c.tip_kas, "conditional pair continuation")?, next.spk()));
                extras.push(Extra::AtInput {
                    slot: Slot::Custody,
                    value: custody.utxo.amount,
                    state: custody.state.with_amount(c.out_amount),
                    token: stok,
                    patch: None,
                });
            } else if c.rearm.is_none() && c.out_amount > 0 {
                let st = custody.state.with_amount(c.out_amount).with_user_owner(s.maker);
                extras.push(Extra::AtInput { slot: Slot::Custody, value: custody.utxo.amount, state: st, token: stok, patch: None });
            }
            let pos_value = if c.left > 0 || c.rearm.is_some() {
                s.delivery_carrier
            } else if c.out_amount > 0 {
                v - c.tip_kas
            } else {
                v + cv - c.tip_kas
            };
            // the maker's token at output i: the T delivery, or a re-arming BID exit's B profit (custody's template)
            let pos_out = if c.d_s {
                PosOut::Deliver(
                    kas_out(pos_value, "profit carrier")?,
                    custody.state.with_amount(c.d_out).with_user_owner(s.maker),
                    stok,
                )
            } else {
                let e = if ttok.1.family() == Family::Kcc20 { s.t_ext } else { [0; 32] };
                PosOut::Deliver(kas_out(pos_value, "delivery carrier")?, TokenState::user(ttok.1.family(), c.d_out, s.maker, e), ttok)
            };
            let mut flows = vec![(stok, c.s_out, 0), (ttok, 0, c.t_out)];
            let merge_plan = match (merge, c.rearm) {
                (None, _) => None,
                (Some(m), Some((proceeds, back))) => {
                    let e = &m.entry;
                    let es = &e.state;
                    if e.utxo.covenant_id != Some(s.parent) {
                        return invalid("the merged entry is not this exit's parent");
                    }
                    if !es.books_exit(s.parent, s) {
                        return invalid("the exit is not one the merged entry books (its terms or repeat fields differ)");
                    }
                    let edaa = e.utxo.block_daa_score as i64;
                    let ev = e.utxo.amount as i64;
                    let buy = es.is_buy_first();
                    let (a_new, b_new) = if buy {
                        let budget = need(es.merge_budget(n), "merge budget")?;
                        if budget != proceeds || budget <= 0 {
                            return invalid("the exit's re-arm budget is not the entry's");
                        }
                        (0, add(es.custody, budget, "merged escrow")?)
                    } else {
                        let pre = need(es.pre_of(n), "merge prefund")?;
                        if pre != back {
                            return invalid("the exit's prefund return is not the entry's");
                        }
                        (add(es.amount_left, n, "merged amount")?, add(es.custody, pre, "merged prefund")?)
                    };
                    let sellout = s.amount_left == n;
                    let mut floor = ev;
                    if sellout {
                        let all = ev + v + cv - c.tip_kas - s.delivery_carrier;
                        floor = floor.max(all);
                    }
                    let entry_in = lay.merge_in[&i];
                    let mut new_custody = false;
                    // the entry's custodies after the merge: an existing one at its index, a new one with exitCarrier
                    let (ea_in, eb_in) = (lay.at.get(&(i, Slot::EntryA)).copied(), lay.at.get(&(i, Slot::EntryB)).copied());
                    if a_new > 0 {
                        kron_output(a.1.family(), a_new, "the merged entry's A custody")?;
                        match &m.a_custody {
                            Some(x) => {
                                check_custody(x, e.utxo.covenant_id, a, es.amount_left).map_err(|e| blame("entry", i, e))?;
                                check_custody_ext(x, es.a_ext).map_err(|e| blame("entry", i, e))?;
                                extras.push(Extra::AtInput {
                                    slot: Slot::EntryA,
                                    value: x.utxo.amount,
                                    state: x.state.with_amount(a_new),
                                    token: a,
                                    patch: None,
                                });
                            }
                            None => {
                                if es.amount_left > 0 {
                                    return invalid("the merged entry's A custody is required");
                                }
                                new_custody = true;
                                floor -= es.exit_carrier;
                                let st = TokenState::custody(
                                    a.1.family(),
                                    a_new,
                                    s.parent,
                                    if a.1.family() == Family::Kcc20 { es.a_ext } else { [0; 32] },
                                );
                                extras.push(Extra::Tok {
                                    value: u(es.exit_carrier, "exitCarrier")?,
                                    state: st,
                                    token: a,
                                    patch: Some((entry_in, 14)),
                                });
                            }
                        }
                    } else if m.a_custody.is_some() {
                        return invalid("a buy-first entry holds no A custody");
                    }
                    if b_new > 0 {
                        kron_output(bt.1.family(), b_new, "the merged entry's B custody")?;
                        match &m.b_custody {
                            Some(x) => {
                                check_custody(x, e.utxo.covenant_id, bt, es.custody).map_err(|e| blame("entry", i, e))?;
                                check_custody_ext(x, es.b_ext).map_err(|e| blame("entry", i, e))?;
                                extras.push(Extra::AtInput {
                                    slot: Slot::EntryB,
                                    value: x.utxo.amount,
                                    state: x.state.with_amount(b_new),
                                    token: bt,
                                    patch: None,
                                });
                            }
                            None => {
                                if es.custody > 0 {
                                    return invalid("the merged entry's B custody is required");
                                }
                                new_custody = true;
                                floor -= es.exit_carrier;
                                let st = TokenState::custody(
                                    bt.1.family(),
                                    b_new,
                                    s.parent,
                                    if bt.1.family() == Family::Kcc20 { es.b_ext } else { [0; 32] },
                                );
                                extras.push(Extra::Tok {
                                    value: u(es.exit_carrier, "exitCarrier")?,
                                    state: st,
                                    token: bt,
                                    patch: Some((entry_in, 15)),
                                });
                            }
                        }
                    }
                    // the exit's own flows (ASK exit: n of A released, the maker's B profit; BID exit: the B its custody
                    // releases, the maker's B profit), then the entry's custodies (old amounts in, new amounts out)
                    flows = if buy {
                        vec![(a, n, 0), (bt, 0, c.d_out)]
                    } else {
                        vec![(bt, s.custody - c.out_amount, 0), (bt, 0, c.d_out)]
                    };
                    if let Some(x) = &m.a_custody {
                        flows.push((a, x.state.amount(), 0));
                    }
                    if a_new > 0 {
                        flows.push((a, 0, a_new));
                    }
                    if let Some(x) = &m.b_custody {
                        flows.push((bt, x.state.amount(), 0));
                    }
                    if b_new > 0 {
                        flows.push((bt, 0, b_new));
                    }
                    let cont = IfdPairState {
                        amount_left: add(es.amount_left, n, "merged amount")?,
                        custody: b_new,
                        armed: es.merged_armed(edaa),
                        ..es.clone()
                    };
                    extras.push(Extra::EntryCont(u(floor, "merged entry")?, cont.spk(), s.parent));
                    let xc = lay.at[&(i, Slot::Custody)] as i64;
                    let args = vec![
                        Arg::Bytes(merge_arg(i, n)?),
                        Arg::Int(ea_in.map(|x| x as i64).unwrap_or(0)),
                        Arg::Int(eb_in.map(|x| x as i64).unwrap_or(0)),
                        Arg::Int(if a_new > 0 { first_in(lay, a, "merge (A template)")? } else { 0 }),
                        Arg::Int(if b_new > 0 { first_in(lay, bt, "merge (B template)")? } else { 0 }),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Bytes(vec![]),
                        Arg::Bytes(vec![]),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(xc),
                        Arg::Int(0),
                    ];
                    let plan = entry(TemplateId::KobIfdPair, es.encode(), "fill", args);
                    let role = format!(
                        "KobIfdPair.fill.merge.{}{}{}",
                        side_name(!buy),
                        if new_custody { ".new" } else { "" },
                        if sellout { ".sellout" } else { "" }
                    );
                    Some((plan, role, e.utxo.clone()))
                }
                (Some(_), None) => unreachable!("a merge is a re-arm"),
            };
            let branch = if c.left > 0 {
                "rest"
            } else if c.rearm.is_none() && c.out_amount > 0 {
                "return"
            } else {
                "close"
            };
            let role = format!(
                "KobCondPair.settle.{}.leg{}{}{}{}.{branch}",
                side_name(ask),
                c.leg,
                if c.trigger {
                    if ev_args[3] == 0 {
                        ".arm0"
                    } else {
                        ".arm1"
                    }
                } else {
                    ""
                },
                if c.t_arg != 0 { ".auction" } else { "" },
                if merge.is_some() { ".merge" } else { "" },
            );
            let t_tpl = if c.d_s {
                lay.first_token_in.get(&ttok.0).map(|x| *x as i64).unwrap_or(0)
            } else {
                first_in(lay, ttok, "conditional pair (T template source)")?
            };
            Ok(LegPlan {
                suffix,
                pos: pos_out,
                role,
                sequence: 0,
                plan: entry(
                    TemplateId::KobCondPair,
                    s.encode(),
                    "settle",
                    vec![
                        nb(n),
                        Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                        Arg::Int(t_tpl),
                        Arg::Int(c.t_arg),
                        Arg::Int(c.s_out),
                        Arg::Int(c.t_out),
                        Arg::Int(c.leg),
                        Arg::Int(ev_args[0]),
                        Arg::Int(ev_args[1]),
                        Arg::Int(ev_args[2]),
                        Arg::Int(ev_args[3]),
                        Arg::Int(0),
                        Arg::Int(0),
                    ],
                ),
                extras,
                flows,
                merge: merge_plan,
                adjust: if ask { Some((ttok, 5)) } else { None },
            })
        }
        (Leg::IfdPair { order, a_custody, b_custody, amount, evidence, evidence_b, .. }, PairCalc::Ifd(c)) => {
            let s = &order.state;
            let n = *amount;
            let buy = s.is_buy_first();
            let v = order.utxo.amount as i64;
            // the custodies: exactly what the entry holds
            let a_held = if buy { 0 } else { s.amount_left };
            match (a_custody, a_held > 0) {
                (Some(x), true) => {
                    check_custody(x, order.utxo.covenant_id, a, a_held)?;
                    check_custody_ext(x, s.a_ext)?;
                }
                (None, false) => {}
                (Some(_), false) => return invalid("this entry holds no A custody"),
                (None, true) => return invalid("a sell-first entry's A custody is required"),
            }
            match (b_custody, s.custody > 0) {
                (Some(x), true) => {
                    check_custody(x, order.utxo.covenant_id, bt, s.custody)?;
                    check_custody_ext(x, s.b_ext)?;
                }
                (None, false) => {}
                (Some(_), false) => return invalid("this entry holds no B custody"),
                (None, true) => return invalid("the entry's B custody is required"),
            }
            let a_car = a_custody.as_ref().map(|x| x.utxo.amount as i64).unwrap_or(0);
            let b_car = b_custody.as_ref().map(|x| x.utxo.amount as i64).unwrap_or(0);
            let mut ev_args = [0i64; 4];
            if c.trigger {
                let (ev, args) = pair_evidence(
                    b,
                    lay,
                    *evidence,
                    *evidence_b,
                    !buy,
                    a,
                    bt,
                    s.a_scale,
                    s.b_scale,
                    s.min_touch,
                    s.min_touch_b(),
                    s.min_rest_daa,
                    lock,
                )?;
                if s.arms(ev) != Some(true) {
                    return invalid(if buy {
                        "a pair buy-stop entry arms only when the rate rose to its trigger or above"
                    } else {
                        "a pair sell-stop entry arms only when the rate fell to its trigger or below"
                    });
                }
                ev_args = args;
            }
            let mut extras = vec![];
            let base = v - s.delivery_carrier - s.exit_carrier - c.tip_kas;
            if c.cont {
                let mut keep = base;
                if a_held > 0 {
                    if c.a_new > 0 {
                        let x = a_custody.as_ref().expect("checked");
                        extras.push(Extra::AtInput {
                            slot: Slot::A,
                            value: x.utxo.amount,
                            state: x.state.with_amount(c.a_new),
                            token: a,
                            patch: None,
                        });
                    } else {
                        keep += a_car;
                    }
                }
                if s.custody > 0 {
                    if c.b_new > 0 {
                        let x = b_custody.as_ref().expect("checked");
                        extras.push(Extra::AtInput {
                            slot: Slot::B,
                            value: x.utxo.amount,
                            state: x.state.with_amount(c.b_new),
                            token: bt,
                            patch: None,
                        });
                    } else {
                        keep += b_car;
                    }
                }
                let udaa = order.utxo.block_daa_score as i64;
                let next = IfdPairState {
                    amount_left: s.amount_left - n,
                    custody: c.b_new,
                    armed: s.next_armed(udaa),
                    rpt_amount: if c.booked { s.rpt_amount - n } else { s.rpt_amount },
                    ..s.clone()
                };
                extras.push(Extra::Cont(kas_out(keep, "entry continuation")?, next.spk()));
                extras.push(Extra::Exit {
                    value: u(s.exit_carrier, "exit carrier")?,
                    spk: c.exit.spk(),
                    tpl: TemplateId::KobCondPair,
                    arg: 5,
                });
            } else {
                let mut left = v + a_car - s.delivery_carrier - c.tip_kas;
                if c.b_new > 0 {
                    let x = b_custody.as_ref().expect("checked");
                    let st = x.state.with_amount(c.b_new).with_user_owner(s.maker);
                    extras.push(Extra::AtInput { slot: Slot::B, value: x.utxo.amount, state: st, token: bt, patch: None });
                } else {
                    left += b_car;
                }
                extras.push(Extra::Exit {
                    value: kas_out(left, "exit value")? as u64,
                    spk: c.exit.spk(),
                    tpl: TemplateId::KobCondPair,
                    arg: 5,
                });
            }
            let (xtok, x_ext) = if buy { (a, s.a_ext) } else { (bt, s.b_ext) };
            let x_ext = if xtok.1.family() == Family::Kcc20 { x_ext } else { [0; 32] };
            kron_output(xtok.1.family(), c.x_cust, "the exit's custody")?;
            let c_tpl = template(TemplateId::KobCondPair);
            let a_in = lay.at.get(&(i, Slot::A)).map(|x| *x as i64).unwrap_or(0);
            let b_in = lay.at.get(&(i, Slot::B)).map(|x| *x as i64).unwrap_or(0);
            let a_tpl = if buy || c.a_new > 0 { first_in(lay, a, "pair entry (A template)")? } else { 0 };
            let b_tpl = if !buy || c.b_new > 0 { first_in(lay, bt, "pair entry (B template)")? } else { 0 };
            let role = format!(
                "KobIfdPair.fill.{}{}{}{}.{}",
                side_name(!buy),
                if c.trigger {
                    if ev_args[3] == 0 {
                        ".arm0"
                    } else {
                        ".arm1"
                    }
                } else {
                    ""
                },
                if s.entry_stop > 0 && s.armed != 0 && s.band_daa > 0 { ".auction" } else { "" },
                if c.booked { ".book" } else { "" },
                if s.amount_left - n > 0 {
                    "cont"
                } else if c.cont {
                    "wait"
                } else {
                    "close"
                }
            );
            let flows =
                if buy { vec![(bt, c.amt, 0), (a, 0, n)] } else { vec![(a, n, 0), (bt, s.custody - c.b_new, 0), (bt, 0, c.x_cust)] };
            Ok(LegPlan {
                suffix,
                pos: PosOut::DeliverToExit {
                    value: u(s.delivery_carrier, "delivery carrier")?,
                    amount: c.x_cust,
                    ext: x_ext,
                    token: xtok,
                },
                role,
                sequence: 0,
                plan: entry(
                    TemplateId::KobIfdPair,
                    s.encode(),
                    "fill",
                    vec![
                        nb(n),
                        Arg::Int(a_in),
                        Arg::Int(b_in),
                        Arg::Int(a_tpl),
                        Arg::Int(b_tpl),
                        Arg::Int(0),
                        Arg::Int(c.amt),
                        Arg::Bytes(c_tpl.prefix.clone()),
                        Arg::Bytes(c_tpl.suffix.clone()),
                        Arg::Int(ev_args[0]),
                        Arg::Int(ev_args[1]),
                        Arg::Int(ev_args[2]),
                        Arg::Int(ev_args[3]),
                        Arg::Int(c.t_arg),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                        Arg::Int(0),
                    ],
                ),
                extras,
                flows,
                merge: None,
                adjust: None,
            })
        }
        _ => invalid("pair leg and its amounts disagree"),
    }
}

/// Plans one [`BatchUpdate`] of a pair conditional or a pair stop entry (`settle` / `fill` with `nb = 0`, `upd = 1`):
/// arm (k = 0) or trail (k >= 1, the maximal k the evidence justifies). Evidence: `evidence` and `evidenceB` (mode 0:
/// a KAS-book leg of A and one of B) or `evidence` alone (mode 1: a KobPair leg of the pair).
pub(crate) fn update_plan_pair(
    b: &Batch,
    lay: &Layout,
    u: &BatchUpdate,
    spent: &BTreeSet<[u8; 32]>,
) -> Result<(SigPlan, String, u64, AnyState, i64)> {
    let o = &u.order;
    let lock = b.lock_time as i64;
    let (a, bt) = pair_programs(&o.state)?;
    let suffix = pair_suffix(a, bt);
    let ask_a = evidence_ask_a(b, u.evidence, u.evidence_b)?;
    let (plan, role, sequence, next) = match &o.state {
        AnyState::KobCondPair(s) => {
            if s.armed != 0 || s.stop_price <= 0 {
                return invalid("only an unarmed stop order can be armed or trailed");
            }
            if s.parent != [0; 32] && spent.contains(&s.parent) {
                return invalid("a booked exit is never armed or trailed in a transaction that also spends its repeat entry");
            }
            if lock < s.active_from {
                return invalid("the order is not active yet");
            }
            let arm = ask_a == s.is_ask();
            let (ev, args) = pair_evidence(
                b,
                lay,
                Some(u.evidence),
                u.evidence_b,
                ask_a,
                a,
                bt,
                s.a_scale(),
                s.b_scale(),
                s.min_touch,
                s.min_touch_b(),
                s.min_rest_daa,
                lock,
            )?;
            let (next, k, what, seq) = if arm {
                if s.arms(ev) != Some(true) {
                    return invalid("the evidence does not reach the stop");
                }
                (CondPairState { armed: 1, ..s.clone() }, 0, "arm", 0)
            } else {
                let k = s.trail_k(ev).ok_or_else(|| Error::Invalid("the evidence justifies no trailing step".into()))?;
                let stop = if s.is_ask() { s.stop_price + k * s.trail_step } else { s.stop_price - k * s.trail_step };
                (CondPairState { stop_price: stop, ..s.clone() }, k, "trail", s.trail_wait.max(0) as u64)
            };
            let mut argv = vec![nb(0)];
            argv.extend([0i64, 0, 0, 0, 0, 0].map(Arg::Int));
            argv.extend(args.map(Arg::Int));
            argv.extend([Arg::Int(1), Arg::Int(k)]);
            let role =
                format!("KobCondPair.update.{}.{what}{}{suffix}", side_name(s.is_ask()), if args[3] == 0 { ".ev0" } else { ".ev1" });
            (entry(TemplateId::KobCondPair, s.encode(), "settle", argv), role, seq, AnyState::KobCondPair(next))
        }
        AnyState::KobIfdPair(s) => {
            if s.entry_stop <= 0 || s.armed != 0 {
                return invalid("only an unarmed stop entry can be armed");
            }
            let buy = s.is_buy_first();
            arm_entry_rules(s.amount_left, if buy { s.entry_stop > s.price } else { s.entry_stop < s.price })?;
            if lock < s.active_from {
                return invalid("the order is not active yet");
            }
            if ask_a == buy {
                return invalid(if buy {
                    "a buy-stop entry is armed by a bid of A and an ask of B, or a pair BID"
                } else {
                    "a sell-stop entry is armed by an ask of A and a bid of B, or a pair ASK"
                });
            }
            let (ev, args) = pair_evidence(
                b,
                lay,
                Some(u.evidence),
                u.evidence_b,
                !buy,
                a,
                bt,
                s.a_scale,
                s.b_scale,
                s.min_touch,
                s.min_touch_b(),
                s.min_rest_daa,
                lock,
            )?;
            if s.arms(ev) != Some(true) {
                return invalid("the evidence does not reach the entry's trigger");
            }
            let argv = vec![
                nb(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Bytes(vec![]),
                Arg::Bytes(vec![]),
                Arg::Int(args[0]),
                Arg::Int(args[1]),
                Arg::Int(args[2]),
                Arg::Int(args[3]),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(1),
            ];
            let role = format!("KobIfdPair.update.{}.arm{}{suffix}", side_name(!buy), if args[3] == 0 { ".ev0" } else { ".ev1" });
            (
                entry(TemplateId::KobIfdPair, s.encode(), "fill", argv),
                role,
                0,
                AnyState::KobIfdPair(IfdPairState { armed: 1, ..s.clone() }),
            )
        }
        _ => return invalid("only pair conditionals and pair stop entries are updated with pair evidence"),
    };
    let max_take = o.state.keeper_tip().expect("updatable kind");
    let take = u.take.unwrap_or(max_take);
    if !(0..=max_take).contains(&take) {
        return invalid(format!("a keeper takes at most keeperTip = {max_take}"));
    }
    Ok((plan, role, sequence, next, take))
}

/// The refund of a pair order (anyone, after its expiry, 90 days idle or an IOC / FOK kill): the signing plan, the role
/// and the custodies it returns `(input slot k, token)`.
pub(crate) fn refund_plan(s: &AnyState, kill: bool) -> Result<(SigPlan, String)> {
    let (a, b) = pair_programs(s)?;
    let suffix = pair_suffix(a, b);
    let st = s.encode();
    Ok(match s {
        AnyState::KobPair(p) => (
            entry(TemplateId::KobPair, st, "settle", vec![nb(0), Arg::Int(1), Arg::Int(0), Arg::Int(0), Arg::Int(0), Arg::Int(0)]),
            format!("KobPair.refund{}.{}{suffix}", if kill { ".kill" } else { "" }, side_name(p.is_ask())),
        ),
        AnyState::KobCondPair(c) => {
            let mut args = vec![nb(0), Arg::Int(1)];
            args.extend([0i64; 11].map(Arg::Int));
            (entry(TemplateId::KobCondPair, st, "settle", args), format!("KobCondPair.refund.{}{suffix}", side_name(c.is_ask())))
        }
        AnyState::KobIfdPair(i) => {
            // custodies at inputs 1 (and 2): the A custody first (sell-first), then the B custody
            let held = s.custodies();
            let a_in = held.iter().position(|c| c.0 == i.a_cov_id).map(|k| k as i64 + 1).unwrap_or(0);
            let b_in = held.iter().position(|c| c.0 == i.b_cov_id).map(|k| k as i64 + 1).unwrap_or(0);
            let args = vec![
                nb(0),
                Arg::Int(a_in),
                Arg::Int(b_in),
                Arg::Int(a_in),
                Arg::Int(b_in),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Bytes(vec![]),
                Arg::Bytes(vec![]),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
            ];
            (
                entry(TemplateId::KobIfdPair, st, "fill", args),
                format!("KobIfdPair.refund.{}{}{suffix}", side_name(!i.is_buy_first()), if held.len() == 2 { ".two" } else { "" }),
            )
        }
        _ => return invalid("not a pair order"),
    })
}
