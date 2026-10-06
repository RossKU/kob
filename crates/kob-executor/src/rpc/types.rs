//! Wire types for `getVirtualChainFromBlockV2` and the few other node calls the indexer uses.
//!
//! Field names follow the node's JSON wRPC (camelCase). Only the fields the indexer consumes are
//! declared; everything else the node sends is ignored. Nothing here is persisted verbatim: the
//! indexer keeps compact extracted records only (`indexer::record`).

use crate::hex::{Hash32, HexBytes};
use serde::{Deserialize, Serialize};

/// Verbosity level of the acceptance data. `High` is the lowest level that carries payloads,
/// previous outpoints and signature scripts (`rpc/core/src/convert/verbosity.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verbosity {
    None,
    Low,
    High,
    Full,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VspcRequest {
    pub start_hash: Hash32,
    pub data_verbosity_level: Verbosity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confirmation_count: Option<u64>,
    /// Not sent: the client waits this many times its request timeout for the answer (a batch window
    /// that cannot shrink any further gets more time instead of the same request again, `rpc::window`).
    #[serde(skip)]
    pub timeout_scale: u32,
    /// Not sent: the largest answer the client may receive, bytes (`None`: the client's `max_message_bytes`). A larger answer is
    /// refused by the transport as its frame header arrives, before its payload is buffered ([`crate::rpc::RpcError::TooLarge`]):
    /// the hard memory bound of the windows fetched ahead (`indexer::prefetch`).
    #[serde(skip)]
    pub max_bytes: Option<usize>,
}

impl VspcRequest {
    /// A request at the client's normal timeout.
    pub fn new(start_hash: Hash32, data_verbosity_level: Verbosity, min_confirmation_count: Option<u64>) -> Self {
        VspcRequest { start_hash, data_verbosity_level, min_confirmation_count, timeout_scale: 1, max_bytes: None }
    }
}

/// One chain block with its accepted transactions, as the node returned it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawChainBlock {
    pub chain_block_header: ChainBlockHeader,
    #[serde(default)]
    pub accepted_transactions: Vec<Tx>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChainBlockHeader {
    pub hash: Hash32,
    pub daa_score: u64,
    #[serde(default)]
    pub blue_score: u64,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub timestamp: u64,
    /// The rest of the header (`Full` verbosity; `High` lacks the UTXO commitment and the pruning point): what the block hash
    /// is computed from, so a window from a node other than the primary is checked against the hash it was asked for
    /// (`rpc::verify`). Not stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parents_by_level: Option<kaspa_consensus_core::header::CompressedParents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_merkle_root: Option<Hash32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_id_merkle_root: Option<Hash32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utxo_commitment: Option<Hash32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blue_work: Option<kaspa_consensus_core::BlueWorkType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruning_point: Option<Hash32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawVspcResponse {
    /// Tip-first.
    #[serde(default)]
    pub removed_chain_block_hashes: Vec<Hash32>,
    #[serde(default)]
    pub added_chain_block_hashes: Vec<Hash32>,
    #[serde(default)]
    pub chain_block_accepted_transactions: Vec<RawChainBlock>,
    /// Size of the response on the wire (JSON bytes of its `params`), set by the client.
    #[serde(skip)]
    pub wire_bytes: usize,
}

/// `getVirtualChainFromBlock` (v1) without accepted transaction ids: the selected chain's block hashes only, a few
/// dozen bytes per chain block (at most `mergeset_size_limit * 10` of them, 2,480 at 10 BPS). The follower plans its
/// parallel windows on it (`indexer::follower`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainHashes {
    /// Tip-first.
    #[serde(default)]
    pub removed_chain_block_hashes: Vec<Hash32>,
    #[serde(default)]
    pub added_chain_block_hashes: Vec<Hash32>,
}

/// `getVirtualChainFromBlock` (v1) WITH accepted transaction ids: the acceptance data of the primary node, about 70 bytes per
/// accepted transaction, against which a window fetched from another node is checked (`rpc::verify`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainIds {
    /// Tip-first.
    #[serde(default)]
    pub removed_chain_block_hashes: Vec<Hash32>,
    #[serde(default)]
    pub added_chain_block_hashes: Vec<Hash32>,
    #[serde(default)]
    pub accepted_transaction_ids: Vec<AcceptedIds>,
}

/// The transactions one chain block accepted, in acceptance order.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedIds {
    pub accepting_block_hash: Hash32,
    #[serde(default)]
    pub accepted_transaction_ids: Vec<Hash32>,
}

/// An accepted transaction with the fields the indexer needs.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Tx {
    #[serde(default)]
    pub inputs: Vec<TxInput>,
    #[serde(default)]
    pub outputs: Vec<TxOutput>,
    /// Hex; empty when the transaction has no payload.
    #[serde(default)]
    pub payload: HexBytes,
    pub verbose_data: TxVerbose,
    /// The fields of the transaction id that only `Full` verbosity carries (`rpc::verify` recomputes the id of a transaction
    /// from a node other than the primary). `None` at `High`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_time: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnetwork_id: Option<HexBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TxVerbose {
    pub transaction_id: Hash32,
    #[serde(default)]
    pub block_time: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Outpoint {
    pub transaction_id: Hash32,
    pub index: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TxInput {
    pub previous_outpoint: Outpoint,
    #[serde(default)]
    pub signature_script: HexBytes,
    /// Part of the transaction id (`High` carries it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default)]
    pub verbose_data: Option<InputVerbose>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InputVerbose {
    #[serde(default)]
    pub utxo_entry: Option<SpentUtxo>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpentUtxo {
    #[serde(default)]
    pub amount: u64,
    /// Version (2 bytes) followed by the script, as hex.
    pub script_public_key: HexBytes,
    /// DAA score of the block that created the spent output. The node reports `null` here in VSPC v2
    /// (at every verbosity), so the indexer does not use it: it keeps the DAA of the block that created
    /// each tracked UTXO itself.
    #[serde(default)]
    pub block_daa_score: Option<u64>,
    #[serde(default)]
    pub covenant_id: Option<Hash32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TxOutput {
    pub value: u64,
    /// Version (2 bytes) followed by the script, as hex.
    pub script_public_key: HexBytes,
    #[serde(default)]
    pub covenant: Option<CovenantBinding>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CovenantBinding {
    pub authorizing_input: u32,
    pub covenant_id: Hash32,
}

/// A chain block whose transactions have been parsed.
#[derive(Debug, Clone)]
pub struct AddedBlock {
    pub header: ChainBlockHeader,
    pub txs: Vec<Tx>,
}

/// A validated VSPC v2 response.
#[derive(Debug, Clone, Default)]
pub struct VspcBatch {
    /// Tip-first, exactly as the node returned it.
    pub removed: Vec<Hash32>,
    /// In selected-chain order.
    pub added: Vec<AddedBlock>,
    /// Size of the node's response on the wire (0 when unknown).
    pub wire_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("added hashes ({hashes}) and accepted-transaction blocks ({blocks}) differ in length")]
    LengthMismatch { hashes: usize, blocks: usize },
    #[error("added hash #{index} {hash} does not match its chain block header {header}")]
    HashMismatch { index: usize, hash: Hash32, header: Hash32 },
}

impl RawVspcResponse {
    /// Check the response is internally consistent.
    pub fn into_batch(self) -> Result<VspcBatch, BatchError> {
        if self.added_chain_block_hashes.len() != self.chain_block_accepted_transactions.len() {
            return Err(BatchError::LengthMismatch {
                hashes: self.added_chain_block_hashes.len(),
                blocks: self.chain_block_accepted_transactions.len(),
            });
        }
        let mut added = Vec::with_capacity(self.added_chain_block_hashes.len());
        for (i, (hash, blk)) in self.added_chain_block_hashes.iter().zip(self.chain_block_accepted_transactions).enumerate() {
            if blk.chain_block_header.hash != *hash {
                return Err(BatchError::HashMismatch { index: i, hash: *hash, header: blk.chain_block_header.hash });
            }
            added.push(AddedBlock { header: blk.chain_block_header, txs: blk.accepted_transactions });
        }
        Ok(VspcBatch { removed: self.removed_chain_block_hashes, added, wire_bytes: self.wire_bytes })
    }
}

/// Subset of `getBlockDagInfo` used for lag and bootstrap decisions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagInfo {
    pub network: String,
    pub sink: Hash32,
    pub pruning_point_hash: Hash32,
    pub virtual_daa_score: u64,
}

/// Subset of `getServerInfo`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub server_version: String,
    pub network_id: String,
    pub is_synced: bool,
    #[serde(default)]
    pub has_utxo_index: bool,
    pub virtual_daa_score: u64,
}

/// One entry of `getUtxosByAddresses`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressUtxo {
    pub outpoint: Outpoint,
    pub utxo_entry: AddressUtxoEntry,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressUtxoEntry {
    pub amount: u64,
    pub script_public_key: HexBytes,
    #[serde(default)]
    pub block_daa_score: u64,
    #[serde(default)]
    pub covenant_id: Option<Hash32>,
}
