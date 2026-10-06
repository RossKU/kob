//! Planner types and settings shared by the global batch planner ([`super::batch`]).
//!
//! The batch itself is planned over every book at once by [`super::batch::plan_batch`]: one transaction may fill several
//! token books, the pair orders of any pair (netted and routed), every order class together, and arm the stops its plain fills trigger
//! (`docs/spec/matcher.md` §3, §4). This module holds what a plan is ([`Plan`], [`Fill`], [`PlanUpdate`]), the planner
//! settings ([`PlannerConfig`]), the physical limits a transaction is sized against ([`PHYSICAL_TX_BYTES`]) and the cost
//! estimates ([`est_fee`], [`update_cost`]).

use std::collections::BTreeSet;

use kob_protocol::family::Family;
use serde::{Deserialize, Serialize};

use super::book::{BookKey, CovId, Outpoint};
use super::candidate::*;

/// The largest transaction a node relays and a block can hold, in serialized bytes: rusty-kaspa v2.1.0 charges a
/// transaction `transient mass = 4 × size` (`consensus/core/src/mass/mod.rs:367`, `TRANSIENT_BYTE_TO_MASS_FACTOR`
/// `consensus/core/src/constants.rs:31`) and its mempool refuses a transient mass above the block limit of 1,000,000
/// (`mining/src/mempool/check_transaction_limits.rs:24`, limits `consensus/core/src/config/params.rs:626`), so no
/// transaction exceeds 250,000 bytes. The compute (500,000) and storage (500,000) limits are checked on the built
/// transaction ([`kob_protocol::tx::MassReport::within_block_limits`]); the planner starts from this size and shrinks its
/// budget when a built batch overruns one of them (`super::engine`).
pub const PHYSICAL_TX_BYTES: u64 = kob_protocol::tx::BLOCK_TRANSIENT_LIMIT / 4;

/// Planner settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlannerConfig {
    /// `t = DAA − margin` (§3.3; default 5).
    pub safety_margin: u64,
    /// Fee rate, sompi per gram (at least 100).
    pub fee_rate: u64,
    /// Optional operator cap on the size of one transaction, bytes. 0 (the default): the physical limit
    /// ([`PHYSICAL_TX_BYTES`] and the block mass limits of the built transaction). A larger batch only costs a larger
    /// fee, which the batch pays; a matcher that builds too large simply loses races, so nothing else bounds it.
    pub max_tx_bytes: u64,
    /// Smallest profit (sompi) a batch must make after its fee.
    pub min_profit: i64,
    /// Arm (or ratchet) the listed stops a batch's plain fills trigger but the batch does not fill, with an `update`
    /// whenever the batch stays profitable (its cost is charged, its `keeperTip` is income, §4.4). Off: stops are only
    /// filled by the batch that triggers them.
    pub arm: bool,
    /// Most transactions one order may take part in within a tick: its first fill and the chained fills of its unaccepted
    /// continuations (§7).
    pub max_chain: usize,
    /// Optional operator knob: at most this many best-priority candidates per (book, class, side). 0 (the default): every
    /// candidate. The planner's walks are sorted and bounded by the tick's wall-clock budget, so no cap is needed for time
    /// (a cap only hides the tail of a book.
    pub max_candidates_per_group: usize,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        PlannerConfig {
            safety_margin: 5,
            fee_rate: 100,
            max_tx_bytes: 0,
            min_profit: 0,
            arm: true,
            max_chain: 4,
            max_candidates_per_group: 0,
        }
    }
}

impl PlannerConfig {
    /// The byte budget a batch starts from: the physical limit, or the operator's cap below it.
    pub fn tx_byte_budget(&self) -> u64 {
        if self.max_tx_bytes == 0 {
            PHYSICAL_TX_BYTES
        } else {
            self.max_tx_bytes.min(PHYSICAL_TX_BYTES)
        }
    }
}

/// One planned fill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    pub cand: Cand,
    /// Base units filled (a pair order: n base units of its base token A; its candidate is its primary leg, the other leg is
    /// no fill of its own).
    pub amount: i64,
    /// An unarmed stop leg / stop entry triggered in this transaction: the index (in [`Plan::fills`], which is the leg
    /// index of the lowered batch) of the plain resting fill that is its evidence (a pair stop: the KAS-book fill of A, or the
    /// resting `KobPair` fill).
    pub evidence: Option<usize>,
    /// A pair stop armed by two KAS-book fills (mode 0): the fill of B.
    pub evidence_b: Option<usize>,
}

/// What an update does to its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UpdateKind {
    /// An unarmed stop leg or stop entry becomes armed (its band auction starts at the continuation's DAA score).
    Arm,
    /// A trailing stop's trigger moves by the steps its evidence justifies.
    Trail,
}

impl UpdateKind {
    pub fn name(self) -> &'static str {
        match self {
            UpdateKind::Arm => "arm",
            UpdateKind::Trail => "trail",
        }
    }
}

/// An arm or trailing ratchet without a fill of the order (`kob_protocol::build::BatchUpdate`), next to its evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanUpdate {
    pub id: CovId,
    pub book: BookKey,
    /// The order UTXO the update spends.
    pub outpoint: Outpoint,
    pub kind: UpdateKind,
    /// Index in [`Plan::fills`] of the plain resting fill that is the evidence (a pair order: the KAS-book fill of A, or the
    /// resting `KobPair` fill).
    pub evidence: usize,
    /// A pair order armed by two KAS-book fills (mode 0): the fill of B.
    pub evidence_b: Option<usize>,
    /// Sompi the batch takes from the order (its `keeperTip`).
    pub take: i64,
    /// Estimated cost of the update (its bytes at the fee rate, at least the measured marginal fee).
    pub cost: i64,
    /// Trailing ratchet: the steps the evidence justifies (1 for an arm).
    pub steps: i64,
}

/// A planned transaction: fills of any number of books and pair orders (§3) and the updates their evidence pays for (§4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The book of the first fill (logs); [`Plan::books`] lists every book the plan trades.
    pub book: BookKey,
    pub lock_time: u64,
    /// Pair order fills first (one per order), then the other fills in allocation order.
    pub fills: Vec<Fill>,
    /// Arms and trailing ratchets without a fill (their inputs follow the token inputs).
    pub updates: Vec<PlanUpdate>,
    /// Σ bid-side spends − Σ ask-side proceeds (sompi): the spread plus the tips of every book and route.
    pub margin: i64,
    /// Σ keeper tips the updates take (sompi).
    pub tips: i64,
    pub est_bytes: u64,
    pub est_fee: i64,
}

impl Plan {
    /// Estimated operator profit: the margin and the update tips less the fee.
    pub fn profit(&self) -> i64 {
        self.margin.saturating_add(self.tips).saturating_sub(self.est_fee)
    }
    /// Every covenant id the plan spends (legs, merged entries and updated orders).
    pub fn spent_ids(&self) -> BTreeSet<CovId> {
        let mut s = BTreeSet::new();
        for f in &self.fills {
            s.insert(f.cand.id);
            if let Some(e) = f.cand.merge {
                s.insert(e);
            }
        }
        for u in &self.updates {
            s.insert(u.id);
        }
        s
    }
    /// Base units filled per order id.
    pub fn amount_of(&self, id: &CovId) -> i64 {
        self.fills.iter().filter(|f| &f.cand.id == id).map(|f| f.amount).sum()
    }
    /// Every book the plan fills (a pair order fill counts in the markets of both its tokens, scale 0).
    pub fn books(&self) -> BTreeSet<BookKey> {
        let mut s = BTreeSet::new();
        for f in &self.fills {
            s.insert(f.cand.book);
            if let Some(x) = &f.cand.pair {
                s.insert(x.info.a.book(0));
                s.insert(x.info.b.book(0));
            }
        }
        s
    }
    /// Token families the plan trades.
    pub fn families(&self) -> BTreeSet<Family> {
        let mut s: BTreeSet<Family> = self.fills.iter().map(|f| f.cand.book.family).collect();
        for f in &self.fills {
            if let Some(x) = &f.cand.pair {
                s.insert(x.info.a.family);
                s.insert(x.info.b.family);
            }
        }
        s
    }
    /// True when the plan fills a pair order.
    pub fn has_pair(&self) -> bool {
        self.fills.iter().any(|f| f.cand.pair.is_some())
    }
    /// Fills triggered in this transaction (unarmed stops next to their evidence).
    pub fn triggered(&self) -> usize {
        self.fills.iter().filter(|f| f.evidence.is_some()).count()
    }
}

/// Whether `bid` pays at least what `ask` receives per token base unit (all-in, §2.1): `(quote ± tip) / scale` of both,
/// compared exactly by cross-multiplication (within one book one scale; across the books of a market, a pair order's legs,
/// like with like).
pub fn crosses(bid: &Cand, ask: &Cand) -> bool {
    let (bn, bd) = bid.per_base();
    let (an, ad) = ask.per_base();
    bn * ad >= an * bd
}

/// Estimated fee of `bytes` at `fee_rate` (sompi per gram): the fee mass is max(compute, 2 × size); compute adds ~10 grams
/// per script-public-key byte and the budgets, which the 10% margin covers.
pub fn est_fee(bytes: u64, fee_rate: u64) -> i64 {
    (fee_rate as i128 * (2 * bytes as i128 + bytes as i128 / 10)).min(i64::MAX as i128) as i64
}

/// Estimated marginal cost of one update in a batch: its bytes at the fee rate, and at least the measured marginal fee of
/// an update of its token program (`keeper_tips.json` `updateFee`, at the minimum fee rate) scaled to the fee rate.
pub fn update_cost(u: &UpCand, fee_rate: u64) -> i64 {
    let floor = (u.fee_floor as i128 * fee_rate as i128 / kob_protocol::tx::MIN_FEE_RATE as i128).min(i64::MAX as i128) as i64;
    est_fee(u.bytes, fee_rate).max(floor)
}
