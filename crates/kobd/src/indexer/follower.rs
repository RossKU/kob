//! The VSPC v2 follower, which polls the node from the stored cursor and commits atomically.
//!
//! # Failure policy
//!
//! The cursor is never advanced on any of these:
//! - A transport error, a timeout, or `consensus is currently in a transitional ibd state`: back
//!   off and retry.
//! - `cannot find header`, which means the node does not know the cursor: walk the stored chain
//!   back to the newest block the node still knows, revert everything above it, and continue from
//!   there. If none is known, or the node says `the queried hash does not have retention root on
//!   its chain`, the downtime exceeded the retention of the node. The state is `gap`, there is no
//!   further progress, and the operator bootstraps the store (see the docs).
//! - With a cross-checking node (the public pair), `cannot find header` and `retention root` count
//!   only when that node confirms them (`ChainSource::confirm_unknown_cursor`). Otherwise nothing
//!   is rewound or halted, and the follower retries.
//! - A `removed` list that contradicts the stored chain: state `gap`. This is a different node or
//!   a bug, and the follower never guesses.
//! - A record-log write that failed, for example on a full disk or an I/O error. The append is
//!   rolled back. The state is `stopped`, and nothing more is appended until the operator
//!   restarts.
//!
//! # Batch size
//!
//! Every request is bounded to a window of blue score ahead of the cursor (`rpc::window`). A
//! follower that fell behind under a transaction flood pages through the backlog in batches it can
//! fetch within its timeout, instead of asking for the whole backlog at once. A timed-out batch is
//! split, so the window shrinks. A fast batch grows the window. A request is never repeated
//! unchanged after a timeout.
//!
//! # Parallel fetch
//!
//! With `fetch_parallel` > 1, the follower fetches several upcoming windows at once over separate
//! connections and applies them in chain order ([`super::prefetch`]). It does so while the cursor
//! is more than `prefetch_min_lag_blue` behind the sink and at least a first window of chain
//! blocks remains. Near the sink, and whenever the plan stops matching the chain of the node, it
//! takes the single steps above.
//!
//! A plan ends at the sink. Nothing more is planned once an applied window leaves the cursor
//! within the threshold, or once the chain after the tail of the plan is shorter than a first
//! window. The windows already fetched are applied, and the single steps follow the sink from
//! there.
//!
//! # A failing chain authority
//!
//! A node can stay reachable and answer its small calls while its chain requests (a VSPC batch,
//! the chain hashes, its copy of a window from another node) time out or lose the connection.
//! Once [`FollowerConfig::authority_failures`] of them failed in a row and the cursor has not
//! moved for [`FollowerConfig::authority_stall`] (the local monotonic clock), the follower tells
//! the source ([`ChainSource::authority_failing`]). With several nodes, another node that can lead
//! becomes the chain authority (`rpc::multi`), and the follower asks it at once
//! ([`StepOutcome::Handover`]). With a single node nothing changes: it retries with its backoff.

/// The text of a gap reason that says the node pruned the history the cursor stood in.
///
/// That is the case for a store that began at the pruning point of the node, and for a downtime
/// that exceeded its retention. A recovering follower ([`super::recovery`]) moves the store to the
/// pruning point of the node and follows from there with a new follower.
pub const PRUNED_GAP: &str = "downtime exceeded";

/// Whether a gap reason says the node pruned the history the cursor stood in, see [`PRUNED_GAP`].
pub fn is_pruned_gap(reason: &str) -> bool {
    reason.contains(PRUNED_GAP)
}

use super::ingest::{Cursor, Ingest, IngestError};
use super::prefetch::{Head, Prefetch, PrefetchLimits};
use super::status::{BackfillPace, CatchUp, FollowerState, HealthState, IndexEvent};
use crate::config::StartMode;
use crate::hex::Hash32;
use crate::rpc::types::{RawVspcResponse, Verbosity, VspcBatch, VspcRequest};
use crate::rpc::window::{BatchWindow, WindowPlan};
use crate::rpc::{ChainSource, NodeErrorKind, Origin, RpcError, WindowRequest};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

/// How often a halted follower repeats that it is halted, as an error line.
pub const HALT_REMIND: Duration = Duration::from_secs(600);

/// Chain requests of the authority that fail on the wire in a row (a timeout, a lost connection, an
/// undecodable answer) ...
pub const AUTHORITY_FAILURES: u32 = 2;
/// ... while the cursor does not move for this long, before the source is told that the authority
/// is failing (module documentation).
pub const AUTHORITY_STALL: Duration = Duration::from_secs(180);

/// Settings of a [`Follower`].
#[derive(Debug, Clone)]
pub struct FollowerConfig {
    /// The network the node must be on.
    pub network: String,
    /// Where a store with no cursor starts.
    pub start: StartMode,
    /// The pause between polls once the cursor is at the sink.
    pub poll_interval: Duration,
    /// The first retry delay. It doubles with each consecutive failure.
    pub backoff: Duration,
    /// The longest retry delay.
    pub max_backoff: Duration,
    /// Passed to the node as `minConfirmationCount`. `None` follows the tip, which is the default:
    /// the API reports confirmations per event and lets consumers choose the tier.
    pub min_confirmations: Option<u64>,
    /// How many stored blocks the walk-back considers when the node does not know the cursor.
    pub max_walkback_blocks: u64,
    /// Refreshes the node status (`getBlockDagInfo`) at most this often.
    pub status_refresh: Duration,
    /// The lag below which the follower reports `following` rather than `catching_up`.
    pub caught_up_daa: u64,
    /// The first batch window in blue score, about one block each at 10 BPS. It adapts from there.
    pub batch_initial_blue: u64,
    /// A batch should take about this long to fetch. The window shrinks above it and grows below
    /// half of it.
    pub batch_target: Duration,
    /// VSPC windows fetched at once while catching up. 1 fetches one at a time. The source needs
    /// that many connections.
    pub fetch_parallel: usize,
    /// Bytes of windows held ahead of the cursor, fetched or estimated in flight.
    pub prefetch_max_bytes: u64,
    /// Parallel fetch runs only while the cursor is more than this far behind the sink, in blue
    /// score.
    pub prefetch_min_lag_blue: u64,
    /// The first parallel window, in chain blocks. It adapts from there.
    pub prefetch_initial_blocks: usize,
    /// Chain requests of the authority that fail on the wire in a row ... ([`AUTHORITY_FAILURES`])
    pub authority_failures: u32,
    /// ... while the cursor does not move for this long, before another node may take over as the
    /// chain authority ([`AUTHORITY_STALL`], module documentation).
    pub authority_stall: Duration,
}

impl FollowerConfig {
    /// Builds the follower settings from the indexer configuration.
    pub fn from_indexer(c: &crate::config::IndexerConfig) -> Self {
        FollowerConfig {
            network: c.network.clone(),
            start: c.start.clone(),
            poll_interval: Duration::from_millis(c.poll_interval_ms),
            backoff: Duration::from_millis(c.backoff_ms),
            max_backoff: Duration::from_millis(c.max_backoff_ms),
            min_confirmations: None,
            max_walkback_blocks: c.max_walkback_blocks,
            status_refresh: Duration::from_secs(2),
            caught_up_daa: 100,
            batch_initial_blue: 600,
            batch_target: (Duration::from_secs(c.rpc_timeout_secs) / 6).max(Duration::from_secs(1)),
            fetch_parallel: c.fetch_windows(),
            prefetch_max_bytes: c.prefetch_max_mb.saturating_mul(1 << 20),
            prefetch_min_lag_blue: c.prefetch_min_lag_blue,
            prefetch_initial_blocks: 64,
            authority_failures: AUTHORITY_FAILURES,
            authority_stall: AUTHORITY_STALL,
        }
    }

    fn window(&self) -> BatchWindow {
        BatchWindow::new(self.batch_initial_blue, 1, 1 << 20, self.batch_target)
    }

    fn prefetch_limits(&self) -> PrefetchLimits {
        PrefetchLimits {
            parallel: self.fetch_parallel.max(1),
            max_bytes: self.prefetch_max_bytes,
            // Windows in flight share the link, so each may take longer than a lone batch: up to
            // half the request timeout.
            target: self.batch_target.saturating_mul(self.fetch_parallel.clamp(1, 3) as u32),
            initial_blocks: self.prefetch_initial_blocks,
            min_confirmations: self.min_confirmations,
        }
    }
}

/// What one step of the follower did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// A batch was committed. `more` is true when the node likely has more chain to give at once.
    Applied { added: usize, removed: usize, more: bool },
    /// The cursor is at the node's sink.
    Idle,
    /// Cursor rewound to an older stored block the node still knows.
    Rewound { blocks: usize },
    /// A bounded request came back without a chain block, because the next one lies beyond the
    /// window. The window grew, so ask again at once.
    Widened { window: u64 },
    /// A transient failure. Back off.
    Retry(String),
    /// An unrecoverable failure. It needs operator action.
    Gap(String),
    /// Writing the record log failed, for example on a full disk or an I/O error. The follower
    /// stops and appends nothing more until the operator has fixed the cause and restarted. The
    /// `open` of the restart checks the log.
    Halted(String),
    /// The authority's chain requests kept failing and another node is the chain authority now
    /// (module documentation). Ask it at once.
    Handover(String),
}

/// How a batch about to be committed was fetched.
struct Committed {
    /// The chain block it was requested from, which must be the cursor.
    start: Hash32,
    /// The window, not the sink, ended it, so more chain follows.
    by_window: bool,
    t_fetch: Duration,
    t_parse: Duration,
    /// The blue score of its last chain block, when known. Prefetched windows learn it.
    end_blue: Option<u64>,
    /// It is one of the windows fetched ahead. The sequential window learns nothing from it.
    prefetched: bool,
}

/// Follows the VSPC v2 chain of a node into the store.
pub struct Follower<S: ChainSource> {
    source: Arc<S>,
    ingest: Arc<Mutex<Ingest>>,
    cfg: FollowerConfig,
    health: Arc<HealthState>,
    events: broadcast::Sender<Arc<IndexEvent>>,
    last_status: Mutex<Option<std::time::Instant>>,
    window: Mutex<BatchWindow>,
    /// The blue score of the cursor block, once known from a header or `getBlock`.
    cursor_blue: Mutex<Option<(Hash32, u64)>>,
    /// The windows fetched ahead of the cursor (parallel fetch).
    prefetch: tokio::sync::Mutex<Prefetch>,
    /// When the next pruning batch of lean storage or archive pruning is due. `None`: no pruning.
    lean_next: Mutex<Option<std::time::Instant>>,
    /// The catch-up pace, from the lag over the last minute, and when its seconds count from.
    pace: Mutex<(BackfillPace, std::time::Instant)>,
    /// When the free space of the data directory and the size of the database are measured next,
    /// see [`Follower::disk_tick`].
    disk_next: Mutex<std::time::Instant>,
    /// The authority's chain requests that failed on the wire in a row, and the last of them.
    chain_failures: Mutex<(u32, String)>,
    /// When the cursor last moved, or the authority last answered from it (the local monotonic
    /// clock).
    progress_at: Mutex<std::time::Instant>,
}

/// How often the free space of the data directory and the size of the database are measured.
const DISK_EVERY: Duration = Duration::from_secs(60);

/// Lean storage: the pause between batches while one hit its limits and there is more to delete.
const LEAN_BUSY_EVERY: Duration = Duration::from_millis(100);
/// Lean storage: the pause after a batch that found nothing more to delete.
const LEAN_IDLE_EVERY: Duration = Duration::from_secs(30);
/// Lean storage prunes only while the store follows the node. When the store is behind the node by
/// more than this many DAA, a minute at 10 blocks a second, it catches up first with every step of
/// the follower going to the chain, and the pruning waits.
const LEAN_MAX_LAG_DAA: u64 = 600;

/// Whether a step failure is routine and not worth a warning.
///
/// A routine failure is the tips of two nodes differing for a moment, until it has repeated as
/// often as it takes to replace a node (`attempt` failures in a row).
fn retry_is_routine(reason: &str, attempt: u32) -> bool {
    reason.contains(crate::rpc::multi::TIPS_DIFFER) && attempt < crate::rpc::multi::STALLS_BEFORE_REPLACING
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl<S: ChainSource> Follower<S> {
    /// Creates a follower of `source` that commits to `ingest` and reports to `health`.
    pub fn new(
        source: Arc<S>,
        ingest: Arc<Mutex<Ingest>>,
        cfg: FollowerConfig,
        health: Arc<HealthState>,
        events: broadcast::Sender<Arc<IndexEvent>>,
    ) -> Self {
        let window = Mutex::new(cfg.window());
        let prefetch = tokio::sync::Mutex::new(Prefetch::new(cfg.prefetch_limits()));
        let lean = ingest.lock().unwrap_or_else(|e| e.into_inner()).prunes();
        Follower {
            source,
            ingest,
            cfg,
            health,
            events,
            last_status: Mutex::new(None),
            window,
            cursor_blue: Mutex::new(None),
            prefetch,
            lean_next: Mutex::new(lean.then(std::time::Instant::now)),
            pace: Mutex::new((BackfillPace::default(), std::time::Instant::now())),
            disk_next: Mutex::new(std::time::Instant::now()),
            chain_failures: Mutex::new((0, String::new())),
            progress_at: Mutex::new(std::time::Instant::now()),
        }
    }

    /// A chain request of the authority (`what`) failed: counted when it failed on the wire.
    fn chain_failed(&self, what: &str, e: &RpcError) {
        if crate::rpc::failover::node_failure(e) {
            self.count_chain_failure(format!("{what}: {e}"));
        }
    }

    fn count_chain_failure(&self, what: String) {
        let mut g = self.chain_failures.lock().unwrap_or_else(|e| e.into_inner());
        g.0 = g.0.saturating_add(1);
        g.1 = what;
    }

    /// The authority answered from the cursor, or the cursor moved: the failures in a row and the
    /// stall begin again.
    fn chain_answered(&self) {
        self.chain_failures.lock().unwrap_or_else(|e| e.into_inner()).0 = 0;
        *self.progress_at.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
    }

    /// After a failed step: when the authority's chain requests failed often enough in a row and
    /// the cursor stalled long enough, the source is told ([`ChainSource::authority_failing`]).
    /// `Some`: another node is the authority now.
    async fn hand_over(&self) -> Option<StepOutcome> {
        let (n, last) = self.chain_failures.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let stalled = self.progress_at.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
        if n < self.cfg.authority_failures.max(1) || stalled < self.cfg.authority_stall {
            return None;
        }
        let cursor = self.blocking(|i| i.cursor()).await.ok().flatten()?;
        let why = format!("{n} chain requests failed in a row, the last {last}; the cursor has not moved for {} s", stalled.as_secs());
        let moved = self.source.authority_failing(cursor.hash, &why).await;
        // Whatever the source did, the count begins again: the next try needs as many failures.
        self.chain_failures.lock().unwrap_or_else(|e| e.into_inner()).0 = 0;
        if !moved {
            return None;
        }
        *self.progress_at.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
        // The plan was made on the old authority's chain, and the window sized for its link.
        self.prefetch.lock().await.discard();
        *self.win() = self.cfg.window();
        let msg = format!("the chain authority's chain requests kept failing ({why}): another node is the chain authority now");
        self.set_state(FollowerState::CatchingUp, Some(msg.clone()));
        Some(StepOutcome::Handover(msg))
    }

    /// Measures about once a minute the free bytes of the file system of the data directory and
    /// the size of the database.
    ///
    /// They are published in `/v1/health`, `kob_data_dir_free_bytes` and `kob_indexer_db_bytes`, so
    /// a disk that fills up is seen before it stops the store.
    async fn disk_tick(&self) {
        {
            let mut next = self.disk_next.lock().unwrap_or_else(|e| e.into_inner());
            if *next > std::time::Instant::now() {
                return;
            }
            *next = std::time::Instant::now() + DISK_EVERY;
        }
        let db = self.blocking(|i| i.conn().path().filter(|p| !p.is_empty()).map(std::path::PathBuf::from)).await;
        let Some(db) = db else { return };
        let (free, size) = tokio::task::spawn_blocking(move || {
            let dir = db.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
            (crate::disk::free_bytes(&dir), crate::disk::db_bytes(&db))
        })
        .await
        .unwrap_or((None, None));
        self.health.update(|h| {
            h.data_dir_free_bytes = free;
            h.db_bytes = size;
        });
    }

    /// Updates how fast the store closes in on the node and when it will have, while it is
    /// catching up.
    ///
    /// The rate comes from the lag over the last minute and is published as `catch_up` of the
    /// health and the status line. It is cleared once the store follows.
    fn note_catch_up(&self) {
        let h = self.health.snapshot();
        let lag = h.lag_daa().filter(|_| h.state == FollowerState::CatchingUp && h.cursor_daa > 0);
        let catch_up = lag.and_then(|l| {
            let mut g = self.pace.lock().unwrap_or_else(|e| e.into_inner());
            let at = g.1.elapsed().as_secs_f64();
            let (rate, eta) = g.0.push(at, l, h.cursor_daa);
            rate.map(|rate_daa_per_sec| CatchUp { rate_daa_per_sec, eta_seconds: eta })
        });
        if lag.is_none() {
            self.pace.lock().unwrap_or_else(|e| e.into_inner()).0 = BackfillPace::default();
        }
        if catch_up != h.catch_up {
            self.health.update(|h| h.catch_up = catch_up);
        }
    }

    /// Runs one bounded pruning batch of lean storage when it is due, see [`super::lean`].
    ///
    /// The follower runs it between its steps, so the store has one writer and the batch, a short
    /// transaction, never overlaps a commit.
    async fn lean_tick(&self) {
        let due = self.lean_next.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t <= std::time::Instant::now());
        if !due {
            return;
        }
        let snap = self.health.snapshot();
        if snap.lag_daa().is_some_and(|lag| lag > LEAN_MAX_LAG_DAA) {
            *self.lean_next.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now() + LEAN_BUSY_EVERY);
            return;
        }
        let node_daa = snap.node_daa;
        let began = std::time::Instant::now();
        let next = match self.blocking(move |i| i.prune_batch(node_daa)).await {
            Ok(Some(rep)) => {
                if rep.over_cap != snap.lean_over_cap {
                    if rep.over_cap {
                        tracing::warn!("lean storage: the store is above its size cap (lean_max_db_mb); pruning closed orders to the reorg window");
                    }
                    self.health.update(|h| h.lean_over_cap = rep.over_cap);
                }
                if rep.rows > 0 || rep.pages > 0 {
                    tracing::debug!(orders = rep.orders, rows = rep.rows, pages = rep.pages, more = rep.more, "storage: pruned");
                    self.health.update(|h| {
                        h.lean_orders_pruned_total += rep.orders as u64;
                        h.lean_rows_pruned_total += rep.rows as u64;
                    });
                }
                if rep.more {
                    // A pause of twice the batch, so pruning takes a third of the thread at most,
                    // and at least the base pause.
                    LEAN_BUSY_EVERY.max(began.elapsed() * 2)
                } else {
                    LEAN_IDLE_EVERY
                }
            }
            Ok(None) => LEAN_IDLE_EVERY,
            Err(e) => {
                tracing::warn!(error = %e, "storage: pruning failed; trying again later");
                self.health.update(|h| h.lean_prune_failures_total += 1);
                LEAN_IDLE_EVERY
            }
        };
        *self.lean_next.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now() + next);
    }

    fn win(&self) -> std::sync::MutexGuard<'_, BatchWindow> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The blue score of the cursor, remembered from the last applied header, else asked from the
    /// node.
    async fn cursor_blue(&self, cursor: Hash32) -> Result<Option<u64>, RpcError> {
        if let Some((h, b)) = *self.cursor_blue.lock().unwrap_or_else(|e| e.into_inner()) {
            if h == cursor {
                return Ok(Some(b));
            }
        }
        let b = self.source.block_blue_score(cursor).await?;
        if let Some(b) = b {
            *self.cursor_blue.lock().unwrap_or_else(|e| e.into_inner()) = Some((cursor, b));
        }
        Ok(b)
    }

    /// The window of the next request from `cursor`.
    ///
    /// A `None` blue score of the cursor means the node does not know it. The unbounded request
    /// then fails fast with `cannot find header` and the walk-back runs.
    async fn plan(&self, cursor: Hash32) -> Result<WindowPlan, RpcError> {
        let extra = self.cfg.min_confirmations;
        let Some(cursor_blue) = self.cursor_blue(cursor).await? else {
            return Ok(WindowPlan { min_confirmations: extra, until_blue: None, span: u64::MAX, timeout_scale: 1 });
        };
        let sink_blue = self.source.sink_blue_score().await?;
        let (plan, window) = {
            let w = self.win();
            (w.plan(cursor_blue, sink_blue, extra), w.current())
        };
        self.health.update(|h| {
            h.lag_blue = Some(sink_blue.saturating_sub(cursor_blue));
            h.batch_window_blue = Some(window);
        });
        Ok(plan)
    }

    async fn blocking<T: Send + 'static>(&self, f: impl FnOnce(&mut Ingest) -> T + Send + 'static) -> T {
        let ing = self.ingest.clone();
        tokio::task::spawn_blocking(move || f(&mut ing.lock().unwrap_or_else(|e| e.into_inner())))
            .await
            .expect("blocking task panicked")
    }

    fn set_state(&self, state: FollowerState, err: Option<String>) {
        self.health.update(|h| {
            h.state = state;
            h.last_poll_unix_ms = now_ms();
            // Progress ends a halt or a storage error. A node error between two failed commits does
            // not.
            if matches!(state, FollowerState::Following | FollowerState::CatchingUp) {
                h.halt_reason = None;
            }
            if let Some(e) = err {
                h.last_error = Some(e);
            }
        });
    }

    /// A record-log write failed. Stops following, with state `stopped` and the reason in
    /// `last_error` and `halt_reason`.
    fn halt(&self, e: &IngestError) -> StepOutcome {
        let msg = format!("record log write failed, the follower stopped (free space or fix the disk, then restart): {e}");
        self.set_state(FollowerState::Stopped, Some(msg.clone()));
        self.health.update(|h| h.halt_reason = Some(msg.clone()));
        StepOutcome::Halted(msg)
    }

    /// The database cannot be written or is corrupt (`what`, [`super::db::storage_problem`]).
    ///
    /// The state is `storage_error`, with the reason in `last_error` and `halt_reason`. The
    /// follower retries, so a disk with free space again goes on.
    fn storage_error(&self, what: &str, e: &IngestError) -> StepOutcome {
        let msg = format!("{what}: {e}");
        tracing::error!("the database cannot be written: {msg}");
        self.set_state(FollowerState::StorageError, Some(msg.clone()));
        self.health.update(|h| h.halt_reason = Some(msg.clone()));
        StepOutcome::Retry(msg)
    }

    async fn ensure_cursor(&self) -> Result<Cursor, StepOutcome> {
        if let Some(c) = self.blocking(|i| i.cursor()).await.map_err(|e| StepOutcome::Retry(format!("database: {e}")))? {
            return Ok(c);
        }
        let info = self.source.server_info().await.map_err(|e| StepOutcome::Retry(format!("getServerInfo: {e}")))?;
        if info.network_id != self.cfg.network {
            return Err(StepOutcome::Gap(format!("node is on `{}`, configured `{}`", info.network_id, self.cfg.network)));
        }
        if !info.is_synced {
            return Err(StepOutcome::Retry("node is not synced yet".into()));
        }
        self.health.update(|h| h.node_version = Some(info.server_version.clone()));
        let hash = match &self.cfg.start {
            StartMode::Hash(h) => *h,
            StartMode::PruningPoint => {
                self.source.dag_info().await.map_err(|e| StepOutcome::Retry(format!("getBlockDagInfo: {e}")))?.pruning_point_hash
            }
            StartMode::Sink => self.source.dag_info().await.map_err(|e| StepOutcome::Retry(format!("getBlockDagInfo: {e}")))?.sink,
        };
        // A start at the sink knows its position, so the book of a quiet chain is not taken for a
        // stale one. The other starts learn it from their first batch.
        let daa = match self.cfg.start {
            StartMode::Sink => self.source.block_daa_score(hash).await.ok().flatten().unwrap_or(0),
            _ => 0,
        };
        let c = Cursor { hash, daa };
        // A start anywhere but the pruning point leaves the older history out, so the book is not
        // complete, for good.
        let partial = !matches!(self.cfg.start, StartMode::PruningPoint);
        self.blocking(move |i| {
            i.init_cursor(&c)?;
            if partial {
                i.mark_history_from(c.daa)?;
            }
            Ok(())
        })
        .await
        .map_err(|e: super::db::DbError| StepOutcome::Retry(format!("database: {e}")))?;
        if partial {
            self.health.update(|h| {
                h.book_complete = false;
                h.history_from_daa = Some(c.daa);
            });
        }
        tracing::info!(start = %hash, mode = ?self.cfg.start, "initialised cursor");
        Ok(c)
    }

    async fn refresh_status(&self) {
        let due = {
            let mut g = self.last_status.lock().unwrap_or_else(|e| e.into_inner());
            let due = g.map(|t| t.elapsed() >= self.cfg.status_refresh).unwrap_or(true);
            if due {
                *g = Some(std::time::Instant::now());
            }
            due
        };
        if !due {
            return;
        }
        match self.source.dag_info().await {
            Ok(d) => self.health.update(|h| h.node_daa = Some(d.virtual_daa_score)),
            Err(e) => tracing::debug!("status refresh failed: {e}"),
        }
    }

    /// The cursor, or the outcome that stops this step.
    async fn cursor_or_outcome(&self) -> Result<Cursor, StepOutcome> {
        match self.ensure_cursor().await {
            Ok(c) => Ok(c),
            Err(StepOutcome::Gap(r)) => {
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(r.clone());
                });
                Err(StepOutcome::Gap(r))
            }
            Err(o) => {
                if let StepOutcome::Retry(r) = &o {
                    self.set_state(FollowerState::NodeUnavailable, Some(r.clone()));
                }
                Err(o)
            }
        }
    }

    /// One poll of the node and at most one atomic commit.
    ///
    /// With `fetch_parallel` > 1 and the cursor far behind, the batch is the next of the windows
    /// fetched ahead, see [`super::prefetch`].
    pub async fn step(&self) -> StepOutcome {
        let mut outcome = if self.cfg.fetch_parallel > 1 { self.step_prefetch().await } else { self.step_sequential().await };
        if matches!(outcome, StepOutcome::Retry(_)) {
            if let Some(o) = self.hand_over().await {
                outcome = o;
            }
        }
        let nodes = self.source.node_stats();
        let source = self.source.source_status();
        // The alarms of named nodes (`rpc::judge`), process-wide.
        let alarms = crate::rpc::judge::alarms();
        self.health.update(|h| {
            if !nodes.is_empty() || source.is_some() {
                // Behind a failover the list is the active node's. It may be empty for a single
                // node.
                h.nodes = nodes;
                h.node_source = source;
            }
            h.node_alarms = alarms;
            h.authority_replaced_total = crate::rpc::resolver::authority_replaced() + crate::rpc::multi::promotions();
        });
        outcome
    }

    /// One window from the cursor, fetched now.
    async fn step_sequential(&self) -> StepOutcome {
        let cursor = match self.cursor_or_outcome().await {
            Ok(c) => c,
            Err(o) => return o,
        };
        let plan = match self.plan(cursor.hash).await {
            Ok(p) => p,
            Err(e) => return self.on_rpc_error(e).await,
        };
        let mut req = VspcRequest::new(cursor.hash, Verbosity::High, plan.min_confirmations);
        req.timeout_scale = plan.timeout_scale;
        let t0 = std::time::Instant::now();
        let raw = match self.source.vspc_v2(req).await {
            Ok(r) => r,
            Err(e) => {
                if matches!(e, RpcError::Timeout(_) | RpcError::TooLarge { .. }) {
                    // Split the batch: the same request would only time out, or be too large,
                    // again.
                    let (window, scale) = {
                        let mut w = self.win();
                        w.on_timeout(plan.span);
                        (w.current(), w.timeout_scale())
                    };
                    tracing::warn!(window_blue = window, timeout_scale = scale, "VSPC batch timed out; asking for a smaller one");
                    self.health.update(|h| {
                        h.vspc_timeouts_total += 1;
                        h.batch_window_blue = Some(window);
                    });
                }
                return self.on_rpc_error(e).await;
            }
        };
        let t_fetch = t0.elapsed();
        let batch = match tokio::task::spawn_blocking(move || raw.into_batch()).await.expect("parse task panicked") {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("bad VSPC response: {e}");
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                return StepOutcome::Retry(msg);
            }
        };
        let t_parse = t0.elapsed() - t_fetch;
        if plan.by_window() && batch.added.is_empty() {
            self.record_fetch(&batch, t_fetch);
            // The next chain block lies beyond the window, or the stripped head held all of them.
            // Widen it.
            let window = {
                let mut w = self.win();
                w.on_empty();
                w.current()
            };
            self.health.update(|h| {
                h.state = FollowerState::CatchingUp;
                h.batch_window_blue = Some(window);
                h.last_poll_unix_ms = now_ms();
            });
            self.chain_answered();
            return StepOutcome::Widened { window };
        }
        let committed =
            Committed { start: cursor.hash, by_window: plan.by_window(), t_fetch, t_parse, end_blue: None, prefetched: false };
        self.commit(batch, committed).await
    }

    /// The next window fetched ahead, or a sequential step where parallel fetch does not apply.
    async fn step_prefetch(&self) -> StepOutcome {
        let cursor = match self.cursor_or_outcome().await {
            Ok(c) => c,
            Err(o) => return o,
        };
        let mut pf = self.prefetch.lock().await;
        if pf.active() && pf.head_start() != Some(cursor.hash) {
            // The cursor moved without the windows, as in a rewind, so they no longer continue it.
            pf.discard();
        }
        if !pf.active() {
            let cursor_blue = match self.cursor_blue(cursor.hash).await {
                Ok(Some(b)) => b,
                Ok(None) => return self.step_sequential().await,
                Err(e) => return self.on_rpc_error(e).await,
            };
            let sink_blue = match self.source.sink_blue_score().await {
                Ok(b) => b,
                Err(e) => return self.on_rpc_error(e).await,
            };
            let lag = sink_blue.saturating_sub(cursor_blue);
            self.health.update(|h| h.lag_blue = Some(lag));
            if lag.saturating_sub(self.cfg.min_confirmations.unwrap_or(0)) <= self.cfg.prefetch_min_lag_blue {
                return self.step_sequential().await;
            }
            match self.source.chain_hashes(cursor.hash).await {
                // Less than a first window to the sink: one request from the cursor does better,
                // see `Prefetch::worth_planning`.
                Ok(c) if c.removed_chain_block_hashes.is_empty() && pf.worth_planning(&c.added_chain_block_hashes) => {
                    pf.start(cursor.hash, c.added_chain_block_hashes);
                }
                Err(e) => {
                    self.chain_failed("the chain hashes", &e);
                    return self.step_sequential().await;
                }
                // A reorg at the cursor, or nothing after it: the single step handles each.
                _ => return self.step_sequential().await,
            }
        }
        let t0 = std::time::Instant::now();
        let head = pf.next(&self.source).await;
        let stats = pf.stats;
        drop(pf);
        self.health.update(|h| {
            h.prefetch_windows = stats.windows as u64;
            h.prefetch_in_flight = stats.in_flight as u64;
            h.prefetch_bytes = stats.bytes;
            h.prefetch_bytes_peak = stats.peak_bytes;
            h.prefetch_window_blocks = stats.window_blocks as u64;
            h.prefetch_discards_total = stats.discards;
            h.prefetch_oversize_total = stats.oversize;
        });
        match head {
            Head::Ready { start, raw, sink_blue, end_blue, elapsed, origin } => {
                if !origin.trusted {
                    // A window from another node: what no hash covers is taken from the primary
                    // where it can matter.
                    if let Err(why) = self.confirm_with_primary(start, &raw, origin).await {
                        self.prefetch.lock().await.discard();
                        tracing::warn!(reason = %why, "window from another node not applied; one step from the cursor");
                        return self.step_sequential().await;
                    }
                }
                let waited = t0.elapsed();
                let batch = match tokio::task::spawn_blocking(move || raw.into_batch()).await.expect("parse task panicked") {
                    Ok(b) => b,
                    Err(e) => {
                        self.prefetch.lock().await.discard();
                        let msg = format!("bad VSPC response: {e}");
                        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                        return StepOutcome::Retry(msg);
                    }
                };
                let t_parse = t0.elapsed() - waited;
                // Within `prefetch_min_lag_blue` of the sink after this window: plan nothing more,
                // apply what is fetched or in flight, then take single steps, see
                // `Prefetch::finish`.
                let near_sink = end_blue.is_some_and(|eb| {
                    sink_blue.saturating_sub(eb).saturating_sub(self.cfg.min_confirmations.unwrap_or(0))
                        <= self.cfg.prefetch_min_lag_blue
                });
                if let Some(eb) = end_blue {
                    self.health.update(|h| h.lag_blue = Some(sink_blue.saturating_sub(eb)));
                }
                let committed = Committed { start, by_window: true, t_fetch: elapsed, t_parse, end_blue, prefetched: true };
                let out = self.commit(batch, committed).await;
                if !matches!(out, StepOutcome::Applied { .. }) {
                    self.prefetch.lock().await.discard();
                } else if near_sink {
                    self.prefetch.lock().await.finish();
                }
                out
            }
            Head::TimedOut { blocks, timeout_scale } => {
                tracing::warn!(blocks, timeout_scale, "prefetched VSPC window timed out; split it");
                self.health.update(|h| h.vspc_timeouts_total += 1);
                let msg = format!("VSPC window timed out (next: {blocks} chain blocks, timeout x{timeout_scale})");
                // The primary's window or its acceptance data for another node's: a secondary that times out is skipped
                // for the next node (`rpc::multi`).
                self.count_chain_failure(msg.clone());
                self.set_state(FollowerState::CatchingUp, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
            Head::Discarded(why) => {
                tracing::info!(reason = %why, "prefetched windows dropped; one step from the cursor");
                self.step_sequential().await
            }
        }
    }

    /// Checks a window `raw` from another node against the primary, as far as it can matter to KOB.
    ///
    /// The window is already checked against the hashes and acceptance data of the primary
    /// (`rpc::verify`). Its chain blocks that hold a transaction that can matter to KOB
    /// (`super::trust`) are fetched from the primary and must agree with it in everything the
    /// indexer reads, signature scripts and spent outputs included. A difference drops the node
    /// (`ChainSource::report_lie`). Any failure leaves the window unapplied, and the `Err` says
    /// why.
    async fn confirm_with_primary(&self, start: Hash32, raw: &RawVspcResponse, origin: Origin) -> Result<(), String> {
        let keys = super::trust::WindowKeys::of(&raw.chain_block_accepted_transactions);
        let ctx = self
            .blocking(move |i| super::trust::TrustContext::load(i.conn(), i.processor().tokens.clone(), &keys))
            .await
            .map_err(|e| format!("database: {e}"))?;
        let flags = ctx.flag(&raw.chain_block_accepted_transactions);
        // Runs of flagged chain blocks, as (the chain block before the run, the index of its first
        // block, the number of blocks). Runs with no more than `MERGE_GAP` unflagged chain blocks
        // between them are fetched as one, because a round trip costs more than a few chain blocks.
        const MERGE_GAP: usize = 3;
        let mut runs: Vec<(Hash32, usize, usize)> = vec![];
        for (i, f) in flags.iter().enumerate() {
            if !*f {
                continue;
            }
            match runs.last_mut() {
                Some((_, first, len)) if *first + *len + MERGE_GAP >= i => *len = i + 1 - *first,
                _ => runs.push((if i == 0 { start } else { raw.added_chain_block_hashes[i - 1] }, i, 1)),
            }
        }
        let flagged = flags.iter().filter(|f| **f).count() as u64;
        self.health.update(|h| {
            h.untrusted_windows_total += 1;
            h.untrusted_blocks_total += flags.len() as u64;
            h.confirmed_blocks_total += flagged;
        });
        if runs.is_empty() {
            return Ok(());
        }
        // The primary's sink once, and the blue score of the end of each run from its checked
        // header.
        let sink_blue = self.source.sink_blue_score().await.map_err(|e| {
            self.chain_failed("the primary's sink", &e);
            format!("the primary's sink: {e}")
        })?;
        let hashes = &raw.added_chain_block_hashes;
        let blocks = &raw.chain_block_accepted_transactions;
        let fetches = runs.iter().map(|(prev, first, len)| {
            let last = first + len - 1;
            let w = WindowRequest {
                min_confirmations: self.cfg.min_confirmations,
                end_blue: Some(blocks[last].chain_block_header.blue_score),
                sink_blue: Some(sink_blue),
                ..WindowRequest::new(*prev, hashes[last])
            };
            self.source.fetch_window_primary(w)
        });
        let answers = futures_util::future::join_all(fetches).await;
        for ((_, first, len), answer) in runs.iter().zip(answers) {
            let f = answer.map_err(|e| {
                self.chain_failed("its copy of a window", &e);
                format!("the primary's copy of {} chain blocks: {e}", len)
            })?;
            let p = &f.raw;
            if !p.removed_chain_block_hashes.is_empty() || p.added_chain_block_hashes.len() < *len {
                return Err(format!("the primary no longer reports the {len} chain blocks after {}", hashes[*first]));
            }
            for k in 0..*len {
                let i = first + k;
                if p.added_chain_block_hashes[k] != hashes[i] || p.chain_block_accepted_transactions.len() <= k {
                    return Err(format!("the primary's chain differs at {}", hashes[i]));
                }
                let mine = &raw.chain_block_accepted_transactions[i];
                if let Some(d) = crate::rpc::verify::body_difference(mine, &p.chain_block_accepted_transactions[k]) {
                    self.source.report_lie(origin, &d);
                    self.health.update(|h| h.lies_total += 1);
                    return Err(format!("node #{} contradicts the primary: {d}", origin.node));
                }
            }
        }
        Ok(())
    }

    fn record_fetch(&self, batch: &VspcBatch, t_fetch: std::time::Duration) {
        let n_txs: usize = batch.added.iter().map(|b| b.txs.len()).sum();
        let wire_bytes = batch.wire_bytes as u64;
        self.health.update(|h| {
            h.wire_bytes_total += wire_bytes;
            h.txs_fetched_total += n_txs as u64;
            h.last_batch_blocks = batch.added.len() as u64;
            h.last_batch_txs = n_txs as u64;
            h.last_batch_bytes = wire_bytes;
            h.last_fetch_ms = t_fetch.as_millis() as u64;
        });
    }

    /// Applies one batch requested from `c.start` and publishes what it changed.
    async fn commit(&self, batch: VspcBatch, c: Committed) -> StepOutcome {
        self.record_fetch(&batch, c.t_fetch);
        let n_txs: usize = batch.added.iter().map(|b| b.txs.len()).sum();
        let wire_bytes = batch.wire_bytes as u64;
        let last_blue = batch.added.last().map(|b| (b.header.hash, b.header.blue_score));
        let start = c.start;
        let t1 = std::time::Instant::now();
        let applied = self.blocking(move |i| i.apply_batch(start, &batch)).await;
        let t_commit = t1.elapsed();
        match applied {
            Ok(a) => {
                let window = {
                    let mut w = self.win();
                    if c.prefetched {
                        w.cursor_moved();
                    } else if a.added > 0 || a.removed_hashes > 0 {
                        w.on_success(c.t_fetch, c.by_window);
                    }
                    w.current()
                };
                let blue = last_blue.filter(|(_, b)| *b > 0).or(c.end_blue.zip(last_blue).map(|(b, (h, _))| (h, b)));
                if let Some((h, b)) = blue.filter(|(h, _)| *h == a.cursor.hash) {
                    *self.cursor_blue.lock().unwrap_or_else(|e| e.into_inner()) = Some((h, b));
                }
                // Per-batch detail is logged at DEBUG (`RUST_LOG=kobd=debug`). The status line
                // every `--status-secs` says how the indexer is doing, as kaspad's "Processed N
                // blocks in the last 10s" does.
                if a.added >= 100 || c.by_window {
                    tracing::debug!(
                        added = a.added,
                        removed = a.removed_hashes,
                        txs = n_txs,
                        relevant = a.relevant_txs,
                        bytes = wire_bytes,
                        window_blue = (c.by_window && !c.prefetched).then_some(window),
                        prefetched = c.prefetched,
                        fetch_ms = c.t_fetch.as_millis() as u64,
                        parse_ms = c.t_parse.as_millis() as u64,
                        commit_ms = t_commit.as_millis() as u64,
                        "applied batch"
                    );
                } else {
                    tracing::debug!(
                        added = a.added,
                        removed = a.removed_hashes,
                        fetch_ms = c.t_fetch.as_millis() as u64,
                        "applied batch"
                    );
                }
                self.refresh_status().await;
                let snapshot_node_daa = self.health.snapshot().node_daa;
                // A batch that the window ended leaves chain behind it, whatever the node position
                // says.
                let more = c.by_window
                    || snapshot_node_daa.map(|n| n.saturating_sub(a.cursor.daa) > self.cfg.caught_up_daa).unwrap_or(a.added > 0);
                let orders_total =
                    if a.new_orders > 0 || a.reverted_rows_blocks > 0 { self.blocking(|i| i.order_count().ok()).await } else { None };
                let records_next = self.blocking(|i| i.records_next()).await;
                // The update notice is read again only when a placement of the batch announced a
                // newer release.
                if a.release_noticed {
                    if let Ok(Some(n)) = self.blocking(|i| i.newer_release_seen()).await {
                        super::release::publish(&self.health, n);
                    }
                }
                self.health.update(|h| {
                    h.state = if a.added == 0 || !more { FollowerState::Following } else { FollowerState::CatchingUp };
                    h.cursor_hash = Some(a.cursor.hash);
                    h.cursor_daa = a.cursor.daa;
                    if !c.prefetched {
                        h.batch_window_blue = Some(window);
                    }
                    h.last_error = None;
                    h.last_poll_unix_ms = now_ms();
                    // Progress, or a poll that confirmed the cursor is at the sink of the node.
                    h.last_progress_unix_ms = now_ms();
                    // A commit went through, so there is no storage error any more.
                    h.halt_reason = None;
                    if a.removed_hashes > 0 {
                        h.reorgs_total += 1;
                    }
                    if a.unknown_removed > 0 {
                        h.deep_reorgs_total += 1;
                    }
                    h.reverted_blocks_total += a.reverted_rows_blocks as u64;
                    h.blocks_applied_total += a.added as u64;
                    h.relevant_txs_total += a.relevant_txs;
                    if let Some(t) = orders_total {
                        h.orders_total = t;
                    }
                    if let Some(n) = records_next {
                        h.records_next_n = n;
                    }
                });
                self.note_catch_up();
                self.chain_answered();
                if !a.event.is_empty() {
                    let _ = self.events.send(Arc::new(a.event));
                }
                if a.added == 0 && a.removed_hashes == 0 {
                    StepOutcome::Idle
                } else {
                    StepOutcome::Applied { added: a.added, removed: a.removed_hashes, more }
                }
            }
            Err(IngestError::EmptyAfterReorg) => {
                let msg = "node reported a reorg with no added blocks; waiting".to_string();
                self.set_state(FollowerState::Following, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
            Err(e @ IngestError::InconsistentRemoved(_)) => {
                let mut msg = format!("stored chain contradicts the node: {e}");
                // A public node that answers inconsistently is given up for another, and the
                // follower asks that one. Once two public nodes on different hosts agree with each
                // other against the store, the store halts (`rpc::multi`).
                match self.source.authority_contradicted(start, &msg) {
                    crate::rpc::StoreContradiction::Replaced => {
                        let msg = format!("{msg}; the public node was given up for another, asking it");
                        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                        return StepOutcome::Retry(msg);
                    }
                    crate::rpc::StoreContradiction::Halt(Some(why)) => msg = format!("{msg}; {why}"),
                    crate::rpc::StoreContradiction::Halt(None) => {}
                }
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(msg.clone());
                    h.last_error = Some(msg.clone());
                });
                StepOutcome::Gap(msg)
            }
            Err(e @ IngestError::Log(_)) => self.halt(&e),
            Err(e) => match super::db::storage_problem(&e) {
                Some(what) => self.storage_error(what, &e),
                None => {
                    let msg = format!("commit failed: {e}");
                    self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                    StepOutcome::Retry(msg)
                }
            },
        }
    }

    async fn on_rpc_error(&self, e: RpcError) -> StepOutcome {
        // Every caller asked the chain authority.
        self.chain_failed("a chain request", &e);
        let mut kind = e.kind();
        if matches!(kind, NodeErrorKind::UnknownHash | NodeErrorKind::RetentionRootMissing) {
            // The store is rewound or halted on this only when the source confirms it.
            if let Some(cursor) = self.blocking(|i| i.cursor()).await.ok().flatten() {
                match self.source.confirm_unknown_cursor(cursor.hash, &e).await {
                    Ok(k) => kind = k,
                    Err(why) => {
                        let msg = format!(
                            "the chain authority disowns the cursor {}, which is not confirmed ({why}): nothing is rewound or halted",
                            cursor.hash
                        );
                        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                        return StepOutcome::Retry(msg);
                    }
                }
            }
        }
        match kind {
            NodeErrorKind::UnknownHash => self.recover_unknown_cursor().await,
            NodeErrorKind::RetentionRootMissing => {
                let msg = format!("{PRUNED_GAP} the node's retention: {e}");
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(msg.clone());
                    h.last_error = Some(msg.clone());
                });
                StepOutcome::Gap(msg)
            }
            NodeErrorKind::TransitionalIbd | NodeErrorKind::Other => {
                let msg = e.to_string();
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
        }
    }

    /// The node does not know the cursor: rewinds to the newest stored block it still knows.
    async fn recover_unknown_cursor(&self) -> StepOutcome {
        let limit = self.cfg.max_walkback_blocks as usize;
        let candidates = match self.blocking(move |i| i.stored_blocks_desc(limit)).await {
            Ok(c) => c,
            Err(e) => return StepOutcome::Retry(format!("database: {e}")),
        };
        // Knowledge is monotone along the chain, because a node that knows a block knows its
        // ancestors. So the newest known stored block is found by bisection: O(log n) node calls
        // for a long window.
        let mut found = None;
        if let Some(oldest) = candidates.last() {
            match self.source.block_exists(oldest.1).await {
                Ok(true) => {
                    let (mut lo, mut hi) = (0usize, candidates.len() - 1);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        match self.source.block_exists(candidates[mid].1).await {
                            Ok(true) => hi = mid,
                            Ok(false) => lo = mid + 1,
                            Err(e) => return StepOutcome::Retry(format!("getBlock: {e}")),
                        }
                    }
                    found = Some(candidates[lo]);
                }
                Ok(false) => {}
                Err(e) => return StepOutcome::Retry(format!("getBlock: {e}")),
            }
        }
        if let Some((seq, hash, daa)) = found {
            let target = Cursor { hash, daa };
            return match self.blocking(move |i| i.rewind_to(seq, target)).await {
                Ok(a) => {
                    tracing::warn!(to = %hash, reverted = a.reverted_rows_blocks, "cursor unknown to the node: rewound");
                    self.chain_answered();
                    self.win().cursor_moved();
                    self.health.update(|h| {
                        h.cursor_hash = Some(hash);
                        h.cursor_daa = daa;
                        h.reverted_blocks_total += a.reverted_rows_blocks as u64;
                        h.reorgs_total += 1;
                    });
                    if !a.event.is_empty() {
                        let _ = self.events.send(Arc::new(a.event));
                    }
                    StepOutcome::Rewound { blocks: a.reverted_rows_blocks }
                }
                Err(e @ IngestError::Log(_)) => self.halt(&e),
                Err(e) => StepOutcome::Retry(format!("rewind failed: {e}")),
            };
        }
        let msg = format!("the node does not know the cursor or any stored block: {PRUNED_GAP} pruning, or the node was reset");
        self.health.update(|h| {
            h.state = FollowerState::Gap;
            h.gap_reason = Some(msg.clone());
            h.last_error = Some(msg.clone());
        });
        StepOutcome::Gap(msg)
    }

    /// Runs until `shutdown` flips to true.
    ///
    /// A gap stops the following for good, with state `gap`, until it does.
    pub async fn run(&self, shutdown: watch::Receiver<bool>) {
        self.run_loop(shutdown, false).await;
    }

    /// Like [`Follower::run`], but a gap that the pruning of the node caused ends the run at once.
    ///
    /// A gap is such a gap when [`is_pruned_gap`] says so. The run does not wait for the shutdown.
    /// It returns the reason, and the state stays `gap`, for the caller to move the store past it
    /// ([`super::recovery`]). It returns `None` on the shutdown, or on a halt that needs the
    /// operator.
    pub async fn run_recovering(&self, shutdown: watch::Receiver<bool>) -> Option<String> {
        self.run_loop(shutdown, true).await
    }

    async fn run_loop(&self, mut shutdown: watch::Receiver<bool>, recover: bool) -> Option<String> {
        let mut failures = 0u32;
        loop {
            if *shutdown.borrow() {
                break;
            }
            // A step waits on the node, and a window can take minutes to arrive, so the stop does
            // not wait for it. Dropping a step loses nothing: its commit is one blocking task that
            // runs to its end, and the next start resumes at the cursor.
            let outcome = tokio::select! {
                o = self.step() => o,
                _ = async { let _ = shutdown.wait_for(|s| *s).await; } => break,
            };
            self.lean_tick().await;
            self.disk_tick().await;
            let delay = match &outcome {
                StepOutcome::Applied { more: true, .. } | StepOutcome::Rewound { .. } => {
                    failures = 0;
                    Duration::ZERO
                }
                // No progress yet, but a different request is due at once. This is bounded, because
                // the window doubles.
                StepOutcome::Widened { .. } => Duration::ZERO,
                // Another node is the chain authority now (the source warned): ask it at once.
                StepOutcome::Handover(r) => {
                    failures = 0;
                    tracing::info!("{r}");
                    Duration::ZERO
                }
                StepOutcome::Applied { .. } | StepOutcome::Idle => {
                    failures = 0;
                    self.cfg.poll_interval
                }
                StepOutcome::Retry(r) => {
                    failures = failures.saturating_add(1);
                    if retry_is_routine(r, failures) {
                        tracing::debug!(attempt = failures, "{r}");
                    } else {
                        tracing::warn!(attempt = failures, "{r}");
                    }
                    let d = self.cfg.backoff.saturating_mul(1u32 << failures.min(10).saturating_sub(1));
                    d.min(self.cfg.max_backoff)
                }
                StepOutcome::Gap(r) => {
                    if recover && is_pruned_gap(r) {
                        return Some(r.clone());
                    }
                    tracing::error!("indexer halted: {r}. See docs/ops/executor.md (Part B, 7.4).");
                    Duration::from_secs(5)
                }
                StepOutcome::Halted(r) => {
                    tracing::error!("indexer halted: {r}. See docs/ops/executor.md (Part B, 8).");
                    Duration::from_secs(5)
                }
            };
            self.health.update(|h| h.consecutive_failures = failures);
            if let StepOutcome::Gap(r) | StepOutcome::Halted(r) = &outcome {
                // Stay halted: do not poll the node again until the operator restarts after
                // bootstrapping. Say so again every `HALT_REMIND`, because an unattended executor
                // stays up and idle, and one line at the start is easily missed.
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tokio::time::sleep(HALT_REMIND) => {
                            tracing::error!("indexer still halted, nothing is indexed or traded: {r}. See docs/ops/executor.md (Part B, 7.4 and 8).");
                        }
                    }
                }
                break;
            }
            if delay.is_zero() {
                tokio::task::yield_now().await;
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = shutdown.changed() => {}
            }
        }
        self.health.update(|h| {
            if h.state != FollowerState::Gap {
                h.state = FollowerState::Stopped;
            }
        });
        None
    }
}

#[cfg(test)]
mod tests {
    use super::retry_is_routine;
    use crate::rpc::multi::TIPS_DIFFER;

    #[test]
    fn differing_tips_warn_only_when_they_repeat() {
        let r = format!("node error: {TIPS_DIFFER} (chain block #0 differs): nothing is indexed until they agree");
        assert!(retry_is_routine(&r, 1) && retry_is_routine(&r, 2));
        assert!(!retry_is_routine(&r, 3));
        assert!(!retry_is_routine("node error: the nodes disagree on the next chain block (x): nothing is indexed", 1));
        assert!(!retry_is_routine("transport: closed", 1));
    }
}
