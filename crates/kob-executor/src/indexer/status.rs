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

/// A committed fill, as pushed to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FillNotice {
    pub order: Hash32,
    pub token: Option<Hash32>,
    /// 1: an ask (sell) was filled, 2: a bid (buy) was filled.
    pub side: u8,
    /// The order's price, sompi per whole token (`null` for a pair order and the conditional kinds).
    pub price: Option<i64>,
    /// Base units filled.
    pub amount: i64,
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
