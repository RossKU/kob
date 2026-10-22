//! Every node call over the node's Borsh wRPC encoding, for the clients that speak JSON (`rpc::WrpcClient`,
//! `matcher::node::WrpcNode`, the x402 facilitator's `x402::node::WrpcClient`).
//!
//! Public Kaspa nodes (the resolver's, `rpc::resolver`) serve Borsh wRPC only, and an own node's Borsh port answers the same
//! calls in a fraction of the bytes. The clients keep building their requests and reading their answers as the JSON wRPC
//! `params` they always did; this module carries them over Borsh with the node's own types (`kaspa-rpc-core`, the release of
//! the node): the JSON parameters are deserialised into the call's request type (serde, the very conversion the node's JSON
//! wRPC server makes) and Borsh-encoded; the Borsh answer is decoded into the call's response type and serialised back to
//! JSON (serde, what the node's JSON server sends). Both directions are the node's own serde, so a client sees the same JSON
//! over either encoding (tests below, and `rpc::borsh` for the windows of transaction bodies, which skip the JSON step).
//!
//! An endpoint is Borsh when its URL path ends in `/borsh` (`wss://host/kaspa/mainnet/wrpc/borsh`, what the resolver
//! returns), see [`is_borsh_url`]; any other URL is JSON.

use kaspa_rpc_core::api::ops::RpcApiOps;
use kaspa_rpc_core::model::*;
use serde_json::Value;
use workflow_serializer::prelude::{Deserializer, Serializer};

/// Whether `url` names a Borsh wRPC endpoint: its path (before any query) ends in `/borsh` (the public nodes'
/// `.../wrpc/borsh`), or it has no path and a node's default Borsh port (rusty-kaspa v2.1.0 `default_borsh_rpc_port`: 17110
/// mainnet, 17210 testnet, 17510 simnet, 17610 devnet; `--rpclisten-borsh=default`). Every other URL is JSON wRPC.
pub fn is_borsh_url(url: &str) -> bool {
    let base = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
    let Some((_, rest)) = base.split_once("://") else { return false };
    if base.ends_with("/borsh") {
        return true;
    }
    !rest.contains('/') && rest.rsplit_once(':').is_some_and(|(_, port)| matches!(port, "17110" | "17210" | "17510" | "17610"))
}

/// A request encoded once: its op and its Borsh body; [`Encoded::frame`] adds the header of a request id.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub op: RpcApiOps,
    pub body: Vec<u8>,
}

impl Encoded {
    /// The request frame for request `id` (workflow-rpc: `borsh(Option<u64> id, op)`, then the body as a byte vector).
    pub fn frame(&self, id: u64) -> Vec<u8> {
        super::borsh::frame_bytes(id, self.op, &self.body)
    }
}

macro_rules! calls {
    ($($method:literal => $op:ident, $req:ty, $resp:ty;)*) => {
        /// The calls this module carries (the method names of the JSON wRPC).
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
            let mut r = super::borsh::payload(body)?;
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
    "getSinkBlueScore" => GetSinkBlueScore, GetSinkBlueScoreRequest, GetSinkBlueScoreResponse;
    "getBlock" => GetBlock, GetBlockRequest, GetBlockResponse;
    "getVirtualChainFromBlock" => GetVirtualChainFromBlock, GetVirtualChainFromBlockRequest, GetVirtualChainFromBlockResponse;
    "getVirtualChainFromBlockV2" => GetVirtualChainFromBlockV2, GetVirtualChainFromBlockV2Request, GetVirtualChainFromBlockV2Response;
    "getUtxosByAddresses" => GetUtxosByAddresses, GetUtxosByAddressesRequest, GetUtxosByAddressesResponse;
    "submitTransaction" => SubmitTransaction, SubmitTransactionRequest, SubmitTransactionResponse;
    "submitTransactionReplacement" => SubmitTransactionReplacement, SubmitTransactionReplacementRequest, SubmitTransactionReplacementResponse;
    "getFeeEstimate" => GetFeeEstimate, GetFeeEstimateRequest, GetFeeEstimateResponse;
    "getMempoolEntry" => GetMempoolEntry, GetMempoolEntryRequest, GetMempoolEntryResponse;
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;
    use serde_json::json;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// An answer body as the node sends it: the response's Borsh, as a byte vector.
    fn body<T: Serializer>(resp: &T) -> Vec<u8> {
        let mut inner = vec![];
        Serializer::serialize(resp, &mut inner).unwrap();
        let mut out = vec![];
        BorshSerialize::serialize(&inner, &mut out).unwrap();
        out
    }

    /// The node's side of a request: the frame's body decodes into the request type the node reads, and that request is the
    /// JSON parameters the client sent (serde both ways, as the node's JSON server would read them).
    fn node_reads<T: Deserializer + serde::Serialize>(method: &str, params: Value) -> Value {
        let e = encode(method, &params).unwrap_or_else(|err| panic!("{method}: {err}"));
        let frame = e.frame(7);
        let mut r = frame.as_slice();
        let (id, op) = <(Option<u64>, RpcApiOps) as borsh::BorshDeserialize>::deserialize(&mut r).unwrap();
        assert_eq!((id, op), (Some(7), e.op), "{method}");
        let inner = <Vec<u8> as borsh::BorshDeserialize>::deserialize(&mut r).unwrap();
        let req = <T as Deserializer>::deserialize(&mut inner.as_slice()).unwrap();
        serde_json::to_value(&req).unwrap()
    }

    /// Every request the executor's clients send, with the parameters their code builds, reaches the node as that request.
    #[test]
    fn every_request_the_clients_send_encodes() {
        let r = node_reads::<GetServerInfoRequest>("getServerInfo", json!({}));
        assert_eq!(r, json!({}));
        node_reads::<GetBlockDagInfoRequest>("getBlockDagInfo", json!({}));
        node_reads::<GetSinkBlueScoreRequest>("getSinkBlueScore", json!({}));
        let r = node_reads::<GetBlockRequest>("getBlock", json!({"hash": H1, "includeTransactions": false}));
        assert_eq!(r, json!({"hash": H1, "includeTransactions": false}));
        let r = node_reads::<GetVirtualChainFromBlockRequest>(
            "getVirtualChainFromBlock",
            json!({"startHash": H1, "includeAcceptedTransactionIds": true, "minConfirmationCount": 10}),
        );
        assert_eq!(r["startHash"], H1);
        assert_eq!(r["includeAcceptedTransactionIds"], true);
        assert_eq!(r["minConfirmationCount"], 10);
        let r = node_reads::<GetVirtualChainFromBlockRequest>(
            "getVirtualChainFromBlock",
            json!({"startHash": H1, "includeAcceptedTransactionIds": false}),
        );
        assert_eq!(r["minConfirmationCount"], Value::Null);
        // the matcher's chain follower and the indexer's windows (crate::rpc::types::VspcRequest)
        let req = crate::rpc::types::VspcRequest {
            start_hash: crate::hex::Hash32([0x11; 32]),
            data_verbosity_level: crate::rpc::types::Verbosity::High,
            min_confirmation_count: Some(3),
            timeout_scale: 1,
            max_bytes: None,
        };
        let r = node_reads::<GetVirtualChainFromBlockV2Request>("getVirtualChainFromBlockV2", serde_json::to_value(&req).unwrap());
        assert_eq!((r["startHash"].as_str(), r["minConfirmationCount"].as_u64()), (Some(H1), Some(3)));
        assert_eq!(r["dataVerbosityLevel"], "High");
        let r = node_reads::<GetVirtualChainFromBlockV2Request>(
            "getVirtualChainFromBlockV2",
            json!({"startHash": H1, "dataVerbosityLevel": "Low"}),
        );
        assert_eq!(r["dataVerbosityLevel"], "Low");
        let addr = "kaspatest:qz484dy4edqk52xx6srdd4a6dcwp7euxw86fl43ve2wm8umqts20kyujkrssm";
        let r = node_reads::<GetUtxosByAddressesRequest>("getUtxosByAddresses", json!({"addresses": [addr]}));
        assert_eq!(r["addresses"][0], addr);
        node_reads::<GetFeeEstimateRequest>("getFeeEstimate", json!({}));
        let r = node_reads::<GetMempoolEntryRequest>(
            "getMempoolEntry",
            json!({"transactionId": H1, "includeOrphanPool": false, "filterTransactionPool": false}),
        );
        assert_eq!(r["transactionId"], H1);
        // a transaction as the matcher and the facilitator send it (kob_protocol::issue::rpc_transaction_json)
        let tx = sample_tx();
        let tx_json = kob_protocol::issue::rpc_transaction_json(&tx);
        let r = node_reads::<SubmitTransactionRequest>("submitTransaction", json!({"transaction": tx_json, "allowOrphan": false}));
        let back: RpcTransaction = serde_json::from_value(r["transaction"].clone()).unwrap();
        let back = kaspa_consensus_core::tx::Transaction::try_from(back).unwrap();
        assert_eq!(back.id(), tx.id(), "the node reads the very transaction");
        let r = node_reads::<SubmitTransactionReplacementRequest>("submitTransactionReplacement", json!({"transaction": tx_json}));
        let back = kaspa_consensus_core::tx::Transaction::try_from(
            serde_json::from_value::<RpcTransaction>(r["transaction"].clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(back.id(), tx.id());
        // what the client does not send correctly is refused here, before the wire
        assert!(encode("getBlock", &json!({"hash": "zz"})).unwrap_err().contains("getBlock parameters"));
        assert!(encode("getPeerAddresses", &json!({})).unwrap_err().contains("no Borsh encoding"));
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

    /// Answers decode to the JSON the node's JSON wRPC sends (the response type's serde), so every parser of the clients reads
    /// them unchanged.
    #[test]
    fn answers_decode_to_the_json_wrpc_shape() {
        let info = GetServerInfoResponse {
            rpc_api_version: 1,
            rpc_api_revision: 0,
            server_version: "2.1.0".into(),
            network_id: kaspa_consensus_core::network::NetworkId::with_suffix(kaspa_consensus_core::network::NetworkType::Testnet, 10),
            has_utxo_index: true,
            is_synced: true,
            virtual_daa_score: 123_456,
        };
        let v = decode("getServerInfo", &body(&info)).unwrap();
        assert_eq!(v, serde_json::to_value(&info).unwrap());
        // what the clients read from it
        let s: crate::matcher::node::ServerInfo = serde_json::from_value(v.clone()).unwrap();
        assert_eq!((s.network_id.as_str(), s.is_synced, s.has_utxo_index, s.virtual_daa_score), ("testnet-10", true, true, 123_456));
        let s: crate::rpc::types::ServerInfo = serde_json::from_value(v).unwrap();
        assert_eq!(s.server_version, "2.1.0");

        let sink = GetSinkBlueScoreResponse::new(42);
        assert_eq!(decode("getSinkBlueScore", &body(&sink)).unwrap(), json!({"blueScore": 42}));

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
        let d: crate::matcher::node::DagInfo = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(d.virtual_daa_score, 777);
        let d: crate::rpc::types::DagInfo = serde_json::from_value(v).unwrap();
        assert_eq!(d.sink, crate::hex::Hash32([0x22; 32]));

        // a body shorter than its length is refused, an unknown method too
        assert!(decode("getServerInfo", &[9, 0, 0, 0, 1]).is_err());
        assert!(decode("getPeerAddresses", &body(&sink)).is_err());
        assert_eq!(METHODS.len(), 11);
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
