//! Node access: the [`ChainSource`] trait and a JSON wRPC client.
//!
//! The JSON calls are read into the indexer's own wire types (`types`), not the node's RPC types,
//! which drift from the JSON schema without a compile error. The integration test in
//! `tests/tn10_follow.rs` checks the schema against a live node. The Borsh calls (`borsh`,
//! `bridge`) use the node's own types from `kaspa-rpc-core`, of the same release as the node.
//!
//! # Submodules
//!
//! - `multi`, `failover` and `resolver` combine several nodes into one source. `judge` decides when
//!   a node counts as failed. `verify` checks a window from one node against another.
//! - When the chain authority's requests keep failing, the source hands the role to another node
//!   (`ChainSource::authority_failing`).

pub mod borsh;
pub mod bridge;
pub mod failover;
pub mod judge;
pub mod multi;
pub mod proof;
pub mod resolver;
pub mod tls;
pub mod types;
pub mod verify;
pub mod window;

use crate::hex::Hash32;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::future::Future;
use std::time::Duration;
use std::time::Instant;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use types::{AddressUtxo, ChainHashes, ChainIds, DagInfo, RawVspcResponse, ServerInfo, Verbosity, VspcRequest};

/// Node URLs as they may be shown or logged, with credentials and API keys left out
/// ([`kob_protocol::redact`]).
pub use kob_protocol::redact::redact_url;

/// A failed node request.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The connection failed; the next call reconnects.
    #[error("transport: {0}")]
    Transport(String),
    /// The node did not answer within the request's timeout.
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    /// The node answered with an error.
    #[error("node error: {0}")]
    Node(String),
    /// The answer is not what the schema says.
    #[error("cannot decode node response: {0}")]
    Decode(String),
    /// The answer is larger than the request allowed (`VspcRequest::max_bytes`). It is refused as
    /// its frame header arrives, its payload is never buffered, and the connection is closed.
    #[error("answer of {size} bytes exceeds the {limit}-byte limit of the request")]
    TooLarge { size: usize, limit: usize },
}

/// The largest answer a client takes at all (`WrpcConfig::max_message_bytes`), 1 GiB.
///
/// No window of an honest node comes near it. A node whose answer claims more failed the request
/// itself (`multi`).
pub const MAX_MESSAGE_BYTES: usize = 1 << 30;

/// The largest window answer (`getVirtualChainFromBlockV2`) a request takes when it names no
/// limit of its own, 256 MiB (R2C-M5).
///
/// One chain block accepts at most its merge set: about 250 blocks at 10 BPS, each of at most
/// 500,000 mass, some 125 KB of transactions. With the spent outputs at full verbosity that is
/// well under 128 MiB even as JSON, so no honest chain block reaches this. A larger honest window
/// is refused as its frame header arrives and split (`indexer::follower`). A hostile node can
/// thus make the client hold at most this much per request, not 1 GiB.
pub const WINDOW_MAX_BYTES: usize = 256 << 20;

/// The largest answer a small call takes, 16 MiB.
///
/// These are the status, blue scores, a block header, the DAG info and the chain hashes (at most
/// a few thousand). Their answers are a few KB to a few hundred KB. A node that sends more is
/// refused as the frame header arrives, instead of being buffered up to [`MAX_MESSAGE_BYTES`].
/// The UTXO queries and the windows keep their own limits.
pub const SMALL_CALL_MAX_BYTES: usize = 16 << 20;

/// The largest acceptance-data answer (`getVirtualChainFromBlock` with transaction ids), 64 MiB.
///
/// A cross-check window is tens to hundreds of KB, and a catch-up of hours stays far below it.
/// Without this cap one node could make the client buffer 1 GiB.
pub const CHAIN_IDS_MAX_BYTES: usize = 64 << 20;

/// A transport error that is the request's size limit ([`RpcError::TooLarge`]), else
/// [`RpcError::Transport`].
fn transport_error(e: tokio_tungstenite::tungstenite::Error) -> RpcError {
    use tokio_tungstenite::tungstenite::error::{CapacityError, Error};
    match e {
        Error::Capacity(CapacityError::MessageTooLong { size, max_size }) => RpcError::TooLarge { size, limit: max_size },
        e => RpcError::Transport(e.to_string()),
    }
}

/// What a node error means for the follower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeErrorKind {
    /// `cannot find header <hash>`: the node does not know the start hash. It never had it, or it
    /// pruned it.
    UnknownHash,
    /// `the queried hash does not have retention root on its chain`: the start hash is too old.
    RetentionRootMissing,
    /// `consensus is currently in a transitional ibd state`: retry later, never advance.
    TransitionalIbd,
    /// `block <hash> is too deep (before the current pruning point)`: the node can no longer
    /// prove at that block (`getSeqCommitLaneProof`). It stays so on that node.
    BlockTooDeep,
    Other,
}

impl RpcError {
    pub fn kind(&self) -> NodeErrorKind {
        match self {
            RpcError::Node(m) => classify_node_message(m),
            _ => NodeErrorKind::Other,
        }
    }

    /// The node's `getBlock` answer that it does not know the block (`cannot find header`, or a
    /// `block ... not found`). Any other node error (IBD, a busy node) proves nothing.
    pub fn is_unknown_block(&self) -> bool {
        matches!(self, RpcError::Node(m) if classify_node_message(m) == NodeErrorKind::UnknownHash || {
            let m = m.to_ascii_lowercase();
            m.contains("block") && m.contains("not found")
        })
    }

    /// The node's `block <hash> is not in the selected parent chain`: the block was on its chain
    /// a moment ago and a tip reorg took it off. The node is fine.
    pub fn left_selected_chain(&self) -> bool {
        matches!(self, RpcError::Node(m) if m.to_ascii_lowercase().contains("not in the selected parent chain"))
    }
}

pub fn classify_node_message(m: &str) -> NodeErrorKind {
    let m = m.to_ascii_lowercase();
    if m.contains("cannot find header") {
        NodeErrorKind::UnknownHash
    } else if m.contains("retention root") {
        NodeErrorKind::RetentionRootMissing
    } else if m.contains("transitional ibd") {
        NodeErrorKind::TransitionalIbd
    } else if m.contains("is too deep") {
        NodeErrorKind::BlockTooDeep
    } else {
        NodeErrorKind::Other
    }
}

/// What a check of a node's calls found ([`ChainSource::check_capabilities`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capable {
    /// It serves every call checked.
    Yes,
    /// It lacks one: it said so ("method not found" and the like).
    No(String),
    /// It closed the connection on a call twice while it answered `getServerInfo`: an older node,
    /// or a flaky link. Enough to demote another node, never to stop on the authority.
    Closed(String),
    /// The check could not tell (no answer, IBD). It is made again later.
    Unknown(String),
}

/// The node's answer that it has no such call. An older node closes the connection instead,
/// which [`probe_calls`] tells apart.
pub fn lacks_call(e: &RpcError) -> bool {
    let RpcError::Node(m) = e else { return false };
    let m = m.to_ascii_lowercase();
    ["method not found", "not implemented", "not supported", "unavailable in safe mode"].iter().any(|w| m.contains(w))
}

/// Whether `src` serves the calls the follower needs of a chain authority:
/// `getVirtualChainFromBlockV2` and, with `lane_proofs`, `getSeqCommitLaneProof`. Both are asked
/// at the sink, where the answer is empty.
///
/// Any answer of the call, an error such as "too deep" too, means the node has it. A node that
/// does not know a call closes the connection on it (`workflow-rpc`). That is [`Capable::Closed`]
/// when it happens twice while the node answers `getServerInfo` each time.
pub async fn probe_calls<S: ChainSource + ?Sized>(src: &S, lane_proofs: bool) -> Capable {
    let sink = match src.dag_info().await {
        Ok(d) => d.sink,
        Err(e) => return Capable::Unknown(format!("getBlockDagInfo: {e}")),
    };
    let vspc = || async move { src.vspc_v2(VspcRequest::new(sink, src.authority_verbosity(), None)).await.map(|_| ()) };
    let c = probe_call(src, "getVirtualChainFromBlockV2", vspc).await;
    if c != Capable::Yes || !lane_proofs {
        return c;
    }
    let lane = Hash32(proof::native_lane().as_bytes());
    probe_call(src, "getSeqCommitLaneProof", || async move { src.lane_proof(sink, lane).await.map(|_| ()) }).await
}

async fn probe_call<S, F, Fut>(src: &S, method: &str, call: F) -> Capable
where
    S: ChainSource + ?Sized,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), RpcError>>,
{
    let mut closed = String::new();
    for _ in 0..2 {
        match call().await {
            Ok(()) => return Capable::Yes,
            Err(e) if lacks_call(&e) => return Capable::No(format!("it does not serve {method} ({e})")),
            Err(e @ RpcError::Transport(_)) => match src.server_info().await {
                Ok(_) => closed = e.to_string(),
                Err(i) => return Capable::Unknown(format!("{method}: {e}; getServerInfo: {i}")),
            },
            Err(e) if matches!(e, RpcError::Timeout(_)) || e.kind() == NodeErrorKind::TransitionalIbd => {
                return Capable::Unknown(format!("{method}: {e}"))
            }
            // Any other answer comes from the call itself.
            Err(_) => return Capable::Yes,
        }
    }
    Capable::Closed(format!("it closes the connection on {method} ({closed}) while it answers getServerInfo"))
}

/// What the source made of a chain authority's answer that contradicts the store
/// ([`ChainSource::authority_contradicted`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreContradiction {
    /// The node was given up for another one (a public node): the follower asks that one.
    Replaced,
    /// The store halts at the contradiction (state `gap`). `Some(why)` means the source has reason
    /// to blame the store itself, because two public nodes on different hosts agree with each other
    /// against it. The gap reason says so.
    Halt(Option<String>),
}

/// Everything the indexer asks of a node. Implemented by [`WrpcClient`] and by the in-memory
/// node used in tests.
pub trait ChainSource: Send + Sync + 'static {
    fn vspc_v2(&self, req: VspcRequest) -> impl Future<Output = Result<RawVspcResponse, RpcError>> + Send;
    fn dag_info(&self) -> impl Future<Output = Result<DagInfo, RpcError>> + Send;
    fn server_info(&self) -> impl Future<Output = Result<ServerInfo, RpcError>> + Send;
    /// Unspent outputs of the given addresses (needs `--utxoindex` on the node).
    fn utxos_by_addresses(&self, addresses: &[String]) -> impl Future<Output = Result<Vec<AddressUtxo>, RpcError>> + Send;
    /// Whether the node still knows this block (header present). Only a node "not found" answer
    /// ([`RpcError::is_unknown_block`]) is `Ok(false)`; any other error is an error.
    fn block_exists(&self, hash: Hash32) -> impl Future<Output = Result<bool, RpcError>> + Send;
    /// Blue score of the node's sink (`getSinkBlueScore`): bounds a VSPC batch (`rpc::window`).
    fn sink_blue_score(&self) -> impl Future<Output = Result<u64, RpcError>> + Send;
    /// The sink blue score that every node vouching for this source's chain reaches: the lowest of
    /// their answers. Another source is compared with it (`failover`), so that one node that
    /// over-reports its sink cannot make that source look behind.
    ///
    /// The default is [`ChainSource::sink_blue_score`], the one node's own.
    fn vouched_sink_blue_score(&self) -> impl Future<Output = Result<u64, RpcError>> + Send {
        self.sink_blue_score()
    }
    /// The cross-checking node's unspent outputs of the given addresses, when this source has one
    /// (`None`: a single node). A rebase closes an order only when neither node has its output.
    fn cross_check_utxos(&self, addresses: &[String]) -> impl Future<Output = Result<Option<Vec<AddressUtxo>>, RpcError>> + Send {
        async move {
            let _ = addresses;
            Ok(None)
        }
    }
    /// Blue score of a block the node knows (`None`: the node does not know it).
    fn block_blue_score(&self, hash: Hash32) -> impl Future<Output = Result<Option<u64>, RpcError>> + Send;
    /// DAA score of a block the node knows, from its header's `daaScore`. `None` means the node
    /// does not know it. It is the DAA of a cursor set to that block (`kobd db rebase --to`).
    fn block_daa_score(&self, hash: Hash32) -> impl Future<Output = Result<Option<u64>, RpcError>> + Send {
        async move {
            let _ = hash;
            Err(RpcError::Node("getBlock (header daaScore) is not supported by this source".into()))
        }
    }
    /// The selected chain from `start` as block hashes only (`getVirtualChainFromBlock` without
    /// transaction ids). The follower plans its parallel windows on it.
    fn chain_hashes(&self, start: Hash32) -> impl Future<Output = Result<ChainHashes, RpcError>> + Send;
    /// The selected chain from `start` with the transaction ids every chain block accepted
    /// (`getVirtualChainFromBlock` with ids). It is bounded like a VSPC request by
    /// `minConfirmationCount`. A window from another node is checked against this acceptance data
    /// (`rpc::verify`).
    fn chain_with_ids(
        &self,
        start: Hash32,
        min_confirmations: Option<u64>,
    ) -> impl Future<Output = Result<ChainIds, RpcError>> + Send {
        async move {
            let _ = (start, min_confirmations);
            Err(RpcError::Node("getVirtualChainFromBlock with transaction ids is not supported by this source".into()))
        }
    }
    /// One window `(start, end]` of the selected chain the follower planned (`indexer::prefetch`),
    /// from whichever node the source picks. A single node picks itself. The answer is the
    /// primary's, or it is checked against the primary ([`Fetched::origin`]).
    fn fetch_window(&self, w: WindowRequest) -> impl Future<Output = Result<Fetched, RpcError>> + Send {
        async move { fetch_window_from(self, &w, self.authority_verbosity(), Origin::PRIMARY).await }
    }
    /// [`ChainSource::fetch_window`] from the primary only (the chain authority).
    fn fetch_window_primary(&self, w: WindowRequest) -> impl Future<Output = Result<Fetched, RpcError>> + Send {
        async move { fetch_window_from(self, &w, self.authority_verbosity(), Origin::PRIMARY).await }
    }
    /// The source serves what chain proofs need (`rpc::proof`): `Full` windows, lane proofs and
    /// blocks. The node clients do. The default, for test sources, does not, and nothing is proven.
    fn proves_chain(&self) -> bool {
        false
    }
    /// The verbosity of the chain authority's windows: `Full` when the source proves the chain.
    fn authority_verbosity(&self) -> Verbosity {
        if self.proves_chain() {
            Verbosity::Full
        } else {
            Verbosity::High
        }
    }
    /// A KIP-21 lane proof of `lane` at the chain block `block` (`getSeqCommitLaneProof`), with
    /// the node that served it: a proof that fails blames that node.
    fn lane_proof(
        &self,
        block: Hash32,
        lane: Hash32,
    ) -> impl Future<Output = Result<(kaspa_rpc_core::GetSeqCommitLaneProofResponse, Origin), RpcError>> + Send {
        async move {
            let _ = (block, lane);
            Err(RpcError::Node("getSeqCommitLaneProof is not supported by this source".into()))
        }
    }
    /// A block (`getBlock`), with its transactions when `txs`, and the node that served it.
    /// `None`: the node does not know it.
    fn block(
        &self,
        hash: Hash32,
        txs: bool,
    ) -> impl Future<Output = Result<Option<(kaspa_consensus_core::block::Block, Origin)>, RpcError>> + Send {
        async move {
            let _ = (hash, txs);
            Err(RpcError::Node("getBlock is not supported by this source".into()))
        }
    }
    /// The node behind `origin` served chain data that failed a chain proof (`why`). The source
    /// stops using it.
    fn chain_refused(&self, origin: Origin, why: &str) {
        let _ = (origin, why);
    }
    /// Whether a second node vouches for the chain authority's answers now.
    fn cross_check(&self) -> CrossCheck {
        CrossCheck::single("a single node")
    }
    /// The node behind `origin` gave data the primary contradicts: stop using it.
    fn report_lie(&self, origin: Origin, why: &str) {
        let _ = (origin, why);
    }
    /// The chain blocks `runs` of the authority's own answer `raw` (from `start`), the ones that
    /// hold a transaction that can matter to KOB, compared in full (signature scripts and spent
    /// outputs, `verify::body_difference`) with the cross-checking node's copies (C-H1: a same-id
    /// copy with forged signature scripts in a real block passes the chain proofs).
    ///
    /// `Ok(true)`: the cross-checking node reports the same. `Ok(false)`: there is no node to ask (a
    /// single node, or the authority goes on alone, `multi::CrossCheckPolicy::single_after`, for
    /// the ids or for these copies). `Err(why)`: they differ, or the node gave no copies and
    /// `single_after` has not passed yet; nothing of `raw` is applied.
    fn confirm_bodies(
        &self,
        start: Hash32,
        raw: &RawVspcResponse,
        runs: &[BodyRun],
    ) -> impl Future<Output = Result<bool, String>> + Send {
        async move {
            let _ = (start, raw, runs);
            Ok(false)
        }
    }
    /// Whether the last chain answer of the authority came with chain past it that another node
    /// showed: a header that hashes to its claimed id, with its proof of work, well past the
    /// authority's answer (`multi::MultiNode`, R4C1-M2). It never counts against the authority by
    /// itself. The follower uses it only to tell a halted network from an authority that holds the
    /// chain back, once the cursor is old by the local clock. The default is `false`.
    fn chain_beyond_shown(&self) -> bool {
        false
    }
    /// The primary's copy of `runs` of a window `raw` from another node (`origin`, from `start`)
    /// differs from it (`why`). The default drops that node ([`ChainSource::report_lie`]). A
    /// source with a third node asks it first when the primary is a public node too
    /// (`multi::MultiNode`, R4C1-L1).
    fn window_contradicted(
        &self,
        origin: Origin,
        start: Hash32,
        raw: &RawVspcResponse,
        runs: &[BodyRun],
        why: &str,
    ) -> impl Future<Output = ()> + Send {
        let _ = (start, raw, runs);
        self.report_lie(origin, why);
        async {}
    }
    /// The chain authority's answer contradicts the store at the cursor `at` (`why`). The result is
    /// [`StoreContradiction::Replaced`] when the source gave that node up for another one (a public
    /// node, `rpc::resolver`), so the follower asks again instead of halting.
    ///
    /// The default halts. A node of one's own is not replaced, and the store halts at the
    /// contradiction.
    fn authority_contradicted(&self, at: Hash32, why: &str) -> StoreContradiction {
        let _ = (at, why);
        StoreContradiction::Halt(None)
    }
    /// The chain authority's chain requests keep failing on the wire while the cursor `at` does not
    /// move, as `why` says. The chain requests are VSPC, the chain hashes and its copies of chain
    /// blocks. `indexer::follower` decides when.
    ///
    /// Returns `true` when the source made another node the chain authority
    /// (`multi::MultiNode::authority_failing`). The follower then asks that node at once.
    ///
    /// The default returns `false`, because there is nothing to hand over to. A single node is
    /// retried with the follower's backoff.
    fn authority_failing(&self, at: Hash32, why: &str) -> impl Future<Output = bool> + Send {
        async move {
            let _ = (at, why);
            false
        }
    }
    /// The chain authority's error `e` says that it does not know the cursor `at` (`cannot find
    /// header`) or that `at` is below its retention root. The follower would rewind or halt the
    /// store on it (`indexer::follower`).
    ///
    /// `Ok(kind)` is the claim to act on. `Err(why)` means it is not confirmed: nothing is rewound
    /// or halted, and the follower retries. The default confirms `e`, because a single node is the
    /// only one to ask. A source with a cross-checking node asks that node too
    /// (`multi::MultiNode`).
    fn confirm_unknown_cursor(&self, at: Hash32, e: &RpcError) -> impl Future<Output = Result<NodeErrorKind, String>> + Send {
        let kind = e.kind();
        async move {
            let _ = at;
            Ok(kind)
        }
    }
    /// Whether the chain authority serves the calls the follower needs ([`probe_calls`]). The
    /// follower asks once per authority ([`ChainSource::authority_key`]). A source over several
    /// nodes demotes another node that lacks one (`multi`). The default, for test sources, is
    /// [`Capable::Yes`].
    fn check_capabilities(&self, lane_proofs: bool) -> impl Future<Output = Capable> + Send {
        async move {
            let _ = lane_proofs;
            Capable::Yes
        }
    }
    /// Which nodes serve now. A new value means a new chain authority, checked again.
    fn authority_key(&self) -> String {
        String::new()
    }
    /// Per-node counters for `/v1/health` (empty for a single node).
    fn node_stats(&self) -> Vec<multi::NodeStatus> {
        vec![]
    }
    /// Which node list serves now, behind a [`failover::Failover`]. `None` means no failover is
    /// configured.
    fn source_status(&self) -> Option<failover::FailoverStatus> {
        None
    }
}

/// Whether a second node vouches for the chain authority's answers (`/v1/health` `cross_checked`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CrossCheck {
    /// The last answer applied was vouched for by a second node.
    pub checked: bool,
    /// Why not, when it was not.
    pub reason: Option<String>,
    /// A cross-checking node is configured, but none answered in time and the chain authority
    /// goes on alone (`rpc::multi::CrossCheckPolicy::single_after`).
    pub alone: bool,
}

impl CrossCheck {
    pub fn single(why: &str) -> CrossCheck {
        CrossCheck { checked: false, reason: Some(why.to_string()), alone: false }
    }
}

/// Where a window came from.
///
/// `node` is the position of the node in the source's list, with 0 for the primary. `trusted` says
/// whether it is the primary's own answer or another node's, which is checked against the primary
/// (`rpc::verify`). `tier` is the node list it came from behind a [`failover::Failover`]. It is 0
/// for the configured nodes and 1 for the fallback, and always 0 without a failover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub node: usize,
    pub trusted: bool,
    pub tier: u8,
}

impl Origin {
    pub const PRIMARY: Origin = Origin { node: 0, trusted: true, tier: 0 };
}

/// One window of the parallel fetch: the chain blocks after `start` up to and including `end`
/// (`indexer::prefetch`).
#[derive(Debug, Clone, Copy)]
pub struct WindowRequest {
    pub start: Hash32,
    pub end: Hash32,
    pub timeout_scale: u32,
    /// `minConfirmationCount` the caller wants anyway.
    pub min_confirmations: Option<u64>,
    /// The answer's size limit (`VspcRequest::max_bytes`).
    pub max_bytes: Option<usize>,
    /// The blue score of `end` and of the node's sink, when the caller knows them. The node is not
    /// asked then.
    pub end_blue: Option<u64>,
    pub sink_blue: Option<u64>,
}

impl WindowRequest {
    pub fn new(start: Hash32, end: Hash32) -> Self {
        WindowRequest { start, end, timeout_scale: 1, min_confirmations: None, max_bytes: None, end_blue: None, sink_blue: None }
    }
}

/// A run of chain blocks of a window to compare with another node ([`ChainSource::confirm_bodies`]):
/// the request for them, and their position and number in the window.
#[derive(Debug, Clone, Copy)]
pub struct BodyRun {
    pub req: WindowRequest,
    pub first: usize,
    pub len: usize,
}

/// The first difference between `ours[first..first + len]` and `theirs`, the same chain blocks
/// fetched again (from position 0), or `None`. `Err` when `theirs` does not report them.
pub fn run_difference(ours: &RawVspcResponse, theirs: &RawVspcResponse, run: &BodyRun) -> Result<Option<String>, String> {
    let (first, len) = (run.first, run.len);
    if !theirs.removed_chain_block_hashes.is_empty() || theirs.added_chain_block_hashes.len() < len {
        return Err(format!("the node no longer reports the {len} chain blocks after {}", ours.added_chain_block_hashes[first]));
    }
    for k in 0..len {
        let i = first + k;
        if theirs.added_chain_block_hashes[k] != ours.added_chain_block_hashes[i]
            || theirs.chain_block_accepted_transactions.len() <= k
        {
            return Err(format!("the node's chain differs at {}", ours.added_chain_block_hashes[i]));
        }
        if let Some(d) =
            verify::body_difference(&ours.chain_block_accepted_transactions[i], &theirs.chain_block_accepted_transactions[k])
        {
            return Ok(Some(d));
        }
    }
    Ok(None)
}

/// A window as a node returned it.
pub struct Fetched {
    pub raw: RawVspcResponse,
    /// Blue score of the node's sink and of the window's last chain block when the request was
    /// sent.
    pub sink_blue: u64,
    pub end_blue: u64,
    pub elapsed: Duration,
    pub origin: Origin,
}

/// Fetches one window from `src`: `(start, end]` at `verbosity`. The request is bounded to end at
/// `end` through `minConfirmationCount`, relative to the sink of `src`. Blocks past `end` may still
/// come back, because the sink moves while the request travels.
pub async fn fetch_window_from<S: ChainSource + ?Sized>(
    src: &S,
    w: &WindowRequest,
    verbosity: Verbosity,
    origin: Origin,
) -> Result<Fetched, RpcError> {
    let t0 = Instant::now();
    let end = w.end;
    let end_blue = match w.end_blue {
        Some(b) => b,
        None => src
            .block_blue_score(end)
            .await?
            .ok_or_else(|| RpcError::Node(format!("prefetch: the node no longer knows the window end {end}")))?,
    };
    let sink_blue = match w.sink_blue {
        Some(b) => b,
        None => src.sink_blue_score().await?,
    };
    let mut req = VspcRequest::new(w.start, verbosity, window_min_confirmations(sink_blue, end_blue, w.min_confirmations));
    req.timeout_scale = w.timeout_scale;
    req.max_bytes = w.max_bytes;
    let raw = src.vspc_v2(req).await?;
    Ok(Fetched { raw, sink_blue, end_blue, elapsed: t0.elapsed(), origin })
}

/// The `minConfirmationCount` that ends a request at the chain block of blue score `end_blue` while
/// the sink is at `sink_blue`, or the caller's `extra` when that is larger. The node keeps a chain
/// block while `sink - blue > minConfirmationCount`.
pub fn window_min_confirmations(sink_blue: u64, end_blue: u64, extra: Option<u64>) -> Option<u64> {
    match (sink_blue.saturating_sub(end_blue.saturating_add(1)), extra) {
        (0, e) => e,
        (b, e) => Some(b.max(e.unwrap_or(0))),
    }
}

#[derive(Debug, Clone)]
pub struct WrpcConfig {
    /// The node. It is a JSON wRPC URL, a Borsh one (`bridge::is_borsh_url`, every call in Borsh)
    /// or a resolver entry (`resolver::is_resolver`, a public node, Borsh).
    pub url: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Upper bound for one response. A `High` VSPC batch is large (tens of MB on TN10).
    pub max_message_bytes: usize,
    /// Connections reserved for `getVirtualChainFromBlockV2`. With 0, VSPC shares the one
    /// connection of every other call. The follower fetches that many windows at once while it
    /// catches up. The other calls keep their own connection, so a small call never waits behind a
    /// large batch.
    pub vspc_connections: usize,
    /// Connections for the small calls (every call but the VSPC batches), one request in flight
    /// each. The first is always there. The others open only while that many small calls are in
    /// flight at once, as the follower's chain proofs are: the merged blocks and lane proofs of a
    /// batch go out together (`indexer::follower::PROOF_FETCHES`). 1 (the default) keeps every small
    /// call on one connection, one after another.
    pub small_connections: usize,
    /// The node's Borsh wRPC endpoint. With VSPC connections of its own, the windows of transaction
    /// bodies and the accepted transaction ids travel in Borsh over it, which is a fraction of
    /// their JSON size ([`borsh`]). Every other call stays JSON on `url`.
    pub borsh_url: Option<String>,
}

impl WrpcConfig {
    pub fn new(url: impl Into<String>) -> Self {
        WrpcConfig {
            url: url.into(),
            request_timeout: Duration::from_secs(180),
            connect_timeout: Duration::from_secs(15),
            max_message_bytes: MAX_MESSAGE_BYTES,
            vspc_connections: 0,
            small_connections: 1,
            borsh_url: None,
        }
    }

    /// Every call travels in Borsh, not only the windows. `url` is a Borsh endpoint or a resolver
    /// entry.
    pub fn all_borsh(&self) -> bool {
        all_borsh(&self.url)
    }
}

/// Whether a configured node URL takes every call in Borsh. That holds for a Borsh endpoint
/// (`bridge::is_borsh_url`) and for a resolver entry, because public nodes serve Borsh only.
pub fn all_borsh(url: &str) -> bool {
    bridge::is_borsh_url(url) || resolver::is_resolver(url)
}

/// The node a configured URL stands for now ([`resolver::endpoint`]). It is the URL itself, or the
/// public node of a resolver entry, resolved on the blocking pool.
pub async fn endpoint(url: &str) -> Result<String, RpcError> {
    if !resolver::is_resolver(url) {
        return Ok(url.to_string());
    }
    let u = url.to_string();
    tokio::task::spawn_blocking(move || resolver::endpoint(&u))
        .await
        .map_err(|e| RpcError::Transport(format!("resolver: {e}")))?
        .map_err(RpcError::Transport)
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// A connection and the node it goes to (a resolver entry's public node; else the URL).
type Conn = (Ws, String);
type Slot = tokio::sync::Mutex<Option<Conn>>;

/// JSON wRPC client: one request in flight per connection, `small_connections` for the small calls
/// plus `vspc_connections` for VSPC batches. Reconnects lazily after any failure.
pub struct WrpcClient {
    cfg: WrpcConfig,
    conn: Slot,
    /// The small calls' connections beyond `conn`, used while it is busy.
    small: Vec<Slot>,
    next_small: std::sync::atomic::AtomicUsize,
    vspc: Vec<Slot>,
    next_vspc: std::sync::atomic::AtomicUsize,
    next_id: std::sync::atomic::AtomicU64,
    /// The node the last connection went to, which is what a failure reports. It is the public node
    /// of a resolver entry, else the URL.
    node: std::sync::Mutex<String>,
}

#[derive(Deserialize)]
struct ErrBody {
    message: String,
}

#[derive(Deserialize)]
struct Response<'a> {
    id: Option<u64>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    error: Option<ErrBody>,
}

/// A request as it goes out.
enum Wire<'a> {
    Json {
        method: &'a str,
        params: serde_json::Value,
    },
    /// Builds the request frame for a request id.
    Borsh {
        frame: Box<dyn FnOnce(u64) -> Vec<u8> + Send + 'a>,
    },
}

/// The body of the matching answer.
enum Reply<'a> {
    /// The JSON `params`.
    Json(&'a str),
    /// The Borsh body, and the size of the whole frame.
    Borsh { body: &'a [u8], wire_bytes: usize },
}

impl WrpcClient {
    pub fn new(cfg: WrpcConfig) -> Self {
        let vspc = (0..cfg.vspc_connections).map(|_| Slot::new(None)).collect();
        let small = (1..cfg.small_connections.max(1)).map(|_| Slot::new(None)).collect();
        let node = std::sync::Mutex::new(cfg.url.clone());
        WrpcClient {
            cfg,
            conn: Slot::new(None),
            small,
            next_small: std::sync::atomic::AtomicUsize::new(0),
            vspc,
            next_vspc: std::sync::atomic::AtomicUsize::new(0),
            next_id: std::sync::atomic::AtomicU64::new(1),
            node,
        }
    }

    /// A connection for a small call: the first idle one in order, so the further ones open only
    /// while calls overlap, else the next in turn.
    async fn small_slot(&self) -> tokio::sync::MutexGuard<'_, Option<Conn>> {
        if self.small.is_empty() {
            return self.conn.lock().await;
        }
        if let Ok(g) = self.conn.try_lock() {
            return g;
        }
        for s in &self.small {
            if let Ok(g) = s.try_lock() {
                return g;
            }
        }
        match self.next_small.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % (self.small.len() + 1) {
            0 => self.conn.lock().await,
            k => self.small[k - 1].lock().await,
        }
    }

    /// A VSPC connection: an idle one if there is one (round robin), else the next in turn.
    async fn vspc_slot(&self) -> tokio::sync::MutexGuard<'_, Option<Conn>> {
        if self.vspc.is_empty() {
            return self.conn.lock().await;
        }
        let first = self.next_vspc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for i in 0..self.vspc.len() {
            if let Ok(g) = self.vspc[(first + i) % self.vspc.len()].try_lock() {
                return g;
            }
        }
        self.vspc[first % self.vspc.len()].lock().await
    }

    /// A connection whose answers may be at most `cap` bytes. A frame or message above it fails as
    /// [`RpcError::TooLarge`].
    async fn connect(&self, url: &str, cap: usize) -> Result<Conn, RpcError> {
        let url = endpoint(url).await?;
        *self.node.lock().unwrap_or_else(|e| e.into_inner()) = url.clone();
        let ws_cfg = WebSocketConfig::default().max_message_size(Some(cap)).max_frame_size(Some(cap));
        let fut = tokio_tungstenite::connect_async_with_config(url.as_str(), Some(ws_cfg), false);
        // a dropped public node's one reconnect has a short timeout (`resolver::failed`)
        let limit = resolver::connect_timeout(&self.cfg.url, self.cfg.connect_timeout);
        match tokio::time::timeout(limit, fut).await {
            Err(_) => Err(RpcError::Timeout(limit)),
            Ok(Err(e)) => Err(RpcError::Transport(e.to_string())),
            Ok(Ok((ws, _))) => Ok((ws, url)),
        }
    }

    /// A connection of a resolver entry to a node the entry no longer uses. It failed elsewhere in
    /// the process, or the authority took the other node over. It is replaced before the next
    /// request.
    fn stale(&self, node: &str) -> bool {
        resolver::is_resolver(&self.cfg.url) && resolver::current(&self.cfg.url).as_deref() != Some(node)
    }

    /// Send one request and return the raw `params` of the matching response.
    pub async fn call_raw(&self, method: &str, params: serde_json::Value) -> Result<String, RpcError> {
        self.call_raw_within(method, params, self.cfg.request_timeout).await
    }

    /// The node the last connection went to (a resolver entry's public node; else the URL).
    fn node_in_use(&self) -> String {
        self.node.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// [`WrpcClient::call_raw`] with another timeout. A timeout drops the connection, so a late
    /// answer is never read as the answer to the next request.
    ///
    /// These are the small calls. A timeout of one counts against the node ([`judge::timed_out`]).
    /// A VSPC batch's timeout does not count, because the windows adapt to it. An answer may be at
    /// most [`SMALL_CALL_MAX_BYTES`].
    pub async fn call_raw_within(&self, method: &str, params: serde_json::Value, timeout: Duration) -> Result<String, RpcError> {
        self.call_raw_capped(method, params, timeout, SMALL_CALL_MAX_BYTES).await
    }

    /// [`WrpcClient::call_raw_within`] whose answer may be at most `cap` bytes.
    ///
    /// The small calls share `small_connections` (one by default), and a connection opened with
    /// another limit is replaced first. So a call with another limit costs a reconnect. Only the UTXO queries have
    /// one, and they are rare outside the Executor's own client (`matcher::node`).
    async fn call_raw_capped(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        cap: usize,
    ) -> Result<String, RpcError> {
        let r = self.call_small(method, params, timeout, cap.min(self.cfg.max_message_bytes)).await;
        if matches!(r, Err(RpcError::Timeout(_))) {
            judge::timed_out(&self.cfg.url, &self.node_in_use());
        }
        r
    }

    async fn call_small(&self, method: &str, params: serde_json::Value, timeout: Duration, cap: usize) -> Result<String, RpcError> {
        if self.cfg.all_borsh() {
            // the same JSON in and out, over Borsh (`bridge`)
            let enc = bridge::encode(method, &params).map_err(RpcError::Decode)?;
            let parse = |p: Reply<'_>| match p {
                Reply::Borsh { body, .. } => bridge::decode(method, body).map(|v| v.to_string()).map_err(RpcError::Decode),
                Reply::Json(_) => Err(RpcError::Decode("a JSON answer to a Borsh request".into())),
            };
            let wire = Wire::Borsh { frame: Box::new(move |id| enc.frame(id)) };
            return self.call_on(self.small_slot().await, wire, timeout, cap, parse).await;
        }
        let parse = |p: Reply<'_>| match p {
            Reply::Json(p) => Ok(p.to_string()),
            Reply::Borsh { .. } => Err(RpcError::Decode("a Borsh answer to a JSON request".into())),
        };
        self.call_on(self.small_slot().await, Wire::Json { method, params }, timeout, cap, parse).await
    }

    /// The Borsh endpoint the VSPC connections use, if any. `None` means JSON, and also means VSPC
    /// shares the JSON connection. A node that takes every call in Borsh is its own endpoint.
    pub fn borsh_url(&self) -> Option<&str> {
        if self.cfg.all_borsh() {
            return Some(self.cfg.url.as_str());
        }
        self.cfg.borsh_url.as_deref().filter(|_| !self.vspc.is_empty())
    }

    /// A failed connection, or one the node dropped, is a transport error. A request that only took
    /// longer than its timeout is not a lost connection. A VSPC batch's timeout adapts the windows
    /// (`rpc::window`), and the small calls' timeouts are counted by [`judge::timed_out`].
    ///
    /// A resolver entry moves to another public node ([`resolver::failed`]).
    fn report(&self, e: &RpcError, node: Option<&str>) {
        let connecting = node.is_none();
        let node_failed = matches!(e, RpcError::Transport(_)) || (connecting && matches!(e, RpcError::Timeout(_)));
        if node_failed && resolver::is_resolver(&self.cfg.url) {
            let node = node.map(str::to_string).unwrap_or_else(|| self.node_in_use());
            resolver::failed(&self.cfg.url, &node);
        }
    }

    /// One request on the connection behind `guard`. `parse` reads the response's `params` in
    /// place. A VSPC batch is tens of MB, so it is parsed straight from the frame and not copied
    /// first.
    ///
    /// A request that times out, fails in transport or is cancelled closes its connection, so a
    /// late answer is never left in the socket. A request is cancelled when its future is dropped,
    /// which happens when the follower aborts prefetched windows it no longer needs.
    ///
    /// The answer may be at most `cap` bytes. A connection opened with another limit is replaced
    /// first. A VSPC window of the prefetch asks for its own limit (`VspcRequest::max_bytes`).
    async fn call_on<T>(
        &self,
        guard: tokio::sync::MutexGuard<'_, Option<Conn>>,
        wire: Wire<'_>,
        timeout: Duration,
        cap: usize,
        parse: impl FnOnce(Reply<'_>) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        struct Pending<'a> {
            guard: tokio::sync::MutexGuard<'a, Option<Conn>>,
            settled: bool,
        }
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                if !self.settled {
                    *self.guard = None;
                }
            }
        }
        let mut p = Pending { guard, settled: false };
        if p.guard.as_ref().is_some_and(|(ws, node)| ws.get_config().max_message_size != Some(cap) || self.stale(node)) {
            *p.guard = None;
        }
        // A connection is used for one encoding only: the JSON one, or a VSPC connection to the
        // Borsh endpoint.
        let url = match &wire {
            Wire::Json { .. } => self.cfg.url.as_str(),
            Wire::Borsh { .. } if self.cfg.all_borsh() => self.cfg.url.as_str(),
            Wire::Borsh { .. } => self.cfg.borsh_url.as_deref().unwrap_or(self.cfg.url.as_str()),
        };
        if p.guard.is_none() {
            match self.connect(url, cap).await {
                Ok(ws) => *p.guard = Some(ws),
                Err(e) => {
                    self.report(&e, None);
                    return Err(e);
                }
            }
        }
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = match wire {
            Wire::Json { method, params } => {
                Message::text(serde_json::json!({"id": id, "method": method, "params": params}).to_string())
            }
            Wire::Borsh { frame } => Message::binary(frame(id)),
        };
        let (ws, node) = p.guard.as_mut().expect("connected above");
        let node = node.clone();
        let res = tokio::time::timeout(timeout, async {
            ws.send(req).await.map_err(|e| RpcError::Transport(e.to_string()))?;
            loop {
                let msg = match ws.next().await {
                    None => return Err(RpcError::Transport("connection closed".into())),
                    Some(Err(e)) => return Err(transport_error(e)),
                    Some(Ok(m)) => m,
                };
                match msg {
                    Message::Text(text) => {
                        let resp: Response = match serde_json::from_str(text.as_str()) {
                            Ok(r) => r,
                            Err(_) => continue, // notification or foreign frame
                        };
                        if resp.id != Some(id) {
                            continue;
                        }
                        if let Some(e) = resp.error {
                            return Err(RpcError::Node(e.message));
                        }
                        return match resp.params {
                            Some(p) => Ok(parse(Reply::Json(p.get()))),
                            None => Err(RpcError::Decode("response without params".into())),
                        };
                    }
                    Message::Binary(bytes) => match borsh::parse_reply(&bytes[..]) {
                        Ok(borsh::Reply::Ok { id: Some(i), body }) if i == id => {
                            return Ok(parse(Reply::Borsh { body, wire_bytes: bytes.len() }));
                        }
                        Ok(borsh::Reply::Err { id: Some(i), message }) if i == id => return Err(RpcError::Node(message)),
                        Ok(_) => continue, // a notification or another request's answer
                        Err(e) => return Err(RpcError::Decode(e)),
                    },
                    Message::Close(_) => return Err(RpcError::Transport("closed by node".into())),
                    _ => continue,
                }
            }
        })
        .await;
        match res {
            Ok(Ok(v)) => {
                p.settled = true;
                v
            }
            Ok(Err(e)) => {
                // a node error leaves the connection usable
                p.settled = matches!(e, RpcError::Node(_));
                self.report(&e, Some(&node));
                Err(e)
            }
            Err(_) => Err(RpcError::Timeout(timeout)),
        }
    }
    /// A Borsh request on a VSPC connection (`frame` builds the request frame for a request id).
    async fn call_borsh<T>(
        &self,
        frame: impl FnOnce(u64) -> Vec<u8> + Send + 'static,
        timeout: Duration,
        cap: usize,
        parse: impl FnOnce(&[u8], usize) -> Result<T, String>,
    ) -> Result<T, RpcError> {
        let parse = |r: Reply<'_>| match r {
            Reply::Borsh { body, wire_bytes } => parse(body, wire_bytes).map_err(RpcError::Decode),
            Reply::Json(_) => Err(RpcError::Decode("a JSON answer to a Borsh request".into())),
        };
        self.call_on(self.vspc_slot().await, Wire::Borsh { frame: Box::new(frame) }, timeout, cap, parse).await
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: serde_json::Value) -> Result<T, RpcError> {
        self.call_capped(method, params, SMALL_CALL_MAX_BYTES).await
    }

    async fn call_capped<T: DeserializeOwned>(&self, method: &str, params: serde_json::Value, cap: usize) -> Result<T, RpcError> {
        let raw = self.call_raw_capped(method, params, self.cfg.request_timeout, cap).await?;
        serde_json::from_str(&raw).map_err(|e| RpcError::Decode(format!("{method}: {e}")))
    }
}

impl ChainSource for WrpcClient {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        let timeout = self.cfg.request_timeout.saturating_mul(req.timeout_scale.max(1));
        let cap = req.max_bytes.unwrap_or(WINDOW_MAX_BYTES).min(self.cfg.max_message_bytes);
        if self.borsh_url().is_some() {
            let (start, level, min_conf) = (req.start_hash, req.data_verbosity_level, req.min_confirmation_count);
            let frame = move |id| borsh::vspc_v2_request(id, start, level, min_conf);
            return self.call_borsh(frame, timeout, cap, borsh::decode_vspc_v2).await;
        }
        let params = serde_json::to_value(&req).map_err(|e| RpcError::Decode(e.to_string()))?;
        let parse = |raw: Reply<'_>| {
            let Reply::Json(raw) = raw else { return Err(RpcError::Decode("a Borsh answer to a JSON request".into())) };
            let mut r: RawVspcResponse =
                serde_json::from_str(raw).map_err(|e| RpcError::Decode(format!("getVirtualChainFromBlockV2: {e}")))?;
            r.wire_bytes = raw.len();
            Ok(r)
        };
        let wire = Wire::Json { method: "getVirtualChainFromBlockV2", params };
        self.call_on(self.vspc_slot().await, wire, timeout, cap, parse).await
    }

    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            blue_score: u64,
        }
        Ok(self.call::<Resp>("getSinkBlueScore", serde_json::json!({})).await?.blue_score)
    }

    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Header {
            blue_score: u64,
        }
        #[derive(Deserialize)]
        struct Block {
            header: Header,
        }
        #[derive(Deserialize)]
        struct Resp {
            block: Block,
        }
        match self.call::<Resp>("getBlock", serde_json::json!({ "hash": hash.to_hex(), "includeTransactions": false })).await {
            Ok(r) => Ok(Some(r.block.header.blue_score)),
            Err(e) if e.is_unknown_block() => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn block_daa_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Header {
            daa_score: u64,
        }
        #[derive(Deserialize)]
        struct Block {
            header: Header,
        }
        #[derive(Deserialize)]
        struct Resp {
            block: Block,
        }
        match self.call::<Resp>("getBlock", serde_json::json!({ "hash": hash.to_hex(), "includeTransactions": false })).await {
            Ok(r) => Ok(Some(r.block.header.daa_score)),
            Err(e) if e.is_unknown_block() => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.call(
            "getVirtualChainFromBlock",
            serde_json::json!({ "startHash": start.to_hex(), "includeAcceptedTransactionIds": false }),
        )
        .await
    }

    async fn chain_with_ids(&self, start: Hash32, min_confirmations: Option<u64>) -> Result<ChainIds, RpcError> {
        if self.borsh_url().is_some() {
            let frame = move |id| borsh::chain_ids_request(id, start, min_confirmations);
            let (timeout, cap) = (self.cfg.request_timeout, CHAIN_IDS_MAX_BYTES.min(self.cfg.max_message_bytes));
            return self.call_borsh(frame, timeout, cap, |body, _| borsh::decode_chain_ids(body)).await;
        }
        let mut params = serde_json::json!({ "startHash": start.to_hex(), "includeAcceptedTransactionIds": true });
        if let Some(m) = min_confirmations {
            params["minConfirmationCount"] = m.into();
        }
        let parse = |raw: Reply<'_>| match raw {
            Reply::Json(raw) => serde_json::from_str(raw).map_err(|e| RpcError::Decode(format!("getVirtualChainFromBlock: {e}"))),
            Reply::Borsh { .. } => Err(RpcError::Decode("a Borsh answer to a JSON request".into())),
        };
        // tens to hundreds of KB: on a VSPC connection, so the small calls never wait behind it
        let (timeout, cap) = (self.cfg.request_timeout, CHAIN_IDS_MAX_BYTES.min(self.cfg.max_message_bytes));
        let wire = Wire::Json { method: "getVirtualChainFromBlock", params };
        self.call_on(self.vspc_slot().await, wire, timeout, cap, parse).await
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.call("getBlockDagInfo", serde_json::json!({})).await
    }

    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        let info: ServerInfo = self.call("getServerInfo", serde_json::json!({})).await?;
        // A node that answers but is useless is judged like a failed one. That means another
        // network, no UTXO index, or not synced for long.
        judge::server_info(&self.cfg.url, &self.node_in_use(), &info.network_id, info.is_synced, info.has_utxo_index)
            .map_err(RpcError::Node)?;
        Ok(info)
    }

    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            entries: Vec<AddressUtxo>,
        }
        // An address with many outputs answers with many MB: the client's own limit.
        let r: Resp =
            self.call_capped("getUtxosByAddresses", serde_json::json!({ "addresses": addresses }), self.cfg.max_message_bytes).await?;
        Ok(r.entries)
    }

    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        match self.call_raw("getBlock", serde_json::json!({ "hash": hash.to_hex(), "includeTransactions": false })).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_unknown_block() => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn proves_chain(&self) -> bool {
        true
    }

    async fn check_capabilities(&self, lane_proofs: bool) -> Capable {
        probe_calls(self, lane_proofs).await
    }

    async fn lane_proof(
        &self,
        block: Hash32,
        lane: Hash32,
    ) -> Result<(kaspa_rpc_core::GetSeqCommitLaneProofResponse, Origin), RpcError> {
        let params = serde_json::json!({ "blockHash": block.to_hex(), "laneKey": lane.to_hex() });
        self.call("getSeqCommitLaneProof", params).await.map(|p| (p, Origin::PRIMARY))
    }

    async fn block(&self, hash: Hash32, txs: bool) -> Result<Option<(kaspa_consensus_core::block::Block, Origin)>, RpcError> {
        let params = serde_json::json!({ "hash": hash.to_hex(), "includeTransactions": txs });
        match self.call::<kaspa_rpc_core::GetBlockResponse>("getBlock", params).await {
            Ok(r) => kaspa_consensus_core::block::Block::try_from(r.block)
                .map(|b| Some((b, Origin::PRIMARY)))
                .map_err(|e| RpcError::Decode(format!("getBlock: {e}"))),
            // Only "unknown" means it does not know the block. Another node error proves nothing.
            Err(e) if e.is_unknown_block() => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Convenience for callers that only have a hex string.
pub fn parse_hash(s: &str) -> Result<Hash32, crate::hex::HexError> {
    Hash32::parse(s)
}

#[cfg(test)]
mod tests {
    use super::{ChainSource, Hash32, RpcError, WrpcClient, WrpcConfig};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    /// A node whose `getBlock` answer depends on the hash's first byte: 1 is known, 2 unknown, 3
    /// in IBD, 4 an unknown method.
    async fn fake_node() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((s, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(s).await else { return };
                    while let Some(Ok(Message::Text(t))) = ws.next().await {
                        let req: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                        let hash = req["params"]["hash"].as_str().unwrap().to_string();
                        let err = |m: String| serde_json::json!({ "id": req["id"], "error": { "message": m } });
                        let answer = match &hash[..2] {
                            "01" => {
                                serde_json::json!({ "id": req["id"], "params": { "block": { "header": { "blueScore": 7, "daaScore": 9 } } } })
                            }
                            "02" => err(format!("cannot find header {hash}")),
                            "03" => err("consensus is currently in a transitional ibd state".into()),
                            _ => err("Method not found".into()),
                        };
                        if ws.send(Message::text(answer.to_string())).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (format!("ws://{addr}"), server)
    }

    /// Only "cannot find header" counts as an unknown block. IBD or any other node error is an
    /// error, so the walk back of the follower does not take it for "not known" and go too deep.
    #[tokio::test]
    async fn only_a_not_found_answer_is_an_unknown_block() {
        let (url, server) = fake_node().await;
        let c = WrpcClient::new(WrpcConfig::new(url));
        let h = |b: u8| Hash32([b; 32]);
        assert!(c.block_exists(h(1)).await.unwrap());
        assert!(!c.block_exists(h(2)).await.unwrap());
        assert_eq!(c.block_blue_score(h(1)).await.unwrap(), Some(7));
        assert_eq!(c.block_daa_score(h(1)).await.unwrap(), Some(9));
        assert_eq!(c.block_blue_score(h(2)).await.unwrap(), None);
        assert_eq!(c.block_daa_score(h(2)).await.unwrap(), None);
        for b in [3, 4] {
            assert!(matches!(c.block_exists(h(b)).await, Err(RpcError::Node(_))), "{b}");
            assert!(c.block_blue_score(h(b)).await.is_err(), "{b}");
            assert!(c.block_daa_score(h(b)).await.is_err(), "{b}");
        }
        assert!(RpcError::Node("Block 0101 not found".into()).is_unknown_block());
        assert!(!RpcError::Transport("cannot find header x".into()).is_unknown_block());
        server.abort();
    }

    /// How a node answers `getSeqCommitLaneProof`.
    #[derive(Clone, Copy)]
    enum Lanes {
        /// "too deep" at the sink of the fake: an answer of the call.
        TooDeep,
        /// The node's "RPC method not found".
        NotFound,
    }

    /// A node for the check of its calls. Without `v2` it closes the connection on
    /// `getVirtualChainFromBlockV2`, as an older node (`workflow-rpc`) does on a call it does not
    /// know.
    async fn calls_node(v2: bool, lanes: Lanes) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((s, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(s).await else { return };
                    while let Some(Ok(Message::Text(t))) = ws.next().await {
                        let req: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                        let ok = |p: serde_json::Value| serde_json::json!({ "id": req["id"], "params": p });
                        let err = |m: &str| serde_json::json!({ "id": req["id"], "error": { "message": m } });
                        let h = "11".repeat(32);
                        let answer = match req["method"].as_str().unwrap() {
                            "getBlockDagInfo" => ok(serde_json::json!({
                                "network": "testnet-10", "sink": h, "pruningPointHash": h, "virtualDaaScore": 1
                            })),
                            "getServerInfo" => ok(serde_json::json!({
                                "serverVersion": "2.1.0", "networkId": "testnet-10", "isSynced": true, "hasUtxoIndex": true, "virtualDaaScore": 1
                            })),
                            "getVirtualChainFromBlockV2" if !v2 => return,
                            "getVirtualChainFromBlockV2" => ok(serde_json::json!({
                                "removedChainBlockHashes": [], "addedChainBlockHashes": [], "chainBlockAcceptedTransactions": []
                            })),
                            "getSeqCommitLaneProof" => match lanes {
                                Lanes::TooDeep => err(&format!("block {h} is too deep (before the current pruning point)")),
                                Lanes::NotFound => err("RPC method not found"),
                            },
                            _ => return,
                        };
                        if ws.send(Message::text(answer.to_string())).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (format!("ws://{addr}"), server)
    }

    /// The check of a node's calls: a "too deep" answer is the call's own; "method not found" is a
    /// lack; a connection closed twice on the call while the node answers `getServerInfo` is
    /// `Closed`; a node that does not answer tells nothing.
    #[tokio::test]
    async fn a_node_lacking_a_call_is_told_apart_from_one_that_does_not_answer() {
        use super::{lacks_call, probe_calls, Capable, NodeErrorKind};
        let (url, server) = calls_node(true, Lanes::TooDeep).await;
        let c = WrpcClient::new(WrpcConfig::new(url));
        assert_eq!(c.check_capabilities(true).await, Capable::Yes);
        server.abort();

        let (url, server) = calls_node(true, Lanes::NotFound).await;
        let c = WrpcClient::new(WrpcConfig::new(url));
        assert_eq!(c.check_capabilities(false).await, Capable::Yes, "no lane proofs asked for");
        let Capable::No(why) = c.check_capabilities(true).await else { panic!() };
        assert!(why.contains("getSeqCommitLaneProof") && why.contains("method not found"), "{why}");
        server.abort();

        let (url, server) = calls_node(false, Lanes::TooDeep).await;
        let c = WrpcClient::new(WrpcConfig::new(url.clone()));
        let Capable::Closed(why) = probe_calls(&c, true).await else { panic!() };
        assert!(why.contains("getVirtualChainFromBlockV2") && why.contains("closes the connection"), "{why}");
        server.abort();
        let _ = server.await;
        let c = WrpcClient::new(WrpcConfig::new(url));
        assert!(matches!(probe_calls(&c, true).await, Capable::Unknown(_)), "a node that does not answer");

        let deep = RpcError::Node("block 1111 is too deep (before the current pruning point)".into());
        assert_eq!(deep.kind(), NodeErrorKind::BlockTooDeep);
        assert!(!lacks_call(&deep));
        assert!(lacks_call(&RpcError::Node("Not implemented".into())));
        assert!(!lacks_call(&RpcError::Transport("method not found".into())));
    }
}
