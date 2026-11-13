//! Several nodes behind one [`ChainSource`].
//!
//! One node is the chain authority. It starts as node 0, the configured primary. The other nodes
//! serve transaction bodies, which are checked against the authority, and they take submissions
//! (`matcher::node::MultiSubmit`).
//!
//! # Roles
//!
//! The authority answers every call the follower makes itself. These are the cursor's position, the
//! chain hashes the windows are planned on, the single steps near the sink and every reorg
//! decision. Only the windows of the parallel fetch (`indexer::prefetch`) are spread over the
//! nodes. They carry the bulk of the bytes.
//!
//! - A window goes to the node with a free connection that delivered the most bytes per second
//!   lately. A node that is not measured yet is tried first. The authority takes part.
//! - A window from another node is asked for at `Full` verbosity and checked before the follower
//!   sees it ([`super::verify`]). Its chain blocks must be the authority's. Every header must hash
//!   to its block hash. Every chain block must have accepted exactly the transactions the authority
//!   reports, fetched at the same time. Every transaction must hash to its id.
//! - The follower takes the parts no hash covers from the authority, for every transaction that can
//!   matter to KOB. It does this before it applies the window (`indexer::trust`).
//! - A node that fails a window is skipped for a while, and the window goes to the next node. A
//!   failure is a transport error, a timeout, an undecodable answer, or an answer that claims more
//!   than the client takes at all (`rpc::MAX_MESSAGE_BYTES`). The pause starts at 5 s and doubles up
//!   to 80 s. A node that does not know the window or follows another chain is paused for 10 s.
//! - An answer too large only for the window's own limit is no failure of the node. The window is
//!   split (`indexer::prefetch`).
//! - A node that contradicts a hash or the authority's acceptance data is dropped until the process
//!   restarts (`lies`).
//!
//! # A cross-checking node
//!
//! A node with `NodeSpec::cross_check` also vouches for the authority's own data. It is the second
//! node of a resolver pair (`rpc::resolver`). Every answer of the authority that the follower
//! applies is compared with this node's acceptance data from the same start. This covers the single
//! steps near the sink and the windows (`verify::agreement`). Both nodes must report the same chain
//! block at every position and the same accepted transactions in it.
//!
//! Only what both report is applied.
//!
//! - The authority alone never makes the store rewind or halt. Its `cannot find header` or
//!   `retention root` about the cursor, and its "unknown" for a stored block in the walk back,
//!   count only when the cross-checking node says the same (`ChainSource::confirm_unknown_cursor`).
//!   When it knows the cursor, that is a contradiction like the others below.
//! - At the first contradiction the answer is cut before it.
//! - When the very next chain block is contradicted, nothing is applied until the two agree. A race
//!   at the tip settles within seconds.
//! - A contradiction is logged and counted (`disagreements`). The tips differing for a moment are
//!   only counted apart, as `tip_differs`, and logged at debug.
//! - A contradiction that blocks the cursor [`STALLS_BEFORE_REPLACING`] times in a row gives the
//!   cross-checking node up for another. Its resolver entry resolves a replacement.
//! - After as many more, the authority's node is given up too. The cross-checking node becomes the
//!   authority (`resolver::replace`).
//!
//! A cross-checking node that cannot answer is skipped with a backoff, and the authority goes on
//! alone meanwhile. Nothing stalls. No contradiction is ignored, but none can be seen while there
//! is no second node. The node has [`CROSS_CHECK_STEP_TIMEOUT`] for a single step near the sink, so
//! that a hanging node does not hold the book back. It has [`CROSS_CHECK_TIMEOUT`] for a window.
//! Its timeouts count against it (`rpc::judge`).
//!
//! # The health watch
//!
//! The watch checks every node of the list the same way, the authority and the others alike. Every
//! [`WATCH_EVERY`] each node is asked `getServerInfo` within [`WATCH_TIMEOUT`]. The answer is
//! judged (`rpc::judge`). It can show another network, no UTXO index or a node that is not synced
//! for long. A timeout counts as a finding too, for a node that hangs.
//!
//! A node is behind when it is more than [`LAG_DAA`] behind the most advanced node for
//! [`LAG_CHECKS`] checks in a row and has confirmed that lead itself. A DAA score is each node's
//! own claim, and a node that over-reports must not put its honest partner behind. So the lead is
//! confirmed in these steps.
//!
//! - The most advanced node's sink is taken at the first check of the streak and at the
//!   [`LAG_CHECKS`]-th.
//! - The node behind is asked for each sink (`getBlock`) at every check after that.
//! - Once it knows a sink, that block's DAA must be more than [`LAG_DAA`] above the node's own DAA
//!   of the check. Both claims must be confirmed this way.
//! - A sink the node does not learn within [`LAG_CONFIRM_CHECKS`] checks, or one that is not that
//!   far ahead, refutes the lead. This is warned about at most every ten minutes. Nothing is held
//!   against the node behind. It is not suspect either until it confirmed a lead.
//!
//! A node that stopped advancing never confirms a lead. The DAG and the local monotonic clock judge
//! it instead, never a block's timestamp, which its miner sets. The node has stopped when its sink
//! has not changed for [`FROZEN_AFTER`] while another node's sink changed within that time. When no
//! other node answers, the limit is [`FROZEN_ALONE_AFTER`]. Only a node whose sink was seen to
//! change counts as advancing. A node seen for the first time has no vote yet. The finding is
//! asserted again at every check while the sink stays where it was found frozen.
//!
//! A lead that the DAG does not back counts against the node that claims it. It is a contradiction
//! when the node's virtual DAA score is more than [`LAG_DAA`] above its own sink's, as the node and
//! the node behind both report it. The node behind knows that block. The finding needs
//! [`INFLATED_CHECKS`] checks in a row.
//!
//! A node that contradicts the other node, by a hash or by the acceptance data, is judged too. What
//! follows depends on the node.
//!
//! - A resolver entry gives it up for another public node. The authority's role moves to the other
//!   node at once, unless that node is suspect itself (`rpc::resolver`).
//! - A node named in the configuration raises an alarm.
//! - A cross-checking node is cut off from the pair until the alarm ends. The authority goes on
//!   alone meanwhile.
//!
//! The `cut_reason` of each node says why it was last cut off.
//!
//! # A failing authority
//!
//! A node can answer its small calls (`getServerInfo`, the sink) while its chain requests time out
//! or lose the connection. The follower finds this when 2 chain requests in a row fail on the wire
//! and the cursor has not moved for 3 minutes (`indexer::follower`, "A failing chain authority").
//! It then calls [`MultiNode::authority_failing`].
//!
//! The old authority gets a chain alarm (`judge::chain_failing`). What follows depends on the
//! nodes.
//!
//! - A resolver entry gives its public node up. The other node of the pair is the authority at
//!   once, unless the judge suspects it. The entry resolves a replacement on another host.
//! - Among named nodes, the first other node that can lead becomes the authority, and the roles
//!   swap (`NodeBoard::authority`).
//! - A single node has nothing to promote. The alarm stays and the follower retries with its
//!   backoff.
//!
//! A named node can lead when all of these hold.
//!
//! - The configuration lets it lead (`NodeSpec::may_lead`). A submit-only node does not, unless the
//!   operator says so.
//! - It is not dropped, has no alarm and is not suspect.
//! - It did not contradict the authority within [`PROMOTE_CLEAN_FOR`].
//! - Within [`PROMOTE_CHECK_TIMEOUT`] it shows the right network and is synced. It knows the
//!   cursor, and the cursor is on its selected chain. When the role reads funds from the authority
//!   (`judge::require_utxo_index`, the Executor), it has a UTXO index.
//!
//! Every answer of the new authority from the cursor where it took over is checked, until the
//! cursor moves on from there: it may remove at most [`SETTLE_MAX_REORG`] chain blocks and must know
//! the cursor. An answer that fails the check is refused. A resolver entry whose node changed, for
//! whatever reason, is checked the same way.
//!
//! The old authority is a secondary node now. It is cut off from the pair until its alarm ends. The
//! health watch probes it with chain requests. The alarm ends when they answered for
//! `judge::ALARM_CLEAR_AFTER`, which is 60 s, without a break. The node leads again only if the new
//! authority fails the same way. So the role does not flap.
//!
//! A swap is logged at warn. It is counted in `nodes[].promoted` and in
//! `kob_indexer_authority_replaced_total`.
//!
//! # Status
//!
//! `GET /v1/health` reports each node under `nodes`.

use super::types::{AddressUtxo, ChainHashes, ChainIds, DagInfo, RawVspcResponse, ServerInfo, Verbosity, VspcRequest};
use super::verify::{verify_window, Rejection};
use super::{fetch_window_from, window_min_confirmations, ChainSource, Fetched, Origin, RpcError, WindowRequest};
use crate::hex::Hash32;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// One node of the list.
#[derive(Debug, Clone)]
pub struct NodeSpec {
    /// The configured URL. A resolver entry has a pseudo URL such as `resolver`.
    pub url: String,
    /// Serves windows of the parallel fetch.
    pub fetch: bool,
    /// Takes the executor's transactions.
    pub submit: bool,
    /// Windows fetched from it at once.
    pub connections: usize,
    /// It vouches for the authority's own answers too (see the module documentation). This is the
    /// second node of a resolver pair.
    pub cross_check: bool,
    /// It may become the chain authority when the authority's chain requests keep failing
    /// (`MultiNode::authority_failing`).
    ///
    /// The configuration decides (`[[nodes]] may_lead`). It defaults to whether the node serves
    /// windows, so a submit-only node never leads unless the operator says so. The configured
    /// primary always may.
    pub may_lead: bool,
}

/// What `/v1/health` reports per node.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NodeStatus {
    /// The URL with credentials and keys removed.
    pub url: String,
    /// The host a resolver entry uses now.
    ///
    /// It is `None` for a node that is not a resolver entry, and while an entry is being replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// How many times a resolver entry gave its node up, after a failure or a disagreement.
    pub replaced: u32,
    /// How many times this node took over as chain authority from a failing one.
    ///
    /// See `MultiNode::authority_failing`.
    pub promoted: u32,
    /// Why the node was last cut off from the pair, or `None` if it never was.
    ///
    /// A resolver entry shows the public node it gave up and why, as `host: reason`. A named node
    /// shows its alarms now (`rpc::judge`) or the reason it was dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut_reason: Option<String>,
    /// Answers of the authority that this node contradicted.
    ///
    /// Only a cross-checking node counts here (`NodeSpec::cross_check`). A contradiction is other
    /// accepted transactions, a different chain block deep in the answer or a deep reorg seen by
    /// one node only. The tips differing for a moment are not counted.
    pub disagreements: u64,
    /// Times the two nodes' newest chain blocks differed for a moment.
    ///
    /// This is routine on a public pair. It settles by the next poll.
    pub tip_differs: u64,
    /// `primary` for the chain authority, else `secondary`.
    pub role: String,
    /// The node's state, one of the values below.
    ///
    /// - `ok` is the normal state.
    /// - `backoff` means it failed lately and is skipped for `backoff_secs`.
    /// - `dropped` means it contradicted the authority.
    /// - `cut` means a named node has an alarm and is left out of the pair until it ends (see
    ///   `cut_reason`).
    /// - `idle` means it neither fetches nor takes submissions.
    pub state: String,
    /// Serves windows of the parallel fetch.
    pub fetch: bool,
    /// Takes the executor's transactions.
    pub submit: bool,
    /// Windows fetched from it at once.
    pub connections: usize,
    /// Windows fetched now.
    pub in_flight: u64,
    /// Windows fetched successfully.
    pub windows_ok: u64,
    /// Windows that failed.
    pub windows_failed: u64,
    /// Chain blocks in the windows fetched.
    pub blocks: u64,
    /// Accepted transactions in the windows fetched.
    pub txs: u64,
    /// Bytes received for the windows fetched.
    pub bytes: u64,
    /// Bytes per second of its recent windows, as a moving average.
    ///
    /// It is `None` until a window of at least 64 KiB was measured.
    pub bytes_per_sec: Option<u64>,
    /// How long its last window took, in milliseconds.
    pub last_fetch_ms: u64,
    /// Requests that timed out.
    pub timeouts: u64,
    /// Windows refused for a node on another chain than the authority's or without the window.
    pub out_of_sync: u64,
    /// Contradictions of a hash or of the authority's data. The node is dropped at the first.
    pub lies: u64,
    /// Why the node was dropped, if it was.
    pub dropped_reason: Option<String>,
    /// Seconds until the node is tried again.
    pub backoff_secs: u64,
    /// The last error the node caused.
    pub last_error: Option<String>,
    /// Submissions the node took.
    ///
    /// A submission counts when the node accepted it, or already had the transaction from a peer or
    /// the chain.
    pub submits_ok: u64,
    /// Submissions the node refused.
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
    /// When it last contradicted the authority as the cross-checking node. Tips that differ for a
    /// moment do not count.
    last_disagreement: Option<Instant>,
}

/// A node whose rate is below the best rate divided by this gets windows only when no faster node
/// qualifies. Windows are applied in chain order, so a slow window holds up every window behind it.
const SLOW_FACTOR: f64 = 4.0;
/// A node's rate is measured again when it got no window for this long. Its next window then counts
/// as unmeasured.
const REMEASURE: Duration = Duration::from_secs(300);

/// Per-node state shared by the chain source and the submitter.
pub struct NodeBoard {
    peers: Vec<Mutex<PeerState>>,
    /// The configured URL of each node.
    ///
    /// A resolver entry has a pseudo URL. The node it uses is looked up when shown.
    entries: Vec<String>,
    /// The index of the chain authority.
    ///
    /// It is node 0, the configured primary, until another node is promoted
    /// (`MultiNode::authority_failing`).
    authority: AtomicUsize,
}

/// How a failure counts against a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// A transport error, a timeout or an undecodable answer. The node backs off, doubling each
    /// time.
    Failed,
    /// The node does not know the window or follows another chain right now. It pauses for 10 s.
    OutOfSync,
    /// It contradicted a hash or the authority. The node is judged, and dropped unless it is the
    /// authority or a resolver entry.
    Lie,
}

impl NodeBoard {
    /// A board for the nodes of `specs`. Node 0 is the authority.
    pub fn new(specs: &[NodeSpec]) -> Arc<NodeBoard> {
        let peers = specs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                Mutex::new(PeerState {
                    status: NodeStatus {
                        // Shown on the public health and metrics routes and in logs, so it never
                        // carries credentials or keys.
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
        Arc::new(NodeBoard { peers, entries: specs.iter().map(|s| s.url.clone()).collect(), authority: AtomicUsize::new(0) })
    }

    /// The index of the chain authority now.
    ///
    /// It is 0, the configured primary, unless another node was promoted.
    pub fn authority(&self) -> usize {
        self.authority.load(Ordering::Relaxed)
    }

    /// The configured URL of node `i`.
    pub fn entry(&self, i: usize) -> &str {
        &self.entries[i]
    }

    /// The host node `i` uses now, for logs and the status line.
    ///
    /// A resolver entry shows its public node. Any other node shows its redacted URL.
    pub fn shown(&self, i: usize) -> String {
        let e = &self.entries[i];
        if super::resolver::is_resolver(e) {
            super::resolver::current(e).map(|n| super::resolver::host(&n)).unwrap_or_else(|| format!("{e} (resolving)"))
        } else {
            super::redact_url(e)
        }
    }

    /// The node entry `i` stands for now.
    ///
    /// A resolver entry gives its public node, or `None` while it is being replaced. Any other
    /// entry gives its URL.
    pub fn node_of(&self, i: usize) -> Option<String> {
        let e = &self.entries[i];
        if super::resolver::is_resolver(e) {
            super::resolver::current(e)
        } else {
            Some(e.clone())
        }
    }

    /// Whether the named node `i` is cut off from the pair by an alarm (`rpc::judge::cuts_off`).
    ///
    /// The authority and resolver entries are never cut off. A cut node is left out until the alarm
    /// ends. A node that contradicted the authority is dropped instead (`dropped_reason`).
    fn cut(&self, i: usize) -> bool {
        i != self.authority() && !super::resolver::is_resolver(&self.entries[i]) && super::judge::cuts_off(&self.entries[i])
    }

    /// The cross-checking node `i` contradicted the authority.
    fn disagreed(&self, i: usize, why: &str, at_tip: bool) {
        self.with(i, |p| {
            if at_tip {
                // Tips that differ for a moment are no contradiction. They are counted apart and
                // show no error.
                p.status.tip_differs += 1;
            } else {
                p.status.disagreements += 1;
                p.status.last_error = Some(why.to_string());
                p.last_disagreement = Some(Instant::now());
            }
        })
    }

    fn with<T>(&self, i: usize, f: impl FnOnce(&mut PeerState) -> T) -> T {
        f(&mut self.peers[i].lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The number of nodes.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Whether there are no nodes.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// The redacted URL of node `i`.
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
            // Only answers of some size say anything about the link.
            if bytes >= 64 * 1024 {
                let bps = bytes as f64 / secs;
                p.ewma_bps = Some(p.ewma_bps.map_or(bps, |o| 0.7 * o + 0.3 * bps));
            }
        })
    }

    fn failed(&self, i: usize, fault: Fault, e: &str) {
        let url = self.url(i);
        let public = super::resolver::is_resolver(&self.entries[i]);
        let authority = i == self.authority();
        if fault == Fault::Lie {
            // Judge it outside the node's lock. A resolver entry gives its node up for another. A
            // named node raises an alarm and is dropped below, unless it is the authority.
            if let Some(node) = self.node_of(i) {
                super::judge::contradicted(&self.entries[i], &node, e);
            }
        }
        self.with(i, |p| {
            let s = &mut p.status;
            s.windows_failed += 1;
            s.last_error = Some(e.to_string());
            match fault {
                Fault::Lie => {
                    s.lies += 1;
                    // A resolver entry's node was replaced, so its successor is not dropped.
                    if !authority && !public && s.dropped_reason.is_none() {
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
                    // A slow or failing link should not keep its old rate.
                    p.ewma_bps = p.ewma_bps.map(|b| b / 2.0);
                }
            }
            if !authority && fault != Fault::Lie {
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

    /// Stops using node `i`, or raises an alarm when it is the authority.
    ///
    /// A resolver entry's node is replaced. When that is the authority's node, the other node
    /// becomes the authority. A named authority stays the authority and gets an alarm.
    pub fn drop_node(&self, i: usize, why: &str) {
        if i == self.authority() {
            tracing::error!(reason = why, node = %self.shown(i), "the primary node's data was contradicted");
            if let Some(node) = self.node_of(i) {
                super::judge::contradicted(&self.entries[i], &node, why);
            }
            return;
        }
        self.failed(i, Fault::Lie, why);
    }

    fn dropped(&self, i: usize) -> bool {
        self.with(i, |p| p.status.dropped_reason.is_some())
    }

    /// The status of every node, as `/v1/health` shows it.
    pub fn snapshot(&self) -> Vec<NodeStatus> {
        let now = Instant::now();
        let authority = self.authority();
        (0..self.peers.len())
            .map(|i| {
                self.with(i, |p| {
                    let mut s = p.status.clone();
                    s.role = if i == authority { "primary" } else { "secondary" }.into();
                    let cut = if super::resolver::is_resolver(&self.entries[i]) {
                        s.node = super::resolver::current(&self.entries[i]).map(|n| super::resolver::host(&n));
                        s.replaced = super::resolver::replacements(&self.entries[i]);
                        s.cut_reason = super::resolver::last_reason(&self.entries[i]);
                        false
                    } else {
                        s.cut_reason = super::judge::alarm_of(&self.entries[i]).or_else(|| s.dropped_reason.clone());
                        super::judge::cuts_off(&self.entries[i])
                    };
                    let wait = p.backoff_until.map(|t| t.saturating_duration_since(now)).unwrap_or_default();
                    s.backoff_secs = wait.as_secs();
                    s.bytes_per_sec = p.ewma_bps.map(|b| b as u64);
                    s.state = if s.dropped_reason.is_some() {
                        "dropped"
                    } else if cut && i != authority {
                        "cut"
                    } else if !wait.is_zero() {
                        "backoff"
                    } else if !s.fetch && !s.submit && i != authority {
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
    /// About the window itself (too large) or the authority's own failure. No other node helps.
    Window(RpcError),
    /// About the node. Try another one.
    Node(RpcError, Fault),
}

/// Several nodes as one [`ChainSource`].
///
/// Node 0 is the configured primary. It is the chain authority until another node is promoted. See
/// the module documentation.
pub struct MultiNode<N: ChainSource> {
    nodes: Vec<Arc<N>>,
    /// Node 0 again, on connections of its own.
    ///
    /// Windows from other nodes are checked against its acceptance data.
    verifier: Arc<N>,
    specs: Vec<NodeSpec>,
    permits: Vec<Arc<Semaphore>>,
    board: Arc<NodeBoard>,
    rr: std::sync::atomic::AtomicUsize,
    /// The authority's sink blue score and when it was read (`MultiNode::primary_sink`).
    sink: Mutex<Option<(Instant, u64)>>,
    /// Contradictions in a row that left nothing to apply (`MultiNode::vouched`).
    stalls: std::sync::atomic::AtomicU32,
    /// When a contradiction was last logged. One line per ten seconds is logged, and the counters
    /// keep every one.
    last_warn: Mutex<Option<Instant>>,
    /// The health watch (module documentation).
    watch: Arc<Mutex<Watch>>,
    /// Public chain authorities that contradicted the store at one cursor.
    ///
    /// It holds the cursor, the hosts and the count (`MultiNode::authority_contradicted`).
    store_contradictions: Mutex<Option<(Hash32, Vec<String>, u32)>>,
    /// The cursor from which the authority is not yet proven, if it is settling.
    ///
    /// It is set when a node is promoted or the authority's resolver entry uses another node. It
    /// stays set until the follower asks from another cursor (`MultiNode::settle`).
    settling: Mutex<Option<Hash32>>,
    /// The node the authority used at its last answer. A change makes it settle.
    authority_node: Mutex<Option<String>>,
    /// The start of the follower's last single step, its cursor then.
    last_step: Mutex<Option<Hash32>>,
    /// The cursor of the last chain request to the authority.
    ///
    /// A promoted node must continue it. A demoted node is probed from it (`probe_demoted`).
    last_start: Mutex<Option<Hash32>>,
    /// Whether a probe of the demoted nodes runs now. Only one runs at a time.
    probing: Arc<AtomicBool>,
}

/// The state of the health watch.
///
/// It holds the period, the number of checks in a row that make a node behind, when it last ran,
/// whether a check runs now, each node's lag and when an unconfirmed lead was last warned about.
struct Watch {
    every: Duration,
    lag_checks: u32,
    last: Option<Instant>,
    busy: bool,
    lag: Vec<LagTrack>,
    last_refuted_warn: Option<Instant>,
    /// The local monotonic clock that elapsed time is measured on. Tests set another one
    /// (`MultiNode::with_clock`). A block's timestamp is set by its miner, so it never serves as
    /// the referee.
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    /// Each node's sink at the last check, and since when it has not changed on the local monotonic
    /// clock.
    progress: Vec<Option<Progress>>,
    /// The most advanced node and its checks in a row with a virtual DAA score more than
    /// [`LAG_DAA`] above its own sink's.
    ///
    /// That is a lead it does not have.
    inflated: Option<(String, u32)>,
}

/// A node's sink and since when it has not changed.
#[derive(Debug, Clone)]
struct Progress {
    node: String,
    sink: Hash32,
    since: Instant,
    /// Whether this stillness was reported already. It is reported once per sink. The finding is
    /// asserted again at every check while the sink stays the same, so a named node's alarm does
    /// not end while it is still frozen.
    reported: bool,
    /// Whether the sink was seen to change on this node.
    ///
    /// Only such a node counts as advancing. A node seen for the first time has no vote until its
    /// sink changes. This covers a new node, a replacement and one that answers again. So a network
    /// stall does not make it look advancing next to a node that stands still.
    moved: bool,
}

/// The most advanced node's lead over a node, as the answers of one check put it.
///
/// It is kept until the node behind can confirm it.
#[derive(Debug, Clone, Copy)]
struct Claim {
    /// The most advanced node's sink at the check.
    sink: Hash32,
    /// The node's own DAA at the check.
    own: u64,
    /// Checks since, while unconfirmed.
    age: u32,
    /// `Some(lead)` once the node knows the sink. Its DAA is then `lead` above `own`. `None` before
    /// that.
    lead: Option<u64>,
}

/// A node's checks in a row behind the most advanced answer, and the claims of that lead.
///
/// The claims are taken at the first check of the streak and at the [`LAG_CHECKS`]-th (module
/// documentation).
#[derive(Debug, Clone, Default)]
struct LagTrack {
    /// The node the streak is about. A resolver entry that moved to another node begins again.
    node: String,
    streak: u32,
    claims: Vec<Claim>,
}

/// What one check makes of a node's lag.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LagVerdict {
    /// Not behind.
    Level,
    /// Behind by the answers (DAA) but not confirmed yet.
    Unconfirmed(u64),
    /// The other node's lead was not borne out. Its sink stayed unknown to this node for
    /// [`LAG_CONFIRM_CHECKS`] checks, or it was not that far ahead.
    Refuted(u64),
    /// Behind for the whole period and confirmed by this node itself. It carries the smallest
    /// confirmed lead, in DAA.
    Confirmed(u64),
}

impl LagTrack {
    /// The streak begins again. The node stays.
    fn clear(&mut self) {
        self.streak = 0;
        self.claims.clear();
    }

    /// Whether this streak has evidence of its own that the node is behind.
    ///
    /// That is a lead the node confirmed. The other node's sink is a real block the node placed,
    /// that far ahead. Only then is the node suspect and not promoted. The answers alone are a
    /// claim.
    fn evidence(&self) -> bool {
        self.claims.iter().any(|c| c.lead.is_some_and(|l| l > LAG_DAA))
    }

    /// The claims a streak needs before a lag is confirmed. These are one at its first check and
    /// one at its `checks`-th.
    fn needed(checks: u32) -> usize {
        if checks <= 1 {
            1
        } else {
            2
        }
    }

    /// Records one check, behind by the answers (`Some(daa)`) or not. Returns whether a new claim
    /// is wanted now.
    fn observe(&mut self, behind: Option<u64>, checks: u32) -> bool {
        if behind.is_none() {
            self.clear();
            return false;
        }
        self.streak += 1;
        let n = self.claims.len();
        n < Self::needed(checks) && (n == 0 || self.streak >= checks)
    }

    /// The verdict after this check's claims and confirmations.
    ///
    /// `behind` is the answers' lag now, in DAA. A refuted or a confirmed streak begins again at
    /// the next check.
    fn verdict(&mut self, behind: Option<u64>, checks: u32) -> LagVerdict {
        let Some(behind) = behind else { return LagVerdict::Level };
        for c in self.claims.iter_mut().filter(|c| c.lead.is_none()) {
            c.age += 1;
        }
        let refuted =
            self.claims.iter().any(|c| c.lead.is_some_and(|l| l <= LAG_DAA) || (c.lead.is_none() && c.age > LAG_CONFIRM_CHECKS));
        if refuted {
            self.clear();
            return LagVerdict::Refuted(behind);
        }
        if self.claims.len() == Self::needed(checks) && self.claims.iter().all(|c| c.lead.is_some()) {
            let lead = self.claims.iter().filter_map(|c| c.lead).min().unwrap_or(behind);
            self.clear();
            return LagVerdict::Confirmed(lead);
        }
        LagVerdict::Unconfirmed(behind)
    }
}

/// The start of the error when the two nodes' newest chain blocks differ and nothing can be applied
/// yet.
///
/// This is routine on a public pair, and the next poll settles it. The follower does not warn about
/// it until it repeats.
pub const TIPS_DIFFER: &str = "the two nodes' newest chain blocks differ for a moment";

/// Contradictions in a row that block the cursor before the cross-checking node is given up for
/// another.
///
/// The authority's node is given up after twice this many (`MultiNode::vouched`).
pub const STALLS_BEFORE_REPLACING: u32 = 3;

/// How long the cross-checking node may take to vouch for one window. The authority goes on alone
/// after that.
pub const CROSS_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long it may take to vouch for one single step near the sink, which is a few chain blocks.
///
/// A hanging node must not hold the book back for long.
pub const CROSS_CHECK_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// The health watch's period.
pub const WATCH_EVERY: Duration = Duration::from_secs(10);
/// How long one node's `getServerInfo` may take in a check of the health watch.
pub const WATCH_TIMEOUT: Duration = Duration::from_secs(5);

/// A node more than this many DAA behind the most advanced node of the list may be behind.
///
/// This is about 30 s of chain.
pub const LAG_DAA: u64 = 300;
/// Checks in a row, about a minute, after which such a node is behind.
///
/// A resolver entry gives the node up and a named node gets an alarm, once the node confirmed the
/// lead itself (module documentation).
pub const LAG_CHECKS: u32 = 6;
/// Checks, about five minutes, within which a node must learn the sink of a check that put it
/// behind.
///
/// A sink it never learns was not real, and nothing is held against the node.
pub const LAG_CONFIRM_CHECKS: u32 = 30;
/// How long a node's sink may stand still while another node's sink changes.
///
/// It is measured on the local monotonic clock. A node over this limit stopped advancing, whatever
/// its DAA says. The chain makes about ten blocks a second on mainnet and testnet-10 alike. The
/// node's own answers decide, because no other node can make an honest node's sink stand still. A
/// resolver entry gives such a node up and a named node gets an alarm.
pub const FROZEN_AFTER: Duration = Duration::from_secs(120);
/// How long a node's sink may stand still when no other node answers and there is nothing to
/// compare it with.
pub const FROZEN_ALONE_AFTER: Duration = Duration::from_secs(600);
/// Checks in a row after which an inflated lead counts against the most advanced node.
///
/// A lead is inflated when the node's virtual DAA score is more than [`LAG_DAA`] above the DAA
/// score of its own sink. Both the node and the node behind, which knows that block, must report it
/// so. The block both sides place in the DAG does not back the lead the node claims.
pub const INFLATED_CHECKS: u32 = 2;
/// Contradictions of the store at one cursor before the store halts there (`gap`).
///
/// The store also halts at once when two authorities on different hosts contradicted it at the same
/// cursor. They agree with each other against the store, so the store is at fault and not the
/// nodes.
pub const STORE_CONTRADICTIONS_MAX: u32 = 3;
/// How long a node may take to show that it can be the chain authority
/// (`MultiNode::authority_failing`).
///
/// It must be on the right network and synced. It must know the cursor, and the cursor must be on
/// its selected chain.
pub const PROMOTE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// A cross-checking node that contradicted the authority within this long is not promoted.
pub const PROMOTE_CLEAN_FOR: Duration = Duration::from_secs(600);
/// The most chain blocks that an answer of the new authority from the cursor where it took over
/// may remove (`MultiNode::settle`).
///
/// This is about 10 s of chain. It covers a reorg that the old authority had not seen yet. A deeper
/// one is refused, and so is a cursor the node does not know.
pub const SETTLE_MAX_REORG: usize = 100;
/// How long a demoted node's chain request may take in a probe (`probe_demoted`).
pub const DEMOTED_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Nodes promoted to chain authority in this process.
///
/// This counts named nodes (`MultiNode::authority_failing`). A resolver entry's replacement is
/// counted by `resolver::authority_replaced`. Both are part of
/// `kob_indexer_authority_replaced_total`.
static PROMOTIONS: AtomicU64 = AtomicU64::new(0);

/// Named nodes promoted to chain authority since the start (see [`PROMOTIONS`]).
pub fn promotions() -> u64 {
    PROMOTIONS.load(Ordering::Relaxed)
}

impl<N: ChainSource> MultiNode<N> {
    /// A source over `nodes`, with `nodes[0]` as the primary.
    ///
    /// `verifier` is a second client of the primary. It serves the acceptance data.
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
            stalls: std::sync::atomic::AtomicU32::new(0),
            last_warn: Mutex::new(None),
            store_contradictions: Mutex::new(None),
            settling: Mutex::new(None),
            authority_node: Mutex::new(None),
            last_step: Mutex::new(None),
            last_start: Mutex::new(None),
            probing: Arc::new(AtomicBool::new(false)),
            // The first check is one period after the start.
            watch: Arc::new(Mutex::new(Watch {
                every: WATCH_EVERY,
                lag_checks: LAG_CHECKS,
                last: Some(Instant::now()),
                busy: false,
                lag: vec![],
                last_refuted_warn: None,
                clock: Arc::new(Instant::now),
                progress: vec![],
                inflated: None,
            })),
        }
    }

    /// Sets the health watch period to `every` and the checks in a row that make a node behind to
    /// `lag_checks`.
    ///
    /// This is for tests. The defaults are [`WATCH_EVERY`] and [`LAG_CHECKS`].
    pub fn with_watch(self, every: Duration, lag_checks: u32) -> Self {
        {
            let mut w = self.watch.lock().unwrap_or_else(|e| e.into_inner());
            (w.every, w.lag_checks) = (every, lag_checks.max(1));
        }
        self
    }

    /// Sets the local monotonic clock that the health watch measures a node's stillness on.
    ///
    /// This is for tests. The default is `Instant::now`.
    pub fn with_clock(self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.watch.lock().unwrap_or_else(|e| e.into_inner()).clock = Arc::new(clock);
        self
    }

    /// Starts the health watch (module documentation) on a task of its own when it is due.
    ///
    /// The follower's request never waits for it. A node that hangs costs the watch its timeout,
    /// not the book. When a chain request of the authority was made, it also starts a probe of the
    /// demoted nodes from its cursor (`probe_demoted`). Only one probe runs at a time.
    fn health_watch(&self) {
        let due = {
            let mut w = self.watch.lock().unwrap_or_else(|e| e.into_inner());
            let due = self.nodes.len() > 1 && w.last.is_none_or(|t| t.elapsed() >= w.every);
            if due {
                w.last = Some(Instant::now());
            }
            due
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        if due {
            let (nodes, verifier, board, watch) = (self.nodes.clone(), self.verifier.clone(), self.board.clone(), self.watch.clone());
            rt.spawn(async move { watch_nodes(&nodes, &verifier, &board, &watch).await });
            let at = *self.last_start.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(at) = at.filter(|_| self.probing.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_ok()) {
                let (nodes, verifier, board, probing) =
                    (self.nodes.clone(), self.verifier.clone(), self.board.clone(), self.probing.clone());
                rt.spawn(async move {
                    probe_demoted(&nodes, &verifier, &board, at).await;
                    probing.store(false, Ordering::Release);
                });
            }
        }
    }

    /// Runs one health check of every node now, on the caller's task.
    ///
    /// This is for tests. The follower's requests start the check on a task of its own every
    /// [`WATCH_EVERY`]. It also probes the demoted nodes ([`MultiNode::authority_failing`]).
    pub async fn check_health(&self) {
        watch_nodes(&self.nodes, &self.verifier, &self.board, &self.watch).await;
        let at = *self.last_start.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = at {
            probe_demoted(&self.nodes, &self.verifier, &self.board, at).await;
        }
    }

    /// The chain authority now.
    ///
    /// It is node 0, the configured primary, unless another node was promoted.
    fn primary(&self) -> &N {
        &self.nodes[self.board.authority()]
    }

    /// The authority on connections of its own, for the acceptance data and its copies of chain
    /// blocks.
    ///
    /// For node 0 that is the verifier. A promoted node has no second client, so its own client is
    /// used.
    fn checker(&self) -> &N {
        match self.board.authority() {
            0 => &self.verifier,
            a => &self.nodes[a],
        }
    }

    /// Remembers `start` as the cursor the authority was last asked from.
    fn asked_from(&self, start: Hash32) {
        *self.last_start.lock().unwrap_or_else(|e| e.into_inner()) = Some(start);
    }

    /// The authority is unproven from the cursor `at` from now on ([`MultiNode::settle`]).
    fn settle_from(&self, at: Hash32) {
        self.settling.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(at);
    }

    /// Checks the authority's answer `r` from `start`, a single step (`step`) or the chain hashes.
    ///
    /// While the authority is settling, after a node took over ([`MultiNode::authority_failing`])
    /// or the authority's resolver entry moved to another node, an answer is refused unless it
    /// removes at most [`SETTLE_MAX_REORG`] chain blocks and the node knows the cursor. Otherwise
    /// the follower would apply a deep reorg the old authority never saw, or rewind the store to
    /// the newest block the node knows. A refused answer applies nothing.
    ///
    /// Every answer is checked until the follower takes a single step from another cursor than the
    /// one the authority took over at. The cursor moved on then, through answers that passed. Any
    /// successful answer ends a chain alarm of the authority (`judge::chain_ok`).
    fn settle<T>(&self, start: Hash32, r: Result<T, RpcError>, removed: impl Fn(&T) -> usize, step: bool) -> Result<T, RpcError> {
        let a = self.board.authority();
        let node = self.board.node_of(a);
        if let Some(node) = node {
            if r.is_ok() {
                super::judge::chain_ok(self.board.entry(a), &node);
            }
            let changed = {
                let mut seen = self.authority_node.lock().unwrap_or_else(|e| e.into_inner());
                let changed = seen.as_ref().is_some_and(|s| *s != node);
                *seen = Some(node);
                changed
            };
            if changed {
                // The follower's cursor is where its last single step began.
                let at = if step { start } else { self.last_step.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or(start) };
                self.settle_from(at);
            }
        }
        let Some(at) = *self.settling.lock().unwrap_or_else(|e| e.into_inner()) else { return r };
        if step && start != at {
            *self.settling.lock().unwrap_or_else(|e| e.into_inner()) = None;
            tracing::info!(authority = %self.board.shown(a), cursor = %start, "the new chain authority continues the cursor");
            return r;
        }
        let refuse = match &r {
            Ok(v) if removed(v) > SETTLE_MAX_REORG => Some(format!(
                "it removes {} chain blocks at the cursor {start} (at most {SETTLE_MAX_REORG} right after it took over)",
                removed(v)
            )),
            Err(e) if matches!(e.kind(), super::NodeErrorKind::UnknownHash | super::NodeErrorKind::RetentionRootMissing) => {
                Some(format!("it disowns the cursor {start} (that would rewind or halt the store)"))
            }
            _ => None,
        };
        if let Some(why) = refuse {
            tracing::error!(authority = %self.board.shown(a), reason = %why, "the new chain authority's answer is refused, nothing is applied");
            return Err(RpcError::Node(format!("the new chain authority {}: {why}; not applied", self.board.shown(a))));
        }
        r
    }

    /// The nodes that may become the chain authority instead of node `a`, in the order of the list.
    ///
    /// A node qualifies when all of these hold.
    ///
    /// - The configuration lets it lead ([`NodeSpec::may_lead`]).
    /// - It has a node. A resolver entry that is being replaced has none.
    /// - It is not dropped.
    /// - It has no alarm (`rpc::judge`). An alarm can mean another network, not synced, hanging,
    ///   behind, frozen, a contradiction or failing chain requests.
    /// - It is not suspect, so it was not behind at the last health check.
    /// - It did not contradict the authority within [`PROMOTE_CLEAN_FOR`]. Only a cross-checking
    ///   node can have done so.
    fn candidates(&self, a: usize) -> Vec<usize> {
        let now = Instant::now();
        (0..self.nodes.len())
            .filter(|&i| i != a && self.specs[i].may_lead)
            .filter(|&i| {
                let entry = self.board.entry(i);
                let Some(node) = self.board.node_of(i) else { return false };
                let contradicted =
                    self.board.with(i, |p| p.last_disagreement.is_some_and(|t| now.duration_since(t) < PROMOTE_CLEAN_FOR));
                !self.board.dropped(i) && !contradicted && super::judge::alarm_of(entry).is_none() && !super::judge::suspect(&node)
            })
            .collect()
    }

    /// Whether node `k` can be the chain authority from the cursor `at`.
    ///
    /// It has [`PROMOTE_CHECK_TIMEOUT`] to show all of these.
    ///
    /// - It is on the right network and synced.
    /// - It has a UTXO index, when the role needs one (`judge::require_utxo_index`). The
    ///   authority's node answers the UTXO queries ([`ChainSource::utxos_by_addresses`]).
    /// - It knows the cursor.
    /// - The cursor is on its selected chain, so its chain hashes from it remove nothing.
    ///
    /// The stored chain is the cursor's own chain of selected parents, fixed by the hashes of its
    /// headers. So such a node agrees with everything the store holds.
    async fn can_lead(&self, k: usize, at: Hash32) -> Result<(), String> {
        let node = &*self.nodes[k];
        let check = async {
            let info = node.server_info().await.map_err(|e| format!("getServerInfo: {e}"))?;
            let network = super::resolver::network();
            if !network.is_empty() && info.network_id != network {
                return Err(format!("it is on {}, not {network}", info.network_id));
            }
            if !info.is_synced {
                return Err("it is not synced".into());
            }
            if super::judge::utxo_index_required() && !info.has_utxo_index {
                return Err("it has no UTXO index (the Executor reads its funds there)".into());
            }
            if !node.block_exists(at).await.map_err(|e| format!("getBlock: {e}"))? {
                return Err(format!("it does not know the cursor {at}"));
            }
            let c = node.chain_hashes(at).await.map_err(|e| format!("its chain from the cursor: {e}"))?;
            if !c.removed_chain_block_hashes.is_empty() {
                return Err(format!(
                    "the cursor {at} is not on its selected chain (it removes {} chain blocks there)",
                    c.removed_chain_block_hashes.len()
                ));
            }
            Ok(())
        };
        match tokio::time::timeout(PROMOTE_CHECK_TIMEOUT, check).await {
            Ok(r) => r,
            Err(_) => Err(format!("no answer within {} s", PROMOTE_CHECK_TIMEOUT.as_secs())),
        }
    }

    /// The node that vouches for the authority's answers now.
    ///
    /// It is the first cross-checking node that is not dropped, not cut off and not backing off.
    fn cross_checker(&self) -> Option<usize> {
        let now = Instant::now();
        let a = self.board.authority();
        (0..self.nodes.len()).filter(|&i| i != a).find(|&i| {
            let entry = &self.specs[i].url;
            // A resolver entry being replaced has no node to ask. The authority goes on alone
            // meanwhile and never waits for it.
            let replacing = super::resolver::is_resolver(entry) && super::resolver::current(entry).is_none();
            self.specs[i].cross_check
                && !replacing
                && !self.board.dropped(i)
                && !self.board.cut(i)
                && !self.board.with(i, |p| p.backoff_until.is_some_and(|t| t > now))
        })
    }

    /// The cross-checking node's acceptance data from `start`.
    ///
    /// It is `None` when there is no such node now or it could not answer within `limit`. The
    /// authority goes on alone then, and the node backs off. A timeout counts against the node
    /// (`rpc::judge`). The node is asked while the authority answers the same request, so it costs
    /// no extra round trip.
    async fn cross_ids(
        &self,
        j: Option<usize>,
        start: Hash32,
        min_confirmations: Option<u64>,
        limit: Duration,
    ) -> Option<(usize, ChainIds)> {
        let j = j?;
        match tokio::time::timeout(limit, self.nodes[j].chain_with_ids(start, min_confirmations)).await {
            Ok(Ok(ids)) => Some((j, ids)),
            Ok(Err(e)) => {
                self.board.failed(j, Fault::Failed, &format!("cross-check: {e}"));
                None
            }
            Err(_) => {
                self.board.failed(j, Fault::Failed, &format!("timeout: cross-check after {limit:?}"));
                if let Some(node) = self.board.node_of(j) {
                    super::judge::timed_out(self.board.entry(j), &node);
                }
                None
            }
        }
    }

    /// A window from the authority, vouched for by the cross-checking node.
    ///
    /// `node` is the authority's own client or the verifier. The cross-checking node is asked for
    /// the same range at the same time. The window's bounds are read from the authority first, as
    /// `fetch_window_from` would. A successful window ends a chain alarm of the authority.
    async fn primary_window(&self, node: &N, w: &WindowRequest) -> Result<Fetched, RpcError> {
        let a = self.board.authority();
        let origin = Origin { node: a, ..Origin::PRIMARY };
        let r = self.primary_window_from(node, w, origin).await;
        if r.is_ok() {
            if let Some(n) = self.board.node_of(a) {
                super::judge::chain_ok(self.board.entry(a), &n);
            }
        }
        r
    }

    async fn primary_window_from(&self, node: &N, w: &WindowRequest, origin: Origin) -> Result<Fetched, RpcError> {
        let Some(j) = self.cross_checker() else { return fetch_window_from(node, w, Verbosity::High, origin).await };
        let end_blue = match w.end_blue {
            Some(b) => b,
            None => node
                .block_blue_score(w.end)
                .await?
                .ok_or_else(|| RpcError::Node(format!("prefetch: the node no longer knows the window end {}", w.end)))?,
        };
        let sink_blue = match w.sink_blue {
            Some(b) => b,
            None => node.sink_blue_score().await?,
        };
        let mine = WindowRequest { end_blue: Some(end_blue), sink_blue: Some(sink_blue), ..*w };
        let min = window_min_confirmations(sink_blue, end_blue, w.min_confirmations);
        let (f, cross) = tokio::join!(
            fetch_window_from(node, &mine, Verbosity::High, origin),
            self.cross_ids(Some(j), w.start, min, CROSS_CHECK_TIMEOUT)
        );
        let Fetched { raw, sink_blue, end_blue, elapsed, origin } = f?;
        Ok(Fetched { raw: self.vouched(raw, cross)?, sink_blue, end_blue, elapsed, origin })
    }

    /// The authority's answer `raw` from `start`, as far as the cross-checking node vouches for it.
    ///
    /// It is returned whole when nothing is contradicted or no such node can answer now. It is cut
    /// before the first contradiction. It is an error when the next chain block itself is
    /// contradicted (module documentation).
    fn vouched(&self, mut raw: RawVspcResponse, cross: Option<(usize, ChainIds)>) -> Result<RawVspcResponse, RpcError> {
        let Some((j, ids)) = cross else { return Ok(raw) };
        let a = super::verify::agreement(&raw, &ids);
        let Some(why) = a.contradiction else {
            self.stalls.store(0, std::sync::atomic::Ordering::Relaxed);
            return Ok(raw);
        };
        self.board.disagreed(j, &why, a.at_tip);
        let total = raw.added_chain_block_hashes.len();
        let log = {
            let mut l = self.last_warn.lock().unwrap_or_else(|e| e.into_inner());
            let due = l.is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
            if due {
                *l = Some(Instant::now());
            }
            due
        };
        if a.at_tip {
            // The tips differ for a moment and are not agreed yet. This is routine on a public
            // pair. A tip that never settles ends in replacing a node below, which is warned about.
            tracing::debug!(applied = a.keep, of = total, reason = %why, "the two nodes' tips differ: only the chain blocks both report are indexed");
        } else if log {
            tracing::warn!(
                authority = %self.board.shown(0),
                cross_check = %self.board.shown(j),
                applied = a.keep,
                of = total,
                reason = %why,
                "the two nodes disagree: only the chain blocks both report are indexed"
            );
        }
        if a.keep > 0 {
            self.stalls.store(0, std::sync::atomic::Ordering::Relaxed);
            cut(&mut raw, a.keep);
            return Ok(raw);
        }
        self.stalled(j);
        if a.at_tip {
            // The follower logs this at debug until it repeats `STALLS_BEFORE_REPLACING` times in a
            // row.
            return Err(RpcError::Node(format!("{TIPS_DIFFER} ({why}): nothing is indexed until they agree")));
        }
        Err(RpcError::Node(format!("the nodes disagree on the next chain block ({why}): nothing is indexed until they agree")))
    }

    /// A contradiction with the cross-checking node `j` left nothing to apply.
    ///
    /// A disagreement that does not settle replaces the cross-checking node after
    /// [`STALLS_BEFORE_REPLACING`] in a row, then the authority's after as many more. This applies
    /// to resolver entries only.
    fn stalled(&self, j: usize) {
        let n = self.stalls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let give_up = |i: usize| {
            let e = self.board.entry(i).to_string();
            if let Some(node) = super::resolver::current(&e) {
                tracing::warn!(entry = %e, node = %super::resolver::host(&node), stalls = n, "a disagreement that does not settle: replacing this node");
                super::resolver::replace(&e, &node, "it disagreed with the other node and the disagreement did not settle");
            }
        };
        if n == STALLS_BEFORE_REPLACING {
            give_up(j);
        } else if n >= 2 * STALLS_BEFORE_REPLACING {
            give_up(self.board.authority());
            self.stalls.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The configured cross-checking node, whatever its state now, if the list has one.
    ///
    /// It is the second node of a resolver pair. Unlike [`MultiNode::cross_checker`], a node that
    /// backs off or is being replaced still counts: what it would have to confirm must wait for it.
    fn cross_node(&self) -> Option<usize> {
        let a = self.board.authority();
        (0..self.nodes.len()).find(|&i| i != a && self.specs[i].cross_check)
    }

    /// The shared per-node state.
    pub fn board(&self) -> Arc<NodeBoard> {
        self.board.clone()
    }

    /// The specs of the nodes.
    pub fn specs(&self) -> &[NodeSpec] {
        &self.specs
    }

    /// Windows the source fetches at once: the connections of every fetching node.
    pub fn fetch_connections(&self) -> usize {
        self.specs.iter().filter(|s| s.fetch).map(|s| s.connections.max(1)).sum::<usize>().max(1)
    }

    /// The node for the next attempt at a window.
    ///
    /// It is chosen among the nodes not tried yet for this window that serve windows and are not
    /// dropped, cut off or backing off. The authority always qualifies. The choice prefers, in this
    /// order, a node that is not slow, then one with a free connection, then the best rate.
    ///
    /// A node is slow when its rate is below a quarter of the best one's ([`SLOW_FACTOR`]). An
    /// unmeasured node counts as fast. A node idle for five minutes is measured again
    /// ([`REMEASURE`]).
    fn pick(&self, tried: &[bool]) -> Option<usize> {
        let now = Instant::now();
        let n = self.nodes.len();
        let first = self.rr.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut cands: Vec<(usize, bool, Option<f64>)> = vec![];
        // An authority that does not serve windows still takes one that no other node can.
        let a = self.board.authority();
        let fallback = !tried[a] && !self.specs[a].fetch;
        for k in 0..n {
            let i = (first + k) % n;
            if tried[i] || !self.specs[i].fetch || self.board.dropped(i) || self.board.cut(i) {
                continue;
            }
            let (backing_off, rate) = self.board.with(i, |p| {
                let stale = p.last_used.is_some_and(|t| now.saturating_duration_since(t) > REMEASURE);
                (p.backoff_until.is_some_and(|t| t > now), p.ewma_bps.filter(|_| !stale))
            });
            if i != a && backing_off {
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
        best.map(|c| c.0).or(fallback.then_some(a))
    }

    /// The authority's sink blue score, at most a second old.
    ///
    /// `at_least` makes it refresh when the cached value is not above it. The value only bounds
    /// requests for the authority's acceptance data. A stale sink makes the answer a few chain
    /// blocks longer and nothing else.
    async fn primary_sink(&self, at_least: u64) -> Result<u64, RpcError> {
        let cached = *self.sink.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, b)) = cached.filter(|(t, b)| t.elapsed() < Duration::from_secs(1) && *b > at_least) {
            return Ok(b);
        }
        let b = self.checker().sink_blue_score().await?;
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), b));
        Ok(b)
    }

    /// The window from secondary `i`, checked against the authority.
    ///
    /// The node's own position is read first, and it must know the window end. Then its answer and
    /// the authority's acceptance data for the same start are fetched at once.
    ///
    /// The node's blue scores bound only its own answer. The authority's acceptance data is
    /// bounded by the authority's own blue score of the window end and its own sink. A node that
    /// reported a huge blue score would otherwise make that request unbounded, a whole backlog, and
    /// its timeout would count against the authority (`indexer::follower`, *A failing chain
    /// authority*).
    async fn fetch_checked(&self, i: usize, w: &WindowRequest) -> Result<Fetched, Attempt> {
        let origin = Origin { node: i, trusted: false, tier: 0 };
        let node = &*self.nodes[i];
        let classify = |e: RpcError| match e {
            // Too large for the window's own limit: the window is split, whichever node serves it.
            e @ RpcError::TooLarge { size, .. } if w.max_bytes.is_some() && size <= super::MAX_MESSAGE_BYTES => Attempt::Window(e),
            // Beyond what the client takes at all. No window of an honest node is that large, so the
            // node failed it: it backs off and the window goes to the next node. Taken as the window's
            // size, the claim would split the window down to one chain block, ask for it without a
            // limit and offer it to the same node again, forever.
            e @ RpcError::TooLarge { .. } => Attempt::Node(e, Fault::Failed),
            e @ RpcError::Node(_) => Attempt::Node(e, Fault::OutOfSync),
            e => Attempt::Node(e, Fault::Failed),
        };
        let (end_blue, own_end) = tokio::join!(node.block_blue_score(w.end), self.checker().block_blue_score(w.end));
        let end_blue = end_blue.map_err(classify)?.ok_or_else(|| {
            Attempt::Node(RpcError::Node(format!("the node does not know the window end {} (behind)", w.end)), Fault::OutOfSync)
        })?;
        // The authority planned the window, so it knows its end unless its chain moved since.
        let own_end = own_end
            .map_err(Attempt::Window)?
            .ok_or_else(|| Attempt::Window(RpcError::Node(format!("the window end {} is no longer known to the primary", w.end))))?;
        let sink_blue = node.sink_blue_score().await.map_err(classify)?;
        let mine = WindowRequest { end_blue: Some(end_blue), sink_blue: Some(sink_blue), ..*w };
        let primary_sink = self.primary_sink(own_end).await.map_err(Attempt::Window)?;
        let ids = self.checker().chain_with_ids(w.start, window_min_confirmations(primary_sink, own_end, w.min_confirmations));
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

/// One check of the health watch (module documentation).
///
/// Every node's `getServerInfo` is asked at once and judged. Then each node's DAA is compared with
/// the most advanced answer. A lag counts only once the node behind confirmed it.
async fn watch_nodes<N: ChainSource>(nodes: &[Arc<N>], verifier: &Arc<N>, board: &NodeBoard, watch: &Mutex<Watch>) {
    let lock = || watch.lock().unwrap_or_else(|e| e.into_inner());
    // One check at a time, so a slow one never overlaps the next.
    {
        let mut w = lock();
        if w.busy {
            return;
        }
        w.busy = true;
    }
    struct Done<'a>(&'a Mutex<Watch>);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
        }
    }
    let _done = Done(watch);
    let n = nodes.len();
    let network = super::resolver::network();
    let network = network.as_str();
    // Node 0 on the verifier's connections, so it is never behind its own windows.
    let src = |i: usize| -> &N {
        if i == 0 {
            verifier
        } else {
            &nodes[i]
        }
    };
    let ask = |i: usize| async move {
        // A resolver entry being replaced has no node to ask.
        let node = board.node_of(i)?;
        let entry = board.entry(i);
        match tokio::time::timeout(WATCH_TIMEOUT, src(i).server_info()).await {
            Ok(Ok(info)) => {
                let judged = super::judge::server_info(entry, &node, &info.network_id, info.is_synced, info.has_utxo_index);
                // The DAA of another network says nothing about this one's.
                let comparable = network.is_empty() || info.network_id == network;
                judged.ok().filter(|()| comparable)?;
                // Its sink, to see whether it advances. This is the DAG, not a number the node
                // reports.
                let sink = tokio::time::timeout(WATCH_TIMEOUT, src(i).dag_info()).await.ok().and_then(Result::ok).map(|d| d.sink);
                Some(((i, node, info.virtual_daa_score), sink))
            }
            // A failed call is reported where it failed, in the client's connection and the judge.
            Ok(Err(_)) => None,
            Err(_) => {
                super::judge::timed_out(entry, &node);
                None
            }
        }
    };
    let asked: Vec<((usize, String, u64), Option<Hash32>)> =
        futures_util::future::join_all((0..n).map(ask)).await.into_iter().flatten().collect();
    let answers: Vec<(usize, String, u64)> = asked.iter().map(|a| a.0.clone()).collect();
    // A node whose sink stands still on the local monotonic clock while another node's advances has
    // stopped advancing, whatever it reports. The limit is longer when the node is alone.
    let still = {
        let mut w = lock();
        w.progress.resize(n, None);
        let now = (w.clock)();
        for ((i, node, _), sink) in &asked {
            let Some(sink) = sink else { continue };
            let same_node = w.progress[*i].as_ref().is_some_and(|p| p.node == *node);
            let changed = !same_node || w.progress[*i].as_ref().is_some_and(|p| p.sink != *sink);
            if changed {
                w.progress[*i] = Some(Progress { node: node.clone(), sink: *sink, since: now, reported: false, moved: same_node });
            }
        }
        let still_for = |p: &Progress| now.saturating_duration_since(p.since);
        let alone = asked.len() == 1;
        let mut out = vec![];
        for ((i, node, _), _) in &asked {
            // Another node of the list, as it answers now, whose sink was seen to change lately.
            let others_advance = asked.iter().any(|((j, other, _), _)| {
                j != i && w.progress[*j].as_ref().is_some_and(|p| p.node == *other && p.moved && still_for(p) < FROZEN_AFTER)
            });
            let limit = if alone {
                Some(FROZEN_ALONE_AFTER)
            } else if others_advance {
                Some(FROZEN_AFTER)
            } else {
                None
            };
            if let Some(p) = w.progress[*i].as_mut().filter(|p| p.node == *node) {
                let s = now.saturating_duration_since(p.since);
                if p.reported {
                    // This is still the sink it was found frozen at. The finding stands and is
                    // asserted again.
                    out.push((*i, node.clone(), s.as_secs(), alone, true));
                } else if limit.is_some_and(|l| s >= l) {
                    p.reported = true;
                    out.push((*i, node.clone(), s.as_secs(), alone, false));
                }
            }
        }
        out
    };
    // The nodes found frozen and still frozen. No fine answer of this check ends their finding.
    let frozen: Vec<usize> = still.iter().map(|s| s.0).collect();
    for (i, node, secs, alone, again) in still {
        let why = if again {
            format!("its sink has not changed for {secs} s (it stopped advancing)")
        } else if alone {
            format!("its sink has not changed for {secs} s and no other node answers (it stopped advancing)")
        } else {
            format!("its sink has not changed for {secs} s while the other node's advances (it stopped advancing)")
        };
        if !again {
            tracing::warn!(node = %board.shown(i), reason = %why, "a node stopped advancing");
        }
        super::judge::frozen(board.entry(i), &node, &why);
    }
    let Some(&(top_i, _, top)) = answers.iter().max_by_key(|a| a.2).filter(|_| answers.len() > 1) else { return };
    let behind_of = |daa: u64| Some(top.saturating_sub(daa)).filter(|b| *b > LAG_DAA);
    // A DAA is the node's own claim, and a node that over-reports would put its honest partner
    // behind. So a lag counts only once the node behind confirms it. The most advanced node's sink
    // at the check must become known to it, and sit that far above its own DAA. A sink it never
    // learns was not real.
    // The nodes that take a claim at this check.
    let wanting: Vec<usize> = {
        let mut w = lock();
        w.lag.resize(n, LagTrack::default());
        let checks = w.lag_checks;
        answers
            .iter()
            .filter(|(i, node, daa)| {
                let t = &mut w.lag[*i];
                // The entry stands for another node now. What was claimed about the last one says
                // nothing about it.
                if t.node != *node {
                    *t = LagTrack { node: node.clone(), ..LagTrack::default() };
                }
                t.observe(behind_of(*daa), checks)
            })
            .map(|a| a.0)
            .collect()
    };
    let lagging: Vec<usize> = answers.iter().filter(|(i, _, daa)| *i != top_i && behind_of(*daa).is_some()).map(|a| a.0).collect();
    let daa_of = |r: Result<Result<Option<u64>, RpcError>, tokio::time::error::Elapsed>| r.ok().and_then(Result::ok).flatten();
    // While a node is behind, the most advanced node's sink gives the claims. Its DAA score is read
    // as that node reports it and as the first node behind does, which places it in its own DAG.
    let sink = match lagging.is_empty() {
        false => match tokio::time::timeout(WATCH_TIMEOUT, src(top_i).dag_info()).await {
            Ok(Ok(d)) => Some(d.sink),
            _ => None, // no claim this check: taken at the next
        },
        true => None,
    };
    let (own_view, partner_view) = match (sink, lagging.first()) {
        (Some(s), Some(&j)) => (
            daa_of(tokio::time::timeout(WATCH_TIMEOUT, src(top_i).block_daa_score(s)).await),
            daa_of(tokio::time::timeout(WATCH_TIMEOUT, src(j).block_daa_score(s)).await),
        ),
        _ => (None, None),
    };
    let pending: Vec<(usize, Vec<(usize, Hash32)>)> = {
        let mut w = lock();
        lagging
            .iter()
            .map(|&i| {
                let own = answers.iter().find(|a| a.0 == i).map(|a| a.2).unwrap_or(0);
                let t = &mut w.lag[i];
                if let Some(sink) = sink.filter(|_| wanting.contains(&i)) {
                    t.claims.push(Claim { sink, own, age: 0, lead: None });
                }
                (i, t.claims.iter().enumerate().filter(|(_, c)| c.lead.is_none()).map(|(k, c)| (k, c.sink)).collect())
            })
            .collect()
    };
    let mut learned = vec![];
    for (i, claims) in pending {
        for (k, sink) in claims {
            match tokio::time::timeout(WATCH_TIMEOUT, src(i).block_daa_score(sink)).await {
                Ok(Ok(Some(daa))) => learned.push((i, k, daa)),
                // Not known yet, and a later claim's sink is not either, or no answer. It is asked
                // again at the next check.
                _ => break,
            }
        }
    }
    let top_node = answers.iter().find(|a| a.0 == top_i).map(|a| a.1.clone()).unwrap_or_default();
    let mut verdicts = vec![];
    let (refuted_warn, inflated) = {
        let mut w = lock();
        let checks = w.lag_checks;
        for (i, k, daa) in learned {
            if let Some(c) = w.lag[i].claims.get_mut(k) {
                c.lead = Some(daa.saturating_sub(c.own));
            }
        }
        for (i, node, daa) in answers {
            let v = if i == top_i { LagVerdict::Level } else { w.lag[i].verdict(behind_of(daa), checks) };
            let evidence = i != top_i && w.lag[i].evidence();
            verdicts.push((i, node, v, evidence));
        }
        // The lead is not backed by the DAG when the most advanced node's own sink is far below the
        // virtual DAA score it reports. That sink is a block the node behind knows too, and both
        // report its DAA.
        let backed = |d: Option<u64>| d.is_some_and(|d| top <= d.saturating_add(LAG_DAA));
        let over = match (own_view, partner_view) {
            (Some(d), Some(_)) if !backed(own_view) && !backed(partner_view) => Some(d),
            _ => None,
        };
        let inflated = match over {
            Some(d) => {
                let n = match &w.inflated {
                    Some((node, n)) if *node == top_node => n + 1,
                    _ => 1,
                };
                w.inflated = Some((top_node.clone(), n));
                (n >= INFLATED_CHECKS).then(|| {
                    w.inflated = None;
                    d
                })
            }
            None => {
                w.inflated = None;
                None
            }
        };
        let refuted = verdicts.iter().any(|v| matches!(v.2, LagVerdict::Refuted(_)));
        let due = w.last_refuted_warn.is_none_or(|t| t.elapsed() >= Duration::from_secs(600));
        if refuted && due {
            w.last_refuted_warn = Some(Instant::now());
        }
        (refuted && due, inflated)
    };
    if let Some(d) = inflated {
        let why = format!(
            "it reports a virtual DAA score {top}, {} above its own sink's ({d}), a block the other node places in the DAG too: a lead it does not have",
            top - d
        );
        tracing::warn!(node = %board.shown(top_i), reason = %why, "a node claims a lead the DAG does not back");
        super::judge::contradicted(board.entry(top_i), &top_node, &why);
    }
    for (i, node, v, evidence) in verdicts {
        if frozen.contains(&i) {
            // Judged frozen above. A lag verdict, fine, refuted or unconfirmed, must not end or
            // soften that.
            continue;
        }
        let entry = board.entry(i);
        match v {
            LagVerdict::Level => super::judge::lag(entry, &node, None, None),
            // Behind by the other node's claim only. The node is not suspect until it has evidence
            // of its own.
            LagVerdict::Unconfirmed(d) if evidence => super::judge::lag(entry, &node, Some(d), None),
            LagVerdict::Unconfirmed(_) => super::judge::unconfirmed(&node),
            LagVerdict::Refuted(d) => {
                if refuted_warn {
                    tracing::warn!(
                        node = %board.shown(i),
                        ahead = %board.shown(top_i),
                        claimed_daa = d,
                        "a node's lead over the other node of the pair is not borne out (its sink stayed unknown, or was not that far ahead): nothing is held against the node behind"
                    );
                }
                super::judge::lag(entry, &node, None, None)
            }
            LagVerdict::Confirmed(d) => {
                tracing::warn!(node = %board.shown(i), behind_daa = d, "a node stays behind the other node of the pair (confirmed by the node itself)");
                super::judge::lag(entry, &node, Some(d), Some(d))
            }
        }
    }
}

/// Keeps the first `n` chain blocks of a window.
///
/// `wire_bytes` shrinks in proportion to the chain blocks and transactions kept, for the size
/// accounting of the prefetch.
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
        let (start, min_confirmations) = (req.start_hash, req.min_confirmation_count);
        let j = self.cross_checker();
        self.health_watch();
        self.asked_from(start);
        *self.last_step.lock().unwrap_or_else(|e| e.into_inner()) = Some(start);
        let (raw, cross) =
            tokio::join!(self.primary().vspc_v2(req), self.cross_ids(j, start, min_confirmations, CROSS_CHECK_STEP_TIMEOUT));
        let raw = self.settle(start, raw, |r| r.removed_chain_block_hashes.len(), true)?;
        self.vouched(raw, cross)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.primary().dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.primary().server_info().await
    }
    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.primary().utxos_by_addresses(addresses).await
    }
    /// Whether the authority knows the block.
    ///
    /// With a cross-checking node, "unknown" counts only when that node does not know the block
    /// either. The follower's walk back rewinds the store on such answers. Any other outcome is an
    /// error that rewinds nothing.
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        if self.primary().block_exists(hash).await? {
            return Ok(true);
        }
        let Some(j) = self.cross_node() else { return Ok(false) };
        let other = self.board.shown(j);
        match tokio::time::timeout(CROSS_CHECK_STEP_TIMEOUT, self.nodes[j].block_exists(hash)).await {
            Ok(Ok(false)) => Ok(false),
            Ok(Ok(true)) => Err(RpcError::Node(format!(
                "the chain authority does not know the block {hash}, but the cross-checking node {other} does"
            ))),
            Ok(Err(e)) => {
                Err(RpcError::Node(format!("the cross-checking node {other} could not say whether it knows the block {hash}: {e}")))
            }
            Err(_) => Err(RpcError::Node(format!(
                "the cross-checking node {other} did not say within {} s whether it knows the block {hash}",
                CROSS_CHECK_STEP_TIMEOUT.as_secs()
            ))),
        }
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.primary().sink_blue_score().await
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.primary().block_blue_score(hash).await
    }
    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.primary().block_daa_score(hash).await
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.asked_from(start);
        let r = self.primary().chain_hashes(start).await;
        self.settle(start, r, |c| c.removed_chain_block_hashes.len(), false)
    }
    async fn chain_with_ids(&self, start: Hash32, min_confirmations: Option<u64>) -> Result<ChainIds, RpcError> {
        self.checker().chain_with_ids(start, min_confirmations).await
    }

    async fn fetch_window(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        // The health watch runs while the cursor catches up too. It is due once per period,
        // whichever window asks first.
        self.health_watch();
        self.fetch_any(w).await
    }

    async fn fetch_window_primary(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        // On connections of their own (the checker), so it never waits behind the authority's
        // windows of the parallel fetch.
        self.primary_window(self.checker(), &w).await
    }

    fn report_lie(&self, origin: Origin, why: &str) {
        self.board.drop_node(origin.node, why);
    }

    /// Handles a public chain authority that contradicts the store at the cursor `at`.
    ///
    /// A resolver entry's node is given up for another (`rpc::judge`), and the follower asks the
    /// next one. A second authority on another host may contradict the store at the same cursor, or
    /// [`STORE_CONTRADICTIONS_MAX`] contradictions may happen there. The nodes then agree with each
    /// other against the store, so the cause is the store, such as a corrupted cursor or a bug. The
    /// store halts there (`gap`) instead of giving up public node after public node. A node named
    /// in the configuration is not replaced, and the store halts at once.
    fn authority_contradicted(&self, at: Hash32, why: &str) -> super::StoreContradiction {
        let a = self.board.authority();
        let entry = self.board.entry(a).to_string();
        let Some(node) = self.board.node_of(a).filter(|_| super::resolver::is_resolver(&entry)) else {
            return super::StoreContradiction::Halt(None);
        };
        let host = super::resolver::host(&node);
        let (hosts, times) = {
            let mut sc = self.store_contradictions.lock().unwrap_or_else(|e| e.into_inner());
            let rec = match sc.as_mut().filter(|(c, _, _)| *c == at) {
                Some(rec) => rec,
                None => sc.insert((at, vec![], 0)),
            };
            if !rec.1.contains(&host) {
                rec.1.push(host.clone());
            }
            rec.2 += 1;
            (rec.1.clone(), rec.2)
        };
        if hosts.len() >= 2 || times >= STORE_CONTRADICTIONS_MAX {
            let why = format!(
                "the public nodes {} contradicted the store {times} times at its cursor {at}, agreeing with each other against it: the store is at fault (a corrupted cursor or a kobd bug), not the nodes",
                hosts.join(", ")
            );
            tracing::error!(cursor = %at, hosts = %hosts.join(", "), "public nodes on different hosts agree with each other against the store: halting");
            return super::StoreContradiction::Halt(Some(why));
        }
        super::judge::contradicted(&entry, &node, why);
        super::StoreContradiction::Replaced
    }

    /// Hands the chain authority over when its chain requests keep failing.
    ///
    /// The follower calls this when 2 chain requests in a row failed on the wire and the cursor
    /// `at` has not moved for 3 minutes (`why` says what failed, `indexer::follower`). The old
    /// authority gets a chain alarm (`judge::chain_failing`). What follows depends on the node.
    ///
    /// - A resolver entry gives its public node up (`rpc::resolver::replace`). The other node of
    ///   the pair is the authority at once, unless the judge suspects it. The entry resolves a
    ///   replacement on another host.
    /// - Among named nodes, the first of [`MultiNode::candidates`] that passes
    ///   [`MultiNode::can_lead`] becomes the authority, and the roles swap. A node that cannot lead
    ///   is logged and skipped.
    /// - A single node has nothing to promote. The alarm stays and the follower retries with its
    ///   backoff.
    ///
    /// The new authority is settling, so its first answers from the cursor are checked
    /// ([`MultiNode::settle`]). The old authority is a secondary node, cut off from the pair until
    /// its alarm ends. The health watch probes it ([`probe_demoted`]), and the alarm ends when its
    /// chain requests answered for `judge::ALARM_CLEAR_AFTER` without a break. It does not take the
    /// role back then. It leads again only if the new authority fails the same way, so the role
    /// does not flap.
    ///
    /// Returns `true` when another node is the authority now.
    async fn authority_failing(&self, at: Hash32, why: &str) -> bool {
        let a = self.board.authority();
        let entry = self.board.entry(a).to_string();
        // A resolver entry being replaced has no node to give up. It is being replaced already.
        let Some(node) = self.board.node_of(a) else { return false };
        let why = format!("its chain requests keep failing ({why})");
        super::judge::chain_failing(&entry, &node, &why);
        // The pair's authority. The resolver state moved the role to the other node, or resolves a
        // replacement.
        if entry == super::resolver::RESOLVER {
            let moved = super::resolver::current(&entry).as_deref() != Some(node.as_str());
            if moved {
                // The other node of the pair, or a replacement, is unproven from the cursor.
                self.settle_from(at);
            }
            return moved;
        }
        // Whether this resolver entry gave its node up above. It uses another node now.
        let replaced = super::resolver::is_resolver(&entry) && super::resolver::current(&entry).as_deref() != Some(node.as_str());
        let candidates = self.candidates(a);
        if self.nodes.len() > 1 && candidates.is_empty() {
            tracing::warn!(authority = %self.board.shown(a), "the chain authority's chain requests keep failing, and no other node can take over (each is dropped, has an alarm or is behind): staying");
        }
        for k in candidates {
            match self.can_lead(k, at).await {
                Ok(()) => {
                    self.board.authority.store(k, Ordering::Relaxed);
                    self.settle_from(at);
                    self.stalls.store(0, Ordering::Relaxed);
                    // The cached sink was the old authority's.
                    *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    self.board.with(k, |p| {
                        p.status.promoted += 1;
                        p.failures = 0;
                        p.backoff_until = None;
                    });
                    PROMOTIONS.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        from = %self.board.shown(a),
                        to = %self.board.shown(k),
                        cursor = %at,
                        reason = %why,
                        "the chain authority's chain requests keep failing: another node is the chain authority now"
                    );
                    return true;
                }
                Err(e) => {
                    tracing::warn!(authority = %self.board.shown(a), candidate = %self.board.shown(k), reason = %e, "the chain authority's chain requests keep failing, but this node cannot take over")
                }
            }
        }
        if replaced {
            // The entry's next node is unproven, so its answers from the cursor are checked.
            self.settle_from(at);
        }
        replaced
    }

    /// Confirms the authority's claim with the cross-checking node, if the list has one.
    ///
    /// The node is asked for its chain from `at`. It confirms when it does not know `at` or has it
    /// below its retention root too. When the two claims differ, the walk back is acted on, the
    /// milder of the two. When it knows `at` and its chain, that is a contradiction: counted as a
    /// disagreement, and replacing nodes as one that does not settle ([`MultiNode::stalled`]). When
    /// it cannot answer, nothing is confirmed.
    async fn confirm_unknown_cursor(&self, at: Hash32, e: &RpcError) -> Result<super::NodeErrorKind, String> {
        use super::NodeErrorKind::{RetentionRootMissing, UnknownHash};
        let kind = e.kind();
        let Some(j) = self.cross_node() else { return Ok(kind) };
        let other = self.board.shown(j);
        match tokio::time::timeout(CROSS_CHECK_TIMEOUT, self.nodes[j].chain_hashes(at)).await {
            Ok(Err(e2)) if matches!(e2.kind(), UnknownHash | RetentionRootMissing) => {
                Ok(if e2.kind() == kind { kind } else { UnknownHash })
            }
            Ok(Ok(_)) => {
                let why = format!("the cross-checking node {other} knows the cursor {at} and its chain");
                tracing::warn!(
                    authority = %self.board.shown(self.board.authority()),
                    cross_check = %other,
                    cursor = %at,
                    "the chain authority disowns the cursor and the other node does not: nothing is rewound or halted"
                );
                self.board.disagreed(j, &why, false);
                self.stalled(j);
                Err(why)
            }
            Ok(Err(e2)) => Err(format!("the cross-checking node {other} could not confirm it ({e2})")),
            Err(_) => Err(format!("the cross-checking node {other} did not answer within {} s", CROSS_CHECK_TIMEOUT.as_secs())),
        }
    }

    fn node_stats(&self) -> Vec<NodeStatus> {
        if self.nodes.len() > 1 {
            self.board.snapshot()
        } else {
            vec![]
        }
    }
}

/// Probes the demoted nodes with a chain request from the cursor `at`.
///
/// A demoted node is a named node other than the authority with a chain alarm
/// (`judge::chain_failing`). Each is asked for the chain from `at` within
/// [`DEMOTED_PROBE_TIMEOUT`]. An answer is a fine one. The alarm ends after
/// `judge::ALARM_CLEAR_AFTER` of them without a break, and the node is a secondary of the pair
/// again. A failure keeps the alarm.
async fn probe_demoted<N: ChainSource>(nodes: &[Arc<N>], verifier: &Arc<N>, board: &NodeBoard, at: Hash32) {
    let a = board.authority();
    for i in (0..nodes.len()).filter(|&i| i != a) {
        let entry = board.entry(i);
        let Some(node) = board.node_of(i) else { continue };
        if super::resolver::is_resolver(entry) || !super::judge::chain_alarm(entry) {
            continue;
        }
        // Node 0 on the verifier's connections, as in the health watch.
        let src: &N = if i == 0 { verifier } else { &nodes[i] };
        match tokio::time::timeout(DEMOTED_PROBE_TIMEOUT, src.chain_hashes(at)).await {
            Ok(Ok(_)) => super::judge::chain_ok(entry, &node),
            Ok(Err(e)) => super::judge::chain_failing(entry, &node, &format!("its chain requests keep failing (probe: {e})")),
            Err(_) => super::judge::chain_failing(
                entry,
                &node,
                &format!("its chain requests keep failing (probe: no answer within {} s)", DEMOTED_PROBE_TIMEOUT.as_secs()),
            ),
        }
    }
}

impl<N: ChainSource> MultiNode<N> {
    /// [`ChainSource::fetch_window`] without the health watch.
    async fn fetch_any(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        let mut tried = vec![false; self.nodes.len()];
        let mut last: Option<RpcError> = None;
        while let Some(i) = self.pick(&tried) {
            tried[i] = true;
            let a = self.board.authority();
            let permit = self.permits[i].clone().acquire_owned().await.expect("the semaphore is never closed");
            self.board.begin(i);
            let r = if i == a {
                self.primary_window(&*self.nodes[a], &w).await.map_err(Attempt::Window)
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
                    if i == a && !matches!(e, RpcError::TooLarge { .. }) {
                        // The authority is never skipped. The error is only counted.
                        self.board.note_error(a, &e);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{redact_url, StoreContradiction};

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
            // A password with an unencoded '?', '#' or '/'.
            ("wss://user:pa?ss@node.example/x", "wss://node.example/..."),
            ("wss://user:pa#ss@node.example", "wss://node.example"),
            ("wss://user:p/ss@host", "wss://<redacted>/..."),
            ("ws://[::1]:18210/x", "ws://[::1]:18210/..."),
        ] {
            assert_eq!(redact_url(url), shown, "{url}");
        }
    }

    /// A node URL can carry a password or an API key for the connection. The public health and
    /// metrics routes and the logs show only the scheme, host and port.
    #[test]
    fn node_credentials_stay_out_of_health_and_metrics() {
        let secret = "wss://user:hunter2@node.example/wrpc/json?apikey=SECRETKEY";
        let board = NodeBoard::new(&[
            NodeSpec {
                url: "ws://127.0.0.1:18210".into(),
                fetch: true,
                submit: true,
                connections: 1,
                cross_check: false,
                may_lead: true,
            },
            NodeSpec { url: secret.into(), fetch: true, submit: true, connections: 1, cross_check: false, may_lead: true },
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

    /// A lag counts only once the node behind confirmed the lead itself.
    ///
    /// Both claims, the streak's first check and its `checks`-th, must become known to it later,
    /// each more than `LAG_DAA` above its own DAA then. A sink it never learns, or one that is not
    /// that far ahead, refutes the lead and the streak begins again.
    #[test]
    fn a_lag_counts_only_once_the_node_behind_confirms_the_lead() {
        let claim = |own| Claim { sink: Hash32([5; 32]), own, age: 0, lead: None };
        let checks = 3;
        // A real lag has a claim at the first check and at the third, both confirmed later.
        let mut t = LagTrack::default();
        assert!(t.observe(Some(400), checks), "a claim at the streak's first check");
        t.claims.push(claim(100));
        assert_eq!(t.verdict(Some(400), checks), LagVerdict::Unconfirmed(400));
        assert!(!t.observe(Some(400), checks));
        assert_eq!(t.verdict(Some(400), checks), LagVerdict::Unconfirmed(400));
        assert!(t.observe(Some(400), checks), "and at its third");
        t.claims.push(claim(200));
        t.claims[0].lead = Some(450);
        assert_eq!(t.verdict(Some(400), checks), LagVerdict::Unconfirmed(400), "one confirmed is not enough");
        assert!(!t.observe(Some(400), checks));
        t.claims[1].lead = Some(380);
        assert_eq!(t.verdict(Some(400), checks), LagVerdict::Confirmed(380), "the smallest confirmed lead");
        assert_eq!(t.streak, 0, "a finding begins the streak again");
        // A sink the node behind never learns is refuted after LAG_CONFIRM_CHECKS checks and never
        // a finding.
        let mut t = LagTrack::default();
        for k in 0..=LAG_CONFIRM_CHECKS {
            if t.observe(Some(5_000), checks) && t.claims.is_empty() {
                t.claims.push(claim(100));
            }
            let v = t.verdict(Some(5_000), checks);
            let want = if k < LAG_CONFIRM_CHECKS { LagVerdict::Unconfirmed(5_000) } else { LagVerdict::Refuted(5_000) };
            assert_eq!(v, want, "check {k}");
        }
        assert_eq!((t.streak, t.claims.len()), (0, 0));
        // A sink it knows, but not that far ahead of it, is refuted at once.
        assert!(t.observe(Some(5_000), checks));
        t.claims.push(Claim { lead: Some(LAG_DAA), ..claim(100) });
        assert_eq!(t.verdict(Some(5_000), checks), LagVerdict::Refuted(5_000));
        // Level: nothing is kept.
        t.observe(Some(5_000), checks);
        t.claims.push(claim(100));
        assert!(!t.observe(None, checks));
        assert_eq!((t.streak, t.claims.len(), t.verdict(None, checks)), (0, 0, LagVerdict::Level));
        // One check to a finding needs one claim.
        let mut t = LagTrack::default();
        assert!(t.observe(Some(400), 1));
        t.claims.push(Claim { lead: Some(400), ..claim(100) });
        assert_eq!(t.verdict(Some(400), 1), LagVerdict::Confirmed(400));
    }

    /// A node with a fixed answer.
    ///
    /// It reports the primary's chain blocks (`added`, no transactions) and the acceptance data
    /// `ids`.
    struct Fixed {
        added: Vec<Hash32>,
        ids: Mutex<Result<ChainIds, String>>,
    }

    impl Fixed {
        fn new(added: &[u8], ids: Result<&[u8], &str>) -> Arc<Fixed> {
            let h = |b: &u8| Hash32([*b; 32]);
            let ids = ids
                .map(|a| ChainIds { added_chain_block_hashes: a.iter().map(h).collect(), ..ChainIds::default() })
                .map_err(str::to_string);
            Arc::new(Fixed { added: added.iter().map(h).collect(), ids: Mutex::new(ids) })
        }
    }

    impl ChainSource for Fixed {
        async fn vspc_v2(&self, _req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
            let blocks = self
                .added
                .iter()
                .map(|h| super::super::types::RawChainBlock {
                    chain_block_header: super::super::types::ChainBlockHeader { hash: *h, ..Default::default() },
                    accepted_transactions: vec![],
                })
                .collect();
            Ok(RawVspcResponse {
                removed_chain_block_hashes: vec![],
                added_chain_block_hashes: self.added.clone(),
                chain_block_accepted_transactions: blocks,
                wire_bytes: 0,
            })
        }
        async fn chain_with_ids(&self, _start: Hash32, _m: Option<u64>) -> Result<ChainIds, RpcError> {
            self.ids.lock().unwrap().clone().map_err(RpcError::Transport)
        }
        async fn dag_info(&self) -> Result<DagInfo, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn server_info(&self) -> Result<ServerInfo, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn block_exists(&self, _h: Hash32) -> Result<bool, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn sink_blue_score(&self) -> Result<u64, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn block_blue_score(&self, _h: Hash32) -> Result<Option<u64>, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn chain_hashes(&self, _s: Hash32) -> Result<ChainHashes, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
    }

    fn pair(primary: Arc<Fixed>, second: Arc<Fixed>) -> MultiNode<Fixed> {
        let spec = |url: &str, cross_check| NodeSpec {
            url: url.into(),
            fetch: true,
            submit: true,
            connections: 1,
            cross_check,
            may_lead: true,
        };
        MultiNode::new(vec![(spec("resolver", false), primary.clone()), (spec("resolver:2", true), second)], primary)
    }

    fn req() -> VspcRequest {
        VspcRequest::new(Hash32([0; 32]), Verbosity::High, None)
    }

    /// A public chain authority whose answer contradicts the store is given up for another.
    ///
    /// The follower asks that one instead of halting. A node named in the configuration is not
    /// given up.
    #[test]
    fn a_public_authority_that_contradicts_the_store_is_replaced() {
        let _g = super::super::resolver::tests::serial();
        super::super::resolver::tests::reset(|_, avoid| {
            let nodes = ["wss://a.example/x/borsh", "wss://b.example/x/borsh", "wss://c.example/x/borsh"];
            let pick = nodes.iter().find(|n| !avoid.contains(&super::super::resolver::host(n))).unwrap_or(&nodes[0]);
            Ok(super::super::resolver::Resolved { url: pick.to_string(), resolver: "fake".into() })
        });
        let (a, b) = (super::super::resolver::endpoint("resolver").unwrap(), super::super::resolver::endpoint("resolver:2").unwrap());
        let m = pair(Fixed::new(&[1], Ok(&[])), Fixed::new(&[], Ok(&[1])));
        let (cursor, other) = (Hash32([1; 32]), Hash32([2; 32]));
        let why = "stored chain contradicts the node";
        let before = super::super::resolver::authority_replaced();
        assert_eq!(m.authority_contradicted(cursor, why), StoreContradiction::Replaced);
        assert_eq!(super::super::resolver::current("resolver"), Some(b.clone()), "the other node is the authority now");
        assert!(super::super::resolver::last_reason("resolver").unwrap().starts_with(&super::super::resolver::host(&a)));
        assert_eq!(super::super::resolver::authority_replaced(), before + 1, "counted (kob_indexer_authority_replaced_total)");
        // The new authority, on another host, contradicts the store at the same cursor too. The two
        // agree with each other against the store, so the store halts (`gap`) instead of giving up
        // public node after public node.
        match m.authority_contradicted(cursor, why) {
            StoreContradiction::Halt(Some(r)) => {
                assert!(r.contains("a.example, b.example") && r.contains("the store is at fault"), "{r}")
            }
            v => panic!("{v:?}"),
        }
        assert_eq!(super::super::resolver::current("resolver"), Some(b.clone()), "not replaced: the store halts");
        assert_eq!(super::super::resolver::authority_replaced(), before + 1);
        // A contradiction at another cursor is judged afresh, because the store moved on.
        assert_eq!(m.authority_contradicted(other, why), StoreContradiction::Replaced);
        // One host named again and again, as the resolvers have no other. The third contradiction
        // at one cursor halts the store.
        let one = pair(Fixed::new(&[1], Ok(&[])), Fixed::new(&[], Ok(&[1])));
        *one.store_contradictions.lock().unwrap() = Some((cursor, vec![super::super::resolver::host(&b)], 1));
        super::super::resolver::tests::set_current("resolver", &b);
        assert_eq!(one.authority_contradicted(cursor, why), StoreContradiction::Replaced);
        super::super::resolver::tests::set_current("resolver", &b);
        assert!(matches!(one.authority_contradicted(cursor, why), StoreContradiction::Halt(Some(_))), "the third at one cursor");
        let spec =
            |url: &str| NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check: false, may_lead: true };
        let own = Fixed::new(&[1], Ok(&[]));
        let m = MultiNode::new(vec![(spec("ws://127.0.0.1:18210"), own.clone())], own);
        assert_eq!(
            m.authority_contradicted(cursor, why),
            StoreContradiction::Halt(None),
            "a node of one's own halts the store as before"
        );
    }

    /// A public chain authority whose chain requests keep failing is given up like any other.
    ///
    /// The other node of the pair is the authority at once. The entry resolves a replacement on
    /// another host, and the replacement is counted.
    #[tokio::test]
    // The guard only serialises the tests that share the process-wide resolver state (a
    // current-thread runtime).
    #[allow(clippy::await_holding_lock)]
    async fn a_public_authority_whose_chain_requests_keep_failing_is_replaced() {
        let _g = super::super::resolver::tests::serial();
        super::super::resolver::tests::reset(|_, avoid| {
            let nodes = ["wss://a.example/x/borsh", "wss://b.example/x/borsh", "wss://c.example/x/borsh"];
            let pick = nodes.iter().find(|n| !avoid.contains(&super::super::resolver::host(n))).unwrap_or(&nodes[0]);
            Ok(super::super::resolver::Resolved { url: pick.to_string(), resolver: "fake".into() })
        });
        let (a, b) = (super::super::resolver::endpoint("resolver").unwrap(), super::super::resolver::endpoint("resolver:2").unwrap());
        let m = pair(Fixed::new(&[1], Ok(&[])), Fixed::new(&[], Ok(&[1])));
        let before = super::super::resolver::authority_replaced();
        assert!(m.authority_failing(Hash32([1; 32]), "2 chain requests failed in a row").await);
        assert_eq!(super::super::resolver::current("resolver"), Some(b), "the other node is the authority now");
        let why = super::super::resolver::last_reason("resolver").unwrap();
        assert!(why.starts_with(&super::super::resolver::host(&a)) && why.contains("chain requests keep failing"), "{why}");
        assert_eq!(super::super::resolver::authority_replaced(), before + 1);
        assert_eq!(m.board.authority(), 0, "the entries take other nodes; the authority's entry stays the first");
    }

    /// The pair's disagreement rules on the authority's single steps.
    ///
    /// - The blocks both nodes report are kept.
    /// - A contradiction cuts the answer before it.
    /// - A contradiction at the very next block applies nothing. If it does not settle, the
    ///   cross-checking node is given up, and then the primary's node (resolver entries).
    /// - A cross-checking node that cannot answer leaves the primary alone, with no stall.
    #[tokio::test]
    // The guard only serialises the tests that share the process-wide resolver state (a
    // current-thread runtime).
    #[allow(clippy::await_holding_lock)]
    async fn the_pair_keeps_only_what_both_nodes_report_and_replaces_a_node_that_keeps_disagreeing() {
        let _g = super::super::resolver::tests::serial();
        super::super::resolver::tests::reset(|_, avoid| {
            let nodes = ["wss://a.example/x/borsh", "wss://b.example/x/borsh", "wss://c.example/x/borsh", "wss://d.example/x/borsh"];
            let pick = nodes.iter().find(|n| !avoid.contains(&super::super::resolver::host(n))).unwrap_or(&nodes[0]);
            Ok(super::super::resolver::Resolved { url: pick.to_string(), resolver: "fake".into() })
        });
        let (a, b) = (super::super::resolver::endpoint("resolver").unwrap(), super::super::resolver::endpoint("resolver:2").unwrap());

        // The other node is a block behind. Nothing is contradicted, so the whole answer applies.
        let m = pair(Fixed::new(&[1, 2, 3], Ok(&[])), Fixed::new(&[], Ok(&[1, 2])));
        assert_eq!(m.vspc_v2(req()).await.unwrap().added_chain_block_hashes.len(), 3);
        assert_eq!(m.board.snapshot()[1].disagreements, 0);
        // Another third block, so the first two are kept.
        let m = pair(Fixed::new(&[1, 2, 3], Ok(&[])), Fixed::new(&[], Ok(&[1, 2, 9])));
        let r = m.vspc_v2(req()).await.unwrap();
        assert_eq!(r.added_chain_block_hashes, vec![Hash32([1; 32]), Hash32([2; 32])]);
        assert_eq!(r.chain_block_accepted_transactions.len(), 2);
        // The tips differing for a moment are counted apart and are not a disagreement.
        assert_eq!((m.board.snapshot()[1].disagreements, m.board.snapshot()[1].tip_differs), (0, 1));
        assert_eq!(m.board.snapshot()[1].last_error, None);
        // The cross-checking node does not answer. The primary goes on alone and the node backs
        // off.
        let m = pair(Fixed::new(&[1, 2], Ok(&[])), Fixed::new(&[], Err("connection closed")));
        assert_eq!(m.vspc_v2(req()).await.unwrap().added_chain_block_hashes.len(), 2);
        assert_eq!(m.board.snapshot()[1].state, "backoff");

        // The very next block is contradicted, so nothing is applied. The third time the
        // cross-checking node is replaced, and the sixth time the primary's. The cross-checking
        // node is the authority then.
        let m = pair(Fixed::new(&[1, 2], Ok(&[])), Fixed::new(&[], Ok(&[7, 8])));
        for n in 1..=6u32 {
            let e = m.vspc_v2(req()).await.unwrap_err();
            assert!(e.to_string().contains(TIPS_DIFFER), "{e}");
            match n {
                1 | 2 => assert_eq!(super::super::resolver::current("resolver:2"), Some(b.clone())),
                3 => {
                    assert_eq!(super::super::resolver::current("resolver:2"), None, "the cross-checking node given up");
                    assert_eq!(super::super::resolver::current("resolver"), Some(a.clone()));
                    let c = super::super::resolver::endpoint("resolver:2").unwrap();
                    assert_eq!(super::super::resolver::host(&c), "c.example", "a third host");
                }
                6 => {
                    let now = super::super::resolver::current("resolver").unwrap();
                    assert_eq!(super::super::resolver::host(&now), "c.example", "the other node is the authority now");
                }
                _ => {}
            }
        }
        assert_eq!(m.board.snapshot()[1].tip_differs, 6);
    }
}
