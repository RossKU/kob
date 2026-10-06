//! The trusted chain view over a Kaspa node's wRPC (JSON encoding).
//!
//! [`WrpcClient`] is a synchronous websocket JSON client (one connection behind a mutex, reconnected
//! on demand, every call under an overall deadline). [`NodeChain`] implements
//! [`kob_x402::chain::ChainView`] on top of any [`Rpc`]:
//!
//! | ChainView | RPC |
//! |---|---|
//! | `utxos`, `utxos_of` | `getUtxosByAddresses` (the address of each claimed script is derived locally) |
//! | `virtual_daa_score` | `getBlockDagInfo` -> `virtualDaaScore` |
//! | `in_mempool` | `getMempoolEntry` (`... not found` is `false`) |
//! | `output_status` | accepted iff the outpoint is in the virtual UTXO set of the address, else mempool presence, else unknown |
//! | `submit` | `submitTransaction` (node JSON from `kob_protocol::issue::rpc_transaction_json`) |
//!
//! The RPC is used rather than REST because REST drops the per-input compute budget. Only plain
//! `ws://` is supported (put a TLS-terminating proxy in front of a remote node).
//!
//! Shapes were verified against a live TN10 node: `getUtxosByAddresses` answers
//! `{"entries":[{"address","outpoint":{"transactionId","index"},"utxoEntry":{"amount","scriptPublicKey"
//! (version-prefixed hex),"blockDaaScore","isCoinbase","covenantId"}}]}`; a `getMempoolEntry` miss is the
//! error `Transaction <id> not found`; a submit with unknown inputs is
//! `Rejected transaction <id>: transaction <id> is an orphan where orphan is disallowed`.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::safe_tx::spk_from_hex;
use kob_x402::wire::{hex, parse_hash32, Network};
use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

/// Errors of one RPC call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    /// Connection, websocket or timeout failure: the request may or may not have been processed.
    #[error("transport: {0}")]
    Transport(String),
    /// The node answered with an error object (the request was processed and refused).
    #[error("node: {0}")]
    Node(String),
    /// The reply was not the expected JSON.
    #[error("protocol: {0}")]
    Protocol(String),
}

/// A JSON RPC endpoint.
pub trait Rpc: Send + Sync {
    /// Calls `method` and returns the response `params`.
    fn call(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}

// ------------------------------------------------------------------------------------- wire client

/// What one incoming text frame means for a pending request.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// A frame for another request or a notification.
    Other,
    /// The `params` of the awaited response.
    Ok(Value),
}

/// Interprets one text frame for request `id`.
pub fn parse_reply(text: &str, id: u64) -> Result<Reply, RpcError> {
    let v: Value = serde_json::from_str(text).map_err(|e| RpcError::Protocol(format!("not JSON: {e}")))?;
    if v.get("id").and_then(Value::as_u64) != Some(id) {
        return Ok(Reply::Other);
    }
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let msg = err.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| err.to_string());
        return Err(RpcError::Node(msg));
    }
    match v.get("params") {
        Some(p) => Ok(Reply::Ok(p.clone())),
        None => Err(RpcError::Protocol("response without params or error".into())),
    }
}

type Ws = WebSocket<TcpStream>;

/// Connections kept per node.
const POOL_SIZE: usize = 4;

struct Conn {
    ws: Ws,
    last_used: Instant,
}

/// Synchronous wRPC JSON client with one reconnecting connection.
pub struct WrpcClient {
    url: String,
    timeout: Duration,
    /// A pooled connection idle for longer than this is replaced before a call (a stale socket would
    /// make a submit outcome ambiguous for no reason).
    max_idle: Duration,
    /// A small pool: sequential calls reuse the first connection, concurrent calls spill to the others.
    conns: Vec<Mutex<Option<Conn>>>,
    next_id: AtomicU64,
}

impl WrpcClient {
    /// `url` is `ws://host:port` (JSON wRPC: 18110 mainnet, 18210 testnet-10); `timeout` bounds connect
    /// and every call.
    pub fn new(url: &str, timeout: Duration) -> WrpcClient {
        WrpcClient {
            url: url.to_string(),
            timeout,
            max_idle: Duration::from_secs(15),
            conns: (0..POOL_SIZE).map(|_| Mutex::new(None)).collect(),
            next_id: AtomicU64::new(1),
        }
    }

    /// The first idle pooled slot, else waits for the first one.
    fn checkout(&self) -> std::sync::MutexGuard<'_, Option<Conn>> {
        use std::sync::TryLockError;
        for slot in &self.conns {
            match slot.try_lock() {
                Ok(g) => return g,
                Err(TryLockError::Poisoned(p)) => return p.into_inner(),
                Err(TryLockError::WouldBlock) => {}
            }
        }
        self.conns[0].lock().unwrap_or_else(|p| p.into_inner())
    }

    fn connect(&self) -> Result<Conn, RpcError> {
        let rest = self.url.strip_prefix("ws://").ok_or_else(|| RpcError::Transport("only ws:// node urls are supported".into()))?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let addr = authority
            .to_socket_addrs()
            .map_err(|e| RpcError::Transport(format!("resolve {authority}: {e}")))?
            .next()
            .ok_or_else(|| RpcError::Transport(format!("no address for {authority}")))?;
        let stream = TcpStream::connect_timeout(&addr, self.timeout).map_err(|e| RpcError::Transport(format!("connect: {e}")))?;
        stream.set_read_timeout(Some(self.timeout)).map_err(|e| RpcError::Transport(e.to_string()))?;
        stream.set_write_timeout(Some(self.timeout)).map_err(|e| RpcError::Transport(e.to_string()))?;
        let _ = stream.set_nodelay(true);
        let (ws, _) = tungstenite::client(self.url.as_str(), stream).map_err(|e| RpcError::Transport(format!("handshake: {e}")))?;
        Ok(Conn { ws, last_used: Instant::now() })
    }

    fn exchange(&self, conn: &mut Conn, method: &str, params: &Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + self.timeout;
        let req = json!({ "id": id, "method": method, "params": params });
        conn.ws.send(Message::text(req.to_string())).map_err(|e| RpcError::Transport(format!("send: {e}")))?;
        loop {
            if Instant::now() >= deadline {
                return Err(RpcError::Transport("timed out waiting for the response".into()));
            }
            match conn.ws.read() {
                Ok(Message::Text(t)) => match parse_reply(t.as_str(), id)? {
                    Reply::Other => continue,
                    Reply::Ok(p) => {
                        conn.last_used = Instant::now();
                        return Ok(p);
                    }
                },
                Ok(Message::Close(_)) => return Err(RpcError::Transport("connection closed before the response".into())),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(RpcError::Transport("timed out waiting for the response".into()))
                }
                Err(e) => return Err(RpcError::Transport(format!("read: {e}"))),
            }
        }
    }
}

impl Rpc for WrpcClient {
    fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut guard = self.checkout();
        if guard.as_ref().is_some_and(|c| c.last_used.elapsed() > self.max_idle) {
            *guard = None;
        }
        // Idempotent reads retry once on a fresh connection; a submit is never resent by the client
        // (its outcome is decided by the caller: `SubmitError::Unavailable` keeps the evidence consumed).
        let attempts = if method == "submitTransaction" { 1 } else { 2 };
        let mut last = RpcError::Transport("no attempt".into());
        for _ in 0..attempts {
            let mut conn = match guard.take() {
                Some(c) => c,
                None => match self.connect() {
                    Ok(c) => c,
                    Err(e) => {
                        last = e;
                        continue;
                    }
                },
            };
            match self.exchange(&mut conn, method, &params) {
                Ok(v) => {
                    *guard = Some(conn);
                    return Ok(v);
                }
                Err(e @ (RpcError::Node(_) | RpcError::Protocol(_))) => {
                    // the connection itself is healthy
                    *guard = Some(conn);
                    return Err(e);
                }
                Err(e) => last = e, // transport failure: drop the connection
            }
        }
        Err(last)
    }
}

// ------------------------------------------------------------------------------------- chain view

/// [`ChainView`] over a node.
pub struct NodeChain {
    rpc: Box<dyn Rpc>,
    network: Network,
}

impl NodeChain {
    /// A view over the wRPC endpoint `url`.
    pub fn connect(url: &str, network: Network, timeout: Duration) -> NodeChain {
        NodeChain { rpc: Box::new(WrpcClient::new(url, timeout)), network }
    }

    /// A view over any [`Rpc`] (tests).
    pub fn with_rpc(rpc: Box<dyn Rpc>, network: Network) -> NodeChain {
        NodeChain { rpc, network }
    }

    fn address_of(&self, spk: &ScriptPublicKey) -> Option<String> {
        kaspa_txscript::extract_script_pub_key_address(spk, self.network.prefix()).ok().map(|a| a.to_string())
    }

    fn read(&self, method: &str, params: Value) -> Result<Value, ChainError> {
        self.rpc.call(method, params).map_err(|e| ChainError(format!("{method}: {e}")))
    }

    /// Every unspent output of `addresses`: `address -> [(outpoint, utxo)]`.
    fn utxos_by_addresses(&self, addresses: &[String]) -> Result<HashMap<String, Vec<(Outpoint, ChainUtxo)>>, ChainError> {
        let mut out: HashMap<String, Vec<(Outpoint, ChainUtxo)>> = HashMap::new();
        if addresses.is_empty() {
            return Ok(out);
        }
        let reply = self.read("getUtxosByAddresses", json!({ "addresses": addresses }))?;
        for (address, op, u) in parse_utxo_entries(&reply)? {
            out.entry(address).or_default().push((op, u));
        }
        Ok(out)
    }
}

/// Why the startup node check failed.
#[derive(Debug)]
pub enum NetworkCheck {
    /// The node is another network or lacks the UTXO index: refuse to start.
    Mismatch(String),
    /// The node could not be queried or is not synced: start, but fail closed until it is.
    Unreachable(String),
}

impl NodeChain {
    /// `getServerInfo`: the node must serve the configured network with a UTXO index.
    pub fn check_network(&self) -> Result<(), NetworkCheck> {
        let info = self.rpc.call("getServerInfo", json!({})).map_err(|e| NetworkCheck::Unreachable(e.to_string()))?;
        let id = info.get("networkId").and_then(Value::as_str).unwrap_or("");
        if id != self.network.registry_name() {
            return Err(NetworkCheck::Mismatch(format!("the node serves {id:?}, the facilitator is configured for {}", self.network)));
        }
        if info.get("hasUtxoIndex").and_then(Value::as_bool) != Some(true) {
            return Err(NetworkCheck::Mismatch("the node runs without --utxoindex".into()));
        }
        if info.get("isSynced").and_then(Value::as_bool) != Some(true) {
            return Err(NetworkCheck::Unreachable("the node is not synced".into()));
        }
        Ok(())
    }
}

/// Parses the `getUtxosByAddresses` response.
pub fn parse_utxo_entries(reply: &Value) -> Result<Vec<(String, Outpoint, ChainUtxo)>, ChainError> {
    let entries = reply
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| ChainError("getUtxosByAddresses: reply without an entries array".into()))?;
    let bad = |what: &str| ChainError(format!("getUtxosByAddresses: malformed entry ({what})"));
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let address = e.get("address").and_then(Value::as_str).ok_or_else(|| bad("address"))?.to_string();
        let o = e.get("outpoint").ok_or_else(|| bad("outpoint"))?;
        let txid = o.get("transactionId").and_then(Value::as_str).and_then(parse_hash32).ok_or_else(|| bad("transactionId"))?;
        let index = o.get("index").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()).ok_or_else(|| bad("index"))?;
        let u = e.get("utxoEntry").ok_or_else(|| bad("utxoEntry"))?;
        let amount = u.get("amount").and_then(Value::as_u64).ok_or_else(|| bad("amount"))?;
        let spk = u
            .get("scriptPublicKey")
            .and_then(Value::as_str)
            .and_then(|s| spk_from_hex(s).ok())
            .ok_or_else(|| bad("scriptPublicKey"))?;
        let block_daa_score = u.get("blockDaaScore").and_then(Value::as_u64).ok_or_else(|| bad("blockDaaScore"))?;
        let is_coinbase = u.get("isCoinbase").and_then(Value::as_bool).ok_or_else(|| bad("isCoinbase"))?;
        let covenant_id = match u.get("covenantId") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_str().and_then(parse_hash32).ok_or_else(|| bad("covenantId"))?),
        };
        out.push((
            address,
            Outpoint::new(txid, index),
            ChainUtxo { amount, script_public_key: spk, block_daa_score, is_coinbase, covenant_id },
        ));
    }
    Ok(out)
}

/// Maps the node's `submitTransaction` error text to a [`SubmitError`].
///
/// Texts (rusty-kaspa v2.1.0): `transaction <id> is already in the mempool`, `... was already accepted
/// by the consensus`, `output <o> already spent by transaction <t> in the mempool`, `at least one
/// outpoint of the transaction is lacking a matching UTXO entry`, `... is an orphan where orphan is
/// disallowed`; anything else (mass, fee, script, standardness) is a rejection.
pub fn map_submit_error(msg: &str) -> SubmitError {
    let m = msg.to_ascii_lowercase();
    if m.contains("already in the mempool") || m.contains("already accepted by the consensus") {
        SubmitError::AlreadyKnown
    } else if m.contains("already spent")
        || m.contains("double spend")
        || m.contains("double-spend")
        || m.contains("lacking a matching utxo")
        || m.contains("impossible to have a matching utxo")
        || m.contains("missing outpoint")
        || m.contains("orphan")
    {
        SubmitError::Conflict(msg.to_string())
    } else {
        SubmitError::Rejected(msg.to_string())
    }
}

impl ChainView for NodeChain {
    fn utxos(&self, wanted: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        let mut addrs: Vec<String> = Vec::new();
        let derived: Vec<Option<String>> = wanted.iter().map(|(_, spk)| self.address_of(spk)).collect();
        for a in derived.iter().flatten() {
            if !addrs.contains(a) {
                addrs.push(a.clone());
            }
        }
        let by_addr = self.utxos_by_addresses(&addrs)?;
        Ok(wanted
            .iter()
            .zip(derived)
            .map(|((op, spk), addr)| {
                let list = by_addr.get(&addr?)?;
                // the script must match exactly, not only the address derived from it
                list.iter().find(|(o, u)| o == op && &u.script_public_key == spk).map(|(_, u)| u.clone())
            })
            .collect())
    }

    fn utxos_of(&self, spk: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        let Some(addr) = self.address_of(spk) else { return Ok(Vec::new()) };
        let mut by_addr = self.utxos_by_addresses(std::slice::from_ref(&addr))?;
        Ok(by_addr.remove(&addr).unwrap_or_default().into_iter().filter(|(_, u)| &u.script_public_key == spk).collect())
    }

    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        let r = self.read("getBlockDagInfo", json!({}))?;
        r.get("virtualDaaScore").and_then(Value::as_u64).ok_or_else(|| ChainError("getBlockDagInfo: no virtualDaaScore".into()))
    }

    fn output_status(&self, out: &Outpoint, spk: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        // The cheap mempool query first: while the transaction waits there, the (address-wide, possibly
        // large) UTXO query is skipped. A transaction that is accepted but not yet dropped from the
        // mempool reports `Mempool` for a moment, which only delays success.
        if self.in_mempool(&out.txid)? {
            return Ok(OutputStatus::Mempool);
        }
        if let Some((_, u)) = self.utxos_of(spk)?.into_iter().find(|(o, _)| o == out) {
            return Ok(OutputStatus::Accepted { block_daa_score: u.block_daa_score });
        }
        Ok(OutputStatus::Unknown)
    }

    fn in_mempool(&self, txid: &Txid) -> Result<bool, ChainError> {
        let params = json!({ "transactionId": hex(txid), "includeOrphanPool": false, "filterTransactionPool": false });
        match self.rpc.call("getMempoolEntry", params) {
            Ok(_) => Ok(true),
            Err(RpcError::Node(m)) if m.to_ascii_lowercase().contains("not found") => Ok(false),
            Err(e) => Err(ChainError(format!("getMempoolEntry: {e}"))),
        }
    }

    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let local = tx.id().as_bytes();
        let params = json!({ "transaction": kob_protocol::issue::rpc_transaction_json(tx), "allowOrphan": false });
        match self.rpc.call("submitTransaction", params) {
            Ok(r) => match r.get("transactionId").and_then(Value::as_str).and_then(parse_hash32) {
                Some(id) if id == local => Ok(id),
                Some(id) => Err(SubmitError::Unavailable(format!("node reports transaction id {} for {}", hex(&id), hex(&local)))),
                None => Err(SubmitError::Unavailable("submitTransaction: no transactionId in the reply".into())),
            },
            Err(RpcError::Node(m)) => Err(map_submit_error(&m)),
            Err(e) => Err(SubmitError::Unavailable(e.to_string())),
        }
    }

    /// `submitTransactionReplacement` (replace-by-fee: the node drops the mempool transactions that spend an input of `tx` when
    /// `tx` pays a higher fee rate).
    fn replace(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let local = tx.id().as_bytes();
        let params = json!({ "transaction": kob_protocol::issue::rpc_transaction_json(tx) });
        match self.rpc.call("submitTransactionReplacement", params) {
            Ok(r) => match r.get("transactionId").and_then(Value::as_str).and_then(parse_hash32) {
                Some(id) if id == local => Ok(id),
                Some(id) => Err(SubmitError::Unavailable(format!("node reports transaction id {} for {}", hex(&id), hex(&local)))),
                None => Err(SubmitError::Unavailable("submitTransactionReplacement: no transactionId in the reply".into())),
            },
            Err(RpcError::Node(m)) => Err(map_submit_error(&m)),
            Err(e) => Err(SubmitError::Unavailable(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_x402::testkit::{p2pk_spk, pubkey};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    type Handler = Box<dyn Fn(&str, &Value) -> Result<Value, RpcError> + Send + Sync>;

    /// A scripted [`Rpc`]: `(method, reply)` handlers, records the calls.
    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
        handler: Handler,
    }

    impl Rpc for Arc<Fake> {
        fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
            self.calls.lock().unwrap().push((method.to_string(), params.clone()));
            (self.handler)(method, &params)
        }
    }

    fn fake(h: impl Fn(&str, &Value) -> Result<Value, RpcError> + Send + Sync + 'static) -> (NodeChain, Arc<Fake>) {
        let f = Arc::new(Fake { calls: Mutex::new(vec![]), handler: Box::new(h) });
        (NodeChain::with_rpc(Box::new(f.clone()), Network::Testnet10), f)
    }

    fn addr_of(spk: &ScriptPublicKey) -> String {
        kaspa_txscript::extract_script_pub_key_address(spk, Network::Testnet10.prefix()).unwrap().to_string()
    }

    fn entry(address: &str, txid: &str, index: u32, amount: u64, spk: &ScriptPublicKey, daa: u64, cov: Option<&str>) -> Value {
        json!({
            "address": address,
            "outpoint": { "transactionId": txid, "index": index },
            "utxoEntry": {
                "amount": amount,
                "scriptPublicKey": kob_x402::safe_tx::spk_to_hex(spk),
                "blockDaaScore": daa,
                "isCoinbase": false,
                "covenantId": cov,
            }
        })
    }

    #[test]
    fn utxos_are_looked_up_by_the_derived_address_and_match_the_script_exactly() {
        let spk = p2pk_spk(&pubkey(1));
        let other = p2pk_spk(&pubkey(2));
        let a = addr_of(&spk);
        let t1 = "11".repeat(32);
        let t2 = "22".repeat(32);
        let cov = "ab".repeat(32);
        let (a2, t1c, t2c, spk2, covc) = (a.clone(), t1.clone(), t2.clone(), spk.clone(), cov.clone());
        let (chain, f) = fake(move |m, p| {
            assert_eq!(m, "getUtxosByAddresses");
            assert!(p["addresses"].as_array().unwrap().contains(&json!(a2)));
            Ok(json!({ "entries": [
                entry(&a2, &t1c, 0, 500, &spk2, 42, None),
                entry(&a2, &t2c, 3, 700, &spk2, 43, Some(&covc)),
            ]}))
        });
        let op1 = Outpoint::new(parse_hash32(&t1).unwrap(), 0);
        let op2 = Outpoint::new(parse_hash32(&t2).unwrap(), 3);
        let missing = Outpoint::new([9; 32], 0);
        let r = chain.utxos(&[(op1, spk.clone()), (op2, spk.clone()), (missing, spk.clone()), (op1, other.clone())]).unwrap();
        assert_eq!(r[0].as_ref().unwrap().amount, 500);
        assert_eq!(r[0].as_ref().unwrap().block_daa_score, 42);
        assert_eq!(r[0].as_ref().unwrap().covenant_id, None);
        assert_eq!(r[1].as_ref().unwrap().covenant_id, Some([0xab; 32]));
        assert!(r[2].is_none(), "not in the set");
        // the same outpoint claimed under another script is not the entry
        assert!(r[3].is_none());
        // one call for the distinct addresses; `other` is a second address
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["addresses"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn malformed_utxo_replies_fail_closed() {
        for bad in [
            json!({}),
            json!({"entries": [{"address": "a"}]}),
            json!({"entries": [{"address": "a", "outpoint": {"transactionId": "zz", "index": 0}, "utxoEntry": {}}]}),
        ] {
            assert!(parse_utxo_entries(&bad).is_err(), "{bad}");
        }
        let spk = p2pk_spk(&pubkey(1));
        let mut e = entry("a", &"11".repeat(32), 0, 5, &spk, 1, None);
        e["utxoEntry"]["covenantId"] = json!("nothex");
        assert!(parse_utxo_entries(&json!({"entries": [e]})).is_err());
    }

    #[test]
    fn output_status_accepted_mempool_unknown() {
        let spk = p2pk_spk(&pubkey(1));
        let a = addr_of(&spk);
        let t1 = "11".repeat(32);
        let (a2, t1c, spk2) = (a.clone(), t1.clone(), spk.clone());
        let (chain, _) = fake(move |m, p| match m {
            "getUtxosByAddresses" => Ok(json!({ "entries": [entry(&a2, &t1c, 0, 500, &spk2, 77, None)] })),
            "getMempoolEntry" => {
                let id = p["transactionId"].as_str().unwrap();
                assert_eq!(p["includeOrphanPool"], false);
                if id == "33".repeat(32) {
                    Ok(json!({ "mempoolEntry": { "fee": 1 } }))
                } else {
                    Err(RpcError::Node(format!("Transaction {id} not found")))
                }
            }
            other => panic!("unexpected {other}"),
        });
        let acc = Outpoint::new(parse_hash32(&t1).unwrap(), 0);
        assert_eq!(chain.output_status(&acc, &spk).unwrap(), OutputStatus::Accepted { block_daa_score: 77 });
        let pool = Outpoint::new([0x33; 32], 0);
        assert_eq!(chain.output_status(&pool, &spk).unwrap(), OutputStatus::Mempool);
        let unknown = Outpoint::new([0x44; 32], 0);
        assert_eq!(chain.output_status(&unknown, &spk).unwrap(), OutputStatus::Unknown);
        // the same outpoint under another script is not accepted
        let other = p2pk_spk(&pubkey(2));
        assert_eq!(chain.output_status(&acc, &other).unwrap(), OutputStatus::Unknown);
        assert!(chain.in_mempool(&[0x33; 32]).unwrap());
        assert!(!chain.in_mempool(&[0x44; 32]).unwrap());
    }

    #[test]
    fn transport_and_odd_errors_are_chain_errors_not_false() {
        let (chain, _) = fake(|_, _| Err(RpcError::Transport("down".into())));
        assert!(chain.in_mempool(&[1; 32]).is_err());
        assert!(chain.virtual_daa_score().is_err());
        assert!(chain.utxos_of(&p2pk_spk(&pubkey(1))).is_err());
        let (chain, _) = fake(|_, _| Err(RpcError::Node("request deserialization error".into())));
        assert!(chain.in_mempool(&[1; 32]).is_err(), "only `not found` means absent");
    }

    #[test]
    fn virtual_daa_score_from_block_dag_info() {
        let (chain, f) = fake(|m, _| {
            assert_eq!(m, "getBlockDagInfo");
            Ok(json!({"network": "testnet-10", "virtualDaaScore": 583413145u64, "blockCount": 1}))
        });
        assert_eq!(chain.virtual_daa_score().unwrap(), 583413145);
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        let (chain, _) = fake(|_, _| Ok(json!({"blockCount": 1})));
        assert!(chain.virtual_daa_score().is_err());
    }

    #[test]
    fn submit_error_mapping() {
        let id = "ab".repeat(32);
        let cases = [
            (format!("Rejected transaction {id}: transaction {id} is already in the mempool"), "known"),
            (format!("Rejected transaction {id}: transaction {id} was already accepted by the consensus"), "known"),
            (format!("Rejected transaction {id}: output {id}:0 already spent by transaction {id} in the mempool"), "conflict"),
            (format!("Rejected transaction {id}: transaction {id} is an orphan where orphan is disallowed"), "conflict"),
            ("Rejected transaction x: at least one outpoint of the transaction is lacking a matching UTXO entry".into(), "conflict"),
            (format!("Rejected transaction {id}: transaction {id} is not standard: non-standard script form"), "rejected"),
            ("Rejected transaction x: transaction has 5 fees which is under the required amount of 100".into(), "rejected"),
        ];
        for (msg, want) in cases {
            let got = match map_submit_error(&msg) {
                SubmitError::AlreadyKnown => "known",
                SubmitError::Conflict(_) => "conflict",
                SubmitError::Rejected(_) => "rejected",
                SubmitError::Unavailable(_) => "unavailable",
            };
            assert_eq!(got, want, "{msg}");
        }
    }

    fn sample_tx() -> Transaction {
        use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
        use kaspa_consensus_core::tx::{TransactionInput, TransactionOutpoint, TransactionOutput};
        let spk = p2pk_spk(&pubkey(1));
        Transaction::new(
            0,
            vec![TransactionInput::new(
                TransactionOutpoint::new(kaspa_consensus_core::Hash::from_bytes([5; 32]), 1),
                vec![0x41; 66],
                0,
                1,
            )],
            vec![TransactionOutput::new(1_000, spk)],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        )
    }

    #[test]
    fn submit_uses_the_rpc_transaction_shape_and_maps_outcomes() {
        let tx = sample_tx();
        let txid = tx.id();
        let expect = hex(&txid.as_bytes());
        let (chain, f) = fake(move |m, p| {
            assert_eq!(m, "submitTransaction");
            assert_eq!(p["allowOrphan"], false);
            assert_eq!(p["transaction"]["inputs"][0]["previousOutpoint"]["index"], 1);
            assert_eq!(p["transaction"]["outputs"][0]["value"], 1_000);
            Ok(json!({ "transactionId": expect }))
        });
        assert_eq!(chain.submit(&tx).unwrap(), txid.as_bytes());
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        // a different id from the node is an unknown outcome, never success
        let (chain, _) = fake(|_, _| Ok(json!({ "transactionId": "00".repeat(32) })));
        assert!(matches!(chain.submit(&tx), Err(SubmitError::Unavailable(_))));
        let (chain, _) = fake(|_, _| Ok(json!({})));
        assert!(matches!(chain.submit(&tx), Err(SubmitError::Unavailable(_))));
        let (chain, _) = fake(|_, _| Err(RpcError::Transport("reset".into())));
        assert!(matches!(chain.submit(&tx), Err(SubmitError::Unavailable(_))));
        let (chain, _) = fake(|_, _| Err(RpcError::Node("transaction abc is already in the mempool".into())));
        assert_eq!(chain.submit(&tx), Err(SubmitError::AlreadyKnown));
        let (chain, _) = fake(|_, _| Err(RpcError::Node("output x already spent by transaction y in the mempool".into())));
        assert!(matches!(chain.submit(&tx), Err(SubmitError::Conflict(_))));
        let (chain, _) = fake(|_, _| Err(RpcError::Node("transaction z is not standard: nope".into())));
        assert!(matches!(chain.submit(&tx), Err(SubmitError::Rejected(_))));
    }

    #[test]
    fn reply_parsing() {
        assert_eq!(parse_reply(r#"{"id":2,"method":"x","params":{}}"#, 1).unwrap(), Reply::Other);
        assert_eq!(parse_reply(r#"{"method":"notification","params":{}}"#, 1).unwrap(), Reply::Other);
        assert!(
            matches!(parse_reply(r#"{"id":1,"error":{"code":0,"message":"boom","data":null}}"#, 1), Err(RpcError::Node(m)) if m == "boom")
        );
        assert!(matches!(parse_reply("nope", 1), Err(RpcError::Protocol(_))));
        assert!(matches!(parse_reply(r#"{"id":1}"#, 1), Err(RpcError::Protocol(_))));
    }

    /// A websocket mock node that serves `handler(method, params)` for every request on every connection.
    fn mock_node(handler: fn(&str, &Value) -> String, connections: usize) -> (String, thread::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let h = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..connections {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                while let Ok(msg) = ws.read() {
                    let Ok(text) = msg.to_text() else { continue };
                    let req: Value = serde_json::from_str(text).unwrap();
                    ws.send(Message::text(r#"{"method":"notification","params":{"x":1}}"#)).unwrap();
                    let body = handler(req["method"].as_str().unwrap(), &req["params"]);
                    let id = req["id"].as_u64().unwrap();
                    let reply = if let Some(err) = body.strip_prefix("ERR:") {
                        json!({"id": id, "method": req["method"], "error": {"code": 0, "message": err, "data": null}})
                    } else {
                        json!({"id": id, "method": req["method"], "params": serde_json::from_str::<Value>(&body).unwrap()})
                    };
                    ws.send(Message::text(reply.to_string())).unwrap();
                    seen.push(req);
                }
            }
            seen
        });
        (url, h)
    }

    #[test]
    fn websocket_client_against_a_mock_node() {
        let (url, h) = mock_node(
            |m, _| match m {
                "getBlockDagInfo" => r#"{"virtualDaaScore":123456}"#.to_string(),
                "getMempoolEntry" => "ERR:Transaction abc not found".to_string(),
                _ => "ERR:unknown".to_string(),
            },
            1,
        );
        let chain = NodeChain::connect(&url, Network::Testnet10, Duration::from_secs(5));
        // several calls reuse the one connection
        assert_eq!(chain.virtual_daa_score().unwrap(), 123456);
        assert!(!chain.in_mempool(&[7; 32]).unwrap());
        assert_eq!(chain.virtual_daa_score().unwrap(), 123456);
        drop(chain);
        let seen = h.join().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1]["params"]["transactionId"], "07".repeat(32));
    }

    #[test]
    fn websocket_client_reconnects_after_the_node_drops_the_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let h = thread::spawn(move || {
            // first connection: answer once, then close
            let (s, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(s).unwrap();
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::text(json!({"id": req["id"], "params": {"virtualDaaScore": 1}}).to_string())).unwrap();
            let _ = ws.close(None);
            let _ = ws.flush();
            drop(ws);
            // second connection answers
            let (s, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(s).unwrap();
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::text(json!({"id": req["id"], "params": {"virtualDaaScore": 2}}).to_string())).unwrap();
            let _ = ws.read();
        });
        let chain = NodeChain::connect(&url, Network::Testnet10, Duration::from_secs(5));
        assert_eq!(chain.virtual_daa_score().unwrap(), 1);
        assert_eq!(chain.virtual_daa_score().unwrap(), 2);
        drop(chain);
        h.join().unwrap();
    }

    #[test]
    fn concurrent_calls_spill_to_more_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = conns.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                c2.fetch_add(1, Ordering::SeqCst);
                thread::spawn(move || {
                    let Ok(mut ws) = tungstenite::accept(stream) else { return };
                    while let Ok(msg) = ws.read() {
                        let Ok(text) = msg.to_text() else { continue };
                        let req: Value = serde_json::from_str(text).unwrap();
                        thread::sleep(Duration::from_millis(150)); // force overlap
                        let _ = ws.send(Message::text(json!({"id": req["id"], "params": {"virtualDaaScore": 9}}).to_string()));
                    }
                });
            }
        });
        let chain = Arc::new(NodeChain::connect(&url, Network::Testnet10, Duration::from_secs(5)));
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let c = chain.clone();
                thread::spawn(move || c.virtual_daa_score().unwrap())
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 9);
        }
        assert_eq!(conns.load(Ordering::SeqCst), 3);
        // sequential calls afterwards reuse the first pooled connection
        assert_eq!(chain.virtual_daa_score().unwrap(), 9);
        assert_eq!(conns.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn unreachable_node_is_unavailable() {
        let chain = NodeChain::connect("ws://127.0.0.1:1", Network::Testnet10, Duration::from_millis(500));
        assert!(chain.virtual_daa_score().is_err());
        assert!(matches!(chain.submit(&sample_tx()), Err(SubmitError::Unavailable(_))));
        let chain = NodeChain::connect("wss://example.invalid", Network::Testnet10, Duration::from_millis(500));
        assert!(chain.virtual_daa_score().is_err());
    }

    #[test]
    fn a_silent_node_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let h = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(s).unwrap();
            let _ = ws.read(); // never answers
            thread::sleep(Duration::from_millis(1200));
        });
        let chain = NodeChain::connect(&url, Network::Testnet10, Duration::from_millis(300));
        let t = Instant::now();
        assert!(matches!(chain.submit(&sample_tx()), Err(SubmitError::Unavailable(_))));
        assert!(t.elapsed() < Duration::from_secs(2));
        h.join().unwrap();
    }
}
