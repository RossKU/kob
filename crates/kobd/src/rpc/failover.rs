//! The Indexer's node failover, enabled by `[failover] fallback = "resolver"`.
//!
//! The configured nodes serve first. When they stop answering, the public Kaspa nodes serve as a
//! cross-checked pair (`rpc::resolver`, `rpc::multi`). The configured nodes serve again once they
//! are healthy.
//!
//! # Tiers
//!
//! Two node lists, the tiers, sit behind one [`ChainSource`]. Tier 0 is the configured primary with
//! its `nodes`. Tier 1 is the fallback. Every call goes to the active tier. The other tier opens no
//! connection except for the checks below.
//!
//! # Away from the primary
//!
//! A call counts as failed when the configured primary fails it on the wire. That means a transport
//! error, a timeout, an undecodable answer or a node in initial block download. A node error such
//! as an unknown hash is an answer and not a failure. Any answer resets the count.
//!
//! The fallback is checked after `after_failures` failed calls in a row and `after` without any
//! answer. The check wants the right network, a synced node and a node that knows the cursor. When
//! the fallback passes, it becomes the chain source. Its answers reset the count, so a slow primary
//! whose small calls still answer is not switched away from this way. Its failing chain requests
//! can still switch it, as the next paragraph says.
//!
//! The primary can fail its chain requests while its small calls answer. The follower then reports
//! a failing authority (`ChainSource::authority_failing`). The active tier first hands the chain
//! authority to another of its own nodes (`rpc::multi`). When the configured nodes cannot do that,
//! the fallback is checked as above and becomes the chain source if it passes.
//!
//! # Back to the primary
//!
//! While the fallback serves, a background probe of the primary runs every `probe_every`. The probe
//! wants the right network, a synced node and a node that knows the cursor. Its sink may be at most
//! `max_behind_blue` behind the sink of the fallback. After `return_after_probes` healthy probes in
//! a row the primary is the chain source again.
//!
//! # Settling
//!
//! A tier is settling right after a switch. It never takes a divergent chain view silently.
//!
//! - Its first answer from the cursor may remove at most `max_reorg_blocks` chain blocks. A longer
//!   removal may be a reorg the old node had not seen yet.
//! - A cursor the tier does not know is not walked back over. `indexer::follower` would rewind the
//!   store to the newest block the tier knows.
//!
//! A refused answer is logged as an error and counted as `refused` in `/v1/health`. Nothing is
//! applied. A refused return to the primary goes back to the fallback at once. The primary is
//! probed again only after ten probe intervals.
//!
//! The tier settles with its first answer that continues the cursor. An answer of a tier that
//! stopped being the chain source while it travelled is not applied, and the follower asks again.
//! After the tier settles, the follower's own rules apply as for any node.
//!
//! - A `removed` list that contradicts the stored chain stops the indexer (`gap`).
//! - The fallback pair applies only what both of its public nodes report (`rpc::multi`).
//!
//! Every switch and every refusal is logged with its reason. `/v1/health` reports the state under
//! `node_source`.

use super::multi::NodeStatus;
use super::types::{AddressUtxo, ChainHashes, ChainIds, DagInfo, RawVspcResponse, ServerInfo, VspcRequest};
use super::{ChainSource, Fetched, NodeErrorKind, Origin, RpcError, WindowRequest};
use crate::hex::Hash32;
use serde::Serialize;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// When to switch and when to come back ([`crate::config::FailoverConfig`]).
#[derive(Debug, Clone)]
pub struct FailoverPolicy {
    pub after_failures: u32,
    pub after: Duration,
    pub probe_every: Duration,
    pub return_after_probes: u32,
    pub max_reorg_blocks: usize,
    pub max_behind_blue: u64,
    /// How long one check of a tier may take. A check is the fallback before a switch or a probe of
    /// the primary.
    pub check_timeout: Duration,
}

impl FailoverPolicy {
    pub fn from_config(c: &crate::config::FailoverConfig) -> Self {
        FailoverPolicy {
            after_failures: c.after_failures.max(1),
            after: Duration::from_secs(c.after_secs),
            probe_every: Duration::from_secs(c.probe_secs.max(1)),
            return_after_probes: c.return_after_probes.max(1),
            max_reorg_blocks: c.max_reorg_blocks,
            max_behind_blue: c.max_behind_blue,
            check_timeout: Duration::from_secs(20),
        }
    }
}

/// What `/v1/health` reports under `node_source`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct FailoverStatus {
    /// `primary` (the configured nodes) or `fallback`.
    pub active: String,
    /// The configured primary (scheme, host and port only) and the fallback (`resolver`).
    pub primary: String,
    pub fallback: String,
    pub switches: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_switch_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_switch_reason: Option<String>,
    /// Whether the serving tier has yet to show that its chain continues the cursor. True right
    /// after a switch.
    pub settling: bool,
    /// Calls the active tier failed in a row.
    pub consecutive_failures: u32,
    /// On the fallback: healthy probes of the primary in a row.
    pub primary_probes_ok: u32,
    /// Answers refused right after a switch (a deep reorg, an unknown cursor), and the last reason.
    pub refused: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refusal: Option<String>,
}

struct State {
    active: usize,
    settling: bool,
    failures: u32,
    last_ok: Instant,
    /// No check of the fallback (on the primary) or probe of the primary (on the fallback) before
    /// this time.
    next_check: Instant,
    probes_ok: u32,
    /// The start of the last applied answer from the cursor. It is the cursor a tier must know.
    last_start: Option<Hash32>,
    switches: u64,
    last_switch_unix_ms: Option<u64>,
    last_switch_reason: Option<String>,
    refused: u64,
    last_refusal: Option<String>,
}

const NAMES: [&str; 2] = ["primary", "fallback"];

struct Inner<S> {
    tiers: [Arc<S>; 2],
    /// How the tiers are shown (the primary's URL redacted, `resolver`).
    shown: [String; 2],
    network: String,
    policy: FailoverPolicy,
    st: Mutex<State>,
}

/// The configured nodes and a fallback as one [`ChainSource`] (see the module documentation).
pub struct Failover<S: ChainSource> {
    inner: Arc<Inner<S>>,
}

/// A failed call that says the node is gone (not an answer such as an unknown hash).
pub(crate) fn node_failure(e: &RpcError) -> bool {
    matches!(e, RpcError::Transport(_) | RpcError::Timeout(_) | RpcError::Decode(_)) || e.kind() == NodeErrorKind::TransitionalIbd
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl<S: ChainSource> Failover<S> {
    /// `primary` is the configured nodes, shown as `primary_shown`. `fallback` is the fallback,
    /// shown as `fallback_shown`. `network` is the network both must report.
    pub fn new(
        primary: Arc<S>,
        fallback: Arc<S>,
        primary_shown: &str,
        fallback_shown: &str,
        network: &str,
        policy: FailoverPolicy,
    ) -> Self {
        let now = Instant::now();
        let st = State {
            active: 0,
            settling: false,
            failures: 0,
            last_ok: now,
            next_check: now,
            probes_ok: 0,
            last_start: None,
            switches: 0,
            last_switch_unix_ms: None,
            last_switch_reason: None,
            refused: 0,
            last_refusal: None,
        };
        let inner = Inner {
            tiers: [primary, fallback],
            shown: [super::redact_url(primary_shown), super::redact_url(fallback_shown)],
            network: network.to_string(),
            policy,
            st: Mutex::new(st),
        };
        Failover { inner: Arc::new(inner) }
    }

    /// The tier serving now (0: the configured nodes, 1: the fallback).
    pub fn active(&self) -> usize {
        self.inner.with(|s| s.active)
    }

    pub fn status(&self) -> FailoverStatus {
        self.inner.status()
    }
}

impl<S: ChainSource> Inner<S> {
    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.st.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn status(&self) -> FailoverStatus {
        self.with(|s| FailoverStatus {
            active: NAMES[s.active].into(),
            primary: self.shown[0].clone(),
            fallback: self.shown[1].clone(),
            switches: s.switches,
            last_switch_unix_ms: s.last_switch_unix_ms,
            last_switch_reason: s.last_switch_reason.clone(),
            settling: s.settling,
            consecutive_failures: s.failures,
            primary_probes_ok: s.probes_ok,
            refused: s.refused,
            last_refusal: s.last_refusal.clone(),
        })
    }

    /// Make tier `to` the chain source (settling: its first answers are checked).
    fn switch(&self, to: usize, reason: &str) {
        let from = self.with(|s| {
            if s.active == to {
                return None;
            }
            let from = s.active;
            let now = Instant::now();
            s.active = to;
            s.settling = true;
            s.failures = 0;
            s.last_ok = now;
            s.probes_ok = 0;
            s.next_check = now + self.policy.probe_every;
            s.switches += 1;
            s.last_switch_unix_ms = Some(unix_ms());
            s.last_switch_reason = Some(reason.to_string());
            Some(from)
        });
        if let Some(from) = from {
            tracing::warn!(
                from = %format!("{} ({})", NAMES[from], self.shown[from]),
                to = %format!("{} ({})", NAMES[to], self.shown[to]),
                reason,
                "node failover: the chain source changes"
            );
        }
    }

    /// Whether tier `t` can serve. It needs the right network, a synced node and a node that knows
    /// the cursor. With `against`, its sink may be at most `max_behind_blue` behind the sink of
    /// that tier.
    async fn healthy(&self, t: usize, against: Option<usize>) -> Result<(), String> {
        let node = self.tiers[t].clone();
        let cursor = self.with(|s| s.last_start);
        let check = async {
            let info = node.server_info().await.map_err(|e| format!("getServerInfo: {e}"))?;
            if info.network_id != self.network {
                return Err(format!("it is on {}, not {}", info.network_id, self.network));
            }
            if !info.is_synced {
                return Err("it is not synced".into());
            }
            if let Some(h) = cursor {
                if !node.block_exists(h).await.map_err(|e| format!("getBlock: {e}"))? {
                    return Err(format!("it does not know the cursor {h}"));
                }
            }
            if let Some(o) = against {
                let mine = node.sink_blue_score().await.map_err(|e| format!("getSinkBlueScore: {e}"))?;
                let theirs = self.tiers[o].sink_blue_score().await.map_err(|e| format!("the {}'s sink: {e}", NAMES[o]))?;
                if mine.saturating_add(self.policy.max_behind_blue) < theirs {
                    return Err(format!("its sink is {} blue score behind the {}'s", theirs - mine, NAMES[o]));
                }
            }
            Ok(())
        };
        match tokio::time::timeout(self.policy.check_timeout, check).await {
            Ok(r) => r,
            Err(_) => Err(format!("no answer within {:?}", self.policy.check_timeout)),
        }
    }

    /// The primary failed: switch to the fallback if it can serve.
    async fn try_fallback(self: Arc<Self>) {
        let why = self.with(|s| format!("{} failed calls in a row, no answer for {} s", s.failures, s.last_ok.elapsed().as_secs()));
        match self.healthy(1, None).await {
            Ok(()) => self.switch(1, &format!("the configured node stopped answering ({why})")),
            Err(e) => tracing::warn!(
                primary = %self.shown[0],
                fallback = %self.shown[1],
                "node failover: the configured node stopped answering ({why}), but the fallback cannot serve ({e}); staying"
            ),
        }
    }

    /// On the fallback: one probe of the primary; enough healthy ones in a row bring it back.
    async fn probe_primary(self: Arc<Self>) {
        match self.healthy(0, Some(1)).await {
            Ok(()) => {
                let n = self.with(|s| {
                    s.probes_ok += 1;
                    s.probes_ok
                });
                if n >= self.policy.return_after_probes {
                    self.switch(0, &format!("the configured node is healthy again ({n} probes in a row)"));
                }
            }
            Err(e) => {
                self.with(|s| s.probes_ok = 0);
                tracing::debug!(primary = %self.shown[0], "node failover: the configured node is not usable yet: {e}");
            }
        }
    }

    /// Before a call: on the fallback, a probe of the primary when one is due (in the background).
    fn maintain(self: &Arc<Self>) {
        let due = self.with(|s| {
            let now = Instant::now();
            let due = s.active == 1 && now >= s.next_check;
            if due {
                s.next_check = now + self.policy.probe_every;
            }
            due
        });
        if due {
            tokio::spawn(self.clone().probe_primary());
        }
    }

    /// After a call of tier `t`, counts a failure or an answer. Enough failures of the primary
    /// check the fallback in the background.
    fn observe<T>(self: &Arc<Self>, t: usize, r: &Result<T, RpcError>) {
        let p = &self.policy;
        let check = self.with(|s| {
            if s.active != t {
                return false;
            }
            let now = Instant::now();
            match r {
                Err(e) if node_failure(e) => {
                    s.failures = s.failures.saturating_add(1);
                    let due = t == 0 && s.failures >= p.after_failures && s.last_ok.elapsed() >= p.after && now >= s.next_check;
                    if due {
                        s.next_check = now + p.probe_every;
                    }
                    due
                }
                _ => {
                    s.failures = 0;
                    s.last_ok = now;
                    false
                }
            }
        });
        if check {
            tokio::spawn(self.clone().try_fallback());
        }
    }

    /// One call on the active tier: `(the tier, its result)`.
    async fn call<T, F, Fut>(self: &Arc<Self>, f: F) -> (usize, Result<T, RpcError>)
    where
        F: FnOnce(Arc<S>) -> Fut,
        Fut: Future<Output = Result<T, RpcError>>,
    {
        self.maintain();
        let t = self.with(|s| s.active);
        let r = f(self.tiers[t].clone()).await;
        self.observe(t, &r);
        (t, r)
    }

    /// An answer of tier `t` from `start` (the cursor) was refused. It is counted and logged. A
    /// refused return to the primary goes back to the fallback. The primary is probed again only
    /// after ten probe intervals.
    fn refuse(&self, t: usize, why: String) -> RpcError {
        self.with(|s| {
            s.refused += 1;
            s.last_refusal = Some(why.clone());
        });
        tracing::error!(source = %format!("{} ({})", NAMES[t], self.shown[t]), reason = %why, "node failover: the new chain source's answer is refused, nothing is applied");
        if t == 0 {
            self.switch(1, &format!("the configured node's chain was refused: {why}"));
            self.with(|s| s.next_check = Instant::now() + self.policy.probe_every.saturating_mul(10));
        }
        RpcError::Node(format!("node failover: {why}; not applied"))
    }

    /// The follower's step from `start` answered by tier `t` (module documentation: the settling checks).
    fn vouch(&self, t: usize, start: Hash32, r: Result<RawVspcResponse, RpcError>) -> Result<RawVspcResponse, RpcError> {
        let (active, settling) = self.with(|s| (s.active, s.settling));
        if active != t {
            if r.is_ok() {
                return Err(RpcError::Node(format!(
                    "node failover: the chain source changed to the {} while the request travelled; asking again",
                    NAMES[active]
                )));
            }
            return r;
        }
        let max = self.policy.max_reorg_blocks;
        match r {
            Ok(raw) if settling && raw.removed_chain_block_hashes.len() > max => Err(self.refuse(
                t,
                format!(
                    "the {} removes {} chain blocks at the cursor {start} (at most {max} right after a switch)",
                    NAMES[t],
                    raw.removed_chain_block_hashes.len()
                ),
            )),
            Err(e) if settling && e.kind() == NodeErrorKind::UnknownHash => {
                Err(self.refuse(t, format!("the {} does not know the cursor {start} (it would rewind the store)", NAMES[t])))
            }
            Ok(raw) => {
                self.settled(t, start);
                Ok(raw)
            }
            Err(e) => Err(e),
        }
    }

    /// Tier `t` answered from `start` in a way that continues it. The tier is settled, and `start`
    /// is the cursor to check from now on.
    fn settled(&self, t: usize, start: Hash32) {
        let was = self.with(|s| {
            if s.active != t {
                return false;
            }
            s.last_start = Some(start);
            std::mem::replace(&mut s.settling, false)
        });
        if was {
            tracing::info!(source = %format!("{} ({})", NAMES[t], self.shown[t]), cursor = %start, "node failover: the new chain source continues the cursor");
        }
    }
}

impl<S: ChainSource> ChainSource for Failover<S> {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        let start = req.start_hash;
        let (t, r) = self.inner.call(move |s| async move { s.vspc_v2(req).await }).await;
        self.inner.vouch(t, start, r)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.inner.call(|s| async move { s.dag_info().await }).await.1
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.inner.call(|s| async move { s.server_info().await }).await.1
    }
    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        let a = addresses.to_vec();
        self.inner.call(move |s| async move { s.utxos_by_addresses(&a).await }).await.1
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.inner.call(move |s| async move { s.block_exists(hash).await }).await.1
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.inner.call(|s| async move { s.sink_blue_score().await }).await.1
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.inner.call(move |s| async move { s.block_blue_score(hash).await }).await.1
    }
    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.inner.call(move |s| async move { s.block_daa_score(hash).await }).await.1
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        let (t, r) = self.inner.call(move |s| async move { s.chain_hashes(start).await }).await;
        if let Ok(c) = &r {
            if c.removed_chain_block_hashes.is_empty() {
                // the tier's selected chain runs through the cursor
                self.inner.settled(t, start);
            }
        }
        r
    }
    async fn chain_with_ids(&self, start: Hash32, min_confirmations: Option<u64>) -> Result<ChainIds, RpcError> {
        self.inner.call(move |s| async move { s.chain_with_ids(start, min_confirmations).await }).await.1
    }
    async fn fetch_window(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        let (t, r) = self.inner.call(move |s| async move { s.fetch_window(w).await }).await;
        r.map(|f| Fetched { origin: Origin { tier: t as u8, ..f.origin }, ..f })
    }
    async fn fetch_window_primary(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        let (t, r) = self.inner.call(move |s| async move { s.fetch_window_primary(w).await }).await;
        r.map(|f| Fetched { origin: Origin { tier: t as u8, ..f.origin }, ..f })
    }
    fn report_lie(&self, origin: Origin, why: &str) {
        let t = (origin.tier as usize).min(1);
        self.inner.tiers[t].report_lie(Origin { tier: 0, ..origin }, why);
    }
    /// Hands the authority over within the active tier first (`rpc::multi`). When the configured
    /// nodes cannot, the fallback is checked. If it can serve, it becomes the chain source and
    /// settles as after any switch.
    async fn authority_failing(&self, at: Hash32, why: &str) -> bool {
        let t = self.active();
        if self.inner.tiers[t].authority_failing(at, why).await {
            return true;
        }
        if t != 0 {
            return false;
        }
        match self.inner.healthy(1, None).await {
            Ok(()) => {
                self.inner.switch(1, &format!("the configured node's chain requests keep failing ({why})"));
                self.active() == 1
            }
            Err(e) => {
                tracing::warn!(primary = %self.inner.shown[0], fallback = %self.inner.shown[1], "node failover: the configured node's chain requests keep failing, but the fallback cannot serve ({e}); staying");
                false
            }
        }
    }
    async fn confirm_unknown_cursor(&self, at: Hash32, e: &RpcError) -> Result<NodeErrorKind, String> {
        self.inner.tiers[self.active()].confirm_unknown_cursor(at, e).await
    }
    fn node_stats(&self) -> Vec<NodeStatus> {
        self.inner.tiers[self.active()].node_stats()
    }
    fn source_status(&self) -> Option<FailoverStatus> {
        Some(self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wire_failures_count_against_a_node() {
        assert!(node_failure(&RpcError::Transport("connection closed".into())));
        assert!(node_failure(&RpcError::Timeout(Duration::from_secs(1))));
        assert!(node_failure(&RpcError::Decode("garbage".into())));
        assert!(node_failure(&RpcError::Node("consensus is currently in a transitional ibd state".into())));
        // an answer: the node is there
        assert!(!node_failure(&RpcError::Node("cannot find header 00".into())));
        assert!(!node_failure(&RpcError::TooLarge { size: 2, limit: 1 }));
    }

    #[test]
    fn the_policy_follows_the_config_and_never_switches_on_zero() {
        let c = crate::config::FailoverConfig { fallback: "resolver".into(), after_failures: 0, ..Default::default() };
        let p = FailoverPolicy::from_config(&c);
        assert_eq!(
            (p.after_failures, p.after, p.probe_every, p.return_after_probes),
            (1, Duration::from_secs(60), Duration::from_secs(30), 3)
        );
        assert_eq!((p.max_reorg_blocks, p.max_behind_blue), (100, 600));
        assert!(c.enabled() && !crate::config::FailoverConfig::default().enabled());
    }
}
