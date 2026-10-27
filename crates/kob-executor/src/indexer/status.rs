//! Shared runtime status (health) and the change feed the follower publishes after each commit.

use crate::hex::Hash32;
use serde::Serialize;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowerState {
    /// Opening the database and connecting to the node.
    Starting,
    /// Applying batches, more chain remains before the node's sink.
    CatchingUp,
    /// At the node's sink and polling.
    Following,
    /// The node is unreachable, not synced, or in a transitional IBD state. The cursor is not advanced.
    NodeUnavailable,
    /// The stored chain cannot be continued from the node (downtime beyond pruning, or a different
    /// network). Operator action needed, see docs/ops/executor.md (Part B, 7.4).
    Gap,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub state: FollowerState,
    pub network: String,
    pub node_version: Option<String>,
    pub cursor_hash: Option<Hash32>,
    pub cursor_daa: u64,
    /// Node virtual DAA score at the last successful poll.
    pub node_daa: Option<u64>,
    pub last_error: Option<String>,
    pub gap_reason: Option<String>,
    pub last_poll_unix_ms: u64,
    /// Last time the cursor advanced or the node confirmed we are at its sink.
    pub last_progress_unix_ms: u64,
    pub reorgs_total: u64,
    pub reverted_blocks_total: u64,
    pub blocks_applied_total: u64,
    pub relevant_txs_total: u64,
    pub orders_total: u64,
    pub records_next_n: u64,
    /// Blue score between the cursor and the node's sink at the last request (`None`: not measured yet).
    pub lag_blue: Option<u64>,
    /// The current VSPC batch window in blue score (`rpc::window`).
    pub batch_window_blue: Option<u64>,
    /// The last batch: chain blocks, accepted transactions, bytes on the wire, fetch time.
    pub last_batch_blocks: u64,
    pub last_batch_txs: u64,
    pub last_batch_bytes: u64,
    pub last_fetch_ms: u64,
    /// Since the start: VSPC bytes and accepted transactions fetched (wire cost per transaction), timeouts.
    pub wire_bytes_total: u64,
    pub txs_fetched_total: u64,
    pub vspc_timeouts_total: u64,
    /// Failed polls in a row (0 after any progress).
    pub consecutive_failures: u32,
    /// Parallel fetch (`fetch_parallel`): windows queued ahead of the cursor, of which in flight; their bytes now and at
    /// most; the current window in chain blocks; plans dropped because the chain moved under them (reorgs).
    pub prefetch_windows: u64,
    pub prefetch_in_flight: u64,
    pub prefetch_bytes: u64,
    pub prefetch_bytes_peak: u64,
    pub prefetch_window_blocks: u64,
    pub prefetch_discards_total: u64,
    /// Prefetched windows whose answer was larger than its allowance (refused, then split or sent again).
    pub prefetch_oversize_total: u64,
    /// The lag in DAA up to which the store counts as usable for planning (`max_lag_secs`; 0: only `following`).
    pub lag_tolerance_daa: u64,
    /// Several nodes (`rpc::multi`): windows fetched from a node other than the primary and their chain blocks; of these, the
    /// chain blocks compared with the primary's copy before applying (`indexer::trust`); contradictions found that way.
    pub untrusted_windows_total: u64,
    pub untrusted_blocks_total: u64,
    pub confirmed_blocks_total: u64,
    pub lies_total: u64,
    /// Per node (empty with a single node).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<crate::rpc::multi::NodeStatus>,
    /// The store holds every order the node's history still shows. `false` during a fast start ([`crate::faststart`]: the store
    /// follows the chain from the node's sink, and the history is being indexed into a second store; `backfill` then says how far
    /// that one is), and for good in a store that began after the oldest history (`start = "sink"` or a block hash, or a rebase past
    /// unread chain: `history_from_daa` says where its history begins).
    pub book_complete: bool,
    /// Where the history of this store begins, in DAA, when it does not begin at the node's pruning point (a start at the sink or
    /// at a block, or a rebase past unread chain); 0: the start block's score was not known yet. Orders created before it are not
    /// in this store. Absent for a store that read the node's whole retained history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_from_daa: Option<u64>,
    /// While `catching_up`: how fast the store closes in on the node and when it will have (absent once it follows, and until two
    /// samples a second apart are known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<CatchUp>,
    /// Lean storage ([`super::lean`]): the store keeps the live book and `lean_retention_days` of closed orders, not the history.
    pub lean: bool,
    /// Closed orders and rows deleted by the lean pruning since the start.
    pub lean_orders_pruned_total: u64,
    pub lean_rows_pruned_total: u64,
    /// The fast start's backfill: the full store's progress (absent once the book is complete).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backfill: Option<Backfill>,
    /// Stretches of chain this store skipped on purpose since the start, oldest first ([`SkippedGap`]): the node pruned the
    /// history the store was following, so it continued from the node's pruning point. Empty (and absent here) when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<SkippedGap>,
}

/// The catch-up of a store that is behind the node (`HealthSnapshot::catch_up`), measured over the last minute.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CatchUp {
    /// How fast the history is indexed: DAA of chain per second of wall time (the chain itself moves at about 10).
    pub rate_daa_per_sec: f64,
    /// Seconds until the store has caught up at that pace (absent while the lag does not shrink).
    pub eta_seconds: Option<u64>,
}

/// A stretch of chain the store skipped on purpose ([`super::recovery`]): the node pruned it before the store had read it.
/// The orders the store already had were reconciled with the node's UTXO set (closed with reason `gap`, or adopted); an order
/// created inside the stretch and still open is not in the store until its maker imports it (docs/ops/executor.md, 7.4/7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedGap {
    /// DAA score of the cursor the store stood at, and of the node's pruning point it continued from.
    pub from_daa: u64,
    pub to_daa: u64,
    /// The block the store continued from.
    pub to_hash: Hash32,
    /// Unix time of the move.
    pub at_unix: u64,
    /// What the reconciliation did to the tracked orders: outputs closed with reason `gap`, continuations adopted.
    pub orders_closed: u64,
    pub orders_adopted: u64,
}

impl SkippedGap {
    /// The skipped chain in DAA.
    pub fn daa(&self) -> u64 {
        self.to_daa.saturating_sub(self.from_daa)
    }
}

/// The backfill of a fast start: where the full store is on its way from the node's pruning point to the tip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Backfill {
    /// The full store's follower state.
    pub state: FollowerState,
    /// DAA score of the full store's cursor.
    pub cursor_daa: u64,
    /// DAA the full store is behind the node (`None`: the node's position is not known yet).
    pub lag_daa: Option<u64>,
    pub lag_seconds: Option<u64>,
    /// How fast the history is indexed: DAA of chain per second of wall time (the chain itself moves at `daa_per_second`).
    pub rate_daa_per_sec: Option<f64>,
    /// Seconds until the full store has caught up at the recent pace (`None`: not known yet, or it is not closing in).
    pub eta_seconds: Option<u64>,
    /// Unix time the fast store started following (the orders it knows were seen since).
    pub tip_since_unix: u64,
    /// Stretches of chain the full store skipped on purpose ([`SkippedGap`]: the node pruned the history it was following).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<SkippedGap>,
}

impl Backfill {
    /// The part of the status line about the fast start: what the executor trades on and when the full book is there.
    pub fn status_text(&self) -> String {
        let mut s = format!("fast start: trading on orders seen since {}", utc_clock(self.tip_since_unix));
        if !self.gaps.is_empty() {
            let skipped: u64 = self.gaps.iter().map(SkippedGap::daa).sum();
            s.push_str(&format!(
                " (the node pruned {} of the history before the full store read it: orders created in it are missing from the full book)",
                human_secs(skipped / 10)
            ));
        }
        match (self.eta_seconds, self.lag_seconds) {
            (Some(eta), Some(lag)) => {
                s.push_str(&format!("; full book in about {} ({} of history to index)", human_secs(eta), human_secs(lag)))
            }
            (None, Some(lag)) => s.push_str(&format!("; full book: estimating ({} of history to index)", human_secs(lag))),
            _ => s.push_str("; full book: estimating"),
        }
        s
    }
}

/// `14:32 UTC` of a unix time.
pub fn utc_clock(unix: u64) -> String {
    format!("{:02}:{:02} UTC", unix % 86_400 / 3600, unix % 3600 / 60)
}

/// Estimates how fast the full store of a fast start closes in on the node from its lag over the last minute.
#[derive(Debug, Default)]
pub struct BackfillPace {
    /// (seconds since the first sample, lag in DAA, cursor DAA), oldest first.
    samples: std::collections::VecDeque<(f64, u64, u64)>,
}

impl BackfillPace {
    /// How long the pace looks back.
    const SPAN_SECS: f64 = 60.0;

    /// Records a sample taken `at` seconds from some fixed start; returns `(index rate in DAA per second, seconds to catch
    /// up)` once two samples at least a second apart are known (the second is `None` while the lag does not shrink).
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
            book_complete: true,
            history_from_daa: None,
            catch_up: None,
            lean: false,
            lean_orders_pruned_total: 0,
            lean_rows_pruned_total: 0,
            backfill: None,
            gaps: vec![],
        }
    }

    /// VSPC bytes on the wire per accepted transaction since the start (the bandwidth an indexer needs is
    /// this times the chain's transaction rate, see docs/ops/executor.md).
    pub fn bytes_per_tx(&self) -> Option<u64> {
        (self.txs_fetched_total > 0).then(|| self.wire_bytes_total / self.txs_fetched_total)
    }

    /// Prometheus text exposition of the follower's state (`GET /v1/metrics`, and the executor's metrics file).
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
        }
        s
    }

    /// Lag behind the node in DAA score (about one block per DAA at 10 BPS is one tenth of a second).
    pub fn lag_daa(&self) -> Option<u64> {
        self.node_daa.map(|n| n.saturating_sub(self.cursor_daa))
    }

    /// The store is caught up with the node: the follower's last poll applied the chain to within its catch-up distance of
    /// the node's sink (`following`). Until then (`starting` after a restart, `catching_up`, node unavailable, gap) the
    /// store lags the chain and an order or token it lists may already be spent: the matcher, the keepers and the
    /// maintenance jobs plan nothing (`GET /v1/health` reports it as `caught_up`).
    pub fn caught_up(&self) -> bool {
        self.state == FollowerState::Following && self.node_daa.is_some()
    }

    /// The store may be planned against although it is not caught up: the follower is applying the chain (`following` or
    /// `catching_up`) and the node's sink is at most `lag_tolerance_daa` ahead of its cursor. A stale view is harmless, the
    /// covenants enforce every order's terms and a spent input only loses the race (`MissingInput`); what the tolerance bounds
    /// is how many such lost transactions a lagging store can cause. 0 (or a state other than the two above: `starting`, node
    /// unavailable, gap, stopped) never qualifies. A caught-up store (`caught_up`) is always within.
    pub fn within_lag_tolerance(&self) -> bool {
        if self.caught_up() {
            return true;
        }
        self.lag_tolerance_daa > 0
            && matches!(self.state, FollowerState::Following | FollowerState::CatchingUp)
            && self.lag_daa().is_some_and(|l| l <= self.lag_tolerance_daa)
    }

    /// Why the matcher, keepers and maintenance must not plan against the store (`None`: they may): it is neither caught up
    /// nor within the lag tolerance.
    pub fn not_ready(&self) -> Option<String> {
        if self.within_lag_tolerance() {
            None
        } else {
            self.not_caught_up()
        }
    }

    /// One line for an operator watching the console (`--status-secs`): the follower's state and lag, in time as well as
    /// DAA (10 per second), the orders indexed, and the last error while the node is unavailable or the chain has a gap.
    pub fn status_line(&self) -> String {
        let state = match self.state {
            FollowerState::Starting => "starting",
            FollowerState::CatchingUp => "catching up",
            FollowerState::Following => "following",
            FollowerState::NodeUnavailable => "waiting for the node",
            FollowerState::Gap => "STOPPED at a gap (docs/ops/executor.md, Part B 7.4)",
            FollowerState::Stopped => "stopped",
        };
        let lag = match self.lag_daa() {
            Some(l) if self.caught_up() => format!("caught up ({l} DAA behind the node)"),
            Some(l) => format!("{l} DAA behind the node (about {})", human_secs(l / 10)),
            None => "node position unknown".to_string(),
        };
        // catching up: the pace and the time to go (measured over the last minute), so a long first sync is not a guess
        let lag = match (&self.catch_up, self.state) {
            (Some(c), FollowerState::CatchingUp) => {
                // the chain itself moves at 10 DAA per second
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
        // several nodes (a resolver pair: the hosts in use): which is the authority, and how each is doing
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
                    }
                    if n.disagreements > 0 {
                        x.push_str(&format!(", {} disagreements", n.disagreements));
                    }
                    x + ")"
                })
                .collect();
            s.push_str(&format!(" | nodes: {}", nodes.join(", ")));
        }
        if matches!(self.state, FollowerState::NodeUnavailable | FollowerState::Gap) {
            if let Some(e) = self.gap_reason.as_ref().or(self.last_error.as_ref()) {
                s.push_str(&format!(" ({e})"));
            }
        }
        if let Some(from) = self.history_from_daa {
            let ago = self.node_daa.filter(|_| from > 0).map(|n| format!("about {} ago", human_secs(n.saturating_sub(from) / 10)));
            s.push_str(&format!(
                " | history begins {}: older orders are not in this store (book_complete false; a start at the node's pruning point reads all it retains)",
                ago.unwrap_or_else(|| "at the start point".to_string())
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
        s
    }

    /// Why the store is not caught up (`None`: it is).
    pub fn not_caught_up(&self) -> Option<String> {
        if self.caught_up() {
            return None;
        }
        let state = serde_json::to_value(self.state).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        Some(match self.lag_daa() {
            Some(lag) => format!("indexer {state}, {lag} DAA behind the node"),
            None => format!("indexer {state}, node position unknown"),
        })
    }
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

/// Shared handle: the follower writes, the API reads.
pub struct HealthState(RwLock<HealthSnapshot>);

impl HealthState {
    pub fn new(network: &str) -> Self {
        HealthState(RwLock::new(HealthSnapshot::new(network)))
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut HealthSnapshot)) {
        let mut g = self.0.write().unwrap_or_else(|e| e.into_inner());
        f(&mut g);
    }
}

/// A 64-bit quantity as a decimal string, as the REST API spells them: a JSON number loses precision above 2^53 in JavaScript
/// (a supply can reach 2.9e18 base units).
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

/// A committed fill, as pushed to WebSocket subscribers. `price`, `amount` and `payout` are decimal strings, like every
/// quantity of the REST API (docs/ops/executor.md, 5.0); `side` and `daa` are numbers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FillNotice {
    pub order: Hash32,
    pub token: Option<Hash32>,
    /// 1: an ask (sell) was filled, 2: a bid (buy) was filled.
    pub side: u8,
    /// The order's price, sompi per whole token (`null` for a pair order and the conditional kinds).
    #[serde(serialize_with = "dec_str::opt")]
    pub price: Option<i64>,
    /// Base units filled.
    #[serde(serialize_with = "dec_str::int")]
    pub amount: i64,
    /// What the maker was paid, sompi (`null` when it is not a payment to the maker).
    #[serde(serialize_with = "dec_str::opt")]
    pub payout: Option<i64>,
    pub txid: Hash32,
    pub block: Hash32,
    pub daa: u64,
}

/// What one committed batch changed. Published on a broadcast channel after the commit.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexEvent {
    pub cursor_hash: Option<Hash32>,
    pub cursor_daa: u64,
    pub reverted_blocks: u64,
    pub added_blocks: u64,
    /// Covenant ids of orders whose state changed (created, filled, closed, or reverted).
    pub orders: Vec<Hash32>,
    /// Token covenant ids whose books may have changed.
    pub tokens: Vec<Hash32>,
    pub fills: Vec<FillNotice>,
}

impl IndexEvent {
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
        // the pace of the last minute: how fast the store closes in on the node, and when it will have
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
        // a resolver pair: both hosts, their roles and states, replacements and disagreements
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

    /// A store that began after the oldest history is not a complete book, for good: the health says where its history begins and the
    /// status line says that older orders are not in it.
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
        assert!(h.status_line().contains("history begins about 60 min ago"), "{}", h.status_line());
        assert!(h.status_line().contains("older orders are not in this store"), "{}", h.status_line());
        // the start block's score was not known: the line still says it
        h.history_from_daa = Some(0);
        assert!(h.status_line().contains("history begins at the start point"), "{}", h.status_line());
        assert_eq!(serde_json::to_value(&h).unwrap()["history_from_daa"], 0);
    }

    #[test]
    fn the_backfill_pace_estimates_when_the_full_book_is_there() {
        let mut p = BackfillPace::default();
        // one sample: nothing to compare with
        assert_eq!(p.push(0.0, 360_000, 1_000), (None, None));
        // the cursor moves 100 DAA per second while the chain moves 10: the lag shrinks by 90 per second
        let (rate, eta) = p.push(10.0, 360_000 - 900, 2_000);
        assert_eq!(rate, Some(100.0));
        assert_eq!(eta, Some(3_990));
        // a store that does not close in has no estimate
        let mut q = BackfillPace::default();
        q.push(0.0, 1_000, 5_000);
        assert_eq!(q.push(10.0, 1_100, 5_100), (Some(10.0), None));
        // only the last minute counts
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
                "fast start: trading on orders seen since {}; full book in about 7 h 40 min (29 h 43 min of history to index)",
                utc_clock(1_790_000_000 + 14 * 3600 + 32 * 60)
            )
        );
        assert!(b(None, Some(107_000)).status_text().ends_with("full book: estimating (29 h 43 min of history to index)"));
        assert!(b(None, None).status_text().ends_with("; full book: estimating"));
        // the health JSON a client reads
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

    /// The lag tolerance (`max_lag_secs`): `caught_up` stays strict, `within_lag_tolerance` / `not_ready` follow the bound.
    #[test]
    fn within_lag_tolerance_follows_the_bound_and_never_overrides_caught_up() {
        let mut h = HealthSnapshot::new("testnet-10");
        h.cursor_daa = 1_000;
        h.lag_tolerance_daa = 300;
        // no node position yet: not within, and a reason
        assert!(!h.within_lag_tolerance());
        assert_eq!(h.not_ready().as_deref(), Some("indexer starting, node position unknown"));
        h.node_daa = Some(1_300);
        // starting / unavailable / gap / stopped never qualify, however small the lag
        for s in [FollowerState::Starting, FollowerState::NodeUnavailable, FollowerState::Gap, FollowerState::Stopped] {
            h.state = s;
            assert!(!h.within_lag_tolerance() && h.not_ready().is_some(), "{s:?}");
        }
        // catching up (or following above the follower's own 100 DAA) within the tolerance: ready, but not caught up
        h.state = FollowerState::CatchingUp;
        assert!(h.within_lag_tolerance() && h.not_ready().is_none());
        assert!(!h.caught_up(), "caught_up stays strict");
        h.node_daa = Some(1_301);
        assert!(!h.within_lag_tolerance());
        assert_eq!(h.not_ready().as_deref(), Some("indexer catching_up, 301 DAA behind the node"));
        // the strict gate (tolerance 0): only `following`
        h.lag_tolerance_daa = 0;
        h.node_daa = Some(1_001);
        assert!(!h.within_lag_tolerance() && h.not_ready().is_some());
        h.state = FollowerState::Following;
        assert!(h.within_lag_tolerance() && h.not_ready().is_none());
        // the metrics carry both
        let m = h.metrics_text();
        assert!(
            m.contains("kob_indexer_caught_up 1")
                && m.contains("kob_indexer_within_lag_tolerance 1")
                && m.contains("kob_indexer_lag_tolerance_daa 0"),
            "{m}"
        );
    }
}
