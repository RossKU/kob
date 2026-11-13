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
//! **A cross-checking node** (`NodeSpec::cross_check`; the second public node of a resolver pair, `rpc::resolver`) also
//! vouches for the primary's own data: every answer of the primary the follower applies (its single steps near the sink and
//! its windows) is compared with that node's acceptance data from the same start (`verify::agreement`: the same chain block
//! at every position, the same accepted transactions in it). Only what both report is applied: at the first contradiction the
//! answer is cut before it, and when the very next chain block is contradicted nothing is applied until the two agree (a race
//! at the tip settles within seconds). A contradiction is logged and counted (`disagreements`; the tips differing for a
//! moment are only counted apart, as `tip_differs`, and logged at debug); one that blocks the cursor three times in a row
//! gives the cross-checking node up for another (its resolver entry resolves a replacement), and three
//! more the primary's node (the cross-checking one becomes the authority, `resolver::replace`). A cross-checking node that
//! cannot answer is skipped (backoff) and the primary goes on alone meanwhile: no stall, and no contradiction is ignored,
//! only none can be seen while there is no second node. It has [`CROSS_CHECK_STEP_TIMEOUT`] for a single step near the sink
//! (a hanging node must not hold the book back) and [`CROSS_CHECK_TIMEOUT`] for a window; its timeouts count against it
//! (`rpc::judge`).
//!
//! **The health watch** checks every node of the list the same way, the authority and the others alike: every
//! [`WATCH_EVERY`] each is asked `getServerInfo` (within [`WATCH_TIMEOUT`]). An answer is judged (`rpc::judge`: another
//! network, no UTXO index, not synced for long), a timeout counts as one (a node that hangs), and a node more than [`LAG_DAA`]
//! behind the most advanced one for [`LAG_CHECKS`] checks in a row is behind, **once it confirmed that lead itself**: a DAA
//! is each node's own claim, and a node that over-reports must not put its honest partner behind. So the most advanced
//! node's sink is taken at the first check of the streak and at the [`LAG_CHECKS`]-th, and the node behind is asked for it
//! (`getBlock`) at each check after: once it knows both and each is more than [`LAG_DAA`] above its own DAA of that check, the
//! lag is confirmed. A sink it does not learn within [`LAG_CONFIRM_CHECKS`] checks, or one not that far ahead, refutes the
//! lead (warned about, nothing held against the node behind, which is not suspect either until it confirmed a lead). A node
//! that stopped advancing never confirms a lead, so the DAG and the local monotonic clock judge it (never a block's
//! timestamp, which its miner sets): its sink has not changed for [`FROZEN_AFTER`] while another node's changed within it
//! (or for [`FROZEN_ALONE_AFTER`] when no other node answers); only a node whose sink was seen to change counts as advancing
//! (one seen for the first time has no vote yet), and the finding is asserted again at every check while the sink stays
//! where it was found frozen. A lead the DAG does not back counts against the node that
//! claims it: its virtual DAA score more than [`LAG_DAA`] above its own sink's, as it and the node behind (which knows that
//! block) both report it, at [`INFLATED_CHECKS`] checks in a row, is a contradiction. A node that contradicts the other (a
//! hash, or the acceptance data) is judged too. Whichever node it is,
//! a resolver entry gives it up for another public node (the authority's role moves to the other node at once, unless that
//! one is suspect itself, `rpc::resolver`); a node named in the configuration raises an alarm, and a cross-checking one is
//! cut off from the pair until the alarm ends (the authority goes on alone meanwhile). Each node's `cut_reason` says why it
//! was last cut off.
//!
//! **A failing authority.** A node can answer its small calls (`getServerInfo`, the sink) while its chain requests time out
//! or lose the connection. When the follower finds that its chain requests keep failing while the cursor stalls
//! (`indexer::follower`, *A failing chain authority*), it tells the source ([`MultiNode::authority_failing`]): a resolver entry
//! gives its node up (the other node of the pair is the authority, a replacement is resolved on another host); among named
//! nodes, the first other node that can lead becomes the chain authority (the roles swap: `NodeBoard::authority`). It must
//! not be dropped, have an alarm, be behind, or have contradicted the authority within [`PROMOTE_CLEAN_FOR`], and it must
//! show, within [`PROMOTE_CHECK_TIMEOUT`], that it is on the right network, synced, knows the cursor and has it on its
//! selected chain. Its first answers from the cursor may remove at most [`SETTLE_MAX_REORG`] chain blocks and must know the
//! cursor, else they are refused. The old authority raises an alarm and is cut off from the pair until it ends: its chain
//! requests answer again (probed with the health watch) for `judge::ALARM_CLEAR_AFTER`. It is a secondary node then, and
//! leads again only if the new authority fails the same way.
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
    pub url: String,
    /// Serves windows of the parallel fetch.
    pub fetch: bool,
    /// Takes the executor's transactions.
    pub submit: bool,
    /// Windows fetched from it at once.
    pub connections: usize,
    /// It vouches for the primary's own answers too (see the module documentation): the second node of a resolver pair.
    pub cross_check: bool,
}

/// What `/v1/health` reports per node.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NodeStatus {
    pub url: String,
    /// The host a resolver entry uses now (`None`: not a resolver entry, or it is being replaced).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// How many times a resolver entry gave its node up (failed, or replaced after disagreeing).
    pub replaced: u32,
    /// How many times this node was made the chain authority because the authority's chain requests kept failing
    /// (`MultiNode::authority_failing`).
    pub promoted: u32,
    /// Why the node was last cut off from the pair: for a resolver entry the public node it gave up and why (`host: reason`);
    /// for a node named in the configuration its alarms now (`rpc::judge`), or why it was dropped. `None`: never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut_reason: Option<String>,
    /// Answers of the primary it contradicted (a cross-checking node, `NodeSpec::cross_check`): other accepted transactions, a
    /// different chain block deep in the answer, a deep reorg seen by one node only. Not the tips differing for a moment.
    pub disagreements: u64,
    /// Times the two nodes' newest chain blocks differed for a moment (routine on a public pair; settles by the next poll).
    pub tip_differs: u64,
    /// `primary` (the chain authority) or `secondary`.
    pub role: String,
    /// `ok`, `backoff` (failed lately: skipped until `backoff_secs`), `dropped` (contradicted the primary), `cut` (a named
    /// node with an alarm, left out of the pair until it ends: `cut_reason`) or `idle` (neither fetches nor takes submissions).
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
    /// When it last contradicted the authority as the cross-checking node (not the tips differing for a moment).
    last_disagreement: Option<Instant>,
}

/// A node whose rate is below the best one's divided by this gets windows only when no faster node qualifies: in chain
/// order, a slow window holds up every window behind it.
const SLOW_FACTOR: f64 = 4.0;
/// A node's rate is measured again (its next window counts as unmeasured) when it got no window for this long.
const REMEASURE: Duration = Duration::from_secs(300);

/// Per-node state shared by the chain source and the submitter.
pub struct NodeBoard {
    peers: Vec<Mutex<PeerState>>,
    /// The configured URL of each node (a resolver entry's pseudo URL: the node it uses is looked up when shown).
    entries: Vec<String>,
    /// The node that is the chain authority now: node 0 (the configured primary) until its chain requests keep failing and
    /// another node is promoted (`MultiNode::authority_failing`).
    authority: AtomicUsize,
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
        Arc::new(NodeBoard { peers, entries: specs.iter().map(|s| s.url.clone()).collect(), authority: AtomicUsize::new(0) })
    }

    /// The node that is the chain authority now (0, the configured primary, unless another one was promoted).
    pub fn authority(&self) -> usize {
        self.authority.load(Ordering::Relaxed)
    }

    /// The configured URL of node `i`.
    pub fn entry(&self, i: usize) -> &str {
        &self.entries[i]
    }

    /// The host node `i` uses now, for logs and the status line: a resolver entry's public node, else the redacted URL.
    pub fn shown(&self, i: usize) -> String {
        let e = &self.entries[i];
        if super::resolver::is_resolver(e) {
            super::resolver::current(e).map(|n| super::resolver::host(&n)).unwrap_or_else(|| format!("{e} (resolving)"))
        } else {
            super::redact_url(e)
        }
    }

    /// The node entry `i` stands for now: a resolver entry's public node (`None`: being replaced), else the URL.
    pub fn node_of(&self, i: usize) -> Option<String> {
        let e = &self.entries[i];
        if super::resolver::is_resolver(e) {
            super::resolver::current(e)
        } else {
            Some(e.clone())
        }
    }

    /// A named node other than the primary with an alarm that cuts it off (`rpc::judge::cuts_off`): left out of the pair
    /// until the alarm ends. One that contradicted the primary is dropped instead (`dropped_reason`).
    fn cut(&self, i: usize) -> bool {
        i != self.authority() && !super::resolver::is_resolver(&self.entries[i]) && super::judge::cuts_off(&self.entries[i])
    }

    /// The cross-checking node `i` contradicted the primary.
    fn disagreed(&self, i: usize, why: &str, at_tip: bool) {
        self.with(i, |p| {
            if at_tip {
                // the tips differing for a moment is no contradiction: counted apart, and no error to show
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
        let public = super::resolver::is_resolver(&self.entries[i]);
        let authority = i == self.authority();
        if fault == Fault::Lie {
            // judged like any other failure (outside the node's lock: a resolver entry gives its node up for another, a named
            // node raises an alarm and, unless it is the primary, is dropped below)
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
                    // a resolver entry's node was replaced: its successor is not dropped
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
                    // a slow or failing link should not keep its old rate
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

    /// Stop using node `i`: a resolver entry's node is replaced (the primary's too: the other node is the authority then); a
    /// named primary stays the authority, with an alarm.
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
    /// Contradictions in a row that left nothing to apply (`MultiNode::vouched`).
    stalls: std::sync::atomic::AtomicU32,
    /// When a contradiction was last logged (one line per ten seconds; the counters keep every one).
    last_warn: Mutex<Option<Instant>>,
    /// The health watch (module documentation).
    watch: Arc<Mutex<Watch>>,
    /// Public chain authorities whose answer contradicted the store at one cursor: the cursor, the hosts, and how many times
    /// (`MultiNode::authority_contradicted`).
    store_contradictions: Mutex<Option<(Hash32, Vec<String>, u32)>>,
    /// A node was promoted to chain authority and has not yet answered from the cursor in a way that continues it
    /// (`MultiNode::settle`).
    settling: AtomicBool,
    /// The start of the last chain request to the authority (the follower's cursor): what a promoted node must continue, and
    /// what a demoted node is probed from (`MultiNode::probe_demoted`).
    last_start: Mutex<Option<Hash32>>,
    /// A probe of demoted nodes runs now (one at a time).
    probing: Arc<AtomicBool>,
}

/// The health watch's state: its period, how many checks in a row make a node behind, when it last ran, whether a check runs
/// now, each node's lag, and when an unconfirmed lead was last warned about.
struct Watch {
    every: Duration,
    lag_checks: u32,
    last: Option<Instant>,
    busy: bool,
    lag: Vec<LagTrack>,
    last_refuted_warn: Option<Instant>,
    /// The local monotonic clock (tests set another, `MultiNode::with_clock`): what elapsed time is measured on. A block's
    /// timestamp is the miner's, never the referee.
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    /// Each node's sink at the last check, and since when (the local monotonic clock) it has not changed.
    progress: Vec<Option<Progress>>,
    /// The most advanced node and its checks in a row with a virtual DAA score above its own newest block's by more than
    /// [`LAG_DAA`] (a lead it does not have).
    inflated: Option<(String, u32)>,
}

/// A node's sink and since when it has not changed (module documentation: a node that stopped advancing).
#[derive(Debug, Clone)]
struct Progress {
    node: String,
    sink: Hash32,
    since: Instant,
    /// This stillness was reported already (once per sink; the finding is asserted again at every check while the sink stays
    /// the same, so a named node's alarm does not end while it is still frozen).
    reported: bool,
    /// The sink was seen to change on this node (a record before this one, of the same node, had another sink). Only such a
    /// node counts as advancing: one seen for the first time (a new node, a replacement, one that answers again) has no vote
    /// until its sink changes, so a network stall does not make it look advancing next to a node that stands still.
    moved: bool,
}

/// The most advanced node's lead over a node, as the answers of one check put it: kept until the node behind can confirm it.
#[derive(Debug, Clone, Copy)]
struct Claim {
    /// The most advanced node's sink at the check.
    sink: Hash32,
    /// The node's own DAA at the check.
    own: u64,
    /// Checks since, while unconfirmed.
    age: u32,
    /// `Some(lead)`: the node knows the sink now, and its DAA is `lead` above `own`; `None`: not yet.
    lead: Option<u64>,
}

/// A node's checks in a row behind the most advanced answer, and the claims of that lead taken at the first of them and at
/// the [`LAG_CHECKS`]-th (module documentation).
#[derive(Debug, Clone, Default)]
struct LagTrack {
    /// The node the streak is about (a resolver entry that moved to another node begins again).
    node: String,
    streak: u32,
    claims: Vec<Claim>,
}

/// What one check makes of a node's lag.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LagVerdict {
    /// Not behind.
    Level,
    /// Behind by the answers (DAA), not confirmed yet.
    Unconfirmed(u64),
    /// The other node's lead was not borne out: its sink stayed unknown to this node for [`LAG_CONFIRM_CHECKS`] checks, or
    /// it was not that far ahead (DAA claimed behind).
    Refuted(u64),
    /// Behind for the whole period, confirmed by this node itself (the smallest confirmed lead, DAA).
    Confirmed(u64),
}

impl LagTrack {
    /// The streak begins again (the node stays).
    fn clear(&mut self) {
        self.streak = 0;
        self.claims.clear();
    }

    /// Whether this streak has evidence of its own that the node is behind (a lead it confirmed: the other node's sink is a
    /// real block it placed, that far ahead): only then is it suspect (not promoted). The answers alone are a claim.
    fn evidence(&self) -> bool {
        self.claims.iter().any(|c| c.lead.is_some_and(|l| l > LAG_DAA))
    }

    /// The claims a streak needs before a lag is confirmed: one at its first check and one at its `checks`-th.
    fn needed(checks: u32) -> usize {
        if checks <= 1 {
            1
        } else {
            2
        }
    }

    /// One check: behind by the answers (`Some(daa)`) or not. Whether a new claim is wanted now.
    fn observe(&mut self, behind: Option<u64>, checks: u32) -> bool {
        if behind.is_none() {
            self.clear();
            return false;
        }
        self.streak += 1;
        let n = self.claims.len();
        n < Self::needed(checks) && (n == 0 || self.streak >= checks)
    }

    /// The verdict after this check's claims and confirmations (`behind`: the answers' lag now, DAA). A refuted or a
    /// confirmed streak begins again at the next check.
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

/// The start of the error when the two nodes' newest chain blocks differ and nothing can be applied yet (routine on a public
/// pair: the next poll settles it); the follower does not warn about it until it repeats.
pub const TIPS_DIFFER: &str = "the two nodes' newest chain blocks differ for a moment";

/// Contradictions in a row that block the cursor before the cross-checking node is given up for another (and twice this
/// many before the primary's node is: `MultiNode::vouched`).
pub const STALLS_BEFORE_REPLACING: u32 = 3;

/// How long the cross-checking node may take to vouch for one window before the primary goes on alone for it.
pub const CROSS_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long it may take to vouch for one single step near the sink (a few chain blocks): a hanging node must not hold the book
/// back for long.
pub const CROSS_CHECK_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// The health watch's period, and how long one node's `getServerInfo` may take in it (module documentation).
pub const WATCH_EVERY: Duration = Duration::from_secs(10);
pub const WATCH_TIMEOUT: Duration = Duration::from_secs(5);

/// A node more than this far (DAA, about 30 s of chain) behind the most advanced node of the list ...
pub const LAG_DAA: u64 = 300;
/// ... at this many health checks in a row (a minute) is behind: given up (a resolver entry) or an alarm (a named node), once
/// it confirmed the lead itself (module documentation).
pub const LAG_CHECKS: u32 = 6;
/// Checks (five minutes) within which a node must learn the most advanced node's sink of a check that put it behind; a sink it
/// never learns was not real, and nothing is held against the node.
pub const LAG_CONFIRM_CHECKS: u32 = 30;
/// A node whose sink has not changed for this long of the local monotonic clock while another node's sink changed within it
/// stopped advancing (the chain makes about ten blocks a second on mainnet and testnet-10 alike): given up or an alarm, whatever
/// its DAA says. Its own answers decide: no other node can make an honest node's sink stand still.
pub const FROZEN_AFTER: Duration = Duration::from_secs(120);
/// ... and this long when no other node answers (nothing to compare it with).
pub const FROZEN_ALONE_AFTER: Duration = Duration::from_secs(600);
/// Checks in a row at which the most advanced node reports a virtual DAA score more than [`LAG_DAA`] above the DAA score of
/// its own sink, as it reports it AND as the node behind (which knows that block) reports it, before that counts against it as
/// a contradiction: the block both sides place in the DAG does not back the lead it claims.
pub const INFLATED_CHECKS: u32 = 2;
/// Public chain authorities that may contradict the store at one cursor before the store halts there (`gap`) however many
/// hosts they were on: two on different hosts agreeing with each other halt it at once (the store is at fault, not the nodes).
pub const STORE_CONTRADICTIONS_MAX: u32 = 3;
/// How long a node may take to show that it can be the chain authority (`MultiNode::authority_failing`): the right network,
/// synced, the cursor known and on its selected chain.
pub const PROMOTE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// A cross-checking node that contradicted the authority within this long is not promoted.
pub const PROMOTE_CLEAN_FOR: Duration = Duration::from_secs(600);
/// Right after a promotion, the new authority's first answer from the cursor may remove at most this many chain blocks (a
/// reorg the old authority had not seen yet, about 10 s of chain); a deeper one, or a cursor it does not know, is refused.
pub const SETTLE_MAX_REORG: usize = 100;
/// How long a demoted node's chain request may take in the probe that ends its alarm (`MultiNode::probe_demoted`).
pub const DEMOTED_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Nodes promoted to chain authority in this process (`MultiNode::authority_failing`, named nodes; a resolver entry's
/// replacement is counted by `resolver::authority_replaced`). Part of `kob_indexer_authority_replaced_total`.
static PROMOTIONS: AtomicU64 = AtomicU64::new(0);

/// Named nodes promoted to chain authority since the start (see [`PROMOTIONS`]).
pub fn promotions() -> u64 {
    PROMOTIONS.load(Ordering::Relaxed)
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
            stalls: std::sync::atomic::AtomicU32::new(0),
            last_warn: Mutex::new(None),
            store_contradictions: Mutex::new(None),
            settling: AtomicBool::new(false),
            last_start: Mutex::new(None),
            probing: Arc::new(AtomicBool::new(false)),
            // the first check one period after the start
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

    /// The health watch every `every`, a node behind after `lag_checks` checks in a row (tests; the defaults are
    /// [`WATCH_EVERY`] and [`LAG_CHECKS`]).
    pub fn with_watch(self, every: Duration, lag_checks: u32) -> Self {
        {
            let mut w = self.watch.lock().unwrap_or_else(|e| e.into_inner());
            (w.every, w.lag_checks) = (every, lag_checks.max(1));
        }
        self
    }

    /// The local monotonic clock the health watch measures a node's stillness on (tests; the default is `Instant::now`).
    pub fn with_clock(self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.watch.lock().unwrap_or_else(|e| e.into_inner()).clock = Arc::new(clock);
        self
    }

    /// Starts the health watch (module documentation) on a task of its own when it is due: the follower's request never waits
    /// for it (a node that hangs costs the watch its timeout, not the book).
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

    /// One health check of every node now, on the caller's task (tests; the follower's requests start it on a task of its own
    /// every [`WATCH_EVERY`]), and a probe of the demoted nodes ([`MultiNode::authority_failing`]).
    pub async fn check_health(&self) {
        watch_nodes(&self.nodes, &self.verifier, &self.board, &self.watch).await;
        let at = *self.last_start.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = at {
            probe_demoted(&self.nodes, &self.verifier, &self.board, at).await;
        }
    }

    /// The chain authority now (node 0, the configured primary, unless another node was promoted).
    fn primary(&self) -> &N {
        &self.nodes[self.board.authority()]
    }

    /// The authority on connections of its own for the acceptance data and its copies of chain blocks: the configured
    /// primary's second client, or a promoted node's own client (it has no second one).
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

    /// The authority's answer `r` from `start` (a single step, or the chain hashes): right after a promotion
    /// ([`MultiNode::authority_failing`]) it may remove at most [`SETTLE_MAX_REORG`] chain blocks and must know the cursor (the
    /// follower would rewind the store to the newest block it knows), else it is refused and nothing is applied. The first
    /// answer that continues the cursor settles it. Any answer ends a chain alarm of the authority (`judge::chain_ok`).
    fn settle<T>(&self, start: Hash32, r: Result<T, RpcError>, removed: impl Fn(&T) -> usize) -> Result<T, RpcError> {
        let a = self.board.authority();
        if r.is_ok() {
            if let Some(node) = self.board.node_of(a) {
                super::judge::chain_ok(self.board.entry(a), &node);
            }
        }
        if !self.settling.load(Ordering::Relaxed) {
            return r;
        }
        let refuse = match &r {
            Ok(v) if removed(v) > SETTLE_MAX_REORG => Some(format!(
                "it removes {} chain blocks at the cursor {start} (at most {SETTLE_MAX_REORG} right after it took over)",
                removed(v)
            )),
            Err(e) if e.kind() == super::NodeErrorKind::UnknownHash => {
                Some(format!("it does not know the cursor {start} (that would rewind the store)"))
            }
            _ => None,
        };
        if let Some(why) = refuse {
            tracing::error!(authority = %self.board.shown(a), reason = %why, "the new chain authority's answer is refused, nothing is applied");
            return Err(RpcError::Node(format!("the new chain authority {}: {why}; not applied", self.board.shown(a))));
        }
        if r.is_ok() && self.settling.swap(false, Ordering::Relaxed) {
            tracing::info!(authority = %self.board.shown(a), cursor = %start, "the new chain authority continues the cursor");
        }
        r
    }

    /// The nodes that may become the chain authority instead of `a`, in the order of the list: not dropped (a node that
    /// contradicted the authority), without an alarm (`rpc::judge`: another network, not synced, hanging, behind, frozen, a
    /// contradiction, failing chain requests), not suspect (behind at the last health check), and, as the cross-checking
    /// node, no contradiction of the authority within [`PROMOTE_CLEAN_FOR`].
    fn candidates(&self, a: usize) -> Vec<usize> {
        let now = Instant::now();
        (0..self.nodes.len())
            .filter(|&i| i != a)
            .filter(|&i| {
                let entry = self.board.entry(i);
                let Some(node) = self.board.node_of(i) else { return false };
                let contradicted =
                    self.board.with(i, |p| p.last_disagreement.is_some_and(|t| now.duration_since(t) < PROMOTE_CLEAN_FOR));
                !self.board.dropped(i) && !contradicted && super::judge::alarm_of(entry).is_none() && !super::judge::suspect(&node)
            })
            .collect()
    }

    /// Whether node `k` can be the chain authority from the cursor `at`, within [`PROMOTE_CHECK_TIMEOUT`]: the right network,
    /// synced, it knows the cursor, and the cursor is on its selected chain (its chain hashes from it remove nothing). The
    /// stored chain is the cursor's own chain of selected parents (fixed by the hashes of its headers), so such a node agrees
    /// with everything the store holds.
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

    /// The node that vouches for the primary's answers now: the first cross-checking one that is neither dropped, cut off
    /// (a named node with an alarm) nor backing off.
    fn cross_checker(&self) -> Option<usize> {
        let now = Instant::now();
        let a = self.board.authority();
        (0..self.nodes.len()).filter(|&i| i != a).find(|&i| {
            let entry = &self.specs[i].url;
            // a resolver entry being replaced has no node to ask: the primary goes on alone meanwhile, never waits for it
            let replacing = super::resolver::is_resolver(entry) && super::resolver::current(entry).is_none();
            self.specs[i].cross_check
                && !replacing
                && !self.board.dropped(i)
                && !self.board.cut(i)
                && !self.board.with(i, |p| p.backoff_until.is_some_and(|t| t > now))
        })
    }

    /// The cross-checking node's acceptance data from `start` (`None`: no such node now, or it could not answer within
    /// `limit`: the primary goes on alone and the node backs off; a timeout counts against it, `rpc::judge`). Asked while the
    /// primary answers the same request, so it costs no extra round trip.
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

    /// A window from the primary (`node`: its own client or the verifier's), vouched for by the cross-checking node, which is
    /// asked for the same range at the same time (the window's bounds are read from the primary first, as
    /// `fetch_window_from` would).
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

    /// The primary's answer `raw` from `start`, as far as the cross-checking node vouches for it (module documentation):
    /// whole when nothing is contradicted or no such node can answer now, cut before the first contradiction, an error when
    /// the next chain block itself is contradicted.
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
            // the tips differ for a moment (not yet agreed): routine on a public pair; a tip that never settles ends in
            // replacing a node below, which is warned about
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
        let n = self.stalls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        // a disagreement that does not settle: replace the cross-checking node, then the primary's (resolver entries only)
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
        if a.at_tip {
            // the follower logs this at debug until it repeats (`STALLS_BEFORE_REPLACING` times in a row)
            return Err(RpcError::Node(format!("{TIPS_DIFFER} ({why}): nothing is indexed until they agree")));
        }
        Err(RpcError::Node(format!("the nodes disagree on the next chain block ({why}): nothing is indexed until they agree")))
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

    /// The primary's sink blue score, at most a second old (`at_least`: refreshed when below it). It only bounds requests
    /// for the primary's acceptance data: a stale sink makes the answer a few chain blocks longer, nothing else.
    async fn primary_sink(&self, at_least: u64) -> Result<u64, RpcError> {
        let cached = *self.sink.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, b)) = cached.filter(|(t, b)| t.elapsed() < Duration::from_secs(1) && *b > at_least) {
            return Ok(b);
        }
        let b = self.checker().sink_blue_score().await?;
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), b));
        Ok(b)
    }

    /// The window from secondary `i`, checked against the primary: the node's own position first (it must know the window
    /// end), then its answer and the primary's acceptance data for the same start at once. The blue score of the window end
    /// from the node only bounds the primary's answer (a wrong one makes it shorter, and fewer chain blocks are verified, or
    /// longer).
    async fn fetch_checked(&self, i: usize, w: &WindowRequest) -> Result<Fetched, Attempt> {
        let origin = Origin { node: i, trusted: false, tier: 0 };
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
        let ids = self.checker().chain_with_ids(w.start, window_min_confirmations(primary_sink, end_blue, w.min_confirmations));
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

/// The health watch's check (module documentation): every node's `getServerInfo` at once, judged; then each node's DAA
/// against the most advanced answer, a lag confirmed by the node behind before it counts.
async fn watch_nodes<N: ChainSource>(nodes: &[Arc<N>], verifier: &Arc<N>, board: &NodeBoard, watch: &Mutex<Watch>) {
    let lock = || watch.lock().unwrap_or_else(|e| e.into_inner());
    // one check at a time: a slow one (its timeouts) never overlaps the next
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
    // the primary on the verifier's connections: never behind its own windows
    let src = |i: usize| -> &N {
        if i == 0 {
            verifier
        } else {
            &nodes[i]
        }
    };
    let ask = |i: usize| async move {
        // a resolver entry being replaced has no node to ask
        let node = board.node_of(i)?;
        let entry = board.entry(i);
        match tokio::time::timeout(WATCH_TIMEOUT, src(i).server_info()).await {
            Ok(Ok(info)) => {
                let judged = super::judge::server_info(entry, &node, &info.network_id, info.is_synced, info.has_utxo_index);
                // the DAA of another network says nothing about this one's
                let comparable = network.is_empty() || info.network_id == network;
                judged.ok().filter(|()| comparable)?;
                // its sink: whether it advances (the DAG, not a number it reports)
                let sink = tokio::time::timeout(WATCH_TIMEOUT, src(i).dag_info()).await.ok().and_then(Result::ok).map(|d| d.sink);
                Some(((i, node, info.virtual_daa_score), sink))
            }
            // a failed call is reported where it failed (the client's connection, the judge)
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
    // A node whose sink stands still (the local monotonic clock) while another's advances stopped advancing, whatever it
    // reports; alone, after a longer while
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
            // another node of the list, as it answers now, whose sink was seen to change lately (audit round 4 R4-L-4)
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
                    // still the sink it was found frozen at (audit round 4 R4-L-3): the finding stands, asserted again
                    out.push((*i, node.clone(), s.as_secs(), alone, true));
                } else if limit.is_some_and(|l| s >= l) {
                    p.reported = true;
                    out.push((*i, node.clone(), s.as_secs(), alone, false));
                }
            }
        }
        out
    };
    // the nodes found frozen and still frozen: no fine answer of this check ends their finding
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
    // A DAA is the node's own claim: a node that over-reports would put its honest partner behind. So a lag counts only once
    // the node behind confirms it: the most advanced node's sink at the check must become known to it, and that far above its
    // own DAA then (a sink it never learns was not real).
    // the nodes that take a claim at this check
    let wanting: Vec<usize> = {
        let mut w = lock();
        w.lag.resize(n, LagTrack::default());
        let checks = w.lag_checks;
        answers
            .iter()
            .filter(|(i, node, daa)| {
                let t = &mut w.lag[*i];
                // the entry stands for another node now: what was claimed about the last one says nothing about it
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
    // while a node is behind: the most advanced node's sink (the claims'), and its DAA score as that node reports it and as
    // the first node behind (which places it in its own DAG) does
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
                // not known yet (a later claim's sink is not either), or no answer: asked again at the next check
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
        // the lead is not backed by the DAG: the most advanced node's own sink, a block the node behind knows too, is far below
        // the virtual DAA score it reports, as both of them report that block
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
            // judged frozen above: a lag verdict (fine, refuted or unconfirmed) must not end or soften that
            continue;
        }
        let entry = board.entry(i);
        match v {
            LagVerdict::Level => super::judge::lag(entry, &node, None, None),
            // behind by the other node's claim only: not suspect until it has evidence of its own
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
        let (start, min_confirmations) = (req.start_hash, req.min_confirmation_count);
        let j = self.cross_checker();
        self.health_watch();
        self.asked_from(start);
        let (raw, cross) =
            tokio::join!(self.primary().vspc_v2(req), self.cross_ids(j, start, min_confirmations, CROSS_CHECK_STEP_TIMEOUT));
        let raw = self.settle(start, raw, |r| r.removed_chain_block_hashes.len())?;
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
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.primary().block_exists(hash).await
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
        self.settle(start, r, |c| c.removed_chain_block_hashes.len())
    }
    async fn chain_with_ids(&self, start: Hash32, min_confirmations: Option<u64>) -> Result<ChainIds, RpcError> {
        self.checker().chain_with_ids(start, min_confirmations).await
    }

    async fn fetch_window(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        // the health watch runs while the cursor catches up too (it is due once per period, whichever window asks first)
        self.health_watch();
        self.fetch_any(w).await
    }

    async fn fetch_window_primary(&self, w: WindowRequest) -> Result<Fetched, RpcError> {
        // on the verifier's connections: never behind the primary's own windows of the parallel fetch
        self.primary_window(self.checker(), &w).await
    }

    fn report_lie(&self, origin: Origin, why: &str) {
        self.board.drop_node(origin.node, why);
    }

    /// A public chain authority (a resolver entry) that contradicts the store at the cursor `at` is given up for another
    /// (`rpc::judge`): the follower asks the next one. Once a second authority on another host contradicts the store at the
    /// same cursor (or [`STORE_CONTRADICTIONS_MAX`] authorities did), the two agree with each other against the store: the
    /// cause is the store (a corrupted cursor, a bug), and it halts there (`gap`) instead of giving up public node after public
    /// node. A node named in the configuration is not replaced: the store halts at once.
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

    /// The chain authority's chain requests keep failing while the cursor `at` does not move (`why`; the follower decides
    /// when, `indexer::follower`). A resolver entry gives its public node up (`rpc::judge`, `rpc::resolver::replace`: the
    /// other node of the pair is the authority at once unless the judge suspects it, and the entry resolves a replacement on
    /// another host). A named node raises an alarm (`judge::chain_failing`), and the first other node that can lead
    /// ([`MultiNode::candidates`], [`MultiNode::can_lead`]) is made the chain authority: the roles swap, the old authority
    /// is a secondary node, cut off from the pair until its alarm ends (its chain requests answer again for
    /// `judge::ALARM_CLEAR_AFTER`, [`probe_demoted`]). It is not made the authority again then: only when the new one fails
    /// the same way (no flapping). `true`: another node is the authority now.
    async fn authority_failing(&self, at: Hash32, why: &str) -> bool {
        let a = self.board.authority();
        let entry = self.board.entry(a).to_string();
        // a resolver entry being replaced has no node to give up: it is being replaced already
        let Some(node) = self.board.node_of(a) else { return false };
        let why = format!("its chain requests keep failing ({why})");
        super::judge::chain_failing(&entry, &node, &why);
        // the pair's authority: the resolver state moved the role to the other node, or resolves a replacement
        if entry == super::resolver::RESOLVER {
            return super::resolver::current(&entry).as_deref() != Some(node.as_str());
        }
        // another resolver entry that took the role over earlier gave its node up: one of the others, or its replacement
        let replaced = super::resolver::is_resolver(&entry) && super::resolver::current(&entry).as_deref() != Some(node.as_str());
        let candidates = self.candidates(a);
        if self.nodes.len() > 1 && candidates.is_empty() {
            tracing::warn!(authority = %self.board.shown(a), "the chain authority's chain requests keep failing, and no other node can take over (each is dropped, has an alarm or is behind): staying");
        }
        for k in candidates {
            match self.can_lead(k, at).await {
                Ok(()) => {
                    self.board.authority.store(k, Ordering::Relaxed);
                    self.settling.store(true, Ordering::Relaxed);
                    self.stalls.store(0, Ordering::Relaxed);
                    // the cached sink was the old authority's
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
            // the entry's next node is unproven: its first answers from the cursor are checked
            self.settling.store(true, Ordering::Relaxed);
        }
        replaced
    }

    fn node_stats(&self) -> Vec<NodeStatus> {
        if self.nodes.len() > 1 {
            self.board.snapshot()
        } else {
            vec![]
        }
    }
}

/// The demoted nodes (named nodes other than the authority with a chain alarm, `judge::chain_failing`) asked for the chain from
/// the cursor `at` again, each within [`DEMOTED_PROBE_TIMEOUT`]: an answer is a fine one (the alarm ends after
/// `judge::ALARM_CLEAR_AFTER` of them without a break, and the node is a secondary of the pair again), a failure keeps the alarm.
async fn probe_demoted<N: ChainSource>(nodes: &[Arc<N>], verifier: &Arc<N>, board: &NodeBoard, at: Hash32) {
    let a = board.authority();
    for i in (0..nodes.len()).filter(|&i| i != a) {
        let entry = board.entry(i);
        let Some(node) = board.node_of(i) else { continue };
        if super::resolver::is_resolver(entry) || !super::judge::chain_alarm(entry) {
            continue;
        }
        // node 0 on the verifier's connections, as in the health watch
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
                        // the primary is never skipped: counted only
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
            // a password with an unencoded '?', '#' or '/'
            ("wss://user:pa?ss@node.example/x", "wss://node.example/..."),
            ("wss://user:pa#ss@node.example", "wss://node.example"),
            ("wss://user:p/ss@host", "wss://<redacted>/..."),
            ("ws://[::1]:18210/x", "ws://[::1]:18210/..."),
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
            NodeSpec { url: "ws://127.0.0.1:18210".into(), fetch: true, submit: true, connections: 1, cross_check: false },
            NodeSpec { url: secret.into(), fetch: true, submit: true, connections: 1, cross_check: false },
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

    /// A lag counts only once the node behind confirmed the lead itself: both claims (the streak's first check and its
    /// `checks`-th) known to it later, each more than `LAG_DAA` above its own DAA then. A sink it never learns, or one not that
    /// far ahead, refutes the lead and the streak begins again.
    #[test]
    fn a_lag_counts_only_once_the_node_behind_confirms_the_lead() {
        let claim = |own| Claim { sink: Hash32([5; 32]), own, age: 0, lead: None };
        let checks = 3;
        // a real lag: a claim at the first check and at the third, both confirmed later
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
        // a sink the node behind never learns: refuted after LAG_CONFIRM_CHECKS checks, never a finding
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
        // a sink it knows, but not that far ahead of it: refuted at once
        assert!(t.observe(Some(5_000), checks));
        t.claims.push(Claim { lead: Some(LAG_DAA), ..claim(100) });
        assert_eq!(t.verdict(Some(5_000), checks), LagVerdict::Refuted(5_000));
        // level: nothing kept
        t.observe(Some(5_000), checks);
        t.claims.push(claim(100));
        assert!(!t.observe(None, checks));
        assert_eq!((t.streak, t.claims.len(), t.verdict(None, checks)), (0, 0, LagVerdict::Level));
        // one check to a finding: one claim
        let mut t = LagTrack::default();
        assert!(t.observe(Some(400), 1));
        t.claims.push(Claim { lead: Some(400), ..claim(100) });
        assert_eq!(t.verdict(Some(400), 1), LagVerdict::Confirmed(400));
    }

    /// A node of a fixed answer: the primary's chain blocks (`added`, no transactions) and the acceptance data it reports.
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
        let spec = |url: &str, cross_check| NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check };
        MultiNode::new(vec![(spec("resolver", false), primary.clone()), (spec("resolver:2", true), second)], primary)
    }

    fn req() -> VspcRequest {
        VspcRequest::new(Hash32([0; 32]), Verbosity::High, None)
    }

    /// Audit round 3 R3-L-2: a public chain authority whose answer contradicts the store is given up for another (the follower
    /// asks that one instead of halting); a node named in the configuration is not.
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
        // audit round 4 R4-L-1: the new authority, on another host, contradicts the store at the same cursor too: the two agree
        // with each other against the store, which halts (gap) instead of giving up public node after public node
        match m.authority_contradicted(cursor, why) {
            StoreContradiction::Halt(Some(r)) => {
                assert!(r.contains("a.example, b.example") && r.contains("the store is at fault"), "{r}")
            }
            v => panic!("{v:?}"),
        }
        assert_eq!(super::super::resolver::current("resolver"), Some(b.clone()), "not replaced: the store halts");
        assert_eq!(super::super::resolver::authority_replaced(), before + 1);
        // a contradiction at another cursor is judged afresh (the store moved on)
        assert_eq!(m.authority_contradicted(other, why), StoreContradiction::Replaced);
        // one host named again and again (the resolvers have no other): halted at the third contradiction at one cursor
        let one = pair(Fixed::new(&[1], Ok(&[])), Fixed::new(&[], Ok(&[1])));
        *one.store_contradictions.lock().unwrap() = Some((cursor, vec![super::super::resolver::host(&b)], 1));
        super::super::resolver::tests::set_current("resolver", &b);
        assert_eq!(one.authority_contradicted(cursor, why), StoreContradiction::Replaced);
        super::super::resolver::tests::set_current("resolver", &b);
        assert!(matches!(one.authority_contradicted(cursor, why), StoreContradiction::Halt(Some(_))), "the third at one cursor");
        let spec = |url: &str| NodeSpec { url: url.into(), fetch: true, submit: true, connections: 1, cross_check: false };
        let own = Fixed::new(&[1], Ok(&[]));
        let m = MultiNode::new(vec![(spec("ws://127.0.0.1:18210"), own.clone())], own);
        assert_eq!(
            m.authority_contradicted(cursor, why),
            StoreContradiction::Halt(None),
            "a node of one's own halts the store as before"
        );
    }

    /// A public chain authority whose chain requests keep failing (the follower's finding) is given up like any other: the
    /// other node of the pair is the authority at once, the entry resolves a replacement on another host, and it is counted.
    #[tokio::test]
    // the guard only serialises the tests that share the process-wide resolver state (a current-thread runtime)
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

    /// The pair's disagreement rules on the primary's single steps: the blocks both report are kept; a contradiction cuts the
    /// answer before it; one at the very next block applies nothing, and if it does not settle the cross-checking node and
    /// then the primary's are given up for others (resolver entries); a cross-checking node that cannot answer leaves the
    /// primary alone (no stall).
    #[tokio::test]
    // the guard only serialises the tests that share the process-wide resolver state (a current-thread runtime)
    #[allow(clippy::await_holding_lock)]
    async fn the_pair_keeps_only_what_both_nodes_report_and_replaces_a_node_that_keeps_disagreeing() {
        let _g = super::super::resolver::tests::serial();
        super::super::resolver::tests::reset(|_, avoid| {
            let nodes = ["wss://a.example/x/borsh", "wss://b.example/x/borsh", "wss://c.example/x/borsh", "wss://d.example/x/borsh"];
            let pick = nodes.iter().find(|n| !avoid.contains(&super::super::resolver::host(n))).unwrap_or(&nodes[0]);
            Ok(super::super::resolver::Resolved { url: pick.to_string(), resolver: "fake".into() })
        });
        let (a, b) = (super::super::resolver::endpoint("resolver").unwrap(), super::super::resolver::endpoint("resolver:2").unwrap());

        // the other node is a block behind: nothing contradicted, the whole answer
        let m = pair(Fixed::new(&[1, 2, 3], Ok(&[])), Fixed::new(&[], Ok(&[1, 2])));
        assert_eq!(m.vspc_v2(req()).await.unwrap().added_chain_block_hashes.len(), 3);
        assert_eq!(m.board.snapshot()[1].disagreements, 0);
        // another third block: the first two are kept
        let m = pair(Fixed::new(&[1, 2, 3], Ok(&[])), Fixed::new(&[], Ok(&[1, 2, 9])));
        let r = m.vspc_v2(req()).await.unwrap();
        assert_eq!(r.added_chain_block_hashes, vec![Hash32([1; 32]), Hash32([2; 32])]);
        assert_eq!(r.chain_block_accepted_transactions.len(), 2);
        // the tips differing for a moment: counted apart, not a disagreement
        assert_eq!((m.board.snapshot()[1].disagreements, m.board.snapshot()[1].tip_differs), (0, 1));
        assert_eq!(m.board.snapshot()[1].last_error, None);
        // the cross-checking node does not answer: the primary goes on alone, the node backs off
        let m = pair(Fixed::new(&[1, 2], Ok(&[])), Fixed::new(&[], Err("connection closed")));
        assert_eq!(m.vspc_v2(req()).await.unwrap().added_chain_block_hashes.len(), 2);
        assert_eq!(m.board.snapshot()[1].state, "backoff");

        // the very next block contradicted: nothing applied; the third time the cross-checking node is replaced, the sixth
        // the primary's (the cross-checking node is the authority then)
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
