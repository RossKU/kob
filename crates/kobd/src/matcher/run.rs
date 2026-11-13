//! The `kobd match` and `kobd keep` loop.
//!
//! The runner takes one [`Runner::step`] per tick, every second by default
//! (`docs/spec/matcher.md` §3):
//!
//! 1. Node. Whether it is synced, its virtual DAA score, and the acceptance of our transactions
//!    from the VSPC v2 cursor. A reorg rolls a transaction back to pending, and it is final after
//!    `final_depth` DAA.
//! 2. Book. A fresh snapshot from the [`BookSource`], the indexer's [`OrderBookView`].
//! 3. Funding. The operator's P2PK UTXOs from the node, minus everything pending transactions
//!    spend.
//! 4. The matcher tick, which plans, chains and validates transactions in the engine, and the
//!    keeper tick.
//! 5. Submission through the [`Tracker`]. Benign double spends back off, and nothing is replaced
//!    blindly. A chained step is sent only after its parent was accepted into the mempool. A kill
//!    or refund of this process that stays unaccepted is rebuilt at a higher rate and replaced
//!    ([`Runner::replace_stuck`]).
//!
//! The kill switch is a file (`--pause-file`). While it exists nothing is built or submitted, and
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
use super::submit::{ResendClock, Tracked, Tracker, TrackerConfig};
use super::wallet::{address_of, p2pk_spk_string, Signer};
use crate::keepers::{tick_with_subsidy as keeper_tick, JobKind, KeeperConfig, KeeperInput, SubsidyAllowance, SubsidyBudget};

/// An event at DEBUG when `$quiet` holds, and at INFO otherwise.
///
/// What a dry run builds again on every tick is detail, and what a live run submits is a state
/// change. As in kaspad, the log keeps the periodic status line and the state changes at INFO.
macro_rules! info_unless {
    ($quiet:expr, $($t:tt)*) => {
        if $quiet {
            tracing::debug!($($t)*)
        } else {
            tracing::info!($($t)*)
        }
    };
}

/// A read of the book that blocks on a database.
///
/// The runner runs it on the blocking thread pool, never on a runtime worker.
pub type BookRead = Box<dyn FnOnce() -> anyhow::Result<MemoryBook> + Send>;

/// Where the book comes from each tick.
pub trait BookSource: Send {
    /// A consistent snapshot of the listed orders and the operator's tokens.
    fn snapshot(&mut self, operator: &[u8; 32]) -> anyhow::Result<MemoryBook>;

    /// [`BookSource::snapshot`] as a read that the runner runs on the blocking thread pool.
    ///
    /// A source that reads a database, such as the indexer's store, returns one. `None`, the
    /// default, makes the runner call `snapshot` in place, for a file or a memory book.
    fn snapshot_read(&mut self, _operator: &[u8; 32]) -> Option<BookRead> {
        None
    }

    /// `Err(why)` while the source lags the chain.
    ///
    /// That is the indexer after a restart, while it catches up, or while it is cut off from the
    /// node. The runner then plans nothing, for the matcher, the keepers or maintenance, and keeps
    /// tracking acceptance. The default is always ready.
    fn ready(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called with the id of a transaction just before it is submitted.
    ///
    /// So a source that follows the chain itself, the indexer, sees its acceptance even if a block
    /// arrives within milliseconds.
    fn watch(&mut self, _txid: [u8; 32]) {}

    /// Where the selected chain accepts each of `tracked` now, when the source follows the chain.
    ///
    /// The map goes from transaction id to the accepting chain block and its DAA score, and a
    /// missing id is not accepted. `None` means the source does not follow the chain, and the
    /// runner follows acceptance through the node (`getVirtualChainFromBlockV2`). In-process the
    /// indexer answers. So there is exactly one VSPC follower and one truth about which
    /// transactions are accepted, the one the book was built from.
    fn acceptance(&mut self, _tracked: &[[u8; 32]]) -> Option<BTreeMap<[u8; 32], (String, u64)>> {
        None
    }

    /// Live orders the indexer proved but does not list.
    ///
    /// Examples are a token outside the allowlist, an exit of an unlisted entry, or a carrier below
    /// the listing minimum. They are never matched, but the keepers kill, refund and close them
    /// when that pays. Otherwise their makers would wait for nobody, though the maker's own cancel
    /// works either way. They come in addition to the snapshot's own `keeper_only`, where the
    /// indexer's snapshot and a `--book-file` carry them. The default is none.
    fn keeper_only_orders(&mut self) -> Vec<ListedOrder> {
        vec![]
    }

    /// The operator's own key-owned token UTXOs, accepted and with their programs.
    ///
    /// The maintenance jobs use them ([`crate::maintenance`]). The default is none, so no
    /// maintenance.
    fn operator_tokens(&mut self, _operator: &[u8; 32]) -> Vec<crate::maintenance::OwnToken> {
        vec![]
    }

    /// The tick's outcome for orders that a token program may refuse to move.
    ///
    /// `flagged` lists (id, reason) of the orders whose engine pre-simulation failed at the token
    /// program, and `cleared` those whose pre-simulation passed. A source that stores the flag
    /// ("possibly frozen") keeps the order off the books. The default ignores both.
    fn report_frozen(&mut self, _daa: u64, _flagged: &[(CovId, String)], _cleared: &[CovId]) {}

    /// Extra Prometheus lines for the metrics file, such as the in-process indexer's follower
    /// state.
    ///
    /// The default is none.
    fn metrics(&mut self) -> String {
        String::new()
    }

    /// The book's part of the console status line ([`status_line`]).
    ///
    /// For the in-process indexer it is its state and lag. The default is none.
    fn status(&mut self) -> Option<String> {
        None
    }

    /// Why the process should end so that a supervisor restarts it, as `(at once, why)`.
    ///
    /// The causes are an in-process indexer that halted or cannot write its database, or a read
    /// API that stopped (`HealthSnapshot::halted`). The runner exits with an error when the cause
    /// lasts `RunConfig::halt_exit_after`, or at once when the first element is true. The default
    /// is never.
    fn fatal(&mut self) -> Option<(bool, String)> {
        None
    }

    /// The runner's facts after each step, for `/v1/health` (`executor`) and `/v1/metrics`.
    ///
    /// The in-process indexer's API serves them. The default drops them.
    fn report_executor(&mut self, _health: crate::indexer::status::ExecutorHealth, _metrics: String) {}
}

/// How long the step in flight may go on once the runner was asked to stop ([`Runner::run`]).
pub const STEP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// DAA scores that a quarantined order stays out of the book, one hour at 10 per second.
///
/// An order is quarantined when its planning panicked or the builder refused it on its own.
pub const QUARANTINE_DAA: u64 = 36_000;

/// DAA scores before an order flagged possibly frozen is probed again, 5 minutes at 10 per second.
pub const FROZEN_RECHECK_DAA: u64 = 3_000;

/// DAA scores that a keeper job which failed to build first stays out of the keepers' queue.
///
/// That is 1 minute at 10 per second. It doubles with every further failure of the same order
/// outpoint, up to [`KEEPER_FAILED_MAX_DAA`].
pub const KEEPER_FAILED_RECHECK_DAA: u64 = 600;

/// Longest keeper backoff of an order outpoint whose jobs keep failing, one hour.
pub const KEEPER_FAILED_MAX_DAA: u64 = 36_000;

/// Any [`OrderBookView`], such as the in-process indexer, as a book source.
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

/// A JSON snapshot file ([`MemoryBook`]), read again when it changes.
///
/// An indexer export writes it.
pub struct FileSource {
    /// Path of the snapshot file.
    pub path: PathBuf,
    cached: Option<(SystemTime, MemoryBook)>,
}

impl FileSource {
    /// A source that reads the snapshot file at `path`.
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
    /// The process matches orders.
    pub matcher: bool,
    /// The process runs the keepers.
    pub keeper: bool,
}

impl Roles {
    /// The roles of `kobd --executor` as `/v1/health` names them (`HealthSnapshot::roles`).
    ///
    /// They are `executor` while the matcher or the keepers work, and `indexer` when the store
    /// keeps the history (`lean = false`). So over a lean store the process is the Executor alone,
    /// and over a full one it is the Executor and the Indexer.
    pub fn health_roles(self, lean: bool) -> Vec<&'static str> {
        let mut v = vec![];
        if self.matcher || self.keeper {
            v.push("executor");
        }
        if !lean {
            v.push("indexer");
        }
        v
    }
}

/// Loop settings.
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// The network the process runs on.
    pub network: String,
    /// Roles of the process.
    pub roles: Roles,
    /// Matcher engine settings.
    pub engine: EngineConfig,
    /// Keeper settings.
    pub keeper: KeeperConfig,
    /// Merges and sales of the operator's own tokens (`crate::maintenance`), with the matcher role.
    pub maintenance: crate::maintenance::MaintenanceConfig,
    /// Submission and acceptance tracking settings.
    pub tracker: TrackerConfig,
    /// Build and validate, never submit.
    pub dry_run: bool,
    /// Time between steps.
    pub tick: Duration,
    /// Kill switch: while this file exists nothing is built or submitted.
    pub pause_file: Option<PathBuf>,
    /// Prometheus textfile-collector output (`*.prom`).
    pub metrics_file: Option<PathBuf>,
    /// Where the surplus-inventory policy's use of its period is kept.
    ///
    /// The use is of `maxFee` and `tokens[].maxAmount`, so a restart does not start the budgets
    /// again. `None` keeps it in memory only.
    pub inventory_state: Option<PathBuf>,
    /// Alert threshold. Funding below this many sompi is reported (`kob_operator_low_funds`).
    pub low_funds: u64,
    /// Plan nothing while the book is more than this many DAA behind the node.
    ///
    /// A lagging indexer would offer spent orders. Acceptance tracking continues. `None` means no
    /// check, for snapshot files.
    pub max_book_lag: Option<u64>,
    /// Lag of the book behind the node, in DAA, from which the node is asked which listed orders it
    /// still holds ([`gone_orders`]).
    ///
    /// The query costs a node round trip every tick, which loses races to other executors. It
    /// only matters while the book can list orders the chain has already spent. 0 asks every step.
    pub verify_live_min_lag: u64,
    /// The fee policy (`crate::fee`).
    ///
    /// Each step that plans reads the node's fee estimate, at most every `refresh_ms`. It sets the
    /// tick's rates of the matcher, the keepers and the maintenance jobs. Their configured
    /// `fee_rate`s stay the lowest rate each pays.
    pub fee_policy: crate::fee::FeePolicy,
    /// Weighted work selection among competing executors (`docs/spec/matcher.md` §3.6).
    ///
    /// See [`super::select`]. It is off by default in the library, which gives the reference
    /// planner, and the live commands turn it on.
    pub select: SelectConfig,
    /// Log a one-line status ([`status_line`]) this often (`--status-secs`). `None` means never.
    pub status_every: Option<Duration>,
    /// Where the runner keeps what must survive a restart, the Executor's data directory.
    ///
    /// It holds the in-flight journal ([`super::journal`]), the keepers' subsidy window and the
    /// count of starts. `None` keeps them in memory only. A dry run never uses it, since nothing is
    /// submitted.
    pub state_dir: Option<PathBuf>,
    /// Seconds the local clock may differ from the chain's before an alarm, 0 for no check.
    ///
    /// The chain's clock is the newest chain block's timestamp. Beyond the bound, day orders near
    /// their deadline are also left alone.
    pub clock_skew_max_secs: u64,
    /// How long the runner waits for a halted book source (`BookSource::fatal`).
    ///
    /// After that it ends with an error, so that a supervisor restarts the process.
    pub halt_exit_after: Duration,
    /// Hand the operator's balance to the book source's `/v1/health` and `/v1/metrics`.
    ///
    /// The balance goes through `BookSource::report_executor`. It is `false` for a read API that
    /// other hosts can reach (`crate::config::ApiConfig::shows_executor_funds`). The amounts are
    /// then left out there, and the metrics file keeps them.
    pub report_funds: bool,
}

/// The start counter's file name in [`RunConfig::state_dir`].
pub const STARTS_FILE: &str = "executor-starts";
/// The keepers' subsidy window's file name in [`RunConfig::state_dir`].
pub const SUBSIDY_FILE: &str = "keeper-subsidy.json";
/// How often the local clock is compared with the chain's.
pub const CLOCK_CHECK_EVERY: Duration = Duration::from_secs(60);
/// The clock samples kept, one per [`CLOCK_CHECK_EVERY`].
pub const CLOCK_SAMPLES: usize = 5;
/// Samples needed before their median is judged against the bound itself.
pub const CLOCK_SAMPLES_MIN: usize = 3;
/// Time between samples until [`CLOCK_SAMPLES_MIN`] samples exist after a start.
///
/// So the check judges within seconds of a start, not minutes.
pub const CLOCK_START_EVERY: Duration = Duration::from_secs(5);
/// Factor of the bound that the median is judged against on fewer than [`CLOCK_SAMPLES_MIN`]
/// samples.
///
/// So only a skew far beyond the bound raises the alarm that early. A skew within the bound clears
/// it, and one in between waits for the samples.
pub const CLOCK_EARLY_FACTOR: u64 = 2;
/// Local time the samples must span before they tell whether the chain advances.
///
/// That tells whether every node stands still.
pub const CLOCK_ADVANCE_SPAN_MS: u64 = 10_000;

/// What a clock skew beyond the bound most likely means, for the alarm's wording ([`clock_cause`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockCause {
    /// Every node's newest block is ahead of the local clock, so the local clock is behind.
    ClockBehind,
    /// The chain advances, and every node's newest block lags the local clock by the same amount.
    ///
    /// So the local clock is ahead.
    ClockAhead,
    /// Every node's newest block is behind the local clock, and the chain did not advance across
    /// the samples.
    ///
    /// So the nodes are behind the network, by a stall or a partition, not the local clock.
    NodesBehind,
    /// Every node's newest block is behind the local clock, and the samples do not span enough time
    /// yet to tell why.
    AheadOrNodesBehind,
}

impl ClockCause {
    /// The status line's words for a skew of `s` seconds.
    pub fn describe(self, s: i64) -> String {
        let a = s.unsigned_abs();
        match self {
            ClockCause::ClockBehind => format!("the local clock is {a} s behind the chain's (set it: NTP)"),
            ClockCause::ClockAhead => format!("the local clock is {a} s ahead of the chain's (set it: NTP)"),
            ClockCause::NodesBehind => {
                format!("every node's newest block is {a} s behind the local clock and not advancing (the nodes are behind: check them)")
            }
            ClockCause::AheadOrNodesBehind => format!(
                "the local clock is {a} s ahead of every node's newest block (the clock is ahead, or every node is behind: check NTP and the nodes)"
            ),
        }
    }

    /// The error line's words.
    fn explain(self) -> &'static str {
        match self {
            ClockCause::ClockBehind => {
                "the local clock is behind the chain's (every node's newest block is ahead of it): set the system clock (NTP)"
            }
            ClockCause::ClockAhead => {
                "the local clock is ahead of the chain's (the chain advances, and every node's newest block lags the local clock by the same amount): set the system clock (NTP)"
            }
            ClockCause::NodesBehind => {
                "every node's newest block is behind the local clock and the chain did not advance over the samples: the nodes are behind the network (a stall or a partition), not the local clock; check the nodes"
            }
            ClockCause::AheadOrNodesBehind => {
                "every node's newest block is behind the local clock: the local clock is ahead (set it: NTP), or every node is behind the network (check them)"
            }
        }
    }
}

/// Why the skew `skew`, the median of the samples, is off.
///
/// `points` are the samples oldest first, as the local time in ms and the sample in seconds. A
/// sample is the local clock minus the newest chain block's time. So while the chain stands still
/// it grows with the local time, and while the local clock is merely off it stays.
pub fn clock_cause(skew: i64, points: &std::collections::VecDeque<(u64, i64)>) -> ClockCause {
    if skew < 0 {
        return ClockCause::ClockBehind;
    }
    let (Some(first), Some(last)) = (points.front(), points.back()) else { return ClockCause::AheadOrNodesBehind };
    let span_ms = last.0.saturating_sub(first.0);
    if span_ms < CLOCK_ADVANCE_SPAN_MS {
        return ClockCause::AheadOrNodesBehind;
    }
    // How far the chain's newest block moved over the span, in seconds. That is the local time
    // passed minus the sample's growth.
    let span = (span_ms / 1000) as i64;
    let advanced = span - (last.1 - first.1);
    if advanced * 2 < span {
        ClockCause::NodesBehind
    } else {
        ClockCause::ClockAhead
    }
}

/// One clock sample, in seconds of the local clock minus the chain's.
///
/// It is taken from the tip timestamps `tips` of the nodes at the local time `local_ms`, as the one
/// nearest the local clock. A block's timestamp is set by its miner, so no node alone decides. A
/// node whose timestamp lies, or that stopped advancing, is outvoted by any node that agrees with
/// the local clock. Only a skew that every node shows counts. `None` means no tip is known.
pub fn chain_skew_secs(local_ms: u64, tips: &[u64]) -> Option<i64> {
    tips.iter().map(|t| (local_ms as i64 - *t as i64) / 1000).min_by_key(|s| s.unsigned_abs())
}

/// The median of `v`, the lower one of the two middle values. `None` when empty.
pub fn median(v: &std::collections::VecDeque<i64>) -> Option<i64> {
    let mut s: Vec<i64> = v.iter().copied().collect();
    s.sort_unstable();
    s.get((s.len().saturating_sub(1)) / 2).copied()
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            // For tests and embedders only. Every command sets the network it resolved
            // (`crate::config::resolve_network`).
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
            state_dir: None,
            clock_skew_max_secs: 30,
            halt_exit_after: Duration::from_secs(180),
            report_funds: true,
        }
    }
}

/// The runner's series that carry the operator's balance.
///
/// A read API that other hosts can reach leaves them out ([`RunConfig::report_funds`]).
pub const FUNDS_SERIES: &[&str] = &["kob_matcher_funding_sompi", "kob_matcher_funding_total_sompi"];

/// `metrics`, in Prometheus text, without the series [`FUNDS_SERIES`] and their `# HELP` and
/// `# TYPE` lines.
pub fn without_funds(metrics: &str) -> String {
    let name = |line: &str| -> String {
        let l = line.strip_prefix("# HELP ").or_else(|| line.strip_prefix("# TYPE ")).unwrap_or(line);
        l.split([' ', '{']).next().unwrap_or("").to_string()
    };
    metrics.lines().filter(|l| !FUNDS_SERIES.contains(&name(l).as_str())).map(|l| format!("{l}\n")).collect()
}

/// What one step did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepReport {
    /// The node's virtual DAA score.
    pub daa: u64,
    /// Whether the node is synced.
    pub synced: bool,
    /// Whether the kill switch was on.
    pub paused: bool,
    /// The book was too far behind the node to plan against (`RunConfig::max_book_lag`).
    pub book_stale: bool,
    /// DAA score the book was synced to, 0 when no book was read.
    pub book_daa: u64,
    /// Why the book source was not ready (`BookSource::ready`). Nothing was planned.
    pub not_ready: Option<String>,
    /// (kind, txid, operator profit) of every transaction built this step.
    pub built: Vec<(String, [u8; 32], i64)>,
    /// Transactions the node took this step, or already knew.
    pub submitted: Vec<[u8; 32]>,
    /// Transactions that lost a race, because another transaction spent one of their inputs.
    pub conflicts: Vec<[u8; 32]>,
    /// Transactions the node refused, with its verdict.
    pub rejected: Vec<([u8; 32], SubmitOutcome)>,
    /// Transactions whose submit got no verdict. They are tracked as pending and resent.
    pub unknown: Vec<[u8; 32]>,
    /// Books and jobs skipped, with the reason.
    pub skipped: Vec<String>,
    /// Orders left out of the matcher's book this step.
    ///
    /// Their numbers failed the sanity gate, their planning panicked, or the builder refused the
    /// order.
    pub quarantined: usize,
    /// Tracked transactions accepted this step.
    pub accepted: usize,
    /// Tracked transactions final this step.
    pub finalized: usize,
    /// Tracked transactions a reorg moved back to pending this step.
    pub rolled_back: usize,
    /// The operator's spendable funding read this step.
    ///
    /// It is the P2PK UTXOs that no pending transaction or reservation holds. `None` means it was
    /// not read, and nothing was planned.
    pub funding: Option<u64>,
    /// The operator's whole P2PK balance read this step, which the low-funds alert judges.
    ///
    /// It includes the UTXOs that pending transactions hold, because they stay the operator's until
    /// spent and their change returns.
    pub funding_total: Option<u64>,
    /// Listed orders left out of this step because the node no longer holds their UTXO.
    ///
    /// The UTXO was spent in a block the book has not reached yet ([`gone_orders`]). It is always 0
    /// while the book lags by less than `RunConfig::verify_live_min_lag`.
    pub gone: usize,
    /// Stuck kills and refunds of this process replaced at a higher rate this step.
    ///
    /// Each is (replaced txid, new txid).
    pub replaced: Vec<([u8; 32], [u8; 32])>,
    /// Pending transactions the node refused when they were checked again, and so dropped.
    ///
    /// See [`Tracker::recheck_pending`].
    pub dropped: Vec<[u8; 32]>,
}

/// A kill or refund this process submitted and may still replace at a higher rate.
///
/// See [`Runner::replace_stuck`]. Only transactions the node took from this process are ever
/// recorded, so nobody else's transaction is replaced. The job is kept in the in-flight journal
/// ([`super::journal`]) with its transaction, so a restart can still replace it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct InFlightJob {
    /// What the keeper job does.
    pub kind: JobKind,
    /// The order the job spends.
    pub order: CovId,
    /// The builder request of the submitted transaction. A replacement changes only its rate.
    pub action: kob_protocol::build::Action,
    /// Replacements so far.
    pub replacements: u32,
    /// DAA score of the last submission, the original or the last replacement.
    pub sent_daa: u64,
    /// The operator's subsidy of the submitted transaction, in sompi.
    pub subsidy: u64,
    /// No further replacement.
    ///
    /// The node refused one, a submit had no verdict, or no higher rate fits.
    pub done: bool,
}

/// The matcher and keeper process.
pub struct Runner<N: NodeApi, S: BookSource> {
    /// The node the process talks to.
    pub node: N,
    /// Where the book comes from.
    pub source: S,
    /// The operator's key.
    pub signer: Box<dyn Signer>,
    /// The builders of each token family.
    pub families: Families,
    /// Submission and acceptance tracking.
    pub tracker: Tracker,
    /// Loop settings.
    pub cfg: RunConfig,
    /// What the last step did.
    pub last: StepReport,
    /// Steps taken so far.
    pub steps: u64,
    /// Orders flagged possibly frozen, and the DAA score before which the matcher does not probe
    /// them again.
    pub frozen_until: BTreeMap<CovId, u64>,
    /// Quarantined orders, and the DAA score before which they stay out of the book.
    ///
    /// An order is quarantined when its planning panicked, the matcher's last line of defence, or
    /// when the builder refused it on its own. So one malformed order costs a skipped order, never
    /// the process or the other books.
    pub quarantined_until: BTreeMap<CovId, u64>,
    /// Refunds found unprofitable at their fixed tip, by (order, order outpoint). They are not
    /// built again.
    pub keeper_unprofitable: BTreeSet<(CovId, super::book::Outpoint)>,
    /// The keepers' floor fee rate when the entries of `keeper_unprofitable` were found.
    ///
    /// A lower floor forgets them, since a refund that did not pay at the old floor may pay at the
    /// new one.
    pub keeper_unprofitable_floor: u64,
    /// Keeper jobs that failed to build, by order outpoint.
    ///
    /// Each holds the DAA score before which the job is not built again, and the failures so far.
    pub keeper_failed: BTreeMap<super::book::Outpoint, (u64, u32)>,
    /// For diagnostics and tests, every transaction submitted, or built in a dry run.
    ///
    /// `None`, the default, keeps nothing.
    pub capture: Option<Vec<kob_protocol::tx::SignedTx>>,
    /// Outpoints that transactions of another part of the process spend while in flight.
    ///
    /// These are the x402 facilitator's swap-and-pay settlements, signed by their payers over the
    /// same book. The batches plan around them as around the runner's own pending transactions.
    pub reservations: Option<Reservations>,
    /// Why the book source is not ready, while the runner waits for it. It is logged once per wait.
    pub waiting: Option<String>,
    /// The spendable funding of the last step that read it.
    ///
    /// The metrics keep it while a step reads none.
    pub last_funding: Option<u64>,
    /// The whole P2PK balance of the last step that read it.
    pub last_funding_total: Option<u64>,
    /// What the surplus-inventory policy used in its current period, of `maxFee` and
    /// `tokens[].maxAmount`.
    pub inventory_use: super::planner::InventoryUse,
    /// The node's fee estimate between steps (`crate::fee`).
    pub fee_state: crate::fee::FeeState,
    /// The rates of the last step that planned, or the floor before the first.
    pub fee_rates: crate::fee::FeeRates,
    /// The matcher's configured rate, `engine.planner.fee_rate` at start.
    ///
    /// It is the lowest rate a batch is planned at.
    pub plan_base_rate: u64,
    /// Where the rates are published for the rest of the process, the x402 facilitator of
    /// `kobd --executor`.
    pub fee_board: Option<crate::fee::FeeBoard>,
    /// Kills and refunds this process submitted that are not accepted yet, by txid.
    ///
    /// See [`Runner::replace_stuck`].
    pub keeper_inflight: BTreeMap<[u8; 32], InFlightJob>,
    /// Keeper jobs submitted with an operator subsidy, for the metrics.
    pub keeper_subsidised: u64,
    /// The sompi of those subsidies.
    pub keeper_subsidy_sompi: u64,
    /// The operator subsidies of the rolling window (`KeeperConfig::subsidy_budget`).
    ///
    /// It holds every subsidy of a kill or refund this process submitted, including the increase of
    /// each replacement. It is kept in `RunConfig::state_dir`, so a restart continues the window.
    /// Without a state directory it is in memory only.
    pub keeper_subsidy: SubsidyBudget,
    /// The local clock of the subsidy window, unix seconds ([`SubsidyBudget`]). Tests set their
    /// own.
    pub subsidy_clock: fn() -> u64,
    /// Stuck kills and refunds replaced at a higher rate, for the metrics.
    pub keeper_replaced: u64,
    /// Replacements the node refused, for the metrics.
    pub keeper_replace_refused: u64,
    /// The weighted selection's random source, the share of each role and its counters
    /// (`kob_select_*`).
    pub selector: Selector,
    /// Orders the selection left for later, with the outpoint they had and the DAA score they were
    /// left at.
    ///
    /// One that another transaction takes before this process does counts as ceded
    /// (`kob_select_ceded_total`).
    pub ceded_watch: BTreeMap<CovId, (super::book::Outpoint, u64)>,
    /// The in-flight journal in `RunConfig::state_dir`. Never in a dry run.
    pub journal: Option<super::journal::Journal>,
    /// Earlier starts of this data directory (`RunConfig::state_dir`), 0 without one.
    pub restarts: u64,
    /// The local clock minus the chain's in seconds at the last check ([`Runner::check_clock`]).
    pub clock_skew: Option<i64>,
    /// When the clock was last checked.
    pub clock_checked: Option<std::time::Instant>,
    clock_logged: Option<std::time::Instant>,
    /// The last clock samples ([`chain_skew_secs`]), in seconds and oldest first.
    ///
    /// At most [`CLOCK_SAMPLES`] are kept.
    pub clock_samples: std::collections::VecDeque<i64>,
    /// The same samples with the local time in ms each was taken at.
    ///
    /// They tell whether the chain advances ([`clock_cause`]).
    clock_points: std::collections::VecDeque<(u64, i64)>,
    /// Why the skew is off, while it is beyond the bound ([`clock_cause`]).
    pub clock_cause: Option<ClockCause>,
    /// Day orders left alone this step because the clock is off and their deadline is near.
    ///
    /// See [`RunConfig::clock_skew_max_secs`].
    pub deadline_near: BTreeSet<CovId>,
    /// Steps that failed, for example with the node unreachable or the network wrong.
    pub tick_errors: u64,
    /// The last step's error, until a step succeeds.
    pub last_tick_error: Option<String>,
}

/// A source of reserved outpoints ([`Runner::reservations`]).
pub type Reservations = std::sync::Arc<dyn Fn() -> BTreeSet<super::book::Outpoint> + Send + Sync>;

/// Counts a start of the runner over `dir` and returns the starts before this one.
///
/// The count is a small file that holds the number of starts.
fn count_start(dir: &Path) -> u64 {
    let p = dir.join(STARTS_FILE);
    let before = std::fs::read_to_string(&p).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    if let Err(e) = std::fs::write(&p, (before + 1).to_string()) {
        tracing::debug!(error = %e, "cannot count the start");
    }
    before
}

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

/// The order each input of a matcher transaction belongs to ([`Tracked::owners`]).
///
/// An input is an order UTXO, whose covenant id is the order's, or the custody of a listed order
/// the transaction spends. So a mempool double spend backs off that order alone.
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

/// The address of every input that `owners` names, from the script the input spends.
///
/// After a missing-input refusal the node is asked which of them are still unspent.
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

/// The listed orders whose current UTXO the node no longer holds.
///
/// `utxos` is one answer of the node to an address query of every order, what it holds at those
/// addresses. The book lags the chain by the indexer's lag, seconds to tens of seconds on a busy
/// network. So an order filled, amended or cancelled meanwhile is still listed. A batch over it is
/// refused (`MissingInput`) and costs every other crossing of the batch a tick.
///
/// Only an order accepted at or before the node's virtual DAA score `node_daa` is judged, since a
/// node behind the book cannot hold a newer one. An answer that holds none of the queried orders is
/// no evidence, because the node's address index may not serve the query. Nothing is taken as spent
/// then.
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

/// The address of every listed order's current UTXO, its covenant script with the listed state.
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

/// DAA after its planning that a maintenance sale may still be sent again, five minutes.
///
/// Its price was the book's at that tick. A later sale is planned anew (`crate::maintenance`),
/// never resent at a stale price.
pub const MAINTENANCE_SALE_FRESH_DAA: u64 = 3_000;

/// What bounds a resend of a transaction that fills the orders `ids` of the book, planned at `daa`.
///
/// The bounds are the earliest day-order deadline (UTC) and the earliest soft expiry (DAA) of those
/// orders (`docs/spec/matcher.md` §2.3, `Tracker::stale`). An order already past its soft expiry
/// when planned is merged, not filled, so it bounds nothing.
fn fill_bounds(orders: &[ListedOrder], ids: &BTreeSet<CovId>, daa: u64) -> (Option<u64>, Option<u64>) {
    let (mut deadline, mut until): (Option<u64>, Option<u64>) = (None, None);
    for o in orders.iter().filter(|o| ids.contains(&o.id())) {
        if let Some(d) = o.deadline {
            deadline = Some(deadline.map_or(d, |x| x.min(d)));
        }
        // An unaccepted continuation of a chained step is accepted at or after `daa`.
        let udaa = if o.utxo_daa() == u64::MAX { daa } else { o.utxo_daa() };
        let due = o.order.state.refund_due(i64::try_from(udaa).unwrap_or(i64::MAX)).unwrap_or(i64::MAX);
        if let Ok(due) = u64::try_from(due) {
            if due > daa {
                until = Some(until.map_or(due, |x| x.min(due)));
            }
        }
    }
    (deadline, until)
}

impl<N: NodeApi, S: BookSource> Runner<N, S> {
    /// A runner over `node` and `source`, with what the state directory kept from the last run.
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
        // What must survive a restart. Never in a dry run, which submits nothing and must not
        // resend what a live run left.
        let state_dir = cfg.state_dir.clone().filter(|_| !cfg.dry_run);
        let restarts = state_dir.as_deref().map(count_start).unwrap_or(0);
        let keeper_subsidy = match state_dir.as_deref().map(|d| SubsidyBudget::load(&d.join(SUBSIDY_FILE))) {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "the keepers' subsidy window could not be read: it starts empty");
                SubsidyBudget::default()
            }
            None => SubsidyBudget::default(),
        };
        let (journal, restored) = match state_dir.as_deref().map(|d| super::journal::Journal::open(&d.join(super::journal::FILE_NAME)))
        {
            Some(Ok((j, r))) => (Some(j), r),
            Some(Err(e)) => {
                // What the journal holds is still reserved and settled with the node, when it can
                // be read at all. It is never planned over.
                let path = state_dir.as_deref().map(|d| d.join(super::journal::FILE_NAME)).unwrap_or_default();
                let restored = super::journal::read(&path).unwrap_or_default();
                tracing::error!(
                    error = %e,
                    file = %path.display(),
                    restored = restored.txs.len(),
                    "the in-flight journal cannot be written: what it held is tracked again, but what this run submits is not kept across a restart (free space or fix the file)"
                );
                (None, restored)
            }
            None => (None, Default::default()),
        };
        let mut tracker = tracker;
        let mut keeper_inflight = BTreeMap::new();
        let mut source = source;
        if !restored.txs.is_empty() || restored.skipped > 0 {
            tracing::info!(
                transactions = restored.txs.len(),
                skipped_lines = restored.skipped,
                "in-flight journal: transactions of the last run are tracked again; the first step resends them (idempotent) and the node's answer settles each"
            );
        }
        for (t, job) in restored.txs {
            source.watch(t.txid);
            if let Some(j) = job {
                keeper_inflight.insert(t.txid, j);
            }
            tracker.restored.insert(t.txid);
            tracker.txs.insert(t.txid, t);
        }
        Runner {
            journal,
            restarts,
            clock_skew: None,
            clock_checked: None,
            clock_logged: None,
            clock_samples: std::collections::VecDeque::new(),
            clock_points: std::collections::VecDeque::new(),
            clock_cause: None,
            deadline_near: BTreeSet::new(),
            tick_errors: 0,
            last_tick_error: None,
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
            keeper_inflight,
            keeper_subsidised: 0,
            keeper_subsidy_sompi: 0,
            keeper_subsidy,
            subsidy_clock: utc_now,
            keeper_replaced: 0,
            keeper_replace_refused: 0,
        }
    }

    /// Replace-by-fee of this process's own stuck kills and refunds (`docs/ops/executor.md`, fee
    /// policy).
    ///
    /// A job that no chain block accepted `keeper.replace_after_daa` DAA after its last submission
    /// is rebuilt from its own request, with the same order inputs and funding. One `spare` funding
    /// UTXO is added when it had none and the operator subsidises. It is rebuilt at its urgency's
    /// current rate, at least a quarter above its rate, within the policy's highest rate and the
    /// total cap ([`crate::keepers::reprice`]). It is sent with `submitTransactionReplacement`, at
    /// most `keeper.max_replacements` times.
    ///
    /// Only transactions the node took from this process are candidates
    /// ([`Runner::keeper_inflight`]). A job is left alone when a tracked transaction spends its
    /// outputs, as a chained keeper job does, when its submit had no verdict, or when a chain block
    /// accepted it. A refused replacement ends the replacements of that job, for example when the
    /// original was accepted meanwhile or another transaction spent the order. The tracker's
    /// pending timeout and backoff then handle it.
    ///
    /// A replacement may add subsidy only within the rolling budget
    /// (`KeeperConfig::subsidy_budget`, [`Runner::keeper_subsidy`]) and the step's subsidised-job
    /// cap (`subsidy_max_jobs`). Past either, the job's own tip and the subsidy it already has must
    /// pay it. Returns how many replacements added subsidy, which count toward the step's cap.
    pub async fn replace_stuck(&mut self, daa: u64, spare: &[KeyUtxo], rep: &mut StepReport) -> anyhow::Result<usize> {
        let tracker = &self.tracker;
        self.keeper_inflight.retain(|id, _| tracker.txs.get(id).is_some_and(|t| t.accepted.is_none()));
        let kc = self.cfg.keeper.clone();
        let mut subsidised = 0usize;
        if kc.max_replacements == 0 || self.cfg.dry_run {
            return Ok(subsidised);
        }
        // Transactions whose outputs a tracked transaction spends. Replacing one would orphan its
        // child.
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
            // The subsidy this replacement may have: what the job has, plus what the budget and the
            // step's cap still allow.
            let more = if subsidised < kc.subsidy_max_jobs { self.keeper_subsidy.left(&kc, daa, (self.subsidy_clock)()) } else { 0 };
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
                retired: None,
                ..t.clone()
            };
            self.source.watch(new);
            // Write-ahead. A kill now leaves the replacement in the journal for the next start to
            // settle.
            if let Some(jn) = self.journal.as_mut() {
                jn.add(&replacement, None);
            }
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
                    // No verdict, so the node may hold either one. Both are followed, the
                    // replacement as an unconfirmed submit that the tracker resends idempotently.
                    // Whichever a chain block accepts settles the order.
                    tracing::warn!(txid = %hex_old, error = %e, "replacement without a verdict; following both");
                    if let Some(e) = self.keeper_inflight.get_mut(&old) {
                        e.done = true;
                    }
                    // The replacement may be accepted, so its added subsidy counts against the
                    // budget.
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

    /// Charges `sompi` of operator subsidy to the rolling budget, and logs when that uses it up.
    ///
    /// The budget is kept in `RunConfig::state_dir`, so a restart continues the window.
    fn charge_subsidy(&mut self, daa: u64, sompi: u64) {
        let kc = &self.cfg.keeper;
        let now = (self.subsidy_clock)();
        let before = self.keeper_subsidy.left(kc, daa, now);
        self.keeper_subsidy.charge(kc, daa, now, sompi);
        if let Some(d) = self.cfg.state_dir.as_deref().filter(|_| !self.cfg.dry_run) {
            if let Err(e) = self.keeper_subsidy.save(&d.join(SUBSIDY_FILE)) {
                tracing::warn!(error = %e, "could not save the keepers' subsidy window (a restart would start it again)");
            }
        }
        if before > 0 && self.keeper_subsidy.left(kc, daa, now) == 0 {
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

    /// Counts the watched orders that another transaction took, and forgets those watched for long.
    ///
    /// An order was taken by another transaction when its outpoint is no longer listed and no
    /// transaction of this process spends it. An order watched for long was taken by nobody, or by
    /// this process after all.
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

    /// Outpoints no plan may spend.
    ///
    /// They are the inputs of the tracker's pending and unfinalized transactions, and the
    /// reservations.
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
        // Resend the rolled-back transactions that are still possible, idempotently. Not while the
        // kill switch is on, because `--pause-file` means nothing leaves the process.
        if !self.cfg.dry_run && !self.paused() {
            let resend: Vec<Tracked> = ev.rolled_back.iter().filter_map(|id| self.tracker.txs.get(id).cloned()).collect();
            for t in resend {
                let _ = self.node.submit(t.rpc_tx.clone()).await;
            }
        }
    }

    async fn follow_chain(&mut self, daa: u64, rep: &mut StepReport) -> Result<(), RpcError> {
        // The book source, the indexer, follows the chain itself: one follower, one truth.
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
                // The cursor block is unknown, pruned or reorged away. Restart from the sink.
                tracing::warn!(error = %m, "VSPC cursor lost; restarting from the sink");
                self.tracker.cursor = None;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// The spendable funding, the whole P2PK balance and the outpoints of every P2PK coin.
    ///
    /// The spendable funding is the P2PK UTXOs not in `spent`.
    async fn funding(
        &self,
        spent: &BTreeSet<super::book::Outpoint>,
    ) -> Result<(Vec<KeyUtxo>, u64, BTreeSet<super::book::Outpoint>), RpcError> {
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
        let coins = utxos.iter().map(|u| (u.transaction_id, u.index)).collect();
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
        Ok((free, total, coins))
    }

    /// Compares the local clock with the chain's, at most every [`CLOCK_CHECK_EVERY`].
    ///
    /// Day-order deadlines are UTC times of the placement record, which a matcher judges on its
    /// own clock (`docs/spec/matcher.md` §2.3). No covenant compares anything with real time. A day
    /// order's on-chain bound is its `expiryDaa`, a DAA score. This only checks the local clock and
    /// never replaces it.
    ///
    /// A block's timestamp is its miner's. So each sample takes the node tip nearest the local
    /// clock ([`chain_skew_secs`]), and one node that agrees outvotes a lying or stopped one. Only
    /// the median of the last [`CLOCK_SAMPLES`] samples is judged:
    /// - From the first sample after a start, samples are [`CLOCK_START_EVERY`] apart until there
    ///   are [`CLOCK_SAMPLES_MIN`], and the median is judged against [`CLOCK_EARLY_FACTOR`] times
    ///   the bound meanwhile.
    /// - Then a sample is taken every [`CLOCK_CHECK_EVERY`] and judged against the bound.
    ///
    /// The alarm names the likely cause ([`clock_cause`]): the local clock, or every node behind
    /// the network. Beyond `clock_skew_max_secs` the skew is logged as an error, again every ten
    /// minutes while it lasts. It is shown on the status line, in `/v1/health` and in the metrics.
    /// [`Runner::deadline_near`] then keeps out of every plan the day orders whose deadline is
    /// within the skew plus the bound of the local clock. Operations: `docs/ops/executor.md`,
    /// Part C, "Monitoring".
    async fn check_clock(&mut self) {
        let every = if self.clock_samples.len() < CLOCK_SAMPLES_MIN { CLOCK_START_EVERY } else { CLOCK_CHECK_EVERY };
        if self.cfg.clock_skew_max_secs == 0 || self.clock_checked.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.clock_checked = Some(std::time::Instant::now());
        let local = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let Some(sample) = chain_skew_secs(local, &self.node.tip_timestamps_ms().await) else {
            tracing::debug!("cannot read the chain's clock");
            return;
        };
        self.clock_samples.push_back(sample);
        self.clock_points.push_back((local, sample));
        while self.clock_samples.len() > CLOCK_SAMPLES {
            self.clock_samples.pop_front();
        }
        while self.clock_points.len() > CLOCK_SAMPLES {
            self.clock_points.pop_front();
        }
        let Some(skew) = median(&self.clock_samples) else { return };
        let max = self.cfg.clock_skew_max_secs as i64;
        // A block's timestamp is its miner's. On fewer samples only a skew far beyond the bound, or
        // one within it, counts.
        if self.clock_samples.len() < CLOCK_SAMPLES_MIN {
            let far = skew.unsigned_abs() > self.cfg.clock_skew_max_secs.saturating_mul(CLOCK_EARLY_FACTOR);
            if !far && skew.abs() > max {
                return;
            }
        }
        let was_off = self.clock_skew.is_some_and(|s| s.abs() > max);
        let cause = clock_cause(skew, &self.clock_points);
        let due = !was_off || self.clock_logged.is_none_or(|t| t.elapsed() >= LOW_FUNDS_LOG_EVERY);
        if skew.abs() > max && due {
            self.clock_logged = Some(std::time::Instant::now());
            tracing::error!(
                skew_seconds = skew,
                bound_seconds = max,
                "{}; day orders near their deadline are not filled meanwhile",
                cause.explain()
            );
        } else if was_off && skew.abs() <= max {
            tracing::info!(skew_seconds = skew, "the local clock agrees with the chain's again");
        }
        self.clock_skew = Some(skew);
        self.clock_cause = (skew.abs() > max).then_some(cause);
    }

    /// The local clock is beyond the bound from the chain's.
    pub fn clock_alarm(&self) -> bool {
        self.cfg.clock_skew_max_secs > 0 && self.clock_skew.is_some_and(|s| s.unsigned_abs() > self.cfg.clock_skew_max_secs)
    }

    /// The day orders of `orders` whose deadline is within the skew plus the bound of the local
    /// time `utc`, while the clock is off ([`Runner::clock_alarm`]).
    ///
    /// Whichever clock is right, a fill of them could fall after the maker's deadline.
    fn near_deadline(&self, orders: &[ListedOrder], utc: u64) -> BTreeSet<CovId> {
        if !self.clock_alarm() {
            return BTreeSet::new();
        }
        let margin = self.clock_skew.unwrap_or(0).unsigned_abs().saturating_add(self.cfg.clock_skew_max_secs);
        orders.iter().filter(|o| o.deadline.is_some_and(|d| d.abs_diff(utc) <= margin)).map(|o| o.id()).collect()
    }

    /// Sets the fee rates of a step that plans (`crate::fee`).
    ///
    /// The node's estimate is read at most every `refresh_ms`. The last good one is used up to
    /// `max_age_ms`, then the floor. The rates go to the matcher, the keepers and the maintenance
    /// jobs.
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

    /// [`gone_orders`] of the book, asked of the node in one query.
    ///
    /// Without an answer nothing is taken as spent.
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
        // Write-ahead (`super::journal`). A kill between here and the node's answer leaves the
        // entry for the next start to settle.
        if let Some(j) = self.journal.as_mut() {
            j.add(&t, None);
        }
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
        // A submit that got no answer may have reached the node. Settle the doubt before planning
        // anything over its inputs. A resend is checked first (`Tracker::stale`), because the
        // planning gates saw the transaction when it was planned, not now.
        if !self.cfg.dry_run {
            let margin = self.cfg.clock_skew_max_secs.saturating_add(self.clock_skew.unwrap_or(0).unsigned_abs());
            let resend = ResendClock { daa, utc: utc_now(), margin_secs: margin };
            for (id, outcome) in self.tracker.resubmit_unconfirmed(&self.node, &resend).await {
                tracing::info!(txid = %kob_protocol::json::to_hex(&id), ?outcome, "resent a submit that had no verdict");
                match outcome {
                    SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => rep.conflicts.push(id),
                    SubmitOutcome::Rejected => rep.rejected.push((id, outcome)),
                    _ => {}
                }
            }
            // A pending transaction that no block accepted for a while. The node says whether it
            // still holds it. It is never forgotten while the node does, or its funding coin would
            // refuse every later batch as a mempool double spend.
            let r = self.tracker.recheck_pending(&self.node, &resend, true).await;
            for (t, why) in &r.dropped {
                tracing::warn!(
                    txid = %kob_protocol::json::to_hex(&t.txid),
                    kind = %t.kind,
                    waited_daa = daa.saturating_sub(t.submitted_daa),
                    %why,
                    "a pending transaction was refused when checked again (its inputs went elsewhere); dropped"
                );
                rep.dropped.push(t.txid);
            }
            for id in &r.rebroadcast {
                tracing::info!(txid = %kob_protocol::json::to_hex(id), "a pending transaction the node no longer held was sent again");
            }
            if !r.kept.is_empty() {
                tracing::debug!(kept = r.kept.len(), "pending transactions still held by the node");
            }
        }
        // Nothing is planned over a store that lags the chain. After a restart the store is where
        // the last run left it, seconds to hours behind. So the orders and the operator's tokens it
        // lists may be spent already, and a transaction over them fails with `MissingInput`.
        // Acceptance tracking above continues.
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
        // A retired transaction that no node's mempool holds any more, twice in a row, is not
        // waited for until its hold ends. Its inputs are free again. An unsure answer keeps the
        // hold.
        for id in self.tracker.retired_to_check(daa) {
            let held = self.node.in_mempool(id).await.ok();
            if let Some(t) = self.tracker.retired_checked(&id, held, daa) {
                tracing::info!(
                    txid = %kob_protocol::json::to_hex(&t.txid),
                    kind = %t.kind,
                    "a transaction no longer sent is in no node's mempool: forgotten before its hold ends, its inputs are free again"
                );
            }
        }
        // The store is current, so a retired transaction it does not show accepted by now was not
        // mined.
        for t in self.tracker.forget_retired(daa) {
            tracing::info!(
                txid = %kob_protocol::json::to_hex(&t.txid),
                kind = %t.kind,
                "a transaction no longer sent was not mined: forgotten, its inputs are free again"
            );
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
        // A pending transaction whose order input the book no longer lists lost its race: its other
        // orders are free again.
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
        let (funding, total, coins) = self.funding(&spent).await?;
        // A held funding coin that the node no longer lists is released, since its spender was
        // mined.
        self.tracker.note_operator_coins(coins, daa);
        rep.funding = Some(funding.iter().map(|k| k.utxo.amount).sum());
        rep.funding_total = Some(total);
        self.last_funding = rep.funding;
        self.last_funding_total = rep.funding_total;
        self.check_clock().await;
        let clock = Clock { daa, utc: utc_now() };
        self.deadline_near = self.near_deadline(&book.orders, clock.utc);
        // The orders the node no longer holds, because the book lags the chain. Neither the matcher
        // nor the keepers plan them. The node is asked only while the book lags by
        // `verify_live_min_lag`, since the round trip costs races when the book is current.
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
        excluded.extend(self.deadline_near.iter().copied());

        if self.cfg.roles.matcher {
            // The inventory policy's budgets of the period, which bound what the tick may keep.
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
            // Orders that a token program refuses in the engine pre-simulation are flagged and
            // probed again later. Those whose pre-simulation passed lose the flag.
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
                    info_unless!(
                        self.cfg.dry_run,
                        txid = %kob_protocol::json::to_hex(&p.txid()),
                        triggered = p.plan.triggered(),
                        updates = p.plan.updates.len(),
                        "the batch triggers or arms stops"
                    );
                }
                for k in &p.plan.kept {
                    info_unless!(
                        self.cfg.dry_run,
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
                let orders = p.plan.spent_ids();
                let (deadline, resend_until) = fill_bounds(&book.orders, &orders, daa);
                let t = Tracked {
                    txid: p.txid(),
                    spends: p.signed.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect(),
                    orders,
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
                    deadline,
                    resend_until,
                    retired: None,
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
            // This process's stuck kills and refunds go first. Their replacements keep their
            // inputs, and may take a funding UTXO.
            let spare: Vec<KeyUtxo> = funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect();
            let replaced_subsidised = self.replace_stuck(daa, &spare, &mut rep).await?;
            spent = self.reserved_or_spent();
            let funding: Vec<KeyUtxo> = funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect();
            // The listed orders plus the proven unlisted ones, for kills, refunds and closes only,
            // since the keepers never match. The unlisted ones are the snapshot's `keeper_only`, of
            // an indexer or a `--book-file`, and the source's.
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
            // A token program refused to move these in the matcher's pre-simulation, so a refund
            // would fail the same way.
            excluded.extend(self.frozen_until.keys().copied());
            // Forget the orders that moved on.
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
            // The subsidy this step may still hand out: the rolling budget left, and the step's cap
            // less the replacements'.
            let allowance = SubsidyAllowance {
                sompi: self.keeper_subsidy.left(&self.cfg.keeper, daa, (self.subsidy_clock)()),
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
            // Bounded retry of jobs that failed to build, with a growing backoff per order
            // outpoint. A new outpoint starts over.
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
                    deadline: None,
                    resend_until: None,
                    retired: None,
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
                // A submit without a verdict may still be accepted, so its subsidy counts against
                // the budget too.
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

    /// The maintenance jobs (`crate::maintenance`), which merge or sell the operator's token UTXOs.
    ///
    /// They run after the matcher's and the keepers' transactions of the tick, and leave their
    /// inputs alone.
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
        excluded.extend(self.deadline_near.iter().copied());
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
            info_unless!(
                self.cfg.dry_run,
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
            let filled: BTreeSet<CovId> = j.order.into_iter().collect();
            // A sale fills a bid at the book's price of this tick. It is never sent again once that
            // price is stale.
            let (deadline, mut resend_until) = fill_bounds(orders, &filled, daa);
            if matches!(j.kind, crate::maintenance::MaintKind::Sell) {
                let fresh = daa.saturating_add(MAINTENANCE_SALE_FRESH_DAA);
                resend_until = Some(resend_until.map_or(fresh, |u| u.min(fresh)));
            }
            let t = Tracked {
                txid: j.signed.tx.id,
                spends: j.spends.clone(),
                orders: filled,
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
                deadline,
                resend_until,
                retired: None,
            };
            self.send(t, daa, rep).await;
        }
        Ok(())
    }

    fn finish(&mut self, rep: &StepReport) {
        self.steps += 1;
        self.last = rep.clone();
        self.last_tick_error = None;
        if rep.daa > 0 {
            self.tracker.prune_backoff(rep.daa);
        }
        self.publish(rep);
    }

    /// Publishes the step's state after every step, a failed one included.
    ///
    /// It brings the journal in line with the tracker, writes the metrics file, and hands the
    /// runner's facts to the book source for `/v1/health` and `/v1/metrics`.
    fn publish(&mut self, rep: &StepReport) {
        if let Some(j) = self.journal.as_mut() {
            j.sync(&self.tracker.txs, &self.keeper_inflight);
        }
        if let Some(p) = self.cfg.metrics_file.clone() {
            if let Err(e) = write_metrics(&p, self, rep) {
                tracing::warn!(error = %e, "cannot write metrics");
            }
        }
        let mut health = self.executor_health(rep);
        let mut metrics = metrics_text(self, rep);
        if !self.cfg.report_funds {
            (health.funds_sompi, health.spendable_sompi, health.funds_hidden) = (None, None, true);
            metrics = without_funds(&metrics);
        }
        self.source.report_executor(health, metrics);
    }

    /// The runner's part of `/v1/health` after the step that reported `rep`.
    pub fn executor_health(&self, rep: &StepReport) -> crate::indexer::status::ExecutorHealth {
        let t = &self.tracker;
        let total = rep.funding_total.or(self.last_funding_total);
        crate::indexer::status::ExecutorHealth {
            funds_sompi: total.map(|v| v.to_string()),
            spendable_sompi: rep.funding.or(self.last_funding).map(|v| v.to_string()),
            funds_hidden: false,
            low_funds: total.is_some_and(|f| f < self.cfg.low_funds) && !self.cfg.dry_run,
            low_funds_threshold_sompi: self.cfg.low_funds.to_string(),
            paused: rep.paused,
            dry_run: self.cfg.dry_run,
            pending: t.pending(),
            oldest_pending_seconds: t.oldest_pending_daa(rep.daa) / 10,
            submitted_total: t.submitted,
            finalized_total: t.finalized_count,
            dropped_total: t.dropped_total,
            rejected_total: t.rejected,
            held_coins: t.held.len(),
            last_submit_unix: (t.last_submit_unix > 0).then_some(t.last_submit_unix),
            last_step_unix: utc_now(),
            last_tick_error: self.last_tick_error.clone(),
            clock_skew_seconds: self.clock_skew,
            clock_alarm: self.clock_alarm(),
            restarts: self.restarts,
            journal_entries: self.journal.as_ref().map(|j| j.live().count()).unwrap_or(0),
        }
    }

    /// Runs until `shutdown` resolves.
    ///
    /// A step in flight at that moment gets [`STEP_GRACE`] to end, for a transaction being
    /// submitted, and is dropped after that. A request to a node that does not answer could
    /// otherwise take its whole timeout.
    pub async fn run(&mut self, shutdown: impl std::future::Future<Output = ()>) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        // The first status line comes after the first tick, then one every `status_every`.
        let mut status_at: Option<std::time::Instant> = None;
        // When and at what balance the low-funds warning was last logged. The status line shows
        // the funds every `--status-secs`.
        let mut low_funds_logged: Option<(std::time::Instant, u64)> = None;
        // Since when the book source has been halted (`BookSource::fatal`).
        let mut halted_since: Option<std::time::Instant> = None;
        // The reasons of the last tick's skipped books and jobs. A new reason is logged at INFO,
        // and a repeated one at DEBUG.
        let mut skipped_before: BTreeSet<String> = BTreeSet::new();
        // The build, logged once. The status line does not repeat it.
        tracing::info!("{}", crate::version::build_line());
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
                        // The per-tick detail. The submissions themselves are logged at INFO where
                        // they are sent.
                        tracing::debug!(
                            daa = r.daa,
                            built = r.built.len(),
                            submitted = r.submitted.len(),
                            conflicts = r.conflicts.len(),
                            skipped = r.skipped.len(),
                            gone = r.gone,
                            "tick"
                        );
                    }
                    // Why a book or job was skipped, such as an engine rejection, unprofitable or
                    // too large. It is the operator's only trace of a batch that keeps failing. A
                    // reason is logged at INFO when it appears, and at DEBUG while it repeats tick
                    // after tick.
                    for why in &r.skipped {
                        info_unless!(skipped_before.contains(why), why = %why, "skipped");
                    }
                    skipped_before = r.skipped.iter().cloned().collect();
                    // Only a funding actually read is judged. A step that planned nothing read
                    // none, which is not `0`. The judgement is on the whole balance, because coins
                    // that pending transactions hold are still the operator's. A busy operator's
                    // free coins are often a few KAS while its balance is large.
                    let low = r.funding_total.filter(|f| *f < self.cfg.low_funds && !self.cfg.dry_run);
                    if low.is_none() && r.funding_total.is_some() {
                        // Funded again, so the next fall below the threshold is reported at once.
                        low_funds_logged = None;
                    }
                    if let Some(f) = low.filter(|f| low_funds_due(low_funds_logged, *f, std::time::Instant::now())) {
                        low_funds_logged = Some((std::time::Instant::now(), f));
                        tracing::warn!(
                            funding = f,
                            spendable = r.funding.unwrap_or(0),
                            address = %crate::matcher::wallet::address_of(&self.signer.pubkey(), &self.cfg.network),
                            "operator funds are low: send KAS to the address"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "tick failed");
                    // A failed step still writes the metrics and the health, with the failure
                    // counted, so a monitor sees it.
                    self.tick_errors += 1;
                    self.last_tick_error = Some(e.to_string());
                    let last = self.last.clone();
                    self.publish(&last);
                }
            }
            // A halted indexer, or a dead read API, does not come back by itself. End, so that the
            // supervisor restarts the process.
            match self.source.fatal() {
                Some((now, why)) => {
                    let since = *halted_since.get_or_insert_with(std::time::Instant::now);
                    if now || since.elapsed() >= self.cfg.halt_exit_after {
                        tracing::error!(%why, "exiting so that a supervisor restarts the executor");
                        anyhow::bail!("{why}: the executor stopped (exit status 1) so that its supervisor restarts it");
                    }
                    if since.elapsed() < self.cfg.tick {
                        tracing::error!(
                            %why,
                            exit_after_secs = self.cfg.halt_exit_after.as_secs(),
                            "the executor exits unless this ends"
                        );
                    }
                }
                None => halted_since = None,
            }
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                _ = tokio::time::sleep(self.cfg.tick) => {}
            }
        }
    }
}

/// How long the low-funds warning stays quiet after it was logged, unless the balance moves a lot.
const LOW_FUNDS_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

/// Whether the low-funds warning for `funding`, in sompi, is due.
///
/// It is due the first time, every ten minutes after, or when the balance is a quarter, and at
/// least 1 KAS, above or below the one last reported. `last` is when and at what balance it was
/// last logged.
fn low_funds_due(last: Option<(std::time::Instant, u64)>, funding: u64, now: std::time::Instant) -> bool {
    let Some((at, then)) = last else { return true };
    now.saturating_duration_since(at) >= LOW_FUNDS_LOG_EVERY || funding.abs_diff(then) >= (then / 4).max(100_000_000)
}

/// What the console status line ([`status_line`]) reports.
///
/// That is the book source's own line, the last step's state, the tracker's totals since the start
/// and the operator's funds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusFacts {
    /// The book source's own part of the line.
    pub source: Option<String>,
    /// Whether the node is synced.
    pub synced: bool,
    /// Whether the kill switch is on.
    pub paused: bool,
    /// Why the book source is not ready.
    pub not_ready: Option<String>,
    /// The book lags the node too far to plan against.
    pub book_stale: bool,
    /// Transactions submitted since the start.
    pub submitted: u64,
    /// Transactions final since the start.
    pub finalized: u64,
    /// Operator profit of the final transactions, in sompi.
    pub profit_sompi: i64,
    /// Transactions pending now.
    pub pending: usize,
    /// Transactions the node rejected since the start.
    pub rejected: u64,
    /// The operator's whole P2PK balance, when read.
    pub funding_total: Option<u64>,
    /// The low-funds threshold, in sompi.
    pub low_funds: u64,
    /// The operator's address, named in the low-funds warning as where to send KAS.
    pub funding_address: Option<String>,
    /// A dry run, which submits nothing.
    pub dry_run: bool,
    /// Why the last tick failed, for example with the node unreachable or the network wrong.
    pub tick_error: Option<String>,
    /// Transactions given up since the start, refused when checked again or lost races.
    pub dropped: u64,
    /// Seconds the oldest pending transaction has waited.
    pub oldest_pending_secs: u64,
    /// The local clock's skew from the chain's in seconds, while it is beyond the bound.
    pub clock_skew_alarm: Option<i64>,
    /// What that skew most likely means ([`clock_cause`]). `None` means no alarm.
    pub clock_cause: Option<ClockCause>,
    /// The build ([`crate::version::build_line`]). `None` means it is not shown.
    pub build: Option<String>,
}

/// A pending transaction waiting longer than this is named on the status line.
pub const PENDING_WARN_SECS: u64 = 300;

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
            dropped: t.dropped_total,
            oldest_pending_secs: t.oldest_pending_daa(rep.daa) / 10,
            clock_skew_alarm: self.clock_skew.filter(|_| self.clock_alarm()),
            clock_cause: self.clock_cause.filter(|_| self.clock_alarm()),
            // The build is logged once at the start, not on every status line.
            build: None,
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

/// The one-line status that an operator watching the console reads.
///
/// `kobd --executor --status-secs` logs it, every 10 s by default, as kaspad prints its "Processed
/// N blocks in the last 10s" line. It says where the book is, what the executor sent and earned
/// since the start, its funds, the mode, and what needs attention. What does not change, such as
/// the build and the configuration, is logged once at the start instead. The metrics file
/// (`--metrics-file`) carries the same facts for monitoring.
pub fn status_line(f: &StatusFacts) -> String {
    let mut parts = vec![];
    if f.dry_run {
        // The operator's choice, not a warning.
        parts.push("dry run (nothing is submitted)".to_string());
    }
    if let Some(s) = &f.source {
        parts.push(s.clone());
    }
    parts.push(format!("sent {}, final {}, pending {}, profit {} KAS", f.submitted, f.finalized, f.pending, kas_text(f.profit_sompi)));
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
    if f.dropped > 0 {
        warn.push(format!("{} transactions dropped (refused when checked again, or lost a race)", f.dropped));
    }
    if f.pending > 0 && f.oldest_pending_secs >= PENDING_WARN_SECS {
        warn.push(format!(
            "a transaction has been pending for {} (the node holds it; its inputs stay reserved)",
            crate::indexer::status::human_secs(f.oldest_pending_secs)
        ));
    }
    if let Some(s) = f.clock_skew_alarm {
        let cause = f.clock_cause.unwrap_or(if s < 0 { ClockCause::ClockBehind } else { ClockCause::AheadOrNodesBehind });
        warn.push(format!("{}; day orders near their deadline are not filled", cause.describe(s)));
    }
    parts.push(if warn.is_empty() { "no warnings".to_string() } else { format!("WARNING: {}", warn.join("; ")) });
    if let Some(b) = &f.build {
        parts.push(b.clone());
    }
    parts.join(" | ")
}

/// The runner's metrics in the Prometheus text format, for the textfile collector.
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
    g(
        "kob_matcher_oldest_pending_seconds",
        "Seconds the oldest pending transaction has waited (at 10 DAA per second; 0: none pending).",
        (t.oldest_pending_daa(rep.daa) / 10).to_string(),
    );
    g(
        "kob_matcher_dropped_total",
        "Transactions given up: refused by the node when checked again, or a race lost (their orders back off).",
        t.dropped_total.to_string(),
    );
    g(
        "kob_matcher_resend_dropped_total",
        "Pending transactions not sent again because they would no longer be valid (a day order's deadline, an order's expiry, a maintenance sale's stale price, a journal entry too old to send blindly).",
        t.resend_dropped_total.to_string(),
    );
    g(
        "kob_matcher_held_coins",
        "Funding coins held out of the funding: a mempool transaction this process does not track spends them.",
        t.held.len().to_string(),
    );
    g("kob_matcher_held_coins_total", "Funding coins held since the start.", t.held_total.to_string());
    g(
        "kob_matcher_last_submit_seconds",
        "Unix time of the last submission a node took (0: none since the start).",
        t.last_submit_unix.to_string(),
    );
    g("kob_matcher_tick_errors_total", "Steps that failed (the node unreachable, the network wrong, ...).", r.tick_errors.to_string());
    g("kob_matcher_last_tick_ok", "1 when the last step succeeded.", (r.last_tick_error.is_none() as u8).to_string());
    g(
        "kob_matcher_journal_entries",
        "Transactions in the in-flight journal (what a restart resends).",
        r.journal.as_ref().map(|j| j.live().count()).unwrap_or(0).to_string(),
    );
    g("kob_executor_restarts_total", "Earlier starts of this data directory (process restarts).", r.restarts.to_string());
    g(
        "kob_clock_skew_seconds",
        "The local clock minus the chain's (the newest chain block's timestamp), seconds (0: not measured).",
        r.clock_skew.unwrap_or(0).to_string(),
    );
    g(
        "kob_clock_skew_alarm",
        "1 while the local clock is beyond the bound from the chain's (day orders near their deadline are not filled).",
        (r.clock_alarm() as u8).to_string(),
    );
    // The last funding read, since a step that waited read none. It is unknown before the first
    // read, which raises no alert.
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
        r.keeper_subsidy.left(&r.cfg.keeper, rep.daa, (r.subsidy_clock)()).to_string(),
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
    let mut extra = r.source.metrics();
    if extra.is_empty() {
        // A book source without metrics of its own, such as a book file. The build is still named.
        extra = crate::version::build_info_metric();
    }
    std::fs::write(&tmp, metrics_text(r, rep) + &extra)?;
    std::fs::rename(tmp, p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_low_funds_warning_is_logged_now_and_then() {
        use super::{low_funds_due, LOW_FUNDS_LOG_EVERY};
        let t0 = std::time::Instant::now();
        let kas = 100_000_000u64;
        assert!(low_funds_due(None, 30 * kas, t0), "the first time");
        let last = Some((t0, 30 * kas));
        let soon = t0 + std::time::Duration::from_secs(60);
        assert!(!low_funds_due(last, 30 * kas, soon) && !low_funds_due(last, 31 * kas, soon), "the same balance stays quiet");
        assert!(low_funds_due(last, 20 * kas, soon) && low_funds_due(last, 40 * kas, soon), "a big move is reported");
        assert!(low_funds_due(last, 30 * kas, t0 + LOW_FUNDS_LOG_EVERY), "and every ten minutes");
        let empty = Some((t0, 0));
        assert!(!low_funds_due(empty, 0, soon) && low_funds_due(empty, 2 * kas, soon), "an empty key does not spam");
    }

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
            "indexer following, caught up (2 DAA behind the node), 9 orders indexed | sent 14, final 12, pending 1, profit 0.42 KAS \
             | funds 182.4 KAS | no warnings"
        );
        // A dry run is a mode, not a warning.
        f.dry_run = true;
        assert!(
            status_line(&f).starts_with("dry run (nothing is submitted) | indexer following")
                && status_line(&f).ends_with("| no warnings")
        );
        f.dry_run = false;
        f.source = None;
        f.funding_total = Some(5_000_000_000);
        f.paused = true;
        f.not_ready = Some("indexer catching_up, 900 DAA behind the node".into());
        f.rejected = 2;
        let s = status_line(&f);
        assert!(s.starts_with("sent 14, final 12"), "{s}");
        assert!(
            s.ends_with(
                "funds 50 KAS | WARNING: PAUSED (the pause file exists); waiting for the indexer (indexer catching_up, 900 DAA behind \
                 the node): nothing is planned; funds low (50 KAS, below 100 KAS); 2 transactions rejected by the node (see the log)"
            ),
            "{s}"
        );
        // With the address known, the warning says where to send KAS.
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
