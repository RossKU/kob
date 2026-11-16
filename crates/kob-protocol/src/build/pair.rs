//! Builders of the pair orders `KobPair`, `KobCondPair` and `KobIfdPair` (`crate::state::pair`).
//!
//! The module holds the order rules a new pair order must pass, and the matcher legs of every pair
//! shape: fills, evidence-armed fills, updates, repeat merges and refunds.
//!
//! # Counterparties
//!
//! A pair order enforces only its own guarantees, so the builders never require a particular
//! counterparty. See `docs/spec/order-types.md`, "Pair orders". A batch may fill a pair order in
//! three ways:
//! - Route it through the KAS books of both tokens. A pair ASK's A is sold to `KobBid`s of A and
//!   its B bought from `KobAsk`s of B. A pair BID's B is sold to `KobBid`s of B and its A bought
//!   from `KobAsk`s of A.
//! - Net any number of opposite pair orders, i.e. pair ASKs and pair BIDs of the same pair in one
//!   transaction. The A that one side releases is what the other receives, and likewise for B.
//! - Fill from the matcher's inventory (taker tokens).
//!
//! Every token amount is the covenant's exact rounded amount. A token surplus goes to the delivery
//! of the first pair ASK buying that token, whose guarantee is a minimum, and else to the batch's
//! taker.
//!
//! # Trigger evidence
//!
//! Pair conditionals arm from two KAS-book legs, one of A and one of B, which give the implied
//! rate, or from a resting `KobPair` leg of the same pair. Prices are recorded only from KAS-book
//! fills, by the indexer rule of `docs/spec/matcher.md`.

use super::*;

/// The base token A and the quote token B of a pair order, with their programs.
///
/// Each is a supported program of the family its state names, template lengths included, and the
/// two tokens differ.
pub fn pair_programs(s: &AnyState) -> Result<(Token, Token)> {
    s.validate()?;
    let t = s.pair_tokens().ok_or_else(|| Error::Invalid(format!("{} is not a pair order", s.template_id().name())))?;
    let prog = |k: &PairToken, what: &str| -> Result<Token> {
        let p = token_program(&k.tpl_hash, k.prefix_len, k.suffix_len)?;
        if Some(p.family()) != k.family_of() {
            return invalid(format!("pair order: token {what} program {} is not of the family its state names", p.name()));
        }
        // The pair templates deliver a KRON token with id_type 3 (address presence,
        // `KRON_DELIVERY`). A program whose holders are another owner type, e.g. KForge with
        // id_type 0, has no pair template, because its maker could never spend such a delivery.
        if !p.serves_pairs() {
            return invalid(format!(
                "pair order: token {what} program {} holds tokens with id_type {}; the pair templates deliver KRON tokens with \
                 id_type 3 only (no pair template serves it)",
                p.name(),
                crate::state::HolderOf::Program(p).owner_type()
            ));
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

/// The order rules of a new pair order.
///
/// The builders refuse what would be unfillable, unrefundable or refused by the wallet rules. The
/// covenants enforce the rest at fill time.
pub(crate) fn check_new_pair(s: &AnyState) -> Result<()> {
    let (a, b) = pair_programs(s)?;
    s.check_numbers().map_err(Error::Invalid)?;
    // The delivery block (bid-delivery-spec §2 Placement): mode 0, 1 or 2, its threshold and tip,
    // bought = 0, mode 0 for a KRON T and for a repeating entry's exits.
    s.check_delivery(true).map_err(Error::Invalid)?;
    check_key(&s.maker(), "order maker")?;
    let floor_of = |program: TemplateId| program.min_token_output().unwrap_or(0).max(crate::tx::COVENANT_OUTPUT_MIN) as i64;
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
            // The T a minimum fill delivers fits one output of T.
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
                // The exit's custody (n of A) carries deliveryCarrier.
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
            if i.exit_carrier < crate::tx::COVENANT_OUTPUT_MIN as i64 {
                return invalid("exitCarrier is dust (the exit UTXO, a covenant output, carries it)");
            }
            if i.rpt_amount > 0 {
                // A merge's new custody (of A or B) carries exitCarrier.
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
            // The exit as a fill of one minimum fill creates it.
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
    let floor = t_prog.min_token_output().unwrap_or(0).max(crate::tx::COVENANT_OUTPUT_MIN) as i64;
    if c.delivery_carrier < floor {
        return invalid(format!(
            "deliveryCarrier {} is below the {floor} sompi a token output of {} carries",
            c.delivery_carrier,
            t_prog.name()
        ));
    }
    if exit && !c.is_ask() {
        // A re-arming sell-first exit pays its profit on the custody's token.
        let s_prog = prog_of(c.s_cov_id);
        let floor = s_prog.min_token_output().unwrap_or(0).max(crate::tx::COVENANT_OUTPUT_MIN) as i64;
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

/// Smallest KAS value of a new pair order UTXO.
///
/// It is the delivery carriers the order funds itself and the tip of its whole amount:
/// - An order that can rest after a fill funds a partial fill and the fill of its rest
///   (`funded_fills`). Below that its first partial fill is impossible, or leaves a continuation
///   that can never deliver.
/// - A TWAP / DCA order funds one delivery per `maxFill` slice.
/// - An IOC / FOK order, or one whose `minFill` takes everything, funds one.
/// - An entry funds a delivery and an exit carrier per possible fill and the tip, and a repeating
///   entry one extra exit carrier.
/// - A mode-1 order (accumulate) funds one: its first fill puts the carrier on the bought-custody,
///   and every later fill moves the custody's KAS along (bid-delivery-spec §10 Funding).
pub(crate) fn min_pair_value(s: &AnyState) -> i64 {
    let sat = |v: Option<i64>| v.unwrap_or(i64::MAX);
    match s {
        AnyState::KobPair(p) => sat(p.kas_value(if p.mode == MODE_ACCUMULATE { 1 } else { p.funded_fills() })),
        AnyState::KobCondPair(c) => sat(c.kas_value(if c.mode == MODE_ACCUMULATE { 1 } else { c.funded_fills() })),
        AnyState::KobIfdPair(i) => sat(i.kas_value()),
        _ => 1,
    }
}

// ---------------------------------------------------------------- matcher legs

/// What a pair leg computes before the layout.
///
/// That is its amounts, and which of its custodies keep an output at their own index.
#[allow(clippy::large_enum_variant)] // one per leg of a batch
pub(crate) enum PairCalc {
    Pair { t_arg: i64, f: PairFill, dlv: PairDelivery },
    Cond(CondCalc),
    Ifd(IfdCalc),
}

// ---------------------------------------------------------------- T delivery modes

/// The slot of a `KobPair` / `KobCondPair` leg's bought-custody input.
///
/// Those legs never use the entry slots `A` and `B`, which only a `KobIfdPair` leg fills.
/// INTEGRATOR: a dedicated `Slot::Bought` in `build.rs` can replace it.
const BOUGHT_SLOT: Slot = Slot::A;
/// The slot of a top-up's receive UTXO input (mode 2 form 2). INTEGRATOR: `Slot::Receive`.
const RECEIVE_SLOT: Slot = Slot::B;

/// The T inputs a delivering pair leg spends besides its order and its sell custody
/// (bid-delivery-spec §3), and the form it asks for.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DeliveryInputs<'a> {
    /// Mode 1 with `bought > 0`: the order's bought-custody (T owned by the order's covenant id,
    /// owner scheme `0x04`, no borrowing, a zero guard, exactly `bought`).
    pub bought_custody: Option<&'a TokenUtxo>,
    /// Mode 2 form 2: the maker's receive UTXO the fill tops up.
    pub receive: Option<&'a TokenUtxo>,
    /// Mode 2: the form, 0 a plain UTXO, 1 a new receive UTXO, 2 a top-up of `receive`.
    ///
    /// Default: 2 with a receive UTXO, else 1.
    pub form: Option<u8>,
}

/// The delivery inputs of a pair leg.
///
/// INTEGRATOR: `Leg::Pair` and `Leg::CondPair` carry them as `bought_custody: Option<TokenUtxo>`,
/// `receive: Option<TokenUtxo>` and `form: Option<u8>` (serde `boughtCustody`, `receive`, `form`;
/// the B3 patch of `build.rs`). Until then every leg has none: modes 0 and 2 forms 0 / 1 and the
/// first fill of a mode-1 order build; a mode-1 order holding a bought-custody and a top-up need
/// the fields.
pub(crate) fn delivery_inputs(l: &Leg) -> DeliveryInputs<'_> {
    let _ = l;
    DeliveryInputs::default()
}

/// A pair order's T delivery at output i, by its delivery mode and form (bid-delivery-spec §4).
#[derive(Clone, Debug)]
pub(crate) struct PairDelivery {
    pub mode: i64,
    /// The `form` argument (the covenant reads it in mode 2 only).
    pub form: i64,
    /// The state of output i, its amount included (`have + tOut`).
    pub state: TokenState,
    /// KAS leaving the order beyond the price and the tip: a carrier, a top-up's `topupTip`, or 0
    /// once the bought-custody exists.
    pub released: i64,
    /// KAS the order itself puts on output i: a carrier, or 0 when the delivery continues a T
    /// input that carries its own KAS.
    pub on_d: i64,
    /// KAS of the T input the delivery continues (a bought-custody or a receive UTXO), else 0.
    pub have_kas: i64,
    /// Base units of that input, which flow in and out again on output i.
    pub have: i64,
    /// `bought` of the continuation (mode 1: `bought + tOut`; else the unchanged 0).
    pub next_bought: i64,
    /// The slot of the continued T input, if any.
    pub input: Option<Slot>,
    /// The role tag: "" (mode 0), ".m1", ".m1c" (a bought-custody is spent), ".m2" (forms 0 and
    /// 1), ".m2t" (a top-up).
    pub tag: &'static str,
}

impl PairDelivery {
    /// The KAS of output i when the order continues: the continued input's KAS and the order's
    /// carrier, if it puts one.
    fn rest_value(&self) -> i64 {
        self.have_kas + self.on_d
    }
    /// The KAS of output i when the order ends: every KAS of the order and of the continued input,
    /// less the tip and what leaves the order beyond the carrier (a top-up's tip).
    fn end_value(&self, order_value: i64, tip_kas: i64) -> i64 {
        order_value + self.have_kas - tip_kas - (self.released - self.on_d)
    }
    /// The token flow of T: the continued input's units in, output i's out.
    fn t_flow(&self, t: Token) -> (Token, i64, i64) {
        (t, self.have, self.state.amount())
    }
}

/// The guard of a receive UTXO with threshold `thr`: `thr` little-endian, then 24 zero bytes.
pub(crate) fn receive_guard(thr: i64) -> [u8; 32] {
    let mut g = [0u8; 32];
    g[..8].copy_from_slice(&thr.to_le_bytes());
    g
}

/// The receive state of `amount` for key `owner` with threshold `thr` (bid-delivery-spec §1).
pub(crate) fn receive_state(amount: i64, owner: [u8; 32], thr: i64, ext: [u8; 32]) -> TokenState {
    TokenState::Kcc20(Kcc20State::p2pk(amount, owner, ext)).with_borrow((1, receive_guard(thr)))
}

/// The terms of a delivering pair order the delivery reads.
pub(crate) struct DeliveryTerms {
    pub d: crate::state::Delivery,
    pub carrier: i64,
    pub maker: [u8; 32],
    /// The commitment of T's deliveries (zero for KRON).
    pub t_ext: [u8; 32],
}

/// The delivery of `t_out` units of T at output i (bid-delivery-spec §4).
///
/// `more`: the order continues. `order_cov`: the order's covenant id. It refuses what the covenant
/// refuses. A top-up of `n <= threshold` of the receive UTXO, which the token program cannot take,
/// falls back to form 1 (§6 rule 6).
pub(crate) fn pair_delivery(
    terms: &DeliveryTerms,
    t: Token,
    order_cov: [u8; 32],
    t_out: i64,
    more: bool,
    inp: DeliveryInputs,
) -> Result<PairDelivery> {
    let DeliveryTerms { d, carrier, maker, t_ext } = *terms;
    let fam = t.1.family();
    if !(MODE_NEW..=MODE_TOPUP).contains(&d.mode) {
        return invalid(format!("delivery mode must be 0, 1 or 2, got {}", d.mode));
    }
    if fam != Family::Kcc20 && d.mode != MODE_NEW {
        return invalid(format!("a KRON delivery is mode 0, got mode {}", d.mode));
    }
    if d.mode != MODE_TOPUP && (inp.receive.is_some() || inp.form.is_some_and(|f| f != 0)) {
        return invalid("only a mode-2 order takes a form or a receive UTXO");
    }
    if d.mode != MODE_ACCUMULATE && inp.bought_custody.is_some() {
        return invalid("only a mode-1 order holds a bought-custody");
    }
    let plain = |amount: i64| TokenState::user(t.1, amount, maker, t_ext);
    let new = |state: TokenState, form: i64, tag: &'static str| PairDelivery {
        mode: d.mode,
        form,
        state,
        released: carrier,
        on_d: carrier,
        have_kas: 0,
        have: 0,
        next_bought: 0,
        input: None,
        tag,
    };
    match d.mode {
        MODE_ACCUMULATE => {
            if d.bought < 0 {
                return invalid(format!("bought must be >= 0, got {}", d.bought));
            }
            let total = add(d.bought, t_out, "bought + tOut")?;
            let state = if more { TokenState::custody(fam, total, order_cov, t_ext) } else { plain(total) };
            if d.bought == 0 {
                if inp.bought_custody.is_some() {
                    return invalid("a mode-1 order with bought = 0 holds no bought-custody");
                }
                return Ok(PairDelivery { next_bought: total, ..new(state, 0, ".m1") });
            }
            let c = inp.bought_custody.ok_or_else(|| {
                Error::Invalid(format!(
                    "a mode-1 order holding bought = {} spends its bought-custody (boughtCustody) in every fill",
                    d.bought
                ))
            })?;
            check_bought_custody(c, t, order_cov, d.bought, t_ext)?;
            Ok(PairDelivery {
                released: 0,
                on_d: 0,
                have_kas: c.utxo.amount as i64,
                have: d.bought,
                next_bought: total,
                input: Some(BOUGHT_SLOT),
                ..new(state, 0, ".m1c")
            })
        }
        MODE_TOPUP => {
            if d.bought != 0 {
                return invalid(format!("modes 0 and 2 hold no bought tokens (bought {})", d.bought));
            }
            let mut form = inp.form.map(i64::from).unwrap_or(if inp.receive.is_some() { 2 } else { 1 });
            if form != 0 && (d.recv_threshold <= 0 || d.topup_tip < 0 || d.topup_tip > carrier) {
                return invalid("mode 2 forms 1 and 2 need recvThreshold > 0 and 0 <= topupTip <= deliveryCarrier");
            }
            let mut receive = inp.receive;
            if form == 2 {
                let r = receive.ok_or_else(|| Error::Invalid("a top-up (form 2) names the maker's receive UTXO (receive)".into()))?;
                let thr = check_receive(r, t, maker, d.recv_threshold, t_ext)?;
                if t_out <= thr {
                    // The token program takes a top-up only with a gain above the threshold.
                    form = 1;
                    receive = None;
                }
            } else if receive.is_some() {
                return invalid(format!("form {form} spends no receive UTXO"));
            }
            match (form, receive) {
                (0, _) => Ok(new(plain(t_out), 0, ".m2")),
                (1, _) => Ok(new(receive_state(t_out, maker, d.recv_threshold, t_ext), 1, ".m2")),
                (2, Some(r)) => {
                    let have = r.state.amount();
                    Ok(PairDelivery {
                        released: d.topup_tip,
                        on_d: 0,
                        have_kas: r.utxo.amount as i64,
                        have,
                        input: Some(RECEIVE_SLOT),
                        ..new(r.state.with_amount(add(have, t_out, "receive + tOut")?), 2, ".m2t")
                    })
                }
                _ => invalid(format!("mode 2 delivers in form 0, 1 or 2, got {form}")),
            }
        }
        _ => {
            if d.bought != 0 {
                return invalid(format!("modes 0 and 2 hold no bought tokens (bought {})", d.bought));
            }
            Ok(new(plain(t_out), 0, ""))
        }
    }
}

/// A bought-custody: T of the order's covenant id, owner scheme `0x04`, no borrowing, a zero guard,
/// the order's commitment and exactly `bought` (bid-delivery-spec §5: known by its exact state).
fn check_bought_custody(c: &TokenUtxo, t: Token, order_cov: [u8; 32], bought: i64, ext: [u8; 32]) -> Result<()> {
    check_custody(c, Some(order_cov), t, bought).map_err(|e| Error::Invalid(format!("bought-custody: {e}")))?;
    if c.state != TokenState::custody(t.1.family(), bought, order_cov, ext) {
        return invalid(
            "bought-custody: its state is not exactly the order's (borrowing disabled, a zero guard, the order's commitment)",
        );
    }
    Ok(())
}

/// A receive UTXO of the maker the order may top up, and its threshold (bid-delivery-spec §4).
///
/// It is T, owned by the maker's key (scheme `0x00`), borrow scheme `0x01`, a canonical guard with a
/// threshold of at least `recvThreshold`, and the order's commitment.
fn check_receive(r: &TokenUtxo, t: Token, maker: [u8; 32], recv_threshold: i64, ext: [u8; 32]) -> Result<i64> {
    check_token_input(r, t)?;
    let TokenState::Kcc20(k) = &r.state else { return invalid("a receive UTXO is a KCC-20 holding") };
    if k.owner != maker || k.owner_scheme != SCHEME_P2PK || k.borrow_scheme != 1 {
        return invalid("the receive UTXO is the maker's key-owned holding with borrow scheme 0x01");
    }
    if k.borrow_guard[8..] != [0u8; 24] {
        return invalid("the receive UTXO's guard is not canonical (threshold, then 24 zero bytes)");
    }
    let thr = i64::from_le_bytes(k.borrow_guard[..8].try_into().expect("8 bytes"));
    if thr < recv_threshold {
        return invalid(format!("the receive UTXO's threshold {thr} is below the order's recvThreshold {recv_threshold}"));
    }
    if k.extension_commitment != ext {
        return invalid("the receive UTXO carries another extension commitment than the order's deliveries");
    }
    if k.amount < 0 {
        return invalid("the receive UTXO holds a negative amount");
    }
    Ok(thr)
}

/// A top-up's batch rules (bid-delivery-spec §6 rules 3 and 4).
///
/// The receive UTXO is T's first input (the leader, spent on the borrow path), the topped-up
/// order's output i is T's first output (no leg before it delivers T at its positional slot, a
/// refund-carrying bid included, which the builder puts first), and a transaction tops up one
/// receive UTXO per token.
fn check_topup(cx: &PairCtx, t: Token) -> Result<()> {
    let PairCtx { batch, lay, index: i, .. } = *cx;
    if lay.first_token_in.get(&t.0) != lay.at.get(&(i, RECEIVE_SLOT)) {
        return invalid("a top-up's receive UTXO must be the first input of its token (put the top-up leg first)");
    }
    for (j, l) in batch.legs.iter().enumerate() {
        if j < i && positional_token(l) == Some(t.0) {
            return invalid(format!(
                "a top-up is the lowest-index delivery of its token, but leg {j} delivers it before (use form 1)"
            ));
        }
        if j != i && delivery_inputs(l).receive.is_some() && leg_t_token(l) == Some(t.0) {
            return invalid("one top-up per token and transaction (use form 1 for the others)");
        }
    }
    Ok(())
}

/// The token a leg's positional output (output j) holds, if it is a token output.
fn positional_token(l: &Leg) -> Option<[u8; 32]> {
    match l {
        Leg::Bid { order, .. } => Some(order.state.token_cov_id),
        Leg::CondBid { order, .. } => Some(order.state.token_cov_id),
        Leg::IfdBid { order, .. } => Some(order.state.token_cov_id),
        Leg::Pair { .. } | Leg::CondPair { .. } => leg_t_token(l),
        Leg::IfdPair { order, .. } => {
            let s = &order.state;
            Some(if s.is_buy_first() { s.a_cov_id } else { s.b_cov_id })
        }
        _ => None,
    }
}

/// The tokens (S, T) of a `KobPair` / `KobCondPair` leg.
fn pair_s_t(l: &Leg) -> Result<(Token, Token)> {
    let (a, b) = leg_pair_tokens(l)?.ok_or_else(|| Error::Invalid("not a pair leg".into()))?;
    let ask = match l {
        Leg::Pair { order, .. } => order.state.is_ask(),
        Leg::CondPair { order, .. } => order.state.is_ask(),
        _ => return invalid("not a delivering pair leg"),
    };
    Ok(if ask { (a, b) } else { (b, a) })
}

/// The covenant id of an order UTXO.
fn order_cov(u: &Utxo) -> Result<[u8; 32]> {
    u.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))
}

/// The delivery terms of a `KobPair`.
fn pair_terms(s: &PairState) -> DeliveryTerms {
    DeliveryTerms {
        d: crate::state::Delivery { mode: s.mode, recv_threshold: s.recv_threshold, topup_tip: s.topup_tip, bought: s.bought },
        carrier: s.delivery_carrier,
        maker: s.maker,
        t_ext: if s.t_token().family_of() == Some(Family::Kcc20) { s.t_ext } else { [0; 32] },
    }
}

/// The delivery terms of a `KobCondPair`.
fn cond_terms(s: &CondPairState) -> DeliveryTerms {
    DeliveryTerms {
        d: crate::state::Delivery { mode: s.mode, recv_threshold: s.recv_threshold, topup_tip: s.topup_tip, bought: s.bought },
        carrier: s.delivery_carrier,
        maker: s.maker,
        t_ext: if s.t_token().family_of() == Some(Family::Kcc20) { s.t_ext } else { [0; 32] },
    }
}

/// The T a `KobPair` / `KobCondPair` leg delivers.
fn leg_t_token(l: &Leg) -> Option<[u8; 32]> {
    match l {
        Leg::Pair { order, .. } => Some(order.state.t_cov_id),
        Leg::CondPair { order, .. } => Some(order.state.t_cov_id),
        _ => None,
    }
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
    /// Re-arming: (proceeds, back), the entry's budget or proceeds of n and a sell-first entry's
    /// prefund of n.
    rearm: Option<(i64, i64)>,
    /// Amount of the maker's token output.
    /// The maker's token output is a re-arming BID exit's B profit on the custody's token (`true`),
    /// or the T delivery (`false`).
    d_out: i64,
    d_s: bool,
    /// The T delivery at output i (mode 0 when the exit re-arms its entry).
    dlv: PairDelivery,
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

/// The tokens A and B of a pair leg, or `None` for a leg that is not a pair order.
pub(crate) fn leg_pair_tokens(l: &Leg) -> Result<Option<(Token, Token)>> {
    Ok(match l {
        Leg::Pair { order, .. } => Some(pair_programs(&AnyState::KobPair(order.state.clone()))?),
        Leg::CondPair { order, .. } => Some(pair_programs(&AnyState::KobCondPair(order.state.clone()))?),
        Leg::IfdPair { order, .. } => Some(pair_programs(&AnyState::KobIfdPair(order.state.clone()))?),
        _ => None,
    })
}

/// The main token of a pair leg.
///
/// It is the custody token S of a `KobPair` or `KobCondPair`, and the A of a sell-first entry or
/// the B of a buy-first one.
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

/// The amounts of a pair leg, by the covenant's arithmetic.
///
/// It refuses what the covenant refuses.
pub(crate) fn calc_leg(l: &Leg, lock: i64, strict: bool) -> Result<Option<PairCalc>> {
    let r = match l {
        Leg::Pair { order, custody, amount, t, .. } => {
            let state = &order.state;
            let n = *amount;
            let utxo_daa = order.utxo.block_daa_score as i64;
            if strict && lock < state.active_from {
                return invalid(format!("pair order not active before DAA {}", state.active_from));
            }
            if custody.state.amount() != state.custody {
                return invalid(format!(
                    "the pair order's custody holds {}, its state needs exactly {}",
                    custody.state.amount(),
                    state.custody
                ));
            }
            check_custody_ext(custody, state.s_ext)?;
            let t_arg = if state.slope != 0 {
                auction_t(*t, lock, need(state.origin(utxo_daa), "decay origin")?, "decaying pair order")?
            } else {
                0
            };
            let price = need(state.price_at(t_arg, utxo_daa), "pair order quote")?;
            // The order funds what its mode releases (`dlv.released`), checked below, so the
            // state's rule runs with the carrier added back.
            let value = order.utxo.amount as i64;
            let fill = state
                .fill(n, price, value.saturating_add(state.delivery_carrier))
                .map_err(|e| Error::Invalid(format!("pair fill: {e}")))?;
            let (_, t_token) = pair_s_t(l)?;
            let dlv = pair_delivery(&pair_terms(state), t_token, order_cov(&order.utxo)?, fill.t_out, fill.rest, delivery_inputs(l))?;
            if fill.rest && value < dlv.released.saturating_add(fill.tip_kas) {
                return invalid(format!(
                    "the order's {value} sompi do not fund what a partial fill releases ({} + the tip {})",
                    dlv.released, fill.tip_kas
                ));
            }
            PairCalc::Pair { t_arg, f: fill, dlv }
        }
        Leg::CondPair { order, custody, amount, leg, evidence, t, merge, .. } => {
            let state = &order.state;
            let n = *amount;
            let utxo_daa = order.utxo.block_daa_score as i64;
            if strict && lock < state.active_from {
                return invalid("conditional pair order not active yet");
            }
            if !state.fill_ok(n) {
                return invalid(format!(
                    "a conditional pair fill of {n} needs n <= amountLeft {} and n >= minFill {} unless it takes everything left",
                    state.amount_left, state.min_fill
                ));
            }
            if custody.state.amount() != state.custody {
                return invalid("the conditional pair order's custody does not hold exactly its custody field");
            }
            check_custody_ext(custody, state.s_ext)?;
            let leg = *leg as i64;
            let mut trigger = false;
            let mut t_arg = 0;
            match leg {
                0 if state.tp_price > 0 => {}
                1 if state.stop_ok() => {
                    if state.armed == 0 {
                        if evidence.is_none() {
                            return invalid("an unarmed stop leg needs trigger evidence");
                        }
                        trigger = true;
                    } else if state.band_daa > 0 {
                        let origin = armed_origin(state.armed, utxo_daa).expect("armed");
                        t_arg = auction_t(*t, lock, origin, "stop auction")?;
                    }
                }
                _ => return invalid("invalid conditional leg for this order"),
            }
            if leg == 0 && evidence.is_some() {
                return invalid("a take-profit / limit fill takes no trigger evidence");
            }
            let leg_price = need(state.leg_price(leg, trigger, t_arg, utxo_daa), "conditional pair leg price")?;
            if leg_price <= 0 {
                return invalid("the leg price must be positive");
            }
            let mut s_out = need(state.s_out(n, leg_price), "conditional pair S out")?;
            let mut t_out = need(state.t_out_min(n, leg_price), "conditional pair T out")?;
            let tip_kas = need(state.tip_kas(n), "conditional pair tip")?;
            if state.tip < 0 || state.delivery_carrier < 0 {
                return invalid("tip and deliveryCarrier must be >= 0");
            }
            let left = state.amount_left - n;
            let mut out_amount = state.custody - s_out;
            let mut rearm = None;
            let mut d_out = t_out;
            let mut d_s = false;
            if state.is_booked() {
                if leg == 0 {
                    if merge.is_none() && lock < state.rpt_until {
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
                let proceeds = need(state.rpt_proceeds(n), "repeat proceeds")?;
                if state.is_ask() {
                    // The entry's B custody takes the budget. The maker's profit must be positive,
                    // so deliver one more unit of B when the rounded minimum leaves nothing.
                    if t_out <= proceeds {
                        t_out = add(proceeds, 1, "repeat delivery")?;
                    }
                    d_out = t_out - proceeds;
                    rearm = Some((proceeds, 0));
                } else {
                    d_s = true;
                    let back = need(state.rpt_back(n), "repeat prefund")?;
                    if left > 0 {
                        // The maker's profit proceeds - sOut must be positive, so pay at most
                        // proceeds - 1.
                        s_out = s_out.min(proceeds - 1);
                        out_amount = state.custody - proceeds - back;
                        d_out = proceeds - s_out;
                    } else {
                        s_out = s_out.min(state.custody - back - 1);
                        out_amount = 0;
                        d_out = state.custody - back - s_out;
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
                return invalid(format!(
                    "the conditional pair order's custody of {} cannot release what this fill needs",
                    state.custody
                ));
            }
            if left > 0 && out_amount == 0 {
                return invalid("a partial conditional pair fill must leave something in the custody");
            }
            if rearm.is_some() && state.mode != MODE_NEW {
                return invalid("a booked exit that re-arms its entry delivers in mode 0");
            }
            let (_, t_token) = pair_s_t(l)?;
            let inputs = if d_s { DeliveryInputs::default() } else { delivery_inputs(l) };
            let dlv = pair_delivery(&cond_terms(state), t_token, order_cov(&order.utxo)?, d_out, left > 0, inputs)?;
            if left > 0 && (order.utxo.amount as i64) < dlv.released.saturating_add(tip_kas) {
                return invalid("the order's KAS do not fund what a partial fill releases (a carrier or a top-up tip) and the tip");
            }
            PairCalc::Cond(CondCalc { leg, trigger, t_arg, s_out, t_out, tip_kas, left, out_amount, rearm, d_out, d_s, dlv })
        }
        Leg::IfdPair { order, amount, evidence, t, .. } => {
            let state = &order.state;
            let n = *amount;
            let utxo_daa = order.utxo.block_daa_score as i64;
            let cov_id = order.utxo.covenant_id.ok_or_else(|| Error::Invalid("entry UTXO has no covenant id".into()))?;
            if strict && lock < state.active_from {
                return invalid("pair entry not active yet");
            }
            if !state.fill_ok(n) {
                return invalid("pair entry fill: n <= amountLeft and n >= minFill unless it takes everything left");
            }
            if state.tip < 0 || state.delivery_carrier < 0 || state.exit_carrier < 0 {
                return invalid("tip and carriers must be >= 0");
            }
            let buy = state.is_buy_first();
            let mut trigger = false;
            let mut t_arg = 0;
            if state.entry_stop > 0 {
                if (buy && state.entry_stop > state.price) || (!buy && state.entry_stop < state.price) {
                    return invalid("a stop entry whose trigger is beyond its limit never fills");
                }
                if state.armed == 0 {
                    if evidence.is_none() {
                        return invalid("an unarmed stop entry needs trigger evidence");
                    }
                    trigger = true;
                } else if state.band_daa > 0 {
                    t_arg = auction_t(*t, lock, armed_origin(state.armed, utxo_daa).expect("armed"), "stop-entry auction")?;
                }
            } else if evidence.is_some() {
                return invalid("a limit entry takes no trigger evidence");
            }
            let booked = state.rpt_amount > n;
            if booked && n >= MERGE_SHIFT {
                return invalid("a booked exit's amount must be below 2^53 (the merge argument)");
            }
            if strict {
                rpt_fill_rule(state.rpt_amount, state.min_fill, n)?;
            }
            if booked && t_arg == 0 {
                t_arg = t.unwrap_or(lock);
            }
            if t_arg > lock {
                return invalid("time t must be <= lockTime");
            }
            let price = need(state.price_at(trigger, t_arg, utxo_daa), "pair entry price")?;
            if price <= 0 {
                return invalid("the entry quote must be positive");
            }
            let tip_kas = need(state.tip_kas(n), "pair entry tip")?;
            let new_left = state.amount_left - n;
            let cont = new_left > 0 || state.rpt_amount > 0;
            let (amt, a_new, b_new, x_cust) = if buy {
                let amt = need(state.spend(n, price), "pair entry spend")?;
                let b_new = sub(state.custody, amt, "pair entry escrow")?;
                if b_new < 0 {
                    return invalid(format!("the entry's escrow of {} cannot pay {amt}", state.custody));
                }
                (amt, 0, b_new, n)
            } else {
                if state.prefund < 0 {
                    return invalid("prefund must be >= 0");
                }
                let amt = need(state.proceeds(n, price), "pair entry proceeds")?;
                let used = if cont { need(state.pre_of(n), "pair entry prefund")? } else { state.custody };
                if used > state.custody {
                    return invalid(format!(
                        "the entry's prefund custody of {} cannot pay the prefund {used} of this fill",
                        state.custody
                    ));
                }
                (amt, new_left, state.custody - used, add(amt, used, "the exit's custody")?)
            };
            let booking =
                booked.then_some(Booking { parent: cov_id, until: need(rpt_until(state.expiry_daa, utxo_daa), "rptUntil")? });
            let exit = state.exit_for(n, x_cust, booking)?;
            PairCalc::Ifd(IfdCalc { trigger, t_arg, amt, a_new, b_new, x_cust, booked, cont, tip_kas, exit })
        }
        _ => return Ok(None),
    };
    Ok(Some(r))
}

/// The custody inputs of a pair leg: `(slot, utxo, token, positional)`.
///
/// `positional` means the transaction keeps an output of that token at the custody's own input
/// index: its rest or return, or a merged entry's existing custody.
pub(crate) fn leg_custodies<'a>(l: &'a Leg, calc: &PairCalc, a: Token, b: Token) -> Result<Vec<(Slot, &'a TokenUtxo, Token, bool)>> {
    let mut v = vec![];
    match (l, calc) {
        (Leg::Pair { order, custody, .. }, PairCalc::Pair { f, dlv, .. }) => {
            v.push((Slot::Custody, custody, if order.state.is_ask() { a } else { b }, f.out_amount > 0));
            push_delivery_input(&mut v, l, dlv, if order.state.is_ask() { b } else { a });
        }
        (Leg::CondPair { order, custody, merge, .. }, PairCalc::Cond(c)) => {
            v.push((Slot::Custody, custody, if order.state.is_ask() { a } else { b }, c.out_amount > 0));
            push_delivery_input(&mut v, l, &c.dlv, if order.state.is_ask() { b } else { a });
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

/// Appends the T input a delivery continues (a bought-custody or a receive UTXO), if any.
///
/// It is never positional: a bought-custody's successor and a topped-up receive UTXO sit at the
/// order's output i, not at their own index.
fn push_delivery_input<'a>(v: &mut Vec<(Slot, &'a TokenUtxo, Token, bool)>, l: &'a Leg, dlv: &PairDelivery, t: Token) {
    let inp = delivery_inputs(l);
    match dlv.input {
        Some(BOUGHT_SLOT) => v.push((BOUGHT_SLOT, inp.bought_custody.expect("checked by pair_delivery"), t, false)),
        Some(RECEIVE_SLOT) => v.push((RECEIVE_SLOT, inp.receive.expect("checked by pair_delivery"), t, false)),
        _ => {}
    }
}

/// The evidence arguments `(evA, evB, tk, evMode)` of a pair conditional.
///
/// With `ask_a` the A leg must be an ask: in mode 0 an ask of A and a bid of B, in mode 1 a pair
/// ASK. Otherwise it is a bid of A and an ask of B, or a pair BID.
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
            // Mode 0: a KAS-book leg of A and one of B, filled in this transaction.
            let lb = b.legs.get(kb).ok_or_else(|| Error::Invalid(format!("trigger evidence {kb} is not a leg of the batch")))?;
            // A leg that cannot be evidence at all is that leg's refusal, not the reader's.
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
            // Mode 1: a resting KobPair of this pair, filled in this transaction.
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
            if s.interval < 0 {
                // The covenant of this build refuses it too, since a negative interval would
                // shorten the rest time.
                return Err(blame(
                    "evidence",
                    k,
                    Error::Invalid("an evidence order with a negative interval is never trigger evidence".into()),
                ));
            }
            let exposed = (order.utxo.block_daa_score as i64 + s.interval).max(s.active_from).max(custody.utxo.block_daa_score as i64);
            if exposed.saturating_add(min_rest) > lock {
                return invalid("trigger evidence was exposed for less than minRestDaa before the lock time");
            }
            Ok((PairEvidence::Pair { price: s.price }, [k as i64, 0, lay.at[&(k, Slot::Custody)] as i64, 1]))
        }
    }
}

/// Whether the evidence's A leg is an ask.
///
/// In mode 0 the first leg is then a KobAsk, and in mode 1 the pair leg sells A.
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

/// The extension commitment a token output of `t` carries: `e` for KCC-20, none for KRON.
fn kcc20_ext(t: Token, e: [u8; 32]) -> [u8; 32] {
    if t.1.family() == Family::Kcc20 {
        e
    } else {
        [0; 32]
    }
}

/// The role tag of a pair fill that arms from evidence of mode `ev_mode` (0 or 1).
fn arm_tag(trigger: bool, ev_mode: i64) -> &'static str {
    if !trigger {
        ""
    } else if ev_mode == 0 {
        ".arm0"
    } else {
        ".arm1"
    }
}

/// The plan of a pair leg: its input, positional output, extras and token flows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_leg(
    batch: &Batch,
    lay: &Layout,
    i: usize,
    leg: &Leg,
    calc: &PairCalc,
    a: Token,
    bt: Token,
    lock: i64,
) -> Result<LegPlan> {
    let suffix = pair_suffix(a, bt);
    let cx = PairCtx { batch, lay, index: i, token_a: a, token_b: bt, lock };
    match (leg, calc) {
        (Leg::Pair { order, custody, amount, .. }, PairCalc::Pair { t_arg, f, dlv }) => {
            plan_pair(&cx, order, custody, *amount, *t_arg, f, dlv, suffix)
        }
        (Leg::CondPair { order, custody, amount, evidence, evidence_b, merge, .. }, PairCalc::Cond(c)) => {
            plan_cond_pair(&cx, order, custody, *amount, *evidence, *evidence_b, merge.as_ref(), c, suffix)
        }
        (Leg::IfdPair { order, a_custody, b_custody, amount, evidence, evidence_b, .. }, PairCalc::Ifd(c)) => {
            plan_ifd_pair(&cx, order, a_custody.as_ref(), b_custody.as_ref(), *amount, *evidence, *evidence_b, c, suffix)
        }
        _ => invalid("pair leg and its amounts disagree"),
    }
}

/// The batch facts every pair leg planner reads.
#[derive(Clone, Copy)]
struct PairCtx<'a> {
    batch: &'a Batch,
    lay: &'a Layout,
    /// The leg's position in the batch.
    index: usize,
    /// The pair's base token A.
    token_a: Token,
    /// The pair's quote token B.
    token_b: Token,
    lock: i64,
}

/// Plans a `KobPair` fill: the T delivery, and the rest or the return of its custody.
#[allow(clippy::too_many_arguments)]
fn plan_pair(
    cx: &PairCtx,
    order: &OrderUtxo<PairState>,
    custody: &TokenUtxo,
    n: i64,
    t_arg: i64,
    fill: &PairFill,
    dlv: &PairDelivery,
    suffix: String,
) -> Result<LegPlan> {
    let PairCtx { lay, index: i, token_a: a, token_b: bt, .. } = *cx;
    let state = &order.state;
    let ask = state.is_ask();
    let (s_token, t_token) = if ask { (a, bt) } else { (bt, a) };
    let order_value = order.utxo.amount as i64;
    let custody_value = custody.utxo.amount as i64;
    if dlv.input == Some(RECEIVE_SLOT) {
        check_topup(cx, t_token)?;
    }
    let mut extras = vec![];
    let (delivery, branch) = if fill.rest {
        extras.push(Extra::Cont(
            kas_out(order_value - dlv.released - fill.tip_kas, "pair continuation")?,
            PairState { amount_left: state.amount_left - n, custody: fill.out_amount, bought: dlv.next_bought, ..state.clone() }.spk(),
        ));
        let st = custody.state.with_amount(fill.out_amount);
        extras.push(Extra::AtInput { slot: Slot::Custody, value: custody.utxo.amount, state: st, token: s_token, patch: None });
        (dlv.rest_value(), "rest")
    } else if fill.out_amount > 0 {
        let st = custody.state.with_amount(fill.out_amount).with_user_owner(state.maker);
        extras.push(Extra::AtInput { slot: Slot::Custody, value: custody.utxo.amount, state: st, token: s_token, patch: None });
        (dlv.end_value(order_value, fill.tip_kas), "return")
    } else {
        (dlv.end_value(order_value, fill.tip_kas) + custody_value, "close")
    };
    let mut role = format!("KobPair.settle.{}.{branch}", side_name(ask));
    if state.interval > 0 {
        role.push_str(".twap");
    }
    if state.slope != 0 {
        role.push_str(".decay");
    }
    role.push_str(dlv.tag);
    let t_tpl = match dlv.input {
        Some(slot) => lay.at[&(i, slot)] as i64,
        None => first_in(lay, t_token, "pair order (T template source)")?,
    };
    Ok(LegPlan {
        suffix,
        pos: PosOut::Deliver(kas_out(delivery, "pair delivery carrier")?, dlv.state.clone(), t_token),
        role,
        sequence: state.interval.max(0) as u64,
        plan: entry(
            TemplateId::KobPair,
            state.encode(),
            "settle",
            vec![
                nb(n),
                Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                Arg::Int(t_tpl),
                Arg::Int(t_arg),
                Arg::Int(fill.s_out),
                Arg::Int(fill.t_out),
                Arg::Int(dlv.form),
            ],
        ),
        extras,
        flows: vec![(s_token, fill.s_out, 0), dlv.t_flow(t_token)],
        merge: None,
        // A pair ask's delivery takes the batch's surplus of T, unless it is a bought-custody
        // that stays with the order: the continuation's `bought` is fixed before the surplus.
        adjust: if ask && !(fill.rest && dlv.mode == MODE_ACCUMULATE) { Some((t_token, 5)) } else { None },
    })
}

/// Plans a `KobCondPair` fill on its take-profit or stop leg, and the merge of a booked exit's entry.
#[allow(clippy::too_many_arguments)]
fn plan_cond_pair(
    cx: &PairCtx,
    order: &OrderUtxo<CondPairState>,
    custody: &TokenUtxo,
    n: i64,
    evidence: Option<usize>,
    evidence_b: Option<usize>,
    merge: Option<&PairEntryMerge>,
    calc: &CondCalc,
    suffix: String,
) -> Result<LegPlan> {
    let PairCtx { batch, lay, index: i, token_a: a, token_b: bt, lock, .. } = *cx;
    let state = &order.state;
    let ask = state.is_ask();
    let (s_token, t_token) = if ask { (a, bt) } else { (bt, a) };
    let utxo_daa = order.utxo.block_daa_score as i64;
    let order_value = order.utxo.amount as i64;
    let custody_value = custody.utxo.amount as i64;
    let mut ev_args = [0i64; 4];
    if calc.trigger {
        let (ev, args) = pair_evidence(
            batch,
            lay,
            evidence,
            evidence_b,
            ask,
            a,
            bt,
            state.a_scale(),
            state.b_scale(),
            state.min_touch,
            state.min_touch_b(),
            state.min_rest_daa,
            lock,
        )?;
        if state.arms(ev) != Some(true) {
            return invalid(if ask {
                "a pair stop sell arms only when the rate fell to its stop or below"
            } else {
                "a pair buy stop arms only when the rate rose to its stop or above"
            });
        }
        ev_args = args;
    }
    let mut extras = vec![];
    if calc.left > 0 {
        let next = CondPairState {
            amount_left: calc.left,
            custody: calc.out_amount,
            armed: state.next_armed(calc.leg, calc.trigger, utxo_daa),
            bought: calc.dlv.next_bought,
            ..state.clone()
        };
        extras
            .push(Extra::Cont(kas_out(order_value - calc.dlv.released - calc.tip_kas, "conditional pair continuation")?, next.spk()));
        extras.push(Extra::AtInput {
            slot: Slot::Custody,
            value: custody.utxo.amount,
            state: custody.state.with_amount(calc.out_amount),
            token: s_token,
            patch: None,
        });
    } else if calc.rearm.is_none() && calc.out_amount > 0 {
        let st = custody.state.with_amount(calc.out_amount).with_user_owner(state.maker);
        extras.push(Extra::AtInput { slot: Slot::Custody, value: custody.utxo.amount, state: st, token: s_token, patch: None });
    }
    let dlv = &calc.dlv;
    if dlv.input == Some(RECEIVE_SLOT) {
        check_topup(cx, t_token)?;
    }
    let pos_value = if calc.rearm.is_some() {
        state.delivery_carrier
    } else if calc.left > 0 {
        dlv.rest_value()
    } else if calc.out_amount > 0 {
        dlv.end_value(order_value, calc.tip_kas)
    } else {
        dlv.end_value(order_value, calc.tip_kas) + custody_value
    };
    // The maker's token at output i: the T delivery, or a re-arming BID exit's B profit on
    // the custody's template.
    let pos_out = if calc.d_s {
        PosOut::Deliver(
            kas_out(pos_value, "profit carrier")?,
            custody.state.with_amount(calc.d_out).with_user_owner(state.maker),
            s_token,
        )
    } else {
        PosOut::Deliver(kas_out(pos_value, "delivery carrier")?, dlv.state.clone(), t_token)
    };
    let mut flows = vec![(s_token, calc.s_out, 0), (t_token, dlv.have, dlv.have + calc.t_out)];
    let merge_plan = match (merge, calc.rearm) {
        (None, _) => None,
        (Some(m), Some(rearm)) => Some(plan_cond_pair_merge(cx, order, custody, n, m, calc, rearm, &mut extras, &mut flows)?),
        (Some(_), None) => unreachable!("a merge is a re-arm"),
    };
    let branch = if calc.left > 0 {
        "rest"
    } else if calc.rearm.is_none() && calc.out_amount > 0 {
        "return"
    } else {
        "close"
    };
    let role = format!(
        "KobCondPair.settle.{}.leg{}{}{}{}.{branch}{}",
        side_name(ask),
        calc.leg,
        arm_tag(calc.trigger, ev_args[3]),
        if calc.t_arg != 0 { ".auction" } else { "" },
        if merge.is_some() { ".merge" } else { "" },
        dlv.tag,
    );
    let t_tpl = if let Some(slot) = dlv.input {
        lay.at[&(i, slot)] as i64
    } else if calc.d_s {
        lay.first_token_in.get(&t_token.0).map(|x| *x as i64).unwrap_or(0)
    } else {
        first_in(lay, t_token, "conditional pair (T template source)")?
    };
    Ok(LegPlan {
        suffix,
        pos: pos_out,
        role,
        sequence: 0,
        plan: entry(
            TemplateId::KobCondPair,
            state.encode(),
            "settle",
            vec![
                nb(n),
                Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                Arg::Int(t_tpl),
                Arg::Int(calc.t_arg),
                Arg::Int(calc.s_out),
                Arg::Int(calc.t_out),
                Arg::Int(calc.leg),
                Arg::Int(ev_args[0]),
                Arg::Int(ev_args[1]),
                Arg::Int(ev_args[2]),
                Arg::Int(ev_args[3]),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(dlv.form),
            ],
        ),
        extras,
        flows,
        merge: merge_plan,
        // As a KobPair ask: a bought-custody that stays with the order takes no surplus.
        adjust: if ask && !(calc.left > 0 && dlv.mode == MODE_ACCUMULATE) { Some((t_token, 5)) } else { None },
    })
}

/// Plans the merge of a booked `KobCondPair` exit's entry: the entry's custodies, flows and input.
///
/// `proceeds` and `back` are the exit's re-arm, the entry's budget or proceeds of n and a sell-first
/// entry's prefund of n. It replaces `flows` with the exit's and the entry's flows.
#[allow(clippy::too_many_arguments)]
fn plan_cond_pair_merge(
    cx: &PairCtx,
    order: &OrderUtxo<CondPairState>,
    custody: &TokenUtxo,
    n: i64,
    m: &PairEntryMerge,
    calc: &CondCalc,
    (proceeds, back): (i64, i64),
    extras: &mut Vec<Extra>,
    flows: &mut Vec<(Token, i64, i64)>,
) -> Result<(SigPlan, String, Utxo)> {
    let PairCtx { lay, index: i, token_a: a, token_b: bt, .. } = *cx;
    let state = &order.state;
    let order_value = order.utxo.amount as i64;
    let custody_value = custody.utxo.amount as i64;
    let e = &m.entry;
    let entry_state = &e.state;
    let (a_new, b_new) = merged_entry_amounts(state, e, n, proceeds, back)?;
    let entry_daa = e.utxo.block_daa_score as i64;
    let entry_value = e.utxo.amount as i64;
    let buy = entry_state.is_buy_first();
    let sellout = state.amount_left == n;
    let mut floor = entry_value;
    if sellout {
        let all = entry_value + order_value + custody_value - calc.tip_kas - state.delivery_carrier;
        floor = floor.max(all);
    }
    let entry_in = lay.merge_in[&i];
    let mut new_custody = false;
    // The entry's custodies after the merge: an existing one at its index, a new
    // one with exitCarrier.
    let (ea_in, eb_in) = (lay.at.get(&(i, Slot::EntryA)).copied(), lay.at.get(&(i, Slot::EntryB)).copied());
    if a_new > 0 {
        kron_output(a.1.family(), a_new, "the merged entry's A custody")?;
        match &m.a_custody {
            Some(x) => {
                check_custody(x, e.utxo.covenant_id, a, entry_state.amount_left).map_err(|e| blame("entry", i, e))?;
                check_custody_ext(x, entry_state.a_ext).map_err(|e| blame("entry", i, e))?;
                extras.push(Extra::AtInput {
                    slot: Slot::EntryA,
                    value: x.utxo.amount,
                    state: x.state.with_amount(a_new),
                    token: a,
                    patch: None,
                });
            }
            None => {
                if entry_state.amount_left > 0 {
                    return invalid("the merged entry's A custody is required");
                }
                new_custody = true;
                floor -= entry_state.exit_carrier;
                let st = TokenState::custody(a.1.family(), a_new, state.parent, kcc20_ext(a, entry_state.a_ext));
                extras.push(Extra::Tok {
                    value: u(entry_state.exit_carrier, "exitCarrier")?,
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
                check_custody(x, e.utxo.covenant_id, bt, entry_state.custody).map_err(|e| blame("entry", i, e))?;
                check_custody_ext(x, entry_state.b_ext).map_err(|e| blame("entry", i, e))?;
                extras.push(Extra::AtInput {
                    slot: Slot::EntryB,
                    value: x.utxo.amount,
                    state: x.state.with_amount(b_new),
                    token: bt,
                    patch: None,
                });
            }
            None => {
                if entry_state.custody > 0 {
                    return invalid("the merged entry's B custody is required");
                }
                new_custody = true;
                floor -= entry_state.exit_carrier;
                let st = TokenState::custody(bt.1.family(), b_new, state.parent, kcc20_ext(bt, entry_state.b_ext));
                extras.push(Extra::Tok {
                    value: u(entry_state.exit_carrier, "exitCarrier")?,
                    state: st,
                    token: bt,
                    patch: Some((entry_in, 15)),
                });
            }
        }
    }
    // The exit's own flows come first. An ASK exit releases n of A and pays the
    // maker's B profit. A BID exit releases the B of its custody and pays the
    // maker's B profit. The entry's custodies follow, with the old amounts in and
    // the new amounts out.
    *flows =
        if buy { vec![(a, n, 0), (bt, 0, calc.d_out)] } else { vec![(bt, state.custody - calc.out_amount, 0), (bt, 0, calc.d_out)] };
    push_entry_flows(flows, m, a, bt, a_new, b_new);
    let cont = IfdPairState {
        amount_left: add(entry_state.amount_left, n, "merged amount")?,
        custody: b_new,
        armed: entry_state.merged_armed(entry_daa),
        ..entry_state.clone()
    };
    extras.push(Extra::EntryCont(u(floor, "merged entry")?, cont.spk(), state.parent));
    let exit_custody_in = lay.at[&(i, Slot::Custody)] as i64;
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
        Arg::Int(exit_custody_in),
        Arg::Int(0),
    ];
    let plan = entry(TemplateId::KobIfdPair, entry_state.encode(), "fill", args);
    let role = format!(
        "KobIfdPair.fill.merge.{}{}{}",
        side_name(!buy),
        if new_custody { ".new" } else { "" },
        if sellout { ".sellout" } else { "" }
    );
    Ok((plan, role, e.utxo.clone()))
}

/// The custodies (A, B) a booked exit's merge leaves its entry.
///
/// The entry must be the exit's parent and book it, and the exit's re-arm (`proceeds`, `back`) must
/// be the entry's.
fn merged_entry_amounts(
    state: &CondPairState,
    merged_entry: &OrderUtxo<IfdPairState>,
    n: i64,
    proceeds: i64,
    back: i64,
) -> Result<(i64, i64)> {
    let entry_state = &merged_entry.state;
    if merged_entry.utxo.covenant_id != Some(state.parent) {
        return invalid("the merged entry is not this exit's parent");
    }
    if !entry_state.books_exit(state.parent, state) {
        return invalid("the exit is not one the merged entry books (its terms or repeat fields differ)");
    }
    let buy = entry_state.is_buy_first();
    let (a_new, b_new) = if buy {
        let budget = need(entry_state.merge_budget(n), "merge budget")?;
        if budget != proceeds || budget <= 0 {
            return invalid("the exit's re-arm budget is not the entry's");
        }
        (0, add(entry_state.custody, budget, "merged escrow")?)
    } else {
        let pre = need(entry_state.pre_of(n), "merge prefund")?;
        if pre != back {
            return invalid("the exit's prefund return is not the entry's");
        }
        (add(entry_state.amount_left, n, "merged amount")?, add(entry_state.custody, pre, "merged prefund")?)
    };
    Ok((a_new, b_new))
}

/// Appends a merged entry's custody flows: the old amounts in and the new amounts out.
fn push_entry_flows(flows: &mut Vec<(Token, i64, i64)>, m: &PairEntryMerge, a: Token, bt: Token, a_new: i64, b_new: i64) {
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
}

/// Plans a `KobIfdPair` entry fill, which delivers into the custody of the exit it creates.
#[allow(clippy::too_many_arguments)]
fn plan_ifd_pair(
    cx: &PairCtx,
    order: &OrderUtxo<IfdPairState>,
    a_custody: Option<&TokenUtxo>,
    b_custody: Option<&TokenUtxo>,
    n: i64,
    evidence: Option<usize>,
    evidence_b: Option<usize>,
    calc: &IfdCalc,
    suffix: String,
) -> Result<LegPlan> {
    let PairCtx { batch, lay, index: i, token_a: a, token_b: bt, lock, .. } = *cx;
    let state = &order.state;
    let buy = state.is_buy_first();
    let a_held = check_entry_custodies(order, a_custody, b_custody, a, bt)?;
    let mut ev_args = [0i64; 4];
    if calc.trigger {
        let (ev, args) = pair_evidence(
            batch,
            lay,
            evidence,
            evidence_b,
            !buy,
            a,
            bt,
            state.a_scale,
            state.b_scale,
            state.min_touch,
            state.min_touch_b(),
            state.min_rest_daa,
            lock,
        )?;
        if state.arms(ev) != Some(true) {
            return invalid(if buy {
                "a pair buy-stop entry arms only when the rate rose to its trigger or above"
            } else {
                "a pair sell-stop entry arms only when the rate fell to its trigger or below"
            });
        }
        ev_args = args;
    }
    let extras = ifd_pair_extras(cx, order, a_custody, b_custody, a_held, n, calc)?;
    let (xtok, x_ext) = if buy { (a, state.a_ext) } else { (bt, state.b_ext) };
    let x_ext = kcc20_ext(xtok, x_ext);
    kron_output(xtok.1.family(), calc.x_cust, "the exit's custody")?;
    let c_tpl = template(TemplateId::KobCondPair);
    let a_in = lay.at.get(&(i, Slot::A)).map(|x| *x as i64).unwrap_or(0);
    let b_in = lay.at.get(&(i, Slot::B)).map(|x| *x as i64).unwrap_or(0);
    let a_tpl = if buy || calc.a_new > 0 { first_in(lay, a, "pair entry (A template)")? } else { 0 };
    let b_tpl = if !buy || calc.b_new > 0 { first_in(lay, bt, "pair entry (B template)")? } else { 0 };
    let role = format!(
        "KobIfdPair.fill.{}{}{}{}.{}",
        side_name(!buy),
        arm_tag(calc.trigger, ev_args[3]),
        if state.entry_stop > 0 && state.armed != 0 && state.band_daa > 0 { ".auction" } else { "" },
        if calc.booked { ".book" } else { "" },
        entry_branch(state.amount_left - n > 0, calc.cont)
    );
    let flows = if buy {
        vec![(bt, calc.amt, 0), (a, 0, n)]
    } else {
        vec![(a, n, 0), (bt, state.custody - calc.b_new, 0), (bt, 0, calc.x_cust)]
    };
    Ok(LegPlan {
        suffix,
        pos: PosOut::DeliverToExit {
            value: u(state.delivery_carrier, "delivery carrier")?,
            amount: calc.x_cust,
            ext: x_ext,
            token: xtok,
        },
        role,
        sequence: 0,
        plan: entry(
            TemplateId::KobIfdPair,
            state.encode(),
            "fill",
            vec![
                nb(n),
                Arg::Int(a_in),
                Arg::Int(b_in),
                Arg::Int(a_tpl),
                Arg::Int(b_tpl),
                Arg::Int(0),
                Arg::Int(calc.amt),
                Arg::Bytes(c_tpl.prefix.clone()),
                Arg::Bytes(c_tpl.suffix.clone()),
                Arg::Int(ev_args[0]),
                Arg::Int(ev_args[1]),
                Arg::Int(ev_args[2]),
                Arg::Int(ev_args[3]),
                Arg::Int(calc.t_arg),
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

/// Checks that a pair entry's custodies hold exactly what the entry holds, and returns its A.
fn check_entry_custodies(
    order: &OrderUtxo<IfdPairState>,
    a_custody: Option<&TokenUtxo>,
    b_custody: Option<&TokenUtxo>,
    a: Token,
    bt: Token,
) -> Result<i64> {
    let state = &order.state;
    let buy = state.is_buy_first();
    let a_held = if buy { 0 } else { state.amount_left };
    match (a_custody, a_held > 0) {
        (Some(x), true) => {
            check_custody(x, order.utxo.covenant_id, a, a_held)?;
            check_custody_ext(x, state.a_ext)?;
        }
        (None, false) => {}
        (Some(_), false) => return invalid("this entry holds no A custody"),
        (None, true) => return invalid("a sell-first entry's A custody is required"),
    }
    match (b_custody, state.custody > 0) {
        (Some(x), true) => {
            check_custody(x, order.utxo.covenant_id, bt, state.custody)?;
            check_custody_ext(x, state.b_ext)?;
        }
        (None, false) => {}
        (Some(_), false) => return invalid("this entry holds no B custody"),
        (None, true) => return invalid("the entry's B custody is required"),
    }
    Ok(a_held)
}

/// The extras of a pair entry fill.
///
/// A fill that continues keeps the entry, the custody rests and a new exit. A closing fill leaves
/// the exit and returns the rest of the B custody to the maker.
fn ifd_pair_extras(
    cx: &PairCtx,
    order: &OrderUtxo<IfdPairState>,
    a_custody: Option<&TokenUtxo>,
    b_custody: Option<&TokenUtxo>,
    a_held: i64,
    n: i64,
    calc: &IfdCalc,
) -> Result<Vec<Extra>> {
    let PairCtx { token_a: a, token_b: bt, .. } = *cx;
    let state = &order.state;
    let order_value = order.utxo.amount as i64;
    let a_carrier = a_custody.map(|x| x.utxo.amount as i64).unwrap_or(0);
    let b_carrier = b_custody.map(|x| x.utxo.amount as i64).unwrap_or(0);
    let mut extras = vec![];
    let base = order_value - state.delivery_carrier - state.exit_carrier - calc.tip_kas;
    if calc.cont {
        let mut keep = base;
        if a_held > 0 {
            if calc.a_new > 0 {
                let x = a_custody.expect("checked");
                extras.push(Extra::AtInput {
                    slot: Slot::A,
                    value: x.utxo.amount,
                    state: x.state.with_amount(calc.a_new),
                    token: a,
                    patch: None,
                });
            } else {
                keep += a_carrier;
            }
        }
        if state.custody > 0 {
            if calc.b_new > 0 {
                let x = b_custody.expect("checked");
                extras.push(Extra::AtInput {
                    slot: Slot::B,
                    value: x.utxo.amount,
                    state: x.state.with_amount(calc.b_new),
                    token: bt,
                    patch: None,
                });
            } else {
                keep += b_carrier;
            }
        }
        let utxo_daa = order.utxo.block_daa_score as i64;
        let next = IfdPairState {
            amount_left: state.amount_left - n,
            custody: calc.b_new,
            armed: state.next_armed(utxo_daa),
            rpt_amount: if calc.booked { state.rpt_amount - n } else { state.rpt_amount },
            ..state.clone()
        };
        extras.push(Extra::Cont(kas_out(keep, "entry continuation")?, next.spk()));
        extras.push(Extra::Exit {
            value: u(state.exit_carrier, "exit carrier")?,
            spk: calc.exit.spk(),
            tpl: TemplateId::KobCondPair,
            arg: 5,
        });
    } else {
        let mut left = order_value + a_carrier - state.delivery_carrier - calc.tip_kas;
        if calc.b_new > 0 {
            let x = b_custody.expect("checked");
            let st = x.state.with_amount(calc.b_new).with_user_owner(state.maker);
            extras.push(Extra::AtInput { slot: Slot::B, value: x.utxo.amount, state: st, token: bt, patch: None });
        } else {
            left += b_carrier;
        }
        extras.push(Extra::Exit {
            value: kas_out(left, "exit value")? as u64,
            spk: calc.exit.spk(),
            tpl: TemplateId::KobCondPair,
            arg: 5,
        });
    }
    Ok(extras)
}

/// Plans one [`BatchUpdate`] of a pair conditional or a pair stop entry.
///
/// The update is `settle` or `fill` with `nb = 0` and `upd = 1`. It arms (k = 0) or trails (k >= 1,
/// the maximal k the evidence justifies). The evidence is `evidence` and `evidenceB` in mode 0, a
/// KAS-book leg of A and one of B, or `evidence` alone in mode 1, a KobPair leg of the pair.
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
            argv.extend([Arg::Int(1), Arg::Int(k), Arg::Int(0)]);
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

/// The token inputs of a pair refund, after the order at input 0.
pub(crate) struct RefundInputs {
    /// The bought-custody's token and amount (mode 1, `bought > 0`): input 1, released at output 1.
    pub bought: Option<(Token, i64)>,
    /// The input index of the sell custody (`KobPair`, `KobCondPair`): 1, or 2 after a bought-custody.
    pub custody_in: usize,
}

/// The token inputs of the refund of pair order `s`.
///
/// INTEGRATOR (`build_refund_pair`): with `bought` the refund takes the bought-custody
/// (`RefundOrder::bought_custody`, checked with [`check_refund_bought`]) as input 1 before the sell
/// custody, and adds its release ([`refund_release`]) as output 1 after the sell custody's refund.
pub(crate) fn refund_inputs(s: &AnyState) -> RefundInputs {
    let bought = match s {
        AnyState::KobPair(_) | AnyState::KobCondPair(_) => s.bought_custody().and_then(|b| {
            let (a, bt) = pair_programs(s).ok()?;
            let t_cov = match s {
                AnyState::KobPair(p) => p.t_cov_id,
                AnyState::KobCondPair(c) => c.t_cov_id,
                _ => unreachable!("pair kinds"),
            };
            Some((if t_cov == a.0 { a } else { bt }, b))
        }),
        _ => None,
    };
    RefundInputs { custody_in: if bought.is_some() { 2 } else { 1 }, bought }
}

/// Checks the bought-custody a pair refund releases.
#[allow(dead_code)] // build_refund_pair calls it once RefundOrder carries the bought-custody (B3 patch)
pub(crate) fn check_refund_bought(s: &AnyState, order_cov: [u8; 32], c: &TokenUtxo) -> Result<()> {
    let Some((t, bought)) = refund_inputs(s).bought else {
        return invalid("this order holds no bought-custody (a refund never moves strays)");
    };
    let ext = match s {
        AnyState::KobPair(p) => pair_terms(p).t_ext,
        AnyState::KobCondPair(c) => cond_terms(c).t_ext,
        _ => unreachable!("refund_inputs"),
    };
    check_bought_custody(c, t, order_cov, bought, ext)
}

/// The release of a pair refund's bought-custody: the maker's plain UTXO of the whole custody,
/// with its KAS (value, state).
#[allow(dead_code)] // build_refund_pair calls it once RefundOrder carries the bought-custody (B3 patch)
pub(crate) fn refund_release(s: &AnyState, c: &TokenUtxo) -> (u64, TokenState) {
    (c.utxo.amount, c.state.with_user_owner(s.maker()))
}

/// The signing plan and the role of the refund of a pair order.
///
/// Anyone may refund after the order's expiry, 90 days idle or an IOC / FOK kill.
///
/// A `KobPair` / `KobCondPair` holding a bought-custody (mode 1, `bought > 0`) releases it whole to
/// the maker at its own input index (bid-delivery-spec §5): the bought-custody is input 1 and the
/// sell custody input 2 ([`refund_inputs`]), so the outputs are the sell custody's refund at 0 and
/// the release at 1.
pub(crate) fn refund_plan(s: &AnyState, kill: bool) -> Result<(SigPlan, String)> {
    let (a, b) = pair_programs(s)?;
    let suffix = pair_suffix(a, b);
    let st = s.encode();
    let inputs = refund_inputs(s);
    let cust_in = inputs.custody_in as i64;
    let (t_in, tag) = if inputs.bought.is_some() { (1, ".m1c") } else { (0, "") };
    Ok(match s {
        AnyState::KobPair(p) => (
            entry(
                TemplateId::KobPair,
                st,
                "settle",
                vec![nb(0), Arg::Int(cust_in), Arg::Int(t_in), Arg::Int(0), Arg::Int(0), Arg::Int(0), Arg::Int(0)],
            ),
            format!("KobPair.refund{}.{}{tag}{suffix}", if kill { ".kill" } else { "" }, side_name(p.is_ask())),
        ),
        AnyState::KobCondPair(c) => {
            let mut args = vec![nb(0), Arg::Int(cust_in), Arg::Int(t_in)];
            args.extend([0i64; 11].map(Arg::Int));
            (entry(TemplateId::KobCondPair, st, "settle", args), format!("KobCondPair.refund.{}{tag}{suffix}", side_name(c.is_ask())))
        }
        AnyState::KobIfdPair(i) => {
            // The custodies are at input 1, and at input 2 when there are two. The A custody
            // (sell-first) comes first, then the B custody.
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
