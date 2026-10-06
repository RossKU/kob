//! What the matcher and the keepers read: the [`OrderBookView`] trait (implemented by the indexer,
//! and by [`MemoryBook`] for tests, dry runs and snapshot files).
//!
//! A view lists only **validated** orders (`docs/spec/matcher.md` §1.1: a valid placement record, a
//! fresh covenant id, and for ask-side kinds exactly one custody UTXO holding `amountLeft` base units, §1.2).
//! Stray token UTXOs owned by an order id are reported with the order so that nothing ever mistakes
//! them for liquidity; matchers and keepers never spend them.

use std::collections::{BTreeMap, BTreeSet};

use kob_protocol::state::AnyState;
use kob_protocol::tx::{OrderUtxo, TokenUtxo, Utxo};
use serde::{Deserialize, Serialize};

use super::family::Family;

/// A 32-byte covenant id (order identity).
pub type CovId = [u8; 32];

/// An outpoint `(transaction id, index)`.
pub type Outpoint = ([u8; 32], u32);

/// Outpoint of a UTXO.
pub fn outpoint(u: &Utxo) -> Outpoint {
    (u.transaction_id, u.index)
}

/// One listed order: its current UTXO and state, its custody (ask-side kinds) and what the
/// placement record said beyond the state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedOrder {
    /// Token family of the order, informational: the state's kind (`KobAsk` / `KobAskKron`) names it.
    #[serde(default = "super::family::default_family")]
    pub family: Family,
    /// The order UTXO (covenant id set) and its decoded state.
    pub order: OrderUtxo<AnyState>,
    /// Token-holding kinds: the custody (the first of `AnyState::custodies`: an ask's exactly `amountLeft` base units, a
    /// pair order's custody of the token it holds, a pair entry's A custody or B escrow), owned by the order id.
    #[serde(default)]
    pub custody: Option<TokenUtxo>,
    /// The second custody of an order that holds two (a sell-first `KobIfdPair`: its B prefund next to its A custody).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody_b: Option<TokenUtxo>,
    /// Day orders: wall-clock deadline (UTC unix seconds) from the placement record (§10.10).
    #[serde(default, with = "kob_protocol::json::field")]
    pub deadline: Option<u64>,
    /// DAA score at which the view first listed this order UTXO (class-1 freshness, §3.1).
    #[serde(default, with = "kob_protocol::json::field")]
    pub seen_daa: u64,
    /// Stray token UTXOs owned by the order id (§1.2): flagged, never spent by matchers or keepers.
    #[serde(default)]
    pub strays: Vec<TokenUtxo>,
    /// Foreign strays (tokens other than the order's, §1.2) with a state the indexer proved, by token: a keeper that
    /// refunds the order may return them to the maker (`KeeperConfig::return_foreign_strays`).
    #[serde(default)]
    pub foreign: Vec<kob_protocol::build::ForeignStrays>,
}

impl ListedOrder {
    /// The order's state as the KCC-20 kind of the same name: the KRON kinds (`KobAskKron`, ...) carry the
    /// same typed states, so family-agnostic logic matches on this.
    pub fn base_state(&self) -> AnyState {
        self.order.state.clone().into_family(kob_protocol::family::Family::Kcc20)
    }

    /// Covenant id of the order.
    pub fn id(&self) -> CovId {
        self.order.utxo.covenant_id.unwrap_or([0; 32])
    }

    /// DAA score of the order UTXO.
    pub fn utxo_daa(&self) -> u64 {
        self.order.utxo.block_daa_score
    }

    /// §1.2: a token-holding order is listable only with exactly its custodies (`AnyState::custodies`, in that order:
    /// `custody`, then `custody_b`), each holding its exact amount of its token, owned by the order id (a repeating
    /// `KobIfdAsk` with nothing left has no custody; a pair entry holds one or two).
    pub fn custody_ok(&self) -> bool {
        if crate::sanity::check(&self.order.state).is_err() {
            return false;
        }
        let want: Vec<([u8; 32], i64)> = self.order.state.custodies().into_iter().filter(|c| c.1 != 0).collect();
        let have: Vec<&TokenUtxo> = self.custody.iter().chain(self.custody_b.iter()).collect();
        if want.len() != have.len() || (self.custody.is_none() && self.custody_b.is_some()) {
            return false;
        }
        want.iter().zip(have).all(|(&(token, amount), c)| {
            c.state.amount() == amount
                && c.state.owner() == self.id()
                && c.state.is_covenant_owned()
                && c.state.is_plain()
                && Some(c.state.family()) == custody_family(&self.order.state, &token)
                && c.utxo.covenant_id == Some(token)
                && !self.strays.iter().any(|s| outpoint(&s.utxo) == outpoint(&c.utxo))
        })
    }

    /// The custody of `token` (pair orders hold custodies of either token; a KAS kind: its one custody).
    pub fn custody_of(&self, token: &[u8; 32]) -> Option<&TokenUtxo> {
        self.custody.iter().chain(self.custody_b.iter()).find(|c| c.utxo.covenant_id.as_ref() == Some(token))
    }

    /// Extension commitment of the order's token (custody for ask-side kinds, state for bids).
    pub fn extension(&self) -> Option<[u8; 32]> {
        match &self.order.state {
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => Some(s.extension_commitment),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => Some(s.extension_commitment),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => Some(s.extension_commitment),
            // An empty repeating sell-first entry takes its custody's commitment from its exit.
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) if s.amount_left == 0 => s.exit().ok().map(|e| e.extension_commitment),
            _ => self.custody.as_ref().map(|c| c.state.extension()),
        }
    }
}

/// The token family of an order's custody of `token` (a pair order: the family its state names for that token; a KAS
/// kind: the order's family).
pub fn custody_family(s: &AnyState, token: &[u8; 32]) -> Option<Family> {
    match s.pair_tokens() {
        Some(t) if &t.a.cov_id == token => t.a.family_of(),
        Some(t) if &t.b.cov_id == token => t.b.family_of(),
        Some(_) => None,
        None => (&s.token_cov_id() == token).then(|| s.family()),
    }
}

/// A book: orders that can share a transaction (same token, program, extension commitment and
/// `scale`, `docs/spec/matcher.md` §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookKey {
    pub family: Family,
    #[serde(with = "kob_protocol::json::field")]
    pub token: [u8; 32],
    #[serde(with = "kob_protocol::json::field")]
    pub template: [u8; 32],
    #[serde(with = "kob_protocol::json::field")]
    pub extension: [u8; 32],
    /// Base units per whole token of the book's orders (their prices are comparable only at the same scale).
    #[serde(with = "kob_protocol::json::field")]
    pub scale: i64,
}

/// A token market: the books of one token (covenant id, program, extension commitment) at every `scale`. Orders of one
/// market can share a transaction's token slots; a routed pair order trades against the plain orders of its two markets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Market {
    pub family: Family,
    pub token: [u8; 32],
    pub template: [u8; 32],
    pub extension: [u8; 32],
}

impl BookKey {
    /// The market of the book (the book without its `scale`).
    pub fn market(&self) -> Market {
        Market { family: self.family, token: self.token, template: self.template, extension: self.extension }
    }
}

impl Market {
    /// The book of this market at `scale`.
    pub fn book(&self, scale: i64) -> BookKey {
        BookKey { family: self.family, token: self.token, template: self.template, extension: self.extension, scale }
    }
}

/// The book an order belongs to (None for a pair order or an order with no known extension).
pub fn book_key(o: &ListedOrder) -> Option<BookKey> {
    let s = &o.order.state;
    // A pair order quotes token B, not KAS: it belongs to no KAS book (`matcher::pair`).
    if s.is_pair() {
        return None;
    }
    Some(BookKey {
        family: s.family(),
        token: s.token_cov_id(),
        template: s.token_tpl_hash()?,
        extension: o.extension()?,
        scale: s.scale(),
    })
}

/// Everything the matcher and the keepers read from the indexer.
pub trait OrderBookView {
    /// Virtual DAA score the view is synced to.
    fn daa_score(&self) -> u64;
    /// Every listed, live order (all tokens).
    fn orders(&self) -> Vec<ListedOrder>;
    /// P2PK token UTXOs of `owner` (the operator's token inventory, exported with the snapshot; the matcher does not
    /// trade it).
    fn wallet_tokens(&self, _owner: &[u8; 32]) -> Vec<TokenUtxo> {
        vec![]
    }
    /// One order by covenant id.
    fn order(&self, id: &CovId) -> Option<ListedOrder> {
        self.orders().into_iter().find(|o| &o.id() == id)
    }
}

/// In-memory book (tests, dry runs, snapshot files). JSON-serialisable: `kob-executor match
/// --book-file` reads this format.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBook {
    #[serde(with = "kob_protocol::json::field")]
    pub daa_score: u64,
    #[serde(default)]
    pub orders: Vec<ListedOrder>,
    /// P2PK token UTXOs by owner (hex key in JSON).
    #[serde(default)]
    pub wallet_tokens: Vec<TokenUtxo>,
}

impl MemoryBook {
    pub fn new(daa_score: u64) -> Self {
        MemoryBook { daa_score, ..Default::default() }
    }

    /// Adds or replaces an order (by covenant id).
    pub fn put(&mut self, o: ListedOrder) {
        let id = o.id();
        self.orders.retain(|x| x.id() != id);
        self.orders.push(o);
    }

    /// Removes an order.
    pub fn remove(&mut self, id: &CovId) {
        self.orders.retain(|x| &x.id() != id);
    }

    /// Every outpoint the book references (orders and custodies).
    pub fn outpoints(&self) -> BTreeSet<Outpoint> {
        let mut s = BTreeSet::new();
        for o in &self.orders {
            s.insert(outpoint(&o.order.utxo));
            for c in o.custody.iter().chain(o.custody_b.iter()) {
                s.insert(outpoint(&c.utxo));
            }
        }
        s
    }
}

impl OrderBookView for MemoryBook {
    fn daa_score(&self) -> u64 {
        self.daa_score
    }
    fn orders(&self) -> Vec<ListedOrder> {
        self.orders.clone()
    }
    fn wallet_tokens(&self, owner: &[u8; 32]) -> Vec<TokenUtxo> {
        self.wallet_tokens.iter().filter(|t| &t.state.owner() == owner && t.state.is_user()).cloned().collect()
    }
}

/// Groups the listed orders of a view by book.
pub fn books(orders: &[ListedOrder]) -> BTreeMap<BookKey, Vec<ListedOrder>> {
    let mut m: BTreeMap<BookKey, Vec<ListedOrder>> = BTreeMap::new();
    for o in orders {
        if let Some(k) = book_key(o) {
            m.entry(k).or_default().push(o.clone());
        }
    }
    m
}

/// The matcher's clock at planning time: the node's virtual DAA score and the UTC wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clock {
    /// Virtual DAA score.
    pub daa: u64,
    /// UTC unix seconds (NTP-synced).
    pub utc: u64,
}

impl Clock {
    /// `t = lockTime = DAA − margin` (§3.3; default margin 5 DAA).
    pub fn lock_time(&self, margin: u64) -> u64 {
        self.daa.saturating_sub(margin)
    }
}

/// UTC unix seconds now.
pub fn utc_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
