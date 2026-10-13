//! Keepers (`docs/spec/matcher.md` §5, §1.2): permissionless maintenance paid by the orders' own tips.
//!
//! | Job | When | Paid by |
//! |---|---|---|
//! | refund | soft expiry, 90 days idle, repeating entries (`KobIfdBid.refund`, `KobIfdAsk.settle(0)`) | `refundTip` |
//! | kill | IOC / FOK at `max(UTXO DAA, activeFrom) + 600` (first block that allows it), IOC / FOK pair orders included | `refundTip` |
//! | close | an empty repeating `KobIfdAsk` (`amountLeft = 0`) | `refundTip` |
//! | sweep | strays of the operator's own orders (cancel-replace; nobody else may move strays) | — |
//!
//! Arming and trailing are not keeper jobs: an `update` reads its trigger evidence from a plain resting fill
//! of the same transaction, so the matcher arms and ratchets stops inside the batches that fill their evidence
//! (`crate::matcher::batch`, §4) and takes their `keeperTip` there.
//!
//! Day orders need no special keeper: conforming matchers stop at the placement record's UTC
//! `deadline` (§2.3) and the refund opens at `expiryDaa`. Repeat-IFD merges are fills and belong
//! to the matcher, which sequences them (one merge per entry per transaction; the next exit merges
//! once the entry's continuation is accepted). Strays of other makers are only reported.
//!
//! Every job is built with `kob_protocol` builders, signed with the hot key and validated in the
//! v2.1.0 engine before submission, exactly like matcher batches.
//!
//! Liveness under fee pressure (`docs/ops/executor.md`, fee policy): a kill or refund whose tip pays it at the floor but not
//! at its urgency's rate may be topped up from the operator's own funding, at most [`KeeperConfig::subsidy_max`] per job
//! (the maker's outputs stay what the covenant requires), and a kill or refund of this process that stays unaccepted is
//! rebuilt at a higher rate ([`reprice`]) and replaced by the runner (`crate::matcher::run`). The subsidies of all jobs
//! together are bounded by a rolling budget ([`KeeperConfig::subsidy_budget`] per [`KeeperConfig::subsidy_window_daa`],
//! kept by the runner in a [`SubsidyBudget`], replacements included) and by a number of subsidised jobs per step
//! ([`KeeperConfig::subsidy_max_jobs`]): once either is used up, jobs go at the rate their own tip pays.

use std::collections::{BTreeSet, VecDeque};

use kob_protocol::build::{build_with, Action, ForeignStrays, RefundOrder, SweepOrder};
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, BuiltTx, FeeOptions, FinalizeOptions, KeyUtxo, OrderUtxo, SignedTx, TokenUtxo};
use kob_protocol::verify::{validate_signed, Validation};
use serde::{Deserialize, Serialize};

use crate::matcher::book::{outpoint, Clock, CovId, ListedOrder, Outpoint};
use crate::matcher::chain::UNACCEPTED_DAA;
use crate::matcher::lower::budgets;
use crate::matcher::wallet::Signer;

/// Keeper settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeeperConfig {
    /// Kept for configuration files (refunds use the exact refund time).
    pub safety_margin: u64,
    /// The lowest fee rate, sompi per gram (the floor every job may fall back to).
    pub fee_rate: u64,
    /// The tick's fee rates (`crate::fee`, set by the runner from the node's estimate): a job is built at its kind's urgency
    /// ([`JobKind::urgency`]), held to the total cap, and a paid job is lowered to the highest rate its tip still pays (not
    /// below `fee_rate`). The default (the floor everywhere, no cap) builds every job at `fee_rate`.
    pub fees: crate::fee::FeeRates,
    pub refund: bool,
    /// Sweep strays of the operator's own orders in place (`SweepOrder`: the order continues unchanged).
    pub sweep_own_strays: bool,
    /// Return an order's proven FOREIGN strays (tokens other than its own, `matcher.md` §1.2) to its maker inside the refund,
    /// kill or close that ends it: their programs authorise them with any spend of the order and no refund path reads
    /// another token, so this is permissionless, and after the order ends nothing could move them again. The maker's own
    /// token strays are never touched (every refund path refuses them; only the maker's cancel or sweep moves them). The
    /// extra inputs cost fee out of the refund tip: a job that does not pay with them is built without them.
    pub return_foreign_strays: bool,
    /// Smallest profit (tip − fee, sompi) a job must make.
    pub min_profit: i64,
    /// Most jobs per tick.
    pub max_jobs: usize,
    /// Most jobs BUILT (signed and engine-validated) per tick, successful or not: a dust order is a build attempt
    /// that yields no job, and building costs about 1.6 ms of CPU.
    pub max_attempts: usize,
    pub validate: bool,
    /// Operator fee subsidy (liveness, `docs/ops/executor.md`, fee policy): the most sompi of its own funding the operator
    /// adds to ONE kill or refund whose refund tip pays the job at the floor but not at its urgency's rate, so the job still
    /// goes at that rate (within `fees.max_tx_fee` and the policy's highest rate). The order's outputs are what the covenant
    /// requires whatever the fee (the maker gets `value - refundTip`): the top-up only comes out of the operator's P2PK
    /// funding inputs. 0: off (the job goes at the highest rate its tip pays). Closes and sweeps are never subsidised.
    pub subsidy_max: u64,
    /// The most sompi of subsidy ([`KeeperConfig::subsidy_max`]) all jobs together may take within any
    /// [`KeeperConfig::subsidy_window_daa`] DAA (a rolling window the runner keeps in a [`SubsidyBudget`], the subsidy
    /// added by replacements included). [`tick`] alone never hands out more than this in one tick. Once it is used up, kills
    /// and refunds go at the highest rate their own tip pays (exactly as with `subsidy_max = 0`). 0: no subsidy at all.
    /// Without it, a flood of small IOC / GTD orders whose tips pay only the floor, expiring under fee pressure (which a miner
    /// can create and collect), drains the operator's funding `subsidy_max` at a time.
    pub subsidy_budget: u64,
    /// The rolling window of [`KeeperConfig::subsidy_budget`], DAA (10 per second; 0 is taken as 1).
    pub subsidy_window_daa: u64,
    /// The most kills and refunds one runner step subsidises (new jobs and replacements that add subsidy together): a burst of
    /// due orders cannot take the whole window's budget in one step. 0: no subsidy.
    pub subsidy_max_jobs: usize,
    /// Replace-by-fee of the operator's own stuck kills and refunds (`crate::matcher::run`): a kill or refund this process
    /// submitted that no chain block accepted after this many DAA is rebuilt at a higher rate and resubmitted with
    /// `submitTransactionReplacement`.
    pub replace_after_daa: u64,
    /// Most replacements of one kill or refund (0: never replace).
    pub max_replacements: u32,
    /// The weighted selection of this tick (`docs/spec/matcher.md` §3.6, [`crate::matcher::select`]), set by the runner every
    /// step. None (the default), or a share of one: every due job is built. A share below one: the jobs due for less than
    /// `max_defer_daa` are sampled, weighted by their tip per byte; the others wait for the next tick (another keeper usually
    /// takes them meanwhile).
    pub select: Option<crate::matcher::select::TickSelect>,
}

impl Default for KeeperConfig {
    fn default() -> Self {
        KeeperConfig {
            safety_margin: 5,
            fee_rate: 100,
            fees: crate::fee::FeeRates::default(),
            refund: true,
            sweep_own_strays: false,
            return_foreign_strays: false,
            min_profit: 0,
            max_jobs: 32,
            max_attempts: 128,
            validate: true,
            subsidy_max: 0,
            subsidy_budget: DEFAULT_SUBSIDY_BUDGET,
            subsidy_window_daa: DEFAULT_SUBSIDY_WINDOW_DAA,
            subsidy_max_jobs: DEFAULT_SUBSIDY_MAX_JOBS,
            replace_after_daa: 100,
            max_replacements: 0,
            select: None,
        }
    }
}

/// Default [`KeeperConfig::subsidy_budget`]: 1 KAS.
pub const DEFAULT_SUBSIDY_BUDGET: u64 = 100_000_000;
/// Default [`KeeperConfig::subsidy_window_daa`]: 36,000 DAA (one hour at 10 blocks per second).
pub const DEFAULT_SUBSIDY_WINDOW_DAA: u64 = 36_000;
/// Default [`KeeperConfig::subsidy_max_jobs`].
pub const DEFAULT_SUBSIDY_MAX_JOBS: usize = 8;

/// Command-line options of the keepers' liveness under fee pressure (`kob-executor run`, `keep`, `match --keep`).
#[derive(clap::Args, Debug, Clone)]
pub struct LivenessArgs {
    /// Keepers: the most KAS of its own funding the operator adds to the fee of ONE kill or refund whose refund tip pays it
    /// at the floor but not at its urgency's rate (the maker's outputs never change; still within `--fee-max-rate` and
    /// `--fee-max-tx-kas`). 0: never (the job goes at the highest rate its tip pays).
    #[arg(long, default_value_t = 0.05)]
    pub keeper_subsidy_max_kas: f64,
    /// Keepers: the most KAS of its own funding the operator adds to kills and refunds ALTOGETHER (replacements included)
    /// within any `--keeper-subsidy-window-daa` (a rolling window; it starts empty when the process starts). Once it is used
    /// up, jobs go at the highest rate their own tip pays until older subsidies leave the window. 0: never subsidise.
    #[arg(long, default_value_t = 1.0)]
    pub keeper_subsidy_budget_kas: f64,
    /// Keepers: the rolling window of `--keeper-subsidy-budget-kas`, DAA (10 per second; 36,000 = one hour).
    #[arg(long, default_value_t = DEFAULT_SUBSIDY_WINDOW_DAA)]
    pub keeper_subsidy_window_daa: u64,
    /// Keepers: the most kills and refunds subsidised per tick (new jobs and replacements together). 0: never subsidise.
    #[arg(long, default_value_t = DEFAULT_SUBSIDY_MAX_JOBS)]
    pub keeper_subsidy_max_jobs: usize,
    /// Keepers: a kill or refund this process submitted that is still unaccepted after this many DAA (10 per second) is
    /// rebuilt at a higher rate and replaced (`submitTransactionReplacement`).
    #[arg(long, default_value_t = 100)]
    pub keeper_replace_after_daa: u64,
    /// Keepers: most replacements of one kill or refund (0: never replace; then a stuck one waits for the pending timeout).
    #[arg(long, default_value_t = 3)]
    pub keeper_max_replacements: u32,
}

impl LivenessArgs {
    /// Sets the liveness options of `cfg`.
    pub fn apply(&self, cfg: &mut KeeperConfig) -> Result<(), String> {
        let k = self.keeper_subsidy_max_kas;
        if !k.is_finite() || !(0.0..=1e9).contains(&k) {
            return Err(format!("--keeper-subsidy-max-kas {k} is not an amount of KAS"));
        }
        cfg.subsidy_max = (k * 100_000_000.0).round() as u64;
        let b = self.keeper_subsidy_budget_kas;
        if !b.is_finite() || !(0.0..=1e9).contains(&b) {
            return Err(format!("--keeper-subsidy-budget-kas {b} is not an amount of KAS"));
        }
        cfg.subsidy_budget = (b * 100_000_000.0).round() as u64;
        if self.keeper_subsidy_window_daa == 0 {
            return Err("--keeper-subsidy-window-daa must be at least 1".into());
        }
        cfg.subsidy_window_daa = self.keeper_subsidy_window_daa;
        cfg.subsidy_max_jobs = self.keeper_subsidy_max_jobs;
        cfg.replace_after_daa = self.keeper_replace_after_daa.max(1);
        cfg.max_replacements = self.keeper_max_replacements;
        Ok(())
    }
}

/// Kind of keeper job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Kill,
    Refund,
    Close,
    Sweep,
}

impl JobKind {
    /// How soon the job should be accepted (`crate::fee`): a kill ends an IOC / FOK order at its deadline (high), a refund
    /// returns a maker's funds (normal), a close or a sweep is housekeeping (low).
    pub fn urgency(self) -> crate::fee::Urgency {
        use crate::fee::Urgency;
        match self {
            JobKind::Kill => Urgency::High,
            JobKind::Refund => Urgency::Normal,
            JobKind::Close | JobKind::Sweep => Urgency::Low,
        }
    }

    /// Whether the operator may top up the job's fee from its own funding ([`KeeperConfig::subsidy_max`]) and replace it
    /// when it is stuck ([`KeeperConfig::max_replacements`]): a kill (an IOC / FOK must end at its deadline) and a refund (a
    /// maker's funds go back), never a close or a sweep (housekeeping).
    pub fn liveness(self) -> bool {
        matches!(self, JobKind::Kill | JobKind::Refund)
    }

    pub fn name(self) -> &'static str {
        match self {
            JobKind::Kill => "kill",
            JobKind::Refund => "refund",
            JobKind::Close => "close",
            JobKind::Sweep => "sweep",
        }
    }
}

/// A signed, validated keeper transaction.
#[derive(Clone, Debug)]
pub struct Job {
    pub kind: JobKind,
    pub order: CovId,
    pub action: Action,
    pub built: BuiltTx,
    pub signed: SignedTx,
    pub validation: Option<Validation>,
    /// Operator profit: change − funding (sompi).
    pub profit: i64,
    /// Sompi of the operator's own funding the fee took beyond the refund tip ([`KeeperConfig::subsidy_max`]); 0 for a job its
    /// tip pays.
    pub subsidy: u64,
    pub spends: BTreeSet<Outpoint>,
}

/// Snapshot the keepers work on.
pub struct KeeperInput {
    pub orders: Vec<ListedOrder>,
    pub clock: Clock,
    pub funding: Vec<KeyUtxo>,
    pub excluded: BTreeSet<CovId>,
    pub excluded_outpoints: BTreeSet<Outpoint>,
    /// (order, order outpoint) pairs an earlier tick found unprofitable at their fixed tip: not built again while the
    /// outpoint stands. The runner keeps the set from [`KeeperReport::unprofitable`].
    pub known_unprofitable: BTreeSet<(CovId, Outpoint)>,
}

/// Result of a keeper tick.
#[derive(Debug, Default)]
pub struct KeeperReport {
    pub jobs: Vec<Job>,
    /// Orders with strays the operator may not sweep (reported for monitoring).
    pub foreign_strays: Vec<CovId>,
    pub skipped: Vec<(CovId, String)>,
    /// Jobs built and found unprofitable at the order's fixed tip, for [`KeeperInput::known_unprofitable`].
    pub unprofitable: Vec<(CovId, Outpoint)>,
    /// Jobs that could not be built, signed or validated (a refund tip above the order's value, a token program that refuses
    /// to move the custody, ...): (order, order outpoint). The runner leaves the outpoint out of the next ticks with a growing
    /// backoff (`excluded_outpoints`), so unbuildable orders cannot eat the attempt budget of every tick and starve the
    /// buildable kills and refunds queued behind them.
    pub failed: Vec<(CovId, Outpoint)>,
    /// Weighted selection ([`KeeperConfig::select`]): the due jobs the sample chose from, those it took, those built because
    /// they were due for `max_defer_daa` (the starvation bound), and the orders whose jobs it left for later.
    pub sampled: usize,
    pub taken: usize,
    pub overdue: usize,
    pub deferred: Vec<CovId>,
}

/// What one keeper tick may still hand out as operator subsidy ([`tick_with_subsidy`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubsidyAllowance {
    /// Sompi, all jobs of the tick together.
    pub sompi: u64,
    /// Subsidised jobs.
    pub jobs: usize,
}

impl SubsidyAllowance {
    /// The allowance of a tick without a runner: one window's budget and the per-step job cap.
    pub fn of(cfg: &KeeperConfig) -> Self {
        SubsidyAllowance { sompi: cfg.subsidy_budget, jobs: cfg.subsidy_max_jobs }
    }

    /// The most subsidy the next job may take (0 when the allowance is used up).
    pub fn per_job(&self, cfg: &KeeperConfig) -> u64 {
        if self.jobs == 0 {
            0
        } else {
            cfg.subsidy_max.min(self.sompi)
        }
    }

    /// Takes one job's subsidy out of the allowance (nothing for a job its tip pays).
    pub fn take(&mut self, subsidy: u64) {
        if subsidy > 0 {
            self.sompi = self.sompi.saturating_sub(subsidy);
            self.jobs = self.jobs.saturating_sub(1);
        }
    }
}

/// The operator subsidies of a rolling window ([`KeeperConfig::subsidy_budget`]), kept by the runner across ticks and
/// replacements: (virtual DAA score, sompi) of every subsidy charged. A subsidy is charged when its transaction is submitted
/// (also without a verdict: it may be accepted), only the increase for a replacement; a transaction that never makes it into
/// a block is not refunded (conservative: the total charged is at least what the operator actually paid). In memory only:
/// a restart starts with an empty window.
#[derive(Clone, Debug, Default)]
pub struct SubsidyBudget {
    spends: VecDeque<(u64, u64)>,
}

impl SubsidyBudget {
    /// Sompi charged within the window that ends at `daa` (charges at a later DAA score, from before a node switch to one
    /// that is behind, count too).
    pub fn spent(&self, cfg: &KeeperConfig, daa: u64) -> u64 {
        let w = cfg.subsidy_window_daa.max(1);
        self.spends.iter().filter(|(d, _)| d.saturating_add(w) > daa).fold(0u64, |a, (_, s)| a.saturating_add(*s))
    }

    /// What the window ending at `daa` still allows.
    pub fn left(&self, cfg: &KeeperConfig, daa: u64) -> u64 {
        cfg.subsidy_budget.saturating_sub(self.spent(cfg, daa))
    }

    /// Records `sompi` of subsidy at `daa` and forgets the charges that left the window.
    pub fn charge(&mut self, cfg: &KeeperConfig, daa: u64, sompi: u64) {
        let w = cfg.subsidy_window_daa.max(1);
        while self.spends.front().is_some_and(|(d, _)| d.saturating_add(w) <= daa) {
            self.spends.pop_front();
        }
        if sompi == 0 {
            return;
        }
        match self.spends.back_mut() {
            Some((d, s)) if *d == daa => *s = s.saturating_add(sompi),
            _ => self.spends.push_back((daa, sompi)),
        }
    }
}

/// Cheap lower bound of what a refund of `o` costs in fees: the order input and its payout output at the standard mass floor
/// (twice the size), without a funding input (a keeper builds a refund without one when the funded one does not pay). A
/// refund tip below it can never be profitable, so the transaction is not even built.
fn refund_fee_floor(o: &ListedOrder, cfg: &KeeperConfig) -> i64 {
    use crate::matcher::candidate::{redeem_len, INPUT_BYTES, OUTPUT_BYTES};
    let bytes = INPUT_BYTES + redeem_len(o.order.state.template_id()) + OUTPUT_BYTES;
    (cfg.fee_rate.saturating_mul(2 * bytes)).min(i64::MAX as u64) as i64
}

fn typed_order(o: &ListedOrder) -> OrderUtxo<AnyState> {
    o.order.clone()
}

/// The refund job of an order due at `daa` (None when not due).
pub fn refund_job(o: &ListedOrder, daa: u64) -> Option<(JobKind, u64)> {
    let udaa = o.utxo_daa();
    // the refund time adds to chain-supplied fields; a state the numeric gate refuses has none (the tick sorts
    // every order by it before the gate)
    if udaa == UNACCEPTED_DAA || udaa > i64::MAX as u64 || crate::sanity::check(&o.order.state).is_err() {
        return None;
    }
    let due = o.order.state.refund_due(udaa as i64)?;
    if due < 0 || (daa as i64) < due {
        return None;
    }
    // an IOC / FOK leaves at the priority rate whatever ends it: the kill time, or its own expiry before that (the wallets give an
    // IOC / FOK `expiryDaa` = start + 300 DAA, before the covenant kill time start + 600, so its end is always the expiry; TN10
    // 10-07..10-09: every IOC / FOK end went out as a normal-rate refund and waited up to 106 minutes in a flooded mempool)
    let kind = match &o.base_state() {
        AnyState::KobIfdAsk(s) if s.amount_left == 0 => JobKind::Close,
        AnyState::KobAsk(s) if s.tif != TIF_GTC => JobKind::Kill,
        AnyState::KobBid(s) if s.tif != TIF_GTC => JobKind::Kill,
        // a pair order is refunded (an IOC / FOK one killed) like an ask: its custodies back to the maker (a sell-first
        // entry's A custody and B prefund at their own indices), never its strays (strays of either token move only with
        // the maker's cancel or a sweep)
        AnyState::KobPair(s) if s.tif != TIF_GTC => JobKind::Kill,
        _ => JobKind::Refund,
    };
    Some((kind, due as u64))
}

/// A sweep IN PLACE of one of the operator's own orders: the order continues unchanged (same state, covenant id and custody;
/// a `SWEEP` record keeps it listed) and its strays go back to the operator, the maker. Strays of the order's token (a pair
/// order's: either token) that share one extension commitment, and its proven foreign strays, each within the program's
/// token inputs; whatever does not fit is swept by a later tick. Only a plain ask may pay from its carrier: every other kind
/// needs the funding.
fn sweep_request(o: &ListedOrder, funding: Option<&KeyUtxo>, fee: &FeeOptions) -> Option<SweepOrder> {
    let carrier_pays = matches!(o.order.state, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
    if funding.is_none() && !carrier_pays {
        return None;
    }
    // token inputs one transfer of that token may carry (its program's; 4, the smallest, when unknown)
    let slots = |tok: [u8; 32]| -> usize {
        let hash = if tok == o.order.state.token_cov_id() {
            o.order.state.token_tpl_hash()
        } else {
            o.order.state.pair_tokens().filter(|x| x.b.cov_id == tok).map(|x| x.b.tpl_hash)
        };
        hash.and_then(|h| kob_protocol::artifacts::token_template_by_hash(&h)).map(|t| t.slots.0).unwrap_or(4)
    };
    let mut strays: Vec<TokenUtxo> = vec![];
    for tok in o.strays.iter().filter_map(|s| s.utxo.covenant_id).collect::<BTreeSet<_>>() {
        let group: Vec<&TokenUtxo> = o.strays.iter().filter(|s| s.utxo.covenant_id == Some(tok) && s.state.is_plain()).collect();
        let Some(first) = group.first() else { continue };
        let ext = first.state.extension();
        let cap = slots(tok);
        strays.extend(group.into_iter().filter(|s| s.state.extension() == ext).take(cap).cloned());
    }
    let foreign = returnable_foreign(o);
    if strays.is_empty() && foreign.is_empty() {
        return None;
    }
    Some(SweepOrder {
        order: typed_order(o),
        strays,
        foreign,
        funding: funding.cloned().into_iter().collect(),
        change: None,
        token_carrier: None,
        lock_time: 0,
        records: vec![],
        fee: fee.clone(),
    })
}

/// Builds, signs and validates a job. The budget table is exact over every generated shape; a budget
/// one unit short is retried once with one unit of slack as a logged, counted safety net
/// ([`crate::matcher::lower::budget_slack_retries`], 0 in every test).
fn finish(
    action: Action,
    signer: &dyn Signer,
    validate: bool,
    funding: &[KeyUtxo],
) -> Result<(BuiltTx, SignedTx, Option<Validation>, i64), String> {
    match finish_with(&action, signer, validate, funding, 0) {
        Err(e) if e.contains("ExceededCommittedScriptUnits") => {
            crate::matcher::lower::note_budget_slack("keeper job", &e);
            finish_with(&action, signer, validate, funding, 1)
        }
        r => r,
    }
}

fn finish_with(
    action: &Action,
    signer: &dyn Signer,
    validate: bool,
    funding: &[KeyUtxo],
    slack: u16,
) -> Result<(BuiltTx, SignedTx, Option<Validation>, i64), String> {
    let with_slack = move |role: &str| budgets(role).map(|b| b.saturating_add(slack));
    let built = build_with(action, &with_slack).map_err(|e| e.to_string())?;
    let sigs = signer.sign(&built)?;
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    let v = if validate { Some(validate_signed(&signed).map_err(|e| format!("engine rejected: {e}"))?) } else { None };
    let change = built.fee.change_output.map(|i| built.tx.outputs[i as usize].value).unwrap_or(0);
    let f: u64 = funding.iter().map(|k| k.utxo.amount).sum();
    Ok((built, signed, v, change as i64 - f as i64))
}

/// The fee rates a job may be built at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JobRate {
    /// What the job's own tip pays ([`job_rate`]).
    plain: u64,
    /// A higher rate the operator's subsidy reaches ([`KeeperConfig::subsidy_max`]): tried first, `plain` when it does not
    /// build or costs more than the subsidy.
    subsidised: Option<u64>,
}

/// The least profit a subsidised job may make: the operator pays at most `cap` (`subsidy_max` within what the budget still
/// allows) of the fee out of its own funding.
fn subsidy_floor(cap: u64) -> i64 {
    -(cap.min(i64::MAX as u64) as i64)
}

/// The fee rate a job is built at (`crate::fee`): its urgency's rate, held to the total cap and, for a paid job, to the
/// highest rate its tip still pays (at least the floor). Priced from an unsigned build at the floor and checked with one at
/// the rate found (none while the urgency's rate is the floor); a job that does not build or pay there goes at the floor.
///
/// A kill or refund with funding whose tip pays the job at the floor but not at its urgency's rate may also get a subsidised
/// rate ([`KeeperConfig::subsidy_max`]): the highest rate up to its urgency's (within the total cap) at which the operator's
/// funding adds at most `cap` (`subsidy_max` within the budget left) to the tip. An order whose tip does not pay even the floor is never subsidised (a maker
/// cannot make the operator pay for its order's end).
fn job_rate(action: &Action, kind: JobKind, cfg: &KeeperConfig, funding: &[KeyUtxo], cap: u64) -> JobRate {
    let floor = cfg.fees.floor_of(cfg.fee_rate);
    let want = cfg.fees.rate(kind.urgency(), cfg.fee_rate);
    let flat = JobRate { plain: floor, subsidised: None };
    if want <= floor {
        return flat;
    }
    let funded: u64 = funding.iter().map(|k| k.utxo.amount).sum();
    let paid = kind != JobKind::Sweep;
    let profit_at = |a: &Action| -> Option<(u64, i64)> {
        let built = build_with(a, &budgets).ok()?;
        let change = built.fee.change_output.map(|i| built.tx.outputs[i as usize].value).unwrap_or(0);
        Some((built.fee.fee, change as i64 - funded as i64))
    };
    let fits = |fee: u64| cfg.fees.max_tx_fee == 0 || fee <= cfg.fees.max_tx_fee;
    let mut a = action.clone();
    crate::fee::set_rate(&mut a, floor);
    let Some((fee0, profit0)) = profit_at(&a) else { return flat };
    let rate = cfg.fees.priced(want, floor, fee0, paid.then_some(profit0), cfg.min_profit);
    let plain = if rate <= floor {
        floor
    } else {
        crate::fee::set_rate(&mut a, rate);
        match profit_at(&a) {
            Some((fee, profit)) if (!paid || profit >= cfg.min_profit) && fits(fee) => rate,
            _ => floor,
        }
    };
    let subsidised = (kind.liveness() && cap > 0 && !funding.is_empty() && profit0 >= cfg.min_profit)
        .then(|| cfg.fees.priced(want, floor, fee0, Some(profit0), subsidy_floor(cap).min(cfg.min_profit)))
        .filter(|r| *r > plain);
    JobRate { plain, subsidised }
}

/// A kill or refund the runner rebuilds at `rate` to replace its stuck transaction (`submitTransactionReplacement`): the same
/// request (the same order inputs, lock time and funding), plus `extra` funding when the request has none and the operator
/// subsidises (the tip alone may not pay the higher rate). The rate is lowered to the total cap; a build that then pays no
/// more than `above` (the stuck transaction's rate: a replacement must pay a higher one), costs the operator more than
/// [`KeeperConfig::subsidy_max`] (or, without a subsidy, leaves less than `min_profit`), or does not build or validate is an
/// error. The maker's outputs are the covenant's whatever the rate. The runner lowers `subsidy_max` to the job's subsidy so
/// far plus what the rolling budget still allows ([`SubsidyBudget`]); 0 then means the job's own tip alone pays.
pub fn reprice(
    kind: JobKind,
    order: CovId,
    action: &Action,
    rate: u64,
    above: u64,
    extra: Option<&KeyUtxo>,
    cfg: &KeeperConfig,
    signer: &dyn Signer,
) -> Result<Job, String> {
    if !kind.liveness() {
        return Err(format!("a {} is never replaced", kind.name()));
    }
    let mut action = action.clone();
    if let (Action::RefundOrder(r), Some(f)) = (&mut action, extra) {
        if r.funding.is_empty() && cfg.subsidy_max > 0 {
            r.funding.push(f.clone());
            r.change = r.change.or(Some(signer.pubkey()));
        }
    }
    let funding: Vec<KeyUtxo> = match &action {
        Action::RefundOrder(r) => r.funding.clone(),
        _ => return Err("not a refund request".into()),
    };
    let min_ok = if cfg.subsidy_max > 0 { subsidy_floor(cfg.subsidy_max).min(cfg.min_profit) } else { cfg.min_profit };
    let floor = cfg.fees.floor_of(cfg.fee_rate);
    let mut rate = rate.max(floor);
    for _ in 0..2 {
        if rate <= above {
            return Err(format!("no rate above {above} fits the fee cap"));
        }
        crate::fee::set_rate(&mut action, rate);
        let (built, signed, validation, profit) = finish(action.clone(), signer, cfg.validate, &funding)?;
        if let Some(r) = cfg.fees.capped(rate, built.fee.fee, floor) {
            rate = r;
            continue;
        }
        if cfg.fees.max_tx_fee > 0 && built.fee.fee > cfg.fees.max_tx_fee {
            return Err(format!("fee {} above the cap {}", built.fee.fee, cfg.fees.max_tx_fee));
        }
        if profit < min_ok {
            return Err(format!("the fee at {rate} sompi/g costs the operator {} (most {})", -profit, -min_ok));
        }
        let spends = spends_of(&built);
        let subsidy = if profit < 0 { profit.unsigned_abs() } else { 0 };
        return Ok(Job { kind, order, action, built, signed, validation, profit, subsidy, spends });
    }
    Err(format!("no rate above {above} fits the fee cap"))
}

/// The foreign strays of an order one transaction can move: per token, the plain UTXOs that share the first one's extension
/// commitment, within the program's token inputs (the rest wait for a later transaction).
fn returnable_foreign(o: &ListedOrder) -> Vec<ForeignStrays> {
    o.foreign
        .iter()
        .filter_map(|g| {
            let cap = g.token.program.token_slots()?.0;
            let first = g.utxos.iter().find(|s| s.state.is_plain())?;
            let ext = first.state.extension();
            let utxos: Vec<TokenUtxo> =
                g.utxos.iter().filter(|s| s.state.is_plain() && s.state.extension() == ext).take(cap).cloned().collect();
            Some(ForeignStrays { token: g.token.clone(), utxos })
        })
        .collect()
}

/// Drops the foreign strays a keeper refund would return. `false` when it returns none.
fn strip_foreign(a: &mut Action) -> bool {
    match a {
        Action::RefundOrder(r) if !r.foreign.is_empty() => {
            r.foreign.clear();
            true
        }
        _ => false,
    }
}

/// Drops the funding inputs of a keeper job (a refund then pays its fee from the order's tip alone). `false` when the job has
/// no funding to drop.
fn strip_funding(a: &mut Action) -> bool {
    match a {
        Action::RefundOrder(r) if !r.funding.is_empty() => {
            r.funding.clear();
            true
        }
        _ => false,
    }
}

fn spends_of(built: &BuiltTx) -> BTreeSet<Outpoint> {
    built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect()
}

/// The weighted selection of a keeper tick (`docs/spec/matcher.md` §3.6): among the kills, refunds and closes due at most
/// `max_defer_daa` ago (a job due longer is built by every keeper: the starvation bound), `share` of them in expectation,
/// drawn without replacement with probability proportional to `(tip per byte)^alpha` (Kaspa's template selection,
/// [`crate::matcher::select`]). Returns the orders whose jobs wait for the next tick.
fn select_jobs(orders: &[&ListedOrder], inp: &KeeperInput, cfg: &KeeperConfig, report: &mut KeeperReport) -> BTreeSet<CovId> {
    use crate::matcher::candidate::{redeem_len, INPUT_BYTES, OUTPUT_BYTES};
    let Some(sel) = cfg.select.filter(|s| s.samples()) else { return BTreeSet::new() };
    if !cfg.refund {
        return BTreeSet::new();
    }
    let daa = inp.clock.daa;
    let mut optional: Vec<(CovId, f64)> = vec![];
    for o in orders {
        let id = o.id();
        if inp.excluded.contains(&id) || inp.excluded_outpoints.contains(&outpoint(&o.order.utxo)) {
            continue;
        }
        let Some((_, due)) = refund_job(o, daa) else { continue };
        // a job the tick would not build anyway (its tip does not pay, found unprofitable before) takes no place in the
        // sample
        let tip = crate::model::terms_of(&o.order.state).refund_tip;
        if inp.known_unprofitable.contains(&(id, outpoint(&o.order.utxo)))
            || tip.saturating_sub(refund_fee_floor(o, cfg)) < cfg.min_profit
        {
            continue;
        }
        if daa >= due.saturating_add(sel.max_defer_daa) {
            report.overdue += 1;
            continue;
        }
        let bytes = INPUT_BYTES + redeem_len(o.order.state.template_id()) + OUTPUT_BYTES;
        optional.push((id, crate::matcher::select::rate(tip.saturating_sub(refund_fee_floor(o, cfg)), bytes)));
    }
    let mut rng = sel.rng();
    let k = crate::matcher::select::take_count(sel.share(), optional.len(), &mut rng);
    let rates: Vec<f64> = optional.iter().map(|x| x.1).collect();
    let picks: BTreeSet<usize> =
        crate::matcher::select::sample(&crate::matcher::select::weights(&rates, sel.alpha()), k, &mut rng).into_iter().collect();
    report.sampled = optional.len();
    report.taken = picks.len();
    let out: BTreeSet<CovId> = optional.iter().enumerate().filter(|(i, _)| !picks.contains(i)).map(|(_, (id, _))| *id).collect();
    report.deferred = out.iter().copied().collect();
    out
}

/// Plans, builds, signs and validates every keeper job of a snapshot. Funding is chained through
/// each job's change output (a P2PK spend never depends on its parent's DAA score). The tick's subsidies are bounded by one
/// window's budget and the per-step job cap ([`SubsidyAllowance::of`]); the runner uses [`tick_with_subsidy`] with what its
/// rolling budget still allows.
pub fn tick(inp: &KeeperInput, cfg: &KeeperConfig, signer: &dyn Signer) -> KeeperReport {
    tick_with_subsidy(inp, cfg, signer, SubsidyAllowance::of(cfg))
}

/// [`tick`] with an explicit subsidy allowance: the subsidies of the tick's jobs together stay within `allowance.sompi`
/// and at most `allowance.jobs` jobs are subsidised; past either, a job goes at the rate its own tip pays.
pub fn tick_with_subsidy(inp: &KeeperInput, cfg: &KeeperConfig, signer: &dyn Signer, allowance: SubsidyAllowance) -> KeeperReport {
    let mut allowance = allowance;
    let mut report = KeeperReport::default();
    let operator = signer.pubkey();
    let daa = inp.clock.daa;
    let fee = FeeOptions::rate(cfg.fee_rate);
    let mut pool: VecDeque<KeyUtxo> = inp.funding.iter().cloned().collect();
    let mut attempts = 0usize;
    let mut orders: Vec<&ListedOrder> = inp.orders.iter().collect();
    // Deterministic: kills first (the first block after the kill time), then by refund time / id.
    orders.sort_by_key(|o| match refund_job(o, daa) {
        Some((k, due)) => (0u8, k, due, o.id()),
        None => (1u8, JobKind::Sweep, 0, o.id()),
    });
    let deferred = select_jobs(&orders, inp, cfg, &mut report);
    for o in orders {
        if report.jobs.len() >= cfg.max_jobs {
            break;
        }
        let id = o.id();
        if inp.excluded.contains(&id) || inp.excluded_outpoints.contains(&outpoint(&o.order.utxo)) || deferred.contains(&id) {
            continue;
        }
        // numbers the builders cannot survive are never built from
        if let Err(why) = crate::sanity::check(&o.order.state) {
            report.skipped.push((id, format!("not serviced: {why}")));
            continue;
        }
        if attempts >= cfg.max_attempts {
            report.skipped.push((id, "keeper attempt budget of this tick used up".into()));
            break;
        }
        if (!o.strays.is_empty() || !o.foreign.is_empty()) && o.order.state.maker() != operator {
            report.foreign_strays.push(id);
        }
        let funding = pool.front().cloned();
        let funding_vec: Vec<KeyUtxo> = funding.iter().cloned().collect();
        let mut attempt: Option<(JobKind, Action)> = None;
        let known = |o: &ListedOrder| inp.known_unprofitable.contains(&(o.id(), outpoint(&o.order.utxo)));
        if cfg.refund {
            if let Some((kind, due)) = refund_job(o, daa) {
                // a refund that pays the keeper less than the fee floor is never worth building
                let tip = crate::model::terms_of(&o.order.state).refund_tip;
                if known(o) || tip.saturating_sub(refund_fee_floor(o, cfg)) < cfg.min_profit {
                    report.skipped.push((id, format!("{}: tip {tip} does not cover the fee", kind.name())));
                    continue;
                }
                // the tip is paid out of the order UTXO: a tip above its value can never be built (the maker's cancel still
                // works). Not worth an attempt; the runner remembers it.
                if tip > i64::try_from(o.order.utxo.amount).unwrap_or(i64::MAX) {
                    report.skipped.push((id, format!("{}: refund tip {tip} exceeds the order's value", kind.name())));
                    report.failed.push((id, outpoint(&o.order.utxo)));
                    continue;
                }
                if !o.custody_ok() && o.order.state.custody_amount().unwrap_or(0) != 0 {
                    // reported, not silently skipped: only the maker's cancel (or a later custody) ends it
                    report
                        .skipped
                        .push((id, format!("{}: the custody is missing or does not hold the order's amountLeft", kind.name())));
                } else {
                    attempt = Some((
                        kind,
                        Action::RefundOrder(RefundOrder {
                            order: typed_order(o),
                            foreign: if cfg.return_foreign_strays { returnable_foreign(o) } else { vec![] },
                            custody: o.custody.clone().filter(|_| o.order.state.custody_amount().unwrap_or(0) > 0),
                            prefund: o.custody_b.clone(),
                            lock_time: due,
                            funding: funding_vec.clone(),
                            change: Some(operator),
                            fee: fee.clone(),
                        }),
                    ));
                }
            }
        }
        if attempt.is_none()
            && cfg.sweep_own_strays
            && o.order.state.maker() == operator
            && (!o.strays.is_empty() || !o.foreign.is_empty())
        {
            if let Some(req) = sweep_request(o, funding.as_ref(), &fee) {
                attempt = Some((JobKind::Sweep, Action::SweepOrder(req)));
            }
        }
        let Some((kind, mut action)) = attempt else { continue };
        attempts += 1;
        let cap = allowance.per_job(cfg);
        let rates = job_rate(&action, kind, cfg, &funding_vec, cap);
        // the subsidised rate first: kept when it builds within the total cap and costs the operator at most `cap`
        let subsidised = rates.subsidised.and_then(|rate| {
            let mut a = action.clone();
            crate::fee::set_rate(&mut a, rate);
            let r = finish(a.clone(), signer, cfg.validate, &funding_vec).ok()?;
            let fits = cfg.fees.max_tx_fee == 0 || r.0.fee.fee <= cfg.fees.max_tx_fee;
            (fits && r.3 >= subsidy_floor(cap)).then_some((a, r))
        });
        if let Some((a, (built, signed, validation, profit))) = subsidised {
            pool.pop_front();
            if let Some(i) = built.fee.change_output {
                let v = built.tx.outputs[i as usize].value;
                let u = kob_protocol::tx::Utxo {
                    transaction_id: built.tx.id,
                    index: i,
                    amount: v,
                    block_daa_score: UNACCEPTED_DAA,
                    covenant_id: None,
                };
                pool.push_back(KeyUtxo { utxo: u, pubkey: operator });
            }
            let spends = spends_of(&built);
            let subsidy = if profit < 0 { profit.unsigned_abs() } else { 0 };
            allowance.take(subsidy);
            report.jobs.push(Job { kind, order: id, action: a, built, signed, validation, profit, subsidy, spends });
            continue;
        }
        crate::fee::set_rate(&mut action, rates.plain);
        let mut funding = funding;
        let mut result = finish(action.clone(), signer, cfg.validate, &funding_vec);
        // A paid job that loses money only because of its funding input (its mass, a change output) or fails or loses money
        // only because of the foreign strays it returns is rebuilt without them: the order's own tip then pays the fee and
        // the keeper keeps what is left (C5 K-7). Only a job that loses money every way is remembered as unprofitable.
        let poor = |r: &Result<(BuiltTx, SignedTx, Option<Validation>, i64), String>| match r {
            Ok((.., p)) => *p < cfg.min_profit,
            Err(_) => true,
        };
        if kind != JobKind::Sweep && poor(&result) {
            for (no_funding, no_foreign) in [(true, false), (false, true), (true, true)] {
                let mut alt = action.clone();
                if (no_funding && !strip_funding(&mut alt)) || (no_foreign && !strip_foreign(&mut alt)) {
                    continue;
                }
                let fv: &[KeyUtxo] = if no_funding { &[] } else { &funding_vec };
                if let Ok(r) = finish(alt.clone(), signer, cfg.validate, fv) {
                    if r.3 >= cfg.min_profit {
                        action = alt;
                        if no_funding {
                            funding = None;
                        }
                        result = Ok(r);
                        break;
                    }
                }
            }
        }
        match result {
            Ok((built, signed, validation, profit)) => {
                if kind != JobKind::Sweep && profit < cfg.min_profit {
                    report.skipped.push((id, format!("{}: tip does not cover the fee ({profit})", kind.name())));
                    report.unprofitable.push((id, outpoint(&o.order.utxo)));
                    continue;
                }
                if funding.is_some() {
                    pool.pop_front();
                    if let Some(i) = built.fee.change_output {
                        let v = built.tx.outputs[i as usize].value;
                        let u = kob_protocol::tx::Utxo {
                            transaction_id: built.tx.id,
                            index: i,
                            amount: v,
                            block_daa_score: UNACCEPTED_DAA,
                            covenant_id: None,
                        };
                        pool.push_back(KeyUtxo { utxo: u, pubkey: operator });
                    }
                }
                let spends = spends_of(&built);
                report.jobs.push(Job { kind, order: id, action, built, signed, validation, profit, subsidy: 0, spends });
            }
            Err(e) => {
                report.skipped.push((id, format!("{}: {e}", kind.name())));
                report.failed.push((id, outpoint(&o.order.utxo)));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(budget: u64, window: u64) -> KeeperConfig {
        KeeperConfig { subsidy_max: 50, subsidy_budget: budget, subsidy_window_daa: window, ..KeeperConfig::default() }
    }

    #[test]
    fn the_budget_rolls_with_the_window() {
        let c = cfg(100, 1_000);
        let mut b = SubsidyBudget::default();
        assert_eq!(b.left(&c, 0), 100);
        b.charge(&c, 10, 40);
        b.charge(&c, 500, 50);
        assert_eq!(b.left(&c, 500), 10);
        // the first charge leaves the window 1,000 DAA after it was made, the second one later
        assert_eq!(b.left(&c, 1_009), 10);
        assert_eq!(b.left(&c, 1_010), 50);
        assert_eq!(b.left(&c, 1_500), 100);
        // over-charging (a replacement without a verdict) saturates at 0
        b.charge(&c, 1_500, 500);
        assert_eq!(b.left(&c, 1_500), 0);
        // charges of one DAA score are coalesced, old ones forgotten
        b.charge(&c, 1_500, 1);
        assert_eq!(b.spends.len(), 1);
        // a node behind the last charge still sees it (no subsidy re-granted by a DAA score going backwards)
        assert_eq!(b.left(&c, 100), 0);
    }

    #[test]
    fn a_zero_budget_or_job_cap_allows_no_subsidy() {
        let mut a = SubsidyAllowance::of(&cfg(0, 1_000));
        assert_eq!(a.per_job(&cfg(0, 1_000)), 0);
        let c = KeeperConfig { subsidy_max_jobs: 0, ..cfg(100, 1_000) };
        assert_eq!(SubsidyAllowance::of(&c).per_job(&c), 0);
        // the per-job cap is subsidy_max within what is left, and every subsidised job takes one of the cap
        let c = KeeperConfig { subsidy_max_jobs: 2, ..cfg(80, 1_000) };
        a = SubsidyAllowance::of(&c);
        assert_eq!(a.per_job(&c), 50);
        a.take(45);
        assert_eq!(a.per_job(&c), 35);
        a.take(0);
        assert_eq!(a.jobs, 1, "a job its tip pays takes nothing");
        a.take(10);
        assert_eq!((a.per_job(&c), a.sompi), (0, 25));
    }

    #[test]
    fn the_liveness_options_set_the_budget() {
        let args = LivenessArgs {
            keeper_subsidy_max_kas: 0.05,
            keeper_subsidy_budget_kas: 1.0,
            keeper_subsidy_window_daa: 36_000,
            keeper_subsidy_max_jobs: 8,
            keeper_replace_after_daa: 100,
            keeper_max_replacements: 3,
        };
        let mut c = KeeperConfig::default();
        args.apply(&mut c).unwrap();
        assert_eq!((c.subsidy_max, c.subsidy_budget, c.subsidy_window_daa, c.subsidy_max_jobs), (5_000_000, 100_000_000, 36_000, 8));
        let d = KeeperConfig::default();
        assert_eq!((d.subsidy_budget, d.subsidy_window_daa, d.subsidy_max_jobs), (c.subsidy_budget, c.subsidy_window_daa, 8));
        assert!(LivenessArgs { keeper_subsidy_budget_kas: -1.0, ..args.clone() }.apply(&mut c).is_err());
        assert!(LivenessArgs { keeper_subsidy_budget_kas: f64::NAN, ..args.clone() }.apply(&mut c).is_err());
        assert!(LivenessArgs { keeper_subsidy_window_daa: 0, ..args.clone() }.apply(&mut c).is_err());
        LivenessArgs { keeper_subsidy_budget_kas: 0.0, ..args }.apply(&mut c).unwrap();
        assert_eq!(c.subsidy_budget, 0);
    }
}
