//! One matcher tick: the global batch ([`super::batch`]) over every book and pair order of the view, lowered, built,
//! signed and validated through the rusty-kaspa v2.1.0 engine; then the next batch on the book that transaction leaves
//! (§7: untouched orders as they are, the continuations of the transaction unaccepted and chain-safe only), until nothing
//! profitable crosses or the tick's wall-clock budget runs out. Stops are triggered and armed inside these batches, next to the
//! plain fills that are their evidence (§4): there is no separate trigger transaction.
//!
//! A batch is sized against the physical limits of a transaction ([`super::planner::PHYSICAL_TX_BYTES`] and the block mass
//! limits of the built transaction), not against a fixed count: when a built batch overruns a limit the byte budget of the
//! next attempt is scaled down by the overrun and the batch re-planned.
//!
//! Every transaction the matcher submits has passed [`kob_protocol::verify::validate_signed`]
//! (scripts with enforced budgets, covenant context, storage-mass commitment, fee floor): the
//! covenants are the reference (`docs/spec/matcher.md` §8).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use kob_protocol::artifacts::TemplateId;
use kob_protocol::family::Family;
use kob_protocol::tx::{
    finalize, FinalizeOptions, KeyUtxo, SignedTx, BLOCK_COMPUTE_LIMIT, BLOCK_STORAGE_LIMIT, BLOCK_TRANSIENT_LIMIT,
};
use kob_protocol::verify::{validate_signed, Validation};
use serde::{Deserialize, Serialize};

use super::batch::{plan_batch, plan_batch_counted, BatchInput, PlanWork};
use super::book::{book_key, books, outpoint, BookKey, Clock, CovId, ListedOrder, Outpoint};
use super::candidate::Class;
use super::family::{token_limits, Families, Lowered};
use super::lower::{accounting, Accounting, LowerCtx};
use super::planner::{Plan, PlannerConfig};
use super::wallet::Signer;

/// Engine settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineConfig {
    pub planner: PlannerConfig,
    /// KAS on the operator's token outputs (the reference planner leaves the operator no tokens).
    pub token_carrier: u64,
    /// Validate every signed transaction in the script engine before it is returned.
    pub validate: bool,
    /// Plan chained steps onto unaccepted parents (§7). Off: a partially filled order continues next tick; the orders a
    /// transaction did not touch still go into the tick's next transactions.
    pub chain_unconfirmed: bool,
    /// Wall-clock budget of one whole tick, milliseconds: the defence against a hostile book. What is
    /// left when it runs out waits for the next tick. 0: unlimited (tests).
    pub tick_budget_ms: u64,
    /// Wall-clock budget of one batch's planning, milliseconds; the planner stops refining at it and hands over what it
    /// has (the historical name is kept for configuration files).
    pub book_budget_ms: u64,
    /// How a batch picks the operator's funding UTXOs.
    pub funding: FundingConfig,
    /// The tick's fee rates (`crate::fee`, set by the runner from the node's estimate): a batch that fills an immediate
    /// order (IOC, FOK, market, streaming) or a triggered stop is built at the high rate when it stays profitable there (else
    /// at the highest rate that keeps it profitable, at least `planner.fee_rate`, the rate it was planned at); every batch
    /// is held to the total cap. The default (the floor everywhere, no cap) builds every batch at `planner.fee_rate`.
    pub fees: crate::fee::FeeRates,
}

/// How a batch picks the operator's P2PK funding (`docs/ops/executor.md`, funding). Deterministic: the pool is ordered by
/// amount (largest first, then outpoint), the change outputs of the tick's earlier transactions after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FundingConfig {
    /// Most funding inputs of one batch. A batch takes the UTXOs of the pool in order until the builder's need is covered;
    /// a batch the first `max_inputs` cannot pay is left for later.
    pub max_inputs: usize,
    /// The pool counts as fragmented above this many accepted UTXOs: every batch then also spends up to `consolidate` of
    /// the smallest, merged into its one change output (only while the batch stays profitable with them).
    pub target_utxos: usize,
    /// Most extra UTXOs one batch consolidates (0: off).
    pub consolidate: usize,
}

impl Default for FundingConfig {
    fn default() -> Self {
        FundingConfig { max_inputs: 8, target_utxos: 4, consolidate: 2 }
    }
}

/// Serialized bytes of one P2PK funding input with its signature script (outpoint, sequence, budget, 66-byte push).
const FUNDING_INPUT_BYTES: u64 = 120;

/// The funding of a batch from `pool` (in pool order): UTXOs until their sum reaches `need` (at least one, at most
/// `max_inputs`); then, when `consolidate` and the pool holds more than `target_utxos` accepted UTXOs, up to
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
        }
    }
}

/// Snapshot the tick plans against.
pub struct TickInput {
    pub orders: Vec<ListedOrder>,
    pub clock: Clock,
    /// The operator's spendable P2PK UTXOs (none reserved by pending transactions).
    pub funding: Vec<KeyUtxo>,
    /// Orders spent by pending transactions or backed off.
    pub excluded: BTreeSet<CovId>,
}

/// A signed, validated transaction ready for submission.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The book of the plan's first fill ([`Plan::books`] lists every book the transaction trades).
    pub book: BookKey,
    /// Position of the transaction in the tick (0: the first).
    pub step: usize,
    /// The latest earlier transaction of the tick whose outputs this one spends (funding change or order continuations).
    pub parent: Option<[u8; 32]>,
    pub plan: Plan,
    pub lowered: Lowered,
    pub signed: SignedTx,
    pub accounting: Accounting,
    pub validation: Option<Validation>,
}

impl Prepared {
    pub fn txid(&self) -> [u8; 32] {
        self.signed.tx.id
    }
}

/// An order the engine pre-simulation says the token program will not let move (frozen, blacklisted, or a program that changed its
/// rules): flagged "possibly frozen" and left out of the books (`indexer::flags`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspect {
    pub id: CovId,
    pub book: BookKey,
    pub reason: String,
}

/// Result of a tick.
#[derive(Default, Debug)]
pub struct TickReport {
    /// Orders whose pre-simulation was rejected by a token program (attributed to the order: its custody input failed, or dropping
    /// it made the same plan pass).
    pub suspects: Vec<Suspect>,
    /// Orders in the transactions prepared this tick: their pre-simulation passed, so any flag they carry is stale.
    pub cleared: Vec<CovId>,
    pub prepared: Vec<Prepared>,
    /// Books skipped, and why.
    pub skipped: Vec<(BookKey, String)>,
    /// Orders left out of this and later ticks: their numbers fail the sanity gate ([`crate::sanity`]), planning them
    /// panicked (the last line of defence, [`PLANNING_PANICKED`]), or the builder refused the order on its own
    /// ([`REFUSED_BY_THE_BUILDER`]). The runner keeps the ids of the last two out of the book for [`super::run::QUARANTINE_DAA`].
    pub quarantined: Vec<(CovId, String)>,
    /// Plans the builders or the engine refused (a planner defect, or a chained step that must
    /// wait for its parent). Logged; the batch is retried without the offending fill.
    pub anomalies: Vec<(BookKey, usize, String)>,
    /// Builds of this tick that had to be retried with measured compute budgets (the table of `kob-protocol` was short for
    /// that shape, [`super::lower::measured_floors`]). Per tick, unlike the process-wide
    /// [`super::lower::budget_slack_retries`] counter.
    pub slack_retries: usize,
    /// What the tick's batch plans cost ([`PlanWork`]: plans, allocation passes, candidate pairs tried, plans the book budget
    /// cut short), summed over the tick. Machine-independent: the cost bounds of `tests/tick_cost_bound.rs` are stated in it.
    pub work: PlanWork,
}

/// The pre-submission validation of a signed transaction: the script engine ([`validate_signed`]). Tests inject their own.
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

/// The input an engine rejection names (`input 3: ...`) when the input runs a TOKEN program (its budget role is
/// `<TokenProgram>.<entry>...`, an order's is `KobAsk.settle.x@<TokenProgram>`).
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

/// The fill a failed attempt gives up: the one whose order input the engine names (order inputs lead the transaction in
/// fill order), or the updated order whose `update` input it names; else the lowest-priority fill that is not class 1. A
/// batch of many books loses only the leg (or the update) at fault.
fn victim_of(plan: &Plan, lowered: &Lowered, err: &str) -> Option<CovId> {
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
    victim(plan)
}

/// The fill a failed attempt gives up: the lowest-priority one that is not class 1.
fn victim(plan: &Plan) -> Option<CovId> {
    plan.fills.iter().rev().find(|f| f.cand.class != Class::Immediate).or(plan.fills.last()).map(|f| f.cand.id)
}

/// Reason of an order quarantined because its planning panicked ([`TickReport::quarantined`]).
pub const PLANNING_PANICKED: &str = "planning panicked";
/// Reason prefix of an order quarantined because the builder refused it on its own (`order of leg <i>: ...`, see
/// [`lowering_leg`]): such an order fails the same way every time it is planned.
pub const REFUSED_BY_THE_BUILDER: &str = "refused by the builder";

/// The leg a lowering error names as the order at fault: the batch builder (`kob_protocol::build`) names the leg of a refusal
/// that depends on that order alone (its token programs, its family) as `order of leg <i>: ...`; leg `i` is `plan.fills[i]`
/// ([`super::lower::lower_batch`]).
pub fn lowering_leg(err: &str) -> Option<usize> {
    leg_named(err, "order of leg ")
}

/// The leg a lowering error names as the fill at fault: the batch builder names the leg of a refusal of the fill the plan
/// chose for a pair order (its quantity, its continuation, its evidence) as `fill of leg <i>: ...`. That order is left out of
/// the batch (not quarantined: another fill of it may be built), and the other legs are planned again without it.
pub fn lowering_fill_leg(err: &str) -> Option<usize> {
    leg_named(err, "fill of leg ")
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
/// Most single-order removals tried to isolate a panicking batch (each is a full attempt).
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
    /// Continuations of this tick's transactions (unaccepted) and how many transactions each order was in.
    unaccepted: BTreeSet<CovId>,
    depth: BTreeMap<CovId, usize>,
    pool: Vec<KeyUtxo>,
    /// Never planned this tick: the input's exclusions, quarantined orders, skipped books.
    excluded: BTreeSet<CovId>,
}

/// What one batch attempt loop produced.
enum Batch {
    Prepared(Box<Prepared>),
    /// Nothing (more) to plan.
    Nothing,
    /// Every attempt failed: the last error, and the orders the attempts gave up (they sit out the rest of the tick).
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

    /// Plans, builds, signs and validates the next batch of the tick. `extra` orders are left out (isolation); `last_plan`
    /// receives the order ids of the latest plan attempted (isolation of a panic after planning).
    fn batch(&self, step: usize, extra: &BTreeSet<CovId>, report: &mut TickReport, last_plan: &mut Vec<CovId>) -> Batch {
        let cfg = self.cfg;
        let mut ex: BTreeSet<CovId> = self.excluded.union(extra).copied().collect();
        // orders quarantined earlier in the tick (the builder refused them) are never planned again
        ex.extend(report.quarantined.iter().map(|q| q.0));
        // Funding (`select_funding`): the first attempt takes the largest UTXO, a builder's "insufficient funds" names the
        // need; a fragmented pool is consolidated while the batch stays profitable with it.
        let mut need = 0u64;
        let mut consolidate = true;
        let mut funding = select_funding(&self.pool, need, consolidate, &cfg.funding);
        let mut max_bytes = cfg.planner.tx_byte_budget();
        // measured budgets for roles the table gives too little (the safety net below)
        let mut floor: BTreeMap<String, u16> = BTreeMap::new();
        let mut measured = false;
        let mut last: Option<(BookKey, String)> = None;
        // a token-program rejection that names no order: the fill dropped next is a suspect if the rest then passes
        let mut pending_suspect: Option<(CovId, BookKey, String)> = None;
        // the high rate of an urgent batch; off once the operator's whole pool cannot pay it (the batch then goes at the rate
        // it was planned at)
        let mut high_ok = true;
        for _attempt in 0..MAX_ATTEMPTS {
            if self.out_of_time() {
                break;
            }
            let bi = BatchInput {
                by_id: &self.by_id,
                lock_time: self.lock,
                utc: self.inp.clock.utc,
                excluded: &ex,
                unaccepted: &self.unaccepted,
                families: &self.fams,
                max_bytes,
                only: None,
                deadline: self.plan_deadline(),
            };
            let (plan, work) = plan_batch_counted(&bi, &cfg.planner);
            report.work.add(work);
            let Some(plan) = plan else { break };
            *last_plan = plan.fills.iter().map(|f| f.cand.id).collect();
            let key = plan.book;
            let Some(adapter) = self.families.get(key.family) else {
                last = Some((key, format!("no {} builders in this build", key.family.as_str())));
                break;
            };
            // The batch's fee rate (`crate::fee`): the high rate when it fills an immediate order or a triggered stop (their
            // deadlines are a minute away), else the rate it was planned at; held to the total cap, and lowered to the highest
            // rate that keeps the batch profitable (never below the planned rate, at which the planner found it profitable).
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
                            // the inventory a plan keeps is income the KAS accounting does not see (its policy value)
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
                    // short of funds: more funding inputs (with a margin for their fee) while the pool has more to give
                    // (the builder counts every input: the funding must grow by the shortfall)
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
                            // the pool cannot pay the high rate: the planned rate
                            high_ok = false;
                            continue;
                        }
                    }
                    let why = format!("lowering: {e}");
                    report.anomalies.push((key, step, why.clone()));
                    last = Some((key, why));
                    // A refusal that names the order at fault (its token programs, its family: nothing the plan chose) fails
                    // the same way every time that order is planned: it is quarantined, for this tick and (by the runner)
                    // the next ones, like an order whose planning panics, and the batch is planned again without it. A
                    // continuation of this tick's own transactions is only left out of this batch.
                    if let Some(id) = lowering_leg(&e).and_then(|i| plan.fills.get(i)).map(|f| f.cand.id) {
                        if !self.unaccepted.contains(&id) && !report.quarantined.iter().any(|q| q.0 == id) {
                            report.quarantined.push((id, format!("{REFUSED_BY_THE_BUILDER}: {e}")));
                        }
                        ex.insert(id);
                        // the order at fault is out: what is left is planned as if it had never been listed
                        last = None;
                        continue;
                    }
                    // A refusal of the fill the plan chose for one pair order: that order sits out this batch, the other
                    // legs (of any book) are planned again without it.
                    if let Some(id) = lowering_fill_leg(&e).and_then(|i| plan.fills.get(i)).map(|f| f.cand.id) {
                        ex.insert(id);
                        last = None;
                        continue;
                    }
                    match victim(&plan) {
                        Some(v) => {
                            ex.insert(v);
                            continue;
                        }
                        None => break,
                    }
                }
            };
            let acct = accounting(&lowered, &funding);
            // The physical limits of the built transaction (the plan's byte estimate is only an estimate): scale the budget
            // down by the overrun and plan again.
            let m = lowered.built.fee.mass;
            let knob = cfg.planner.max_tx_bytes;
            let over: f64 = [
                m.compute as f64 / BLOCK_COMPUTE_LIMIT as f64,
                m.transient as f64 / BLOCK_TRANSIENT_LIMIT as f64,
                m.storage as f64 / BLOCK_STORAGE_LIMIT as f64,
                if knob > 0 { acct.bytes as f64 / knob as f64 } else { 0.0 },
            ]
            .into_iter()
            .fold(0.0, f64::max);
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
            // the batch's profit: its exact KAS (`change − funding`) and the policy value of the inventory it keeps
            if acct.profit.saturating_add(plan.kept_value()) < cfg.planner.min_profit {
                // the consolidated inputs' fee must not cost the batch: plan it again without them
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
            match sign_and_validate(&lowered, self.signer, cfg.validate, self.validator) {
                Ok((signed, validation)) => {
                    if let Some((id, book, reason)) = pending_suspect.take() {
                        report.suspects.push(Suspect { id, book, reason });
                    }
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
                    // Safety net only (the budget table is exact over every shape the planner builds): a budget the
                    // table gives too little is measured in the engine and the batch retried once with the budgets its
                    // inputs need, logged and counted (`lower::budget_slack_retries`, 0 in every test). The batch is
                    // never skipped tick after tick for a short table entry.
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
                    // The token program rejected the plan (not an order covenant, not a budget): a frozen or blacklisted
                    // balance looks like this. Attribute it to the (accepted) order whose custody the failing input spends;
                    // when it names none, the fill dropped next is the suspect if the rest passes.
                    if let Some(i) = token_input_failure(&e, &lowered) {
                        let why = format!("the token program rejected the pre-simulated spend ({})", lowered.built.roles[i]);
                        if let Some(id) = custody_owner(&plan, &lowered, &self.by_id, i).filter(|id| !self.unaccepted.contains(id)) {
                            report.suspects.push(Suspect { id, book: key, reason: why });
                            ex.insert(id);
                            continue;
                        }
                        if let Some(v) = victim(&plan).filter(|v| !self.unaccepted.contains(v)) {
                            pending_suspect = Some((v, key, why));
                        }
                    }
                    match victim_of(&plan, &lowered, &e) {
                        Some(v) => {
                            ex.insert(v);
                            continue;
                        }
                        None => break,
                    }
                }
            }
        }
        match last {
            Some((k, why)) => {
                let mut gave_up: Vec<CovId> = ex.difference(&self.excluded).copied().collect();
                gave_up.extend(last_plan.iter().copied());
                Batch::Failed(k, why, gave_up)
            }
            None => Batch::Nothing,
        }
    }

    /// The book after a prepared transaction: continuations replace their orders (unaccepted, one transaction deeper),
    /// everything else it spent leaves; the funding pool gives up every UTXO the transaction spends and takes the change.
    fn apply(&mut self, p: &Prepared) {
        let (continued, next_funding) = super::chain::apply(&mut self.by_id, &p.plan, &p.lowered.built, self.operator);
        for f in &p.plan.fills {
            *self.depth.entry(f.cand.id).or_insert(0) += 1;
        }
        for id in continued {
            let deep = self.depth.get(&id).copied().unwrap_or(0) >= self.cfg.planner.max_chain.max(1);
            if !self.cfg.chain_unconfirmed || deep {
                // continues next tick
                self.by_id.remove(&id);
            } else {
                self.unaccepted.insert(id);
            }
        }
        let spent: BTreeSet<Outpoint> = p.lowered.built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
        let before = self.pool.len();
        self.pool.retain(|k| !spent.contains(&outpoint(&k.utxo)));
        if self.pool.len() < before {
            // The change funds the next transactions (a P2PK spend never reads its parent's DAA score, so it chains safely).
            if let Some(f) = next_funding {
                self.pool.push(f);
            }
        }
    }

    /// Isolation after a panic (the last line of defence): the order whose removal makes the batch pass is
    /// quarantined and that pass is the batch's result. The orders tried are those of the plan that panicked (a defect in
    /// lowering or validation), else those of the books whose planning panics on its own. When no single order explains
    /// it, those books sit out the rest of the tick.
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
            // planning itself panicked: the books whose planning panics alone
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
                };
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plan_batch(&bi, &self.cfg.planner))).is_err() {
                    books_of.insert(k);
                    candidates.extend(self.by_id.values().filter(|o| book_key(o) == Some(k)).map(|o| o.id()));
                }
            }
            // a pair order belongs to no book: its planning is tried with the books of its tokens
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
        // no single order explains it: those books wait for the next tick, the process lives
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

/// [`tick`] with a custom pre-submission validator (the default is the script engine).
pub fn tick_with(inp: &TickInput, cfg: &EngineConfig, families: &Families, signer: &dyn Signer, validator: Validator) -> TickReport {
    let mut report = TickReport::default();
    let started = Instant::now();
    let tick_deadline = (cfg.tick_budget_ms > 0).then(|| started + Duration::from_millis(cfg.tick_budget_ms));
    // no order with hostile numbers reaches the planner; it is quarantined with the reason
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
    // books this build cannot plan are reported once and left out
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
    };
    let mut step = 0usize;
    let mut out_of_time = false;
    loop {
        if tk.out_of_time() {
            out_of_time = true;
            break;
        }
        let marks = (report.suspects.len(), report.cleared.len(), report.anomalies.len(), report.skipped.len(), report.slack_retries);
        let mut last_plan: Vec<CovId> = vec![];
        let none = BTreeSet::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tk.batch(step, &none, &mut report, &mut last_plan)));
        let outcome = match r {
            Ok(b) => b,
            Err(_) => {
                // roll back what the failed pass added to the report, then find the order to blame
                report.suspects.truncate(marks.0);
                report.cleared.truncate(marks.1);
                report.anomalies.truncate(marks.2);
                report.skipped.truncate(marks.3);
                report.slack_retries = marks.4;
                match tk.isolate(step, last_plan, &mut report) {
                    Some(b) => b,
                    // the guilty books are excluded for the tick: plan the rest
                    None => continue,
                }
            }
        };
        match outcome {
            Batch::Prepared(p) => {
                tk.apply(&p);
                report.prepared.push(*p);
                step += 1;
            }
            Batch::Nothing => break,
            Batch::Failed(key, why, gave_up) => {
                // the rest of the view is still planned; what failed waits for the next tick
                report.skipped.push((key, why));
                if gave_up.iter().all(|id| tk.excluded.contains(id)) {
                    break;
                }
                tk.excluded.extend(gave_up);
            }
        }
    }
    if out_of_time {
        if let Some(k) = tk.by_id.values().find_map(book_key) {
            report.skipped.push((k, "tick budget exhausted; the rest is planned next tick".into()));
        }
    }
    report
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
        // more than target_utxos (4) accepted UTXOs: the two smallest ride along
        assert_eq!(kas(&select_funding(&pool, 0, true, &cfg)), [100, 1, 5]);
        assert_eq!(kas(&select_funding(&pool, 0, false, &cfg)), [100], "off when the batch cannot afford it");
        assert_eq!(kas(&select_funding(&pool, 0, true, &FundingConfig { consolidate: 0, ..cfg.clone() })), [100]);
        // a pool at the target is left alone, and the tick's unaccepted change is never consolidated
        assert_eq!(kas(&select_funding(&pool[..4], 0, true, &cfg)), [100]);
        let mut chained = pool[..4].to_vec();
        chained.push(k(9, 3, false));
        assert_eq!(kas(&select_funding(&chained, 0, true, &cfg)), [100]);
        // never more than max_inputs, never the same UTXO twice
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
