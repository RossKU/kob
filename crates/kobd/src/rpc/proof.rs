//! Chain proofs: one node cannot make the indexer apply a chain block it forged.
//!
//! Every batch of chain blocks is checked before the follower applies it (`indexer::follower`),
//! whichever node served it. A node that fails a check is refused and backed off from, and the
//! cursor does not move.
//!
//! # What is checked
//!
//! - **Headers.** Every header is complete (`Full` verbosity) and hashes to its block hash. Its
//!   first direct parent, the selected parent of a chain block, is the chain block before it, or
//!   the cursor.
//! - **Proof of work.** kHeavyHash of every header meets the target of its `bits` (`kaspa-pow`).
//! - **Difficulty floor.** A header's target may be at most [`ChainProofConfig::floor_factor`]
//!   times the median target of the last [`ChainProofConfig::floor_window`] headers accepted, and
//!   never easier than a fixed floor per network ([`ChainProofConfig::min_difficulty`]). This is a
//!   heuristic against a forged chain with trivial `bits`, not the node's full difficulty
//!   adjustment.
//! - **Transactions.** Every accepted transaction hashes to its id.
//! - **Acceptance.** The accepted transactions of every chain block are those its
//!   `accepted_id_merkle_root` commits to. Since KIP-21 that root is the sequencing commitment: a
//!   chain of lane tips over every lane's accepted transaction ids, folded into a sparse Merkle
//!   tree. The checker keeps the tip of each lane it saw at the cursor, folds the batch's accepted
//!   transactions into it block by block, and checks the result with one lane proof
//!   (`getSeqCommitLaneProof`) per lane at the batch's last chain block. The native lane is always
//!   checked, so no native transaction can be hidden or added anywhere in the batch.
//! - **Coinbase lane.** Its transactions are not proven. They must look like coinbase
//!   transactions: no input, no covenant output, no KOB payload.
//! - **Signature scripts.** No id covers them. For every transaction that can matter to KOB
//!   (`indexer::trust`), the merged block that holds it is fetched once: its header is checked as
//!   above, its transactions must match its `hash_merkle_root`, and the signature scripts must be
//!   the same ([`check_merged_block`]).
//!
//! # Left to the node
//!
//! - A lane other than the native and coinbase lanes that the batch does not show at all. Its
//!   proof is asked for only when the batch has a transaction in it.
//! - The spent outputs attached to inputs. No hash the indexer can check covers them. Tracked
//!   outputs are the store's own.
//! - On testnet-10 the proof of work is cheap. A single GPU mines a block there in seconds.
//!
//! # Cost
//!
//! One kHeavyHash and one header hash per chain block, one id hash per transaction, and one lane
//! proof per lane per batch, a call of about 1 KB, and one `getBlock` per merged block that holds a
//! transaction that can matter to KOB. No thread of its own: the follower runs the CPU part on its
//! blocking pool, as it parses a batch. The calls of a batch go out together
//! (`indexer::follower::PROOF_FETCHES` at once), so a batch waits about one round trip per kind of
//! call (the anchor, its lanes, the lanes at the end, the merged blocks), not one per call.

use crate::hex::Hash32;
use crate::rpc::types::{ChainBlockHeader, RawVspcResponse, Tx};
use kaspa_consensus_core::block::Block;
use kaspa_consensus_core::header::Header;
use kaspa_consensus_core::subnets::{SUBNETWORK_ID_COINBASE, SUBNETWORK_ID_NATIVE};
use kaspa_hashes::{Hash, SeqCommitActiveNode};
use kaspa_math::Uint256;
use kaspa_rpc_core::GetSeqCommitLaneProofResponse;
use kaspa_seq_commit::hashing::{
    activity_digest_lane, activity_leaf, activity_root_hash, lane_key, lane_tip_next, mergeset_context_hash, seq_commit,
    seq_state_root, smt_leaf_hash,
};
use kaspa_seq_commit::types::{LaneTipInput, MergesetContext, SeqCommitInput, SeqState, SmtLeafInput};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// The `[chain_proof]` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ChainProofConfig {
    /// Check every batch before applying it. Default `true`.
    pub enabled: bool,
    /// A header's target may be at most this many times the recent median. Default 4.
    pub floor_factor: u64,
    /// Headers the median is taken over. Default 600, a minute at 10 BPS.
    pub floor_window: usize,
    /// The easiest difficulty ever accepted. Default per network: 1e13 on mainnet (its
    /// difficulty was 1.6e16 in November 2026), 1e4 on testnet-10 (1.6e6), 1 elsewhere.
    pub min_difficulty: Option<u64>,
}

impl Default for ChainProofConfig {
    fn default() -> Self {
        ChainProofConfig { enabled: true, floor_factor: 4, floor_window: 600, min_difficulty: None }
    }
}

impl ChainProofConfig {
    fn min_difficulty_for(&self, network: &str) -> u64 {
        self.min_difficulty
            .unwrap_or(match network {
                "mainnet" => 10_000_000_000_000,
                "testnet-10" => 10_000,
                _ => 1,
            })
            .max(1)
    }
}

/// Why a batch was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofError {
    /// The node's data fails a check. The node is backed off from.
    Refused(String),
    /// A proof could not be fetched. Nothing is held against the node.
    Unavailable(String),
}

impl std::fmt::Display for ProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProofError::Refused(w) => write!(f, "chain proof refused: {w}"),
            ProofError::Unavailable(w) => write!(f, "chain proof unavailable: {w}"),
        }
    }
}

fn refuse<T>(why: impl Into<String>) -> Result<T, ProofError> {
    Err(ProofError::Refused(why.into()))
}

fn kh(h: Hash32) -> Hash {
    Hash::from_bytes(h.0)
}

/// The consensus header of a `Full` header, or `None` when a field is missing.
pub fn consensus_header(h: &ChainBlockHeader) -> Option<Header> {
    Some(Header::new_finalized(
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
    ))
}

/// Whether kHeavyHash of `h` meets the target of its `bits`.
pub fn pow_ok(h: &Header) -> bool {
    kaspa_pow::State::new(h).check_pow(h.nonce).0
}

/// The KIP-21 lane key of a subnetwork.
pub fn lane_of(subnetwork: &[u8; 20]) -> Hash {
    lane_key(subnetwork)
}

/// The key of the native lane.
pub fn native_lane() -> Hash {
    lane_of(SUBNETWORK_ID_NATIVE.as_bytes())
}

/// A lane's tip and the blue score of its last update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneTip {
    pub tip: Hash,
    pub blue_score: u64,
}

/// What the checker needs of the last chain block it accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub hash: Hash32,
    pub timestamp: u64,
    pub blue_score: u64,
    pub accepted_id_merkle_root: Hash,
}

impl Anchor {
    fn of(h: &Header) -> Anchor {
        Anchor {
            hash: Hash32(h.hash.as_bytes()),
            timestamp: h.timestamp,
            blue_score: h.blue_score,
            accepted_id_merkle_root: h.accepted_id_merkle_root,
        }
    }
}

/// Checks a lane proof of `block` (its `accepted_id_merkle_root` is `root`) against what the
/// checker computed for the lane, `expected` (`None`: no active lane).
pub fn check_lane_proof(root: Hash, lane: Hash, expected: Option<LaneTip>, p: &GetSeqCommitLaneProofResponse) -> Result<(), String> {
    let claimed = p.lane.as_ref().map(|l| LaneTip { tip: l.tip, blue_score: l.blue_score });
    if claimed != expected {
        return Err(format!("lane {lane}: the node proves {claimed:?}, the accepted transactions give {expected:?}"));
    }
    let proof = kaspa_smt::proof::OwnedSmtProof::from_bytes(&p.smt_proof).map_err(|e| format!("lane {lane}: bad SMT proof: {e}"))?;
    let leaf = expected.map(|l| smt_leaf_hash(&SmtLeafInput { lane_tip: &l.tip, blue_score: l.blue_score }));
    let lanes_root = proof.compute_root::<SeqCommitActiveNode>(&lane, leaf).map_err(|e| format!("lane {lane}: bad SMT proof: {e}"))?;
    let activity_root = activity_root_hash(&p.inactivity_shortcut, &lanes_root);
    let state_root = seq_state_root(&SeqState { activity_root: &activity_root, payload_and_ctx_digest: &p.payload_and_ctx_digest });
    let commit = seq_commit(&SeqCommitInput { parent_seq_commit: &p.parent_seq_commit, state_root: &state_root });
    if commit != root {
        return Err(format!("lane {lane}: the proof gives the commitment {commit}, the header has {root}"));
    }
    Ok(())
}

/// The activity of one chain block: the digest of each lane it touched.
#[derive(Debug, Clone)]
struct BlockActivity {
    daa_score: u64,
    blue_score: u64,
    /// The selected parent's timestamp and `accepted_id_merkle_root` (a new lane starts from it).
    /// `None` for the first block, whose selected parent is the anchor.
    parent: Option<(u64, Hash)>,
    lanes: BTreeMap<Hash, Hash>,
}

/// What [`ChainProof::check_local`] found in a batch, for the steps that need the node.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The block the batch continues: the cursor, or the fork point of a reorg.
    pub parent: Hash32,
    /// The lanes to prove: the native lane and every other lane the batch touched, coinbase aside.
    pub lanes: Vec<Hash>,
    /// The batch's last chain block.
    pub end: Hash32,
    end_root: Hash,
    end_blue: u64,
    blocks: Vec<BlockActivity>,
    targets: Vec<Uint256>,
    last: Option<Anchor>,
}

/// Counters for `/v1/health`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ChainProofStatus {
    /// Batches are checked.
    pub enabled: bool,
    /// Chain blocks that passed.
    pub blocks_total: u64,
    /// Lane proofs checked.
    pub lane_proofs_total: u64,
    /// Merged blocks fetched for signature scripts.
    pub merged_blocks_total: u64,
    /// Batches refused.
    pub refused_total: u64,
    /// Why the last batch was refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refusal: Option<String>,
    /// CPU time of the local checks per chain block, in microseconds (mean since the start).
    pub cpu_us_per_block: f64,
    #[serde(skip)]
    cpu_ns_total: u128,
}

/// The checker's state: the last accepted chain block, the lane tips there and the recent
/// targets.
pub struct ChainProof {
    cfg: ChainProofConfig,
    finality_depth: u64,
    static_max: Uint256,
    anchor: Option<Anchor>,
    /// Lane tips at the anchor, `None` for a lane known to be inactive there.
    lanes: HashMap<Hash, Option<LaneTip>>,
    targets: VecDeque<Uint256>,
    status: ChainProofStatus,
    /// Checks left out, for the ablation tests only ([`skip`]).
    skip: u8,
}

/// The checks the ablation tests can leave out, one bit each.
pub(crate) mod skip {
    pub const POW: u8 = 1;
    pub const FLOOR: u8 = 2;
    pub const LINK: u8 = 4;
    pub const TXID: u8 = 8;
    pub const LANES: u8 = 16;
    pub const MERKLE: u8 = 32;
    pub const SCRIPTS: u8 = 64;
}

/// Lanes remembered at most. A lane beyond this is proven again at the anchor when it shows up.
const MAX_LANES: usize = 64;

fn median(v: impl Iterator<Item = Uint256>) -> Option<Uint256> {
    let mut all: Vec<Uint256> = v.collect();
    if all.is_empty() {
        return None;
    }
    let mid = all.len() / 2;
    Some(*all.select_nth_unstable(mid).1)
}

impl ChainProof {
    pub fn new(cfg: ChainProofConfig, network: &str) -> ChainProof {
        use kaspa_consensus_core::config::constants::consensus::MAX_DIFFICULTY_TARGET;
        use kaspa_consensus_core::config::params::{MAINNET_PARAMS, TESTNET_PARAMS};
        let finality_depth = if network == "mainnet" { MAINNET_PARAMS.finality_depth() } else { TESTNET_PARAMS.finality_depth() };
        let static_max = MAX_DIFFICULTY_TARGET / cfg.min_difficulty_for(network);
        let status = ChainProofStatus { enabled: cfg.enabled, ..Default::default() };
        ChainProof { cfg, finality_depth, static_max, anchor: None, lanes: HashMap::new(), targets: VecDeque::new(), status, skip: 0 }
    }

    /// Leaves the checks `s` out (`skip`). For the ablation tests.
    #[cfg(test)]
    pub(crate) fn skipping(mut self, s: u8) -> Self {
        self.skip = s;
        self
    }

    fn on(&self, check: u8) -> bool {
        self.skip & check == 0
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn status(&self) -> ChainProofStatus {
        self.status.clone()
    }

    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor
    }

    /// Notes a refusal for the status.
    pub fn refused(&mut self, why: &str) {
        self.status.refused_total += 1;
        self.status.last_refusal = Some(why.to_string());
    }

    /// The largest target a header may have now.
    fn max_target(&self, recent: Option<Uint256>) -> Uint256 {
        match recent.map(|m| m.overflowing_mul_u64(self.cfg.floor_factor.max(1))) {
            Some((t, false)) if t < self.static_max => t,
            _ => self.static_max,
        }
    }

    /// Checks one header: complete, hashing to `hash`, proof of work, the floor.
    fn check_header(&self, hash: Hash32, h: &ChainBlockHeader, max_target: Uint256) -> Result<Header, ProofError> {
        let Some(header) = consensus_header(h) else {
            return refuse(format!("the header of {hash} is incomplete (not a Full answer)"));
        };
        if header.hash.as_bytes() != hash.0 || h.hash != hash {
            return refuse(format!("the header of {hash} hashes to {}", header.hash));
        }
        self.check_work(&header, max_target)?;
        Ok(header)
    }

    fn check_work(&self, header: &Header, max_target: Uint256) -> Result<(), ProofError> {
        let target = Uint256::from_compact_target_bits(header.bits);
        if self.on(skip::FLOOR) && target > max_target {
            return refuse(format!(
                "block {} claims bits {:#x}, easier than the floor (at most {}x the recent median, never under the network minimum)",
                header.hash, header.bits, self.cfg.floor_factor
            ));
        }
        if self.on(skip::POW) && !pow_ok(header) {
            return refuse(format!("block {} fails its proof of work (bits {:#x})", header.hash, header.bits));
        }
        Ok(())
    }

    /// The local checks of a batch from the cursor `start`: headers, links, proof of work, the
    /// floor, transaction ids and the coinbase lane. `None`: nothing to prove (no chain block).
    pub fn check_local(&mut self, start: Hash32, raw: &RawVspcResponse) -> Result<Option<Plan>, ProofError> {
        let t0 = std::time::Instant::now();
        let r = self.check_local_inner(start, raw);
        self.status.cpu_ns_total += t0.elapsed().as_nanos();
        r
    }

    fn check_local_inner(&self, start: Hash32, raw: &RawVspcResponse) -> Result<Option<Plan>, ProofError> {
        let n = raw.added_chain_block_hashes.len();
        if n != raw.chain_block_accepted_transactions.len() {
            return refuse("the added hashes and the chain blocks differ in number");
        }
        if n == 0 {
            return Ok(None);
        }
        let window = self.cfg.floor_window.max(1);
        let mut recent = median(self.targets.iter().copied());
        let mut targets = Vec::with_capacity(n);
        let mut blocks = Vec::with_capacity(n);
        let mut lanes: Vec<Hash> = vec![native_lane()];
        let coinbase = SUBNETWORK_ID_COINBASE.as_bytes();
        let mut prev: Option<Header> = None;
        let mut parent = start;
        for (i, (hash, blk)) in raw.added_chain_block_hashes.iter().zip(&raw.chain_block_accepted_transactions).enumerate() {
            let header = self.check_header(*hash, &blk.chain_block_header, self.max_target(recent))?;
            let Some(&sp) = header.direct_parents().first() else { return refuse(format!("chain block {hash} has no parent")) };
            match &prev {
                _ if !self.on(skip::LINK) => {}
                Some(p) if sp != p.hash => {
                    return refuse(format!(
                        "chain block {hash} has the selected parent {sp}, not the chain block {} before it",
                        p.hash
                    ))
                }
                None if raw.removed_chain_block_hashes.is_empty() && sp.as_bytes() != start.0 => {
                    return refuse(format!("chain block {hash} has the selected parent {sp}, not the cursor {start}"))
                }
                None => parent = Hash32(sp.as_bytes()),
                _ => {}
            }
            targets.push(Uint256::from_compact_target_bits(header.bits));
            if i % 64 == 63 {
                let keep = window.saturating_sub(targets.len());
                recent = median(self.targets.iter().rev().take(keep).copied().chain(targets.iter().rev().take(window).copied()));
            }
            let mut leaves: BTreeMap<Hash, Vec<Hash>> = BTreeMap::new();
            for (k, t) in blk.accepted_transactions.iter().enumerate() {
                let id = t.verbose_data.transaction_id;
                match crate::rpc::verify::tx_id(t) {
                    _ if !self.on(skip::TXID) => {}
                    Some(x) if x == id => {}
                    Some(x) => return refuse(format!("transaction {id} in chain block {hash} hashes to {x}")),
                    None => return refuse(format!("transaction {id} in chain block {hash} is incomplete (not a Full answer)")),
                }
                let sub: [u8; 20] = match t.subnetwork_id.as_ref().and_then(|s| s.0.as_slice().try_into().ok()) {
                    Some(s) => s,
                    None => return refuse(format!("transaction {id} has no 20-byte subnetwork id")),
                };
                if &sub == coinbase {
                    let kob = t.payload.0.starts_with(kob_protocol::payload::MAGIC);
                    if !t.inputs.is_empty() || t.outputs.iter().any(|o| o.covenant.is_some()) || kob {
                        return refuse(format!("coinbase-lane transaction {id} has inputs, covenant outputs or a KOB payload"));
                    }
                    continue;
                }
                let version = t.version.unwrap_or_default();
                leaves.entry(lane_of(&sub)).or_default().push(activity_leaf(&kh(id), version, k as u32));
            }
            let mut digests = BTreeMap::new();
            for (lane, l) in leaves {
                if !lanes.contains(&lane) {
                    lanes.push(lane);
                }
                digests.insert(lane, activity_digest_lane(l.into_iter()));
            }
            let parent = prev.as_ref().map(|p| (p.timestamp, p.accepted_id_merkle_root));
            blocks.push(BlockActivity { daa_score: header.daa_score, blue_score: header.blue_score, parent, lanes: digests });
            prev = Some(header);
        }
        let last = prev.expect("n > 0");
        Ok(Some(Plan {
            parent,
            lanes,
            end: Hash32(last.hash.as_bytes()),
            end_root: last.accepted_id_merkle_root,
            end_blue: last.blue_score,
            blocks,
            targets,
            last: Some(Anchor::of(&last)),
        }))
    }

    /// The anchor is not the block `plan` continues: the follower fetches that block's header.
    pub fn needs_anchor(&self, plan: &Plan) -> bool {
        self.anchor.is_none_or(|a| a.hash != plan.parent)
    }

    /// Sets the anchor from the header of `plan.parent`, fetched from the node.
    pub fn set_anchor(&mut self, plan: &Plan, header: &Header) -> Result<(), ProofError> {
        if header.hash.as_bytes() != plan.parent.0 {
            return refuse(format!("the header asked for {} is that of {}", plan.parent, header.hash));
        }
        if kaspa_consensus_core::hashing::header::hash(header) != header.hash {
            return refuse(format!("the header of {} does not hash to it", plan.parent));
        }
        self.check_work(header, self.max_target(median(self.targets.iter().copied())))?;
        self.anchor = Some(Anchor::of(header));
        self.lanes.clear();
        Ok(())
    }

    /// The lanes of `plan` with no known tip at the anchor: the follower proves them there.
    pub fn unknown_lanes(&self, plan: &Plan) -> Vec<Hash> {
        plan.lanes.iter().copied().filter(|l| !self.lanes.contains_key(l)).collect()
    }

    /// Learns the tip of `lane` at the anchor from its proof there.
    pub fn learn_lane(&mut self, lane: Hash, p: &GetSeqCommitLaneProofResponse) -> Result<(), ProofError> {
        let Some(a) = self.anchor else { return refuse("no anchor") };
        let claimed = p.lane.as_ref().map(|l| LaneTip { tip: l.tip, blue_score: l.blue_score });
        if self.on(skip::LANES) {
            check_lane_proof(a.accepted_id_merkle_root, lane, claimed, p).map_err(ProofError::Refused)?;
        }
        self.status.lane_proofs_total += 1;
        if self.lanes.len() >= MAX_LANES && !self.lanes.contains_key(&lane) {
            let native = native_lane();
            if let Some(k) = self.lanes.keys().copied().find(|k| *k != native) {
                self.lanes.remove(&k);
            }
        }
        self.lanes.insert(lane, claimed);
        Ok(())
    }

    /// Folds the batch into the lane tips known at the anchor: the tip each lane of `plan` must
    /// have at its last chain block.
    pub fn expected(&self, plan: &Plan) -> Result<Vec<(Hash, Option<LaneTip>)>, ProofError> {
        let Some(anchor) = self.anchor.filter(|a| a.hash == plan.parent) else {
            return refuse("the anchor is not the batch's parent");
        };
        let mut tips: HashMap<Hash, Option<LaneTip>> = HashMap::new();
        for lane in &plan.lanes {
            let Some(t) = self.lanes.get(lane) else { return refuse(format!("lane {lane} is not known at the anchor")) };
            tips.insert(*lane, *t);
        }
        for b in &plan.blocks {
            // The context of a chain block: its selected parent's timestamp, its DAA and blue score.
            let (parent_ts, parent_commit) = b.parent.unwrap_or((anchor.timestamp, anchor.accepted_id_merkle_root));
            let context =
                mergeset_context_hash(&MergesetContext { timestamp: parent_ts, daa_score: b.daa_score, blue_score: b.blue_score });
            for (lane, digest) in &b.lanes {
                let entry = tips.entry(*lane).or_insert(None);
                // A lane idle for longer than the finality depth has expired and starts anew.
                let live = entry.filter(|t| t.blue_score >= b.blue_score.saturating_sub(self.finality_depth));
                let parent_ref = live.map(|t| t.tip).unwrap_or(parent_commit);
                let tip = lane_tip_next(&LaneTipInput {
                    parent_ref: &parent_ref,
                    lane_key: lane,
                    activity_digest: digest,
                    context_hash: &context,
                });
                *entry = Some(LaneTip { tip, blue_score: b.blue_score });
            }
        }
        Ok(plan
            .lanes
            .iter()
            .map(|l| {
                let t = tips.get(l).copied().flatten().filter(|t| t.blue_score >= plan.end_blue.saturating_sub(self.finality_depth));
                (*l, t)
            })
            .collect())
    }

    /// Checks the lane proofs at the batch's last chain block against `expected` and accepts the
    /// batch: the anchor moves to its last chain block.
    pub fn finish(
        &mut self,
        plan: Plan,
        expected: &[(Hash, Option<LaneTip>)],
        proofs: &[GetSeqCommitLaneProofResponse],
    ) -> Result<(), ProofError> {
        if proofs.len() != expected.len() {
            return refuse("a lane proof is missing");
        }
        for ((lane, want), p) in expected.iter().zip(proofs).filter(|_| self.on(skip::LANES)) {
            check_lane_proof(plan.end_root, *lane, *want, p)
                .map_err(|e| ProofError::Refused(format!("chain block {}: {e}", plan.end)))?;
        }
        self.status.lane_proofs_total += proofs.len() as u64;
        self.status.blocks_total += plan.blocks.len() as u64;
        if self.status.blocks_total > 0 {
            self.status.cpu_us_per_block = self.status.cpu_ns_total as f64 / 1000.0 / self.status.blocks_total as f64;
        }
        for (lane, t) in expected {
            self.lanes.insert(*lane, *t);
        }
        self.targets.extend(plan.targets);
        let window = self.cfg.floor_window.max(1);
        while self.targets.len() > window {
            self.targets.pop_front();
        }
        self.anchor = plan.last;
        Ok(())
    }

    /// Checks a merged block fetched for the signature scripts of `txs` (its transactions that can
    /// matter to KOB, from the batch).
    pub fn check_merged_block(&mut self, hash: Hash32, block: &Block, txs: &[&Tx]) -> Result<(), ProofError> {
        let header = &block.header;
        if header.hash.as_bytes() != hash.0 || kaspa_consensus_core::hashing::header::hash(header) != header.hash {
            return refuse(format!("the merged block asked for as {hash} has the header of {}", header.hash));
        }
        self.check_work(header, self.max_target(median(self.targets.iter().copied())))?;
        let root = kaspa_consensus_core::merkle::calc_hash_merkle_root(block.transactions.iter());
        if self.on(skip::MERKLE) && root != header.hash_merkle_root {
            return refuse(format!(
                "the transactions of merged block {hash} give the Merkle root {root}, its header has {}",
                header.hash_merkle_root
            ));
        }
        for t in txs {
            let id = t.verbose_data.transaction_id;
            let Some(k) = block.transactions.iter().find(|x| x.id().as_bytes() == id.0) else {
                return refuse(format!("merged block {hash} does not hold transaction {id}"));
            };
            let same = k.inputs.len() == t.inputs.len()
                && k.inputs.iter().zip(&t.inputs).all(|(a, b)| a.signature_script == b.signature_script.0);
            if self.on(skip::SCRIPTS) && !same {
                return refuse(format!("the signature scripts of transaction {id} differ from merged block {hash}"));
            }
        }
        self.status.merged_blocks_total += 1;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Real headers, bodies and lane proofs recorded from public TN10 and mainnet nodes
    //! (`testdata/record.mjs`), then tampered with. Each check has an ablation pair: the forgery it
    //! catches passes once that check is left out.
    use super::*;
    use kaspa_rpc_core::{RpcBlock, RpcHeader};
    use serde_json::Value;

    pub(crate) const TN10: &str = include_str!("testdata/proof_tn10.json");
    pub(crate) const MAINNET: &str = include_str!("testdata/proof_mainnet.json");

    pub(crate) type Proofs = HashMap<Hash, (GetSeqCommitLaneProofResponse, GetSeqCommitLaneProofResponse)>;

    pub(crate) struct Fixture {
        pub(crate) network: String,
        pub(crate) start: Hash32,
        pub(crate) start_header: Header,
        json: Value,
        /// Per lane key: its proofs at the start and at the last chain block.
        pub(crate) proofs: Proofs,
        pub(crate) merged: Block,
    }

    pub(crate) fn fixture(text: &str) -> Fixture {
        let json: Value = serde_json::from_str(text).unwrap();
        let start = Hash32::parse(json["start"].as_str().unwrap()).unwrap();
        let rpc_header: RpcHeader = serde_json::from_value(json["startHeader"].clone()).unwrap();
        let start_header = Header::try_from(rpc_header).unwrap();
        let proofs = json["proofs"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, p)| {
                let lane = Hash::from_bytes(Hash32::parse(k).unwrap().0);
                let at = |w: &str| serde_json::from_value::<GetSeqCommitLaneProofResponse>(p[w].clone()).unwrap();
                (lane, (at("start"), at("end")))
            })
            .collect();
        let merged = Block::try_from(serde_json::from_value::<RpcBlock>(json["mergedBlock"].clone()).unwrap()).unwrap();
        Fixture { network: json["network"].as_str().unwrap().to_string(), start, start_header, json, proofs, merged }
    }

    impl Fixture {
        pub(crate) fn raw(&self) -> RawVspcResponse {
            serde_json::from_value(self.json["vspc"].clone()).unwrap()
        }
        fn checker(&self) -> ChainProof {
            ChainProof::new(ChainProofConfig::default(), &self.network)
        }
        /// Every step of a batch as the follower runs it, with the recorded node answers.
        fn run(&self, cp: &mut ChainProof, raw: &RawVspcResponse) -> Result<(), ProofError> {
            let plan = cp.check_local(self.start, raw)?.expect("chain blocks");
            if cp.needs_anchor(&plan) {
                cp.set_anchor(&plan, &self.start_header)?;
            }
            for lane in cp.unknown_lanes(&plan) {
                let (p, _) =
                    self.proofs.get(&lane).ok_or_else(|| ProofError::Unavailable(format!("no recorded proof of lane {lane}")))?;
                cp.learn_lane(lane, p)?;
            }
            let expected = cp.expected(&plan)?;
            let proofs = expected
                .iter()
                .map(|(l, _)| {
                    self.proofs.get(l).map(|p| p.1.clone()).ok_or_else(|| ProofError::Unavailable(format!("no proof of {l}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            cp.finish(plan, &expected, &proofs)
        }
        /// The batch's transactions held by the recorded merged block.
        fn merged_txs<'a>(&self, raw: &'a RawVspcResponse) -> Vec<&'a Tx> {
            let m = Hash32(self.merged.header.hash.as_bytes());
            raw.chain_block_accepted_transactions
                .iter()
                .flat_map(|b| &b.accepted_transactions)
                .filter(|t| t.verbose_data.block_hash == Some(m))
                .collect()
        }
    }

    fn both() -> [Fixture; 2] {
        [fixture(TN10), fixture(MAINNET)]
    }

    fn refused(r: Result<(), ProofError>, what: &str) {
        match r {
            Err(ProofError::Refused(w)) => assert!(w.contains(what), "refused for another reason: {w}"),
            other => panic!("not refused ({what}): {other:?}"),
        }
    }

    /// The header of chain block `i` re-finalized after `f`, as a forger who hashes it himself would
    /// send it. The block's hash in the answer is replaced too.
    fn forge_header(raw: &mut RawVspcResponse, i: usize, f: impl FnOnce(&mut ChainBlockHeader)) {
        let h = &mut raw.chain_block_accepted_transactions[i].chain_block_header;
        f(h);
        let new = Hash32(consensus_header(h).unwrap().hash.as_bytes());
        h.hash = new;
        raw.added_chain_block_hashes[i] = new;
    }

    /// A native transaction of the batch with inputs: (block, position).
    fn native_tx(raw: &RawVspcResponse) -> (usize, usize) {
        for (b, blk) in raw.chain_block_accepted_transactions.iter().enumerate() {
            for (k, t) in blk.accepted_transactions.iter().enumerate() {
                if t.subnetwork_id.as_ref().is_some_and(|s| s.0 == [0; 20]) && !t.inputs.is_empty() {
                    return (b, k);
                }
            }
        }
        panic!("no native transaction")
    }

    /// The id a forger would give a transaction after changing it.
    fn reid(t: &mut Tx) {
        t.verbose_data.transaction_id = crate::rpc::verify::tx_id(t).unwrap();
    }

    #[test]
    fn real_batches_pass_every_check() {
        for fx in both() {
            let raw = fx.raw();
            let mut cp = fx.checker();
            let t0 = std::time::Instant::now();
            fx.run(&mut cp, &raw).unwrap_or_else(|e| panic!("{}: {e}", fx.network));
            let took = t0.elapsed();
            let n = raw.added_chain_block_hashes.len();
            let txs: usize = raw.chain_block_accepted_transactions.iter().map(|b| b.accepted_transactions.len()).sum();
            let s = cp.status();
            assert_eq!(s.blocks_total, n as u64);
            assert!(s.lane_proofs_total >= 2, "at the start and the end");
            assert_eq!(cp.anchor().unwrap().hash, *raw.added_chain_block_hashes.last().unwrap(), "the anchor moved to the end");
            let txs_m = fx.merged_txs(&raw);
            assert!(!txs_m.is_empty());
            let m = Hash32(fx.merged.header.hash.as_bytes());
            cp.check_merged_block(m, &fx.merged, &txs_m).unwrap();
            let t1 = std::time::Instant::now();
            for _ in 0..100 {
                assert!(pow_ok(&fx.start_header));
            }
            let pow = t1.elapsed() / 100;
            // The state kept between batches: the anchor, the lane tips and the recent targets.
            let state = std::mem::size_of::<ChainProof>() + cp.targets.capacity() * 32 + cp.lanes.capacity() * 64;
            eprintln!(
                "{}: {n} chain blocks, {txs} transactions: local checks {:.0} us per block (kHeavyHash alone {pow:?}), the whole batch {took:?}, state {state} bytes (this build's opt-level)",
                fx.network, s.cpu_us_per_block
            );
        }
    }

    /// A header changed without its hash: refused, as before.
    #[test]
    fn a_tampered_header_is_refused() {
        for fx in both() {
            let mut raw = fx.raw();
            raw.chain_block_accepted_transactions[1].chain_block_header.timestamp += 1;
            refused(fx.run(&mut fx.checker(), &raw), "hashes to");
        }
    }

    /// A forged last block with its own hash but no work: refused by the proof of work alone.
    #[test]
    fn bad_proof_of_work_is_refused_and_passes_without_the_check() {
        for fx in both() {
            let mut raw = fx.raw();
            let last = raw.added_chain_block_hashes.len() - 1;
            forge_header(&mut raw, last, |h| h.nonce = Some(h.nonce.unwrap().wrapping_add(1)));
            refused(fx.run(&mut fx.checker(), &raw), "proof of work");
            fx.run(&mut fx.checker().skipping(skip::POW), &raw).unwrap();
        }
    }

    /// Trivial `bits`, mined in a few hashes: refused by the floor alone.
    #[test]
    fn easy_bits_are_refused_and_pass_without_the_floor() {
        for fx in both() {
            let mut raw = fx.raw();
            let last = raw.added_chain_block_hashes.len() - 1;
            for nonce in 0.. {
                forge_header(&mut raw, last, |h| {
                    h.bits = Some(0x207f_ffff);
                    h.nonce = Some(nonce);
                });
                if pow_ok(&consensus_header(&raw.chain_block_accepted_transactions[last].chain_block_header).unwrap()) {
                    break;
                }
            }
            refused(fx.run(&mut fx.checker(), &raw), "easier than the floor");
            fx.run(&mut fx.checker().skipping(skip::FLOOR), &raw).unwrap();
        }
    }

    /// The running median: a target 8x the recent median is refused with the default factor 4
    /// even when the network minimum allows it, and let through with a factor 16.
    #[test]
    fn the_floor_follows_the_recent_median() {
        let fx = fixture(MAINNET);
        let raw = fx.raw();
        let cfg = |k| ChainProofConfig { floor_factor: k, min_difficulty: Some(1), ..Default::default() };
        for (k, through) in [(4, false), (16, true)] {
            let mut cp = ChainProof::new(cfg(k), "mainnet");
            fx.run(&mut cp, &raw).unwrap();
            let mut header = fx.start_header.clone();
            let median = median(cp.targets.iter().copied()).unwrap();
            header.bits = (median * 8u64).compact_target_bits();
            match (cp.check_work(&header, cp.max_target(Some(median))), through) {
                (Err(ProofError::Refused(w)), false) => assert!(w.contains("easier than the floor"), "{w}"),
                // The floor let the bits through; the work itself is not there.
                (Err(ProofError::Refused(w)), true) => assert!(w.contains("proof of work"), "{w}"),
                (r, _) => panic!("{r:?}"),
            }
        }
    }

    /// A batch that skips a chain block claims a selected parent that is not the block before it:
    /// refused by the link check first, by the lane proof without it.
    #[test]
    fn a_broken_link_is_refused() {
        for fx in both() {
            let mut raw = fx.raw();
            raw.added_chain_block_hashes.remove(1);
            raw.chain_block_accepted_transactions.remove(1);
            refused(fx.run(&mut fx.checker(), &raw), "selected parent");
            let r = fx.run(&mut fx.checker().skipping(skip::LINK), &raw);
            assert!(!matches!(&r, Err(ProofError::Refused(w)) if w.contains("selected parent")), "{r:?}");
            fx.run(&mut fx.checker().skipping(skip::LINK | skip::LANES), &raw).unwrap();
        }
    }

    /// An output changed without a new id: refused by the id check alone (the lane proofs commit to
    /// ids only).
    #[test]
    fn a_tampered_body_is_refused_and_passes_without_the_id_check() {
        for fx in both() {
            let mut raw = fx.raw();
            let (b, k) = native_tx(&raw);
            raw.chain_block_accepted_transactions[b].accepted_transactions[k].outputs[0].value += 1;
            refused(fx.run(&mut fx.checker(), &raw), "hashes to");
            fx.run(&mut fx.checker().skipping(skip::TXID), &raw).unwrap();
        }
    }

    /// The acceptance data changed: a native transaction hidden, another added with a valid id,
    /// the body changed with a new id. Each is refused by the lane proof alone.
    #[test]
    fn tampered_acceptance_is_refused_and_passes_without_the_lane_proof() {
        for fx in both() {
            let (b, k) = native_tx(&fx.raw());
            let mut hidden = fx.raw();
            hidden.chain_block_accepted_transactions[b].accepted_transactions.remove(k);
            let mut added = fx.raw();
            let mut t = added.chain_block_accepted_transactions[b].accepted_transactions[k].clone();
            t.payload.0 = kob_protocol::payload::MAGIC.to_vec();
            reid(&mut t);
            added.chain_block_accepted_transactions[b].accepted_transactions.push(t);
            let mut changed = fx.raw();
            let t = &mut changed.chain_block_accepted_transactions[b].accepted_transactions[k];
            t.outputs[0].value += 1;
            reid(t);
            for raw in [&hidden, &added, &changed] {
                refused(fx.run(&mut fx.checker(), raw), "the accepted transactions give");
                fx.run(&mut fx.checker().skipping(skip::LANES), raw).unwrap();
            }
        }
    }

    /// The mainnet window has a transaction in a lane other than the native one (subnetwork
    /// 97b1...). Hiding it is refused too.
    #[test]
    fn another_lane_is_proven_too() {
        let fx = fixture(MAINNET);
        let mut raw = fx.raw();
        assert!(fx.proofs.len() >= 2, "the recording holds a second lane");
        let (b, k) = raw
            .chain_block_accepted_transactions
            .iter()
            .enumerate()
            .find_map(|(b, blk)| {
                blk.accepted_transactions.iter().position(|t| t.subnetwork_id.as_ref().is_some_and(|s| s.0[0] == 0x97)).map(|k| (b, k))
            })
            .unwrap();
        let t = &mut raw.chain_block_accepted_transactions[b].accepted_transactions[k];
        t.payload.0.push(0);
        reid(t);
        refused(fx.run(&mut fx.checker(), &raw), "the accepted transactions give");
    }

    /// A tampered lane proof (another tip, another payload digest) is refused.
    #[test]
    fn a_tampered_lane_proof_is_refused() {
        for text in [TN10, MAINNET] {
            let native = native_lane();
            let mut fx = fixture(text);
            if let Some(l) = fx.proofs.get_mut(&native).unwrap().1.lane.as_mut() {
                l.tip = Hash::from_bytes([7; 32]);
            }
            refused(fx.run(&mut fx.checker(), &fx.raw()), "the node proves");
            let mut fx = fixture(text);
            fx.proofs.get_mut(&native).unwrap().1.payload_and_ctx_digest = Hash::from_bytes([7; 32]);
            refused(fx.run(&mut fx.checker(), &fx.raw()), "the proof gives the commitment");
        }
    }

    /// A coinbase-lane transaction is not proven, so it may carry nothing KOB reads.
    #[test]
    fn a_coinbase_lane_transaction_with_kob_data_is_refused() {
        for fx in both() {
            let mut raw = fx.raw();
            let t = &mut raw.chain_block_accepted_transactions[0].accepted_transactions[0];
            assert_eq!(t.subnetwork_id.as_ref().unwrap().0[0], 1, "the first is the coinbase");
            t.payload.0 = kob_protocol::payload::MAGIC.to_vec();
            reid(t);
            refused(fx.run(&mut fx.checker(), &raw), "coinbase-lane");
        }
    }

    /// The signature scripts of a transaction that can matter to KOB are checked against its merged
    /// block: another script is refused (and passes without the check). A merged block whose
    /// transactions were changed with the batch's is refused by its Merkle root (and passes
    /// without it).
    #[test]
    fn merged_blocks_prove_the_signature_scripts() {
        for fx in both() {
            let m = Hash32(fx.merged.header.hash.as_bytes());
            let mut raw = fx.raw();
            let t = raw
                .chain_block_accepted_transactions
                .iter_mut()
                .flat_map(|b| &mut b.accepted_transactions)
                .find(|t| t.verbose_data.block_hash == Some(m) && !t.inputs.is_empty())
                .unwrap();
            t.inputs[0].signature_script.0.push(0x51);
            let (script, id) = (t.inputs[0].signature_script.0.clone(), t.verbose_data.transaction_id);
            refused(fx.checker().check_merged_block(m, &fx.merged, &fx.merged_txs(&raw)), "signature scripts");
            fx.checker().skipping(skip::SCRIPTS).check_merged_block(m, &fx.merged, &fx.merged_txs(&raw)).unwrap();
            // The forger changes the merged block too.
            let mut txs = (*fx.merged.transactions).clone();
            let k = txs.iter().position(|x| x.id().as_bytes() == id.0).unwrap();
            txs[k].inputs[0].signature_script = script;
            let block = Block::new(fx.merged.header.as_ref().clone(), txs);
            refused(fx.checker().check_merged_block(m, &block, &fx.merged_txs(&raw)), "Merkle root");
            fx.checker().skipping(skip::MERKLE).check_merged_block(m, &block, &fx.merged_txs(&raw)).unwrap();
        }
    }
}
