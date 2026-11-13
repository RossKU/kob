//! One matcher tick, a sequence of batches that are each planned, built, signed and validated.
//!
//! A batch is the global batch of [`super::batch`] over every book and pair order of the view. It
//! is lowered, built, signed and validated through the rusty-kaspa v2.1.0 engine. The next batch
//! plans on the book that the transaction leaves (§7). Untouched orders stay as they are. The
//! continuations of the transaction are unaccepted, and only their chain-safe fills are planned.
//! The tick ends when nothing profitable crosses or its wall-clock budget runs out.
//!
//! Stops are triggered and armed inside these batches, next to the plain fills that are their
//! evidence (§4). There is no separate trigger transaction.
//!
//! # Size
//!
//! A batch is sized against the physical limits of a transaction, not against a fixed count. The
//! limits are [`super::planner::PHYSICAL_TX_BYTES`] and the block mass limits of the built
//! transaction. When a built batch overruns a limit, the byte budget of the next attempt is scaled
//! down by the overrun and the batch is planned again.
//!
//! # Validation
//!
//! Every transaction the matcher submits has passed [`kob_protocol::verify::validate_signed`]. It
//! runs the scripts with enforced budgets and checks the covenant context, the storage-mass
//! commitment and the fee floor. The covenants are the reference (`docs/spec/matcher.md` §8).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use kob_protocol::artifacts::TemplateId;
use kob_protocol::family::Family;
use kob_protocol::tx::{
    finalize, FinalizeOptions, KeyUtxo, SignedTx, BLOCK_COMPUTE_LIMIT, BLOCK_STORAGE_LIMIT, BLOCK_TRANSIENT_LIMIT,
};
use kob_protocol::verify::{validate_signed, Validation};
use serde::{Deserialize, Serialize};

use super::batch::{plan_batch, plan_batch_counted, work_units, BatchInput, PlanWork, WorkUnit};
use super::book::{book_key, books, outpoint, BookKey, Clock, CovId, ListedOrder, Outpoint};
use super::candidate::Class;
use super::family::{token_limits, Families, Lowered};
use super::lower::{accounting, Accounting, LowerCtx};
use super::planner::{Plan, PlannerConfig};
use super::select::TickSelect;
use super::wallet::Signer;

/// Engine settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineConfig {
    /// Planner settings.
    pub planner: PlannerConfig,
    /// KAS on the operator's token outputs.
    ///
    /// The reference planner leaves the operator no tokens.
    pub token_carrier: u64,
    /// Validate every signed transaction in the script engine before it is returned.
    pub validate: bool,
    /// Plan chained steps onto unaccepted parents (§7).
    ///
    /// When off, a partially filled order continues next tick. The orders a transaction did not
    /// touch still go into the tick's next transactions.
    pub chain_unconfirmed: bool,
    /// Wall-clock budget of one whole tick in milliseconds, the defence against a hostile book.
    ///
    /// What is left when it runs out waits for the next tick. 0 means unlimited, for tests.
    pub tick_budget_ms: u64,
    /// Wall-clock budget of one batch's planning in milliseconds.
    ///
    /// The planner stops refining at it and hands over what it has. The name says "book" because
    /// configuration files use it.
    pub book_budget_ms: u64,
    /// How a batch picks the operator's funding UTXOs.
    pub funding: FundingConfig,
    /// The tick's fee rates (`crate::fee`), which the runner sets from the node's estimate.
    ///
    /// A batch that fills an immediate order (IOC, FOK, market, streaming) or a triggered stop is
    /// built at the high rate when it stays profitable there, lowered to the highest rate that keeps
    /// it profitable but not below `planner.fee_rate`, the rate it was planned at. Otherwise it is
    /// built at `planner.fee_rate`. Every batch is held to the total cap, which may lower the rate
    /// below `planner.fee_rate`, down to the policy floor (at least `MIN_FEE_RATE`). The default,
    /// the floor everywhere and no cap, builds every batch at `planner.fee_rate`.
    pub fees: crate::fee::FeeRates,
    /// The weighted selection of this tick (`docs/spec/matcher.md` §3.6, [`super::select`]).
    ///
    /// The runner sets it every step. None, the default, or a share of one gives the reference
    /// planner, with every unit of the view in one transaction sequence. With a share below one the
    /// tick plans the mandatory units and a weighted sample of the others, each unit in
    /// transactions of its own. A lost race then costs that unit alone.
    pub select: Option<TickSelect>,
    /// The DAA score from which each order has been crossable, by the runner's own observation.
    ///
    /// Crossable means crossable in a work unit's standalone allocation, and the observation is
    /// [`super::select::Selector::history`]. Every executor plans a unit that holds an order
    /// crossable for `max_defer_daa`. An order the history does not hold became crossable now.
    /// None means no history, for one tick on its own. The unit's crossings then count from the
    /// listing of their orders ([`WorkUnit::since`]).
    #[serde(skip)]
    pub select_since: Option<BTreeMap<CovId, u64>>,
}

/// How a batch picks the operator's P2PK funding (`docs/ops/executor.md`, funding).
///
/// The choice is deterministic. The pool is ordered by amount, largest first, then by outpoint.
/// The change outputs of the tick's earlier transactions come after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FundingConfig {
    /// Most funding inputs of one batch.
    ///
    /// A batch takes the UTXOs of the pool in order until the builder's need is covered. A batch
    /// that the first `max_inputs` cannot pay is left for later.
    pub max_inputs: usize,
    /// The pool counts as fragmented above this many accepted UTXOs.
    ///
    /// Every batch then also spends up to `consolidate` of the smallest, merged into its one change
    /// output. It does so only while the batch stays profitable with them.
    pub target_utxos: usize,
    /// Most extra UTXOs one batch consolidates, 0 for off.
    pub consolidate: usize,
}

impl Default for FundingConfig {
    fn default() -> Self {
        FundingConfig { max_inputs: 8, target_utxos: 4, consolidate: 2 }
    }
}

/// Serialized bytes of one P2PK funding input with its signature script.
///
/// That is the outpoint, the sequence, the budget and a 66-byte push.
const FUNDING_INPUT_BYTES: u64 = 120;

/// How far a built transaction is over its limits, as the largest ratio.
///
/// The limits are the block masses and the operator's byte cap `knob`, where 0 means none. Above 1
/// the transaction does not fit.
fn overrun(l: &Lowered, a: &Accounting, knob: u64) -> f64 {
    let m = l.built.fee.mass;
    [
        m.compute as f64 / BLOCK_COMPUTE_LIMIT as f64,
        m.transient as f64 / BLOCK_TRANSIENT_LIMIT as f64,
        m.storage as f64 / BLOCK_STORAGE_LIMIT as f64,
        if knob > 0 { a.bytes as f64 / knob as f64 } else { 0.0 },
    ]
    .into_iter()
    .fold(0.0, f64::max)
}

/// The funding of a batch from `pool`, taken in pool order.
///
/// It takes UTXOs until their sum reaches `need`, at least one and at most `max_inputs`. Then, when
/// `consolidate` is set and the pool holds more than `target_utxos` accepted UTXOs, it adds up to
/// `cfg.consolidate` of the smallest accepted ones not taken yet. Pure and deterministic.
pub fn select_funding(pool: &[KeyUtxo], need: u64, consolidate: bool, cfg: &FundingConfig) -> Vec<KeyUtxo> {
    let max = cfg.max_inputs.max(1);
    let mut out: Vec<KeyUtxo> = vec![];
    let mut sum = 0u64;
    for k in pool {
        if out.len() >= max || (!out.is_empty() && sum >= need) {
            break;
        }
        sum = sum.saturating_add(k.utxo.amount);
        out.push(k.clone());
    }
    let accepted: Vec<&KeyUtxo> = pool.iter().filter(|k| k.utxo.block_daa_score != super::chain::UNACCEPTED_DAA).collect();
    if consolidate && cfg.consolidate > 0 && accepted.len() > cfg.target_utxos {
        let mut small = accepted;
        small.sort_by(|a, b| a.utxo.amount.cmp(&b.utxo.amount).then(outpoint(&a.utxo).cmp(&outpoint(&b.utxo))));
        let mut added = 0;
        for k in small {
            if added >= cfg.consolidate || out.len() >= max {
                break;
            }
            if !out.iter().any(|o| outpoint(&o.utxo) == outpoint(&k.utxo)) {
                out.push(k.clone());
                added += 1;
            }
        }
    }
    out
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            planner: PlannerConfig::default(),
            token_carrier: 1_000_000_000,
            validate: true,
            chain_unconfirmed: true,
            tick_budget_ms: 20_000,
            book_budget_ms: 5_000,
            funding: FundingConfig::default(),
            fees: crate::fee::FeeRates::default(),
            select: None,
            select_since: None,
        }
    }
}

/// Snapshot the tick plans against.
pub struct TickInput {
    /// The listed orders of the view.
    pub orders: Vec<ListedOrder>,
    /// The clock the tick plans at.
    pub clock: Clock,
    /// The operator's spendable P2PK UTXOs. None of them is reserved by a pending transaction.
    pub funding: Vec<KeyUtxo>,
    /// Orders spent by pending transactions or backed off.
    pub excluded: BTreeSet<CovId>,
}

/// A signed, validated transaction ready for submission.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The book of the plan's first fill.
    ///
    /// [`Plan::books`] lists every book the transaction trades.
    pub book: BookKey,
    /// Position of the transaction in the tick, 0 for the first.
    pub step: usize,
    /// The latest earlier transaction of the tick whose outputs this one spends.
    ///
    /// Those outputs are funding change or order continuations.
    pub parent: Option<[u8; 32]>,
    /// The plan the transaction carries out.
    pub plan: Plan,
    /// The plan as built by the builders of its family.
    pub lowered: Lowered,
    /// The signed transaction.
    pub signed: SignedTx,
    /// The exact KAS accounting of the transaction.
    pub accounting: Accounting,
    /// The engine's validation result, when validation is on.
    pub validation: Option<Validation>,
}

impl Prepared {
    /// Transaction id.
    pub fn txid(&self) -> [u8; 32] {
        self.signed.tx.id
    }
}

/// An order whose tokens the token program will not let move, by the engine's pre-simulation.
///
/// The cause may be a frozen or blacklisted balance, or a program that changed its rules. The order
/// is flagged "possibly frozen" and left out of the books (`indexer::flags`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspect {
    /// Covenant id of the order.
    pub id: CovId,
    /// The book of the batch that failed.
    pub book: BookKey,
    /// Why the order is suspected.
    pub reason: String,
}

/// Result of a tick.
#[derive(Default, Debug)]
pub struct TickReport {
    /// Orders whose pre-simulation a token program rejected.
    ///
    /// The rejection is attributed to the order when its custody input failed, or when dropping it
    /// made the same plan pass.
    pub suspects: Vec<Suspect>,
    /// Orders in the transactions prepared this tick.
    ///
    /// Their pre-simulation passed, so any flag they carry is stale.
    pub cleared: Vec<CovId>,
    /// The transactions prepared this tick, in order.
    pub prepared: Vec<Prepared>,
    /// Books skipped, and why.
    pub skipped: Vec<(BookKey, String)>,
    /// Orders left out of this and later ticks.
    ///
    /// An order is quarantined when:
    /// - its numbers fail the sanity gate ([`crate::sanity`]);
    /// - planning it panicked, the last line of defence ([`PLANNING_PANICKED`]);
    /// - the builder refused the order on its own ([`REFUSED_BY_THE_BUILDER`]).
    ///
    /// The runner keeps the ids of the last two out of the book for [`super::run::QUARANTINE_DAA`].
    pub quarantined: Vec<(CovId, String)>,
    /// Plans the builders or the engine refused.
    ///
    /// The cause is a planner defect, or a chained step that must wait for its parent. They are
    /// logged, and the batch is retried without the offending fill.
    pub anomalies: Vec<(BookKey, usize, String)>,
    /// Builds of this tick that had to be retried with measured compute budgets.
    ///
    /// The budget table of `kob-protocol` was short for that shape
    /// ([`super::lower::measured_floors`]). The count is per tick, unlike the process-wide
    /// [`super::lower::budget_slack_retries`] counter.
    pub slack_retries: usize,
    /// What the tick's batch plans cost ([`PlanWork`]), summed over the tick.
    ///
    /// It counts plans, allocation passes, candidate pairs tried and plans the book budget cut
    /// short. It is machine-independent, and the cost bounds of `tests/tick_cost_bound.rs` are
    /// stated in it.
    pub work: PlanWork,
    /// What the weighted selection did ([`EngineConfig::select`]). None when the tick did not
    /// sample.
    pub selection: Option<SelectionReport>,
}

/// The weighted selection of a tick (`docs/spec/matcher.md` §3.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionReport {
    /// Work units of the view, the components with a standalone allocation.
    pub units: usize,
    /// Units every executor plans.
    ///
    /// They hold class 1 or class 2 work, or a crossing visible for `max_defer_daa`.
    pub mandatory: usize,
    /// Mandatory units that are mandatory only because of their age, the starvation bound.
    pub overdue: usize,
    /// Units the sample chose from.
    pub optional: usize,
    /// Units the sample took.
    pub taken: usize,
    /// The tokens of each planned unit, in planning order.
    ///
    /// The mandatory units come first, then the sample in draw order.
    pub planned: Vec<BTreeSet<[u8; 32]>>,
    /// Orders that the units left for later fill on their own.
    ///
    /// The runner watches whether another transaction takes them.
    pub deferred: Vec<CovId>,
    /// Every order the units fill on their own, which makes it crossable now.
    ///
    /// The runner keeps it as its history ([`EngineConfig::select_since`]).
    pub crossable: Vec<CovId>,
}

/// The pre-submission validation of a signed transaction.
///
/// The default is the script engine ([`validate_signed`]). Tests inject their own.
pub type Validator<'a> = &'a dyn Fn(&SignedTx) -> Result<Validation, String>;

fn engine_validator(signed: &SignedTx) -> Result<Validation, String> {
    validate_signed(signed).map_err(|e| e.to_string())
}

pub(super) fn sign_and_validate(
    lowered: &Lowered,
    signer: &dyn Signer,
    validate: bool,
    validator: Validator,
) -> Result<(SignedTx, Option<Validation>), String> {
    let sigs = signer.sign(&lowered.built)?;
    let signed = finalize(&lowered.built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    let v = if validate {
        Some(validator(&signed).map_err(|e| format!("engine rejected: {e} (input roles: {:?})", lowered.built.roles))?)
    } else {
        None
    };
    Ok((signed, v))
}

/// The input an engine rejection names (`input 3: ...`), when that input runs a token program.
///
/// A token input's budget role is `<TokenProgram>.<entry>...`, and an order's is
/// `KobAsk.settle.x@<TokenProgram>`.
pub(super) fn token_input_failure(err: &str, lowered: &Lowered) -> Option<usize> {
    let at = err.find("input ")? + "input ".len();
    let digits: String = err[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    if !err[at + digits.len()..].starts_with(':') {
        return None;
    }
    let i: usize = digits.parse().ok()?;
    let role = lowered.built.roles.get(i)?;
    let program = role.split('.').next()?;
    TemplateId::from_name(program).filter(|t| t.is_token()).map(|_| i)
}

/// The order of `plan` whose custody UTXO is the outpoint that token input `i` spends.
pub(super) fn custody_owner(plan: &Plan, lowered: &Lowered, by_id: &BTreeMap<CovId, ListedOrder>, i: usize) -> Option<CovId> {
    let inp = lowered.built.tx.inputs.get(i)?;
    let spent = (inp.transaction_id, inp.index);
    plan.fills.iter().map(|f| f.cand.id).find(|id| {
        by_id.get(id).is_some_and(|o| o.custody.iter().chain(o.custody_b.iter()).any(|c| super::book::outpoint(&c.utxo) == spent))
    })
}

/// The order a validation error names.
///
/// That is the fill whose order input the engine names, since order inputs lead the transaction in
/// fill order. Or it is the updated order whose `update` input the engine names. So a batch of many
/// books loses only the leg or the update at fault. [`bisect`] searches for the order behind an
/// error that names neither.
fn named_victim(plan: &Plan, lowered: &Lowered, err: &str) -> Option<CovId> {
    let named = err.find("input ").and_then(|at| {
        let at = at + "input ".len();
        let digits: String = err[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
        err[at + digits.len()..].starts_with(':').then(|| digits.parse::<usize>().ok()).flatten()
    });
    if let Some(f) = named.and_then(|i| plan.fills.get(i)) {
        return Some(f.cand.id);
    }
    let spent: Option<Outpoint> = named.and_then(|i| lowered.built.tx.inputs.get(i)).map(|x| (x.transaction_id, x.index));
    if let Some(u) = spent.and_then(|op| plan.updates.iter().find(|u| u.outpoint == op)) {
        return Some(u.id);
    }
    None
}

/// What an attempt does after a refusal that names no order.
///
/// `probe` holds the fills left out only to find the one the refusal comes from.
/// - A plan of one fill names it as `Some(Some(id))`. That order sits out the batch, and the fills
///   left out only to find it are planned again.
/// - A larger plan leaves out the lower-priority half of its fills for the next attempt, as
///   `Some(None)`. A plan that then passes is the batch, and the fills left out are planned in the
///   next batches of the tick.
/// - `None` means nothing is left.
///
/// So an order the refusal does not come from sits out only the attempts that look for it, never
/// the tick. The arming updates of the plan are suspects too, after the fills, because a refusal
/// that an update causes names no fill.
fn bisect(plan: &Plan, probe: &mut BTreeSet<CovId>) -> Option<Option<CovId>> {
    let ids: Vec<CovId> = plan.fills.iter().map(|f| f.cand.id).chain(plan.updates.iter().map(|u| u.id)).collect();
    match ids.len() {
        0 => None,
        1 => {
            probe.clear();
            Some(Some(ids[0]))
        }
        k => {
            probe.extend(ids[k - k / 2..].iter().copied());
            Some(None)
        }
    }
}

/// The fill a failed attempt gives up, the lowest-priority one that is not class 1.
fn victim(plan: &Plan) -> Option<CovId> {
    plan.fills.iter().rev().find(|f| f.cand.class != Class::Immediate).or(plan.fills.last()).map(|f| f.cand.id)
}

/// Reason of an order quarantined because its planning panicked ([`TickReport::quarantined`]).
pub const PLANNING_PANICKED: &str = "planning panicked";
/// Reason prefix of an order quarantined because the builder refused it on its own.
///
/// The builder's error reads `order of leg <i>: ...`, see [`lowering_leg`]. Such an order fails the
/// same way every time it is planned.
pub const REFUSED_BY_THE_BUILDER: &str = "refused by the builder";

/// The leg that a lowering error names as the order at fault.
///
/// The batch builder (`kob_protocol::build`) names the leg of a refusal that depends on that order
/// alone, such as its token programs or its family, as `order of leg <i>: ...`. Leg `i` is
/// `plan.fills[i]` ([`super::lower::lower_batch`]).
pub fn lowering_leg(err: &str) -> Option<usize> {
    leg_named(err, "order of leg ")
}

/// The leg that a lowering error names as the fill at fault.
///
/// The batch builder names the leg of a refusal of the fill the plan chose for a pair order as
/// `fill of leg <i>: ...` or `evidence of leg <i>: ...`. The refusal is about the fill's quantity,
/// continuation or evidence. That order is left out of the batch but not quarantined, because
/// another fill of it may build. The other legs are planned again without it.
pub fn lowering_fill_leg(err: &str) -> Option<usize> {
    leg_named(err, "fill of leg ").or_else(|| leg_named(err, "evidence of leg "))
}

/// The leg whose merged entry a lowering error names as at fault.
///
/// The error reads `entry of leg <i>: ...` and is about the custodies of the entry that the booked
/// exit of leg `i` re-arms. That entry is left out of the batch, so the exit is not merged into it.
pub fn lowering_entry_leg(err: &str) -> Option<usize> {
    leg_named(err, "entry of leg ")
}

/// The update `plan.updates[i]` whose continuation the builder refused.
///
/// The error reads `arming update <i>: ...` and is about a continuation below the covenant-output
/// bound. That order is left out of the batch.
pub fn lowering_update(err: &str) -> Option<usize> {
    leg_named(err, "arming update ")
}

fn leg_named(err: &str, prefix: &str) -> Option<usize> {
    let at = err.find(prefix)? + prefix.len();
    let digits: String = err[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !err[at + digits.len()..].starts_with(':') {
        return None;
    }
    digits.parse().ok()
}

/// Most attempts (plan, build, validate) per batch before the tick gives up on it.
const MAX_ATTEMPTS: usize = 24;
/// Most single-order removals tried to isolate a panicking batch. Each is a full attempt.
const MAX_ISOLATION_TRIES: usize = 48;

/// The tick's view and what it has decided so far.
struct Tick<'a> {
    inp: &'a TickInput,
    cfg: &'a EngineConfig,
    families: &'a Families,
    signer: &'a dyn Signer,
    validator: Validator<'a>,
    fams: BTreeSet<Family>,
    operator: [u8; 32],
    lock: u64,
    tick_deadline: Option<Instant>,
    /// The book as the transactions of the tick so far leave it.
    by_id: BTreeMap<CovId, ListedOrder>,
    /// Continuations of this tick's transactions, which are unaccepted.
    unaccepted: BTreeSet<CovId>,
    /// How many transactions of the tick each order was in.
    depth: BTreeMap<CovId, usize>,
    pool: Vec<KeyUtxo>,
    /// Orders never planned this tick: the input's exclusions, quarantined orders and skipped
    /// books.
    excluded: BTreeSet<CovId>,
    /// What the surplus-inventory policy has used in its period, including this tick's batches.
    ///
    /// Each batch is planned against what the earlier ones left of `maxFee` and
    /// `tokens[].maxAmount`, not against the tick's start.
    inventory_used: super::planner::InventoryUse,
    /// The orders of the work unit being planned, for the weighted selection (§3.6).
    ///
    /// Every other order sits out its transactions. None means the whole view.
    scope: Option<BTreeSet<CovId>>,
}

/// What one batch attempt loop produced.
enum Batch {
    Prepared(Box<Prepared>),
    /// Nothing, or nothing more, to plan.
    Nothing,
    /// Every attempt failed.
    ///
    /// It carries the last error and the orders the attempts gave up. They sit out the rest of the
    /// tick.
    Failed(BookKey, String, Vec<CovId>),
}

impl<'a> Tick<'a> {
    fn out_of_time(&self) -> bool {
        self.tick_deadline.is_some_and(|d| Instant::now() >= d)
    }

    fn plan_deadline(&self) -> Option<Instant> {
        let b = (self.cfg.book_budget_ms > 0).then(|| Instant::now() + Duration::from_millis(self.cfg.book_budget_ms));
        match (b, self.tick_deadline) {
            (Some(b), Some(t)) => Some(b.min(t)),
            (b, t) => b.or(t),
        }
    }

    /// The latest transaction of this tick whose outputs `signed` spends.
    fn parent_of(signed: &SignedTx, report: &TickReport) -> Option<[u8; 32]> {
        let ids: BTreeSet<[u8; 32]> = signed.tx.inputs.iter().map(|i| i.transaction_id).collect();
        report.prepared.iter().rev().map(|p| p.txid()).find(|t| ids.contains(t))
    }

    /// The built batch with its auction legs at the earliest ticks at which it still pays.
    ///
    /// See `docs/spec/matcher.md` §3.3 and [`super::auction`]. `built` is the batch as planned at
    /// the lock time. `build` lowers a plan at the batch's rate and funding, and returns None when
    /// the plan is refused, over the fee cap or over a physical limit. Every candidate is built. It
    /// is kept only when its exact profit, with the kept inventory's value, is at least
    /// `min_profit` and at least one sompi. Otherwise `built` goes as it is.
    ///
    /// 1. Pair auctions move token amounts. The books of the batch are planned again with the
    ///    quotes of the pair auctions at the same progress from their earliest ticks. A bisection
    ///    finds the smallest progress at which the planner still fills each of them, and every
    ///    immediate or triggered order of the batch, in full, and the batch pays. The planner's fee
    ///    estimate is corrected by what the built batch paid above it.
    /// 2. KAS-book auctions keep the allocation. [`super::auction::earliest_ticks`] prices them
    ///    against the batch's exact surplus. A build short of `min_profit` by a few sompi, for
    ///    example because a tick's push is one byte longer, is retried with less.
    fn earliest_ticks(
        &self,
        built: (Plan, Lowered, Accounting),
        planned_ex: &BTreeSet<CovId>,
        max_bytes: u64,
        planner: &PlannerConfig,
        build: &dyn Fn(&Plan) -> Option<(Lowered, Accounting)>,
    ) -> (Plan, Lowered, Accounting) {
        // An earlier tick never takes the batch to nothing. It keeps at least one sompi above the
        // fee.
        let min = self.cfg.planner.min_profit.max(1);
        let pays = |p: &Plan, a: &Accounting| a.profit.saturating_add(p.kept_value()) >= min;
        let lock = self.lock as i64;
        let mut best = built;
        // 1. Pair auctions, planned again.
        let pairs: Vec<(CovId, i64)> = best
            .0
            .fills
            .iter()
            .filter_map(|f| Some((f.cand.id, super::auction::pair_auction_from(f, self.by_id.get(&f.cand.id)?, lock)?)))
            .collect();
        if !pairs.is_empty() {
            // What the planner's fee estimate fell short of the built fee, at the rate the batch
            // goes at.
            let short = (best.2.fee.min(i64::MAX as u64) as i64).saturating_sub(best.0.est_fee).max(0);
            let want = min.saturating_add(short);
            // The orders that must keep their fills: the pair auctions and every immediate or
            // triggered order of the batch.
            let keep: Vec<(CovId, i64)> = best
                .0
                .fills
                .iter()
                .filter(|f| f.cand.class != Class::Resting || pairs.iter().any(|p| p.0 == f.cand.id))
                .map(|f| (f.cand.id, best.0.amount_of(&f.cand.id)))
                .collect();
            let books = best.0.books();
            let span = pairs.iter().map(|p| lock - p.1).max().unwrap_or(0);
            let mut found: Option<Plan> = None;
            super::auction::first_paying(span, |e| {
                if self.out_of_time() {
                    return false;
                }
                let ticks: BTreeMap<CovId, i64> = pairs.iter().map(|p| (p.0, super::auction::tick_at(p.1, e, lock))).collect();
                let bi = BatchInput {
                    by_id: &self.by_id,
                    lock_time: self.lock,
                    utc: self.inp.clock.utc,
                    excluded: planned_ex,
                    unaccepted: &self.unaccepted,
                    families: &self.fams,
                    max_bytes,
                    only: Some(&books),
                    deadline: self.plan_deadline(),
                    pair_ticks: Some(&ticks),
                };
                match plan_batch(&bi, planner) {
                    Some(p) if p.profit() >= want && keep.iter().all(|k| p.amount_of(&k.0) >= k.1) => {
                        found = Some(p);
                        true
                    }
                    _ => false,
                }
            });
            if let Some(p) = found {
                if let Some((l, a)) = build(&p).filter(|(_, a)| pays(&p, a)) {
                    best = (p, l, a);
                }
            }
        }
        // 2. KAS-book auctions, priced against the exact surplus.
        let mut budget = best.2.profit.saturating_add(best.0.kept_value()).saturating_sub(min);
        for _ in 0..3 {
            let Some(p) = super::auction::earliest_ticks(&best.0, &self.by_id, self.lock, budget) else { break };
            let Some((l, a)) = build(&p) else { break };
            if pays(&p, &a) {
                return (p, l, a);
            }
            let short = min.saturating_sub(a.profit.saturating_add(p.kept_value()));
            budget = budget.saturating_sub(short.max(1));
        }
        best
    }

    /// Plans, builds, signs and validates the next batch of the tick.
    ///
    /// The `extra` orders are left out, for isolation. `last_plan` receives the order ids of the
    /// latest plan attempted, to isolate a panic after planning.
    fn batch(&self, step: usize, extra: &BTreeSet<CovId>, report: &mut TickReport, last_plan: &mut Vec<CovId>) -> Batch {
        let cfg = self.cfg;
        let mut ex: BTreeSet<CovId> = self.excluded.union(extra).copied().collect();
        // Orders quarantined earlier in the tick, which the builder refused, are never planned
        // again.
        ex.extend(report.quarantined.iter().map(|q| q.0));
        // The unit being planned by the weighted selection. The other orders sit out its
        // transactions.
        let scope_out: BTreeSet<CovId> = match &self.scope {
            Some(scope) => self.by_id.keys().filter(|id| !scope.contains(*id)).copied().collect(),
            None => BTreeSet::new(),
        };
        ex.extend(scope_out.iter().copied());
        // Funding (`select_funding`). The first attempt takes the largest UTXO, and a builder's
        // "insufficient funds" names the need. A fragmented pool is consolidated while the batch
        // stays profitable with it.
        let mut need = 0u64;
        let mut consolidate = true;
        let mut funding = select_funding(&self.pool, need, consolidate, &cfg.funding);
        let mut max_bytes = cfg.planner.tx_byte_budget();
        // Measured budgets for roles the table gives too little, the safety net below.
        let mut floor: BTreeMap<String, u16> = BTreeMap::new();
        let mut measured = false;
        let mut last: Option<(BookKey, String)> = None;
        // The high rate of an urgent batch. It is off once the operator's whole pool cannot pay it,
        // and the batch then goes at the rate it was planned at.
        let mut high_ok = true;
        // Fills left out of this batch only to find the one that a refusal naming no order comes
        // from (`bisect`). They are never given up.
        let mut probe: BTreeSet<CovId> = BTreeSet::new();
        for _attempt in 0..MAX_ATTEMPTS {
            if self.out_of_time() {
                break;
            }
            let planned_ex: BTreeSet<CovId> = if probe.is_empty() { ex.clone() } else { ex.union(&probe).copied().collect() };
            let bi = BatchInput {
                by_id: &self.by_id,
                lock_time: self.lock,
                utc: self.inp.clock.utc,
                excluded: &planned_ex,
                unaccepted: &self.unaccepted,
                families: &self.fams,
                max_bytes,
                only: None,
                deadline: self.plan_deadline(),
                pair_ticks: None,
            };
            let own = (self.inventory_used != cfg.planner.inventory.used).then(|| {
                let mut planner = cfg.planner.clone();
                planner.inventory.used = self.inventory_used.clone();
                planner
            });
            let planner = own.as_ref().unwrap_or(&cfg.planner);
            let (plan, work) = plan_batch_counted(&bi, planner);
            report.work.add(work);
            let Some(plan) = plan else { break };
            *last_plan = plan.fills.iter().map(|f| f.cand.id).collect();
            let key = plan.book;
            let Some(adapter) = self.families.get(key.family) else {
                last = Some((key, format!("no {} builders in this build", key.family.as_str())));
                break;
            };
            // The batch's fee rate (`crate::fee`). It is the high rate when the batch fills an
            // immediate order or a triggered stop, whose deadlines are a minute away. Otherwise it
            // is the rate the batch was planned at. The rate is lowered to the highest rate that
            // keeps the batch profitable, never below the planned rate, at which the planner found
            // the batch profitable. The total cap may lower it further, down to the policy floor.
            let plan_rate = cfg.planner.fee_rate;
            let mut rate = if high_ok && plan.fills.iter().any(|f| f.cand.class != Class::Resting) {
                cfg.fees.rate(crate::fee::Urgency::High, plan_rate)
            } else {
                plan_rate
            };
            let cap_floor = cfg.fees.floor_of(kob_protocol::tx::MIN_FEE_RATE);
            let lowered = loop {
                let cx = LowerCtx {
                    budget_floor: floor.clone(),
                    by_id: &self.by_id,
                    funding: funding.clone(),
                    operator: self.operator,
                    fee_rate: rate,
                    token_carrier: cfg.token_carrier,
                    keep_carrier: cfg.planner.inventory.keep_carrier,
                };
                match adapter.lower(&plan, &cx) {
                    Ok(l) => {
                        if let Some(r) = cfg.fees.capped(rate, l.built.fee.fee, cap_floor) {
                            rate = r;
                            continue;
                        }
                        if rate > plan_rate {
                            let a = accounting(&l, &funding);
                            // The inventory a plan keeps is income that the KAS accounting does not
                            // see, valued by its policy.
                            let p = a.profit.saturating_add(plan.kept_value());
                            if p < cfg.planner.min_profit {
                                rate = crate::fee::FeeRates::affordable(rate, a.fee, p, cfg.planner.min_profit, plan_rate)
                                    .unwrap_or(plan_rate);
                                continue;
                            }
                        }
                        break Ok(l);
                    }
                    Err(e) => break Err(e),
                }
            };
            let lowered = match lowered {
                Ok(l) => l,
                Err(e) => {
                    // Short of funds. Add funding inputs, with a margin for their fee, while the
                    // pool has more to give. The builder counts every input, so the funding must
                    // grow by the shortfall.
                    if let Some((n, h)) = super::lower::insufficient_funds(&e) {
                        let per_input = super::planner::est_fee(FUNDING_INPUT_BYTES, rate).max(0) as u64;
                        let total = |v: &[KeyUtxo]| v.iter().map(|k| k.utxo.amount).sum::<u64>();
                        let want = total(&funding)
                            .saturating_add(n.saturating_sub(h))
                            .saturating_add(per_input.saturating_mul(cfg.funding.max_inputs as u64));
                        let more = select_funding(&self.pool, want, consolidate, &cfg.funding);
                        if want > need && total(&more) > total(&funding) {
                            need = want;
                            funding = more;
                            continue;
                        }
                        if rate > plan_rate {
                            // The pool cannot pay the high rate, so use the planned rate.
                            high_ok = false;
                            continue;
                        }
                    }
                    let why = format!("lowering: {e}");
                    report.anomalies.push((key, step, why.clone()));
                    last = Some((key, why));
                    // A refusal that names the order at fault, for its token programs or its family
                    // and nothing the plan chose, fails the same way every time that order is
                    // planned. So the order is quarantined like one whose planning panics, for this
                    // tick and, by the runner, the next ones. The batch is planned again without
                    // it. A continuation of this tick's own transactions is only left out of this
                    // batch.
                    if let Some(id) = lowering_leg(&e).and_then(|i| plan.fills.get(i)).map(|f| f.cand.id) {
                        if !self.unaccepted.contains(&id) && !report.quarantined.iter().any(|q| q.0 == id) {
                            report.quarantined.push((id, format!("{REFUSED_BY_THE_BUILDER}: {e}")));
                        }
                        ex.insert(id);
                        // The order at fault is out. What is left is planned as if it had never
                        // been listed.
                        last = None;
                        continue;
                    }
                    // A refusal of the fill the plan chose for one order, for its quantity, its
                    // continuation or its evidence. That order sits out this batch, and the other
                    // legs of any book are planned again without it.
                    if let Some(id) = lowering_fill_leg(&e).and_then(|i| plan.fills.get(i)).map(|f| f.cand.id) {
                        ex.insert(id);
                        probe.clear();
                        last = None;
                        continue;
                    }
                    // A refusal of the continuation of an arming update. That order sits out this
                    // batch.
                    if let Some(id) = lowering_update(&e).and_then(|i| plan.updates.get(i)).map(|u| u.id) {
                        ex.insert(id);
                        probe.clear();
                        last = None;
                        continue;
                    }
                    // A refusal of the entry that a booked exit re-arms. The entry sits out this
                    // batch, and the exit is not merged.
                    if let Some(id) = lowering_entry_leg(&e).and_then(|i| plan.fills.get(i)).and_then(|f| f.cand.merge) {
                        ex.insert(id);
                        probe.clear();
                        last = None;
                        continue;
                    }
                    match bisect(&plan, &mut probe) {
                        Some(Some(v)) => {
                            ex.insert(v);
                            continue;
                        }
                        Some(None) => continue,
                        None => break,
                    }
                }
            };
            let acct = accounting(&lowered, &funding);
            // The physical limits of the built transaction, since the plan's byte count is only an
            // estimate. Scale the budget down by the overrun and plan again.
            let m = lowered.built.fee.mass;
            let over = overrun(&lowered, &acct, cfg.planner.max_tx_bytes);
            if over > 1.0 {
                let shrunk = ((plan.est_bytes as f64 / over) * 0.97) as u64;
                if plan.fills.len() > 1 && shrunk < plan.est_bytes && shrunk > 0 {
                    max_bytes = shrunk.min(max_bytes);
                    continue;
                }
                last = Some((key, format!("too large ({} bytes, masses {:?})", acct.bytes, m)));
                match victim(&plan) {
                    Some(v) if plan.fills.len() > 1 => {
                        ex.insert(v);
                        continue;
                    }
                    _ => break,
                }
            }
            // The batch's profit is its exact KAS (`change − funding`) plus the policy value of the
            // inventory it keeps.
            if acct.profit.saturating_add(plan.kept_value()) < cfg.planner.min_profit {
                // The fee of the consolidated inputs must not cost the batch. Plan it again without
                // them.
                let plain = select_funding(&self.pool, need, false, &cfg.funding);
                if consolidate && plain.len() < funding.len() {
                    consolidate = false;
                    funding = plain;
                    continue;
                }
                last = Some((key, format!("unprofitable (profit {}, {} bytes)", acct.profit, acct.bytes)));
                match victim(&plan) {
                    Some(v) if plan.fills.len() > 1 => {
                        ex.insert(v);
                        continue;
                    }
                    _ => break,
                }
            }
            // §3.3. The auction legs go at the earliest ticks at which the batch still pays its
            // fee, at the rate it is built at, and `min_profit`, not at the lock time. So a market
            // order fills at about its start price, whatever the matcher's latency. The batch is
            // built again that way. It is kept only when it is exactly as profitable as required
            // and within every limit. Otherwise it goes as planned.
            let build = |p: &Plan| -> Option<(Lowered, Accounting)> {
                let cx = LowerCtx {
                    budget_floor: floor.clone(),
                    by_id: &self.by_id,
                    funding: funding.clone(),
                    operator: self.operator,
                    fee_rate: rate,
                    token_carrier: cfg.token_carrier,
                    keep_carrier: cfg.planner.inventory.keep_carrier,
                };
                let l = adapter.lower(p, &cx).ok()?;
                let a = accounting(&l, &funding);
                (cfg.fees.capped(rate, l.built.fee.fee, cap_floor).is_none() && overrun(&l, &a, cfg.planner.max_tx_bytes) <= 1.0)
                    .then_some((l, a))
            };
            let (plan, lowered, acct) = self.earliest_ticks((plan, lowered, acct), &planned_ex, max_bytes, planner, &build);
            match sign_and_validate(&lowered, self.signer, cfg.validate, self.validator) {
                Ok((signed, validation)) => {
                    report.cleared.extend(plan.fills.iter().map(|f| f.cand.id));
                    let parent = Self::parent_of(&signed, report);
                    return Batch::Prepared(Box::new(Prepared {
                        book: key,
                        step,
                        parent,
                        plan,
                        lowered,
                        signed,
                        accounting: acct,
                        validation,
                    }));
                }
                Err(e) => {
                    // A safety net only, because the budget table is exact over every shape the
                    // planner builds. A budget the table gives too little is measured in the
                    // engine, and the batch is retried once with the budgets its inputs need. The
                    // retry is logged and counted (`lower::budget_slack_retries`, 0 in every test).
                    // So a short table entry never skips the batch tick after tick.
                    if e.contains("ExceededCommittedScriptUnits") && !measured {
                        measured = true;
                        match super::lower::measured_floors(&lowered.built, self.signer) {
                            Ok(f) if !f.is_empty() => {
                                floor = f;
                                report.slack_retries += 1;
                                super::lower::note_budget_slack("matcher batch", &e);
                                continue;
                            }
                            Ok(_) => {}
                            Err(m) => tracing::warn!(error = %m, "could not measure the script units of a batch"),
                        }
                    }
                    // A chained step the engine refuses waits for its parent's acceptance.
                    report.anomalies.push((key, step, e.clone()));
                    last = Some((key, e.clone()));
                    // The token program rejected the plan, not an order covenant or a budget. A
                    // frozen or blacklisted balance looks like this. Attribute it to the accepted
                    // order whose custody the failing input spends. When it names none, the fill
                    // that the search below isolates is the suspect.
                    let mut token_why = None;
                    if let Some(i) = token_input_failure(&e, &lowered) {
                        let why = format!("the token program rejected the pre-simulated spend ({})", lowered.built.roles[i]);
                        if let Some(id) = custody_owner(&plan, &lowered, &self.by_id, i).filter(|id| !self.unaccepted.contains(id)) {
                            report.suspects.push(Suspect { id, book: key, reason: why });
                            ex.insert(id);
                            probe.clear();
                            continue;
                        }
                        token_why = Some(why);
                    }
                    if let Some(v) = named_victim(&plan, &lowered, &e) {
                        ex.insert(v);
                        probe.clear();
                        continue;
                    }
                    match bisect(&plan, &mut probe) {
                        Some(Some(v)) => {
                            if let Some(reason) = token_why.filter(|_| !self.unaccepted.contains(&v)) {
                                report.suspects.push(Suspect { id: v, book: key, reason });
                            }
                            ex.insert(v);
                            continue;
                        }
                        Some(None) => continue,
                        None => break,
                    }
                }
            }
        }
        match last {
            Some((k, why)) => {
                let mut gave_up: Vec<CovId> = ex.difference(&self.excluded).filter(|id| !scope_out.contains(*id)).copied().collect();
                gave_up.extend(last_plan.iter().copied());
                Batch::Failed(k, why, gave_up)
            }
            None => Batch::Nothing,
        }
    }

    /// Moves the tick's book past a prepared transaction.
    ///
    /// Continuations replace their orders, unaccepted and one transaction deeper. Everything else
    /// the transaction spent leaves. The funding pool gives up every UTXO the transaction spends
    /// and takes the change.
    fn apply(&mut self, p: &Prepared) {
        // The inventory budgets that the next batches of the tick are planned against.
        self.inventory_used.record(&p.plan.kept, p.lowered.built.fee.fee);
        let (continued, next_funding) = super::chain::apply(&mut self.by_id, &p.plan, &p.lowered.built, self.operator);
        for f in &p.plan.fills {
            *self.depth.entry(f.cand.id).or_insert(0) += 1;
        }
        for id in continued {
            let deep = self.depth.get(&id).copied().unwrap_or(0) >= self.cfg.planner.max_chain.max(1);
            if !self.cfg.chain_unconfirmed || deep {
                // It continues next tick.
                self.by_id.remove(&id);
            } else {
                self.unaccepted.insert(id);
            }
        }
        let spent: BTreeSet<Outpoint> = p.lowered.built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
        let before = self.pool.len();
        self.pool.retain(|k| !spent.contains(&outpoint(&k.utxo)));
        if self.pool.len() < before {
            // The change funds the next transactions. A P2PK spend never reads its parent's DAA
            // score, so it chains safely.
            if let Some(f) = next_funding {
                self.pool.push(f);
            }
        }
    }

    /// Isolation after a panic, the last line of defence.
    ///
    /// The order whose removal makes the batch pass is quarantined, and that pass is the batch's
    /// result. The orders tried are those of the plan that panicked, for a defect in lowering or
    /// validation. Otherwise they are those of the books whose planning panics on its own. When no
    /// single order explains the panic, those books sit out the rest of the tick.
    fn isolate(&mut self, step: usize, suspects: Vec<CovId>, report: &mut TickReport) -> Option<Batch> {
        tracing::error!("the matcher panicked on a batch; isolating the order");
        let mut tried = 0usize;
        let try_without = |me: &Tick, id: CovId, report: &mut TickReport| -> Option<Batch> {
            let marks =
                (report.suspects.len(), report.cleared.len(), report.anomalies.len(), report.skipped.len(), report.slack_retries);
            let extra: BTreeSet<CovId> = [id].into_iter().collect();
            let mut lp = vec![];
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| me.batch(step, &extra, report, &mut lp)));
            match r {
                Ok(b) => Some(b),
                Err(_) => {
                    report.suspects.truncate(marks.0);
                    report.cleared.truncate(marks.1);
                    report.anomalies.truncate(marks.2);
                    report.skipped.truncate(marks.3);
                    report.slack_retries = marks.4;
                    None
                }
            }
        };
        let mut candidates: Vec<CovId> = suspects;
        let mut books_of: BTreeSet<BookKey> = BTreeSet::new();
        if candidates.is_empty() {
            // Planning itself panicked. Find the books whose planning panics alone.
            let keys: Vec<BookKey> = books(&self.by_id.values().cloned().collect::<Vec<_>>()).into_keys().collect();
            for k in keys {
                if self.out_of_time() {
                    break;
                }
                let only: BTreeSet<BookKey> = [k].into_iter().collect();
                let bi = BatchInput {
                    by_id: &self.by_id,
                    lock_time: self.lock,
                    utc: self.inp.clock.utc,
                    excluded: &self.excluded,
                    unaccepted: &self.unaccepted,
                    families: &self.fams,
                    max_bytes: self.cfg.planner.tx_byte_budget(),
                    only: Some(&only),
                    deadline: self.plan_deadline(),
                    pair_ticks: None,
                };
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plan_batch(&bi, &self.cfg.planner))).is_err() {
                    books_of.insert(k);
                    candidates.extend(self.by_id.values().filter(|o| book_key(o) == Some(k)).map(|o| o.id()));
                }
            }
            // A pair order belongs to no book. Its planning is tried with the books of its tokens.
            candidates.extend(self.by_id.values().filter(|o| o.order.state.is_pair()).map(|o| o.id()));
        } else {
            for id in &candidates {
                if let Some(k) = self.by_id.get(id).and_then(book_key) {
                    books_of.insert(k);
                }
            }
        }
        for id in candidates {
            if tried >= MAX_ISOLATION_TRIES || self.out_of_time() {
                break;
            }
            if self.excluded.contains(&id) {
                continue;
            }
            tried += 1;
            if let Some(b) = try_without(self, id, report) {
                report.quarantined.push((id, PLANNING_PANICKED.into()));
                self.excluded.insert(id);
                return Some(b);
            }
        }
        // No single order explains it. Those books wait for the next tick, and the process lives.
        if books_of.is_empty() {
            books_of = books(&self.by_id.values().cloned().collect::<Vec<_>>()).into_keys().collect();
        }
        for k in books_of {
            report.skipped.push((k, "planning panicked; book skipped".into()));
            let ids: Vec<CovId> = self.by_id.values().filter(|o| book_key(o) == Some(k)).map(|o| o.id()).collect();
            self.excluded.extend(ids);
        }
        None
    }
}

/// Runs one tick over every book of the snapshot.
pub fn tick(inp: &TickInput, cfg: &EngineConfig, families: &Families, signer: &dyn Signer) -> TickReport {
    tick_with(inp, cfg, families, signer, &engine_validator)
}

/// [`tick`] with a custom pre-submission validator. The default is the script engine.
pub fn tick_with(inp: &TickInput, cfg: &EngineConfig, families: &Families, signer: &dyn Signer, validator: Validator) -> TickReport {
    let mut report = TickReport::default();
    let started = Instant::now();
    let tick_deadline = (cfg.tick_budget_ms > 0).then(|| started + Duration::from_millis(cfg.tick_budget_ms));
    // No order with hostile numbers reaches the planner. It is quarantined with the reason.
    let mut by_id: BTreeMap<CovId, ListedOrder> = BTreeMap::new();
    for o in &inp.orders {
        match crate::sanity::check(&o.order.state) {
            Ok(()) => {
                by_id.insert(o.id(), o.clone());
            }
            Err(why) => report.quarantined.push((o.id(), why)),
        }
    }
    let mut pool: Vec<KeyUtxo> = inp.funding.clone();
    // Largest first; deterministic.
    pool.sort_by(|a, b| {
        b.utxo.amount.cmp(&a.utxo.amount).then((a.utxo.transaction_id, a.utxo.index).cmp(&(b.utxo.transaction_id, b.utxo.index)))
    });
    let fams: BTreeSet<Family> = [Family::Kcc20, Family::Kron].into_iter().filter(|f| families.supports(*f)).collect();
    let mut excluded = inp.excluded.clone();
    // Books this build cannot plan are reported once and left out.
    for (key, orders) in books(&by_id.values().cloned().collect::<Vec<_>>()) {
        let why = if token_limits(key.family, &key.template).is_none() {
            "unsupported token program".to_string()
        } else if !families.supports(key.family) {
            format!("no {} builders in this build", key.family.as_str())
        } else {
            continue;
        };
        report.skipped.push((key, why));
        excluded.extend(orders.iter().map(|o| o.id()));
    }
    let mut tk = Tick {
        inp,
        cfg,
        families,
        signer,
        validator,
        fams,
        operator: signer.pubkey(),
        lock: inp.clock.lock_time(cfg.planner.safety_margin),
        tick_deadline,
        by_id,
        unaccepted: BTreeSet::new(),
        depth: BTreeMap::new(),
        pool,
        excluded,
        inventory_used: cfg.planner.inventory.used.clone(),
        scope: None,
    };
    let mut step = 0usize;
    let mut out_of_time = false;
    match cfg.select.filter(|s| s.samples()).and_then(|sel| select_units(&mut tk, &sel, &mut report)) {
        // The reference: every unit of the view in one transaction sequence.
        None => out_of_time = run_batches(&mut tk, &mut report, &mut step),
        // Weighted selection: each unit planned in transactions of its own, mandatory units first,
        // then the sample.
        Some(groups) => {
            let several = groups.len() > 1;
            let all: BTreeSet<CovId> = groups.iter().flatten().copied().collect();
            for g in groups {
                tk.scope = Some(g);
                if run_batches(&mut tk, &mut report, &mut step) {
                    out_of_time = true;
                    break;
                }
            }
            // Then the planned units together, as the reference plans them. This takes what the
            // units' own transactions left: crossings that pay a transaction only together with
            // another unit's, which the reference planner puts into one.
            if several && !out_of_time {
                tk.scope = Some(all);
                out_of_time = run_batches(&mut tk, &mut report, &mut step);
            }
            tk.scope = None;
        }
    }
    if out_of_time {
        if let Some(k) = tk.by_id.values().find_map(book_key) {
            report.skipped.push((k, "tick budget exhausted; the rest is planned next tick".into()));
        }
    }
    report
}

/// The tokens an order belongs to, both of them for a pair order.
fn order_tokens(o: &ListedOrder) -> Vec<[u8; 32]> {
    match o.order.state.pair_tokens() {
        Some(t) => vec![t.a.cov_id, t.b.cov_id],
        None => vec![o.order.state.token_cov_id()],
    }
}

/// The weighted selection of a tick (`docs/spec/matcher.md` §3.6).
///
/// It takes the work units of the view: the mandatory ones and a weighted sample of the others.
/// A unit is mandatory when it holds class 1 or class 2 work, or a crossing visible for
/// `max_defer_daa`. The sample takes `share` of the others in expectation, drawn without
/// replacement with probability proportional to `(profit per byte)^alpha` ([`super::select`]).
/// The orders of the units left out sit out the tick.
///
/// Returns the order sets of the units to plan, in planning order. Returns None, the reference,
/// when the view has no unit or its planning panicked.
fn select_units(tk: &mut Tick, sel: &TickSelect, report: &mut TickReport) -> Option<Vec<BTreeSet<CovId>>> {
    let deadline = tk.plan_deadline();
    let units: Vec<WorkUnit> = {
        let none = BTreeSet::new();
        let bi = BatchInput {
            by_id: &tk.by_id,
            lock_time: tk.lock,
            utc: tk.inp.clock.utc,
            excluded: &tk.excluded,
            unaccepted: &none,
            families: &tk.fams,
            max_bytes: u64::MAX,
            only: None,
            deadline,
            pair_ticks: None,
        };
        let cfg = &tk.cfg.planner;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work_units(&bi, cfg))).ok()?
    };
    if units.is_empty() {
        return None;
    }
    let now = tk.inp.clock.daa;
    // Since when the unit's oldest crossable order has waited. That is the runner's history, else
    // the listing of its crossings.
    let since = |w: &WorkUnit| -> u64 {
        match &tk.cfg.select_since {
            Some(h) => w.orders.iter().map(|id| h.get(id).copied().unwrap_or(now).min(now)).min().unwrap_or(now),
            None => w.since,
        }
    };
    let overdue = |w: &WorkUnit| since(w).saturating_add(sel.max_defer_daa) <= now;
    let (mandatory, optional): (Vec<&WorkUnit>, Vec<&WorkUnit>) = units.iter().partition(|w| w.urgent || overdue(w));
    let mut rng = sel.rng();
    let k = super::select::take_count(sel.share(), optional.len(), &mut rng);
    let rates: Vec<f64> = optional.iter().map(|w| super::select::rate(w.profit, w.bytes)).collect();
    let picks = super::select::sample(&super::select::weights(&rates, sel.alpha()), k, &mut rng);
    let mut planned: Vec<&WorkUnit> = mandatory.clone();
    planned.extend(picks.iter().map(|&i| optional[i]));
    let mut rep = SelectionReport {
        units: units.len(),
        mandatory: mandatory.len(),
        overdue: mandatory.iter().filter(|w| !w.urgent).count(),
        optional: optional.len(),
        taken: picks.len(),
        planned: planned.iter().map(|w| w.tokens.clone()).collect(),
        deferred: vec![],
        crossable: units.iter().flat_map(|w| w.orders.iter().copied()).collect(),
    };
    let members = |w: &WorkUnit| -> BTreeSet<CovId> {
        tk.by_id
            .values()
            .filter(|o| {
                let t = order_tokens(o);
                !t.is_empty() && t.iter().all(|x| w.tokens.contains(x))
            })
            .map(|o| o.id())
            .collect()
    };
    let groups: Vec<BTreeSet<CovId>> = planned.iter().map(|w| members(w)).collect();
    for (i, w) in optional.iter().enumerate() {
        if !picks.contains(&i) {
            rep.deferred.extend(w.orders.iter().copied());
            let out = members(w);
            tk.excluded.extend(out);
        }
    }
    report.selection = Some(rep);
    Some(groups)
}

/// Plans, builds and applies the tick's transactions until nothing more crosses in the scope.
///
/// Returns true when the tick's budget ran out.
fn run_batches(tk: &mut Tick, report: &mut TickReport, step: &mut usize) -> bool {
    loop {
        if tk.out_of_time() {
            return true;
        }
        let marks = (report.suspects.len(), report.cleared.len(), report.anomalies.len(), report.skipped.len(), report.slack_retries);
        let mut last_plan: Vec<CovId> = vec![];
        let none = BTreeSet::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tk.batch(*step, &none, report, &mut last_plan)));
        let outcome = match r {
            Ok(b) => b,
            Err(_) => {
                // Roll back what the failed pass added to the report, then find the order to blame.
                report.suspects.truncate(marks.0);
                report.cleared.truncate(marks.1);
                report.anomalies.truncate(marks.2);
                report.skipped.truncate(marks.3);
                report.slack_retries = marks.4;
                match tk.isolate(*step, last_plan, report) {
                    Some(b) => b,
                    // The guilty books are excluded for the tick. Plan the rest.
                    None => continue,
                }
            }
        };
        match outcome {
            Batch::Prepared(p) => {
                tk.apply(&p);
                report.prepared.push(*p);
                *step += 1;
            }
            Batch::Nothing => return false,
            Batch::Failed(key, why, gave_up) => {
                // The rest of the view is still planned. What failed waits for the next tick.
                report.skipped.push((key, why));
                if gave_up.iter().all(|id| tk.excluded.contains(id)) {
                    return false;
                }
                tk.excluded.extend(gave_up);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::tx::Utxo;

    fn k(tag: u8, kas: u64, accepted: bool) -> KeyUtxo {
        KeyUtxo {
            utxo: Utxo {
                transaction_id: [tag; 32],
                index: 0,
                amount: kas * 100_000_000,
                block_daa_score: if accepted { 1_000 } else { super::super::chain::UNACCEPTED_DAA },
                covenant_id: None,
            },
            pubkey: [7; 32],
        }
    }

    fn kas(v: &[KeyUtxo]) -> Vec<u64> {
        v.iter().map(|k| k.utxo.amount / 100_000_000).collect()
    }

    #[test]
    fn funding_takes_the_pool_in_order_until_the_need_is_covered() {
        let cfg = FundingConfig::default();
        let pool = vec![k(1, 100, true), k(2, 50, true), k(3, 20, true)];
        assert_eq!(kas(&select_funding(&pool, 0, true, &cfg)), [100], "one UTXO when nothing is known");
        assert_eq!(kas(&select_funding(&pool, 120 * 100_000_000, true, &cfg)), [100, 50]);
        assert_eq!(kas(&select_funding(&pool, 1_000 * 100_000_000, true, &cfg)), [100, 50, 20], "all it has");
        let two = FundingConfig { max_inputs: 2, ..cfg.clone() };
        assert_eq!(kas(&select_funding(&pool, 1_000 * 100_000_000, true, &two)), [100, 50], "at most max_inputs");
        assert!(select_funding(&[], 5, true, &cfg).is_empty());
    }

    #[test]
    fn a_fragmented_pool_is_consolidated_into_the_change() {
        let cfg = FundingConfig::default();
        let pool = vec![k(1, 100, true), k(2, 50, true), k(3, 20, true), k(4, 10, true), k(5, 5, true), k(6, 1, true)];
        // More than target_utxos (4) accepted UTXOs, so the two smallest ride along.
        assert_eq!(kas(&select_funding(&pool, 0, true, &cfg)), [100, 1, 5]);
        assert_eq!(kas(&select_funding(&pool, 0, false, &cfg)), [100], "off when the batch cannot afford it");
        assert_eq!(kas(&select_funding(&pool, 0, true, &FundingConfig { consolidate: 0, ..cfg.clone() })), [100]);
        // A pool at the target is left alone, and the tick's unaccepted change is never
        // consolidated.
        assert_eq!(kas(&select_funding(&pool[..4], 0, true, &cfg)), [100]);
        let mut chained = pool[..4].to_vec();
        chained.push(k(9, 3, false));
        assert_eq!(kas(&select_funding(&chained, 0, true, &cfg)), [100]);
        // Never more than max_inputs, and never the same UTXO twice.
        let tight = FundingConfig { max_inputs: 2, ..cfg.clone() };
        assert_eq!(kas(&select_funding(&pool, 0, true, &tight)), [100, 1]);
        assert_eq!(kas(&select_funding(&pool, 175 * 100_000_000, true, &cfg)), [100, 50, 20, 10, 1, 5]);
    }

    #[test]
    fn the_builders_shortfall_is_read_back() {
        let e = kob_protocol::Error::InsufficientFunds { need: 47_308_760_000, have: 46_766_411_000 }.to_string();
        assert_eq!(super::super::lower::insufficient_funds(&format!("lowering: {e}")), Some((47_308_760_000, 46_766_411_000)));
        assert_eq!(super::super::lower::insufficient_funds("engine rejected: input 3"), None);
    }
}
