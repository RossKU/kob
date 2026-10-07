//! Node access: the [`ChainSource`] trait and a JSON wRPC client.
//!
//! The indexer only needs four node calls, so instead of pulling in the node's RPC crates (which
//! drift from the wire schema without a compile error) it speaks the node's JSON wRPC directly.
//! The schema is checked against a live node by the integration test in `tests/tn10_follow.rs`.

pub mod borsh;
pub mod multi;
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

/// A node URL as it may be shown or logged: `scheme://host[:port]`. User name and password, path, query and fragment are
/// left out (they can hold credentials or API keys: `wss://user:pass@host/key?apikey=...`); a removed path or query is
/// marked with `/...`. A value without a scheme keeps its host part only.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (Some(s), r),
        None => (None, url),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let tail = &rest[end..];
    let marker = if tail.is_empty() || tail == "/" { "" } else { "/..." };
    match scheme {
        Some(s) => format!("{s}://{host}{marker}"),
        None => format!("{host}{marker}"),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The connection failed; the next call reconnects.
    #[error("transport: {0}")]
    Transport(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    /// The node answered with an error.
    #[error("node error: {0}")]
    Node(String),
    #[error("cannot decode node response: {0}")]
    Decode(String),
    /// The answer is larger than the request allowed (`VspcRequest::max_bytes`): refused as its frame header arrived, its
    /// payload never buffered; the connection is closed.
    #[error("answer of {size} bytes exceeds the {limit}-byte limit of the request")]
    TooLarge { size: usize, limit: usize },
}

/// A transport error that is the request's size limit ([`RpcError::TooLarge`]), else [`RpcError::Transport`].
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
    /// `cannot find header <hash>`: the node does not know the start hash (never had it, or pruned it).
    UnknownHash,
    /// `the queried hash does not have retention root on its chain`: the start hash is too old.
    RetentionRootMissing,
    /// `consensus is currently in a transitional ibd state`: retry later, never advance.
    TransitionalIbd,
    Other,
}

impl RpcError {
    pub fn kind(&self) -> NodeErrorKind {
        match self {
            RpcError::Node(m) => classify_node_message(m),
            _ => NodeErrorKind::Other,
        }
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
    } else {
        NodeErrorKind::Other
    }
}

/// Everything the indexer asks of a node. Implemented by [`WrpcClient`] and by the in-memory
/// node used in tests.
pub trait ChainSource: Send + Sync + 'static {
    fn vspc_v2(&self, req: VspcRequest) -> impl Future<Output = Result<RawVspcResponse, RpcError>> + Send;
    fn dag_info(&self) -> impl Future<Output = Result<DagInfo, RpcError>> + Send;
    fn server_info(&self) -> impl Future<Output = Result<ServerInfo, RpcError>> + Send;
    /// Unspent outputs of the given addresses (needs `--utxoindex` on the node).
    fn utxos_by_addresses(&self, addresses: &[String]) -> impl Future<Output = Result<Vec<AddressUtxo>, RpcError>> + Send;
    /// Whether the node still knows this block (header present). Transport errors are errors; a
    /// node "not found" answer is `Ok(false)`.
    fn block_exists(&self, hash: Hash32) -> impl Future<Output = Result<bool, RpcError>> + Send;
    /// Blue score of the node's sink (`getSinkBlueScore`): bounds a VSPC batch (`rpc::window`).
    fn sink_blue_score(&self) -> impl Future<Output = Result<u64, RpcError>> + Send;
    /// Blue score of a block the node knows (`None`: the node does not know it).
    fn block_blue_score(&self, hash: Hash32) -> impl Future<Output = Result<Option<u64>, RpcError>> + Send;
    /// The selected chain from `start` as block hashes only (`getVirtualChainFromBlock` without transaction ids): what the
    /// follower plans its parallel windows on.
    fn chain_hashes(&self, start: Hash32) -> impl Future<Output = Result<ChainHashes, RpcError>> + Send;
    /// The selected chain from `start` with the transaction ids every chain block accepted (`getVirtualChainFromBlock` with
    /// ids), bounded like a VSPC request by `minConfirmationCount`: the acceptance data a window from another node is checked
    /// against (`rpc::verify`).
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
    /// One window `(start, end]` of the selected chain the follower planned (`indexer::prefetch`), from whichever node the
    /// source picks (a single node: itself). The answer is the primary's, or checked against it ([`Fetched::origin`]).
    fn fetch_window(&self, w: WindowRequest) -> impl Future<Output = Result<Fetched, RpcError>> + Send {
        async move { fetch_window_from(self, &w, Verbosity::High, Origin::PRIMARY).await }
    }
    /// [`ChainSource::fetch_window`] from the primary only (the chain authority).
    fn fetch_window_primary(&self, w: WindowRequest) -> impl Future<Output = Result<Fetched, RpcError>> + Send {
        async move { fetch_window_from(self, &w, Verbosity::High, Origin::PRIMARY).await }
    }
    /// The node behind `origin` gave data the primary contradicts: stop using it.
    fn report_lie(&self, origin: Origin, why: &str) {
        let _ = (origin, why);
    }
    /// Per-node counters for `/v1/health` (empty for a single node).
    fn node_stats(&self) -> Vec<multi::NodeStatus> {
        vec![]
    }
}

/// Where a window came from: the index of the node in the source's list (0: the primary) and whether it is the primary's own
/// answer (`trusted`) or another node's, checked against the primary (`rpc::verify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub node: usize,
    pub trusted: bool,
}

impl Origin {
    pub const PRIMARY: Origin = Origin { node: 0, trusted: true };
}

/// One window of the parallel fetch: the chain blocks after `start` up to and including `end` (`indexer::prefetch`).
#[derive(Debug, Clone, Copy)]
pub struct WindowRequest {
    pub start: Hash32,
    pub end: Hash32,
    pub timeout_scale: u32,
    /// `minConfirmationCount` the caller wants anyway.
    pub min_confirmations: Option<u64>,
    /// The answer's size limit (`VspcRequest::max_bytes`).
    pub max_bytes: Option<usize>,
    /// The blue score of `end` and of the node's sink when the caller knows them (the node is not asked).
    pub end_blue: Option<u64>,
    pub sink_blue: Option<u64>,
}

impl WindowRequest {
    pub fn new(start: Hash32, end: Hash32) -> Self {
        WindowRequest { start, end, timeout_scale: 1, min_confirmations: None, max_bytes: None, end_blue: None, sink_blue: None }
    }
}

/// A window as a node returned it.
pub struct Fetched {
    pub raw: RawVspcResponse,
    /// Blue score of the node's sink and of the window's last chain block when the request was sent.
    pub sink_blue: u64,
    pub end_blue: u64,
    pub elapsed: Duration,
    pub origin: Origin,
}

/// Fetch one window from `src`: `(start, end]` at `verbosity`, bounded to end at `end` through `minConfirmationCount`
/// relative to `src`'s own sink (blocks past `end` may still come back: the sink moves while the request travels).
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

/// `minConfirmationCount` that ends a request at the chain block of blue score `end_blue` while the sink is at `sink_blue`
/// (the node keeps a chain block while `sink - blue > minConfirmationCount`), or the caller's `extra` when larger.
pub fn window_min_confirmations(sink_blue: u64, end_blue: u64, extra: Option<u64>) -> Option<u64> {
    match (sink_blue.saturating_sub(end_blue.saturating_add(1)), extra) {
        (0, e) => e,
        (b, e) => Some(b.max(e.unwrap_or(0))),
    }
}

#[derive(Debug, Clone)]
pub struct WrpcConfig {
    pub url: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Upper bound for one response. A `High` VSPC batch is large (tens of MB on TN10).
    pub max_message_bytes: usize,
    /// Connections reserved for `getVirtualChainFromBlockV2` (0: VSPC shares the one connection of every other call).
    /// The follower fetches that many windows at once while it catches up; the other calls keep their own connection,
    /// so a small call never waits behind a large batch.
    pub vspc_connections: usize,
    /// The node's Borsh wRPC endpoint: with VSPC connections of its own, the windows of transaction bodies and the accepted
    /// transaction ids travel in Borsh over it, a fraction of their JSON size ([`borsh`]); every other call stays JSON on `url`.
    pub borsh_url: Option<String>,
}

impl WrpcConfig {
    pub fn new(url: impl Into<String>) -> Self {
        WrpcConfig {
            url: url.into(),
            request_timeout: Duration::from_secs(180),
            connect_timeout: Duration::from_secs(15),
            max_message_bytes: 1 << 30,
            vspc_connections: 0,
            borsh_url: None,
        }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Slot = tokio::sync::Mutex<Option<Ws>>;

/// JSON wRPC client: one request in flight per connection, one connection for the small calls plus
/// `vspc_connections` for VSPC batches. Reconnects lazily after any failure.
pub struct WrpcClient {
    cfg: WrpcConfig,
    conn: Slot,
    vspc: Vec<Slot>,
    next_vspc: std::sync::atomic::AtomicUsize,
    next_id: std::sync::atomic::AtomicU64,
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
        WrpcClient {
            cfg,
            conn: Slot::new(None),
            vspc,
            next_vspc: std::sync::atomic::AtomicUsize::new(0),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// A VSPC connection: an idle one if there is one (round robin), else the next in turn.
    async fn vspc_slot(&self) -> tokio::sync::MutexGuard<'_, Option<Ws>> {
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

    /// A connection whose answers may be at most `cap` bytes (a frame or message above it fails as [`RpcError::TooLarge`]).
    async fn connect(&self, url: &str, cap: usize) -> Result<Ws, RpcError> {
        let ws_cfg = WebSocketConfig::default().max_message_size(Some(cap)).max_frame_size(Some(cap));
        let fut = tokio_tungstenite::connect_async_with_config(url, Some(ws_cfg), false);
        match tokio::time::timeout(self.cfg.connect_timeout, fut).await {
            Err(_) => Err(RpcError::Timeout(self.cfg.connect_timeout)),
            Ok(Err(e)) => Err(RpcError::Transport(e.to_string())),
            Ok(Ok((ws, _))) => Ok(ws),
        }
    }

    /// Send one request and return the raw `params` of the matching response.
    pub async fn call_raw(&self, method: &str, params: serde_json::Value) -> Result<String, RpcError> {
        self.call_raw_within(method, params, self.cfg.request_timeout).await
    }

    /// [`WrpcClient::call_raw`] with another timeout. A timeout drops the connection (a late answer must
    /// never be read as the answer to the next request).
    pub async fn call_raw_within(&self, method: &str, params: serde_json::Value, timeout: Duration) -> Result<String, RpcError> {
        let parse = |p: Reply<'_>| match p {
            Reply::Json(p) => Ok(p.to_string()),
            Reply::Borsh { .. } => Err(RpcError::Decode("a Borsh answer to a JSON request".into())),
        };
        self.call_on(self.conn.lock().await, Wire::Json { method, params }, timeout, self.cfg.max_message_bytes, parse).await
    }

    /// The Borsh endpoint the VSPC connections use, if any (`None`: JSON; also when VSPC shares the JSON connection).
    pub fn borsh_url(&self) -> Option<&str> {
        self.cfg.borsh_url.as_deref().filter(|_| !self.vspc.is_empty())
    }

    /// One request on the connection behind `guard`; `parse` reads the response's `params` in place (a VSPC batch is
    /// tens of MB: it is parsed straight from the frame, not copied first). A request that times out, fails in
    /// transport or is cancelled (its future dropped: the follower aborts prefetched windows it no longer needs)
    /// closes its connection, so a late answer is never left in the socket. The answer may be at most `cap` bytes: a connection
    /// opened with another limit is replaced first (a VSPC window of the prefetch asks for its own, `VspcRequest::max_bytes`).
    async fn call_on<T>(
        &self,
        guard: tokio::sync::MutexGuard<'_, Option<Ws>>,
        wire: Wire<'_>,
        timeout: Duration,
        cap: usize,
        parse: impl FnOnce(Reply<'_>) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        struct Pending<'a> {
            guard: tokio::sync::MutexGuard<'a, Option<Ws>>,
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
        if p.guard.as_ref().is_some_and(|ws| ws.get_config().max_message_size != Some(cap)) {
            *p.guard = None;
        }
        // a connection is used for one encoding only (the JSON one, or a VSPC connection to the Borsh endpoint)
        let url = match &wire {
            Wire::Json { .. } => self.cfg.url.as_str(),
            Wire::Borsh { .. } => self.cfg.borsh_url.as_deref().unwrap_or(self.cfg.url.as_str()),
        };
        if p.guard.is_none() {
            *p.guard = Some(self.connect(url, cap).await?);
        }
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = match wire {
            Wire::Json { method, params } => {
                Message::text(serde_json::json!({"id": id, "method": method, "params": params}).to_string())
            }
            Wire::Borsh { frame } => Message::binary(frame(id)),
        };
        let ws = p.guard.as_mut().expect("connected above");
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
        let raw = self.call_raw(method, params).await?;
        serde_json::from_str(&raw).map_err(|e| RpcError::Decode(format!("{method}: {e}")))
    }
}

impl ChainSource for WrpcClient {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        let timeout = self.cfg.request_timeout.saturating_mul(req.timeout_scale.max(1));
        let cap = req.max_bytes.unwrap_or(self.cfg.max_message_bytes).min(self.cfg.max_message_bytes);
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
            Err(RpcError::Node(_)) => Ok(None),
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
            let (timeout, cap) = (self.cfg.request_timeout, self.cfg.max_message_bytes);
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
        let (timeout, cap) = (self.cfg.request_timeout, self.cfg.max_message_bytes);
        let wire = Wire::Json { method: "getVirtualChainFromBlock", params };
        self.call_on(self.vspc_slot().await, wire, timeout, cap, parse).await
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.call("getBlockDagInfo", serde_json::json!({})).await
    }

    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.call("getServerInfo", serde_json::json!({})).await
    }

    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            entries: Vec<AddressUtxo>,
        }
        let r: Resp = self.call("getUtxosByAddresses", serde_json::json!({ "addresses": addresses })).await?;
        Ok(r.entries)
    }

    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        match self.call_raw("getBlock", serde_json::json!({ "hash": hash.to_hex(), "includeTransactions": false })).await {
            Ok(_) => Ok(true),
            Err(RpcError::Node(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Convenience for callers that only have a hex string.
pub fn parse_hash(s: &str) -> Result<Hash32, crate::hex::HexError> {
    Hash32::parse(s)
}
