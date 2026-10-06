//! Transaction builders for every user action and every matcher shape (protocol v3: amounts in base units, no lots).
//!
//! Each builder takes a serde request (the same JSON the wasm API accepts), lays out inputs and
//! outputs exactly as the covenants require, and hands the draft to the fee pass
//! ([`crate::tx::Draft::seal`]). Layout rules shared by all builders:
//!
//! * **Anti-aliasing (payout at output i).** An order spent at input *i* is paid, delivered or
//!   refunded at output *i*. Order inputs therefore come first and outputs `0..n` are their
//!   positional outputs; extras (continuations, token remainders, exits, merged entries,
//!   update continuations, payments, the taker's tokens) follow, and the change output is always last.
//! * **One leader per token.** Token inputs follow the order inputs; the first input of
//!   each token is its leader and authorises every output of that token (KCC-20: its `next_states`
//!   list them in output order and the others are delegators; KRON: every input carries the same
//!   next-state columns and a witness byte naming its authorising input). A transaction may trade
//!   several tokens (two-token routes, pair orders); each token has its own leader and slot limits.
//! * **Token families.** The family of a leg is the family of its order's token program (`kcc20` or
//!   `kron`, see [`crate::family`]); every layout rule, entry argument and economic rule is the same, only the
//!   templates, the token states and the token input authorisation differ. KRON tokens held by a key are
//!   authorised by *address presence*: a P2PK input of that key must be in the transaction (funding).
//! * **Exact custody, no strays.** An ask-side custody must hold exactly `amountLeft` base units and be
//!   owned by its order; taker tokens must be key-owned; a transaction carries at most
//!   [`Family::max_tok_in`] inputs of a token whose orders it spends. Only the maker's cancel sweeps strays.
//! * **Amounts.** Every leg names `amount`, the base units it fills. Every KAS amount a covenant checks is computed
//!   with the exact per-kind helpers of [`crate::state`] (the quote rule, rounded in the maker's favour), and every
//!   builder refuses a fill the covenant refuses: the minimum fill (`minFill`), the TWAP / DCA `maxFill`, a price below
//!   its tip, an arithmetic overflow.
//! * **Makers get exactly their bound.** Asks are paid exactly their all-in minimum, bid
//!   continuations carry exactly the unused budget, merged entries get back exactly what their
//!   covenants require; everything else (spread, tips) goes to the change output, i.e. the
//!   matcher or taker. The fee pass never touches a covenant output, except the maker's own new
//!   order of an amend ([`absorber_for`]: a tiny change, or the fee of an in-place amend, rides on it).

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
    build_cancel_intent, build_create_intent, build_execute_intent, build_expire_intent, intent_budgets, router_role, CancelIntent,
    CreateIntent, ExecuteIntent, ExecutionFacts, ExpireIntent, IntentAsk, IntentBid, INTENT_OUTPUT, LOCK_OUTPUT, ROUTER_CANCEL_BUDGET,
    ROUTER_EXPIRE_BUDGET, ROUTER_FILL_BUDGET_FALLBACK,
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

/// A covenant amount the builder computed with a `state` helper: `None` is where the covenant fails (an overflow, a
/// price below its tip), so the builder refuses the fill.
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

/// Repeat merge argument `-(k · 2^53 + m)` as a fixed 8-byte script number (sign-magnitude,
/// little endian): the booked exit at input `k` (`k <= 1023`) takes profit on `m` base units (`0 < m < 2^53`).
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

/// The token of an order and its program (a pair order: its base token A). Also checks that the order kind is of the
/// family of that program (a `KobBidKron` trades a KRON token) and that the state is consistent with the family.
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

/// Order state of a leg, as the kind of the family of its token program.
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

/// Protocol sanity of a new order: rules that would otherwise make the order unfillable or
/// unrefundable, and the normative wallet rules of `docs/spec/matcher.md` §10 that the covenants
/// cannot check (the covenants enforce the rest at fill time; builders refuse to create duds), and the numeric gate
/// ([`AnyState::check_numbers`]: a power-of-ten scale up to 10^9, every full fill worth less than 2^62).
pub fn check_new_order(s: &AnyState) -> Result<()> {
    if s.is_pair() {
        return pair::check_new_pair(s);
    }
    let fam = s.family();
    // Family and extension-commitment consistency of the order and its token program.
    let program = order_token(s)?.1;
    s.check_numbers().map_err(Error::Invalid)?;
    // A carrier that becomes a token output of `program` (a delivery, a new custody) must clear the program's floor and the
    // dust bound; one that becomes a plain KAS output (an exit order UTXO) the dust bound.
    let token_carrier = |what: &str, v: i64, program: TemplateId| -> Result<()> {
        let floor = program.min_token_output().unwrap_or(0).max(crate::tx::DUST_OUTPUT_MIN);
        if v < floor as i64 {
            return invalid(format!(
                "{what} {v} sompi is below {floor} sompi: the least KAS a token output of {} can carry (program floor, and the KIP-9 dust bound of {} sompi)",
                program.name(),
                crate::tx::DUST_OUTPUT_MIN
            ));
        }
        Ok(())
    };
    let kas_carrier = |what: &str, v: i64| -> Result<()> {
        if v < crate::tx::DUST_OUTPUT_MIN as i64 {
            return invalid(format!(
                "{what} {v} sompi is dust: below {} sompi an output's KIP-9 storage mass alone exceeds the block limit",
                crate::tx::DUST_OUTPUT_MIN
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
        // a KRON fill moves at most the program's output limit: a minimum fill above it could never fill partially
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
    // a TWAP / DCA cap below the minimum fill: every fill is either below minFill or above maxFill, the order never fills
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
            // a repeating entry that sold out takes its exitCarrier back as the carrier of a new custody
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
            // The exit holds the proceeds plus the prefund of its amount: enough for its worst leg (rates per whole token;
            // the exit's spend is rounded down and the entry's proceeds and prefund up, so the rates bound every fill).
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

/// Smallest order value that keeps a new order fillable (bids: one minimum fill; if-done entries: their
/// whole escrow; a pair order: the KAS of its fills, `pair::min_pair_value`).
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

/// Create any order kind in one transaction: the order genesis (authorised by the first funding
/// input) and, for token-holding kinds, the move of exactly `amountLeft` base units of the maker's tokens
/// into 0x04 custody of the new covenant id (a pair order: each custody it holds, [`AnyState::custodies`], of its own
/// token; `tokens` may then hold both tokens), plus the `KOB1` placement record.
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
    /// Day orders: the wall-clock deadline (UTC unix seconds) published in the placement record
    /// (see [`crate::defaults::day_order`]).
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
    /// Token-holding kinds: the custody token UTXO (a pair order: the first of [`AnyState::custodies`]).
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    /// A sell-first `KobIfdPair`'s B prefund custody (the second of [`AnyState::custodies`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund: Option<TokenUtxo>,
    /// Stray token UTXOs owned by the order's covenant id: the cancel is the only path that
    /// moves them; they are swept to the maker. A pair order's strays may be of either of its
    /// tokens (A or B): each token is swept in its own output.
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
    /// Foreign strays (tokens of other covenant ids owned by the order id, `matcher.md` §1.2): returned to the maker, one
    /// output per token.
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

/// The new order of a cancel-replace. Token kinds take `amountLeft` base units from the old custody,
/// the strays and the top-up tokens; the rest returns to the maker.
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

/// Amend a plain ask IN PLACE (maker, SIGHASH_ALL `cancel`): the order's covenant id continues with the new
/// state at output 0 and the custody, owned by that id, stays where it is, so the token program is never
/// revealed (an 8/8 ask amend is a fifth of a cancel-replace). Only the terms [`payload::check_amend_terms`]
/// allows change (price, tip, minimum fill, time-in-force, timing, decay; not the maker, the token, the scale or `amountLeft`:
/// those re-custody, use [`CancelOrder::replace`]). The `KOB1` payload carries an `AMEND` record.
///
/// A plain bid is amended the same way: it owns no tokens, its quantity is its escrow, so it keeps the maker, the
/// token and the scale and may change every other term; the continuation must still fund one minimum fill at the new terms
/// ([`min_order_value`]), and a larger escrow (a top-up) needs `funding` and `value`.
///
/// Fee: without `funding` the order's own carrier (a bid: its escrow) pays it (the continuation holds the order's
/// value minus the fee, at least [`MIN_AMEND_CARRIER`] and a bid's one-minimum-fill floor); with funding, the continuation holds `value` (default: the order's
/// value) and the change returns to the maker unless it is tiny (then it rides on the order, see [`absorber_for`]).
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

/// Least KAS an in-place amend that pays its fee from the order's carrier leaves on the order (0.1 KAS, and never
/// less than the order's `refundTip`, which a refund keeper takes from the carriers); a smaller carrier needs funding.
pub const MIN_AMEND_CARRIER: u64 = 10_000_000;

/// Refund an order after its soft expiry, 90 days idle or (IOC / FOK) its kill time (anyone;
/// the keeper keeps at most `refundTip`). An empty repeating `KobIfdAsk` is refunded by `close`.
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
    /// Foreign strays (`matcher.md` §1.2) the refund returns to the MAKER, one output per token: their programs authorise
    /// them with any spend of the order and no refund path reads another token, so a keeper may carry them out of an order
    /// that is about to end (after which nothing could ever move them). Never the order's own token (the refund refuses it).
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

/// Stray UTXOs of ONE token other than the order's own (a foreign stray, `matcher.md` §1.2), all owned by the order's
/// covenant id, with the token's program (the builders need it to move them). They are swept to the maker in one output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignStrays {
    pub token: TokenRef,
    pub utxos: Vec<TokenUtxo>,
}

/// The maker's SWEEP of an order's strays IN PLACE (maker, SIGHASH_ALL `cancel`): the order's covenant id continues with
/// the SAME state (the same script) at output 0 and its custody, owned by that id, stays where it is; the strays (of its
/// own token, of either token of a pair order, and foreign ones) go back to the maker, one output per token. A `SWEEP` record
/// (`kob1-payload.md`) names the continuation so indexers keep the order live; nothing about the order changes except its
/// UTXO (its DAA score restarts the 90-day idle window, as any continuation does). Strays of one token must share one
/// extension commitment and fit the program's token inputs: sweep the rest in another sweep (the order is still there).
///
/// Fee: without `funding` the order's own carrier pays it (the continuation keeps at least [`MIN_AMEND_CARRIER`] and the
/// order's `refundTip`; a bid keeps its whole escrow, so a bid's sweep needs funding).
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

fn token_witness(s: &TokenState) -> Result<Witness> {
    match s {
        TokenState::Kcc20(k) => match k.owner_scheme {
            SCHEME_P2PK => Ok(Witness::P2pk(k.owner)),
            SCHEME_COVID => Ok(Witness::CovenantId),
            other => invalid(format!("token owner scheme {other:#04x} is not supported by the builders")),
        },
        TokenState::Kron(k) => match k.id_type {
            crate::family::KRON_TYPE_ADDR => Ok(Witness::P2pk(k.owner)),
            crate::family::KRON_TYPE_COVID => Ok(Witness::CovenantId),
            other => invalid(format!(
                "KRON tokens of id_type {other} (pubkey / script hash) are not supported by the builders: move them to an address-presence (id_type 3) UTXO first"
            )),
        },
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

/// The custody of an order: owned by the order's covenant id and holding exactly `amount`
/// (anything else owned by the id is a stray and never stands in for the custody).
fn check_custody(c: &TokenUtxo, order_cov: Option<[u8; 32]>, token: Token, amount: i64) -> Result<()> {
    check_token_input(c, token)?;
    // a KRON token UTXO holds at most 10^9 base units (both pinned KRON programs refuse more): such a custody cannot exist
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
    let mut d = Draft::new(r.lock_time, &r.fee, Some(r.change.unwrap_or(r.funding[0].pubkey)))?;
    let holds = r.order.holds_tokens();
    let mut ext = None;
    if holds {
        if r.tokens.is_empty() {
            return invalid("token-holding orders need token inputs");
        }
        for t in &r.tokens {
            check_token_input(t, token)?;
            if !t.state.is_user() {
                return invalid("order tokens must come from key-owned UTXOs (P2PK owner scheme / KRON address presence)");
            }
            if *ext.get_or_insert(t.state.extension()) != t.state.extension() {
                return invalid("token inputs mix extension commitments");
            }
            d.add_token_input(t, token, token_witness(&t.state)?)?;
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
        let carrier = pos(r.token_carrier as i64, "tokenCarrier")?;
        let idx = d.add_token_output(token, TokenState::custody(fam, amount, order_id, ext), carrier)?;
        custody = Some(Custody { token_output: idx as u16, extension_commitment: ext });
        if have > amount {
            // Token change returns to the owner of the first token input.
            let owner = r.tokens[0].state.owner();
            d.add_token_output(token, TokenState::user(fam, have - amount, owner, ext), carrier)?;
        }
    }
    let mut records = vec![Record::order(0, &r.order, custody, r.deadline)];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode(&records)?;
    d.seal(budgets)
}

/// [`build_create_order`] of a pair order: one custody output per custody it holds (of its own token, with that token's
/// change), the placement record with its custody parts.
fn build_create_pair(r: &CreateOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let (a, b) = pair_programs(&r.order)?;
    if r.funding.is_empty() {
        return invalid("creating an order needs a P2PK funding input (it authorises the genesis)");
    }
    let min = min_order_value(&r.order);
    if (r.value as i64) < min {
        return invalid(format!("order value {} is below the {min} sompi this order needs to be fillable", r.value));
    }
    let custodies = r.order.custodies();
    if custodies.is_empty() {
        return invalid("a pair order holds tokens in custody");
    }
    let mut d = Draft::new(r.lock_time, &r.fee, Some(r.change.unwrap_or(r.funding[0].pubkey)))?;
    let tok_of = |cov: [u8; 32]| if cov == a.0 { a } else { b };
    // the maker's tokens, per token: (token, total, extension, first owner)
    let mut groups: Vec<(Token, i64, [u8; 32], [u8; 32])> = vec![];
    for t in &r.tokens {
        let tok = match t.utxo.covenant_id {
            Some(c) if c == a.0 || c == b.0 => tok_of(c),
            _ => return invalid("a pair order takes tokens of its own two tokens only"),
        };
        check_token_input(t, tok)?;
        if !t.state.is_user() {
            return invalid("order tokens must come from key-owned UTXOs (P2PK owner scheme / KRON address presence)");
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
        d.add_token_input(t, tok, token_witness(&t.state)?)?;
    }
    let auth = d.inputs.len();
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let (_, order_id) = d.add_genesis(auth, vec![(r.value, r.order.spk())], Some(r.order.template_id()))?;
    let carrier = pos(r.token_carrier as i64, "tokenCarrier")?;
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
        g.1 -= amount;
        let idx = d.add_token_output(tok, TokenState::custody(tok.1.family(), *amount, order_id, g.2), carrier)?;
        parts.push(Custody { token_output: idx as u16, extension_commitment: g.2 });
    }
    for (tok, rest, ext, owner) in groups {
        if rest > 0 {
            d.add_token_output(tok, TokenState::user(tok.1.family(), rest, owner, ext), carrier)?;
        }
    }
    let mut parts = parts.into_iter();
    let mut records = vec![Record::order_with_prefund(0, &r.order, parts.next(), parts.next(), r.deadline)];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode(&records)?;
    d.seal(budgets)
}

/// The order's own tokens (a pair order: A and B; a KAS kind: its token), its custodies (token, exact amount) and the
/// program suffix of its roles.
/// An order's own tokens, its custodies and its role suffix ([`own_tokens`]).
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

pub fn build_cancel_order(r: &CancelOrder, budgets: BudgetFn) -> Result<BuiltTx> {
    let o = &r.order;
    let id = o.state.template_id();
    let (tokens, custodies, suffix) = own_tokens(&o.state)?;
    let subject = CancelSubject {
        maker: o.state.maker(),
        tokens,
        custodies,
        plan: entry(id, o.state.encode(), "cancel", vec![Arg::Sig(o.state.maker())]),
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

/// The inputs and options of a cancel (of [`CancelOrder`] or [`CancelRetired`]).
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

/// What the maker's cancel of one order needs: its maker, its own tokens (a pair order or a retired cross limit: A and
/// B), the custodies it holds (token, exact amount), the order input's signing plan and budget role (a pinned template's
/// state, or a retired template's lot state: [`build_cancel_retired`]).
struct CancelSubject {
    maker: [u8; 32],
    tokens: Vec<Token>,
    custodies: Vec<(Token, i64)>,
    plan: SigPlan,
    role: String,
}

/// The maker's cancel of an order (custody, strays of either of its tokens, foreign strays and the carrier back to the
/// maker), optionally replacing it (`r.replace`; never for a retired template).
fn cancel_with(subject: CancelSubject, utxo: &Utxo, r: CancelParts<'_>, budgets: BudgetFn) -> Result<BuiltTx> {
    let CancelSubject { maker, tokens, custodies, plan, role } = subject;
    let mut d = Draft::new(r.lock_time, r.fee, Some(r.change.unwrap_or(maker)))?;
    let oi = d.add_input(utxo, plan, role, 0);
    let order_cov = utxo.covenant_id.ok_or_else(|| Error::Invalid("order UTXO has no covenant id".into()))?;
    // one group per own token: (token, units, extension, carriers)
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
        if !t.state.is_user() || t.state.owner() != maker {
            return invalid("top-up tokens must be the maker's own key-owned tokens");
        }
        put(&mut groups, t, "top-up")?;
        d.add_token_input(t, tok, Witness::P2pk(maker))?;
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
        let (outs, new_id) = d.add_genesis(oi, vec![(rep.value, rep.order.spk())], Some(rep.order.template_id()))?;
        // a tiny change rides on the replacement (the change rule of the amend builders)
        d.absorber = absorber_for(&rep.order, rep.value, outs[0]);
        let default_carrier = groups.iter().find_map(|g| g.3.first().copied()).unwrap_or(0);
        let mut parts = vec![];
        for (tok, amount) in rep_custodies {
            let g = groups.iter_mut().find(|g| g.0 == tok).expect("same tokens");
            if amount > g.1 {
                return invalid(format!("the replacement needs {amount} base units; custody, strays and top-up hold {}", g.1));
            }
            let e = g.2.ok_or_else(|| Error::Invalid("the replacement needs tokens (custody or top-up)".into()))?;
            let carrier =
                pos(rep.token_carrier.unwrap_or(g.3.first().copied().unwrap_or(default_carrier)) as i64, "replacement tokenCarrier")?;
            let idx = d.add_token_output(tok, TokenState::custody(tok.1.family(), amount, new_id, e), carrier)?;
            parts.push(Custody { token_output: idx as u16, extension_commitment: e });
            g.1 -= amount;
        }
        let mut parts = parts.into_iter();
        records.push(Record::order_with_prefund(0, &rep.order, parts.next(), parts.next(), rep.deadline));
    }
    for (tok, back, e, carriers) in groups {
        if back > 0 {
            let st = TokenState::user(tok.1.family(), back, maker, e.expect("tokens"));
            d.add_token_output(tok, st, pos(carriers[0] as i64, "token carrier")?)?;
        }
    }
    add_foreign_outputs(&mut d, foreign, maker)?;
    records.extend(r.records.iter().cloned());
    if !records.is_empty() {
        d.payload = payload::encode(&records)?;
    }
    d.seal(budgets)
}

/// The maker's cancel of an order placed under a RETIRED template (`crate::retired`: spend-only support, so a live order
/// of an older template can always be ended by its maker). Same transaction as [`build_cancel_order`] (custody, strays of
/// either token and the carrier back to the maker), with the order input spending the retired script. The order's lot
/// state ([`crate::retired::lot::LotState`]) gives the maker, the token, the custody amount (`lotsLeft × lotUnits ×
/// unit`) and a cross limit's token B (strays).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelRetired {
    /// The retired template's hash (`crate::retired::retired`).
    #[serde(with = "crate::json::field")]
    pub template_hash: [u8; 32],
    /// The order's state span under the retired template (its current script).
    #[serde(with = "crate::json::field")]
    pub state: Vec<u8>,
    /// The order UTXO.
    pub order: Utxo,
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
    /// Foreign strays returned to the maker (as [`CancelOrder::foreign`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignStrays>,
    #[serde(default)]
    pub funding: Vec<KeyUtxo>,
    /// Change key (default: the maker).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Builds the maker's cancel of an order of a retired template.
pub fn build_cancel_retired(r: &CancelRetired, budgets: BudgetFn) -> Result<BuiltTx> {
    let rt = crate::retired::by_hash(&r.template_hash).ok_or_else(|| {
        Error::Invalid(format!("{} is not a retired template this build can spend", crate::json::to_hex(&r.template_hash)))
    })?;
    let state = crate::retired::decode_any(rt, &r.state)?;
    let maker = state.maker();
    check_key(&maker, "order maker")?;
    let (cov, tpl_hash, pre, suf) = state.token();
    let program = token_program(&tpl_hash, pre, suf)?;
    // the token's family: the template's (a lot template), or the v3 cross limit's aFamily
    let family = if rt.is_no_lot() {
        state.a_family().ok_or_else(|| Error::Invalid("cross limit: aFamily must be 1 (KCC-20) or 2 (KRON)".into()))?
    } else {
        rt.family
    };
    if program.family() != family {
        return invalid(format!("the retired {} order's token program {} is of the other family", rt.kind_name(), program.name()));
    }
    let token: Token = (cov, program);
    let mut custodies = vec![];
    if state.holds_tokens() {
        let amount = state.custody_amount().ok_or_else(|| Error::Invalid("the retired order's custody amount overflows".into()))?;
        if amount > 0 {
            custodies.push((token, amount));
        }
    }
    let mut tokens = vec![token];
    if let Some((b_family, b_cov, b_tpl, b_pre, b_suf)) = state.cross_b() {
        let fam = crate::state::family_of_code(b_family)
            .ok_or_else(|| Error::Invalid("cross limit: bFamily must be 1 (KCC-20) or 2 (KRON)".into()))?;
        let bp = token_program(&b_tpl, b_pre, b_suf)?;
        if bp.family() != fam {
            return invalid(format!("cross limit: token B program {} is not of the bFamily {fam:?}", bp.name()));
        }
        tokens.push((b_cov, bp));
    }
    let subject = CancelSubject {
        maker,
        tokens,
        custodies,
        plan: SigPlan::Retired {
            template_hash: r.template_hash,
            state: r.state.clone(),
            entry: "cancel".into(),
            args: vec![Arg::Sig(maker)],
        },
        // a retired template's cancel has its own budget role: its script is not today's (an older template is often larger)
        role: format!("{}.cancel.retired.{}@{}", rt.kind.name(), &crate::json::to_hex(&r.template_hash)[..8], program.name()),
    };
    let parts = CancelParts {
        custody: r.custody.as_ref(),
        prefund: None,
        strays: &r.strays,
        foreign: &r.foreign,
        tokens: &[],
        funding: &r.funding,
        change: r.change,
        replace: None,
        lock_time: 0,
        records: &[],
        fee: &r.fee,
    };
    cancel_with(subject, &r.order, parts, budgets)
}

/// The change rule of the amend builders (cancel-replace and in-place amend; `docs/spec/kob1-payload.md`, *Change*):
/// the maker's new order output takes a TINY change instead of a change output ([`crate::tx::Absorber`]). A plain
/// ask's order UTXO is a carrier (it returns to the maker with the order's proceeds or tokens, and no covenant rule
/// reads more than its floor), so it takes any tiny change; a plain bid's is its escrow, so it takes at most what
/// keeps its buying power ([`BidState::buying_power`]: the most a fill can buy) unchanged; every other kind keeps the change output (their values fund keeper tips, exits or prefunded deliveries).
pub(crate) fn absorber_for(s: &AnyState, value: u64, output: usize) -> Option<crate::tx::Absorber> {
    let max_add = match s {
        AnyState::KobAsk(_) | AnyState::KobAskKron(_) => u64::MAX,
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => {
            // buying_power(v) = floor(budget * scale / rate), budget = v - deliveryCarrier - reserve: it stays while
            // budget + x <= used(power + 1) - 1
            let budget = (value as i64).checked_sub(b.delivery_carrier)?.checked_sub(b.reserve).filter(|f| *f >= 0)?;
            let next = b.used(b.buying_power(value as i64).checked_add(1)?)?;
            u64::try_from(next.checked_sub(1)?.checked_sub(budget)?).ok()?
        }
        _ => return None,
    };
    (max_add > 0).then_some(crate::tx::Absorber { output, max_add, pays: None })
}

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
    // a bid's escrow is its quantity: the continuation must still fund one minimum fill at the new terms (as a cancel-replace's
    // replacement must); an explicit value above the order's without funding cannot be paid (insufficient funds)
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
    // the continuation: the order's covenant id, authorised by the order input (its custody stays owned by that id)
    let out = d.add_output(value, r.amended.spk(), Some((oi as u16, order_cov)));
    d.absorber = if funded {
        absorber_for(&r.amended, value, out)
    } else {
        let tip = match &r.amended {
            AnyState::KobAsk(a) | AnyState::KobAskKron(a) => a.refund_tip.max(0) as u64,
            AnyState::KobBid(b) | AnyState::KobBidKron(b) => b.refund_tip.max(0) as u64,
            _ => 0,
        };
        // a bid pays the fee out of its escrow: never below what funds one minimum fill at the new terms
        let floor = MIN_AMEND_CARRIER.max(tip).max(bid_floor.unwrap_or(0));
        Some(crate::tx::Absorber { output: out, max_add: u64::MAX, pays: Some(floor) })
    };
    let mut records = vec![Record::amend(out as u16, oi as u16, &r.amended, r.deadline)];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode(&records)?;
    d.seal(budgets)
}

/// One order of a position cancel, with its custody and any strays to sweep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelItem {
    pub order: OrderUtxo<AnyState>,
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    /// A sell-first `KobIfdPair`'s B prefund custody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund: Option<TokenUtxo>,
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
}

/// Cancel several orders of one maker and token (a pair position: one pair of tokens) in ONE transaction (`matcher.md`
/// §10.9, §10.13): an if-done entry with all its exits, a repeat with its booked exits ("cancel all"). Every token
/// (custodies, strays) returns to the maker in one output per token.
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

pub fn build_cancel_position(r: &CancelPosition, budgets: BudgetFn) -> Result<BuiltTx> {
    let first = r.orders.first().ok_or_else(|| Error::Invalid("nothing to cancel".into()))?;
    let maker = first.order.state.maker();
    let (tokens, _, _) = own_tokens(&first.order.state)?;
    let mut d = Draft::new(r.lock_time, &r.fee, Some(r.change.unwrap_or(maker)))?;
    let mut items = vec![];
    for it in &r.orders {
        let s = &it.order.state;
        let (own, custodies, suffix) = own_tokens(s)?;
        if s.maker() != maker || own != tokens {
            return invalid("a position cancel covers one maker's orders of one token (or one pair)");
        }
        let id = s.template_id();
        d.add_input(
            &it.order.utxo,
            entry(id, s.encode(), "cancel", vec![Arg::Sig(maker)]),
            format!("{}.cancel{suffix}", id.name()),
            0,
        );
        items.push((it, custodies));
    }
    // per token: (units, extension, first carrier)
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
            d.add_token_output(tok, TokenState::user(tok.1.family(), total, maker, ext.expect("tokens")), v)?;
        }
    }
    if !r.records.is_empty() {
        d.payload = payload::encode(&r.records)?;
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
    // without funding only a plain ask's carrier pays (every other kind's value is an escrow, a prefund or keeper tips)
    let carrier_pays = matches!(&o.state, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
    if !funded && !carrier_pays {
        return invalid(format!("a {} sweep needs funding (its value is not a plain carrier)", id.name()));
    }
    let mut d = Draft::new(r.lock_time, &r.fee, funded.then(|| r.change.unwrap_or(maker)))?;
    let (tokens, _, suffix) = own_tokens(&o.state)?;
    let oi =
        d.add_input(&o.utxo, entry(id, o.state.encode(), "cancel", vec![Arg::Sig(maker)]), format!("{}.cancel{suffix}", id.name()), 0);
    // the order's own token (a pair order: A and B): one group each, swept to the maker
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
    // the continuation: the same script under the order's covenant id, authorised by the order input (a custody, owned by
    // that id, stays where it is)
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
        d.add_token_output(t, TokenState::user(t.1.family(), units, maker, e), pos(carrier as i64, "stray carrier")?)?;
    }
    add_foreign_outputs(&mut d, foreign, maker)?;
    let mut records = vec![Record::Sweep { output: out as u16, input: oi as u16 }];
    records.extend(r.records.iter().cloned());
    d.payload = payload::encode(&records)?;
    d.seal(budgets)
}

/// One group of foreign strays, laid out: the token, the units and the KAS carriers it holds, its extension commitment.
type ForeignOut = (Token, i64, u64, [u8; 32]);

/// Adds the inputs of foreign stray groups owned by `order_cov` (tokens other than `exclude`, the order's own); the outputs
/// that return them ([`add_foreign_outputs`]) come after the transaction's positional outputs. Each group is one token, its
/// UTXOs share one extension commitment and fit the program's token inputs.
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

/// The maker's outputs of [`add_foreign_inputs`]: each token's units, with the KAS its strays carried.
fn add_foreign_outputs(d: &mut Draft, groups: Vec<ForeignOut>, maker: [u8; 32]) -> Result<()> {
    for (t, units, kas, e) in groups {
        d.add_token_output(t, TokenState::user(t.1.family(), units, maker, e), pos(kas as i64, "foreign stray carrier")?)?;
    }
    Ok(())
}

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
    // Funding without a change key would burn the whole funding as fee: the change goes back to the first
    // funding key. Without funding the tip is the fee (the keeper takes the difference elsewhere).
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
            d.add_token_input(c, token, Witness::CovenantId)?;
            for f in &r.funding {
                d.add_p2pk(f);
            }
            let foreign = add_foreign_inputs(&mut d, &r.foreign, order_cov, &own)?;
            let st = c.state.with_user_owner(maker);
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

/// The refund of a pair order: `KobPair` / `KobCondPair` return their custody to the maker at output 0 with every carrier
/// (minus refundTip); `KobIfdPair` pays its KAS to the maker at output 0 and returns each custody at its own index.
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

pub fn build_send_tokens(r: &SendTokens, budgets: BudgetFn) -> Result<BuiltTx> {
    let token = (r.token.covenant_id, r.token.program);
    if r.tokens.is_empty() || r.recipients.is_empty() {
        return invalid("send needs token inputs and recipients");
    }
    let fam = token.1.family();
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
        d.add_token_input(t, token, token_witness(&t.state)?)?;
    }
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let have: i64 = r.tokens.iter().map(|t| t.state.amount()).sum();
    let mut sent = 0;
    for rc in &r.recipients {
        pos(rc.amount, "recipient amount")?;
        sent += rc.amount;
        d.add_token_output(token, TokenState::user(fam, rc.amount, rc.pubkey, ext), pos(rc.carrier as i64, "recipient carrier")?)?;
    }
    if sent > have {
        return invalid(format!("sending {sent} with only {have}"));
    }
    if sent < have {
        let owner = r.token_change.unwrap_or(r.tokens[0].state.owner());
        d.add_token_output(
            token,
            TokenState::user(fam, have - sent, owner, ext),
            pos(r.token_change_carrier as i64, "token change carrier")?,
        )?;
    }
    if !r.records.is_empty() {
        d.payload = payload::encode(&r.records)?;
    }
    d.seal(budgets)
}

// ---------------------------------------------------------------- matcher shapes

/// One order filled by a batch. `amount` is the base units the leg fills (a bid's delivery, an ask's sale, a pair
/// order's base units of A). `t` is the auction / cycle time the covenant proves against the
/// lock time (CLTV); it defaults to the batch's `lockTime` (the matcher's `now − margin`, the
/// best price for the maker and the longest `rptUntil`).
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
        /// Trigger evidence of an unarmed stop leg: index of a plain `Ask` leg of this batch (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
        /// Repeat IFD: the booked exit's entry, merged (re-armed) in this transaction.
        #[serde(default)]
        merge: Option<OrderUtxo<IfdBidState>>,
    },
    /// Conditional buy; a booked sell-first exit's take-profit re-arms its entry with `merge`
    /// (the bought tokens go into the entry's custody).
    #[serde(rename_all = "camelCase")]
    CondBid {
        order: OrderUtxo<CondBidState>,
        #[serde(with = "crate::json::field")]
        amount: i64,
        leg: u8,
        /// Trigger evidence of an unarmed stop leg: index of a plain `Bid` leg of this batch (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
        #[serde(default)]
        merge: Option<SellFirstEntry>,
    },
    /// If-done buy entry (limit or stop entry, optionally repeating): creates a fresh exit
    /// holding the bought tokens.
    #[serde(rename_all = "camelCase")]
    IfdBid {
        order: OrderUtxo<IfdBidState>,
        #[serde(with = "crate::json::field")]
        amount: i64,
        /// Evidence arming an unarmed buy-stop entry in this fill: index of a plain `Bid` leg (touch).
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
        /// Evidence arming an unarmed sell-stop entry in this fill: index of a plain `Ask` leg (touch).
        #[serde(default)]
        evidence: Option<usize>,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// Pair order (`KobPair`, ask or bid): fills `amount` base units of A at its quote (a decaying order at `t`): an ask
    /// releases exactly `amount` of A and receives at least the ceil of B at its positional output (the batch's surplus of
    /// B, if any, rides on it), a bid pays exactly the floor of B from its escrow and receives exactly `amount` of A.
    #[serde(rename_all = "camelCase")]
    Pair {
        order: OrderUtxo<PairState>,
        /// The custody of S (A for an ask, B for a bid): exactly the state's `custody`, owned by the order.
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        #[serde(with = "crate::json::field", default)]
        t: Option<i64>,
    },
    /// Pair conditional (`KobCondPair`) on leg 0 (take-profit / limit) or 1 (stop: armed, or armed in this fill by the
    /// evidence). A booked exit's take-profit re-arms its `KobIfdPair` entry with `merge`.
    #[serde(rename_all = "camelCase")]
    CondPair {
        order: OrderUtxo<CondPairState>,
        /// The custody of S, exactly the state's `custody`.
        custody: TokenUtxo,
        #[serde(with = "crate::json::field")]
        amount: i64,
        leg: u8,
        /// Trigger evidence of an unarmed stop leg: with `evidenceB`, a KAS-book leg of A and one of B (mode 0); alone, a
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
    /// Pair if-done entry (`KobIfdPair`): creates a fresh `KobCondPair` exit holding the bought A (buy-first) or the
    /// proceeds plus the prefund of the fill (sell-first).
    #[serde(rename_all = "camelCase")]
    IfdPair {
        order: OrderUtxo<IfdPairState>,
        /// A sell-first entry's A custody (exactly `amountLeft`).
        #[serde(default)]
        a_custody: Option<TokenUtxo>,
        /// The entry's B custody (buy-first: the escrow; sell-first: the prefund), when it holds one.
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

/// A repeating pair entry (`KobIfdPair`) merged by its exit's take-profit, with the custodies it holds (a sell-first entry:
/// its A custody when it holds A, its B prefund when it holds one; a buy-first entry: its B escrow when it holds one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairEntryMerge {
    pub entry: OrderUtxo<IfdPairState>,
    #[serde(default)]
    pub a_custody: Option<TokenUtxo>,
    #[serde(default)]
    pub b_custody: Option<TokenUtxo>,
}

/// A repeating sell-first entry merged by its exit's take-profit, with its custody (when it
/// still holds tokens).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SellFirstEntry {
    pub entry: OrderUtxo<IfdAskState>,
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
}

/// An arm or trailing ratchet of a conditional order, or the arm of a stop entry, WITHOUT a fill of that
/// order (its `update` entry), next to the fill of the plain leg `evidence` of the same batch (touch,
/// protocol v2.6: the evidence exists only in that transaction, so arming is done by matchers inside
/// their batches). The batch's change key (the matcher) takes up to the order's `keeperTip`. A booked exit is never
/// updated in a transaction that also spends its repeat entry (the covenants refuse it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdate {
    /// A `KobCondAsk` / `KobCondBid` / `KobCondPair` (arm or trail) or an unarmed stop entry (`KobIfdBid`, `KobIfdAsk`,
    /// `KobIfdPair`).
    pub order: OrderUtxo<AnyState>,
    /// Index of the plain `Ask` / `Bid` leg whose fill is the evidence (a pair order: the KAS-book leg of A with
    /// `evidenceB`, or a `KobPair` leg of the same pair alone).
    pub evidence: usize,
    /// Pair orders, evidence mode 0: the KAS-book leg of B (the leg `evidence` is the one of A).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_b: Option<usize>,
    /// Sompi the keeper takes (default: the order's `keeperTip`).
    #[serde(with = "crate::json::field", default)]
    pub take: Option<i64>,
}

/// An N:M matcher batch, a taker transaction, or any mix: orders at their own all-in prices,
/// auctions, conditional trigger fills and updates armed by a plain fill of the same batch (touch),
/// IFD/IFO partial and stop-entry fills, repeat merges, TWAP/DCA/Dutch; legs may trade several tokens
/// (routes).
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
    /// A token output at the index of the leg's custody input `slot` (a rest or return the covenant pins there); `patch`
    /// as for `Tok`.
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
    /// Token base units the leg adds to (`sold`) and takes from (`bought`) the transaction's free balance, per token.
    flows: Vec<(Token, i64, i64)>,
    /// Merged entry input (plan, role, utxo).
    merge: Option<(SigPlan, String, Utxo)>,
    /// A pair ask's delivery that takes the batch's surplus of the token it buys: (token, the plan's argument of the
    /// delivered amount).
    adjust: Option<(Token, usize)>,
}

/// What a conditional order reads from its trigger evidence: a plain `KobAsk` / `KobBid` leg filled in
/// the same transaction (mirrors `touch` in `KobCondAsk` & co.). Pair, conditional and if-done fills are never evidence
/// of the KAS kinds (pair conditionals read pair orders: [`pair::pair_evidence`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Touch {
    /// [`SIDE_ASK`] (a resting ask sold at its quote) or [`SIDE_BID`] (a resting bid bought at its quote).
    pub side: i64,
    #[serde(with = "crate::json::field")]
    pub token_cov_id: [u8; 32],
    /// The resting order's scale (prices are comparable only at the same scale).
    pub scale: i64,
    /// The resting order's quote (sompi per whole token).
    pub price: i64,
    /// Base units traded (the evidence fill's n).
    pub amount: i64,
    /// Latest of the order UTXO's DAA plus its TWAP / DCA interval, its custody's DAA (ask) and its activeFrom.
    pub exposed_since: i64,
}

impl Touch {
    /// Evidence rules of an order (`minTouch`, `minRestDaa`) in a transaction whose lock time is
    /// `lock` (the covenant proves `tx.daa >= exposedSince + minRestDaa` by CLTV).
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

/// The touch a plain leg filling `amount` base units provides (an error for any other leg and for decaying
/// / rising orders, whose price is not a quote).
pub fn touch_of(leg: &Leg) -> Result<Touch> {
    match leg {
        Leg::Ask { order, custody, amount, .. } => {
            let s = &order.state;
            if s.slope != 0 {
                return invalid("a decaying ask is never trigger evidence");
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

/// The evidence leg `k` of a batch, its touch and its covenant arguments `(ev, tk)`: the input index of the
/// evidence order (a leg's input index is its leg index) and of its custody (asks; `-1` for a bid).
fn evidence(b: &Batch, lay: &Layout, k: usize) -> Result<(Touch, i64, i64)> {
    let l = b.legs.get(k).ok_or_else(|| Error::Invalid(format!("trigger evidence {k} is not a leg of the batch")))?;
    let t = touch_of(l)?;
    let tk = if t.side == SIDE_ASK { lay.at[&(k, Slot::Custody)] as i64 } else { -1 };
    Ok((t, k as i64, tk))
}

/// The order state of a leg as the kind of its token's family (the family follows from the token
/// program the state pins by template hash; a pair order is one template for both families).
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

/// One custody input of a leg: (leg, slot, utxo, token, positional). `positional`: the transaction keeps an output of that
/// token at the custody's own input index (an IOC return, a pair order's rest or return, a merged pair entry's custody).
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

/// The input layout. Order inputs first (legs, then merged entries), then the token inputs: FIRST every custody whose
/// output must sit at its own input index (of any token: an IOC ask's return, a pair order's rest or return, a merged pair
/// entry's custodies), right after the order inputs, so those positional outputs follow the positional slots of the legs
/// and the extras directly and no output slot below them is ever left empty, whichever token they are of; then, token by
/// token, the other custodies and the taker's tokens.
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

pub fn build_batch(b: &Batch, budgets: BudgetFn) -> Result<BuiltTx> {
    build_batch_mode(b, budgets, true)
}

/// Adversarial tests only (feature `adversarial`, never in a release build): [`build_batch`] WITHOUT some of the
/// builder's own refusals of pair shapes (activation, covenant-owned taker tokens). It lays out transactions the covenants
/// must reject, so the engine-level attack suites can show that the covenant, not the builder, refuses them; the attack
/// suites mutate the built transactions for everything else.
#[cfg(feature = "adversarial")]
pub fn build_batch_unchecked(b: &Batch, budgets: BudgetFn) -> Result<BuiltTx> {
    build_batch_mode(b, budgets, false)
}

fn build_batch_mode(b: &Batch, budgets: BudgetFn, strict: bool) -> Result<BuiltTx> {
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
    // the tokens of each leg: its main token, and a pair leg's (A, B) and amounts
    let mut leg_tokens: Vec<Token> = vec![];
    let mut pair_toks: Vec<Option<(Token, Token)>> = vec![];
    let mut calcs: Vec<Option<pair::PairCalc>> = vec![];
    for l in &b.legs {
        match pair::leg_pair_tokens(l)? {
            Some((a, bt)) => {
                leg_tokens.push(pair::leg_main_token(l, a, bt));
                pair_toks.push(Some((a, bt)));
                calcs.push(pair::calc_leg(l, lock, strict)?);
            }
            None => {
                leg_tokens.push(order_token(&leg_state(l)?)?);
                pair_toks.push(None);
                calcs.push(None);
            }
        }
    }
    // every token the legs trade, in first-use order
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
    // the custody inputs of every leg
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
    let lay = layout(b, tokens, &custs)?;
    let n_legs = b.legs.len();

    // Covenant ids spent by the batch: each order at most once; a booked exit's entry only in
    // its own merge.
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

    // Extension commitment per token (every token input and output of a token shares it).
    let mut exts: BTreeMap<[u8; 32], [u8; 32]> = BTreeMap::new();
    let mut merge_ext = |tok: [u8; 32], e: [u8; 32]| -> Result<()> {
        if *exts.entry(tok).or_insert(e) != e {
            return invalid("orders and tokens of different extension commitments cannot share a batch");
        }
        Ok(())
    };

    let mut plans: Vec<LegPlan> = Vec::with_capacity(n_legs);
    for (i, l) in b.legs.iter().enumerate() {
        let tok = leg_tokens[i];
        let fam = tok.1.family();
        let n = leg_amount(l);
        pos(n, "amount")?;
        let first_tok = |what: &str| -> Result<usize> {
            lay.first_token_in
                .get(&tok.0)
                .copied()
                .ok_or_else(|| Error::Invalid(format!("{what}: no input of its token in the batch")))
        };
        let utxo = l_utxo(l);
        let udaa = utxo.block_daa_score as i64;
        let v = utxo.amount as i64;
        let cov_id = utxo.covenant_id.expect("checked");
        let p = match l {
            Leg::Ask { order, custody, t, .. } => {
                let s = &order.state;
                check_custody(custody, Some(cov_id), tok, s.custody_amount())?;
                merge_ext(tok.0, custody.state.extension())?;
                if lock < s.active_from {
                    return invalid(format!("ask at leg {i} is not active before DAA {}", s.active_from));
                }
                let t = if s.slope != 0 { auction_t(*t, lock, need(s.origin(udaa), "decay origin")?, "decaying ask")? } else { 0 };
                if !s.fill_ok(n) {
                    return invalid(format!(
                        "leg {i}: an ask fill of {n} needs n <= amountLeft {}, n >= minFill {} unless it takes everything left, and n <= maxFill {} (0 = no cap)",
                        s.amount_left, s.min_fill, s.max_fill
                    ));
                }
                let proceeds = need(s.proceeds(n, t, udaa), "ask proceeds")?;
                let rest = s.amount_left - n;
                let mut extras = vec![];
                let (payout, branch) = if rest > 0 && s.tif == TIF_GTC {
                    extras.push(Extra::Cont(utxo.amount, AskState { amount_left: rest, ..s.clone() }.spk_for(fam)));
                    extras.push(Extra::Tok {
                        value: custody.utxo.amount,
                        state: custody.state.with_amount(rest),
                        token: tok,
                        patch: Some((i, 2)),
                    });
                    (proceeds, "rest")
                } else if rest > 0 {
                    if s.tif == TIF_FOK {
                        return invalid("FOK ask must be filled completely");
                    }
                    let st = custody.state.with_amount(rest).with_user_owner(s.maker);
                    extras.push(Extra::AtInput {
                        slot: Slot::Custody,
                        value: custody.utxo.amount,
                        state: st,
                        token: tok,
                        patch: Some((i, 2)),
                    });
                    (add(proceeds, v, "ask payout")?, "ioc")
                } else {
                    (add(add(proceeds, v, "ask payout")?, custody.utxo.amount as i64, "ask payout")?, "close")
                };
                let mut role = format!("{}.settle.{branch}", fam.kind_name("KobAsk"));
                if s.interval > 0 {
                    role.push_str(".twap");
                }
                if s.slope != 0 {
                    role.push_str(".decay");
                }
                LegPlan {
                    pos: PosOut::Pay(pos(payout, "ask payout")?, s.maker),
                    role,
                    sequence: s.interval.max(0) as u64,
                    plan: entry(
                        TemplateId::KobAsk.in_family(fam),
                        s.encode_for(fam),
                        "settle",
                        vec![nb(n), Arg::Int(lay.at[&(i, Slot::Custody)] as i64), Arg::Int(0), Arg::Int(t)],
                    ),
                    extras,
                    suffix: format!("@{}", tok.1.name()),
                    flows: vec![(tok, n, 0)],
                    merge: None,
                    adjust: None,
                }
            }
            Leg::Bid { order, t, .. } => {
                let s = &order.state;
                if lock < s.active_from {
                    return invalid(format!("bid at leg {i} is not active before DAA {}", s.active_from));
                }
                let t = if s.slope != 0 { auction_t(*t, lock, need(s.origin(udaa), "rise origin")?, "rising bid")? } else { 0 };
                if s.max_fill > 0 && n > s.max_fill {
                    return invalid("amount above the DCA maxFill");
                }
                if s.min_fill <= 0 {
                    return invalid("a bid with minFill <= 0 never fills (the covenant requires minFill > 0)");
                }
                let used = need(s.used(n), "bid budget")?;
                let spend = need(s.spend(n, t, udaa), "bid spend")?;
                let left = sub(v, used, "bid escrow")?;
                if left < s.reserve {
                    return invalid(format!("leg {i}: the bid cannot afford {n} base units"));
                }
                let can_continue = s.can_continue(left);
                if n < s.min_fill && can_continue {
                    return invalid(format!(
                        "leg {i}: a bid fill of {n} is below minFill {} while the bid could continue (only its last fill may be smaller)",
                        s.min_fill
                    ));
                }
                kron_output(fam, n, "bid delivery")?;
                let mut extras = vec![];
                let (delivery, branch) = if s.tif == TIF_GTC && can_continue {
                    extras.push(Extra::Cont(pos(left - s.delivery_carrier, "bid continuation")?, s.spk_for(fam)));
                    (add(s.delivery_carrier, used - spend, "bid delivery")?, "cont")
                } else {
                    if s.tif == TIF_FOK && can_continue {
                        return invalid("FOK bid must be filled completely");
                    }
                    (v - spend, "close")
                };
                let mut role = format!("{}.fill.{branch}", fam.kind_name("KobBid"));
                if s.interval > 0 {
                    role.push_str(".dca");
                }
                if s.slope != 0 {
                    role.push_str(".rising");
                }
                merge_ext(tok.0, s.extension_commitment)?;
                LegPlan {
                    pos: PosOut::Deliver(
                        pos(delivery, "bid delivery")?,
                        TokenState::user(fam, n, s.maker, s.extension_commitment),
                        tok,
                    ),
                    role,
                    sequence: s.interval.max(0) as u64,
                    plan: entry(
                        TemplateId::KobBid.in_family(fam),
                        s.encode_for(fam),
                        "fill",
                        vec![nb(n), Arg::Int(first_tok("bid")? as i64), Arg::Int(t)],
                    ),
                    extras,
                    suffix: format!("@{}", tok.1.name()),
                    flows: vec![(tok, 0, n)],
                    merge: None,
                    adjust: None,
                }
            }
            Leg::CondAsk { order, custody, leg, evidence: ev, t, merge, .. } => {
                let s = &order.state;
                check_custody(custody, Some(cov_id), tok, s.custody_amount())?;
                merge_ext(tok.0, custody.state.extension())?;
                if lock < s.active_from {
                    return invalid("conditional ask not active yet");
                }
                if !s.fill_ok(n) {
                    return invalid(format!(
                        "leg {i}: a conditional ask fill of {n} needs n <= amountLeft {} and n >= minFill {} unless it takes everything left",
                        s.amount_left, s.min_fill
                    ));
                }
                let leg = *leg as i64;
                let mut trigger = false;
                let mut t_arg = 0;
                let (mut ev_arg, mut tk_arg) = (0, 0);
                match leg {
                    0 if s.tp_price > 0 => {}
                    1 if s.stop_price > 0 && s.stop_price <= MAX_STOP_PRICE && (0..=10_000).contains(&s.slip_bps) => {
                        if s.armed == 0 {
                            let k = ev.ok_or_else(|| Error::Invalid("unarmed stop leg needs trigger evidence".into()))?;
                            let (e, a, tk) = evidence(b, &lay, k)?;
                            e.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock)?;
                            if e.side != SIDE_ASK || e.price > s.stop_price {
                                return invalid("a stop sell triggers only on a resting ask filled at or below the stop");
                            }
                            (ev_arg, tk_arg) = (a, tk);
                            trigger = true;
                        } else if s.band_daa > 0 {
                            t_arg = auction_t(*t, lock, armed_origin(s.armed, udaa).expect("armed"), "stop auction")?;
                        }
                    }
                    _ => return invalid("invalid conditional leg for this order"),
                }
                let leg_price = need(s.leg_price(leg, trigger, t_arg, udaa), "conditional ask leg price")?;
                let proceeds = need(s.proceeds(n, leg_price), "conditional ask proceeds")?;
                let rest = s.amount_left - n;
                let carriers = add(v, custody.utxo.amount as i64, "carriers")?;
                let mut extras = vec![];
                if rest > 0 {
                    let next = CondAskState { amount_left: rest, armed: s.next_armed(leg, trigger, udaa), ..s.clone() };
                    extras.push(Extra::Cont(utxo.amount, next.spk_for(fam)));
                    extras.push(Extra::Tok {
                        value: custody.utxo.amount,
                        state: custody.state.with_amount(rest),
                        token: tok,
                        patch: Some((i, 2)),
                    });
                }
                if s.is_booked() && leg == 0 && merge.is_none() && lock < s.rpt_until {
                    return invalid("a booked exit takes profit only with its entry's merge until rptUntil");
                }
                if merge.is_some() && (leg != 0 || !s.is_booked()) {
                    return invalid("only a booked exit's take-profit merges its entry");
                }
                let (payout, merge_plan) = match merge {
                    None => (if rest > 0 { proceeds } else { add(proceeds, carriers, "payout")? }, None),
                    Some(e) => {
                        if e.utxo.covenant_id != Some(s.parent) {
                            return invalid("the merged entry is not this exit's parent");
                        }
                        let es = &e.state;
                        if es.token_cov_id != s.token_cov_id || !es.books_exit(s.parent, s) {
                            return invalid("the exit is not one the merged entry books (its terms or repeat fields differ)");
                        }
                        let budget = need(s.rpt_budget(n), "repeat budget")?;
                        let back = if rest == 0 { add(budget, carriers, "merge")? } else { budget };
                        let cont = IfdBidState {
                            amount_left: add(es.amount_left, n, "merged amount")?,
                            armed: es.merged_armed(e.utxo.block_daa_score as i64),
                            ..es.clone()
                        };
                        extras.push(Extra::EntryCont(
                            u(add(e.utxo.amount as i64, back, "merged entry")?, "merge")?,
                            cont.spk_for(fam),
                            s.parent,
                        ));
                        let args = vec![
                            Arg::Bytes(merge_arg(i, n)?),
                            Arg::Int(first_tok("merge")? as i64),
                            Arg::Int(0),
                            Arg::Bytes(vec![]),
                            Arg::Bytes(vec![]),
                            Arg::Int(0),
                            Arg::Int(0),
                        ];
                        let plan = entry(TemplateId::KobIfdBid.in_family(fam), es.encode_for(fam), "fill", args);
                        (
                            proceeds - budget,
                            Some((
                                plan,
                                format!("{}.fill.merge{}", fam.kind_name("KobIfdBid"), if rest == 0 { ".sellout" } else { "" }),
                                e.utxo.clone(),
                            )),
                        )
                    }
                };
                let role = format!(
                    "{}.settle.leg{leg}{}{}{}.{}",
                    fam.kind_name("KobCondAsk"),
                    if trigger { ".trigger" } else { "" },
                    if t_arg != 0 { ".auction" } else { "" },
                    if merge.is_some() { ".merge" } else { "" },
                    if rest > 0 { "rest" } else { "close" }
                );
                LegPlan {
                    pos: PosOut::Pay(pos(payout, "payout")?, s.maker),
                    role,
                    sequence: 0,
                    plan: entry(
                        TemplateId::KobCondAsk.in_family(fam),
                        s.encode_for(fam),
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
                    suffix: format!("@{}", tok.1.name()),
                    flows: vec![(tok, n, 0)],
                    merge: merge_plan,
                    adjust: None,
                }
            }
            Leg::CondBid { order, leg, evidence: ev, t, merge, .. } => {
                let s = &order.state;
                if lock < s.active_from {
                    return invalid("conditional bid not active yet");
                }
                if !s.fill_ok(n) {
                    return invalid(format!(
                        "leg {i}: a conditional bid fill of {n} needs n <= amountLeft {} and n >= minFill {} unless it takes everything left",
                        s.amount_left, s.min_fill
                    ));
                }
                let leg = *leg as i64;
                let mut trigger = false;
                let mut t_arg = 0;
                let mut ev_arg = 0;
                match leg {
                    0 if s.tp_price > 0 => {}
                    1 if s.stop_price > 0 && s.stop_price <= MAX_STOP_PRICE && (0..=10_000).contains(&s.slip_bps) => {
                        if s.armed == 0 {
                            let k = ev.ok_or_else(|| Error::Invalid("unarmed stop leg needs trigger evidence".into()))?;
                            let (e, a, _) = evidence(b, &lay, k)?;
                            e.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock)?;
                            if e.side != SIDE_BID || e.price < s.stop_price {
                                return invalid("a buy stop triggers only on a resting bid filled at or above the stop");
                            }
                            ev_arg = a;
                            trigger = true;
                        } else if s.band_daa > 0 {
                            t_arg = auction_t(*t, lock, armed_origin(s.armed, udaa).expect("armed"), "stop auction")?;
                        }
                    }
                    _ => return invalid("invalid conditional leg for this order"),
                }
                let spend = need(
                    s.spend(n, need(s.leg_price(leg, trigger, t_arg, udaa), "conditional bid leg price")?),
                    "conditional bid spend",
                )?;
                let left = sub(v, spend, "conditional bid escrow")?;
                let rest = s.amount_left - n;
                let next = CondBidState { amount_left: rest, armed: s.next_armed(leg, trigger, udaa), ..s.clone() };
                if s.is_booked() && leg == 0 && merge.is_none() && lock < s.rpt_until {
                    return invalid("a booked exit takes profit only with its entry's merge until rptUntil");
                }
                if merge.is_some() && (leg != 0 || !s.is_booked()) {
                    return invalid("only a booked exit's take-profit merges its entry");
                }
                merge_ext(tok.0, s.extension_commitment)?;
                let mut extras = vec![];
                let (pos_out, merge_plan) = match merge {
                    None => {
                        kron_output(fam, n, "conditional bid delivery")?;
                        if rest > 0 {
                            extras.push(Extra::Cont(pos(left - s.delivery_carrier, "continuation")?, next.spk_for(fam)));
                        }
                        let delivery = if rest > 0 { s.delivery_carrier } else { left };
                        (
                            PosOut::Deliver(
                                pos(delivery, "delivery")?,
                                TokenState::user(fam, n, s.maker, s.extension_commitment),
                                tok,
                            ),
                            None,
                        )
                    }
                    Some(m) => {
                        let e = &m.entry;
                        if e.utxo.covenant_id != Some(s.parent) {
                            return invalid("the merged entry is not this exit's parent");
                        }
                        let es = &e.state;
                        if es.token_cov_id != s.token_cov_id || !es.books_exit(s.parent, s) {
                            return invalid("the exit is not one the merged entry books (its terms or repeat fields differ)");
                        }
                        let proceeds = need(s.rpt_proceeds(n), "repeat proceeds")?;
                        let pre = need(s.rpt_prefund(n), "repeat prefund")?;
                        if rest > 0 {
                            let keep = sub(sub(v, proceeds, "exit continuation")?, pre, "exit continuation")?;
                            extras.push(Extra::Cont(pos(keep, "exit continuation")?, next.spk_for(fam)));
                        }
                        // The entry's custody after the merge: exactly amountLeft + n.
                        let new_amount = add(es.amount_left, n, "merged amount")?;
                        kron_output(fam, new_amount, "the merged entry's custody")?;
                        let (cust_state, cust_value, token_in) = if es.amount_left > 0 {
                            let c =
                                m.custody.as_ref().ok_or_else(|| Error::Invalid("the merged entry's custody is required".into()))?;
                            check_custody(c, e.utxo.covenant_id, tok, es.custody_amount())?;
                            merge_ext(tok.0, c.state.extension())?;
                            (c.state.with_amount(new_amount), c.utxo.amount, lay.at[&(i, Slot::Entry)])
                        } else {
                            if m.custody.is_some() {
                                return invalid("an empty entry has no custody (a token UTXO owned by it is a stray)");
                            }
                            (
                                TokenState::custody(fam, new_amount, s.parent, s.extension_commitment),
                                u(es.exit_carrier, "exitCarrier")?,
                                first_tok("merge")?,
                            )
                        };
                        extras.push(Extra::Tok {
                            value: cust_value,
                            state: cust_state,
                            token: tok,
                            patch: Some((lay.merge_in[&i], 2)),
                        });
                        // Sell-out: everything the exit held beyond the maker's proceeds, never less than
                        // the prefund of n (KobIfdAsk merge floor).
                        let back = if n == s.amount_left { need(es.merge_sellout_back(n, v), "merge return")? } else { pre };
                        let back = back - if es.amount_left == 0 { es.exit_carrier } else { 0 };
                        let cont = IfdAskState {
                            amount_left: new_amount,
                            armed: es.merged_armed(e.utxo.block_daa_score as i64),
                            ..es.clone()
                        };
                        extras.push(Extra::EntryCont(
                            u(add(e.utxo.amount as i64, back, "merged entry")?, "merged entry")?,
                            cont.spk_for(fam),
                            s.parent,
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
                        let plan = entry(TemplateId::KobIfdAsk.in_family(fam), es.encode_for(fam), "settle", args);
                        let role = format!(
                            "{}.settle.merge{}{}",
                            fam.kind_name("KobIfdAsk"),
                            if es.amount_left == 0 { ".new" } else { "" },
                            if n == s.amount_left { ".sellout" } else { "" }
                        );
                        (PosOut::Pay(pos(proceeds - spend, "take-profit")?, s.maker), Some((plan, role, e.utxo.clone())))
                    }
                };
                let role = format!(
                    "{}.settle.leg{leg}{}{}{}.{}",
                    fam.kind_name("KobCondBid"),
                    if trigger { ".trigger" } else { "" },
                    if t_arg != 0 { ".auction" } else { "" },
                    if merge.is_some() { ".merge" } else { "" },
                    if rest > 0 { "cont" } else { "close" }
                );
                LegPlan {
                    pos: pos_out,
                    role,
                    sequence: 0,
                    plan: entry(
                        TemplateId::KobCondBid.in_family(fam),
                        s.encode_for(fam),
                        "settle",
                        vec![nb(n), Arg::Int(first_tok("conditional bid")? as i64), Arg::Int(leg), Arg::Int(ev_arg), Arg::Int(t_arg)],
                    ),
                    extras,
                    suffix: format!("@{}", tok.1.name()),
                    flows: vec![(tok, 0, n)],
                    merge: merge_plan,
                    adjust: None,
                }
            }
            Leg::IfdBid { order, evidence: ev, t, .. } => {
                let s = &order.state;
                if lock < s.active_from {
                    return invalid("if-done bid not active yet");
                }
                if !s.fill_ok(n) {
                    return invalid("if-done fill: n <= amountLeft and n >= minFill unless it takes everything left");
                }
                kron_output(fam, n, "the exit's custody")?;
                let (trigger, t_arg, ev_arg, _) = ifd_trigger(b, &lay, s.entry_stop, s.armed, s.band_daa, *ev, *t, lock, udaa, |e| {
                    e.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock)?;
                    if e.side != SIDE_BID || e.price < s.entry_stop {
                        return invalid("a buy-stop entry triggers only on a resting bid filled at or above its trigger");
                    }
                    Ok(())
                })?;
                let booked = s.rpt_amount > n;
                if booked && n >= MERGE_SHIFT {
                    return invalid("a booked exit's amount must be below 2^53 (the merge argument)");
                }
                let t_arg = if booked && t_arg == 0 { t.unwrap_or(lock) } else { t_arg };
                if booked && t_arg > lock {
                    return invalid("cycle time t must be <= lockTime");
                }
                let p = need(s.price_at(trigger, t_arg, udaa), "if-done entry price")?;
                let spend = need(s.spend(n, p), "if-done bid spend")?;
                let left = sub(sub(v, spend, "if-done bid escrow")?, s.delivery_carrier, "if-done bid escrow")?;
                let rest = s.amount_left - n;
                let booking =
                    booked.then_some(Booking { parent: cov_id, until: need(rpt_until(s.expiry_daa, t_arg, udaa), "rptUntil")? });
                let exit = s.exit_for(n, booking)?;
                let cont = rest > 0 || s.rpt_amount > 0;
                let mut extras = vec![];
                if cont {
                    let next = IfdBidState {
                        amount_left: rest,
                        armed: s.next_armed(udaa),
                        rpt_amount: if booked { s.rpt_amount - n } else { s.rpt_amount },
                        ..s.clone()
                    };
                    extras.push(Extra::Cont(pos(left - s.exit_carrier, "entry continuation")?, next.spk_for(fam)));
                    extras.push(Extra::Exit {
                        value: u(s.exit_carrier, "exit carrier")?,
                        spk: exit.spk_for(fam),
                        tpl: TemplateId::KobCondAsk.in_family(fam),
                        arg: 2,
                    });
                } else {
                    extras.push(Extra::Exit {
                        value: pos(left, "exit value")?,
                        spk: exit.spk_for(fam),
                        tpl: TemplateId::KobCondAsk.in_family(fam),
                        arg: 2,
                    });
                }
                merge_ext(tok.0, s.extension_commitment)?;
                let c = template(TemplateId::KobCondAsk.in_family(fam));
                LegPlan {
                    pos: PosOut::DeliverToExit {
                        value: u(s.delivery_carrier, "delivery carrier")?,
                        amount: n,
                        ext: s.extension_commitment,
                        token: tok,
                    },
                    role: format!(
                        "{}.fill{}{}{}.{}",
                        fam.kind_name("KobIfdBid"),
                        if trigger { ".trigger" } else { "" },
                        if s.entry_stop > 0 && s.armed != 0 && s.band_daa > 0 { ".auction" } else { "" },
                        if booked { ".book" } else { "" },
                        if rest > 0 {
                            "cont"
                        } else if cont {
                            "wait"
                        } else {
                            "close"
                        }
                    ),
                    sequence: 0,
                    plan: entry(
                        TemplateId::KobIfdBid.in_family(fam),
                        s.encode_for(fam),
                        "fill",
                        vec![
                            nb(n),
                            Arg::Int(first_tok("if-done bid")? as i64),
                            Arg::Int(0),
                            Arg::Bytes(c.prefix.clone()),
                            Arg::Bytes(c.suffix.clone()),
                            Arg::Int(ev_arg),
                            Arg::Int(t_arg),
                        ],
                    ),
                    extras,
                    suffix: format!("@{}", tok.1.name()),
                    flows: vec![(tok, 0, n)],
                    merge: None,
                    adjust: None,
                }
            }
            Leg::IfdAsk { order, custody, evidence: ev, t, .. } => {
                let s = &order.state;
                check_custody(custody, Some(cov_id), tok, s.custody_amount())?;
                merge_ext(tok.0, custody.state.extension())?;
                if lock < s.active_from {
                    return invalid("if-done ask not active yet");
                }
                if !s.fill_ok(n) {
                    return invalid("if-done fill: n <= amountLeft and n >= minFill unless it takes everything left");
                }
                let (trigger, t_arg, ev_arg, tk_arg) =
                    ifd_trigger(b, &lay, s.entry_stop, s.armed, s.band_daa, *ev, *t, lock, udaa, |e| {
                        e.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock)?;
                        if e.side != SIDE_ASK || e.price > s.entry_stop {
                            return invalid("a sell-stop entry triggers only on a resting ask filled at or below its trigger");
                        }
                        Ok(())
                    })?;
                let booked = s.rpt_amount > n;
                if booked && n >= MERGE_SHIFT {
                    return invalid("a booked exit's amount must be below 2^53 (the merge argument)");
                }
                let t_arg = if booked && t_arg == 0 { t.unwrap_or(lock) } else { t_arg };
                if booked && t_arg > lock {
                    return invalid("cycle time t must be <= lockTime");
                }
                let p = need(s.price_at(trigger, t_arg, udaa), "if-done entry price")?;
                let proceeds = need(s.proceeds(n, p), "if-done ask proceeds")?;
                let pre = need(s.prefund_of(n), "if-done ask prefund")?;
                let rest = s.amount_left - n;
                let booking =
                    booked.then_some(Booking { parent: cov_id, until: need(rpt_until(s.expiry_daa, t_arg, udaa), "rptUntil")? });
                let exit = s.exit_for(n, booking)?;
                let cont = rest > 0 || s.rpt_amount > 0;
                let mut extras = vec![];
                let exit_value = if cont {
                    let next = IfdAskState {
                        amount_left: rest,
                        armed: s.next_armed(udaa),
                        rpt_amount: if booked { s.rpt_amount - n } else { s.rpt_amount },
                        ..s.clone()
                    };
                    let keep = v - pre - s.exit_carrier + if rest == 0 { custody.utxo.amount as i64 } else { 0 };
                    extras.push(Extra::Cont(pos(keep, "entry continuation")?, next.spk_for(fam)));
                    if rest > 0 {
                        extras.push(Extra::Tok {
                            value: custody.utxo.amount,
                            state: custody.state.with_amount(rest),
                            token: tok,
                            patch: Some((i, 2)),
                        });
                    }
                    add(add(proceeds, pre, "exit value")?, s.exit_carrier, "exit value")?
                } else {
                    add(add(proceeds, v, "exit value")?, custody.utxo.amount as i64, "exit value")?
                };
                let c = template(TemplateId::KobCondBid.in_family(fam));
                LegPlan {
                    pos: PosOut::Exit(pos(exit_value, "exit value")?, exit.spk_for(fam), TemplateId::KobCondBid.in_family(fam)),
                    role: format!(
                        "{}.settle{}{}{}.{}",
                        fam.kind_name("KobIfdAsk"),
                        if trigger { ".trigger" } else { "" },
                        if s.entry_stop > 0 && s.armed != 0 && s.band_daa > 0 { ".auction" } else { "" },
                        if booked { ".book" } else { "" },
                        if rest > 0 {
                            "cont"
                        } else if cont {
                            "wait"
                        } else {
                            "close"
                        }
                    ),
                    sequence: 0,
                    plan: entry(
                        TemplateId::KobIfdAsk.in_family(fam),
                        s.encode_for(fam),
                        "settle",
                        vec![
                            nb(n),
                            Arg::Int(lay.at[&(i, Slot::Custody)] as i64),
                            Arg::Int(0),
                            Arg::Int(i as i64),
                            Arg::Bytes(c.prefix.clone()),
                            Arg::Bytes(c.suffix.clone()),
                            Arg::Int(ev_arg),
                            Arg::Int(tk_arg),
                            Arg::Int(t_arg),
                        ],
                    ),
                    extras,
                    suffix: format!("@{}", tok.1.name()),
                    flows: vec![(tok, n, 0)],
                    merge: None,
                    adjust: None,
                }
            }
            Leg::Pair { .. } | Leg::CondPair { .. } | Leg::IfdPair { .. } => {
                let (a, bt) = pair_toks[i].expect("pair leg");
                pair_exts(l, a, bt, &mut merge_ext)?;
                pair::plan_leg(b, &lay, i, l, calcs[i].as_ref().expect("pair calc"), a, bt, lock)?
            }
        };
        plans.push(p);
    }
    deliver_pair_surplus(b, &mut plans)?;

    // Inputs: legs, merged entries, token inputs (positional custodies first, see `layout`), updates, funding.
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
                if strict && !tk.state.is_user() {
                    return invalid("taker tokens must be key-owned (a token owned by an order id is a stray)");
                }
                merge_ext(t.0, tk.state.extension())?;
                let w = if tk.state.is_user() { Witness::P2pk(tk.state.owner()) } else { Witness::CovenantId };
                d.add_token_input(tk, *t, w)?;
            }
        }
    }
    // Updates (arm / trail without a fill), after the token inputs: their evidence is a leg of this batch.
    let mut update_outs = vec![];
    for (k, u) in b.updates.iter().enumerate() {
        let (plan, role, sequence, next, take) = update_plan(b, &lay, u, &spent_ids)?;
        let at = d.add_input(&u.order.utxo, plan, role, sequence);
        debug_assert_eq!(at, lay.update_base + k);
        update_outs.push((at, next, take));
    }
    for f in &b.funding {
        d.add_p2pk(f);
    }

    // Outputs: positional slots, extras per leg, update continuations, payments, the taker's tokens.
    for _ in 0..n_legs {
        d.reserve_output();
    }
    // Outputs pinned at a custody input's index (IOC returns, pair rests and returns, merged pair entries' custodies):
    // reserved now so the extras skip them.
    for (i, p) in plans.iter().enumerate() {
        for x in &p.extras {
            if let Extra::AtInput { slot, .. } = x {
                d.pin_output(lay.at[&(i, *slot)]);
            }
        }
    }
    type Pinned = (usize, u64, TokenState, Token, Option<(usize, usize)>);
    let mut returns: Vec<Pinned> = vec![];
    let mut patches: Vec<(usize, usize, usize)> = vec![];
    for (i, l) in b.legs.iter().enumerate() {
        let cov = l_utxo(l).covenant_id.expect("checked");
        let extras = std::mem::take(&mut plans[i].extras);
        for x in extras {
            match x {
                Extra::Cont(v, spk) => {
                    d.add_output(v, spk, Some((i as u16, cov)));
                }
                Extra::Tok { value, state, token, patch } => {
                    let o = d.add_token_output(token, state, value)?;
                    if let Some((input, arg)) = patch {
                        patches.push((input, arg, o));
                    }
                }
                Extra::AtInput { slot, value, state, token, patch } => returns.push((lay.at[&(i, slot)], value, state, token, patch)),
                Extra::Exit { value, spk, tpl, arg } => {
                    let x = d.reserve_output();
                    let id = d.fill_genesis(x, i, value, spk, Some(tpl))?;
                    patches.push((i, arg, x));
                    if let PosOut::DeliverToExit { value: dv, amount, ext, token } = plans[i].pos {
                        plans[i].pos = PosOut::Deliver(dv, TokenState::custody(token.1.family(), amount, id, ext), token);
                    }
                }
                Extra::EntryCont(v, spk, entry_cov) => {
                    d.add_output(v, spk, Some((lay.merge_in[&i] as u16, entry_cov)));
                }
            }
        }
    }
    for (k, (at, next, take)) in update_outs.into_iter().enumerate() {
        let u = &b.updates[k];
        let cov = u.order.utxo.covenant_id.expect("checked");
        d.add_output(pos(u.order.utxo.amount as i64 - take, "update continuation")?, next.spk(), Some((at as u16, cov)));
    }
    for p in &b.payments {
        d.add_output(pos(p.amount as i64, "payment")?, spk_from_string(&p.script_public_key)?, None);
    }
    // The taker's net tokens, per token.
    for t in &lay.tokens {
        let (sold, bought) = token_flow(&plans, *t)?;
        let supplied: i64 = b.taker_tokens.iter().filter(|x| x.utxo.covenant_id == Some(t.0)).map(|x| x.state.amount()).sum();
        let net = sold + supplied - bought;
        if net < 0 {
            return invalid(format!("the batch delivers {bought} token base units but only {} are available", sold + supplied));
        }
        if net > 0 {
            let to = b.receivers.iter().find(|r| r.covenant_id == t.0).map(|r| r.pubkey).or(b.taker).or(change);
            let to = to.ok_or_else(|| Error::Invalid("no receiver for the taker's tokens".into()))?;
            let e = exts.get(&t.0).copied().ok_or_else(|| Error::Invalid("no token state in the batch".into()))?;
            d.add_token_output(
                *t,
                TokenState::user(t.1.family(), net, to, e),
                pos(b.taker_token_carrier as i64, "takerTokenCarrier")?,
            )?;
        }
    }
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
    // Patch index arguments (tokOut / exitOut / a merged pair entry's new custodies).
    for (input, arg, out) in patches {
        if let crate::tx::DPlan::Plain(SigPlan::Entry { args, .. }) = &mut d.inputs[input].plan {
            args[arg] = Arg::Int(out as i64);
        }
    }
    if !b.records.is_empty() {
        d.payload = payload::encode(&b.records)?;
    }
    d.seal(budgets)
}

/// Base units of token `t` the legs release into the transaction (`sold`: asks, pair custodies, ...) and take out of it
/// (`bought`: bid deliveries, pair deliveries, exit custodies).
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

/// The extension commitments a pair leg's token outputs carry: its custodies' (copied to their rests and returns) and the
/// state's (T deliveries, an entry's new outputs).
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

/// The tokens a batch releases beyond what its legs take go to the pair asks that buy them: the surplus of each token goes
/// to the delivery of the first pair ask (`KobPair`, `KobCondPair`) of the batch buying it, whose guarantee is a minimum
/// (the covenant pins the delivered amount the filler names). Taker tokens of a token mean the matcher fills from its
/// inventory: its surplus then stays with the taker. A KRON delivery above the program's output limit keeps the excess with
/// the taker.
fn deliver_pair_surplus(b: &Batch, plans: &mut [LegPlan]) -> Result<()> {
    let mut bought: Vec<Token> = vec![];
    for t in plans.iter().filter_map(|p| p.adjust.map(|(t, _)| t)) {
        if !bought.contains(&t) {
            bought.push(t);
        }
    }
    for t in bought {
        if b.taker_tokens.iter().any(|x| x.utxo.covenant_id == Some(t.0)) {
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

/// Trigger, auction time and evidence arguments `(ev, tk)` of an if-done entry fill: an unarmed stop entry
/// needs trigger evidence (a plain leg of the batch, checked by `check`) and fills at its trigger; an armed
/// one runs its auction at `t`.
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

/// What `update` of a stop entry requires besides the evidence: an amount to fill (an entry whose amount all sits in its
/// exits has nothing an arm could serve) and the stop on the limit's side (else no fill can ever follow the arm).
fn arm_entry_rules(amount_left: i64, stop_beyond_limit: bool) -> Result<()> {
    if amount_left <= 0 {
        return invalid("an entry with nothing left cannot be armed (its amount is in its exits)");
    }
    if stop_beyond_limit {
        return invalid("a stop entry whose trigger is beyond its limit can never fill and cannot be armed");
    }
    Ok(())
}

/// Plans one [`BatchUpdate`]: the `update` input (sigscript plan, role, sequence), the order's next state
/// and the keeper's take. The evidence rules are the conditional leg's (`Touch::check`); an arm needs the
/// trigger side, a trailing ratchet the opposite side and at least one justified step. A booked exit is never updated next
/// to its repeat entry (`spent`: the covenant ids the batch spends; the covenants refuse `update` when the parent is
/// among the inputs).
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
    let max_take = o.state.keeper_tip().expect("updatable kind");
    let take = u.take.unwrap_or(max_take);
    if !(0..=max_take).contains(&take) {
        return invalid(format!("a keeper takes at most keeperTip = {max_take}"));
    }
    let id = o.state.template_id();
    // KobIfdBid.update(ev) reads a bid only; the others take (ev, tk).
    let args = if id.base() == TemplateId::KobIfdBid { vec![Arg::Int(ev_arg)] } else { vec![Arg::Int(ev_arg), Arg::Int(tk_arg)] };
    let role = format!("{}.update.{what}@{}", fam.kind_name(id.base().name()), tok.1.name());
    Ok((entry(id, o.state.encode(), "update", args), role, sequence, next, take))
}

// ---------------------------------------------------------------- two-token routes

/// A two-token route A → KAS → B in ONE transaction: the payer's token A is sold into A's bids,
/// the KAS it releases (plus any funding) buys token B from B's asks, and B goes to `receiver`
/// (e.g. a merchant for swap-and-pay). Each token keeps its own KCC-20 leader and slot limits; the
/// transaction is atomic, so the payer never ends up holding KAS or a partial route.
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
        receivers,
        payments: r.payments.clone(),
        funding: r.funding.clone(),
        change,
        records: r.records.clone(),
        fee: r.fee.clone(),
    })
}

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
