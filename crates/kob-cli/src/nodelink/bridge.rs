//! Every node call over the Borsh wRPC encoding, for the JSON-shaped client of the command line.
//!
//! This is the same code as `rpc::bridge` of the executor. Public Kaspa nodes serve Borsh wRPC
//! only, and the Borsh port of an own node answers the same calls in a fraction of the bytes. The
//! clients keep building requests and reading answers as JSON wRPC `params`, and this module
//! carries them over Borsh with the types of the node (`kaspa-rpc-core`):
//! - The JSON parameters are deserialised into the request type of the call, as the JSON wRPC
//!   server of the node does, and then Borsh-encoded.
//! - The Borsh answer is decoded into the response type and serialised back to JSON, as the JSON
//!   server sends it.
//!
//! Both directions use the serde of the node, so a client sees the same JSON over either
//! encoding. Only the calls that the command line makes are carried.
//!
//! An endpoint is Borsh when the path of its URL ends in `/borsh`, such as
//! `wss://host/kaspa/mainnet/wrpc/borsh` from the resolver. See [`is_borsh_url`].

use kaspa_rpc_core::api::ops::RpcApiOps;
use kaspa_rpc_core::model::*;
use serde_json::Value;
use workflow_serializer::prelude::{Deserializer, Serializer};

/// Whether `url` names a Borsh wRPC endpoint.
///
/// That is a path (before any query) that ends in `/borsh`, or no path and a default Borsh port
/// of a node: 17110 mainnet, 17210 testnet, 17510 simnet or 17610 devnet. Every other URL is
/// JSON wRPC.
pub fn is_borsh_url(url: &str) -> bool {
    let base = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
    let Some((_, rest)) = base.split_once("://") else { return false };
    if base.ends_with("/borsh") {
        return true;
    }
    !rest.contains('/') && rest.rsplit_once(':').is_some_and(|(_, port)| matches!(port, "17110" | "17210" | "17510" | "17610"))
}

/// A request encoded once: its op and its Borsh body.
///
/// [`Encoded::frame`] adds the header with the request id.
#[derive(Debug, Clone)]
pub struct Encoded {
    /// The operation the request calls.
    pub op: RpcApiOps,
    /// The Borsh encoding of the request.
    pub body: Vec<u8>,
}

impl Encoded {
    /// The request frame for request `id`.
    ///
    /// It is `borsh(Option<u64> id, op)`, then the body as a byte vector.
    pub fn frame(&self, id: u64) -> Vec<u8> {
        super::frame::frame_bytes(id, self.op, &self.body)
    }
}

macro_rules! calls {
    ($($method:literal => $op:ident, $req:ty, $resp:ty;)*) => {
        /// The calls this module carries (the method names of the JSON wRPC).
        #[allow(dead_code)]
        pub const METHODS: &[&str] = &[$($method),*];

        /// The JSON parameters of `method` as its Borsh request.
        pub fn encode(method: &str, params: &Value) -> Result<Encoded, String> {
            match method {
                $($method => {
                    let req: $req = serde_json::from_value(params.clone()).map_err(|e| format!("{method} parameters: {e}"))?;
                    let mut body = Vec::with_capacity(64);
                    Serializer::serialize(&req, &mut body).map_err(|e| format!("{method} (Borsh): {e}"))?;
                    Ok(Encoded { op: RpcApiOps::$op, body })
                })*
                other => Err(format!("{other} has no Borsh encoding in this client")),
            }
        }

        /// A Borsh answer body of `method` as the JSON `params` the node's JSON wRPC would send.
        pub fn decode(method: &str, body: &[u8]) -> Result<Value, String> {
            let mut r = super::frame::payload(body)?;
            match method {
                $($method => {
                    let resp = <$resp as Deserializer>::deserialize(&mut r).map_err(|e| format!("{method} (Borsh): {e}"))?;
                    serde_json::to_value(&resp).map_err(|e| format!("{method}: {e}"))
                })*
                other => Err(format!("{other} has no Borsh encoding in this client")),
            }
        }
    };
}

calls! {
    "getServerInfo" => GetServerInfo, GetServerInfoRequest, GetServerInfoResponse;
    "getBlockDagInfo" => GetBlockDagInfo, GetBlockDagInfoRequest, GetBlockDagInfoResponse;
    "getUtxosByAddresses" => GetUtxosByAddresses, GetUtxosByAddressesRequest, GetUtxosByAddressesResponse;
    "submitTransaction" => SubmitTransaction, SubmitTransactionRequest, SubmitTransactionResponse;
    "getFeeEstimate" => GetFeeEstimate, GetFeeEstimateRequest, GetFeeEstimateResponse;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::borsh::BorshSerialize;
    use serde_json::json;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// An answer body as the node sends it: the response's Borsh, as a byte vector.
    pub fn body<T: Serializer>(resp: &T) -> Vec<u8> {
        let mut inner = vec![];
        Serializer::serialize(resp, &mut inner).unwrap();
        let mut out = vec![];
        BorshSerialize::serialize(&inner, &mut out).unwrap();
        out
    }

    /// The node's side of a request.
    ///
    /// The body of the frame must decode into the request type that the node reads, and that
    /// request must equal the JSON parameters that the client sent.
    fn node_reads<T: Deserializer + serde::Serialize>(method: &str, params: Value) -> Value {
        let e = encode(method, &params).unwrap_or_else(|err| panic!("{method}: {err}"));
        let frame = e.frame(7);
        let mut r = frame.as_slice();
        let (id, op) = <(Option<u64>, RpcApiOps) as ::borsh::BorshDeserialize>::deserialize(&mut r).unwrap();
        assert_eq!((id, op), (Some(7), e.op), "{method}");
        let inner = <Vec<u8> as ::borsh::BorshDeserialize>::deserialize(&mut r).unwrap();
        let req = <T as Deserializer>::deserialize(&mut inner.as_slice()).unwrap();
        serde_json::to_value(&req).unwrap()
    }

    fn sample_tx() -> kaspa_consensus_core::tx::Transaction {
        use kaspa_consensus_core::subnets::SubnetworkId;
        use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput};
        Transaction::new(
            1,
            vec![TransactionInput::new_with_compute_budget(
                TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([3; 32]), 1),
                vec![0x41, 1, 2],
                0,
                12,
            )],
            vec![TransactionOutput::new(5_000, ScriptPublicKey::from_vec(0, vec![0x20, 9, 0xac]))],
            0,
            SubnetworkId::from([0; 20]),
            0,
            vec![],
        )
    }

    /// Every request the command line sends reaches the node as that request.
    #[test]
    fn every_request_the_cli_sends_encodes() {
        node_reads::<GetServerInfoRequest>("getServerInfo", json!({}));
        node_reads::<GetBlockDagInfoRequest>("getBlockDagInfo", json!({}));
        node_reads::<GetFeeEstimateRequest>("getFeeEstimate", json!({}));
        let addr = "kaspatest:qz484dy4edqk52xx6srdd4a6dcwp7euxw86fl43ve2wm8umqts20kyujkrssm";
        let r = node_reads::<GetUtxosByAddressesRequest>("getUtxosByAddresses", json!({"addresses": [addr]}));
        assert_eq!(r["addresses"][0], addr);
        // A transaction as the command line submits it.
        let tx = sample_tx();
        let tx_json = kob_protocol::issue::rpc_transaction_json(&tx);
        let r = node_reads::<SubmitTransactionRequest>("submitTransaction", json!({"transaction": tx_json, "allowOrphan": false}));
        let back: RpcTransaction = serde_json::from_value(r["transaction"].clone()).unwrap();
        let back = kaspa_consensus_core::tx::Transaction::try_from(back).unwrap();
        assert_eq!(back.id(), tx.id(), "the node reads the very transaction");
        // A request that the client builds wrongly is refused here, before the wire.
        assert!(encode("getUtxosByAddresses", &json!({"addresses": ["nonsense"]}))
            .unwrap_err()
            .contains("getUtxosByAddresses parameters"));
        assert!(encode("getPeerAddresses", &json!({})).unwrap_err().contains("no Borsh encoding"));
    }

    /// Answers decode to the JSON that the JSON wRPC of the node sends, so every parser of the
    /// client reads them unchanged.
    #[test]
    fn answers_decode_to_the_json_wrpc_shape() {
        let entries = vec![RpcUtxosByAddressesEntry {
            address: Some("kaspatest:qz484dy4edqk52xx6srdd4a6dcwp7euxw86fl43ve2wm8umqts20kyujkrssm".try_into().unwrap()),
            outpoint: RpcTransactionOutpoint { transaction_id: kaspa_hashes::Hash::from_bytes([0x11; 32]), index: 2 },
            utxo_entry: RpcUtxoEntry::new(
                7_000,
                kaspa_consensus_core::tx::ScriptPublicKey::from_vec(0, vec![0x20, 1, 0xac]),
                99,
                false,
                None,
            ),
        }];
        let utxos = GetUtxosByAddressesResponse::new(entries);
        let v = decode("getUtxosByAddresses", &body(&utxos)).unwrap();
        assert_eq!(v, serde_json::to_value(&utxos).unwrap());
        assert_eq!(v["entries"][0]["outpoint"]["transactionId"], H1);
        assert_eq!(v["entries"][0]["utxoEntry"]["amount"], 7_000);
        // What the client reads from the answer.
        let parsed = crate::wrpc::parse_utxo_entries(&v).unwrap();
        assert_eq!((parsed[0].amount, parsed[0].index, parsed[0].daa), (7_000, 2, 99));

        let submitted = SubmitTransactionResponse::new(kaspa_hashes::Hash::from_bytes([0x11; 32]));
        assert_eq!(decode("submitTransaction", &body(&submitted)).unwrap()["transactionId"], H1);

        let dag = GetBlockDagInfoResponse::new(
            kaspa_consensus_core::network::NetworkId::new(kaspa_consensus_core::network::NetworkType::Mainnet),
            10,
            11,
            vec![],
            0.0,
            12,
            vec![],
            kaspa_hashes::Hash::from_bytes([0x11; 32]),
            777,
            kaspa_hashes::Hash::from_bytes([0x22; 32]),
        );
        let v = decode("getBlockDagInfo", &body(&dag)).unwrap();
        assert_eq!(v, serde_json::to_value(&dag).unwrap());
        assert_eq!(crate::wrpc::normalize_network(v["network"].as_str().unwrap()), "mainnet");
        assert_eq!(v["virtualDaaScore"], 777);

        // A body shorter than its length is refused, and so is an unknown method.
        assert!(decode("getServerInfo", &[9, 0, 0, 0, 1]).is_err());
        assert!(decode("getPeerAddresses", &body(&submitted)).is_err());
        assert_eq!(METHODS.len(), 5);
    }

    #[test]
    fn borsh_urls_are_recognised_by_their_path() {
        assert!(is_borsh_url("wss://iris.kaspa.blue/kaspa/mainnet/wrpc/borsh"));
        assert!(is_borsh_url("wss://alpha-10.kaspa.stream/kaspa/testnet-10/wrpc/borsh/"));
        assert!(is_borsh_url("ws://127.0.0.1:17210/borsh?x=1"));
        assert!(!is_borsh_url("ws://127.0.0.1:18210"));
        assert!(is_borsh_url("ws://127.0.0.1:17210"), "an own node's default Borsh port");
        assert!(is_borsh_url("ws://10.0.0.2:17110/"));
        assert!(!is_borsh_url("ws://10.0.0.2:17111"));
        assert!(!is_borsh_url("wss://host/kaspa/mainnet/wrpc/json"));
        assert!(!is_borsh_url("borsh"));
    }
}
