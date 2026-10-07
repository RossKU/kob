//! The node's Borsh wRPC encoding for the bulk calls: the windows of transaction bodies (`getVirtualChainFromBlockV2`) and
//! the primary's accepted transaction ids (`getVirtualChainFromBlock` with ids).
//!
//! The JSON wRPC answer of a VSPC window spends most of its bytes on field names, hex (every hash, script and signature
//! twice its size) and decimal numbers: about 1.4 KB per plain transaction on TN10. The same answer in Borsh is a fraction
//! of that, and on a link of a few MB/s the bytes per transaction decide whether the indexer keeps up with a busy chain
//! (docs/ops/executor.md, *Borsh windows*). The small calls (status, blue scores, chain hashes, submissions) stay JSON.
//!
//! The message frame is workflow-rpc's: a request is `borsh(Option<u64> id, RpcApiOps op)` followed by the request body, an
//! answer `borsh(Option<u64> id, ServerMessageKind kind, Option<RpcApiOps> op)` followed by the Borsh `Result` of the call (a
//! `ServerError` when the kind is `Error`). Bodies are encoded and decoded with the node's own types (`kaspa-rpc-core`, the
//! same release as the node), then converted to the indexer's wire types ([`super::types`]): the follower, the checks of
//! other nodes (`rpc::verify`) and the extractor see exactly what the JSON path gives them.

use super::types::{
    AcceptedIds, ChainBlockHeader, ChainIds, CovenantBinding, InputVerbose, Outpoint, RawChainBlock, RawVspcResponse, SpentUtxo, Tx,
    TxInput, TxOutput, TxVerbose, Verbosity,
};
use crate::hex::{Hash32, HexBytes};
use borsh::{BorshDeserialize, BorshSerialize};
use kaspa_rpc_core::api::ops::RpcApiOps;
use kaspa_rpc_core::model::{
    GetVirtualChainFromBlockRequest, GetVirtualChainFromBlockResponse, GetVirtualChainFromBlockV2Request,
    GetVirtualChainFromBlockV2Response, RpcChainBlockAcceptedTransactions, RpcDataVerbosityLevel, RpcOptionalHeader,
    RpcOptionalTransaction,
};
use workflow_serializer::prelude::{Deserializer, Serializer};

/// workflow-rpc `ServerMessageKind`.
const KIND_SUCCESS: u8 = 0;
const KIND_ERROR: u8 = 1;
const KIND_NOTIFICATION: u8 = 0xff;

fn kh(h: Hash32) -> kaspa_hashes::Hash {
    kaspa_hashes::Hash::from_bytes(h.0)
}

fn h32(h: kaspa_hashes::Hash) -> Hash32 {
    Hash32(h.as_bytes())
}

/// A request frame: header, then the body.
fn frame(id: u64, op: RpcApiOps, body: &impl Serializer) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    BorshSerialize::serialize(&(Some(id), op), &mut out).expect("a Vec never fails to grow");
    // the body travels as a Borsh byte vector (workflow-serializer `Payload`: u32 length, then the bytes)
    let mut payload = Vec::with_capacity(64);
    Serializer::serialize(body, &mut payload).expect("a Vec never fails to grow");
    BorshSerialize::serialize(&payload, &mut out).expect("a Vec never fails to grow");
    out
}

fn verbosity(v: Verbosity) -> RpcDataVerbosityLevel {
    match v {
        Verbosity::None => RpcDataVerbosityLevel::None,
        Verbosity::Low => RpcDataVerbosityLevel::Low,
        Verbosity::High => RpcDataVerbosityLevel::High,
        Verbosity::Full => RpcDataVerbosityLevel::Full,
    }
}

/// `getVirtualChainFromBlockV2` as a Borsh request frame.
pub fn vspc_v2_request(id: u64, start: Hash32, level: Verbosity, min_confirmation_count: Option<u64>) -> Vec<u8> {
    let req = GetVirtualChainFromBlockV2Request::new(kh(start), Some(verbosity(level)), min_confirmation_count);
    frame(id, RpcApiOps::GetVirtualChainFromBlockV2, &req)
}

/// `getVirtualChainFromBlock` with accepted transaction ids as a Borsh request frame.
pub fn chain_ids_request(id: u64, start: Hash32, min_confirmation_count: Option<u64>) -> Vec<u8> {
    let req = GetVirtualChainFromBlockRequest::new(kh(start), true, min_confirmation_count);
    frame(id, RpcApiOps::GetVirtualChainFromBlock, &req)
}

/// One answer frame of the node.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply<'a> {
    /// The answer to request `id`.
    Ok { id: Option<u64>, body: &'a [u8] },
    /// The node refused request `id`, with its message.
    Err { id: Option<u64>, message: String },
    /// A notification (never subscribed to; skipped).
    Notification,
}

/// Split an answer frame into header and body: `borsh(Option<u64> id, u8 kind, Option<RpcApiOps> op)`, then for a success
/// the Borsh `Result` of the call (Ok: the body, a byte vector holding the answer), for an error the `ServerError`.
pub fn parse_reply(buf: &[u8]) -> Result<Reply<'_>, String> {
    let mut rest = buf;
    let id = <Option<u64> as BorshDeserialize>::deserialize(&mut rest).map_err(|e| format!("Borsh answer header: {e}"))?;
    let (&kind, after) = rest.split_first().ok_or("Borsh answer header: no message kind")?;
    rest = after;
    if kind == KIND_NOTIFICATION {
        return Ok(Reply::Notification);
    }
    // the op the answer is about: not needed (the id says which request it answers), but it must be read past
    <Option<RpcApiOps> as BorshDeserialize>::deserialize(&mut rest).map_err(|e| format!("Borsh answer header: {e}"))?;
    match kind {
        // a success carries a Borsh `Result<body, ServerError>` (workflow-rpc `ServerResult`): tag 1 Ok, 0 Err
        KIND_SUCCESS => match rest.split_first() {
            Some((1, body)) => Ok(Reply::Ok { id, body }),
            Some((0, err)) => Ok(Reply::Err { id, message: server_error_text(err) }),
            _ => Err("Borsh answer: no result tag".into()),
        },
        KIND_ERROR => Ok(Reply::Err { id, message: server_error_text(rest) }),
        k => Err(format!("Borsh answer header: unknown message kind {k}")),
    }
}

/// The text of a workflow-rpc `ServerError` (a Borsh enum; the node's RPC errors travel as its `Text` variant).
pub fn server_error_text(body: &[u8]) -> String {
    const UNIT: [&str; 10] = [
        "connection is closed",
        "RPC call timed out",
        "no data",
        "RPC method not found",
        "resource lock error",
        "not a borsh request",
        "not a serde request",
        "request serialization error",
        "request deserialization error",
        "response serialization error",
    ];
    let Some((&tag, mut rest)) = body.split_first() else {
        return "node error (empty)".into();
    };
    match tag {
        t if (t as usize) < UNIT.len() => UNIT[t as usize].into(),
        // NotificationDeserialize, RespDeserialize, Text, WebSocketError: one string
        10 | 11 | 13 | 14 => {
            <String as BorshDeserialize>::deserialize(&mut rest).unwrap_or_else(|_| format!("node error (variant {tag})"))
        }
        // Data: bytes
        12 => <Vec<u8> as BorshDeserialize>::deserialize(&mut rest)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| "node error (data)".into()),
        t => format!("node error (variant {t})"),
    }
}

/// Script public key in the wire form of the JSON path: version (two bytes, big-endian), then the script.
fn spk_bytes(s: &kaspa_consensus_core::tx::ScriptPublicKey) -> HexBytes {
    let mut v = Vec::with_capacity(2 + s.script().len());
    v.extend_from_slice(&s.version().to_be_bytes());
    v.extend_from_slice(s.script());
    HexBytes(v)
}

fn header(h: RpcOptionalHeader) -> Result<ChainBlockHeader, String> {
    let hash = h.hash.ok_or("chain block header without hash")?;
    Ok(ChainBlockHeader {
        hash: h32(hash),
        daa_score: h.daa_score.ok_or_else(|| format!("chain block header {hash} without DAA score"))?,
        blue_score: h.blue_score.unwrap_or(0),
        timestamp: h.timestamp.unwrap_or(0),
        version: h.version,
        parents_by_level: h.parents_by_level,
        hash_merkle_root: h.hash_merkle_root.map(h32),
        accepted_id_merkle_root: h.accepted_id_merkle_root.map(h32),
        utxo_commitment: h.utxo_commitment.map(h32),
        bits: h.bits,
        nonce: h.nonce,
        blue_work: h.blue_work,
        pruning_point: h.pruning_point.map(h32),
    })
}

fn tx(t: RpcOptionalTransaction) -> Result<Tx, String> {
    let v = t.verbose_data.ok_or("transaction without verbose data")?;
    let id = v.transaction_id.ok_or("transaction without id")?;
    let inputs = t
        .inputs
        .into_iter()
        .map(|i| {
            let op = i.previous_outpoint.ok_or_else(|| format!("input of {id} without previous outpoint"))?;
            let previous_outpoint = Outpoint {
                transaction_id: h32(op.transaction_id.ok_or_else(|| format!("outpoint of an input of {id} without id"))?),
                index: op.index.ok_or_else(|| format!("outpoint of an input of {id} without index"))?,
            };
            let verbose_data = i
                .verbose_data
                .map(|vd| {
                    let utxo_entry = vd
                        .utxo_entry
                        .map(|u| {
                            Ok::<_, String>(SpentUtxo {
                                amount: u.amount.unwrap_or(0),
                                script_public_key: spk_bytes(
                                    u.script_public_key.as_ref().ok_or_else(|| format!("spent output of {id} without script"))?,
                                ),
                                block_daa_score: u.block_daa_score,
                                covenant_id: u.covenant_id.map(h32),
                            })
                        })
                        .transpose()?;
                    Ok::<_, String>(InputVerbose { utxo_entry })
                })
                .transpose()?;
            Ok(TxInput {
                previous_outpoint,
                signature_script: HexBytes(i.signature_script.unwrap_or_default()),
                sequence: i.sequence,
                verbose_data,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let outputs = t
        .outputs
        .into_iter()
        .map(|o| {
            Ok(TxOutput {
                value: o.value.ok_or_else(|| format!("output of {id} without value"))?,
                script_public_key: spk_bytes(o.script_public_key.as_ref().ok_or_else(|| format!("output of {id} without script"))?),
                covenant: o
                    .covenant
                    .and_then(|c| c.0)
                    .map(|c| CovenantBinding { authorizing_input: c.0.authorizing_input as u32, covenant_id: h32(c.0.covenant_id) }),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Tx {
        inputs,
        outputs,
        payload: HexBytes(t.payload.unwrap_or_default()),
        verbose_data: TxVerbose { transaction_id: h32(id), block_time: v.block_time.unwrap_or(0) },
        version: t.version,
        lock_time: t.lock_time,
        subnetwork_id: t.subnetwork_id.map(|s| HexBytes(AsRef::<[u8]>::as_ref(&s).to_vec())),
        gas: t.gas,
    })
}

fn chain_block(b: RpcChainBlockAcceptedTransactions) -> Result<RawChainBlock, String> {
    Ok(RawChainBlock {
        chain_block_header: header(b.chain_block_header)?,
        accepted_transactions: b.accepted_transactions.into_iter().map(tx).collect::<Result<_, _>>()?,
    })
}

/// The bytes of a workflow-serializer `Payload` (a Borsh byte vector: u32 length, then the bytes), without copying them.
fn payload(body: &[u8]) -> Result<&[u8], String> {
    let (len, rest) = body.split_at_checked(4).ok_or("Borsh answer body shorter than its length")?;
    let len = u32::from_le_bytes(len.try_into().expect("four bytes")) as usize;
    rest.get(..len).ok_or_else(|| format!("Borsh answer body of {} bytes, its length says {len}", rest.len()))
}

/// The body of a `getVirtualChainFromBlockV2` answer as the indexer's wire type; `wire_bytes` is the frame's size.
pub fn decode_vspc_v2(body: &[u8], wire_bytes: usize) -> Result<RawVspcResponse, String> {
    let mut r = payload(body)?;
    let resp = <GetVirtualChainFromBlockV2Response as Deserializer>::deserialize(&mut r)
        .map_err(|e| format!("getVirtualChainFromBlockV2 (Borsh): {e}"))?;
    let blocks = std::sync::Arc::try_unwrap(resp.chain_block_accepted_transactions).unwrap_or_else(|a| (*a).clone());
    Ok(RawVspcResponse {
        removed_chain_block_hashes: resp.removed_chain_block_hashes.iter().copied().map(h32).collect(),
        added_chain_block_hashes: resp.added_chain_block_hashes.iter().copied().map(h32).collect(),
        chain_block_accepted_transactions: blocks.into_iter().map(chain_block).collect::<Result<_, _>>()?,
        wire_bytes,
    })
}

/// The body of a `getVirtualChainFromBlock` answer with ids.
pub fn decode_chain_ids(body: &[u8]) -> Result<ChainIds, String> {
    let mut r = payload(body)?;
    let resp = <GetVirtualChainFromBlockResponse as Deserializer>::deserialize(&mut r)
        .map_err(|e| format!("getVirtualChainFromBlock (Borsh): {e}"))?;
    Ok(ChainIds {
        removed_chain_block_hashes: resp.removed_chain_block_hashes.into_iter().map(h32).collect(),
        added_chain_block_hashes: resp.added_chain_block_hashes.into_iter().map(h32).collect(),
        accepted_transaction_ids: resp
            .accepted_transaction_ids
            .into_iter()
            .map(|a| AcceptedIds {
                accepting_block_hash: h32(a.accepting_block_hash),
                accepted_transaction_ids: a.accepted_transaction_ids.into_iter().map(h32).collect(),
            })
            .collect(),
    })
}

/// The Borsh endpoint of the node behind a JSON wRPC URL: a path ending in `/json` (public nodes:
/// `wss://host/kaspa/testnet-10/wrpc/json`) becomes `.../borsh`; else a node's default JSON port `18xxx` becomes its Borsh
/// port `17xxx` (`ws://host:18210` on TN10 serves Borsh on `17210`). `None` when neither applies.
pub fn borsh_url_for(json_url: &str) -> Option<String> {
    let (base, query) = match json_url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (json_url, None),
    };
    let with_query = |b: String| match query {
        Some(q) => format!("{b}?{q}"),
        None => b,
    };
    let trimmed = base.trim_end_matches('/');
    if let Some(stem) = trimmed.strip_suffix("/json") {
        return Some(with_query(format!("{stem}/borsh")));
    }
    let (scheme, rest) = trimmed.split_once("://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if !(18_000..19_000).contains(&port) {
        return None;
    }
    Some(with_query(format!("{scheme}://{host}:{}{path}", port - 1_000)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::verify::{header_hash, tx_id};
    use kaspa_consensus_core::header::{CompressedParents, Header};
    use kaspa_consensus_core::subnets::SubnetworkId;
    use kaspa_consensus_core::tx::{
        CovenantBinding as KCov, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
    };
    use kaspa_rpc_core::model::{
        RpcAcceptedTransactionIds, RpcCovenantBinding, RpcNullableCovenantBinding, RpcOptionalTransactionInput,
        RpcOptionalTransactionInputVerboseData, RpcOptionalTransactionOutpoint, RpcOptionalTransactionOutput,
        RpcOptionalTransactionVerboseData, RpcOptionalUtxoEntry,
    };
    use std::sync::Arc;

    /// A body as the node sends it (a Borsh byte vector).
    fn wrap(inner: Vec<u8>) -> Vec<u8> {
        let mut out = vec![];
        BorshSerialize::serialize(&inner, &mut out).unwrap();
        out
    }

    fn hh(b: u8) -> kaspa_hashes::Hash {
        kaspa_hashes::Hash::from_bytes([b; 32])
    }

    /// A consensus transaction with a covenant input, a covenant output, a plain output and a payload, and its `Full` /
    /// `High` RPC form.
    fn sample_tx(full: bool, seed: u8) -> (Transaction, RpcOptionalTransaction) {
        let spk = ScriptPublicKey::from_vec(0, vec![0xaa, 0x20, seed, 0x87]);
        let t = Transaction::new(
            1,
            vec![TransactionInput::new(TransactionOutpoint::new(hh(seed), 2), vec![0x41, seed, 9], 7, 1)],
            vec![
                TransactionOutput::with_covenant(5_000, spk.clone(), Some(KCov::new(0, hh(seed ^ 0x55)))),
                TransactionOutput::new(1_234, ScriptPublicKey::from_vec(0, vec![0x20, seed, 0xac])),
            ],
            0,
            SubnetworkId::from([0; 20]),
            0,
            vec![0x4b, 0x4f, 0x42, 0x31, seed],
        );
        let id = t.id();
        let rpc = RpcOptionalTransaction {
            version: full.then_some(t.version),
            inputs: vec![RpcOptionalTransactionInput {
                previous_outpoint: Some(RpcOptionalTransactionOutpoint { transaction_id: Some(hh(seed)), index: Some(2) }),
                signature_script: Some(vec![0x41, seed, 9]),
                sequence: Some(7),
                sig_op_count: Some(1),
                compute_budget: Some(0),
                verbose_data: Some(RpcOptionalTransactionInputVerboseData {
                    utxo_entry: Some(RpcOptionalUtxoEntry::new(
                        Some(9_999),
                        Some(spk.clone()),
                        full.then_some(77),
                        Some(false),
                        None,
                        Some(hh(seed ^ 0x55)),
                    )),
                }),
            }],
            outputs: t
                .outputs
                .iter()
                .map(|o| RpcOptionalTransactionOutput {
                    value: Some(o.value),
                    script_public_key: Some(o.script_public_key.clone()),
                    verbose_data: None,
                    covenant: Some(RpcNullableCovenantBinding(o.covenant.map(RpcCovenantBinding))),
                })
                .collect(),
            lock_time: full.then_some(0),
            subnetwork_id: full.then(|| SubnetworkId::from([0; 20])),
            gas: full.then_some(0),
            payload: Some(t.payload.clone()),
            storage_mass: Some(0),
            verbose_data: Some(RpcOptionalTransactionVerboseData {
                transaction_id: Some(id),
                hash: Some(hh(1)),
                compute_mass: Some(1_000),
                block_hash: Some(hh(2)),
                block_time: Some(1_700_000_000_000 + seed as u64),
            }),
        };
        (t, rpc)
    }

    fn sample_header(full: bool, seed: u8) -> (kaspa_hashes::Hash, RpcOptionalHeader) {
        let h = Header::new_finalized(
            1,
            CompressedParents::try_from(vec![vec![hh(seed), hh(seed + 1)]]).unwrap(),
            hh(3),
            hh(4),
            hh(5),
            1_700_000_000_000,
            0x1e7fffff,
            42,
            1_000 + seed as u64,
            500u64.into(),
            900 + seed as u64,
            hh(6),
        );
        let rpc = RpcOptionalHeader {
            hash: Some(h.hash),
            version: Some(h.version),
            parents_by_level: Some(h.parents_by_level.clone()),
            hash_merkle_root: Some(h.hash_merkle_root),
            accepted_id_merkle_root: Some(h.accepted_id_merkle_root),
            utxo_commitment: full.then_some(h.utxo_commitment),
            timestamp: Some(h.timestamp),
            bits: Some(h.bits),
            nonce: Some(h.nonce),
            daa_score: Some(h.daa_score),
            blue_work: Some(h.blue_work),
            blue_score: Some(h.blue_score),
            pruning_point: full.then_some(h.pruning_point),
        };
        (h.hash, rpc)
    }

    fn sample_response(full: bool) -> GetVirtualChainFromBlockV2Response {
        let mut added = vec![];
        let mut blocks = vec![];
        for seed in [10u8, 20] {
            let (hash, header) = sample_header(full, seed);
            added.push(hash);
            let txs = (0..3).map(|k| sample_tx(full, seed + k).1).collect();
            blocks.push(RpcChainBlockAcceptedTransactions { chain_block_header: header, accepted_transactions: txs });
        }
        GetVirtualChainFromBlockV2Response {
            removed_chain_block_hashes: Arc::new(vec![hh(99)]),
            added_chain_block_hashes: Arc::new(added),
            chain_block_accepted_transactions: Arc::new(blocks),
        }
    }

    /// The same answer through the node's JSON and through Borsh gives the indexer identical data, at both levels it asks for.
    #[test]
    fn borsh_and_json_answers_decode_alike() {
        for full in [false, true] {
            let resp = sample_response(full);
            let json = serde_json::to_string(&resp).unwrap();
            let from_json: RawVspcResponse = serde_json::from_str(&json).unwrap();
            let mut body = vec![];
            Serializer::serialize(&resp, &mut body).unwrap();
            let body = wrap(body);
            let from_borsh = decode_vspc_v2(&body, body.len()).unwrap();
            assert_eq!(from_borsh.removed_chain_block_hashes, from_json.removed_chain_block_hashes);
            assert_eq!(from_borsh.added_chain_block_hashes, from_json.added_chain_block_hashes);
            assert_eq!(from_borsh.chain_block_accepted_transactions.len(), 2);
            for (a, b) in from_borsh.chain_block_accepted_transactions.iter().zip(&from_json.chain_block_accepted_transactions) {
                assert_eq!(a.chain_block_header, b.chain_block_header, "full={full}");
                assert_eq!(a.accepted_transactions, b.accepted_transactions, "full={full}");
            }
            assert!(body.len() * 2 < json.len(), "Borsh {} bytes, JSON {}", body.len(), json.len());
        }
    }

    /// A `Full` Borsh answer carries everything the checks of another node's window need: headers hash to their block hash,
    /// transactions to their id.
    #[test]
    fn borsh_full_answers_verify() {
        let resp = sample_response(true);
        let mut body = vec![];
        Serializer::serialize(&resp, &mut body).unwrap();
        let body = wrap(body);
        let raw = decode_vspc_v2(&body, body.len()).unwrap();
        for (hash, blk) in raw.added_chain_block_hashes.iter().zip(&raw.chain_block_accepted_transactions) {
            assert_eq!(header_hash(&blk.chain_block_header), Some(*hash));
            for t in &blk.accepted_transactions {
                assert_eq!(tx_id(t), Some(t.verbose_data.transaction_id));
            }
        }
        let (t, _) = sample_tx(true, 10);
        assert_eq!(raw.chain_block_accepted_transactions[0].accepted_transactions[0].verbose_data.transaction_id, h32(t.id()));
    }

    #[test]
    fn chain_ids_decode() {
        let resp = GetVirtualChainFromBlockResponse::new(
            vec![hh(1)],
            vec![hh(2), hh(3)],
            vec![RpcAcceptedTransactionIds { accepting_block_hash: hh(2), accepted_transaction_ids: vec![hh(7), hh(8)] }],
        );
        let mut body = vec![];
        Serializer::serialize(&resp, &mut body).unwrap();
        let body = wrap(body);
        let ids = decode_chain_ids(&body).unwrap();
        let json: ChainIds = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(ids.added_chain_block_hashes, json.added_chain_block_hashes);
        assert_eq!(ids.removed_chain_block_hashes, json.removed_chain_block_hashes);
        assert_eq!(ids.accepted_transaction_ids, json.accepted_transaction_ids);
    }

    /// Frames are workflow-rpc's: the request header the node's server reads, the answer header it writes.
    #[test]
    fn frames_round_trip() {
        let f = vspc_v2_request(7, Hash32([3; 32]), Verbosity::High, Some(12));
        let mut r = f.as_slice();
        let (id, op) = <(Option<u64>, RpcApiOps) as BorshDeserialize>::deserialize(&mut r).unwrap();
        assert_eq!((id, op), (Some(7), RpcApiOps::GetVirtualChainFromBlockV2));
        let inner = <Vec<u8> as BorshDeserialize>::deserialize(&mut r).unwrap();
        let req = <GetVirtualChainFromBlockV2Request as Deserializer>::deserialize(&mut inner.as_slice()).unwrap();
        assert_eq!(req.start_hash, hh(3));
        assert!(matches!(req.data_verbosity_level, Some(RpcDataVerbosityLevel::High)));
        assert_eq!(req.min_confirmation_count, Some(12));
        assert!(r.is_empty());

        let mut ok = vec![];
        BorshSerialize::serialize(&(Some(9u64), KIND_SUCCESS, Some(RpcApiOps::GetVirtualChainFromBlockV2)), &mut ok).unwrap();
        ok.extend_from_slice(&[1, 1, 2, 3]);
        assert_eq!(parse_reply(&ok).unwrap(), Reply::Ok { id: Some(9), body: &[1, 2, 3] });
        let mut failed = ok[..ok.len() - 4].to_vec();
        failed.push(0);
        BorshSerialize::serialize(&13u8, &mut failed).unwrap();
        BorshSerialize::serialize(&"boom".to_string(), &mut failed).unwrap();
        assert_eq!(parse_reply(&failed).unwrap(), Reply::Err { id: Some(9), message: "boom".into() });

        let mut err = vec![];
        BorshSerialize::serialize(&(Some(9u64), KIND_ERROR, Option::<RpcApiOps>::None), &mut err).unwrap();
        BorshSerialize::serialize(&13u8, &mut err).unwrap();
        BorshSerialize::serialize(&"cannot find header abc".to_string(), &mut err).unwrap();
        let reply = parse_reply(&err).unwrap();
        assert_eq!(reply, Reply::Err { id: Some(9), message: "cannot find header abc".into() });
        assert_eq!(crate::rpc::classify_node_message("cannot find header abc"), crate::rpc::NodeErrorKind::UnknownHash);

        let mut note = vec![];
        BorshSerialize::serialize(&(Option::<u64>::None, KIND_NOTIFICATION), &mut note).unwrap();
        assert_eq!(parse_reply(&note).unwrap(), Reply::Notification);

        // a body shorter than its length prefix is refused, not read past
        assert!(decode_vspc_v2(&[9, 0, 0, 0, 1, 0], 6).is_err());
        assert!(decode_chain_ids(&[1, 0]).is_err());
    }

    /// A captured answer frame of a live node (`KOB_BORSH_DUMP`): decodes.
    #[test]
    #[ignore]
    fn decode_dumped_frame() {
        let path = std::env::var("KOB_BORSH_DUMP").expect("KOB_BORSH_DUMP");
        let buf = std::fs::read(path).unwrap();
        let Reply::Ok { body, .. } = parse_reply(&buf).unwrap() else { panic!("not an answer") };
        let raw = decode_vspc_v2(body, buf.len()).unwrap();
        let txs: usize = raw.chain_block_accepted_transactions.iter().map(|b| b.accepted_transactions.len()).sum();
        eprintln!("{} chain blocks, {txs} transactions, {} bytes", raw.added_chain_block_hashes.len(), buf.len());
    }

    #[test]
    fn borsh_urls() {
        assert_eq!(borsh_url_for("ws://65.108.107.30:18210").as_deref(), Some("ws://65.108.107.30:17210"));
        assert_eq!(borsh_url_for("ws://127.0.0.1:18110/").as_deref(), Some("ws://127.0.0.1:17110"));
        assert_eq!(
            borsh_url_for("wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/json").as_deref(),
            Some("wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/borsh")
        );
        assert_eq!(borsh_url_for("wss://x.example/wrpc/json?k=1").as_deref(), Some("wss://x.example/wrpc/borsh?k=1"));
        assert_eq!(borsh_url_for("ws://host:9000"), None);
        assert_eq!(borsh_url_for("wss://host/kaspa"), None);
    }
}
