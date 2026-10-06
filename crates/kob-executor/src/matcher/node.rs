//! Node access over the node's JSON wRPC (rusty-kaspa v2.1.0 `RpcApiOps`, camelCase).
//!
//! The matcher needs five calls: `getServerInfo` (network, sync, virtual DAA score),
//! `getBlockDagInfo` (sink, the acceptance cursor), `submitTransaction` (v1 transactions with
//! per-input compute budgets: the REST API drops them), `getVirtualChainFromBlockV2` at `Low`
//! verbosity (accepted transaction ids per chain block, and `removed` blocks for reorgs) and
//! `getUtxosByAddresses` (the operator's funding; needs `--utxoindex`), and `getFeeEstimate` for the fee policy
//! (`crate::fee`). Only plain `ws://` is
//! supported: run the node's RPC on loopback (`docs/ops/executor.md`).

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
    #[error("transport: {0}")]
    Transport(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    /// The node answered with an error message.
    #[error("node: {0}")]
    Node(String),
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
    /// No verdict: the request failed on the wire (transport error, timeout, undecodable answer), so the node may or may not have
    /// taken the transaction. It is tracked as pending, its inputs stay reserved, and it is resent idempotently until
    /// the node answers or the pending timeout drops it.
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
    #[serde(default)]
    pub server_version: String,
    pub network_id: String,
    pub is_synced: bool,
    #[serde(default)]
    pub has_utxo_index: bool,
    pub virtual_daa_score: u64,
}

/// Subset of `getBlockDagInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagInfo {
    #[serde(default)]
    pub network: String,
    pub sink: String,
    pub virtual_daa_score: u64,
}

/// One added chain block with the ids of the transactions it accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBlock {
    pub hash: String,
    pub daa_score: u64,
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
    pub transaction_id: [u8; 32],
    pub index: u32,
    pub amount: u64,
    /// `version ‖ script` hex.
    pub script_public_key: String,
    pub block_daa_score: u64,
    pub covenant_id: Option<[u8; 32]>,
}

/// Everything the matcher and keepers ask of a node. Implemented by [`WrpcNode`] and by test nodes.
pub trait NodeApi: Send + Sync {
    fn server_info(&self) -> impl Future<Output = Result<ServerInfo, RpcError>> + Send;
    fn dag_info(&self) -> impl Future<Output = Result<DagInfo, RpcError>> + Send;
    /// Submits an `RpcTransaction` JSON; returns the transaction id or the node's message.
    fn submit(&self, tx: Value) -> impl Future<Output = Result<[u8; 32], RpcError>> + Send;
    fn chain_from(&self, start_hash: &str) -> impl Future<Output = Result<ChainUpdate, RpcError>> + Send;
    fn utxos_by_addresses(&self, addresses: &[String]) -> impl Future<Output = Result<Vec<AddressUtxo>, RpcError>> + Send;
    /// The node's fee estimate (`getFeeEstimate`, `crate::fee`). The default: none (the fee policy pays its floor).
    fn fee_estimate(&self) -> impl Future<Output = Result<crate::fee::FeeEstimate, RpcError>> + Send {
        async { Err(RpcError::Node("no fee estimate".into())) }
    }
}

/// Connection settings.
#[derive(Debug, Clone)]
pub struct WrpcConfig {
    pub url: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_message_bytes: usize,
}

impl WrpcConfig {
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
    /// Bounds each `chain_from` batch (`crate::rpc::window`): a matcher that fell behind under a flood
    /// pages through the backlog instead of asking for all of it and timing out forever.
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
    pub fn new(cfg: WrpcConfig) -> Self {
        let window = std::sync::Mutex::new(crate::rpc::window::BatchWindow::for_timeout(cfg.request_timeout));
        WrpcNode { cfg, conn: tokio::sync::Mutex::new(None), next_id: std::sync::atomic::AtomicU64::new(1), window }
    }

    fn win(&self) -> std::sync::MutexGuard<'_, crate::rpc::window::BatchWindow> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The window of a `chain_from` request (unbounded when the node does not know `start`: the request
    /// then fails fast and the runner restarts from the sink).
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
        let ws_cfg = WebSocketConfig::default()
            .max_message_size(Some(self.cfg.max_message_bytes))
            .max_frame_size(Some(self.cfg.max_message_bytes));
        let fut = tokio_tungstenite::connect_async_with_config(self.cfg.url.as_str(), Some(ws_cfg), false);
        match tokio::time::timeout(self.cfg.connect_timeout, fut).await {
            Err(_) => Err(RpcError::Timeout(self.cfg.connect_timeout)),
            Ok(Err(e)) => Err(RpcError::Transport(e.to_string())),
            Ok(Ok((ws, _))) => Ok(ws),
        }
    }

    /// One request; returns the response `params`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_within(method, params, self.cfg.request_timeout).await
    }

    /// [`WrpcNode::call`] with another timeout.
    pub async fn call_within(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connect().await?);
        }
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = json!({"id": id, "method": method, "params": params}).to_string();
        let ws = guard.as_mut().expect("connected");
        let res = tokio::time::timeout(timeout, async {
            ws.send(Message::text(req)).await.map_err(|e| RpcError::Transport(e.to_string()))?;
            loop {
                let msg = match ws.next().await {
                    None => return Err(RpcError::Transport("connection closed".into())),
                    Some(Err(e)) => return Err(RpcError::Transport(e.to_string())),
                    Some(Ok(m)) => m,
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => return Err(RpcError::Transport("closed by node".into())),
                    _ => continue,
                };
                if let Some(p) = parse_frame(text.as_str(), id)? {
                    return Ok(p);
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
                Err(e)
            }
            Err(_) => {
                *guard = None;
                Err(RpcError::Timeout(timeout))
            }
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
        let daa_score = header.get("daaScore").and_then(Value::as_u64).unwrap_or(0);
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
        decode("getServerInfo", self.call("getServerInfo", json!({})).await?)
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        decode("getBlockDagInfo", self.call("getBlockDagInfo", json!({})).await?)
    }

    async fn submit(&self, tx: Value) -> Result<[u8; 32], RpcError> {
        let p = self.call("submitTransaction", json!({"transaction": tx, "allowOrphan": false})).await?;
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
            // the next chain block lies beyond the window: widen it, apply nothing (the next tick asks again)
            self.win().on_empty();
            return Ok(ChainUpdate::default());
        }
        self.win().on_success(t0.elapsed(), plan.by_window());
        Ok(u)
    }

    async fn fee_estimate(&self) -> Result<crate::fee::FeeEstimate, RpcError> {
        // a small answer: a short timeout, so a stalled node delays a tick by seconds, not by the request timeout
        let p = self.call_within("getFeeEstimate", json!({}), self.cfg.request_timeout.min(Duration::from_secs(5))).await?;
        crate::fee::FeeEstimate::parse(&p).map_err(RpcError::Decode)
    }

    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        let p = self.call("getUtxosByAddresses", json!({"addresses": addresses})).await?;
        let entries = p.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
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

/// The matcher's node with more nodes for submissions (`rpc::multi`): every call goes to the primary, and a transaction is
/// submitted to every node at once. The first node that takes it answers the matcher; the others go on in the background
/// (propagation: the transaction reaches the miners through several peers). When none takes it, the primary's rejection is
/// the answer (it decides double spends and missing inputs), unless the primary failed on the wire and another node gave a
/// verdict.
pub struct MultiSubmit<N: NodeApi + 'static> {
    primary: std::sync::Arc<N>,
    /// `(index in the node board, node)`.
    others: Vec<(usize, std::sync::Arc<N>)>,
    board: Option<std::sync::Arc<crate::rpc::multi::NodeBoard>>,
}

impl<N: NodeApi + 'static> MultiSubmit<N> {
    pub fn new(primary: N, others: Vec<(usize, N)>, board: Option<std::sync::Arc<crate::rpc::multi::NodeBoard>>) -> Self {
        let others = others.into_iter().map(|(i, n)| (i, std::sync::Arc::new(n))).collect();
        MultiSubmit { primary: std::sync::Arc::new(primary), others, board }
    }

    pub fn primary(&self) -> &N {
        &self.primary
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
        // a node that already has the transaction (from a peer, or the chain) took it too
        let note = move |i: usize, r: &Result<[u8; 32], RpcError>| {
            if let Some(b) = &board {
                let known = matches!(r, Err(RpcError::Node(m)) if classify_submit_error(m) == SubmitOutcome::AlreadyKnown);
                b.submitted(i, r.is_ok() || known, r.as_ref().err().filter(|_| !known).map(|e| e.to_string()));
            }
        };
        let (mut primary_err, mut other_verdict, mut first_err) = (None, None, None);
        while let Some((i, r)) = recv.recv().await {
            note(i, &r);
            match r {
                Ok(id) => {
                    // the rest go on for propagation; their outcomes are only counted
                    tokio::spawn(async move {
                        while let Some((i, r)) = recv.recv().await {
                            note(i, &r);
                        }
                    });
                    return Ok(id);
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
        Err(match primary_err {
            Some(e @ RpcError::Node(_)) => e,
            p => other_verdict.or(p).or(first_err).unwrap_or_else(|| RpcError::Transport("no node took the transaction".into())),
        })
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

    /// Submissions go to every node; the first that takes the transaction answers; with no taker the primary's verdict
    /// counts, unless the primary failed on the wire and another node gave one.
    #[tokio::test]
    async fn multi_submit_first_taker_answers_and_every_node_gets_the_transaction() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fake =
            |verdict: Result<[u8; 32], RpcError>, ms: u64| Fake { verdict, delay: Duration::from_millis(ms), calls: calls.clone() };
        let board = crate::rpc::multi::NodeBoard::new(
            &(0..3)
                .map(|i| crate::rpc::multi::NodeSpec { url: format!("n{i}"), fetch: true, submit: true, connections: 1 })
                .collect::<Vec<_>>(),
        );
        // a slow primary, a fast taker: the fast one answers at once, the primary still gets it
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
        // nobody takes it: the primary's verdict
        let ds = RpcError::Node("output already spent by transaction x in the mempool".into());
        let m = MultiSubmit::new(fake(Err(ds.clone()), 1), vec![(1, fake(Err(RpcError::Node("orphan".into())), 1))], None);
        assert_eq!(m.submit(json!({})).await, Err(ds));
        // the primary failed on the wire: another node's verdict
        let m = MultiSubmit::new(
            fake(Err(RpcError::Timeout(Duration::from_secs(1))), 1),
            vec![(1, fake(Err(RpcError::Node("transaction x is already in the mempool".into())), 1))],
            None,
        );
        assert_eq!(m.submit(json!({})).await, Err(RpcError::Node("transaction x is already in the mempool".into())));
        // a single node: exactly that node
        let m: MultiSubmit<Fake> = MultiSubmit::new(fake(Ok([2; 32]), 1), vec![], None);
        assert_eq!(m.submit(json!({})).await, Ok([2; 32]));
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
    }
}
