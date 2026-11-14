//! Transaction builders for every user action and every matcher shape.
//!
//! Each builder takes a serde request, the same JSON the wasm API accepts. It lays out the inputs
//! and outputs exactly as the covenants require, and hands the draft to the fee pass
//! ([`crate::tx::Draft::seal`]). Amounts are in base units.
//!
//! # Layout rules
//!
//! - **Anti-aliasing.** An order spent at input *i* is paid, delivered or refunded at output *i*.
//!   Order inputs therefore come first, and outputs `0..n` are their positional outputs. Extras
//!   follow: continuations, token remainders, exits, merged entries, update continuations, payments
//!   and the taker's tokens. The change output is always last.
//! - **One leader per token.** Token inputs follow the order inputs. The first input of each token
//!   is its leader and authorises every output of that token. In KCC-20 its `next_states` list them
//!   in output order, and the other inputs are delegators. In KRON every input carries the same
//!   next-state columns and a witness byte naming its authorising input. A transaction may trade
//!   several tokens, as two-token routes and pair orders do, and each token has its own leader and
//!   slot limits.
//! - **Token families.** The family of a leg is the family of its order's token program (`kcc20` or
//!   `kron`, see [`crate::family`]). Every layout rule, entry argument and economic rule is the
//!   same. Only the templates, the token states and the token input authorisation differ. KRON
//!   tokens held by a key are authorised by *address presence*: a P2PK input of that key must be in
//!   the transaction, as funding.
//! - **Exact custody, no strays.** An ask-side custody must hold exactly `amountLeft` base units
//!   and be owned by its order. Taker tokens must be key-owned. A transaction carries at most
//!   [`Family::max_tok_in`] inputs of a token whose orders it spends. Only the maker's cancel
//!   sweeps strays.
//! - **Amounts.** Every leg names `amount`, the base units it fills. Every KAS amount a covenant
//!   checks is computed with the exact per-kind helpers of [`crate::state`], which round in the
//!   maker's favour. Every builder refuses a fill the covenant refuses: below the minimum fill
//!   (`minFill`), above the TWAP / DCA `maxFill`, a price below its tip, or an arithmetic overflow.
//! - **Makers get exactly their bound.** Asks are paid exactly their all-in minimum, bid
//!   continuations carry exactly the unused budget, and merged entries get back exactly what their
//!   covenants require. Everything else, the spread and the tips, goes to the change output, i.e.
//!   to the matcher or taker. The fee pass never touches a covenant output, except the maker's own
//!   new order of an amend, on which a tiny change or the fee of an in-place amend rides
//!   ([`absorber_for`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::artifacts::{template, token_template_by_hash, TemplateId};
use crate::error::{invalid, Error, Result};
use crate::family::Family;
use crate::payload::{self, Custody, Record};
use crate::registry::KRON_MAX_OUTPUT_AMOUNT;
use crate::script::p2pk_spk;
use crate::state::*;
use crate::tx::{
    check_key, spk_from_string, Arg, BuiltTx, Draft, FeeOptions, KeyUtxo, OrderUtxo, SigPlan, TokenUtxo, Utxo, Witness,
    SEQUENCE_NONFINAL,
};

mod intent;
mod pair;
#[cfg(feature = "engine")]
pub use intent::execute_intent;
pub use intent::{
    build_cancel_intent, build_create_intent, build_execute_intent, build_expire_intent, default_max_extra, execution_fee_mass_bound,
    intent_budgets, kas_to_token_extra_needed, kas_to_token_fillers, router_role, CancelIntent, CreateIntent, ExecuteIntent,
    ExecutionFacts, ExpireIntent, IntentAsk, IntentBid, INTENT_OUTPUT, LOCK_OUTPUT, MAX_EXTRA_MARGIN, MAX_EXTRA_RATE_FACTOR,
    REFERENCE_FILLER, ROUTER_CANCEL_BUDGET, ROUTER_EXPIRE_BUDGET, ROUTER_FILL_BUDGET_FALLBACK,
};
pub use pair::{pair_programs, pair_suffix};

/// Budget lookup used by the builders (the committed table by default).
pub type BudgetFn<'a> = &'a dyn Fn(&str) -> Result<u16>;

fn table_budgets() -> impl Fn(&str) -> Result<u16> {
    crate::budget::lookup
}

fn u(v: i64, what: &str) -> Result<u64> {
    if v < 0 {
        return invalid(format!("{what} would be negative ({v})"));
    }
    Ok(v as u64)
}

fn pos(v: i64, what: &str) -> Result<u64> {
    if v <= 0 {
        return invalid(format!("{what} must be positive ({v})"));
    }
    Ok(v as u64)
}

/// Checks a KAS amount that a fill or a new order puts on an output with a covenant id.
///
/// Such outputs are a pair order's continuation, its exit order UTXO and the carrier of its
/// delivery, or a new order's value and custody carrier. KIP-9 dust is refused here, naming the
/// output. `seal` would refuse the whole transaction for it without knowing which leg made it,
/// because the output's storage mass alone exceeds the block limit (two storage units,
/// [`crate::tx::COVENANT_OUTPUT_MIN`]).
fn kas_out(v: i64, what: &str) -> Result<u64> {
    if v < crate::tx::COVENANT_OUTPUT_MIN as i64 {
        return invalid(format!(
            "{what} of {v} sompi is below {} sompi: its KIP-9 storage mass alone exceeds the block limit",
            crate::tx::COVENANT_OUTPUT_MIN
        ));
    }
    Ok(v as u64)
}

/// The fill bound of the repeat entries ([`rpt_fill_max`]).
///
/// A fill of more than the re-arms left, while at least one minimum fill of them is left, would
/// leave the rest unused for good, and the covenants refuse it.
fn rpt_fill_rule(rpt_amount: i64, min_fill: i64, n: i64) -> Result<()> {
    match rpt_fill_max(rpt_amount, min_fill) {
        Some(m) if n > m => invalid(format!(
            "a repeating entry fills at most the {m} base units of re-arms it has left while they are at least one minimum fill"
        )),
        _ => Ok(()),
    }
}

/// A covenant amount the builder computed with a `state` helper.
///
/// `None` is where the covenant fails, on an overflow or a price below its tip, so the builder
/// refuses the fill.
fn need(v: Option<i64>, what: &str) -> Result<i64> {
    v.ok_or_else(|| Error::Invalid(format!("{what}: the covenant arithmetic fails here (an overflow, or a price below its tip)")))
}

/// Checked `a + b` / `a - b` of builder amounts.
fn add(a: i64, b: i64, what: &str) -> Result<i64> {
    a.checked_add(b).ok_or_else(|| Error::Invalid(format!("{what} overflows")))
}
fn sub(a: i64, b: i64, what: &str) -> Result<i64> {
    a.checked_sub(b).ok_or_else(|| Error::Invalid(format!("{what} overflows")))
}

/// Fill quantity argument: a fixed 8-byte little-endian push (`nb`).
fn nb(n: i64) -> Arg {
    Arg::Bytes(n.to_le_bytes().to_vec())
}

/// Repeat merge argument `-(k · 2^53 + m)` as a fixed 8-byte script number.
///
/// The number is sign-magnitude and little endian. The booked exit at input `k` (`k <= 1023`) takes
/// profit on `m` base units (`0 < m < 2^53`).
pub fn merge_arg(k: usize, m: i64) -> Result<Vec<u8>> {
    if !(1..MERGE_SHIFT).contains(&m) || k > MERGE_MAX_INDEX {
        return invalid("merge: the amount must be within 1..2^53 and the exit index at most 1023");
    }
    let mut b = (k as i64 * MERGE_SHIFT + m).to_le_bytes();
    b[7] |= 0x80;
    Ok(b.to_vec())
}

fn entry(t: TemplateId, state: Vec<u8>, name: &str, args: Vec<Arg>) -> SigPlan {
    SigPlan::Entry { template: t, state, entry: name.to_string(), args }
}

/// Resolves the token program (either family) of an order's token fields.
pub fn token_program(tpl_hash: &[u8; 32], prefix_len: i64, suffix_len: i64) -> Result<TemplateId> {
    let t = token_template_by_hash(tpl_hash).ok_or_else(|| {
        Error::Invalid(format!("token template {} is not a supported KCC-20 or KRON program", crate::json::to_hex(tpl_hash)))
    })?;
    if t.prefix.len() as i64 != prefix_len || t.suffix.len() as i64 != suffix_len {
        return invalid("token prefix/suffix lengths do not match the token program");
    }
    Ok(t.id)
}

type Token = ([u8; 32], TemplateId);

/// The token of an order and its program, for a pair order its base token A.
///
/// It also checks that the order kind is of the family of that program, e.g. a `KobBidKron` trades
/// a KRON token, and that the state is consistent with the family.
fn order_token(s: &AnyState) -> Result<Token> {
    s.validate()?;
    let (p, x) = s.token_tpl_lens();
    let program = token_program(&s.token_tpl_hash().expect("orders pin a token program"), p, x)?;
    if program.family() != s.family() {
        return invalid(format!(
            "{} trades {:?} tokens but the order's token program {} is of the {:?} family",
            s.template_id().name(),
            s.family(),
            program.name(),
            program.family()
        ));
    }
    Ok((s.token_cov_id(), program))
}

/// The token programs an existing order pins, resolved as every builder that spends the order does.
///
/// Each is a supported program whose template hash and prefix and suffix lengths the state names,
/// of the family the order kind trades. A pair order names both tokens, each of the family its
/// state names, and the two must differ. Returns the program of the order's token (for a pair
/// order, of A) and, for a pair order, the program of B.
///
/// These builders can never fill, refund or close an order that fails this, whatever else its
/// numbers say. The executor's sanity gate applies it before an order is listed or planned.
pub fn order_programs(s: &AnyState) -> Result<(TemplateId, Option<TemplateId>)> {
    if s.is_pair() {
        let (a, b) = pair::pair_programs(s)?;
        return Ok((a.1, Some(b.1)));
    }
    Ok((order_token(s)?.1, None))
}

/// A refusal of the batch builder that depends on the order of leg `i` alone.
///
/// Such a refusal concerns its token programs or its family. The message names the leg
/// (`order of leg <i>: ...`), so a caller that planned the batch can tell which order to leave out.
fn leg_order_error(i: usize, e: Error) -> Error {
    match e {
        Error::Invalid(m) if attributed(&m) => Error::Invalid(m),
        Error::Invalid(m) => Error::Invalid(format!("order of leg {i}: {m}")),
        Error::State(m) => Error::Invalid(format!("order of leg {i}: {m}")),
        other => other,
    }
}

/// A refusal of the batch builder of the fill a pair leg `i` asks for.
///
/// Such a refusal concerns its quantity, its continuation or its evidence. The message names the
/// leg (`fill of leg <i>: ...`), so a caller that planned the batch can leave that fill out and
/// keep the other legs. Unlike [`leg_order_error`] it depends on what the plan chose, not on the
/// order alone.
fn leg_fill_error(i: usize, e: Error) -> Error {
    match e {
        Error::Invalid(m) if attributed(&m) => Error::Invalid(m),
        Error::Invalid(m) => Error::Invalid(format!("fill of leg {i}: {m}")),
        Error::State(m) => Error::Invalid(format!("fill of leg {i}: {m}")),
        other => other,
    }
}

/// A refusal of leg `i` that another leg or order of the batch causes.
///
/// The message names that one instead, so a caller leaves out the order at fault and not the leg
/// that reads it. `evidence of leg <k>` names the evidence leg's own state, and `entry of leg <k>`
/// the custodies that the fill of entry leg `k` merges.
fn blame(prefix: &str, k: usize, e: Error) -> Error {
    match e {
        Error::Invalid(m) if !attributed(&m) => Error::Invalid(format!("{prefix} of leg {k}: {m}")),
        Error::State(m) => Error::Invalid(format!("{prefix} of leg {k}: {m}")),
        other => other,
    }
}

/// Whether a refusal already names the leg at fault.
fn attributed(m: &str) -> bool {
    ["order of leg ", "fill of leg ", "evidence of leg ", "entry of leg "].iter().any(|p| m.starts_with(p))
}

/// Family of the supported token program whose template hash is `h`.
fn family_of_token_hash(h: &[u8; 32]) -> Result<Family> {
    token_template_by_hash(h)
        .map(|t| t.family)
        .ok_or_else(|| Error::Invalid(format!("token template {} is not a supported KCC-20 or KRON program", crate::json::to_hex(h))))
}

/// The KRON token programs refuse an output above [`KRON_MAX_OUTPUT_AMOUNT`] base units.
fn kron_output(fam: Family, amount: i64, what: &str) -> Result<()> {
    if fam == Family::Kron && amount > KRON_MAX_OUTPUT_AMOUNT {
        return invalid(format!("{what}: a KRON token output holds at most {KRON_MAX_OUTPUT_AMOUNT} base units (got {amount})"));
    }
    Ok(())
}

/// Protocol sanity of a new order.
///
/// It checks the rules that would otherwise make the order unfillable or unrefundable, and the
/// normative wallet rules of `docs/spec/matcher.md` §10 that the covenants cannot check. The
/// covenants enforce the rest at fill time, and builders refuse to create duds. It also applies the
/// numeric gate ([`AnyState::check_numbers`]): a power-of-ten scale up to 10^9, and every full fill
/// worth less than 2^62.
pub fn check_new_order(s: &AnyState) -> Result<()> {
    if s.is_pair() {
        return pair::check_new_pair(s);
    }
    let fam = s.family();
    // Family and extension-commitment consistency of the order and its token program.
    let program = order_token(s)?.1;
    s.check_numbers().map_err(Error::Invalid)?;
    // Every carrier of a new order becomes an output with a covenant id. A token output of
    // `program`, i.e. a delivery or a new custody, must clear the program's floor and the KIP-9
    // bound of a covenant output (`tx::COVENANT_OUTPUT_MIN`). An exit order UTXO must clear the
    // covenant output bound.
    let token_carrier = |what: &str, v: i64, program: TemplateId| -> Result<()> {
        let floor = program.min_token_output().unwrap_or(0).max(crate::tx::COVENANT_OUTPUT_MIN);
        if v < floor as i64 {
            return invalid(format!(
                "{what} {v} sompi is below {floor} sompi: the least KAS a token output of {} can carry (program floor, and the KIP-9 bound of a covenant output, {} sompi)",
                program.name(),
                crate::tx::COVENANT_OUTPUT_MIN
            ));
        }
        Ok(())
    };
    let kas_carrier = |what: &str, v: i64| -> Result<()> {
        if v < crate::tx::COVENANT_OUTPUT_MIN as i64 {
            return invalid(format!(
                "{what} {v} sompi is dust: below {} sompi the KIP-9 storage mass of a covenant output alone exceeds the block limit",
                crate::tx::COVENANT_OUTPUT_MIN
            ));
        }
        Ok(())
    };
    check_key(&s.maker(), "order maker")?;
    let base = |min_fill: i64, tip: i64, refund_tip: i64| -> Result<()> {
        if min_fill <= 0 {
            return invalid("minFill must be positive (at least one base unit)");
        }
        if tip < 0 || refund_tip < 0 {
            return invalid("tip and refundTip must be >= 0");
        }
        // A KRON fill moves at most the program's output limit, so a minimum fill above it could
        // never fill partially.
        kron_output(fam, min_fill, "minFill")?;
        Ok(())
    };
    let timing = |slope: i64, step: i64, interval: i64, max_fill: i64| -> Result<()> {
        if slope < 0 || interval < 0 || max_fill < 0 {
            return invalid("slope, interval and maxFill must be >= 0");
        }
        if slope > 0 && step <= 0 {
            return invalid("a decaying / rising order needs decayStep > 0");
        }
        Ok(())
    };
    // With a TWAP / DCA cap below the minimum fill, every fill is either below minFill or above
    // maxFill, and the order never fills.
    let caps = |min_fill: i64, max_fill: i64| -> Result<()> {
        if max_fill > 0 && max_fill < min_fill {
            return invalid(format!("maxFill {max_fill} is below minFill {min_fill}: the order could never fill"));
        }
        Ok(())
    };
    let cond = |tp: i64, stop: i64, slip: i64, step: i64, armed: i64, band: i64, keeper: i64, amount: i64, touch: i64| -> Result<()> {
        if tp <= 0 && stop <= 0 {
            return invalid("a conditional order needs a take-profit or a stop leg");
        }
        if !(0..=10_000).contains(&slip) {
            return invalid("slipBps must be within 0..=10000");
        }
        if stop > MAX_STOP_PRICE {
            return invalid(format!(
                "stopPrice must be at most {MAX_STOP_PRICE} (the covenant band stopPrice * slipBps must fit in 63 bits)"
            ));
        }
        if step < 0 || armed != 0 {
            return invalid("trailStep must be >= 0 and a new order must not be armed");
        }
        if band < 0 || keeper < 0 || touch < 0 {
            return invalid("bandDaa, keeperTip and minTouch must be >= 0");
        }
        if amount <= 0 {
            return invalid("amountLeft must be positive");
        }
        Ok(())
    };
    let entry_rules = |stop: i64, band: i64, keeper: i64, armed: i64, rpt: i64, touch: i64| -> Result<()> {
        if stop < 0 || band < 0 || keeper < 0 || rpt < 0 || touch < 0 {
            return invalid("entryStop, bandDaa, keeperTip, rptAmount and minTouch must be >= 0");
        }
        if armed != 0 {
            return invalid("a new entry must not be armed");
        }
        Ok(())
    };
    // The trailing gap and wait and the trigger exposure must not be negative, as in the pair
    // builder (`check_new_cond_pair`). A negative gap ratchets a stop through the market, so it
    // arms at once. A negative wait or exposure means none.
    let trail_rules = |gap: i64, wait: i64, rest: i64| -> Result<()> {
        if gap < 0 || wait < 0 || rest < 0 {
            return invalid("trailGap, trailWait and minRestDaa must be >= 0");
        }
        Ok(())
    };
    match s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => {
            base(a.min_fill, a.tip, a.refund_tip)?;
            timing(a.slope, a.decay_step, a.interval, a.max_fill)?;
            caps(a.min_fill, a.max_fill)?;
            if !(0..=2).contains(&a.tif) || a.price <= 0 || a.amount_left <= 0 {
                return invalid("ask: tif 0..=2, price > 0, amountLeft > 0");
            }
            if a.price.min(if a.slope != 0 { a.price_end } else { a.price }) < a.tip {
                return invalid("ask: the price (and a decay's floor priceEnd) must not be below the tip");
            }
            kron_output(fam, a.amount_left, "the custody")?;
        }
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => {
            base(b.min_fill, b.tip, b.refund_tip)?;
            timing(b.slope, b.decay_step, b.interval, b.max_fill)?;
            caps(b.min_fill, b.max_fill)?;
            if !(0..=2).contains(&b.tif) || b.price <= 0 || b.reserve < 0 || b.delivery_carrier < 0 {
                return invalid("bid: tif 0..=2, price > 0, reserve and deliveryCarrier >= 0");
            }
            token_carrier("deliveryCarrier", b.delivery_carrier, program)?;
            need(b.used(b.min_fill), "bid: the budget of one minimum fill")?;
        }
        AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) => {
            base(c.min_fill, c.tip, c.refund_tip)?;
            cond(c.tp_price, c.stop_price, c.slip_bps, c.trail_step, c.armed, c.band_daa, c.keeper_tip, c.amount_left, c.min_touch)?;
            trail_rules(c.trail_gap, c.trail_wait, c.min_rest_daa)?;
            if c.is_booked() || c.rpt_price != 0 || c.rpt_until != 0 {
                return invalid("repeat fields are written by an if-done entry, never by a new order");
            }
            if (c.tp_price > 0 && c.tp_price < c.tip) || (c.stop_price > 0 && c.stop_floor() < c.tip) {
                return invalid("conditional ask: every leg price (the stop band's floor included) must not be below the tip");
            }
            kron_output(fam, c.amount_left, "the custody")?;
        }
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => {
            base(c.min_fill, c.tip, c.refund_tip)?;
            cond(c.tp_price, c.stop_price, c.slip_bps, c.trail_step, c.armed, c.band_daa, c.keeper_tip, c.amount_left, c.min_touch)?;
            trail_rules(c.trail_gap, c.trail_wait, c.min_rest_daa)?;
            if c.delivery_carrier < 0 {
                return invalid("deliveryCarrier must be >= 0");
            }
            token_carrier("deliveryCarrier", c.delivery_carrier, program)?;
            if c.is_booked() || c.rpt_price != 0 || c.rpt_pre != 0 || c.rpt_until != 0 {
                return invalid("repeat fields are written by an if-done entry, never by a new order");
            }
        }
        AnyState::KobIfdBid(i) | AnyState::KobIfdBidKron(i) => {
            base(i.min_fill, i.tip, i.refund_tip)?;
            entry_rules(i.entry_stop, i.band_daa, i.keeper_tip, i.armed, i.rpt_amount, i.min_touch)?;
            trail_rules(0, 0, i.min_rest_daa)?;
            if i.amount_left <= 0 || i.price <= 0 || i.delivery_carrier < 0 || i.exit_carrier < 0 {
                return invalid("if-done bid: amountLeft and price > 0, carriers >= 0");
            }
            if i.entry_stop > i.price {
                return invalid("a buy-stop entry's trigger must not be above its limit");
            }
            token_carrier("deliveryCarrier", i.delivery_carrier, program)?;
            kas_carrier("exitCarrier", i.exit_carrier)?;
            let e = i.exit()?;
            check_new_order(
                &AnyState::KobCondAsk(CondAskState { amount_left: i.min_fill.min(i.amount_left), ..e.clone() }).into_family(fam),
            )?;
            if e.maker != i.maker || e.token_cov_id != i.token_cov_id || e.token_tpl_hash != i.token_tpl_hash {
                return invalid("if-done exit must be the maker's order of the same token");
            }
            if e.scale != i.scale {
                return invalid("if-done exit must quote at the entry's scale");
            }
            if i.rpt_amount > 0 && (e.tp_price <= 0 || e.tp_price - e.tip <= i.price + i.tip) {
                return invalid("a repeating buy-first entry needs a take-profit that beats the entry (all-in)");
            }
        }
        AnyState::KobIfdAsk(i) | AnyState::KobIfdAskKron(i) => {
            base(i.min_fill, i.tip, i.refund_tip)?;
            entry_rules(i.entry_stop, i.band_daa, i.keeper_tip, i.armed, i.rpt_amount, i.min_touch)?;
            trail_rules(0, 0, i.min_rest_daa)?;
            if i.amount_left <= 0 || i.price <= 0 || i.prefund < 0 || i.exit_carrier < 0 {
                return invalid("if-done ask: amountLeft and price > 0, prefund and exitCarrier >= 0");
            }
            if i.price < i.tip {
                return invalid("a sell-first entry's price must not be below its tip (the covenant refuses every fill)");
            }
            if i.entry_stop > 0 && i.entry_stop < i.price {
                return invalid("a sell-stop entry's trigger must not be below its limit");
            }
            kron_output(fam, i.amount_left, "the custody")?;
            // A repeating entry that sold out takes its exitCarrier back as the carrier of a new
            // custody.
            token_carrier("exitCarrier", i.exit_carrier, program)?;
            let e = i.exit()?;
            check_new_order(
                &AnyState::KobCondBid(CondBidState { amount_left: i.min_fill.min(i.amount_left), ..e.clone() }).into_family(fam),
            )?;
            if e.maker != i.maker || e.token_cov_id != i.token_cov_id || e.token_tpl_hash != i.token_tpl_hash {
                return invalid("if-done exit must be the maker's order of the same token");
            }
            if e.scale != i.scale {
                return invalid("if-done exit must quote at the entry's scale");
            }
            // The exit holds the proceeds plus the prefund of its amount, which must cover its
            // worst leg. The rates are per whole token. The exit's spend is rounded down and the
            // entry's proceeds and prefund up, so the rates bound every fill.
            if i.proceeds_rate() + i.prefund < e.worst() + e.tip {
                return invalid("prefund does not cover the exit's worst buy-back (its stop band or limit)");
            }
            if i.exit_carrier < e.delivery_carrier {
                return invalid("exitCarrier must cover the exit's deliveryCarrier");
            }
            if i.rpt_amount > 0 && (e.tp_price <= 0 || i.proceeds_rate() <= e.tp_price + e.tip) {
                return invalid("a repeating sell-first entry needs a take-profit that beats the entry (all-in)");
            }
        }
        AnyState::KobPair(_) | AnyState::KobCondPair(_) | AnyState::KobIfdPair(_) => unreachable!("checked above"),
    }
    Ok(())
}

/// Smallest order value that keeps a new order fillable.
///
/// For a bid it is one minimum fill, for an if-done entry its whole escrow, and for a pair order
/// the KAS of its fills (`pair::min_pair_value`).
pub fn min_order_value(s: &AnyState) -> i64 {
    let sat = |v: Option<i64>| v.unwrap_or(i64::MAX);
    match s {
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => sat(b.escrow(b.min_fill, 1)),
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => sat(c.escrow(1)),
        AnyState::KobIfdBid(i) | AnyState::KobIfdBidKron(i) => sat(i.escrow()),
        AnyState::KobIfdAsk(i) | AnyState::KobIfdAskKron(i) => sat(i.escrow(1)),
        AnyState::KobPair(_) | AnyState::KobCondPair(_) | AnyState::KobIfdPair(_) => pair::min_pair_value(s),
        _ => 1,
    }
}
// ---------------------------------------------------------------- user actions

/// Create any order kind in one transaction.
///
/// The transaction holds the order genesis, authorised by the first funding input, and the `KOB1`
/// placement record. For token-holding kinds it also moves exactly `amountLeft` base units of the
/// maker's tokens into 0x04 custody of the new covenant id. A pair order moves each custody it
/// holds ([`AnyState::custodies`]) of its own token, and `tokens` may then hold both tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrder {
    pub order: AnyState,
    /// KAS on the order UTXO: the carrier (token kinds) or the escrow (bid kinds).
    #[serde(with = "crate::json::field")]
    pub value: u64,
    /// Token kinds: the maker's P2PK-owned token UTXOs to draw `amountLeft` base units from.
    #[serde(default)]
    pub tokens: Vec<TokenUtxo>,
    /// KAS on the custody token UTXO and on any token change.
    #[serde(with = "crate::json::field", default)]
    pub token_carrier: u64,
    /// P2PK funding; the first input authorises the genesis.
    pub funding: Vec<KeyUtxo>,
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(with = "crate::json::field", default)]
    pub lock_time: u64,
    /// Day orders: the wall-clock deadline, in UTC unix seconds, published in the placement record.
    ///
    /// See [`crate::defaults::day_order`].
    #[serde(with = "crate::json::field", default)]
    pub deadline: Option<u64>,
    /// Extra payload records (x402 reference, client note).
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Cancel an order (maker, SIGHASH_ALL), optionally replacing it in the same transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrder {
    pub order: OrderUtxo<AnyState>,
    /// The hash of the retired template the order is under, of the kind the state decodes as.
    ///
    /// Retired templates are in the cancel-only table (`crate::retired`). Only the maker's plain
    /// cancel is built for such an order ([`SigPlan::RetiredCancel`]), with no replacement, no
    /// top-up and no records.
    #[serde(with = "crate::json::field", default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<[u8; 32]>,
    /// Token-holding kinds: the custody token UTXO.
    ///
    /// For a pair order it is the first of [`AnyState::custodies`].
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    /// A sell-first `KobIfdPair`'s B prefund custody (the second of [`AnyState::custodies`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund: Option<TokenUtxo>,
    /// Stray token UTXOs owned by the order's covenant id, swept to the maker.
    ///
    /// The cancel is the only path that moves them. A pair order's strays may be of either of its
    /// tokens, A or B, and each token is swept in its own output.
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
    /// Foreign strays, returned to the maker in one output per token.
    ///
    /// These are tokens of other covenant ids owned by the order id (`matcher.md` §1.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignStrays>,
    /// Extra maker tokens (P2PK) for a replacement that needs more tokens (a top-up).
    #[serde(default)]
    pub tokens: Vec<TokenUtxo>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// Change key (default: the maker).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    /// Cancel-replace: the new order, created in the same transaction.
    #[serde(default)]
    pub replace: Option<Replacement>,
    #[serde(with = "crate::json::field", default)]
    pub lock_time: u64,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// The new order of a cancel-replace.
///
/// Token kinds take `amountLeft` base units from the old custody, the strays and the top-up tokens,
/// and the rest returns to the maker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    pub order: AnyState,
    #[serde(with = "crate::json::field")]
    pub value: u64,
    #[serde(with = "crate::json::field", default)]
    pub token_carrier: Option<u64>,
    #[serde(with = "crate::json::field", default)]
    pub deadline: Option<u64>,
}

/// Amend a plain ask in place, by the maker's SIGHASH_ALL `cancel`.
///
/// The order's covenant id continues with the new state at output 0, and the custody, owned by that
/// id, stays where it is. The token program is therefore never revealed, and an 8/8 ask amend
/// costs about a quarter of a cancel-replace (2,168 B against 8,818 B), and about 40% on the
/// reference program. Only the terms [`payload::check_amend_terms`] allows may change:
/// price, tip, minimum fill, time in force, timing and decay. The maker, the token, the scale and
/// `amountLeft` cannot, because they re-custody; use [`CancelOrder::replace`] for them. The `KOB1`
/// payload carries an `AMEND` record.
///
/// A plain bid is amended the same way. It owns no tokens and its quantity is its escrow, so it
/// keeps the maker, the token and the scale and may change every other term. The continuation must
/// still fund one minimum fill at the new terms ([`min_order_value`]), and a larger escrow (a
/// top-up) needs `funding` and `value`.
///
/// Fee:
/// - Without `funding`, the order's own carrier pays it, for a bid its escrow. The continuation
///   holds the order's value minus the fee, at least [`MIN_AMEND_CARRIER`] and, for a bid, the
///   one-minimum-fill floor.
/// - With funding, the continuation holds `value`, by default the order's value. The change returns
///   to the maker unless it is tiny, and then it rides on the order (see [`absorber_for`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmendOrder {
    pub order: OrderUtxo<AnyState>,
    /// The amended state (same template).
    pub amended: AnyState,
    /// KAS on the continuation (with funding; default: the order UTXO's value).
    #[serde(with = "crate::json::field", default)]
    pub value: Option<u64>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// Change key (default: the maker).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(with = "crate::json::field", default)]
    pub lock_time: u64,
    /// Day orders: the wall-clock deadline published in the record.
    #[serde(with = "crate::json::field", default)]
    pub deadline: Option<u64>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Least KAS an in-place amend that pays its fee from the order's carrier leaves on the order.
///
/// It is 0.1 KAS, and never less than the order's `refundTip`, which a refund keeper takes from the
/// carriers. A smaller carrier needs funding.
pub const MIN_AMEND_CARRIER: u64 = 10_000_000;

/// Refund an order after its soft expiry, 90 days idle or its IOC / FOK kill time.
///
/// Anyone may refund, and the keeper keeps at most `refundTip`. An empty repeating `KobIfdAsk` is
/// refunded by `close`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefundOrder {
    pub order: OrderUtxo<AnyState>,
    /// The custody (a pair order: the first of [`AnyState::custodies`]).
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    /// A sell-first `KobIfdPair`'s B prefund custody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund: Option<TokenUtxo>,
    /// Foreign strays (`matcher.md` §1.2) the refund returns to the maker, one output per token.
    ///
    /// Their programs authorise them with any spend of the order, and no refund path reads another
    /// token. A keeper may therefore carry them out of an order that is about to end, after which
    /// nothing could ever move them. The order's own token is never one of them, and the refund
    /// refuses it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignStrays>,
    /// Current DAA score; must be >= the order's refund time ([`AnyState::refund_due`]).
    #[serde(with = "crate::json::field")]
    pub lock_time: u64,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// Keeper change key; without funding and change the whole refund tip is the fee.
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Stray UTXOs of one token other than the order's own, all owned by the order's covenant id.
///
/// These are foreign strays (`matcher.md` §1.2). The token's program comes with them, because the
/// builders need it to move them. They are swept to the maker in one output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignStrays {
    pub token: TokenRef,
    pub utxos: Vec<TokenUtxo>,
}

/// The maker's sweep of an order's strays in place, by the maker's SIGHASH_ALL `cancel`.
///
/// The order's covenant id continues with the same state, i.e. the same script, at output 0, and
/// its custody, owned by that id, stays where it is. The strays go back to the maker, one output
/// per token. They may be of the order's own token, of either token of a pair order, or foreign.
///
/// A `SWEEP` record (`kob1-payload.md`) names the continuation, so indexers keep the order live.
/// Nothing about the order changes except its UTXO, whose DAA score restarts the 90-day idle
/// window, as any continuation does. Strays of one token must share one extension commitment and
/// fit the program's token inputs. The rest can go in another sweep, since the order is still
/// there.
///
/// Fee: without `funding` the order's own carrier pays it, and the continuation keeps at least
/// [`MIN_AMEND_CARRIER`] and the order's `refundTip`. A bid keeps its whole escrow, so a bid's
/// sweep needs funding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepOrder {
    pub order: OrderUtxo<AnyState>,
    /// Strays of the order's own token (a pair order: of either of its tokens).
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
    #[serde(default)]
    pub foreign: Vec<ForeignStrays>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// Change key (default: the maker).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    /// Carrier of each swept token output (default: the first stray's).
    #[serde(with = "crate::json::field", default)]
    pub token_carrier: Option<u64>,
    #[serde(with = "crate::json::field", default)]
    pub lock_time: u64,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Plain token transfer from key owners (the CLI `send`; either family).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTokens {
    pub token: TokenRef,
    pub tokens: Vec<TokenUtxo>,
    pub recipients: Vec<TokenRecipient>,
    /// Owner of the token change (default: owner of the first token input).
    #[serde(with = "crate::json::field", default)]
    pub token_change: Option<[u8; 32]>,
    #[serde(with = "crate::json::field", default)]
    pub token_change_carrier: u64,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    /// Extra payload records (an x402 payment commitment, client note). Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// A token: covenant id and program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRef {
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    pub program: TemplateId,
}

/// One token recipient.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecipient {
    #[serde(with = "crate::json::field")]
    pub pubkey: [u8; 32],
    #[serde(with = "crate::json::field")]
    pub amount: i64,
    #[serde(with = "crate::json::field")]
    pub carrier: u64,
}

/// A plain KAS payment output (swap-and-pay, x402).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payment {
    /// Script public key in the transaction JSON form (`version ‖ script`, hex).
    pub script_public_key: String,
    #[serde(with = "crate::json::field")]
    pub amount: u64,
}

/// Receiver of a token's net output in a batch (default: the batch's taker).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPayee {
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    #[serde(with = "crate::json::field")]
    pub pubkey: [u8; 32],
}

/// How a token input of `program` is authorised.
///
/// A covenant-owned state is authorised by its covenant's input. A key-held state with the
/// program's holder owner is authorised by its key: in KCC-20 and KRON id_type 0 by a signature in
/// the input, in KRON id_type 3 by a P2PK input of the key.
fn token_witness(s: &TokenState, program: TemplateId) -> Result<Witness> {
    match s {
        TokenState::Kcc20(k) => match k.owner_scheme {
            SCHEME_P2PK => Ok(Witness::P2pk(k.owner)),
            SCHEME_COVID => Ok(Witness::CovenantId),
            other => invalid(format!("token owner scheme {other:#04x} is not supported by the builders")),
        },
        TokenState::Kron(k) if k.id_type == crate::family::KRON_TYPE_COVID => Ok(Witness::CovenantId),
        TokenState::Kron(k) if s.is_holder(program) => Ok(Witness::P2pk(k.owner)),
        TokenState::Kron(k) => {
            let holder = crate::state::HolderOf::Program(program).owner_type();
            invalid(format!(
                "KRON tokens of id_type {} are not spendable under {} (its holders are id_type {holder}{})",
                k.id_type,
                program.name(),
                if holder == crate::family::KRON_TYPE_ADDR { ", address presence: move them to such a UTXO first" } else { "" }
            ))
        }
    }
}

fn check_token_input(t: &TokenUtxo, token: Token) -> Result<()> {
    if t.utxo.covenant_id != Some(token.0) {
        return invalid("token UTXO is not of this token");
    }
    if t.state.family() != token.1.family() {
        return invalid(format!("a {:?} token state on the {} token", t.state.family(), token.1.name()));
    }
    if !t.state.is_plain() {
        return invalid(match t.state.family() {
            Family::Kcc20 => "token UTXOs with borrowing enabled are refused (a borrow can invalidate a signed transaction)",
            Family::Kron => "KRON minter token UTXOs are refused",
        });
    }
    if t.state.amount() <= 0 {
        return invalid("token UTXO amount must be positive");
    }
    Ok(())
}

/// The custody of an order: owned by the order's covenant id and holding exactly `amount`.
///
/// Anything else owned by the id is a stray and never stands in for the custody.
fn check_custody(c: &TokenUtxo, order_cov: Option<[u8; 32]>, token: Token, amount: i64) -> Result<()> {
    check_token_input(c, token)?;
    // A KRON token UTXO holds at most 10^9 base units, since both pinned KRON programs refuse more,
    // so such a custody cannot exist.
    kron_output(token.1.family(), c.state.amount(), "a KRON custody")?;
    if Some(c.state.owner()) != order_cov || !c.state.is_covenant_owned() {
        return invalid("custody token is not owned by its order");
    }
    if c.state.amount() != amount {
        return invalid(format!(
            "custody holds {} base units, the order's amountLeft needs exactly {amount} (a stray is never custody)",
            c.state.amount()
        ));
    }
    Ok(())
}

/// An order's custody carries the extension commitment the order pins.
///
/// This applies to KCC-20, since a KRON token has none. Units of the token's covenant id with
/// another commitment are another token, and the order's covenant refuses them.
pub(crate) fn check_custody_ext(c: &TokenUtxo, ext: [u8; 32]) -> Result<()> {
    if matches!(c.state, TokenState::Kcc20(_)) && c.state.extension() != ext {
        return invalid("the custody carries another extension commitment than the order pins");
    }
    Ok(())
}

/// The extension commitment an order pins for its custody of `token` ([`AnyState::custody_ext`]).
pub(crate) fn pinned_ext(s: &AnyState, token: [u8; 32]) -> Option<[u8; 32]> {
    s.custody_ext(token)
}

/// Builds a [`CreateOrder`].
pub fn build_create_order(r: &CreateOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    check_new_order(&r.order)?;
    if r.order.is_pair() {
        return build_create_pair(r, budgets);
    }
    let token = order_token(&r.order)?;
    let fam = token.1.family();
    if r.funding.is_empty() {
        return invalid("creating an order needs a P2PK funding input (it authorises the genesis)");
    }
    let min = min_order_value(&r.order);
    if (r.value as i64) < min {
        return invalid(format!("order value {} is below the {min} sompi this order needs to be fillable", r.value));
    }
    kas_out(r.value as i64, "the order value")?;
    let mut d = Draft::new(r.lock_time, &r.fee, Some(r.change.unwrap_or(r.funding[0].pubkey)))?;
    let holds = r.order.holds_tokens();
    let mut ext = None;
    if holds {
        if r.tokens.is_empty() {
            return invalid("token-holding orders need token inputs");
        }
        for t in &r.tokens {
            check_token_input(t, token)?;
            if !t.state.is_holder(token.1) {
                return invalid("order tokens must come from key-owned UTXOs (P2PK owner scheme / the KRON program's holder owner)");
            }
            if *ext.get_or_insert(t.state.extension()) != t.state.extension() {
                return invalid("token inputs mix extension commitments");
            }
            d.add_token_input(t, token, token_witness(&t.state, token.1)?)?;
        }
        // The custody carries the commitment the order pins, since its covenant refuses any other.
        if fam == Family::Kcc20 && pinned_ext(&r.order, token.0).is_some_and(|e| Some(e) != ext) {
            return invalid("the token inputs carry another extension commitment than the order pins for its custody");
        }
    } else if !r.tokens.is_empty() {
        return invalid("KAS-holding orders take no token inputs");
    }
    let auth = d.inputs.len();
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let (_, order_id) = d.add_genesis(auth, vec![(r.value, r.order.spk())], Some(r.order.template_id()))?;
    let mut custody = None;
    if holds {
        let ext = ext.expect("ext");
        let amount = r.order.custody_amount().expect("token kind");
        let have = r
            .tokens
            .iter()
            .try_fold(0i64, |a, t| a.checked_add(t.state.amount()))
            .ok_or_else(|| Error::Invalid("token input amounts overflow".into()))?;
        if have < amount {
            return invalid(format!("token inputs hold {have} < the {amount} base units of amountLeft"));
        }
        let carrier = kas_out(r.token_carrier as i64, "tokenCarrier")?;
        let idx = d.add_token_output(token, TokenState::custody(fam, amount, order_id, ext), carrier)?;
        custody = Some(Custody { token_output: idx as u16, extension_commitment: ext });
        if have > amount {
            // Token change returns to the owner of the first token input.
            let owner = r.tokens[0].state.owner();
            d.add_token_output(token, TokenState::user(token.1, have - amount, owner, ext), carrier)?;
        }
    }
    let mut records = vec![Record::order(0, &r.order, custody, r.deadline)];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode_with_release(&records)?;
    d.seal(budgets)
}

/// [`build_create_order`] of a pair order.
///
/// It adds one custody output per custody the order holds, each of its own token with that token's
/// change, and the placement record with its custody parts.
fn build_create_pair(r: &CreateOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let (a, b) = pair_programs(&r.order)?;
    if r.funding.is_empty() {
        return invalid("creating an order needs a P2PK funding input (it authorises the genesis)");
    }
    let min = min_order_value(&r.order);
    if (r.value as i64) < min {
        return invalid(format!("order value {} is below the {min} sompi this order needs to be fillable", r.value));
    }
    kas_out(r.value as i64, "the order value")?;
    let custodies = r.order.custodies();
    if custodies.is_empty() {
        return invalid("a pair order holds tokens in custody");
    }
    let mut d = Draft::new(r.lock_time, &r.fee, Some(r.change.unwrap_or(r.funding[0].pubkey)))?;
    let tok_of = |cov: [u8; 32]| if cov == a.0 { a } else { b };
    // The maker's tokens, per token: (token, total, extension, first owner).
    let mut groups: Vec<(Token, i64, [u8; 32], [u8; 32])> = vec![];
    for t in &r.tokens {
        let tok = match t.utxo.covenant_id {
            Some(c) if c == a.0 || c == b.0 => tok_of(c),
            _ => return invalid("a pair order takes tokens of its own two tokens only"),
        };
        check_token_input(t, tok)?;
        if !t.state.is_holder(tok.1) {
            return invalid("order tokens must come from key-owned UTXOs (P2PK owner scheme / the KRON program's holder owner)");
        }
        match groups.iter_mut().find(|g| g.0 == tok) {
            Some(g) => {
                if g.2 != t.state.extension() {
                    return invalid("token inputs mix extension commitments");
                }
                g.1 = add(g.1, t.state.amount(), "token input amounts")?;
            }
            None => groups.push((tok, t.state.amount(), t.state.extension(), t.state.owner())),
        }
        d.add_token_input(t, tok, token_witness(&t.state, tok.1)?)?;
    }
    let auth = d.inputs.len();
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let (_, order_id) = d.add_genesis(auth, vec![(r.value, r.order.spk())], Some(r.order.template_id()))?;
    let carrier = kas_out(r.token_carrier as i64, "tokenCarrier")?;
    let mut parts = vec![];
    for (cov, amount) in &custodies {
        let tok = tok_of(*cov);
        let g = groups
            .iter_mut()
            .find(|g| g.0 == tok)
            .ok_or_else(|| Error::Invalid(format!("the order needs {amount} base units of {} (no token input)", tok.1.name())))?;
        if g.1 < *amount {
            return invalid(format!("token inputs hold {} < the {amount} base units of the custody", g.1));
        }
        // The custody carries the commitment the order pins, since its covenant refuses any other.
        if tok.1.family() == Family::Kcc20 && pinned_ext(&r.order, *cov).is_some_and(|e| e != g.2) {
            return invalid("the token inputs carry another extension commitment than the order pins for its custody");
        }
        g.1 -= amount;
        let idx = d.add_token_output(tok, TokenState::custody(tok.1.family(), *amount, order_id, g.2), carrier)?;
        parts.push(Custody { token_output: idx as u16, extension_commitment: g.2 });
    }
    for (tok, rest, ext, owner) in groups {
        if rest > 0 {
            d.add_token_output(tok, TokenState::user(tok.1, rest, owner, ext), carrier)?;
        }
    }
    let mut parts = parts.into_iter();
    let mut records = vec![Record::order_with_prefund(0, &r.order, parts.next(), parts.next(), r.deadline)];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode_with_release(&records)?;
    d.seal(budgets)
}

/// An order's own tokens, its custodies (token, exact amount) and the program suffix of its roles.
///
/// A pair order owns A and B, and a KAS kind its token. See [`own_tokens`].
type OwnTokens = (Vec<Token>, Vec<(Token, i64)>, String);

fn own_tokens(s: &AnyState) -> Result<OwnTokens> {
    if s.is_pair() {
        let (a, b) = pair_programs(s)?;
        let custodies = s.custodies().into_iter().map(|(c, amount)| (if c == a.0 { a } else { b }, amount)).collect();
        return Ok((vec![a, b], custodies, pair_suffix(a, b)));
    }
    let token = order_token(s)?;
    let custodies = s.custody_amount().filter(|a| *a > 0).map(|a| vec![(token, a)]).unwrap_or_default();
    Ok((vec![token], custodies, format!("@{}", token.1.name())))
}

/// Builds a [`CancelOrder`], the maker's cancel with an optional replacement.
pub fn build_cancel_order(r: &CancelOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let o = &r.order;
    let id = o.state.template_id();
    let (tokens, custodies, suffix) = own_tokens(&o.state)?;
    if r.retired.is_some() && (r.replace.is_some() || !r.tokens.is_empty() || !r.records.is_empty()) {
        return invalid("an order under a retired template is only cancelled: no replacement, top-up or records");
    }
    let subject = CancelSubject {
        maker: o.state.maker(),
        tokens,
        custodies,
        plan: cancel_plan(&o.state, r.retired)?,
        role: format!("{}.cancel{suffix}", id.name()),
    };
    let parts = CancelParts {
        custody: r.custody.as_ref(),
        prefund: r.prefund.as_ref(),
        strays: &r.strays,
        foreign: &r.foreign,
        tokens: &r.tokens,
        funding: &r.funding,
        change: r.change,
        replace: r.replace.as_ref(),
        lock_time: r.lock_time,
        records: &r.records,
        fee: &r.fee,
    };
    cancel_with(subject, &o.utxo, parts, budgets)
}

/// The signing plan of the maker's cancel of an order.
///
/// It is the template's `cancel(sig)`. For an order under a retired template (`retired`, its
/// template hash) it is [`SigPlan::RetiredCancel`] of that template, which must be in the
/// cancel-only table and of the kind the state decodes as.
fn cancel_plan(s: &AnyState, retired: Option<[u8; 32]>) -> Result<SigPlan> {
    let id = s.template_id();
    Ok(match retired {
        None => entry(id, s.encode(), "cancel", vec![Arg::Sig(s.maker())]),
        Some(h) => {
            let r = crate::retired::retired_for(&h, id)?;
            SigPlan::RetiredCancel { template: r.kind, retired: r.hash, state: s.encode(), maker: s.maker() }
        }
    })
}

/// The inputs and options of a cancel (of [`CancelOrder`]).
struct CancelParts<'a> {
    custody: Option<&'a TokenUtxo>,
    prefund: Option<&'a TokenUtxo>,
    strays: &'a [TokenUtxo],
    foreign: &'a [ForeignStrays],
    tokens: &'a [TokenUtxo],
    funding: &'a [KeyUtxo],
    change: Option<[u8; 32]>,
    replace: Option<&'a Replacement>,
    lock_time: u64,
    records: &'a [Record],
    fee: &'a FeeOptions,
}

/// What the maker's cancel of one order needs.
///
/// That is its maker, its own tokens (A and B for a pair order), the custodies it holds (token,
/// exact amount), and the order input's signing plan and budget role.
struct CancelSubject {
    maker: [u8; 32],
    tokens: Vec<Token>,
    custodies: Vec<(Token, i64)>,
    plan: SigPlan,
    role: String,
}

/// The maker's cancel of an order, optionally replacing it (`r.replace`).
///
/// The custody, the strays of either of its tokens, the foreign strays and the carrier go back to
/// the maker.
fn cancel_with(subject: CancelSubject, utxo: &Utxo, r: CancelParts<'_>, budgets: BudgetFn) -> Result<BuiltTx> {
    let CancelSubject { maker, tokens, custodies, plan, role } = subject;
    let mut d = Draft::new(r.lock_time, r.fee, Some(r.change.unwrap_or(maker)))?;
    let oi = d.add_input(utxo, plan, role, 0);
    let order_cov = utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
    // One group per own token: (token, units, extension, carriers).
    type Group = (Token, i64, Option<[u8; 32]>, Vec<u64>);
    let mut groups: Vec<Group> = tokens.iter().map(|t| (*t, 0, None, vec![])).collect();
    fn put(groups: &mut [Group], t: &TokenUtxo, what: &str) -> Result<Token> {
        let g = groups.iter_mut().find(|g| t.utxo.covenant_id == Some(g.0 .0)).ok_or_else(|| {
            Error::Invalid(format!("{what}: token UTXO is not of this token (a stray of another token is a foreign stray)"))
        })?;
        if *g.2.get_or_insert(t.state.extension()) != t.state.extension() {
            return invalid("all tokens of one transfer share one extension commitment (move other strays separately)");
        }
        g.1 = add(g.1, t.state.amount(), "token amounts")?;
        g.3.push(t.utxo.amount);
        Ok(g.0)
    }
    let given: Vec<&TokenUtxo> = r.custody.iter().chain(r.prefund.iter()).copied().collect();
    if r.prefund.is_some() && r.custody.is_none() {
        return invalid("the prefund custody comes with the first custody");
    }
    if given.len() > custodies.len() {
        return invalid("this order has no custody token");
    }
    if given.len() < custodies.len() {
        return invalid("token-holding order: custody token UTXO required");
    }
    for (c, (tok, amount)) in given.into_iter().zip(&custodies) {
        check_custody(c, Some(order_cov), *tok, *amount)?;
        put(&mut groups, c, "custody")?;
        d.add_token_input(c, *tok, Witness::CovenantId)?;
    }
    for st in r.strays {
        let tok = tokens.iter().copied().find(|t| st.utxo.covenant_id == Some(t.0)).unwrap_or(tokens[0]);
        check_token_input(st, tok)?;
        if st.state.owner() != order_cov || !st.state.is_covenant_owned() {
            return invalid("a stray must be owned by the cancelled order's covenant id");
        }
        put(&mut groups, st, "stray")?;
        d.add_token_input(st, tok, Witness::CovenantId)?;
    }
    for t in r.tokens {
        let tok = tokens.iter().copied().find(|x| t.utxo.covenant_id == Some(x.0)).unwrap_or(tokens[0]);
        check_token_input(t, tok)?;
        if !t.state.is_holder(tok.1) || t.state.owner() != maker {
            return invalid("top-up tokens must be the maker's own key-owned tokens");
        }
        put(&mut groups, t, "top-up")?;
        d.add_token_input(t, tok, token_witness(&t.state, tok.1)?)?;
    }
    let own: Vec<[u8; 32]> = tokens.iter().map(|t| t.0).collect();
    let foreign = add_foreign_inputs(&mut d, r.foreign, order_cov, &own)?;
    for f in r.funding {
        d.add_p2pk(f);
    }
    let mut records = vec![];
    if let Some(rep) = r.replace {
        check_new_order(&rep.order)?;
        if rep.order.maker() != maker {
            return invalid("the replacement must belong to the same maker");
        }
        let (rep_tokens, rep_custodies, _) = own_tokens(&rep.order)?;
        let mut a = rep_tokens.clone();
        let mut b = tokens.clone();
        a.sort();
        b.sort();
        if a != b {
            return invalid("the replacement must trade the same token(s)");
        }
        if (rep.value as i64) < min_order_value(&rep.order) {
            return invalid("replacement value is below what the new order needs to be fillable");
        }
        kas_out(rep.value as i64, "the replacement value")?;
        let (outs, new_id) = d.add_genesis(oi, vec![(rep.value, rep.order.spk())], Some(rep.order.template_id()))?;
        // A tiny change rides on the replacement, as in the amend builders.
        d.absorber = absorber_for(&rep.order, rep.value, outs[0]);
        let default_carrier = groups.iter().find_map(|g| g.3.first().copied()).unwrap_or(0);
        let mut parts = vec![];
        for (tok, amount) in rep_custodies {
            let g = groups.iter_mut().find(|g| g.0 == tok).expect("same tokens");
            if amount > g.1 {
                return invalid(format!("the replacement needs {amount} base units; custody, strays and top-up hold {}", g.1));
            }
            let e = g.2.ok_or_else(|| Error::Invalid("the replacement needs tokens (custody or top-up)".into()))?;
            let carrier = kas_out(
                rep.token_carrier.unwrap_or(g.3.first().copied().unwrap_or(default_carrier)) as i64,
                "replacement tokenCarrier",
            )?;
            let idx = d.add_token_output(tok, TokenState::custody(tok.1.family(), amount, new_id, e), carrier)?;
            parts.push(Custody { token_output: idx as u16, extension_commitment: e });
            g.1 -= amount;
        }
        let mut parts = parts.into_iter();
        records.push(Record::order_with_prefund(0, &rep.order, parts.next(), parts.next(), rep.deadline));
    }
    for (tok, back, e, carriers) in groups {
        if back > 0 {
            let st = TokenState::user(tok.1, back, maker, e.expect("tokens"));
            d.add_token_output(tok, st, pos(carriers[0] as i64, "token carrier")?)?;
        }
    }
    add_foreign_outputs(&mut d, foreign, maker)?;
    records.extend(r.records.iter().cloned());
    if !records.is_empty() {
        d.payload = payload::encode_with_release(&records)?;
    }
    d.seal(budgets)
}

/// The change rule of the amend builders, cancel-replace and in-place amend.
///
/// The maker's new order output takes a tiny change instead of a change output
/// ([`crate::tx::Absorber`]). See `docs/spec/kob1-payload.md`, *Change*.
///
/// - A plain ask's order UTXO is a carrier. It returns to the maker with the order's proceeds or
///   tokens, and no covenant rule reads more than its floor, so it takes any tiny change.
/// - A plain bid's order UTXO is its escrow, so it takes at most what keeps its buying power
///   unchanged ([`BidState::buying_power`], the most a fill can buy).
/// - Every other kind keeps the change output, because its value funds keeper tips, exits or
///   prefunded deliveries.
pub(crate) fn absorber_for(s: &AnyState, value: u64, output: usize) -> Option<crate::tx::Absorber> {
    let max_add = match s {
        AnyState::KobAsk(_) | AnyState::KobAskKron(_) => u64::MAX,
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => {
            // buying_power(v) = floor(budget * scale / rate) with budget = v - deliveryCarrier -
            // reserve. It stays the same while budget + x <= used(power + 1) - 1.
            let budget = (value as i64).checked_sub(b.delivery_carrier)?.checked_sub(b.reserve).filter(|f| *f >= 0)?;
            let next = b.used(b.buying_power(value as i64).checked_add(1)?)?;
            u64::try_from(next.checked_sub(1)?.checked_sub(budget)?).ok()?
        }
        _ => return None,
    };
    (max_add > 0).then_some(crate::tx::Absorber { output, max_add, pays: None })
}

/// Builds an [`AmendOrder`], the in-place amend of a plain ask or bid.
pub fn build_amend_order(r: &AmendOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let o = &r.order;
    let id = o.state.template_id();
    if !payload::amendable(id) {
        return invalid(format!("{} orders are not amended in place (cancel-replace them)", id.name()));
    }
    payload::check_amend_terms(&o.state, &r.amended).map_err(|e| Error::Invalid(e.to_string()))?;
    check_new_order(&r.amended)?;
    let token = order_token(&o.state)?;
    let maker = o.state.maker();
    let order_cov = o.utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
    let funded = !r.funding.is_empty();
    // A bid's escrow is its quantity, so the continuation must still fund one minimum fill at the
    // new terms, as a cancel-replace's replacement must. Without funding, an explicit value above
    // the order's cannot be paid (insufficient funds).
    let bid_floor =
        matches!(&r.amended, AnyState::KobBid(_) | AnyState::KobBidKron(_)).then(|| min_order_value(&r.amended).max(0) as u64);
    if let Some(floor) = bid_floor {
        let want = r.value.unwrap_or(o.utxo.amount);
        if funded && want < floor {
            return invalid(format!(
                "the amended bid's escrow of {want} sompi is below what the new order needs to be fillable: {floor} sompi (one minimum fill at the new terms)"
            ));
        }
        if !funded && want > o.utxo.amount {
            return Err(Error::InsufficientFunds { need: want, have: o.utxo.amount });
        }
    }
    let mut d = Draft::new(r.lock_time, &r.fee, funded.then(|| r.change.unwrap_or(maker)))?;
    let oi = d.add_input(
        &o.utxo,
        entry(id, o.state.encode(), "cancel", vec![Arg::Sig(maker)]),
        format!("{}.cancel@{}", id.name(), token.1.name()),
        0,
    );
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let value = r.value.unwrap_or(o.utxo.amount);
    // The continuation: the order's covenant id, authorised by the order input. Its custody stays
    // owned by that id.
    let out = d.add_output(value, r.amended.spk(), Some((oi as u16, order_cov)));
    d.absorber = if funded {
        absorber_for(&r.amended, value, out)
    } else {
        let tip = match &r.amended {
            AnyState::KobAsk(a) | AnyState::KobAskKron(a) => a.refund_tip.max(0) as u64,
            AnyState::KobBid(b) | AnyState::KobBidKron(b) => b.refund_tip.max(0) as u64,
            _ => 0,
        };
        // A bid pays the fee out of its escrow, but never goes below what funds one minimum fill at
        // the new terms.
        let floor = MIN_AMEND_CARRIER.max(tip).max(bid_floor.unwrap_or(0));
        Some(crate::tx::Absorber { output: out, max_add: u64::MAX, pays: Some(floor) })
    };
    let mut records = vec![Record::amend(out as u16, oi as u16, &r.amended, r.deadline)];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode_with_release(&records)?;
    d.seal(budgets)
}

/// One order of a position cancel, with its custody and any strays to sweep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelItem {
    pub order: OrderUtxo<AnyState>,
    /// The hash of the retired template the order is under ([`CancelOrder::retired`]).
    #[serde(with = "crate::json::field", default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<[u8; 32]>,
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    /// A sell-first `KobIfdPair`'s B prefund custody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund: Option<TokenUtxo>,
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
}

/// Cancel several orders of one maker and token in one transaction.
///
/// For a pair position the orders share one pair of tokens. Typical positions are an if-done entry
/// with all its exits, and a repeat with its booked exits ("cancel all", `matcher.md` §10.9 and
/// §10.13). Every token, custodies and strays, returns to the maker in one output per token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelPosition {
    pub orders: Vec<CancelItem>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(with = "crate::json::field", default)]
    pub lock_time: u64,
    #[serde(with = "crate::json::field", default)]
    pub token_carrier: Option<u64>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Builds a [`CancelPosition`].
pub fn build_cancel_position(r: &CancelPosition, budgets: BudgetFn) -> Result<BuiltTx> {
    let first = r.orders.first().ok_or_else(|| Error::Invalid("nothing to cancel".into()))?;
    let maker = first.order.state.maker();
    let (tokens, _, _) = own_tokens(&first.order.state)?;
    if r.orders.iter().any(|o| o.retired.is_some()) && !r.records.is_empty() {
        return invalid("a cancel of orders under a retired template carries no records");
    }
    let mut d = Draft::new(r.lock_time, &r.fee, Some(r.change.unwrap_or(maker)))?;
    let mut items = vec![];
    for it in &r.orders {
        let s = &it.order.state;
        let (own, custodies, suffix) = own_tokens(s)?;
        if s.maker() != maker || own != tokens {
            return invalid("a position cancel covers one maker's orders of one token (or one pair)");
        }
        let id = s.template_id();
        d.add_input(&it.order.utxo, cancel_plan(s, it.retired)?, format!("{}.cancel{suffix}", id.name()), 0);
        items.push((it, custodies));
    }
    // Per token: (units, extension, first carrier).
    let mut groups: Vec<(Token, i64, Option<[u8; 32]>, u64)> = tokens.iter().map(|t| (*t, 0, None, 0)).collect();
    for (it, custodies) in items {
        let cov = it.order.utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
        let given: Vec<&TokenUtxo> = it.custody.iter().chain(it.prefund.iter()).collect();
        if given.len() > custodies.len() {
            return invalid("this order has no custody token");
        }
        if given.len() < custodies.len() {
            return invalid("token-holding order: custody token UTXO required");
        }
        for (c, (tok, amount)) in given.iter().zip(&custodies) {
            check_custody(c, Some(cov), *tok, *amount)?;
        }
        for t in given.into_iter().chain(&it.strays) {
            let tok = tokens.iter().copied().find(|x| t.utxo.covenant_id == Some(x.0)).unwrap_or(tokens[0]);
            check_token_input(t, tok)?;
            if t.state.owner() != cov || !t.state.is_covenant_owned() {
                return invalid("a stray must be owned by its order's covenant id");
            }
            let g = groups.iter_mut().find(|g| g.0 == tok).expect("own token");
            if *g.2.get_or_insert(t.state.extension()) != t.state.extension() {
                return invalid("all tokens of one transfer share one extension commitment");
            }
            d.add_token_input(t, tok, Witness::CovenantId)?;
            g.1 = add(g.1, t.state.amount(), "token amounts")?;
            if g.3 == 0 {
                g.3 = t.utxo.amount;
            }
        }
    }
    for f in &r.funding {
        d.add_p2pk(f);
    }
    for (tok, total, ext, carrier) in groups {
        if total > 0 {
            let v = pos(r.token_carrier.unwrap_or(carrier) as i64, "token carrier")?;
            d.add_token_output(tok, TokenState::user(tok.1, total, maker, ext.expect("tokens")), v)?;
        }
    }
    if !r.records.is_empty() {
        d.payload = payload::encode_with_release(&r.records)?;
    }
    d.seal(budgets)
}

/// Builds the maker's sweep in place ([`SweepOrder`]).
pub fn build_sweep_order(r: &SweepOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let o = &r.order;
    let id = o.state.template_id();
    let maker = o.state.maker();
    let order_cov = o.utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
    if r.strays.is_empty() && r.foreign.is_empty() {
        return invalid("nothing to sweep: no strays");
    }
    let funded = !r.funding.is_empty();
    // Without funding only a plain ask's carrier pays. Every other kind's value is an escrow, a
    // prefund or keeper tips.
    let carrier_pays = matches!(&o.state, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
    if !funded && !carrier_pays {
        return invalid(format!("a {} sweep needs funding (its value is not a plain carrier)", id.name()));
    }
    let mut d = Draft::new(r.lock_time, &r.fee, funded.then(|| r.change.unwrap_or(maker)))?;
    let (tokens, _, suffix) = own_tokens(&o.state)?;
    let oi =
        d.add_input(&o.utxo, entry(id, o.state.encode(), "cancel", vec![Arg::Sig(maker)]), format!("{}.cancel{suffix}", id.name()), 0);
    // The order's own tokens (A and B for a pair order), one group each, swept to the maker.
    let mut own_groups: Vec<ForeignOut> = vec![];
    for t in tokens.iter().copied() {
        let group: Vec<&TokenUtxo> = r.strays.iter().filter(|s| s.utxo.covenant_id == Some(t.0)).collect();
        let Some(first) = group.first() else { continue };
        let (max_in, _) = t.1.token_slots().ok_or_else(|| Error::Invalid(format!("{} is not a token program", t.1.name())))?;
        if group.len() > max_in {
            return invalid(format!(
                "{} strays of one token exceed the {max_in} token inputs of {} (sweep the rest in another sweep)",
                group.len(),
                t.1.name()
            ));
        }
        let e = first.state.extension();
        let (mut units, mut kas) = (0i64, 0u64);
        for s in group {
            check_token_input(s, t)?;
            if s.state.owner() != order_cov || !s.state.is_covenant_owned() {
                return invalid("a stray must be owned by the swept order's covenant id");
            }
            if s.state.extension() != e {
                return invalid("all strays of one token in a sweep share one extension commitment (sweep the others separately)");
            }
            d.add_token_input(s, t, Witness::CovenantId)?;
            units = units.checked_add(s.state.amount()).ok_or_else(|| Error::Invalid("stray amounts overflow".into()))?;
            kas = kas.checked_add(s.utxo.amount).ok_or_else(|| Error::Invalid("stray carriers overflow".into()))?;
        }
        own_groups.push((t, units, kas, e));
    }
    if r.strays.iter().any(|s| !tokens.iter().any(|t| s.utxo.covenant_id == Some(t.0))) {
        return invalid("a stray of another token is a foreign stray (pass it in `foreign` with its program)");
    }
    let own: Vec<[u8; 32]> = tokens.iter().map(|t| t.0).collect();
    let foreign = add_foreign_inputs(&mut d, &r.foreign, order_cov, &own)?;
    for f in &r.funding {
        d.add_p2pk(f);
    }
    // The continuation: the same script under the order's covenant id, authorised by the order
    // input. A custody, owned by that id, stays where it is.
    let out = d.add_output(o.utxo.amount, o.state.spk(), Some((oi as u16, order_cov)));
    if !funded {
        let tip = match &o.state {
            AnyState::KobAsk(a) | AnyState::KobAskKron(a) => a.refund_tip.max(0) as u64,
            _ => 0,
        };
        d.absorber = Some(crate::tx::Absorber { output: out, max_add: u64::MAX, pays: Some(MIN_AMEND_CARRIER.max(tip)) });
    }
    for (t, units, kas, e) in own_groups {
        let carrier = r.token_carrier.unwrap_or(kas);
        d.add_token_output(t, TokenState::user(t.1, units, maker, e), pos(carrier as i64, "stray carrier")?)?;
    }
    add_foreign_outputs(&mut d, foreign, maker)?;
    let mut records = vec![Record::Sweep { output: out as u16, input: oi as u16 }];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode_with_release(&records)?;
    d.seal(budgets)
}

/// One laid-out group of foreign strays.
///
/// It holds the token, the units, the KAS carriers it holds and its extension commitment.
type ForeignOut = (Token, i64, u64, [u8; 32]);

/// Adds the inputs of the foreign stray groups owned by `order_cov`.
///
/// The groups are of tokens other than `exclude`, the order's own. The outputs that return them
/// ([`add_foreign_outputs`]) come after the transaction's positional outputs. Each group is one
/// token, and its UTXOs share one extension commitment and fit the program's token inputs.
fn add_foreign_inputs(d: &mut Draft, foreign: &[ForeignStrays], order_cov: [u8; 32], exclude: &[[u8; 32]]) -> Result<Vec<ForeignOut>> {
    let mut out: Vec<ForeignOut> = vec![];
    for g in foreign {
        let t: Token = (g.token.covenant_id, g.token.program);
        if exclude.contains(&t.0) {
            return invalid("a foreign stray group names the order's own token (pass those as strays)");
        }
        if out.iter().any(|(x, ..)| x.0 == t.0) {
            return invalid("one group per foreign token");
        }
        let first = g.utxos.first().ok_or_else(|| Error::Invalid("an empty foreign stray group".into()))?;
        let (max_in, _) = t.1.token_slots().ok_or_else(|| Error::Invalid(format!("{} is not a token program", t.1.name())))?;
        if g.utxos.len() > max_in {
            return invalid(format!(
                "{} foreign strays of one token exceed the {max_in} token inputs of {} (sweep the rest in another transaction)",
                g.utxos.len(),
                t.1.name()
            ));
        }
        let e = first.state.extension();
        let (mut units, mut kas) = (0i64, 0u64);
        for s in &g.utxos {
            check_token_input(s, t)?;
            if s.state.owner() != order_cov || !s.state.is_covenant_owned() || s.state.extension() != e {
                return invalid("a foreign stray must be owned by the order's covenant id (one extension commitment per token)");
            }
            d.add_token_input(s, t, Witness::CovenantId)?;
            units = units.checked_add(s.state.amount()).ok_or_else(|| Error::Invalid("foreign stray amounts overflow".into()))?;
            kas = kas.checked_add(s.utxo.amount).ok_or_else(|| Error::Invalid("foreign stray carriers overflow".into()))?;
        }
        out.push((t, units, kas, e));
    }
    Ok(out)
}

/// The maker's outputs of [`add_foreign_inputs`]: each token's units with the KAS its strays
/// carried.
fn add_foreign_outputs(d: &mut Draft, groups: Vec<ForeignOut>, maker: [u8; 32]) -> Result<()> {
    for (t, units, kas, e) in groups {
        d.add_token_output(t, TokenState::user(t.1, units, maker, e), pos(kas as i64, "foreign stray carrier")?)?;
    }
    Ok(())
}

/// Builds a [`RefundOrder`].
pub fn build_refund_order(r: &RefundOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    if r.order.state.is_pair() {
        return build_refund_pair(r, budgets);
    }
    let o = &r.order;
    let token = order_token(&o.state)?;
    let (_, tip) = o.state.expiry().expect("order");
    let due = o.state.refund_due(o.utxo.block_daa_score as i64).expect("order");
    if (r.lock_time as i64) < due {
        return invalid(format!("not refundable before DAA {due} (lockTime {})", r.lock_time));
    }
    let id = o.state.template_id();
    let maker = o.state.maker();
    // Funding without a change key would burn the whole funding as fee, so the change goes back to
    // the first funding key. Without funding the tip is the fee, and the keeper takes the
    // difference elsewhere.
    let mut d = Draft::new(r.lock_time, &r.fee, r.change.or(r.funding.first().map(|f| f.pubkey)))?;
    let state = o.state.encode();
    let custody_amount = o.state.custody_amount();
    let (plan, what) = match &o.state {
        AnyState::KobAsk(_) | AnyState::KobAskKron(_) => {
            (entry(id, state, "settle", vec![nb(0), Arg::Int(1), Arg::Int(0), Arg::Int(0)]), "refund")
        }
        AnyState::KobCondAsk(_) | AnyState::KobCondAskKron(_) => (
            entry(id, state, "settle", vec![nb(0), Arg::Int(1), Arg::Int(0), Arg::Int(0), Arg::Int(0), Arg::Int(0), Arg::Int(0)]),
            "refund",
        ),
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) if s.amount_left == 0 => (entry(id, state, "close", vec![]), "close"),
        AnyState::KobIfdAsk(_) | AnyState::KobIfdAskKron(_) => {
            let args = vec![
                nb(0),
                Arg::Int(1),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Bytes(vec![]),
                Arg::Bytes(vec![]),
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(0),
            ];
            (entry(id, state, "settle", args), "refund")
        }
        _ => (entry(id, state, "refund", vec![]), "refund"),
    };
    let kill = matches!(&o.state, AnyState::KobAsk(s) | AnyState::KobAskKron(s) if s.tif != TIF_GTC)
        || matches!(&o.state, AnyState::KobBid(s) | AnyState::KobBidKron(s) if s.tif != TIF_GTC);
    let role = format!("{}.{what}{}@{}", id.name(), if kill { ".kill" } else { "" }, token.1.name());
    d.add_input(&o.utxo, plan, role, SEQUENCE_NONFINAL);
    let order_cov = o.utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
    let keep = u(o.utxo.amount as i64 - tip, "refund payout")?;
    let own = vec![token.0];
    let foreign = match custody_amount {
        Some(amount) if amount > 0 => {
            let c = r.custody.as_ref().ok_or_else(|| Error::Invalid("token-holding order: custody token UTXO required".into()))?;
            check_custody(c, Some(order_cov), token, amount)?;
            if let Some(e) = pinned_ext(&o.state, token.0) {
                check_custody_ext(c, e)?;
            }
            d.add_token_input(c, token, Witness::CovenantId)?;
            for f in &r.funding {
                d.add_p2pk(f);
            }
            let foreign = add_foreign_inputs(&mut d, &r.foreign, order_cov, &own)?;
            let st = c.state.with_holder_owner(token.1, maker);
            d.add_token_output(token, st, keep + c.utxo.amount)?;
            foreign
        }
        _ => {
            if r.custody.is_some() {
                return invalid("this order has no custody token (a refund never moves strays)");
            }
            for f in &r.funding {
                d.add_p2pk(f);
            }
            let foreign = add_foreign_inputs(&mut d, &r.foreign, order_cov, &own)?;
            d.add_output(pos(keep as i64, "refund payout")?, p2pk_spk(&maker), None);
            foreign
        }
    };
    add_foreign_outputs(&mut d, foreign, maker)?;
    d.seal(budgets)
}

/// The refund of a pair order.
///
/// `KobPair` and `KobCondPair` return their custody to the maker at output 0 with every carrier,
/// minus `refundTip`. `KobIfdPair` pays its KAS to the maker at output 0 and returns each custody
/// at its own index.
fn build_refund_pair(r: &RefundOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let o = &r.order;
    let (_, tip) = o.state.expiry().expect("order");
    let due = o.state.refund_due(o.utxo.block_daa_score as i64).expect("order");
    if (r.lock_time as i64) < due {
        return invalid(format!("not refundable before DAA {due} (lockTime {})", r.lock_time));
    }
    let maker = o.state.maker();
    let (tokens, custodies, _) = own_tokens(&o.state)?;
    let kill = matches!(&o.state, AnyState::KobPair(s) if s.tif != TIF_GTC);
    let (plan, role) = pair::refund_plan(&o.state, kill)?;
    let mut d = Draft::new(r.lock_time, &r.fee, r.change.or(r.funding.first().map(|f| f.pubkey)))?;
    d.add_input(&o.utxo, plan, role, SEQUENCE_NONFINAL);
    let order_cov = o.utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
    let given: Vec<&TokenUtxo> = r.custody.iter().chain(r.prefund.iter()).collect();
    if given.len() != custodies.len() {
        return invalid(format!(
            "this order's refund needs exactly its {} custody token UTXO(s) (a refund never moves strays)",
            custodies.len()
        ));
    }
    if matches!(&o.state, AnyState::KobPair(_) | AnyState::KobCondPair(_)) && custodies.is_empty() {
        return invalid("a pair order without a custody is cancel-only");
    }
    for (c, (tok, amount)) in given.iter().zip(&custodies) {
        check_custody(c, Some(order_cov), *tok, *amount)?;
        if let Some(e) = pinned_ext(&o.state, tok.0) {
            check_custody_ext(c, e)?;
        }
        d.add_token_input(c, *tok, Witness::CovenantId)?;
    }
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let own: Vec<[u8; 32]> = tokens.iter().map(|t| t.0).collect();
    let foreign = add_foreign_inputs(&mut d, &r.foreign, order_cov, &own)?;
    let keep = u(o.utxo.amount as i64 - tip, "refund payout")?;
    match &o.state {
        AnyState::KobIfdPair(_) => {
            d.add_output(pos(keep as i64, "refund payout")?, p2pk_spk(&maker), None);
            for (c, (tok, _)) in given.iter().zip(&custodies) {
                d.add_token_output(*tok, c.state.with_user_owner(maker), c.utxo.amount)?;
            }
        }
        _ => {
            let (c, (tok, _)) = (given[0], custodies[0]);
            d.add_token_output(tok, c.state.with_user_owner(maker), keep + c.utxo.amount)?;
        }
    }
    add_foreign_outputs(&mut d, foreign, maker)?;
    d.seal(budgets)
}

/// Builds a [`SendTokens`] transfer.
pub fn build_send_tokens(r: &SendTokens, budgets: BudgetFn) -> Result<BuiltTx> {
    let token = (r.token.covenant_id, r.token.program);
    if r.tokens.is_empty() || r.recipients.is_empty() {
        return invalid("send needs token inputs and recipients");
    }
    for rc in &r.recipients {
        check_key(&rc.pubkey, "recipient")?;
    }
    if let Some(k) = &r.token_change {
        check_key(k, "token change owner")?;
    }
    let change = r.change.or(r.funding.first().map(|f| f.pubkey)).or(Some(r.tokens[0].state.owner()));
    let mut d = Draft::new(0, &r.fee, change)?;
    let ext = r.tokens[0].state.extension();
    for t in &r.tokens {
        check_token_input(t, token)?;
        if t.state.extension() != ext {
            return invalid("token inputs mix extension commitments");
        }
        d.add_token_input(t, token, token_witness(&t.state, token.1)?)?;
    }
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let have = r
        .tokens
        .iter()
        .try_fold(0i64, |a, t| a.checked_add(t.state.amount()))
        .ok_or_else(|| Error::Invalid("the token inputs' amounts overflow an i64".into()))?;
    let mut sent: i64 = 0;
    for rc in &r.recipients {
        pos(rc.amount, "recipient amount")?;
        sent = sent.checked_add(rc.amount).ok_or_else(|| Error::Invalid("the recipients' amounts overflow an i64".into()))?;
        d.add_token_output(token, TokenState::user(token.1, rc.amount, rc.pubkey, ext), pos(rc.carrier as i64, "recipient carrier")?)?;
    }
    if sent > have {
        return invalid(format!("sending {sent} with only {have}"));
    }
    if sent < have {
        let owner = r.token_change.unwrap_or(r.tokens[0].state.owner());
        d.add_token_output(
            token,
            TokenState::user(token.1, have - sent, owner, ext),
            pos(r.token_change_carrier as i64, "token change carrier")?,
        )?;
    }
    if !r.records.is_empty() {
        d.payload = payload::encode_with_release(&r.records)?;
    }
    d.seal(budgets)
}

// ---------------------------------------------------------------- matcher shapes

/// One order filled by a batch.
///
/// `amount` is the base units the leg fills: a bid's delivery, an ask's sale, or a pair order's
/// base units of A. `t` is the auction or cycle time the covenant proves against the lock time
/// (CLTV). It defaults to the batch's `lockTime`, the matcher's `now − margin`, which gives the
/// best price for the maker and the longest `rptUntil`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)] // a request value, built once per transaction
pub enum Leg {
    /// Limit / IOC / FOK / TWAP / Dutch / market-auction sell.
    #[serde(rename_all = "camelCase")]
    Ask {
        order: OrderUtxo<AskState>,
        /// The custody: exactly `amountLeft` base units owned by the order.
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// Limit / IOC / FOK / DCA / rising / market-auction buy.
    #[serde(rename_all = "camelCase")]
    Bid {
        order: OrderUtxo<BidState>,
        #[serde(with = "crate::json::field")]
        amount: i64,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// Conditional sell on leg 0 (take-profit) or 1 (stop: armed, or triggered by `evidence`).
    /// A booked exit's take-profit re-arms its buy-first entry with `merge`.
    #[serde(rename_all = "camelCase")]
    CondAsk {
        order: OrderUtxo<CondAskState>,
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        leg: u8,
        /// Trigger evidence of an unarmed stop leg: the index of a plain `Ask` leg of this batch
        /// (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
        /// Repeat IFD: the booked exit's entry, merged (re-armed) in this transaction.
        #[serde(default)]
        merge: Option<OrderUtxo<IfdBidState>>,
    },
    /// Conditional buy.
    ///
    /// A booked sell-first exit's take-profit re-arms its entry with `merge`, and the bought tokens
    /// go into the entry's custody.
    #[serde(rename_all = "camelCase")]
    CondBid {
        order: OrderUtxo<CondBidState>,
        #[serde(with = "crate::json::field")]
        amount: i64,
        leg: u8,
        /// Trigger evidence of an unarmed stop leg: the index of a plain `Bid` leg of this batch
        /// (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
        #[serde(default)]
        merge: Option<SellFirstEntry>,
    },
    /// If-done buy entry, a limit or stop entry that may repeat.
    ///
    /// It creates a fresh exit holding the bought tokens.
    #[serde(rename_all = "camelCase")]
    IfdBid {
        order: OrderUtxo<IfdBidState>,
        #[serde(with = "crate::json::field")]
        amount: i64,
        /// Evidence arming an unarmed buy-stop entry in this fill: a plain `Bid` leg index (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// If-done sell entry: creates a fresh exit with `amountLeft = amount`.
    #[serde(rename_all = "camelCase")]
    IfdAsk {
        order: OrderUtxo<IfdAskState>,
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        /// Evidence arming an unarmed sell-stop entry in this fill: a plain `Ask` leg index
        /// (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// Pair order (`KobPair`, ask or bid), filling `amount` base units of A at its quote.
    ///
    /// A decaying order quotes at `t`. An ask releases exactly `amount` of A and receives at least
    /// the ceil of B at its positional output, and the batch's surplus of B, if any, rides on it. A
    /// bid pays exactly the floor of B from its escrow and receives exactly `amount` of A.
    #[serde(rename_all = "camelCase")]
    Pair {
        order: OrderUtxo<PairState>,
        /// The custody of S (A for an ask, B for a bid), exactly the state's `custody`, owned by
        /// the order.
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// Pair conditional (`KobCondPair`) on leg 0 (take-profit / limit) or leg 1 (stop).
    ///
    /// The stop leg is armed, or armed in this fill by the evidence. A booked exit's take-profit
    /// re-arms its `KobIfdPair` entry with `merge`.
    #[serde(rename_all = "camelCase")]
    CondPair {
        order: OrderUtxo<CondPairState>,
        /// The custody of S, exactly the state's `custody`.
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        leg: u8,
        /// Trigger evidence of an unarmed stop leg.
        ///
        /// With `evidenceB` it is a KAS-book leg of A and one of B (mode 0). Alone it is a
        /// `KobPair` leg of the same pair (mode 1).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_b: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
        #[serde(default)]
        merge: Option<PairEntryMerge>,
    },
    /// Pair if-done entry (`KobIfdPair`), which creates a fresh `KobCondPair` exit.
    ///
    /// The exit holds the bought A for a buy-first entry, or the proceeds plus the prefund of the
    /// fill for a sell-first one.
    #[serde(rename_all = "camelCase")]
    IfdPair {
        order: OrderUtxo<IfdPairState>,
        /// A sell-first entry's A custody (exactly `amountLeft`).
        #[serde(default)]
        a_custody: Option<TokenUtxo>,
        /// The entry's B custody, when it holds one: the escrow if buy-first, the prefund if
        /// sell-first.
        #[serde(default)]
        b_custody: Option<TokenUtxo>,
        #[serde(with = "crate::json::field")]
        amount: i64,
        /// Evidence arming an unarmed stop entry in this fill (as [`Leg::CondPair`]).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_b: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
}

/// A repeating pair entry (`KobIfdPair`) merged by its exit's take-profit, with its custodies.
///
/// A sell-first entry lists its A custody when it holds A, and its B prefund when it holds one. A
/// buy-first entry lists its B escrow when it holds one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairEntryMerge {
    pub entry: OrderUtxo<IfdPairState>,
    #[serde(default)]
    pub a_custody: Option<TokenUtxo>,
    #[serde(default)]
    pub b_custody: Option<TokenUtxo>,
}

/// A repeating sell-first entry merged by its exit's take-profit.
///
/// It comes with its custody when it still holds tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SellFirstEntry {
    pub entry: OrderUtxo<IfdAskState>,
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
}

/// An arm or trailing ratchet of an order without a fill of that order, by its `update` entry.
///
/// It arms or trails a conditional order, or arms a stop entry, next to the fill of the plain leg
/// `evidence` of the same batch. The touch evidence exists only in that transaction, so matchers
/// arm inside their batches. The batch's change key, the matcher, takes up to the order's
/// `keeperTip`. A booked exit is never updated in a transaction that also spends its repeat entry,
/// because the covenants refuse it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdate {
    /// The order to update.
    ///
    /// It is a `KobCondAsk`, `KobCondBid` or `KobCondPair` to arm or trail, or an unarmed stop
    /// entry (`KobIfdBid`, `KobIfdAsk`, `KobIfdPair`).
    pub order: OrderUtxo<AnyState>,
    /// Index of the plain `Ask` or `Bid` leg whose fill is the evidence.
    ///
    /// For a pair order it is the KAS-book leg of A together with `evidenceB`, or a `KobPair` leg
    /// of the same pair alone.
    pub evidence: usize,
    /// Pair orders, evidence mode 0: the KAS-book leg of B (the leg `evidence` is the one of A).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_b: Option<usize>,
    /// Sompi the keeper takes (default: the order's `keeperTip`).
    #[serde(with = "crate::json::field", default)]
    pub take: Option<i64>,
}

/// An N:M matcher batch, a taker transaction, or any mix of them.
///
/// A batch may hold:
/// - orders at their own all-in prices, and auctions,
/// - conditional trigger fills, and updates armed by a plain fill of the same batch (touch),
/// - IFD / IFO partial and stop-entry fills, and repeat merges,
/// - TWAP, DCA and Dutch fills.
///
/// Legs may trade several tokens (routes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    /// The DAA score the transaction proves (CLTV): activation, auction and cycle times.
    #[serde(with = "crate::json::field")]
    pub lock_time: u64,
    pub legs: Vec<Leg>,
    /// Arms and trailing ratchets without a fill (see [`BatchUpdate`]).
    #[serde(default)]
    pub updates: Vec<BatchUpdate>,
    /// Tokens the taker sells into bids (P2PK owned; the owner signs).
    #[serde(default)]
    pub taker_tokens: Vec<TokenUtxo>,
    /// Receiver of the net tokens (bought tokens and token change); default: the change key.
    #[serde(with = "crate::json::field", default)]
    pub taker: Option<[u8; 32]>,
    /// KAS carrier on each of the taker's token outputs.
    #[serde(with = "crate::json::field", default)]
    pub taker_token_carrier: u64,
    /// Tokens (covenant ids) whose surplus the taker keeps.
    ///
    /// The base units the batch releases beyond what its legs take go to the taker's token output
    /// (`taker`, or the token's `receivers` entry) instead of the delivery of the first pair ask
    /// buying the token. A pair ask's guarantee is a floor (`contracts/orders/KobPair.sil`:
    /// `tOut >= ceil`), so its delivery is then exactly what its covenant amount needs. When this
    /// is empty, the default, the surplus goes to the pair ask.
    #[serde(with = "crate::json::field_vec", default, skip_serializing_if = "Vec::is_empty")]
    pub keep_surplus: Vec<[u8; 32]>,
    /// KAS carrier on the taker's token output of a kept token, one named in `keep_surplus`.
    ///
    /// By default (`None`) it is `taker_token_carrier`. An inventory output only has to clear its
    /// program's floor and the dust bound, so an operator may lock less KAS on it than on its other
    /// token outputs (`docs/spec/matcher.md` §3.5).
    #[serde(with = "crate::json::field", default, skip_serializing_if = "Option::is_none")]
    pub keep_carrier: Option<u64>,
    /// Per-token receivers overriding `taker` (e.g. a route's bought token paid to a merchant).
    #[serde(default)]
    pub receivers: Vec<TokenPayee>,
    /// Plain KAS payments (swap-and-pay).
    #[serde(default)]
    pub payments: Vec<Payment>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// Matcher / taker key receiving the spread, tips and change.
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Positional output of a leg.
pub(crate) enum PosOut {
    Pay(u64, [u8; 32]),
    /// A token output of `Token` to the maker.
    Deliver(u64, TokenState, Token),
    /// Tokens into the custody of the exit this leg creates (resolved once the exit id is known).
    DeliverToExit {
        value: u64,
        amount: i64,
        ext: [u8; 32],
        token: Token,
    },
    /// The exit itself, a single-output genesis at the positional slot.
    Exit(u64, kaspa_consensus_core::tx::ScriptPublicKey, TemplateId),
}

/// The custody inputs of a leg, by role (the layout gives each its input index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Slot {
    /// The leg's own custody (an ask, a conditional ask, a sell-first entry, a pair order's S).
    Custody,
    /// A pair entry's A custody (sell-first).
    A,
    /// A pair entry's B custody (escrow or prefund).
    B,
    /// The custody of a merged KAS sell-first entry.
    Entry,
    /// A merged pair entry's A custody.
    EntryA,
    /// A merged pair entry's B custody.
    EntryB,
}

/// Extra outputs of a leg (after the positional slots).
pub(crate) enum Extra {
    /// Continuation of the leg's order (same covenant id).
    Cont(u64, kaspa_consensus_core::tx::ScriptPublicKey),
    /// A token output at any index; `patch`: (input, argument) receiving its index.
    Tok { value: u64, state: TokenState, token: Token, patch: Option<(usize, usize)> },
    /// A token output at the index of the leg's custody input `slot`.
    ///
    /// It is a rest or return the covenant pins there. `patch` is as for `Tok`.
    AtInput { slot: Slot, value: u64, state: TokenState, token: Token, patch: Option<(usize, usize)> },
    /// A fresh exit authorised by the leg input (its index goes into argument `arg` of the leg).
    Exit { value: u64, spk: kaspa_consensus_core::tx::ScriptPublicKey, tpl: TemplateId, arg: usize },
    /// A merged entry's continuation.
    EntryCont(u64, kaspa_consensus_core::tx::ScriptPublicKey, [u8; 32]),
}

pub(crate) struct LegPlan {
    /// The role suffix (`@<program>`, a pair order `@<A>+<B>`).
    suffix: String,
    pos: PosOut,
    role: String,
    sequence: u64,
    plan: SigPlan,
    extras: Vec<Extra>,
    /// Per token, the base units the leg adds to (`sold`) and takes from (`bought`) the free
    /// balance.
    flows: Vec<(Token, i64, i64)>,
    /// Merged entry input (plan, role, utxo).
    merge: Option<(SigPlan, String, Utxo)>,
    /// A pair ask's delivery that takes the batch's surplus of the token it buys.
    ///
    /// It holds the token and the plan's argument of the delivered amount.
    adjust: Option<(Token, usize)>,
}

/// What a conditional order reads from its trigger evidence.
///
/// The evidence is a plain `KobAsk` or `KobBid` leg filled in the same transaction, mirroring
/// `touch` in `KobCondAsk` and the other conditionals. Pair, conditional and if-done fills are
/// never evidence for the KAS kinds. Pair conditionals read pair orders ([`pair::pair_evidence`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Touch {
    /// [`SIDE_ASK`] (a resting ask sold at its quote) or [`SIDE_BID`] (a resting bid bought).
    pub side: i64,
    #[serde(with = "crate::json::field")]
    pub token_cov_id: [u8; 32],
    /// The resting order's scale (prices are comparable only at the same scale).
    pub scale: i64,
    /// The resting order's quote (sompi per whole token).
    pub price: i64,
    /// Base units traded (the evidence fill's n).
    pub amount: i64,
    /// Latest of the order UTXO's DAA plus its TWAP / DCA interval, its custody's DAA (ask) and its
    /// `activeFrom`.
    pub exposed_since: i64,
}

impl Touch {
    /// Evidence rules of an order (`minTouch`, `minRestDaa`) in a transaction with lock time
    /// `lock`.
    ///
    /// The covenant proves `tx.daa >= exposedSince + minRestDaa` by CLTV.
    pub fn check(&self, token: [u8; 32], scale: i64, min_touch: i64, min_rest: i64, lock: i64) -> Result<()> {
        if self.token_cov_id != token || self.scale != scale {
            return invalid("trigger evidence is of another token or scale");
        }
        if self.amount < min_touch {
            return invalid("trigger evidence below the order's minTouch");
        }
        if self.exposed_since.saturating_add(min_rest) > lock {
            return invalid("trigger evidence was exposed for less than minRestDaa before the lock time");
        }
        Ok(())
    }
}

/// The touch a plain leg filling `amount` base units provides.
///
/// Any other leg is an error, and so are decaying or rising orders, whose price is not a quote.
pub fn touch_of(leg: &Leg) -> Result<Touch> {
    match leg {
        Leg::Ask { order, custody, amount, .. } => {
            let s = &order.state;
            if s.slope != 0 {
                return invalid("a decaying ask is never trigger evidence");
            }
            if s.interval < 0 {
                return invalid(
                    "an evidence order with a negative interval is never trigger evidence (it would shorten the rest time)",
                );
            }
            let exposed = (order.utxo.block_daa_score as i64 + s.interval).max(custody.utxo.block_daa_score as i64).max(s.active_from);
            Ok(Touch {
                side: SIDE_ASK,
                token_cov_id: s.token_cov_id,
                scale: s.scale,
                price: s.price,
                amount: *amount,
                exposed_since: exposed,
            })
        }
        Leg::Bid { order, amount, .. } => {
            let s = &order.state;
            if s.slope != 0 {
                return invalid("a rising bid is never trigger evidence");
            }
            if s.interval < 0 {
                return invalid(
                    "an evidence order with a negative interval is never trigger evidence (it would shorten the rest time)",
                );
            }
            let exposed = (order.utxo.block_daa_score as i64 + s.interval).max(s.active_from);
            Ok(Touch {
                side: SIDE_BID,
                token_cov_id: s.token_cov_id,
                scale: s.scale,
                price: s.price,
                amount: *amount,
                exposed_since: exposed,
            })
        }
        _ => invalid("trigger evidence is a plain KobAsk / KobBid leg (pair, conditional and if-done fills are not)"),
    }
}

/// The evidence leg `k` of a batch, its touch and its covenant arguments `(ev, tk)`.
///
/// `ev` is the input index of the evidence order, which equals its leg index. `tk` is the input
/// index of its custody for an ask, and `-1` for a bid.
fn evidence(b: &Batch, lay: &Layout, k: usize) -> Result<(Touch, i64, i64)> {
    let l = b.legs.get(k).ok_or_else(|| Error::Invalid(format!("trigger evidence {k} is not a leg of the batch")))?;
    let t = touch_of(l).map_err(|e| blame("evidence", k, e))?;
    let tk = if t.side == SIDE_ASK { lay.at[&(k, Slot::Custody)] as i64 } else { -1 };
    Ok((t, k as i64, tk))
}

/// The order state of a leg as the kind of its token's family.
///
/// The family follows from the token program the state pins by template hash. A pair order is one
/// template for both families.
fn leg_state(l: &Leg) -> Result<AnyState> {
    let s = match l {
        Leg::Ask { order, .. } => AnyState::KobAsk(order.state.clone()),
        Leg::Bid { order, .. } => AnyState::KobBid(order.state.clone()),
        Leg::CondAsk { order, .. } => AnyState::KobCondAsk(order.state.clone()),
        Leg::CondBid { order, .. } => AnyState::KobCondBid(order.state.clone()),
        Leg::IfdBid { order, .. } => AnyState::KobIfdBid(order.state.clone()),
        Leg::IfdAsk { order, .. } => AnyState::KobIfdAsk(order.state.clone()),
        Leg::Pair { order, .. } => return Ok(AnyState::KobPair(order.state.clone())),
        Leg::CondPair { order, .. } => return Ok(AnyState::KobCondPair(order.state.clone())),
        Leg::IfdPair { order, .. } => return Ok(AnyState::KobIfdPair(order.state.clone())),
    };
    let fam = family_of_token_hash(&s.token_tpl_hash().expect("orders pin a token program"))?;
    Ok(s.into_family(fam))
}

/// The base units a leg fills.
pub fn leg_amount(l: &Leg) -> i64 {
    match l {
        Leg::Ask { amount, .. }
        | Leg::Bid { amount, .. }
        | Leg::CondAsk { amount, .. }
        | Leg::CondBid { amount, .. }
        | Leg::IfdBid { amount, .. }
        | Leg::IfdAsk { amount, .. }
        | Leg::Pair { amount, .. }
        | Leg::CondPair { amount, .. }
        | Leg::IfdPair { amount, .. } => *amount,
    }
}

fn l_utxo(l: &Leg) -> &Utxo {
    match l {
        Leg::Ask { order, .. } => &order.utxo,
        Leg::Bid { order, .. } => &order.utxo,
        Leg::CondAsk { order, .. } => &order.utxo,
        Leg::CondBid { order, .. } => &order.utxo,
        Leg::IfdBid { order, .. } => &order.utxo,
        Leg::IfdAsk { order, .. } => &order.utxo,
        Leg::Pair { order, .. } => &order.utxo,
        Leg::CondPair { order, .. } => &order.utxo,
        Leg::IfdPair { order, .. } => &order.utxo,
    }
}

fn l_merges(l: &Leg) -> bool {
    matches!(l, Leg::CondAsk { merge: Some(_), .. } | Leg::CondBid { merge: Some(_), .. } | Leg::CondPair { merge: Some(_), .. })
}

/// A token input of a batch: a custody of a leg (by slot) or a taker token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokIn {
    Leg(usize, Slot),
    Taker(usize),
}

/// One custody input of a leg: (leg, slot, utxo, token, positional).
///
/// `positional` means the transaction keeps an output of that token at the custody's own input
/// index: an IOC return, a pair order's rest or return, or a merged pair entry's custody.
type CustIn<'a> = (usize, Slot, &'a TokenUtxo, Token, bool);

/// Input indices of a batch (inputs: legs, merged entries, token inputs, updates, funding).
pub(crate) struct Layout {
    tokens: Vec<Token>,
    at: BTreeMap<(usize, Slot), usize>,
    merge_in: BTreeMap<usize, usize>,
    first_token_in: BTreeMap<[u8; 32], usize>,
    /// The token inputs in input order.
    token_inputs: Vec<(TokIn, Token)>,
    update_base: usize,
}

/// The input layout.
///
/// Order inputs come first, the legs and then the merged entries. The token inputs follow them:
/// - First every custody whose output must sit at its own input index, of any token: an IOC ask's
///   return, a pair order's rest or return, or a merged pair entry's custodies. They come right
///   after the order inputs. Those positional outputs then directly follow the positional slots of
///   the legs and the extras, so no output slot below them is ever left empty, whichever token they
///   are of.
/// - Then, token by token, the other custodies and the taker's tokens.
fn layout(b: &Batch, tokens: Vec<Token>, custs: &[CustIn]) -> Result<Layout> {
    let n_legs = b.legs.len();
    let mut next = n_legs;
    let mut merge_in = BTreeMap::new();
    for (i, l) in b.legs.iter().enumerate() {
        if l_merges(l) {
            merge_in.insert(i, next);
            next += 1;
        }
    }
    let mut at = BTreeMap::new();
    let mut token_inputs = vec![];
    for c in custs.iter().filter(|c| c.4) {
        at.insert((c.0, c.1), next);
        token_inputs.push((TokIn::Leg(c.0, c.1), c.3));
        next += 1;
    }
    let mut taker_in = vec![false; b.taker_tokens.len()];
    for t in &tokens {
        for c in custs.iter().filter(|c| !c.4 && c.3 == *t) {
            at.insert((c.0, c.1), next);
            token_inputs.push((TokIn::Leg(c.0, c.1), c.3));
            next += 1;
        }
        for (k, tk) in b.taker_tokens.iter().enumerate() {
            if tk.utxo.covenant_id == Some(t.0) {
                taker_in[k] = true;
                token_inputs.push((TokIn::Taker(k), *t));
                next += 1;
            }
        }
    }
    if taker_in.contains(&false) {
        return invalid("taker tokens of a token no leg trades");
    }
    let base = n_legs + merge_in.len();
    let mut first_token_in = BTreeMap::new();
    for (k, (_, t)) in token_inputs.iter().enumerate() {
        first_token_in.entry(t.0).or_insert(base + k);
    }
    for t in &tokens {
        let count = token_inputs.iter().filter(|(_, x)| x.0 == t.0).count();
        let max = t.1.family().max_tok_in();
        if count > max {
            return invalid(format!(
                "a transaction spending KOB orders carries at most {max} token inputs per {:?} token",
                t.1.family()
            ));
        }
    }
    Ok(Layout { tokens, at, merge_in, first_token_in, token_inputs, update_base: next })
}

/// Auction time of a leg: `t` (default the lock time), which must lie in `[origin, lockTime]`.
fn auction_t(t: Option<i64>, lock: i64, origin: i64, what: &str) -> Result<i64> {
    let t = t.unwrap_or(lock);
    if t > lock || t < origin {
        return invalid(format!("{what}: auction time t = {t} must satisfy {origin} <= t <= lockTime {lock}"));
    }
    Ok(t)
}

/// The merge rules of a conditional exit's fill.
///
/// Until `rptUntil` a booked exit takes profit (leg 0) only together with its entry's merge. Only
/// a booked exit's take-profit merges its entry.
fn check_booked_merge(booked: bool, leg: i64, merges: bool, lock: i64, rpt_until: i64) -> Result<()> {
    if booked && leg == 0 && !merges && lock < rpt_until {
        return invalid("a booked exit takes profit only with its entry's merge until rptUntil");
    }
    if merges && (leg != 0 || !booked) {
        return invalid("only a booked exit's take-profit merges its entry");
    }
    Ok(())
}

/// Whether an if-done entry fill of `n` books its exit, and the fill's time argument.
///
/// A fill below `rptAmount` books its exit. A booked fill without an auction time runs at `t`,
/// which defaults to the lock time.
fn booked_cycle(rpt_amount: i64, min_fill: i64, n: i64, t_arg: i64, t: Option<i64>, lock: i64, strict: bool) -> Result<(bool, i64)> {
    let booked = rpt_amount > n;
    if booked && n >= MERGE_SHIFT {
        return invalid("a booked exit's amount must be below 2^53 (the merge argument)");
    }
    if strict {
        rpt_fill_rule(rpt_amount, min_fill, n)?;
    }
    let t_arg = if booked && t_arg == 0 { t.unwrap_or(lock) } else { t_arg };
    if booked && t_arg > lock {
        return invalid("cycle time t must be <= lockTime");
    }
    Ok((booked, t_arg))
}

/// The role branch of an if-done entry fill: it continues, waits for its exits, or closes.
fn entry_branch(rest: bool, cont: bool) -> &'static str {
    if rest {
        "cont"
    } else if cont {
        "wait"
    } else {
        "close"
    }
}

/// Builds a [`Batch`].
pub fn build_batch(b: &Batch, budgets: BudgetFn) -> Result<BuiltTx> {
    build_batch_mode(b, budgets, true)
}

/// [`build_batch`] without some of the builder's own refusals, for adversarial tests only.
///
/// It needs feature `adversarial` and is never in a release build. It skips the refusals of pair
/// activation, of covenant-owned taker tokens and of a repeat entry's fill beyond its re-arms. It
/// lays out transactions the covenants must reject, so the engine-level suites can show that the
/// covenant, not the builder, refuses them. Those suites mutate the built transactions for
/// everything else.
#[cfg(feature = "adversarial")]
pub fn build_batch_unchecked(b: &Batch, budgets: BudgetFn) -> Result<BuiltTx> {
    build_batch_mode(b, budgets, false)
}

/// What one pass of the batch builder yields: the transaction, or the order of legs it needs.
enum Pass {
    Built(Box<BuiltTx>),
    /// New position `j` holds the original leg `perm[j]`.
    Reorder(Vec<usize>),
}

/// The order of legs the refund slots need, or `None` when the legs are already in it.
///
/// A terminal bid fill with a refund sends its unspent KAS to the maker at output `M - 1 - i`,
/// where i is its input index and M the number of outputs. The fills with a refund are inputs 0..k,
/// so their refunds are the last k outputs, in the reverse order of their inputs. The other legs
/// keep their order.
fn refund_order(refunds: &[Option<(u64, [u8; 32])>]) -> Option<Vec<usize>> {
    let k = refunds.iter().filter(|r| r.is_some()).count();
    if refunds[..k].iter().all(|r| r.is_some()) {
        return None;
    }
    Some((0..refunds.len()).filter(|i| refunds[*i].is_some()).chain((0..refunds.len()).filter(|i| refunds[*i].is_none())).collect())
}

/// The batch with its legs in the order `perm`.
///
/// New position `j` holds the original leg `perm[j]`, and the evidence indices of the legs and
/// updates follow their legs.
fn permute_batch(b: &Batch, perm: &[usize]) -> Batch {
    let mut inv = vec![0usize; perm.len()];
    for (j, o) in perm.iter().enumerate() {
        inv[*o] = j;
    }
    let map = |x: &mut Option<usize>| {
        if let Some(k) = x {
            *k = inv.get(*k).copied().unwrap_or(*k);
        }
    };
    let mut out = b.clone();
    out.legs = perm.iter().map(|o| b.legs[*o].clone()).collect();
    for l in out.legs.iter_mut() {
        match l {
            Leg::CondAsk { evidence, .. }
            | Leg::CondBid { evidence, .. }
            | Leg::IfdBid { evidence, .. }
            | Leg::IfdAsk { evidence, .. } => map(evidence),
            Leg::CondPair { evidence, evidence_b, .. } | Leg::IfdPair { evidence, evidence_b, .. } => {
                map(evidence);
                map(evidence_b);
            }
            Leg::Ask { .. } | Leg::Bid { .. } | Leg::Pair { .. } => {}
        }
    }
    for u in out.updates.iter_mut() {
        u.evidence = inv.get(u.evidence).copied().unwrap_or(u.evidence);
        map(&mut u.evidence_b);
    }
    out
}

/// Renames the legs in a refusal of the reordered batch back to their original positions.
///
/// `leg <j>` becomes `leg <perm[j]>`.
fn unpermute_error(e: Error, perm: &[usize]) -> Error {
    let Error::Invalid(m) = e else { return e };
    let mut out = String::new();
    let mut rest = m.as_str();
    while let Some(at) = rest.find("leg ") {
        let (head, tail) = rest.split_at(at + 4);
        out.push_str(head);
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        match digits.parse::<usize>().ok().and_then(|j| perm.get(j)) {
            Some(o) if !digits.is_empty() => {
                out.push_str(&o.to_string());
                rest = &tail[digits.len()..];
            }
            _ => rest = tail,
        }
    }
    out.push_str(rest);
    Error::Invalid(out)
}

fn build_batch_mode(b: &Batch, budgets: BudgetFn, strict: bool) -> Result<BuiltTx> {
    match build_batch_pass(b, budgets, strict)? {
        Pass::Built(t) => Ok(*t),
        Pass::Reorder(perm) => {
            match build_batch_pass(&permute_batch(b, &perm), budgets, strict).map_err(|e| unpermute_error(e, &perm))? {
                Pass::Built(t) => Ok(*t),
                Pass::Reorder(_) => invalid("the legs with a refund output could not be put first"),
            }
        }
    }
}

/// The tokens of each leg of a batch, by leg.
struct LegTokens {
    /// The leg's main token.
    main: Vec<Token>,
    /// A pair leg's tokens (A, B).
    pair: Vec<Option<(Token, Token)>>,
    /// A pair leg's amounts.
    calcs: Vec<Option<pair::PairCalc>>,
}

/// The tokens of every leg, and the amounts of the pair legs.
fn collect_leg_tokens(b: &Batch, lock: i64, strict: bool) -> Result<LegTokens> {
    let mut leg_tokens: Vec<Token> = vec![];
    let mut pair_toks: Vec<Option<(Token, Token)>> = vec![];
    let mut calcs: Vec<Option<pair::PairCalc>> = vec![];
    for (i, l) in b.legs.iter().enumerate() {
        match pair::leg_pair_tokens(l).map_err(|e| leg_order_error(i, e))? {
            Some((a, bt)) => {
                leg_tokens.push(pair::leg_main_token(l, a, bt));
                pair_toks.push(Some((a, bt)));
                calcs.push(pair::calc_leg(l, lock, strict).map_err(|e| leg_fill_error(i, e))?);
            }
            None => {
                leg_tokens.push(leg_state(l).and_then(|s| order_token(&s)).map_err(|e| leg_order_error(i, e))?);
                pair_toks.push(None);
                calcs.push(None);
            }
        }
    }
    Ok(LegTokens { main: leg_tokens, pair: pair_toks, calcs })
}

/// Every token the legs trade, in first-use order.
///
/// One covenant id never comes with two programs.
fn batch_tokens(leg_tokens: &[Token], pair_toks: &[Option<(Token, Token)>]) -> Result<Vec<Token>> {
    let mut tokens: Vec<Token> = vec![];
    for (i, t) in leg_tokens.iter().enumerate() {
        for x in std::iter::once(*t).chain(pair_toks[i].map(|(a, bt)| [a, bt]).into_iter().flatten()) {
            if !tokens.contains(&x) {
                tokens.push(x);
            }
        }
    }
    for (i, x) in tokens.iter().enumerate() {
        if tokens[..i].iter().any(|y| y.0 == x.0) {
            return invalid("one token covenant id with two different programs");
        }
    }
    Ok(tokens)
}

/// The custody inputs of every leg.
fn custody_inputs<'a>(
    b: &'a Batch,
    leg_tokens: &[Token],
    pair_toks: &[Option<(Token, Token)>],
    calcs: &[Option<pair::PairCalc>],
) -> Result<Vec<CustIn<'a>>> {
    let mut custs: Vec<CustIn> = vec![];
    for (i, l) in b.legs.iter().enumerate() {
        let tok = leg_tokens[i];
        match l {
            Leg::Ask { order, custody, amount, .. } => {
                let ioc = order.state.tif != TIF_GTC && *amount < order.state.amount_left;
                custs.push((i, Slot::Custody, custody, tok, ioc));
            }
            Leg::CondAsk { custody, .. } | Leg::IfdAsk { custody, .. } => custs.push((i, Slot::Custody, custody, tok, false)),
            Leg::CondBid { merge: Some(m), .. } => {
                if let Some(c) = &m.custody {
                    custs.push((i, Slot::Entry, c, tok, false));
                }
            }
            Leg::Pair { .. } | Leg::CondPair { .. } | Leg::IfdPair { .. } => {
                let (a, bt) = pair_toks[i].expect("pair leg");
                for (slot, c, t, positional) in pair::leg_custodies(l, calcs[i].as_ref().expect("pair calc"), a, bt)? {
                    custs.push((i, slot, c, t, positional));
                }
            }
            _ => {}
        }
    }
    Ok(custs)
}

/// Covenant ids spent by the batch.
///
/// Each order is spent at most once, and a booked exit's entry only in its own merge.
fn spent_covenant_ids(b: &Batch) -> Result<BTreeSet<[u8; 32]>> {
    let mut spent_ids: BTreeSet<[u8; 32]> = BTreeSet::new();
    for l in &b.legs {
        let id = l_utxo(l).covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
        if !spent_ids.insert(id) {
            return invalid("an order appears twice in one transaction");
        }
    }
    for l in &b.legs {
        let merged = match l {
            Leg::CondAsk { merge: Some(e), .. } => e.utxo.covenant_id,
            Leg::CondBid { merge: Some(e), .. } => e.entry.utxo.covenant_id,
            _ => continue,
        };
        let id = merged.ok_or_else(|| Error::Invalid("merged entry has no covenant id".into()))?;
        if !spent_ids.insert(id) {
            return invalid("an entry merges one exit per transaction and is not otherwise spent in it");
        }
    }
    for u in &b.updates {
        let id = u.order.utxo.covenant_id.ok_or_else(|| Error::Invalid("updated order has no covenant id".into()))?;
        if !spent_ids.insert(id) {
            return invalid("an updated order is spent once and not filled in the same transaction");
        }
    }
    Ok(spent_ids)
}

/// The extension commitment of each token of a batch.
///
/// Every token input and output of a token shares it.
#[derive(Default)]
struct Exts(BTreeMap<[u8; 32], [u8; 32]>);

impl Exts {
    /// Records commitment `e` for token `tok`, which must agree with the one already recorded.
    fn merge(&mut self, tok: [u8; 32], e: [u8; 32]) -> Result<()> {
        if *self.0.entry(tok).or_insert(e) != e {
            return invalid("orders and tokens of different extension commitments cannot share a batch");
        }
        Ok(())
    }
}

/// The facts of one leg of a batch that its planner reads.
#[derive(Clone, Copy)]
struct LegCtx<'a> {
    batch: &'a Batch,
    lay: &'a Layout,
    /// The leg's position in the batch.
    index: usize,
    /// The leg's main token.
    token: Token,
    family: Family,
    /// The base units the leg fills.
    amount: i64,
    lock: i64,
    /// The order UTXO.
    utxo: &'a Utxo,
    utxo_daa: i64,
    /// The order UTXO's KAS.
    value: i64,
    cov_id: [u8; 32],
    strict: bool,
}

impl LegCtx<'_> {
    /// The input index of the first input of the leg's token.
    fn first_tok(&self, what: &str) -> Result<usize> {
        self.lay
            .first_token_in
            .get(&self.token.0)
            .copied()
            .ok_or_else(|| Error::Invalid(format!("{what}: no input of its token in the batch")))
    }
}

/// Plans a `KobAsk` fill: the maker's payment, and the rest or the IOC return of its custody.
fn plan_ask(cx: &LegCtx, order: &OrderUtxo<AskState>, custody: &TokenUtxo, t: Option<i64>, exts: &mut Exts) -> Result<LegPlan> {
    let LegCtx { lay, index: i, token, family, amount: n, lock, utxo, utxo_daa, value: order_value, cov_id, .. } = *cx;
    let state = &order.state;
    check_custody(custody, Some(cov_id), token, state.custody_amount())?;
    check_custody_ext(custody, state.extension_commitment)?;
    exts.merge(token.0, custody.state.extension())?;
    if lock < state.active_from {
        return invalid(format!("ask at leg {i} is not active before DAA {}", state.active_from));
    }
    let t = if state.slope != 0 { auction_t(t, lock, need(state.origin(utxo_daa), "decay origin")?, "decaying ask")? } else { 0 };
    if !state.fill_ok(n) {
        return invalid(format!(
            "leg {i}: an ask fill of {n} needs n <= amountLeft {}, n >= minFill {} unless it takes everything left, and n <= maxFill {} (0 = no cap)",
            state.amount_left, state.min_fill, state.max_fill
        ));
    }
    let proceeds = need(state.proceeds(n, t, utxo_daa), "ask proceeds")?;
    let rest = state.amount_left - n;
    let mut extras = vec![];
    let (payout, branch) = if rest > 0 && state.tif == TIF_GTC {
        extras.push(Extra::Cont(utxo.amount, AskState { amount_left: rest, ..state.clone() }.spk_for(family)));
        extras.push(Extra::Tok { value: custody.utxo.amount, state: custody.state.with_amount(rest), token, patch: Some((i, 2)) });
        (proceeds, "rest")
    } else if rest > 0 {
        if state.tif == TIF_FOK {
            return invalid("FOK ask must be filled completely");
        }
        let token_state = custody.state.with_amount(rest).with_holder_owner(token.1, state.maker);
        extras.push(Extra::AtInput {
            slot: Slot::Custody,
            value: custody.utxo.amount,
            state: token_state,
            token,
            patch: Some((i, 2)),
        });
        (add(proceeds, order_value, "ask payout")?, "ioc")
    } else {
        (add(add(proceeds, order_value, "ask payout")?, custody.utxo.amount as i64, "ask payout")?, "close")
    };
    let mut role = format!("{}.settle.{branch}", TemplateId::KobAsk.for_program(token.1).name());
    if state.interval > 0 {
        role.push_str(".twap");
    }
    if state.slope != 0 {
        role.push_str(".decay");
    }
    Ok(LegPlan {
        pos: PosOut::Pay(pos(payout, "ask payout")?, state.maker),
        role,
        sequence: state.interval.max(0) as u64,
        plan: entry(
            TemplateId::KobAsk.for_program(token.1),
            state.encode_for(family),
            "settle",
            vec![nb(n), Arg::Int(lay.at[&(i, Slot::Custody)] as i64), Arg::Int(0), Arg::Int(t)],
        ),
        extras,
        suffix: format!("@{}", token.1.name()),
        flows: vec![(token, n, 0)],
        merge: None,
        adjust: None,
    })
}

/// Plans a `KobBid` fill: the delivery, and the continuation or the refund of its KAS.
fn plan_bid(
    cx: &LegCtx,
    order: &OrderUtxo<BidState>,
    t: Option<i64>,
    exts: &mut Exts,
    refund_slot: &mut Option<(u64, [u8; 32])>,
) -> Result<LegPlan> {
    let LegCtx { index: i, token, family, amount: n, lock, utxo_daa, value: order_value, .. } = *cx;
    let state = &order.state;
    if lock < state.active_from {
        return invalid(format!("bid at leg {i} is not active before DAA {}", state.active_from));
    }
    let t = if state.slope != 0 { auction_t(t, lock, need(state.origin(utxo_daa), "rise origin")?, "rising bid")? } else { 0 };
    if state.max_fill > 0 && n > state.max_fill {
        return invalid("amount above the DCA maxFill");
    }
    if state.min_fill <= 0 {
        return invalid("a bid with minFill <= 0 never fills (the covenant requires minFill > 0)");
    }
    let used = need(state.used(n), "bid budget")?;
    let spend = need(state.spend(n, t, utxo_daa), "bid spend")?;
    let left = sub(order_value, used, "bid escrow")?;
    if left < state.reserve {
        return invalid(format!("leg {i}: the bid cannot afford {n} base units"));
    }
    let can_continue = state.can_continue(left);
    if n < state.min_fill && can_continue {
        return invalid(format!(
            "leg {i}: a bid fill of {n} is below minFill {} while the bid could continue (only its last fill may be smaller)",
            state.min_fill
        ));
    }
    kron_output(family, n, "bid delivery")?;
    let mut extras = vec![];
    let (delivery, branch) = if state.tif == TIF_GTC && can_continue {
        extras.push(Extra::Cont(pos(left - state.delivery_carrier, "bid continuation")?, state.spk_for(family)));
        (add(state.delivery_carrier, used - spend, "bid delivery")?, "cont")
    } else {
        if state.tif == TIF_FOK && can_continue {
            return invalid("FOK bid must be filled completely");
        }
        // The KAS the bid does not spend beyond the delivery carrier returns to the
        // maker at the end of the transaction once it is REFUND_MIN or more. Below that
        // it rides on the delivery.
        let refund_value = sub(order_value - spend, state.delivery_carrier, "bid refund")?;
        if refund_value >= crate::tx::REFUND_MIN as i64 {
            *refund_slot = Some((refund_value as u64, state.maker));
            (state.delivery_carrier, "close")
        } else {
            (order_value - spend, "close")
        }
    };
    let mut role = format!("{}.fill.{branch}", TemplateId::KobBid.for_program(token.1).name());
    if state.interval > 0 {
        role.push_str(".dca");
    }
    if state.slope != 0 {
        role.push_str(".rising");
    }
    exts.merge(token.0, state.extension_commitment)?;
    Ok(LegPlan {
        pos: PosOut::Deliver(
            pos(delivery, "bid delivery")?,
            TokenState::user(token.1, n, state.maker, state.extension_commitment),
            token,
        ),
        role,
        sequence: state.interval.max(0) as u64,
        plan: entry(
            TemplateId::KobBid.for_program(token.1),
            state.encode_for(family),
            "fill",
            vec![nb(n), Arg::Int(cx.first_tok("bid")? as i64), Arg::Int(t)],
        ),
        extras,
        suffix: format!("@{}", token.1.name()),
        flows: vec![(token, 0, n)],
        merge: None,
        adjust: None,
    })
}

/// Plans a `KobCondAsk` fill on its take-profit or stop leg, and the merge of a booked exit's entry.
#[allow(clippy::too_many_arguments)]
fn plan_cond_ask(
    cx: &LegCtx,
    order: &OrderUtxo<CondAskState>,
    custody: &TokenUtxo,
    leg: u8,
    ev: Option<usize>,
    t: Option<i64>,
    merge: Option<&OrderUtxo<IfdBidState>>,
    exts: &mut Exts,
) -> Result<LegPlan> {
    let LegCtx { batch, lay, index: i, token, family, amount: n, lock, utxo, utxo_daa, value: order_value, cov_id, .. } = *cx;
    let state = &order.state;
    check_custody(custody, Some(cov_id), token, state.custody_amount())?;
    check_custody_ext(custody, state.extension_commitment)?;
    exts.merge(token.0, custody.state.extension())?;
    if lock < state.active_from {
        return invalid("conditional ask not active yet");
    }
    if !state.fill_ok(n) {
        return invalid(format!(
            "leg {i}: a conditional ask fill of {n} needs n <= amountLeft {} and n >= minFill {} unless it takes everything left",
            state.amount_left, state.min_fill
        ));
    }
    let leg = leg as i64;
    let mut trigger = false;
    let mut t_arg = 0;
    let (mut ev_arg, mut tk_arg) = (0, 0);
    match leg {
        0 if state.tp_price > 0 => {}
        1 if state.stop_price > 0 && state.stop_price <= MAX_STOP_PRICE && (0..=10_000).contains(&state.slip_bps) => {
            if state.armed == 0 {
                let k = ev.ok_or_else(|| Error::Invalid("unarmed stop leg needs trigger evidence".into()))?;
                let (e, ev_index, tk_index) = evidence(batch, lay, k)?;
                e.check(state.token_cov_id, state.scale, state.min_touch, state.min_rest_daa, lock)?;
                if e.side != SIDE_ASK || e.price > state.stop_price {
                    return invalid("a stop sell triggers only on a resting ask filled at or below the stop");
                }
                (ev_arg, tk_arg) = (ev_index, tk_index);
                trigger = true;
            } else if state.band_daa > 0 {
                t_arg = auction_t(t, lock, armed_origin(state.armed, utxo_daa).expect("armed"), "stop auction")?;
            }
        }
        _ => return invalid("invalid conditional leg for this order"),
    }
    let leg_price = need(state.leg_price(leg, trigger, t_arg, utxo_daa), "conditional ask leg price")?;
    let proceeds = need(state.proceeds(n, leg_price), "conditional ask proceeds")?;
    let rest = state.amount_left - n;
    let carriers = add(order_value, custody.utxo.amount as i64, "carriers")?;
    let mut extras = vec![];
    if rest > 0 {
        let next = CondAskState { amount_left: rest, armed: state.next_armed(leg, trigger, utxo_daa), ..state.clone() };
        extras.push(Extra::Cont(utxo.amount, next.spk_for(family)));
        extras.push(Extra::Tok { value: custody.utxo.amount, state: custody.state.with_amount(rest), token, patch: Some((i, 2)) });
    }
    check_booked_merge(state.is_booked(), leg, merge.is_some(), lock, state.rpt_until)?;
    let (payout, merge_plan) = match merge {
        None => (if rest > 0 { proceeds } else { add(proceeds, carriers, "payout")? }, None),
        Some(e) => {
            if e.utxo.covenant_id != Some(state.parent) {
                return invalid("the merged entry is not this exit's parent");
            }
            let entry_state = &e.state;
            if entry_state.token_cov_id != state.token_cov_id || !entry_state.books_exit(state.parent, state) {
                return invalid("the exit is not one the merged entry books (its terms or repeat fields differ)");
            }
            let budget = need(state.rpt_budget(n), "repeat budget")?;
            let back = if rest == 0 { add(budget, carriers, "merge")? } else { budget };
            let cont = IfdBidState {
                amount_left: add(entry_state.amount_left, n, "merged amount")?,
                armed: entry_state.merged_armed(e.utxo.block_daa_score as i64),
                ..entry_state.clone()
            };
            extras.push(Extra::EntryCont(
                u(add(e.utxo.amount as i64, back, "merged entry")?, "merge")?,
                cont.spk_for(family),
                state.parent,
            ));
            let args = vec![
                Arg::Bytes(merge_arg(i, n)?),
                Arg::Int(cx.first_tok("merge")? as i64),
                Arg::Int(0),
                Arg::Bytes(vec![]),
                Arg::Bytes(vec![]),
                Arg::Int(0),
                Arg::Int(0),
            ];
            let plan = entry(TemplateId::KobIfdBid.for_program(token.1), entry_state.encode_for(family), "fill", args);
            (
                proceeds - budget,
                Some((
                    plan,
                    format!(
                        "{}.fill.merge{}",
                        TemplateId::KobIfdBid.for_program(token.1).name(),
                        if rest == 0 { ".sellout" } else { "" }
                    ),
                    e.utxo.clone(),
                )),
            )
        }
    };
    let role = format!(
        "{}.settle.leg{leg}{}{}{}.{}",
        TemplateId::KobCondAsk.for_program(token.1).name(),
        if trigger { ".trigger" } else { "" },
        if t_arg != 0 { ".auction" } else { "" },
        if merge.is_some() { ".merge" } else { "" },
        if rest > 0 { "rest" } else { "close" }
    );
    Ok(LegPlan {
        pos: PosOut::Pay(pos(payout, "payout")?, state.maker),
        role,
        sequence: 0,
        plan: entry(
            TemplateId::KobCondAsk.for_program(token.1),
            state.encode_for(family),
            "settle",
            vec![
                nb(n),
                Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                Arg::Int(0),
                Arg::Int(leg),
                Arg::Int(ev_arg),
                Arg::Int(tk_arg),
                Arg::Int(t_arg),
            ],
        ),
        extras,
        suffix: format!("@{}", token.1.name()),
        flows: vec![(token, n, 0)],
        merge: merge_plan,
        adjust: None,
    })
}

/// Plans a `KobCondBid` fill on its take-profit or stop leg, and the merge of a booked exit's entry.
#[allow(clippy::too_many_arguments)]
fn plan_cond_bid(
    cx: &LegCtx,
    order: &OrderUtxo<CondBidState>,
    leg: u8,
    ev: Option<usize>,
    t: Option<i64>,
    merge: Option<&SellFirstEntry>,
    exts: &mut Exts,
    refund_slot: &mut Option<(u64, [u8; 32])>,
) -> Result<LegPlan> {
    let LegCtx { batch, lay, index: i, token, family, amount: n, lock, utxo_daa, value: order_value, .. } = *cx;
    let state = &order.state;
    if lock < state.active_from {
        return invalid("conditional bid not active yet");
    }
    if !state.fill_ok(n) {
        return invalid(format!(
            "leg {i}: a conditional bid fill of {n} needs n <= amountLeft {} and n >= minFill {} unless it takes everything left",
            state.amount_left, state.min_fill
        ));
    }
    let leg = leg as i64;
    let mut trigger = false;
    let mut t_arg = 0;
    let mut ev_arg = 0;
    match leg {
        0 if state.tp_price > 0 => {}
        1 if state.stop_price > 0 && state.stop_price <= MAX_STOP_PRICE && (0..=10_000).contains(&state.slip_bps) => {
            if state.armed == 0 {
                let k = ev.ok_or_else(|| Error::Invalid("unarmed stop leg needs trigger evidence".into()))?;
                let (touch, ev_index, _) = evidence(batch, lay, k)?;
                touch.check(state.token_cov_id, state.scale, state.min_touch, state.min_rest_daa, lock)?;
                if touch.side != SIDE_BID || touch.price < state.stop_price {
                    return invalid("a buy stop triggers only on a resting bid filled at or above the stop");
                }
                ev_arg = ev_index;
                trigger = true;
            } else if state.band_daa > 0 {
                t_arg = auction_t(t, lock, armed_origin(state.armed, utxo_daa).expect("armed"), "stop auction")?;
            }
        }
        _ => return invalid("invalid conditional leg for this order"),
    }
    let spend = need(
        state.spend(n, need(state.leg_price(leg, trigger, t_arg, utxo_daa), "conditional bid leg price")?),
        "conditional bid spend",
    )?;
    let left = sub(order_value, spend, "conditional bid escrow")?;
    let rest = state.amount_left - n;
    let next = CondBidState { amount_left: rest, armed: state.next_armed(leg, trigger, utxo_daa), ..state.clone() };
    check_booked_merge(state.is_booked(), leg, merge.is_some(), lock, state.rpt_until)?;
    exts.merge(token.0, state.extension_commitment)?;
    let mut extras = vec![];
    let (pos_out, merge_plan) = match merge {
        None => {
            kron_output(family, n, "conditional bid delivery")?;
            if rest > 0 {
                extras.push(Extra::Cont(pos(left - state.delivery_carrier, "continuation")?, next.spk_for(family)));
            }
            let refund_value = sub(left, state.delivery_carrier, "conditional bid refund")?;
            let refund = rest == 0 && refund_value >= crate::tx::REFUND_MIN as i64;
            if refund {
                *refund_slot = Some((refund_value as u64, state.maker));
            }
            let delivery = if rest > 0 || refund { state.delivery_carrier } else { left };
            (
                PosOut::Deliver(
                    pos(delivery, "delivery")?,
                    TokenState::user(token.1, n, state.maker, state.extension_commitment),
                    token,
                ),
                None,
            )
        }
        Some(m) => plan_cond_bid_merge(cx, order, m, rest, &next, spend, &mut extras, exts)?,
    };
    let role = format!(
        "{}.settle.leg{leg}{}{}{}.{}",
        TemplateId::KobCondBid.for_program(token.1).name(),
        if trigger { ".trigger" } else { "" },
        if t_arg != 0 { ".auction" } else { "" },
        if merge.is_some() { ".merge" } else { "" },
        if rest > 0 { "cont" } else { "close" }
    );
    Ok(LegPlan {
        pos: pos_out,
        role,
        sequence: 0,
        plan: entry(
            TemplateId::KobCondBid.for_program(token.1),
            state.encode_for(family),
            "settle",
            vec![nb(n), Arg::Int(cx.first_tok("conditional bid")? as i64), Arg::Int(leg), Arg::Int(ev_arg), Arg::Int(t_arg)],
        ),
        extras,
        suffix: format!("@{}", token.1.name()),
        flows: vec![(token, 0, n)],
        merge: merge_plan,
        adjust: None,
    })
}

/// A merged entry's input: (sigscript plan, role, UTXO).
type MergeInput = (SigPlan, String, Utxo);

/// Plans the merge of a booked `KobCondBid` exit's sell-first entry.
///
/// It adds the exit's continuation, the entry's custody and continuation to `extras`, and returns
/// the maker's take-profit payment and the entry's input.
#[allow(clippy::too_many_arguments)]
fn plan_cond_bid_merge(
    cx: &LegCtx,
    order: &OrderUtxo<CondBidState>,
    m: &SellFirstEntry,
    rest: i64,
    next: &CondBidState,
    spend: i64,
    extras: &mut Vec<Extra>,
    exts: &mut Exts,
) -> Result<(PosOut, Option<MergeInput>)> {
    let LegCtx { lay, index: i, token, family, amount: n, value: order_value, .. } = *cx;
    let state = &order.state;
    let merged_entry = &m.entry;
    if merged_entry.utxo.covenant_id != Some(state.parent) {
        return invalid("the merged entry is not this exit's parent");
    }
    let entry_state = &merged_entry.state;
    if entry_state.token_cov_id != state.token_cov_id || !entry_state.books_exit(state.parent, state) {
        return invalid("the exit is not one the merged entry books (its terms or repeat fields differ)");
    }
    let proceeds = need(state.rpt_proceeds(n), "repeat proceeds")?;
    let prefund = need(state.rpt_prefund(n), "repeat prefund")?;
    if rest > 0 {
        let keep = sub(sub(order_value, proceeds, "exit continuation")?, prefund, "exit continuation")?;
        extras.push(Extra::Cont(pos(keep, "exit continuation")?, next.spk_for(family)));
    }
    // The entry's custody after the merge: exactly amountLeft + n.
    let new_amount = add(entry_state.amount_left, n, "merged amount")?;
    kron_output(family, new_amount, "the merged entry's custody")?;
    let (cust_state, cust_value, token_in) = if entry_state.amount_left > 0 {
        let entry_custody = m.custody.as_ref().ok_or_else(|| Error::Invalid("the merged entry's custody is required".into()))?;
        check_custody(entry_custody, merged_entry.utxo.covenant_id, token, entry_state.custody_amount())?;
        // The entry's custody carries its committed exit's commitment, i.e.
        // this exit's.
        check_custody_ext(entry_custody, state.extension_commitment)?;
        exts.merge(token.0, entry_custody.state.extension())?;
        (entry_custody.state.with_amount(new_amount), entry_custody.utxo.amount, lay.at[&(i, Slot::Entry)])
    } else {
        if m.custody.is_some() {
            return invalid("an empty entry has no custody (a token UTXO owned by it is a stray)");
        }
        (
            TokenState::custody(family, new_amount, state.parent, state.extension_commitment),
            u(entry_state.exit_carrier, "exitCarrier")?,
            cx.first_tok("merge")?,
        )
    };
    extras.push(Extra::Tok { value: cust_value, state: cust_state, token, patch: Some((lay.merge_in[&i], 2)) });
    // Sell-out: everything the exit held beyond the maker's proceeds, never
    // less than the prefund of n (the KobIfdAsk merge floor).
    let back = if n == state.amount_left { need(entry_state.merge_sellout_back(n, order_value), "merge return")? } else { prefund };
    let back = back - if entry_state.amount_left == 0 { entry_state.exit_carrier } else { 0 };
    let cont = IfdAskState {
        amount_left: new_amount,
        armed: entry_state.merged_armed(merged_entry.utxo.block_daa_score as i64),
        ..entry_state.clone()
    };
    extras.push(Extra::EntryCont(
        u(add(merged_entry.utxo.amount as i64, back, "merged entry")?, "merged entry")?,
        cont.spk_for(family),
        state.parent,
    ));
    let args = vec![
        Arg::Bytes(merge_arg(i, n)?),
        Arg::Int(token_in as i64),
        Arg::Int(0),
        Arg::Int(0),
        Arg::Bytes(vec![]),
        Arg::Bytes(vec![]),
        Arg::Int(0),
        Arg::Int(0),
        Arg::Int(0),
    ];
    let plan = entry(TemplateId::KobIfdAsk.for_program(token.1), entry_state.encode_for(family), "settle", args);
    let role = format!(
        "{}.settle.merge{}{}",
        TemplateId::KobIfdAsk.for_program(token.1).name(),
        if entry_state.amount_left == 0 { ".new" } else { "" },
        if n == state.amount_left { ".sellout" } else { "" }
    );
    Ok((PosOut::Pay(pos(proceeds - spend, "take-profit")?, state.maker), Some((plan, role, merged_entry.utxo.clone()))))
}

/// Plans a `KobIfdBid` entry fill, which delivers into the custody of the exit it creates.
fn plan_ifd_bid(cx: &LegCtx, order: &OrderUtxo<IfdBidState>, ev: Option<usize>, t: Option<i64>, exts: &mut Exts) -> Result<LegPlan> {
    let LegCtx { batch, lay, token, family, amount: n, lock, utxo_daa, value: order_value, cov_id, strict, .. } = *cx;
    let state = &order.state;
    if lock < state.active_from {
        return invalid("if-done bid not active yet");
    }
    if !state.fill_ok(n) {
        return invalid("if-done fill: n <= amountLeft and n >= minFill unless it takes everything left");
    }
    kron_output(family, n, "the exit's custody")?;
    let (trigger, t_arg, ev_arg, _) =
        ifd_trigger(batch, lay, state.entry_stop, state.armed, state.band_daa, ev, t, lock, utxo_daa, |e| {
            e.check(state.token_cov_id, state.scale, state.min_touch, state.min_rest_daa, lock)?;
            if e.side != SIDE_BID || e.price < state.entry_stop {
                return invalid("a buy-stop entry triggers only on a resting bid filled at or above its trigger");
            }
            Ok(())
        })?;
    let (booked, t_arg) = booked_cycle(state.rpt_amount, state.min_fill, n, t_arg, t, lock, strict)?;
    let price = need(state.price_at(trigger, t_arg, utxo_daa), "if-done entry price")?;
    let spend = need(state.spend(n, price), "if-done bid spend")?;
    let left = sub(sub(order_value, spend, "if-done bid escrow")?, state.delivery_carrier, "if-done bid escrow")?;
    let rest = state.amount_left - n;
    let booking = booked.then_some(Booking { parent: cov_id, until: need(rpt_until(state.expiry_daa, utxo_daa), "rptUntil")? });
    let exit = state.exit_for(n, booking)?;
    let cont = rest > 0 || state.rpt_amount > 0;
    let mut extras = vec![];
    if cont {
        let next = IfdBidState {
            amount_left: rest,
            armed: state.next_armed(utxo_daa),
            rpt_amount: if booked { state.rpt_amount - n } else { state.rpt_amount },
            ..state.clone()
        };
        extras.push(Extra::Cont(pos(left - state.exit_carrier, "entry continuation")?, next.spk_for(family)));
        extras.push(Extra::Exit {
            value: u(state.exit_carrier, "exit carrier")?,
            spk: exit.spk_for(family),
            tpl: TemplateId::KobCondAsk.for_program(token.1),
            arg: 2,
        });
    } else {
        extras.push(Extra::Exit {
            value: pos(left, "exit value")?,
            spk: exit.spk_for(family),
            tpl: TemplateId::KobCondAsk.for_program(token.1),
            arg: 2,
        });
    }
    exts.merge(token.0, state.extension_commitment)?;
    let exit_template = template(TemplateId::KobCondAsk.for_program(token.1));
    Ok(LegPlan {
        pos: PosOut::DeliverToExit {
            value: u(state.delivery_carrier, "delivery carrier")?,
            amount: n,
            ext: state.extension_commitment,
            token,
        },
        role: format!(
            "{}.fill{}{}{}.{}",
            TemplateId::KobIfdBid.for_program(token.1).name(),
            if trigger { ".trigger" } else { "" },
            if state.entry_stop > 0 && state.armed != 0 && state.band_daa > 0 { ".auction" } else { "" },
            if booked { ".book" } else { "" },
            entry_branch(rest > 0, cont)
        ),
        sequence: 0,
        plan: entry(
            TemplateId::KobIfdBid.for_program(token.1),
            state.encode_for(family),
            "fill",
            vec![
                nb(n),
                Arg::Int(cx.first_tok("if-done bid")? as i64),
                Arg::Int(0),
                Arg::Bytes(exit_template.prefix.clone()),
                Arg::Bytes(exit_template.suffix.clone()),
                Arg::Int(ev_arg),
                Arg::Int(t_arg),
            ],
        ),
        extras,
        suffix: format!("@{}", token.1.name()),
        flows: vec![(token, 0, n)],
        merge: None,
        adjust: None,
    })
}

/// Plans a `KobIfdAsk` entry fill, whose positional output is the exit it creates.
fn plan_ifd_ask(
    cx: &LegCtx,
    order: &OrderUtxo<IfdAskState>,
    custody: &TokenUtxo,
    ev: Option<usize>,
    t: Option<i64>,
    exts: &mut Exts,
) -> Result<LegPlan> {
    let LegCtx { batch, lay, index: i, token, family, amount: n, lock, utxo_daa, value: order_value, cov_id, strict, .. } = *cx;
    let state = &order.state;
    check_custody(custody, Some(cov_id), token, state.custody_amount())?;
    check_custody_ext(custody, state.exit()?.extension_commitment)?;
    exts.merge(token.0, custody.state.extension())?;
    if lock < state.active_from {
        return invalid("if-done ask not active yet");
    }
    if !state.fill_ok(n) {
        return invalid("if-done fill: n <= amountLeft and n >= minFill unless it takes everything left");
    }
    let (trigger, t_arg, ev_arg, tk_arg) =
        ifd_trigger(batch, lay, state.entry_stop, state.armed, state.band_daa, ev, t, lock, utxo_daa, |e| {
            e.check(state.token_cov_id, state.scale, state.min_touch, state.min_rest_daa, lock)?;
            if e.side != SIDE_ASK || e.price > state.entry_stop {
                return invalid("a sell-stop entry triggers only on a resting ask filled at or below its trigger");
            }
            Ok(())
        })?;
    let (booked, t_arg) = booked_cycle(state.rpt_amount, state.min_fill, n, t_arg, t, lock, strict)?;
    let price = need(state.price_at(trigger, t_arg, utxo_daa), "if-done entry price")?;
    let proceeds = need(state.proceeds(n, price), "if-done ask proceeds")?;
    let prefund = need(state.prefund_of(n), "if-done ask prefund")?;
    let rest = state.amount_left - n;
    let booking = booked.then_some(Booking { parent: cov_id, until: need(rpt_until(state.expiry_daa, utxo_daa), "rptUntil")? });
    let exit = state.exit_for(n, booking)?;
    let cont = rest > 0 || state.rpt_amount > 0;
    let mut extras = vec![];
    let exit_value = if cont {
        let next = IfdAskState {
            amount_left: rest,
            armed: state.next_armed(utxo_daa),
            rpt_amount: if booked { state.rpt_amount - n } else { state.rpt_amount },
            ..state.clone()
        };
        let keep = order_value - prefund - state.exit_carrier + if rest == 0 { custody.utxo.amount as i64 } else { 0 };
        extras.push(Extra::Cont(pos(keep, "entry continuation")?, next.spk_for(family)));
        if rest > 0 {
            extras.push(Extra::Tok { value: custody.utxo.amount, state: custody.state.with_amount(rest), token, patch: Some((i, 2)) });
        }
        add(add(proceeds, prefund, "exit value")?, state.exit_carrier, "exit value")?
    } else {
        add(add(proceeds, order_value, "exit value")?, custody.utxo.amount as i64, "exit value")?
    };
    let exit_template = template(TemplateId::KobCondBid.for_program(token.1));
    Ok(LegPlan {
        pos: PosOut::Exit(pos(exit_value, "exit value")?, exit.spk_for(family), TemplateId::KobCondBid.for_program(token.1)),
        role: format!(
            "{}.settle{}{}{}.{}",
            TemplateId::KobIfdAsk.for_program(token.1).name(),
            if trigger { ".trigger" } else { "" },
            if state.entry_stop > 0 && state.armed != 0 && state.band_daa > 0 { ".auction" } else { "" },
            if booked { ".book" } else { "" },
            entry_branch(rest > 0, cont)
        ),
        sequence: 0,
        plan: entry(
            TemplateId::KobIfdAsk.for_program(token.1),
            state.encode_for(family),
            "settle",
            vec![
                nb(n),
                Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                Arg::Int(0),
                Arg::Int(i as i64),
                Arg::Bytes(exit_template.prefix.clone()),
                Arg::Bytes(exit_template.suffix.clone()),
                Arg::Int(ev_arg),
                Arg::Int(tk_arg),
                Arg::Int(t_arg),
            ],
        ),
        extras,
        suffix: format!("@{}", token.1.name()),
        flows: vec![(token, n, 0)],
        merge: None,
        adjust: None,
    })
}

fn build_batch_pass(b: &Batch, budgets: BudgetFn, strict: bool) -> Result<Pass> {
    if b.legs.is_empty() {
        return invalid("empty batch");
    }
    let lock = b.lock_time as i64;
    let change = b.change.or(b.funding.first().map(|f| f.pubkey));
    if change.is_none() {
        // Without a change key the spread and tips would all become fee.
        return invalid("a batch needs a change key (or a funding input) to receive the spread");
    }
    if let Some(t) = &b.taker {
        check_key(t, "taker key")?;
    }
    for p in &b.receivers {
        check_key(&p.pubkey, "token receiver")?;
    }
    let mut d = Draft::new(b.lock_time, &b.fee, change)?;
    let LegTokens { main: leg_tokens, pair: pair_toks, calcs } = collect_leg_tokens(b, lock, strict)?;
    let tokens = batch_tokens(&leg_tokens, &pair_toks)?;
    let custs = custody_inputs(b, &leg_tokens, &pair_toks, &calcs)?;
    let lay = layout(b, tokens, &custs)?;
    let n_legs = b.legs.len();
    let spent_ids = spent_covenant_ids(b)?;

    let mut exts = Exts::default();

    let mut plans: Vec<LegPlan> = Vec::with_capacity(n_legs);
    // Terminal bid fills whose unspent KAS returns to the maker in an output of its own: (value,
    // maker) per leg.
    let mut refunds: Vec<Option<(u64, [u8; 32])>> = vec![None; n_legs];
    for (i, l) in b.legs.iter().enumerate() {
        let tok = leg_tokens[i];
        let fam = tok.1.family();
        let n = leg_amount(l);
        pos(n, "amount")?;
        let utxo = l_utxo(l);
        let cx = LegCtx {
            batch: b,
            lay: &lay,
            index: i,
            token: tok,
            family: fam,
            amount: n,
            lock,
            utxo,
            utxo_daa: utxo.block_daa_score as i64,
            value: utxo.amount as i64,
            cov_id: utxo.covenant_id.expect("checked"),
            strict,
        };
        let p = match l {
            Leg::Ask { order, custody, t, .. } => plan_ask(&cx, order, custody, *t, &mut exts)?,
            Leg::Bid { order, t, .. } => plan_bid(&cx, order, *t, &mut exts, &mut refunds[i])?,
            Leg::CondAsk { order, custody, leg, evidence: ev, t, merge, .. } => {
                plan_cond_ask(&cx, order, custody, *leg, *ev, *t, merge.as_ref(), &mut exts)?
            }
            Leg::CondBid { order, leg, evidence: ev, t, merge, .. } => {
                plan_cond_bid(&cx, order, *leg, *ev, *t, merge.as_ref(), &mut exts, &mut refunds[i])?
            }
            Leg::IfdBid { order, evidence: ev, t, .. } => plan_ifd_bid(&cx, order, *ev, *t, &mut exts)?,
            Leg::IfdAsk { order, custody, evidence: ev, t, .. } => plan_ifd_ask(&cx, order, custody, *ev, *t, &mut exts)?,
            Leg::Pair { .. } | Leg::CondPair { .. } | Leg::IfdPair { .. } => {
                let (a, bt) = pair_toks[i].expect("pair leg");
                pair_exts(l, a, bt, &mut |t, e| exts.merge(t, e))?;
                pair::plan_leg(b, &lay, i, l, calcs[i].as_ref().expect("pair calc"), a, bt, lock).map_err(|e| leg_fill_error(i, e))?
            }
        };
        plans.push(p);
    }
    deliver_pair_surplus(b, &mut plans)?;
    // The covenants pin a refund at output M - 1 - i for the bid at input i, so the fills with a
    // refund are the first legs.
    if let Some(perm) = refund_order(&refunds) {
        return Ok(Pass::Reorder(perm));
    }

    // Inputs: legs, merged entries, token inputs (positional custodies first, see `layout`),
    // updates and funding.
    add_leg_inputs(&mut d, b, &lay, &plans, &custs, &mut exts, strict)?;
    let update_outs = add_update_inputs(&mut d, b, &lay, &spent_ids)?;
    for f in &b.funding {
        d.add_p2pk(f);
    }

    // Outputs: positional slots, extras per leg, update continuations, payments and the taker's
    // tokens.
    let LegOutputs { returns, mut patches, owner } = add_leg_outputs(&mut d, b, &lay, &mut plans)?;
    add_update_outputs(&mut d, b, update_outs)?;
    for p in &b.payments {
        d.add_output(pos(p.amount as i64, "payment")?, spk_from_string(&p.script_public_key)?, None);
    }
    add_taker_tokens(&mut d, b, &lay, &plans, &exts, change)?;
    // Pinned outputs at their custody input's index.
    for (at, v, st, token, patch) in returns {
        d.ensure_output(at);
        d.fill_token_output(at, token, st, v)?;
        if let Some((input, arg)) = patch {
            patches.push((input, arg, at));
        }
    }
    // Positional outputs.
    for (i, p) in plans.iter().enumerate() {
        match &p.pos {
            PosOut::Pay(v, maker) => d.fill_output(i, *v, p2pk_spk(maker), None),
            PosOut::Deliver(v, st, token) => d.fill_token_output(i, *token, st.clone(), *v)?,
            PosOut::DeliverToExit { .. } => unreachable!("resolved with the exit"),
            PosOut::Exit(v, spk, tpl) => {
                d.fill_genesis(i, i, *v, spk.clone(), Some(*tpl))?;
            }
        }
    }
    for (i, r) in refunds.iter().enumerate() {
        if let Some((value, maker)) = r {
            d.add_refund(i, *value, *maker, leg_tokens[i].0);
        }
    }
    // Patch index arguments (tokOut / exitOut / a merged pair entry's new custodies).
    for (input, arg, out) in patches {
        if let crate::tx::DPlan::Plain(SigPlan::Entry { args, .. }) = &mut d.inputs[input].plan {
            args[arg] = Arg::Int(out as i64);
        }
    }
    if !b.records.is_empty() {
        d.payload = payload::encode_with_release(&b.records)?;
    }
    d.seal(budgets).map(|t| Pass::Built(Box::new(t))).map_err(|e| dust_leg(e, &owner))
}

/// Adds the inputs of the legs and of their merged entries, then the token inputs in layout order.
fn add_leg_inputs(
    d: &mut Draft,
    b: &Batch,
    lay: &Layout,
    plans: &[LegPlan],
    custs: &[CustIn],
    exts: &mut Exts,
    strict: bool,
) -> Result<()> {
    for (i, l) in b.legs.iter().enumerate() {
        let p = &plans[i];
        d.add_input(l_utxo(l), p.plan.clone(), format!("{}{}", p.role, p.suffix), p.sequence);
    }
    for (i, p) in plans.iter().enumerate() {
        if let Some((plan, role, utxo)) = &p.merge {
            let at = d.add_input(utxo, plan.clone(), format!("{role}{}", p.suffix), SEQUENCE_NONFINAL);
            debug_assert_eq!(at, lay.merge_in[&i]);
        }
    }
    for (tin, t) in &lay.token_inputs {
        match *tin {
            TokIn::Leg(i, slot) => {
                let c = custs.iter().find(|c| c.0 == i && c.1 == slot).expect("laid out").2;
                let got = d.add_token_input(c, *t, Witness::CovenantId)?;
                debug_assert_eq!(got, lay.at[&(i, slot)]);
            }
            TokIn::Taker(k) => {
                let tk = &b.taker_tokens[k];
                check_token_input(tk, *t)?;
                if strict && !tk.state.is_holder(t.1) {
                    return invalid("taker tokens must be key-owned (a token owned by an order id is a stray)");
                }
                exts.merge(t.0, tk.state.extension())?;
                let w = if tk.state.is_user() { token_witness(&tk.state, t.1)? } else { Witness::CovenantId };
                d.add_token_input(tk, *t, w)?;
            }
        }
    }
    Ok(())
}

/// Adds the update inputs, which arm or trail an order without a fill, after the token inputs.
///
/// Their evidence is a leg of this batch. It returns each update's input index, the order's next
/// state and the keeper's take.
fn add_update_inputs(d: &mut Draft, b: &Batch, lay: &Layout, spent_ids: &BTreeSet<[u8; 32]>) -> Result<Vec<(usize, AnyState, i64)>> {
    let mut update_outs = vec![];
    for (k, u) in b.updates.iter().enumerate() {
        let (plan, role, sequence, next, take) = update_plan(b, lay, u, spent_ids)?;
        let at = d.add_input(&u.order.utxo, plan, role, sequence);
        debug_assert_eq!(at, lay.update_base + k);
        update_outs.push((at, next, take));
    }
    Ok(update_outs)
}

/// A token output pinned at a custody input's index: (index, value, state, token, patch).
type Pinned = (usize, u64, TokenState, Token, Option<(usize, usize)>);

/// What adding the outputs of the legs leaves for the end of the transaction.
struct LegOutputs {
    /// The outputs pinned at a custody input's index, filled once every other output is placed.
    returns: Vec<Pinned>,
    /// (input, argument, output): the argument of an input that receives an output's index.
    patches: Vec<(usize, usize, usize)>,
    /// The leg each output belongs to.
    owner: BTreeMap<usize, usize>,
}

/// Reserves the positional outputs and the pinned ones, and adds the extras of every leg.
fn add_leg_outputs(d: &mut Draft, b: &Batch, lay: &Layout, plans: &mut [LegPlan]) -> Result<LegOutputs> {
    let n_legs = b.legs.len();
    for _ in 0..n_legs {
        d.reserve_output();
    }
    // Outputs pinned at a custody input's index (IOC returns, pair rests and returns, merged pair
    // entries' custodies) are reserved now, so the extras skip them.
    for (i, p) in plans.iter().enumerate() {
        for x in &p.extras {
            if let Extra::AtInput { slot, .. } = x {
                d.pin_output(lay.at[&(i, *slot)]);
            }
        }
    }
    let mut returns: Vec<Pinned> = vec![];
    let mut patches: Vec<(usize, usize, usize)> = vec![];
    // The leg each output belongs to (its positional output, its extras), so that a refusal of
    // `seal` that names an output can name the leg.
    let mut owner: BTreeMap<usize, usize> = (0..n_legs).map(|i| (i, i)).collect();
    for (i, l) in b.legs.iter().enumerate() {
        let cov = l_utxo(l).covenant_id.expect("checked");
        let extras = std::mem::take(&mut plans[i].extras);
        for x in extras {
            match x {
                Extra::Cont(v, spk) => {
                    owner.insert(d.add_output(v, spk, Some((i as u16, cov))), i);
                }
                Extra::Tok { value, state, token, patch } => {
                    let o = d.add_token_output(token, state, value)?;
                    owner.insert(o, i);
                    if let Some((input, arg)) = patch {
                        patches.push((input, arg, o));
                    }
                }
                Extra::AtInput { slot, value, state, token, patch } => {
                    owner.insert(lay.at[&(i, slot)], i);
                    returns.push((lay.at[&(i, slot)], value, state, token, patch))
                }
                Extra::Exit { value, spk, tpl, arg } => {
                    let x = d.reserve_output();
                    owner.insert(x, i);
                    let id = d.fill_genesis(x, i, value, spk, Some(tpl))?;
                    patches.push((i, arg, x));
                    if let PosOut::DeliverToExit { value: dv, amount, ext, token } = plans[i].pos {
                        plans[i].pos = PosOut::Deliver(dv, TokenState::custody(token.1.family(), amount, id, ext), token);
                    }
                }
                Extra::EntryCont(v, spk, entry_cov) => {
                    owner.insert(d.add_output(v, spk, Some((lay.merge_in[&i] as u16, entry_cov))), i);
                }
            }
        }
    }
    Ok(LegOutputs { returns, patches, owner })
}

/// Adds the continuation of every update.
fn add_update_outputs(d: &mut Draft, b: &Batch, update_outs: Vec<(usize, AnyState, i64)>) -> Result<()> {
    for (k, (at, next, take)) in update_outs.into_iter().enumerate() {
        let u = &b.updates[k];
        let cov = u.order.utxo.covenant_id.expect("checked");
        // A continuation below the covenant-output bound could never be mined. It is refused here,
        // naming the update, because `seal` would refuse the whole transaction without knowing
        // which update made it.
        let cont = u.order.utxo.amount as i64 - take;
        if cont < crate::tx::COVENANT_OUTPUT_MIN as i64 {
            return invalid(format!(
                "arming update {k}: the continuation of {cont} sompi is below {} sompi (the order's value less the keeper's take {take}): its KIP-9 storage mass alone exceeds the block limit",
                crate::tx::COVENANT_OUTPUT_MIN
            ));
        }
        d.add_output(pos(cont, "update continuation")?, next.spk(), Some((at as u16, cov)));
    }
    Ok(())
}

/// Adds the taker's net tokens, per token.
fn add_taker_tokens(d: &mut Draft, b: &Batch, lay: &Layout, plans: &[LegPlan], exts: &Exts, change: Option<[u8; 32]>) -> Result<()> {
    for t in &lay.tokens {
        let (sold, bought) = token_flow(plans, *t)?;
        let supplied: i64 = b.taker_tokens.iter().filter(|x| x.utxo.covenant_id == Some(t.0)).map(|x| x.state.amount()).sum();
        let net = sold + supplied - bought;
        if net < 0 {
            return invalid(format!("the batch delivers {bought} token base units but only {} are available", sold + supplied));
        }
        if net > 0 {
            let to = b.receivers.iter().find(|r| r.covenant_id == t.0).map(|r| r.pubkey).or(b.taker).or(change);
            let to = to.ok_or_else(|| Error::Invalid("no receiver for the taker's tokens".into()))?;
            let e = exts.0.get(&t.0).copied().ok_or_else(|| Error::Invalid("no token state in the batch".into()))?;
            // A kept surplus (inventory) carries `keep_carrier` when the request names one.
            let carrier = match b.keep_carrier {
                Some(c) if b.keep_surplus.contains(&t.0) => pos(c as i64, "keepCarrier")?,
                _ => pos(b.taker_token_carrier as i64, "takerTokenCarrier")?,
            };
            d.add_token_output(*t, TokenState::user(t.1, net, to, e), carrier)?;
        }
    }
    Ok(())
}

/// Names the leg of a `seal` refusal for KIP-9 dust.
///
/// Such a refusal (`dust payout: output k (...)`) names outputs, not legs. The first named output
/// that a leg made names that leg (`fill of leg <i>: ...`), so the caller leaves out the fill that
/// made it and not another one.
fn dust_leg(e: Error, owner: &BTreeMap<usize, usize>) -> Error {
    let Error::Invalid(m) = &e else { return e };
    if !m.starts_with("dust payout:") {
        return e;
    }
    let leg = m.match_indices("output ").find_map(|(at, w)| {
        let rest = &m[at + w.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || !rest[digits.len()..].starts_with(" (") {
            return None;
        }
        digits.parse::<usize>().ok().and_then(|k| owner.get(&k).copied())
    });
    match leg {
        Some(i) => leg_fill_error(i, e),
        None => e,
    }
}

/// Base units of token `t` the legs release into the transaction and take out of it.
///
/// `sold` comes from asks, pair custodies and the like. `bought` goes to bid deliveries, pair
/// deliveries and exit custodies.
fn token_flow(plans: &[LegPlan], t: Token) -> Result<(i64, i64)> {
    let mut sold = 0i64;
    let mut bought = 0i64;
    for (tok, s, b) in plans.iter().flat_map(|p| p.flows.iter()) {
        if tok.0 == t.0 {
            sold = add(sold, *s, "token amounts of the batch")?;
            bought = add(bought, *b, "token amounts of the batch")?;
        }
    }
    Ok((sold, bought))
}

/// The extension commitments a pair leg's token outputs carry.
///
/// They are its custodies' commitments, copied to their rests and returns, and the state's, for T
/// deliveries and an entry's new outputs.
fn pair_exts(l: &Leg, a: Token, bt: Token, merge_ext: &mut impl FnMut([u8; 32], [u8; 32]) -> Result<()>) -> Result<()> {
    let ext = |t: Token, e: [u8; 32]| if t.1.family() == Family::Kcc20 { e } else { [0; 32] };
    match l {
        Leg::Pair { order, custody, .. } => {
            let s = &order.state;
            let (st, tt) = if s.is_ask() { (a, bt) } else { (bt, a) };
            merge_ext(st.0, custody.state.extension())?;
            merge_ext(tt.0, ext(tt, s.t_ext))?;
        }
        Leg::CondPair { order, custody, merge, .. } => {
            let s = &order.state;
            let (st, tt) = if s.is_ask() { (a, bt) } else { (bt, a) };
            merge_ext(st.0, custody.state.extension())?;
            merge_ext(tt.0, ext(tt, s.t_ext))?;
            if let Some(m) = merge {
                for c in m.a_custody.iter().chain(m.b_custody.iter()) {
                    merge_ext(c.utxo.covenant_id.unwrap_or([0; 32]), c.state.extension())?;
                }
                merge_ext(a.0, ext(a, m.entry.state.a_ext))?;
                merge_ext(bt.0, ext(bt, m.entry.state.b_ext))?;
            }
        }
        Leg::IfdPair { order, a_custody, b_custody, .. } => {
            for c in a_custody.iter().chain(b_custody.iter()) {
                merge_ext(c.utxo.covenant_id.unwrap_or([0; 32]), c.state.extension())?;
            }
            merge_ext(a.0, ext(a, order.state.a_ext))?;
            merge_ext(bt.0, ext(bt, order.state.b_ext))?;
        }
        _ => {}
    }
    Ok(())
}

/// Gives the tokens a batch releases beyond what its legs take to the pair asks that buy them.
///
/// The surplus of each token goes to the delivery of the first pair ask (`KobPair`, `KobCondPair`)
/// of the batch that buys it. Its guarantee is a minimum, and the covenant pins the delivered
/// amount the filler names. Taker tokens of a token mean the matcher fills from its inventory, and
/// [`Batch::keep_surplus`] names the tokens the taker takes into inventory. Their surplus then
/// stays with the taker. A KRON delivery above the program's output limit keeps the excess with the
/// taker.
fn deliver_pair_surplus(b: &Batch, plans: &mut [LegPlan]) -> Result<()> {
    let mut bought: Vec<Token> = vec![];
    for t in plans.iter().filter_map(|p| p.adjust.map(|(t, _)| t)) {
        if !bought.contains(&t) {
            bought.push(t);
        }
    }
    for t in bought {
        if b.taker_tokens.iter().any(|x| x.utxo.covenant_id == Some(t.0)) || b.keep_surplus.contains(&t.0) {
            continue;
        }
        let (sold, taken) = token_flow(plans, t)?;
        let rest = sold - taken;
        if rest <= 0 {
            continue;
        }
        let Some(p) = plans.iter_mut().find(|p| p.adjust.is_some_and(|(bt, _)| bt == t)) else { continue };
        let arg = p.adjust.expect("found").1;
        let PosOut::Deliver(_, st, _) = &mut p.pos else { continue };
        let total = add(st.amount(), rest, "pair delivery")?;
        if t.1.family() == Family::Kron && total > KRON_MAX_OUTPUT_AMOUNT {
            continue;
        }
        *st = st.with_amount(total);
        if let SigPlan::Entry { args, .. } = &mut p.plan {
            if let Arg::Int(x) = &mut args[arg] {
                *x = add(*x, rest, "pair delivery")?;
            }
        }
        p.flows.push((t, 0, rest));
    }
    Ok(())
}

/// Trigger, auction time and evidence arguments `(ev, tk)` of an if-done entry fill.
///
/// An unarmed stop entry needs trigger evidence, a plain leg of the batch checked by `check`, and
/// fills at its trigger. An armed one runs its auction at `t`.
#[allow(clippy::too_many_arguments)]
fn ifd_trigger(
    b: &Batch,
    lay: &Layout,
    entry_stop: i64,
    armed: i64,
    band_daa: i64,
    ev: Option<usize>,
    t: Option<i64>,
    lock: i64,
    utxo_daa: i64,
    check: impl Fn(&Touch) -> Result<()>,
) -> Result<(bool, i64, i64, i64)> {
    if entry_stop <= 0 {
        if ev.is_some() {
            return invalid("a limit entry takes no trigger evidence");
        }
        return Ok((false, 0, 0, 0));
    }
    if armed == 0 {
        let k = ev.ok_or_else(|| Error::Invalid("an unarmed stop entry needs trigger evidence".into()))?;
        let (e, a, tk) = evidence(b, lay, k)?;
        check(&e)?;
        return Ok((true, 0, a, tk));
    }
    if band_daa > 0 {
        return Ok((false, auction_t(t, lock, armed_origin(armed, utxo_daa).expect("armed"), "stop-entry auction")?, 0, 0));
    }
    Ok((false, 0, 0, 0))
}

/// What `update` of a stop entry requires besides the evidence.
///
/// The entry needs an amount to fill, since an entry whose whole amount sits in its exits has
/// nothing an arm could serve. The stop must be on the limit's side, or no fill can ever follow the
/// arm.
fn arm_entry_rules(amount_left: i64, stop_beyond_limit: bool) -> Result<()> {
    if amount_left <= 0 {
        return invalid("an entry with nothing left cannot be armed (its amount is in its exits)");
    }
    if stop_beyond_limit {
        return invalid("a stop entry whose trigger is beyond its limit can never fill and cannot be armed");
    }
    Ok(())
}

/// Plans one [`BatchUpdate`].
///
/// It returns the `update` input (sigscript plan, role, sequence), the order's next state and the
/// keeper's take. The evidence rules are the conditional leg's (`Touch::check`). An arm needs the
/// trigger side, and a trailing ratchet the opposite side and at least one justified step. A booked
/// exit is never updated next to its repeat entry. `spent` holds the covenant ids the batch spends,
/// and the covenants refuse `update` when the parent is among the inputs.
fn update_plan(b: &Batch, lay: &Layout, u: &BatchUpdate, spent: &BTreeSet<[u8; 32]>) -> Result<(SigPlan, String, u64, AnyState, i64)> {
    if u.order.state.is_pair() {
        return pair::update_plan_pair(b, lay, u, spent);
    }
    if u.evidence_b.is_some() {
        return invalid("evidenceB names the B leg of a pair order's evidence (mode 0)");
    }
    let o = &u.order;
    let lock = b.lock_time as i64;
    let fam = o.state.family();
    let tok = order_token(&o.state)?;
    let (e, ev_arg, tk_arg) = evidence(b, lay, u.evidence)?;
    let check = |min_touch, min_rest, active_from, token, scale| -> Result<()> {
        if lock < active_from {
            return invalid("the order is not active yet");
        }
        e.check(token, scale, min_touch, min_rest, lock)
    };
    let not_next_to_parent = |parent: [u8; 32]| -> Result<()> {
        if parent != [0; 32] && spent.contains(&parent) {
            return invalid("a booked exit is never armed or trailed in a transaction that also spends its repeat entry");
        }
        Ok(())
    };
    let (next, what, sequence): (AnyState, &str, u64) = match &o.state {
        AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => {
            if s.armed != 0 || s.stop_price <= 0 {
                return invalid("only an unarmed stop order can be armed or trailed");
            }
            not_next_to_parent(s.parent)?;
            check(s.min_touch, s.min_rest_daa, s.active_from, s.token_cov_id, s.scale)?;
            if e.side == SIDE_ASK {
                if e.price > s.stop_price {
                    return invalid("arming a stop sell needs a resting ask filled at or below the stop");
                }
                (AnyState::KobCondAsk(CondAskState { armed: 1, ..s.clone() }).into_family(fam), "arm", 0)
            } else {
                let k = s.trail_steps(e.price);
                if k < 1 {
                    return invalid("the evidence justifies no trailing step");
                }
                let stop = s.stop_price + k * s.trail_step;
                (
                    AnyState::KobCondAsk(CondAskState { stop_price: stop, ..s.clone() }).into_family(fam),
                    "trail",
                    s.trail_wait.max(0) as u64,
                )
            }
        }
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => {
            if s.armed != 0 || s.stop_price <= 0 {
                return invalid("only an unarmed stop order can be armed or trailed");
            }
            not_next_to_parent(s.parent)?;
            check(s.min_touch, s.min_rest_daa, s.active_from, s.token_cov_id, s.scale)?;
            if e.side == SIDE_BID {
                if e.price < s.stop_price {
                    return invalid("arming a buy stop needs a resting bid filled at or above the stop");
                }
                (AnyState::KobCondBid(CondBidState { armed: 1, ..s.clone() }).into_family(fam), "arm", 0)
            } else {
                let k = s.trail_steps(e.price);
                if k < 1 {
                    return invalid("the evidence justifies no trailing step");
                }
                let stop = s.stop_price - k * s.trail_step;
                (
                    AnyState::KobCondBid(CondBidState { stop_price: stop, ..s.clone() }).into_family(fam),
                    "trail",
                    s.trail_wait.max(0) as u64,
                )
            }
        }
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => {
            if s.entry_stop <= 0 || s.armed != 0 {
                return invalid("only an unarmed stop entry can be armed");
            }
            arm_entry_rules(s.amount_left, s.entry_stop > s.price)?;
            check(s.min_touch, s.min_rest_daa, s.active_from, s.token_cov_id, s.scale)?;
            if e.side != SIDE_BID || e.price < s.entry_stop {
                return invalid("a buy-stop entry is armed by a resting bid filled at or above its trigger");
            }
            (AnyState::KobIfdBid(IfdBidState { armed: 1, ..s.clone() }).into_family(fam), "arm", 0)
        }
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => {
            if s.entry_stop <= 0 || s.armed != 0 {
                return invalid("only an unarmed stop entry can be armed");
            }
            arm_entry_rules(s.amount_left, s.entry_stop < s.price)?;
            check(s.min_touch, s.min_rest_daa, s.active_from, s.token_cov_id, s.scale)?;
            if e.side != SIDE_ASK || e.price > s.entry_stop {
                return invalid("a sell-stop entry is armed by a resting ask filled at or below its trigger");
            }
            (AnyState::KobIfdAsk(IfdAskState { armed: 1, ..s.clone() }).into_family(fam), "arm", 0)
        }
        _ => return invalid("only conditional orders and stop entries are updated"),
    };
    let max_take = match (&o.state, &next) {
        // A buy stop's tip never comes out of what buying its amount needs (KobCondBid.update).
        (AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s), AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x)) => {
            need(s.keeper_take(o.utxo.amount.min(i64::MAX as u64) as i64, x.stop_price), "the keeper's take of a conditional bid")?
        }
        _ => o.state.keeper_tip().expect("updatable kind"),
    };
    let take = u.take.unwrap_or(max_take);
    if !(0..=max_take).contains(&take) {
        return invalid(format!("a keeper takes at most {max_take} (keeperTip, and of a conditional bid what its buy does not need)"));
    }
    let id = o.state.template_id();
    // KobIfdBid.update(ev) reads a bid only; the others take (ev, tk).
    let args = if id.base() == TemplateId::KobIfdBid { vec![Arg::Int(ev_arg)] } else { vec![Arg::Int(ev_arg), Arg::Int(tk_arg)] };
    let role = format!("{}.update.{what}@{}", id.name(), tok.1.name());
    Ok((entry(id, o.state.encode(), "update", args), role, sequence, next, take))
}

// ---------------------------------------------------------------- two-token routes

/// A two-token route A → KAS → B in one transaction.
///
/// The payer's token A is sold into A's bids, the KAS it releases (plus any funding) buys token B
/// from B's asks, and B goes to `receiver`, e.g. a merchant for swap-and-pay. Each token keeps its
/// own KCC-20 leader and slot limits. The transaction is atomic, so the payer never ends up holding
/// KAS or a partial route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapRoute {
    #[serde(with = "crate::json::field")]
    pub lock_time: u64,
    /// Legs of token A that buy the payer's tokens (`bid`, `condBid`, `ifdBid`).
    pub sell: Vec<Leg>,
    /// Legs of token B that sell to the payer (`ask`, `condAsk`, `ifdAsk`).
    pub buy: Vec<Leg>,
    /// The payer's P2PK token A UTXOs.
    pub tokens: Vec<TokenUtxo>,
    /// Receiver of token B (default: the change key).
    #[serde(with = "crate::json::field", default)]
    pub receiver: Option<[u8; 32]>,
    /// KAS carrier on each token output of the payer / receiver.
    #[serde(with = "crate::json::field")]
    pub token_carrier: u64,
    /// Plain KAS payments in the same transaction (swap-and-pay).
    #[serde(default)]
    pub payments: Vec<Payment>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// The payer's key: token A change, KAS change.
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    /// Payload records (e.g. the x402 payment reference).
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Lowers a route to the batch it is (sell legs first, then buy legs).
pub fn route_batch(r: &SwapRoute) -> Result<Batch> {
    if r.sell.is_empty() || r.buy.is_empty() {
        return invalid("a route sells token A into bids and buys token B from asks");
    }
    let tok_of = |l: &Leg| leg_state(l).and_then(|s| order_token(&s));
    let a = tok_of(&r.sell[0])?;
    let b = tok_of(&r.buy[0])?;
    if a.0 == b.0 {
        return invalid("a route needs two different tokens (use a batch for one token)");
    }
    for l in &r.sell {
        if !matches!(l, Leg::Bid { .. } | Leg::CondBid { merge: None, .. } | Leg::IfdBid { .. }) || tok_of(l)? != a {
            return invalid("route sell legs are bids of token A");
        }
    }
    for l in &r.buy {
        if !matches!(l, Leg::Ask { .. } | Leg::CondAsk { merge: None, .. } | Leg::IfdAsk { .. }) || tok_of(l)? != b {
            return invalid("route buy legs are asks of token B");
        }
    }
    if r.tokens.iter().any(|t| t.utxo.covenant_id != Some(a.0)) {
        return invalid("the payer's tokens must all be token A");
    }
    let change = r.change.or(r.funding.first().map(|f| f.pubkey)).or(r.tokens.first().map(|t| t.state.owner()));
    let mut receivers = vec![];
    if let Some(to) = r.receiver {
        receivers.push(TokenPayee { covenant_id: b.0, pubkey: to });
    }
    Ok(Batch {
        lock_time: r.lock_time,
        legs: r.sell.iter().chain(&r.buy).cloned().collect(),
        updates: vec![],
        taker_tokens: r.tokens.clone(),
        taker: change,
        taker_token_carrier: r.token_carrier,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers,
        payments: r.payments.clone(),
        funding: r.funding.clone(),
        change,
        records: r.records.clone(),
        fee: r.fee.clone(),
    })
}

/// Builds a [`SwapRoute`] as the batch [`route_batch`] lowers it to.
pub fn build_swap_route(r: &SwapRoute, budgets: BudgetFn) -> Result<BuiltTx> {
    build_batch(&route_batch(r)?, budgets)
}

// ---------------------------------------------------------------- dispatcher

/// Every builder request (JSON: `{"action": "createOrder", ...}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)] // a request value, built once per transaction
pub enum Action {
    CreateOrder(CreateOrder),
    CancelOrder(CancelOrder),
    AmendOrder(AmendOrder),
    CancelPosition(CancelPosition),
    RefundOrder(RefundOrder),
    SendTokens(SendTokens),
    Batch(Batch),
    SwapRoute(SwapRoute),
    SweepOrder(SweepOrder),
}

/// Builds any action with the committed compute-budget table.
pub fn build(a: &Action) -> Result<BuiltTx> {
    build_with(a, &table_budgets())
}

/// Builds any action with a custom budget lookup.
pub fn build_with(a: &Action, budgets: BudgetFn) -> Result<BuiltTx> {
    match a {
        Action::CreateOrder(r) => build_create_order(r, budgets),
        Action::CancelOrder(r) => build_cancel_order(r, budgets),
        Action::AmendOrder(r) => build_amend_order(r, budgets),
        Action::CancelPosition(r) => build_cancel_position(r, budgets),
        Action::RefundOrder(r) => build_refund_order(r, budgets),
        Action::SendTokens(r) => build_send_tokens(r, budgets),
        Action::Batch(r) => build_batch(r, budgets),
        Action::SwapRoute(r) => build_swap_route(r, budgets),
        Action::SweepOrder(r) => build_sweep_order(r, budgets),
    }
}

#[cfg(test)]
mod leg_naming_tests {
    use super::*;

    #[test]
    fn a_dust_refusal_of_seal_names_the_leg_of_the_output() {
        let owner: BTreeMap<usize, usize> = [(0, 0), (1, 1), (4, 1), (5, 0)].into_iter().collect();
        let e = Error::Invalid(format!(
            "dust payout: output 4 (1000000 sompi) below {} sompi; the transaction's storage mass 1 exceeds the block limit 2 (KIP-9), so it could never be mined",
            crate::tx::DUST_OUTPUT_MIN
        ));
        let named = dust_leg(e, &owner).to_string();
        assert!(named.contains("fill of leg 1: dust payout: output 4"), "{named}");
        // An output no leg made (the change) leaves the refusal as it is.
        let e = Error::Invalid("dust payout: output 9 (5 sompi) below 2000000 sompi".into());
        assert!(!dust_leg(e, &owner).to_string().contains("of leg"));
        // Other refusals are left alone.
        let e = Error::Invalid("output 4 (1 sompi)".into());
        assert!(!dust_leg(e, &owner).to_string().contains("of leg"));
    }

    #[test]
    fn a_refusal_that_names_its_leg_is_not_named_again() {
        let e = blame("evidence", 3, Error::Invalid("a decaying ask is never trigger evidence".into()));
        let e = leg_fill_error(0, e);
        assert!(e.to_string().contains("evidence of leg 3: a decaying ask"), "{e}");
        assert!(!e.to_string().contains("fill of leg 0"), "{e}");
        let e = leg_fill_error(2, Error::Invalid("pair continuation of 5 sompi is below".into()));
        assert!(e.to_string().contains("fill of leg 2: pair continuation"), "{e}");
    }
}
