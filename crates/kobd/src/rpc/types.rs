//! Wire types for `getVirtualChainFromBlockV2` and the few other node calls the indexer uses.
//!
//! Field names follow the JSON wRPC of the node, in camelCase. Only the fields that the indexer
//! consumes are declared, and everything else the node sends is ignored. Nothing here is
//! persisted verbatim. The indexer keeps compact extracted records only (`indexer::record`).

use crate::hex::{Hash32, HexBytes};
use serde::{Deserialize, Serialize};

/// Verbosity level of the acceptance data.
///
/// `High` is the lowest level that carries payloads, previous outpoints and signature scripts,
/// see `rpc/core/src/convert/verbosity.rs` in rusty-kaspa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verbosity {
    /// The least data.
    None,
    /// Less data than `High`.
    Low,
    /// Payloads, previous outpoints and signature scripts.
    High,
    /// The header and transaction fields that the block hash and the transaction ids commit to.
    Full,
}

/// The request of `getVirtualChainFromBlockV2`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VspcRequest {
    /// The block to start the window from, the cursor.
    pub start_hash: Hash32,
    /// How much acceptance data the node returns.
    pub data_verbosity_level: Verbosity,
    /// The minimum confirmation count of the chain blocks, as the node defines it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confirmation_count: Option<u64>,
    /// Not sent. The client waits this many times its request timeout for the answer.
    ///
    /// A batch window that cannot shrink any further gets more time instead of the same request
    /// again, see `rpc::window`.
    #[serde(skip)]
    pub timeout_scale: u32,
    /// Not sent. The largest answer that the client may receive, in bytes.
    ///
    /// `None` means the `max_message_bytes` of the client. The transport refuses a larger answer
    /// as its frame header arrives, before its payload is buffered
    /// ([`crate::rpc::RpcError::TooLarge`]). This is the hard memory bound of the windows fetched
    /// ahead (`indexer::prefetch`).
    #[serde(skip)]
    pub max_bytes: Option<usize>,
}

impl VspcRequest {
    /// A request at the normal timeout of the client.
    pub fn new(start_hash: Hash32, data_verbosity_level: Verbosity, min_confirmation_count: Option<u64>) -> Self {
        VspcRequest { start_hash, data_verbosity_level, min_confirmation_count, timeout_scale: 1, max_bytes: None }
    }
}

/// One chain block with its accepted transactions, as the node returned it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawChainBlock {
    /// The header of the chain block.
    pub chain_block_header: ChainBlockHeader,
    /// The transactions that the chain block accepted.
    #[serde(default)]
    pub accepted_transactions: Vec<Tx>,
}

/// The header of a chain block.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChainBlockHeader {
    /// The block hash.
    pub hash: Hash32,
    /// The DAA score of the block.
    pub daa_score: u64,
    /// The blue score of the block.
    #[serde(default)]
    pub blue_score: u64,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub timestamp: u64,
    /// The first of the other header fields, which are `None` at `High`.
    ///
    /// They come with `Full` verbosity, except that `High` lacks the UTXO commitment and the
    /// pruning point. The block hash is computed from them, so a window from a node other than
    /// the primary is checked against the hash it was asked for (`rpc::verify`). They are not
    /// stored.
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
    /// The removed chain blocks, tip-first.
    #[serde(default)]
    pub removed_chain_block_hashes: Vec<Hash32>,
    /// The added chain blocks.
    #[serde(default)]
    pub added_chain_block_hashes: Vec<Hash32>,
    /// The added chain blocks with their accepted transactions.
    #[serde(default)]
    pub chain_block_accepted_transactions: Vec<RawChainBlock>,
    /// Size of the response on the wire, set by the client.
    ///
    /// It is the JSON bytes of its `params`, or the size of the Borsh frame.
    #[serde(skip)]
    pub wire_bytes: usize,
}

/// `getVirtualChainFromBlock` (v1) without accepted transaction ids.
///
/// It holds the block hashes of the selected chain only, a few dozen bytes per chain block. There
/// are at most `mergeset_size_limit * 10` of them, 2,480 at 10 BPS. The follower plans its
/// parallel windows on it (`indexer::follower`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainHashes {
    /// The removed chain blocks, tip-first.
    #[serde(default)]
    pub removed_chain_block_hashes: Vec<Hash32>,
    /// The added chain blocks.
    #[serde(default)]
    pub added_chain_block_hashes: Vec<Hash32>,
}

/// `getVirtualChainFromBlock` (v1) with accepted transaction ids.
///
/// It is the acceptance data of the primary node, about 70 bytes per accepted transaction. A
/// window fetched from another node is checked against it (`rpc::verify`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainIds {
    /// The removed chain blocks, tip-first.
    #[serde(default)]
    pub removed_chain_block_hashes: Vec<Hash32>,
    /// The added chain blocks.
    #[serde(default)]
    pub added_chain_block_hashes: Vec<Hash32>,
    /// The accepted transaction ids of each accepting block.
    #[serde(default)]
    pub accepted_transaction_ids: Vec<AcceptedIds>,
}

/// The transactions that one chain block accepted, in acceptance order.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedIds {
    /// The chain block that accepted the transactions.
    pub accepting_block_hash: Hash32,
    /// The ids of the accepted transactions.
    #[serde(default)]
    pub accepted_transaction_ids: Vec<Hash32>,
}

/// An accepted transaction with the fields the indexer needs.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Tx {
    /// The inputs.
    #[serde(default)]
    pub inputs: Vec<TxInput>,
    /// The outputs.
    #[serde(default)]
    pub outputs: Vec<TxOutput>,
    /// The payload as hex. It is empty when the transaction has no payload.
    #[serde(default)]
    pub payload: HexBytes,
    /// The transaction id and the block time.
    pub verbose_data: TxVerbose,
    /// The first of the fields of the transaction id that only `Full` verbosity carries.
    ///
    /// `rpc::verify` recomputes the id of a transaction from a node other than the primary from
    /// them. They are `None` at `High`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_time: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnetwork_id: Option<HexBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas: Option<u64>,
}

/// The verbose data of a transaction.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TxVerbose {
    /// The transaction id.
    pub transaction_id: Hash32,
    /// The time of the block that accepted the transaction.
    #[serde(default)]
    pub block_time: u64,
}

/// A transaction output, named by the transaction and the index.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Outpoint {
    /// The id of the transaction.
    pub transaction_id: Hash32,
    /// The index of the output.
    pub index: u32,
}

/// A transaction input.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TxInput {
    /// The output that the input spends.
    pub previous_outpoint: Outpoint,
    /// The signature script.
    #[serde(default)]
    pub signature_script: HexBytes,
    /// The sequence. It is part of the transaction id, and `High` carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    /// The verbose data of the input, with the spent UTXO.
    #[serde(default)]
    pub verbose_data: Option<InputVerbose>,
}

/// The verbose data of a transaction input.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InputVerbose {
    /// The UTXO that the input spends.
    #[serde(default)]
    pub utxo_entry: Option<SpentUtxo>,
}

/// The UTXO that an input spends.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpentUtxo {
    /// The value of the spent output. It comes at `High` verbosity and up, and is `None` when the
    /// node did not report it.
    #[serde(default)]
    pub amount: Option<u64>,
    /// Version (2 bytes) followed by the script, as hex.
    pub script_public_key: HexBytes,
    /// DAA score of the block that created the spent output.
    ///
    /// The node reports `null` here in VSPC v2 at every verbosity, so the indexer does not use
    /// it. It keeps the DAA of the block that created each tracked UTXO itself.
    #[serde(default)]
    pub block_daa_score: Option<u64>,
    /// The covenant id of the spent output, if it has one.
    #[serde(default)]
    pub covenant_id: Option<Hash32>,
}

/// A transaction output.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TxOutput {
    /// The value in sompi.
    pub value: u64,
    /// Version (2 bytes) followed by the script, as hex.
    pub script_public_key: HexBytes,
    /// The covenant binding of the output, if it has one.
    #[serde(default)]
    pub covenant: Option<CovenantBinding>,
}

/// The binding of an output to a covenant.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CovenantBinding {
    /// The index of the input that authorizes the binding.
    pub authorizing_input: u32,
    /// The covenant id.
    pub covenant_id: Hash32,
}

/// A chain block whose transactions have been parsed.
#[derive(Debug, Clone)]
pub struct AddedBlock {
    /// The header of the chain block.
    pub header: ChainBlockHeader,
    /// The transactions that it accepted.
    pub txs: Vec<Tx>,
}

/// A validated VSPC v2 response.
#[derive(Debug, Clone, Default)]
pub struct VspcBatch {
    /// The removed chain blocks, tip-first, exactly as the node returned them.
    pub removed: Vec<Hash32>,
    /// In selected-chain order.
    pub added: Vec<AddedBlock>,
    /// Size of the response of the node on the wire, 0 when unknown.
    pub wire_bytes: usize,
}

/// Why a VSPC response is not consistent.
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    /// The added hashes and the blocks with accepted transactions differ in number.
    #[error("added hashes ({hashes}) and accepted-transaction blocks ({blocks}) differ in length")]
    LengthMismatch { hashes: usize, blocks: usize },
    /// An added hash is not the hash in the header of its block.
    #[error("added hash #{index} {hash} does not match its chain block header {header}")]
    HashMismatch { index: usize, hash: Hash32, header: Hash32 },
}

impl RawVspcResponse {
    /// Checks that the response is internally consistent and turns it into a batch.
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

/// The subset of `getBlockDagInfo` used for lag and bootstrap decisions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagInfo {
    /// The network name.
    pub network: String,
    /// The sink block.
    pub sink: Hash32,
    /// The pruning point.
    pub pruning_point_hash: Hash32,
    /// The DAA score of the virtual block.
    pub virtual_daa_score: u64,
}

/// The subset of `getServerInfo`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// The version of the node.
    pub server_version: String,
    /// The network id, such as `mainnet` or `testnet-10`.
    pub network_id: String,
    /// Whether the node is synced.
    pub is_synced: bool,
    /// Whether the node has the UTXO index.
    #[serde(default)]
    pub has_utxo_index: bool,
    /// The DAA score of the virtual block.
    pub virtual_daa_score: u64,
}

/// One entry of `getUtxosByAddresses`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressUtxo {
    /// The output.
    pub outpoint: Outpoint,
    /// The UTXO entry.
    pub utxo_entry: AddressUtxoEntry,
}

/// The UTXO entry of a [`AddressUtxo`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressUtxoEntry {
    /// The value in sompi.
    pub amount: u64,
    /// The script public key.
    pub script_public_key: HexBytes,
    /// The DAA score of the block that created the output.
    #[serde(default)]
    pub block_daa_score: u64,
    /// The covenant id, if the output has one.
    #[serde(default)]
    pub covenant_id: Option<Hash32>,
}
