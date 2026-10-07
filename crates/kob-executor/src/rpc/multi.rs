//! Several nodes behind one [`ChainSource`]: a primary that is the authority for the selected chain, and further nodes that
//! serve transaction bodies (checked against the primary) and take submissions (`matcher::node::MultiSubmit`).
//!
//! The primary answers every call the follower makes itself: the cursor's position, the chain hashes the windows are planned
//! on, the single steps near the sink and every reorg decision. Only the windows of the parallel fetch
//! (`indexer::prefetch`), the bulk of the bytes, are spread over the nodes:
//!
//! * a window goes to the node with a free connection that delivered the most bytes per second lately (a node not measured
//!   yet is tried first), the primary included;
//! * a window from another node is asked for at `Full` verbosity and checked before the follower sees it
//!   ([`super::verify`]): its chain blocks are the primary's, every header hashes to its block hash, every chain block
//!   accepted exactly the transactions the primary reports (the primary's ids, fetched at the same time), and every
//!   transaction hashes to its id. The follower takes the parts no hash covers for every transaction that can matter to
//!   KOB from the primary before applying the window (`indexer::trust`);
//! * a node that fails a window (transport, timeout, it does not know the window, another chain than the primary) is
//!   skipped for a while (doubling backoff, at most two minutes) and the window goes to the next node, the primary last;
//! * a node that contradicts a hash or the primary's acceptance data is dropped until the process restarts (`lies`).
//!
//! `GET /v1/health` reports each node under `nodes`.

use super::types::{AddressUtxo, ChainHashes, ChainIds, DagInfo, RawVspcResponse, ServerInfo, Verbosity, VspcRequest};
use super::verify::{verify_window, Rejection};
use super::{fetch_window_from, window_min_confirmations, ChainSource, Fetched, Origin, RpcError, WindowRequest};
use crate::hex::Hash32;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// One node of the list.
#[derive(Debug, Clone)]
pub struct NodeSpec {
    pub url: String,
    /// Serves windows of the parallel fetch.
    pub fetch: bool,
    /// Takes the executor's transactions.
    pub submit: bool,
    /// Windows fetched from it at once.
    pub connections: usize,
}

/// What `/v1/health` reports per node.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NodeStatus {
    pub url: String,
    /// `primary` (the chain authority) or `secondary`.
    pub role: String,
    /// `ok`, `backoff` (failed lately: skipped until `backoff_secs`), `dropped` (contradicted the primary) or `idle` (neither
    /// fetches nor takes submissions).
    pub state: String,
    pub fetch: bool,
    pub submit: bool,
    pub connections: usize,
    /// Windows fetched now.
    pub in_flight: u64,
    pub windows_ok: u64,
    pub windows_failed: u64,
    pub blocks: u64,
    pub txs: u64,
    pub bytes: u64,
    /// Bytes per second of its recent windows (moving average; `None`: not measured yet).
    pub bytes_per_sec: Option<u64>,
    pub last_fetch_ms: u64,
    pub timeouts: u64,
    /// Windows refused because the node was on another chain than the primary or did not know the window.
    pub out_of_sync: u64,
    /// Contradictions of a hash or of the primary's data (the node is dropped at the first).
    pub lies: u64,
    pub dropped_reason: Option<String>,
    pub backoff_secs: u64,
    pub last_error: Option<String>,
    /// Submissions the node took (accepted, or it already had the transaction from a peer or the chain), and refused.
    pub submits_ok: u64,
    pub submits_failed: u64,
}

#[derive(Default)]
struct PeerState {
    status: NodeStatus,
    ewma_bps: Option<f64>,
    failures: u32,
    backoff_until: Option<Instant>,
    /// When a window last went to it.
    last_used: Option<Instant>,
}

/// A node whose rate is below the best one's divided by this gets windows only when no faster node qualifies: in chain
/// order, a slow window holds up every window behind it.
const SLOW_FACTOR: f64 = 4.0;
/// A node's rate is measured again (its next window counts as unmeasured) when it got no window for this long.
const REMEASURE: Duration = Duration::from_secs(300);

/// Per-node state shared by the chain source and the submitter.
pub struct NodeBoard {
    peers: Vec<Mutex<PeerState>>,
}

/// How a failure counts against a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Transport, timeout, an undecodable answer: back off, doubling.
    Failed,
    /// It does not know the window, or follows another chain right now: a short pause.
    OutOfSync,
    /// It contradicted a hash or the primary: dropped.
    Lie,
}

impl NodeBoard {
    pub fn new(specs: &[NodeSpec]) -> Arc<NodeBoard> {
        let peers = specs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                Mutex::new(PeerState {
                    status: NodeStatus {
                        // shown on the public health and metrics routes and in logs: never credentials or keys
                        url: super::redact_url(&s.url),
                        role: if i == 0 { "primary" } else { "secondary" }.into(),
                        state: "ok".into(),
                        fetch: s.fetch,
                        submit: s.submit,
                        connections: s.connections,
                        ..NodeStatus::default()
                    },
                    ..PeerState::default()
                })
            })
            .collect();
        Arc::new(NodeBoard { peers })
    }

    fn with<T>(&self, i: usize, f: impl FnOnce(&mut PeerState) -> T) -> T {
        f(&mut self.peers[i].lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn url(&self, i: usize) -> String {
        self.with(i, |p| p.status.url.clone())
    }

    /// A submission to node `i` ended.
    pub fn submitted(&self, i: usize, ok: bool, err: Option<String>) {
        self.with(i, |p| {
            if ok {
                p.status.submits_ok += 1;
            } else {
                p.status.submits_failed += 1;
                if err.is_some() {
                    p.status.last_error = err;
                }
            }
        })
    }

    fn begin(&self, i: usize) {
        self.with(i, |p| {
            p.status.in_flight += 1;
            p.last_used = Some(Instant::now());
        })
    }

    fn end(&self, i: usize) {
        self.with(i, |p| p.status.in_flight = p.status.in_flight.saturating_sub(1))
    }

    fn fetched(&self, i: usize, f: &Fetched) {
        let bytes = f.raw.wire_bytes as u64;
        let blocks = f.raw.added_chain_block_hashes.len() as u64;
        let txs: usize = f.raw.chain_block_accepted_transactions.iter().map(|b| b.accepted_transactions.len()).sum();
        let secs = f.elapsed.as_secs_f64().max(1e-3);
        self.with(i, |p| {
            p.failures = 0;
            p.backoff_until = None;
            let s = &mut p.status;
            s.windows_ok += 1;
            s.blocks += blocks;
            s.txs += txs as u64;
            s.bytes += bytes;
            s.last_fetch_ms = f.elapsed.as_millis() as u64;
            // only answers of some size say anything about the link
            if bytes >= 64 * 1024 {
                let bps = bytes as f64 / secs;
                p.ewma_bps = Some(p.ewma_bps.map_or(bps, |o| 0.7 * o + 0.3 * bps));
            }
        })
    }

    fn failed(&self, i: usize, fault: Fault, e: &str) {
        let url = self.url(i);
        self.with(i, |p| {
            let s = &mut p.status;
            s.windows_failed += 1;
            s.last_error = Some(e.to_string());
            match fault {
                Fault::Lie => {
                    s.lies += 1;
                    if i > 0 && s.dropped_reason.is_none() {
                        tracing::error!(node = %url, reason = e, "node contradicts the primary: dropped");
                        s.dropped_reason = Some(e.to_string());
                    }
                }
                Fault::OutOfSync => {
                    s.out_of_sync += 1;
                    p.backoff_until = Some(Instant::now() + Duration::from_secs(10));
                }
                Fault::Failed => {
                    if e.starts_with("timeout") {
                        s.timeouts += 1;
                    }
                    p.failures = p.failures.saturating_add(1);
                    let d = Duration::from_secs(5)
                        .saturating_mul(1u32 << p.failures.min(5).saturating_sub(1))
                        .min(Duration::from_secs(120));
                    p.backoff_until = Some(Instant::now() + d);
                    // a slow or failing link should not keep its old rate
                    p.ewma_bps = p.ewma_bps.map(|b| b / 2.0);
                }
            }
            if i > 0 && fault != Fault::Lie {
                tracing::warn!(node = %url, error = e, "window from a secondary node failed; trying another node");
            }
        })
    }

    fn note_error(&self, i: usize, e: &RpcError) {
        self.with(i, |p| {
            p.status.windows_failed += 1;
            p.status.last_error = Some(e.to_string());
            if matches!(e, RpcError::Timeout(_)) {
                p.status.timeouts += 1;
            }
        })
    }

    /// Stop using node `i` (never the primary).
    pub fn drop_node(&self, i: usize, why: &str) {
        if i == 0 {
            tracing::error!(reason = why, "the primary node's data was contradicted; it stays the authority");
            return;
        }
        self.failed(i, Fault::Lie, why);
    }

    fn dropped(&self, i: usize) -> bool {
        self.with(i, |p| p.status.dropped_reason.is_some())
    }

    pub fn snapshot(&self) -> Vec<NodeStatus> {
        let now = Instant::now();
        (0..self.peers.len())
            .map(|i| {
                self.with(i, |p| {
                    let mut s = p.status.clone();
                    let wait = p.backoff_until.map(|t| t.saturating_duration_since(now)).unwrap_or_default();
                    s.backoff_secs = wait.as_secs();
                    s.bytes_per_sec = p.ewma_bps.map(|b| b as u64);
                    s.state = if s.dropped_reason.is_some() {
                        "dropped"
                    } else if !wait.is_zero() {
                        "backoff"
                    } else if !s.fetch && !s.submit && i > 0 {
                        "idle"
                    } else {
                        "ok"
                    }
                    .into();
                    s
                })
            })
            .collect()
    }
}

/// A failed attempt at a window on one node.
enum Attempt {
    /// About the window itself (too large) or the primary's own failure: no other node helps.
    Window(RpcError),
    /// About the node: try another one.
    Node(RpcError, Fault),
}

/// Several nodes as one [`ChainSource`] (see the module documentation). Node 0 is the primary.
pub struct MultiNode<N: ChainSource> {
    nodes: Vec<Arc<N>>,
    /// The primary again, on connections of its own: the acceptance data windows from other nodes are checked against.
    verifier: Arc<N>,
    specs: Vec<NodeSpec>,
    permits: Vec<Arc<Semaphore>>,
    board: Arc<NodeBoard>,
    rr: std::sync::atomic::AtomicUsize,
    /// The primary's sink blue score and when it was read (`MultiNode::primary_sink`).
    sink: Mutex<Option<(Instant, u64)>>,
}

impl<N: ChainSource> MultiNode<N> {
    /// `nodes[0]` is the primary; `verifier` a second client of the primary for the acceptance data.
    pub fn new(nodes: Vec<(NodeSpec, Arc<N>)>, verifier: Arc<N>) -> Self {
        assert!(!nodes.is_empty(), "a primary node");
        let specs: Vec<NodeSpec> = nodes.iter().map(|(s, _)| s.clone()).collect();
        let permits = specs.iter().map(|s| Arc::new(Semaphore::new(s.connections.max(1)))).collect();
        let board = NodeBoard::new(&specs);
        MultiNode {
            nodes: nodes.into_iter().map(|(_, n)| n).collect(),
            verifier,
            specs,
            permits,
            board,
            rr: std::sync::atomic::AtomicUsize::new(0),
            sink: Mutex::new(None),
        }
    }

    pub fn board(&self) -> Arc<NodeBoard> {
        self.board.clone()
    }

    pub fn specs(&self) -> &[NodeSpec] {
        &self.specs
    }

    /// Windows the source fetches at once: the connections of every fetching node.
    pub fn fetch_connections(&self) -> usize {
        self.specs.iter().filter(|s| s.fetch).map(|s| s.connections.max(1)).sum::<usize>().max(1)
    }

    /// The node for the next attempt, among those not tried yet for this window, not dropped and not backing off (the
    /// primary always qualifies): a node that is not slow (its rate at least a quarter of the best one's; an unmeasured
    /// node counts as fast, and one idle for five minutes is measured again) before a slow one, then one with a free
    /// connection, then the best rate.
    fn pick(&self, tried: &[bool]) -> Option<usize> {
        let now = Instant::now();
        let n = self.nodes.len();
        let first = self.rr.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut cands: Vec<(usize, bool, Option<f64>)> = vec![];
        // a primary that does not serve windows still takes one no other node can
        let fallback = !tried[0] && !self.specs[0].fetch;
        for k in 0..n {
            let i = (first + k) % n;
            if tried[i] || !self.specs[i].fetch || self.board.dropped(i) {
                continue;
            }
            let (backing_off, rate) = self.board.with(i, |p| {
                let stale = p.last_used.is_some_and(|t| now.saturating_duration_since(t) > REMEASURE);
                (p.backoff_until.is_some_and(|t| t > now), p.ewma_bps.filter(|_| !stale))
            });
            if i > 0 && backing_off {
                continue;
            }
            cands.push((i, self.permits[i].available_permits() > 0, rate));
        }
        let best_rate = cands.iter().filter_map(|c| c.2).fold(0.0, f64::max);
        let key = |&(_, free, rate): &(usize, bool, Option<f64>)| {
            let fast = rate.is_none_or(|r| r * SLOW_FACTOR >= best_rate);
            (fast, free, rate.unwrap_or(f64::MAX))
        };
        let mut best: Option<&(usize, bool, Option<f64>)> = None;
        for c in &cands {
            if best.is_none_or(|b| key(c) > key(b)) {
                best = Some(c);
            }
        }
        best.map(|c| c.0).or(fallback.then_some(0))
    }

    /// The primary's sink blue score, at most a second old (`at_least`: refreshed when below it). It only bounds requests
    /// for the primary's acceptance data: a stale sink makes the answer a few chain blocks longer, nothing else.
    async fn primary_sink(&self, at_least: u64) -> Result<u64, RpcError> {
        let cached = *self.sink.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, b)) = cached.filter(|(t, b)| t.elapsed() < Duration::from_secs(1) && *b > at_least) {
            return Ok(b);
        }
        let b = self.verifier.sink_blue_score().await?;
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), b));
        Ok(b)
    }

    /// The window from secondary `i`, checked against the primary: the node's own position first (it must know the window
    /// end), then its answer and the primary's acceptance data for the same start at once. The blue score of the window end
    /// from the node only bounds the primary's answer (a wrong one makes it shorter, and fewer chain blocks are verified, or
    /// longer).
    async fn fetch_checked(&self, i: usize, w: &WindowRequest) -> Result<Fetched, Attempt> {
        let origin = Origin { node: i, trusted: false };
        let node = &*self.nodes[i];
        let classify = |e: RpcError| match e {
            e @ RpcError::TooLarge { .. } => Attempt::Window(e),
            e @ RpcError::Node(_) => Attempt::Node(e, Fault::OutOfSync),
            e => Attempt::Node(e, Fault::Failed),
        };
        let end_blue = node.block_blue_score(w.end).await.map_err(classify)?.ok_or_else(|| {
            Attempt::Node(RpcError::Node(format!("the node does not know the window end {} (behind)", w.end)), Fault::OutOfSync)
        })?;
        let sink_blue = node.sink_blue_score().await.map_err(classify)?;
        let mine = WindowRequest { end_blue: Some(end_blue), sink_blue: Some(sink_blue), ..*w };
        let primary_sink = self.primary_sink(end_blue).await.map_err(Attempt::Window)?;
        let ids = self.verifier.chain_with_ids(w.start, window_min_confirmations(primary_sink, end_blue, w.min_confirmations));
        let (body, ids) = tokio::join!(fetch_window_from(node, &mine, Verbosity::Full, origin), ids);
        let f = body.map_err(classify)?;
        let ids = ids.map_err(Attempt::Window)?;
        let (mut f, verdict) = tokio::task::spawn_blocking(move || {
            let v = verify_window(&f.raw, &ids);
            (f, v)
        })
        .await
        .map_err(|e| Attempt::Window(RpcError::Decode(format!("verification task: {e}"))))?;
        match verdict {
            Ok(0) => Err(Attempt::Node(RpcError::Node("no chain block of the window could be checked".into()), Fault::OutOfSync)),
            Ok(n) => {
                cut(&mut f.raw, n);
                Ok(f)
            }
            Err(Rejection::Lie(why)) => Err(Attempt::Node(RpcError::Node(why), Fault::Lie)),
            Err(Rejection::OutOfSync(why)) => Err(Attempt::Node(RpcError::Node(why), Fault::OutOfSync)),
            Err(Rejection::ChainMoved) => {
                Err(Attempt::Window(RpcError::Node(format!("the window start {} left the primary's selected chain", w.start))))
            }
        }
    }
}

/// Keep the first `n` chain blocks of a window (`wire_bytes` shrinks in proportion to the chain blocks and transactions
/// kept: the size accounting of the prefetch).
fn cut(raw: &mut RawVspcResponse, n: usize) {
    let total = raw.added_chain_block_hashes.len();
    if total > n {
        let units = |blocks: &[super::types::RawChainBlock]| -> u128 {
            blocks.iter().map(|b| 1 + b.accepted_transactions.len() as u128).sum::<u128>().max(1)
        };
        let (keep, all) = (units(&raw.chain_block_accepted_transactions[..n]), units(&raw.chain_block_accepted_transactions));
        raw.wire_bytes = (raw.wire_bytes as u128 * keep / all) as usize;
        raw.added_chain_block_hashes.truncate(n);
        raw.chain_block_accepted_transactions.truncate(n);
    }
}

impl<N: ChainSource> ChainSource for MultiNode<N> {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        self.nodes[0].vspc_v2(req).await
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.nodes[0].dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.nodes[0].server_info().await
    }
    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.nodes[0].utxos_by_addresses(addresses).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.nodes[0].block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.nodes[0].sink_blue_score().await
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.nodes[0].block_blue_score(hash).await
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.nodes[0].chain_hashes(start).await
    }
    async fn chain_with_ids(&self, start: Hash32, min_confirmations: Option<u64>) -> Result<ChainIds, RpcError> {
        self.verifier.chain_with_ids(start, min_confirmations).await
    }

    async fn fetch_window(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        let mut tried = vec![false; self.nodes.len()];
        let mut last: Option<RpcError> = None;
        while let Some(i) = self.pick(&tried) {
            tried[i] = true;
            let permit = self.permits[i].clone().acquire_owned().await.expect("the semaphore is never closed");
            self.board.begin(i);
            let r = if i == 0 {
                fetch_window_from(&*self.nodes[0], &w, Verbosity::High, Origin::PRIMARY).await.map_err(Attempt::Window)
            } else {
                self.fetch_checked(i, &w).await
            };
            self.board.end(i);
            drop(permit);
            match r {
                Ok(f) => {
                    self.board.fetched(i, &f);
                    return Ok(f);
                }
                Err(Attempt::Window(e)) => {
                    if i == 0 && !matches!(e, RpcError::TooLarge { .. }) {
                        // the primary is never skipped: counted only
                        self.board.note_error(0, &e);
                    }
                    return Err(e);
                }
                Err(Attempt::Node(e, fault)) => {
                    self.board.failed(i, fault, &e.to_string());
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| RpcError::Transport("no node available for the window".into())))
    }

    async fn fetch_window_primary(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        // on the verifier's connections: never behind the primary's own windows of the parallel fetch
        fetch_window_from(&*self.verifier, &w, Verbosity::High, Origin::PRIMARY).await
    }

    fn report_lie(&self, origin: Origin, why: &str) {
        self.board.drop_node(origin.node, why);
    }

    fn node_stats(&self) -> Vec<NodeStatus> {
        if self.nodes.len() > 1 {
            self.board.snapshot()
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::redact_url;

    #[test]
    fn redacted_urls_keep_scheme_host_and_port_only() {
        for (url, shown) in [
            ("ws://127.0.0.1:18210", "ws://127.0.0.1:18210"),
            ("ws://127.0.0.1:18210/", "ws://127.0.0.1:18210"),
            ("wss://user:pw@node.example/wrpc/json?apikey=K", "wss://node.example/..."),
            ("wss://node.example:443?apikey=K", "wss://node.example:443/..."),
            ("wss://node.example/v2/K", "wss://node.example/..."),
            ("wss://a@b@node.example#f", "wss://node.example/..."),
            ("n0", "n0"),
            ("user:pw@host:1/x", "host:1/..."),
        ] {
            assert_eq!(redact_url(url), shown, "{url}");
        }
    }

    /// Node URLs with a password or an API key are configured for the connection, but the public health and metrics
    /// routes and the logs show only the scheme, host and port.
    #[test]
    fn node_credentials_stay_out_of_health_and_metrics() {
        let secret = "wss://user:hunter2@node.example/wrpc/json?apikey=SECRETKEY";
        let board = NodeBoard::new(&[
            NodeSpec { url: "ws://127.0.0.1:18210".into(), fetch: true, submit: true, connections: 1 },
            NodeSpec { url: secret.into(), fetch: true, submit: true, connections: 1 },
        ]);
        assert_eq!(board.url(1), "wss://node.example/...");
        let mut h = crate::indexer::status::HealthSnapshot::new("testnet-10");
        h.nodes = board.snapshot();
        let (m, j) = (h.metrics_text(), serde_json::to_string(&h).unwrap());
        for text in [&m, &j] {
            assert!(!text.contains("hunter2") && !text.contains("SECRETKEY") && !text.contains("user:"), "{text}");
            assert!(text.contains("wss://node.example/..."), "{text}");
        }
    }
}
