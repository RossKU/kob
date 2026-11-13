//! Shared runtime status (health) and the change feed the follower publishes after each commit.

use crate::hex::Hash32;
use serde::Serialize;
use std::sync::RwLock;

/// Where the follower is in its life cycle, as `/v1/health` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowerState {
    /// Opening the database and connecting to the node.
    Starting,
    /// Applying batches, more chain remains before the node's sink.
    CatchingUp,
    /// At the node's sink and polling.
    Following,
    /// The node is unreachable, not synced, or in a transitional IBD state.
    ///
    /// The cursor does not advance.
    NodeUnavailable,
    /// The stored chain cannot be continued from the node, after downtime beyond pruning or on a
    /// different network. The operator must act, see `docs/ops/executor.md` (Part B, 7.4).
    Gap,
    /// The follower halted, for example because its record log could not be written.
    ///
    /// `halt_reason` says why.
    Stopped,
    /// The database cannot be written (the disk is full, an I/O error) or is corrupt.
    ///
    /// Nothing is indexed until it is fixed. The follower retries, and goes on once space is freed.
    /// `kobd --executor` exits after a few minutes of it, so that a supervisor restarts it.
    /// `last_error` says what is wrong and what to do. A corrupt lean store can be deleted, and a
    /// fast start rebuilds it.
    StorageError,
}

/// The follower's health, as `GET /v1/health` serves it.
#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    /// Where the follower is.
    pub state: FollowerState,
    /// Network name, for example `testnet-10`.
    pub network: String,
    /// Version the node reports.
    pub node_version: Option<String>,
    /// Hash of the block the cursor stands at.
    pub cursor_hash: Option<Hash32>,
    /// DAA score of the cursor.
    pub cursor_daa: u64,
    /// Node virtual DAA score at the last successful poll.
    pub node_daa: Option<u64>,
    /// The most recent follower error.
    pub last_error: Option<String>,
    /// Why the store is halted at a gap.
    pub gap_reason: Option<String>,
    /// Unix time in milliseconds of the last poll of the node.
    pub last_poll_unix_ms: u64,
    /// Unix time in milliseconds when the cursor last advanced or the node last confirmed that the
    /// store is at its sink.
    pub last_progress_unix_ms: u64,
    /// Reorgs handled.
    pub reorgs_total: u64,
    /// Chain blocks reverted by reorgs.
    pub reverted_blocks_total: u64,
    /// Chain blocks applied.
    pub blocks_applied_total: u64,
    /// Relevant transactions applied.
    pub relevant_txs_total: u64,
    /// Orders indexed.
    pub orders_total: u64,
    /// Next position in the record log.
    pub records_next_n: u64,
    /// Blue score between the cursor and the node's sink at the last request.
    ///
    /// `None`: not measured yet.
    pub lag_blue: Option<u64>,
    /// The current VSPC batch window in blue score (`rpc::window`).
    pub batch_window_blue: Option<u64>,
    /// Chain blocks in the last batch.
    pub last_batch_blocks: u64,
    /// Accepted transactions in the last batch.
    pub last_batch_txs: u64,
    /// Bytes on the wire for the last batch.
    pub last_batch_bytes: u64,
    /// Fetch time of the last batch in milliseconds.
    pub last_fetch_ms: u64,
    /// VSPC bytes fetched since the start.
    pub wire_bytes_total: u64,
    /// Accepted transactions fetched since the start, for the wire cost per transaction.
    pub txs_fetched_total: u64,
    /// VSPC requests that timed out since the start.
    pub vspc_timeouts_total: u64,
    /// Failed polls in a row (0 after any progress).
    pub consecutive_failures: u32,
    /// Windows the parallel fetch (`fetch_parallel`) has queued ahead of the cursor.
    pub prefetch_windows: u64,
    /// Of those, the windows in flight.
    pub prefetch_in_flight: u64,
    /// Bytes held by the queued windows now.
    pub prefetch_bytes: u64,
    /// The most bytes the queued windows have held.
    pub prefetch_bytes_peak: u64,
    /// The current window in chain blocks.
    pub prefetch_window_blocks: u64,
    /// Plans dropped because the chain moved under them (reorgs).
    pub prefetch_discards_total: u64,
    /// Prefetched windows whose answer was larger than its allowance.
    ///
    /// Such an answer is refused, then split or sent again.
    pub prefetch_oversize_total: u64,
    /// The lag in DAA up to which the store counts as usable for planning.
    ///
    /// It follows `max_lag_secs`. 0 means only `following` counts.
    pub lag_tolerance_daa: u64,
    /// Windows fetched from a node other than the primary, with several nodes (`rpc::multi`).
    pub untrusted_windows_total: u64,
    /// Chain blocks of those windows.
    pub untrusted_blocks_total: u64,
    /// Chain blocks of those windows compared with the primary's copy before applying
    /// (`indexer::trust`).
    pub confirmed_blocks_total: u64,
    /// Contradictions the comparison found.
    pub lies_total: u64,
    /// Status per node. Empty with a single node.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<crate::rpc::multi::NodeStatus>,
    /// Status of the node failover: which node list is the chain source now, its switches and
    /// refusals.
    ///
    /// See `[failover]` and `rpc::failover`. Absent without a failover. `nodes` lists the nodes of
    /// the active list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_source: Option<crate::rpc::failover::FailoverStatus>,
    /// Alarms for nodes named in the configuration, one `node: problem` string each.
    ///
    /// The problems (`rpc::judge`) are: another network, no UTXO index, not synced for long,
    /// hanging, behind the other node, and contradicting it. A public node is replaced instead,
    /// and its `nodes[].cut_reason` says why. Absent when there is none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub node_alarms: Vec<String>,
    /// Public chain authorities given up for a finding since the start.
    ///
    /// A finding is a judgement of useless, a disagreement that did not settle, or an answer that
    /// contradicted the store (`rpc::resolver::authority_replaced`). A lost connection does not
    /// count.
    pub authority_replaced_total: u64,
    /// Whether the store holds every order the node's history still shows.
    ///
    /// It is `false` during a fast start ([`crate::faststart`]). The store then follows the chain
    /// from the node's sink while the history is indexed into a second store, and `backfill` says
    /// how far that one is. It stays `false` for good in a store that began after the oldest
    /// history, with `start = "sink"` or a block hash, or after a rebase past unread chain.
    /// `history_from_daa` says where its history begins.
    pub book_complete: bool,
    /// Where the history of this store begins, in DAA.
    ///
    /// It is set only when the history does not begin at the node's pruning point: a start at the
    /// sink or at a block, or a rebase past unread chain. 0 means the start block's score was not
    /// known yet. Orders created before it are not in this store. Absent for a store that read the
    /// node's whole retained history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_from_daa: Option<u64>,
    /// How fast the store closes in on the node and when it will have, while `catching_up`.
    ///
    /// Absent once it follows, and until two samples a second apart are known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<CatchUp>,
    /// Whether the store is lean ([`super::lean`]).
    ///
    /// A lean store keeps the live book and `lean_retention_days` of closed orders, not the
    /// history.
    pub lean: bool,
    /// Closed orders deleted by lean pruning since the start.
    pub lean_orders_pruned_total: u64,
    /// Rows deleted by lean pruning since the start.
    pub lean_rows_pruned_total: u64,
    /// What this process does: `executor`, `indexer`, or both.
    ///
    /// `executor` means the matcher and the keepers of `kobd --executor` work on this store.
    /// `indexer` means the store keeps the history and this API serves it. A lean store makes the
    /// process the Executor alone. `kobd --indexer` is `["indexer"]`.
    pub roles: Vec<&'static str>,
    /// The fast start's backfill: the progress of the full store. Absent once the book is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backfill: Option<Backfill>,
    /// Stretches of chain this store skipped on purpose since the start, oldest first.
    ///
    /// The node pruned the history the store was following, so the store continued from the node's
    /// pruning point ([`SkippedGap`]). Empty, and then absent, when there is none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<SkippedGap>,
    /// Why the follower is halted or cannot write, as an alarm shows it.
    ///
    /// It is set in the states `stopped`, `gap` and `storage_error`. Absent while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt_reason: Option<String>,
    /// Why the read API stopped serving, because it could not keep its listener.
    ///
    /// `kobd --executor` exits on it, and a supervisor restarts it. Absent while it serves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_error: Option<String>,
    /// Free bytes on the data directory's file system, measured about once a minute.
    ///
    /// `None`: not measured yet, or the platform cannot tell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir_free_bytes: Option<u64>,
    /// Size of the database in bytes, its file and write-ahead log, measured with the free bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_bytes: Option<u64>,
    /// Reorgs that removed chain blocks the store no longer holds, so their rows were not reverted.
    ///
    /// Such a reorg is deeper than the reorg window of the store.
    pub deep_reorgs_total: u64,
    /// Whether a lean store was above its size cap (`lean_max_db_mb`) at the last pruning.
    pub lean_over_cap: bool,
    /// Lean pruning failures since the start.
    pub lean_prune_failures_total: u64,
    /// The Executor of this process (`kobd --executor`): its funds, submissions and alarms.
    ///
    /// Absent for `kobd --indexer`. Visible to whoever can reach this API, like the rest of
    /// `/v1/health`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorHealth>,
    /// A KOB release newer than this build that is in use on chain.
    ///
    /// Placements of at least two makers announce it (`super::release`). It is informational:
    /// update when convenient (`docs/ops/release.md`). `null` when there is none.
    pub newer_release_seen: Option<super::release::NewerRelease>,
    /// The Executor's own Prometheus series, served after the indexer's by `GET /v1/metrics`.
    ///
    /// The runner writes them.
    #[serde(skip)]
    pub executor_metrics: String,
}

/// The Executor's part of `/v1/health` (`HealthSnapshot::executor`).
///
/// The runner writes it after each step. Amounts are decimal strings of sompi, like every amount
/// of the API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExecutorHealth {
    /// The operator's balance, including the coins pending transactions hold.
    ///
    /// `None`: not read yet, or not shown (`funds_hidden`).
    pub funds_sompi: Option<String>,
    /// What is spendable now. `None` as for `funds_sompi`.
    pub spendable_sompi: Option<String>,
    /// Whether the amounts are left out.
    ///
    /// They are when the read API is reachable from other hosts and `api.executor_funds` is off,
    /// which is the default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub funds_hidden: bool,
    /// The balance is below the alert threshold (`--low-funds-kas`).
    pub low_funds: bool,
    /// The alert threshold in sompi.
    pub low_funds_threshold_sompi: String,
    /// Whether the Executor is paused.
    pub paused: bool,
    /// Whether the Executor runs in dry-run mode.
    pub dry_run: bool,
    /// Transactions submitted and not accepted yet.
    pub pending: usize,
    /// How long the oldest pending transaction has waited, in seconds (at 10 DAA per second).
    pub oldest_pending_seconds: u64,
    /// Transactions submitted.
    pub submitted_total: u64,
    /// Transactions finalized.
    pub finalized_total: u64,
    /// Transactions given up: the node refused them at a check, or they lost a race.
    pub dropped_total: u64,
    /// Other refusals.
    pub rejected_total: u64,
    /// Funding coins held because a mempool transaction this process does not track spends them.
    pub held_coins: usize,
    /// Unix time of the last submission a node took. `None`: none yet.
    pub last_submit_unix: Option<u64>,
    /// Unix time of the last step.
    pub last_step_unix: u64,
    /// Why the last step failed. `None`: it did not.
    pub last_tick_error: Option<String>,
    /// The local clock minus the chain's, in seconds.
    ///
    /// The chain's clock is the timestamp of the newest chain block. `None`: not measured yet.
    pub clock_skew_seconds: Option<i64>,
    /// Whether the skew is beyond the bound (30 s), so that day orders near their deadline are not
    /// filled.
    pub clock_alarm: bool,
    /// Starts of this data directory before this one, that is, process restarts.
    pub restarts: u64,
    /// Transactions in the in-flight journal, which a restart resends.
    pub journal_entries: usize,
}

/// The catch-up of a store that is behind the node (`HealthSnapshot::catch_up`).
///
/// It is measured over the last minute.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CatchUp {
    /// How fast the history is indexed, in DAA of chain per second of wall time.
    ///
    /// The chain itself moves at about 10.
    pub rate_daa_per_sec: f64,
    /// Seconds until the store has caught up at that pace. Absent while the lag does not shrink.
    pub eta_seconds: Option<u64>,
}

/// A stretch of chain the store skipped on purpose ([`super::recovery`]).
///
/// The node pruned it before the store had read it. The orders the store already had were
/// reconciled with the node's UTXO set: closed with reason `gap`, or adopted. An order created
/// inside the stretch and still open is not in the store until its maker imports it
/// (`docs/ops/executor.md`, 7.4 and 7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedGap {
    /// DAA score of the cursor the store stood at.
    pub from_daa: u64,
    /// DAA score of the node's pruning point the store continued from.
    pub to_daa: u64,
    /// The block the store continued from.
    pub to_hash: Hash32,
    /// Unix time of the move.
    pub at_unix: u64,
    /// Outputs of tracked orders closed with reason `gap` by the reconciliation.
    pub orders_closed: u64,
    /// Continuations of tracked orders adopted by the reconciliation.
    pub orders_adopted: u64,
}

impl SkippedGap {
    /// The skipped chain in DAA.
    pub fn daa(&self) -> u64 {
        self.to_daa.saturating_sub(self.from_daa)
    }
}

/// The backfill of a fast start: the full store on its way from the pruning point to the tip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Backfill {
    /// The full store's follower state.
    pub state: FollowerState,
    /// DAA score of the full store's cursor.
    pub cursor_daa: u64,
    /// DAA the full store is behind the node (`None`: the node's position is not known yet).
    pub lag_daa: Option<u64>,
    /// The same lag in seconds.
    pub lag_seconds: Option<u64>,
    /// How fast the history is indexed, in DAA of chain per second of wall time.
    ///
    /// The chain itself moves at `daa_per_second`.
    pub rate_daa_per_sec: Option<f64>,
    /// Seconds until the full store has caught up at the recent pace.
    ///
    /// `None`: not known yet, or it is not closing in.
    pub eta_seconds: Option<u64>,
    /// Unix time the fast store started following (the orders it knows were seen since).
    pub tip_since_unix: u64,
    /// Stretches of chain the full store skipped on purpose, because the node pruned the history
    /// it was following ([`SkippedGap`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<SkippedGap>,
}

impl Backfill {
    /// The part of the status line about the fast start.
    ///
    /// It says what the executor trades on and when the full book is there.
    pub fn status_text(&self) -> String {
        let mut s = format!("fast start: trading on the orders since {}", utc_clock(self.tip_since_unix));
        if !self.gaps.is_empty() {
            let skipped: u64 = self.gaps.iter().map(SkippedGap::daa).sum();
            s.push_str(&format!(
                " (the node pruned {} of the history before the full store read it: orders created in it are missing from the full book)",
                human_secs(skipped / 10)
            ));
        }
        match (self.eta_seconds, self.lag_seconds) {
            (Some(eta), Some(lag)) => s.push_str(&format!(", full book in about {} ({} to index)", human_secs(eta), human_secs(lag))),
            (None, Some(lag)) => s.push_str(&format!(", full book: estimating ({} to index)", human_secs(lag))),
            _ => s.push_str(", full book: estimating"),
        }
        s
    }
}

/// `14:32 UTC` of a unix time.
pub fn utc_clock(unix: u64) -> String {
    format!("{:02}:{:02} UTC", unix % 86_400 / 3600, unix % 3600 / 60)
}

/// Estimates how fast the full store of a fast start closes in on the node.
///
/// It uses the lag over the last minute.
#[derive(Debug, Default)]
pub struct BackfillPace {
    /// (seconds since the first sample, lag in DAA, cursor DAA), oldest first.
    samples: std::collections::VecDeque<(f64, u64, u64)>,
}

impl BackfillPace {
    /// How long the pace looks back.
    const SPAN_SECS: f64 = 60.0;

    /// Records a sample taken `at` seconds from some fixed start.
    ///
    /// Returns `(index rate in DAA per second, seconds to catch up)` once two samples at least a
    /// second apart are known. The second value is `None` while the lag does not shrink.
    pub fn push(&mut self, at: f64, lag_daa: u64, cursor_daa: u64) -> (Option<f64>, Option<u64>) {
        self.samples.push_back((at, lag_daa, cursor_daa));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|f| at - f.0 > Self::SPAN_SECS) {
            self.samples.pop_front();
        }
        let Some(&(t0, lag0, cur0)) = self.samples.front() else { return (None, None) };
        let dt = at - t0;
        if dt < 1.0 {
            return (None, None);
        }
        let rate = cursor_daa.saturating_sub(cur0) as f64 / dt;
        let closing = (lag0 as f64 - lag_daa as f64) / dt;
        let eta = (closing > 0.0).then(|| (lag_daa as f64 / closing).ceil() as u64);
        (Some(rate), eta)
    }
}

impl HealthSnapshot {
    /// A snapshot in the `starting` state for `network`, with no progress recorded.
    pub fn new(network: &str) -> Self {
        HealthSnapshot {
            state: FollowerState::Starting,
            network: network.to_string(),
            node_version: None,
            cursor_hash: None,
            cursor_daa: 0,
            node_daa: None,
            last_error: None,
            gap_reason: None,
            last_poll_unix_ms: 0,
            last_progress_unix_ms: 0,
            reorgs_total: 0,
            reverted_blocks_total: 0,
            blocks_applied_total: 0,
            relevant_txs_total: 0,
            orders_total: 0,
            records_next_n: 0,
            lag_blue: None,
            batch_window_blue: None,
            last_batch_blocks: 0,
            last_batch_txs: 0,
            last_batch_bytes: 0,
            last_fetch_ms: 0,
            wire_bytes_total: 0,
            txs_fetched_total: 0,
            vspc_timeouts_total: 0,
            consecutive_failures: 0,
            prefetch_windows: 0,
            prefetch_in_flight: 0,
            prefetch_bytes: 0,
            prefetch_bytes_peak: 0,
            prefetch_window_blocks: 0,
            prefetch_discards_total: 0,
            prefetch_oversize_total: 0,
            lag_tolerance_daa: 0,
            untrusted_windows_total: 0,
            untrusted_blocks_total: 0,
            confirmed_blocks_total: 0,
            lies_total: 0,
            nodes: vec![],
            node_source: None,
            node_alarms: vec![],
            authority_replaced_total: 0,
            book_complete: true,
            history_from_daa: None,
            catch_up: None,
            lean: false,
            lean_orders_pruned_total: 0,
            lean_rows_pruned_total: 0,
            roles: vec!["indexer"],
            backfill: None,
            gaps: vec![],
            halt_reason: None,
            api_error: None,
            data_dir_free_bytes: None,
            db_bytes: None,
            deep_reorgs_total: 0,
            lean_over_cap: false,
            lean_prune_failures_total: 0,
            executor: None,
            newer_release_seen: None,
            executor_metrics: String::new(),
        }
    }

    /// Why this process should end so that a supervisor restarts it (`kobd --executor`).
    ///
    /// The reasons are that the read API stopped serving, that the follower stopped (`stopped`: its
    /// record log could not be written), and that it cannot write its database (`storage_error`).
    /// A restart may cure the last two once the disk has space again. The caller waits a few
    /// minutes before acting on a halt.
    ///
    /// A store halted at a gap (`gap`) is not one of them. The follower moves a pruned gap on its
    /// own. A gap it gave up on needs the operator, because it was moved too often in this run or
    /// the node contradicts the store. A restart would only move the store again and reset that
    /// bound every few minutes. So the process stays up and says so (`kob_indexer_halted`, the
    /// status line). Returns `None` when none of this applies.
    pub fn halted(&self) -> Option<(bool, String)> {
        if let Some(e) = &self.api_error {
            return Some((true, format!("the read API stopped: {e}")));
        }
        // A storage error stays one while node errors come between the failed commits. Only
        // progress clears `halt_reason`.
        let halted = matches!(self.state, FollowerState::Stopped | FollowerState::StorageError)
            || (self.halt_reason.is_some() && self.state == FollowerState::NodeUnavailable);
        if !halted {
            return None;
        }
        let why = self.halt_reason.clone().or_else(|| self.gap_reason.clone()).or_else(|| self.last_error.clone());
        Some((false, format!("the indexer is {}: {}", state_name(self.state), why.unwrap_or_else(|| "no reason recorded".into()))))
    }

    /// VSPC bytes on the wire per accepted transaction since the start.
    ///
    /// The bandwidth an indexer needs is this times the chain's transaction rate, see
    /// `docs/ops/executor.md`.
    pub fn bytes_per_tx(&self) -> Option<u64> {
        (self.txs_fetched_total > 0).then(|| self.wire_bytes_total / self.txs_fetched_total)
    }

    /// Prometheus text exposition of the follower's state.
    ///
    /// `GET /v1/metrics` and the executor's metrics file serve it.
    pub fn metrics_text(&self) -> String {
        let mut s = String::new();
        let mut m = |name: &str, kind: &str, help: &str, v: u64| {
            s.push_str(&format!(
                "# HELP {name} {help}
# TYPE {name} {kind}
{name} {v}
"
            ));
        };
        m(
            "kob_indexer_within_lag_tolerance",
            "gauge",
            "1 while the store is caught up or within the lag tolerance (max_lag_secs): the matcher plans then.",
            self.within_lag_tolerance() as u64,
        );
        m(
            "kob_indexer_lag_tolerance_daa",
            "gauge",
            "The lag tolerance in DAA (max_lag_secs x DAA per second; 0: strict).",
            self.lag_tolerance_daa,
        );
        m(
            "kob_indexer_caught_up",
            "gauge",
            "1 while the store is caught up with the node (the matcher plans only then).",
            self.caught_up() as u64,
        );
        m(
            "kob_indexer_book_complete",
            "gauge",
            "0 while the store lacks part of the history: a fast start (the full book is being indexed) or a start at the sink / a block.",
            self.book_complete as u64,
        );
        m("kob_indexer_lean", "gauge", "1 with lean storage: the store keeps the live book, not the history.", self.lean as u64);
        if self.lean {
            m(
                "kob_indexer_lean_orders_pruned_total",
                "counter",
                "Closed orders the lean storage deleted since the start.",
                self.lean_orders_pruned_total,
            );
            m(
                "kob_indexer_lean_rows_pruned_total",
                "counter",
                "Rows the lean storage deleted since the start.",
                self.lean_rows_pruned_total,
            );
        }
        m(
            "kob_indexer_gaps_total",
            "counter",
            "Stretches of chain skipped on purpose since the start (the node pruned the history the store was following).",
            self.gaps.len() as u64,
        );
        m(
            "kob_indexer_gap_daa_total",
            "counter",
            "DAA of chain skipped on purpose since the start (orders created in it are not in the store).",
            self.gaps.iter().map(SkippedGap::daa).sum(),
        );
        if let Some(b) = &self.backfill {
            m(
                "kob_indexer_backfill_lag_daa",
                "gauge",
                "DAA the full store of a fast start is behind the node.",
                b.lag_daa.unwrap_or(0),
            );
        }
        m(
            "kob_indexer_halted",
            "gauge",
            "1 while the follower is halted (stopped, gap) or cannot write its database (storage_error): halt_reason of /v1/health says why.",
            matches!(self.state, FollowerState::Stopped | FollowerState::Gap | FollowerState::StorageError) as u64,
        );
        m(
            "kob_indexer_storage_error",
            "gauge",
            "1 while the database cannot be written (disk full, I/O error) or is corrupt.",
            (self.state == FollowerState::StorageError) as u64,
        );
        m(
            "kob_api_failed",
            "gauge",
            "1 once the read API stopped serving (api_error of /v1/health).",
            self.api_error.is_some() as u64,
        );
        if let Some(b) = self.data_dir_free_bytes {
            m("kob_data_dir_free_bytes", "gauge", "Free bytes on the data directory's file system.", b);
        }
        if let Some(b) = self.db_bytes {
            m("kob_indexer_db_bytes", "gauge", "Bytes of the database (its file and write-ahead log).", b);
        }
        m(
            "kob_indexer_deep_reorgs_total",
            "counter",
            "Reorgs deeper than the stored chain (their rows were not reverted).",
            self.deep_reorgs_total,
        );
        if self.lean {
            m("kob_indexer_lean_over_cap", "gauge", "1 while a lean store is above its size cap.", self.lean_over_cap as u64);
            m(
                "kob_indexer_lean_prune_failures_total",
                "counter",
                "Lean pruning batches that failed since the start.",
                self.lean_prune_failures_total,
            );
        }
        m("kob_indexer_cursor_daa", "gauge", "DAA score of the cursor.", self.cursor_daa);
        m("kob_indexer_lag_daa", "gauge", "DAA the store is behind the node.", self.lag_daa().unwrap_or(0));
        m(
            "kob_indexer_lag_blue",
            "gauge",
            "Blue score the cursor is behind the node's sink at the last request.",
            self.lag_blue.unwrap_or(0),
        );
        m("kob_indexer_batch_window_blue", "gauge", "Current VSPC batch window (blue score).", self.batch_window_blue.unwrap_or(0));
        m("kob_indexer_last_batch_txs", "gauge", "Accepted transactions in the last batch.", self.last_batch_txs);
        m("kob_indexer_last_batch_bytes", "gauge", "Bytes of the last batch on the wire.", self.last_batch_bytes);
        m("kob_indexer_last_fetch_ms", "gauge", "Fetch time of the last batch.", self.last_fetch_ms);
        m("kob_indexer_wire_bytes_total", "counter", "VSPC bytes fetched since the start.", self.wire_bytes_total);
        m("kob_indexer_txs_fetched_total", "counter", "Accepted transactions fetched since the start.", self.txs_fetched_total);
        m(
            "kob_indexer_bytes_per_tx",
            "gauge",
            "VSPC bytes per accepted transaction since the start.",
            self.bytes_per_tx().unwrap_or(0),
        );
        m(
            "kob_indexer_vspc_timeouts_total",
            "counter",
            "VSPC requests that timed out (each one split the batch).",
            self.vspc_timeouts_total,
        );
        m("kob_indexer_prefetch_windows", "gauge", "VSPC windows fetched or in flight ahead of the cursor.", self.prefetch_windows);
        m("kob_indexer_prefetch_in_flight", "gauge", "VSPC windows in flight ahead of the cursor.", self.prefetch_in_flight);
        m("kob_indexer_prefetch_bytes", "gauge", "Bytes of the windows held ahead of the cursor.", self.prefetch_bytes);
        m(
            "kob_indexer_prefetch_bytes_peak",
            "gauge",
            "Most bytes held ahead of the cursor since the start.",
            self.prefetch_bytes_peak,
        );
        m(
            "kob_indexer_prefetch_discards_total",
            "counter",
            "Prefetched window plans dropped because the chain moved under them.",
            self.prefetch_discards_total,
        );
        m(
            "kob_indexer_prefetch_oversize_total",
            "counter",
            "Prefetched windows refused as larger than their share of the memory budget (then split).",
            self.prefetch_oversize_total,
        );
        m("kob_indexer_consecutive_failures", "gauge", "Failed polls in a row.", self.consecutive_failures as u64);
        m("kob_indexer_blocks_applied_total", "counter", "Chain blocks applied since the start.", self.blocks_applied_total);
        m(
            "kob_indexer_last_progress_seconds",
            "gauge",
            "Unix time of the last progress (or confirmed idle poll).",
            self.last_progress_unix_ms / 1000,
        );
        m(
            "kob_indexer_untrusted_blocks_total",
            "counter",
            "Chain blocks fetched from a node other than the primary (checked against it).",
            self.untrusted_blocks_total,
        );
        m(
            "kob_indexer_confirmed_blocks_total",
            "counter",
            "Chain blocks from another node compared with the primary's copy before applying (they can matter to KOB).",
            self.confirmed_blocks_total,
        );
        m("kob_indexer_node_lies_total", "counter", "Contradictions of the primary found in another node's data.", self.lies_total);
        if let Some(f) = &self.node_source {
            m(
                "kob_indexer_node_failover_active",
                "gauge",
                "1 while the fallback (public nodes) is the chain source, 0 on the configured node.",
                (f.active == "fallback") as u64,
            );
            m("kob_indexer_node_failover_switches_total", "counter", "Switches of the chain source since the start.", f.switches);
            m(
                "kob_indexer_node_failover_refused_total",
                "counter",
                "Answers refused right after a switch (a deep reorg or an unknown cursor).",
                f.refused,
            );
        }
        m(
            "kob_newer_release_seen",
            "gauge",
            "1 while a KOB release newer than this build is in use on chain (newer_release_seen of /v1/health says which): update.",
            self.newer_release_seen.is_some() as u64,
        );
        m(
            "kob_indexer_node_alarms",
            "gauge",
            "Alarms of nodes named in the configuration (node_alarms of /v1/health); 0 when every node is fine.",
            self.node_alarms.len() as u64,
        );
        m(
            "kob_indexer_authority_replaced_total",
            "counter",
            "Public chain authorities given up for a finding (useless, a disagreement that did not settle, an answer contradicting the store); not lost connections.",
            self.authority_replaced_total,
        );
        if !self.nodes.is_empty() {
            let mut per = |name: &str, kind: &str, help: &str, v: &dyn Fn(&crate::rpc::multi::NodeStatus) -> u64| {
                s.push_str(&format!(
                    "# HELP {name} {help}
# TYPE {name} {kind}
"
                ));
                for n in &self.nodes {
                    s.push_str(&format!(
                        "{name}{{node=\"{}\",role=\"{}\"}} {}
",
                        n.url,
                        n.role,
                        v(n)
                    ));
                }
            };
            per("kob_indexer_node_bytes_total", "counter", "Window bytes fetched from the node.", &|n| n.bytes);
            per("kob_indexer_node_windows_total", "counter", "Windows fetched from the node.", &|n| n.windows_ok);
            per("kob_indexer_node_failures_total", "counter", "Windows the node failed.", &|n| n.windows_failed);
            per("kob_indexer_node_bytes_per_sec", "gauge", "Recent window rate of the node.", &|n| n.bytes_per_sec.unwrap_or(0));
            per("kob_indexer_node_dropped", "gauge", "1 once the node contradicted the primary.", &|n| {
                n.dropped_reason.is_some() as u64
            });
            per(
                "kob_indexer_node_replaced_total",
                "counter",
                "Public nodes the resolver entry gave up (failed, hanging, unsynced, behind, wrong network, no UTXO index, contradicting).",
                &|n| n.replaced as u64,
            );
        }
        s.push_str(&crate::version::build_info_metric());
        s.push_str(
            "# HELP kob_role 1 for each role this process runs: executor (matcher and keepers), indexer (the full history and the read API).
# TYPE kob_role gauge
",
        );
        for role in ["executor", "indexer"] {
            s.push_str(&format!(
                "kob_role{{role=\"{role}\"}} {}
",
                self.roles.contains(&role) as u64
            ));
        }
        s
    }

    /// Lag behind the node in DAA score. At 10 DAA per second, one DAA is a tenth of a second.
    pub fn lag_daa(&self) -> Option<u64> {
        self.node_daa.map(|n| n.saturating_sub(self.cursor_daa))
    }

    /// Whether the store is caught up with the node.
    ///
    /// That holds when the follower's last poll applied the chain to within its catch-up distance
    /// of the node's sink (`following`). Until then the store lags the chain, and an order or token
    /// it lists may already be spent. That covers `starting` after a restart, `catching_up`, node
    /// unavailable and gap. The matcher, the keepers and the maintenance jobs plan nothing in that
    /// time. `GET /v1/health` reports it as `caught_up`.
    pub fn caught_up(&self) -> bool {
        self.state == FollowerState::Following && self.node_daa.is_some()
    }

    /// Whether the store may be planned against although it is not caught up.
    ///
    /// That holds when the follower is applying the chain (`following` or `catching_up`) and the
    /// node's sink is at most `lag_tolerance_daa` ahead of its cursor. A stale view is harmless:
    /// the covenants enforce every order's terms, and a spent input only loses the race
    /// (`MissingInput`). The tolerance bounds how many such lost transactions a lagging store can
    /// cause. A tolerance of 0 never qualifies, and neither does any state other than the two
    /// above (`starting`, node unavailable, gap, stopped). A caught-up store (`caught_up`) is
    /// always within.
    pub fn within_lag_tolerance(&self) -> bool {
        if self.caught_up() {
            return true;
        }
        self.lag_tolerance_daa > 0
            && matches!(self.state, FollowerState::Following | FollowerState::CatchingUp)
            && self.lag_daa().is_some_and(|l| l <= self.lag_tolerance_daa)
    }

    /// Why the matcher, keepers and maintenance must not plan against the store.
    ///
    /// It is neither caught up nor within the lag tolerance. `None`: they may plan.
    pub fn not_ready(&self) -> Option<String> {
        if self.within_lag_tolerance() {
            None
        } else {
            self.not_caught_up()
        }
    }

    /// One line for an operator watching the console (`--status-secs`).
    ///
    /// It gives the follower's state and lag, in time as well as DAA (10 per second), and the
    /// orders indexed. While the node is unavailable or the chain has a gap, it adds the last
    /// error.
    pub fn status_line(&self) -> String {
        let state = match self.state {
            FollowerState::Starting => "starting",
            FollowerState::CatchingUp => "catching up",
            FollowerState::Following => "following",
            FollowerState::NodeUnavailable => "waiting for the node",
            FollowerState::Gap => "STOPPED at a gap (docs/ops/executor.md, Part B 7.4)",
            FollowerState::Stopped => "STOPPED",
            FollowerState::StorageError => "CANNOT WRITE ITS DATABASE",
        };
        let lag = match self.lag_daa() {
            Some(l) if self.caught_up() => format!("caught up ({l} DAA behind the node)"),
            Some(l) => format!("{l} DAA behind the node (about {})", human_secs(l / 10)),
            None => "node position unknown".to_string(),
        };
        // While catching up, show the pace and the time to go, measured over the last minute. A
        // long first sync is then not a guess.
        let lag = match (&self.catch_up, self.state) {
            (Some(c), FollowerState::CatchingUp) => {
                // The chain itself moves at 10 DAA per second.
                let x = c.rate_daa_per_sec / 10.0;
                let x = if x >= 10.0 { format!("{x:.0}") } else { format!("{x:.1}") };
                match c.eta_seconds {
                    Some(eta) => format!("{lag}, indexing {x}x chain speed, caught up in about {}", human_secs(eta)),
                    None => format!("{lag}, indexing {x}x chain speed (not closing in)"),
                }
            }
            (None, FollowerState::CatchingUp) => format!("{lag}, catch-up time: estimating"),
            _ => lag,
        };
        let mut s = format!("indexer {state}, {lag}, {} orders indexed", self.orders_total);
        // With several nodes (a resolver pair), show the hosts in use, which is the authority and
        // how each is doing.
        if !self.nodes.is_empty() {
            let nodes: Vec<String> = self
                .nodes
                .iter()
                .map(|n| {
                    let name = n.node.clone().unwrap_or_else(|| n.url.clone());
                    let role = if n.role == "primary" { "authority" } else { "cross-check" };
                    let mut x = format!("{name} ({role}, {}", n.state);
                    if n.replaced > 0 {
                        x.push_str(&format!(", replaced {}x", n.replaced));
                        if let Some(why) = &n.cut_reason {
                            x.push_str(&format!(", last: {why}"));
                        }
                    }
                    if n.disagreements > 0 {
                        x.push_str(&format!(", {} disagreements", n.disagreements));
                    }
                    x + ")"
                })
                .collect();
            s.push_str(&format!(" | nodes: {}", nodes.join(", ")));
        }
        if let Some(f) = &self.node_source {
            if f.active != "primary" || f.switches > 0 {
                s.push_str(&format!(" | node source: {} ({} switches", f.active, f.switches));
                if f.refused > 0 {
                    s.push_str(&format!(", {} refused", f.refused));
                }
                s.push(')');
            }
        }
        if matches!(
            self.state,
            FollowerState::NodeUnavailable | FollowerState::Gap | FollowerState::Stopped | FollowerState::StorageError
        ) {
            if let Some(e) = self.halt_reason.as_ref().or(self.gap_reason.as_ref()).or(self.last_error.as_ref()) {
                s.push_str(&format!(" ({e})"));
            }
        }
        if let Some(e) = &self.api_error {
            s.push_str(&format!(" | READ API STOPPED: {e}"));
        }
        if self.lean && self.lean_over_cap {
            s.push_str(" | WARNING: the lean store is above its size cap (lean_max_db_mb)");
        }
        if !self.node_alarms.is_empty() {
            s.push_str(&format!(" | NODE ALARM: {}", self.node_alarms.join("; ")));
        }
        if let Some(from) = self.history_from_daa {
            let ago = self.node_daa.filter(|_| from > 0).map(|n| format!("about {} ago", human_secs(n.saturating_sub(from) / 10)));
            s.push_str(&format!(
                " | history from {} (older orders are not in this store)",
                ago.unwrap_or_else(|| "the start point".to_string())
            ));
        }
        if !self.gaps.is_empty() {
            let skipped: u64 = self.gaps.iter().map(SkippedGap::daa).sum();
            s.push_str(&format!(
                " | WARNING: {} of chain was skipped ({} time{}): the node had pruned it; orders created in it are not in the store (docs/ops/executor.md, Part B 7.4)",
                human_secs(skipped / 10),
                self.gaps.len(),
                if self.gaps.len() == 1 { "" } else { "s" }
            ));
        }
        if let Some(n) = &self.newer_release_seen {
            s.push_str(&format!(" | {}", super::release::notice_text(n)));
        }
        s
    }

    /// Why the store is not caught up. `None`: it is.
    pub fn not_caught_up(&self) -> Option<String> {
        if self.caught_up() {
            return None;
        }
        let state = state_name(self.state);
        Some(match self.lag_daa() {
            Some(lag) => format!("indexer {state}, {lag} DAA behind the node"),
            None => format!("indexer {state}, node position unknown"),
        })
    }
}

/// The state as `/v1/health` spells it, for example `catching_up` or `storage_error`.
pub fn state_name(s: FollowerState) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// A duration for a person: `45 s`, `12 min`, `3 h 20 min`, `2 d 4 h`.
pub fn human_secs(s: u64) -> String {
    match s {
        0..=89 => format!("{s} s"),
        90..=5_399 => format!("{} min", (s + 30) / 60),
        5_400..=172_799 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        _ => format!("{} d {} h", s / 86_400, s % 86_400 / 3600),
    }
}

/// Shared handle to the health snapshot: the follower writes, the API reads.
pub struct HealthState(RwLock<HealthSnapshot>);

impl HealthState {
    /// A handle holding a fresh [`HealthSnapshot`] for `network`.
    pub fn new(network: &str) -> Self {
        HealthState(RwLock::new(HealthSnapshot::new(network)))
    }

    /// A copy of the current snapshot.
    pub fn snapshot(&self) -> HealthSnapshot {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Changes the snapshot in place through `f`.
    pub fn update(&self, f: impl FnOnce(&mut HealthSnapshot)) {
        let mut g = self.0.write().unwrap_or_else(|e| e.into_inner());
        f(&mut g);
    }
}

/// A 64-bit quantity as a decimal string, as the REST API spells them.
///
/// A JSON number loses precision above 2^53 in JavaScript, and a supply can reach 2.9e18 base
/// units.
mod dec_str {
    use serde::Serializer;

    pub fn int<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(v)
    }

    pub fn opt<S: Serializer>(v: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.collect_str(v),
            None => s.serialize_none(),
        }
    }
}

/// A committed fill, as pushed to WebSocket subscribers.
///
/// `price`, `amount` and `payout` are decimal strings, like every quantity of the REST API
/// (`docs/ops/executor.md`, 5.0). `side` and `daa` are numbers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FillNotice {
    /// Covenant id of the order.
    pub order: Hash32,
    /// The token the order trades, if any.
    pub token: Option<Hash32>,
    /// 1: an ask (sell) was filled, 2: a bid (buy) was filled.
    pub side: u8,
    /// The order's price, in sompi per whole token.
    ///
    /// It is `null` for a pair order and the conditional kinds.
    #[serde(serialize_with = "dec_str::opt")]
    pub price: Option<i64>,
    /// Base units filled.
    #[serde(serialize_with = "dec_str::int")]
    pub amount: i64,
    /// What the maker was paid, sompi (`null` when it is not a payment to the maker).
    #[serde(serialize_with = "dec_str::opt")]
    pub payout: Option<i64>,
    /// The fill transaction.
    pub txid: Hash32,
    /// The chain block that accepted it.
    pub block: Hash32,
    /// DAA score the fill was accepted at.
    pub daa: u64,
}

/// What one committed batch changed.
///
/// It is published on a broadcast channel after the commit.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexEvent {
    /// Hash of the cursor after the batch.
    pub cursor_hash: Option<Hash32>,
    /// DAA score of the cursor after the batch.
    pub cursor_daa: u64,
    /// Chain blocks reverted by the batch.
    pub reverted_blocks: u64,
    /// Chain blocks added by the batch.
    pub added_blocks: u64,
    /// Covenant ids of orders whose state changed (created, filled, closed, or reverted).
    pub orders: Vec<Hash32>,
    /// Token covenant ids whose books may have changed.
    pub tokens: Vec<Hash32>,
    /// Fills committed by the batch.
    pub fills: Vec<FillNotice>,
}

impl IndexEvent {
    /// Whether the batch changed nothing that subscribers follow.
    pub fn is_empty(&self) -> bool {
        self.reverted_blocks == 0 && self.orders.is_empty() && self.fills.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_says_where_the_indexer_is() {
        let mut h = HealthSnapshot::new("testnet-10");
        assert_eq!(h.status_line(), "indexer starting, node position unknown, 0 orders indexed");
        h.state = FollowerState::CatchingUp;
        h.cursor_daa = 1_000;
        h.node_daa = Some(1_000 + 36_000 * 3 + 12_000);
        h.orders_total = 7;
        assert_eq!(
            h.status_line(),
            "indexer catching up, 120000 DAA behind the node (about 3 h 20 min), catch-up time: estimating, 7 orders indexed"
        );
        // The pace of the last minute: how fast the store closes in on the node, and when it will
        // have.
        h.catch_up = Some(CatchUp { rate_daa_per_sec: 150.0, eta_seconds: Some(8_640) });
        assert_eq!(
            h.status_line(),
            "indexer catching up, 120000 DAA behind the node (about 3 h 20 min), indexing 15x chain speed, caught up in about 2 h 24 min, 7 orders indexed"
        );
        h.catch_up = Some(CatchUp { rate_daa_per_sec: 5.0, eta_seconds: None });
        assert!(h.status_line().contains("indexing 0.5x chain speed (not closing in)"), "{}", h.status_line());
        h.catch_up = None;
        h.state = FollowerState::Following;
        h.node_daa = Some(1_004);
        assert_eq!(h.status_line(), "indexer following, caught up (4 DAA behind the node), 7 orders indexed");
        h.state = FollowerState::NodeUnavailable;
        h.last_error = Some("getServerInfo: transport: refused".into());
        assert!(
            h.status_line().starts_with("indexer waiting for the node, ")
                && h.status_line().ends_with("(getServerInfo: transport: refused)")
        );
        // A resolver pair: both hosts, their roles and states, replacements and disagreements.
        h.state = FollowerState::Following;
        h.last_error = None;
        h.nodes = vec![
            crate::rpc::multi::NodeStatus {
                url: "resolver".into(),
                node: Some("muon-10.kaspa.blue".into()),
                role: "primary".into(),
                state: "ok".into(),
                replaced: 1,
                ..Default::default()
            },
            crate::rpc::multi::NodeStatus {
                url: "resolver:2".into(),
                node: Some("vector-10.kaspa.green".into()),
                role: "secondary".into(),
                state: "backoff".into(),
                disagreements: 2,
                ..Default::default()
            },
        ];
        assert!(
            h.status_line().ends_with(
                " | nodes: muon-10.kaspa.blue (authority, ok, replaced 1x), vector-10.kaspa.green (cross-check, backoff, 2 disagreements)"
            ),
            "{}",
            h.status_line()
        );
        assert_eq!(
            [0, 89, 90, 3_599, 5_400, 172_799, 200_000].map(human_secs),
            ["0 s", "89 s", "2 min", "60 min", "1 h 30 min", "47 h 59 min", "2 d 7 h"]
        );
    }

    /// A newer release seen on chain shows in the status line, the health and the metrics.
    #[test]
    fn a_newer_release_on_chain_is_in_the_status_line_the_health_and_the_metrics() {
        let mut h = HealthSnapshot::new("testnet-10");
        h.state = FollowerState::Following;
        h.node_daa = Some(4);
        let plain = h.status_line();
        assert!(!plain.contains("release"), "{plain}");
        assert_eq!(serde_json::to_value(&h).unwrap()["newer_release_seen"], serde_json::Value::Null);
        assert!(h.metrics_text().contains("\nkob_newer_release_seen 0\n"));
        h.newer_release_seen =
            Some(super::super::release::NewerRelease { version: kob_protocol::payload::Release::new(0, 1, 1), first_seen_daa: 123 });
        assert_eq!(h.status_line(), format!("{plain} | a newer KOB release is in use on chain (0.1.1): update"));
        assert_eq!(
            serde_json::to_value(&h).unwrap()["newer_release_seen"],
            serde_json::json!({"version": "0.1.1", "first_seen_daa": 123})
        );
        assert!(h.metrics_text().contains("\nkob_newer_release_seen 1\n"));
    }

    /// A store that began after the oldest history is not a complete book, for good.
    ///
    /// The health says where its history begins, and the status line says that older orders are
    /// not in it.
    #[test]
    fn a_store_that_began_late_says_so() {
        let mut h = HealthSnapshot::new("testnet-10");
        assert!(h.book_complete && h.history_from_daa.is_none());
        let v = serde_json::to_value(&h).unwrap();
        assert!(v.get("history_from_daa").is_none() && v.get("catch_up").is_none());
        h.state = FollowerState::Following;
        h.cursor_daa = 100_000;
        h.node_daa = Some(100_004);
        h.book_complete = false;
        h.history_from_daa = Some(64_000);
        assert!(h.status_line().contains("history from about 60 min ago"), "{}", h.status_line());
        assert!(h.status_line().contains("older orders are not in this store"), "{}", h.status_line());
        // The start block's score was not known: the line still says it.
        h.history_from_daa = Some(0);
        assert!(h.status_line().contains("history from the start point"), "{}", h.status_line());
        assert_eq!(serde_json::to_value(&h).unwrap()["history_from_daa"], 0);
    }

    #[test]
    fn the_backfill_pace_estimates_when_the_full_book_is_there() {
        let mut p = BackfillPace::default();
        // One sample: nothing to compare with.
        assert_eq!(p.push(0.0, 360_000, 1_000), (None, None));
        // The cursor moves 100 DAA per second while the chain moves 10: the lag shrinks by 90 per
        // second.
        let (rate, eta) = p.push(10.0, 360_000 - 900, 2_000);
        assert_eq!(rate, Some(100.0));
        assert_eq!(eta, Some(3_990));
        // A store that does not close in has no estimate.
        let mut q = BackfillPace::default();
        q.push(0.0, 1_000, 5_000);
        assert_eq!(q.push(10.0, 1_100, 5_100), (Some(10.0), None));
        // Only the last minute counts.
        let mut r = BackfillPace::default();
        r.push(0.0, 10_000_000, 0);
        r.push(100.0, 9_000_000, 10_000_000);
        let (rate, _) = r.push(130.0, 8_999_000, 10_003_000);
        assert_eq!(rate, Some(100.0), "the sample 100 s ago is out of the window");

        let b = |eta: Option<u64>, lag: Option<u64>| Backfill {
            state: FollowerState::CatchingUp,
            cursor_daa: 1,
            lag_daa: lag.map(|l| l * 10),
            lag_seconds: lag,
            rate_daa_per_sec: None,
            eta_seconds: eta,
            tip_since_unix: 1_790_000_000 + 14 * 3600 + 32 * 60,
            gaps: vec![],
        };
        assert_eq!(utc_clock(14 * 3600 + 32 * 60 + 7), "14:32 UTC");
        assert_eq!(
            b(Some(27_600), Some(107_000)).status_text(),
            format!(
                "fast start: trading on the orders since {}, full book in about 7 h 40 min (29 h 43 min to index)",
                utc_clock(1_790_000_000 + 14 * 3600 + 32 * 60)
            )
        );
        assert!(b(None, Some(107_000)).status_text().ends_with("full book: estimating (29 h 43 min to index)"));
        assert!(b(None, None).status_text().ends_with(", full book: estimating"));
        // The health JSON a client reads.
        let mut h = HealthSnapshot::new("testnet-10");
        assert!(h.book_complete && serde_json::to_value(&h).unwrap().get("backfill").is_none());
        h.book_complete = false;
        h.backfill = Some(b(Some(60), Some(600)));
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!((v["book_complete"].clone(), v["backfill"]["eta_seconds"].clone()), (false.into(), 60.into()));
        assert!(
            h.metrics_text().contains("kob_indexer_book_complete 0") && h.metrics_text().contains("kob_indexer_backfill_lag_daa 6000")
        );
    }

    /// What ends `kobd --executor`: a halted follower, a storage error that node errors
    /// interrupt, or a dead API. What does not: the node away, or catching up. The metrics and
    /// the status line name it.
    #[test]
    fn a_halt_or_a_storage_error_is_named_and_ends_the_executor() {
        let mut h = HealthSnapshot::new("testnet-10");
        for s in [FollowerState::Starting, FollowerState::CatchingUp, FollowerState::Following, FollowerState::NodeUnavailable] {
            h.state = s;
            assert!(h.halted().is_none(), "{s:?}");
        }
        h.state = FollowerState::StorageError;
        h.halt_reason = Some("the disk of the data directory is full: free space".into());
        let (now, why) = h.halted().unwrap();
        assert!(!now && why.contains("storage_error") && why.contains("disk"), "{why}");
        assert!(h.status_line().contains("CANNOT WRITE ITS DATABASE") && h.status_line().contains("disk"), "{}", h.status_line());
        let m = h.metrics_text();
        assert!(m.contains("kob_indexer_storage_error 1") && m.contains("kob_indexer_halted 1"), "{m}");
        assert_eq!(serde_json::to_value(&h).unwrap()["state"], "storage_error");
        // A node error between two failed commits keeps it a storage error.
        h.state = FollowerState::NodeUnavailable;
        assert!(h.halted().is_some());
        h.halt_reason = None;
        assert!(h.halted().is_none());
        // A store halted at a gap is not restarted. The follower moves a pruned gap itself, and a
        // gap it gave up on needs the operator, because a restart would reset the bound of moves
        // every few minutes.
        h.state = FollowerState::Gap;
        h.gap_reason = Some("pruned".into());
        assert!(h.halted().is_none());
        h.state = FollowerState::Stopped;
        h.halt_reason = Some("record log write failed".into());
        assert!(h.halted().unwrap().1.contains("stopped: record log write failed"));
        h.halt_reason = None;
        h.state = FollowerState::Following;
        h.data_dir_free_bytes = Some(5);
        h.db_bytes = Some(7);
        let m = h.metrics_text();
        assert!(
            m.contains("kob_data_dir_free_bytes 5") && m.contains("kob_indexer_db_bytes 7") && m.contains("kob_build_info{"),
            "{m}"
        );
        assert!(m.contains("kob_indexer_halted 0"));
    }

    #[test]
    fn caught_up_only_while_following_at_a_known_node_position() {
        let mut h = HealthSnapshot::new("testnet-10");
        assert_eq!(h.not_caught_up().as_deref(), Some("indexer starting, node position unknown"));
        h.cursor_hash = Some(Hash32([1; 32]));
        h.cursor_daa = 900;
        h.node_daa = Some(1_000);
        for (s, name) in [
            (FollowerState::Starting, "starting"),
            (FollowerState::CatchingUp, "catching_up"),
            (FollowerState::NodeUnavailable, "node_unavailable"),
            (FollowerState::Gap, "gap"),
            (FollowerState::Stopped, "stopped"),
        ] {
            h.state = s;
            assert!(!h.caught_up());
            assert_eq!(h.not_caught_up(), Some(format!("indexer {name}, 100 DAA behind the node")));
        }
        h.state = FollowerState::Following;
        assert!(h.caught_up() && h.not_caught_up().is_none());
    }

    /// The lag tolerance (`max_lag_secs`).
    ///
    /// `caught_up` stays strict, and `within_lag_tolerance` and `not_ready` follow the bound.
    #[test]
    fn within_lag_tolerance_follows_the_bound_and_never_overrides_caught_up() {
        let mut h = HealthSnapshot::new("testnet-10");
        h.cursor_daa = 1_000;
        h.lag_tolerance_daa = 300;
        // No node position yet: not within, and a reason.
        assert!(!h.within_lag_tolerance());
        assert_eq!(h.not_ready().as_deref(), Some("indexer starting, node position unknown"));
        h.node_daa = Some(1_300);
        // Starting, unavailable, gap and stopped never qualify, however small the lag.
        for s in [FollowerState::Starting, FollowerState::NodeUnavailable, FollowerState::Gap, FollowerState::Stopped] {
            h.state = s;
            assert!(!h.within_lag_tolerance() && h.not_ready().is_some(), "{s:?}");
        }
        // Catching up, or following above the follower's own 100 DAA, within the tolerance: ready,
        // but not caught up.
        h.state = FollowerState::CatchingUp;
        assert!(h.within_lag_tolerance() && h.not_ready().is_none());
        assert!(!h.caught_up(), "caught_up stays strict");
        h.node_daa = Some(1_301);
        assert!(!h.within_lag_tolerance());
        assert_eq!(h.not_ready().as_deref(), Some("indexer catching_up, 301 DAA behind the node"));
        // The strict gate (tolerance 0): only `following`.
        h.lag_tolerance_daa = 0;
        h.node_daa = Some(1_001);
        assert!(!h.within_lag_tolerance() && h.not_ready().is_some());
        h.state = FollowerState::Following;
        assert!(h.within_lag_tolerance() && h.not_ready().is_none());
        // The metrics carry both.
        let m = h.metrics_text();
        assert!(
            m.contains("kob_indexer_caught_up 1")
                && m.contains("kob_indexer_within_lag_tolerance 1")
                && m.contains("kob_indexer_lag_tolerance_daa 0"),
            "{m}"
        );
    }

    #[test]
    fn health_and_metrics_name_the_roles_of_the_process() {
        let roles = |h: &HealthSnapshot| serde_json::to_value(h).unwrap()["roles"].clone();
        // A bare indexer (`kobd --indexer`) is the Indexer alone.
        let mut h = HealthSnapshot::new("testnet-10");
        assert_eq!(roles(&h), serde_json::json!(["indexer"]));
        let m = h.metrics_text();
        assert!(m.contains("kob_role{role=\"indexer\"} 1") && m.contains("kob_role{role=\"executor\"} 0"), "{m}");
        // `kobd --executor` over a full store is the Executor and the Indexer. Over a lean store
        // it is the Executor alone.
        let both = crate::matcher::run::Roles { matcher: true, keeper: true };
        h.roles = both.health_roles(false);
        assert_eq!(roles(&h), serde_json::json!(["executor", "indexer"]));
        let m = h.metrics_text();
        assert!(m.contains("kob_role{role=\"executor\"} 1") && m.contains("kob_role{role=\"indexer\"} 1"), "{m}");
        h.roles = both.health_roles(true);
        assert_eq!(roles(&h), serde_json::json!(["executor"]));
        let m = h.metrics_text();
        assert!(m.contains("kob_role{role=\"executor\"} 1") && m.contains("kob_role{role=\"indexer\"} 0"), "{m}");
        // The keepers alone are still the Executor.
        h.roles = crate::matcher::run::Roles { matcher: false, keeper: true }.health_roles(true);
        assert_eq!(roles(&h), serde_json::json!(["executor"]));
    }
}
