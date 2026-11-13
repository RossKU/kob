//! A minimal blocking Kaspa wRPC client (JSON encoding).
//!
//! It offers `submitTransaction`, `getUtxosByAddresses` and a generic `call`.
//!
//! # Protocol
//!
//! The framing is workflow-rpc 0.18 JSON with the camelCase `RpcApiOps` of rusty-kaspa v2.1.0.
//!
//! ```text
//! -> {"id":1,"method":"submitTransaction","params":{"transaction":{<RpcTransaction>},
//!                                          "allowOrphan":false}}
//! <- {"id":1,"method":"submitTransaction","params":{"transactionId":"<hex>"}}
//! <- {"id":1,"error":{"code":0,"message":"...","data":null}}
//! -> {"id":2,"method":"getUtxosByAddresses","params":{"addresses":["kaspatest:..."]}}
//! <- {"id":2,"method":"getUtxosByAddresses","params":{"entries":[{"address":"kaspatest:...",
//!       "outpoint":{"transactionId":"<hex>","index":0},
//!       "utxoEntry":{"amount":1000,"scriptPublicKey":"0000<script hex>","blockDaaScore":1,
//!                    "isCoinbase":false,
//!                    "covenantId":"<hex>"|null}}]}}
//! ```
//!
//! # Nodes
//!
//! The node RPC is used rather than the public REST API, because the REST API drops the per-input
//! `computeBudget`, which a v1 transaction needs. A node is one of these:
//! - A JSON wRPC endpoint, such as `ws://host:18110`.
//! - A Borsh endpoint, such as `ws://host:17110` or `wss://host/.../borsh`. [`crate::nodelink`]
//!   carries the same calls over Borsh.
//! - The default, the two public nodes that the Kaspa resolvers name (`--node resolver`,
//!   [`crate::nodelink::pair`]).
//!
//! `getUtxosByAddresses` needs a node that was started with `--utxoindex`.

use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Addresses per `getUtxosByAddresses` request.
pub const ADDRESS_CHUNK: usize = 50;

/// The node endpoint when `--node` is not given.
///
/// It is the name that the executor uses for the same endpoint. Command-line arguments are visible
/// to other local users (`ps`), so a URL with a password or an API key belongs here, not in
/// `--node`.
pub const NODE_ENV: &str = "KOB_NODE_WRPC";

/// `--node` if given, else [`NODE_ENV`]. `env` is its value, and a blank one counts as unset.
pub fn node_or_env(flag: Option<String>, env: Option<String>) -> Option<String> {
    flag.filter(|v| !v.trim().is_empty()).or_else(|| env.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

/// The node of a command on `network`.
///
/// It is `--node`, else `$KOB_NODE_WRPC`, else the public nodes that the Kaspa resolvers name,
/// which `--node resolver` names too.
pub fn node_for(flag: Option<String>, network: &str) -> String {
    node_for_env(flag, std::env::var(NODE_ENV).ok(), network)
}

/// [`node_for`] with the environment variable given.
pub fn node_for_env(flag: Option<String>, env: Option<String>, network: &str) -> String {
    match node_or_env(flag, env) {
        Some(n) if n == crate::nodelink::resolver::RESOLVER => crate::nodelink::resolver::pseudo(network),
        Some(n) => n,
        None => crate::nodelink::resolver::pseudo(network),
    }
}

/// A node URL as messages and reports show it.
///
/// That is the scheme, host and port only ([`kob_protocol::redact::redact_url`]). The public nodes
/// show as `resolver (public nodes)`.
pub fn shown(url: &str) -> String {
    if crate::nodelink::resolver::network_of(url).is_some() {
        return "resolver (public nodes)".into();
    }
    kob_protocol::redact::redact_url(url)
}

/// Errors of the wRPC client.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WrpcError {
    /// Connection or websocket failure.
    #[error("{0}")]
    Socket(String),
    /// No public node could be named (the Kaspa resolvers answered none).
    #[error("{0}")]
    Resolve(String),
    /// The node answered with an error object.
    #[error("node rejected the request: {0}")]
    Node(String),
    /// The reply was not the expected JSON.
    #[error("unexpected reply: {0}")]
    Protocol(String),
}

/// The target of a plain `ws://` URL ([`crate::nodelink::client::target`]), else `None`.
fn plain_target(url: &str) -> Option<Result<crate::nodelink::client::Target, String>> {
    url.starts_with("ws://").then(|| crate::nodelink::client::target(url))
}

/// A node error as a sentence: `node <url>: <error>`.
///
/// A failed connection already names the node, so its message is used as it is.
pub fn node_failure(node: &str, e: &WrpcError) -> String {
    match e {
        WrpcError::Socket(m) if m.starts_with("cannot connect to the node") => m.clone(),
        _ => format!("node {}: {e}", shown(node)),
    }
}

/// A JSON wRPC request.
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "id": id, "method": method, "params": params })
}

/// The JSON request for `submitTransaction`.
pub fn submit_request(id: u64, transaction: &Value, allow_orphan: bool) -> Value {
    request(id, "submitTransaction", json!({ "transaction": transaction, "allowOrphan": allow_orphan }))
}

/// What one incoming text frame means for a pending request.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// A frame for another request or a notification: keep reading.
    Other,
    /// The `params` of the awaited response.
    Ok(Value),
}

/// Interpret one text frame for request `id`.
pub fn parse_reply(text: &str, id: u64) -> Result<Reply, WrpcError> {
    let v: Value = serde_json::from_str(text).map_err(|e| WrpcError::Protocol(format!("not JSON ({e}): {text}")))?;
    if v.get("id").and_then(Value::as_u64) != Some(id) {
        return Ok(Reply::Other);
    }
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let msg = err.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| err.to_string());
        return Err(WrpcError::Node(msg));
    }
    match v.get("params") {
        Some(p) => Ok(Reply::Ok(p.clone())),
        None => Err(WrpcError::Protocol(format!("response without params or error: {text}"))),
    }
}

/// Extract the transaction id from the `submitTransaction` response params.
pub fn transaction_id(params: &Value) -> Result<String, WrpcError> {
    params
        .get("transactionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| WrpcError::Protocol(format!("no transactionId in {params}")))
}

/// One open wRPC connection; requests are sent one at a time.
pub struct Conn {
    link: Link,
    next_id: u64,
}

enum Link {
    /// JSON wRPC over a plain socket.
    Json(WebSocket<MaybeTlsStream<TcpStream>>),
    /// Borsh wRPC (an own node's Borsh port, `wss://.../borsh`).
    Borsh(crate::nodelink::client::BorshConn),
}

impl Conn {
    /// Connect to `url` (`ws://host:port`, JSON wRPC port: 18110 mainnet, 18210 testnet-10).
    pub fn connect(url: &str, timeout: Duration) -> Result<Conn, WrpcError> {
        if crate::nodelink::bridge::is_borsh_url(url) {
            return Ok(Conn { link: Link::Borsh(crate::nodelink::client::BorshConn::connect(url, timeout)?), next_id: 1 });
        }
        // A plain `ws://` socket is opened here, so that a refused or timed-out connection says
        // why. tungstenite reports only that it could not connect. The handshake runs over the
        // socket with the same timeouts.
        if let Some(plain) = plain_target(url) {
            let t = plain.map_err(WrpcError::Socket)?;
            let stream = crate::tls::connect(false, &t.host, t.port, timeout)
                .map_err(|e| WrpcError::Socket(format!("cannot connect to the node {} ({e})", shown(url))))?;
            let crate::tls::Stream::Plain(tcp) = stream else { unreachable!("a plain connection") };
            let mut req = t.url.as_str().into_client_request().map_err(|e| WrpcError::Socket(e.to_string()))?;
            if let Some(a) = &t.auth {
                req.headers_mut().insert(
                    "Authorization",
                    tungstenite::http::HeaderValue::from_str(&format!("Basic {a}")).map_err(|e| WrpcError::Socket(e.to_string()))?,
                );
            }
            return match tungstenite::client(req, MaybeTlsStream::Plain(tcp)) {
                Ok((ws, _)) => Ok(Conn { link: Link::Json(ws), next_id: 1 }),
                Err(tungstenite::HandshakeError::Failure(e)) => {
                    Err(WrpcError::Socket(format!("node {}: {}", shown(url), kob_protocol::redact::scrub_url(&e.to_string(), url))))
                }
                Err(tungstenite::HandshakeError::Interrupted(_)) => {
                    Err(WrpcError::Socket(format!("node {}: the handshake timed out", shown(url))))
                }
            };
        }
        // tungstenite echoes the URL it could not reach, so the message uses the redacted URL.
        let (mut ws, _) = tungstenite::connect(url).map_err(|e| {
            WrpcError::Socket(match e {
                tungstenite::Error::Url(tungstenite::error::UrlError::UnableToConnect(_)) => {
                    format!("URL error: Unable to connect to {}", shown(url))
                }
                other => kob_protocol::redact::scrub_url(&other.to_string(), url),
            })
        })?;
        if let MaybeTlsStream::Plain(s) = ws.get_mut() {
            set_timeouts(s, timeout)?;
        }
        Ok(Conn { link: Link::Json(ws), next_id: 1 })
    }

    /// Send `method` with `params` and return the `params` of the response.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, WrpcError> {
        let ws = match &mut self.link {
            Link::Json(ws) => ws,
            Link::Borsh(b) => return b.call(method, params),
        };
        let id = self.next_id;
        self.next_id += 1;
        ws.send(Message::text(request(id, method, params).to_string())).map_err(|e| WrpcError::Socket(e.to_string()))?;
        loop {
            match ws.read().map_err(|e| WrpcError::Socket(e.to_string()))? {
                Message::Text(t) => match parse_reply(t.as_str(), id)? {
                    Reply::Other => continue,
                    Reply::Ok(p) => return Ok(p),
                },
                Message::Close(_) => return Err(WrpcError::Socket("connection closed before the response".into())),
                _ => continue,
            }
        }
    }

    /// `getUtxosByAddresses`, in requests of [`ADDRESS_CHUNK`] addresses.
    pub fn utxos_by_addresses(&mut self, addresses: &[String]) -> Result<Vec<NodeUtxo>, WrpcError> {
        let mut out = vec![];
        for chunk in addresses.chunks(ADDRESS_CHUNK) {
            let p = self.call("getUtxosByAddresses", json!({ "addresses": chunk }))?;
            out.extend(parse_utxo_entries(&p)?);
        }
        Ok(out)
    }

    /// Close the connection (best effort).
    pub fn close(self) {
        match self.link {
            Link::Json(mut ws) => {
                let _ = ws.close(None);
            }
            Link::Borsh(b) => b.close(),
        }
    }
}

/// One-shot call: connect, send one request, return the response `params`.
pub fn call(url: &str, method: &str, params: Value, timeout: Duration) -> Result<Value, WrpcError> {
    if let Some(net) = crate::nodelink::resolver::network_of(url) {
        return crate::nodelink::pair::with_real(net, |io| crate::nodelink::pair::call_any(io, method, params, timeout));
    }
    let mut c = Conn::connect(url, timeout)?;
    let r = c.call(method, params);
    c.close();
    r
}

/// A network name in the canonical form of the CLI: `mainnet`, `testnet-10`, `simnet` or `devnet`.
///
/// What `getBlockDagInfo` reports (`mainnet`, `testnet-10`) and spellings like `kaspa-testnet-10`
/// or `Testnet_10` compare equal. It matches `normalizeNetwork` of the web app.
pub fn normalize_network(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    let s = s.strip_prefix("kaspa").map(|r| r.trim_start_matches([':', '_', '-', ' '])).unwrap_or(&s).replace(['_', ' '], "-");
    match s.as_str() {
        "main" => "mainnet".into(),
        _ => match s.strip_prefix("testnet") {
            Some(n) if !n.is_empty() && n.trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) => {
                format!("testnet-{}", n.trim_start_matches('-'))
            }
            _ => s,
        },
    }
}

/// The network the node at `url` runs, normalized by [`normalize_network`].
///
/// It is the `network` of `getBlockDagInfo` (a rusty-kaspa `NetworkId`, such as `mainnet`).
pub fn node_network(url: &str, timeout: Duration) -> Result<String, WrpcError> {
    // For the public nodes, both of the pair must report the same network.
    if let Some(net) = crate::nodelink::resolver::network_of(url) {
        return crate::nodelink::pair::with_real(net, |io| crate::nodelink::pair::network(io, timeout));
    }
    let p = call(url, "getBlockDagInfo", json!({}), timeout)?;
    p.get("network")
        .and_then(Value::as_str)
        .map(normalize_network)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| WrpcError::Protocol("getBlockDagInfo names no network".into()))
}

/// Refuse a node that runs another network than `network`.
///
/// A signature commits to no network. A transaction signed for testnet-10 and sent to a mainnet
/// node would spend mainnet coins of the same key, so every signing command checks the node first.
pub fn check_node_network(url: &str, network: &str, timeout: Duration) -> Result<(), String> {
    let got = node_network(url, timeout).map_err(|e| match &e {
        WrpcError::Socket(m) if m.starts_with("cannot connect to the node") => m.clone(),
        _ => format!("node {}: cannot confirm its network: {e}", shown(url)),
    })?;
    let want = normalize_network(network);
    if got != want {
        return Err(format!(
            "the node {} runs {got}, not {want}: nothing signed (check --node / ${NODE_ENV} and --network)",
            shown(url)
        ));
    }
    Ok(())
}

/// Submit a transaction (RPC JSON shape) to the node at `url` and return its transaction id.
pub fn submit_transaction(url: &str, transaction: &Value, allow_orphan: bool, timeout: Duration) -> Result<String, WrpcError> {
    if let Some(net) = crate::nodelink::resolver::network_of(url) {
        return crate::nodelink::pair::with_real(net, |io| crate::nodelink::pair::submit(io, transaction, allow_orphan, timeout));
    }
    let p = call(url, "submitTransaction", json!({ "transaction": transaction, "allowOrphan": allow_orphan }), timeout)?;
    transaction_id(&p)
}

/// The unspent outputs at `addresses`, in requests of [`ADDRESS_CHUNK`] addresses on one link.
pub fn utxos_by_addresses(url: &str, addresses: &[String], timeout: Duration) -> Result<Vec<NodeUtxo>, WrpcError> {
    if addresses.is_empty() {
        return Ok(vec![]);
    }
    // For the public nodes, both of the pair must list the same outputs.
    if let Some(net) = crate::nodelink::resolver::network_of(url) {
        return crate::nodelink::pair::with_real(net, |io| crate::nodelink::pair::utxos(io, addresses, timeout));
    }
    let mut c = Conn::connect(url, timeout)?;
    let r = c.utxos_by_addresses(addresses);
    c.close();
    r
}

fn set_timeouts(s: &TcpStream, timeout: Duration) -> Result<(), WrpcError> {
    s.set_read_timeout(Some(timeout)).and_then(|_| s.set_write_timeout(Some(timeout))).map_err(|e| WrpcError::Socket(e.to_string()))
}

/// An unspent output as `getUtxosByAddresses` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeUtxo {
    /// The address the entry was reported for.
    pub address: Option<String>,
    /// Outpoint transaction id.
    pub txid: [u8; 32],
    /// Outpoint index.
    pub index: u32,
    /// Sompi.
    pub amount: u64,
    /// Script public key: 2-byte big-endian version followed by the script.
    pub spk: Vec<u8>,
    /// DAA score of the block that created it.
    pub daa: u64,
    /// Coinbase output.
    pub coinbase: bool,
    /// Covenant id the output carries (absent or null: none).
    pub covenant_id: Option<[u8; 32]>,
}

fn hex32(v: &Value, what: &str) -> Result<[u8; 32], WrpcError> {
    let s = v.as_str().ok_or_else(|| WrpcError::Protocol(format!("{what} is not a string: {v}")))?;
    kob_protocol::json::hex32(s).map_err(|e| WrpcError::Protocol(format!("{what}: {e}")))
}

fn u64_of(v: Option<&Value>, what: &str) -> Result<u64, WrpcError> {
    match v {
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| WrpcError::Protocol(format!("{what} out of range: {n}"))),
        Some(Value::String(s)) => s.parse().map_err(|_| WrpcError::Protocol(format!("{what} is not an integer: {s}"))),
        None | Some(Value::Null) => Ok(0),
        Some(other) => Err(WrpcError::Protocol(format!("{what} is not an integer: {other}"))),
    }
}

/// Script public key of an entry: the hex `version (u16 BE) ‖ script`, or `{version, script}`.
fn spk_of(v: &Value) -> Result<Vec<u8>, WrpcError> {
    let bad = || WrpcError::Protocol(format!("unexpected scriptPublicKey {v}"));
    match v {
        Value::String(s) => {
            let b = kob_protocol::json::from_hex(s).map_err(|_| bad())?;
            if b.len() < 2 {
                return Err(bad());
            }
            Ok(b)
        }
        Value::Object(o) => {
            let version = o.get("version").and_then(Value::as_u64).filter(|v| *v <= u16::MAX as u64).ok_or_else(bad)? as u16;
            let script = o.get("script").or_else(|| o.get("scriptPublicKey")).and_then(Value::as_str).ok_or_else(bad)?;
            let mut b = version.to_be_bytes().to_vec();
            b.extend(kob_protocol::json::from_hex(script).map_err(|_| bad())?);
            Ok(b)
        }
        _ => Err(bad()),
    }
}

/// Parse the `params` of a `getUtxosByAddresses` response. A missing `entries` is an empty list.
pub fn parse_utxo_entries(params: &Value) -> Result<Vec<NodeUtxo>, WrpcError> {
    let entries = match params.get("entries") {
        None | Some(Value::Null) => return Ok(vec![]),
        Some(Value::Array(a)) => a,
        Some(other) => return Err(WrpcError::Protocol(format!("entries is not an array: {other}"))),
    };
    entries
        .iter()
        .map(|e| {
            let op = e.get("outpoint").ok_or_else(|| WrpcError::Protocol(format!("entry without outpoint: {e}")))?;
            let u = e.get("utxoEntry").ok_or_else(|| WrpcError::Protocol(format!("entry without utxoEntry: {e}")))?;
            let index = op.get("index").and_then(Value::as_u64).filter(|i| *i <= u32::MAX as u64);
            Ok(NodeUtxo {
                address: e.get("address").and_then(Value::as_str).map(str::to_owned),
                txid: hex32(op.get("transactionId").unwrap_or(&Value::Null), "outpoint.transactionId")?,
                index: index.ok_or_else(|| WrpcError::Protocol(format!("bad outpoint index: {op}")))? as u32,
                amount: u64_of(u.get("amount"), "amount")?,
                spk: spk_of(u.get("scriptPublicKey").unwrap_or(&Value::Null))?,
                daa: u64_of(u.get("blockDaaScore"), "blockDaaScore")?,
                coinbase: u.get("isCoinbase").and_then(Value::as_bool).unwrap_or(false),
                covenant_id: match u.get("covenantId") {
                    None | Some(Value::Null) => None,
                    Some(c) => Some(hex32(c, "covenantId")?),
                },
            })
        })
        .collect()
}

/// A `getUtxosByAddresses` entry in the node's JSON shape (mock nodes, fixtures).
#[cfg(test)]
pub fn utxo_entry_json(u: &NodeUtxo) -> Value {
    use kob_protocol::json::to_hex;
    json!({
        "address": u.address,
        "outpoint": { "transactionId": to_hex(&u.txid), "index": u.index },
        "utxoEntry": {
            "amount": u.amount,
            "scriptPublicKey": to_hex(&u.spk),
            "blockDaaScore": u.daa,
            "isCoinbase": u.coinbase,
            "covenantId": u.covenant_id.map(|c| to_hex(&c)),
        },
    })
}

#[cfg(test)]
pub mod mock {
    //! A mock wRPC node for tests.
    //!
    //! It answers `getUtxosByAddresses` from a fixed UTXO set and records `submitTransaction`.

    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    /// Shared state of a mock node.
    #[derive(Default)]
    pub struct MockState {
        /// The UTXO set (`address` must be set: entries are answered by address).
        pub utxos: Vec<NodeUtxo>,
        /// Every submitted transaction (RPC JSON).
        pub submitted: Vec<Value>,
        /// Every request seen (method, params).
        pub requests: Vec<(String, Value)>,
        /// The virtual DAA score `getBlockDagInfo` reports.
        pub daa: u64,
        /// The network `getBlockDagInfo` reports (`testnet-10` by default).
        pub network: String,
        /// What `getFeeEstimate` answers (none: an error, as a node without the method).
        pub fee_estimate: Option<Value>,
    }

    /// Start a mock node; it serves connections until the test process ends.
    pub fn start(utxos: Vec<NodeUtxo>) -> (String, Arc<Mutex<MockState>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState { utxos, daa: 1_000_000, network: "testnet-10".into(), ..Default::default() }));
        let st = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let st = st.clone();
                thread::spawn(move || {
                    let Ok(mut ws) = tungstenite::accept(stream) else { return };
                    while let Ok(msg) = ws.read() {
                        let Message::Text(t) = msg else {
                            if matches!(msg, Message::Close(_)) {
                                break;
                            }
                            continue;
                        };
                        let req: Value = serde_json::from_str(t.as_str()).unwrap();
                        let id = req["id"].as_u64().unwrap();
                        let method = req["method"].as_str().unwrap_or("").to_string();
                        let params = req["params"].clone();
                        let reply = {
                            let mut s = st.lock().unwrap();
                            s.requests.push((method.clone(), params.clone()));
                            match method.as_str() {
                                "getUtxosByAddresses" => {
                                    let addrs: Vec<&str> =
                                        params["addresses"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
                                    assert!(addrs.len() <= ADDRESS_CHUNK, "the client chunks its address lists");
                                    let entries: Vec<Value> = s
                                        .utxos
                                        .iter()
                                        .filter(|u| u.address.as_deref().is_some_and(|a| addrs.contains(&a)))
                                        .map(utxo_entry_json)
                                        .collect();
                                    json!({"id": id, "method": method, "params": {"entries": entries}})
                                }
                                "getBlockDagInfo" => {
                                    json!({"id": id, "method": method, "params": {"virtualDaaScore": s.daa, "network": s.network}})
                                }
                                "getFeeEstimate" if s.fee_estimate.is_some() => {
                                    json!({"id": id, "method": method, "params": s.fee_estimate.clone()})
                                }
                                "submitTransaction" => {
                                    s.submitted.push(params["transaction"].clone());
                                    json!({"id": id, "method": method, "params": {"transactionId": "ab".repeat(32)}})
                                }
                                _ => json!({"id": id, "error": {"code": 0, "message": "unknown method", "data": null}}),
                            }
                        };
                        // A notification first: the client must skip frames of other ids.
                        let _ = ws.send(Message::text(r#"{"method":"notification","params":{"x":1}}"#));
                        if ws.send(Message::text(reply.to_string())).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (url, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn request_shape() {
        let r = submit_request(7, &json!({"version": 1}), false);
        assert_eq!(r["id"], 7);
        assert_eq!(r["method"], "submitTransaction");
        assert_eq!(r["params"]["transaction"]["version"], 1);
        assert_eq!(r["params"]["allowOrphan"], false);
    }

    #[test]
    fn reply_parsing() {
        assert_eq!(parse_reply(r#"{"id":2,"method":"x","params":{}}"#, 1).unwrap(), Reply::Other);
        assert_eq!(parse_reply(r#"{"method":"notification","params":{}}"#, 1).unwrap(), Reply::Other);
        let ok = parse_reply(r#"{"id":1,"method":"submitTransaction","params":{"transactionId":"ab"}}"#, 1).unwrap();
        let Reply::Ok(p) = ok else { panic!() };
        assert_eq!(transaction_id(&p).unwrap(), "ab");
        let e = parse_reply(r#"{"id":1,"error":{"code":0,"message":"rejected: mass","data":null}}"#, 1).unwrap_err();
        assert!(matches!(e, WrpcError::Node(m) if m == "rejected: mass"));
        assert!(matches!(parse_reply("nope", 1), Err(WrpcError::Protocol(_))));
        assert!(matches!(parse_reply(r#"{"id":1}"#, 1), Err(WrpcError::Protocol(_))));
    }

    /// One-shot mock node: accepts a websocket, checks the request, sends a notification, replies.
    fn mock_node(reply: fn(u64) -> String) -> (String, thread::JoinHandle<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let h = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let msg = ws.read().unwrap();
            let req: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
            ws.send(Message::text(r#"{"method":"notification","params":{"x":1}}"#)).unwrap();
            ws.send(Message::text(reply(req["id"].as_u64().unwrap()))).unwrap();
            let _ = ws.read();
            req
        });
        (url, h)
    }

    #[test]
    fn submit_against_a_mock_node() {
        let (url, h) = mock_node(|id| {
            format!(r#"{{"id":{id},"method":"submitTransaction","params":{{"transactionId":"{}"}}}}"#, "cd".repeat(32))
        });
        let tx = json!({"version": 1, "outputs": []});
        let txid = submit_transaction(&url, &tx, false, Duration::from_secs(5)).unwrap();
        assert_eq!(txid, "cd".repeat(32));
        let req = h.join().unwrap();
        assert_eq!(req["method"], "submitTransaction");
        assert_eq!(req["params"]["transaction"], tx);
    }

    #[test]
    fn node_errors_surface() {
        let (url, h) =
            mock_node(|id| format!(r#"{{"id":{id},"error":{{"code":0,"message":"transaction is not standard","data":null}}}}"#));
        let e = submit_transaction(&url, &json!({}), false, Duration::from_secs(5)).unwrap_err();
        assert!(matches!(e, WrpcError::Node(m) if m.contains("not standard")));
        let _ = h.join();
    }

    #[test]
    fn unreachable_node_is_a_socket_error() {
        let e = submit_transaction("ws://127.0.0.1:1", &json!({}), false, Duration::from_millis(500)).unwrap_err();
        // An English sentence that names the node once, with the reason. Never the system language.
        let WrpcError::Socket(m) = e else { panic!("{e:?}") };
        assert!(m.starts_with("cannot connect to the node ws://127.0.0.1:1 ("), "{m}");
        assert!(m.ends_with("(connection refused)") || m.ends_with("(timed out)"), "{m}");
        assert_eq!(m.matches("127.0.0.1:1").count(), 1, "{m}");
    }

    /// A node URL with credentials or an API key never reaches an error message.
    #[test]
    fn connection_errors_name_the_node_redacted() {
        for url in ["ws://alice:s3cret@127.0.0.1:1/apikey/K?token=T", "ws://alice:s3cret@127.0.0.1:1"] {
            let e = submit_transaction(url, &json!({}), false, Duration::from_millis(500)).unwrap_err().to_string();
            assert!(e.contains("127.0.0.1:1"), "{e}");
            for secret in ["alice", "s3cret", "apikey", "token"] {
                assert!(!e.contains(secret), "{e}");
            }
        }
    }

    #[test]
    fn network_names_normalize_like_the_web_app() {
        for (raw, want) in [
            ("mainnet", "mainnet"),
            ("kaspa-mainnet", "mainnet"),
            ("Main", "mainnet"),
            ("testnet-10", "testnet-10"),
            ("kaspa-testnet-10", "testnet-10"),
            ("Testnet_10", "testnet-10"),
            ("testnet10", "testnet-10"),
            ("testnet-11", "testnet-11"),
            ("simnet", "simnet"),
        ] {
            assert_eq!(normalize_network(raw), want, "{raw}");
        }
        assert_ne!(normalize_network("testnet-10"), normalize_network("mainnet"));
    }

    /// The node's own network decides: another one, or none named, is refused.
    #[test]
    fn a_node_of_another_network_is_refused() {
        let (url, st) = mock::start(vec![]);
        assert!(check_node_network(&url, "testnet-10", Duration::from_secs(5)).is_ok());
        st.lock().unwrap().network = "mainnet".into();
        let e = check_node_network(&url, "testnet-10", Duration::from_secs(5)).unwrap_err();
        assert!(e.contains("runs mainnet, not testnet-10"), "{e}");
        assert!(check_node_network(&url, "mainnet", Duration::from_secs(5)).is_ok());
        st.lock().unwrap().network = String::new();
        assert!(check_node_network(&url, "mainnet", Duration::from_secs(5)).unwrap_err().contains("cannot confirm"));
    }

    #[test]
    fn the_node_comes_from_the_flag_else_the_environment() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(node_or_env(s("ws://a:1"), s("ws://b:2")), s("ws://a:1"));
        assert_eq!(node_or_env(None, s(" ws://b:2 ")), s("ws://b:2"));
        assert_eq!(node_or_env(s(""), s("ws://b:2")), s("ws://b:2"));
        assert_eq!(node_or_env(None, s("  ")), None);
        assert_eq!(node_or_env(None, None), None);
    }

    #[test]
    fn utxo_entries_parse_tolerantly() {
        let txid = "11".repeat(32);
        let cov = "22".repeat(32);
        let p = json!({"entries": [
            {"address": "kaspatest:x", "outpoint": {"transactionId": txid, "index": 3},
             "utxoEntry": {"amount": 1000, "scriptPublicKey": "0000aa", "blockDaaScore": 7, "isCoinbase": false, "covenantId": cov}},
            {"address": "kaspatest:y", "outpoint": {"transactionId": txid, "index": 4},
             "utxoEntry": {"amount": "2000", "scriptPublicKey": {"version": 0, "script": "bb"}, "blockDaaScore": "8", "covenantId": null}},
            {"outpoint": {"transactionId": txid, "index": 5}, "utxoEntry": {"amount": 1, "scriptPublicKey": "0000cc"}},
        ]});
        let u = parse_utxo_entries(&p).unwrap();
        assert_eq!(u.len(), 3);
        assert_eq!((u[0].index, u[0].amount, u[0].daa, u[0].spk.clone()), (3, 1000, 7, vec![0, 0, 0xaa]));
        assert_eq!(u[0].covenant_id, Some([0x22; 32]));
        assert_eq!((u[1].amount, u[1].daa, u[1].spk.clone(), u[1].covenant_id), (2000, 8, vec![0, 0, 0xbb], None));
        assert_eq!((u[2].address.clone(), u[2].covenant_id), (None, None));
        // Round trip through the node shape.
        assert_eq!(parse_utxo_entries(&json!({"entries": [utxo_entry_json(&u[0])]})).unwrap()[0], u[0]);
        assert!(parse_utxo_entries(&json!({})).unwrap().is_empty());
        assert!(parse_utxo_entries(&json!({"entries": null})).unwrap().is_empty());
        // Malformed entries are errors and are never silently dropped.
        for bad in [
            json!({"entries": 5}),
            json!({"entries": [{"utxoEntry": {}}]}),
            json!({"entries": [{"outpoint": {"transactionId": "zz", "index": 0}, "utxoEntry": {"scriptPublicKey": "0000"}}]}),
            json!({"entries": [{"outpoint": {"transactionId": txid, "index": 0}, "utxoEntry": {"scriptPublicKey": "00"}}]}),
            json!({"entries": [{"outpoint": {"transactionId": txid, "index": 0}, "utxoEntry": {"scriptPublicKey": "0000", "covenantId": "12"}}]}),
        ] {
            assert!(parse_utxo_entries(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mock_node_answers_by_address_in_chunks() {
        let mk = |i: u32| NodeUtxo {
            address: Some(format!("kaspatest:a{i}")),
            txid: [1; 32],
            index: i,
            amount: 10,
            spk: vec![0, 0, 1],
            daa: 1,
            coinbase: false,
            covenant_id: None,
        };
        let (url, st) = mock::start((0..120).map(mk).collect());
        let addrs: Vec<String> = (0..120).map(|i| format!("kaspatest:a{i}")).collect();
        let got = utxos_by_addresses(&url, &addrs, Duration::from_secs(5)).unwrap();
        assert_eq!(got.len(), 120);
        assert_eq!(st.lock().unwrap().requests.len(), 3, "120 addresses in requests of 50");
        assert!(call(&url, "bogus", json!({}), Duration::from_secs(5)).is_err());
    }
}
