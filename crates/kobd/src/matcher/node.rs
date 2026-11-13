//! Node access over the node's JSON wRPC (rusty-kaspa v2.1.0 `RpcApiOps`, camelCase).
//!
//! The matcher needs these calls:
//!
//! - `getServerInfo`: network, sync state, virtual DAA score.
//! - `getBlockDagInfo`: the sink, which is the acceptance cursor.
//! - `submitTransaction`: v1 transactions with per-input compute budgets, which the REST API drops.
//! - `getVirtualChainFromBlockV2` at `Low` verbosity: accepted transaction ids per chain block, and
//!   `removed` blocks for reorgs.
//! - `getUtxosByAddresses`: the operator's funding. It needs `--utxoindex`.
//! - `getFeeEstimate`: input of the fee policy (`crate::fee`).
//!
//! [`NodeApi`] adds replace-by-fee, tip timestamps and mempool lookups.
//!
//! The transport is JSON wRPC by default. A Borsh URL or a resolver entry (a public node) takes
//! every call in Borsh (`crate::rpc::bridge`, `crate::rpc::resolver`), with the same JSON in and
//! out of [`WrpcNode::call`].

use std::future::Future;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Errors of a node call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    /// The connection failed.
    #[error("transport: {0}")]
    Transport(String),
    /// The node did not answer within the timeout.
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    /// The node answered with an error message.
    #[error("node: {0}")]
    Node(String),
    /// The node's answer could not be decoded.
    #[error("cannot decode node response: {0}")]
    Decode(String),
}

/// What a `submitTransaction` rejection means for the matcher (`docs/spec/matcher.md` §7, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// In the mempool now.
    Accepted,
    /// Already in the mempool or already accepted by consensus: idempotent resend, success.
    AlreadyKnown,
    /// `RejectDoubleSpendInMempool`: another transaction (a competing matcher, a cancel, or our own
    /// earlier one) spends an input. Benign: back off, never replace blindly.
    DoubleSpend,
    /// An input is missing (spent on chain: the race is lost, or the parent is not in the mempool
    /// yet): re-read the book.
    MissingInput,
    /// The mempool is full: retry later.
    Busy,
    /// No verdict: the request failed on the wire (transport error, timeout, undecodable answer),
    /// so the node may or may not have taken the transaction.
    ///
    /// It is tracked as pending and its inputs stay reserved. It is resent idempotently until the
    /// node answers or the pending timeout drops it.
    Unknown,
    /// Any other rejection (a bug or a stale plan): logged, never resent unchanged.
    Rejected,
}

/// Classifies a node rejection message (rusty-kaspa `mining/errors/src/mempool.rs`).
pub fn classify_submit_error(msg: &str) -> SubmitOutcome {
    let m = msg.to_ascii_lowercase();
    if m.contains("already spent by transaction") && m.contains("in the mempool") || m.contains("double spend") {
        SubmitOutcome::DoubleSpend
    } else if m.contains("is already in the mempool") || m.contains("already accepted by the consensus") {
        SubmitOutcome::AlreadyKnown
    } else if m.contains("lacking a matching utxo") || m.contains("orphan") || m.contains("missing outpoint") {
        SubmitOutcome::MissingInput
    } else if m.contains("mempool is full") || m.contains("full with transactions") {
        SubmitOutcome::Busy
    } else {
        SubmitOutcome::Rejected
    }
}

/// Subset of `getServerInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// The node's software version.
    #[serde(default)]
    pub server_version: String,
    /// The node's network id.
    pub network_id: String,
    /// Whether the node says it is synced.
    pub is_synced: bool,
    /// Whether the node has a UTXO index (`--utxoindex`).
    #[serde(default)]
    pub has_utxo_index: bool,
    /// The node's virtual DAA score.
    pub virtual_daa_score: u64,
}

/// Subset of `getBlockDagInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagInfo {
    /// The node's network name.
    #[serde(default)]
    pub network: String,
    /// Hash of the sink, the newest chain block.
    pub sink: String,
    /// The node's virtual DAA score.
    pub virtual_daa_score: u64,
}

/// One added chain block with the ids of the transactions it accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBlock {
    /// Hash of the chain block.
    pub hash: String,
    /// DAA score of the chain block.
    pub daa_score: u64,
    /// Ids of the transactions the block accepted.
    pub accepted: Vec<[u8; 32]>,
}

/// A `getVirtualChainFromBlockV2` step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainUpdate {
    /// Chain blocks removed by a reorg (tip first).
    pub removed: Vec<String>,
    /// Chain blocks added, in chain order.
    pub added: Vec<ChainBlock>,
}

/// One P2PK UTXO of an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressUtxo {
    /// Id of the transaction that created the output.
    pub transaction_id: [u8; 32],
    /// Output index.
    pub index: u32,
    /// Amount in sompi.
    pub amount: u64,
    /// `version ‖ script` hex.
    pub script_public_key: String,
    /// DAA score of the UTXO's block.
    pub block_daa_score: u64,
    /// Covenant id of the output, if it has one.
    pub covenant_id: Option<[u8; 32]>,
}

/// Everything the matcher and keepers ask of a node. Implemented by [`WrpcNode`] and by test nodes.
pub trait NodeApi: Send + Sync {
    /// The node's `getServerInfo`.
    fn server_info(&self) -> impl Future<Output = Result<ServerInfo, RpcError>> + Send;
    /// The node's `getBlockDagInfo`.
    fn dag_info(&self) -> impl Future<Output = Result<DagInfo, RpcError>> + Send;
    /// Submits an `RpcTransaction` JSON; returns the transaction id or the node's message.
    fn submit(&self, tx: Value) -> impl Future<Output = Result<[u8; 32], RpcError>> + Send;
    /// Submits a pending transaction again to settle it
    /// (`crate::matcher::submit::Tracker::recheck_pending`).
    ///
    /// The default is [`NodeApi::submit`]. A client of several nodes ([`MultiSubmit`]) lets the
    /// primary's double spend or missing input decide here, even when another node still holds the
    /// transaction.
    fn resubmit(&self, tx: Value) -> impl Future<Output = Result<[u8; 32], RpcError>> + Send {
        self.submit(tx)
    }
    /// One `getVirtualChainFromBlockV2` step starting at `start_hash`.
    fn chain_from(&self, start_hash: &str) -> impl Future<Output = Result<ChainUpdate, RpcError>> + Send;
    /// The UTXOs of `addresses` (`getUtxosByAddresses`).
    fn utxos_by_addresses(&self, addresses: &[String]) -> impl Future<Output = Result<Vec<AddressUtxo>, RpcError>> + Send;
    /// The node's fee estimate (`getFeeEstimate`, `crate::fee`). The default is none: the fee
    /// policy then pays its floor.
    fn fee_estimate(&self) -> impl Future<Output = Result<crate::fee::FeeEstimate, RpcError>> + Send {
        async { Err(RpcError::Node("no fee estimate".into())) }
    }
    /// Replace-by-fee (`submitTransactionReplacement`, rusty-kaspa v2.1.0 `RbfPolicy::Mandatory`).
    ///
    /// The node takes `tx` only when exactly one mempool transaction spends an input of `tx` and
    /// `tx` pays a strictly higher fee rate. That transaction and its mempool descendants are
    /// removed. Returns the new transaction id or the node's refusal. The default is a refusal (a
    /// node client without replacement), and the runner then waits for the pending timeout.
    fn submit_replacement(&self, _tx: Value) -> impl Future<Output = Result<[u8; 32], RpcError>> + Send {
        async { Err(RpcError::Node("submitTransactionReplacement is not supported by this node client".into())) }
    }
    /// The timestamp (unix milliseconds) in the header of the node's sink, the newest chain block.
    ///
    /// It is the chain's clock, which the runner compares with the local one, since day-order
    /// deadlines are judged on the local clock. The default is unknown.
    fn tip_timestamp_ms(&self) -> impl Future<Output = Result<u64, RpcError>> + Send {
        async { Err(RpcError::Node("no tip timestamp from this node client".into())) }
    }
    /// The tip timestamps of every node the client asks (`MultiSubmit`: the primary and the others;
    /// with one node, its own).
    ///
    /// A timestamp is the miner's, so the runner never trusts one alone
    /// (`matcher::run::chain_skew_secs`). Empty: none known.
    fn tip_timestamps_ms(&self) -> impl Future<Output = Vec<u64>> + Send {
        async { self.tip_timestamp_ms().await.ok().into_iter().collect() }
    }
    /// Whether the node's mempool (its orphan pool included) holds the transaction `txid`
    /// (`getMempoolEntry`).
    ///
    /// `Ok(false)` only when the node says it does not (`Transaction <id> not found`). Anything
    /// else is an error, which means unsure. The default is unsure.
    fn in_mempool(&self, txid: [u8; 32]) -> impl Future<Output = Result<bool, RpcError>> + Send {
        async move {
            let _ = txid;
            Err(RpcError::Node("no mempool query from this node client".into()))
        }
    }
}

/// Whether a node's refusal of `getMempoolEntry` for `txid` says the mempool does not hold it.
///
/// rusty-kaspa answers `RpcError::TransactionNotFound`, `Transaction <id> not found`. Another
/// failure (an unknown method, a bad request) is not a miss.
pub fn mempool_miss(msg: &str, txid: &[u8; 32]) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("not found") && m.contains(&kob_protocol::json::to_hex(txid))
}

/// Connection settings.
#[derive(Debug, Clone)]
pub struct WrpcConfig {
    /// Node URL: a wRPC URL or a resolver entry.
    pub url: String,
    /// Timeout of one request (default 30 s).
    pub request_timeout: Duration,
    /// Timeout of one connection attempt (default 10 s).
    pub connect_timeout: Duration,
    /// Largest WebSocket message and frame accepted (default 256 MiB).
    pub max_message_bytes: usize,
}

impl WrpcConfig {
    /// Settings for `url` with the default timeouts and message size.
    pub fn new(url: impl Into<String>) -> Self {
        WrpcConfig {
            url: url.into(),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            max_message_bytes: 256 << 20,
        }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Sequential JSON wRPC client (one request in flight); reconnects lazily after a failure.
pub struct WrpcNode {
    cfg: WrpcConfig,
    conn: tokio::sync::Mutex<Option<Ws>>,
    next_id: std::sync::atomic::AtomicU64,
    /// The node of the current connection (a resolver entry's public node, else the URL): what a
    /// failure reports.
    node: std::sync::Mutex<String>,
    /// Bounds each `chain_from` batch (`crate::rpc::window`).
    ///
    /// A matcher that fell behind under a flood pages through the backlog instead of asking for all
    /// of it and timing out forever.
    window: std::sync::Mutex<crate::rpc::window::BatchWindow>,
}

/// Interprets one frame for request `id`: `Ok(None)` for another request's frame or a notification.
pub fn parse_frame(text: &str, id: u64) -> Result<Option<Value>, RpcError> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if v.get("id").and_then(Value::as_u64) != Some(id) {
        return Ok(None);
    }
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        let msg = e.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| e.to_string());
        return Err(RpcError::Node(msg));
    }
    v.get("params").cloned().map(Some).ok_or_else(|| RpcError::Decode("response without params".into()))
}

impl WrpcNode {
    /// A client for `cfg`. It connects on the first call.
    pub fn new(cfg: WrpcConfig) -> Self {
        let window = std::sync::Mutex::new(crate::rpc::window::BatchWindow::for_timeout(cfg.request_timeout));
        let node = std::sync::Mutex::new(cfg.url.clone());
        WrpcNode { cfg, conn: tokio::sync::Mutex::new(None), next_id: std::sync::atomic::AtomicU64::new(1), node, window }
    }

    fn win(&self) -> std::sync::MutexGuard<'_, crate::rpc::window::BatchWindow> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The window of a `chain_from` request. It is unbounded when the node does not know `start`:
    /// the request then fails fast and the runner restarts from the sink.
    async fn chain_plan(&self, start_hash: &str) -> Result<crate::rpc::window::WindowPlan, RpcError> {
        let unbounded = crate::rpc::window::WindowPlan { min_confirmations: None, until_blue: None, span: u64::MAX, timeout_scale: 1 };
        let start_blue = match self.call("getBlock", json!({"hash": start_hash, "includeTransactions": false})).await {
            Ok(p) => p.get("block").and_then(|b| b.get("header")).and_then(|h| h.get("blueScore")).and_then(Value::as_u64),
            Err(RpcError::Node(_)) => None,
            Err(e) => return Err(e),
        };
        let Some(start_blue) = start_blue else { return Ok(unbounded) };
        let sink_blue = self
            .call("getSinkBlueScore", json!({}))
            .await?
            .get("blueScore")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::Decode("getSinkBlueScore without blueScore".into()))?;
        Ok(self.win().plan(start_blue, sink_blue, None))
    }

    async fn connect(&self) -> Result<Ws, RpcError> {
        let url = crate::rpc::endpoint(&self.cfg.url).await.map_err(|e| RpcError::Transport(e.to_string()))?;
        *self.node.lock().unwrap_or_else(|e| e.into_inner()) = url.clone();
        let ws_cfg = WebSocketConfig::default()
            .max_message_size(Some(self.cfg.max_message_bytes))
            .max_frame_size(Some(self.cfg.max_message_bytes));
        let fut = tokio_tungstenite::connect_async_with_config(url.as_str(), Some(ws_cfg), false);
        // A dropped public node's one reconnect has a short timeout
        // (`crate::rpc::resolver::failed`).
        let limit = crate::rpc::resolver::connect_timeout(&self.cfg.url, self.cfg.connect_timeout);
        match tokio::time::timeout(limit, fut).await {
            Err(_) => Err(RpcError::Timeout(limit)),
            Ok(Err(e)) => Err(RpcError::Transport(e.to_string())),
            Ok(Ok((ws, _))) => Ok(ws),
        }
    }

    /// One request; returns the response `params`.
    ///
    /// These are the small calls. A timeout counts against the node
    /// (`crate::rpc::judge::timed_out`: a node that hangs), unlike a `chain_from` batch's or the
    /// short fee estimate's.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let r = self.call_within(method, params, self.cfg.request_timeout).await;
        if matches!(r, Err(RpcError::Timeout(_))) {
            crate::rpc::judge::timed_out(&self.cfg.url, &self.node_in_use());
        }
        r
    }

    /// The node of the current connection (a resolver entry's public node, else the URL).
    fn node_in_use(&self) -> String {
        self.node.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// [`WrpcNode::call`] with another timeout.
    pub async fn call_within(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let borsh = crate::rpc::all_borsh(&self.cfg.url);
        // A call this client cannot carry in Borsh is refused before the wire, so nothing was sent.
        let encoded = if borsh { Some(crate::rpc::bridge::encode(method, &params).map_err(RpcError::Decode)?) } else { None };
        let mut guard = self.conn.lock().await;
        // A resolver entry's connection to a node the entry gave up (it failed elsewhere, or the
        // authority moved) is replaced.
        if guard.is_some() && crate::rpc::resolver::is_resolver(&self.cfg.url) {
            let node = self.node.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if crate::rpc::resolver::current(&self.cfg.url).as_deref() != Some(node.as_str()) {
                *guard = None;
            }
        }
        if guard.is_none() {
            match self.connect().await {
                Ok(ws) => *guard = Some(ws),
                Err(e) => {
                    self.report();
                    return Err(e);
                }
            }
        }
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = match &encoded {
            Some(e) => Message::binary(e.frame(id)),
            None => Message::text(json!({"id": id, "method": method, "params": params}).to_string()),
        };
        let ws = guard.as_mut().expect("connected");
        let res = tokio::time::timeout(timeout, async {
            ws.send(req).await.map_err(|e| RpcError::Transport(e.to_string()))?;
            loop {
                let msg = match ws.next().await {
                    None => return Err(RpcError::Transport("connection closed".into())),
                    Some(Err(e)) => return Err(RpcError::Transport(e.to_string())),
                    Some(Ok(m)) => m,
                };
                match msg {
                    Message::Text(text) if !borsh => {
                        if let Some(p) = parse_frame(text.as_str(), id)? {
                            return Ok(p);
                        }
                    }
                    Message::Binary(bytes) if borsh => match crate::rpc::borsh::parse_reply(&bytes[..]) {
                        Ok(crate::rpc::borsh::Reply::Ok { id: Some(i), body }) if i == id => {
                            return crate::rpc::bridge::decode(method, body).map_err(RpcError::Decode);
                        }
                        Ok(crate::rpc::borsh::Reply::Err { id: Some(i), message }) if i == id => return Err(RpcError::Node(message)),
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
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                if !matches!(e, RpcError::Node(_)) {
                    *guard = None;
                }
                if matches!(e, RpcError::Transport(_)) {
                    self.report();
                }
                Err(e)
            }
            Err(_) => {
                *guard = None;
                Err(RpcError::Timeout(timeout))
            }
        }
    }

    /// The connection failed or the node dropped it: a resolver entry moves to another public node
    /// (`crate::rpc::resolver::failed`).
    fn report(&self) {
        if crate::rpc::resolver::is_resolver(&self.cfg.url) {
            crate::rpc::resolver::failed(&self.cfg.url, &self.node_in_use());
        }
    }
}

fn hex32(v: &Value) -> Option<[u8; 32]> {
    kob_protocol::json::hex32(v.as_str()?).ok()
}

/// Parses a `getVirtualChainFromBlockV2` response (any verbosity from `Low`).
pub fn parse_chain_update(p: &Value) -> Result<ChainUpdate, RpcError> {
    let strings = |k: &str| -> Vec<String> {
        p.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let removed = strings("removedChainBlockHashes");
    let added_hashes = strings("addedChainBlockHashes");
    let blocks = p.get("chainBlockAcceptedTransactions").and_then(Value::as_array).cloned().unwrap_or_default();
    if blocks.len() != added_hashes.len() {
        return Err(RpcError::Decode(format!("{} added hashes but {} accepted-transaction blocks", added_hashes.len(), blocks.len())));
    }
    let mut added = vec![];
    for (h, b) in added_hashes.into_iter().zip(blocks) {
        let header = b.get("chainBlockHeader").cloned().unwrap_or(Value::Null);
        if let Some(hh) = header.get("hash").and_then(Value::as_str) {
            if hh != h {
                return Err(RpcError::Decode(format!("chain block {h} carries header {hh}")));
            }
        }
        // A block without its DAA score is refused, as the Borsh path does (`rpc::borsh`). A score of
        // 0 would make every transaction it accepts final at once.
        let daa_score = header
            .get("daaScore")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::Decode(format!("chain block {h} without DAA score")))?;
        let accepted = b
            .get("acceptedTransactions")
            .and_then(Value::as_array)
            .map(|txs| txs.iter().filter_map(|t| t.get("verboseData").and_then(|v| v.get("transactionId")).and_then(hex32)).collect())
            .unwrap_or_default();
        added.push(ChainBlock { hash: h, daa_score, accepted });
    }
    Ok(ChainUpdate { removed, added })
}

fn decode<T: serde::de::DeserializeOwned>(method: &str, v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| RpcError::Decode(format!("{method}: {e}")))
}

impl NodeApi for WrpcNode {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        let info: ServerInfo = decode("getServerInfo", self.call("getServerInfo", json!({})).await?)?;
        // A node that answers but is useless (another network, no UTXO index, not synced for long)
        // is judged like a failed one.
        crate::rpc::judge::server_info(&self.cfg.url, &self.node_in_use(), &info.network_id, info.is_synced, info.has_utxo_index)
            .map_err(RpcError::Node)?;
        Ok(info)
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        decode("getBlockDagInfo", self.call("getBlockDagInfo", json!({})).await?)
    }

    async fn submit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let p = self.call("submitTransaction", json!({"transaction": tx, "allowOrphan": false})).await?;
        p.get("transactionId").and_then(hex32).ok_or_else(|| RpcError::Decode(format!("no transactionId in {p}")))
    }

    async fn submit_replacement(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let p = self.call("submitTransactionReplacement", json!({"transaction": tx})).await?;
        p.get("transactionId").and_then(hex32).ok_or_else(|| RpcError::Decode(format!("no transactionId in {p}")))
    }

    async fn chain_from(&self, start_hash: &str) -> Result<ChainUpdate, RpcError> {
        let plan = self.chain_plan(start_hash).await?;
        let timeout = self.cfg.request_timeout.saturating_mul(plan.timeout_scale.max(1));
        let t0 = std::time::Instant::now();
        let params = json!({"startHash": start_hash, "dataVerbosityLevel": "Low", "minConfirmationCount": plan.min_confirmations});
        let p = match self.call_within("getVirtualChainFromBlockV2", params, timeout).await {
            Ok(p) => p,
            Err(e) => {
                if matches!(e, RpcError::Timeout(_)) {
                    let mut w = self.win();
                    w.on_timeout(plan.span);
                    tracing::warn!(
                        window_blue = w.current(),
                        timeout_scale = w.timeout_scale(),
                        "VSPC batch timed out; asking for a smaller one"
                    );
                }
                return Err(e);
            }
        };
        let u = parse_chain_update(&p)?;
        if plan.by_window() && u.added.is_empty() {
            // The next chain block lies beyond the window: widen it and apply nothing. The next
            // tick asks again.
            self.win().on_empty();
            return Ok(ChainUpdate::default());
        }
        self.win().on_success(t0.elapsed(), plan.by_window());
        Ok(u)
    }

    async fn tip_timestamp_ms(&self) -> Result<u64, RpcError> {
        let sink = self.dag_info().await?.sink;
        let p = self.call("getBlock", json!({"hash": sink, "includeTransactions": false})).await?;
        p.get("block")
            .and_then(|b| b.get("header"))
            .and_then(|h| h.get("timestamp"))
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::Decode("getBlock without a header timestamp".into()))
    }

    async fn in_mempool(&self, txid: [u8; 32]) -> Result<bool, RpcError> {
        let params =
            json!({"transactionId": kob_protocol::json::to_hex(&txid), "includeOrphanPool": true, "filterTransactionPool": false});
        match self.call("getMempoolEntry", params).await {
            Ok(_) => Ok(true),
            Err(RpcError::Node(m)) if mempool_miss(&m, &txid) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn fee_estimate(&self) -> Result<crate::fee::FeeEstimate, RpcError> {
        // A small answer: a short timeout, so a stalled node delays a tick by seconds, not by the
        // request timeout.
        let p = self.call_within("getFeeEstimate", json!({}), self.cfg.request_timeout.min(Duration::from_secs(5))).await?;
        crate::fee::FeeEstimate::parse(&p).map_err(RpcError::Decode)
    }

    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        let p = self.call("getUtxosByAddresses", json!({"addresses": addresses})).await?;
        // An answer without its entries is not "no UTXOs": the caller would take every queried
        // output as spent.
        let entries =
            p.get("entries").and_then(Value::as_array).cloned().ok_or_else(|| RpcError::Decode(format!("no entries in {p}")))?;
        let mut out = vec![];
        for e in entries {
            let op = e.get("outpoint").cloned().unwrap_or(Value::Null);
            let u = e.get("utxoEntry").cloned().unwrap_or(Value::Null);
            let (Some(txid), Some(index)) = (op.get("transactionId").and_then(hex32), op.get("index").and_then(Value::as_u64)) else {
                continue;
            };
            out.push(AddressUtxo {
                transaction_id: txid,
                index: index as u32,
                amount: u.get("amount").and_then(Value::as_u64).unwrap_or(0),
                script_public_key: u.get("scriptPublicKey").and_then(Value::as_str).unwrap_or_default().to_string(),
                block_daa_score: u.get("blockDaaScore").and_then(Value::as_u64).unwrap_or(0),
                covenant_id: u.get("covenantId").and_then(hex32),
            });
        }
        Ok(out)
    }
}

/// The matcher's node with more nodes for submissions (`rpc::multi`).
///
/// Every call goes to the primary, and a transaction is submitted to every node at once. The first
/// node that takes it answers the matcher. The others go on in the background for propagation, so
/// the transaction reaches the miners through several peers. When none takes it, the primary's
/// rejection is the answer, since it decides double spends and missing inputs, unless the primary
/// failed on the wire and another node gave a verdict.
///
/// At the first submission, a node that already holds the transaction answers "already known" over
/// the primary's refusal, since the race is still open. When a pending transaction is checked again
/// ([`NodeApi::resubmit`]), the primary's double spend or missing input wins. Otherwise a stalled or
/// partitioned node that still holds the losing copy would keep it pending, and its funding coin
/// reserved, forever.
pub struct MultiSubmit<N: NodeApi + 'static> {
    primary: std::sync::Arc<N>,
    /// `(index in the node board, node)` of every other node.
    others: Vec<(usize, std::sync::Arc<N>)>,
    board: Option<std::sync::Arc<crate::rpc::multi::NodeBoard>>,
}

impl<N: NodeApi + 'static> MultiSubmit<N> {
    /// A client that submits to `primary` and to `others`, with an optional board that counts the
    /// outcomes.
    pub fn new(primary: N, others: Vec<(usize, N)>, board: Option<std::sync::Arc<crate::rpc::multi::NodeBoard>>) -> Self {
        let others = others.into_iter().map(|(i, n)| (i, std::sync::Arc::new(n))).collect();
        MultiSubmit { primary: std::sync::Arc::new(primary), others, board }
    }

    /// The primary node.
    pub fn primary(&self) -> &N {
        &self.primary
    }

    /// Submits `tx` to every node. With `primary_decides`, the primary's double spend or missing
    /// input is the answer whatever the other nodes say, and a taker other than the primary answers
    /// only once the primary has answered too.
    async fn submit_to_all(&self, tx: Value, primary_decides: bool) -> Result<[u8; 32], RpcError> {
        if self.others.is_empty() {
            return self.primary.submit(tx).await;
        }
        let (send, mut recv) = tokio::sync::mpsc::unbounded_channel();
        let all = std::iter::once((0usize, self.primary.clone())).chain(self.others.iter().cloned());
        for (i, node) in all {
            let (send, tx) = (send.clone(), tx.clone());
            tokio::spawn(async move {
                let r = node.submit(tx).await;
                let _ = send.send((i, r));
            });
        }
        drop(send);
        let board = self.board.clone();
        // A node that already has the transaction (from a peer, or the chain) took it too.
        let note = move |i: usize, r: &Result<[u8; 32], RpcError>| {
            if let Some(b) = &board {
                let known = matches!(r, Err(RpcError::Node(m)) if classify_submit_error(m) == SubmitOutcome::AlreadyKnown);
                b.submitted(i, r.is_ok() || known, r.as_ref().err().filter(|_| !known).map(|e| e.to_string()));
            }
        };
        let (mut primary_err, mut other_verdict, mut first_err, mut known, mut taken) = (None, None, None, None, None);
        let mut answer: Option<Result<[u8; 32], RpcError>> = None;
        while let Some((i, r)) = recv.recv().await {
            note(i, &r);
            match r {
                // At a recheck the primary decides: the transaction's inputs went elsewhere.
                Err(RpcError::Node(m))
                    if i == 0
                        && primary_decides
                        && matches!(classify_submit_error(&m), SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput) =>
                {
                    answer = Some(Err(RpcError::Node(m)));
                    break;
                }
                // A node that holds the transaction already (or the chain does): it is out there,
                // whatever another node says. The primary's refusal must not drop a transaction a
                // mempool holds, except at a recheck (above).
                Err(RpcError::Node(m)) if classify_submit_error(&m) == SubmitOutcome::AlreadyKnown => {
                    known.get_or_insert(RpcError::Node(m));
                }
                Ok(id) if i == 0 || !primary_decides => {
                    answer = Some(Ok(id));
                    break;
                }
                Ok(id) => {
                    taken.get_or_insert(id);
                }
                Err(e) if i == 0 => primary_err = Some(e),
                Err(e @ RpcError::Node(_)) => {
                    other_verdict.get_or_insert(e);
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if let Some(r) = answer {
            // The rest go on for propagation. Their outcomes are only counted.
            tokio::spawn(async move {
                while let Some((i, r)) = recv.recv().await {
                    note(i, &r);
                }
            });
            return r;
        }
        if let Some(id) = taken {
            return Ok(id);
        }
        if let Some(k) = known {
            return Err(k);
        }
        Err(match primary_err {
            Some(e @ RpcError::Node(_)) => e,
            p => other_verdict.or(p).or(first_err).unwrap_or_else(|| RpcError::Transport("no node took the transaction".into())),
        })
    }
}

impl<N: NodeApi + 'static> NodeApi for MultiSubmit<N> {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.primary.server_info().await
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.primary.dag_info().await
    }

    async fn submit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        self.submit_to_all(tx, false).await
    }

    async fn resubmit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        self.submit_to_all(tx, true).await
    }

    /// The primary decides, since it holds the replaced transaction. The other nodes get the
    /// replacement in the background, and their verdicts are only counted.
    async fn submit_replacement(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let r = self.primary.submit_replacement(tx.clone()).await;
        if r.is_ok() {
            for (i, node) in self.others.iter().cloned() {
                let (tx, board) = (tx.clone(), self.board.clone());
                tokio::spawn(async move {
                    let r = node.submit_replacement(tx).await;
                    if let Some(b) = &board {
                        b.submitted(i, r.is_ok(), r.as_ref().err().map(|e| e.to_string()));
                    }
                });
            }
        }
        r
    }

    async fn chain_from(&self, start_hash: &str) -> Result<ChainUpdate, RpcError> {
        self.primary.chain_from(start_hash).await
    }

    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.primary.utxos_by_addresses(addresses).await
    }

    async fn fee_estimate(&self) -> Result<crate::fee::FeeEstimate, RpcError> {
        self.primary.fee_estimate().await
    }

    async fn tip_timestamp_ms(&self) -> Result<u64, RpcError> {
        self.primary.tip_timestamp_ms().await
    }

    /// Held by any node the transaction was submitted to.
    ///
    /// `Ok(false)` only when every node says it does not hold it, since the transaction went to all
    /// of them. An error from any node, with none holding it, is unsure.
    async fn in_mempool(&self, txid: [u8; 32]) -> Result<bool, RpcError> {
        let all = std::iter::once(self.primary.clone()).chain(self.others.iter().map(|(_, n)| n.clone()));
        let answers = futures_util::future::join_all(all.map(|n| async move { n.in_mempool(txid).await })).await;
        if answers.iter().any(|a| matches!(a, Ok(true))) {
            return Ok(true);
        }
        match answers.into_iter().find_map(Result::err) {
            Some(e) => Err(e),
            None => Ok(false),
        }
    }

    /// Every node's tip timestamp (each within 5 s). A node that does not answer is left out.
    async fn tip_timestamps_ms(&self) -> Vec<u64> {
        let ask = |n: std::sync::Arc<N>| async move {
            tokio::time::timeout(Duration::from_secs(5), n.tip_timestamp_ms()).await.ok().and_then(Result::ok)
        };
        let all = std::iter::once(self.primary.clone()).chain(self.others.iter().map(|(_, n)| n.clone()));
        futures_util::future::join_all(all.map(ask)).await.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        verdict: Result<[u8; 32], RpcError>,
        delay: Duration,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl NodeApi for Fake {
        async fn server_info(&self) -> Result<ServerInfo, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn dag_info(&self) -> Result<DagInfo, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn submit(&self, _tx: Value) -> Result<[u8; 32], RpcError> {
            tokio::time::sleep(self.delay).await;
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.verdict.clone()
        }
        async fn chain_from(&self, _start_hash: &str) -> Result<ChainUpdate, RpcError> {
            Ok(ChainUpdate::default())
        }
        async fn utxos_by_addresses(&self, _addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
            Ok(vec![])
        }
    }

    /// Submissions go to every node. The first that takes the transaction answers. With no taker
    /// the primary's verdict counts, unless the primary failed on the wire and another node gave
    /// one.
    #[tokio::test]
    async fn multi_submit_first_taker_answers_and_every_node_gets_the_transaction() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fake =
            |verdict: Result<[u8; 32], RpcError>, ms: u64| Fake { verdict, delay: Duration::from_millis(ms), calls: calls.clone() };
        let board = crate::rpc::multi::NodeBoard::new(
            &(0..3)
                .map(|i| crate::rpc::multi::NodeSpec {
                    url: format!("n{i}"),
                    fetch: true,
                    submit: true,
                    connections: 1,
                    cross_check: false,
                    may_lead: true,
                })
                .collect::<Vec<_>>(),
        );
        // A slow primary, a fast taker: the fast one answers at once, the primary still gets it.
        let m = MultiSubmit::new(
            fake(Ok([1; 32]), 300),
            vec![(1, fake(Ok([1; 32]), 5)), (2, fake(Err(RpcError::Transport("down".into())), 1))],
            Some(board.clone()),
        );
        let t0 = std::time::Instant::now();
        assert_eq!(m.submit(json!({})).await, Ok([1; 32]));
        assert!(t0.elapsed() < Duration::from_millis(250), "{:?}", t0.elapsed());
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3, "every node got the transaction");
        let st = board.snapshot();
        assert_eq!((st[0].submits_ok, st[1].submits_ok, st[2].submits_failed), (1, 1, 1), "{st:?}");
        // Nobody takes it: the primary's verdict.
        let ds = RpcError::Node("output already spent by transaction x in the mempool".into());
        let m = MultiSubmit::new(fake(Err(ds.clone()), 1), vec![(1, fake(Err(RpcError::Node("orphan".into())), 1))], None);
        assert_eq!(m.submit(json!({})).await, Err(ds.clone()));
        // The primary failed on the wire: another node's verdict.
        let m = MultiSubmit::new(
            fake(Err(RpcError::Timeout(Duration::from_secs(1))), 1),
            vec![(1, fake(Err(RpcError::Node("transaction x is already in the mempool".into())), 1))],
            None,
        );
        assert_eq!(m.submit(json!({})).await, Err(RpcError::Node("transaction x is already in the mempool".into())));
        // The primary refuses, another node already holds it: the transaction is out there.
        let m = MultiSubmit::new(
            fake(Err(ds.clone()), 1),
            vec![(1, fake(Err(RpcError::Node("transaction x is already in the mempool".into())), 5))],
            None,
        );
        assert_eq!(m.submit(json!({})).await, Err(RpcError::Node("transaction x is already in the mempool".into())));
        // A single node: exactly that node.
        let m: MultiSubmit<Fake> = MultiSubmit::new(fake(Ok([2; 32]), 1), vec![], None);
        assert_eq!(m.submit(json!({})).await, Ok([2; 32]));
    }

    /// A stalled or partitioned secondary that still holds the losing copy of a transaction cannot keep it pending. At the
    /// first submission its "already known" keeps the transaction (the race is open). At a recheck the primary's double
    /// spend or missing input wins and the tracker drops it, so its funding coin is free again. A transaction without
    /// order inputs (a keeper job) is dropped no other way.
    #[tokio::test]
    async fn at_a_recheck_the_primary_decides_a_double_spend_or_a_missing_input() {
        use crate::matcher::submit::{ResendClock, Tracked, Tracker, TrackerConfig};
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fake =
            |verdict: Result<[u8; 32], RpcError>, ms: u64| Fake { verdict, delay: Duration::from_millis(ms), calls: calls.clone() };
        let ds =
            RpcError::Node(format!("output ({}, 0) already spent by transaction {} in the mempool", "ab".repeat(32), "cd".repeat(32)));
        let held = RpcError::Node("transaction x is already in the mempool".into());
        let gone = RpcError::Node("transaction x is an orphan where orphan is disallowed".into());
        let m = MultiSubmit::new(fake(Err(ds.clone()), 5), vec![(1, fake(Err(held.clone()), 1))], None);
        assert_eq!(m.submit(json!({})).await, Err(held.clone()), "the first submission: the race is open");
        assert_eq!(m.resubmit(json!({})).await, Err(ds.clone()), "a recheck: the primary decides");
        // at a recheck, a taker other than the primary waits for the primary's verdict
        let m = MultiSubmit::new(fake(Err(gone.clone()), 30), vec![(1, fake(Ok([3; 32]), 1))], None);
        assert_eq!(m.submit(json!({})).await, Ok([3; 32]));
        assert_eq!(m.resubmit(json!({})).await, Err(gone));
        // the primary has no verdict, or refuses for another reason: as at the first submission
        let m = MultiSubmit::new(fake(Err(RpcError::Transport("down".into())), 1), vec![(1, fake(Err(held.clone()), 5))], None);
        assert_eq!(m.resubmit(json!({})).await, Err(held.clone()));
        let m =
            MultiSubmit::new(fake(Err(RpcError::Node("transaction x is invalid".into())), 1), vec![(1, fake(Ok([4; 32]), 5))], None);
        assert_eq!(m.resubmit(json!({})).await, Ok([4; 32]));
        // through the tracker: a keeper job pending past the timeout, a secondary still holding it
        let m = MultiSubmit::new(fake(Err(ds.clone()), 5), vec![(1, fake(Err(held), 1))], None);
        let mut tr = Tracker::new(TrackerConfig::default());
        let t = Tracked {
            txid: [7; 32],
            spends: [([8; 32], 0)].into_iter().collect(),
            orders: [[9; 32]].into_iter().collect(),
            owners: Default::default(),
            addresses: Default::default(),
            parent: None,
            submitted_daa: 1_000,
            rpc_tx: json!({}),
            accepted: None,
            kind: "refund".into(),
            profit: 1,
            fee: 1,
            fee_rate: 100,
            unconfirmed: false,
            deadline: None,
            resend_until: None,
            retired: None,
        };
        tr.txs.insert(t.txid, t);
        let at = 1_000 + tr.cfg.pending_timeout;
        let r = tr.recheck_pending(&m, &ResendClock::at(at), true).await;
        assert_eq!(r.dropped.len(), 1, "{r:?}");
        assert!(tr.txs.is_empty() && tr.spent_outpoints().is_empty(), "its inputs are free again");
        assert!(tr.backed_off(at).contains(&[9; 32]));
    }

    #[test]
    fn submit_errors_are_classified() {
        let c = classify_submit_error;
        assert_eq!(c("output abc:0 already spent by transaction def in the mempool"), SubmitOutcome::DoubleSpend);
        assert_eq!(c("transaction abc is already in the mempool"), SubmitOutcome::AlreadyKnown);
        assert_eq!(c("transaction abc was already accepted by the consensus"), SubmitOutcome::AlreadyKnown);
        assert_eq!(c("at least one outpoint of the transaction is lacking a matching UTXO entry"), SubmitOutcome::MissingInput);
        assert_eq!(c("transaction abc is an orphan where orphan is disallowed"), SubmitOutcome::MissingInput);
        assert_eq!(
            c("transaction could not be added to the mempool because it's full with transactions with higher priority"),
            SubmitOutcome::Busy
        );
        assert_eq!(c("transaction abc is invalid"), SubmitOutcome::Rejected);
    }

    #[test]
    fn frames_and_chain_updates_parse() {
        assert_eq!(parse_frame(r#"{"id":2,"params":{}}"#, 1).unwrap(), None);
        assert_eq!(parse_frame(r#"{"id":1,"params":{"x":1}}"#, 1).unwrap(), Some(json!({"x":1})));
        assert_eq!(parse_frame(r#"{"id":1,"error":{"message":"bad"}}"#, 1), Err(RpcError::Node("bad".into())));
        let txid = "ab".repeat(32);
        let p = json!({
            "removedChainBlockHashes": ["01"],
            "addedChainBlockHashes": ["02", "03"],
            "chainBlockAcceptedTransactions": [
                {"chainBlockHeader": {"hash": "02", "daaScore": 10}, "acceptedTransactions": [{"verboseData": {"transactionId": txid}}]},
                {"chainBlockHeader": {"hash": "03", "daaScore": 11}, "acceptedTransactions": []}
            ]
        });
        let u = parse_chain_update(&p).unwrap();
        assert_eq!(u.removed, vec!["01".to_string()]);
        assert_eq!(u.added.len(), 2);
        assert_eq!(u.added[0].accepted, vec![[0xab; 32]]);
        assert_eq!(u.added[1].daa_score, 11);
        let bad = json!({"addedChainBlockHashes": ["02"], "chainBlockAcceptedTransactions": []});
        assert!(parse_chain_update(&bad).is_err());
        // a chain block without its DAA score is refused, not taken as DAA 0
        for header in [json!({"hash": "02"}), Value::Null] {
            let p = json!({
                "addedChainBlockHashes": ["02"],
                "chainBlockAcceptedTransactions": [{"chainBlockHeader": header, "acceptedTransactions": []}]
            });
            assert!(matches!(parse_chain_update(&p), Err(RpcError::Decode(m)) if m.contains("without DAA score")));
        }
    }
}
