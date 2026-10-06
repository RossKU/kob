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
    /// Token surplus the operator may keep as inventory (`docs/spec/matcher.md` §3.5, *inventory*). Off by default: the
    /// reference planner then holds no tokens.
    pub inventory: InventoryPolicy,
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
            inventory: InventoryPolicy::default(),
        }
    }
}

/// A price as a fraction: `sompi` per `per` base units of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitPrice {
    #[serde(with = "kob_protocol::json::field")]
    pub sompi: u64,
    #[serde(with = "kob_protocol::json::field")]
    pub per: u64,
}

impl UnitPrice {
    /// The KAS `q` base units are worth at this price, rounded down (0 for a malformed price).
    pub fn value(&self, q: i64) -> i128 {
        if self.per == 0 || q <= 0 {
            return 0;
        }
        q as i128 * self.sompi as i128 / self.per as i128
    }
}

/// One token the operator accepts as inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryToken {
    /// The token's covenant id (hex).
    #[serde(with = "kob_protocol::json::field")]
    pub token: [u8; 32],
    /// The owner's own valuation of the token (it sells its inventory off-matcher), used instead of the plain KAS bids.
    /// Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_price: Option<UnitPrice>,
    /// Least surplus (base units) worth keeping. Default: with `ref_price`, none; otherwise the smallest fill one of the
    /// valued bids accepts (its minimum fill), so the kept amount alone could fill it (no unsellable dust). Set it lower to
    /// accumulate small surpluses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_amount: Option<i64>,
}

/// The surplus-inventory policy (owner decision 2026-10-06, `docs/spec/matcher.md` §3.5): when a batch leaves a token
/// surplus that would go to a pair ask's delivery (the ask's guarantee is a floor), the operator may take it into its own
/// key instead and count its value as income, so a crossed pair match that pays no KAS (no tips, no KAS bid able to take
/// the surplus) still pays its fee. A surplus a KAS bid takes in the same transaction is still sold there first (no
/// inventory risk); only the rest is kept. The matcher only accumulates: the owner sells the inventory off-matcher, and the
/// maintenance jobs never sell a listed token ([`InventoryPolicy::holds`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct InventoryPolicy {
    /// Master switch (off by default).
    pub accept_surplus_tokens: bool,
    /// The share of a kept surplus's valuation counted as income, basis points (default 8,000: 80 %).
    pub haircut_bps: u32,
    /// The tokens it may keep (an allowlist; every other token's surplus goes to the pair ask as before).
    pub tokens: Vec<InventoryToken>,
}

impl Default for InventoryPolicy {
    fn default() -> Self {
        InventoryPolicy { accept_surplus_tokens: false, haircut_bps: 8_000, tokens: vec![] }
    }
}

impl InventoryPolicy {
    /// Reads a policy file (strict JSON, the camelCase fields of [`InventoryPolicy`]; `--inventory-policy`) and checks it.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let p: InventoryPolicy = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        p.check().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(p)
    }
    /// The policy's own consistency: a haircut of at most 100 %, each token once, a `refPrice` positive, a `minAmount` not
    /// negative.
    pub fn check(&self) -> Result<(), String> {
        if self.haircut_bps > 10_000 {
            return Err(format!("haircutBps {} above 10000", self.haircut_bps));
        }
        let mut seen = BTreeSet::new();
        for t in &self.tokens {
            let id = kob_protocol::json::to_hex(&t.token);
            if !seen.insert(t.token) {
                return Err(format!("token {id} listed twice"));
            }
            if t.ref_price.is_some_and(|r| r.per == 0 || r.sompi == 0) {
                return Err(format!("token {id}: refPrice must be positive"));
            }
            if t.min_amount.is_some_and(|a| a < 0) {
                return Err(format!("token {id}: minAmount must not be negative"));
            }
        }
        Ok(())
    }
    /// The rule of `token` when the policy is on and lists it.
    pub fn rule(&self, token: &[u8; 32]) -> Option<&InventoryToken> {
        if !self.accept_surplus_tokens {
            return None;
        }
        self.tokens.iter().find(|t| &t.token == token)
    }
    /// `v` after the haircut (at most 100 %).
    pub fn haircut(&self, v: i128) -> i128 {
        v.max(0) * self.haircut_bps.min(10_000) as i128 / 10_000
    }
    /// Whether `token` is held as surplus inventory: listed, whether or not the switch is on (tokens already accumulated stay
    /// put). The maintenance jobs never sell such a token (`crate::maintenance`).
    pub fn holds(&self, token: &[u8; 32]) -> bool {
        self.tokens.iter().any(|t| &t.token == token)
    }
}

/// Byte estimate of the operator's token output a kept surplus adds (the output and its leader `next_states` entry).
pub const KEPT_OUTPUT_BYTES: u64 = OUTPUT_BYTES + NEXT_STATE_BYTES;

/// A token surplus the batch keeps as the operator's inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub token: [u8; 32],
    /// Base units the operator's token output receives.
    pub amount: i64,
    /// Their valuation under the policy (after the haircut), sompi: counted as income of the batch.
    pub value: i64,
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
    /// Token surpluses the operator keeps as inventory ([`InventoryPolicy`]; empty with the policy off).
    pub kept: Vec<Kept>,
}

impl Plan {
    /// Estimated operator profit: the margin, the update tips and the value of the kept inventory less the fee.
    pub fn profit(&self) -> i64 {
        self.margin.saturating_add(self.tips).saturating_add(self.kept_value()).saturating_sub(self.est_fee)
    }
    /// The policy value of the inventory the plan keeps (sompi): income the built transaction's KAS accounting does not see.
    pub fn kept_value(&self) -> i64 {
        self.kept.iter().fold(0i64, |a, k| a.saturating_add(k.value))
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

#[cfg(test)]
mod inventory_tests {
    use super::*;

    /// The documented policy file (`docs/ops/executor.md`, "Surplus inventory") parses, and the checks refuse what makes no
    /// sense: an unknown key, a haircut above 100 %, a zero reference price, a token listed twice.
    #[test]
    fn the_policy_file_parses_and_is_checked() {
        let doc = r#"{
          "acceptSurplusTokens": true,
          "haircutBps": 8000,
          "tokens": [
            { "token": "7272727272727272727272727272727272727272727272727272727272727272", "minAmount": 0 },
            { "token": "7171717171717171717171717171717171717171717171717171717171717171",
              "refPrice": { "sompi": "2300000000", "per": "100000000" } }
          ]
        }"#;
        let p: InventoryPolicy = serde_json::from_str(doc).expect("parses");
        p.check().expect("consistent");
        let t = [0x72u8; 32];
        assert_eq!(p.rule(&t).and_then(|r| r.min_amount), Some(0));
        assert_eq!(p.rule(&[0x71; 32]).and_then(|r| r.ref_price).map(|r| r.value(50_000_000)), Some(1_150_000_000));
        assert!(p.rule(&[0x70; 32]).is_none() && !p.holds(&[0x70; 32]), "an unlisted token");
        assert_eq!(p.haircut(1_000), 800);
        // off: no rule, but a listed token is still held (never sold by maintenance)
        let off = InventoryPolicy { accept_surplus_tokens: false, ..p.clone() };
        assert!(off.rule(&t).is_none() && off.holds(&t));
        assert_eq!(InventoryPolicy::default(), serde_json::from_str::<InventoryPolicy>("{}").unwrap(), "off by default");
        assert!(serde_json::from_str::<InventoryPolicy>(r#"{"acceptSurplus": true}"#).is_err(), "unknown key");
        assert!(InventoryPolicy { haircut_bps: 10_001, ..p.clone() }.check().is_err());
        let mut zero = p.clone();
        zero.tokens[1].ref_price = Some(UnitPrice { sompi: 0, per: 100_000_000 });
        assert!(zero.check().is_err(), "a zero reference price");
        let mut twice = p.clone();
        twice.tokens[1].token = t;
        assert!(twice.check().is_err(), "listed twice");
    }
}
