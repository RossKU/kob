//! Checking a window fetched from a node other than the primary (`rpc::multi`).
//!
//! The primary node is the authority for the selected chain: the follower plans its windows on the primary's chain hashes
//! and takes every reorg decision from it. A window of transaction bodies may come from any configured node; before the
//! follower sees it, it is checked against what the primary says and against the hashes it was asked for:
//!
//! * the chain blocks are the primary's (`getVirtualChainFromBlock` from the same start), with no removed block;
//! * every chain block header hashes to the block hash it was asked for (`Full` verbosity carries the whole header), so its
//!   DAA score, blue score and timestamp are the real ones;
//! * every chain block accepted exactly the transactions the primary reports for it, in the same order (the primary's
//!   accepted transaction ids, about 70 bytes per transaction against kilobytes for the bodies);
//! * every transaction hashes to its id (`Full` verbosity carries the version, lock time, subnetwork and gas), so its
//!   outpoints, outputs (covenant bindings included) and payload are the real ones.
//!
//! What no hash covers: signature scripts (outside the transaction id) and the spent outputs a node attaches to each input.
//! The follower therefore takes those of every transaction that can matter to KOB from the primary before it applies the
//! window (`indexer::trust`); for everything else a node could only alter data the indexer ignores.

use crate::hex::Hash32;
use crate::rpc::types::{ChainBlockHeader, ChainIds, RawChainBlock, RawVspcResponse, SpentUtxo, Tx, TxInput};
use kaspa_consensus_core::header::Header;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    CovenantBinding, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
};
use kaspa_consensus_core::Hash;
use std::collections::HashMap;

fn kh(h: Hash32) -> Hash {
    Hash::from_bytes(h.0)
}

/// The hash of a header with every field (`Full` verbosity); `None` when a field is missing.
pub fn header_hash(h: &ChainBlockHeader) -> Option<Hash32> {
    let header = Header::new_finalized(
        h.version?,
        h.parents_by_level.clone()?,
        kh(h.hash_merkle_root?),
        kh(h.accepted_id_merkle_root?),
        kh(h.utxo_commitment?),
        h.timestamp,
        h.bits?,
        h.nonce?,
        h.daa_score,
        h.blue_work?,
        h.blue_score,
        kh(h.pruning_point?),
    );
    Some(Hash32(header.hash.as_bytes()))
}

/// Script public key from the wire form (version, two bytes big-endian, then the script).
fn spk(b: &[u8]) -> Option<ScriptPublicKey> {
    let (v, script) = (b.get(..2)?, &b[2..]);
    Some(ScriptPublicKey::from_vec(u16::from_be_bytes([v[0], v[1]]), script.to_vec()))
}

/// The id of a transaction with every field the id covers (`Full` verbosity); `None` when a field is missing.
pub fn tx_id(t: &Tx) -> Option<Hash32> {
    let subnetwork: [u8; 20] = t.subnetwork_id.as_ref()?.0.as_slice().try_into().ok()?;
    let inputs = t
        .inputs
        .iter()
        .map(|i| {
            let op = TransactionOutpoint::new(kh(i.previous_outpoint.transaction_id), i.previous_outpoint.index);
            // the id leaves out the signature script and the compute commitment
            Some(TransactionInput::new(op, vec![], i.sequence?, 0))
        })
        .collect::<Option<Vec<_>>>()?;
    let outputs = t
        .outputs
        .iter()
        .map(|o| {
            let cov = match o.covenant {
                Some(c) => Some(CovenantBinding::new(u16::try_from(c.authorizing_input).ok()?, kh(c.covenant_id))),
                None => None,
            };
            Some(TransactionOutput::with_covenant(o.value, spk(&o.script_public_key.0)?, cov))
        })
        .collect::<Option<Vec<_>>>()?;
    let tx = Transaction::new(t.version?, inputs, outputs, t.lock_time?, SubnetworkId::from(subnetwork), t.gas?, t.payload.0.clone());
    Some(Hash32(tx.id().as_bytes()))
}

/// Why a window from another node was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The node contradicts a hash or the primary's acceptance data for the same chain block: it lies (or is broken). It is
    /// dropped.
    Lie(String),
    /// The node is on another chain than the primary right now (behind, or on a side of a reorg): not used for a while.
    OutOfSync(String),
    /// The primary itself no longer has `start` on its selected chain: the plan is stale (the follower re-plans).
    ChainMoved,
}

/// Check a `Full` window `raw` from another node against the primary's acceptance data `ids` from the same start. Returns
/// how many leading chain blocks were verified (the caller drops the rest: blocks the primary did not report yet).
pub fn verify_window(raw: &RawVspcResponse, ids: &ChainIds) -> Result<usize, Rejection> {
    if !ids.removed_chain_block_hashes.is_empty() {
        return Err(Rejection::ChainMoved);
    }
    if !raw.removed_chain_block_hashes.is_empty() {
        return Err(Rejection::OutOfSync(format!("reports {} removed chain blocks", raw.removed_chain_block_hashes.len())));
    }
    if raw.added_chain_block_hashes.len() != raw.chain_block_accepted_transactions.len() {
        return Err(Rejection::Lie(format!(
            "{} added hashes but {} chain blocks with transactions",
            raw.added_chain_block_hashes.len(),
            raw.chain_block_accepted_transactions.len()
        )));
    }
    let accepted: HashMap<Hash32, &[Hash32]> =
        ids.accepted_transaction_ids.iter().map(|a| (a.accepting_block_hash, a.accepted_transaction_ids.as_slice())).collect();
    let n = raw.added_chain_block_hashes.len().min(ids.added_chain_block_hashes.len());
    if raw.added_chain_block_hashes[..n] != ids.added_chain_block_hashes[..n] {
        return Err(Rejection::OutOfSync("its selected chain differs from the primary's".into()));
    }
    for (hash, blk) in raw.added_chain_block_hashes[..n].iter().zip(&raw.chain_block_accepted_transactions) {
        verify_block(*hash, blk, accepted.get(hash).copied().unwrap_or(&[]))?;
    }
    Ok(n)
}

fn verify_block(hash: Hash32, blk: &RawChainBlock, expected: &[Hash32]) -> Result<(), Rejection> {
    let h = &blk.chain_block_header;
    if h.hash != hash {
        return Err(Rejection::Lie(format!("chain block {hash} carries the header of {}", h.hash)));
    }
    match header_hash(h) {
        Some(x) if x == hash => {}
        Some(x) => return Err(Rejection::Lie(format!("the header of chain block {hash} hashes to {x}"))),
        None => return Err(Rejection::Lie(format!("the header of chain block {hash} is incomplete (not a Full answer)"))),
    }
    let txs = &blk.accepted_transactions;
    if txs.len() != expected.len() {
        return Err(Rejection::Lie(format!(
            "chain block {hash} accepted {} transactions, the primary reports {}",
            txs.len(),
            expected.len()
        )));
    }
    for (pos, (t, want)) in txs.iter().zip(expected).enumerate() {
        if t.verbose_data.transaction_id != *want {
            return Err(Rejection::Lie(format!(
                "chain block {hash} transaction #{pos} is {}, the primary reports {want}",
                t.verbose_data.transaction_id
            )));
        }
        match tx_id(t) {
            Some(x) if x == *want => {}
            Some(x) => return Err(Rejection::Lie(format!("transaction {want} in chain block {hash} hashes to {x}"))),
            None => return Err(Rejection::Lie(format!("transaction {want} in chain block {hash} is incomplete (not a Full answer)"))),
        }
    }
    Ok(())
}

fn spent_eq(a: Option<&SpentUtxo>, b: Option<&SpentUtxo>) -> bool {
    match (a, b) {
        // the DAA score of the creating block is not used (and reported null) and may differ between verbosity levels
        (Some(a), Some(b)) => a.amount == b.amount && a.script_public_key == b.script_public_key && a.covenant_id == b.covenant_id,
        (None, None) => true,
        _ => false,
    }
}

fn spent(i: &TxInput) -> Option<&SpentUtxo> {
    i.verbose_data.as_ref().and_then(|v| v.utxo_entry.as_ref())
}

fn input_eq(a: &TxInput, b: &TxInput) -> bool {
    a.previous_outpoint == b.previous_outpoint
        && a.signature_script == b.signature_script
        && a.sequence == b.sequence
        && spent_eq(spent(a), spent(b))
}

/// The first difference between the same chain block from another node (`a`, `Full`) and from the primary (`b`, `High`), in
/// everything the indexer reads: the header fields it stores, and per transaction the id, inputs (signature scripts and
/// spent outputs included), outputs and payload. `None`: they agree.
pub fn body_difference(a: &RawChainBlock, b: &RawChainBlock) -> Option<String> {
    let (ha, hb) = (&a.chain_block_header, &b.chain_block_header);
    if (ha.hash, ha.daa_score, ha.blue_score, ha.timestamp) != (hb.hash, hb.daa_score, hb.blue_score, hb.timestamp) {
        return Some(format!("header of {} differs", hb.hash));
    }
    if a.accepted_transactions.len() != b.accepted_transactions.len() {
        return Some(format!(
            "chain block {} accepted {} transactions, not {}",
            hb.hash,
            a.accepted_transactions.len(),
            b.accepted_transactions.len()
        ));
    }
    for (x, y) in a.accepted_transactions.iter().zip(&b.accepted_transactions) {
        let id = y.verbose_data.transaction_id;
        if x.verbose_data.transaction_id != id {
            return Some(format!("chain block {} carries {} where the primary has {id}", hb.hash, x.verbose_data.transaction_id));
        }
        if x.inputs.len() != y.inputs.len() || !x.inputs.iter().zip(&y.inputs).all(|(p, q)| input_eq(p, q)) {
            return Some(format!("inputs of transaction {id} differ (signature script or spent output)"));
        }
        if x.outputs != y.outputs || x.payload != y.payload {
            return Some(format!("outputs or payload of transaction {id} differ"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::HexBytes;
    use crate::rpc::types::{CovenantBinding as WireBinding, Outpoint, TxOutput, TxVerbose};

    fn sample_tx(version: u16) -> Tx {
        Tx {
            inputs: vec![TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash32([7; 32]), index: 3 },
                signature_script: HexBytes(vec![1, 2, 3]),
                sequence: Some(5),
                verbose_data: None,
            }],
            outputs: vec![TxOutput {
                value: 1_000,
                script_public_key: HexBytes(vec![0, 0, 0x20, 1, 0xac]),
                covenant: (version > 0).then_some(WireBinding { authorizing_input: 0, covenant_id: Hash32([9; 32]) }),
            }],
            payload: HexBytes(vec![0xaa, 0xbb]),
            verbose_data: TxVerbose { transaction_id: Hash32::default(), block_time: 0 },
            version: Some(version),
            lock_time: Some(0),
            subnetwork_id: Some(HexBytes(vec![0; 20])),
            gas: Some(0),
        }
    }

    /// The id is the consensus id of the same transaction built with the consensus types (v0 and v1), and every field the
    /// id covers changes it; the signature script does not.
    #[test]
    fn tx_ids_follow_consensus() {
        for version in [0u16, 1] {
            let t = sample_tx(version);
            let id = tx_id(&t).unwrap();
            let k = Transaction::new(
                version,
                vec![TransactionInput::new(TransactionOutpoint::new(Hash::from_bytes([7; 32]), 3), vec![1, 2, 3], 5, 1)],
                vec![TransactionOutput::with_covenant(
                    1_000,
                    ScriptPublicKey::from_vec(0, vec![0x20, 1, 0xac]),
                    (version > 0).then(|| CovenantBinding::new(0, Hash::from_bytes([9; 32]))),
                )],
                0,
                SubnetworkId::from([0; 20]),
                0,
                vec![0xaa, 0xbb],
            );
            assert_eq!(id.0, k.id().as_bytes(), "v{version}");
            let mut s = t.clone();
            s.inputs[0].signature_script = HexBytes(vec![9; 70]);
            assert_eq!(tx_id(&s), Some(id), "the signature script is outside the id");
            let mut v = t.clone();
            v.outputs[0].value += 1;
            assert_ne!(tx_id(&v), Some(id));
            let mut p = t.clone();
            p.payload.0.push(0);
            assert_ne!(tx_id(&p), Some(id));
            let mut o = t.clone();
            o.inputs[0].previous_outpoint.index = 4;
            assert_ne!(tx_id(&o), Some(id));
            let mut missing = t.clone();
            missing.version = None;
            assert_eq!(tx_id(&missing), None, "a High answer cannot be checked");
        }
    }
}
