//! Minimal blocking Kaspa wRPC (JSON encoding) client: `submitTransaction`, `getUtxosByAddresses` and a generic `call`.
//!
//! Protocol (workflow-rpc 0.18 JSON framing, rusty-kaspa v2.1.0 `RpcApiOps`, camelCase):
//!
//! ```text
//! -> {"id":1,"method":"submitTransaction","params":{"transaction":{<RpcTransaction>},"allowOrphan":false}}
//! <- {"id":1,"method":"submitTransaction","params":{"transactionId":"<hex>"}}
//! <- {"id":1,"error":{"code":0,"message":"...","data":null}}
//! -> {"id":2,"method":"getUtxosByAddresses","params":{"addresses":["kaspatest:..."]}}
//! <- {"id":2,"method":"getUtxosByAddresses","params":{"entries":[{"address":"kaspatest:...",
//!       "outpoint":{"transactionId":"<hex>","index":0},
//!       "utxoEntry":{"amount":1000,"scriptPublicKey":"0000<script hex>","blockDaaScore":1,"isCoinbase":false,
//!                    "covenantId":"<hex>"|null}}]}}
//! ```
//!
//! The node RPC is used rather than the public REST API because the REST API drops the per-input
//! `computeBudget`, which a v1 transaction needs. Only plain `ws://` is supported (no TLS
//! features are compiled in); put a TLS-terminating proxy in front of a remote node if needed.
//! `getUtxosByAddresses` needs a node started with `--utxoindex`.

use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Addresses per `getUtxosByAddresses` request.
pub const ADDRESS_CHUNK: usize = 50;

/// Errors of the wRPC client.
#[derive(Debug, thiserror::Error)]
pub enum WrpcError {
    /// Connection or websocket failure.
    #[error("websocket: {0}")]
    Socket(String),
    /// The node answered with an error object.
    #[error("node rejected the request: {0}")]
    Node(String),
    /// The reply was not the expected JSON.
    #[error("unexpected reply: {0}")]
    Protocol(String),
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
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Conn {
    /// Connect to `url` (`ws://host:port`, JSON wRPC port: 18110 mainnet, 18210 testnet-10).
    pub fn connect(url: &str, timeout: Duration) -> Result<Conn, WrpcError> {
        let (mut ws, _) = tungstenite::connect(url).map_err(|e| WrpcError::Socket(e.to_string()))?;
        if let MaybeTlsStream::Plain(s) = ws.get_mut() {
            set_timeouts(s, timeout)?;
        }
        Ok(Conn { ws, next_id: 1 })
    }

    /// Send `method` with `params` and return the `params` of the response.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, WrpcError> {
        let id = self.next_id;
        self.next_id += 1;
        self.ws.send(Message::text(request(id, method, params).to_string())).map_err(|e| WrpcError::Socket(e.to_string()))?;
        loop {
            match self.ws.read().map_err(|e| WrpcError::Socket(e.to_string()))? {
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
    pub fn close(mut self) {
        let _ = self.ws.close(None);
    }
}

/// One-shot call: connect, send one request, return the response `params`.
pub fn call(url: &str, method: &str, params: Value, timeout: Duration) -> Result<Value, WrpcError> {
    let mut c = Conn::connect(url, timeout)?;
    let r = c.call(method, params);
    c.close();
    r
}

/// Submit a transaction (RPC JSON shape) to the node at `url` and return the accepted transaction id.
pub fn submit_transaction(url: &str, transaction: &Value, allow_orphan: bool, timeout: Duration) -> Result<String, WrpcError> {
    let p = call(url, "submitTransaction", json!({ "transaction": transaction, "allowOrphan": allow_orphan }), timeout)?;
    transaction_id(&p)
}

/// The unspent outputs at `addresses` (one connection, requests of [`ADDRESS_CHUNK`] addresses).
pub fn utxos_by_addresses(url: &str, addresses: &[String], timeout: Duration) -> Result<Vec<NodeUtxo>, WrpcError> {
    if addresses.is_empty() {
        return Ok(vec![]);
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

/// Script public key of an entry: the hex string `version (u16 BE) ‖ script`, or `{version, script}`.
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

/// Parse the `params` of a `getUtxosByAddresses` response (no `entries`: none).
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
    //! A mock wRPC node for tests: answers `getUtxosByAddresses` from a fixed UTXO set and records `submitTransaction`.

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
    }

    /// Start a mock node; it serves connections until the test process ends.
    pub fn start(utxos: Vec<NodeUtxo>) -> (String, Arc<Mutex<MockState>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState { utxos, ..Default::default() }));
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
                                "submitTransaction" => {
                                    s.submitted.push(params["transaction"].clone());
                                    json!({"id": id, "method": method, "params": {"transactionId": "ab".repeat(32)}})
                                }
                                _ => json!({"id": id, "error": {"code": 0, "message": "unknown method", "data": null}}),
                            }
                        };
                        // a notification first: the client must skip frames of other ids
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

    /// One-shot mock node: accepts a websocket, checks the request, sends a notification first, then `reply(id)`.
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
        assert!(matches!(e, WrpcError::Socket(_)));
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
        // round trip through the node shape
        assert_eq!(parse_utxo_entries(&json!({"entries": [utxo_entry_json(&u[0])]})).unwrap()[0], u[0]);
        assert!(parse_utxo_entries(&json!({})).unwrap().is_empty());
        assert!(parse_utxo_entries(&json!({"entries": null})).unwrap().is_empty());
        // malformed entries are errors, never silently dropped
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
