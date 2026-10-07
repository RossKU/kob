//! The VSPC v2 follower: polls the node from the stored cursor and commits each response atomically.
//!
//! Failure policy (the cursor is never advanced on any of these):
//! * transport errors, timeouts, `consensus is currently in a transitional ibd state`: back off and retry;
//! * `cannot find header`: the node does not know the cursor. Walk the stored chain back to the newest
//!   block the node still knows, revert everything above it, and continue from there. If none is known,
//!   or the node says `the queried hash does not have retention root on its chain`, the downtime
//!   exceeded the node's retention: state `gap`, no further progress, operator bootstrap (see docs);
//! * a `removed` list that contradicts the stored chain: state `gap` (a different node or a bug; never guess);
//! * a record-log write that failed (full disk, I/O error; the append is rolled back): state `stopped`, nothing more is
//!   appended until the operator restarts.
//!
//! Batch size: every request is bounded to a window of blue score ahead of the cursor (`rpc::window`), so
//! a follower that fell behind under a transaction flood pages through the backlog in batches it can
//! fetch within its timeout instead of asking for the whole backlog at once. A timed-out batch is split
//! (the window shrinks), a fast one grows it, and a request is never repeated unchanged after a timeout.
//!
//! Parallel fetch (`fetch_parallel` > 1): while the cursor is more than `prefetch_min_lag_blue` behind the sink, the
//! follower fetches several upcoming windows at once over separate connections and applies them in chain order
//! ([`super::prefetch`]); near the sink, and whenever the plan stops matching the node's chain, it takes the single
//! steps above.

use super::ingest::{Cursor, Ingest, IngestError};
use super::prefetch::{Head, Prefetch, PrefetchLimits};
use super::status::{FollowerState, HealthState, IndexEvent};
use crate::config::StartMode;
use crate::hex::Hash32;
use crate::rpc::types::{RawVspcResponse, Verbosity, VspcBatch, VspcRequest};
use crate::rpc::window::{BatchWindow, WindowPlan};
use crate::rpc::{ChainSource, NodeErrorKind, Origin, RpcError, WindowRequest};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

#[derive(Debug, Clone)]
pub struct FollowerConfig {
    pub network: String,
    pub start: StartMode,
    pub poll_interval: Duration,
    pub backoff: Duration,
    pub max_backoff: Duration,
    /// Passed to the node as `minConfirmationCount` (None = follow the tip, the default: the API
    /// reports confirmations per event and lets consumers choose the tier).
    pub min_confirmations: Option<u64>,
    pub max_walkback_blocks: u64,
    /// Refresh the node status (`getBlockDagInfo`) at most this often.
    pub status_refresh: Duration,
    /// Lag below which the follower reports `following` rather than `catching_up`.
    pub caught_up_daa: u64,
    /// First batch window in blue score (about one block each at 10 BPS); it adapts from there.
    pub batch_initial_blue: u64,
    /// A batch should take about this long to fetch (the window shrinks above, grows below half of it).
    pub batch_target: Duration,
    /// VSPC windows fetched at once while catching up (1: one at a time; the source needs that many connections).
    pub fetch_parallel: usize,
    /// Bytes of windows held ahead of the cursor (fetched, or estimated in flight).
    pub prefetch_max_bytes: u64,
    /// Parallel fetch only while the cursor is more than this far (blue score) behind the sink.
    pub prefetch_min_lag_blue: u64,
    /// First parallel window, in chain blocks (it adapts from there).
    pub prefetch_initial_blocks: usize,
}

impl FollowerConfig {
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
        }
    }

    fn window(&self) -> BatchWindow {
        BatchWindow::new(self.batch_initial_blue, 1, 1 << 20, self.batch_target)
    }

    fn prefetch_limits(&self) -> PrefetchLimits {
        PrefetchLimits {
            parallel: self.fetch_parallel.max(1),
            max_bytes: self.prefetch_max_bytes,
            // windows in flight share the link: each may take longer than a lone batch (up to half the request timeout)
            target: self.batch_target.saturating_mul(self.fetch_parallel.clamp(1, 3) as u32),
            initial_blocks: self.prefetch_initial_blocks,
            min_confirmations: self.min_confirmations,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// A batch was committed. `more` is true when the node likely has more chain to give right away.
    Applied { added: usize, removed: usize, more: bool },
    /// The cursor is at the node's sink.
    Idle,
    /// Cursor rewound to an older stored block the node still knows.
    Rewound { blocks: usize },
    /// A bounded request came back without a chain block (the next one lies beyond the window): the
    /// window grew; ask again at once.
    Widened { window: u64 },
    /// Transient failure; back off.
    Retry(String),
    /// Unrecoverable without operator action.
    Gap(String),
    /// Writing the record log failed (a full disk, an I/O error): the follower stops and appends nothing more until the
    /// operator has fixed the cause and restarted (the restart's `open` checks the log).
    Halted(String),
}

/// How a batch about to be committed was fetched.
struct Committed {
    /// The chain block it was requested from (must be the cursor).
    start: Hash32,
    /// The window, not the sink, ended it (more chain follows).
    by_window: bool,
    t_fetch: Duration,
    t_parse: Duration,
    /// Blue score of its last chain block when known (prefetched windows learn it from the node).
    end_blue: Option<u64>,
    /// One of the windows fetched ahead (the sequential window learns nothing from it).
    prefetched: bool,
}

pub struct Follower<S: ChainSource> {
    source: Arc<S>,
    ingest: Arc<Mutex<Ingest>>,
    cfg: FollowerConfig,
    health: Arc<HealthState>,
    events: broadcast::Sender<Arc<IndexEvent>>,
    last_status: Mutex<Option<std::time::Instant>>,
    window: Mutex<BatchWindow>,
    /// Blue score of the cursor block, once known (the header of the last applied block, or `getBlock`).
    cursor_blue: Mutex<Option<(Hash32, u64)>>,
    /// Windows fetched ahead of the cursor (parallel fetch).
    prefetch: tokio::sync::Mutex<Prefetch>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl<S: ChainSource> Follower<S> {
    pub fn new(
        source: Arc<S>,
        ingest: Arc<Mutex<Ingest>>,
        cfg: FollowerConfig,
        health: Arc<HealthState>,
        events: broadcast::Sender<Arc<IndexEvent>>,
    ) -> Self {
        let window = Mutex::new(cfg.window());
        let prefetch = tokio::sync::Mutex::new(Prefetch::new(cfg.prefetch_limits()));
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
        }
    }

    fn win(&self) -> std::sync::MutexGuard<'_, BatchWindow> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cursor's blue score: remembered from the last applied header, else asked from the node.
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

    /// The window of the next request from `cursor`. `None` blue score of the cursor: the node does not
    /// know it; the unbounded request then fails fast with `cannot find header` and the walk-back runs.
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
            if let Some(e) = err {
                h.last_error = Some(e);
            }
        });
    }

    /// A record-log write failed: stop following (state `stopped`, the reason in `last_error`).
    fn halt(&self, e: &IngestError) -> StepOutcome {
        let msg = format!("record log write failed, the follower stopped (free space or fix the disk, then restart): {e}");
        self.set_state(FollowerState::Stopped, Some(msg.clone()));
        StepOutcome::Halted(msg)
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
        let c = Cursor { hash, daa: 0 };
        self.blocking(move |i| i.init_cursor(&c)).await.map_err(|e| StepOutcome::Retry(format!("database: {e}")))?;
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

    /// One poll of the node and (at most) one atomic commit. With `fetch_parallel` > 1 and the cursor far behind,
    /// the batch is the next of the windows fetched ahead ([`super::prefetch`]).
    pub async fn step(&self) -> StepOutcome {
        let outcome = if self.cfg.fetch_parallel > 1 { self.step_prefetch().await } else { self.step_sequential().await };
        let nodes = self.source.node_stats();
        if !nodes.is_empty() {
            self.health.update(|h| h.nodes = nodes);
        }
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
                    // split the batch: the same request would only time out (or be too large) again
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
            // the next chain block lies beyond the window (or the stripped head held all of them): widen
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
            // the cursor moved without the windows (a rewind): they no longer continue it
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
                Ok(c) if c.removed_chain_block_hashes.is_empty() && !c.added_chain_block_hashes.is_empty() => {
                    pf.start(cursor.hash, c.added_chain_block_hashes);
                }
                // a reorg at the cursor, nothing after it, or no answer: the single step handles each
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
                    // a window from another node: what no hash covers is taken from the primary where it can matter
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
                if let Some(eb) = end_blue {
                    self.health.update(|h| h.lag_blue = Some(sink_blue.saturating_sub(eb)));
                }
                let committed = Committed { start, by_window: true, t_fetch: elapsed, t_parse, end_blue, prefetched: true };
                let out = self.commit(batch, committed).await;
                if !matches!(out, StepOutcome::Applied { .. }) {
                    self.prefetch.lock().await.discard();
                }
                out
            }
            Head::TimedOut { blocks, timeout_scale } => {
                tracing::warn!(blocks, timeout_scale, "prefetched VSPC window timed out; split it");
                self.health.update(|h| h.vspc_timeouts_total += 1);
                let msg = format!("VSPC window timed out (next: {blocks} chain blocks, timeout x{timeout_scale})");
                self.set_state(FollowerState::CatchingUp, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
            Head::Discarded(why) => {
                tracing::info!(reason = %why, "prefetched windows dropped; one step from the cursor");
                self.step_sequential().await
            }
        }
    }

    /// A window `raw` from another node (checked against the primary's hashes and acceptance data, `rpc::verify`): its
    /// chain blocks holding a transaction that can matter to KOB (`super::trust`) are fetched from the primary and must
    /// agree with it in everything the indexer reads, signature scripts and spent outputs included. A difference drops the
    /// node (`ChainSource::report_lie`); any failure leaves the window unapplied (`Err`: why).
    async fn confirm_with_primary(&self, start: Hash32, raw: &RawVspcResponse, origin: Origin) -> Result<(), String> {
        let ctx = self
            .blocking(|i| super::trust::TrustContext::load(i.conn(), i.processor().tokens.clone()))
            .await
            .map_err(|e| format!("database: {e}"))?;
        let flags = ctx.flag(&raw.chain_block_accepted_transactions);
        // runs of flagged chain blocks: (the chain block before the run, index of its first block, blocks); runs at most
        // `MERGE_GAP` chain blocks apart are fetched as one (a round trip costs more than a few chain blocks)
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
        // the primary's sink once; the blue score of each run's end from its checked header
        let sink_blue = self.source.sink_blue_score().await.map_err(|e| format!("the primary's sink: {e}"))?;
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
            let f = answer.map_err(|e| format!("the primary's copy of {} chain blocks: {e}", len))?;
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

    /// Apply one batch requested from `c.start` and publish what it changed.
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
                if a.added >= 100 || c.by_window {
                    tracing::info!(
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
                // a batch the window ended leaves chain behind it, whatever the node position says
                let more = c.by_window
                    || snapshot_node_daa.map(|n| n.saturating_sub(a.cursor.daa) > self.cfg.caught_up_daa).unwrap_or(a.added > 0);
                let orders_total =
                    if a.new_orders > 0 || a.reverted_rows_blocks > 0 { self.blocking(|i| i.order_count().ok()).await } else { None };
                let records_next = self.blocking(|i| i.records_next()).await;
                self.health.update(|h| {
                    h.state = if a.added == 0 || !more { FollowerState::Following } else { FollowerState::CatchingUp };
                    h.cursor_hash = Some(a.cursor.hash);
                    h.cursor_daa = a.cursor.daa;
                    if !c.prefetched {
                        h.batch_window_blue = Some(window);
                    }
                    h.last_error = None;
                    h.last_poll_unix_ms = now_ms();
                    // progress or a poll that confirmed we are at the node's sink
                    h.last_progress_unix_ms = now_ms();
                    if a.removed_hashes > 0 {
                        h.reorgs_total += 1;
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
                let msg = format!("stored chain contradicts the node: {e}");
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(msg.clone());
                    h.last_error = Some(msg.clone());
                });
                StepOutcome::Gap(msg)
            }
            Err(e @ IngestError::Log(_)) => self.halt(&e),
            Err(e) => {
                let msg = format!("commit failed: {e}");
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
        }
    }

    async fn on_rpc_error(&self, e: RpcError) -> StepOutcome {
        match e.kind() {
            NodeErrorKind::UnknownHash => self.recover_unknown_cursor().await,
            NodeErrorKind::RetentionRootMissing => {
                let msg = format!("downtime exceeded the node's retention: {e}");
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

    /// The node does not know the cursor: rewind to the newest stored block it still knows.
    async fn recover_unknown_cursor(&self) -> StepOutcome {
        let limit = self.cfg.max_walkback_blocks as usize;
        let candidates = match self.blocking(move |i| i.stored_blocks_desc(limit)).await {
            Ok(c) => c,
            Err(e) => return StepOutcome::Retry(format!("database: {e}")),
        };
        // Knowledge is monotone along the chain (a node that knows a block knows its ancestors), so the
        // newest known stored block is found by bisection: O(log n) node calls for a long window.
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
        let msg =
            "the node does not know the cursor or any stored block: downtime exceeded pruning, or the node was reset".to_string();
        self.health.update(|h| {
            h.state = FollowerState::Gap;
            h.gap_reason = Some(msg.clone());
            h.last_error = Some(msg.clone());
        });
        StepOutcome::Gap(msg)
    }

    /// Run until `shutdown` flips to true.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let mut failures = 0u32;
        loop {
            if *shutdown.borrow() {
                break;
            }
            let outcome = self.step().await;
            let delay = match &outcome {
                StepOutcome::Applied { more: true, .. } | StepOutcome::Rewound { .. } => {
                    failures = 0;
                    Duration::ZERO
                }
                // no progress yet, but a different request is due at once (bounded: the window doubles)
                StepOutcome::Widened { .. } => Duration::ZERO,
                StepOutcome::Applied { .. } | StepOutcome::Idle => {
                    failures = 0;
                    self.cfg.poll_interval
                }
                StepOutcome::Retry(r) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!(attempt = failures, "{r}");
                    let d = self.cfg.backoff.saturating_mul(1u32 << failures.min(10).saturating_sub(1));
                    d.min(self.cfg.max_backoff)
                }
                StepOutcome::Gap(r) => {
                    tracing::error!("indexer halted: {r}. See docs/ops/executor.md (Part B, 7.4).");
                    Duration::from_secs(5)
                }
                StepOutcome::Halted(r) => {
                    tracing::error!("indexer halted: {r}. See docs/ops/executor.md (Part B, 8).");
                    Duration::from_secs(5)
                }
            };
            self.health.update(|h| h.consecutive_failures = failures);
            if let StepOutcome::Gap(_) | StepOutcome::Halted(_) = outcome {
                // Stay halted: do not poll the node again until the operator restarts after bootstrapping.
                let _ = shutdown.changed().await;
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
    }
}
