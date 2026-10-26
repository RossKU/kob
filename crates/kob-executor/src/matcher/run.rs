//! The `kob-executor match` / `keep` loop.
//!
//! One [`Runner::step`] per tick (default 1 s, `docs/spec/matcher.md` §3):
//!
//! 1. node: synced? virtual DAA score; acceptance of our transactions from the VSPC v2 cursor
//!    (reorgs roll back to pending, final after `final_depth` DAA);
//! 2. book: a fresh snapshot from the [`BookSource`] (the indexer's [`OrderBookView`]);
//! 3. funding: the operator's P2PK UTXOs from the node, minus everything pending transactions spend;
//! 4. matcher tick (plans, chains, engine-validated transactions) and keeper tick;
//! 5. submission through the [`Tracker`]: benign double spends back off, nothing is replaced
//!    blindly, chained steps are only sent after their parent was accepted into the mempool; a kill or refund
//!    of this process that stays unaccepted is rebuilt at a higher rate and replaced ([`Runner::replace_stuck`]).
//!
//! The kill switch is a file (`--pause-file`): while it exists nothing is built or submitted,
//! acceptance tracking continues.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kob_protocol::tx::{KeyUtxo, Utxo};

use super::book::{outpoint, utc_now, Clock, CovId, ListedOrder, MemoryBook, OrderBookView};
use super::engine::{tick as matcher_tick, EngineConfig, TickInput};
use super::family::Families;
use super::node::{NodeApi, RpcError, SubmitOutcome};
use super::select::{Role, SelectConfig, Selector};
use super::submit::{Tracked, Tracker, TrackerConfig};
use super::wallet::{address_of, p2pk_spk_string, Signer};
use crate::keepers::{tick_with_subsidy as keeper_tick, JobKind, KeeperConfig, KeeperInput, SubsidyAllowance, SubsidyBudget};

/// A read of the book that blocks on a database: the runner runs it on the blocking thread pool, never on a runtime worker.
pub type BookRead = Box<dyn FnOnce() -> anyhow::Result<MemoryBook> + Send>;

/// Where the book comes from each tick.
pub trait BookSource: Send {
    /// A consistent snapshot of the listed orders and the operator's tokens.
    fn snapshot(&mut self, operator: &[u8; 32]) -> anyhow::Result<MemoryBook>;

    /// [`BookSource::snapshot`] as a read the runner runs on the blocking thread pool (a source that reads a database:
    /// the indexer's store). `None`, the default: the runner calls `snapshot` in place (a file, a memory book).
    fn snapshot_read(&mut self, _operator: &[u8; 32]) -> Option<BookRead> {
        None
    }

    /// `Err(why)` while the source lags the chain (the indexer after a restart, catching up, or cut off from the node): the
    /// runner plans nothing (matcher, keepers, maintenance) and keeps tracking acceptance. The default: always ready.
    fn ready(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called with the id of a transaction just before it is submitted, so a source that follows the
    /// chain itself (the indexer) sees its acceptance even if a block arrives within milliseconds.
    fn watch(&mut self, _txid: [u8; 32]) {}

    /// Where the selected chain accepts each of `tracked` now (transaction id to accepting chain block
    /// and its DAA score; absent: not accepted), when the source follows the chain itself. `None`: the
    /// source does not, and the runner follows acceptance through the node (`getVirtualChainFromBlockV2`).
    /// In-process the indexer answers, so there is exactly one VSPC follower and one truth about which
    /// transactions are accepted: the one the book was built from.
    fn acceptance(&mut self, _tracked: &[[u8; 32]]) -> Option<BTreeMap<[u8; 32], (String, u64)>> {
        None
    }

    /// Live orders the indexer proved but does not LIST (a token outside the allowlist, an exit of an unlisted entry, a carrier
    /// below the listing minimum, ...): never matched, but the keepers kill, refund and close them when that pays (their
    /// makers would otherwise wait for nobody; the maker's own cancel works either way). Besides the snapshot's own
    /// `keeper_only` (the indexer's snapshot and a `--book-file` carry them there). The default: none.
    fn keeper_only_orders(&mut self) -> Vec<ListedOrder> {
        vec![]
    }

    /// The operator's own key-owned token UTXOs (accepted, with their programs) for the maintenance jobs
    /// ([`crate::maintenance`]). The default: none (no maintenance).
    fn operator_tokens(&mut self, _operator: &[u8; 32]) -> Vec<crate::maintenance::OwnToken> {
        vec![]
    }

    /// The tick's outcome for orders a token program may refuse to move: `flagged` (id, reason) failed their engine
    /// pre-simulation at the token program, `cleared` passed it. A source that stores the flag ("possibly frozen") keeps it
    /// off the books; the default ignores both.
    fn report_frozen(&mut self, _daa: u64, _flagged: &[(CovId, String)], _cleared: &[CovId]) {}

    /// Extra Prometheus lines for the metrics file (the in-process indexer's follower state). The default: none.
    fn metrics(&mut self) -> String {
        String::new()
    }

    /// The book's part of the console status line ([`status_line`]; the in-process indexer's state and lag). The default:
    /// none.
    fn status(&mut self) -> Option<String> {
        None
    }
}

/// How long the step in flight may go on once the runner was asked to stop ([`Runner::run`]).
pub const STEP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// DAA scores (10 per second) an order whose planning panicked, or that the builder refused on its own, stays out of the
/// book: one hour.
pub const QUARANTINE_DAA: u64 = 36_000;

/// DAA scores (10 per second) before an order flagged possibly frozen is probed again: 5 minutes.
pub const FROZEN_RECHECK_DAA: u64 = 3_000;

/// DAA scores (10 per second) a keeper job that failed to build stays out of the keepers' queue the first time: 1 minute,
/// doubling with every further failure of the same order outpoint up to [`KEEPER_FAILED_MAX_DAA`].
pub const KEEPER_FAILED_RECHECK_DAA: u64 = 600;

/// Longest keeper backoff of an order outpoint whose jobs keep failing: one hour.
pub const KEEPER_FAILED_MAX_DAA: u64 = 36_000;

/// Any [`OrderBookView`] (the indexer in-process) is a book source.
pub struct ViewSource<V: OrderBookView + Send>(pub V);

impl<V: OrderBookView + Send> BookSource for ViewSource<V> {
    fn snapshot(&mut self, operator: &[u8; 32]) -> anyhow::Result<MemoryBook> {
        Ok(MemoryBook {
            daa_score: self.0.daa_score(),
            orders: self.0.orders(),
            wallet_tokens: self.0.wallet_tokens(operator),
            keeper_only: vec![],
        })
    }
}

/// A JSON snapshot file ([`MemoryBook`]), re-read when it changes (written by an indexer export).
pub struct FileSource {
    pub path: PathBuf,
    cached: Option<(SystemTime, MemoryBook)>,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileSource { path: path.into(), cached: None }
    }
}

/// Reads a snapshot file.
pub fn read_snapshot(path: &Path) -> anyhow::Result<MemoryBook> {
    let s = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

impl BookSource for FileSource {
    fn snapshot(&mut self, _operator: &[u8; 32]) -> anyhow::Result<MemoryBook> {
        let m = std::fs::metadata(&self.path)?.modified()?;
        if let Some((t, b)) = &self.cached {
            if *t == m {
                return Ok(b.clone());
            }
        }
        let b = read_snapshot(&self.path)?;
        self.cached = Some((m, b.clone()));
        Ok(b)
    }
}

/// Roles of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles {
    pub matcher: bool,
    pub keeper: bool,
}

/// Loop settings.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub network: String,
    pub roles: Roles,
    pub engine: EngineConfig,
    pub keeper: KeeperConfig,
    /// The operator's own tokens: merge and sell (`crate::maintenance`), with the matcher role.
    pub maintenance: crate::maintenance::MaintenanceConfig,
    pub tracker: TrackerConfig,
    /// Build and validate, never submit.
    pub dry_run: bool,
    pub tick: Duration,
    /// Kill switch: while this file exists nothing is built or submitted.
    pub pause_file: Option<PathBuf>,
    /// Prometheus textfile-collector output (`*.prom`).
    pub metrics_file: Option<PathBuf>,
    /// Where the surplus-inventory policy's use of its period is kept (`maxFee`, `tokens[].maxAmount`), so a restart does
    /// not start the budgets again. `None`: in memory only.
    pub inventory_state: Option<PathBuf>,
    /// Alert threshold: funding below this many sompi is reported (`kob_operator_low_funds`).
    pub low_funds: u64,
    /// Plan nothing while the book is more than this many DAA behind the node (a lagging indexer would
    /// offer spent orders). Acceptance tracking continues. `None`: no check (snapshot files).
    pub max_book_lag: Option<u64>,
    /// Ask the node which listed orders it still holds ([`gone_orders`]) only while the book is at least this many DAA behind
    /// the node (10 per second): the query costs a node round trip every tick, which loses races to other executors, and it
    /// only matters while the book can list orders the chain has already spent. 0: ask every step.
    pub verify_live_min_lag: u64,
    /// The fee policy (`crate::fee`): each step that plans reads the node's fee estimate (at most every `refresh_ms`) and
    /// sets the tick's rates of the matcher, the keepers and the maintenance jobs. Their configured `fee_rate`s stay the
    /// lowest rate each pays.
    pub fee_policy: crate::fee::FeePolicy,
    /// Weighted work selection among competing executors (`docs/spec/matcher.md` §3.6, [`super::select`]). Off by default
    /// in the library (the reference planner); the live commands turn it on.
    pub select: SelectConfig,
    /// Log a one-line status ([`status_line`]) this often (`--status-secs`); `None`: never.
    pub status_every: Option<Duration>,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            // tests and embedders only: every command sets the network it resolved (`crate::config::resolve_network`)
            network: "testnet-10".into(),
            roles: Roles { matcher: true, keeper: false },
            engine: EngineConfig::default(),
            keeper: KeeperConfig::default(),
            maintenance: crate::maintenance::MaintenanceConfig::default(),
            tracker: TrackerConfig::default(),
            dry_run: false,
            tick: Duration::from_secs(1),
            pause_file: None,
            metrics_file: None,
            inventory_state: None,
            low_funds: 100 * 100_000_000,
            max_book_lag: None,
            verify_live_min_lag: 100,
            fee_policy: crate::fee::FeePolicy::default(),
            select: SelectConfig::default(),
            status_every: None,
        }
    }
}

/// What one step did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepReport {
    pub daa: u64,
    pub synced: bool,
    pub paused: bool,
    /// The book was too far behind the node to plan against (`RunConfig::max_book_lag`).
    pub book_stale: bool,
    /// DAA score the book was synced to (0: no book read).
    pub book_daa: u64,
    /// The book source was not ready (`BookSource::ready`): why. Nothing was planned.
    pub not_ready: Option<String>,
    /// (kind, txid, operator profit) of every transaction built this step.
    pub built: Vec<(String, [u8; 32], i64)>,
    pub submitted: Vec<[u8; 32]>,
    pub conflicts: Vec<[u8; 32]>,
    pub rejected: Vec<([u8; 32], SubmitOutcome)>,
    /// Transactions whose submit got no verdict (tracked as pending, resent).
    pub unknown: Vec<[u8; 32]>,
    pub skipped: Vec<String>,
    /// Orders left out of the matcher's book this step (numbers refused by the sanity gate, planning panicked, or the builder
    /// refused the order).
    pub quarantined: usize,
    pub accepted: usize,
    pub finalized: usize,
    pub rolled_back: usize,
    /// The operator's spendable funding read this step (`None`: not read, nothing was planned): its P2PK UTXOs that no
    /// pending transaction or reservation holds.
    pub funding: Option<u64>,
    /// The operator's whole P2PK balance read this step, the UTXOs pending transactions hold included (they stay the
    /// operator's until spent, and their change returns): what the low-funds alert judges.
    pub funding_total: Option<u64>,
    /// Listed orders left out of this step because the node no longer holds their UTXO (spent in a block the book has not
    /// reached yet, [`gone_orders`]; always 0 while the book lags by less than `RunConfig::verify_live_min_lag`).
    pub gone: usize,
    /// Stuck kills and refunds of this process replaced at a higher rate this step: (replaced txid, new txid).
    pub replaced: Vec<([u8; 32], [u8; 32])>,
}

/// A kill or refund this process submitted and may still replace at a higher rate ([`Runner::replace_stuck`]). Only
/// transactions the node took from this process are ever recorded: nobody else's transaction is replaced.
#[derive(Clone, Debug)]
pub struct InFlightJob {
    pub kind: JobKind,
    pub order: CovId,
    /// The builder request of the submitted transaction (its rate is replaced, nothing else).
    pub action: kob_protocol::build::Action,
    /// Replacements so far.
    pub replacements: u32,
    /// DAA score of the last submission (the original or the last replacement).
    pub sent_daa: u64,
    /// The operator's subsidy of the submitted transaction (sompi).
    pub subsidy: u64,
    /// No further replacement (refused by the node, no verdict, or no higher rate fits).
    pub done: bool,
}

/// The matcher / keeper process.
pub struct Runner<N: NodeApi, S: BookSource> {
    pub node: N,
    pub source: S,
    pub signer: Box<dyn Signer>,
    pub families: Families,
    pub tracker: Tracker,
    pub cfg: RunConfig,
    pub last: StepReport,
    pub steps: u64,
    /// Orders flagged possibly frozen and the DAA score before which the matcher does not probe them again.
    pub frozen_until: BTreeMap<CovId, u64>,
    /// Orders whose planning panicked (the matcher's last line of defence) or that the builder refused on their own, and the
    /// DAA score before which they stay out of the book: one malformed order costs a skipped order, never the process or the
    /// other books.
    pub quarantined_until: BTreeMap<CovId, u64>,
    /// Refunds found unprofitable at their fixed tip, by (order, order outpoint): not built again.
    pub keeper_unprofitable: BTreeSet<(CovId, super::book::Outpoint)>,
    /// The keepers' floor fee rate when the entries of `keeper_unprofitable` were found: a lower floor forgets them (a refund
    /// that did not pay at the old floor may pay at the new one).
    pub keeper_unprofitable_floor: u64,
    /// Keeper jobs that failed to build, by order outpoint: (DAA before which it is not built again, failures so far).
    pub keeper_failed: BTreeMap<super::book::Outpoint, (u64, u32)>,
    /// Diagnostics and tests: every transaction submitted (or built, in a dry run) is also pushed here.
    /// `None` (the default) keeps nothing.
    pub capture: Option<Vec<kob_protocol::tx::SignedTx>>,
    /// Outpoints that transactions of another part of the process spend while in flight (the x402 facilitator's swap-and-pay
    /// settlements, signed by their payers over the same book): the batches plan around them like around the runner's own
    /// pending transactions.
    pub reservations: Option<Reservations>,
    /// Why the book source is not ready, while the runner waits for it (logged once per wait).
    pub waiting: Option<String>,
    /// The funding of the last step that read it (the metrics keep it while a step reads none).
    pub last_funding: Option<u64>,
    pub last_funding_total: Option<u64>,
    /// What the surplus-inventory policy used in its current period (`maxFee`, `tokens[].maxAmount`).
    pub inventory_use: super::planner::InventoryUse,
    /// The node's fee estimate between steps (`crate::fee`).
    pub fee_state: crate::fee::FeeState,
    /// The rates of the last step that planned (the floor before the first).
    pub fee_rates: crate::fee::FeeRates,
    /// The matcher's configured rate (`engine.planner.fee_rate` at start): the lowest rate a batch is planned at.
    pub plan_base_rate: u64,
    /// Where the rates are published for the rest of the process (the x402 facilitator of `kob-executor run`).
    pub fee_board: Option<crate::fee::FeeBoard>,
    /// Kills and refunds this process submitted that are not accepted yet, by txid ([`Runner::replace_stuck`]).
    pub keeper_inflight: BTreeMap<[u8; 32], InFlightJob>,
    /// Keeper jobs submitted with an operator subsidy, and the sompi of those subsidies (metrics).
    pub keeper_subsidised: u64,
    pub keeper_subsidy_sompi: u64,
    /// The operator subsidies of the rolling window (`KeeperConfig::subsidy_budget`): every subsidy of a kill or refund this
    /// process submitted, the increase of each replacement included. In memory: a restart starts with an empty window.
    pub keeper_subsidy: SubsidyBudget,
    /// Stuck kills and refunds replaced at a higher rate, and replacements the node refused (metrics).
    pub keeper_replaced: u64,
    pub keeper_replace_refused: u64,
    /// The weighted selection's random source, the share of each role and its counters (`kob_select_*`).
    pub selector: Selector,
    /// Orders the selection left for later, with the outpoint they had and the DAA score they were left at: one that another
    /// transaction takes before this process does counts as ceded (`kob_select_ceded_total`).
    pub ceded_watch: BTreeMap<CovId, (super::book::Outpoint, u64)>,
}

/// A source of reserved outpoints ([`Runner::reservations`]).
pub type Reservations = std::sync::Arc<dyn Fn() -> BTreeSet<super::book::Outpoint> + Send + Sync>;

fn rpc_json(signed: &kob_protocol::tx::SignedTx) -> anyhow::Result<serde_json::Value> {
    let (tx, _) = signed.tx.to_tx()?;
    Ok(kob_protocol::issue::rpc_transaction_json(&tx))
}

/// Orders whose current order or custody outpoint a pending transaction spends.
fn busy_orders(orders: &[ListedOrder], spent: &BTreeSet<super::book::Outpoint>) -> BTreeSet<CovId> {
    orders
        .iter()
        .filter(|o| {
            spent.contains(&outpoint(&o.order.utxo))
                || o.custody.iter().chain(o.custody_b.iter()).any(|c| spent.contains(&outpoint(&c.utxo)))
        })
        .map(|o| o.id())
        .collect()
}

/// The order each input of a matcher transaction belongs to: an order UTXO (its covenant id is the order's), and the custody
/// of a listed order the transaction spends ([`Tracked::owners`]: a mempool double spend backs off that order alone).
fn input_owners(p: &super::engine::Prepared, orders: &[ListedOrder]) -> BTreeMap<super::book::Outpoint, CovId> {
    let ids = p.plan.spent_ids();
    let mut owners = BTreeMap::new();
    for i in &p.signed.tx.inputs {
        if let Some(id) = i.utxo.covenant_id.filter(|c| ids.contains(c)) {
            owners.insert((i.transaction_id, i.index), id);
        }
    }
    for o in orders.iter().filter(|o| ids.contains(&o.id())) {
        for c in o.custody.iter().chain(o.custody_b.iter()) {
            owners.insert(outpoint(&c.utxo), o.id());
        }
    }
    owners
}

/// The address of every input `owners` names (from the script the input spends), for the node's view of which of them are
/// still unspent after a missing-input refusal.
fn input_addresses(
    p: &super::engine::Prepared,
    owners: &BTreeMap<super::book::Outpoint, CovId>,
    network: &str,
) -> BTreeMap<super::book::Outpoint, String> {
    let mut out = BTreeMap::new();
    for i in &p.signed.tx.inputs {
        let op = (i.transaction_id, i.index);
        if !owners.contains_key(&op) {
            continue;
        }
        let addr = crate::hex::decode(&i.utxo.script_public_key).ok().and_then(|spk| crate::script::spk_address(&spk, network));
        if let Some(a) = addr {
            out.insert(op, a);
        }
    }
    out
}

/// The listed orders whose current UTXO the node no longer holds, from one answer of the node to an address query of every
/// order (`utxos`: what it holds at those addresses). The book lags the chain by the indexer's lag (seconds to tens of seconds
/// on a busy network), so an order filled, amended or cancelled meanwhile is still listed; a batch over it is refused
/// (`MissingInput`) and costs every other crossing of the batch a tick, which kept takers unfilled (and their auctions moving)
/// for tens of seconds on the soak. Only an order accepted at or before the node's virtual DAA score `node_daa` is judged (a
/// node behind the book cannot hold a newer one), and an answer that holds none of the queried orders is no evidence (a node
/// whose address index cannot serve the query): nothing is taken as spent then.
pub fn gone_orders(
    orders: &[ListedOrder],
    addresses: &BTreeMap<CovId, String>,
    utxos: &[super::node::AddressUtxo],
    node_daa: u64,
) -> BTreeSet<CovId> {
    let held: BTreeSet<super::book::Outpoint> = utxos.iter().map(|u| (u.transaction_id, u.index)).collect();
    let judged: Vec<&ListedOrder> = orders.iter().filter(|o| addresses.contains_key(&o.id()) && o.utxo_daa() <= node_daa).collect();
    if !judged.iter().any(|o| held.contains(&outpoint(&o.order.utxo))) {
        return BTreeSet::new();
    }
    judged.into_iter().filter(|o| !held.contains(&outpoint(&o.order.utxo))).map(|o| o.id()).collect()
}

/// The address of every listed order's current UTXO (its covenant script with the listed state).
fn order_addresses(orders: &[ListedOrder], network: &str) -> BTreeMap<CovId, String> {
    orders
        .iter()
        .filter_map(|o| {
            let spk = kob_protocol::tx::spk_to_string(&o.order.state.spk());
            let a = crate::hex::decode(&spk).ok().and_then(|b| crate::script::spk_address(&b, network))?;
            Some((o.id(), a))
        })
        .collect()
}

/// Every outpoint the book lists: the order UTXOs and their custodies.
fn listed_outpoints(orders: &[ListedOrder]) -> BTreeSet<super::book::Outpoint> {
    let mut s = BTreeSet::new();
    for o in orders {
        s.insert(outpoint(&o.order.utxo));
        for c in o.custody.iter().chain(o.custody_b.iter()) {
            s.insert(outpoint(&c.utxo));
        }
    }
    s
}

impl<N: NodeApi, S: BookSource> Runner<N, S> {
    pub fn new(node: N, source: S, signer: Box<dyn Signer>, cfg: RunConfig) -> Self {
        let tracker = Tracker::new(cfg.tracker.clone());
        let fee_rates = cfg.fee_policy.rates(None);
        let inventory_use = match cfg.inventory_state.as_deref().map(super::planner::InventoryUse::load) {
            Some(Ok(Some(u))) => u,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "the inventory policy's saved use could not be read: its budgets start again");
                super::planner::InventoryUse::default()
            }
            _ => super::planner::InventoryUse::default(),
        };
        let plan_base_rate = cfg.engine.planner.fee_rate;
        let selector = Selector::new(cfg.select.clone());
        Runner {
            selector,
            ceded_watch: BTreeMap::new(),
            fee_state: crate::fee::FeeState::default(),
            fee_rates,
            plan_base_rate,
            fee_board: None,
            node,
            source,
            signer,
            families: Families::default(),
            tracker,
            cfg,
            last: StepReport::default(),
            steps: 0,
            frozen_until: BTreeMap::new(),
            quarantined_until: BTreeMap::new(),
            keeper_unprofitable: BTreeSet::new(),
            keeper_unprofitable_floor: 0,
            keeper_failed: BTreeMap::new(),
            capture: None,
            reservations: None,
            waiting: None,
            last_funding: None,
            last_funding_total: None,
            inventory_use,
            keeper_inflight: BTreeMap::new(),
            keeper_subsidised: 0,
            keeper_subsidy_sompi: 0,
            keeper_subsidy: SubsidyBudget::default(),
            keeper_replaced: 0,
            keeper_replace_refused: 0,
        }
    }

    /// Replace-by-fee of this process's own stuck kills and refunds (`docs/ops/executor.md`, fee policy): one that no chain
    /// block accepted `keeper.replace_after_daa` DAA after its last submission is rebuilt from its own request (the same order
    /// inputs and funding; one `spare` funding UTXO added when it had none and the operator subsidises) at its urgency's
    /// current rate, at least a quarter above its rate, within the policy's highest rate and the total cap
    /// ([`crate::keepers::reprice`]), and sent with `submitTransactionReplacement`; at most `keeper.max_replacements` times.
    /// Only transactions the node took from this process are candidates ([`Runner::keeper_inflight`]); one whose outputs a
    /// tracked transaction spends (a chained keeper job), whose submit had no verdict, or that a chain block accepted is left
    /// alone. A refused replacement (the original was accepted meanwhile, another transaction spent the order, ...) ends the
    /// replacements of that job: the tracker's pending timeout and backoff handle it as before.
    ///
    /// A replacement may add subsidy only within the rolling budget (`KeeperConfig::subsidy_budget`, [`Runner::keeper_subsidy`])
    /// and the step's subsidised-job cap (`subsidy_max_jobs`): past either, it must be paid by the job's own tip and the
    /// subsidy it already has. Returns how many replacements added subsidy (they count toward the step's cap).
    pub async fn replace_stuck(&mut self, daa: u64, spare: &[KeyUtxo], rep: &mut StepReport) -> anyhow::Result<usize> {
        let tracker = &self.tracker;
        self.keeper_inflight.retain(|id, _| tracker.txs.get(id).is_some_and(|t| t.accepted.is_none()));
        let kc = self.cfg.keeper.clone();
        let mut subsidised = 0usize;
        if kc.max_replacements == 0 || self.cfg.dry_run {
            return Ok(subsidised);
        }
        // transactions whose outputs a tracked transaction spends: replacing one would orphan its child
        let parents: BTreeSet<[u8; 32]> = self.tracker.txs.values().flat_map(|t| t.spends.iter().map(|op| op.0)).collect();
        let due: Vec<[u8; 32]> = self
            .keeper_inflight
            .iter()
            .filter(|(id, j)| {
                !j.done
                    && j.replacements < kc.max_replacements
                    && daa >= j.sent_daa.saturating_add(kc.replace_after_daa)
                    && !parents.contains(*id)
            })
            .map(|(id, _)| *id)
            .collect();
        let mut spare: std::collections::VecDeque<KeyUtxo> = spare.iter().cloned().collect();
        let ceiling = self.cfg.fee_policy.max_rate.max(kc.fees.floor_of(kc.fee_rate));
        for old in due {
            let (Some(t), Some(j)) = (self.tracker.txs.get(&old).cloned(), self.keeper_inflight.get(&old).cloned()) else {
                continue;
            };
            if t.unconfirmed || t.parent.is_some() {
                continue;
            }
            let hex_old = kob_protocol::json::to_hex(&old);
            let want = kc.fees.rate(j.kind.urgency(), kc.fee_rate);
            let rate = want.max(t.fee_rate.saturating_add((t.fee_rate / 4).max(1))).min(ceiling);
            // the subsidy this replacement may have: what the job has, plus what the budget and the step's cap still allow
            let more = if subsidised < kc.subsidy_max_jobs { self.keeper_subsidy.left(&kc, daa) } else { 0 };
            let jc = KeeperConfig { subsidy_max: kc.subsidy_max.min(j.subsidy.saturating_add(more)), ..kc.clone() };
            let job = if rate <= t.fee_rate {
                Err(format!("already at the highest rate {ceiling}"))
            } else {
                crate::keepers::reprice(j.kind, j.order, &j.action, rate, t.fee_rate, spare.front(), &jc, self.signer.as_ref())
            };
            let job = match job {
                Ok(job) => job,
                Err(why) => {
                    tracing::info!(txid = %hex_old, kind = j.kind.name(), %why, "stuck keeper job not replaced");
                    if let Some(e) = self.keeper_inflight.get_mut(&old) {
                        e.done = true;
                    }
                    continue;
                }
            };
            if spare.front().is_some_and(|f| job.spends.contains(&outpoint(&f.utxo))) {
                spare.pop_front();
            }
            let new = job.signed.tx.id;
            let replacement = Tracked {
                txid: new,
                spends: job.spends.clone(),
                rpc_tx: rpc_json(&job.signed)?,
                profit: job.profit,
                fee: job.built.fee.fee,
                fee_rate: job.built.fee.fee_rate,
                unconfirmed: false,
                ..t.clone()
            };
            self.source.watch(new);
            let (fee, fee_rate) = (replacement.fee, replacement.fee_rate);
            let added = job.subsidy.saturating_sub(j.subsidy);
            match self.node.submit_replacement(replacement.rpc_tx.clone()).await {
                Ok(_) => {
                    tracing::info!(
                        replaced = %hex_old,
                        txid = %kob_protocol::json::to_hex(&new),
                        kind = j.kind.name(),
                        old_rate = t.fee_rate,
                        fee_rate,
                        fee,
                        subsidy = job.subsidy,
                        "stuck keeper job replaced at a higher rate"
                    );
                    self.tracker.txs.remove(&old);
                    self.tracker.txs.insert(new, replacement);
                    self.keeper_inflight.remove(&old);
                    if added > 0 {
                        if j.subsidy == 0 {
                            self.keeper_subsidised += 1;
                        }
                        self.keeper_subsidy_sompi += added;
                        self.charge_subsidy(daa, added);
                        subsidised += 1;
                    }
                    let next = InFlightJob {
                        action: job.action,
                        replacements: j.replacements + 1,
                        sent_daa: daa,
                        subsidy: job.subsidy.max(j.subsidy),
                        ..j
                    };
                    self.keeper_inflight.insert(new, next);
                    self.keeper_replaced += 1;
                    rep.replaced.push((old, new));
                }
                Err(RpcError::Node(m)) => {
                    tracing::info!(txid = %hex_old, kind = j.kind.name(), why = %m, "replacement refused; the original stays pending");
                    self.keeper_replace_refused += 1;
                    if let Some(e) = self.keeper_inflight.get_mut(&old) {
                        e.done = true;
                    }
                }
                Err(e) => {
                    // no verdict: the node may hold either one; both are followed (the replacement as an unconfirmed submit
                    // the tracker resends idempotently), whichever a chain block accepts settles the order
                    tracing::warn!(txid = %hex_old, error = %e, "replacement without a verdict; following both");
                    if let Some(e) = self.keeper_inflight.get_mut(&old) {
                        e.done = true;
                    }
                    // the replacement may be accepted: its added subsidy counts against the budget
                    if added > 0 {
                        self.charge_subsidy(daa, added);
                        subsidised += 1;
                    }
                    self.tracker.unknown += 1;
                    self.tracker.txs.insert(new, Tracked { unconfirmed: true, ..replacement });
                }
            }
        }
        Ok(subsidised)
    }

    /// Charges `sompi` of operator subsidy to the rolling budget; logs when that uses it up.
    fn charge_subsidy(&mut self, daa: u64, sompi: u64) {
        let kc = &self.cfg.keeper;
        let before = self.keeper_subsidy.left(kc, daa);
        self.keeper_subsidy.charge(kc, daa, sompi);
        if before > 0 && self.keeper_subsidy.left(kc, daa) == 0 {
            tracing::warn!(
                budget = kc.subsidy_budget,
                window_daa = kc.subsidy_window_daa,
                "keeper subsidy budget of the window used up: kills and refunds go at the rate their own tip pays"
            );
        }
    }

    /// Watches the orders the weighted selection left for later ([`Runner::ceded_watch`]).
    fn defer(&mut self, orders: &[ListedOrder], deferred: &[CovId], daa: u64) {
        if deferred.is_empty() {
            return;
        }
        let ids: BTreeSet<&CovId> = deferred.iter().collect();
        for o in orders.iter().filter(|o| ids.contains(&o.id())) {
            self.ceded_watch.entry(o.id()).or_insert((outpoint(&o.order.utxo), daa));
        }
    }

    /// Counts the watched orders another transaction took (their outpoint is no longer listed and no transaction of this
    /// process spends it) and forgets those watched for long (taken by nobody, or by this process after all).
    fn watch_ceded(&mut self, listed: &BTreeSet<super::book::Outpoint>, daa: u64) {
        if self.ceded_watch.is_empty() {
            return;
        }
        let ours = self.tracker.spent_outpoints();
        let horizon = self.cfg.select.max_defer_daa.saturating_mul(10).max(100);
        let mut ceded = 0u64;
        self.ceded_watch.retain(|_, (op, since)| {
            if !listed.contains(op) {
                if !ours.contains(op) {
                    ceded += 1;
                }
                return false;
            }
            daa < since.saturating_add(horizon)
        });
        self.selector.stats.ceded += ceded;
    }

    /// Outpoints no plan may spend: the tracker's pending and unfinalized transactions and the reservations.
    fn reserved_or_spent(&self) -> BTreeSet<super::book::Outpoint> {
        let mut s = self.tracker.spent_outpoints();
        if let Some(r) = &self.reservations {
            s.extend(r());
        }
        s
    }

    fn paused(&self) -> bool {
        crate::pause::is_set_opt(self.cfg.pause_file.as_deref())
    }

    /// Reports a tracker step and resends what a reorg moved back to pending.
    async fn on_events(&mut self, ev: super::submit::TrackerEvents, rep: &mut StepReport) {
        rep.accepted = ev.accepted.len();
        rep.finalized = ev.finalized.len();
        rep.rolled_back = ev.rolled_back.len();
        for t in &ev.finalized {
            tracing::info!(txid = %kob_protocol::json::to_hex(&t.txid), kind = %t.kind, profit = t.profit, "final");
        }
        for id in &ev.rolled_back {
            tracing::warn!(txid = %kob_protocol::json::to_hex(id), "rolled back by a reorg; re-checking");
        }
        // Idempotent resend of rolled-back transactions still possible. Not while the kill switch is on: `--pause-file` means
        // nothing leaves the process.
        if !self.cfg.dry_run && !self.paused() {
            let resend: Vec<Tracked> = ev.rolled_back.iter().filter_map(|id| self.tracker.txs.get(id).cloned()).collect();
            for t in resend {
                let _ = self.node.submit(t.rpc_tx.clone()).await;
            }
        }
    }

    async fn follow_chain(&mut self, daa: u64, rep: &mut StepReport) -> Result<(), RpcError> {
        // The book source follows the chain itself (the indexer): one follower, one truth.
        let tracked: Vec<[u8; 32]> = self.tracker.txs.keys().copied().collect();
        if let Some(view) = self.source.acceptance(&tracked) {
            let ev = self.tracker.observe(&view, daa);
            self.on_events(ev, rep).await;
            return Ok(());
        }
        let cursor = match &self.tracker.cursor {
            Some(c) => c.clone(),
            None => {
                let sink = self.node.dag_info().await?.sink;
                self.tracker.cursor = Some(sink.clone());
                sink
            }
        };
        match self.node.chain_from(&cursor).await {
            Ok(u) => {
                let ev = self.tracker.apply(&u, daa);
                self.on_events(ev, rep).await;
                Ok(())
            }
            Err(RpcError::Node(m)) => {
                // The cursor block is unknown (pruned or reorged away): restart from the sink.
                tracing::warn!(error = %m, "VSPC cursor lost; restarting from the sink");
                self.tracker.cursor = None;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// The spendable funding (P2PK UTXOs not in `spent`) and the whole P2PK balance.
    async fn funding(&self, spent: &BTreeSet<super::book::Outpoint>) -> Result<(Vec<KeyUtxo>, u64), RpcError> {
        let pk = self.signer.pubkey();
        let addr = address_of(&pk, &self.cfg.network).to_string();
        let spk = p2pk_spk_string(&pk);
        let utxos: Vec<_> = self
            .node
            .utxos_by_addresses(&[addr])
            .await?
            .into_iter()
            .filter(|u| u.covenant_id.is_none() && u.script_public_key == spk)
            .collect();
        let total = utxos.iter().map(|u| u.amount).sum();
        let free = utxos
            .into_iter()
            .filter(|u| !spent.contains(&(u.transaction_id, u.index)))
            .map(|u| KeyUtxo {
                utxo: Utxo {
                    transaction_id: u.transaction_id,
                    index: u.index,
                    amount: u.amount,
                    block_daa_score: u.block_daa_score,
                    covenant_id: None,
                },
                pubkey: pk,
            })
            .collect();
        Ok((free, total))
    }

    /// The fee rates of a step that plans (`crate::fee`): the node's estimate is read at most every `refresh_ms`, the last good
    /// one used up to `max_age_ms`, then the floor. Sets the rates of the matcher, the keepers and the maintenance jobs.
    async fn refresh_fees(&mut self) {
        let policy = self.cfg.fee_policy.clone();
        let now = std::time::Instant::now();
        if self.fee_state.due(&policy, now) {
            let r = self.node.fee_estimate().await.map_err(|e| e.to_string());
            self.fee_state.record(now, r);
        }
        let rates = self.fee_state.rates(&policy, now);
        if rates != self.fee_rates {
            tracing::info!(
                high = rates.high,
                normal = rates.normal,
                low = rates.low,
                floor = rates.floor,
                estimated = rates.estimated,
                "fee rates (sompi per gram)"
            );
        }
        self.fee_rates = rates;
        self.cfg.engine.planner.fee_rate = rates.rate(crate::fee::Urgency::Normal, self.plan_base_rate);
        self.cfg.engine.fees = rates;
        self.cfg.keeper.fees = rates;
        self.cfg.maintenance.fees = rates;
        if let Some(b) = &self.fee_board {
            *b.lock().unwrap_or_else(|e| e.into_inner()) = rates;
        }
    }

    /// [`gone_orders`] of the book, asked of the node in one query (no answer: nothing is taken as spent).
    async fn gone(&self, orders: &[ListedOrder], daa: u64) -> BTreeSet<CovId> {
        let addresses = order_addresses(orders, &self.cfg.network);
        if addresses.is_empty() {
            return BTreeSet::new();
        }
        let query: Vec<String> = addresses.values().cloned().collect::<BTreeSet<_>>().into_iter().collect();
        match self.node.utxos_by_addresses(&query).await {
            Ok(utxos) => gone_orders(orders, &addresses, &utxos, daa),
            Err(e) => {
                tracing::debug!(error = %e, "cannot ask the node which listed orders it still holds");
                BTreeSet::new()
            }
        }
    }

    async fn send(&mut self, t: Tracked, daa: u64, rep: &mut StepReport) {
        let txid = t.txid;
        let kind = t.kind.clone();
        let (fee, fee_rate) = (t.fee, t.fee_rate);
        self.source.watch(txid);
        let outcome = self.tracker.submit(&self.node, t, daa).await;
        match outcome {
            SubmitOutcome::Accepted | SubmitOutcome::AlreadyKnown => {
                tracing::info!(txid = %kob_protocol::json::to_hex(&txid), kind = %kind, fee, fee_rate, "submitted");
                rep.submitted.push(txid);
            }
            SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => {
                tracing::info!(txid = %kob_protocol::json::to_hex(&txid), ?outcome, "lost a race; backing off");
                rep.conflicts.push(txid);
            }
            SubmitOutcome::Unknown => {
                tracing::warn!(txid = %kob_protocol::json::to_hex(&txid), "no verdict from the node; tracking it as pending and resending");
                rep.unknown.push(txid);
            }
            other => {
                tracing::warn!(txid = %kob_protocol::json::to_hex(&txid), ?other, "rejected");
                rep.rejected.push((txid, other));
            }
        }
    }

    /// One tick.
    pub async fn step(&mut self) -> anyhow::Result<StepReport> {
        let mut rep = StepReport::default();
        let info = self.node.server_info().await?;
        rep.daa = info.virtual_daa_score;
        rep.synced = info.is_synced;
        if info.network_id != self.cfg.network {
            anyhow::bail!("node reports network `{}`, expected `{}`", info.network_id, self.cfg.network);
        }
        if !info.is_synced {
            self.finish(&rep);
            return Ok(rep);
        }
        let daa = info.virtual_daa_score;
        self.follow_chain(daa, &mut rep).await?;
        if self.paused() {
            rep.paused = true;
            self.finish(&rep);
            return Ok(rep);
        }
        // A submit that got no answer may have reached the node: settle the doubt before planning anything over its inputs.
        if !self.cfg.dry_run {
            for (id, outcome) in self.tracker.resubmit_unconfirmed(&self.node, daa).await {
                tracing::info!(txid = %kob_protocol::json::to_hex(&id), ?outcome, "resent a submit that had no verdict");
                match outcome {
                    SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => rep.conflicts.push(id),
                    SubmitOutcome::Rejected => rep.rejected.push((id, outcome)),
                    _ => {}
                }
            }
        }
        // Nothing is planned over a store that lags the chain: after a restart the store is where the last run left it (seconds
        // to hours behind), so the orders and the operator's tokens it lists may be spent already (the soak's restarts lost
        // maintenance sells to `MissingInput` that way). Acceptance tracking above continues.
        if let Err(why) = self.source.ready() {
            if self.waiting.is_none() {
                tracing::info!(%why, "waiting for the indexer to catch up; planning nothing");
            }
            self.waiting = Some(why.clone());
            rep.not_ready = Some(why);
            self.finish(&rep);
            return Ok(rep);
        }
        if let Some(why) = self.waiting.take() {
            tracing::info!(was = %why, "the indexer caught up; planning");
        }
        let operator = self.signer.pubkey();
        let book = match self.source.snapshot_read(&operator) {
            Some(read) => tokio::task::spawn_blocking(read).await.map_err(|e| anyhow::anyhow!("the book read task failed: {e}"))??,
            None => self.source.snapshot(&operator)?,
        };
        rep.book_daa = book.daa_score;
        if self.cfg.max_book_lag.is_some_and(|lag| book.daa_score.saturating_add(lag) < daa) {
            tracing::warn!(book_daa = book.daa_score, node_daa = daa, "the book lags the node; planning nothing until it catches up");
            rep.book_stale = true;
            self.finish(&rep);
            return Ok(rep);
        }
        // A pending transaction whose order input the book no longer lists lost its race: its other orders are free again.
        let listed = listed_outpoints(&book.orders);
        let mut match_lost = 0usize;
        for txid in self.tracker.drop_lost(&self.node, &listed, daa).await {
            tracing::info!(txid = %kob_protocol::json::to_hex(&txid), "lost a race (an input was spent by another transaction); dropped");
            rep.conflicts.push(txid);
            match_lost += 1;
        }
        self.watch_ceded(&listed, daa);
        self.refresh_fees().await;
        let mut spent = self.reserved_or_spent();
        let (funding, total) = self.funding(&spent).await?;
        rep.funding = Some(funding.iter().map(|k| k.utxo.amount).sum());
        rep.funding_total = Some(total);
        self.last_funding = rep.funding;
        self.last_funding_total = rep.funding_total;
        let clock = Clock { daa, utc: utc_now() };
        // the orders the node no longer holds (the book lags the chain): never planned, by the matcher or the keepers
        // (only while the book lags by `verify_live_min_lag`: the node round trip costs races when the book is current)
        let verify = info.has_utxo_index && daa.saturating_sub(book.daa_score) >= self.cfg.verify_live_min_lag;
        let gone = if verify {
            let all: Vec<ListedOrder> = book.orders.iter().chain(&book.keeper_only).cloned().collect();
            self.gone(&all, daa).await
        } else {
            BTreeSet::new()
        };
        rep.gone = gone.len();
        let mut excluded = self.tracker.backed_off(daa);
        excluded.extend(gone.iter().copied());
        excluded.extend(busy_orders(&book.orders, &spent));
        self.frozen_until.retain(|_, until| *until > daa);
        excluded.extend(self.frozen_until.keys().copied());
        self.quarantined_until.retain(|_, until| *until > daa);
        excluded.extend(self.quarantined_until.keys().copied());

        if self.cfg.roles.matcher {
            // the inventory policy's budgets of the period (they bound what the tick may keep)
            let period = self.cfg.engine.planner.inventory.period_daa;
            self.inventory_use.roll(daa, period);
            self.cfg.engine.planner.inventory.used = self.inventory_use.clone();
            self.cfg.engine.select = self.selector.tick(Role::Match);
            self.cfg.engine.select_since = self.cfg.engine.select.map(|_| self.selector.history.clone());
            let inp = TickInput { orders: book.orders.clone(), clock, funding: funding.clone(), excluded: excluded.clone() };
            let report = matcher_tick(&inp, &self.cfg.engine, &self.families, self.signer.as_ref());
            self.selector.record(report.selection.as_ref().map(|s| s.crossable.as_slice()).unwrap_or(&[]), daa);
            if let Some(s) = &report.selection {
                let st = &mut self.selector.stats;
                st.sampled_ticks += 1;
                st.units += s.units as u64;
                st.overdue += s.overdue as u64;
                st.taken += s.taken as u64;
                st.deferred += s.optional.saturating_sub(s.taken) as u64;
                self.defer(&book.orders, &s.deferred, daa);
            }
            let mut match_sent = 0usize;
            for (id, why) in &report.quarantined {
                tracing::error!(order = %kob_protocol::json::to_hex(id), %why, "order quarantined: left out of the matcher's book");
                if why == super::engine::PLANNING_PANICKED || why.starts_with(super::engine::REFUSED_BY_THE_BUILDER) {
                    self.quarantined_until.insert(*id, daa + QUARANTINE_DAA);
                }
                rep.quarantined += 1;
            }
            // Orders a token program refuses in the engine pre-simulation are flagged and probed again later; those whose
            // pre-simulation passed lose the flag.
            let mut flagged: Vec<(CovId, String)> = vec![];
            for s in &report.suspects {
                if !flagged.iter().any(|(id, _)| *id == s.id) {
                    flagged.push((s.id, s.reason.clone()));
                }
            }
            if !flagged.is_empty() || !report.cleared.is_empty() {
                for (id, why) in &flagged {
                    tracing::warn!(order = %kob_protocol::json::to_hex(id), %why, "order possibly frozen: left out of the books");
                    self.frozen_until.insert(*id, daa + FROZEN_RECHECK_DAA);
                }
                let cleared: Vec<CovId> = report.cleared.iter().filter(|c| !flagged.iter().any(|(id, _)| id == *c)).copied().collect();
                self.source.report_frozen(daa, &flagged, &cleared);
            }
            for (k, why) in &report.skipped {
                rep.skipped.push(format!("{}: {why}", kob_protocol::json::to_hex(&k.token)));
            }
            for p in report.prepared {
                let kind = "match";
                if !p.plan.updates.is_empty() || p.plan.triggered() > 0 {
                    tracing::info!(
                        txid = %kob_protocol::json::to_hex(&p.txid()),
                        triggered = p.plan.triggered(),
                        updates = p.plan.updates.len(),
                        "the batch triggers or arms stops"
                    );
                }
                for k in &p.plan.kept {
                    tracing::info!(
                        txid = %kob_protocol::json::to_hex(&p.txid()),
                        token = %kob_protocol::json::to_hex(&k.token),
                        amount = k.amount,
                        value = k.value,
                        "the batch keeps a token surplus as inventory"
                    );
                }
                rep.built.push((kind.into(), p.txid(), p.accounting.profit));
                if let Some(c) = self.capture.as_mut() {
                    c.push(p.signed.clone());
                }
                if self.cfg.dry_run {
                    continue;
                }
                let owners = input_owners(&p, &book.orders);
                let addresses = input_addresses(&p, &owners, &self.cfg.network);
                let t = Tracked {
                    txid: p.txid(),
                    spends: p.signed.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect(),
                    orders: p.plan.spent_ids(),
                    owners,
                    addresses,
                    parent: p.parent,
                    submitted_daa: daa,
                    rpc_tx: rpc_json(&p.signed)?,
                    accepted: None,
                    kind: kind.into(),
                    profit: p.accounting.profit,
                    fee: p.lowered.built.fee.fee,
                    fee_rate: p.lowered.built.fee.fee_rate,
                    unconfirmed: false,
                };
                let spent_ids = t.orders.clone();
                self.send(t, daa, &mut rep).await;
                if rep.conflicts.last() == Some(&p.txid()) {
                    match_lost += 1;
                }
                if rep.submitted.last() == Some(&p.txid()) {
                    match_sent += 1;
                    self.ceded_watch.retain(|id, _| !spent_ids.contains(id));
                }
                if rep.submitted.last() == Some(&p.txid()) && !p.plan.kept.is_empty() {
                    self.inventory_use.record(&p.plan.kept, p.lowered.built.fee.fee);
                    if let Some(path) = &self.cfg.inventory_state {
                        if let Err(e) = self.inventory_use.save(path) {
                            tracing::warn!(error = %e, "could not save the inventory policy's use");
                        }
                    }
                }
            }
            spent = self.reserved_or_spent();
            self.selector.observe(Role::Match, match_lost, match_sent);
        }

        if self.cfg.roles.keeper {
            // this process's stuck kills and refunds first: their replacements keep their inputs (and may take a funding UTXO)
            let spare: Vec<KeyUtxo> = funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect();
            let replaced_subsidised = self.replace_stuck(daa, &spare, &mut rep).await?;
            spent = self.reserved_or_spent();
            let funding: Vec<KeyUtxo> = funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect();
            // the listed orders plus the proven unlisted ones (kill / refund / close only: the keepers never match): the
            // snapshot's `keeper_only` (an indexer's, or a `--book-file`'s) and the source's
            let mut orders = book.orders.clone();
            let mut seen: BTreeSet<CovId> = orders.iter().map(|o| o.id()).collect();
            for o in book.keeper_only.iter().cloned().chain(self.source.keeper_only_orders()) {
                if seen.insert(o.id()) {
                    orders.push(o);
                }
            }
            let mut excluded = self.tracker.backed_off(daa);
            excluded.extend(gone.iter().copied());
            excluded.extend(busy_orders(&orders, &spent));
            // a token program refused to move these in the matcher's pre-simulation: a refund would fail the same way
            excluded.extend(self.frozen_until.keys().copied());
            // forget the orders that moved on
            let live: BTreeSet<(CovId, super::book::Outpoint)> = orders.iter().map(|o| (o.id(), outpoint(&o.order.utxo))).collect();
            let live_outpoints: BTreeSet<super::book::Outpoint> = live.iter().map(|(_, op)| *op).collect();
            self.keeper_unprofitable.retain(|k| live.contains(k));
            let floor = self.cfg.keeper.fees.floor_of(self.cfg.keeper.fee_rate);
            if floor < self.keeper_unprofitable_floor {
                self.keeper_unprofitable.clear();
            }
            self.keeper_unprofitable_floor = floor;
            self.keeper_failed.retain(|op, _| live_outpoints.contains(op));
            let mut excluded_outpoints = spent;
            excluded_outpoints.extend(self.keeper_failed.iter().filter(|(_, (until, _))| *until > daa).map(|(op, _)| *op));
            let inp = KeeperInput {
                orders,
                clock,
                funding,
                excluded,
                excluded_outpoints,
                known_unprofitable: self.keeper_unprofitable.clone(),
            };
            // the subsidy this step may still hand out: the rolling budget left, the step's cap less the replacements'
            let allowance = SubsidyAllowance {
                sompi: self.keeper_subsidy.left(&self.cfg.keeper, daa),
                jobs: self.cfg.keeper.subsidy_max_jobs.saturating_sub(replaced_subsidised),
            };
            self.cfg.keeper.select = self.selector.tick(Role::Keep);
            let report = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                keeper_tick(&inp, &self.cfg.keeper, self.signer.as_ref(), allowance)
            })) {
                Ok(r) => r,
                Err(_) => {
                    tracing::error!("keeper tick panicked; skipped (the process lives)");
                    rep.skipped.push("keeper: tick panicked".into());
                    crate::keepers::KeeperReport::default()
                }
            };
            for (id, why) in &report.skipped {
                rep.skipped.push(format!("{}: {why}", kob_protocol::json::to_hex(id)));
            }
            self.keeper_unprofitable.extend(report.unprofitable.iter().copied());
            if self.cfg.keeper.select.is_some_and(|s| s.samples()) {
                let st = &mut self.selector.stats;
                st.keeper_taken += report.taken as u64;
                st.keeper_deferred += report.deferred.len() as u64;
                st.keeper_overdue += report.overdue as u64;
                self.defer(&inp.orders, &report.deferred, daa);
            }
            let (mut keep_lost, mut keep_sent) = (0usize, 0usize);
            // bounded retry of jobs that failed to build: a growing backoff per order outpoint (a new outpoint starts over)
            for (_, op) in &report.failed {
                let e = self.keeper_failed.entry(*op).or_insert((0, 0));
                e.1 = e.1.saturating_add(1);
                let wait = KEEPER_FAILED_RECHECK_DAA.saturating_mul(1u64 << (e.1 - 1).min(16)).min(KEEPER_FAILED_MAX_DAA);
                e.0 = daa.saturating_add(wait);
            }
            for id in &report.foreign_strays {
                tracing::debug!(order = %kob_protocol::json::to_hex(id), "order holds strays (only its maker's cancel moves them)");
            }
            for j in report.jobs {
                rep.built.push((j.kind.name().into(), j.signed.tx.id, j.profit));
                if let Some(c) = self.capture.as_mut() {
                    c.push(j.signed.clone());
                }
                if self.cfg.dry_run {
                    continue;
                }
                let t = Tracked {
                    txid: j.signed.tx.id,
                    spends: j.spends.clone(),
                    orders: [j.order].into_iter().collect(),
                    owners: BTreeMap::new(),
                    addresses: BTreeMap::new(),
                    parent: None,
                    submitted_daa: daa,
                    rpc_tx: rpc_json(&j.signed)?,
                    accepted: None,
                    kind: j.kind.name().into(),
                    profit: j.profit,
                    fee: j.built.fee.fee,
                    fee_rate: j.built.fee.fee_rate,
                    unconfirmed: false,
                };
                let txid = t.txid;
                self.send(t, daa, &mut rep).await;
                let sent = rep.submitted.last() == Some(&txid);
                if rep.conflicts.last() == Some(&txid) {
                    keep_lost += 1;
                }
                if sent {
                    keep_sent += 1;
                    self.ceded_watch.remove(&j.order);
                }
                // a submit without a verdict may still be accepted: its subsidy counts against the budget too
                if j.subsidy > 0 && (sent || rep.unknown.last() == Some(&txid)) {
                    self.charge_subsidy(daa, j.subsidy);
                }
                if !sent {
                    continue;
                }
                if j.subsidy > 0 {
                    tracing::info!(
                        txid = %kob_protocol::json::to_hex(&txid),
                        kind = j.kind.name(),
                        subsidy = j.subsidy,
                        fee_rate = j.built.fee.fee_rate,
                        "keeper job subsidised from the operator's funding"
                    );
                    self.keeper_subsidised += 1;
                    self.keeper_subsidy_sompi += j.subsidy;
                }
                if j.kind.liveness() && self.cfg.keeper.max_replacements > 0 {
                    let f = InFlightJob {
                        kind: j.kind,
                        order: j.order,
                        action: j.action,
                        replacements: 0,
                        sent_daa: daa,
                        subsidy: j.subsidy,
                        done: false,
                    };
                    self.keeper_inflight.insert(txid, f);
                }
            }
            self.selector.observe(Role::Keep, keep_lost, keep_sent);
        }

        if self.cfg.roles.matcher && self.cfg.maintenance.enabled {
            self.maintenance(&book.orders, clock, &funding, daa, &mut rep).await?;
        }
        self.finish(&rep);
        Ok(rep)
    }

    /// The maintenance jobs (`crate::maintenance`): the operator's token UTXOs merged or sold, after the matcher's and the
    /// keepers' transactions of the tick (whose inputs are left alone).
    async fn maintenance(
        &mut self,
        orders: &[ListedOrder],
        clock: Clock,
        funding: &[KeyUtxo],
        daa: u64,
        rep: &mut StepReport,
    ) -> anyhow::Result<()> {
        let operator = self.signer.pubkey();
        let spent = self.reserved_or_spent();
        let tokens: Vec<crate::maintenance::OwnToken> =
            self.source.operator_tokens(&operator).into_iter().filter(|t| !spent.contains(&outpoint(&t.token.utxo))).collect();
        if tokens.is_empty() {
            return Ok(());
        }
        let mut excluded = self.tracker.backed_off(daa);
        excluded.extend(busy_orders(orders, &spent));
        excluded.extend(self.frozen_until.keys().copied());
        excluded.extend(self.quarantined_until.keys().copied());
        let inp = crate::maintenance::MaintenanceInput {
            tokens,
            orders: orders.to_vec(),
            clock,
            funding: funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect(),
            excluded,
            excluded_outpoints: spent,
        };
        let report = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::maintenance::tick(&inp, &self.cfg.maintenance, self.signer.as_ref())
        })) {
            Ok(r) => r,
            Err(_) => {
                tracing::error!("maintenance tick panicked; skipped (the process lives)");
                rep.skipped.push("maintenance: tick panicked".into());
                crate::maintenance::MaintReport::default()
            }
        };
        for (token, why) in &report.skipped {
            tracing::debug!(token = %kob_protocol::json::to_hex(token), %why, "maintenance job not built");
        }
        for j in report.jobs {
            tracing::info!(
                kind = j.kind.name(),
                token = %kob_protocol::json::to_hex(&j.token),
                txid = %kob_protocol::json::to_hex(&j.signed.tx.id),
                released = j.released,
                profit = j.profit,
                "maintenance: the operator's token UTXOs"
            );
            rep.built.push((j.kind.name().into(), j.signed.tx.id, j.profit));
            if let Some(c) = self.capture.as_mut() {
                c.push(j.signed.clone());
            }
            if self.cfg.dry_run {
                continue;
            }
            let t = Tracked {
                txid: j.signed.tx.id,
                spends: j.spends.clone(),
                orders: j.order.into_iter().collect(),
                owners: BTreeMap::new(),
                addresses: BTreeMap::new(),
                parent: None,
                submitted_daa: daa,
                rpc_tx: rpc_json(&j.signed)?,
                accepted: None,
                kind: j.kind.name().into(),
                profit: j.profit,
                fee: j.built.fee.fee,
                fee_rate: j.built.fee.fee_rate,
                unconfirmed: false,
            };
            self.send(t, daa, rep).await;
        }
        Ok(())
    }

    fn finish(&mut self, rep: &StepReport) {
        self.steps += 1;
        self.last = rep.clone();
        if let Some(p) = self.cfg.metrics_file.clone() {
            if let Err(e) = write_metrics(&p, self, rep) {
                tracing::warn!(error = %e, "cannot write metrics");
            }
        }
    }

    /// Runs until `shutdown` resolves. A step in flight when it does (a request to a node that does not answer can take its
    /// whole timeout) gets [`STEP_GRACE`] to end, for a transaction being submitted, and is dropped after that.
    pub async fn run(&mut self, shutdown: impl std::future::Future<Output = ()>) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        // the first status line comes after the first tick, then one every `status_every`
        let mut status_at: Option<std::time::Instant> = None;
        loop {
            let res = {
                let step = self.step();
                tokio::pin!(step);
                tokio::select! {
                    r = &mut step => r,
                    _ = &mut shutdown => {
                        let _ = tokio::time::timeout(STEP_GRACE, &mut step).await;
                        return Ok(());
                    }
                }
            };
            if let Some(every) = self.cfg.status_every {
                if status_at.is_none_or(|t| t.elapsed() >= every) {
                    let rep = res.as_ref().cloned().unwrap_or_else(|_| self.last.clone());
                    let mut f = self.status_facts(&rep);
                    f.tick_error = res.as_ref().err().map(|e| e.to_string());
                    tracing::info!("status: {}", status_line(&f));
                    status_at = Some(std::time::Instant::now());
                }
            }
            match res {
                Ok(r) => {
                    if !r.built.is_empty() || !r.skipped.is_empty() {
                        tracing::info!(
                            daa = r.daa,
                            built = r.built.len(),
                            submitted = r.submitted.len(),
                            conflicts = r.conflicts.len(),
                            skipped = r.skipped.len(),
                            gone = r.gone,
                            "tick"
                        );
                        // why a book or job was skipped (an engine rejection, unprofitable, too large): the operator's only
                        // trace of a batch that keeps failing (the soak's cross-limit budget finding needed it)
                        for why in &r.skipped {
                            tracing::info!(why = %why, "skipped");
                        }
                    }
                    // only a funding actually read is judged (a step that planned nothing read none: not `0`), and on the
                    // whole balance: coins pending transactions hold are still the operator's (a busy operator's free
                    // coins are often a few KAS while its balance is large)
                    if let Some(f) = r.funding_total.filter(|f| *f < self.cfg.low_funds && !self.cfg.dry_run) {
                        tracing::warn!(
                            funding = f,
                            spendable = r.funding.unwrap_or(0),
                            address = %crate::matcher::wallet::address_of(&self.signer.pubkey(), &self.cfg.network),
                            "operator funds are low: send KAS to the address"
                        );
                    }
                }
                Err(e) => tracing::warn!(error = %e, "tick failed"),
            }
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                _ = tokio::time::sleep(self.cfg.tick) => {}
            }
        }
    }
}

/// What the console status line ([`status_line`]) reports: the book source's own line, the last step's state, the
/// tracker's totals since the start and the operator's funds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusFacts {
    pub source: Option<String>,
    pub synced: bool,
    pub paused: bool,
    pub not_ready: Option<String>,
    pub book_stale: bool,
    pub submitted: u64,
    pub finalized: u64,
    pub profit_sompi: i64,
    pub pending: usize,
    pub rejected: u64,
    pub funding_total: Option<u64>,
    pub low_funds: u64,
    /// The operator's address, named in the low-funds warning (where to send KAS).
    pub funding_address: Option<String>,
    pub dry_run: bool,
    /// The last tick failed (the node unreachable, the network wrong): why.
    pub tick_error: Option<String>,
}

impl<N: NodeApi, S: BookSource> Runner<N, S> {
    /// The status line's facts after the step that reported `rep`.
    pub fn status_facts(&mut self, rep: &StepReport) -> StatusFacts {
        let t = &self.tracker;
        StatusFacts {
            source: self.source.status(),
            synced: rep.synced,
            paused: rep.paused,
            not_ready: rep.not_ready.clone(),
            book_stale: rep.book_stale,
            submitted: t.submitted,
            finalized: t.finalized_count,
            profit_sompi: t.finalized_profit,
            pending: t.pending(),
            rejected: t.rejected,
            funding_total: rep.funding_total.or(self.last_funding_total),
            low_funds: self.cfg.low_funds,
            funding_address: Some(crate::matcher::wallet::address_of(&self.signer.pubkey(), &self.cfg.network).to_string()),
            dry_run: self.cfg.dry_run,
            tick_error: None,
        }
    }
}

/// KAS of an amount of sompi, without trailing zeros (`182.4`, `-0.0012`).
pub fn kas_text(sompi: i64) -> String {
    let sign = if sompi < 0 { "-" } else { "" };
    let a = sompi.unsigned_abs();
    let frac = format!("{:08}", a % 100_000_000);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        format!("{sign}{}", a / 100_000_000)
    } else {
        format!("{sign}{}.{frac}", a / 100_000_000)
    }
}

/// The one-line status an operator watching the console reads (`kob-executor run --status-secs`, every 60 s by default):
/// where the book is, what the executor sent and earned since the start, its funds, and what needs attention. The metrics
/// file (`--metrics-file`) carries the same facts for monitoring.
pub fn status_line(f: &StatusFacts) -> String {
    let mut parts = vec![];
    if let Some(s) = &f.source {
        parts.push(s.clone());
    }
    parts.push(format!(
        "{} transactions sent, {} final, {} pending, profit of the final ones {} KAS",
        f.submitted,
        f.finalized,
        f.pending,
        kas_text(f.profit_sompi)
    ));
    parts.push(match f.funding_total {
        Some(t) => format!("funds {} KAS", kas_text(t as i64)),
        None => "funds not read yet".to_string(),
    });
    let mut warn = vec![];
    if let Some(e) = &f.tick_error {
        warn.push(format!("the last tick failed ({e})"));
    } else if !f.synced {
        warn.push("the node is not synced: nothing is planned".to_string());
    }
    if f.paused {
        warn.push("PAUSED (the pause file exists)".to_string());
    }
    if let Some(why) = &f.not_ready {
        warn.push(format!("waiting for the indexer ({why}): nothing is planned"));
    } else if f.book_stale {
        warn.push("the book lags the node: nothing is planned".to_string());
    }
    if let Some(t) = f.funding_total.filter(|t| *t < f.low_funds && !f.dry_run) {
        let to = f.funding_address.as_ref().map(|a| format!("; send KAS to {a}")).unwrap_or_default();
        warn.push(format!("funds low ({} KAS, below {} KAS{to})", kas_text(t as i64), kas_text(f.low_funds as i64)));
    }
    if f.rejected > 0 {
        warn.push(format!("{} transactions rejected by the node (see the log)", f.rejected));
    }
    if f.dry_run {
        warn.push("dry run: nothing is submitted".to_string());
    }
    parts.push(if warn.is_empty() { "no warnings".to_string() } else { format!("WARNING: {}", warn.join("; ")) });
    parts.join(" | ")
}

/// Prometheus textfile-collector metrics.
pub fn metrics_text<N: NodeApi, S: BookSource>(r: &Runner<N, S>, rep: &StepReport) -> String {
    let t = &r.tracker;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut s = String::new();
    let mut g = |name: &str, help: &str, v: String| {
        s.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}\n"));
    };
    g("kob_matcher_last_step_seconds", "Unix time of the last completed step.", now.to_string());
    g("kob_matcher_daa_score", "Node virtual DAA score at the last step.", rep.daa.to_string());
    g("kob_matcher_node_synced", "1 when the node reports synced.", (rep.synced as u8).to_string());
    g("kob_matcher_paused", "1 while the kill-switch file exists.", (rep.paused as u8).to_string());
    g("kob_matcher_book_stale", "1 when the book lagged the node and nothing was planned.", (rep.book_stale as u8).to_string());
    g(
        "kob_matcher_budget_slack_retries_total",
        "Builds retried with more compute budget than the table gives (a shape the budget generator misses: report it; 0 expected).",
        super::lower::budget_slack_retries().to_string(),
    );
    g(
        "kob_matcher_book_lag_daa",
        "DAA the book is behind the node at the last step.",
        if rep.book_daa == 0 { 0 } else { rep.daa.saturating_sub(rep.book_daa) }.to_string(),
    );
    g("kob_matcher_submitted_total", "Transactions accepted into the mempool.", t.submitted.to_string());
    g("kob_matcher_conflicts_total", "Benign double spends / lost races.", t.conflicts.to_string());
    g("kob_matcher_rejected_total", "Other rejections (investigate).", t.rejected.to_string());
    g(
        "kob_matcher_unknown_submits_total",
        "Submits without a verdict from the node (tracked as pending, resent).",
        t.unknown.to_string(),
    );
    g("kob_matcher_finalized_total", "Transactions final (accepted + depth).", t.finalized_count.to_string());
    g("kob_matcher_finalized_profit_sompi", "Operator profit of final transactions.", t.finalized_profit.to_string());
    g("kob_matcher_pending_txs", "Submitted, not yet accepted.", t.pending().to_string());
    g("kob_matcher_tracked_txs", "Pending or accepted but not final.", t.txs.len().to_string());
    // the last funding read (a step that waited read none); unknown before the first read: no alert
    let funding = rep.funding.or(r.last_funding);
    let total = rep.funding_total.or(r.last_funding_total);
    g(
        "kob_matcher_funding_sompi",
        "Spendable operator P2PK balance (not held by pending transactions).",
        funding.unwrap_or(0).to_string(),
    );
    g(
        "kob_matcher_funding_total_sompi",
        "Operator P2PK balance, pending transactions' inputs included.",
        total.unwrap_or(0).to_string(),
    );
    g(
        "kob_operator_low_funds",
        "1 when the operator's balance (pending inputs included) is below the alert threshold.",
        (total.is_some_and(|f| f < r.cfg.low_funds) as u8).to_string(),
    );
    g(
        "kob_matcher_waiting_for_indexer",
        "1 when the indexer lagged the chain and nothing was planned.",
        (rep.not_ready.is_some() as u8).to_string(),
    );
    g("kob_matcher_skipped_books", "Books or jobs skipped in the last step.", rep.skipped.len().to_string());
    let f = &r.fee_rates;
    g("kob_fee_rate_high", "Fee rate of urgent transactions (sompi per gram).", f.high.to_string());
    g("kob_fee_rate_normal", "Fee rate of normal transactions (sompi per gram).", f.normal.to_string());
    g("kob_fee_rate_low", "Fee rate of housekeeping transactions (sompi per gram).", f.low.to_string());
    g("kob_fee_estimated", "1 when the rates come from the node's fee estimate, 0 at the floor.", (f.estimated as u8).to_string());
    g("kob_fee_estimate_failures_total", "Fee estimate reads that failed.", r.fee_state.failures.to_string());
    g(
        "kob_keeper_subsidised_total",
        "Kills and refunds submitted with a fee top-up from the operator's funding.",
        r.keeper_subsidised.to_string(),
    );
    g("kob_keeper_subsidy_sompi_total", "Sompi of the operator's funding those top-ups took.", r.keeper_subsidy_sompi.to_string());
    g(
        "kob_keeper_subsidy_budget_left_sompi",
        "Sompi of keeper subsidy the rolling window still allows (--keeper-subsidy-budget-kas).",
        r.keeper_subsidy.left(&r.cfg.keeper, rep.daa).to_string(),
    );
    g("kob_keeper_replaced_total", "Stuck kills and refunds replaced at a higher fee rate.", r.keeper_replaced.to_string());
    g(
        "kob_keeper_replace_refused_total",
        "Replacements the node refused (the original stayed pending).",
        r.keeper_replace_refused.to_string(),
    );
    let sel = &r.selector;
    let st = &sel.stats;
    g(
        "kob_select_enabled",
        "1 when the weighted work selection is on (--no-weighted-select: 0).",
        (sel.cfg.enabled as u8).to_string(),
    );
    g(
        "kob_select_share_match",
        "Share of the optional work units the matcher takes per tick (1: all, the reference).",
        sel.share_match.to_string(),
    );
    g("kob_select_share_keep", "Share of the optional due jobs the keepers take per tick (1: all).", sel.share_keep.to_string());
    g(
        "kob_select_sampled_ticks_total",
        "Matcher ticks that sampled their work units (a share below 1).",
        st.sampled_ticks.to_string(),
    );
    g("kob_select_units_total", "Work units the sampling ticks judged (mandatory ones included).", st.units.to_string());
    g("kob_select_taken_total", "Optional work units the matcher's sample took.", st.taken.to_string());
    g("kob_select_deferred_total", "Optional work units the matcher left for later (for another executor).", st.deferred.to_string());
    g(
        "kob_select_overdue_total",
        "Work units taken by every executor because a crossing waited max-defer-daa (the starvation bound).",
        st.overdue.to_string(),
    );
    g("kob_select_keeper_taken_total", "Due keeper jobs the sample took.", st.keeper_taken.to_string());
    g("kob_select_keeper_deferred_total", "Due keeper jobs left for later.", st.keeper_deferred.to_string());
    g("kob_select_keeper_overdue_total", "Keeper jobs built because they were due for max-defer-daa.", st.keeper_overdue.to_string());
    g(
        "kob_select_ceded_total",
        "Orders left for later that another transaction took first: races not run (a proxy of the collisions avoided).",
        st.ceded.to_string(),
    );
    g(
        "kob_select_lost_steps_match_total",
        "Matcher steps that lost a race (each halves the matcher's share).",
        st.lost_steps_match.to_string(),
    );
    g(
        "kob_select_lost_steps_keep_total",
        "Keeper steps that lost a race (each halves the keepers' share).",
        st.lost_steps_keep.to_string(),
    );
    s
}

fn write_metrics<N: NodeApi, S: BookSource>(p: &Path, r: &mut Runner<N, S>, rep: &StepReport) -> std::io::Result<()> {
    let tmp = p.with_extension("prom.tmp");
    let extra = r.source.metrics();
    std::fs::write(&tmp, metrics_text(r, rep) + &extra)?;
    std::fs::rename(tmp, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kas_text_drops_trailing_zeros() {
        assert_eq!(kas_text(0), "0");
        assert_eq!(kas_text(18_240_000_000), "182.4");
        assert_eq!(kas_text(100_000_000), "1");
        assert_eq!(kas_text(-120_000), "-0.0012");
        assert_eq!(kas_text(1), "0.00000001");
    }

    #[test]
    fn the_status_line_names_what_needs_attention() {
        let mut f = StatusFacts {
            source: Some("indexer following, caught up (2 DAA behind the node), 9 orders indexed".into()),
            synced: true,
            submitted: 14,
            finalized: 12,
            profit_sompi: 42_000_000,
            pending: 1,
            funding_total: Some(18_240_000_000),
            low_funds: 10_000_000_000,
            ..StatusFacts::default()
        };
        assert_eq!(
            status_line(&f),
            "indexer following, caught up (2 DAA behind the node), 9 orders indexed | 14 transactions sent, 12 final, 1 pending, \
             profit of the final ones 0.42 KAS | funds 182.4 KAS | no warnings"
        );
        f.source = None;
        f.funding_total = Some(5_000_000_000);
        f.paused = true;
        f.not_ready = Some("indexer catching_up, 900 DAA behind the node".into());
        f.rejected = 2;
        let s = status_line(&f);
        assert!(s.starts_with("14 transactions sent"), "{s}");
        assert!(
            s.ends_with(
                "funds 50 KAS | WARNING: PAUSED (the pause file exists); waiting for the indexer (indexer catching_up, 900 DAA behind \
                 the node): nothing is planned; funds low (50 KAS, below 100 KAS); 2 transactions rejected by the node (see the log)"
            ),
            "{s}"
        );
        // with the address known, the warning says where to send KAS
        f.funding_address = Some("kaspa:qqexample".into());
        assert!(status_line(&f).contains("funds low (50 KAS, below 100 KAS; send KAS to kaspa:qqexample)"));
        f.funding_address = None;
        f.funding_total = None;
        f.synced = false;
        let s = status_line(&f);
        assert!(s.contains("funds not read yet") && s.contains("WARNING: the node is not synced"), "{s}");
        f.tick_error = Some("transport: refused".into());
        let s = status_line(&f);
        assert!(s.contains("WARNING: the last tick failed (transport: refused); PAUSED") && !s.contains("not synced"), "{s}");
    }
}
