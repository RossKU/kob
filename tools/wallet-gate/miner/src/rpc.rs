//! Minimal wRPC JSON client: one request, one response (no subscriptions, so no notifications arrive).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A coinbase-dust address can hold 100k+ UTXOs: the soak bank's getUtxosByAddresses answer was 29.9 MB on 2026-10-02 (tungstenite's
/// default limit is 16 MiB).
const MAX_MESSAGE: usize = 512 << 20;

pub struct Rpc {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

/// Plain KAS of some addresses per the node's UTXO index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Balance {
    /// non-coinbase UTXOs plus coinbase UTXOs at least `maturity` DAA old (covenant-bound UTXOs excluded)
    pub spendable: u64,
    /// coinbase UTXOs younger than the maturity
    pub immature: u64,
    pub utxos: usize,
    pub virtual_daa: u64,
    /// from a full UTXO listing (else: the getBalanceByAddress total)
    pub listed: bool,
}

#[derive(Deserialize)]
struct Head {
    id: Option<u64>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct UtxosResponse {
    params: UtxosParams,
}

#[derive(Deserialize)]
struct UtxosParams {
    entries: Vec<UtxoRow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtxoRow {
    pub utxo_entry: UtxoLite,
}

/// The fields of `RpcUtxoEntry` the balance needs (the script, the outpoint and the address are skipped while parsing).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtxoLite {
    pub amount: u64,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
    #[serde(default)]
    pub covenant_id: Option<IgnoredAny>,
}

impl Rpc {
    pub async fn connect(url: &str) -> Result<Self> {
        let config = WebSocketConfig { max_message_size: Some(MAX_MESSAGE), max_frame_size: Some(MAX_MESSAGE), ..Default::default() };
        let (ws, _) =
            tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async_with_config(url, Some(config), false))
                .await
                .map_err(|_| anyhow!("connect to {url} timed out"))?
                .with_context(|| format!("connect to {url}"))?;
        Ok(Self { ws, next_id: 1 })
    }

    /// The raw text of the response to one request (id matched, `error` checked).
    async fn call_text(&mut self, method: &str, params: Value) -> Result<String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "id": id, "method": method, "params": params });
        self.ws.send(Message::Text(req.to_string())).await?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = tokio::time::timeout(left, self.ws.next())
                .await
                .map_err(|_| anyhow!("rpc {method} timed out"))?
                .ok_or_else(|| anyhow!("connection closed"))??;
            let Message::Text(text) = msg else {
                if let Message::Close(_) = msg {
                    bail!("connection closed by node");
                }
                continue;
            };
            let head: Head = serde_json::from_str(&text)?;
            if head.id != Some(id) {
                continue;
            }
            if let Some(e) = head.error.filter(|e| !e.is_null()) {
                bail!("rpc error from {method}: {e}");
            }
            return Ok(text);
        }
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let text = self.call_text(method, params).await?;
        let v: Value = serde_json::from_str(&text)?;
        Ok(v.get("params").cloned().unwrap_or(Value::Null))
    }

    pub async fn balance(&mut self, addr: &str) -> Result<u64> {
        let r = self.call("getBalanceByAddress", json!({ "address": addr })).await?;
        r.get("balance").and_then(Value::as_u64).ok_or_else(|| anyhow!("bad balance response: {r}"))
    }

    /// Total KAS of `addrs` per the node (getBalanceByAddress: every UTXO, immature coinbase included). Cheap.
    pub async fn total(&mut self, addrs: &[String]) -> Result<u64> {
        let mut t = 0;
        for a in addrs {
            t += self.balance(a).await?;
        }
        Ok(t)
    }

    pub async fn virtual_daa(&mut self) -> Result<u64> {
        let r = self.call("getBlockDagInfo", json!({})).await?;
        r.get("virtualDaaScore").and_then(Value::as_u64).ok_or_else(|| anyhow!("bad getBlockDagInfo response"))
    }

    /// Spendable plain KAS of `addrs`: the UTXOs of the node's index, coinbase outputs only once `maturity` DAA old. One full UTXO
    /// listing (tens of MB for a coinbase-dust address).
    pub async fn spendable(&mut self, addrs: &[String], maturity: u64) -> Result<Balance> {
        let virtual_daa = self.virtual_daa().await?;
        let text = self.call_text("getUtxosByAddresses", json!({ "addresses": addrs })).await?;
        let r: UtxosResponse = serde_json::from_str(&text).context("parse getUtxosByAddresses")?;
        Ok(tally(r.params.entries.iter().map(|r| &r.utxo_entry), virtual_daa, maturity))
    }
}

pub fn tally<'a>(entries: impl Iterator<Item = &'a UtxoLite>, virtual_daa: u64, maturity: u64) -> Balance {
    let mut b = Balance { virtual_daa, listed: true, ..Default::default() };
    for u in entries {
        if u.covenant_id.is_some() {
            continue;
        }
        b.utxos += 1;
        if u.is_coinbase && virtual_daa.saturating_sub(u.block_daa_score) < maturity {
            b.immature += u.amount;
        } else {
            b.spendable += u.amount;
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tally_counts_matured_coinbase_and_skips_covenants() {
        let e = |amount: u64, daa: u64, cb: bool, cov: bool| {
            json!({ "address": "kaspatest:x", "outpoint": {"transactionId": "00", "index": 0},
                    "utxoEntry": { "amount": amount, "scriptPublicKey": "0000aa", "blockDaaScore": daa, "isCoinbase": cb,
                                   "covenantId": if cov { json!("ab") } else { Value::Null } } })
        };
        let text = json!({ "id": 1, "params": { "entries": [
            e(100, 9_000, true, false),  // 1000 old: mature
            e(200, 9_001, true, false),  // 999 old: immature
            e(400, 9_999, false, false), // not coinbase: spendable at once
            e(800, 1, false, true),      // covenant-bound: not plain KAS
        ] } })
        .to_string();
        let r: UtxosResponse = serde_json::from_str(&text).unwrap();
        let b = tally(r.params.entries.iter().map(|r| &r.utxo_entry), 10_000, 1000);
        assert_eq!((b.spendable, b.immature, b.utxos), (500, 200, 3));
        // an entry without covenantId (older nodes) parses as plain
        let r: UtxoRow =
            serde_json::from_value(json!({ "utxoEntry": { "amount": 1, "blockDaaScore": 0, "isCoinbase": false } })).unwrap();
        assert!(r.utxo_entry.covenant_id.is_none());
    }
}
